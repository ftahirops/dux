use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the index DB lives. Root-writable system path if we can, else per-user.
pub fn data_dir() -> Result<PathBuf> {
    // Prefer the system path (shared with xtop-style tooling) when writable.
    let system = PathBuf::from("/var/lib/dux");
    if std::fs::File::open(system.join("dux.db")).is_ok() || can_use(&system) {
        return Ok(system);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("cannot determine data dir: set HOME or XDG_DATA_HOME")?;
    let dir = base.join("dux");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn can_use(p: &PathBuf) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    // crude writability probe
    let probe = p.join(".dux_write_probe");
    let ok = std::fs::write(&probe, b"1").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("dux.db"))
}

/// Read metadata when a path exists, distinguishing absence from permission,
/// I/O, and other inspection errors. `Path::exists()` discards all such errors.
pub fn metadata_if_exists(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    Ok(())
}

/// True for dux's own SQLite database and transient sidecars. These must never
/// feed back into the index or growth history: doing so wastes space and makes
/// "fastest growth" report the observer rather than the workload.
pub fn is_index_artifact(path: &std::path::Path, db: &std::path::Path) -> bool {
    use std::ffi::OsString;
    let with_suffix = |suffix: &str| {
        let mut value: OsString = db.as_os_str().to_owned();
        value.push(suffix);
        std::path::PathBuf::from(value)
    };
    if path == db
        || [
            "-wal",
            "-shm",
            "-journal",
            ".lock",
            ".new",
            ".new-wal",
            ".new-shm",
            ".heartbeat",
            ".watch.status",
            ".scan.progress",
        ]
        .iter()
        .any(|suffix| path == with_suffix(suffix))
    {
        return true;
    }
    let Some(parent) = db.parent() else {
        return false;
    };
    if path.parent() != Some(parent) {
        return false;
    }
    let Some(base) = db.file_name().map(|s| s.to_string_lossy()) else {
        return false;
    };
    path.file_name()
        .map(|s| {
            [
                ".new.scan-",
                ".heartbeat.",
                ".watch.status.",
                ".scan.progress.",
            ]
            .iter()
            .any(|suffix| s.to_string_lossy().starts_with(&format!("{base}{suffix}")))
        })
        .unwrap_or(false)
}

/// Acquire an EXCLUSIVE, non-blocking advisory lock for `db`, held for the
/// lifetime of the returned file handle. This is the real mutual-exclusion guard
/// (the heartbeat is only advisory): two concurrent writers — scan+scan,
/// scan+daemon, or daemon+daemon — race on the same `<db>.new`/WAL and corrupt
/// the index. Both `dux scan` and the daemon take this before touching the DB.
pub fn lock_db(db: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let mut lp = db.to_path_buf().into_os_string();
    lp.push(".lock");
    let lock_path = PathBuf::from(lp);
    ensure_parent_dir(&lock_path)?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening lock {}", lock_path.display()))?;
    // LOCK_EX | LOCK_NB: fail fast rather than block if another process holds it.
    // SAFETY: `f` owns a live descriptor and flock only inspects its open-file description.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        if matches!(e.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            anyhow::bail!(
                "another dux scan or daemon already holds {} — stop it first \
                 (e.g. `sudo systemctl stop dux`) before scanning",
                db.display()
            );
        }
        return Err(anyhow::anyhow!("flock {}: {e}", lock_path.display()));
    }
    Ok(f)
}

/// Live filesystem capacity for the filesystem containing `path` (via statvfs).
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)] // free/inodes_free kept for completeness / future use
pub struct FsStat {
    pub total: i64,
    pub free: i64,  // free to root
    pub avail: i64, // available to unprivileged users
    pub used: i64,
    pub inodes_total: i64,
    pub inodes_free: i64,
    pub inodes_used: i64,
}

impl FsStat {
    /// Percent used (df convention: used / (used + available)).
    pub fn use_pct(&self) -> f64 {
        // Convert before adding: both values are non-negative byte counts, but
        // their sum can exceed i64 even when each field is representable.
        let denom = self.used as f64 + self.avail as f64;
        if denom <= 0.0 {
            0.0
        } else {
            self.used as f64 / denom * 100.0
        }
    }
    pub fn inode_pct(&self) -> f64 {
        if self.inodes_total <= 0 {
            0.0
        } else {
            self.inodes_used as f64 / self.inodes_total as f64 * 100.0
        }
    }
}

pub fn fs_stat(path: &std::path::Path) -> Option<FsStat> {
    fs_stat_checked(path).ok()
}

/// Return filesystem capacity or the underlying path/statvfs error.
pub fn fs_stat_checked(path: &std::path::Path) -> Result<FsStat> {
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("filesystem path contains NUL: {}", path.display()))?;
    let mut s = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is NUL-terminated and `s` points to writable statvfs storage.
    if unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("statvfs {}", path.display()));
    }
    // SAFETY: successful statvfs initialized the complete output structure.
    let s = unsafe { s.assume_init() };
    fs_stat_from_counts(
        s.f_blocks, s.f_bfree, s.f_bavail, s.f_frsize, s.f_files, s.f_ffree,
    )
}

fn fs_stat_from_counts(
    blocks: u64,
    blocks_free: u64,
    blocks_available: u64,
    fragment_size: u64,
    files: u64,
    files_free: u64,
) -> Result<FsStat> {
    fn bytes(count: u64, size: u64, field: &str) -> Result<i64> {
        let value = u128::from(count) * u128::from(size);
        i64::try_from(value).with_context(|| format!("statvfs {field} exceeds supported range"))
    }
    fn count(value: u64, field: &str) -> Result<i64> {
        i64::try_from(value).with_context(|| format!("statvfs {field} exceeds supported range"))
    }

    let total = bytes(blocks, fragment_size, "total capacity")?;
    let free = bytes(blocks_free, fragment_size, "free capacity")?;
    let avail = bytes(blocks_available, fragment_size, "available capacity")?;
    let inodes_total = count(files, "inode capacity")?;
    let inodes_free = count(files_free, "free inode capacity")?;
    anyhow::ensure!(
        free <= total,
        "statvfs free capacity exceeds total capacity"
    );
    let used = total
        .checked_sub(free)
        .context("statvfs free capacity exceeds total capacity")?;
    anyhow::ensure!(
        avail <= total,
        "statvfs available capacity exceeds total capacity"
    );
    anyhow::ensure!(
        inodes_free <= inodes_total,
        "statvfs free inode capacity exceeds total"
    );
    let inodes_used = inodes_total
        .checked_sub(inodes_free)
        .context("statvfs free inode capacity exceeds total")?;
    Ok(FsStat {
        total,
        free,
        avail,
        used,
        inodes_total,
        inodes_free,
        inodes_used,
    })
}

/// Convert the kernel's 512-byte allocated-block count to the signed byte
/// representation used in SQLite. Refuse overflow so disk totals never become
/// negative or silently wrap when a filesystem exposes an unusually large file.
pub fn allocated_bytes(metadata: &std::fs::Metadata) -> Result<i64> {
    use std::os::unix::fs::MetadataExt;
    let bytes = u128::from(metadata.blocks()) * 512;
    i64::try_from(bytes).context("allocated file size exceeds supported range")
}

/// Convert Linux metadata identities into the signed range used by SQLite.
/// `MetadataExt::dev()` and `ino()` are `u64`; unchecked casts can alias an
/// out-of-range filesystem identity to a different positive inode.
pub fn indexed_identity(metadata: &std::fs::Metadata) -> Result<(i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    indexed_identity_from_values(metadata.dev(), metadata.ino())
}

fn indexed_identity_from_values(dev: u64, inode: u64) -> Result<(i64, i64)> {
    let dev = i64::try_from(dev).context("filesystem device ID exceeds i64 range")?;
    let inode = i64::try_from(inode).context("filesystem inode ID exceeds i64 range")?;
    Ok((dev, inode))
}

#[cfg(test)]
fn allocated_bytes_from_blocks(blocks: u64) -> Result<i64> {
    let bytes = u128::from(blocks) * 512;
    i64::try_from(bytes).context("allocated file size exceeds supported range")
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Send an sd_notify datagram when systemd supplied NOTIFY_SOCKET. Implemented
/// directly to avoid a runtime dependency and to support both pathname and
/// Linux abstract (@name) UNIX sockets. Best-effort outside systemd.
pub fn systemd_notify(message: &str) {
    use std::os::unix::ffi::OsStrExt;
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let bytes = socket.as_os_str().as_bytes();
    if bytes.is_empty() {
        return;
    }
    // SAFETY: socket takes scalar arguments and returns a new owned descriptor.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return;
    }
    // SAFETY: zero is valid for sockaddr_un; the family and path are filled below.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let max = addr.sun_path.len();
    let (copy, abstract_socket) = if bytes[0] == b'@' {
        (&bytes[1..], true)
    } else {
        (bytes, false)
    };
    let start = usize::from(abstract_socket);
    if copy.len() + start >= max {
        // SAFETY: `fd` is the newly created socket and has not been transferred.
        unsafe { libc::close(fd) };
        return;
    }
    for (i, b) in copy.iter().enumerate() {
        addr.sun_path[start + i] = *b as libc::c_char;
    }
    // One leading NUL for abstract sockets or one trailing NUL for path sockets.
    let path_len = copy.len() + 1;
    let addr_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_len) as libc::socklen_t;
    // SAFETY: `addr` is initialized, `addr_len` is bounded by sun_path, and the
    // message slice remains alive for the duration of the synchronous sendto.
    let _ = unsafe {
        libc::sendto(
            fd,
            message.as_ptr().cast(),
            message.len(),
            libc::MSG_NOSIGNAL,
            (&addr as *const libc::sockaddr_un).cast(),
            addr_len,
        )
    };
    // SAFETY: `fd` is still uniquely owned by this function.
    unsafe { libc::close(fd) };
}

/// System installs retain the stable /run paths. Independent indexes use
/// adjacent sidecars so tests and per-user daemons never impersonate each other.
pub fn runtime_path(db: &std::path::Path, kind: &str) -> PathBuf {
    let db = db.canonicalize().unwrap_or_else(|_| {
        if db.is_absolute() {
            db.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(db)
        }
    });
    // In particular, fault tests put runtime records outside the disposable
    // index filesystem so ENOSPC/read-only errors cannot suppress health.
    if let Some(directory) = std::env::var_os("DUX_RUNTIME_DIR") {
        use std::os::unix::ffi::OsStrExt;
        let mut hash = 0xcbf29ce484222325u64;
        for byte in db.as_os_str().as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        return PathBuf::from(directory).join(format!("{hash:016x}.{kind}"));
    }
    if db == std::path::Path::new("/var/lib/dux/dux.db") {
        PathBuf::from("/run/dux").join(kind)
    } else {
        let mut path = db.into_os_string();
        path.push(format!(".{kind}"));
        PathBuf::from(path)
    }
}

/// Stable inode incarnation when the filesystem exposes birth time. Unlike
/// ctime, this does not change on writes, renames or hardlink operations.
pub fn inode_birth_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

#[repr(C)]
struct InodeStatxTime {
    tv_sec: i64,
    tv_nsec: u32,
    _reserved: i32,
}

// Linux UAPI linux/stat.h. libc's musl bindings do not expose this structure;
// the kernel syscall uses the same fixed 256-byte layout with either libc.
#[repr(C)]
struct InodeStatx {
    stx_mask: u32,
    _blksize: u32,
    _attributes: u64,
    _nlink: u32,
    _uid: u32,
    _gid: u32,
    _mode: u16,
    _spare0: u16,
    stx_ino: u64,
    _size: u64,
    _blocks: u64,
    _attributes_mask: u64,
    _atime: InodeStatxTime,
    stx_btime: InodeStatxTime,
    _ctime: InodeStatxTime,
    _mtime: InodeStatxTime,
    _rdev_major: u32,
    _rdev_minor: u32,
    stx_dev_major: u32,
    stx_dev_minor: u32,
    stx_mnt_id: u64,
    _reserved: [u64; 13],
}
const _: () = {
    assert!(std::mem::size_of::<InodeStatx>() == 256);
    assert!(std::mem::offset_of!(InodeStatx, stx_btime) == 80);
    assert!(std::mem::offset_of!(InodeStatx, stx_dev_major) == 136);
    assert!(std::mem::offset_of!(InodeStatx, stx_mnt_id) == 144);
};
const STATX_BIRTH: u32 = 0x800;
const STATX_INODE: u32 = 0x100;
const STATX_MNT_ID: u32 = 0x1000;

/// Return the available incarnation birth time and mount ID from one identity-
/// checked statx call. Scan staging needs both values for v7 path instances; a
/// combined request avoids two path lookups on filesystems where Rust metadata
/// does not expose birth time.
pub fn inode_path_metadata_at(
    metadata: &std::fs::Metadata,
    path: &std::path::Path,
) -> (i64, Option<i64>) {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let cached_birth = inode_birth_ns(metadata);
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return (cached_birth, None);
    };
    let mut stat = std::mem::MaybeUninit::<InodeStatx>::zeroed();
    // SAFETY: statx receives a valid NUL-terminated path and writable output
    // storage sized as InodeStatx; syscall success is checked before reading it.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
            STATX_INODE | STATX_BIRTH | STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return (cached_birth, None);
    }
    // SAFETY: a successful statx call initialized the requested output structure.
    let stat = unsafe { stat.assume_init() };
    if stat.stx_mask & STATX_INODE == 0
        || stat.stx_ino != metadata.ino()
        || libc::makedev(stat.stx_dev_major, stat.stx_dev_minor) != metadata.dev()
    {
        return (cached_birth, None);
    }
    let birth_ns = if stat.stx_mask & STATX_BIRTH != 0 {
        stat.stx_btime
            .tv_sec
            .checked_mul(1_000_000_000)
            .and_then(|seconds| seconds.checked_add(stat.stx_btime.tv_nsec as i64))
            .unwrap_or(0)
    } else {
        cached_birth
    };
    let mount_id = (stat.stx_mask & STATX_MNT_ID != 0)
        .then(|| i64::try_from(stat.stx_mnt_id).ok())
        .flatten();
    (birth_ns, mount_id)
}

/// musl's std::fs::Metadata may omit birth time even on ext4. Ask Linux statx
/// directly in that case, and reject a path that now identifies another inode.
pub fn inode_birth_ns_at(metadata: &std::fs::Metadata, path: &std::path::Path) -> i64 {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let cached = inode_birth_ns(metadata);
    if cached != 0 {
        return cached;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    let mut stat = std::mem::MaybeUninit::<InodeStatx>::zeroed();
    // SAFETY: statx receives a valid NUL-terminated path and writable output
    // storage sized as InodeStatx; syscall success is checked before reading it.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
            STATX_INODE | STATX_BIRTH,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return 0;
    }
    // SAFETY: a successful statx call initialized the requested output structure.
    let stat = unsafe { stat.assume_init() };
    if stat.stx_mask & STATX_BIRTH == 0
        || stat.stx_ino != metadata.ino()
        || libc::makedev(stat.stx_dev_major, stat.stx_dev_minor) != metadata.dev()
    {
        return 0;
    }
    stat.stx_btime
        .tv_sec
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(stat.stx_btime.tv_nsec as i64))
        .unwrap_or(0)
}

/// A reader sees either the previous complete record or the next one, never an
/// empty/truncated heartbeat during concurrent publication.
fn publish_runtime(path: &std::path::Path, body: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = PathBuf::from(temp);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(&temp)?;
    if let Err(error) = file.write_all(body.as_ref()) {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn warn_runtime_publish_failure(path: &std::path::Path, error: &std::io::Error) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_WARNING_AT: AtomicU64 = AtomicU64::new(0);
    let now = now_secs().max(0) as u64;
    let previous = LAST_WARNING_AT.load(Ordering::Relaxed);
    if now.saturating_sub(previous) >= 60
        && LAST_WARNING_AT
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::warn!(path = %path.display(), %error, "failed to publish Dux runtime health record");
    }
}

/// Live progress of an in-flight scan.
#[derive(Clone)]
pub struct ScanProgress {
    pub started: i64, // epoch seconds the scan began
    pub files: u64,
    pub dirs: u64,
    pub bytes: i64,
    pub indexing: bool, // false = still walking, true = building the index
    /// True when no prior indexed baseline exists. Readers must not open the
    /// placeholder database while its first atomic publication is in progress.
    pub initial_baseline: bool,
}

/// Publish scan progress (best-effort; format: "<started> <files> <dirs> <bytes> <indexing>").
pub fn write_scan_progress(
    db: &std::path::Path,
    started: i64,
    files: u64,
    dirs: u64,
    bytes: i64,
    indexing: bool,
) {
    let initial_baseline =
        read_scan_progress_for(db).is_some_and(|progress| progress.initial_baseline);
    write_scan_progress_with_baseline(db, started, files, dirs, bytes, indexing, initial_baseline);
}

/// Publish progress while explicitly retaining whether this scan is creating
/// the first usable index. Older progress records without the final field are
/// treated as non-initial for compatibility.
pub fn write_scan_progress_with_baseline(
    db: &std::path::Path,
    started: i64,
    files: u64,
    dirs: u64,
    bytes: i64,
    indexing: bool,
    initial_baseline: bool,
) {
    let _ = std::fs::create_dir_all("/run/dux");
    let progress_path = runtime_path(db, "scan.progress");
    if let Err(error) = publish_runtime(
        &progress_path,
        format!(
            "{started} {files} {dirs} {bytes} {} {}",
            indexing as u8, initial_baseline as u8
        ),
    ) {
        warn_runtime_publish_failure(&progress_path, &error);
    }
    refresh_owned_heartbeat(db);
    systemd_notify("WATCHDOG=1");
}

/// A daemon-triggered atomic rebuild runs inside the daemon process. Keep its
/// existing per-DB heartbeat fresh while the scan loop is busy so CLI/TUI do not
/// incorrectly report "daemon off" during a long reconciliation.
pub(crate) fn refresh_owned_heartbeat(index: &std::path::Path) {
    let Some((_, pid, db)) = read_heartbeat_for(index) else {
        return;
    };
    if pid != std::process::id() as i32 {
        return; // a manual scan must never impersonate another daemon
    }
    let heartbeat_path = runtime_path(std::path::Path::new(&db), "heartbeat");
    if let Err(error) = publish_runtime(
        &heartbeat_path,
        format!("{} {} {db}", now_secs(), std::process::id()),
    ) {
        warn_runtime_publish_failure(&heartbeat_path, &error);
    }
}

/// Remove the scan-progress file (scan finished).
pub fn clear_scan_progress(db: &std::path::Path) {
    let _ = std::fs::remove_file(runtime_path(db, "scan.progress"));
}

/// Read in-flight scan progress, if a scan is running and the file is FRESH
/// (updated within 10s — a crashed scan leaves a stale file we should ignore).
pub fn read_scan_progress_for(db: &std::path::Path) -> Option<ScanProgress> {
    let path = runtime_path(db, "scan.progress");
    let meta = std::fs::metadata(&path).ok()?;
    // Missing or future timestamps are unknown/stale, not proof of a fresh
    // scan. In particular, `SystemTime::elapsed()` errors for future mtimes;
    // defaulting that error to zero would leave a phantom scan visible.
    let age = std::time::SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .ok()?;
    if age > std::time::Duration::from_secs(10) {
        return None; // stale (scan died) — don't report a phantom scan
    }
    let s = std::fs::read_to_string(&path).ok()?;
    let mut it = s.trim().split(' ');
    let started: i64 = it.next()?.parse().ok()?;
    let files: u64 = it.next()?.parse().ok()?;
    let dirs: u64 = it.next()?.parse().ok()?;
    let bytes: i64 = it.next()?.parse().ok()?;
    let indexing = it.next()? == "1";
    let initial_baseline = it.next().is_some_and(|value| value == "1");
    Some(ScanProgress {
        started,
        files,
        dirs,
        bytes,
        indexing,
        initial_baseline,
    })
}

#[derive(Clone, Default)]
pub struct WatchStatus {
    pub updated_at: i64,
    pub last_commit_at: i64,
    pub pending: u64,
    pub kernel_empty: bool,
    pub events_seen: u64,
    pub events_resolved: u64,
    pub updates_committed: u64,
    pub updates_dropped: u64,
    /// Number of FAN_Q_OVERFLOW notifications observed. The kernel does not
    /// report how many individual events were lost; this counts loss episodes.
    /// `None` means the status record predates kernel overflow accounting.
    pub kernel_overflow_notifications: Option<u64>,
    /// `true` when fanotify records are hints only and path resolution is not
    /// attempted for individual events.
    pub scan_first: bool,
    pub behind_ms: u64,
    pub max_pending: u64,
    pub uncertain: bool,
}

const WATCH_STATUS_MAX_AGE_SECS: i64 = 5;

pub(crate) fn timestamp_is_fresh(timestamp: i64, max_age: i64) -> bool {
    now_secs()
        .checked_sub(timestamp)
        .is_some_and(|age| (0..=max_age).contains(&age))
}

fn parse_status_bool(value: Option<&str>) -> Option<bool> {
    match value? {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Publish the current event pipeline in a small, versioned text record.
#[allow(clippy::too_many_arguments)]
pub fn write_watch_status(
    db: &std::path::Path,
    pending: usize,
    kernel_empty: bool,
    events_seen: u64,
    events_resolved: u64,
    updates_committed: u64,
    last_commit_at: i64,
    updates_dropped: u64,
    behind_ms: u64,
    max_pending: usize,
    uncertain: bool,
) {
    write_watch_status_with_overflows(
        db,
        pending,
        kernel_empty,
        events_seen,
        events_resolved,
        updates_committed,
        last_commit_at,
        updates_dropped,
        behind_ms,
        max_pending,
        0,
        uncertain,
    );
}

/// Publish watch status including explicit kernel queue-overflow episodes.
/// Kept separate from `updates_dropped`, which counts known userspace backlog
/// entries discarded to enforce the memory bound.
#[allow(clippy::too_many_arguments)]
pub fn write_watch_status_with_overflows(
    db: &std::path::Path,
    pending: usize,
    kernel_empty: bool,
    events_seen: u64,
    events_resolved: u64,
    updates_committed: u64,
    last_commit_at: i64,
    updates_dropped: u64,
    behind_ms: u64,
    max_pending: usize,
    kernel_overflow_notifications: u64,
    uncertain: bool,
) {
    write_watch_status_with_mode(
        db,
        pending,
        kernel_empty,
        events_seen,
        events_resolved,
        updates_committed,
        last_commit_at,
        updates_dropped,
        behind_ms,
        max_pending,
        kernel_overflow_notifications,
        uncertain,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn write_watch_status_with_mode(
    db: &std::path::Path,
    pending: usize,
    kernel_empty: bool,
    events_seen: u64,
    events_resolved: u64,
    updates_committed: u64,
    last_commit_at: i64,
    updates_dropped: u64,
    behind_ms: u64,
    max_pending: usize,
    kernel_overflow_notifications: u64,
    uncertain: bool,
    scan_first: bool,
) {
    let _ = std::fs::create_dir_all("/run/dux");
    let db = db
        .canonicalize()
        .unwrap_or_else(|_| db.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let now = now_secs();
    let body = format!(
        "5 {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n{}",
        now,
        std::process::id(),
        pending,
        kernel_empty as u8,
        events_seen,
        events_resolved,
        updates_committed,
        updates_dropped,
        behind_ms,
        max_pending,
        uncertain as u8,
        last_commit_at,
        kernel_overflow_notifications,
        scan_first as u8,
        db,
    );
    let status_path = runtime_path(std::path::Path::new(&db), "watch.status");
    if let Err(error) = publish_runtime(&status_path, body) {
        warn_runtime_publish_failure(&status_path, &error);
    }
}

/// Read live pipeline metrics only when fresh and belonging to this database.
pub fn read_watch_status(db: &std::path::Path) -> Option<WatchStatus> {
    let s = std::fs::read_to_string(runtime_path(db, "watch.status")).ok()?;
    let (head, status_db) = s.split_once('\n')?;
    let mut f = head.split_whitespace();
    let version = f.next()?;
    if !matches!(version, "1" | "2" | "3" | "4" | "5") {
        return None;
    }
    let updated_at: i64 = f.next()?.parse().ok()?;
    let pid: i32 = f.next()?.parse().ok()?;
    let pending = f.next()?.parse().ok()?;
    let kernel_empty = parse_status_bool(f.next())?;
    let events_seen = f.next()?.parse().ok()?;
    let events_resolved = f.next()?.parse().ok()?;
    let updates_committed = f.next()?.parse().ok()?;
    let updates_dropped = f.next()?.parse().ok()?;
    let behind_ms = f.next()?.parse().ok()?;
    let max_pending = f.next()?.parse().ok()?;
    let uncertain = if matches!(version, "2" | "3" | "4" | "5") {
        parse_status_bool(f.next())?
    } else {
        false
    };
    let last_commit_at = if matches!(version, "3" | "4" | "5") {
        f.next()?.parse().ok()?
    } else {
        0
    };
    let kernel_overflow_notifications = if matches!(version, "4" | "5") {
        Some(f.next()?.parse().ok()?)
    } else {
        None
    };
    let scan_first = if version == "5" {
        parse_status_bool(f.next())?
    } else {
        false
    };
    if f.next().is_some() {
        return None;
    }
    if !timestamp_is_fresh(updated_at, WATCH_STATUS_MAX_AGE_SECS) || !process_alive(pid) {
        return None;
    }
    let want = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
    if std::path::Path::new(status_db.trim()) != want.as_path() {
        return None;
    }
    Some(WatchStatus {
        updated_at,
        last_commit_at,
        pending,
        kernel_empty,
        events_seen,
        events_resolved,
        updates_committed,
        updates_dropped,
        kernel_overflow_notifications,
        scan_first,
        behind_ms,
        max_pending,
        uncertain,
    })
}

/// Stamp the heartbeat file with the current epoch seconds, the daemon's PID,
/// and the absolute path of the DB it's writing (best-effort). The PID lets
/// `dux scan` signal the daemon to rescan itself instead of telling the user to
/// stop/start it by hand. Format: "<secs> <pid> <db>".
pub fn write_heartbeat(db: &std::path::Path) {
    let _ = std::fs::create_dir_all("/run/dux");
    let db = db
        .canonicalize()
        .unwrap_or_else(|_| db.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let heartbeat_path = runtime_path(std::path::Path::new(&db), "heartbeat");
    if let Err(error) = publish_runtime(
        &heartbeat_path,
        format!("{} {} {db}", now_secs(), std::process::id()),
    ) {
        warn_runtime_publish_failure(&heartbeat_path, &error);
    }
    systemd_notify("WATCHDOG=1");
}

/// (epoch, pid, db_path) of the last heartbeat. Tolerates the older "<secs> <db>"
/// format (pid = 0) so an upgrade-in-place still reads cleanly.
pub fn read_heartbeat_for(db: &std::path::Path) -> Option<(i64, i32, String)> {
    let s = std::fs::read_to_string(runtime_path(db, "heartbeat")).ok()?;
    let s = s.trim();
    // first token = secs; if the second token is a bare integer it's the pid and
    // the remainder is the db path; otherwise the remainder (old format) is the db.
    let (secs_str, rest) = s.split_once(' ')?;
    let secs: i64 = secs_str.trim().parse().ok()?;
    match rest.split_once(' ') {
        Some((pid_str, db)) if pid_str.parse::<i32>().is_ok() => {
            Some((secs, pid_str.parse().unwrap_or(0), db.trim().to_string()))
        }
        _ => Some((secs, 0, rest.trim().to_string())), // legacy "<secs> <db>"
    }
}

pub fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 takes only the validated PID and performs no
    // signal delivery; it is used as an existence/permission probe.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn daemon_live_for(db: &std::path::Path) -> bool {
    match read_heartbeat_for(db) {
        Some((secs, pid, hbdb)) => {
            let fresh = timestamp_is_fresh(secs, 30);
            let want = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
            fresh && process_alive(pid) && std::path::Path::new(&hbdb) == want.as_path()
        }
        None => false,
    }
}

/// Parse durations like "1h", "30m", "24h", "7d", "90s" into seconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| c.is_alphabetic())
            .context("duration needs a unit, e.g. 1h, 30m, 7d")?,
    );
    let n: i64 = num.trim().parse().context("invalid duration number")?;
    if n < 0 {
        anyhow::bail!("duration must not be negative");
    }
    let mult: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        other => anyhow::bail!("unknown duration unit: {other}"),
    };
    n.checked_mul(mult).context("duration too large")
}

pub fn human(bytes: i64) -> String {
    use humansize::{format_size, BINARY};
    format_size(bytes.max(0) as u64, BINARY)
}

/// Format the magnitude of a signed delta without overflowing on `i64::MIN`.
pub fn human_magnitude(bytes: i64) -> String {
    use humansize::{format_size, BINARY};
    format_size(bytes.unsigned_abs(), BINARY)
}

/// Ceiling division for non-negative byte counts. Avoids `bytes + unit - 1`,
/// which overflows for valid values near `i64::MAX`.
pub fn ceil_div_nonnegative(bytes: i64, unit: i64) -> i64 {
    if bytes <= 0 || unit <= 0 {
        return 0;
    }
    bytes / unit + i64::from(bytes % unit != 0)
}

/// Render a raw filename (bytes) for safe terminal display. Preserve valid UTF-8,
/// escape invalid bytes and literal backslashes distinctly, and escape controls.
/// A local user can otherwise craft a filename containing newlines or ANSI/OSC
/// escape sequences (e.g. OSC 52 clipboard writes) that forge or hijack an admin's
/// terminal. Byte escapes also keep different Linux filenames distinguishable.
pub fn display_name(raw: &[u8]) -> String {
    display_path_bytes(raw)
}

/// Render a raw absolute path without collapsing distinct non-UTF-8 components.
/// Slash bytes remain path separators; invalid bytes and literal backslashes are
/// escaped so the display form remains unambiguous to a human reader.
pub fn display_path_bytes(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut offset = 0;
    while offset < raw.len() {
        match std::str::from_utf8(&raw[offset..]) {
            Ok(valid) => {
                append_display_text(&mut out, valid);
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    // `from_utf8` reported this prefix as valid.
                    let valid = std::str::from_utf8(&raw[offset..offset + valid_len])
                        .expect("UTF-8 validator accepted prefix");
                    append_display_text(&mut out, valid);
                    offset += valid_len;
                }
                let invalid_len = error.error_len().unwrap_or(raw.len() - offset);
                for byte in &raw[offset..offset + invalid_len] {
                    out.push_str(&format!("\\x{byte:02x}"));
                }
                offset += invalid_len;
            }
        }
    }
    out
}

fn append_display_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
}

/// Same escaping for an already-decoded path string (CLI output paths).
pub fn display_path(s: &str) -> String {
    escape_controls(s)
}

fn escape_controls(s: &str) -> String {
    // Fast path: most paths have no control characters.
    if !s.chars().any(char::is_control) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Human-readable "time ago" — coarse (no ticking seconds).
pub fn ago(secs: i64) -> String {
    let d = now_secs().saturating_sub(secs).max(0);
    if d < 60 {
        "now".to_string()
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_path_display_keeps_invalid_names_distinct_and_controls_safe() {
        assert_eq!(display_name(b"\xff"), r"\xff");
        assert_eq!(display_name(b"\xfe"), r"\xfe");
        assert_eq!(display_name("�".as_bytes()), "�");
        assert_eq!(display_name(br"\xff"), r"\\xff");
        assert_eq!(display_name(b"a\n\x1b"), r"a\n\x1b");
        assert_eq!(display_name(b"\x85"), r"\x85");
        assert_eq!(display_name("\u{85}".as_bytes()), r"\u{85}");
        assert_eq!(display_path_bytes(b"/bad/\xff"), r"/bad/\xff");
        assert_eq!(display_path("\u{85}"), r"\u{85}");
    }

    #[test]
    fn scan_progress_rejects_future_mtime_instead_of_treating_it_as_fresh() {
        let db = std::env::temp_dir().join(format!(
            "dux-scan-progress-future-{}.db",
            std::process::id()
        ));
        let progress_path = runtime_path(&db, "scan.progress");
        std::fs::write(&progress_path, "123 1 1 4096 0").unwrap();
        assert!(read_scan_progress_for(&db).is_some());

        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&progress_path)
            .unwrap();
        let future_mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        file.set_times(std::fs::FileTimes::new().set_modified(future_mtime))
            .unwrap();
        assert!(
            read_scan_progress_for(&db).is_none(),
            "future scan-progress timestamp must fail closed"
        );
        let _ = std::fs::remove_file(progress_path);
    }

    #[test]
    fn scan_progress_preserves_initial_baseline_and_reads_legacy_records() {
        let db = std::env::temp_dir().join(format!(
            "dux-scan-progress-initial-{}.db",
            std::process::id()
        ));
        let progress_path = runtime_path(&db, "scan.progress");
        std::fs::write(&progress_path, "123 1 1 4096 0").unwrap();
        assert!(!read_scan_progress_for(&db).unwrap().initial_baseline);

        write_scan_progress_with_baseline(&db, now_secs(), 1, 1, 4096, false, true);
        assert!(read_scan_progress_for(&db).unwrap().initial_baseline);
        write_scan_progress(&db, now_secs(), 2, 1, 8192, true);
        let progress = read_scan_progress_for(&db).unwrap();
        assert!(progress.indexing);
        assert!(progress.initial_baseline);

        let _ = std::fs::remove_file(progress_path);
    }

    #[test]
    fn metadata_lookup_distinguishes_missing_paths_from_inspection_errors() {
        let blocker =
            std::env::temp_dir().join(format!("dux-metadata-blocker-{}", std::process::id()));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();
        assert!(metadata_if_exists(&blocker.join("dux.db")).is_err());
        assert!(metadata_if_exists(&blocker.join("dux.db"))
            .unwrap_err()
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotADirectory));
        std::fs::remove_file(&blocker).unwrap();

        let missing =
            std::env::temp_dir().join(format!("dux-metadata-missing-{}", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        assert!(metadata_if_exists(&missing).unwrap().is_none());
    }

    #[test]
    fn parent_directory_creation_preserves_errors_and_accepts_relative_paths() {
        let blocker =
            std::env::temp_dir().join(format!("dux-parent-blocker-{}", std::process::id()));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();
        let error = ensure_parent_dir(&blocker.join("dux.db")).unwrap_err();
        assert!(
            error.to_string().contains("creating parent directory"),
            "{error:#}"
        );
        assert!(
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists),
            "{error:#}"
        );
        std::fs::remove_file(&blocker).unwrap();
        assert!(ensure_parent_dir(Path::new("dux-relative.db")).is_ok());
    }

    #[test]
    fn runtime_record_publication_returns_filesystem_failures() {
        let blocker =
            std::env::temp_dir().join(format!("dux-runtime-blocker-{}", std::process::id()));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();
        let record = blocker.join("watch.status");
        let error = publish_runtime(&record, b"status").unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::NotADirectory | std::io::ErrorKind::AlreadyExists
            ),
            "unexpected publication error: {error}"
        );
        std::fs::remove_file(blocker).unwrap();
    }

    #[test]
    fn checked_fs_stat_preserves_stat_errors() {
        let missing = std::env::temp_dir().join(format!("dux-missing-fs-{}", std::process::id()));
        let err = fs_stat_checked(&missing).unwrap_err();
        assert!(err.to_string().contains("statvfs"));
        assert!(fs_stat(&missing).is_none());
        assert!(fs_stat_checked(std::path::Path::new("/")).is_ok());
    }

    #[test]
    fn checked_fs_stat_rejects_unrepresentable_or_inconsistent_counts() {
        assert!(fs_stat_from_counts(u64::MAX, 0, 0, 4096, 0, 0)
            .unwrap_err()
            .to_string()
            .contains("total capacity exceeds supported range"));
        assert!(fs_stat_from_counts(10, 11, 0, 512, 0, 0)
            .unwrap_err()
            .to_string()
            .contains("free capacity exceeds total capacity"));
        assert!(fs_stat_from_counts(10, 1, 11, 512, 0, 0)
            .unwrap_err()
            .to_string()
            .contains("available capacity exceeds total capacity"));
        assert!(fs_stat_from_counts(1, 0, 0, 512, 1, 2)
            .unwrap_err()
            .to_string()
            .contains("free inode capacity exceeds total"));
        assert!(fs_stat_from_counts(10, 1, 1, 512, u64::MAX, 0)
            .unwrap_err()
            .to_string()
            .contains("inode capacity exceeds supported range"));
    }

    #[test]
    fn checked_fs_stat_uses_checked_capacity_arithmetic() {
        let stats = fs_stat_from_counts(10, 2, 1, 512, 100, 25).unwrap();
        assert_eq!(stats.total, 5120);
        assert_eq!(stats.free, 1024);
        assert_eq!(stats.avail, 512);
        assert_eq!(stats.used, 4096);
        assert_eq!(stats.inodes_total, 100);
        assert_eq!(stats.inodes_free, 25);
        assert_eq!(stats.inodes_used, 75);
        assert!((stats.use_pct() - 88.8888888889).abs() < 0.000001);

        let near_limit = FsStat {
            used: i64::MAX,
            avail: i64::MAX,
            ..FsStat::default()
        };
        assert!((near_limit.use_pct() - 50.0).abs() < 0.000001);
    }

    #[test]
    fn allocated_block_conversion_rejects_overflow() {
        assert_eq!(allocated_bytes_from_blocks(2).unwrap(), 1024);
        assert!(allocated_bytes_from_blocks(u64::MAX)
            .unwrap_err()
            .to_string()
            .contains("allocated file size exceeds supported range"));
    }

    #[test]
    fn indexed_identity_rejects_unsigned_ids_outside_sqlite_range() {
        assert_eq!(
            indexed_identity_from_values(i64::MAX as u64, 1).unwrap(),
            (i64::MAX, 1)
        );
        assert!(indexed_identity_from_values((i64::MAX as u64) + 1, 1).is_err());
        assert!(indexed_identity_from_values(1, (i64::MAX as u64) + 1).is_err());
    }

    #[test]
    fn byte_formatting_handles_signed_and_rounding_boundaries() {
        assert_ne!(human_magnitude(i64::MIN), human(0));
        assert_eq!(ceil_div_nonnegative(0, 1024), 0);
        assert_eq!(ceil_div_nonnegative(1024, 1024), 1);
        assert_eq!(ceil_div_nonnegative(1025, 1024), 2);
        assert_eq!(ceil_div_nonnegative(i64::MAX, 1024), i64::MAX / 1024 + 1);
        assert_eq!(ceil_div_nonnegative(-1, 1024), 0);
    }

    #[test]
    fn recognizes_only_dux_index_artifacts() {
        let db = std::path::Path::new("/var/lib/dux/dux.db");
        for path in [
            "/var/lib/dux/dux.db",
            "/var/lib/dux/dux.db-wal",
            "/var/lib/dux/dux.db-shm",
            "/var/lib/dux/dux.db.new",
            "/var/lib/dux/dux.db.new.scan-123-456.tmp",
        ] {
            assert!(is_index_artifact(std::path::Path::new(path), db), "{path}");
        }
        assert!(!is_index_artifact(
            std::path::Path::new("/var/lib/dux/dux.db.backup"),
            db
        ));
        assert!(!is_index_artifact(
            std::path::Path::new("/var/lib/app/data.db"),
            db
        ));
    }

    #[test]
    fn statx_mount_id_checks_identity_and_handles_missing_paths() {
        let path = std::path::Path::new("/");
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let (birth_ns, mount_id) = inode_path_metadata_at(&metadata, path);
        let mount_id =
            mount_id.expect("supported Linux kernels must expose STATX_MNT_ID for the root mount");
        assert!(mount_id >= 0);
        assert_eq!(birth_ns, inode_birth_ns_at(&metadata, path));
        let other = std::path::Path::new("/etc/passwd");
        let other_metadata = std::fs::symlink_metadata(other).unwrap();
        assert_eq!(inode_path_metadata_at(&metadata, other).1, None);
        assert!(inode_path_metadata_at(&other_metadata, other).1.is_some());
        assert_eq!(
            inode_path_metadata_at(
                &metadata,
                std::path::Path::new("/dux-missing-mount-id-test")
            )
            .1,
            None
        );
    }

    #[test]
    fn independent_indexes_have_independent_atomic_heartbeats() {
        let first =
            std::env::temp_dir().join(format!("dux-heartbeat-first-{}", std::process::id()));
        let second = first.with_extension("second");
        write_heartbeat(&first);
        assert!(daemon_live_for(&first));
        assert!(!daemon_live_for(&second));
        assert_ne!(
            runtime_path(&first, "heartbeat"),
            runtime_path(&second, "heartbeat")
        );
        for _ in 0..20 {
            write_heartbeat(&first);
            assert!(read_heartbeat_for(&first).is_some());
        }
        let _ = std::fs::remove_file(runtime_path(&first, "heartbeat"));
    }
    #[test]
    fn volatile_uncertainty_is_reported_and_legacy_status_remains_readable() {
        let db = std::env::temp_dir().join(format!("dux-uncertain-{}", std::process::id()));
        write_watch_status_with_overflows(&db, 7, true, 100, 90, 30, 0, 5, 500, 500000, 3, true);
        let status = read_watch_status(&db).unwrap();
        assert!(status.uncertain);
        assert_eq!(status.pending, 7);
        assert_eq!(status.updates_dropped, 5);
        assert_eq!(status.kernel_overflow_notifications, Some(3));
        write_watch_status_with_mode(&db, 0, true, 7, 0, 30, 0, 0, 0, 500000, 0, true, true);
        let scan_first = read_watch_status(&db).unwrap();
        assert!(scan_first.scan_first);
        assert_eq!(scan_first.events_seen, 7);
        assert_eq!(scan_first.events_resolved, 0);
        let stale = format!(
            "3 {} {} 0 1 10 10 10 0 0 500000 0 {}\n{}",
            now_secs() - WATCH_STATUS_MAX_AGE_SECS - 1,
            std::process::id(),
            now_secs(),
            db.display()
        );
        std::fs::write(runtime_path(&db, "watch.status"), stale).unwrap();
        assert!(
            read_watch_status(&db).is_none(),
            "stale status must not look live"
        );
        let legacy = format!(
            "1 {} {} 0 1 10 10 10 0 0 500000\n{}",
            now_secs(),
            std::process::id(),
            db.display()
        );
        std::fs::write(runtime_path(&db, "watch.status"), legacy).unwrap();
        assert!(!read_watch_status(&db).unwrap().uncertain);
        let _ = std::fs::remove_file(runtime_path(&db, "watch.status"));
    }

    #[test]
    fn malformed_watch_status_booleans_and_trailing_fields_are_rejected() {
        let db = std::env::temp_dir().join(format!("dux-invalid-watch-{}", std::process::id()));
        write_watch_status_with_overflows(&db, 0, true, 10, 10, 5, now_secs(), 0, 0, 100, 0, false);
        let status_path = runtime_path(&db, "watch.status");
        let valid = std::fs::read_to_string(&status_path).unwrap();
        assert!(read_watch_status(&db).is_some());

        let (head, body) = valid.split_once('\n').unwrap();
        let fields: Vec<_> = head.split_whitespace().collect();
        for (index, value) in [(4, "2"), (11, "invalid")] {
            let mut malformed = fields.clone();
            malformed[index] = value;
            std::fs::write(&status_path, format!("{}\n{body}", malformed.join(" "))).unwrap();
            assert!(
                read_watch_status(&db).is_none(),
                "accepted field {index}={value}"
            );
        }

        std::fs::write(&status_path, format!("{head} unexpected\n{body}")).unwrap();
        assert!(
            read_watch_status(&db).is_none(),
            "accepted trailing status fields"
        );
        let _ = std::fs::remove_file(status_path);
    }

    #[test]
    fn extreme_runtime_timestamps_are_stale_without_overflowing() {
        let db = std::env::temp_dir().join(format!("dux-extreme-time-{}", std::process::id()));
        let canonical = db.canonicalize().unwrap_or_else(|_| db.clone());
        let heartbeat_path = runtime_path(&db, "heartbeat");
        std::fs::write(
            &heartbeat_path,
            format!(
                "{} {} {}",
                i64::MIN,
                std::process::id(),
                canonical.display()
            ),
        )
        .unwrap();
        assert!(!daemon_live_for(&db));

        let status_path = runtime_path(&db, "watch.status");
        std::fs::write(
            &status_path,
            format!(
                "4 {} {} 0 1 10 10 10 0 0 500000 0 0 0\n{}",
                i64::MIN,
                std::process::id(),
                canonical.display()
            ),
        )
        .unwrap();
        assert!(read_watch_status(&db).is_none());
        assert!(ago(i64::MIN).ends_with('d'));
        let _ = std::fs::remove_file(heartbeat_path);
        let _ = std::fs::remove_file(status_path);
    }

    #[test]
    fn legacy_v3_watch_status_reports_kernel_overflows_as_unavailable() {
        let db = std::env::temp_dir().join(format!("dux-legacy-watch-{}", std::process::id()));
        let canonical = db.canonicalize().unwrap_or_else(|_| db.clone());
        let body = format!(
            "3 {} {} 7 1 10 9 5 2 500000 1 1 12345\n{}",
            now_secs(),
            std::process::id(),
            canonical.display()
        );
        std::fs::write(runtime_path(&db, "watch.status"), body).unwrap();
        let status = read_watch_status(&db).unwrap();
        assert_eq!(status.pending, 7);
        assert_eq!(status.max_pending, 1);
        assert!(status.uncertain);
        assert_eq!(status.last_commit_at, 12345);
        assert_eq!(status.updates_dropped, 2);
        assert_eq!(status.kernel_overflow_notifications, None);
        let _ = std::fs::remove_file(runtime_path(&db, "watch.status"));
    }
}
