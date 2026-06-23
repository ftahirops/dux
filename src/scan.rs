use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::params;
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
}

/// True if `path` sits on a virtual/pseudo filesystem whose "sizes" are not real
/// disk usage (/proc, /sys, cgroup, devpts, …). /proc/kcore reports the entire
/// address space — counting it is meaningless. Shared with the daemon so both
/// cover exactly the same set of filesystems.
pub(crate) fn is_pseudo_fs(path: &Path) -> bool {
    use std::mem::MaybeUninit;
    let c = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut s = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return false;
    }
    let t = unsafe { s.assume_init() }.f_type;
    matches!(
        t,
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
        | 0x9fa2 // USBDEVICE
    )
}

#[derive(Default)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: i64,
    pub errors: u64,
}

/// Full scan of `root` into the index. Computes recursive directory totals
/// bottom-up in a single transaction (batched inserts — never row-at-a-time IO).
pub fn scan(store: &mut Store, root: &Path, opts: &ScanOptions) -> Result<ScanStats> {
    if opts.low_priority {
        set_low_priority();
    }
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

    // Reset the whole index — one scan == one tree. This avoids stale roots
    // from earlier scans of other paths/devices polluting the view.
    store
        .conn
        .execute_batch("DELETE FROM nodes; DELETE FROM names_fts;")?;

    // ---- shared progress counters + background printer thread ----
    let n_files = Arc::new(AtomicU64::new(0));
    let n_dirs = Arc::new(AtomicU64::new(1)); // counting root
    let n_bytes = Arc::new(AtomicI64::new((meta.blocks() as i64) * 512));
    let done = Arc::new(AtomicBool::new(false));
    let indexing = Arc::new(AtomicBool::new(false));
    let progress_thread = if opts.progress {
        let (f, d, b, dn, ix) = (
            n_files.clone(),
            n_dirs.clone(),
            n_bytes.clone(),
            done.clone(),
            indexing.clone(),
        );
        Some(std::thread::spawn(move || {
            let spin = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0usize;
            while !dn.load(Ordering::Relaxed) {
                let verb = if ix.load(Ordering::Relaxed) {
                    "building index…"
                } else {
                    "scanning…      "
                };
                eprint!(
                    "\r\x1b[K {} {}  {} files  {} dirs  {}  {:.0}s",
                    spin[i % spin.len()],
                    verb,
                    f.load(Ordering::Relaxed),
                    d.load(Ordering::Relaxed),
                    crate::util::human(b.load(Ordering::Relaxed)),
                    started.elapsed().as_secs_f64(),
                );
                std::io::stderr().flush().ok();
                i += 1;
                std::thread::sleep(Duration::from_millis(120));
            }
        }))
    } else {
        None
    };

    // ---- phase 1: parallel walk, collect raw nodes ----
    let n_errors = Arc::new(AtomicU64::new(0));
    let raw = parallel_collect(
        &root, root_dev, root_inode, opts, &n_files, &n_dirs, &n_bytes, &n_errors,
    );

    // ---- phase 2: recursive totals ----
    // Switch the progress message to "indexing" — the walk is done.
    indexing.store(true, Ordering::Relaxed);
    let mut nodes = raw;

    // (dev,inode) compose into one key — inode numbers collide ACROSS devices,
    // and scanning `/` crosses many mounts, so we must never key by inode alone.
    #[inline]
    fn key(dev: i64, ino: i64) -> i128 {
        ((dev as i128) << 64) | (ino as u64 as i128)
    }

    // the root node itself (parent = self marks the root; name = absolute path)
    nodes.push(RawNode {
        dev: root_dev,
        inode: root_inode,
        parent_dev: root_dev,
        parent_inode: root_inode,
        depth: 0,
        kind: 'd',
        size: meta.size() as i64,
        blocks: (meta.blocks() as i64) * 512,
        uid: meta.uid() as i64,
        gid: meta.gid() as i64,
        mode: meta.mode() as i64,
        mtime: meta.mtime(),
        name: root.to_string_lossy().into_owned(),
        recursive: 0,
        rinodes: 1,
    });

    // bottom-up totals via the tree (index-based). Directories are unique, so we
    // map each dir (dev,inode) -> node index and roll child subtotals into the
    // parent. Hardlinked files (same inode at multiple paths) have their blocks
    // counted ONCE — matching `du`/`df`, which never double-count shared inodes.
    let mut dir_idx: std::collections::HashMap<i128, usize> =
        std::collections::HashMap::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        if n.kind == 'd' {
            dir_idx.insert(key(n.dev, n.inode), i);
        }
    }
    let mut seen_file: std::collections::HashSet<i128> =
        std::collections::HashSet::with_capacity(nodes.len());
    let mut bytes_sub = vec![0i64; nodes.len()];
    let mut inode_sub = vec![0i64; nodes.len()];
    // `primary[i]` = this is the single canonical row/name for its inode (a dir,
    // or the first-seen link of a file). Only primaries get a node row + FTS
    // name, so a search never resolves to a different hardlink's path.
    let mut primary = vec![false; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        if n.kind == 'd' {
            bytes_sub[i] = n.blocks;
            inode_sub[i] = 1;
            primary[i] = true;
        } else if seen_file.insert(key(n.dev, n.inode)) {
            // first link to this inode: count its blocks and the inode once
            bytes_sub[i] = n.blocks;
            inode_sub[i] = 1;
            primary[i] = true;
        } else {
            // additional hardlinks: already counted (0 bytes, 0 inodes, no row)
            bytes_sub[i] = 0;
            inode_sub[i] = 0;
        }
    }
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| nodes[b].depth.cmp(&nodes[a].depth)); // deepest first
    for &i in &order {
        let pk = key(nodes[i].parent_dev, nodes[i].parent_inode);
        if let Some(&p) = dir_idx.get(&pk) {
            if p != i {
                bytes_sub[p] += bytes_sub[i];
                inode_sub[p] += inode_sub[i];
            }
        }
    }
    for (i, n) in nodes.iter_mut().enumerate() {
        if n.kind == 'd' {
            n.recursive = bytes_sub[i];
            n.rinodes = inode_sub[i];
        } else {
            n.recursive = n.blocks;
            n.rinodes = 1;
        }
    }
    let root_total = dir_idx
        .get(&key(root_dev, root_inode))
        .map(|&i| bytes_sub[i])
        .unwrap_or(0);

    // ---- phase 3: batched insert (nodes + trigram FTS inline) ----
    let mut idx = 0;
    while idx < nodes.len() {
        let end = (idx + 50_000).min(nodes.len());
        let tx = store.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO nodes
                 (dev_id,inode,parent_dev,parent_inode,name,kind,size,blocks,recursive_bytes,
                  recursive_inodes,uid,gid,mode,mtime,last_seen,deleted)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,0)",
            )?;
            let mut fts = tx.prepare("INSERT INTO names_fts(name,dev,ino) VALUES(?1,?2,?3)")?;
            for j in idx..end {
                let n = &nodes[j];
                if !primary[j] {
                    continue; // extra hardlink: no row, no FTS name (one per inode)
                }
                stmt.execute(params![
                    n.dev,
                    n.inode,
                    n.parent_dev,
                    n.parent_inode,
                    n.name,
                    n.kind.to_string(),
                    n.size,
                    n.blocks,
                    n.recursive,
                    n.rinodes,
                    n.uid,
                    n.gid,
                    n.mode,
                    n.mtime,
                    now,
                ])?;
                fts.execute(params![n.name, n.dev, n.inode])?;
            }
        }
        tx.commit()?;
        idx = end;
    }

    let stats = ScanStats {
        files: n_files.load(Ordering::Relaxed),
        dirs: n_dirs.load(Ordering::Relaxed),
        bytes: root_total,
        errors: n_errors.load(Ordering::Relaxed),
    };

    done.store(true, Ordering::Relaxed);
    if let Some(t) = progress_thread {
        t.join().ok();
    }
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

/// A node captured during the parallel walk. parent (dev,inode) and depth are
/// recorded at walk time so post-processing needs no path map. `recursive` is
/// filled in phase 2.
struct RawNode {
    dev: i64,
    inode: i64,
    parent_dev: i64,
    parent_inode: i64,
    depth: u32,
    kind: char,
    size: i64,
    blocks: i64,
    uid: i64,
    gid: i64,
    mode: i64,
    mtime: i64,
    name: String,
    recursive: i64,
    rinodes: i64,
}

/// Parallel directory walk (jwalk). All per-entry stat work happens on worker
/// threads; nodes stream back over a channel. Pseudo-fs, excludes, and (with
/// one_file_system) other mounts are pruned so we never descend into them.
#[allow(clippy::too_many_arguments)]
fn parallel_collect(
    root: &Path,
    root_dev: i64,
    root_inode: i64,
    opts: &ScanOptions,
    n_files: &Arc<AtomicU64>,
    n_dirs: &Arc<AtomicU64>,
    n_bytes: &Arc<AtomicI64>,
    n_errors: &Arc<AtomicU64>,
) -> Vec<RawNode> {
    use jwalk::WalkDirGeneric;

    let (tx, rx) = crossbeam_channel::unbounded::<RawNode>();
    // canonicalize excludes so relative paths match the canonical entry paths
    let exclude: Vec<PathBuf> = opts
        .exclude
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let include_pseudo = opts.include_pseudo;
    let one_fs = opts.one_file_system;
    let nf = n_files.clone();
    let nd = n_dirs.clone();
    let nb = n_bytes.clone();
    let ne = n_errors.clone();

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);

    {
        let walk = WalkDirGeneric::<((), ())>::new(root)
            .skip_hidden(false)
            .follow_links(false)
            .parallelism(jwalk::Parallelism::RayonNewPool(threads))
            .process_read_dir(move |_depth, dir_path, _state, children| {
                // stat the directory once to learn the parent (dev,inode); all of
                // these children share it. depth is the dir's depth + 1.
                let (pdev, pino) = match std::fs::symlink_metadata(dir_path) {
                    Ok(m) => (m.dev() as i64, m.ino() as i64),
                    Err(_) => (root_dev, root_inode),
                };
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
                    if exclude.iter().any(|x| path.starts_with(x)) {
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
                    if is_dir && !include_pseudo && is_pseudo_fs(&path) {
                        return false;
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
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let _ = tx.send(RawNode {
                        dev,
                        inode: m.ino() as i64,
                        parent_dev: pdev,
                        parent_inode: pino,
                        depth: cdepth,
                        kind,
                        size: m.size() as i64,
                        blocks,
                        uid: m.uid() as i64,
                        gid: m.gid() as i64,
                        mode: m.mode() as i64,
                        mtime: m.mtime(),
                        name,
                        recursive: 0,
                        rinodes: 1,
                    });
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

    rx.into_iter().collect()
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
