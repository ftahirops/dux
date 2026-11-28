//! Fast filesystem walker that builds an in-memory folder tree.
//!
//! This is the attribution half of the counters-first design: the sentinel
//! knows *that* a filesystem grew (from statvfs); a walk says *where*.
//!
//! Speed comes from doing as little per entry as possible:
//! - directories are read with raw `getdents64` into a 64 KiB buffer,
//! - each entry is stat-ed relative to its open parent directory (`fstatat`),
//!   so no full path strings are built for files,
//! - directories are processed in parallel on a small rayon pool,
//! - nothing is written anywhere during the walk.
//!
//! The result keeps one node per directory, never one per file: own and
//! cumulative allocated bytes, file and directory counts, the newest change
//! time, and the five largest files directly inside it.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub const TOP_FILES: usize = 5;
/// Files smaller than this are never listed as a folder's largest files.
/// Keeping names only for big files is most of the memory saving.
pub const TOP_MIN_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DirNode {
    pub name: String,
    /// Allocated bytes of the files directly in this directory (plus the
    /// directory's own blocks).
    pub own_bytes: u64,
    /// own_bytes plus every descendant.
    pub total_bytes: u64,
    /// Apparent size (st_size) of everything under this directory.
    pub total_apparent: u64,
    pub own_files: u64,
    /// Files plus directories under this directory, itself included.
    pub total_entries: u64,
    /// Newest ctime of anything under this directory (seconds).
    pub newest_change: i64,
    pub top_files: Vec<FileEntry>,
    pub children: Vec<DirNode>,
    /// Children skipped because they are another filesystem or a mount point.
    pub skipped_mounts: u32,
    pub errors: u32,
    /// Project marker files found directly in this directory (see `MARK_*`).
    pub markers: u16,
    /// Newest ctime of the entries directly in this directory.
    pub own_change: i64,
}

pub const MARK_NODE: u16 = 1;
pub const MARK_RUST: u16 = 1 << 1;
pub const MARK_GO: u16 = 1 << 2;
pub const MARK_PYTHON: u16 = 1 << 3;
pub const MARK_PHP: u16 = 1 << 4;
pub const MARK_RUBY: u16 = 1 << 5;
pub const MARK_JAVA: u16 = 1 << 6;
pub const MARK_COMPOSE: u16 = 1 << 7;
pub const MARK_GIT: u16 = 1 << 8;
pub const MARK_CMAKE: u16 = 1 << 9;
pub const MARK_DOTNET: u16 = 1 << 10;

/// Marker bit for a file name, if it identifies a project.
pub fn marker_for(name: &[u8]) -> u16 {
    match name {
        b"package.json" => MARK_NODE,
        b"Cargo.toml" => MARK_RUST,
        b"go.mod" => MARK_GO,
        b"pyproject.toml" | b"requirements.txt" | b"setup.py" | b"Pipfile" => MARK_PYTHON,
        b"composer.json" => MARK_PHP,
        b"Gemfile" => MARK_RUBY,
        b"pom.xml" | b"build.gradle" | b"build.gradle.kts" => MARK_JAVA,
        b"docker-compose.yml" | b"docker-compose.yaml" | b"compose.yml" | b"compose.yaml" => {
            MARK_COMPOSE
        }
        b"CMakeLists.txt" => MARK_CMAKE,
        n if n.ends_with(b".csproj") || n.ends_with(b".sln") => MARK_DOTNET,
        _ => 0,
    }
}

/// Metadata of one file, offered to a walk's file filter. Not every filter
/// reads every field.
#[allow(dead_code)]
pub struct FileView<'a> {
    pub dir: &'a [u8],
    pub name: &'a [u8],
    pub bytes: u64,
    pub apparent: u64,
    pub mtime: i64,
    pub uid: u32,
    pub mode: u32,
    pub nlink: u64,
}

/// A file a walk's filter selected, with its full path.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundFile {
    pub path: String,
    pub bytes: u64,
    pub apparent: u64,
    pub mtime: i64,
    pub uid: u32,
    pub dev: u64,
    pub ino: u64,
}

type FileFilter = dyn Fn(&FileView) -> bool + Send + Sync;

/// Extra work a walk can do while it already has every file's metadata.
#[derive(Default)]
pub struct WalkOptions {
    /// Collect files this returns true for (up to `max_found`).
    pub filter: Option<Box<FileFilter>>,
    pub max_found: usize,
    /// Incremented once per entry, for progress display.
    pub progress: Option<std::sync::Arc<AtomicU64>>,
}

#[derive(Clone, Debug, Default)]
pub struct WalkStats {
    pub entries: u64,
    pub dirs: u64,
    pub files: u64,
    pub errors: u64,
    pub hardlinks_deduped: u64,
    pub seconds: f64,
}

impl WalkStats {
    pub fn entries_per_second(&self) -> f64 {
        if self.seconds > 0.0 {
            self.entries as f64 / self.seconds
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tree {
    pub root_path: String,
    pub root: DirNode,
    pub stats: WalkStats,
    /// Unix time the walk started.
    pub started_at: i64,
}

struct Ctx {
    dev: u64,
    /// Mount points under the root (other than the root itself) that must not
    /// be entered, even when they are on the same device (bind mounts).
    mount_points: HashSet<Vec<u8>>,
    /// (ino) of files with more than one link, so each is counted once.
    hardlinks: Vec<Mutex<HashSet<u64>>>,
    entries: AtomicU64,
    dirs: AtomicU64,
    files: AtomicU64,
    errors: AtomicU64,
    deduped: AtomicU64,
    opts: WalkOptions,
    found: Mutex<Vec<FoundFile>>,
}

const SHARDS: usize = 64;

fn mount_points_under(root: &[u8]) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
        for line in text.lines() {
            let Some(p) = line.split(' ').nth(4) else {
                continue;
            };
            let p = crate::sentinel::unescape_mount(p).into_bytes();
            if p != root && (root == b"/" || p.starts_with(root)) {
                out.insert(p);
            }
        }
    }
    out
}

/// Read every entry of an open directory with getdents64.
fn read_entries(fd: libc::c_int) -> std::io::Result<Vec<(CString, u8)>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // SAFETY: buf is valid for buf.len() bytes; fd is an open directory.
        let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(out);
        }
        let mut off = 0usize;
        while off < n as usize {
            // linux_dirent64: u64 ino, i64 off, u16 reclen, u8 type, name[]
            let reclen = u16::from_ne_bytes([buf[off + 16], buf[off + 17]]) as usize;
            let d_type = buf[off + 18];
            let name_bytes = &buf[off + 19..off + reclen];
            let end = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = &name_bytes[..end];
            if name != b"." && name != b".." {
                if let Ok(c) = CString::new(name) {
                    out.push((c, d_type));
                }
            }
            off += reclen;
        }
    }
}

fn open_dir_at(parent: libc::c_int, name: &CStr) -> libc::c_int {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // O_NOATIME avoids an atime write per directory, but only the owner or a
    // privileged caller may use it; retry without it on EPERM.
    // SAFETY: name is NUL-terminated; parent is an open directory fd or AT_FDCWD.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags | libc::O_NOATIME) };
    if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
        // SAFETY: as above.
        return unsafe { libc::openat(parent, name.as_ptr(), flags) };
    }
    fd
}

fn stat_at(parent: libc::c_int, name: &CStr) -> Option<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: name is NUL-terminated; st is written on success.
    let rc = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    // SAFETY: fstatat returned 0, so st is initialized.
    (rc == 0).then(|| unsafe { st.assume_init() })
}

fn push_top(top: &mut Vec<FileEntry>, name: &CStr, bytes: u64) {
    if top.len() < TOP_FILES || top.last().is_some_and(|l| bytes > l.bytes) {
        let e = FileEntry {
            name: String::from_utf8_lossy(name.to_bytes()).into_owned(),
            bytes,
        };
        let pos = top
            .iter()
            .position(|t| bytes > t.bytes)
            .unwrap_or(top.len());
        top.insert(pos, e);
        top.truncate(TOP_FILES);
    }
}

/// Walk one directory whose fd is already open. `path` is its full path,
/// used only to recognise mount points below it.
#[allow(clippy::unnecessary_cast)]
fn walk_dir(ctx: &Ctx, fd: libc::c_int, path: &[u8], name: String, self_blocks: u64) -> DirNode {
    let mut node = DirNode {
        name,
        own_bytes: self_blocks,
        ..DirNode::default()
    };
    ctx.dirs.fetch_add(1, Ordering::Relaxed);
    let entries = match read_entries(fd) {
        Ok(e) => e,
        Err(_) => {
            node.errors += 1;
            ctx.errors.fetch_add(1, Ordering::Relaxed);
            return finish(node);
        }
    };
    let mut subdirs: Vec<(CString, u64)> = Vec::new();
    for (ename, d_type) in entries {
        ctx.entries.fetch_add(1, Ordering::Relaxed);
        if let Some(p) = &ctx.opts.progress {
            p.fetch_add(1, Ordering::Relaxed);
        }
        // A known mount point is skipped by name, before any stat: looking up
        // a dead network mount (NFS, CIFS) would block this thread forever.
        if !ctx.mount_points.is_empty() {
            let mut child = path.to_vec();
            if child != b"/" {
                child.push(b'/');
            }
            child.extend_from_slice(ename.to_bytes());
            if ctx.mount_points.contains(&child) {
                node.skipped_mounts += 1;
                continue;
            }
        }
        let Some(st) = stat_at(fd, &ename) else {
            node.errors += 1;
            ctx.errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let is_dir = (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
        let _ = d_type;
        let bytes = (st.st_blocks as u64).saturating_mul(512);
        node.newest_change = node.newest_change.max(st.st_ctime as i64);
        if is_dir {
            if ename.to_bytes() == b".git" {
                node.markers |= MARK_GIT;
            }
            if st.st_dev as u64 != ctx.dev {
                node.skipped_mounts += 1;
                continue;
            }
            let mut child_path = path.to_vec();
            if child_path != b"/" {
                child_path.push(b'/');
            }
            child_path.extend_from_slice(ename.to_bytes());
            if ctx.mount_points.contains(&child_path) {
                node.skipped_mounts += 1;
                continue;
            }
            subdirs.push((ename, bytes));
            continue;
        }
        ctx.files.fetch_add(1, Ordering::Relaxed);
        node.markers |= marker_for(ename.to_bytes());
        if st.st_nlink > 1 {
            let ino = st.st_ino as u64;
            let shard = &ctx.hardlinks[(ino as usize) % SHARDS];
            if !shard.lock().unwrap().insert(ino) {
                ctx.deduped.fetch_add(1, Ordering::Relaxed);
                node.own_files += 1;
                continue;
            }
        }
        node.own_bytes += bytes;
        node.total_apparent += st.st_size.max(0) as u64;
        node.own_files += 1;
        if bytes >= TOP_MIN_BYTES {
            push_top(&mut node.top_files, &ename, bytes);
        }
        if let Some(filter) = &ctx.opts.filter {
            let view = FileView {
                dir: path,
                name: ename.to_bytes(),
                bytes,
                apparent: st.st_size.max(0) as u64,
                mtime: st.st_mtime as i64,
                uid: st.st_uid,
                mode: st.st_mode,
                nlink: st.st_nlink as u64,
            };
            if filter(&view) {
                let mut found = ctx.found.lock().unwrap();
                if found.len() < ctx.opts.max_found {
                    let mut p = path.to_vec();
                    if p != b"/" {
                        p.push(b'/');
                    }
                    p.extend_from_slice(ename.to_bytes());
                    found.push(FoundFile {
                        path: String::from_utf8_lossy(&p).into_owned(),
                        bytes,
                        apparent: view.apparent,
                        mtime: view.mtime,
                        uid: st.st_uid,
                        dev: st.st_dev as u64,
                        ino: st.st_ino as u64,
                    });
                }
            }
        }
    }
    use rayon::prelude::*;
    node.children = subdirs
        .into_par_iter()
        .map(|(cname, blocks)| {
            let child_name = String::from_utf8_lossy(cname.to_bytes()).into_owned();
            let mut child_path = path.to_vec();
            if child_path != b"/" {
                child_path.push(b'/');
            }
            child_path.extend_from_slice(cname.to_bytes());
            let cfd = open_dir_at(fd, &cname);
            if cfd < 0 {
                ctx.errors.fetch_add(1, Ordering::Relaxed);
                return DirNode {
                    name: child_name,
                    own_bytes: blocks,
                    errors: 1,
                    ..DirNode::default()
                }
                .finished();
            }
            let n = walk_dir(ctx, cfd, &child_path, child_name, blocks);
            // SAFETY: cfd was opened above and is closed once.
            unsafe { libc::close(cfd) };
            n
        })
        .collect();
    node.children.sort_by(|a, b| a.name.cmp(&b.name));
    finish(node)
}

impl DirNode {
    fn finished(self) -> DirNode {
        finish(self)
    }

    /// Find a descendant by a relative path like "var/log".
    pub fn find(&self, rel: &str) -> Option<&DirNode> {
        let mut cur = self;
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            let i = cur
                .children
                .binary_search_by(|c| c.name.as_str().cmp(part))
                .ok()?;
            cur = &cur.children[i];
        }
        Some(cur)
    }
}

fn finish(mut n: DirNode) -> DirNode {
    n.own_change = n.newest_change;
    n.total_bytes = n.own_bytes + n.children.iter().map(|c| c.total_bytes).sum::<u64>();
    n.total_apparent += n.children.iter().map(|c| c.total_apparent).sum::<u64>();
    n.total_entries = 1 + n.own_files + n.children.iter().map(|c| c.total_entries).sum::<u64>();
    n.newest_change = n
        .children
        .iter()
        .map(|c| c.newest_change)
        .fold(n.newest_change, i64::max);
    n
}

/// Walk `root` (one filesystem only) with `threads` worker threads.
pub fn walk(root: &str, threads: usize) -> Result<Tree> {
    walk_with(root, threads, WalkOptions::default()).map(|(t, _)| t)
}

/// Walk and also collect the files `opts.filter` selects.
#[allow(clippy::unnecessary_cast)]
pub fn walk_with(root: &str, threads: usize, opts: WalkOptions) -> Result<(Tree, Vec<FoundFile>)> {
    let started_at = crate::util::now_secs();
    let t = Instant::now();
    let croot = CString::new(root).context("path contains NUL")?;
    let st = stat_at(libc::AT_FDCWD, &croot).with_context(|| format!("stat {root}"))?;
    let root_bytes = OsStr::new(root).as_bytes().to_vec();
    let ctx = Ctx {
        dev: st.st_dev as u64,
        mount_points: mount_points_under(&root_bytes),
        hardlinks: (0..SHARDS).map(|_| Mutex::new(HashSet::new())).collect(),
        entries: AtomicU64::new(0),
        dirs: AtomicU64::new(0),
        files: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        deduped: AtomicU64::new(0),
        opts,
        found: Mutex::new(Vec::new()),
    };
    let fd = open_dir_at(libc::AT_FDCWD, &croot);
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("open {root}"));
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|i| format!("dux-walk-{i}"))
        .build()
        .context("building walk thread pool")?;
    let root_node = pool.install(|| {
        walk_dir(
            &ctx,
            fd,
            &root_bytes,
            root.to_string(),
            (st.st_blocks as u64) * 512,
        )
    });
    // SAFETY: fd was opened above and is closed once.
    unsafe { libc::close(fd) };
    let stats = WalkStats {
        entries: ctx.entries.load(Ordering::Relaxed),
        dirs: ctx.dirs.load(Ordering::Relaxed),
        files: ctx.files.load(Ordering::Relaxed),
        errors: ctx.errors.load(Ordering::Relaxed),
        hardlinks_deduped: ctx.deduped.load(Ordering::Relaxed),
        seconds: t.elapsed().as_secs_f64(),
    };
    let found = ctx.found.into_inner().unwrap_or_default();
    Ok((
        Tree {
            root_path: root.to_string(),
            root: root_node,
            stats,
            started_at,
        },
        found,
    ))
}

/// One folder whose size changed between two walks.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    pub before: u64,
    pub after: u64,
}

impl Change {
    pub fn delta(&self) -> i64 {
        self.after as i64 - self.before as i64
    }
}

/// Folders that grew (or shrank) by at least `min_bytes`, reported at the
/// most specific level: a folder is listed only if no single child explains
/// 80% or more of its change. Largest changes first.
pub fn diff(old: &DirNode, new: &DirNode, min_bytes: u64) -> Vec<Change> {
    let mut out = Vec::new();
    diff_rec(Some(old), new, "", min_bytes, &mut out);
    out.sort_by_key(|c| std::cmp::Reverse(c.delta().unsigned_abs()));
    out
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else if prefix.ends_with('/') {
        format!("{prefix}{name}")
    } else {
        format!("{prefix}/{name}")
    }
}

fn diff_rec(old: Option<&DirNode>, new: &DirNode, prefix: &str, min: u64, out: &mut Vec<Change>) {
    let path = join(prefix, &new.name);
    let before = old.map_or(0, |o| o.total_bytes);
    let delta = new.total_bytes as i64 - before as i64;
    if delta.unsigned_abs() < min {
        return;
    }
    let mut explained = false;
    for c in &new.children {
        let oc = old.and_then(|o| {
            o.children
                .binary_search_by(|x| x.name.cmp(&c.name))
                .ok()
                .map(|i| &o.children[i])
        });
        let cd = c.total_bytes as i64 - oc.map_or(0, |o| o.total_bytes as i64);
        if cd.signum() == delta.signum() && cd.unsigned_abs() * 10 >= delta.unsigned_abs() * 8 {
            explained = true;
        }
        diff_rec(oc, c, &path, min, out);
    }
    // Children that disappeared count toward shrinking.
    if let Some(o) = old {
        for oc in &o.children {
            if new
                .children
                .binary_search_by(|x| x.name.cmp(&oc.name))
                .is_err()
            {
                let cd = -(oc.total_bytes as i64);
                if cd.signum() == delta.signum()
                    && cd.unsigned_abs() * 10 >= delta.unsigned_abs() * 8
                {
                    explained = true;
                }
                if oc.total_bytes >= min {
                    out.push(Change {
                        path: join(&path, &oc.name),
                        before: oc.total_bytes,
                        after: 0,
                    });
                }
            }
        }
    }
    if !explained {
        out.push(Change {
            path,
            before,
            after: new.total_bytes,
        });
    }
}

/// Folders holding the most entries (files and directories), drilled down
/// to the most specific folder: from each top-level folder, keep descending
/// while one child holds at least half of the entries.
pub fn most_entries(root: &DirNode, n: usize) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for top in &root.children {
        let mut node = top;
        let mut path = join(&root.name, &top.name);
        while let Some(c) = node.children.iter().max_by_key(|c| c.total_entries) {
            if c.total_entries * 2 < node.total_entries {
                break;
            }
            path = join(&path, &c.name);
            node = c;
        }
        out.push((path, node.total_entries));
    }
    out.sort_by_key(|(_, e)| std::cmp::Reverse(*e));
    out.truncate(n);
    out
}

/// The `n` largest folders at a given depth below the root (depth 1 = children).
pub fn largest_at_depth(root: &DirNode, depth: usize, n: usize) -> Vec<(String, u64)> {
    let mut acc = Vec::new();
    fn rec(node: &DirNode, prefix: &str, d: usize, depth: usize, acc: &mut Vec<(String, u64)>) {
        let path = join(prefix, &node.name);
        if d == depth {
            acc.push((path, node.total_bytes));
            return;
        }
        for c in &node.children {
            rec(c, &path, d + 1, depth, acc);
        }
    }
    rec(root, "", 0, depth, &mut acc);
    acc.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    acc.truncate(n);
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dux-walker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &std::path::Path, bytes: usize) {
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(&vec![7u8; bytes]).unwrap();
        f.sync_all().unwrap();
    }

    #[test]
    fn walk_counts_match_du_semantics_and_dedupes_hardlinks() {
        let d = tmpdir("basic");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::create_dir_all(d.join("c")).unwrap();
        write(&d.join("a/b/big"), 1 << 20);
        write(&d.join("a/small"), 4096);
        write(&d.join("c/x"), 100_000);
        std::fs::hard_link(d.join("c/x"), d.join("c/x-link")).unwrap();
        std::os::unix::fs::symlink("/etc", d.join("a/etc-link")).unwrap();
        let t = walk(d.to_str().unwrap(), 2).unwrap();
        assert_eq!(t.stats.errors, 0);
        assert_eq!(t.stats.hardlinks_deduped, 1);
        // 3 dirs + root
        assert_eq!(t.stats.dirs, 4);
        let a = t.root.find("a").unwrap();
        let b = t.root.find("a/b").unwrap();
        assert!(b.own_bytes >= 1 << 20);
        assert_eq!(b.top_files[0].name, "big");
        assert!(a.total_bytes >= b.total_bytes + 4096);
        let c = t.root.find("c").unwrap();
        assert_eq!(c.own_files, 2, "both names counted as files");
        // The hard link adds no bytes: c holds one copy of x plus its own block.
        assert!(c.own_bytes < 2 * 100_000);
        assert_eq!(
            t.root.total_bytes,
            t.root.own_bytes + a.total_bytes + c.total_bytes
        );
        // Symlinks are not followed.
        assert!(t.root.find("a/etc-link").is_none());
        // Compare with du -sB1 when available.
        if let Ok(out) = std::process::Command::new("du")
            .args(["-sxB1", d.to_str().unwrap()])
            .output()
        {
            let du: u64 = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(t.root.total_bytes, du, "walker total must equal du");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn top_files_keep_the_five_largest_in_order() {
        let mut top = Vec::new();
        for (i, s) in [5u64, 50, 1, 70, 20, 90, 3, 60].iter().enumerate() {
            push_top(&mut top, &CString::new(format!("f{i}")).unwrap(), *s);
        }
        let sizes: Vec<u64> = top.iter().map(|f| f.bytes).collect();
        assert_eq!(sizes, vec![90, 70, 60, 50, 20]);
    }

    #[test]
    fn diff_names_the_most_specific_growing_folder() {
        let d = tmpdir("diff");
        std::fs::create_dir_all(d.join("var/log/app")).unwrap();
        std::fs::create_dir_all(d.join("var/cache")).unwrap();
        std::fs::create_dir_all(d.join("home")).unwrap();
        write(&d.join("var/cache/c"), 200_000);
        write(&d.join("home/doc"), 300_000);
        let before = walk(d.to_str().unwrap(), 2).unwrap();
        write(&d.join("var/log/app/big.log"), 3 << 20);
        std::fs::remove_file(d.join("home/doc")).unwrap();
        let after = walk(d.to_str().unwrap(), 2).unwrap();
        let ch = diff(&before.root, &after.root, 256 * 1024);
        assert!(!ch.is_empty());
        assert!(ch[0].path.ends_with("var/log/app"), "{ch:?}");
        assert!(ch[0].delta() >= 3 << 20);
        assert!(
            ch.iter().any(|c| c.path.ends_with("home") && c.delta() < 0),
            "{ch:?}"
        );
        assert!(
            !ch.iter()
                .any(|c| c.path.ends_with("/var") || c.path.ends_with("var/log")),
            "parents explained by a child are not listed: {ch:?}"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn most_entries_drills_down_to_the_crowded_folder() {
        let d = tmpdir("entries");
        std::fs::create_dir_all(d.join("var/lib/app/sessions")).unwrap();
        std::fs::create_dir_all(d.join("usr")).unwrap();
        for i in 0..200 {
            std::fs::write(d.join(format!("var/lib/app/sessions/s{i}")), b"x").unwrap();
        }
        for i in 0..20 {
            std::fs::write(d.join(format!("usr/f{i}")), b"x").unwrap();
        }
        let t = walk(d.to_str().unwrap(), 2).unwrap();
        let m = most_entries(&t.root, 2);
        assert!(m[0].0.ends_with("var/lib/app/sessions"), "{m:?}");
        assert!(m[0].1 >= 200);
        assert!(m[1].0.ends_with("/usr"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn walk_with_collects_filtered_files_and_marks_projects() {
        let d = tmpdir("filter");
        std::fs::create_dir_all(d.join("app/.git")).unwrap();
        std::fs::create_dir_all(d.join("app/node_modules/x")).unwrap();
        std::fs::write(d.join("app/package.json"), b"{}").unwrap();
        std::fs::write(d.join("app/node_modules/x/package.json"), b"{}").unwrap();
        write(&d.join("app/server.log"), 200_000);
        write(&d.join("app/small.log"), 10);
        let opts = WalkOptions {
            filter: Some(Box::new(|f: &FileView| {
                f.name.ends_with(b".log") && f.apparent >= 4096
            })),
            max_found: 10,
            progress: Some(std::sync::Arc::new(AtomicU64::new(0))),
        };
        let (t, found) = walk_with(d.to_str().unwrap(), 2, opts).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].path.ends_with("/app/server.log"));
        let app = t.root.find("app").unwrap();
        assert_eq!(app.markers & (MARK_NODE | MARK_GIT), MARK_NODE | MARK_GIT);
        assert_eq!(t.root.markers, 0);
        assert_eq!(marker_for(b"Cargo.toml"), MARK_RUST);
        assert_eq!(marker_for(b"App.csproj"), MARK_DOTNET);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn largest_at_depth_ranks_children() {
        let d = tmpdir("depth");
        for (n, s) in [("x", 10_000usize), ("y", 300_000), ("z", 90_000)] {
            std::fs::create_dir_all(d.join(n)).unwrap();
            write(&d.join(n).join("f"), s);
        }
        let t = walk(d.to_str().unwrap(), 3).unwrap();
        let top = largest_at_depth(&t.root, 1, 2);
        assert!(
            top[0].0.ends_with("/y") && top[1].0.ends_with("/z"),
            "{top:?}"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unreadable_directories_are_counted_not_fatal() {
        let d = tmpdir("perm");
        std::fs::create_dir_all(d.join("locked/inner")).unwrap();
        write(&d.join("locked/inner/f"), 1000);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let t = walk(d.to_str().unwrap(), 2).unwrap();
        // Root can still read it; an unprivileged user records an error.
        assert!(t.stats.errors == 0 || t.root.find("locked").is_some());
        std::fs::set_permissions(d.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }
}
