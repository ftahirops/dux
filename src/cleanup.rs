//! `dux cleanup`: safe, levelled disk cleanup. Preview by default.
//!
//! Level 0  monitor only (nothing to do)
//! Level 1  shrink the systemd journal to a budget
//! Level 2  + rotated logs (oldest first, newest 3 kept), and big or fast
//!            active logs when the disk is at least 80% full
//! Level 3  + data that rebuilds itself: package caches, old temp files,
//!            old core dumps, the Docker build cache
//!
//! Safety rules applied to every action:
//! - nothing runs without `--apply`; the preview lists every action
//! - `/etc/dux/disable-actions` existing stops all actions
//! - symlinks are never followed and other filesystems are never entered
//! - files open by any process are never removed or truncated
//! - security and accounting logs are never touched
//! - commands run without a shell, with fixed arguments
//! - every applied action is appended to an audit log

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const KILL_SWITCH: &str = "/etc/dux/disable-actions";
pub const AUDIT_LOG: &str = "/var/log/dux/actions.log";

#[derive(Clone, Debug)]
pub struct Options {
    pub level: u8,
    /// Filesystem root the plan works under; "/" in production, a temp dir in tests.
    pub root: PathBuf,
    pub journal_max: u64,
    pub journal_max_age: String,
    pub big_log: u64,
    pub tail_keep: u64,
    pub rotated_keep: usize,
    /// Big active logs are only touched at or above this used %.
    pub logs_min_used_pct: f64,
    pub core_keep: usize,
    pub core_min_age_days: u64,
    /// Current used % of the filesystem holding /var/log.
    pub used_pct: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            level: 0,
            root: PathBuf::from("/"),
            journal_max: 1 << 30,
            journal_max_age: "30d".into(),
            big_log: 50 << 20,
            tail_keep: 10 << 20,
            rotated_keep: 3,
            logs_min_used_pct: 80.0,
            core_keep: 5,
            core_min_age_days: 14,
            used_pct: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run a fixed command (program and arguments, no shell).
    Run { program: String, args: Vec<String> },
    /// Remove one regular file that no process has open.
    Remove { path: PathBuf },
    /// Copy the last `keep` bytes to `<path>.dux-tail`, then truncate in place.
    TailAndTruncate { path: PathBuf, keep: u64 },
    /// Remove a build-output folder (`node_modules`, `target`...) and everything in it.
    RemoveTree { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub level: u8,
    pub what: String,
    pub action: Action,
    /// Estimated bytes freed.
    pub frees: u64,
}

/// Logs that may be legally or forensically required, or are sparse files
/// whose apparent size is not real disk use.
const PROTECTED: &[&str] = &[
    "auth.log", "secure", "wtmp", "btmp", "lastlog", "faillog", "audit",
];

/// Binary logs that are never truncated or deleted by path: journald's
/// database (shrink it with `journalctl --vacuum-*` only) and sysstat data.
pub fn is_binary_log(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.contains("/journal/")
        || p.ends_with(".journal")
        || p.ends_with(".journal~")
        || p.contains("/sysstat/")
}

pub fn is_protected(rel_to_log: &Path) -> bool {
    rel_to_log.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        PROTECTED
            .iter()
            .any(|p| s == *p || s.starts_with(&format!("{p}.")) || s.starts_with(&format!("{p}-")))
    })
}

/// "syslog.2.gz" -> Some("syslog"); "app.log.1" -> Some("app.log");
/// "dpkg.log-20261120.gz" -> Some("dpkg.log"); "syslog" -> None.
pub fn rotated_base(name: &str) -> Option<String> {
    let mut n = name.to_string();
    for ext in [".gz", ".xz", ".zst", ".bz2"] {
        if let Some(s) = n.strip_suffix(ext) {
            n = s.to_string();
            break;
        }
    }
    let compressed = n.len() != name.len();
    if let Some(s) = n.strip_suffix(".old") {
        return Some(s.to_string());
    }
    if let Some((base, num)) = n.rsplit_once('.') {
        if !num.is_empty()
            && num.len() <= 3
            && num.chars().all(|c| c.is_ascii_digit())
            && !base.is_empty()
        {
            return Some(base.to_string());
        }
    }
    if let Some((base, date)) = n.rsplit_once('-') {
        if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) && !base.is_empty() {
            return Some(base.to_string());
        }
    }
    // A plain compressed log ("foo.log.gz") is an archive too.
    compressed.then_some(n)
}

/// Every path any process currently has open (inode identity).
fn open_files() -> HashSet<(u64, u64)> {
    let mut out = HashSet::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return out;
    };
    for p in procs.flatten() {
        if !p
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(m) = std::fs::metadata(fd.path()) {
                if m.is_file() {
                    out.insert((m.dev(), m.ino()));
                }
            }
        }
    }
    out
}

/// Regular files under `dir` on the same filesystem, without following symlinks.
fn files_under(dir: &Path, max_depth: usize) -> Vec<(PathBuf, std::fs::Metadata)> {
    let mut out = Vec::new();
    let Ok(root) = std::fs::symlink_metadata(dir) else {
        return out;
    };
    let dev = root.dev();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.dev() != dev {
                continue;
            }
            if m.is_dir() && depth < max_depth {
                stack.push((e.path(), depth + 1));
            } else if m.is_file() {
                out.push((e.path(), m));
            }
        }
    }
    out
}

fn has_program(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

fn dir_bytes(dir: &Path) -> u64 {
    files_under(dir, 8)
        .iter()
        .map(|(_, m)| m.blocks() * 512)
        .sum()
}

/// logrotate config path covering `log`, if any (simple glob match on the
/// path lines of /etc/logrotate.conf and /etc/logrotate.d/*).
fn logrotate_config_for(root: &Path, log: &Path) -> Option<PathBuf> {
    let mut files = vec![root.join("etc/logrotate.conf")];
    if let Ok(rd) = std::fs::read_dir(root.join("etc/logrotate.d")) {
        files.extend(rd.flatten().map(|e| e.path()));
    }
    let target = Path::new("/").join(log.strip_prefix(root).unwrap_or(log));
    let target = target.to_string_lossy();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with('/') {
                continue;
            }
            for pat in line.trim_end_matches('{').split_whitespace() {
                if glob_match(pat.trim_matches('"'), &target) {
                    return Some(f);
                }
            }
        }
    }
    None
}

/// `*` matches any run of characters except '/'; everything else is literal.
pub fn glob_match(pat: &str, s: &str) -> bool {
    fn rec(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| rec(&p[1..], &s[i..])),
            Some(&c) => s.first() == Some(&c) && rec(&p[1..], &s[1..]),
        }
    }
    rec(pat.as_bytes(), s.as_bytes())
}

pub fn journal_usage() -> Option<u64> {
    let out = Command::new("journalctl")
        .arg("--disk-usage")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "Archived and active journals take up 800.4M in the file system."
    let word = text.split_whitespace().find(|w| {
        w.ends_with(['K', 'M', 'G', 'T', 'B'])
            && w.chars().next().is_some_and(|c| c.is_ascii_digit())
    })?;
    crate::sentinel::parse_bytes(word.trim_end_matches('B')).ok()
}

/// Build the plan for a level. Nothing is changed.
pub fn plan(o: &Options) -> Vec<Item> {
    let mut items = Vec::new();
    if o.level == 0 {
        return items;
    }
    let open = open_files();

    // Level 1: journal.
    if o.root == Path::new("/") && has_program("journalctl") {
        if let Some(used) = journal_usage() {
            if used > o.journal_max {
                items.push(Item {
                    level: 1,
                    what: format!(
                        "systemd journal is {}, budget {}",
                        crate::util::human(used as i64),
                        crate::util::human(o.journal_max as i64)
                    ),
                    action: Action::Run {
                        program: "journalctl".into(),
                        args: vec![
                            format!("--vacuum-size={}", o.journal_max),
                            format!("--vacuum-time={}", o.journal_max_age),
                        ],
                    },
                    frees: used - o.journal_max,
                });
            }
        }
    }
    if o.level < 2 {
        return items;
    }

    // Level 2a: rotated logs, newest `rotated_keep` of each kept.
    let log_dir = o.root.join("var/log");
    let mut groups: HashMap<(PathBuf, String), Vec<(PathBuf, std::fs::Metadata)>> = HashMap::new();
    for (p, m) in files_under(&log_dir, 6) {
        let rel = p.strip_prefix(&log_dir).unwrap_or(&p).to_path_buf();
        if is_protected(&rel) || is_binary_log(&p) || open.contains(&(m.dev(), m.ino())) {
            continue;
        }
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(base) = rotated_base(&name) {
            groups
                .entry((p.parent().unwrap_or(&log_dir).to_path_buf(), base))
                .or_default()
                .push((p, m));
        }
    }
    let mut rotated: Vec<Item> = Vec::new();
    for ((_, base), mut files) in groups {
        files.sort_by_key(|(_, m)| std::cmp::Reverse(m.mtime()));
        for (p, m) in files.into_iter().skip(o.rotated_keep) {
            rotated.push(Item {
                level: 2,
                what: format!("rotated log of {base}"),
                action: Action::Remove { path: p },
                frees: m.blocks() * 512,
            });
        }
    }
    rotated.sort_by_key(|i| std::cmp::Reverse(i.frees));
    items.extend(rotated);

    // Level 2b: big active logs, only when the disk is getting full.
    if o.used_pct >= o.logs_min_used_pct {
        let mut candidates: Vec<(PathBuf, std::fs::Metadata, bool)> = files_under(&log_dir, 6)
            .into_iter()
            .filter(|(p, _)| {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                rotated_base(&name).is_none()
                    && !is_protected(p.strip_prefix(&log_dir).unwrap_or(p))
                    && !is_binary_log(p)
            })
            .map(|(p, m)| (p, m, false))
            .collect();
        let docker = o.root.join("var/lib/docker/containers");
        candidates.extend(
            files_under(&docker, 1)
                .into_iter()
                .filter(|(p, _)| p.to_string_lossy().ends_with("-json.log"))
                .map(|(p, m)| (p, m, true)),
        );
        for (p, m, is_docker) in candidates {
            let bytes = m.blocks() * 512;
            if bytes < o.big_log {
                continue;
            }
            let conf = (!is_docker)
                .then(|| logrotate_config_for(&o.root, &p))
                .flatten();
            let item = match conf {
                Some(c) => Item {
                    level: 2,
                    what: format!(
                        "big log {} ({}), rotate with its logrotate config",
                        p.display(),
                        crate::util::human(bytes as i64)
                    ),
                    action: Action::Run {
                        program: "logrotate".into(),
                        args: vec!["-f".into(), c.to_string_lossy().into_owned()],
                    },
                    frees: bytes.saturating_sub(o.tail_keep),
                },
                None => Item {
                    level: 2,
                    what: format!(
                        "big {} {} ({}), keep last {} then truncate",
                        if is_docker { "container log" } else { "log" },
                        p.display(),
                        crate::util::human(bytes as i64),
                        crate::util::human(o.tail_keep as i64)
                    ),
                    action: Action::TailAndTruncate {
                        path: p,
                        keep: o.tail_keep,
                    },
                    frees: bytes.saturating_sub(o.tail_keep),
                },
            };
            items.push(item);
        }
    }
    if o.level < 3 {
        return items;
    }

    // Level 3: data that rebuilds itself.
    let prod = o.root == Path::new("/");
    let apt = o.root.join("var/cache/apt/archives");
    let apt_bytes = dir_bytes(&apt);
    if prod && apt_bytes >= 16 << 20 && has_program("apt-get") {
        items.push(Item {
            level: 3,
            what: "apt package cache".into(),
            action: Action::Run {
                program: "apt-get".into(),
                args: vec!["clean".into()],
            },
            frees: apt_bytes,
        });
    }
    let dnf = o.root.join("var/cache/dnf");
    let dnf_bytes = dir_bytes(&dnf);
    if prod && dnf_bytes >= 16 << 20 && has_program("dnf") {
        items.push(Item {
            level: 3,
            what: "dnf package cache".into(),
            action: Action::Run {
                program: "dnf".into(),
                args: vec!["clean".into(), "packages".into()],
            },
            frees: dnf_bytes,
        });
    }
    if prod && has_program("systemd-tmpfiles") {
        items.push(Item {
            level: 3,
            what: "old temporary files (systemd-tmpfiles age rules)".into(),
            action: Action::Run {
                program: "systemd-tmpfiles".into(),
                args: vec!["--clean".into()],
            },
            frees: 0,
        });
    }
    let now = crate::util::now_secs();
    for d in ["var/crash", "var/lib/systemd/coredump"] {
        let mut dumps: Vec<(PathBuf, std::fs::Metadata)> = files_under(&o.root.join(d), 1)
            .into_iter()
            .filter(|(_, m)| !open.contains(&(m.dev(), m.ino())))
            .collect();
        dumps.sort_by_key(|(_, m)| std::cmp::Reverse(m.mtime()));
        for (p, m) in dumps.into_iter().skip(o.core_keep) {
            if (now - m.mtime()) as u64 >= o.core_min_age_days * 86_400 {
                items.push(Item {
                    level: 3,
                    what: "old crash or core dump".into(),
                    action: Action::Remove { path: p },
                    frees: m.blocks() * 512,
                });
            }
        }
    }
    if prod && has_program("docker") {
        items.push(Item {
            level: 3,
            what: "Docker build cache older than 7 days".into(),
            action: Action::Run {
                program: "docker".into(),
                args: vec![
                    "builder".into(),
                    "prune".into(),
                    "-f".into(),
                    "--filter".into(),
                    "until=168h".into(),
                ],
            },
            frees: 0,
        });
    }
    items
}

fn audit(entry: serde_json::Value) {
    if let Some(dir) = Path::new(AUDIT_LOG).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(AUDIT_LOG)
    {
        use std::io::Write;
        let _ = writeln!(f, "{entry}");
    }
}

/// Apply one action. Re-checks safety at run time, not only when planning.
pub fn apply(item: &Item) -> Result<()> {
    anyhow::ensure!(
        !Path::new(KILL_SWITCH).exists(),
        "actions disabled by {KILL_SWITCH}"
    );
    let result = match &item.action {
        Action::Run { program, args } => {
            let out = Command::new(program)
                .args(args)
                .output()
                .with_context(|| format!("running {program}"))?;
            anyhow::ensure!(
                out.status.success(),
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(())
        }
        Action::Remove { path } => {
            let m = std::fs::symlink_metadata(path)?;
            anyhow::ensure!(
                m.is_file(),
                "{} is no longer a regular file",
                path.display()
            );
            anyhow::ensure!(
                !open_files().contains(&(m.dev(), m.ino())),
                "{} is open; skipped",
                path.display()
            );
            std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
        }
        Action::TailAndTruncate { path, keep } => {
            anyhow::ensure!(
                !is_binary_log(path),
                "{} is a binary log; use journalctl --vacuum-* instead",
                path.display()
            );
            tail_and_truncate(path, *keep)
        }
        Action::RemoveTree { path } => remove_tree(path),
    };
    audit(serde_json::json!({
        "t": crate::util::now_secs(),
        "level": item.level,
        "what": item.what,
        "action": format!("{:?}", item.action),
        "frees": item.frees,
        "ok": result.is_ok(),
        "error": result.as_ref().err().map(|e| format!("{e:#}")),
    }));
    result
}

/// Remove a build-output folder. Refuses anything that is not a real folder
/// with a known build-output name, or that has a filesystem mounted inside it
/// (removing it would delete files on the other filesystem).
pub fn remove_tree(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "{} is not a real folder",
        path.display()
    );
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    anyhow::ensure!(
        crate::insight::ARTIFACTS.contains(&name.as_ref()),
        "{} is not a known build-output folder",
        path.display()
    );
    let p = path.to_string_lossy().into_owned();
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let inside = mounts
        .lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(crate::sentinel::unescape_mount)
        .any(|mp| mp == p || mp.starts_with(&format!("{p}/")));
    anyhow::ensure!(
        !inside,
        "{} has a filesystem mounted inside it",
        path.display()
    );
    std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
}

/// Keep the last `keep` bytes in `<path>.dux-tail` and truncate the log in
/// place, so the writer keeps its file handle and nothing breaks.
pub fn tail_and_truncate(path: &Path, keep: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let m = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(m.is_file(), "{} is not a regular file", path.display());
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(keep);
    f.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut tail)?;
    let tail_path = PathBuf::from(format!("{}.dux-tail", path.display()));
    let mut t = std::fs::File::create(&tail_path)?;
    t.write_all(&tail)?;
    t.sync_all()?;
    f.set_len(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dux-cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path, bytes: usize, age_days: i64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![b'x'; bytes]).unwrap();
        let t = crate::util::now_secs() - age_days * 86_400;
        let ts = [libc::timespec {
            tv_sec: t,
            tv_nsec: 0,
        }; 2];
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        // SAFETY: valid path and timespec array.
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), 0) };
    }

    #[test]
    fn remove_tree_only_takes_build_output_folders() {
        let r = tmp("rmtree");
        std::fs::create_dir_all(r.join("proj/node_modules/a/b")).unwrap();
        std::fs::write(r.join("proj/node_modules/a/b/f.js"), b"x").unwrap();
        std::fs::create_dir_all(r.join("proj/src")).unwrap();
        std::os::unix::fs::symlink(r.join("proj/src"), r.join("proj/dist")).unwrap();
        assert!(
            remove_tree(&r.join("proj/src")).is_err(),
            "not a build-output name"
        );
        assert!(
            remove_tree(&r.join("proj/dist")).is_err(),
            "symlinks are refused"
        );
        remove_tree(&r.join("proj/node_modules")).unwrap();
        assert!(!r.join("proj/node_modules").exists());
        assert!(r.join("proj/src").exists());
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn rotated_names() {
        assert_eq!(rotated_base("syslog.1").as_deref(), Some("syslog"));
        assert_eq!(rotated_base("syslog.2.gz").as_deref(), Some("syslog"));
        assert_eq!(
            rotated_base("dpkg.log-20261120.gz").as_deref(),
            Some("dpkg.log")
        );
        assert_eq!(rotated_base("app.log.old").as_deref(), Some("app.log"));
        assert_eq!(rotated_base("error.log.gz").as_deref(), Some("error.log"));
        assert_eq!(rotated_base("syslog"), None);
        assert_eq!(rotated_base("kern.log"), None);
        assert_eq!(rotated_base("v1.2-beta"), None);
    }

    #[test]
    fn protected_logs_are_recognised() {
        assert!(is_protected(Path::new("auth.log.1")));
        assert!(is_protected(Path::new("audit/audit.log.3")));
        assert!(is_protected(Path::new("btmp.1")));
        assert!(!is_protected(Path::new("syslog.1")));
        assert!(!is_protected(Path::new("nginx/access.log.2.gz")));
    }

    #[test]
    fn binary_logs_are_never_truncated() {
        let r = tmp("binary");
        std::fs::create_dir_all(r.join("var/log/journal/abc")).unwrap();
        std::fs::write(
            r.join("var/log/journal/abc/system.journal"),
            vec![0u8; 3 << 20],
        )
        .unwrap();
        std::fs::write(
            r.join("var/log/journal/abc/system@x.journal~"),
            vec![0u8; 3 << 20],
        )
        .unwrap();
        let o = Options {
            level: 2,
            root: r.clone(),
            big_log: 1 << 20,
            used_pct: 95.0,
            ..Options::default()
        };
        assert!(plan(&o).is_empty(), "journal files are never planned");
        let item = Item {
            level: 2,
            what: "x".into(),
            action: Action::TailAndTruncate {
                path: r.join("var/log/journal/abc/system.journal"),
                keep: 1,
            },
            frees: 1,
        };
        assert!(apply(&item).is_err(), "apply refuses too");
        assert_eq!(
            std::fs::metadata(r.join("var/log/journal/abc/system.journal"))
                .unwrap()
                .len(),
            3 << 20
        );
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn globbing() {
        assert!(glob_match(
            "/var/log/nginx/*.log",
            "/var/log/nginx/access.log"
        ));
        assert!(!glob_match(
            "/var/log/nginx/*.log",
            "/var/log/nginx/sub/access.log"
        ));
        assert!(glob_match("/var/log/syslog", "/var/log/syslog"));
        assert!(!glob_match("/var/log/sys", "/var/log/syslog"));
    }

    #[test]
    fn level_two_plan_keeps_newest_three_and_never_touches_protected() {
        let r = tmp("plan");
        for i in 1..=6 {
            put(
                &r.join(format!("var/log/syslog.{i}.gz")),
                1000 * i,
                i as i64,
            );
        }
        put(&r.join("var/log/auth.log.1"), 5000, 10);
        put(&r.join("var/log/auth.log.2.gz"), 5000, 11);
        put(&r.join("var/log/syslog"), 2000, 0);
        put(&r.join("var/log/nginx/access.log.1"), 3000, 2);
        let o = Options {
            level: 2,
            root: r.clone(),
            ..Options::default()
        };
        let items = plan(&o);
        let removed: Vec<String> = items
            .iter()
            .filter_map(|i| match &i.action {
                Action::Remove { path } => {
                    Some(path.file_name().unwrap().to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect();
        let mut sorted = removed.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec!["syslog.4.gz", "syslog.5.gz", "syslog.6.gz"],
            "oldest three of six go: {removed:?}"
        );
        assert!(!removed.iter().any(|n| n.starts_with("auth")));
        assert!(
            !removed.contains(&"access.log.1".to_string()),
            "only one rotated file of access.log: kept"
        );
        assert!(
            !items
                .iter()
                .any(|i| matches!(i.action, Action::TailAndTruncate { .. })),
            "disk not full: active logs left alone"
        );
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn big_active_logs_only_when_disk_is_full_and_logrotate_wins() {
        let r = tmp("big");
        put(&r.join("var/log/app/app.log"), 3 << 20, 0);
        put(&r.join("var/log/nginx/access.log"), 3 << 20, 0);
        put(&r.join("var/log/small.log"), 1000, 0);
        put(&r.join("var/log/auth.log"), 3 << 20, 0);
        std::fs::create_dir_all(r.join("etc/logrotate.d")).unwrap();
        std::fs::write(
            r.join("etc/logrotate.d/nginx"),
            "/var/log/nginx/*.log {\n daily\n}\n",
        )
        .unwrap();
        let o = Options {
            level: 2,
            root: r.clone(),
            big_log: 1 << 20,
            tail_keep: 1 << 20,
            used_pct: 92.0,
            ..Options::default()
        };
        let items = plan(&o);
        let mut kinds: Vec<String> = items
            .iter()
            .map(|i| match &i.action {
                Action::Run { program, .. } => program.clone(),
                Action::TailAndTruncate { path, .. } => {
                    format!("tail:{}", path.file_name().unwrap().to_string_lossy())
                }
                Action::Remove { path } => format!("rm:{}", path.display()),
                Action::RemoveTree { path } => format!("rmtree:{}", path.display()),
            })
            .collect();
        kinds.sort();
        assert_eq!(kinds, vec!["logrotate", "tail:app.log"], "{kinds:?}");
        let o2 = Options {
            used_pct: 50.0,
            ..o
        };
        assert!(
            plan(&o2).is_empty(),
            "below 80% full nothing active is touched"
        );
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn tail_and_truncate_keeps_the_end_and_the_handle() {
        let r = tmp("tail");
        let p = r.join("app.log");
        let body: Vec<u8> = (0..100_000u32)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        std::fs::write(&p, &body).unwrap();
        let mut writer = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        tail_and_truncate(&p, 1000).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0);
        let tail = std::fs::read(r.join("app.log.dux-tail")).unwrap();
        assert_eq!(tail.len(), 1000);
        assert!(tail.ends_with(b"line 99999\n"));
        use std::io::Write;
        writer.write_all(b"after\n").unwrap();
        assert_eq!(
            std::fs::read(&p).unwrap(),
            b"after\n",
            "writer keeps working after truncate"
        );
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn level_three_removes_only_old_dumps_beyond_the_newest_five() {
        let r = tmp("core");
        for i in 0..8 {
            put(&r.join(format!("var/crash/core.{i}")), 10_000, 20 + i);
        }
        put(&r.join("var/crash/core.recent"), 10_000, 1);
        let o = Options {
            level: 3,
            root: r.clone(),
            ..Options::default()
        };
        let n = plan(&o)
            .iter()
            .filter(|i| i.what.contains("core dump"))
            .count();
        assert_eq!(
            n, 4,
            "9 dumps, newest 5 kept, the other 4 are older than 14 days"
        );
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn open_files_are_never_planned_for_removal() {
        let r = tmp("open");
        for i in 1..=5 {
            put(&r.join(format!("var/log/x.log.{i}")), 1000, i as i64);
        }
        let _held = std::fs::File::open(r.join("var/log/x.log.5")).unwrap();
        let o = Options {
            level: 2,
            root: r.clone(),
            ..Options::default()
        };
        let removed: Vec<PathBuf> = plan(&o)
            .into_iter()
            .filter_map(|i| {
                if let Action::Remove { path } = i.action {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            removed,
            vec![r.join("var/log/x.log.4")],
            "x.log.5 is open, so only x.log.4 goes"
        );
        std::fs::remove_dir_all(&r).unwrap();
    }
}
