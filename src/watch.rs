use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Set by the SIGTERM/SIGINT handler so the daemon loop can flush pending events
/// and exit cleanly (systemd sends SIGTERM on stop). Async-signal-safe: the
/// handler only stores to this flag.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

// ---- fanotify constants (FID / dirent-event mode) ----
const FAN_CLASS_NOTIF: libc::c_uint = 0x0000_0000;
const FAN_CLOEXEC: libc::c_uint = 0x0000_0001;
const FAN_NONBLOCK: libc::c_uint = 0x0000_0002;
const FAN_UNLIMITED_QUEUE: libc::c_uint = 0x0000_0010;
const FAN_REPORT_DFID_NAME: libc::c_uint = 0x0000_0c00; // parent-dir FID + entry name
const FAN_MARK_ADD: libc::c_uint = 0x0000_0001;
const FAN_MARK_FILESYSTEM: libc::c_uint = 0x0000_0100;

// event mask bits
const FAN_MODIFY: u64 = 0x0000_0002;
const FAN_ATTRIB: u64 = 0x0000_0004; // metadata change (owner, mode, mtime, nlink)
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
const FAN_MOVED_FROM: u64 = 0x0000_0040;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
const FAN_ONDIR: u64 = 0x4000_0000;

const FAN_EVENT_METADATA_LEN: usize = 24;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const MAX_HANDLE_SZ: usize = 128;
const FANOTIFY_METADATA_VERSION: u8 = 3;

#[repr(C)]
struct FanotifyEventMetadata {
    event_len: u32,
    vers: u8,
    reserved: u8,
    metadata_len: u16,
    mask: u64,
    fd: i32,
    pid: i32,
}

#[repr(C)]
struct InfoHeader {
    info_type: u8,
    pad: u8,
    len: u16,
}

#[repr(C)]
struct FileHandle {
    handle_bytes: u32,
    handle_type: i32,
    f_handle: [u8; MAX_HANDLE_SZ],
}

/// What happened to a path within the flush window (latest event wins).
#[derive(Clone, Copy, PartialEq)]
enum Op {
    Upsert,    // create / modify / close-write -> (re)stat and insert/update
    Delete,    // unlink / rmdir                -> remove node + subtree
    MovedFrom, // rename source                 -> relocated away (or moved out)
    MovedTo,   // rename dest                   -> relocate the inode here, keep subtree
}

/// Growth-alert configuration.
pub struct AlertConfig {
    pub threshold: i64,
    pub window: i64,
    pub exec: Option<String>,
    pub debounce: i64,
}

/// Run the watch daemon. Uses fanotify FID mode so creates, deletes, renames,
/// and size-growth are all tracked live — no rescan needed for normal activity.
/// True if the index at `db` is already rooted at `root_canon` (its stored
/// root_dev/root_inode match). Returns true when it can't tell (missing meta /
/// unreadable / unstattable root) so we don't trigger a spurious rebuild — the
/// needs_rebuild check handles the genuinely-broken cases.
fn root_matches(db: &Path, root_canon: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let want = match std::fs::symlink_metadata(root_canon) {
        Ok(m) => (m.dev() as i64, m.ino() as i64),
        Err(_) => return true,
    };
    let store = match Store::open_ro(db) {
        Ok(s) => s,
        Err(_) => return true,
    };
    let rdev: Option<i64> = store
        .get_meta("root_dev")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    let rino: Option<i64> = store
        .get_meta("root_inode")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    match (rdev, rino) {
        (Some(d), Some(i)) => (d, i) == want,
        _ => true,
    }
}

pub fn run_daemon(
    db: &Path,
    root: &Path,
    flush_ms: u64,
    one_file_system: bool,
    alert: Option<AlertConfig>,
) -> Result<()> {
    // Exclusive per-db lock for the daemon's whole lifetime — no second daemon or
    // concurrent scan can write this index (the heartbeat is only advisory). Held
    // until `_lock` drops when run_daemon returns.
    let _lock = crate::util::lock_db(db).context("acquiring daemon db lock")?;
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    // Never start on a missing/incompatible index, OR one indexed for a DIFFERENT
    // root than we're told to watch (that would attach live events to the wrong
    // tree → missing parents, wrong totals). Rebuild atomically in either case;
    // a no-op when the index is current AND already rooted here.
    if Store::needs_rebuild(db) || !root_matches(db, &root_canon) {
        tracing::info!("index missing, schema-incompatible, or rooted elsewhere — rebuilding before watching");
        let opts = crate::scan::ScanOptions {
            one_file_system,
            progress: false,
            ..Default::default()
        };
        crate::scan::rebuild_atomic(db, root, &opts).context("rebuilding index before watch")?;
    }
    let mut store = Store::open_rw(db)?;

    let fan = init_fanotify().context("fanotify_init (need CAP_SYS_ADMIN, kernel >= 5.9)")?;

    // A scan of `/` crosses mount points by default, so the daemon must watch
    // EVERY real filesystem under root (not just one) or live updates miss
    // /home, /var, /boot, etc. We mark each, keyed by fsid, and keep a dir fd
    // per filesystem for open_by_handle_at (the event carries its fsid).
    let mut fsfds: HashMap<(i32, i32), RawFd> = HashMap::new();
    for mp in real_mounts(&root_canon, one_file_system) {
        let fsid = match statfs_fsid(&mp) {
            Some(f) => f,
            None => continue,
        };
        if let std::collections::hash_map::Entry::Vacant(e) = fsfds.entry(fsid) {
            match open_dir(&mp) {
                Ok(fd) if mark_fs(fan, &mp).is_ok() => {
                    e.insert(fd);
                }
                Ok(fd) => unsafe {
                    libc::close(fd);
                },
                Err(_) => {}
            }
        }
    }
    if fsfds.is_empty() {
        unsafe { libc::close(fan) };
        anyhow::bail!(
            "no watchable filesystem found under {}",
            root_canon.display()
        );
    }
    tracing::info!(
        "dux daemon watching {} ({} filesystem(s), FID mode: create/delete/rename/modify, flush {}ms)",
        root.display(),
        fsfds.len(),
        flush_ms
    );
    if let Some(a) = &alert {
        tracing::info!(
            "alerts: >{} growth in {}s{}",
            crate::util::human(a.threshold),
            a.window,
            a.exec
                .as_deref()
                .map(|e| format!(", exec: {e}"))
                .unwrap_or_default()
        );
    }

    // Flush pending events on SIGTERM/SIGINT instead of dropping up to a full
    // flush window on shutdown (systemd stops the daemon with SIGTERM).
    unsafe {
        let h = on_term as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }

    crate::util::write_heartbeat(db);
    let flush_every = Duration::from_millis(flush_ms);
    let mut last_flush = Instant::now();
    let mut last_ckpt = Instant::now();
    let mut last_alert: HashMap<(i64, i64), i64> = HashMap::new();
    let mut pending: HashMap<PathBuf, Op> = HashMap::new();
    let mut buf = [0u8; 1 << 15];

    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            if !pending.is_empty() {
                if let Err(e) = flush(&mut store, &mut pending, db) {
                    tracing::warn!("final flush on shutdown failed (events retried on restart): {e}");
                }
            }
            tracing::info!("dux daemon: received SIGTERM/SIGINT — flushed pending, exiting");
            unsafe { libc::close(fan) };
            return Ok(());
        }
        let n = unsafe { libc::read(fan, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            parse_events(
                &buf[..n as usize],
                &fsfds,
                &root_canon,
                &mut pending,
                &mut store,
            );
        } else if n < 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => {}
                _ => tracing::warn!("fanotify read error: {err}"),
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        if last_flush.elapsed() >= flush_every {
            if !pending.is_empty() {
                if let Err(e) = flush(&mut store, &mut pending, db) {
                    tracing::warn!("flush error: {e}");
                }
                if let Some(cfg) = &alert {
                    if let Err(e) = check_alerts(&store, cfg, &mut last_alert) {
                        tracing::warn!("alert check error: {e}");
                    }
                }
            }
            // Heartbeat EVERY cycle, independent of flush success — a failing
            // flush must not make `daemon_live` read false (which would let a
            // concurrent `dux scan` corrupt the index). Written to tmpfs, so
            // an idle daemon makes no database/WAL writes at all.
            crate::util::write_heartbeat(db);
            // Checkpoint the WAL occasionally with PASSIVE — never every flush and
            // never TRUNCATE: TRUNCATE blocks on a live TUI reader (up to the busy
            // timeout), which stalls the daemon, burns CPU and freezes the heartbeat.
            // PASSIVE reclaims what it can without blocking; ~every 60s is plenty.
            if last_ckpt.elapsed() >= Duration::from_secs(60) {
                let _ = store.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
                last_ckpt = Instant::now();
            }
            last_flush = Instant::now();
        }
    }
}

/// Mountpoints at/under `root` on real filesystems (same set the scanner indexes,
/// i.e. excluding pseudo filesystems). With `one_fs`, only `root` itself.
fn real_mounts(root: &Path, one_fs: bool) -> Vec<PathBuf> {
    if one_fs {
        return vec![root.to_path_buf()];
    }
    let mut out = vec![root.to_path_buf()];
    if let Ok(mi) = std::fs::read_to_string("/proc/self/mountinfo") {
        for line in mi.lines() {
            // fields: ... [4]=mountpoint ... " - " fstype source opts
            let left = match line.split(" - ").next() {
                Some(l) => l,
                None => continue,
            };
            let f: Vec<&str> = left.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let p = unescape_mount(f[4]);
            if (root == Path::new("/") || p == *root || p.starts_with(root))
                && !crate::scan::is_pseudo_fs(&p)
            {
                out.push(p);
            }
        }
    }
    out
}

/// mountinfo octal-escapes space/tab/newline/backslash as \040 etc.
/// Works on raw bytes so non-ASCII (UTF-8) mountpoints survive intact.
fn unescape_mount(s: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() {
            if let Ok(n) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or(""), 8)
            {
                out.push(n);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

/// The filesystem id (matches the fanotify event fsid) for the fs containing path.
fn statfs_fsid(path: &Path) -> Option<(i32, i32)> {
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return None;
    }
    let v = unsafe { s.assume_init() }.f_fsid;
    // fsid_t is an opaque 8-byte struct ({ __val: [i32;2] }); read its two ints.
    let raw: [i32; 2] = unsafe { std::mem::transmute(v) };
    Some((raw[0], raw[1]))
}

fn open_dir(path: &Path) -> Result<RawFd> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

fn init_fanotify() -> Result<RawFd> {
    let fd = unsafe {
        libc::fanotify_init(
            FAN_CLASS_NOTIF
                | FAN_REPORT_DFID_NAME
                | FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_UNLIMITED_QUEUE,
            (libc::O_RDONLY | libc::O_LARGEFILE) as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

fn mark_fs(fan: RawFd, root: &Path) -> Result<()> {
    let cpath = std::ffi::CString::new(root.as_os_str().to_string_lossy().as_bytes())?;
    let mask = FAN_CREATE
        | FAN_DELETE
        | FAN_MOVED_FROM
        | FAN_MOVED_TO
        | FAN_MODIFY
        | FAN_ATTRIB
        | FAN_CLOSE_WRITE
        | FAN_ONDIR;
    let rc = unsafe {
        libc::fanotify_mark(
            fan,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            libc::AT_FDCWD,
            cpath.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Parse a buffer of fanotify events into pending ops keyed by full path.
fn parse_events(
    mut buf: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    root: &Path,
    pending: &mut HashMap<PathBuf, Op>,
    store: &mut Store,
) {
    while buf.len() >= FAN_EVENT_METADATA_LEN {
        let meta: FanotifyEventMetadata =
            unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const FanotifyEventMetadata) };
        // a version mismatch means the struct layout differs from ours — bail
        // rather than misinterpret every subsequent offset.
        if meta.vers != FANOTIFY_METADATA_VERSION {
            break;
        }
        let len = meta.event_len as usize;
        if len < FAN_EVENT_METADATA_LEN || len > buf.len() {
            break;
        }
        let event = &buf[..len];

        if meta.mask & FAN_Q_OVERFLOW != 0 {
            // queue overflowed (rare with UNLIMITED_QUEUE) — mark index dirty
            tracing::warn!("fanotify queue overflow — index may have missed events");
            store.set_meta("dirty_since", &now_secs().to_string()).ok();
        } else if let Some((dir, name)) = resolve_record(event, meta.metadata_len as usize, fsfds) {
            let full = if name.is_empty() || name == "." {
                dir
            } else {
                dir.join(&name)
            };
            if full.starts_with(root) {
                let op = if meta.mask & FAN_DELETE != 0 {
                    Op::Delete
                } else if meta.mask & FAN_MOVED_FROM != 0 {
                    Op::MovedFrom
                } else if meta.mask & FAN_MOVED_TO != 0 {
                    Op::MovedTo
                } else {
                    Op::Upsert
                };
                pending.insert(full, op);
            }
        }

        buf = &buf[len..];
    }
}

/// Find the DFID_NAME info record, resolve the directory via open_by_handle_at,
/// and return (dir_path, entry_name).
fn resolve_record(
    event: &[u8],
    meta_len: usize,
    fsfds: &HashMap<(i32, i32), RawFd>,
) -> Option<(PathBuf, String)> {
    let mut off = meta_len;
    while off + 4 <= event.len() {
        let hdr: InfoHeader =
            unsafe { std::ptr::read_unaligned(event[off..].as_ptr() as *const InfoHeader) };
        let rec_len = hdr.len as usize;
        if rec_len < 4 || off + rec_len > event.len() {
            break;
        }
        if hdr.info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
            let payload = &event[off + 4..off + rec_len]; // fsid(8) + file_handle + name
            return resolve_handle(payload, fsfds);
        }
        off += rec_len;
    }
    None
}

fn resolve_handle(payload: &[u8], fsfds: &HashMap<(i32, i32), RawFd>) -> Option<(PathBuf, String)> {
    if payload.len() < 16 {
        return None;
    }
    // payload: [fsid:8][handle_bytes:4][handle_type:4][f_handle:hb][name...]
    // pick the mount fd for THIS event's filesystem (open_by_handle_at needs an
    // fd on the same superblock as the handle).
    let fsid = (
        i32::from_ne_bytes(payload[0..4].try_into().ok()?),
        i32::from_ne_bytes(payload[4..8].try_into().ok()?),
    );
    let mount_fd = *fsfds.get(&fsid)?;
    let hb = u32::from_ne_bytes(payload[8..12].try_into().ok()?) as usize;
    let ht = i32::from_ne_bytes(payload[12..16].try_into().ok()?);
    if hb == 0 || hb > MAX_HANDLE_SZ || payload.len() < 16 + hb {
        return None;
    }
    let mut fh = FileHandle {
        handle_bytes: hb as u32,
        handle_type: ht,
        f_handle: [0u8; MAX_HANDLE_SZ],
    };
    fh.f_handle[..hb].copy_from_slice(&payload[16..16 + hb]);

    // name follows the handle, NUL-terminated
    let name_bytes = &payload[16 + hb..];
    let name_end = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(name_bytes.len());
    let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();

    let dfd = unsafe {
        libc::syscall(
            libc::SYS_open_by_handle_at,
            mount_fd,
            &mut fh as *mut FileHandle,
            libc::O_PATH | libc::O_CLOEXEC,
        )
    } as i32;
    if dfd < 0 {
        return None;
    }
    let dir = std::fs::read_link(format!("/proc/self/fd/{dfd}")).ok();
    unsafe {
        libc::close(dfd);
    }
    dir.map(|d| (d, name))
}

/// Prime parent of an inode = the parent dir of its block-bearing (prime) dirent.
/// Directory totals roll up this single chain, so the walk is unambiguous.
fn prime_parent(tx: &rusqlite::Transaction, dev: i64, ino: i64) -> Option<(i64, i64)> {
    tx.query_row(
        "SELECT parent_dev, parent_inode FROM dirents
         WHERE dev_id=?1 AND inode=?2 AND prime=1 LIMIT 1",
        params![dev, ino],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .ok()
}

/// Accumulate a (bytes,inodes) delta into every ancestor inode, starting at
/// `(sdev,sino)` and walking prime parents. Coalesced in `anc` and written once
/// at the end of the flush. Depth-guarded against corrupt cycles.
fn accrue(
    tx: &rusqlite::Transaction,
    anc: &mut HashMap<(i64, i64), (i64, i64)>,
    sdev: i64,
    sino: i64,
    dbytes: i64,
    dinodes: i64,
) {
    let (mut cd, mut ci) = (sdev, sino);
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 4096 {
            break;
        }
        let e = anc.entry((cd, ci)).or_insert((0, 0));
        e.0 += dbytes;
        e.1 += dinodes;
        match prime_parent(tx, cd, ci) {
            Some((pd, pi)) if !(pd == cd && pi == ci) => {
                cd = pd;
                ci = pi;
            }
            _ => break,
        }
    }
}

/// Every inode at/under (d,i) following dirent parent→child edges (UNION dedups
/// hardlinks/cycles; depth-guarded).
fn collect_descendants(tx: &rusqlite::Transaction, d: i64, i: i64) -> Result<Vec<(i64, i64)>> {
    let mut stmt = tx.prepare_cached(
        "WITH RECURSIVE sub(d,i,depth) AS (
            SELECT ?1,?2,0
            UNION
            SELECT de.dev_id,de.inode,sub.depth+1 FROM dirents de
            JOIN sub ON de.parent_dev=sub.d AND de.parent_inode=sub.i
            WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode) AND sub.depth<4096
         ) SELECT d,i FROM sub",
    )?;
    let v = stmt
        .query_map(params![d, i], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|x| x.ok())
        .collect();
    Ok(v)
}

/// Delete a whole subtree: every inode at/under (d,i) plus every dirent within or
/// pointing into it. FTS rows follow via the AFTER DELETE trigger.
fn del_subtree(tx: &rusqlite::Transaction, d: i64, i: i64) -> Result<()> {
    for (dd, ii) in collect_descendants(tx, d, i)? {
        tx.execute(
            "DELETE FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dd, ii],
        )?;
        tx.execute(
            "DELETE FROM dirents WHERE dev_id=?1 AND inode=?2",
            params![dd, ii],
        )?;
        tx.execute(
            "DELETE FROM dirents WHERE parent_dev=?1 AND parent_inode=?2",
            params![dd, ii],
        )?;
    }
    Ok(())
}

/// Remove one directory entry (a single link/path). Handles the three cases:
/// last link → drop the inode + its subtree; prime link with others remaining →
/// promote a sibling dirent and move the block attribution; non-prime link →
/// just unlink. Returns the (bytes,inodes) the subtree carried, for growth.
fn unlink_dirent(
    tx: &rusqlite::Transaction,
    anc: &mut HashMap<(i64, i64), (i64, i64)>,
    bucket: i64,
    pdev: i64,
    pino: i64,
    name: &[u8],
) -> Result<()> {
    let de: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT dev_id, inode, prime FROM dirents
             WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
            params![pdev, pino, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (cdev, cino, was_prime) = match de {
        Some(v) => v,
        None => return Ok(()),
    };
    let (crb, cri): (i64, i64) = tx
        .query_row(
            "SELECT recursive_bytes, recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![cdev, cino],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let adj = anc.get(&(cdev, cino)).copied().unwrap_or((0, 0));
    let (eff_rb, eff_ri) = (crb + adj.0, cri + adj.1);

    tx.execute(
        "DELETE FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
        params![pdev, pino, name],
    )?;
    let remaining: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM dirents WHERE dev_id=?1 AND inode=?2",
            params![cdev, cino],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if remaining == 0 {
        // last link gone: remove the inode (and any subtree) and subtract totals
        // from this (prime) parent chain.
        del_subtree(tx, cdev, cino)?;
        accrue(tx, anc, pdev, pino, -eff_rb, -eff_ri);
        if eff_rb != 0 {
            tx.execute(
                "INSERT INTO growth(bucket,dev_id,inode,delta) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(bucket,dev_id,inode) DO UPDATE SET delta=delta+excluded.delta",
                params![bucket, cdev, cino, -eff_rb],
            )?;
        }
    } else if was_prime != 0 {
        // the block-bearing link was removed but the inode lives on through another
        // link: promote a sibling dirent and MOVE the attribution to its chain.
        if let Ok((opdev, opino, oname)) = tx.query_row(
            "SELECT parent_dev, parent_inode, name FROM dirents
                 WHERE dev_id=?1 AND inode=?2 ORDER BY rowid LIMIT 1",
            params![cdev, cino],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            },
        ) {
            tx.execute(
                "UPDATE dirents SET prime=1 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![opdev, opino, oname],
            )?;
            accrue(tx, anc, pdev, pino, -eff_rb, -eff_ri);
            accrue(tx, anc, opdev, opino, eff_rb, eff_ri);
        }
    }
    // non-prime link with others remaining: the dirent delete above is all there is.
    Ok(())
}

/// Growth-history bucket upsert (delta of allocated blocks for an inode).
fn gro(tx: &rusqlite::Transaction, bucket: i64, dev: i64, ino: i64, delta: i64) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO growth(bucket,dev_id,inode,delta) VALUES(?1,?2,?3,?4)
         ON CONFLICT(bucket,dev_id,inode) DO UPDATE SET delta=delta+excluded.delta",
        params![bucket, dev, ino, delta],
    )?;
    Ok(())
}

/// True if making (npdev,npino) the parent of inode (dev,ino) would create a
/// cycle (the proposed new parent is at or under the node being moved).
fn would_cycle(tx: &rusqlite::Transaction, dev: i64, ino: i64, npdev: i64, npino: i64) -> bool {
    let (mut cd, mut ci) = (npdev, npino);
    let mut g = 0;
    loop {
        if cd == dev && ci == ino {
            return true;
        }
        g += 1;
        if g > 4096 {
            return false;
        }
        match prime_parent(tx, cd, ci) {
            Some((pd, pi)) if !(pd == cd && pi == ci) => {
                cd = pd;
                ci = pi;
            }
            _ => return false,
        }
    }
}

/// Create / modify / hardlink a single path, maintaining both tables with prime-
/// dirent block attribution (an inode's blocks are counted exactly once).
#[allow(clippy::too_many_arguments)]
fn upsert_path(
    tx: &rusqlite::Transaction,
    anc: &mut HashMap<(i64, i64), (i64, i64)>,
    bucket: i64,
    m: &std::fs::Metadata,
    dev: i64,
    ino: i64,
    pdev: i64,
    pino: i64,
    name: &[u8],
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let blocks = (m.blocks() as i64) * 512;
    let mtime = m.mtime();
    let uid = m.uid() as i64;
    let is_dir = m.is_dir();
    let kind = kind_of(m);

    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT dev_id, inode FROM dirents
             WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
            params![pdev, pino, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    match existing {
        Some((edev, eino)) if edev == dev && eino == ino => {
            // same inode at the same path: a metadata/size change
            let (eb, erb): (i64, i64) = tx
                .query_row(
                    "SELECT blocks, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((0, 0));
            if is_dir {
                // a directory's OWN allocation can change (entries added/removed);
                // that delta flows into its recursive_bytes and its ancestors'.
                let delta = blocks - eb;
                tx.execute(
                    "UPDATE inodes SET blocks=?3, uid=?4, mtime=?5 WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino, blocks, uid, mtime],
                )?;
                if delta != 0 {
                    accrue(tx, anc, dev, ino, delta, 0);
                    gro(tx, bucket, dev, ino, delta)?;
                }
            } else {
                let delta = blocks - erb;
                tx.execute(
                    "UPDATE inodes SET blocks=?3, recursive_bytes=?3, uid=?4, mtime=?5
                     WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino, blocks, uid, mtime],
                )?;
                if delta != 0 {
                    accrue(tx, anc, pdev, pino, delta, 0);
                    gro(tx, bucket, dev, ino, delta)?;
                }
            }
        }
        other => {
            if other.is_some() {
                // a DIFFERENT inode currently occupies this path (a missed delete):
                // unlink the stale occupant before installing the new one.
                unlink_dirent(tx, anc, bucket, pdev, pino, name)?;
            }
            let inode_exists = tx
                .query_row(
                    "SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino],
                    |_| Ok(()),
                )
                .is_ok();
            if inode_exists {
                // additional HARDLINK: record the path (searchable) but count the
                // inode's blocks only once -> a non-prime dirent, no attribution.
                tx.execute(
                    "INSERT OR REPLACE INTO dirents
                     (parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?1,?2,?3,?4,?5,0)",
                    params![pdev, pino, name, dev, ino],
                )?;
            } else {
                // fresh inode + its first (prime) dirent
                tx.execute(
                    "INSERT OR REPLACE INTO inodes
                     (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                     VALUES(?1,?2,?3,?4,?5,1,?6,?7)",
                    params![dev, ino, kind, blocks, blocks, uid, mtime],
                )?;
                tx.execute(
                    "INSERT OR REPLACE INTO dirents
                     (parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?1,?2,?3,?4,?5,1)",
                    params![pdev, pino, name, dev, ino],
                )?;
                accrue(tx, anc, pdev, pino, blocks, 1);
                gro(tx, bucket, dev, ino, blocks)?;
            }
        }
    }
    Ok(())
}

/// Index the existing contents of a directory that was moved INTO the tree — its
/// descendants produced no per-file events, so a plain move-to would leave the
/// subtree empty. Bounded breadth-first walk; marks the index dirty if the entry
/// budget is exhausted (rather than silently indexing a partial subtree).
fn reconcile_subtree(
    tx: &rusqlite::Transaction,
    anc: &mut HashMap<(i64, i64), (i64, i64)>,
    bucket: i64,
    dir_path: &Path,
    dev: i64,
    ino: i64,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let mut queue: Vec<(PathBuf, i64, i64)> = vec![(dir_path.to_path_buf(), dev, ino)];
    let mut budget = 1_000_000usize;
    while let Some((dir, ddev, dino)) = queue.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            if budget == 0 {
                tracing::warn!(
                    "reconcile_subtree: entry budget exhausted at {}",
                    dir.display()
                );
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES('dirty_since',?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![now_secs().to_string()],
                )?;
                return Ok(());
            }
            budget -= 1;
            let p = ent.path();
            let m = match std::fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let cdev = m.dev() as i64;
            let cino = m.ino() as i64;
            let name = ent.file_name().as_bytes().to_vec();
            upsert_path(tx, anc, bucket, &m, cdev, cino, ddev, dino, &name)?;
            if m.is_dir() {
                queue.push((p, cdev, cino));
            }
        }
    }
    Ok(())
}

/// Apply all pending ops in one transaction, in correctness-preserving phases:
///   B. deletes (deep-first)            — unlink path; drop inode only on last link
///   C. move-from                       — record renames; else unlink (left tree)
///   D. move-to                         — relocate the renamed dirent (keep subtree),
///                                        else add a hardlink path / fresh inode
///   E. upserts (shallow-first)         — create / modify / hardlink
///
/// The whole transaction is ATOMIC: any operation error aborts it via `?`, so
/// `pending` is left intact and retried next flush — a single failed op never
/// commits partial drift.
fn flush(store: &mut Store, pending: &mut HashMap<PathBuf, Op>, db: &Path) -> Result<()> {
    let now = now_secs();
    // COPY, don't drain: if the transaction below fails (disk full, lock, I/O),
    // we return Err with `pending` intact so the events are retried next flush
    // instead of being lost. `pending` is only cleared after a durable commit.
    let items: Vec<(PathBuf, Op)> = pending.iter().map(|(p, op)| (p.clone(), *op)).collect();

    let mut deletes: Vec<&PathBuf> = Vec::new();
    let mut moved_from: Vec<&PathBuf> = Vec::new();
    let mut moved_to: Vec<&PathBuf> = Vec::new();
    let mut upserts: Vec<&PathBuf> = Vec::new();
    for (p, op) in &items {
        match op {
            Op::Delete => deletes.push(p),
            Op::MovedFrom => moved_from.push(p),
            Op::MovedTo => moved_to.push(p),
            Op::Upsert => upserts.push(p),
        }
    }
    let depth = |p: &PathBuf| p.components().count();
    deletes.sort_by_key(|p| std::cmp::Reverse(depth(p))); // children first
    upserts.sort_by_key(|a| depth(a)); // parents first

    // inodes that are being relocated INTO the tree this flush (so a matching
    // move-from is a rename, not a removal)
    let mut moved_in: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    for p in &moved_to {
        if let Some(id) = stat_id(p) {
            moved_in.insert(id);
        }
    }

    tracing::debug!(
        "flush: {} del, {} moved_from, {} moved_to, {} upsert",
        deletes.len(),
        moved_from.len(),
        moved_to.len(),
        upserts.len()
    );

    let tx = store.conn.transaction()?;
    let bucket = now / crate::store::GROWTH_BUCKET_SECS;
    // Ancestor totals are COALESCED: each affected dir's delta is summed here and
    // written ONCE at the end, instead of one UPDATE per changed file.
    let mut anc: HashMap<(i64, i64), (i64, i64)> = HashMap::new();
    // Renames seen this flush: target inode -> the old (parent_dev,parent_inode,name).
    let mut rename_src: HashMap<(i64, i64), (i64, i64, Vec<u8>)> = HashMap::new();

    // ---- Phase B: deletes (unlink one path; drop the inode only on last link) ----
    for full in &deletes {
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (pdev, pino) = match stat_id(dir) {
            Some(v) => v,
            None => continue,
        };
        unlink_dirent(&tx, &mut anc, bucket, pdev, pino, &name)?;
    }

    // ---- Phase C: move-from (record renames; otherwise unlink the left path) ----
    for full in &moved_from {
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (pdev, pino) = match stat_id(dir) {
            Some(v) => v,
            None => continue,
        };
        let de: Option<(i64, i64)> = tx
            .query_row(
                "SELECT dev_id, inode FROM dirents
                 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
                params![pdev, pino, &name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((cdev, cino)) = de {
            if moved_in.contains(&(cdev, cino)) {
                rename_src.insert((cdev, cino), (pdev, pino, name)); // phase D relocates
            } else {
                unlink_dirent(&tx, &mut anc, bucket, pdev, pino, &name)?;
            }
        }
    }

    // ---- Phase D: move-to (relocate a renamed dirent; else add link / fresh) ----
    for full in &moved_to {
        use std::os::unix::fs::MetadataExt;
        let m = match std::fs::symlink_metadata(full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (npdev, npino) = match stat_id(dir) {
            Some(v) => v,
            None => continue,
        };
        let dev = m.dev() as i64;
        let ino = m.ino() as i64;

        if let Some((opdev, opino, oldname)) = rename_src.remove(&(dev, ino)) {
            // RENAME of an existing link. Cycle guard: never move a dir under its
            // own subtree (a stale tree could install A→B→A and hang the CTEs).
            if would_cycle(&tx, dev, ino, npdev, npino) {
                tracing::debug!("skip move-to (would cycle): dev={dev} ino={ino}");
                continue;
            }
            let prime: i64 = tx
                .query_row(
                    "SELECT prime FROM dirents
                     WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
                    params![opdev, opino, &oldname],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            // relocate THIS specific dirent old->new; children keep pointing at
            // the inode, so a directory's whole subtree follows for free.
            tx.execute(
                "UPDATE dirents SET parent_dev=?1, parent_inode=?2, name=?3
                 WHERE parent_dev=?4 AND parent_inode=?5 AND name=?6",
                params![npdev, npino, &name, opdev, opino, &oldname],
            )?;
            if prime != 0 {
                // move the block attribution from the old parent chain to the new.
                let (rb, ri): (i64, i64) = tx
                    .query_row(
                        "SELECT recursive_bytes, recursive_inodes FROM inodes
                         WHERE dev_id=?1 AND inode=?2",
                        params![dev, ino],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap_or((0, 0));
                let adj = anc.get(&(dev, ino)).copied().unwrap_or((0, 0));
                let (eff_rb, eff_ri) = (rb + adj.0, ri + adj.1);
                accrue(&tx, &mut anc, opdev, opino, -eff_rb, -eff_ri);
                accrue(&tx, &mut anc, npdev, npino, eff_rb, eff_ri);
            }
            // a rename has zero net byte delta -> no growth row
        } else {
            // not a rename: a path appeared (moved in from outside, or a new link)
            upsert_path(&tx, &mut anc, bucket, &m, dev, ino, npdev, npino, &name)?;
            // a populated directory moved in carries existing descendants that
            // produced no per-file events — index its current contents.
            if m.is_dir() {
                reconcile_subtree(&tx, &mut anc, bucket, full, dev, ino)?;
            }
        }
    }

    // ---- Phase E: upserts (create / modify / hardlink) ----
    for full in &upserts {
        use std::os::unix::fs::MetadataExt;
        let m = match std::fs::symlink_metadata(full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (pdev, pino) = match stat_id(dir) {
            Some(v) => v,
            None => continue,
        };
        let dev = m.dev() as i64;
        let ino = m.ino() as i64;
        upsert_path(&tx, &mut anc, bucket, &m, dev, ino, pdev, pino, &name)?;
    }

    // ---- apply coalesced ancestor totals: ONE write per affected dir ----
    for (&(d, i), &(rb, ri)) in &anc {
        if rb != 0 || ri != 0 {
            tx.execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes+?3,
                 recursive_inodes=recursive_inodes+?4 WHERE dev_id=?1 AND inode=?2",
                params![d, i, rb, ri],
            )?;
        }
    }
    tx.commit()?;
    // Events are now durably applied — safe to drop them. (Clearing BEFORE the
    // best-effort prune below ensures a prune error can't trigger a re-apply.)
    pending.clear();
    crate::util::write_heartbeat(db);
    // keep ~7 days of growth buckets (cheap: ~288 rows/day per active inode).
    // best-effort: a prune failure must not fail the (already committed) flush.
    let keep_bucket = (now - 7 * 86400) / crate::store::GROWTH_BUCKET_SECS;
    let _ = store
        .conn
        .execute("DELETE FROM growth WHERE bucket < ?1", params![keep_bucket]);
    Ok(())
}

/// (parent_dir, file_name_bytes) from a full path — raw bytes, identity-safe.
fn split(full: &Path) -> Option<(&Path, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt;
    let dir = full.parent()?;
    let name = full.file_name()?.as_bytes().to_vec();
    Some((dir, name))
}

fn kind_of(m: &std::fs::Metadata) -> &'static str {
    if m.is_dir() {
        "d"
    } else if m.file_type().is_symlink() {
        "l"
    } else if m.is_file() {
        "f"
    } else {
        "o"
    }
}

/// Stat a path for its (dev, inode).
fn stat_id(path: &Path) -> Option<(i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev() as i64, m.ino() as i64))
}

/// Fire the alert command for paths whose growth in the window exceeds threshold.
fn check_alerts(
    store: &Store,
    cfg: &AlertConfig,
    last: &mut HashMap<(i64, i64), i64>,
) -> Result<()> {
    let now = now_secs();
    // bucket granularity means the window is rounded up to the next 5 min
    let cutoff = (now - cfg.window) / crate::store::GROWTH_BUCKET_SECS;
    let mut stmt = store.conn.prepare(
        "SELECT dev_id, inode, SUM(delta) d FROM growth
         WHERE bucket >= ?1 GROUP BY dev_id, inode HAVING d >= ?2 ORDER BY d DESC",
    )?;
    let rows = stmt.query_map(params![cutoff, cfg.threshold], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (dev, inode, delta) = row?;
        if let Some(&t) = last.get(&(dev, inode)) {
            if now - t < cfg.debounce {
                continue;
            }
        }
        last.insert((dev, inode), now);
        let path = store
            .path_of(dev, inode)
            .unwrap_or_else(|_| format!("inode:{inode}"));
        tracing::warn!(
            "ALERT: {} grew {} in {}s",
            path,
            crate::util::human(delta),
            cfg.window
        );
        if let Some(cmd) = &cfg.exec {
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("DUX_PATH", &path)
                .env("DUX_DELTA", delta.to_string())
                .env("DUX_DELTA_HUMAN", crate::util::human(delta))
                .env("DUX_WINDOW", cfg.window.to_string())
                .spawn();
        }
    }
    Ok(())
}
