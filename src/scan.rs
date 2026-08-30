use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// True if `path` sits on a virtual/pseudo filesystem whose "sizes" are not real
/// disk usage (/proc, /sys, cgroup, devpts, …). /proc/kcore reports the entire
/// address space — counting it is meaningless. Shared with the daemon so both
/// cover exactly the same set of filesystems.
pub(crate) fn is_pseudo_fs(path: &Path) -> bool {
    matches!(
        fs_magic(path),
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
    pub bytes: i64,
    pub errors: u64,
}

/// Atomic full rebuild: scan `root` into a brand-new index next to `db`, then
/// replace `db` in a single rename. Guarantees a fragmentation-free file and
/// never leaves a half-built or empty index in place (callers: `dux scan` and
/// the daemon's self-heal when the on-disk schema is incompatible/missing).
pub fn rebuild_atomic(db: &Path, root: &Path, opts: &ScanOptions) -> Result<ScanStats> {
    let mut new_os = db.to_path_buf().into_os_string();
    new_os.push(".new");
    let db_new = PathBuf::from(new_os);
    cleanup_stale_scan_files(&db_new);
    for suf in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suf}", db_new.display()));
    }
    let stats = {
        let mut store = Store::create_fresh(&db_new)?;
        let s = scan(&mut store, root, opts)?;
        // drain the WAL so the file is self-contained before the swap
        store
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .ok();
        s
    }; // store dropped here -> connection closed, -wal/-shm removed
       // fsync the finished file before the rename: the rename is namespace-atomic
       // but a crash could otherwise expose a rename to not-yet-durable contents.
    if let Ok(f) = std::fs::File::open(&db_new) {
        let _ = f.sync_all();
    }
    for suf in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suf}", db.display()));
    }
    std::fs::rename(&db_new, db)
        .with_context(|| format!("installing new index at {}", db.display()))?;
    // fsync the parent directory so the rename itself survives a crash.
    if let Some(parent) = db.parent() {
        if let Ok(d) = std::fs::File::open(parent) {
            let _ = d.sync_all();
        }
    }
    Ok(stats)
}

/// A SIGKILL/power loss cannot run StageGuard. The exclusive DB lock guarantees
/// no other rebuild is active, so remove only staging files for this exact
/// destination before starting the next atomic rebuild.
fn cleanup_stale_scan_files(db_new: &Path) {
    let Some(parent) = db_new.parent() else {
        return;
    };
    let Some(base) = db_new.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        return;
    };
    let prefix = format!("{base}.scan-");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix)
            && (name.ends_with(".tmp")
                || name.ends_with(".tmp-journal")
                || name.ends_with(".tmp-wal")
                || name.ends_with(".tmp-shm"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Full scan of `root` into the index. Computes recursive directory totals
/// bottom-up in a single transaction (batched inserts — never row-at-a-time IO).
pub fn scan(store: &mut Store, root: &Path, opts: &ScanOptions) -> Result<ScanStats> {
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
    let root_dev = meta.dev() as i64;
    let root_inode = meta.ino() as i64;
    let now = now_secs();
    let started = Instant::now();

    // The caller builds into a FRESH, empty index (atomic rescan), so there is
    // nothing to reset here — and the FTS sync triggers don't exist yet, which
    // is why the bulk load below is fast (FTS is rebuilt once in finalize_bulk).

    // ---- shared progress counters + background printer thread ----
    let n_files = Arc::new(AtomicU64::new(0));
    let n_dirs = Arc::new(AtomicU64::new(1)); // counting root
    let n_bytes = Arc::new(AtomicI64::new((meta.blocks() as i64) * 512));
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
                    crate::util::write_scan_progress(started_epoch, fc, dc, bc, indexing);
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
            crate::util::clear_scan_progress(); // scan finished — remove the file
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
           dev INTEGER NOT NULL, inode INTEGER NOT NULL,
           parent_dev INTEGER NOT NULL, parent_inode INTEGER NOT NULL,
           depth INTEGER NOT NULL, kind TEXT NOT NULL, blocks INTEGER NOT NULL,
           uid INTEGER NOT NULL, mtime INTEGER NOT NULL, name BLOB NOT NULL
         );",
    )?;
    parallel_stage(
        &mut stage, &root, root_dev, root_inode, &index_db, opts, &n_files, &n_dirs, &n_bytes,
        &n_errors,
    )?;
    stage.execute(
        "INSERT INTO scan_nodes
         (dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name)
         VALUES (?1,?2,?1,?2,0,'d',?3,?4,?5,?6)",
        params![
            root_dev,
            root_inode,
            (meta.blocks() as i64) * 512,
            meta.uid() as i64,
            meta.mtime(),
            root.as_os_str().as_bytes(),
        ],
    )?;

    // ---- phase 2: deterministic hardlinks + bottom-up totals in SQLite ----
    indexing.store(true, Ordering::Relaxed);
    stage.execute_batch(
        "CREATE INDEX scan_nodes_target ON scan_nodes(dev,inode,parent_dev,parent_inode,name);
         CREATE INDEX scan_nodes_depth ON scan_nodes(depth);",
    )?;
    drop(stage);
    store.conn.execute(
        "ATTACH DATABASE ?1 AS scan",
        params![stage_path.to_string_lossy()],
    )?;
    let build_result: Result<i64> = (|| {
        store.conn.execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO inodes
               (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
             SELECT dev,inode,MAX(kind),MAX(blocks),MAX(blocks),1,MAX(uid),MAX(mtime)
             FROM scan.scan_nodes GROUP BY dev,inode;

             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             SELECT parent_dev,parent_inode,name,dev,inode,
                    CASE WHEN ROW_NUMBER() OVER (
                      PARTITION BY dev,inode ORDER BY parent_dev,parent_inode,name
                    )=1 THEN 1 ELSE 0 END
             FROM scan.scan_nodes;

             CREATE TEMP TABLE rollup (
               dev INTEGER NOT NULL, inode INTEGER NOT NULL,
               bytes INTEGER NOT NULL, items INTEGER NOT NULL,
               PRIMARY KEY(dev,inode)
             ) WITHOUT ROWID;
             COMMIT;",
        )?;
        let max_depth: i64 = store.conn.query_row(
            "SELECT COALESCE(MAX(depth),0) FROM scan.scan_nodes",
            [],
            |r| r.get(0),
        )?;
        for depth in (1..=max_depth).rev() {
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM rollup", [])?;
            tx.execute(
                "INSERT INTO rollup(dev,inode,bytes,items)
                 SELECT s.parent_dev,s.parent_inode,
                        COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_bytes ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN d.prime=1 THEN i.recursive_inodes ELSE 0 END),0)
                 FROM scan.scan_nodes s
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
    let _ = store
        .conn
        .execute_batch("DROP TABLE IF EXISTS temp.rollup; DETACH DATABASE scan;");
    let root_total = build_result?;
    drop(stage_guard);

    // ---- phase 3: build query indexes once (no per-row trigger overhead) ----
    // Build the FTS index in one pass and install the sync triggers + indexes.
    store.finalize_bulk()?;
    // Restore the query-optimized connection default. In the atomic rebuild path
    // this connection is about to close, but direct scan tests may reuse it.
    store.conn.execute_batch("PRAGMA temp_store=MEMORY;")?;

    let stats = ScanStats {
        files: n_files.load(Ordering::Relaxed),
        dirs: n_dirs.load(Ordering::Relaxed),
        bytes: root_total,
        errors: n_errors.load(Ordering::Relaxed),
    };

    // stop + join the printer thread (the guard would also do this on early
    // error returns); then clear the progress line.
    drop(progress_guard);
    if opts.progress {
        eprint!("\r\x1b[K");
        std::io::stderr().flush().ok();
    }

    store.set_meta("last_scan_ts", &now.to_string())?;
    store.set_meta("last_scan_root", &root.to_string_lossy())?;
    store.set_meta("root_dev", &root_dev.to_string())?;
    store.set_meta("root_inode", &root_inode.to_string())?;
    Ok(stats)
}

/// A node in the bounded handoff between stat workers and the staging writer.
struct RawNode {
    dev: i64,
    inode: i64,
    parent_dev: i64,
    parent_inode: i64,
    depth: u32,
    kind: char,
    blocks: i64,
    uid: i64,
    mtime: i64,
    name: Vec<u8>, // raw filename bytes — identity-preserving (no lossy UTF-8)
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
    index_db: &Path,
    opts: &ScanOptions,
    n_files: &Arc<AtomicU64>,
    n_dirs: &Arc<AtomicU64>,
    n_bytes: &Arc<AtomicI64>,
    n_errors: &Arc<AtomicU64>,
) -> Result<()> {
    use jwalk::WalkDirGeneric;

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

    let threads = scan_threads(opts);

    // Cycle guard: bind mounts / directory hardlinks can make a directory
    // reachable from WITHIN its own subtree. follow_links(false) stops symlink
    // loops but not these real path cycles — without a visited set the walk would
    // recurse forever and exhaust memory. Track already-walked directory
    // (dev,inode)s and prune on a repeat.
    let visited: Arc<std::sync::Mutex<std::collections::HashSet<(i64, i64)>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    // For the pseudo-fs check below: normally we only statfs at a mount boundary
    // (child dev != parent dev), but if the scan root is ITSELF a pseudo fs we
    // must check every subdir to preserve the "exclude pseudo contents" behavior.
    let root_pseudo = is_pseudo_fs(root);

    std::thread::scope(|scope| -> Result<()> {
        let producer = scope.spawn(move || {
            {
                let walk = WalkDirGeneric::<((), ())>::new(root)
                    .skip_hidden(false)
                    .follow_links(false)
                    .parallelism(jwalk::Parallelism::RayonNewPool(threads))
                    .process_read_dir(move |_depth, dir_path, _state, children| {
                        // stat the directory once to learn the parent (dev,inode); all of
                        // these children share it. depth is the dir's depth + 1.
                        let pinfo = std::fs::symlink_metadata(dir_path)
                            .ok()
                            .map(|m| (m.dev() as i64, m.ino() as i64));
                        // cycle guard: if this exact directory was already walked, a
                        // bind-mount / dir-hardlink cycle is sending us back through it —
                        // prune so the walk terminates. Only guard on a successful stat so
                        // a stat failure doesn't collide with the root's fallback key.
                        if let Some(id) = pinfo {
                            // poison-tolerant: the critical section is a single HashSet
                            // insert (can't panic), but recover the guard rather than let
                            // one walker's panic cascade into every other walker's unwrap.
                            // Scope the guard to just the insert so it's NOT held across the
                            // per-directory processing below (that would serialize walkers).
                            let already_seen = {
                                let mut vis = visited.lock().unwrap_or_else(|e| e.into_inner());
                                !vis.insert(id)
                            };
                            if already_seen {
                                children.clear();
                                return;
                            }
                        }
                        let (pdev, pino) = pinfo.unwrap_or((root_dev, root_inode));
                        let cdepth = dir_path.components().count() as u32 + 1;
                        children.retain(|res| {
                            let entry = match res {
                                Ok(e) => e,
                                Err(_) => {
                                    ne.fetch_add(1, Ordering::Relaxed);
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
                                Err(_) => {
                                    ne.fetch_add(1, Ordering::Relaxed);
                                    return false;
                                }
                            };
                            let dev = m.dev() as i64;
                            if one_fs && dev != root_dev {
                                return false;
                            }
                            let is_dir = m.is_dir();
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
                            let blocks = (m.blocks() as i64) * 512;
                            let kind = if is_dir {
                                'd'
                            } else if m.file_type().is_symlink() {
                                'l'
                            } else if m.is_file() {
                                'f'
                            } else {
                                'o'
                            };
                            let ino = m.ino() as i64;
                            // The walker also surfaces the scan root as a child of its real
                            // parent dir. Don't emit or count that duplicate (the canonical
                            // self-parented root is added separately) — but still recurse
                            // into it so the real subtree is walked.
                            if dev == root_dev && ino == root_inode {
                                return true;
                            }
                            let name = entry.file_name().as_bytes().to_vec();
                            let sent = tx
                                .send(RawNode {
                                    dev,
                                    inode: ino,
                                    parent_dev: pdev,
                                    parent_inode: pino,
                                    depth: cdepth,
                                    kind,
                                    blocks,
                                    uid: m.uid() as i64,
                                    mtime: m.mtime(),
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
                            nb.fetch_add(blocks, Ordering::Relaxed);
                            // keep only directories so jwalk recurses; files already sent
                            is_dir
                        });
                    });
                // drive the walk to completion; all work happens in the closure
                for _ in walk {}
            } // walk + its tx clones dropped here -> channel closes
        });

        let mut batch = Vec::with_capacity(INSERT_BATCH);
        let mut write_error = None;
        loop {
            match rx.recv() {
                Ok(node) => batch.push(node),
                Err(_) => {
                    if !batch.is_empty() {
                        if let Err(e) = insert_stage_batch(stage, &mut batch) {
                            write_error = Some(e);
                        }
                    }
                    break;
                }
            }
            if batch.len() >= INSERT_BATCH {
                if let Err(e) = insert_stage_batch(stage, &mut batch) {
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
        if let Some(e) = write_error {
            return Err(e);
        }
        Ok(())
    })
}

fn insert_stage_batch(stage: &mut Connection, batch: &mut Vec<RawNode>) -> Result<()> {
    let tx = stage.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO scan_nodes
             (dev,inode,parent_dev,parent_inode,depth,kind,blocks,uid,mtime,name)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?;
        for n in batch.iter() {
            stmt.execute(params![
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

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-scan-{tag}-{}", std::process::id()))
    }

    fn rm_db(db: &Path) {
        for s in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{s}", db.display()));
        }
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
        let opts = ScanOptions::default();
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

        // The canonical link is the one under `aaa` (smaller name), not `zzz`.
        let prime_name: Vec<u8> = store
            .conn
            .query_row(
                "SELECT p.name FROM dirents d JOIN dirents p
                   ON p.dev_id=d.parent_dev AND p.inode=d.parent_inode
                 WHERE d.inode=?1 AND d.prime=1",
                params![ino],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&prime_name),
            "aaa",
            "canonical link must be the min (parent,name), deterministically"
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
}
