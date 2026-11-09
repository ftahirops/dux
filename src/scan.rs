use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
type ScanTestHook = Arc<dyn Fn(&Path) + Send + Sync>;

/// A directory or child can disappear after `jwalk` enumerates its parent.
/// The resulting `NotFound` means there is no current subtree to index; other
/// traversal failures remain coverage errors and must keep the index dirty.
fn scan_race_is_vanished(error: Option<&std::io::Error>) -> bool {
    error.is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[derive(Default)]
pub struct ScanOptions {
    pub one_file_system: bool,
    pub exclude: Vec<PathBuf>,
    pub low_priority: bool,
    /// Include pseudo-filesystems (/proc, /sys, cgroup, …). Off by default.
    pub include_pseudo: bool,
    /// Print live progress to stderr.
    pub progress: bool,
    /// Cap walker threads. None = auto (all cores, capped 16; or cores/4 under
    /// low_priority so a production box keeps headroom). Some(n) forces n.
    pub jobs: Option<usize>,
    /// Cooperative cancellation for background rebuilds when the host enters
    /// critical pressure. The old live index remains installed on cancellation.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Directories observed as changed. The full walk remains authoritative;
    /// these paths only bias jwalk's directory queue toward affected branches.
    pub priority_paths: Vec<PathBuf>,
    /// Deterministic race injection for scanner tests; never compiled into
    /// production builds.
    #[cfg(test)]
    pub(crate) test_after_entry_stat: Option<ScanTestHook>,
}

fn priority_child_names(
    root: &Path,
    hints: &[PathBuf],
) -> std::collections::HashMap<PathBuf, std::collections::HashSet<std::ffi::OsString>> {
    let mut prioritized = std::collections::HashMap::new();
    for hint in hints.iter().take(256) {
        if !hint.starts_with(root) {
            continue;
        }
        let mut current = hint.as_path();
        while current != root {
            let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                break;
            };
            if !parent.starts_with(root) {
                break;
            }
            prioritized
                .entry(parent.to_path_buf())
                .or_insert_with(std::collections::HashSet::new)
                .insert(name.to_os_string());
            current = parent;
        }
    }
    prioritized
}

fn promote_priority<T>(items: &mut [T], mut is_priority: impl FnMut(&T) -> bool) {
    let mut next_priority = 0;
    for index in 0..items.len() {
        if is_priority(&items[index]) {
            items.swap(next_priority, index);
            next_priority += 1;
        }
    }
}

#[derive(Debug)]
pub struct ScanCancelled;

impl std::fmt::Display for ScanCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scan cancelled by host pressure guard")
    }
}

impl std::error::Error for ScanCancelled {}

fn cancellation_requested(cancel: &Option<Arc<AtomicBool>>) -> bool {
    cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn ensure_not_cancelled(cancel: &Option<Arc<AtomicBool>>) -> Result<()> {
    if cancellation_requested(cancel) {
        Err(ScanCancelled.into())
    } else {
        Ok(())
    }
}

/// Resolve the walker thread count from options + host. Fewer threads = less CPU
/// burned by the (CPU-heavy) parallel stat walk, so a production scan can be told
/// to run slowly in the background.
fn scan_threads(opts: &ScanOptions) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    if let Some(n) = opts.jobs {
        return n.max(1);
    }
    if opts.low_priority {
        (cores / 4).max(1) // leave most of the box for real work
    } else {
        cores.min(16)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryAncestor {
    identity: (i64, i64),
    parent: Option<Arc<DirectoryAncestor>>,
}

fn extend_ancestors(
    ancestors: &Option<Arc<DirectoryAncestor>>,
    identity: (i64, i64),
) -> Option<Arc<DirectoryAncestor>> {
    Some(Arc::new(DirectoryAncestor {
        identity,
        parent: ancestors.clone(),
    }))
}

fn has_ancestor(ancestors: &Option<Arc<DirectoryAncestor>>, identity: (i64, i64)) -> bool {
    let mut current = ancestors.as_deref();
    while let Some(ancestor) = current {
        if ancestor.identity == identity {
            return true;
        }
        current = ancestor.parent.as_deref();
    }
    false
}

/// True if `path` sits on a virtual/pseudo filesystem whose "sizes" are not real
/// disk usage (/proc, /sys, cgroup, devpts, …). /proc/kcore reports the entire
/// address space — counting it is meaningless. Shared with the daemon so both
/// cover exactly the same set of filesystems.
pub(crate) fn is_pseudo_fs(path: &Path) -> bool {
    is_pseudo_magic(fs_magic(path))
}

fn is_pseudo_magic(magic: Option<u64>) -> bool {
    matches!(
        magic,
        Some(
            0x9fa0       // PROC
            | 0x62656572 // SYSFS
            | 0x27e0eb   // CGROUP
            | 0x63677270 // CGROUP2
            | 0x1cd1     // DEVPTS
            | 0x64626720 // DEBUGFS
            | 0x74726163 // TRACEFS
            | 0x73636673 // SECURITYFS
            | 0xcafe4a11 // BPF
            | 0x19800202 // MQUEUE
            | 0x6165676c // PSTORE
            | 0x42494e4d // BINFMTFS
            | 0x9fa2     // USBDEVICE
            | 0x65735543 // FUSECTL
            | 0x62656570 // CONFIGFS
            | 0x65735546 // FUSE (portal/remote userspace view)
            | 0x6e736673 // NSFS (network namespaces)
            | 0x958458f6 // HUGETLBFS
            | 0x858458f6 // RAMFS
            | 0x01021994 // TMPFS
            | 0x0187 // AUTOFS
            | 0x67596969 // RPC_PIPEFS
            | 0xf97cff8c // SELINUXFS
        )
    )
}

/// Overlay mountpoints are merged views backed by upper/lower directories that
/// are already indexed on their real filesystems. Crossing a nested overlay
/// double-counts physical storage and fanotify often cannot mark the merged
/// view. The scan root itself is handled by callers and remains allowed.
pub(crate) fn is_duplicate_storage_view(path: &Path) -> bool {
    matches!(fs_magic(path), Some(0x794c7630)) // OVERLAYFS_SUPER_MAGIC
}

fn fs_magic(path: &Path) -> Option<u64> {
    use std::mem::MaybeUninit;
    let c = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let mut s = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return None;
    }
    // libc exposes `f_type` as a signed long on glibc and an unsigned long on
    // musl. Filesystem magic values are bit patterns, so normalize both ABI
    // representations to the same unsigned value before matching.
    Some(unsafe { s.assume_init() }.f_type as u64)
}

#[derive(Default)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub staged_path_instances: u64,
    pub staged_directory_alias_paths: u64,
    pub staged_root_visible_bytes: i64,
    pub staged_root_visible_entries: u64,
    pub staged_root_unique_bytes: i64,
    pub staged_root_unique_inodes: u64,
    pub bytes: i64,
    /// Total coverage issues. Nonzero prevents a scan from claiming exactness.
    pub errors: u64,
    pub unreadable_entries: u64,
    pub repeated_directory_identities: u64,
    pub cycle_paths: u64,
    pub conflicting_inode_edges: u64,
    pub conflicting_inode_allocations: u64,
    pub conflicting_inode_incarnations: u64,
    pub unknown_repeated_inode_identities: u64,
    /// Staged path entries that disappeared before an inode-reuse conflict
    /// could be validated. These are discarded with their staged subtrees.
    pub vanished_reuse_paths: u64,
    /// Staged non-directory entries whose path now refers to a directly
    /// revalidated inode incarnation after inode-number reuse.
    pub revalidated_reuse_paths: u64,
    /// Candidate paths still ambiguous after direct current-path revalidation.
    pub unresolved_reuse_paths: u64,
}

/// Atomic full rebuild: scan beside `db`, then install through a SQLite backup
/// transaction for existing indexes or a rename for first installation. Readers
/// retain complete snapshots; failed installation leaves the previous database
/// intact (callers hold the exclusive writer lock).
pub fn rebuild_atomic(db: &Path, root: &Path, opts: &ScanOptions) -> Result<ScanStats> {
    rebuild_atomic_with_install_observer(db, root, opts, |_| {})
}

/// Test seam at the point where the fully built staging database is ready to
/// replace the live index. The progress marker must still describe the active
/// publication until this function returns.
fn rebuild_atomic_with_install_observer(
    db: &Path,
    root: &Path,
    opts: &ScanOptions,
    on_install: impl FnOnce(&Path),
) -> Result<ScanStats> {
    ensure_not_cancelled(&opts.cancel)?;
    let _progress_cleanup = ScanProgressCleanup::start(db.to_path_buf());
    // Inspect directory entries without following them before SQLite opens the
    // source. A dangling main-database symlink is an existing install target;
    // sidecar symlinks are rejected by Store's artifact check.
    Store::has_index_artifacts(db)?;
    // Unknown/future/corrupt sources must not silently lose committed history.
    // A first installation has no source; known v4/v5/v6 upgrades preserve it.
    let previous = if Store::has_recoverable_database_state(db)? {
        Some(
            Store::open_rebuild_source(db)
                .context("validating previous index and growth history")?,
        )
    } else {
        None
    };
    let mut new_os = db.to_path_buf().into_os_string();
    new_os.push(".new");
    let db_new = PathBuf::from(new_os);
    cleanup_stale_scan_files(&db_new)?;
    for suf in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suf}", db_new.display()));
    }
    struct StagingIndexGuard(PathBuf);
    impl Drop for StagingIndexGuard {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    let _staging_index_guard = StagingIndexGuard(db_new.clone());
    let stats = {
        let mut store = Store::create_fresh(&db_new)?;
        // This database is private staging state: no reader can observe it, and
        // the previous live database remains untouched until atomic install.
        // Avoid generating a database-sized WAL for the bulk transaction. The
        // completed file is returned to WAL/FULL mode and fsynced before install,
        // so a crash during this build only discards `.new` and cannot corrupt
        // the active index.
        let journal_mode: String = store
            .conn
            .query_row("PRAGMA journal_mode=OFF", [], |row| row.get(0))?;
        anyhow::ensure!(
            journal_mode.eq_ignore_ascii_case("off"),
            "could not disable journaling for private rebuild database"
        );
        store.conn.execute_batch("PRAGMA synchronous=OFF;")?;
        let s = scan_with_progress_policy(&mut store, root, opts, true)?;
        _progress_cleanup.refresh_until_drop();
        if let Some(cancel) = opts.cancel.as_ref() {
            let cancel = Arc::clone(cancel);
            store
                .conn
                .progress_handler(10_000, Some(move || cancel.load(Ordering::Relaxed)))?;
        }
        ensure_not_cancelled(&opts.cancel)?;
        // A full filesystem baseline repairs current state; it must not erase
        // the independent history used by growth reports and alert windows.
        // The daemon pauses index writes while rebuilding, so this is a stable
        // source. Never mix histories from a different indexed root/schema.
        if let Some(previous) = previous.as_ref() {
            if crate::store::indexed_root_path(&previous.conn)?
                == crate::store::indexed_root_path(&store.conn)?
            {
                store.conn.execute(
                    "ATTACH DATABASE ?1 AS previous",
                    params![db.to_str().context("index path is not UTF-8")?],
                )?;
                let copied = (|| -> Result<()> {
                    let tx = store.conn.transaction()?;
                    if previous.schema_version >= 6 {
                        tx.execute_batch("INSERT INTO main.growth(bucket,dev_id,inode,birth_ns,delta) SELECT bucket,dev_id,inode,birth_ns,delta FROM previous.growth;")?;
                    } else {
                        // Pre-v6 bucket rows do not identify an inode incarnation.
                        // Keep them for forensic retention but label them unknown
                        // so growth_current cannot claim them for today's inode.
                        tx.execute_batch("INSERT INTO main.growth(bucket,dev_id,inode,birth_ns,delta) SELECT bucket,dev_id,inode,0,delta FROM previous.growth;")?;
                    }
                    let bucket = now_secs() / crate::store::GROWTH_BUCKET_SECS;
                    // Record net allocation observed by the new baseline. Without
                    // this, a write already seen by the scanner would produce a
                    // zero replay delta and disappear from growth history.
                    if previous.schema_version >= 5 {
                        tx.execute(
                            "INSERT INTO main.growth(bucket,dev_id,inode,birth_ns,delta)
                             SELECT ?1,n.dev_id,n.inode,n.birth_ns,n.blocks-COALESCE(p.blocks,0)
                             FROM main.inodes n LEFT JOIN previous.inodes p
                               ON p.dev_id=n.dev_id AND p.inode=n.inode AND p.birth_ns=n.birth_ns
                             WHERE n.birth_ns!=0 AND n.blocks!=COALESCE(p.blocks,0)
                             ON CONFLICT(bucket,dev_id,inode,birth_ns) DO UPDATE SET delta=delta+excluded.delta",
                            params![bucket])?;
                        tx.execute(
                            "INSERT INTO main.growth(bucket,dev_id,inode,birth_ns,delta)
                             SELECT ?1,p.dev_id,p.inode,p.birth_ns,-p.blocks FROM previous.inodes p
                             WHERE p.birth_ns!=0 AND p.blocks!=0 AND NOT EXISTS(
                               SELECT 1 FROM main.inodes n WHERE n.dev_id=p.dev_id AND n.inode=p.inode AND n.birth_ns=p.birth_ns)
                             ON CONFLICT(bucket,dev_id,inode,birth_ns) DO UPDATE SET delta=delta+excluded.delta",
                            params![bucket])?;
                    }
                    tx.commit()?;
                    Ok(())
                })();
                store.conn.execute_batch("DETACH DATABASE previous;")?;
                copied.context("preserving growth history during reconciliation")?;
            }
        }
        ensure_not_cancelled(&opts.cancel)?;
        // Restore the normal durable mode before the completed file is closed
        // and fsynced. The old database remains the recovery point until then.
        let journal_mode: String = store
            .conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        anyhow::ensure!(
            journal_mode.eq_ignore_ascii_case("wal"),
            "could not restore WAL mode for rebuilt index"
        );
        store
            .conn
            .execute_batch("PRAGMA synchronous=FULL; PRAGMA wal_checkpoint(TRUNCATE);")?;
        ensure_not_cancelled(&opts.cancel)?;
        s
    }; // store dropped here -> connection closed, -wal/-shm removed
       // fsync the finished file before the rename: the rename is namespace-atomic
       // but a crash could otherwise expose a rename to not-yet-durable contents.
    crate::fault::checkpoint("scan.build_complete");
    crate::fault::fail_scan("scan.build_complete")?;
    std::fs::File::open(&db_new)?
        .sync_all()
        .context("sync rebuilt index")?;
    // Once installation starts, finish its atomic transaction even if pressure
    // changes; cancellation before this point leaves the old index untouched.
    ensure_not_cancelled(&opts.cancel)?;
    on_install(db);
    if Store::has_index_artifacts(db)? {
        // Never unlink WAL/SHM underneath readers. SQLite's online backup API
        // installs the rebuilt pages in ONE destination transaction; existing
        // WAL readers keep their snapshot, new readers see the committed rebuild.
        let source =
            Connection::open_with_flags(&db_new, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut target = Connection::open(db)?;
        target.busy_timeout(Duration::from_secs(1))?;
        target.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut target)?;
            crate::fault::checkpoint("scan.install_started");
            let mut blocked_since = None;
            loop {
                match backup.step(crate::fault::backup_step_pages())? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => {
                        crate::fault::checkpoint("scan.install_midway");
                        blocked_since = None;
                    }
                    rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                        let start = blocked_since.get_or_insert_with(Instant::now);
                        anyhow::ensure!(
                            start.elapsed() < Duration::from_secs(30),
                            "index replacement remained locked for 30s"
                        );
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => anyhow::bail!("unexpected SQLite backup result"),
                }
                crate::util::refresh_owned_heartbeat(db);
            }
        }
        crate::fault::checkpoint("scan.install_committed");
        let _ = target.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        drop(target);
        drop(source);
        std::fs::remove_file(&db_new)?;
    } else {
        std::fs::rename(&db_new, db)
            .with_context(|| format!("installing new index at {}", db.display()))?;
    }
    // fsync the parent directory so the rename itself survives a crash.
    if let Some(parent) = db.parent() {
        std::fs::File::open(parent)?
            .sync_all()
            .context("sync index directory")?;
    }
    Ok(stats)
}

struct ScanProgressCleanup {
    db: PathBuf,
    stop: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ScanProgressCleanup {
    fn start(db: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let refresh = Arc::new(AtomicBool::new(false));
        let thread_refresh = Arc::clone(&refresh);
        let thread_db = db.clone();
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if thread_refresh.load(Ordering::Relaxed) {
                    if let Some(progress) = crate::util::read_scan_progress_for(&thread_db) {
                        crate::util::write_scan_progress(
                            &thread_db,
                            progress.started,
                            progress.files,
                            progress.dirs,
                            progress.bytes,
                            progress.indexing,
                        );
                    }
                }
                for _ in 0..10 {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self {
            db,
            stop,
            refresh,
            handle: Some(handle),
        }
    }

    fn refresh_until_drop(&self) {
        self.refresh.store(true, Ordering::Relaxed);
    }
}

impl Drop for ScanProgressCleanup {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        crate::util::clear_scan_progress(&self.db);
    }
}

/// A SIGKILL/power loss cannot run StageGuard. Callers must hold the exclusive
/// per-database lock, so no other scan is active. Remove only generated staging
/// files for this exact destination before starting the next scan.
fn cleanup_stale_scan_files(db_new: &Path) -> Result<()> {
    let Some(parent) = db_new.parent() else {
        anyhow::bail!("staging database path has no parent directory");
    };
    let base = db_new
        .file_name()
        .context("staging database path has no filename")?
        .to_string_lossy();
    let prefix = format!("{base}.scan-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        // A not-yet-created custom DB parent cannot contain stale staging files;
        // the normal store creation path will create it when needed.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("listing scan staging directory {}", parent.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| {
            format!("reading scan staging directory entry {}", parent.display())
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_stale_scan_artifact_name(&name, &prefix) {
            continue;
        }
        // Never remove a directory, symlink, or special file that happens to
        // occupy the reserved internal filename pattern.
        if !entry
            .file_type()
            .with_context(|| format!("checking scan staging entry {}", entry.path().display()))?
            .is_file()
        {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "removing stale scan staging file {}",
                        entry.path().display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn is_stale_scan_artifact_name(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let stem = [".tmp-journal", ".tmp-wal", ".tmp-shm", ".tmp"]
        .iter()
        .find_map(|suffix| rest.strip_suffix(suffix));
    let Some(stem) = stem else {
        return false;
    };
    let Some((pid, nonce)) = stem.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && !nonce.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && nonce.bytes().all(|byte| byte.is_ascii_digit())
        && pid.parse::<u32>().is_ok_and(|value| value > 0)
        && nonce.parse::<u128>().is_ok_and(|value| value > 0)
}

/// Full scan of `root` into the index. Computes recursive directory totals
/// bottom-up in a single transaction (batched inserts — never row-at-a-time IO).
#[cfg(test)]
pub fn scan(store: &mut Store, root: &Path, opts: &ScanOptions) -> Result<ScanStats> {
    scan_with_progress_policy(store, root, opts, false)
}

/// Reconcile a complete filesystem observation into an existing v7 index.
/// Only rows whose values changed are written to the compatibility projection;
/// path-instance IDs are reused by parent/name identity. An incomplete walk
/// leaves the previous index rows untouched and records the failure. The caller
/// must hold the exclusive per-database lock for this entire operation.
pub(crate) fn reconcile_in_place(
    store: &mut Store,
    root: &Path,
    opts: &ScanOptions,
) -> Result<ScanStats> {
    anyhow::ensure!(
        store.schema_version == crate::store::SCHEMA_VERSION,
        "incremental reconciliation requires the current schema"
    );
    anyhow::ensure!(
        !opts.one_file_system && opts.exclude.is_empty(),
        "incremental reconciliation requires an unfiltered full-root observation"
    );
    crate::fault::checkpoint("scan.in_place_started");
    let result = scan_with_progress_policy_mode(store, root, opts, false, true);
    if result.is_ok() {
        crate::fault::checkpoint("scan.in_place_completed");
    }
    result
}

/// Keep the final progress snapshot visible while an atomic caller fsyncs and
/// publishes the staging database. `rebuild_atomic` owns marker cleanup.
fn scan_with_progress_policy(
    store: &mut Store,
    root: &Path,
    opts: &ScanOptions,
    retain_progress_for_publication: bool,
) -> Result<ScanStats> {
    scan_with_progress_policy_mode(store, root, opts, retain_progress_for_publication, false)
}

fn scan_with_progress_policy_mode(
    store: &mut Store,
    root: &Path,
    opts: &ScanOptions,
    retain_progress_for_publication: bool,
    incremental: bool,
) -> Result<ScanStats> {
    let attempt_id = incremental.then(|| {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        format!("{}-{nonce}", std::process::id())
    });
    if let Some(cancel) = opts.cancel.as_ref() {
        let cancel = Arc::clone(cancel);
        store
            .conn
            .progress_handler(10_000, Some(move || cancel.load(Ordering::Relaxed)))?;
    }
    let result = scan_inner(
        store,
        root,
        opts,
        retain_progress_for_publication,
        incremental,
    );
    let cleanup = store.conn.progress_handler(0, None::<fn() -> bool>);
    let outcome = if cancellation_requested(&opts.cancel) {
        Err(ScanCancelled.into())
    } else {
        match result {
            Ok(stats) => cleanup.map(|()| stats).map_err(anyhow::Error::from),
            Err(error) => Err(error),
        }
    };
    if let Some(attempt_id) = attempt_id {
        let result = match &outcome {
            Ok(stats) if stats.errors == 0 => "complete",
            Ok(_) => "incomplete",
            Err(error) if error.downcast_ref::<ScanCancelled>().is_some() => "cancelled",
            Err(_) => "failed",
        };
        let tx = store.conn.transaction()?;
        for (key, value) in [
            ("last_reconcile_attempt_id", attempt_id),
            ("last_reconcile_attempt_result", result.to_owned()),
        ] {
            tx.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![key, value],
            )?;
        }
        tx.commit()?;
    }
    outcome
}

fn scan_inner(
    store: &mut Store,
    root: &Path,
    opts: &ScanOptions,
    retain_progress_for_publication: bool,
    incremental: bool,
) -> Result<ScanStats> {
    ensure_not_cancelled(&opts.cancel)?;
    if opts.low_priority {
        set_low_priority();
    }
    // Whole-index GROUP/window/FTS operations may need large sort scratch. Keep
    // that scratch on disk during a scan; temp_store=MEMORY reached ~1.8 GiB on
    // a 2.1M-entry production index despite the Rust pipeline being bounded.
    store.conn.execute_batch("PRAGMA temp_store=FILE;")?;
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let meta = fs::symlink_metadata(&root).with_context(|| format!("statx {}", root.display()))?;
    if !meta.is_dir() {
        anyhow::bail!("{} is not a directory", root.display());
    }
    let root_allocated = crate::util::allocated_bytes(&meta)
        .with_context(|| format!("reading allocated size for scan root {}", root.display()))?;
    let (root_dev, root_inode) = crate::util::indexed_identity(&meta).with_context(|| {
        format!(
            "reading filesystem identity for scan root {}",
            root.display()
        )
    })?;
    let (root_birth_ns, root_mount_id) = crate::util::inode_path_metadata_at(&meta, &root);
    let now = now_secs();
    let started = Instant::now();

    // The caller builds into a FRESH, empty index (atomic rescan), so there is
    // nothing to reset here — and the FTS sync triggers don't exist yet, which
    // is why the bulk load below is fast (FTS is rebuilt once in finalize_bulk).

    // ---- shared progress counters + background printer thread ----
    let n_files = Arc::new(AtomicU64::new(0));
    let n_dirs = Arc::new(AtomicU64::new(1)); // counting root
    let n_bytes = Arc::new(AtomicI64::new(root_allocated));
    let done = Arc::new(AtomicBool::new(false));
    let indexing = Arc::new(AtomicBool::new(false));
    // RAII guard: signals `done` and joins the printer on EVERY exit path
    // (including an early `?` error in a later phase), so the thread never leaks
    // or keeps spinning.
    struct ProgressGuard {
        done: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for ProgressGuard {
        fn drop(&mut self) {
            self.done.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    // The progress thread ALWAYS runs and publishes a tmpfs progress file (so a
    // background/--quiet scan is still observable via `dux status`); the stderr
    // spinner is only drawn for an interactive scan (opts.progress).
    let progress_db: String = store.conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |r| r.get(0),
    )?;
    let progress_db = PathBuf::from(progress_db.strip_suffix(".new").unwrap_or(&progress_db));
    let progress_thread = {
        let (f, d, b, dn, ix) = (
            n_files.clone(),
            n_dirs.clone(),
            n_bytes.clone(),
            done.clone(),
            indexing.clone(),
        );
        let show_stderr = opts.progress;
        let started_epoch = now_secs();
        let retain_progress = retain_progress_for_publication;
        Some(std::thread::spawn(move || {
            let spin = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0usize;
            let mut last_pub = Instant::now() - Duration::from_secs(2);
            while !dn.load(Ordering::Relaxed) {
                let (fc, dc, bc) = (
                    f.load(Ordering::Relaxed),
                    d.load(Ordering::Relaxed),
                    b.load(Ordering::Relaxed),
                );
                let indexing = ix.load(Ordering::Relaxed);
                // publish to tmpfs ~1×/s so a concurrent `dux status` can show it
                if last_pub.elapsed() >= Duration::from_millis(1000) {
                    crate::util::write_scan_progress(
                        &progress_db,
                        started_epoch,
                        fc,
                        dc,
                        bc,
                        indexing,
                    );
                    last_pub = Instant::now();
                }
                if show_stderr {
                    let verb = if indexing {
                        "building index…"
                    } else {
                        "scanning…      "
                    };
                    eprint!(
                        "\r\x1b[K {} {}  {} files  {} dirs  {}  {:.0}s",
                        spin[i % spin.len()],
                        verb,
                        fc,
                        dc,
                        crate::util::human(bc),
                        started.elapsed().as_secs_f64(),
                    );
                    std::io::stderr().flush().ok();
                }
                i += 1;
                std::thread::sleep(Duration::from_millis(120));
            }
            if retain_progress {
                crate::util::write_scan_progress(
                    &progress_db,
                    started_epoch,
                    f.load(Ordering::Relaxed),
                    d.load(Ordering::Relaxed),
                    b.load(Ordering::Relaxed),
                    true,
                );
            } else {
                crate::util::clear_scan_progress(&progress_db);
            }
        }))
    };
    let progress_guard = ProgressGuard {
        done: done.clone(),
        handle: progress_thread,
    };

    // ---- phase 1: parallel walk into a bounded, disk-backed staging DB ----
    let n_errors = Arc::new(AtomicU64::new(0));
    let main_db: String = store.conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |r| r.get(0),
    )?;
    // Atomic rebuilds write `<live>.new`; normalize back to the live DB name so
    // the walker can exclude the live file, `.new`, WAL/SHM, lock and scan-stage
    // files as one family.
    let index_db = PathBuf::from(main_db.strip_suffix(".new").unwrap_or(&main_db));
    // An in-place scan writes its observation to `<index>.scan-<pid>-<nonce>.tmp`.
    // SIGKILL or power loss skips StageGuard::drop, so remove abandoned stages
    // before starting another observation. The caller holds the exclusive
    // database writer lock, which ensures these matching files are stale.
    if incremental {
        cleanup_stale_scan_files(&index_db)?;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stage_path = PathBuf::from(format!(
        "{}.scan-{}-{nonce}.tmp",
        main_db,
        std::process::id(),
    ));
    struct StageGuard(PathBuf);
    impl Drop for StageGuard {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    let stage_guard = StageGuard(stage_path.clone());
    let mut stage = Connection::open(&stage_path)
        .with_context(|| format!("creating scan staging DB {}", stage_path.display()))?;
    stage.execute_batch(
        "PRAGMA journal_mode=OFF;
         PRAGMA synchronous=OFF;
         PRAGMA temp_store=FILE;
         PRAGMA cache_size=-16384;
         CREATE TABLE scan_nodes (
           path_id INTEGER PRIMARY KEY CHECK(path_id>0), stable_order INTEGER NOT NULL DEFAULT 0,
           parent_path_id INTEGER NOT NULL,
           mount_id INTEGER,
           dev INTEGER NOT NULL, inode INTEGER NOT NULL,
           parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL,
           depth INTEGER NOT NULL, kind TEXT NOT NULL, blocks INTEGER NOT NULL,
           uid INTEGER NOT NULL, mtime INTEGER NOT NULL, name BLOB NOT NULL,
           birth_ns INTEGER NOT NULL, cycle INTEGER NOT NULL DEFAULT 0,
           UNIQUE(parent_path_id,name)
         );",
    )?;
    if incremental {
        stage.execute("ATTACH DATABASE ?1 AS existing", params![main_db])?;
    }
    stage.execute_batch(crate::store::PATH_GRAPH_TABLES_SQL)?;
    parallel_stage(
        &mut stage,
        &root,
        root_dev,
        root_inode,
        root_mount_id,
        &index_db,
        opts,
        &n_files,
        &n_dirs,
        &n_bytes,
        &n_errors,
    )?;
    stage.execute(
        "INSERT INTO scan_nodes
         (path_id,parent_path_id,mount_id,dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name,birth_ns)
         VALUES (1,-1,?1,?2,?3,?2,?3,0,'d',?4,?5,?6,?7,?8)",
        params![
            root_mount_id,
            root_dev,
            root_inode,
            root_allocated,
            meta.uid() as i64,
            meta.mtime(),
            root.as_os_str().as_bytes(),
            root_birth_ns,
        ],
    )?;

    // The walker assigns provisional IDs concurrently, so their order depends
    // on worker scheduling. Atomic builds compute the stable order here. An
    // in-place observation computes the same stable order while assigning
    // persistent path IDs, avoiding a second full-stage ordering/update pass.
    if incremental {
        assign_persistent_scan_path_ids(&mut stage)?;
    } else {
        assign_stable_scan_path_order(&mut stage)?;
    }

    stage.execute_batch(
        "CREATE INDEX scan_nodes_target ON scan_nodes(dev,inode,parent_dev,parent_inode,name);",
    )?;

    // A long walk can observe a file, then see its inode number reused for a
    // different file later in the same walk. The older path is no longer in
    // the tree, so it must not poison the entire candidate forever. Revalidate
    // only paths participating in inode-incarnation conflicts: prune vanished
    // subtrees and refresh directly revalidated non-directory leaves. Other
    // ambiguous paths remain fail-closed below.
    let mut conflicting_inode_incarnations = count_conflicting_inode_incarnations(&stage)?;
    let (vanished_reuse_paths, revalidated_reuse_paths, unresolved_reuse_paths) =
        if conflicting_inode_incarnations > 0 {
            let (removed, revalidated, unresolved) = revalidate_inode_reuse_paths(&stage, &root)
                .context("validating inode-reuse races")?;
            if removed > 0 || revalidated > 0 {
                conflicting_inode_incarnations = count_conflicting_inode_incarnations(&stage)?;
            }
            (removed, revalidated, unresolved)
        } else {
            (0, 0, 0)
        };
    if vanished_reuse_paths > 0 {
        tracing::warn!(
            vanished_reuse_paths,
            "discarded staged paths that vanished during inode-reuse races"
        );
    }
    if revalidated_reuse_paths > 0 {
        tracing::warn!(
            revalidated_reuse_paths,
            "refreshed staged file identities after inode-reuse races"
        );
    }
    if unresolved_reuse_paths > 0 {
        tracing::error!(
            unresolved_reuse_paths,
            "inode-reuse paths remain ambiguous after revalidation"
        );
    }

    let repeated_directory_identities = count_repeated_directory_identities(&stage)?;
    if repeated_directory_identities > 0 {
        tracing::error!(
            repeated_directory_identities,
            "multiple visible directory paths share inode identities; legacy index remains incomplete"
        );
    }
    let conflicting_inode_edges = count_conflicting_inode_edges(&stage)?;
    if conflicting_inode_edges > 0 {
        tracing::error!(
            conflicting_inode_edges,
            "the same inode-parent/name edge resolved to different objects during the scan"
        );
    }
    let conflicting_inode_allocations = count_conflicting_inode_allocations(&stage)?;
    if conflicting_inode_allocations > 0 {
        tracing::error!(
            conflicting_inode_allocations,
            "the same inode had different allocated sizes during the scan"
        );
    }
    if conflicting_inode_incarnations > 0 {
        tracing::error!(
            conflicting_inode_incarnations,
            "inode numbers were observed with different birth-time incarnations during the scan"
        );
    }
    let unknown_repeated_inode_identities = count_unknown_repeated_inode_identities(&stage)?;
    if unknown_repeated_inode_identities > 0 {
        tracing::error!(
            unknown_repeated_inode_identities,
            "repeated inode numbers lack birth-time evidence and cannot be proven to share an incarnation"
        );
    }
    let cycle_paths = count_cycle_paths(&stage)?;

    validate_scan_path_stage(&stage, root_dev, root_inode)?;

    let (staged_path_instances, staged_directory_alias_paths) =
        build_and_validate_staged_path_graph(&stage)?;
    stage.execute_batch("CREATE INDEX scan_nodes_depth ON scan_nodes(depth);")?;
    let (
        staged_root_visible_bytes,
        staged_root_visible_entries,
        staged_root_unique_bytes,
        staged_root_unique_inodes,
    ) = build_staged_path_totals(&mut stage)?;
    let staged_files = stage.query_row(
        "SELECT COUNT(*) FROM scan_nodes WHERE path_id<>1 AND kind<>'d'",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    let staged_dirs = stage.query_row(
        "SELECT COUNT(*) FROM scan_nodes WHERE kind='d'",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;

    // ---- phase 2: deterministic hardlinks + bottom-up totals in SQLite ----
    indexing.store(true, Ordering::Relaxed);
    drop(stage);
    store.conn.execute(
        "ATTACH DATABASE ?1 AS scan",
        params![stage_path.to_string_lossy()],
    )?;
    let build_result: Result<i64> = (|| {
        if incremental {
            let complete = n_errors.load(Ordering::Relaxed) == 0
                && repeated_directory_identities == 0
                && conflicting_inode_edges == 0
                && conflicting_inode_allocations == 0
                && conflicting_inode_incarnations == 0
                && unknown_repeated_inode_identities == 0
                && !opts.one_file_system
                && opts.exclude.is_empty();
            if complete {
                store.conn.execute_batch(V6_SCAN_PROJECTION_SQL)?;
            }
            crate::fault::checkpoint("scan.in_place_projection_ready");
            return reconcile_staged_projection(
                &mut store.conn,
                root_dev,
                root_inode,
                now,
                nonce,
                &root,
                &opts.cancel,
                complete,
            );
        }
        store.conn.execute_batch(V6_SCAN_PROJECTION_SQL)?;
        store.conn.execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO inodes
               (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             SELECT dev,inode,MAX(kind),MAX(blocks),MAX(blocks),1,MAX(uid),MAX(mtime),MAX(birth_ns)
             FROM scan.scan_nodes GROUP BY dev,inode;

             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             SELECT parent_dev,parent_inode,name,dev,inode,
                    CASE WHEN ROW_NUMBER() OVER (
                      PARTITION BY dev,inode ORDER BY parent_dev,parent_inode,name
                    )=1 THEN 1 ELSE 0 END
             FROM scan_v6_nodes;

             INSERT INTO paths
               (path_id,parent_path_id,name,dev_id,inode,birth_ns,kind,mount_id,prime,cycle)
             SELECT path_id,parent_path_id,name,dev_id,inode,birth_ns,kind,mount_id,prime,cycle
             FROM scan.paths;

             INSERT INTO dir_totals
               (path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes)
             SELECT path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes
             FROM scan.dir_totals;

             CREATE TEMP TABLE rollup (
               dev INTEGER NOT NULL, inode INTEGER NOT NULL,
               bytes INTEGER NOT NULL, items INTEGER NOT NULL,
               PRIMARY KEY(dev,inode)
             ) WITHOUT ROWID;
             COMMIT;",
        )?;
        let max_depth: i64 = store.conn.query_row(
            "SELECT COALESCE(MAX(depth),0) FROM scan_v6_nodes",
            [],
            |r| r.get(0),
        )?;
        for depth in (1..=max_depth).rev() {
            ensure_not_cancelled(&opts.cancel)?;
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM rollup", [])?;
            tx.execute(
                "INSERT INTO rollup(dev,inode,bytes,items)
                 SELECT s.parent_dev,s.parent_inode,
                        COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_bytes ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_inodes ELSE 0 END),0)
                 FROM scan_v6_nodes s
                 JOIN dirents d ON d.parent_dev=s.parent_dev
                   AND d.parent_inode=s.parent_inode AND d.name=s.name
                 JOIN inodes i ON i.dev_id=s.dev AND i.inode=s.inode
                 WHERE s.depth=?1 AND NOT (s.dev=s.parent_dev AND s.inode=s.parent_inode)
                 GROUP BY s.parent_dev,s.parent_inode",
                params![depth],
            )?;
            tx.execute_batch(
                "UPDATE inodes SET
                   recursive_bytes=recursive_bytes+COALESCE((
                     SELECT bytes FROM rollup r WHERE r.dev=inodes.dev_id AND r.inode=inodes.inode
                   ),0),
                   recursive_inodes=recursive_inodes+COALESCE((
                     SELECT items FROM rollup r WHERE r.dev=inodes.dev_id AND r.inode=inodes.inode
                   ),0)
                 WHERE EXISTS (
                   SELECT 1 FROM rollup r WHERE r.dev=inodes.dev_id AND r.inode=inodes.inode
                 );",
            )?;
            tx.commit()?;
        }
        Ok(store.conn.query_row(
            "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![root_dev, root_inode],
            |r| r.get(0),
        )?)
    })();
    let _ = store.conn.execute_batch(
        "DROP TABLE IF EXISTS temp.rollup;
         DROP TABLE IF EXISTS temp.scan_v6_nodes;
         DROP TABLE IF EXISTS temp.scan_dirents;
         DROP TABLE IF EXISTS temp.scan_inodes;
         DROP TABLE IF EXISTS temp.scan_rollup;
         DETACH DATABASE scan;",
    );
    let root_total = build_result?;
    drop(stage_guard);

    // Existing indexes and FTS triggers are maintained incrementally; a fresh
    // atomic candidate still builds them once after its bulk load.
    if !incremental {
        store.finalize_bulk()?;
    }
    // Restore the query-optimized connection default. In the atomic rebuild path
    // this connection is about to close, but direct scan tests may reuse it.
    store.conn.execute_batch("PRAGMA temp_store=MEMORY;")?;

    let stats = ScanStats {
        files: staged_files,
        dirs: staged_dirs,
        staged_path_instances: staged_path_instances as u64,
        staged_directory_alias_paths: staged_directory_alias_paths as u64,
        staged_root_visible_bytes,
        staged_root_visible_entries: staged_root_visible_entries as u64,
        staged_root_unique_bytes,
        staged_root_unique_inodes: staged_root_unique_inodes as u64,
        bytes: root_total,
        unreadable_entries: n_errors.load(Ordering::Relaxed),
        repeated_directory_identities: repeated_directory_identities as u64,
        cycle_paths: cycle_paths as u64,
        conflicting_inode_edges: conflicting_inode_edges as u64,
        conflicting_inode_allocations: conflicting_inode_allocations as u64,
        conflicting_inode_incarnations: conflicting_inode_incarnations as u64,
        unknown_repeated_inode_identities: unknown_repeated_inode_identities as u64,
        vanished_reuse_paths,
        revalidated_reuse_paths,
        unresolved_reuse_paths,
        errors: n_errors.load(Ordering::Relaxed)
            + repeated_directory_identities as u64
            + conflicting_inode_edges as u64
            + conflicting_inode_allocations as u64
            + conflicting_inode_incarnations as u64
            + unknown_repeated_inode_identities as u64
            + unresolved_reuse_paths,
    };

    // stop + join the printer thread (the guard would also do this on early
    // error returns); then clear the progress line.
    drop(progress_guard);
    if opts.progress {
        eprint!("\r\x1b[K");
        std::io::stderr().flush().ok();
    }

    if stats.errors > 0 {
        store.set_meta("dirty_since", &now_secs().to_string())?;
        store.set_meta("dirty_reason", &incomplete_scan_reason(&stats))?;
    }
    if !incremental || stats.errors == 0 {
        store.set_meta(
            "scan_generation",
            &format!("{}-{nonce}", std::process::id()),
        )?;
        store.set_meta("last_scan_root", &root.to_string_lossy())?;
        store.set_meta(
            "last_scan_root_bytes",
            &crate::store::encode_path_bytes(&root),
        )?;
        store.set_meta("root_dev", &root_dev.to_string())?;
        store.set_meta("root_inode", &root_inode.to_string())?;
        record_last_scan_completion(&store.conn, now, now_secs())?;
    }
    Ok(stats)
}

/// A node in the bounded handoff between stat workers and the staging writer.
struct RawNode {
    path_id: i64,
    parent_path_id: i64,
    mount_id: Option<i64>,
    dev: i64,
    inode: i64,
    parent_dev: i64,
    parent_inode: i64,
    depth: u32,
    kind: char,
    blocks: i64,
    uid: i64,
    mtime: i64,
    birth_ns: i64,
    cycle: bool,
    name: Vec<u8>, // raw filename bytes — identity-preserving (no lossy UTF-8)
}

/// Ephemeral scan path IDs flow through jwalk's entry state. They are scoped to
/// one private scan generation and are not published into the v6 index.
#[derive(Debug, Default)]
struct ScanPathState;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScanPathInfo {
    path_id: i64,
    mount_id: Option<i64>,
    /// Directory identities above this path instance, excluding itself.
    ancestors: Option<Arc<DirectoryAncestor>>,
}

impl jwalk::ClientState for ScanPathState {
    type ReadDirState = ScanPathInfo;
    type DirEntryState = ScanPathInfo;

    fn read_dir_state_for_entry(
        entry_state: &Self::DirEntryState,
        _inherited: &Self::ReadDirState,
    ) -> Self::ReadDirState {
        entry_state.clone()
    }
}

fn allocate_scan_path_id(next: &AtomicI64) -> Option<i64> {
    next.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
}

/// Assign a stable breadth-first order to provisional parallel-walk IDs. At
/// each depth, paths sort by their parent's stable order and raw name bytes.
/// This makes tie-breaks reproducible across worker schedules without
/// retaining complete paths in memory or rewriting the path graph.
fn assign_stable_scan_path_order(stage: &mut Connection) -> Result<()> {
    let tx = stage.transaction()?;
    tx.execute_batch(
        "CREATE INDEX scan_nodes_stable_order ON scan_nodes(depth,parent_path_id,name);
         CREATE TEMP TABLE stable_path_ids(
           old_id INTEGER PRIMARY KEY,
           stable_id INTEGER NOT NULL UNIQUE
         );",
    )?;
    let root_rows = tx.execute(
        "INSERT INTO stable_path_ids(old_id,stable_id)
         SELECT path_id,1 FROM scan_nodes WHERE parent_path_id=-1",
        [],
    )?;
    anyhow::ensure!(
        root_rows == 1,
        "cannot stabilize scan IDs without exactly one root"
    );

    let max_depth: i64 =
        tx.query_row("SELECT COALESCE(MAX(depth),0) FROM scan_nodes", [], |row| {
            row.get(0)
        })?;
    for depth in 1..=max_depth {
        let base: i64 = tx.query_row(
            "SELECT COALESCE(MAX(stable_id),0) FROM stable_path_ids",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO stable_path_ids(old_id,stable_id)
             SELECT child.path_id,
                    ?2 + ROW_NUMBER() OVER (ORDER BY parent.stable_id,child.name)
             FROM scan_nodes child
             JOIN stable_path_ids parent ON parent.old_id=child.parent_path_id
             WHERE child.depth=?1",
            rusqlite::params![depth, base],
        )?;
    }

    let node_count: i64 = tx.query_row("SELECT COUNT(*) FROM scan_nodes", [], |row| row.get(0))?;
    let mapped_count: i64 =
        tx.query_row("SELECT COUNT(*) FROM stable_path_ids", [], |row| row.get(0))?;
    anyhow::ensure!(
        mapped_count == node_count,
        "cannot assign stable scan order: mapped {mapped_count} of {node_count} path instances"
    );
    let assigned = tx.execute(
        "UPDATE scan_nodes
         SET stable_order=(SELECT stable_id FROM stable_path_ids WHERE old_id=scan_nodes.path_id)",
        [],
    )?;
    anyhow::ensure!(
        assigned as i64 == node_count,
        "cannot assign stable scan order: updated {assigned} of {node_count} path instances"
    );
    let (min_order, max_order, distinct_orders): (i64, i64, i64) = tx.query_row(
        "SELECT MIN(stable_order),MAX(stable_order),COUNT(DISTINCT stable_order) FROM scan_nodes",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    anyhow::ensure!(
        min_order == 1 && max_order == node_count && distinct_orders == node_count,
        "invalid stable scan order: min={min_order}, max={max_order}, distinct={distinct_orders}, rows={node_count}"
    );
    tx.execute_batch(
        "DROP TABLE stable_path_ids;
         DROP INDEX scan_nodes_stable_order;",
    )?;
    tx.commit()?;
    Ok(())
}

/// Reuse committed v7 path IDs by `(parent_path_id, name)` and allocate IDs
/// monotonically for new paths. This prevents one insertion near the front of
/// a tree from renumbering every later path in a parallel scan.
fn assign_persistent_scan_path_ids(stage: &mut Connection) -> Result<()> {
    let root: (i64, i64, i64, i64, i64) = stage.query_row(
        "SELECT p.path_id,p.dev_id,p.inode,s.dev,s.inode
         FROM existing.paths p, scan_nodes s
         WHERE p.parent_path_id=-1 AND s.parent_path_id=-1",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    anyhow::ensure!(
        root.0 == 1 && root.1 == root.3 && root.2 == root.4,
        "incremental scan root identity differs from the installed v7 graph"
    );
    let stored_next: Option<String> = stage
        .query_row(
            "SELECT value FROM existing.meta WHERE key='path_id_next'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let stored_next = stored_next
        .map(|value| value.parse::<i64>())
        .transpose()
        .context("reading persistent next path ID")?
        .unwrap_or(1);
    let existing_max: i64 = stage.query_row(
        "SELECT COALESCE(MAX(path_id),0) FROM existing.paths",
        [],
        |row| row.get(0),
    )?;
    let mut next_id = stored_next.max(existing_max.saturating_add(1)).max(2);
    stage.execute_batch(
        "CREATE TEMP TABLE persistent_path_ids(
           old_id INTEGER PRIMARY KEY,
           new_id INTEGER NOT NULL UNIQUE,
           stable_order INTEGER NOT NULL UNIQUE
         );
         INSERT INTO persistent_path_ids(old_id,new_id,stable_order)
         SELECT path_id,1,1 FROM scan_nodes WHERE parent_path_id=-1;",
    )?;
    let max_depth: i64 =
        stage.query_row("SELECT COALESCE(MAX(depth),0) FROM scan_nodes", [], |row| {
            row.get(0)
        })?;
    let mut next_stable_order = 2i64;
    for depth in 1..=max_depth {
        let rows: Vec<(i64, Option<i64>)> = stage
            .prepare(
                "SELECT child.path_id,existing.path_id
                 FROM scan_nodes child
                 JOIN persistent_path_ids parent ON parent.old_id=child.parent_path_id
                 LEFT JOIN existing.paths existing
                   ON existing.parent_path_id=parent.new_id AND existing.name=child.name
                 WHERE child.depth=?1
                 ORDER BY parent.stable_order,child.name",
            )?
            .query_map([depth], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut additions = Vec::new();
        for (old_id, existing_id) in rows {
            let new_id = if let Some(existing_id) = existing_id {
                existing_id
            } else {
                anyhow::ensure!(next_id < i64::MAX, "persistent path-ID space exhausted");
                let allocated = next_id;
                next_id += 1;
                allocated
            };
            anyhow::ensure!(
                next_stable_order < i64::MAX,
                "stable scan-order space exhausted"
            );
            additions.push((old_id, new_id, next_stable_order));
            next_stable_order += 1;
        }
        let tx = stage.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO persistent_path_ids(old_id,new_id,stable_order) VALUES(?1,?2,?3)",
            )?;
            for (old_id, new_id, stable_order) in additions {
                insert.execute(params![old_id, new_id, stable_order])?;
            }
        }
        tx.commit()?;
    }
    let (rows, mapped): (i64, i64) = stage.query_row(
        "SELECT (SELECT COUNT(*) FROM scan_nodes),
                (SELECT COUNT(*) FROM persistent_path_ids)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    anyhow::ensure!(
        rows == mapped,
        "persistent path-ID mapping covered {mapped} of {rows} scan nodes"
    );
    let (stable_min, stable_max, stable_count): (i64, i64, i64) = stage.query_row(
        "SELECT MIN(stable_order),MAX(stable_order),COUNT(DISTINCT stable_order)
         FROM persistent_path_ids",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    anyhow::ensure!(
        stable_min == 1 && stable_max == rows && stable_count == rows,
        "invalid persistent stable order: min={stable_min}, max={stable_max}, distinct={stable_count}, rows={rows}"
    );
    stage.execute(
        "CREATE TABLE scan_path_allocator(next_id INTEGER NOT NULL CHECK(next_id>0));",
        [],
    )?;
    stage.execute(
        "INSERT INTO scan_path_allocator(next_id) VALUES(?1)",
        [next_id],
    )?;
    stage.execute_batch(
        "CREATE TABLE scan_nodes_remapped (
           path_id INTEGER PRIMARY KEY CHECK(path_id>0), stable_order INTEGER NOT NULL DEFAULT 0,
           parent_path_id INTEGER NOT NULL, mount_id INTEGER,
           dev INTEGER NOT NULL, inode INTEGER NOT NULL,
           parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL,
           depth INTEGER NOT NULL, kind TEXT NOT NULL, blocks INTEGER NOT NULL,
           uid INTEGER NOT NULL, mtime INTEGER NOT NULL, name BLOB NOT NULL,
           birth_ns INTEGER NOT NULL, cycle INTEGER NOT NULL DEFAULT 0,
           UNIQUE(parent_path_id,name)
         );
         INSERT INTO scan_nodes_remapped
           (path_id,stable_order,parent_path_id,mount_id,dev,inode,parent_dev,
            parent_inode,depth,kind,blocks,uid,mtime,name,birth_ns,cycle)
         SELECT child_id.new_id,child_id.stable_order,
                COALESCE(parent_id.new_id,-1),child.mount_id,child.dev,child.inode,
                child.parent_dev,child.parent_inode,child.depth,child.kind,child.blocks,
                child.uid,child.mtime,child.name,child.birth_ns,child.cycle
         FROM scan_nodes child
         JOIN persistent_path_ids child_id ON child_id.old_id=child.path_id
         LEFT JOIN persistent_path_ids parent_id ON parent_id.old_id=child.parent_path_id;
         DROP TABLE scan_nodes;
         ALTER TABLE scan_nodes_remapped RENAME TO scan_nodes;
         DROP TABLE persistent_path_ids;",
    )?;
    Ok(())
}

/// Verify the private walk's path-instance edges before deriving v6 inode
/// edges. A lost parent context must fail the candidate scan, not publish
/// apparently complete inode-keyed totals.
fn validate_scan_path_stage(stage: &Connection, root_dev: i64, root_inode: i64) -> Result<()> {
    let (root_rows, root_sentinels, bad_ids, bad_parents, duplicate_names, invalid_names): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = stage.query_row(
        "SELECT
               (SELECT COUNT(*) FROM scan_nodes
                 WHERE path_id=1 AND parent_path_id=-1 AND depth=0
                   AND dev=?1 AND inode=?2 AND kind='d'),
               (SELECT COUNT(*) FROM scan_nodes WHERE parent_path_id=-1),
               (SELECT COUNT(*) FROM scan_nodes WHERE path_id<=0),
               (SELECT COUNT(*) FROM scan_nodes c
                 LEFT JOIN scan_nodes p ON p.path_id=c.parent_path_id
                 WHERE c.path_id<>1 AND (
                   c.parent_path_id<=0 OR c.parent_path_id>=c.path_id
                   OR p.path_id IS NULL OR p.kind<>'d'
                   OR p.dev<>c.parent_dev OR p.inode<>c.parent_inode)),
               (SELECT COUNT(*) FROM (
                  SELECT parent_path_id,name FROM scan_nodes WHERE path_id<>1
                  GROUP BY parent_path_id,name HAVING COUNT(*)>1)),
               (SELECT COUNT(*) FROM scan_nodes WHERE path_id<>1 AND (
                  length(name)=0 OR instr(name,X'2f')>0 OR instr(name,X'00')>0
                  OR name IN (X'2e',X'2e2e'))) ",
        params![root_dev, root_inode],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    if root_rows != 1
        || root_sentinels != 1
        || bad_ids != 0
        || bad_parents != 0
        || duplicate_names != 0
        || invalid_names != 0
    {
        anyhow::bail!(
            "invalid scan path staging graph: root_rows={root_rows}, root_sentinels={root_sentinels}, bad_ids={bad_ids}, bad_parents={bad_parents}, duplicate_names={duplicate_names}, invalid_names={invalid_names}"
        );
    }
    Ok(())
}

fn count_repeated_directory_identities(stage: &Connection) -> Result<i64> {
    stage
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT dev,inode,birth_ns FROM scan_nodes
               WHERE kind='d' AND cycle=0
               GROUP BY dev,inode,birth_ns HAVING COUNT(*)>1
             )",
            [],
            |row| row.get(0),
        )
        .context("counting directory path aliases in scan staging")
}

fn count_conflicting_inode_edges(stage: &Connection) -> Result<i64> {
    stage
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT parent_dev,parent_inode,name FROM scan_nodes
               GROUP BY parent_dev,parent_inode,name
               HAVING COUNT(DISTINCT dev)>1 OR COUNT(DISTINCT inode)>1
                  OR COUNT(DISTINCT birth_ns)>1
             )",
            [],
            |row| row.get(0),
        )
        .context("checking inode-parent edges for scan-time identity conflicts")
}

fn count_conflicting_inode_allocations(stage: &Connection) -> Result<i64> {
    stage
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT dev,inode,birth_ns FROM scan_nodes
               GROUP BY dev,inode,birth_ns HAVING MIN(blocks)!=MAX(blocks)
             )",
            [],
            |row| row.get(0),
        )
        .context("checking inode allocation stability during scan")
}

fn count_conflicting_inode_incarnations(stage: &Connection) -> Result<i64> {
    stage
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT dev,inode FROM scan_nodes
               GROUP BY dev,inode HAVING MIN(birth_ns)!=MAX(birth_ns)
             )",
            [],
            |row| row.get(0),
        )
        .context("checking inode-number reuse during scan")
}

/// Revalidate staged entries after inode-number reuse during a walk. Vanished
/// paths are pruned with their subtree; a present non-directory leaf can be
/// refreshed only after every ancestor and its current file type are verified.
/// Directory replacement and other ambiguous identities remain incomplete.
fn revalidate_inode_reuse_paths(stage: &Connection, root: &Path) -> Result<(u64, u64, u64)> {
    use std::os::unix::ffi::OsStringExt;

    let mut stmt = stage.prepare(
        "SELECT n.path_id FROM scan_nodes n
         JOIN (SELECT dev,inode FROM scan_nodes GROUP BY dev,inode
              HAVING MIN(birth_ns)!=MAX(birth_ns)) c
           ON c.dev=n.dev AND c.inode=n.inode
         WHERE n.path_id<>1 ORDER BY n.depth,n.path_id",
    )?;
    let candidates = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut vanished = Vec::new();
    let mut revalidated = 0u64;
    let mut unresolved = 0u64;
    for path_id in candidates {
        // An earlier vanished ancestor may already have removed this row.
        let exists: bool = stage.query_row(
            "SELECT EXISTS(SELECT 1 FROM scan_nodes WHERE path_id=?1)",
            [path_id],
            |row| row.get(0),
        )?;
        if !exists {
            continue;
        }
        let mut ancestry = Vec::new();
        let mut current = path_id;
        let mut depth_guard = 0usize;
        let mut ambiguous = false;
        while current != 1 {
            let (parent, name, dev, inode, birth_ns, kind):
                (i64, Vec<u8>, i64, i64, i64, String) = stage.query_row(
                "SELECT parent_path_id,name,dev,inode,birth_ns,kind FROM scan_nodes WHERE path_id=?1",
                [current],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
            anyhow::ensure!(
                parent > 0 && parent < current,
                "invalid staged ancestry for path {path_id}"
            );
            ancestry.push((
                current,
                std::ffi::OsString::from_vec(name),
                dev,
                inode,
                birth_ns,
                kind,
            ));
            current = parent;
            depth_guard += 1;
            anyhow::ensure!(
                depth_guard <= 1_000_000,
                "staged path ancestry is unreasonably deep"
            );
        }
        let mut path = root.to_path_buf();
        for (ancestor_id, component, expected_dev, expected_inode, expected_birth, expected_kind) in
            ancestry.into_iter().rev()
        {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // A parent directory may have vanished after its child was
                    // staged. Prune from the first absent path component so no
                    // stale ancestor or sibling subtree is retained.
                    vanished.push(ancestor_id);
                    break;
                }
                Ok(metadata) => {
                    let identity = crate::util::indexed_identity(&metadata).ok();
                    let current_birth = crate::util::inode_path_metadata_at(&metadata, &path).0;
                    let actual_kind = if metadata.is_dir() {
                        "d"
                    } else if metadata.file_type().is_symlink() {
                        "l"
                    } else if metadata.is_file() {
                        "f"
                    } else {
                        "o"
                    };
                    let kind_matches = expected_kind == actual_kind;
                    if identity != Some((expected_dev, expected_inode))
                        || !kind_matches
                        || current_birth != expected_birth
                    {
                        // The path itself may have been replaced while the
                        // walk was in flight. If all ancestors were validated
                        // and this is a non-directory leaf, refresh its
                        // current identity and allocation directly. A
                        // directory replacement needs a subtree walk and
                        // remains fail-closed below.
                        if ancestor_id == path_id && !metadata.is_dir() && kind_matches {
                            if let Some((current_dev, current_inode)) = identity {
                                let (birth_ns, mount_id) =
                                    crate::util::inode_path_metadata_at(&metadata, &path);
                                let blocks = crate::util::allocated_bytes(&metadata)?;
                                stage.execute(
                                    "UPDATE scan_nodes
                                     SET mount_id=?1,dev=?2,inode=?3,blocks=?4,uid=?5,mtime=?6,birth_ns=?7
                                     WHERE path_id=?8",
                                    params![
                                        mount_id,
                                        current_dev,
                                        current_inode,
                                        blocks,
                                        metadata.uid() as i64,
                                        metadata.mtime(),
                                        birth_ns,
                                        path_id,
                                    ],
                                )?;
                                revalidated = revalidated.saturating_add(1);
                                break;
                            }
                        }
                        // A replacement or symlinked ancestor is ambiguous; do
                        // not follow it or discard rows on that evidence alone.
                        ambiguous = true;
                        break;
                    }
                }
                // Permission, I/O, and other failures are not proof that the
                // path disappeared. Preserve it so validators fail closed.
                Err(_) => {
                    ambiguous = true;
                    break;
                }
            }
        }
        if ambiguous {
            unresolved = unresolved.saturating_add(1);
        }
    }

    let mut removed_paths = 0u64;
    for path_id in &vanished {
        removed_paths = removed_paths.saturating_add(stage.execute(
            "WITH RECURSIVE stale(path_id) AS (
                 SELECT ?1 UNION ALL
                 SELECT child.path_id FROM scan_nodes child
                 JOIN stale parent ON child.parent_path_id=parent.path_id
             )
             DELETE FROM scan_nodes WHERE path_id IN (SELECT path_id FROM stale)",
            [path_id],
        )? as u64);
    }
    Ok((removed_paths, revalidated, unresolved))
}

fn count_unknown_repeated_inode_identities(stage: &Connection) -> Result<i64> {
    stage
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT dev,inode FROM scan_nodes
               GROUP BY dev,inode
               HAVING COUNT(*)>1 AND MIN(birth_ns)=0 AND MAX(birth_ns)=0
             )",
            [],
            |row| row.get(0),
        )
        .context("checking repeated inode identities without birth-time evidence")
}

fn count_cycle_paths(stage: &Connection) -> Result<i64> {
    stage
        .query_row("SELECT COUNT(*) FROM scan_nodes WHERE cycle=1", [], |row| {
            row.get(0)
        })
        .context("counting cycle-closing paths in scan staging")
}

fn build_and_validate_staged_path_graph(stage: &Connection) -> Result<(i64, i64)> {
    stage.execute(
        "INSERT INTO paths(path_id,parent_path_id,name,dev_id,inode,birth_ns,kind,mount_id,prime,cycle)
         SELECT path_id,parent_path_id,name,dev,inode,birth_ns,kind,COALESCE(mount_id,-1),
                CASE WHEN ROW_NUMBER() OVER (
                  PARTITION BY dev,inode,birth_ns ORDER BY blocks DESC,stable_order
                )=1 THEN 1 ELSE 0 END,cycle
         FROM scan_nodes",
        [],
    )?;
    stage.execute_batch(crate::store::PATH_GRAPH_INDEXES_SQL)?;

    let (row_mismatch, field_mismatch, root_rows, bad_prime_groups, bad_parents): (
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = stage.query_row(
        "SELECT
           ABS((SELECT COUNT(*) FROM paths)-(SELECT COUNT(*) FROM scan_nodes)),
           (SELECT COUNT(*) FROM paths p JOIN scan_nodes s USING(path_id)
             WHERE p.parent_path_id!=s.parent_path_id OR p.name!=s.name
               OR p.dev_id!=s.dev OR p.inode!=s.inode OR p.birth_ns!=s.birth_ns OR p.kind!=s.kind
               OR p.mount_id!=COALESCE(s.mount_id,-1) OR p.cycle!=s.cycle),
           (SELECT COUNT(*) FROM paths WHERE parent_path_id=-1 AND path_id=1),
           (SELECT COUNT(*) FROM (
              SELECT dev_id,inode,birth_ns FROM paths GROUP BY dev_id,inode,birth_ns
              HAVING SUM(prime)!=1)),
           (SELECT COUNT(*) FROM paths c
             LEFT JOIN paths p ON p.path_id=c.parent_path_id
             JOIN scan_nodes s ON s.path_id=c.path_id
             WHERE c.parent_path_id!=-1 AND (
               p.path_id IS NULL OR p.kind!='d'
               OR p.dev_id!=s.parent_dev OR p.inode!=s.parent_inode))",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    if row_mismatch != 0
        || field_mismatch != 0
        || root_rows != 1
        || bad_prime_groups != 0
        || bad_parents != 0
    {
        anyhow::bail!(
            "invalid staged v7 path graph: row_mismatch={row_mismatch}, field_mismatch={field_mismatch}, root_rows={root_rows}, bad_prime_groups={bad_prime_groups}, bad_parents={bad_parents}"
        );
    }
    let path_instances: i64 =
        stage.query_row("SELECT COUNT(*) FROM paths", [], |row| row.get(0))?;
    let directory_alias_paths: i64 = stage.query_row(
        "SELECT COUNT(*) FROM paths p JOIN scan_nodes s USING(path_id)
         WHERE p.kind='d' AND p.prime=0 AND s.cycle=0",
        [],
        |row| row.get(0),
    )?;
    Ok((path_instances, directory_alias_paths))
}

fn build_staged_path_totals(stage: &mut Connection) -> Result<(i64, i64, i64, i64)> {
    stage.execute_batch(
        "CREATE TEMP TABLE staged_path_totals(
           path_id INTEGER PRIMARY KEY,
           visible_bytes INTEGER NOT NULL,
           visible_entries INTEGER NOT NULL,
           unique_bytes INTEGER NOT NULL,
           unique_inodes INTEGER NOT NULL
         );",
    )?;
    let max_depth: i64 =
        stage.query_row("SELECT COALESCE(MAX(depth),0) FROM scan_nodes", [], |row| {
            row.get(0)
        })?;
    for depth in (0..=max_depth).rev() {
        let tx = stage.transaction()?;
        tx.execute(
            "INSERT INTO staged_path_totals
             SELECT p.path_id,
                    s.blocks+COALESCE(SUM(child_totals.visible_bytes),0),
                    1+COALESCE(SUM(child_totals.visible_entries),0),
                    CASE WHEN p.prime=1 THEN s.blocks ELSE 0 END
                      +COALESCE(SUM(child_totals.unique_bytes),0),
                    CASE WHEN p.prime=1 THEN 1 ELSE 0 END
                      +COALESCE(SUM(child_totals.unique_inodes),0)
             FROM paths p JOIN scan_nodes s USING(path_id)
             LEFT JOIN paths child ON child.parent_path_id=p.path_id
             LEFT JOIN staged_path_totals child_totals ON child_totals.path_id=child.path_id
             WHERE s.depth=?1
             GROUP BY p.path_id,s.blocks,p.prime",
            [depth],
        )?;
        tx.commit()?;
    }
    stage.execute(
        "INSERT INTO dir_totals(path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes)
         SELECT p.path_id,t.visible_bytes,t.visible_entries,t.unique_bytes,t.unique_inodes
         FROM paths p JOIN staged_path_totals t USING(path_id)
         WHERE p.kind='d'",
        [],
    )?;
    let (visible_bytes, visible_entries, unique_bytes, unique_inodes): (i64, i64, i64, i64) = stage
        .query_row(
            "SELECT visible_bytes,visible_entries,unique_bytes,unique_inodes
             FROM dir_totals WHERE path_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let expected_visible_bytes: i64 = stage.query_row(
        "SELECT COALESCE(SUM(blocks),0) FROM scan_nodes",
        [],
        |row| row.get(0),
    )?;
    let expected_visible_entries: i64 =
        stage.query_row("SELECT COUNT(*) FROM scan_nodes", [], |row| row.get(0))?;
    let expected_unique_bytes: i64 = stage.query_row(
        "SELECT COALESCE(SUM(blocks),0) FROM (
           SELECT dev,inode,birth_ns,MAX(blocks) AS blocks
           FROM scan_nodes GROUP BY dev,inode,birth_ns
         )",
        [],
        |row| row.get(0),
    )?;
    let expected_unique_inodes: i64 = stage.query_row(
        "SELECT COUNT(*) FROM (
           SELECT dev,inode,birth_ns FROM scan_nodes GROUP BY dev,inode,birth_ns
         )",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        visible_bytes == expected_visible_bytes
            && visible_entries == expected_visible_entries
            && unique_bytes == expected_unique_bytes
            && unique_inodes == expected_unique_inodes,
        "staged root totals mismatch: visible=({visible_bytes},{visible_entries}) expected=({expected_visible_bytes},{expected_visible_entries}); unique=({unique_bytes},{unique_inodes}) expected=({expected_unique_bytes},{expected_unique_inodes})"
    );
    Ok((visible_bytes, visible_entries, unique_bytes, unique_inodes))
}

fn incomplete_scan_reason(stats: &ScanStats) -> String {
    format!(
        "scan incomplete: {} unreadable entries, {} repeated directory identities ({} cycle paths), {} conflicting inode-parent edges, {} conflicting inode allocations, {} conflicting inode incarnations, {} repeated inode identities with unknown incarnation, {} unresolved inode-reuse paths",
        stats.unreadable_entries,
        stats.repeated_directory_identities,
        stats.cycle_paths,
        stats.conflicting_inode_edges,
        stats.conflicting_inode_allocations,
        stats.conflicting_inode_incarnations,
        stats.unknown_repeated_inode_identities,
        stats.unresolved_reuse_paths,
    )
}

/// Build a compatibility projection for the current inode-parent schema. The
/// private walk keeps every path instance; v6 gets one deterministic row per
/// parent inode/name edge and excludes cycle-closing edges.
const V6_SCAN_PROJECTION_SQL: &str = r#"
CREATE TEMP TABLE scan_v6_nodes AS
SELECT * FROM scan.scan_nodes
WHERE cycle=0 AND path_id IN (
  SELECT path_id FROM (
    SELECT path_id,ROW_NUMBER() OVER (
      PARTITION BY parent_dev,parent_inode,name ORDER BY stable_order
    ) AS edge_rank
    FROM scan.scan_nodes
    WHERE cycle=0
  ) WHERE edge_rank=1
);
CREATE INDEX temp.scan_v6_nodes_depth ON scan_v6_nodes(depth);
"#;

/// Build the compatibility projection in the attached scan database, then
/// publish a complete observation with value-guarded UPSERTs. Failed or
/// ambiguous walks are observations only: they never delete or overwrite live
/// rows, so a transient permission/race error cannot make the index worse.
#[allow(clippy::too_many_arguments)]
fn reconcile_staged_projection(
    conn: &mut Connection,
    root_dev: i64,
    root_inode: i64,
    scan_time: i64,
    nonce: u128,
    root: &Path,
    cancel: &Option<Arc<AtomicBool>>,
    complete: bool,
) -> Result<i64> {
    // An incomplete observation is deliberately not publishable. In that case
    // building the compatibility rollups below only performs a full depth by
    // depth SQLite reduction whose results must be discarded. On large trees
    // this can dominate the cost of a failed reconciliation and add avoidable
    // disk I/O exactly when the host is already under pressure. Record the
    // attempt and return the last published total without materializing any
    // scan projection tables.
    if !complete {
        ensure_not_cancelled(cancel)?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('last_scan_attempt_ts',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [scan_time.to_string()],
        )?;
        tx.commit()?;
        ensure_not_cancelled(cancel)?;
        return Ok(conn.query_row(
            "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            [root_dev, root_inode],
            |r| r.get(0),
        )?);
    }

    conn.execute_batch(
        "CREATE TEMP TABLE scan_dirents AS
         SELECT parent_dev,parent_inode,name,dev,inode,
                CASE WHEN ROW_NUMBER() OVER (
                  PARTITION BY dev,inode ORDER BY parent_dev,parent_inode,name
                )=1 THEN 1 ELSE 0 END AS prime
         FROM scan_v6_nodes;
         CREATE UNIQUE INDEX temp.scan_dirents_edge ON scan_dirents(parent_dev,parent_inode,name);
         CREATE TEMP TABLE scan_inodes AS
         SELECT dev AS dev_id,inode,MAX(kind) AS kind,MAX(blocks) AS blocks,
                MAX(blocks) AS recursive_bytes,1 AS recursive_inodes,
                MAX(uid) AS uid,MAX(mtime) AS mtime,MAX(birth_ns) AS birth_ns
         FROM scan_v6_nodes GROUP BY dev,inode;
         CREATE UNIQUE INDEX temp.scan_inodes_id ON scan_inodes(dev_id,inode);
         CREATE TEMP TABLE scan_rollup(
           dev INTEGER NOT NULL,inode INTEGER NOT NULL,bytes INTEGER NOT NULL,items INTEGER NOT NULL,
           PRIMARY KEY(dev,inode)
         ) WITHOUT ROWID;"
    )?;
    let max_depth: i64 = conn.query_row(
        "SELECT COALESCE(MAX(depth),0) FROM scan_v6_nodes",
        [],
        |r| r.get(0),
    )?;
    for depth in (1..=max_depth).rev() {
        ensure_not_cancelled(cancel)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM scan_rollup", [])?;
        tx.execute(
            "INSERT INTO scan_rollup(dev,inode,bytes,items)
             SELECT n.parent_dev,n.parent_inode,
                    COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_bytes ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_inodes ELSE 0 END),0)
             FROM scan_v6_nodes n
             JOIN scan_dirents d ON d.parent_dev=n.parent_dev AND d.parent_inode=n.parent_inode AND d.name=n.name
             JOIN scan_inodes i ON i.dev_id=n.dev AND i.inode=n.inode
             WHERE n.depth=?1 AND NOT(n.dev=n.parent_dev AND n.inode=n.parent_inode)
             GROUP BY n.parent_dev,n.parent_inode", [depth],
        )?;
        tx.execute(
            "UPDATE scan_inodes SET
               recursive_bytes=blocks+COALESCE((SELECT bytes FROM scan_rollup r WHERE r.dev=dev_id AND r.inode=scan_inodes.inode),0),
               recursive_inodes=1+COALESCE((SELECT items FROM scan_rollup r WHERE r.dev=dev_id AND r.inode=scan_inodes.inode),0)
             WHERE EXISTS(SELECT 1 FROM scan_rollup r WHERE r.dev=dev_id AND r.inode=scan_inodes.inode)", [],
        )?;
        tx.commit()?;
    }
    if complete {
        let tx = conn.transaction()?;
        // Remove vanished keys first to free unique path/prime constraints.
        tx.execute("DELETE FROM dir_totals WHERE NOT EXISTS(SELECT 1 FROM scan.dir_totals s WHERE s.path_id=dir_totals.path_id)", [])?;
        tx.execute("DELETE FROM paths WHERE NOT EXISTS(SELECT 1 FROM scan.paths s WHERE s.path_id=paths.path_id)", [])?;
        tx.execute("UPDATE paths SET prime=0 WHERE prime=1 AND EXISTS(
             SELECT 1 FROM scan.paths s WHERE s.dev_id=paths.dev_id AND s.inode=paths.inode AND s.birth_ns=paths.birth_ns AND s.prime=1 AND s.path_id<>paths.path_id)", [])?;
        tx.execute_batch(
            "INSERT INTO paths(path_id,parent_path_id,name,dev_id,inode,birth_ns,kind,mount_id,prime,cycle)
             SELECT path_id,parent_path_id,name,dev_id,inode,birth_ns,kind,mount_id,prime,cycle FROM scan.paths
             WHERE 1 ON CONFLICT(path_id) DO UPDATE SET parent_path_id=excluded.parent_path_id,
               name=excluded.name,dev_id=excluded.dev_id,inode=excluded.inode,birth_ns=excluded.birth_ns,
               kind=excluded.kind,mount_id=excluded.mount_id,prime=excluded.prime,cycle=excluded.cycle
             WHERE paths.parent_path_id IS NOT excluded.parent_path_id OR paths.name IS NOT excluded.name
                OR paths.dev_id IS NOT excluded.dev_id OR paths.inode IS NOT excluded.inode
                OR paths.birth_ns IS NOT excluded.birth_ns OR paths.kind IS NOT excluded.kind
                OR paths.mount_id IS NOT excluded.mount_id OR paths.prime IS NOT excluded.prime OR paths.cycle IS NOT excluded.cycle;
             INSERT INTO dir_totals(path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes)
             SELECT path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes FROM scan.dir_totals
             WHERE 1 ON CONFLICT(path_id) DO UPDATE SET visible_bytes=excluded.visible_bytes,
               visible_entries=excluded.visible_entries,unique_bytes=excluded.unique_bytes,unique_inodes=excluded.unique_inodes
             WHERE dir_totals.visible_bytes IS NOT excluded.visible_bytes OR dir_totals.visible_entries IS NOT excluded.visible_entries
                OR dir_totals.unique_bytes IS NOT excluded.unique_bytes OR dir_totals.unique_inodes IS NOT excluded.unique_inodes;
             INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             SELECT dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns FROM scan_inodes
             WHERE 1 ON CONFLICT(dev_id,inode) DO UPDATE SET kind=excluded.kind,blocks=excluded.blocks,
               recursive_bytes=excluded.recursive_bytes,recursive_inodes=excluded.recursive_inodes,
               uid=excluded.uid,mtime=excluded.mtime,birth_ns=excluded.birth_ns
             WHERE inodes.kind IS NOT excluded.kind OR inodes.blocks IS NOT excluded.blocks
                OR inodes.recursive_bytes IS NOT excluded.recursive_bytes OR inodes.recursive_inodes IS NOT excluded.recursive_inodes
                OR inodes.uid IS NOT excluded.uid OR inodes.mtime IS NOT excluded.mtime OR inodes.birth_ns IS NOT excluded.birth_ns;
             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             SELECT parent_dev,parent_inode,name,dev,inode,prime FROM scan_dirents
             WHERE 1 ON CONFLICT(parent_dev,parent_inode,name) DO UPDATE SET dev_id=excluded.dev_id,inode=excluded.inode,prime=excluded.prime
             WHERE dirents.dev_id IS NOT excluded.dev_id OR dirents.inode IS NOT excluded.inode OR dirents.prime IS NOT excluded.prime;
             DELETE FROM dirents WHERE NOT EXISTS(SELECT 1 FROM scan_dirents s
               WHERE s.parent_dev=dirents.parent_dev AND s.parent_inode=dirents.parent_inode AND s.name=dirents.name);
             DELETE FROM inodes WHERE NOT EXISTS(SELECT 1 FROM scan_inodes s WHERE s.dev_id=inodes.dev_id AND s.inode=inodes.inode);
             INSERT INTO meta(key,value) SELECT 'path_id_next',CAST(next_id AS TEXT) FROM scan.scan_path_allocator WHERE 1
               ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
        )?;
        for (key, value) in [
            ("scan_generation", format!("{}-{nonce}", std::process::id())),
            ("last_scan_root", root.to_string_lossy().into_owned()),
            (
                "last_scan_root_bytes",
                crate::store::encode_path_bytes(root),
            ),
            ("root_dev", root_dev.to_string()),
            ("root_inode", root_inode.to_string()),
        ] {
            tx.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![key, value],
            )?;
        }
        crate::fault::checkpoint("scan.in_place_before_commit");
        tx.commit()?;
        crate::fault::checkpoint("scan.in_place_after_commit");
    } else {
        conn.execute("INSERT INTO meta(key,value) VALUES('last_scan_attempt_ts',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [scan_time.to_string()])?;
    }
    let total: i64 = conn.query_row(
        "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
        [root_dev, root_inode],
        |r| r.get(0),
    )?;
    Ok(total)
}

fn record_last_scan_completion(
    conn: &Connection,
    scan_started_at: i64,
    scan_completed_at: i64,
) -> Result<i64> {
    // Wall-clock adjustments must not make a completed scan appear to predate
    // its start. Keep the completion instant where possible; clamp only when
    // the system clock moved backwards during the scan.
    let completed_at = scan_completed_at.max(scan_started_at);
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('last_scan_ts',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [completed_at.to_string()],
    )?;
    Ok(completed_at)
}

/// Parallel directory walk (jwalk). All per-entry stat work happens on worker
/// threads; nodes stream back over a channel. Pseudo-fs, excludes, and (with
/// one_file_system) other mounts are pruned so we never descend into them.
#[allow(clippy::too_many_arguments)]
fn parallel_stage(
    stage: &mut Connection,
    root: &Path,
    root_dev: i64,
    root_inode: i64,
    root_mount_id: Option<i64>,
    index_db: &Path,
    opts: &ScanOptions,
    n_files: &Arc<AtomicU64>,
    n_dirs: &Arc<AtomicU64>,
    n_bytes: &Arc<AtomicI64>,
    n_errors: &Arc<AtomicU64>,
) -> Result<()> {
    use jwalk::WalkDirGeneric;

    let cancel = opts.cancel.clone();
    if let Some(flag) = cancel.as_ref() {
        let flag = Arc::clone(flag);
        stage.progress_handler(10_000, Some(move || flag.load(Ordering::Relaxed)))?;
    }

    // Backpressure bounds queued names/metadata even when storage is slower than
    // stat workers. The producer runs concurrently so a full channel cannot deadlock.
    const QUEUE_CAP: usize = 8192;
    const INSERT_BATCH: usize = 8192;
    let (tx, rx) = crossbeam_channel::bounded::<RawNode>(QUEUE_CAP);
    // canonicalize excludes so relative paths match the canonical entry paths
    let exclude: Vec<PathBuf> = opts
        .exclude
        .iter()
        .map(|p| {
            p.canonicalize().unwrap_or_else(|_| {
                // A nonexistent or relative exclude can't be canonicalized. Still
                // absolutize it against cwd so it can prefix-match the absolute
                // entry paths — a raw relative path never would, silently
                // excluding nothing.
                if p.is_absolute() {
                    p.clone()
                } else {
                    std::env::current_dir()
                        .map(|c| c.join(p))
                        .unwrap_or_else(|_| p.clone())
                }
            })
        })
        .collect();
    let include_pseudo = opts.include_pseudo;
    let one_fs = opts.one_file_system;
    let index_db = index_db.to_path_buf();
    let nf = n_files.clone();
    let nd = n_dirs.clone();
    let nb = n_bytes.clone();
    let ne = n_errors.clone();
    let walk_errors = n_errors.clone();

    let threads = scan_threads(opts);

    // Use path-specific ancestor chains to stop only actual traversal cycles.
    // Alias subtrees still enter disk-backed scan_nodes for the v7 graph; the
    // v6 projection deterministically collapses duplicate edges. Repeated
    // directory identities are counted from SQLite after the walk, avoiding a
    // per-directory in-memory alias map.
    // For the pseudo-fs check below: normally we only statfs at a mount boundary
    // (child dev != parent dev), but if the scan root is ITSELF a pseudo fs we
    // must check every subdir to preserve the "exclude pseudo contents" behavior.
    let root_pseudo = is_pseudo_fs(root);
    let walk_cancel = cancel.clone();
    let callback_cancel = walk_cancel.clone();
    let next_path_id = Arc::new(AtomicI64::new(2));
    let path_ids = Arc::clone(&next_path_id);
    let scan_root = root.to_path_buf();
    let priority_children = Arc::new(priority_child_names(root, &opts.priority_paths));
    let callback_priority_children = Arc::clone(&priority_children);
    #[cfg(test)]
    let callback_test_after_entry_stat = opts.test_after_entry_stat.clone();

    std::thread::scope(|scope| -> Result<()> {
        let producer = scope.spawn(move || {
            {
                let walk = WalkDirGeneric::<ScanPathState>::new(&scan_root)
                    .skip_hidden(false)
                    .follow_links(false)
                    .parallelism(jwalk::Parallelism::RayonNewPool(threads))
                    .root_read_dir_state(ScanPathInfo {
                        path_id: 1,
                        mount_id: root_mount_id,
                        ancestors: None,
                    })
                    .process_read_dir(move |depth, dir_path, parent_path_info, children| {
                        if cancellation_requested(&callback_cancel) {
                            children.clear();
                            return;
                        }
                        // jwalk invokes this callback once for its synthetic root
                        // entry, passing that entry's parent. For `/` the parent
                        // is an empty path; processing it drops the root entry,
                        // so the walk silently indexes nothing. The root entry is
                        // already handled by the normal iteration below.
                        if depth.is_none() {
                            return;
                        }
                        // jwalk's callback runs before it schedules child
                        // directories. Put branches containing recent hints at
                        // the front; all entries are still visited this pass.
                        if let Some(priority_names) = callback_priority_children.get(dir_path) {
                            promote_priority(children, |child| {
                                child.as_ref().ok().is_some_and(|entry| {
                                    priority_names.contains(entry.file_name())
                                })
                            });
                        }
                        // Stat the directory once to learn the parent (dev,inode); all of
                        // these children share it. The parent path ID is per occurrence,
                        // carried in jwalk state rather than reconstructed from a pathname.
                        let parent_path_id = parent_path_info.path_id;
                        if parent_path_id <= 0 {
                            ne.fetch_add(1, Ordering::Relaxed);
                            children.clear();
                            return;
                        }
                        let pinfo = match std::fs::symlink_metadata(dir_path) {
                            Ok(metadata) => crate::util::indexed_identity(&metadata).ok(),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                // The walker enumerated this directory, but it
                                // vanished before its children were attributed.
                                // Its current subtree is absent, so this is a
                                // normal concurrent-delete race.
                                children.clear();
                                return;
                            }
                            Err(error) => {
                                let prior = ne.fetch_add(1, Ordering::Relaxed);
                                if prior < 32 {
                                    tracing::warn!(path = %dir_path.display(), %error, "could not stat directory while scanning its children");
                                }
                                None
                            }
                        };
                        if pinfo.is_none() {
                            // Never attribute children to the scan root when
                            // their real parent cannot be identified.
                            let prior = ne.fetch_add(1, Ordering::Relaxed);
                            if prior < 32 {
                                tracing::warn!(path = %dir_path.display(), "could not stat directory while scanning its children");
                            }
                            children.clear();
                            return;
                        }
                        let (pdev, pino) = pinfo.unwrap_or((root_dev, root_inode));
                        let child_ancestors =
                            extend_ancestors(&parent_path_info.ancestors, (pdev, pino));
                        let cdepth = dir_path.components().count() as u32 + 1;
                        children.retain_mut(|res| {
                            if cancellation_requested(&callback_cancel) {
                                return false;
                            }
                            let entry = match res {
                                Ok(e) => e,
                                Err(error) => {
                                    let prior = ne.fetch_add(1, Ordering::Relaxed);
                                    if prior < 32 {
                                        tracing::warn!(path = %dir_path.display(), %error, "walker returned an unreadable child entry");
                                    }
                                    return false;
                                }
                            };
                            let path = entry.path();
                            if crate::util::is_index_artifact(&path, &index_db)
                                || exclude.iter().any(|x| path.starts_with(x))
                            {
                                return false;
                            }
                            let m = match std::fs::symlink_metadata(&path) {
                                Ok(m) => m,
                                Err(e) => {
                                    if e.kind() != std::io::ErrorKind::NotFound {
                                        let prior = ne.fetch_add(1, Ordering::Relaxed);
                                        if prior < 32 {
                                            tracing::warn!(path = %path.display(), %e, "could not stat entry during scan");
                                        }
                                    }
                                    return false;
                                }
                            };
                            let (dev, ino) = match crate::util::indexed_identity(&m) {
                                Ok(identity) => identity,
                                Err(error) => {
                                    let prior = ne.fetch_add(1, Ordering::Relaxed);
                                    if prior < 32 {
                                        tracing::warn!(
                                            path = %path.display(),
                                            %error,
                                            "filesystem identity cannot be represented during scan"
                                        );
                                    }
                                    return false;
                                }
                            };
                            let is_dir = m.is_dir();
                            if one_fs && dev != root_dev {
                                return false;
                            }
                            // Only statfs at a MOUNT boundary (child dev != parent dev) —
                            // pseudo filesystems are always mounts, so this skips a statfs
                            // syscall on every ordinary subdir (millions on a deep tree)
                            // with no loss of coverage. `root_pseudo` forces the check when
                            // the scan root itself is a pseudo fs.
                            if is_dir && !include_pseudo {
                                let nested_mount = dev != pdev;
                                if ((nested_mount || root_pseudo) && is_pseudo_fs(&path))
                                    || (nested_mount && is_duplicate_storage_view(&path))
                                {
                                    return false;
                                }
                            }
                            let blocks = match crate::util::allocated_bytes(&m) {
                                Ok(blocks) => blocks,
                                Err(error) => {
                                    let prior = ne.fetch_add(1, Ordering::Relaxed);
                                    if prior < 32 {
                                        tracing::warn!(
                                            path = %path.display(),
                                            %error,
                                            "allocated size cannot be represented during scan"
                                        );
                                    }
                                    return false;
                                }
                            };
                            let kind = if is_dir {
                                'd'
                            } else if m.file_type().is_symlink() {
                                'l'
                            } else if m.is_file() {
                                'f'
                            } else {
                                'o'
                            };
                            // The walker also surfaces the scan root as a child of its real
                            // parent dir. Don't emit or count that duplicate (the canonical
                            // self-parented root is added separately) — but still recurse
                            // into it so the real subtree is walked.
                            if path == scan_root {
                                return true;
                            }
                            let Some(path_id) = allocate_scan_path_id(&path_ids) else {
                                ne.fetch_add(1, Ordering::Relaxed);
                                return false;
                            };
                            let (birth_ns, mount_id) =
                                crate::util::inode_path_metadata_at(&m, &path);
                            #[cfg(test)]
                            if let Some(hook) = callback_test_after_entry_stat.as_ref() {
                                hook(&path);
                            }
                            let is_cycle = is_dir && has_ancestor(&child_ancestors, (dev, ino));
                            entry.client_state = ScanPathInfo {
                                path_id,
                                mount_id,
                                ancestors: child_ancestors.clone(),
                            };
                            let name = entry.file_name().as_bytes().to_vec();
                            let sent = tx
                                .send(RawNode {
                                    path_id,
                                    parent_path_id,
                                    mount_id,
                                    dev,
                                    inode: ino,
                                    parent_dev: pdev,
                                    parent_inode: pino,
                                    depth: cdepth,
                                    kind,
                                    blocks,
                                    uid: m.uid() as i64,
                                    mtime: m.mtime(),
                                    birth_ns,
                                    cycle: is_cycle,
                                    name,
                                })
                                .is_ok();
                            // The staging writer failed/cancelled (most commonly
                            // disk full). Stop descending immediately; otherwise
                            // a failed consumer would still stat the entire tree.
                            if !sent {
                                return false;
                            }
                            if is_dir {
                                nd.fetch_add(1, Ordering::Relaxed);
                            } else {
                                nf.fetch_add(1, Ordering::Relaxed);
                            }
                            let _ = nb.try_update(
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                                |total| Some(total.saturating_add(blocks)),
                            );
                            // keep only directories so jwalk recurses; files already sent
                            is_dir && !is_cycle
                        });
                    });
                // jwalk attaches failures opening a directory to the yielded
                // entry, outside process_read_dir. Ignoring these would label
                // a partial index clean after EACCES or an unavailable mount.
                for result in walk {
                    if cancellation_requested(&walk_cancel) {
                        break;
                    }
                    match result {
                        Ok(entry) => {
                            if let Some(ref error) = entry.read_children_error {
                                if !scan_race_is_vanished(error.io_error()) {
                                    let prior = walk_errors.fetch_add(1, Ordering::Relaxed);
                                    if prior < 32 {
                                        tracing::warn!(path = %entry.path().display(), %error, "could not read directory during scan");
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            if !scan_race_is_vanished(error.io_error()) {
                                let prior = walk_errors.fetch_add(1, Ordering::Relaxed);
                                if prior < 32 {
                                    tracing::warn!(%error, "filesystem walker could not read an entry");
                                }
                            }
                        }
                    }
                }
            } // walk + its tx clones dropped here -> channel closes
        });

        let mut batch = Vec::with_capacity(INSERT_BATCH);
        let mut write_error = None;
        let mut cancelled = false;
        loop {
            if cancellation_requested(&cancel) {
                cancelled = true;
                break;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(node) => batch.push(node),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    if !batch.is_empty() {
                        if let Err(e) = insert_stage_batch(stage, &mut batch, &cancel) {
                            write_error = Some(e);
                        }
                    }
                    break;
                }
            }
            if batch.len() >= INSERT_BATCH {
                if let Err(e) = insert_stage_batch(stage, &mut batch, &cancel) {
                    write_error = Some(e);
                    break;
                }
            }
        }
        // Wake any producer blocked in send before joining it. This is essential
        // on ENOSPC/SQLite failure: keeping the receiver alive would deadlock.
        drop(rx);
        producer
            .join()
            .map_err(|_| anyhow::anyhow!("scan worker panicked"))?;
        if cancelled || cancellation_requested(&cancel) {
            return Err(ScanCancelled.into());
        }
        if let Some(e) = write_error {
            return Err(e);
        }
        Ok(())
    })
}

fn insert_stage_batch(
    stage: &mut Connection,
    batch: &mut Vec<RawNode>,
    cancel: &Option<Arc<AtomicBool>>,
) -> Result<()> {
    let tx = stage.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO scan_nodes
             (path_id,parent_path_id,mount_id,dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name,birth_ns,cycle)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        )?;
        for (i, n) in batch.iter().enumerate() {
            if i % 256 == 0 {
                ensure_not_cancelled(cancel)?;
            }
            stmt.execute(params![
                n.path_id,
                n.parent_path_id,
                n.mount_id,
                n.dev,
                n.inode,
                n.parent_dev,
                n.parent_inode,
                n.depth,
                n.kind.to_string(),
                n.blocks,
                n.uid,
                n.mtime,
                n.name,
                n.birth_ns,
                n.cycle,
            ])?;
        }
    }
    tx.commit()?;
    batch.clear();
    Ok(())
}

fn set_low_priority() {
    unsafe {
        // best-effort: nice + idle IO class
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    // IO priority (ioprio_set IDLE) is best-effort via syscall.
    #[cfg(target_os = "linux")]
    unsafe {
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        const IOPRIO_CLASS_IDLE: libc::c_int = 3;
        let ioprio = IOPRIO_CLASS_IDLE << 13;
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, ioprio);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_scan_timestamp_records_completion_not_start() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();

        let recorded = record_last_scan_completion(&conn, 100, 145).unwrap();

        assert_eq!(recorded, 145);
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='last_scan_ts'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "145"
        );
    }

    #[test]
    fn completed_scan_timestamp_clamps_clock_rollback_to_scan_start() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();

        let recorded = record_last_scan_completion(&conn, 100, 90).unwrap();

        assert_eq!(recorded, 100);
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='last_scan_ts'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "100"
        );
    }

    #[test]
    fn incomplete_projection_skips_discarded_rollups_and_keeps_published_total() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE inodes(dev_id INTEGER NOT NULL,inode INTEGER NOT NULL,recursive_bytes INTEGER NOT NULL,
               PRIMARY KEY(dev_id,inode));
             INSERT INTO inodes VALUES(7,11,123456);",
        )
        .unwrap();

        let total = reconcile_staged_projection(
            &mut conn,
            7,
            11,
            99,
            123,
            Path::new("/fixture"),
            &None,
            false,
        )
        .unwrap();

        assert_eq!(
            total, 123456,
            "an incomplete scan must report the last published total"
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='last_scan_attempt_ts'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "99"
        );
        let discarded_projection_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE name IN ('scan_dirents','scan_inodes','scan_rollup')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            discarded_projection_tables, 0,
            "incomplete scans must not build rollup tables that cannot be published"
        );
    }

    #[test]
    fn scan_priority_hints_map_only_rooted_child_names() {
        let root = Path::new("/tree");
        let hints = vec![PathBuf::from("/tree/a/b"), PathBuf::from("/outside/x")];
        let prioritized = priority_child_names(root, &hints);
        assert!(prioritized[Path::new("/tree")].contains(std::ffi::OsStr::new("a")));
        assert!(prioritized[Path::new("/tree/a")].contains(std::ffi::OsStr::new("b")));
        assert!(!prioritized.contains_key(Path::new("/outside")));
    }

    #[test]
    fn scan_priority_moves_hint_branches_forward_without_dropping_siblings() {
        let prioritized =
            priority_child_names(Path::new("/tree"), &[PathBuf::from("/tree/changed/deep")]);
        let mut children = vec![
            std::ffi::OsString::from("unrelated"),
            std::ffi::OsString::from("changed"),
            std::ffi::OsString::from("other"),
        ];
        promote_priority(&mut children, |child| {
            prioritized[Path::new("/tree")].contains(child.as_os_str())
        });
        assert_eq!(children[0], std::ffi::OsString::from("changed"));
        assert_eq!(children.len(), 3);
        assert!(children.contains(&std::ffi::OsString::from("unrelated")));
        assert!(children.contains(&std::ffi::OsString::from("other")));
    }

    #[test]
    fn vanished_scan_entries_do_not_count_as_coverage_errors() {
        let vanished = std::io::Error::from(std::io::ErrorKind::NotFound);
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let other = std::io::Error::from(std::io::ErrorKind::Other);
        assert!(scan_race_is_vanished(Some(&vanished)));
        assert!(!scan_race_is_vanished(Some(&denied)));
        assert!(!scan_race_is_vanished(Some(&other)));
        assert!(!scan_race_is_vanished(None));
    }

    #[test]
    fn stale_scan_cleanup_removes_only_generated_regular_stage_files() {
        let dir = std::env::temp_dir().join(format!(
            "dux-stale-stage-cleanup-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let staging_db = dir.join("dux.db.new");
        let stale = dir.join("dux.db.new.scan-123-456.tmp");
        let stale_wal = dir.join("dux.db.new.scan-123-456.tmp-wal");
        let unrelated = dir.join("dux.db.new.scan-not-a-stage.tmp");
        let malformed = dir.join("dux.db.new.scan-123-456.backup");
        let directory = dir.join("dux.db.new.scan-123-456.tmp-shm");
        let symlink = dir.join("dux.db.new.scan-123-456.tmp-journal");
        std::fs::write(&stale, b"partial stage").unwrap();
        std::fs::write(&stale_wal, b"stale wal").unwrap();
        std::fs::write(&unrelated, b"user data").unwrap();
        std::fs::write(&malformed, b"user backup").unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::os::unix::fs::symlink(&unrelated, &symlink).unwrap();

        cleanup_stale_scan_files(&staging_db).unwrap();

        assert!(!stale.exists());
        assert!(!stale_wal.exists());
        assert!(unrelated.exists());
        assert!(malformed.exists());
        assert!(directory.is_dir());
        assert!(symlink.symlink_metadata().unwrap().file_type().is_symlink());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_scan_cleanup_allows_a_not_yet_created_database_parent() {
        let dir = std::env::temp_dir().join(format!(
            "dux-stale-stage-missing-{}-{}",
            std::process::id(),
            now_secs()
        ));
        cleanup_stale_scan_files(&dir.join("dux.db.new")).unwrap();
    }

    #[test]
    fn stale_scan_cleanup_propagates_non_absence_directory_errors() {
        let dir = std::env::temp_dir().join(format!(
            "dux-stale-stage-not-directory-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let parent_file = dir.join("parent-file");
        std::fs::write(&parent_file, b"not a directory").unwrap();

        let error = cleanup_stale_scan_files(&parent_file.join("dux.db.new")).unwrap_err();
        assert!(error.to_string().contains("listing scan staging directory"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staged_alias_count_uses_bounded_sqlite_state() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(kind TEXT,dev INTEGER,inode INTEGER,birth_ns INTEGER,cycle INTEGER);
             INSERT INTO scan_nodes VALUES
               ('d',1,1,100,0),('d',1,2,200,0),('d',1,2,200,0),('f',1,3,300,0),('f',1,3,300,0);",
        )
        .unwrap();
        assert_eq!(count_repeated_directory_identities(&conn).unwrap(), 1);
    }

    #[test]
    fn staged_v7_path_graph_preserves_aliases_and_cycle_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER PRIMARY KEY,stable_order INTEGER NOT NULL DEFAULT 0,
               parent_path_id INTEGER,mount_id INTEGER,
               dev INTEGER,inode INTEGER,parent_dev INTEGER,parent_inode INTEGER,
               depth INTEGER,kind TEXT,name BLOB,blocks INTEGER,birth_ns INTEGER,cycle INTEGER
             );
             INSERT INTO scan_nodes(path_id,parent_path_id,mount_id,dev,inode,parent_dev,parent_inode,depth,kind,name,blocks,birth_ns,cycle) VALUES
               (1,-1,1,1,1,1,1,0,'d',X'2f',4096,1000,0),
               (2,1,7,2,10,1,1,1,'d',X'6f726967696e616c',4096,2000,0),
               (3,2,7,2,11,2,10,2,'d',X'6e6573746564',4096,3000,0),
               (4,1,8,2,10,1,1,1,'d',X'616c696173',4096,2000,0),
               (5,4,8,2,11,2,10,2,'d',X'6e6573746564',4096,3000,0),
               (6,3,7,2,42,2,11,3,'f',X'70726f6265',8192,4000,0),
               (7,5,8,2,42,2,11,3,'f',X'70726f6265',8192,4000,0),
               (8,5,8,1,1,2,11,3,'d',X'6c6f6f70',4096,1000,1),
               (9,1,8,2,42,1,1,1,'f',X'6f746865722d70726f6265',8192,5000,0);
             UPDATE scan_nodes SET stable_order=path_id;",
        )
        .unwrap();
        conn.execute_batch(crate::store::PATH_GRAPH_TABLES_SQL)
            .unwrap();

        let (paths, alias_paths) = build_and_validate_staged_path_graph(&conn).unwrap();
        assert_eq!(paths, 9);
        assert_eq!(alias_paths, 2);
        assert_eq!(count_repeated_directory_identities(&conn).unwrap(), 2);
        assert_eq!(count_cycle_paths(&conn).unwrap(), 1);
        let file_incarnations: Vec<(i64, i64, i64)> = conn
            .prepare(
                "SELECT birth_ns,COUNT(*),SUM(prime) FROM paths
                 WHERE dev_id=2 AND inode=42 GROUP BY birth_ns ORDER BY birth_ns",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            file_incarnations,
            vec![(4000, 2, 1), (5000, 1, 1)],
            "same dev/inode with different birth times are distinct incarnations; hardlinks within one incarnation share a prime path",
        );
        assert_eq!(
            conn.query_row("SELECT cycle FROM paths WHERE path_id=8", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            crate::store::path_graph_resolve(&conn, 7).unwrap().unwrap(),
            Path::new("/alias/nested/probe")
        );
        let totals = build_staged_path_totals(&mut conn).unwrap();
        assert_eq!(totals, (49_152, 9, 28_672, 5));
        let alias_totals: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT visible_bytes,visible_entries,unique_bytes,unique_inodes
                 FROM dir_totals WHERE path_id=4",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(alias_totals, (20_480, 4, 0, 0));
    }

    #[test]
    fn prime_attribution_ignores_parallel_allocation_order() {
        fn run(ids: [i64; 5]) -> (PathBuf, PathBuf, (i64, i64, i64, i64)) {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE scan_nodes (
                   path_id INTEGER PRIMARY KEY CHECK(path_id>0),
                   stable_order INTEGER NOT NULL DEFAULT 0, parent_path_id INTEGER NOT NULL,
                   mount_id INTEGER, dev INTEGER NOT NULL, inode INTEGER NOT NULL,
                   parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL,
                   depth INTEGER NOT NULL, kind TEXT NOT NULL, blocks INTEGER NOT NULL,
                   uid INTEGER NOT NULL, mtime INTEGER NOT NULL, name BLOB NOT NULL,
                   birth_ns INTEGER NOT NULL, cycle INTEGER NOT NULL DEFAULT 0,
                   UNIQUE(parent_path_id,name)
                 );",
            )
            .unwrap();
            let rows = [
                (
                    ids[0],
                    -1,
                    1,
                    1,
                    1,
                    1,
                    1,
                    0,
                    "d",
                    4096,
                    0,
                    1,
                    b"/".as_slice(),
                    1,
                    0,
                ),
                (
                    ids[1],
                    ids[0],
                    1,
                    1,
                    2,
                    1,
                    1,
                    1,
                    "d",
                    4096,
                    0,
                    1,
                    b"a".as_slice(),
                    2,
                    0,
                ),
                (
                    ids[2],
                    ids[0],
                    2,
                    1,
                    2,
                    1,
                    1,
                    1,
                    "d",
                    4096,
                    0,
                    1,
                    b"z".as_slice(),
                    2,
                    0,
                ),
                (
                    ids[3],
                    ids[1],
                    1,
                    1,
                    3,
                    1,
                    2,
                    2,
                    "f",
                    8192,
                    0,
                    1,
                    b"item".as_slice(),
                    3,
                    0,
                ),
                (
                    ids[4],
                    ids[2],
                    2,
                    1,
                    3,
                    1,
                    2,
                    2,
                    "f",
                    8192,
                    0,
                    1,
                    b"item".as_slice(),
                    3,
                    0,
                ),
            ];
            for (
                path_id,
                parent_path_id,
                mount_id,
                dev,
                inode,
                parent_dev,
                parent_inode,
                depth,
                kind,
                blocks,
                uid,
                mtime,
                name,
                birth_ns,
                cycle,
            ) in rows
            {
                conn.execute(
                    "INSERT INTO scan_nodes(path_id,parent_path_id,mount_id,dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name,birth_ns,cycle) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    rusqlite::params![path_id,parent_path_id,mount_id,dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name,birth_ns,cycle],
                ).unwrap();
            }

            let mut conn = conn;
            assign_stable_scan_path_order(&mut conn).unwrap();
            validate_scan_path_stage(&conn, 1, 1).unwrap();
            conn.execute_batch(crate::store::PATH_GRAPH_TABLES_SQL)
                .unwrap();
            build_and_validate_staged_path_graph(&conn).unwrap();
            let dir_id: i64 = conn
                .query_row(
                    "SELECT path_id FROM paths WHERE dev_id=1 AND inode=2 AND birth_ns=2 AND prime=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let file_id: i64 = conn
                .query_row(
                    "SELECT path_id FROM paths WHERE dev_id=1 AND inode=3 AND birth_ns=3 AND prime=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let dir_path = crate::store::path_graph_resolve(&conn, dir_id)
                .unwrap()
                .unwrap();
            let file_path = crate::store::path_graph_resolve(&conn, file_id)
                .unwrap()
                .unwrap();
            let totals = build_staged_path_totals(&mut conn).unwrap();
            (dir_path, file_path, totals)
        }

        let first = run([1, 2, 3, 4, 5]);
        // Reverse alias allocation order while keeping each parent before its children.
        let second = run([1, 3, 2, 5, 4]);
        assert_eq!(first, second);
        assert_eq!(first.0, Path::new("/a"));
        assert_eq!(first.1, Path::new("/a/item"));
        assert_eq!(first.2, (28_672, 5, 16_384, 3));
    }

    #[test]
    fn conflicting_inode_parent_edges_fail_scan_coverage_check() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(parent_dev INTEGER,parent_inode INTEGER,name BLOB,dev INTEGER,inode INTEGER,birth_ns INTEGER);
             INSERT INTO scan_nodes VALUES
               (1,10,X'78',1,20,100),
               (1,10,X'78',1,21,100),
               (1,10,X'79',1,30,100),
               (1,10,X'79',1,30,100);",
        )
        .unwrap();
        assert_eq!(count_conflicting_inode_edges(&conn).unwrap(), 1);
    }

    #[test]
    fn scan_detects_allocation_changes_between_inode_path_instances() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(dev INTEGER,inode INTEGER,birth_ns INTEGER,blocks INTEGER);
             INSERT INTO scan_nodes VALUES(1,10,100,4096),(1,10,100,8192),(1,11,200,4096);",
        )
        .unwrap();
        assert_eq!(count_conflicting_inode_allocations(&conn).unwrap(), 1);
    }

    #[test]
    fn scan_detects_inode_number_reuse_even_when_allocations_match() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(dev INTEGER,inode INTEGER,birth_ns INTEGER,blocks INTEGER);
             INSERT INTO scan_nodes VALUES(1,10,100,4096),(1,10,200,4096),(1,11,300,4096);",
        )
        .unwrap();
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 1);
        assert_eq!(count_conflicting_inode_allocations(&conn).unwrap(), 0);
    }

    #[test]
    fn inode_reuse_revalidation_prunes_missing_ancestor_subtree() {
        use std::os::unix::ffi::OsStrExt;

        let root = tmp("reuse-prune-ancestor");
        let dead_dir = root.join("dead");
        let nested_dir = dead_dir.join("nested");
        let stale_path = nested_dir.join("old");
        let live_path = root.join("live");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(&stale_path, b"hard-linked inode").unwrap();
        fs::hard_link(&stale_path, &live_path).unwrap();

        let root_meta = fs::symlink_metadata(&root).unwrap();
        let dead_meta = fs::symlink_metadata(&dead_dir).unwrap();
        let nested_meta = fs::symlink_metadata(&nested_dir).unwrap();
        let stale_meta = fs::symlink_metadata(&stale_path).unwrap();
        let live_meta = fs::symlink_metadata(&live_path).unwrap();
        let (root_dev, root_ino) = crate::util::indexed_identity(&root_meta).unwrap();
        let (dead_dev, dead_ino) = crate::util::indexed_identity(&dead_meta).unwrap();
        let (nested_dev, nested_ino) = crate::util::indexed_identity(&nested_meta).unwrap();
        let (file_dev, file_ino) = crate::util::indexed_identity(&live_meta).unwrap();
        assert_eq!(
            crate::util::indexed_identity(&stale_meta).unwrap(),
            (file_dev, file_ino)
        );
        let (live_birth, _) = crate::util::inode_path_metadata_at(&live_meta, &live_path);
        let (stale_birth, _) = crate::util::inode_path_metadata_at(&stale_meta, &stale_path);
        let (root_birth, _) = crate::util::inode_path_metadata_at(&root_meta, &root);
        let (dead_birth, _) = crate::util::inode_path_metadata_at(&dead_meta, &dead_dir);
        let (nested_birth, _) = crate::util::inode_path_metadata_at(&nested_meta, &nested_dir);

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER PRIMARY KEY,parent_path_id INTEGER,depth INTEGER,name BLOB,
               dev INTEGER,inode INTEGER,birth_ns INTEGER,kind TEXT,
               mount_id INTEGER,blocks INTEGER DEFAULT 0,uid INTEGER DEFAULT 0,mtime INTEGER DEFAULT 0
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(1,-1,0,?1,?2,?3,?4,'d')",
            params![root.as_os_str().as_bytes(), root_dev, root_ino, root_birth],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(2,1,1,?1,?2,?3,?4,'d')",
            params![b"dead".as_slice(), dead_dev, dead_ino, dead_birth],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(3,2,2,?1,?2,?3,?4,'d')",
            params![b"nested".as_slice(), nested_dev, nested_ino, nested_birth],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(4,3,3,?1,?2,?3,?4,'f')",
            params![
                b"old".as_slice(),
                file_dev,
                file_ino,
                stale_birth.saturating_add(1)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(5,1,1,?1,?2,?3,?4,'f')",
            params![b"live".as_slice(), file_dev, file_ino, live_birth],
        )
        .unwrap();

        fs::remove_dir_all(&dead_dir).unwrap();
        let removed = revalidate_inode_reuse_paths(&conn, &root).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM scan_nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            removed,
            (3, 0, 0),
            "missing directory and its descendants are pruned"
        );
        assert_eq!(rows, 2, "root and surviving hardlink remain staged");
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 0);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inode_reuse_revalidation_refreshes_present_file_identity_from_stat() {
        use std::os::unix::ffi::OsStrExt;

        let root = tmp("reuse-ambiguous-present");
        let first = root.join("first");
        let second = root.join("second");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(&first, b"same inode").unwrap();
        fs::hard_link(&first, &second).unwrap();
        let root_meta = fs::symlink_metadata(&root).unwrap();
        let file_meta = fs::symlink_metadata(&first).unwrap();
        let (root_dev, root_ino) = crate::util::indexed_identity(&root_meta).unwrap();
        let (file_dev, file_ino) = crate::util::indexed_identity(&file_meta).unwrap();
        let (birth, _) = crate::util::inode_path_metadata_at(&file_meta, &first);

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER PRIMARY KEY,parent_path_id INTEGER,depth INTEGER,name BLOB,
               dev INTEGER,inode INTEGER,birth_ns INTEGER,kind TEXT,
               mount_id INTEGER,blocks INTEGER DEFAULT 0,uid INTEGER DEFAULT 0,mtime INTEGER DEFAULT 0
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(1,-1,0,?1,?2,?3,?4,'d')",
            params![root.as_os_str().as_bytes(), root_dev, root_ino, 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(2,1,1,?1,?2,?3,?4,'f')",
            params![b"first".as_slice(), file_dev, file_ino, birth],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind) VALUES(3,1,1,?1,?2,?3,?4,'f')",
            params![
                b"second".as_slice(),
                file_dev,
                file_ino,
                birth.saturating_add(1)
            ],
        )
        .unwrap();

        assert_eq!(
            revalidate_inode_reuse_paths(&conn, &root).unwrap(),
            (0, 1, 0)
        );
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 0);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM scan_nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            rows, 3,
            "revalidation refreshes but does not drop live paths"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_directory_reuse_stays_incomplete_when_leaf_refresh_clears_conflict_group() {
        use std::os::unix::ffi::OsStrExt;

        let root = tmp("reuse-unresolved-dir");
        let directory = root.join("directory");
        let source = root.join("source");
        let replacement = root.join("replacement");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&source, b"old incarnation").unwrap();
        fs::write(&replacement, b"new current file").unwrap();

        let root_meta = fs::symlink_metadata(&root).unwrap();
        let source_meta = fs::symlink_metadata(&source).unwrap();
        let (root_dev, root_inode) = crate::util::indexed_identity(&root_meta).unwrap();
        let (source_dev, source_inode) = crate::util::indexed_identity(&source_meta).unwrap();
        let (source_birth, _) = crate::util::inode_path_metadata_at(&source_meta, &source);

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER PRIMARY KEY,parent_path_id INTEGER,depth INTEGER,name BLOB,
               dev INTEGER,inode INTEGER,birth_ns INTEGER,kind TEXT,
               mount_id INTEGER,blocks INTEGER DEFAULT 0,uid INTEGER DEFAULT 0,mtime INTEGER DEFAULT 0
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind)
             VALUES(1,-1,0,?1,?2,?3,?4,'d')",
            params![root.as_os_str().as_bytes(), root_dev, root_inode, 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind)
             VALUES(2,1,1,?1,?2,?3,?4,'d')",
            params![
                b"directory".as_slice(),
                source_dev,
                source_inode,
                source_birth.saturating_add(1)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_nodes(path_id,parent_path_id,depth,name,dev,inode,birth_ns,kind)
             VALUES(3,1,1,?1,?2,?3,?4,'f')",
            params![
                b"replacement".as_slice(),
                source_dev,
                source_inode,
                source_birth
            ],
        )
        .unwrap();
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 1);

        let (removed, refreshed, unresolved) = revalidate_inode_reuse_paths(&conn, &root).unwrap();
        assert_eq!((removed, refreshed, unresolved), (0, 1, 1));
        // Refreshing the leaf removes the duplicate-incarnation SQL signal,
        // but the still-present directory mismatch must independently keep
        // the scan incomplete.
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 0);
        let stats = ScanStats {
            errors: unresolved,
            unresolved_reuse_paths: unresolved,
            ..ScanStats::default()
        };
        assert!(incomplete_scan_reason(&stats).contains("1 unresolved inode-reuse paths"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_place_scan_revalidates_replaced_file_after_ext4_inode_reuse_mid_walk() {
        let root = tmp("live-inode-reuse-root");
        let db = tmp("live-inode-reuse-db");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        let parent = root.join("a");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        let old_path = parent.join("old-entry");
        fs::write(&old_path, b"same allocation size").unwrap();

        // This test depends on ext4 exposing stable birth-time identity and
        // promptly reusing freed inode numbers. Exercise it where those kernel
        // semantics exist; the VPS2 qualification image also uses ext4.
        if fs_magic(&root) != Some(0xef53) {
            eprintln!("skipping live inode-reuse scan fixture: test root is not ext4");
            let _ = fs::remove_dir_all(&root);
            rm_db(&db);
            return;
        }
        let old_meta = fs::symlink_metadata(&old_path).unwrap();
        let old_inode = old_meta.ino() as i64;
        let (old_birth_ns, _) = crate::util::inode_path_metadata_at(&old_meta, &old_path);
        if old_birth_ns == 0 {
            eprintln!("skipping live inode-reuse scan fixture: ext4 birth time unavailable");
            let _ = fs::remove_dir_all(&root);
            rm_db(&db);
            return;
        }

        let baseline_opts = ScanOptions {
            include_pseudo: true,
            ..Default::default()
        };
        rebuild_atomic(&db, &root, &baseline_opts).unwrap();

        // Pause immediately after the walker stats old-entry, while the read
        // directory result already contains that name but has not descended
        // into child/. The test then unlinks it and allocates the exact inode
        // number for child/new-entry with a different ext4 birth time.
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let resume_rx = Arc::new(std::sync::Mutex::new(resume_rx));
        let fired = Arc::new(AtomicBool::new(false));
        let hook_path = old_path.clone();
        let hook_resume = Arc::clone(&resume_rx);
        let hook_fired = Arc::clone(&fired);
        let hook = Arc::new(move |path: &Path| {
            if path == hook_path && !hook_fired.swap(true, Ordering::SeqCst) {
                let _ = entered_tx.send(());
                let _ = hook_resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30));
            }
        });
        let scan_opts = ScanOptions {
            include_pseudo: true,
            jobs: Some(2),
            test_after_entry_stat: Some(hook),
            ..Default::default()
        };
        let scan_root = root.clone();
        let scan_db = db.clone();
        let scan_thread = std::thread::spawn(move || {
            let mut store = Store::open_rw(&scan_db).unwrap();
            reconcile_in_place(&mut store, &scan_root, &scan_opts)
        });

        let reached_hook = entered_rx.recv_timeout(Duration::from_secs(30)).is_ok();
        let new_path = child.join("new-entry");
        let mut reused = false;
        let mut candidates = Vec::new();
        if reached_hook {
            fs::remove_file(&old_path).unwrap();
            // Keep nonmatching candidates allocated so ext4 advances through
            // the parent inode group's free bitmap instead of repeatedly
            // reusing the same temporary inode. This private test tree is
            // removed below; the candidate count remains bounded.
            for attempt in 0..20_000 {
                let candidate = parent.join(format!("reuse-candidate-{attempt}"));
                fs::write(&candidate, b"same allocation size").unwrap();
                candidates.push(candidate.clone());
                let metadata = fs::symlink_metadata(&candidate).unwrap();
                let (birth_ns, _) = crate::util::inode_path_metadata_at(&metadata, &candidate);
                if metadata.ino() as i64 == old_inode && birth_ns != 0 && birth_ns != old_birth_ns {
                    fs::rename(&candidate, &new_path).unwrap();
                    reused = true;
                    break;
                }
            }
            // Keep the reused inode alive under new-entry, but remove the
            // nonmatching allocator candidates so the oracle stays tiny.
            for candidate in candidates {
                let _ = fs::remove_file(candidate);
            }
            if reused {
                // Recreate the old name with a different inode while the scan
                // still has its pre-unlink row staged. This reproduces the
                // live case that remained ambiguous: the stale path exists,
                // but its current identity no longer matches the staged one.
                fs::write(&old_path, b"replacement at the original path").unwrap();
            }
        }
        let _ = resume_tx.send(());
        let stats = scan_thread.join().unwrap().unwrap();

        // The walk captured old-entry before unlink and the reused inode under
        // new-entry afterwards. The old name was also recreated with a new
        // inode. Revalidation must refresh that leaf row, then publish both
        // currently visible files without retaining a stale incarnation.
        let prior = Store::open_ro(&db).unwrap();
        let old_path_indexed = crate::store::path_graph_lookup(&prior.conn, &old_path)
            .unwrap()
            .is_some();
        let new_path_indexed = crate::store::path_graph_lookup(&prior.conn, &new_path)
            .unwrap()
            .is_some();
        let expected_paths = [
            root.clone(),
            parent.clone(),
            child.clone(),
            old_path.clone(),
            new_path.clone(),
        ];
        let indexed_paths: i64 = prior
            .conn
            .query_row("SELECT COUNT(*) FROM paths", [], |row| row.get(0))
            .unwrap();
        let mut expected_allocated = 0i64;
        let mut expected_inodes = std::collections::HashSet::new();
        for path in &expected_paths {
            let metadata = fs::symlink_metadata(path).unwrap();
            let identity = crate::util::indexed_identity(&metadata).unwrap();
            let (birth_ns, _) = crate::util::inode_path_metadata_at(&metadata, path);
            let path_id = crate::store::path_graph_lookup(&prior.conn, path)
                .unwrap()
                .unwrap_or_else(|| {
                    panic!(
                        "current filesystem path missing from index: {}",
                        path.display()
                    )
                });
            let indexed_identity: (i64, i64, i64) = prior
                .conn
                .query_row(
                    "SELECT dev_id,inode,birth_ns FROM paths WHERE path_id=?1",
                    [path_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                indexed_identity,
                (identity.0, identity.1, birth_ns),
                "{}",
                path.display()
            );
            if expected_inodes.insert(identity) {
                expected_allocated += crate::util::allocated_bytes(&metadata).unwrap();
            }
        }
        let root_meta = fs::symlink_metadata(&root).unwrap();
        let (root_dev, root_inode) = crate::util::indexed_identity(&root_meta).unwrap();
        let indexed_root_totals: (i64, i64) = prior
            .conn
            .query_row(
                "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                rusqlite::params![root_dev, root_inode],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let dirty_marker_present = prior.get_meta("dirty_since").unwrap().is_some();
        drop(prior);
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);

        assert!(
            reached_hook,
            "scanner did not reach the deterministic race hook"
        );
        assert!(
            reused,
            "ext4 did not reuse inode {old_inode} with a new birth time"
        );
        assert_eq!(stats.conflicting_inode_incarnations, 0);
        assert_eq!(stats.vanished_reuse_paths, 0);
        assert_eq!(stats.revalidated_reuse_paths, 1);
        assert!(
            stats.errors == 0,
            "a vanished path proven stale by revalidation should not poison the scan"
        );
        assert!(
            old_path_indexed,
            "the replacement at the original path was omitted"
        );
        assert!(
            new_path_indexed,
            "the replacement incarnation was not published"
        );
        assert!(
            !dirty_marker_present,
            "a complete revalidated scan left a dirty marker behind"
        );
        assert_eq!(indexed_paths, expected_paths.len() as i64);
        assert_eq!(
            indexed_root_totals,
            (expected_allocated, expected_inodes.len() as i64)
        );
    }

    #[test]
    fn scan_marks_repeated_inode_without_birth_time_as_uncertain() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(dev INTEGER,inode INTEGER,birth_ns INTEGER);
             INSERT INTO scan_nodes VALUES
               (1,10,0),(1,10,0),
               (1,11,0),
               (1,12,100),(1,12,100),
               (1,13,0),(1,13,200);",
        )
        .unwrap();
        assert_eq!(count_unknown_repeated_inode_identities(&conn).unwrap(), 1);
        assert_eq!(count_conflicting_inode_incarnations(&conn).unwrap(), 1);
    }

    #[test]
    fn incomplete_scan_reason_names_aliases_instead_of_calling_them_unreadable() {
        let reason = incomplete_scan_reason(&ScanStats {
            errors: 2,
            repeated_directory_identities: 2,
            conflicting_inode_incarnations: 1,
            unknown_repeated_inode_identities: 1,
            ..ScanStats::default()
        });
        assert!(reason.contains("2 repeated directory identities"));
        assert!(reason.contains("1 conflicting inode incarnations"));
        assert!(reason.contains("1 repeated inode identities with unknown incarnation"));
        assert!(reason.contains("0 unreadable entries"));
        assert!(!reason.contains("scan skipped"));
    }

    #[test]
    fn ancestor_chain_stops_cycles_but_allows_non_ancestor_aliases() {
        let root = extend_ancestors(&None, (1, 10));
        let first_path_to_dir = extend_ancestors(&root, (1, 20));

        assert!(has_ancestor(&first_path_to_dir, (1, 10)));
        assert!(
            !has_ancestor(&root, (1, 20)),
            "a sibling alias is traversable"
        );

        let inside_first_path = extend_ancestors(&first_path_to_dir, (1, 30));
        assert!(has_ancestor(&inside_first_path, (1, 20)));
        assert!(has_ancestor(&inside_first_path, (1, 10)));
        assert!(!has_ancestor(&inside_first_path, (1, 40)));
    }

    #[test]
    fn v6_scan_projection_collapses_alias_edges_and_omits_cycle_edges() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "ATTACH DATABASE ':memory:' AS scan;
             CREATE TABLE scan.scan_nodes(
               path_id INTEGER PRIMARY KEY, stable_order INTEGER, parent_dev INTEGER, parent_inode INTEGER,
               name BLOB, cycle INTEGER, depth INTEGER
             );
             INSERT INTO scan.scan_nodes VALUES
               (1,1,1,1,X'2f',0,0),
               (2,2,1,1,X'61',0,1),
               (3,3,1,2,X'78',0,2),
               (4,4,1,2,X'78',0,3),
               (5,5,1,2,X'6c6f6f70',1,4);",
        )
        .unwrap();

        conn.execute_batch(V6_SCAN_PROJECTION_SQL).unwrap();
        let rows: Vec<(i64, Vec<u8>)> = conn
            .prepare("SELECT path_id,name FROM scan_v6_nodes ORDER BY path_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            vec![(1, b"/".to_vec()), (2, b"a".to_vec()), (3, b"x".to_vec())]
        );
    }

    #[test]
    fn jwalk_hands_each_directory_its_own_parallel_read_state() {
        let root = std::env::temp_dir().join(format!(
            "dux-jwalk-path-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(root.join("a/leaf")).unwrap();
        std::fs::create_dir_all(root.join("b/leaf")).unwrap();
        let next_id = Arc::new(AtomicI64::new(2));
        let observed = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            PathBuf,
            ScanPathInfo,
        >::new()));
        observed.lock().unwrap().insert(
            root.clone(),
            ScanPathInfo {
                path_id: 1,
                mount_id: Some(9),
                ancestors: None,
            },
        );
        let callback_observed = Arc::clone(&observed);
        let callback_ids = Arc::clone(&next_id);
        let walk = jwalk::WalkDirGeneric::<ScanPathState>::new(&root)
            .root_read_dir_state(ScanPathInfo {
                path_id: 1,
                mount_id: Some(9),
                ancestors: None,
            })
            .parallelism(jwalk::Parallelism::RayonNewPool(2))
            .process_read_dir(move |depth, dir_path, read_state, children| {
                if depth.is_none() {
                    return;
                }
                assert_eq!(
                    callback_observed.lock().unwrap().get(dir_path),
                    Some(&*read_state),
                    "child read state must match the path metadata assigned to this directory"
                );
                for entry in children
                    .iter_mut()
                    .filter_map(|result| result.as_mut().ok())
                {
                    if !entry.file_type().is_dir() {
                        continue;
                    }
                    let path_id = allocate_scan_path_id(&callback_ids).unwrap();
                    let path_info = ScanPathInfo {
                        path_id,
                        mount_id: Some(9),
                        ancestors: extend_ancestors(
                            &read_state.ancestors,
                            (read_state.path_id, read_state.path_id),
                        ),
                    };
                    entry.client_state = path_info.clone();
                    callback_observed
                        .lock()
                        .unwrap()
                        .insert(entry.path(), path_info);
                }
            });
        for entry in walk {
            entry.unwrap();
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 5, "root plus four directory instances");
        let ids: std::collections::HashSet<_> =
            observed.values().map(|path| path.path_id).collect();
        assert_eq!(ids.len(), observed.len(), "each path has its own ID");
        drop(observed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scan_path_stage_rejects_missing_or_mismatched_parent_instances() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER NOT NULL, parent_path_id INTEGER NOT NULL,
               depth INTEGER NOT NULL, dev INTEGER NOT NULL, inode INTEGER NOT NULL,
               parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL, kind TEXT NOT NULL,
               name BLOB NOT NULL
             );
             INSERT INTO scan_nodes VALUES(1,-1,0,1,1,1,1,'d',X'2f');
             INSERT INTO scan_nodes VALUES(2,1,1,1,2,1,1,'d',X'61');
             INSERT INTO scan_nodes VALUES(3,2,2,1,3,1,2,'f',X'62');",
        )
        .unwrap();
        validate_scan_path_stage(&conn, 1, 1).unwrap();

        conn.execute("UPDATE scan_nodes SET parent_path_id=1 WHERE path_id=3", [])
            .unwrap();
        let error = validate_scan_path_stage(&conn, 1, 1).unwrap_err();
        assert!(error.to_string().contains("bad_parents=1"));
    }

    #[test]
    fn scan_path_stage_rejects_duplicate_sibling_and_invalid_component_names() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scan_nodes(
               path_id INTEGER NOT NULL, parent_path_id INTEGER NOT NULL,
               depth INTEGER NOT NULL, dev INTEGER NOT NULL, inode INTEGER NOT NULL,
               parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL, kind TEXT NOT NULL,
               name BLOB NOT NULL
             );
             INSERT INTO scan_nodes VALUES(1,-1,0,1,1,1,1,'d',X'2f');
             INSERT INTO scan_nodes VALUES(2,1,1,1,2,1,1,'d',X'61');
             INSERT INTO scan_nodes VALUES(3,1,1,1,3,1,1,'f',X'61');",
        )
        .unwrap();

        let duplicate = validate_scan_path_stage(&conn, 1, 1).unwrap_err();
        assert!(duplicate.to_string().contains("duplicate_names=1"));

        conn.execute("UPDATE scan_nodes SET name=X'612f62' WHERE path_id=3", [])
            .unwrap();
        let invalid = validate_scan_path_stage(&conn, 1, 1).unwrap_err();
        assert!(invalid.to_string().contains("invalid_names=1"));
    }

    #[test]
    fn persistent_path_id_assignment_also_builds_stable_breadth_first_order() {
        let mut stage = Connection::open_in_memory().unwrap();
        stage
            .execute_batch(
                "ATTACH DATABASE ':memory:' AS existing;
                 CREATE TABLE existing.paths(
                   path_id INTEGER PRIMARY KEY, parent_path_id INTEGER NOT NULL,
                   name BLOB NOT NULL, dev_id INTEGER NOT NULL, inode INTEGER NOT NULL,
                   birth_ns INTEGER NOT NULL, kind TEXT NOT NULL, mount_id INTEGER NOT NULL,
                   prime INTEGER NOT NULL, cycle INTEGER NOT NULL,
                   UNIQUE(parent_path_id,name));
                 CREATE INDEX existing.path_parent_name
                   ON paths(parent_path_id,name);
                 CREATE TABLE existing.meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                 INSERT INTO existing.paths VALUES
                   (1,-1,X'2f',1,1,0,'d',-1,1,0),
                   (10,1,X'61',1,2,0,'d',-1,1,0),
                   (11,10,X'78',1,3,0,'f',-1,1,0),
                   (20,1,X'62',1,4,0,'d',-1,1,0);
                 INSERT INTO existing.meta VALUES('path_id_next','21');
                 CREATE TABLE scan_nodes(
                   path_id INTEGER PRIMARY KEY CHECK(path_id>0), stable_order INTEGER NOT NULL DEFAULT 0,
                   parent_path_id INTEGER NOT NULL, mount_id INTEGER,
                   dev INTEGER NOT NULL, inode INTEGER NOT NULL,
                   parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL,
                   depth INTEGER NOT NULL, kind TEXT NOT NULL, blocks INTEGER NOT NULL,
                   uid INTEGER NOT NULL, mtime INTEGER NOT NULL, name BLOB NOT NULL,
                   birth_ns INTEGER NOT NULL, cycle INTEGER NOT NULL DEFAULT 0,
                   UNIQUE(parent_path_id,name));
                 INSERT INTO scan_nodes VALUES
                   (1,0,-1,-1,1,1,1,1,0,'d',8,0,0,X'2f',0,0),
                   (2,0,1,-1,1,4,1,1,1,'d',8,0,0,X'62',0,0),
                   (3,0,1,-1,1,2,1,1,1,'d',8,0,0,X'61',0,0),
                   (4,0,3,-1,1,3,1,2,2,'f',1,0,0,X'78',0,0),
                   (5,0,2,-1,1,6,1,4,2,'f',1,0,0,X'79',0,0);",
            )
            .unwrap();

        assign_persistent_scan_path_ids(&mut stage).unwrap();

        let rows: Vec<(i64, Vec<u8>, i64, i64)> = stage
            .prepare(
                "SELECT path_id,name,stable_order,parent_path_id
                 FROM scan_nodes ORDER BY stable_order",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, b"/".to_vec(), 1, -1),
                (10, b"a".to_vec(), 2, 1),
                (20, b"b".to_vec(), 3, 1),
                (11, b"x".to_vec(), 4, 10),
                (21, b"y".to_vec(), 5, 20),
            ],
            "existing path IDs must be reused while stable order stays breadth-first and lexical",
        );
        assert_eq!(
            stage
                .query_row("SELECT next_id FROM scan_path_allocator", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            22
        );
    }

    #[test]
    fn rpc_and_selinux_filesystems_are_pseudo_filesystems() {
        assert!(is_pseudo_magic(Some(0x6759_6969)));
        assert!(is_pseudo_magic(Some(0xf97c_ff8c)));
        assert!(!is_pseudo_magic(Some(0x5846_5342))); // XFS
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-scan-{tag}-{}", std::process::id()))
    }

    #[test]
    fn in_place_reconciliation_is_noop_when_unchanged_and_tracks_add_delete() {
        let db = tmp("reconcile-db");
        let root = tmp("reconcile-root");
        // The test root may live on tmpfs when `TMPDIR` isolates tests from the
        // watched production filesystem. Include that explicitly pseudo
        // filesystem so this test exercises reconciliation instead of pruning
        // its own fixture tree.
        let opts = ScanOptions {
            include_pseudo: true,
            ..ScanOptions::default()
        };
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("existing")).unwrap();
        fs::write(root.join("existing/keep"), b"stable-data").unwrap();
        rebuild_atomic(&db, &root, &opts).unwrap();

        let mut store = Store::open_rw(&db).unwrap();
        let stable_id = crate::store::path_graph_lookup(&store.conn, &root.join("existing/keep"))
            .unwrap()
            .unwrap();
        store.conn.execute_batch(
            "CREATE TABLE reconcile_write_count(n INTEGER NOT NULL);
             INSERT INTO reconcile_write_count VALUES(0);
             CREATE TRIGGER count_paths_i AFTER INSERT ON paths BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_paths_u AFTER UPDATE ON paths BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_paths_d AFTER DELETE ON paths BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_totals_i AFTER INSERT ON dir_totals BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_totals_u AFTER UPDATE ON dir_totals BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_totals_d AFTER DELETE ON dir_totals BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_inodes_i AFTER INSERT ON inodes BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_inodes_u AFTER UPDATE ON inodes BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_inodes_d AFTER DELETE ON inodes BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_dirents_i AFTER INSERT ON dirents BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_dirents_u AFTER UPDATE ON dirents BEGIN UPDATE reconcile_write_count SET n=n+1; END;
             CREATE TRIGGER count_dirents_d AFTER DELETE ON dirents BEGIN UPDATE reconcile_write_count SET n=n+1; END;",
        ).unwrap();
        reconcile_in_place(&mut store, &root, &opts).unwrap();
        assert_eq!(
            store
                .get_meta("last_reconcile_attempt_result")
                .unwrap()
                .as_deref(),
            Some("complete")
        );
        let writes: i64 = store
            .conn
            .query_row("SELECT n FROM reconcile_write_count", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            writes, 0,
            "identical observations must not rewrite indexed rows"
        );
        assert_eq!(
            crate::store::path_graph_lookup(&store.conn, &root.join("existing/keep")).unwrap(),
            Some(stable_id),
            "path-instance identity must persist across observations",
        );

        fs::write(root.join("aaa-new"), b"new-value").unwrap();
        reconcile_in_place(&mut store, &root, &opts).unwrap();
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("aaa-new"))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            crate::store::path_graph_lookup(&store.conn, &root.join("existing/keep")).unwrap(),
            Some(stable_id),
            "inserting an earlier sibling must not renumber existing paths",
        );

        fs::remove_file(root.join("aaa-new")).unwrap();
        reconcile_in_place(&mut store, &root, &opts).unwrap();
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("aaa-new"))
                .unwrap()
                .is_none()
        );
        fs::hard_link(root.join("existing/keep"), root.join("hardlink")).unwrap();
        reconcile_in_place(&mut store, &root, &opts).unwrap();
        let oracle_db = tmp("reconcile-oracle-db");
        rm_db(&oracle_db);
        rebuild_atomic(&oracle_db, &root, &opts).unwrap();
        let oracle = Store::open_ro(&oracle_db).unwrap();
        for table in ["inodes", "dirents"] {
            let sql = format!("SELECT * FROM {table} ORDER BY 1,2,3");
            let left: Vec<String> = store
                .conn
                .prepare(&sql)
                .unwrap()
                .query_map([], |r| {
                    Ok((0..r.as_ref().column_count())
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let right: Vec<String> = oracle
                .conn
                .prepare(&sql)
                .unwrap()
                .query_map([], |r| {
                    Ok((0..r.as_ref().column_count())
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(
                left, right,
                "in-place {table} projection must match a fresh full scan"
            );
        }
        type GraphRow = (
            String,
            i64,
            i64,
            i64,
            String,
            i64,
            i64,
            Option<(i64, i64, i64, i64)>,
        );
        let graph = |conn: &Connection| {
            let rows: Vec<(i64, i64, i64, i64, String, i64, i64)> = conn
                .prepare("SELECT path_id,dev_id,inode,birth_ns,kind,mount_id,prime FROM paths")
                .unwrap()
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let mut values: Vec<GraphRow> = rows.into_iter().map(|(id,dev,ino,birth,kind,mount,prime)| {
                let path=crate::store::path_graph_resolve(conn,id).unwrap().unwrap().to_string_lossy().into_owned();
                let total=conn.query_row("SELECT visible_bytes,visible_entries,unique_bytes,unique_inodes FROM dir_totals WHERE path_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().unwrap();
                (path,dev,ino,birth,kind,mount,prime,total)
            }).collect();
            values.sort();
            values
        };
        assert_eq!(
            graph(&store.conn),
            graph(&oracle.conn),
            "v7 graph and totals must match the atomic scan by resolved path"
        );
        drop(oracle);
        rm_db(&oracle_db);
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_place_reconciliation_cleans_stages_left_by_a_crashed_scan() {
        let db = tmp("reconcile-stale-stage-db");
        let root = tmp("reconcile-stale-stage-root");
        let opts = ScanOptions {
            include_pseudo: true,
            ..ScanOptions::default()
        };
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("present"), b"indexed").unwrap();
        rebuild_atomic(&db, &root, &opts).unwrap();

        let stale = PathBuf::from(format!("{}.scan-123-456.tmp", db.display()));
        let stale_wal = PathBuf::from(format!("{}.scan-123-456.tmp-wal", db.display()));
        let unrelated = PathBuf::from(format!("{}.scan-not-a-stage.tmp", db.display()));
        fs::write(&stale, b"interrupted staging database").unwrap();
        fs::write(&stale_wal, b"interrupted staging WAL").unwrap();
        fs::write(&unrelated, b"unrelated file").unwrap();

        let mut store = Store::open_rw(&db).unwrap();
        reconcile_in_place(&mut store, &root, &opts).unwrap();

        assert!(!stale.exists(), "abandoned scan database must be removed");
        assert!(!stale_wal.exists(), "abandoned scan WAL must be removed");
        assert!(unrelated.exists(), "unrecognized files must be preserved");
        drop(store);
        rm_db(&db);
        fs::remove_file(unrelated).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn in_place_reconciliation_refuses_filtered_observations() {
        let db = tmp("reconcile-filtered-db");
        let root = tmp("reconcile-filtered-root");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("preserved"), b"baseline").unwrap();
        rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        let mut store = Store::open_rw(&db).unwrap();
        let before: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM paths", [], |r| r.get(0))
            .unwrap();
        let opts = ScanOptions {
            exclude: vec![root.join("preserved")],
            ..Default::default()
        };
        assert!(reconcile_in_place(&mut store, &root, &opts).is_err());
        let cancelled = Arc::new(AtomicBool::new(true));
        let cancel_opts = ScanOptions {
            cancel: Some(cancelled),
            ..Default::default()
        };
        assert!(reconcile_in_place(&mut store, &root, &cancel_opts).is_err());
        let after: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM paths", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_place_publication_sql_failure_rolls_back_every_projection() {
        let db = tmp("reconcile-rollback-db");
        let root = tmp("reconcile-rollback-root");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep"), b"baseline").unwrap();
        rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        let mut store = Store::open_rw(&db).unwrap();
        let before: (i64, i64, i64, i64) = store
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM paths),(SELECT COUNT(*) FROM dir_totals),
                    (SELECT COUNT(*) FROM inodes),(SELECT COUNT(*) FROM dirents)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        fs::write(root.join("new"), b"must roll back").unwrap();
        store.conn.execute_batch(
            "CREATE TRIGGER fail_reconcile_insert BEFORE INSERT ON dirents
             WHEN new.name=X'6e6577' BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;",
        ).unwrap();
        assert!(reconcile_in_place(&mut store, &root, &ScanOptions::default()).is_err());
        assert_eq!(
            store
                .get_meta("last_reconcile_attempt_result")
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        let after: (i64, i64, i64, i64) = store
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM paths),(SELECT COUNT(*) FROM dir_totals),
                    (SELECT COUNT(*) FROM inodes),(SELECT COUNT(*) FROM dirents)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            before, after,
            "failed publication must roll back all four indexed projections"
        );
        assert!(
            crate::store::path_graph_lookup(&store.conn, &root.join("new"))
                .unwrap()
                .is_none()
        );
        store
            .conn
            .execute_batch("DROP TRIGGER fail_reconcile_insert;")
            .unwrap();
        reconcile_in_place(&mut store, &root, &ScanOptions::default()).unwrap();
        assert_eq!(
            store
                .get_meta("last_reconcile_attempt_result")
                .unwrap()
                .as_deref(),
            Some("complete")
        );
        assert!(crate::store::indexed_path(&store.conn, &root.join("new"))
            .unwrap()
            .is_some());
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "manual production-shaped scan-first scale measurement; run explicitly"]
    fn reconciliation_scale_smoke_reports_scan_cost() {
        struct Cleanup {
            db: PathBuf,
            root: PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                rm_db(&self.db);
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let count = std::env::var("DUX_RECONCILE_SCALE_ENTRIES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(100_000);
        let db = tmp("reconcile-scale-db");
        let root = tmp("reconcile-scale-root");
        let _cleanup = Cleanup {
            db: db.clone(),
            root: root.clone(),
        };
        // This manual scale fixture may run on tmpfs in an isolated test
        // environment. Explicitly include that pseudo filesystem so the walker
        // measures the generated tree instead of pruning the entire fixture.
        let opts = ScanOptions {
            include_pseudo: true,
            ..Default::default()
        };
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for i in 0..count {
            let parent = root.join(format!("d{:04}", i / 1_000));
            if i % 1_000 == 0 {
                fs::create_dir_all(&parent).unwrap();
            }
            fs::write(parent.join(format!("f{i:09}")), [0x5au8; 64]).unwrap();
        }

        let started = Instant::now();
        rebuild_atomic(&db, &root, &opts).unwrap();
        let baseline_seconds = started.elapsed().as_secs_f64();
        let baseline_bytes = fs::metadata(&db).unwrap().len();
        let mut store = Store::open_rw(&db).unwrap();
        let started = Instant::now();
        reconcile_in_place(&mut store, &root, &opts).unwrap();
        let reconcile_seconds = started.elapsed().as_secs_f64();
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM dirents", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows as u64, count + count.div_ceil(1_000) + 1);
        eprintln!(
            "reconciliation scale evidence: entries={count}, baseline_s={baseline_seconds:.3}, unchanged_reconcile_s={reconcile_seconds:.3}, baseline_db_bytes={baseline_bytes}, final_db_bytes={}",
            fs::metadata(&db).unwrap().len(),
        );
        drop(store);
    }

    #[test]
    fn zero_byte_index_is_rebuilt_atomically_as_an_empty_initial_source() {
        let db = tmp("zero-byte-index");
        let root = tmp("zero-byte-root");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("payload"), b"baseline").unwrap();
        fs::File::create(&db).unwrap();

        assert!(Store::needs_rebuild(&db).unwrap());
        let stats = rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        let store = Store::open_ro(&db).unwrap();
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("payload"))
                .unwrap()
                .is_some()
        );
        let path_count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM paths", [], |row| row.get(0))
            .unwrap();
        assert_eq!(path_count as u64, stats.staged_path_instances);
        let payload_path_id: i64 = store
            .conn
            .query_row(
                "SELECT path_id FROM paths WHERE parent_path_id=1 AND name=?1",
                [b"payload".as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            crate::store::path_graph_resolve(&store.conn, payload_path_id).unwrap(),
            Some(root.join("payload"))
        );
        let root_totals: (i64, i64) = store
            .conn
            .query_row(
                "SELECT visible_bytes,visible_entries FROM dir_totals WHERE path_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(root_totals.1 as u64, stats.staged_path_instances);
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn v6_upgrade_persists_v7_path_graph_and_preserves_growth_history() {
        let db = tmp("v6-to-v7-index");
        let root = tmp("v6-to-v7-root");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let entry_count = std::env::var("DUX_V6_UPGRADE_SCALE_ENTRIES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);
        let growth_bucket_count = std::env::var("DUX_V6_UPGRADE_GROWTH_BUCKETS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        assert!(
            entry_count <= 10_000_000,
            "scale fixture is capped at ten million files"
        );
        assert!(
            growth_bucket_count <= 32,
            "growth history fixture is capped at thirty-two buckets"
        );
        for i in 0..entry_count {
            let parent = root.join(format!("d{:04}", i / 1_000));
            if i % 1_000 == 0 {
                fs::create_dir_all(&parent).unwrap();
            }
            fs::write(parent.join(format!("f{i:09}")), b"v6 migration fixture").unwrap();
        }
        let baseline = rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        assert_eq!(baseline.files, entry_count);
        assert_eq!(baseline.errors, 0);
        eprintln!(
            "schema-v6 source fixture: files={}, directories={}, path_instances={}, coverage_issues={}, unique_inodes={}",
            baseline.files,
            baseline.dirs,
            baseline.staged_path_instances,
            baseline.errors,
            baseline.staged_root_unique_inodes
        );

        // Model a real v6 database: the v7 path graph does not exist, but the
        // compatibility inode/dirent and incarnation-aware growth tables do.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "DROP INDEX paths_single_root;
                 DROP INDEX paths_target;
                 DROP INDEX paths_parent;
                 DROP INDEX paths_prime;
                 DROP INDEX paths_one_prime;
                 DROP TABLE dir_totals;
                 DROP TABLE paths;
                 UPDATE meta SET value='6' WHERE key='schema_version';
                 INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta)
                   VALUES(123,1,42,0,8192);",
            )
            .unwrap();
            if growth_bucket_count > 0 {
                let mut seed = String::from(
                    "DROP INDEX idx_growth_bucket_delta;
                     DROP INDEX idx_growth_inode_bucket_delta;
                     CREATE TEMP TABLE scale_growth_buckets(bucket INTEGER PRIMARY KEY);",
                );
                for bucket in 0..growth_bucket_count {
                    seed.push_str(&format!(
                        "INSERT INTO scale_growth_buckets VALUES({});",
                        1_000 + bucket
                    ));
                }
                seed.push_str(
                    "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta)
                     SELECT b.bucket,i.dev_id,i.inode,i.birth_ns,i.blocks
                     FROM scale_growth_buckets b CROSS JOIN inodes i;
                     DROP TABLE scale_growth_buckets;
                     CREATE INDEX idx_growth_bucket_delta ON growth(bucket,delta DESC,dev_id,inode,birth_ns);
                     CREATE INDEX idx_growth_inode_bucket_delta ON growth(dev_id,inode,birth_ns,bucket,delta);",
                );
                conn.execute_batch(&seed).unwrap();
            }
        }
        assert!(Store::needs_rebuild(&db).unwrap());

        let stats = rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        let expected_directories = entry_count.saturating_add(999) / 1_000 + 1;
        assert_eq!(stats.files, entry_count);
        assert_eq!(stats.dirs, expected_directories);
        assert_eq!(stats.errors, 0);
        assert_eq!(
            stats.staged_path_instances,
            entry_count + expected_directories
        );
        assert_eq!(
            stats.staged_root_visible_entries,
            entry_count + expected_directories
        );
        let expected_growth_rows = 1_u64
            .checked_add(
                growth_bucket_count
                    .checked_mul(baseline.staged_root_unique_inodes)
                    .expect("growth row count multiplication overflow"),
            )
            .expect("growth row count overflow");
        eprintln!(
            "schema-v7 migrated fixture: files={}, directories={}, path_instances={}, visible_entries={}, coverage_issues={}, expected_growth_rows={}",
            stats.files,
            stats.dirs,
            stats.staged_path_instances,
            stats.staged_root_visible_entries,
            stats.errors,
            expected_growth_rows
        );
        let store = Store::open_ro(&db).unwrap();
        assert_eq!(store.schema_version, 7);
        let path_count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM paths", [], |row| row.get(0))
            .unwrap();
        assert_eq!(path_count as u64, stats.staged_path_instances);
        assert_eq!(
            crate::store::path_graph_resolve(&store.conn, 1).unwrap(),
            Some(root.clone())
        );
        let retained_history: i64 = store
            .conn
            .query_row(
                "SELECT delta FROM growth WHERE bucket=123 AND dev_id=1 AND inode=42 AND birth_ns=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_history, 8192);
        let growth_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM growth", [], |row| row.get(0))
            .unwrap();
        assert_eq!(growth_rows as u64, expected_growth_rows);
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_refuses_zero_byte_main_with_recovery_sidecar() {
        let db = tmp("zero-byte-main-with-wal");
        let root = tmp("zero-byte-main-with-wal-root");
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("payload"), b"baseline").unwrap();
        fs::File::create(&db).unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        fs::write(&wal, b"nonempty recovery candidate").unwrap();

        let error = match rebuild_atomic(&db, &root, &ScanOptions::default()) {
            Ok(_) => panic!("rebuild must preserve an existing recovery sidecar"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("validating previous index"),
            "{error:#}"
        );
        assert_eq!(fs::metadata(&db).unwrap().len(), 0);
        let _ = fs::remove_file(&wal);
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_preserves_dangling_database_symlink_on_first_install() {
        let db = tmp("dangling-index-link");
        let target = tmp("dangling-index-target");
        let root = tmp("dangling-index-root");
        rm_db(&db);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("payload"), b"baseline").unwrap();
        std::os::unix::fs::symlink(&target, &db).unwrap();

        rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        assert!(fs::symlink_metadata(&db).unwrap().file_type().is_symlink());
        assert!(target.exists());
        let store = Store::open_ro(&db).unwrap();
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("payload"))
                .unwrap()
                .is_some()
        );
        drop(store);
        rm_db(&db);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn atomic_rebuild_progress_remains_visible_until_publication_finishes() {
        let db = tmp("progress-publication-db");
        let root = tmp("progress-publication-root");
        rm_db(&db);
        crate::util::clear_scan_progress(&db);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("payload"), b"baseline").unwrap();

        let stats =
            rebuild_atomic_with_install_observer(&db, &root, &ScanOptions::default(), |live_db| {
                let progress = crate::util::read_scan_progress_for(live_db)
                    .expect("scan progress must remain visible through publication");
                assert!(
                    progress.indexing,
                    "publication is part of index finalization"
                );
                assert_eq!(progress.files, 1);
                assert_eq!(progress.dirs, 1);
                assert!(PathBuf::from(format!("{}.new", live_db.display())).exists());
                // Status rejects progress files older than ten seconds. A
                // publication can legitimately exceed that while SQLite waits
                // for a reader, so verify the owner keeps the marker fresh.
                std::thread::sleep(Duration::from_secs(11));
                assert!(
                    crate::util::read_scan_progress_for(live_db).is_some(),
                    "long atomic publication must keep progress fresh"
                );
            })
            .unwrap();

        assert_eq!(stats.files, 1);
        assert!(
            crate::util::read_scan_progress_for(&db).is_none(),
            "successful publication must clear the progress marker"
        );
        assert!(!PathBuf::from(format!("{}.new", db.display())).exists());
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_pseudo_root_keeps_root_entries_but_prunes_nested_directories_by_default() {
        // Use an explicit tmpfs root: temp_dir() is ext4 on some qualification
        // guests and tmpfs on others, which made this policy test depend on the
        // host's TMPDIR rather than the filesystem behavior under test.
        let pseudo_root = Path::new("/dev/shm");
        assert!(is_pseudo_fs(pseudo_root), "test requires /dev/shm tmpfs");
        let suffix = std::process::id();
        let root = pseudo_root.join(format!("dux-scan-pseudo-root-policy-{suffix}"));
        let db = pseudo_root.join(format!("dux-scan-pseudo-root-policy-db-{suffix}"));
        let _ = fs::remove_dir_all(&root);
        rm_db(&db);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("direct-file"), b"included at the explicit root").unwrap();
        fs::write(root.join("nested/child-file"), b"pruned pseudo subtree").unwrap();

        rebuild_atomic(&db, &root, &ScanOptions::default()).unwrap();
        let store = Store::open_ro(&db).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &root)
            .unwrap()
            .is_some());
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("direct-file"))
                .unwrap()
                .is_some()
        );
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("nested"))
                .unwrap()
                .is_none()
        );
        assert!(
            crate::store::indexed_path(&store.conn, &root.join("nested/child-file"))
                .unwrap()
                .is_none()
        );
        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    fn rm_db(db: &Path) {
        for s in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    #[test]
    fn critical_pressure_cancels_rebuild_and_preserves_live_index() {
        let root = tmp("cancel-root");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for i in 0..20_000 {
            fs::write(root.join(format!("f{i:05}")), b"fixture").unwrap();
        }

        let db = tmp("cancel-db");
        rm_db(&db);
        let old = Store::create_fresh(&db).unwrap();
        old.finalize_bulk().unwrap();
        old.set_meta("cancellation_fixture", "old-index-preserved")
            .unwrap();
        drop(old);
        crate::util::clear_scan_progress(&db);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_scan = Arc::clone(&cancel);
        let progress_db = db.clone();
        let canceller = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if crate::util::read_scan_progress_for(&progress_db).is_some() {
                    cancel_scan.store(true, Ordering::SeqCst);
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        });
        let opts = ScanOptions {
            low_priority: true,
            cancel: Some(cancel),
            ..Default::default()
        };
        let result = rebuild_atomic(&db, &root, &opts);
        assert!(
            canceller.join().unwrap(),
            "scan never reached active progress"
        );
        let error = match result {
            Ok(_) => panic!("pressure cancellation must abort the private rebuild"),
            Err(error) => error,
        };
        assert!(
            error.downcast_ref::<ScanCancelled>().is_some(),
            "unexpected cancellation error: {error:#}"
        );
        let old = Store::open_ro(&db).unwrap();
        assert_eq!(
            old.get_meta("cancellation_fixture").unwrap().as_deref(),
            Some("old-index-preserved")
        );
        drop(old);
        for suffix in [".new", ".new-wal", ".new-shm", ".new-journal"] {
            assert!(
                !PathBuf::from(format!("{}{suffix}", db.display())).exists(),
                "cancelled staging file {suffix} must be removed"
            );
        }
        crate::util::clear_scan_progress(&db);
        rm_db(&db);
        let _ = fs::remove_dir_all(root);
    }

    fn write_file(p: &Path, kb: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; kb * 1024]).unwrap();
    }

    /// Hardlinked blocks are counted exactly ONCE (like du/df), and the canonical
    /// link is chosen deterministically as the smallest
    /// (parent_dev, parent_inode, name) — never "whichever the parallel walk saw
    /// first", which would make per-dir totals flap between scans.
    #[test]
    fn hardlinks_counted_once_and_canonical_is_stable() {
        let root = tmp("hardlink-tree");
        let _ = fs::remove_dir_all(&root);
        write_file(&root.join("aaa/f"), 64);
        fs::create_dir_all(root.join("zzz")).unwrap();
        // second link to the same inode, in a lexicographically LATER directory
        fs::hard_link(root.join("aaa/f"), root.join("zzz/f")).unwrap();

        let db = tmp("hardlink-db");
        rm_db(&db);
        // The test harness puts fixture roots under TMPDIR on tmpfs. Opt in to
        // that filesystem so this inode-accounting test exercises the tree,
        // while pseudo-mount exclusion has a separate explicit-root test above.
        let opts = ScanOptions {
            include_pseudo: true,
            ..Default::default()
        };
        let stats = rebuild_atomic(&db, &root, &opts).unwrap();

        let store = Store::open_ro(&db).unwrap();
        // exactly one `inodes` row for the shared inode...
        let ino: i64 = fs::metadata(root.join("aaa/f")).unwrap().ino() as i64;
        let rows: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM inodes WHERE inode=?1",
                params![ino],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "one inodes row per shared inode");

        // ...but BOTH paths exist as dirents, with exactly one marked prime.
        let dirents: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE inode=?1",
                params![ino],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dirents, 2, "both hardlink paths are indexed");
        let primes: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE inode=?1 AND prime=1",
                params![ino],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(primes, 1, "exactly one canonical link carries the blocks");

        // The canonical link is the minimum identity tuple, independent of the
        // filesystem's inode allocation order (which need not follow path order).
        let prime_link: (i64, i64, Vec<u8>) = store
            .conn
            .query_row(
                "SELECT parent_dev,parent_inode,name FROM dirents
                 WHERE inode=?1 AND prime=1",
                params![ino],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let minimum_link: (i64, i64, Vec<u8>) = store
            .conn
            .query_row(
                "SELECT parent_dev,parent_inode,name FROM dirents
                 WHERE inode=?1 ORDER BY parent_dev,parent_inode,name LIMIT 1",
                params![ino],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            prime_link, minimum_link,
            "canonical link must be the minimum (parent_dev,parent_inode,name) tuple"
        );

        // The shared 64K is counted once in the root total, not twice.
        let root_ino: i64 = fs::metadata(&root).unwrap().ino() as i64;
        let rb: i64 = store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE inode=?1",
                params![root_ino],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (65536..200_000).contains(&rb),
            "64K counted once (plus dir blocks), got {rb} — 128K means double-counted"
        );
        assert_eq!(stats.files, 2, "both links are still reported as files");

        drop(store);
        rm_db(&db);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn priority_hinted_scan_indexes_same_complete_graph_as_unhinted_scan() {
        let root = tmp("priority-scan-tree");
        let _ = fs::remove_dir_all(&root);
        write_file(&root.join("alpha/unchanged.bin"), 2);
        write_file(&root.join("omega/nested/changed.bin"), 3);

        let plain_db = tmp("priority-scan-plain-db");
        let hinted_db = tmp("priority-scan-hinted-db");
        rm_db(&plain_db);
        rm_db(&hinted_db);
        let common = ScanOptions {
            include_pseudo: true,
            ..Default::default()
        };
        rebuild_atomic(&plain_db, &root, &common).unwrap();
        let hinted = ScanOptions {
            priority_paths: vec![root.join("omega/nested")],
            ..common
        };
        rebuild_atomic(&hinted_db, &root, &hinted).unwrap();

        type DirentRows = Vec<(i64, i64, Vec<u8>, i64, i64, i64)>;
        type InodeRows = Vec<(i64, i64, String, i64, i64)>;
        fn indexed_rows(db: &Path) -> (DirentRows, InodeRows) {
            let store = Store::open_ro(db).unwrap();
            let dirents = store
                .conn
                .prepare("SELECT parent_dev,parent_inode,name,dev_id,inode,prime FROM dirents ORDER BY parent_dev,parent_inode,name")
                .unwrap()
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let inodes = store
                .conn
                .prepare("SELECT dev_id,inode,kind,blocks,recursive_bytes FROM inodes ORDER BY dev_id,inode")
                .unwrap()
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (dirents, inodes)
        }

        assert_eq!(indexed_rows(&plain_db), indexed_rows(&hinted_db));
        rm_db(&plain_db);
        rm_db(&hinted_db);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rebuild_preserves_existing_reader_snapshot() {
        let dir = std::env::temp_dir().join(format!("dux-reader-{}", std::process::id()));
        let db = dir.with_extension("db");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old"), b"old").unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let reader = Store::open_ro(&db).unwrap();
        reader.conn.execute_batch("BEGIN").unwrap();
        let before: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
            .unwrap();
        std::fs::write(dir.join("new"), b"new").unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let during: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            before, during,
            "an existing WAL reader keeps its complete snapshot"
        );
        reader.conn.execute_batch("COMMIT").unwrap();
        let after: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after,
            before + 1,
            "the same connection sees the rebuilt index after ending its snapshot"
        );
        assert!(reader.totals_consistent().unwrap());
        drop(reader);
        let _ = std::fs::remove_dir_all(dir);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn rebuilt_index_is_fsynced_in_wal_mode_without_staging_files() {
        let dir = std::env::temp_dir().join(format!("dux-rebuild-mode-{}", std::process::id()));
        let db = dir.with_extension("db");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("indexed"), b"durable").unwrap();

        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let store = Store::open_ro(&db).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(store);
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        assert!(
            !wal.exists() || fs::metadata(wal).unwrap().len() == 0,
            "rebuilt index must not retain uncheckpointed WAL contents"
        );
        assert!(!PathBuf::from(format!("{}.new", db.display())).exists());

        let _ = std::fs::remove_dir_all(dir);
        rm_db(&db);
    }

    #[test]
    fn rebuild_preserves_growth_history() {
        let dir = std::env::temp_dir().join(format!("dux-history-{}", std::process::id()));
        let db = dir.with_extension("db");
        std::fs::create_dir_all(&dir).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        {
            let store = Store::open_rw(&db).unwrap();
            store
                .conn
                .execute("INSERT INTO growth VALUES(123,1,42,0,8192)", [])
                .unwrap();
        }
        std::fs::write(dir.join("during-reconcile"), vec![1; 8192]).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let store = Store::open_ro(&db).unwrap();
        let delta: i64 = store
            .conn
            .query_row(
                "SELECT delta FROM growth WHERE bucket=123 AND dev_id=1 AND inode=42 AND birth_ns=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(delta, 8192);
        let observed: i64 = store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM growth WHERE bucket!=123",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            observed, 8192,
            "new allocation observed by the baseline belongs in growth history"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn rebuild_from_v5_preserves_growth_as_unknown_incarnations() {
        let dir = tmp("v5-growth-upgrade-tree");
        let db = tmp("v5-growth-upgrade-db");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("indexed-file"), vec![5; 8192]).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();

        // Convert only the growth table to the v5 layout. The inode and root
        // metadata are preserved because v5 already had birth_ns on inodes,
        // while its history rows did not carry an incarnation key.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "DROP VIEW growth_current;
                 DROP TRIGGER growth_delta_integer_insert;
                 DROP TRIGGER growth_delta_integer_update;
                 ALTER TABLE growth RENAME TO growth_v6;
                 CREATE TABLE growth(
                    bucket INTEGER NOT NULL,
                    dev_id INTEGER NOT NULL,
                    inode INTEGER NOT NULL,
                    delta INTEGER NOT NULL,
                    PRIMARY KEY(bucket,dev_id,inode)
                 ) WITHOUT ROWID;
                 INSERT INTO growth(bucket,dev_id,inode,delta)
                    SELECT bucket,dev_id,inode,delta FROM growth_v6;
                 DROP TABLE growth_v6;
                 INSERT INTO growth(bucket,dev_id,inode,delta) VALUES(123,1,42,8192);
                 UPDATE meta SET value='5' WHERE key='schema_version';",
            )
            .unwrap();
        }

        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let upgraded = Store::open_ro(&db).unwrap();
        let (birth_ns, delta): (i64, i64) = upgraded
            .conn
            .query_row(
                "SELECT birth_ns,delta FROM growth WHERE bucket=123 AND dev_id=1 AND inode=42",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(birth_ns, 0, "v5 history must remain unattributed");
        assert_eq!(delta, 8192);
        assert_eq!(
            upgraded
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM growth_current WHERE bucket=123 AND dev_id=1 AND inode=42",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "legacy history must not be claimed by the current inode incarnation"
        );
        assert!(upgraded
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name='growth_delta_integer_update')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        drop(upgraded);
        fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }

    #[test]
    fn rebuild_preserves_birth_keyed_history_for_the_same_incarnation() {
        let dir = tmp("history-birth-preserve-tree");
        let db = tmp("history-birth-preserve-db");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("history-target");
        fs::write(&file, vec![8; 8192]).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let metadata = fs::metadata(&file).unwrap();
        let dev = metadata.dev() as i64;
        let inode = metadata.ino() as i64;
        let store = Store::open_rw(&db).unwrap();
        let birth_ns: i64 = store
            .conn
            .query_row(
                "SELECT birth_ns FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, inode],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(birth_ns, 0, "the fixture filesystem must expose birth time");
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(123,?1,?2,?3,8192)",
                params![dev, inode, birth_ns],
            )
            .unwrap();
        drop(store);

        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let upgraded = Store::open_ro(&db).unwrap();
        let retained: i64 = upgraded
            .conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=?1 AND inode=?2",
                params![dev, inode],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, 8192);
        drop(upgraded);
        fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }

    #[test]
    fn rebuild_growth_overflow_keeps_previous_index_published() {
        let dir = tmp("history-overflow-tree");
        let db = tmp("history-overflow-db");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("history-target");
        fs::write(&file, vec![3; 4096]).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();

        let (dev, inode, birth_ns, old_blocks): (i64, i64, i64, i64) = {
            let store = Store::open_rw(&db).unwrap();
            let metadata = fs::metadata(&file).unwrap();
            let identity = (metadata.dev() as i64, metadata.ino() as i64);
            let row = store
                .conn
                .query_row(
                    "SELECT birth_ns,blocks FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![identity.0, identity.1],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            (identity.0, identity.1, row.0, row.1)
        };
        assert_ne!(birth_ns, 0, "the fixture filesystem must expose birth time");
        let bucket = now_secs() / crate::store::GROWTH_BUCKET_SECS;
        Store::open_rw(&db)
            .unwrap()
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(?1,?2,?3,?4,?5)",
                params![bucket, dev, inode, birth_ns, i64::MAX],
            )
            .unwrap();
        fs::write(&file, vec![9; 1024 * 1024]).unwrap();

        let error = match rebuild_atomic(&db, &dir, &ScanOptions::default()) {
            Ok(_) => panic!("overflowing growth history must abort the rebuild"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("growth delta must be an SQLite integer"),
            "{error}"
        );

        let previous = Store::open_ro(&db).unwrap();
        let (published_blocks, retained_delta, storage_type): (i64, i64, String) = previous
            .conn
            .query_row(
                "SELECT i.blocks,g.delta,typeof(g.delta) FROM inodes i JOIN growth g
                 ON g.dev_id=i.dev_id AND g.inode=i.inode
                 WHERE i.dev_id=?1 AND i.inode=?2 AND g.bucket=?3 AND g.birth_ns=?4",
                params![dev, inode, bucket, birth_ns],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            published_blocks, old_blocks,
            "failed staging build replaced live state"
        );
        assert_eq!(retained_delta, i64::MAX);
        assert_eq!(storage_type, "integer");
        drop(previous);
        fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }

    #[test]
    fn v4_upgrade_preserves_ambiguous_history_without_attributing_it() {
        let dir = tmp("upgrade-v4-tree");
        let db = tmp("upgrade-v4-db");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), b"retained through upgrade").unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        {
            let previous = Store::open_rw(&db).unwrap();
            previous
                .conn
                .execute_batch(
                    "UPDATE meta SET value='4' WHERE key='schema_version';
                 DROP VIEW temp.growth_current;
                 DROP VIEW main.growth_current;
                 DROP INDEX idx_growth_bucket_delta;
                 DROP INDEX idx_growth_inode_bucket_delta;
                 DROP TABLE growth;
                 CREATE TABLE growth(bucket INTEGER NOT NULL,dev_id INTEGER NOT NULL,inode INTEGER NOT NULL,delta INTEGER NOT NULL,PRIMARY KEY(bucket,dev_id,inode)) WITHOUT ROWID;
                 ALTER TABLE inodes DROP COLUMN birth_ns;
                 INSERT INTO growth VALUES(123,1,42,8192);",
                )
                .unwrap();
        }
        assert!(Store::needs_rebuild(&db).unwrap());
        let legacy = Store::open_ro(&db).unwrap();
        assert_eq!(
            legacy
                .conn
                .query_row("SELECT COUNT(*) FROM growth_current", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "legacy ambiguous growth is hidden until v7 migration"
        );
        drop(legacy);
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let upgraded = Store::open_ro(&db).unwrap();
        assert_eq!(
            upgraded.get_meta("schema_version").unwrap().as_deref(),
            Some("7")
        );
        let history: i64 = upgraded
            .conn
            .query_row(
                "SELECT delta FROM growth WHERE bucket=123 AND dev_id=1 AND inode=42 AND birth_ns=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history, 8192);
        let attributed: i64 = upgraded.conn.query_row(
            "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE bucket=123 AND dev_id=1 AND inode=42",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(attributed, 0, "legacy history must remain unattributed");
        let born: i64 = upgraded
            .conn
            .query_row(
                "SELECT birth_ns FROM inodes WHERE inode=?1",
                params![fs::metadata(dir.join("file")).unwrap().ino() as i64],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            born,
            crate::util::inode_birth_ns_at(
                &fs::metadata(dir.join("file")).unwrap(),
                &dir.join("file")
            )
        );
        drop(upgraded);
        std::fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }

    #[test]
    fn v5_upgrade_keeps_legacy_history_unknown_and_tracks_current_incarnation() {
        let dir = tmp("upgrade-v5-tree");
        let db = tmp("upgrade-v5-db");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, vec![1; 4096]).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        {
            let previous = Store::open_rw(&db).unwrap();
            previous.conn.execute_batch(
                "UPDATE meta SET value='5' WHERE key='schema_version';
                 DROP VIEW temp.growth_current;
                 DROP VIEW main.growth_current;
                 DROP INDEX idx_growth_bucket_delta;
                 DROP INDEX idx_growth_inode_bucket_delta;
                 DROP TABLE growth;
                 CREATE TABLE growth(bucket INTEGER NOT NULL,dev_id INTEGER NOT NULL,inode INTEGER NOT NULL,delta INTEGER NOT NULL,PRIMARY KEY(bucket,dev_id,inode)) WITHOUT ROWID;
                 INSERT INTO growth VALUES(123,1,42,8192);",
            ).unwrap();
        }
        let before = fs::metadata(&file).unwrap();
        let identity = (before.dev() as i64, before.ino() as i64);
        let before_blocks = before.blocks() as i64 * 512;
        fs::write(&file, vec![2; 16 * 1024 * 1024]).unwrap();
        let after_blocks = fs::metadata(&file).unwrap().blocks() as i64 * 512;
        assert!(
            after_blocks > before_blocks,
            "fixture must allocate additional blocks"
        );

        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        let upgraded = Store::open_ro(&db).unwrap();
        assert_eq!(upgraded.schema_version, crate::store::SCHEMA_VERSION);
        let legacy: i64 = upgraded.conn.query_row(
            "SELECT COALESCE(SUM(delta),0) FROM growth WHERE bucket=123 AND dev_id=1 AND inode=42 AND birth_ns=0",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(legacy, 8192);
        let current: i64 = upgraded
            .conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=?1 AND inode=?2",
                params![identity.0, identity.1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current, after_blocks - before_blocks);
        drop(upgraded);
        fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }

    #[test]
    fn rebuild_refuses_future_schema_without_modifying_the_database() {
        let dir = tmp("future-schema-tree");
        let db = tmp("future-schema-db");
        fs::create_dir_all(&dir).unwrap();
        rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        {
            let source = Store::open_rw(&db).unwrap();
            source
                .set_meta(
                    "schema_version",
                    &(crate::store::SCHEMA_VERSION + 1).to_string(),
                )
                .unwrap();
        }
        let original = fs::read(&db).unwrap();
        let error = rebuild_atomic(&db, &dir, &ScanOptions::default())
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("destructive downgrade"));
        assert_eq!(fs::read(&db).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
        rm_db(&db);
    }
}
