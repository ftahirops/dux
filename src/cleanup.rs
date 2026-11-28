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
/// Also database, mail and kernel logs (spec A7.3).
const PROTECTED: &[&str] = &[
    "auth.log",
    "secure",
    "wtmp",
    "btmp",
    "lastlog",
    "faillog",
    "audit",
    "mysql",
    "mariadb",
    "postgresql",
    "mongodb",
    "redis",
    "mail",
    "mail.log",
    "maillog",
    "kern.log",
    "kern",
    "dmesg",
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
///
/// `logrotate -f CONFIG` rotates every log the config lists, so a config
/// that also lists a protected log (auth.log, secure, audit...) is never
/// returned: the caller then falls back to keeping the tail of this one log.
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
        let patterns: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('/'))
            .flat_map(|l| l.trim_end_matches('{').split_whitespace())
            .map(|p| p.trim_matches('"'))
            .collect();
        if patterns.iter().any(|p| glob_match(p, &target)) {
            // Forced only when every path the config lists is under
            // /var/log, not protected (any part of it) and not near a
            // never-touch path; otherwise the log is truncated on its own.
            // Patterns name real paths (/var/log/...), whatever `root` is.
            let safe = patterns.iter().all(|p| {
                let p = Path::new(p);
                let Ok(rel) = p.strip_prefix("/var/log") else {
                    return false;
                };
                let literal: PathBuf = p
                    .components()
                    .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
                    .collect();
                !is_protected(rel) && !crate::protect::is_protected(&literal)
            });
            return safe.then_some(f);
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

    items.extend(log_items(
        o,
        &o.root.join("var/log"),
        Some(&o.root.join("var/lib/docker/containers")),
        &open,
    ));
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
            // A future mtime (clock skew, restored backup) is age 0, not "very old".
            let age = u64::try_from(now - m.mtime()).unwrap_or(0);
            if age >= o.core_min_age_days * 86_400 {
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

pub fn audit(entry: serde_json::Value) {
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

/// The never-touch check for one action, on every path it writes. Fails
/// closed when the list cannot be read.
/// Rotated copies to remove and big active logs to truncate under `log_dir`
/// (and container logs in `docker`): the level-2 rules, for /var/log or any
/// folder approved for automatic cleanup.
pub fn log_items(
    o: &Options,
    log_dir: &Path,
    docker: Option<&Path>,
    open: &HashSet<(u64, u64)>,
) -> Vec<Item> {
    let mut items = Vec::new();
    // Everything under /var/log is a log; elsewhere only files that look
    // like one (a big database file or backup.tar.1 is never touched).
    let all_logs = log_dir == o.root.join("var/log");
    // Level 2a: rotated logs, newest `rotated_keep` of each kept.
    let mut groups: HashMap<(PathBuf, String), Vec<(PathBuf, std::fs::Metadata)>> = HashMap::new();
    for (p, m) in files_under(log_dir, 6) {
        let rel = p.strip_prefix(log_dir).unwrap_or(&p).to_path_buf();
        if is_protected(&rel) || is_binary_log(&p) || open.contains(&(m.dev(), m.ino())) {
            continue;
        }
        if !all_logs && !looks_like_log(&p) {
            continue;
        }
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(base) = rotated_base(&name) {
            groups
                .entry((p.parent().unwrap_or(log_dir).to_path_buf(), base))
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
        let mut candidates: Vec<(PathBuf, std::fs::Metadata, bool)> = files_under(log_dir, 6)
            .into_iter()
            .filter(|(p, _)| {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                rotated_base(&name).is_none()
                    && !is_protected(p.strip_prefix(log_dir).unwrap_or(p))
                    && !is_binary_log(p)
                    && (all_logs || looks_like_log(p))
            })
            .map(|(p, m)| (p, m, false))
            .collect();
        if let Some(docker) = docker {
            candidates.extend(
                files_under(docker, 1)
                    .into_iter()
                    .filter(|(p, _)| p.to_string_lossy().ends_with("-json.log"))
                    .map(|(p, m)| (p, m, true)),
            );
        }
        let mut forced: HashSet<PathBuf> = HashSet::new();
        for (p, m, is_docker) in candidates {
            let bytes = m.blocks() * 512;
            if bytes < o.big_log {
                continue;
            }
            let conf = (!is_docker)
                .then(|| logrotate_config_for(&o.root, &p))
                .flatten();
            // One forced rotation per config, however many of its logs are big.
            if let Some(c) = &conf {
                if !forced.insert(c.clone()) {
                    continue;
                }
            }
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
    items
}

fn looks_like_log(p: &Path) -> bool {
    let dir = p.parent().unwrap_or(Path::new("")).to_string_lossy();
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    crate::insight::is_log(dir.as_bytes(), name.as_bytes())
}

/// Files some process has open (dev, inode).
pub fn open_now() -> HashSet<(u64, u64)> {
    open_files()
}

/// The paths an action changes, for never-touch and approved-folder checks.
pub fn targets(item: &Item) -> Option<Vec<PathBuf>> {
    Some(match &item.action {
        Action::Remove { path } | Action::RemoveTree { path } => {
            vec![path.clone()]
        }
        Action::TailAndTruncate { path, .. } => vec![path.clone(), tail_path(path)],
        Action::Run { program, args } => crate::protect::command_scope(program, args)?,
    })
}

pub fn allowed(item: &Item) -> Result<()> {
    let targets: Vec<PathBuf> = match &item.action {
        Action::Remove { path } | Action::RemoveTree { path } => {
            vec![path.clone()]
        }
        Action::TailAndTruncate { path, .. } => vec![path.clone(), tail_path(path)],
        Action::Run { program, args } => match crate::protect::command_scope(program, args) {
            Some(dirs) => dirs,
            None => {
                // A command whose reach dux does not know may only run when
                // nothing is protected.
                crate::refuse_unless!(
                    crate::protect::policy_from(Path::new(crate::protect::FILE))?.is_empty(),
                    "{program} has no known scope and a never-touch list is in effect; skipped"
                );
                Vec::new()
            }
        },
    };
    for p in &targets {
        crate::protect::check(p)?;
    }
    Ok(())
}

/// Apply one action. Re-checks safety at run time, not only when planning.
pub fn apply(item: &Item) -> Result<()> {
    allowed(item)?;
    crate::refuse_unless!(
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
        Action::Remove { path } => remove_file_beneath(path),
        Action::TailAndTruncate { path, keep } => {
            crate::refuse_unless!(
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

/// Paths a user may never delete from the browser: the system and the
/// top-level folders themselves.
pub fn system_path(p: &str) -> bool {
    const UNDER: &[&str] = &[
        "/proc",
        "/sys",
        "/dev",
        "/boot",
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib32",
        "/lib64",
        "/run",
        "/snap",
        "/var/lib/dpkg",
        "/var/lib/apt",
        "/var/lib/rpm",
        "/var/lib/systemd",
        // Container runtimes and databases: clean these with their own tools.
        "/var/lib/docker",
        "/var/lib/containerd",
        "/var/lib/containers",
        "/var/lib/kubelet",
        "/var/lib/etcd",
        "/var/lib/mysql",
        "/var/lib/postgresql",
        "/var/lib/mongodb",
        "/var/lib/redis",
        "/var/lib/clickhouse",
        "/var/lib/elasticsearch",
    ];
    let p = p.trim_end_matches('/');
    p.is_empty()
        || p.matches('/').count() <= 1
        || p == "/var/lib"
        || p == "/var/log"
        || p == "/home"
        || UNDER
            .iter()
            .any(|u| p == *u || p.starts_with(&format!("{u}/")))
}

/// The real path of `path`: its parent canonicalized, plus the final name.
fn real_location(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .with_context(|| format!("{} has no final name", path.display()))?;
    anyhow::ensure!(
        name != ".." && name != ".",
        "{} must name an entry, not . or ..",
        path.display()
    );
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("/"),
    };
    let dir =
        std::fs::canonicalize(parent).with_context(|| format!("resolving {}", parent.display()))?;
    Ok(dir.join(name))
}

/// Is a filesystem mounted at or under `p`? Errors when the mount table
/// cannot be read: without it, dux cannot know, so it must not delete.
fn has_mount_inside(p: &str) -> Result<bool> {
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .context("cannot read /proc/self/mountinfo; refusing a recursive delete")?;
    Ok(mounts
        .lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(crate::sentinel::unescape_mount)
        .any(|mp| mp == p || mp.starts_with(&format!("{p}/"))))
}

/// Where tail_and_truncate keeps the end of a log.
fn tail_path(log: &Path) -> PathBuf {
    let mut s = log.as_os_str().to_owned();
    s.push(".dux-tail");
    PathBuf::from(s)
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
    let parent = path.parent().unwrap_or(Path::new("/"));
    anyhow::ensure!(
        crate::insight::ARTIFACTS.contains(&name.as_ref())
            || crate::insight::conditional_artifact(
                &name,
                crate::insight::markers_on_disk(parent),
                &parent.to_string_lossy()
            ),
        "{} is not a known build-output folder",
        path.display()
    );
    // The path must be its own real location: a folder on the way swapped
    // for a symlink would point the checks below at the wrong place. The
    // delete then walks the same path with O_NOFOLLOW, so a swap after this
    // check fails instead of following the link.
    let real = real_location(path)?;
    let lexical = std::path::absolute(path)?;
    crate::refuse_unless!(
        real == lexical,
        "{} is reached through a symlink ({}); skipped",
        path.display(),
        real.display()
    );
    let p = lexical.to_string_lossy().into_owned();
    crate::refuse_unless!(
        !system_path(&p),
        "{p} is a system or top-level path; dux will not delete it"
    );
    crate::refuse_unless!(
        !has_mount_inside(&p)?,
        "{} has a filesystem mounted inside it",
        path.display()
    );
    remove_dir_same_fs(&lexical).with_context(|| format!("removing {p}"))
}

/// Delete a folder tree without ever leaving its filesystem or following a
/// symlink: every step goes through directory handles (openat/unlinkat with
/// O_NOFOLLOW), so a folder swapped for a symlink, or a filesystem mounted
/// inside while the delete runs, stops the delete instead of reaching
/// another disk. Stops at the first such entry and reports it.
pub fn remove_dir_same_fs(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("/"),
    };
    let name = path.file_name().context("no folder name")?;
    // The parent is reached without following any symlink on the way.
    let pfd = std::os::fd::IntoRawFd::into_raw_fd(open_dir_nofollow(parent)?);
    anyhow::ensure!(
        pfd >= 0,
        "opening {}: {}",
        parent.display(),
        std::io::Error::last_os_error()
    );
    let cname = CString::new(name.as_bytes())?;
    // The mount the tree starts on, taken from the folder itself.
    // SAFETY: pfd is open; cname is valid.
    let fd = unsafe {
        libc::openat(
            pfd,
            cname.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        // SAFETY: closing our own fd.
        unsafe { libc::close(pfd) };
        anyhow::bail!("{} is not a real folder: {e}", path.display());
    }
    let mount = mount_of(fd);
    // SAFETY: closing our own fd.
    unsafe { libc::close(fd) };
    let Some(mount) = mount else {
        // SAFETY: closing our own fd.
        unsafe { libc::close(pfd) };
        anyhow::bail!("cannot identify the filesystem of {}", path.display());
    };
    let r = rm_at(pfd, &cname, mount, path, 0);
    // SAFETY: closing our own fd.
    unsafe { libc::close(pfd) };
    r
}

/// Which mount an open directory is on: (device, mount id). The mount id
/// (statx STATX_MNT_ID) also tells apart a bind mount from the same disk;
/// kernels without it give 0 and only the device is compared.
fn mount_of(fd: libc::c_int) -> Option<(u64, u64)> {
    crate::util::fd_mount(fd)
}

fn rm_at(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    mount: (u64, u64),
    shown: &Path,
    depth: usize,
) -> Result<()> {
    use std::ffi::CStr;
    anyhow::ensure!(
        depth < 4096,
        "{} is nested too deeply to delete safely",
        shown.display()
    );
    // SAFETY: parent is an open directory fd; name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return match e.raw_os_error() {
            // Not a directory (a file, or a symlink, which is removed as a link).
            Some(libc::ENOTDIR) | Some(libc::ELOOP) => {
                // SAFETY: unlinking a name inside our open parent.
                let rc = unsafe { libc::unlinkat(parent, name.as_ptr(), 0) };
                anyhow::ensure!(
                    rc == 0,
                    "removing {}: {}",
                    shown.display(),
                    std::io::Error::last_os_error()
                );
                Ok(())
            }
            _ => Err(anyhow::anyhow!("opening {}: {e}", shown.display())),
        };
    }
    if mount_of(fd) != Some(mount) {
        // SAFETY: closing our own fd.
        unsafe { libc::close(fd) };
        anyhow::bail!(
            "{} is on another filesystem (mounted during the delete); stopped there",
            shown.display()
        );
    }
    // List the entries first, then delete them, through this fd only.
    let mut names: Vec<std::ffi::CString> = Vec::new();
    {
        // SAFETY: dup of our fd; fdopendir takes ownership of the copy.
        let d = unsafe { libc::fdopendir(libc::dup(fd)) };
        if d.is_null() {
            // SAFETY: closing our own fd.
            unsafe { libc::close(fd) };
            anyhow::bail!(
                "listing {}: {}",
                shown.display(),
                std::io::Error::last_os_error()
            );
        }
        loop {
            // SAFETY: d is a valid DIR*.
            let e = unsafe { libc::readdir(d) };
            if e.is_null() {
                break;
            }
            // SAFETY: d_name is NUL-terminated within the dirent.
            let n = unsafe { CStr::from_ptr((*e).d_name.as_ptr()) };
            if n.to_bytes() != b"." && n.to_bytes() != b".." {
                names.push(n.to_owned());
            }
        }
        // SAFETY: closing the DIR* we opened.
        unsafe { libc::closedir(d) };
    }
    let mut result = Ok(());
    for n in &names {
        let child = shown.join(std::ffi::OsStr::new(
            std::str::from_utf8(n.to_bytes()).unwrap_or("?"),
        ));
        if let Err(e) = rm_at(fd, n, mount, &child, depth + 1) {
            result = Err(e);
            break;
        }
    }
    // SAFETY: closing our own fd.
    unsafe { libc::close(fd) };
    result?;
    // SAFETY: removing the now-empty directory inside our open parent.
    let rc = unsafe { libc::unlinkat(parent, name.as_ptr(), libc::AT_REMOVEDIR) };
    anyhow::ensure!(
        rc == 0,
        "removing {}: {}",
        shown.display(),
        std::io::Error::last_os_error()
    );
    Ok(())
}

/// Open the directory `dir` without following a symlink anywhere on the
/// way: every component is opened with O_NOFOLLOW from `/`, so no part of
/// the path can be swapped for a link to somewhere else while dux works.
pub fn open_dir_nofollow(dir: &Path) -> Result<std::os::fd::OwnedFd> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    anyhow::ensure!(
        dir.is_absolute(),
        "{} is not an absolute path",
        dir.display()
    );
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: a valid NUL-terminated path; the fd is owned below.
    let fd = unsafe { libc::open(c"/".as_ptr(), flags) };
    anyhow::ensure!(fd >= 0, "opening /: {}", std::io::Error::last_os_error());
    // SAFETY: fd was just opened and is owned by nothing else.
    let mut cur = unsafe { OwnedFd::from_raw_fd(fd) };
    for c in dir.components() {
        match c {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(n) => {
                let cn = CString::new(n.as_bytes())?;
                // SAFETY: cur is an open directory; cn is NUL-terminated.
                let fd = unsafe {
                    libc::openat(std::os::fd::AsRawFd::as_raw_fd(&cur), cn.as_ptr(), flags)
                };
                anyhow::ensure!(
                    fd >= 0,
                    "{}: {} (a symlink on the way is refused; use the real path)",
                    dir.display(),
                    std::io::Error::last_os_error()
                );
                // SAFETY: as above.
                cur = unsafe { OwnedFd::from_raw_fd(fd) };
            }
            _ => anyhow::bail!("{} must not contain ..", dir.display()),
        }
    }
    Ok(cur)
}

fn fstatat_nofollow(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::CStr,
) -> std::io::Result<libc::stat> {
    use std::os::fd::AsRawFd;
    // SAFETY: stat is plain data; dir is open; name is NUL-terminated.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if r == 0 {
        Ok(st)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn split_parent(path: &Path) -> Result<(PathBuf, std::ffi::CString)> {
    use std::os::unix::ffi::OsStrExt;
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/"));
    Ok((
        parent.to_path_buf(),
        std::ffi::CString::new(name.as_bytes())?,
    ))
}

/// Remove one regular file that no process has open, through its parent
/// directory handle (never through a symlink).
pub fn remove_file_beneath(path: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let (parent, name) = split_parent(path)?;
    let dir = open_dir_nofollow(&parent)?;
    let st = fstatat_nofollow(&dir, &name).with_context(|| format!("{}", path.display()))?;
    anyhow::ensure!(
        st.st_mode & libc::S_IFMT == libc::S_IFREG,
        "{} is no longer a regular file",
        path.display()
    );
    crate::refuse_unless!(
        !open_files().contains(&(st.st_dev, st.st_ino)),
        "{} is open; skipped",
        path.display()
    );
    // SAFETY: dir is open; name is NUL-terminated.
    let r = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
    anyhow::ensure!(
        r == 0,
        "removing {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    Ok(())
}

/// Keep the last `keep` bytes in `<path>.dux-tail` and truncate the log in
/// place, so the writer keeps its file handle and nothing breaks.
///
/// Everything goes through the parent directory's handle: the log is opened
/// with O_NOFOLLOW and must have one link (a hard link to another file is
/// refused); the tail is always a new file (O_EXCL), never one already there.
pub fn tail_and_truncate(path: &Path, keep: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    let (parent, name) = split_parent(path)?;
    let dir = open_dir_nofollow(&parent)?;
    // SAFETY: dir is open; name is NUL-terminated; the fd is owned below.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    anyhow::ensure!(
        fd >= 0,
        "opening {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    // SAFETY: fd was just opened and is owned by nothing else.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let m = f.metadata()?;
    anyhow::ensure!(m.is_file(), "{} is not a regular file", path.display());
    crate::refuse_unless!(
        m.nlink() == 1,
        "{} has {} hard links; it may be another file under this name, skipped",
        path.display(),
        m.nlink()
    );
    let len = m.len();
    let start = len.saturating_sub(keep);
    f.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut tail)?;
    // The copy gets the log's own owner and mode (logs are often 0640
    // root:adm). A file already at the tail name is replaced, never reused.
    let tail_path = tail_path(path);
    let (_, tname) = split_parent(&tail_path)?;
    if let Ok(old) = fstatat_nofollow(&dir, &tname) {
        anyhow::ensure!(
            old.st_mode & libc::S_IFMT != libc::S_IFDIR,
            "{} exists and is a folder",
            tail_path.display()
        );
        // SAFETY: dir is open; tname is NUL-terminated.
        let r = unsafe { libc::unlinkat(dir.as_raw_fd(), tname.as_ptr(), 0) };
        anyhow::ensure!(
            r == 0,
            "replacing {}: {}",
            tail_path.display(),
            std::io::Error::last_os_error()
        );
    }
    {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: as above; O_EXCL makes this a new file or an error.
        let tfd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                tname.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        anyhow::ensure!(
            tfd >= 0,
            "creating {}: {}",
            tail_path.display(),
            std::io::Error::last_os_error()
        );
        // SAFETY: tfd was just created and is owned by nothing else.
        let mut t = unsafe { std::fs::File::from_raw_fd(tfd) };
        // If the owner cannot be copied (not root), the copy stays private (0600).
        if std::os::unix::fs::fchown(&t, Some(m.uid()), Some(m.gid())).is_ok() {
            t.set_permissions(std::fs::Permissions::from_mode(m.mode() & 0o7777 & !0o111))?;
        }
        t.write_all(&tail)?;
        t.sync_all()?;
    }
    f.set_len(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logrotate_is_forced_only_for_configs_that_stay_in_safe_places() {
        let r = tmp("lrsafe");
        put(&r.join("var/log/app/app.log"), 3 << 20, 0);
        std::fs::create_dir_all(r.join("etc/logrotate.d")).unwrap();
        let log = r.join("var/log/app/app.log");
        for (conf, forced) in [
            ("/var/log/app/*.log {\n daily\n}\n", true),
            ("/var/log/app/*.log /srv/data/*.log {\n daily\n}\n", false),
            (
                "/var/log/app/*.log /var/log/mysql/*.log {\n daily\n}\n",
                false,
            ),
            (
                "/var/log/app/*.log /var/log/audit/*.log {\n daily\n}\n",
                false,
            ),
        ] {
            std::fs::write(r.join("etc/logrotate.d/app"), conf).unwrap();
            assert_eq!(logrotate_config_for(&r, &log).is_some(), forced, "{conf}");
        }
        std::fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn a_build_folder_reached_through_a_symlink_is_never_deleted() {
        let d = tmp("treeswap");
        let other = d.join("srv/bob/app");
        std::fs::create_dir_all(other.join("node_modules/x")).unwrap();
        std::fs::write(other.join("package.json"), b"{}").unwrap();
        std::fs::write(other.join("node_modules/x/keep.js"), b"k").unwrap();
        std::fs::create_dir_all(d.join("home/alice/code")).unwrap();
        // alice's project folder swapped for a link to bob's app
        std::os::unix::fs::symlink(&other, d.join("home/alice/code/proj")).unwrap();
        assert!(remove_tree(&d.join("home/alice/code/proj/node_modules")).is_err());
        assert!(other.join("node_modules/x/keep.js").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn file_actions_never_follow_a_swapped_symlink_or_hard_link() {
        let d = tmp("swap");
        let victim = d.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("x.log.1"), b"victim rotated").unwrap();
        std::fs::write(victim.join("secret"), b"do not touch").unwrap();
        // A folder on the way replaced by a symlink to somewhere else.
        std::os::unix::fs::symlink(&victim, d.join("logs")).unwrap();
        assert!(remove_file_beneath(&d.join("logs/x.log.1")).is_err());
        assert!(tail_and_truncate(&d.join("logs/secret"), 4).is_err());
        assert!(victim.join("x.log.1").exists());
        assert_eq!(
            std::fs::read(victim.join("secret")).unwrap(),
            b"do not touch"
        );
        // The file itself a symlink: refused.
        let real = d.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(victim.join("secret"), real.join("app.log")).unwrap();
        assert!(tail_and_truncate(&real.join("app.log"), 4).is_err());
        assert_eq!(
            std::fs::read(victim.join("secret")).unwrap(),
            b"do not touch"
        );
        // A hard link to another file under a log's name: refused.
        std::fs::hard_link(victim.join("secret"), real.join("hl.log")).unwrap();
        assert!(tail_and_truncate(&real.join("hl.log"), 4).is_err());
        assert_eq!(
            std::fs::read(victim.join("secret")).unwrap(),
            b"do not touch"
        );
        // A planted .dux-tail (hard link to the victim) is replaced, not written.
        std::fs::write(real.join("w.log"), b"0123456789").unwrap();
        std::fs::hard_link(victim.join("secret"), real.join("w.log.dux-tail")).unwrap();
        tail_and_truncate(&real.join("w.log"), 4).unwrap();
        assert_eq!(
            std::fs::read(victim.join("secret")).unwrap(),
            b"do not touch"
        );
        assert_eq!(std::fs::read(real.join("w.log.dux-tail")).unwrap(), b"6789");
        assert_eq!(std::fs::metadata(real.join("w.log")).unwrap().len(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn same_fs_delete_removes_trees_and_never_follows_symlinks() {
        let d = tmp("samefs");
        let outside = d.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"k").unwrap();
        let t = d.join("tree");
        std::fs::create_dir_all(t.join("a/b/c")).unwrap();
        std::fs::write(t.join("a/b/c/f"), b"x").unwrap();
        std::fs::write(t.join("top"), b"y").unwrap();
        std::os::unix::fs::symlink(&outside, t.join("a/link")).unwrap();
        remove_dir_same_fs(&t).unwrap();
        assert!(!t.exists());
        assert!(
            outside.join("keep").exists(),
            "a symlink inside is removed as a link, never followed"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn same_fs_delete_stops_at_a_mounted_filesystem() {
        // Only meaningful as root with /dev/shm (tmpfs) available.
        if unsafe { libc::geteuid() } != 0 || !Path::new("/dev/shm").is_dir() {
            return;
        }
        let d = tmp("mountmid");
        let t = d.join("tree");
        std::fs::create_dir_all(t.join("sub/mnt")).unwrap();
        std::fs::write(t.join("sub/file"), b"x").unwrap();
        // A bind mount of a folder on the SAME disk: same device, other mount.
        let src = d.join("elsewhere");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("precious"), b"p").unwrap();
        let ok = std::process::Command::new("mount")
            .args(["--bind"])
            .arg(&src)
            .arg(t.join("sub/mnt"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let r = remove_dir_same_fs(&t);
            let _ = std::process::Command::new("umount")
                .arg(t.join("sub/mnt"))
                .status();
            assert!(r.is_err(), "must stop at the mount");
            assert!(
                src.join("precious").exists(),
                "nothing on the other filesystem is deleted"
            );
        }
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&d);
    }

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
    fn user_deletes_refuse_system_and_top_level_paths() {
        for p in [
            "/",
            "/home",
            "/root",
            "/tmp",
            "/var",
            "/var/log",
            "/etc/nginx",
            "/usr/bin/x",
            "/boot/vmlinuz",
            "/var/lib/dpkg/status",
        ] {
            assert!(system_path(p), "{p} must be refused");
        }
        for p in [
            "/home/ann/Downloads/big.iso",
            "/srv/app/cache",
            "/var/log/app/old.log",
            "/root/tmp/x",
        ] {
            assert!(!system_path(p), "{p} may be deleted by the user");
        }
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
        assert!(is_protected(Path::new("mysql/error.log.1")));
        assert!(is_protected(Path::new("postgresql/postgresql-16-main.log")));
        assert!(is_protected(Path::new("mail.log.2.gz")));
        assert!(is_protected(Path::new("kern.log")));
        assert!(
            !is_protected(Path::new("mailman-web/x.log")),
            "a prefix only, not any word"
        );
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
