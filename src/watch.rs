use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
pub fn run_daemon(
    db: &Path,
    root: &Path,
    flush_ms: u64,
    one_file_system: bool,
    alert: Option<AlertConfig>,
) -> Result<()> {
    let mut store = Store::open_rw(db)?;
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

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

    crate::util::write_heartbeat();
    let flush_every = Duration::from_millis(flush_ms);
    let mut last_flush = Instant::now();
    let mut last_ckpt = Instant::now();
    let mut last_alert: HashMap<(i64, i64), i64> = HashMap::new();
    let mut pending: HashMap<PathBuf, Op> = HashMap::new();
    let mut buf = [0u8; 1 << 15];

    loop {
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
                if let Err(e) = flush(&mut store, &mut pending) {
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
            crate::util::write_heartbeat();
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

/// Apply all pending ops in one transaction, in correctness-preserving phases:
///   B. deletes (deep-first)            — remove subtree, subtract totals
///   C. move-from                       — skip if the inode is relocating (move),
///                                        else treat as moved-out-of-tree delete
///   D. move-to                         — RELOCATE the inode in place (keep its
///                                        subtree; children reference the inode),
///                                        or insert if it's new to the tree
///   E. upserts (shallow-first)         — create / modify / hardlink
fn flush(store: &mut Store, pending: &mut HashMap<PathBuf, Op>) -> Result<()> {
    let now = now_secs();
    let items: Vec<(PathBuf, Op)> = pending.drain().collect();

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
    {
        let mut find = tx.prepare(
            "SELECT dev_id, inode, kind, recursive_bytes, recursive_inodes, blocks
             FROM nodes WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 AND deleted=0 LIMIT 1",
        )?;
        let mut find_inode = tx.prepare(
            "SELECT name, parent_dev, parent_inode, kind, recursive_bytes, recursive_inodes
             FROM nodes WHERE dev_id=?1 AND inode=?2 LIMIT 1",
        )?;
        let mut insert_node = tx.prepare(
            "INSERT OR REPLACE INTO nodes
             (dev_id,inode,parent_dev,parent_inode,name,kind,size,blocks,recursive_bytes,
              recursive_inodes,uid,gid,mode,mtime,last_seen,fts_rowid,deleted)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0)",
        )?;
        let mut insert_fts = tx.prepare("INSERT INTO names_fts(name,dev,ino) VALUES(?1,?2,?3)")?;
        let mut get_fts_rowid =
            tx.prepare("SELECT fts_rowid FROM nodes WHERE dev_id=?1 AND inode=?2")?;
        let mut upd_file = tx.prepare(
            "UPDATE nodes SET size=?3, blocks=?4, recursive_bytes=?4, mtime=?5, last_seen=?6
             WHERE dev_id=?1 AND inode=?2",
        )?;
        let mut relocate = tx.prepare(
            "UPDATE nodes SET name=?3, parent_dev=?4, parent_inode=?5, last_seen=?6, fts_rowid=?7
             WHERE dev_id=?1 AND inode=?2",
        )?;
        let mut bump = tx.prepare(
            "UPDATE nodes SET recursive_bytes=recursive_bytes+?3, recursive_inodes=recursive_inodes+?4
             WHERE dev_id=?1 AND inode=?2",
        )?;
        let mut get_parent =
            tx.prepare("SELECT parent_dev, parent_inode FROM nodes WHERE dev_id=?1 AND inode=?2")?;
        let mut descendants = tx.prepare(
            "WITH RECURSIVE sub(d,i,depth) AS (
                SELECT ?1,?2,0
                UNION ALL
                SELECT n.dev_id,n.inode,sub.depth+1 FROM nodes n
                JOIN sub ON n.parent_dev=sub.d AND n.parent_inode=sub.i
                WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode) AND sub.depth<4096
             ) SELECT s.d, s.i, n.fts_rowid FROM sub s
               JOIN nodes n ON n.dev_id=s.d AND n.inode=s.i",
        )?;
        let mut del_node = tx.prepare("DELETE FROM nodes WHERE dev_id=?1 AND inode=?2")?;
        // fast path: delete the name by its FTS docid (O(1)); slow fallback for
        // legacy rows whose fts_rowid was never recorded (pre-migration scans).
        let mut del_fts = tx.prepare("DELETE FROM names_fts WHERE rowid=?1")?;
        let mut del_fts_legacy = tx.prepare("DELETE FROM names_fts WHERE dev=?1 AND ino=?2")?;
        let mut log = tx.prepare(
            "INSERT INTO changes(ts,dev_id,inode,size_before,size_after,delta,event_type)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;

        // delete a node's whole subtree (rows + FTS); caller subtracts totals
        let del_subtree = |descendants: &mut rusqlite::Statement,
                           del_fts: &mut rusqlite::Statement,
                           del_fts_legacy: &mut rusqlite::Statement,
                           del_node: &mut rusqlite::Statement,
                           d: i64,
                           i: i64|
         -> Result<()> {
            let sub: Vec<(i64, i64, Option<i64>)> = descendants
                .query_map(params![d, i], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .filter_map(|x| x.ok())
                .collect();
            for (dd, ii, frow) in &sub {
                match frow {
                    Some(rid) => {
                        del_fts.execute(params![rid])?;
                    }
                    None => {
                        del_fts_legacy.execute(params![dd, ii])?;
                    }
                }
                del_node.execute(params![dd, ii])?;
            }
            Ok(())
        };

        // ---- Phase B: deletes ----
        for full in &deletes {
            let r: Result<()> = (|| {
                let (dir, name) = match split(full) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let (pdev, pino) = match stat_id(dir) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if let Ok((cdev, cino, _k, crb, cri, _b)) =
                    find.query_row(params![pdev, pino, name], row6)
                {
                    del_subtree(&mut descendants, &mut del_fts, &mut del_fts_legacy, &mut del_node, cdev, cino)?;
                    walk_ancestors(&mut bump, &mut get_parent, pdev, pino, -crb, -cri)?;
                    log.execute(params![now, cdev, cino, crb, 0i64, -crb, "delete"])?;
                }
                Ok(())
            })();
            if let Err(e) = r {
                tracing::debug!("delete {} failed: {e}", full.display());
            }
        }

        // ---- Phase C: move-from ----
        for full in &moved_from {
            let r: Result<()> = (|| {
                let (dir, name) = match split(full) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let (pdev, pino) = match stat_id(dir) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if let Ok((cdev, cino, _k, crb, cri, _b)) =
                    find.query_row(params![pdev, pino, name], row6)
                {
                    if moved_in.contains(&(cdev, cino)) {
                        return Ok(()); // it's a rename — move-to will relocate it
                    }
                    // genuinely left the watched tree: remove it
                    del_subtree(&mut descendants, &mut del_fts, &mut del_fts_legacy, &mut del_node, cdev, cino)?;
                    walk_ancestors(&mut bump, &mut get_parent, pdev, pino, -crb, -cri)?;
                    log.execute(params![now, cdev, cino, crb, 0i64, -crb, "moved_out"])?;
                }
                Ok(())
            })();
            if let Err(e) = r {
                tracing::debug!("move-from {} failed: {e}", full.display());
            }
        }

        // ---- Phase D: move-to (relocate the inode, keep its subtree) ----
        for full in &moved_to {
            let r: Result<()> = (|| {
                use std::os::unix::fs::MetadataExt;
                let m = match std::fs::symlink_metadata(full) {
                    Ok(m) => m,
                    Err(_) => return Ok(()),
                };
                let (dir, name) = match split(full) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let (npdev, npino) = match stat_id(dir) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let dev = m.dev() as i64;
                let ino = m.ino() as i64;
                let existing: Option<(String, i64, i64, String, i64, i64)> = find_inode
                    .query_row(params![dev, ino], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })
                    .ok();
                if let Some((_oldname, opdev, opino, _k, rb, ri)) = existing {
                    // Cycle guard: never relocate a node under its OWN subtree
                    // (a stale tree could otherwise install A→B→A and hang the
                    // recursive CTEs). Walk up from the new parent looking for
                    // the node being moved.
                    let (mut cd, mut ci) = (npdev, npino);
                    let mut g = 0;
                    let mut cycle = false;
                    loop {
                        if cd == dev && ci == ino {
                            cycle = true;
                            break;
                        }
                        g += 1;
                        if g > 4096 {
                            break;
                        }
                        match get_parent
                            .query_row(params![cd, ci], |r| {
                                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                            })
                            .ok()
                        {
                            Some((pd, pi)) if !(pd == cd && pi == ci) => {
                                cd = pd;
                                ci = pi;
                            }
                            _ => break,
                        }
                    }
                    if cycle {
                        tracing::debug!("skip move-to (would cycle): dev={dev} ino={ino}");
                        return Ok(());
                    }
                    // RELOCATE: move the row + its (unchanged) subtree to the new
                    // parent. Children reference (dev,ino) so they follow for free.
                    // refresh the FTS name: drop the old docid, insert the new
                    // name, and store its docid back on the relocated row.
                    let old_frow: Option<i64> = get_fts_rowid
                        .query_row(params![dev, ino], |r| r.get(0))
                        .ok()
                        .flatten();
                    match old_frow {
                        Some(rid) => {
                            del_fts.execute(params![rid])?;
                        }
                        None => {
                            del_fts_legacy.execute(params![dev, ino])?;
                        }
                    }
                    insert_fts.execute(params![name, dev, ino])?;
                    let new_frow = tx.last_insert_rowid();
                    walk_ancestors(&mut bump, &mut get_parent, opdev, opino, -rb, -ri)?;
                    relocate.execute(params![dev, ino, name, npdev, npino, now, new_frow])?;
                    walk_ancestors(&mut bump, &mut get_parent, npdev, npino, rb, ri)?;
                    log.execute(params![now, dev, ino, rb, rb, 0i64, "rename"])?;
                } else {
                    // moved in from outside the tree -> treat as a fresh create
                    let blocks = (m.blocks() as i64) * 512;
                    let kind = kind_of(&m);
                    insert_fts.execute(params![name, dev, ino])?;
                    let frow = tx.last_insert_rowid();
                    insert_node.execute(params![
                        dev,
                        ino,
                        npdev,
                        npino,
                        name,
                        kind,
                        m.size() as i64,
                        blocks,
                        blocks,
                        1i64,
                        m.uid() as i64,
                        m.gid() as i64,
                        m.mode() as i64,
                        m.mtime(),
                        now,
                        frow
                    ])?;
                    walk_ancestors(&mut bump, &mut get_parent, npdev, npino, blocks, 1)?;
                    log.execute(params![now, dev, ino, 0i64, blocks, blocks, "create"])?;
                }
                Ok(())
            })();
            if let Err(e) = r {
                tracing::debug!("move-to {} failed: {e}", full.display());
            }
        }

        // ---- Phase E: upserts (create / modify / hardlink) ----
        for full in &upserts {
            let r: Result<()> = (|| {
                use std::os::unix::fs::MetadataExt;
                let m = match std::fs::symlink_metadata(full) {
                    Ok(m) => m,
                    Err(_) => return Ok(()),
                };
                let (dir, name) = match split(full) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let (pdev, pino) = match stat_id(dir) {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let dev = m.dev() as i64;
                let ino = m.ino() as i64;
                let blocks = (m.blocks() as i64) * 512;
                let size = m.size() as i64;
                let mtime = m.mtime();
                let is_dir = m.is_dir();
                let kind = kind_of(&m);

                let existing: Option<(i64, i64, String, i64, i64, i64)> =
                    find.query_row(params![pdev, pino, name], row6).ok();

                match existing {
                    Some((edev, eino, _ek, erb, _eri, _eb)) if edev == dev && eino == ino => {
                        if is_dir {
                            return Ok(()); // dir totals driven by children
                        }
                        let delta = blocks - erb;
                        upd_file.execute(params![dev, ino, size, blocks, mtime, now])?;
                        if delta != 0 {
                            walk_ancestors(&mut bump, &mut get_parent, pdev, pino, delta, 0)?;
                            log.execute(params![now, dev, ino, erb, blocks, delta, "modify"])?;
                        }
                    }
                    other => {
                        if let Some((edev, eino, _ek, erb, eri, _eb)) = other {
                            // name reused by a DIFFERENT inode: drop the stale occupant
                            del_subtree(&mut descendants, &mut del_fts, &mut del_fts_legacy, &mut del_node, edev, eino)?;
                            walk_ancestors(&mut bump, &mut get_parent, pdev, pino, -erb, -eri)?;
                        }
                        // Skip if this inode is already indexed under another name
                        // (a hardlink): count it once, and keep one FTS name per
                        // inode so a search never resolves to a different path.
                        let is_hardlink =
                            find_inode.query_row(params![dev, ino], |_| Ok(())).is_ok();
                        if !is_hardlink {
                            insert_fts.execute(params![name, dev, ino])?;
                            let frow = tx.last_insert_rowid();
                            insert_node.execute(params![
                                dev,
                                ino,
                                pdev,
                                pino,
                                name,
                                kind,
                                size,
                                blocks,
                                blocks,
                                1i64,
                                m.uid() as i64,
                                m.gid() as i64,
                                m.mode() as i64,
                                mtime,
                                now,
                                frow
                            ])?;
                            walk_ancestors(&mut bump, &mut get_parent, pdev, pino, blocks, 1)?;
                            log.execute(params![now, dev, ino, 0i64, blocks, blocks, "create"])?;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(e) = r {
                tracing::debug!("upsert {} failed: {e}", full.display());
            }
        }
    }
    tx.commit()?;
    crate::util::write_heartbeat();
    store
        .conn
        .execute("DELETE FROM changes WHERE ts < ?1", params![now - 86400])?;
    pending.clear();
    Ok(())
}

/// (parent_dir, file_name) from a full path.
fn split(full: &Path) -> Option<(&Path, String)> {
    let dir = full.parent()?;
    let name = full.file_name()?.to_string_lossy().into_owned();
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

/// row mapper: (dev, inode, kind, recursive_bytes, recursive_inodes, blocks)
fn row6(r: &rusqlite::Row) -> rusqlite::Result<(i64, i64, String, i64, i64, i64)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

/// Walk ancestors from (dev,ino) upward, applying byte/inode deltas once each.
fn walk_ancestors(
    bump: &mut rusqlite::Statement,
    get_parent: &mut rusqlite::Statement,
    sdev: i64,
    sino: i64,
    dbytes: i64,
    dinodes: i64,
) -> Result<()> {
    let (mut cd, mut ci) = (sdev, sino);
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 4096 {
            break;
        }
        bump.execute(params![cd, ci, dbytes, dinodes])?;
        let nxt: Option<(i64, i64)> = get_parent
            .query_row(params![cd, ci], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok();
        match nxt {
            Some((pd, pi)) if !(pd == cd && pi == ci) => {
                cd = pd;
                ci = pi;
            }
            _ => break,
        }
    }
    Ok(())
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
    let cutoff = now - cfg.window;
    let mut stmt = store.conn.prepare(
        "SELECT dev_id, inode, SUM(delta) d FROM changes
         WHERE ts >= ?1 GROUP BY dev_id, inode HAVING d >= ?2 ORDER BY d DESC",
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
