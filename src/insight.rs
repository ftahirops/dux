//! `dux biggest`, `dux logs` and `dux pp`: answers built on one fast walk.

use crate::cleanup::{Action, Item};
use crate::walker::{self, DirNode, FileView, FoundFile, Tree, WalkOptions};
use anyhow::Result;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;

/// uid -> user name, from /etc/passwd.
pub fn users() -> HashMap<u32, String> {
    std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
        })
        .collect()
}

/// (dev, ino) of every open regular file -> "pid name".
pub fn open_file_owners() -> HashMap<(u64, u64), String> {
    let mut out = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return out;
    };
    for p in procs.flatten() {
        let pid = p.file_name().to_string_lossy().into_owned();
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(p.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(m) = std::fs::metadata(fd.path()) {
                if m.is_file() {
                    out.entry((m.dev(), m.ino()))
                        .or_insert_with(|| format!("{pid} {comm}"));
                }
            }
        }
    }
    out
}

pub fn age_text(mtime: i64, now: i64) -> String {
    let d = (now - mtime).max(0);
    match d {
        0..=3599 => format!("{}m", d / 60),
        3600..=86_399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86_400),
    }
}

// ---------------------------------------------------------------- biggest

pub struct Biggest {
    pub tree: Tree,
    pub files: Vec<FoundFile>,
}

pub fn biggest(root: &str, threads: usize, n: usize, min_bytes: u64) -> Result<Biggest> {
    let opts = WalkOptions {
        filter: Some(Box::new(move |f: &FileView| f.bytes >= min_bytes)),
        max_found: 500_000,
        progress: None,
    };
    let (tree, mut files) = walker::walk_with(root, threads, opts)?;
    files.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    files.truncate(n);
    Ok(Biggest { tree, files })
}

// ---------------------------------------------------------------- logs

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogKind {
    System,
    Docker,
    App,
}

impl LogKind {
    pub fn label(self) -> &'static str {
        match self {
            LogKind::System => "system",
            LogKind::Docker => "docker",
            LogKind::App => "app",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogFile {
    pub path: String,
    pub bytes: u64,
    pub mtime: i64,
    pub kind: LogKind,
    pub rotated: bool,
    pub protected: bool,
    pub per_min: Option<f64>,
    pub writer: Option<String>,
    pub container: Option<String>,
}

/// Is this file a log, by name or location?
pub fn is_log(dir: &[u8], name: &[u8]) -> bool {
    let n = String::from_utf8_lossy(name);
    let d = String::from_utf8_lossy(dir);
    n.ends_with(".log")
        || n.contains(".log.")
        || n.ends_with("_log")
        || n.ends_with(".log.gz")
        || d.starts_with("/var/log")
        || d.contains("/log/")
        || d.contains("/logs/")
        || d.ends_with("/log")
        || d.ends_with("/logs")
}

fn classify_log(path: &str) -> LogKind {
    if path.contains("/docker/containers/") && path.ends_with("-json.log") {
        LogKind::Docker
    } else if path.starts_with("/var/log/") {
        LogKind::System
    } else {
        LogKind::App
    }
}

fn container_name(path: &str) -> Option<String> {
    let id = path.split("/containers/").nth(1)?.split('/').next()?;
    let cfg = std::fs::read_to_string(format!(
        "{}/containers/{id}/config.v2.json",
        path.split("/containers/").next()?
    ))
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&cfg).ok()?;
    Some(v["Name"].as_str()?.trim_start_matches('/').to_string())
}

pub struct LogReport {
    pub journal_bytes: Option<u64>,
    pub logs: Vec<LogFile>,
    pub walk_seconds: f64,
}

impl LogReport {
    pub fn total(&self, kind: Option<LogKind>) -> u64 {
        self.logs
            .iter()
            .filter(|l| kind.is_none_or(|k| l.kind == k))
            .map(|l| l.bytes)
            .sum()
    }
    pub fn rotated_total(&self) -> u64 {
        self.logs
            .iter()
            .filter(|l| l.rotated)
            .map(|l| l.bytes)
            .sum()
    }
}

/// Find every log of at least `min_bytes` on `root`'s filesystem, then sample
/// the growth of the largest active ones for `sample_secs`.
pub fn logs(root: &str, threads: usize, min_bytes: u64, sample_secs: u64) -> Result<LogReport> {
    let opts = WalkOptions {
        filter: Some(Box::new(move |f: &FileView| {
            f.bytes >= min_bytes && is_log(f.dir, f.name)
        })),
        max_found: 200_000,
        progress: None,
    };
    let (tree, found) = walker::walk_with(root, threads, opts)?;
    let owners = open_file_owners();
    let mut logs: Vec<LogFile> = found
        .into_iter()
        .map(|f| {
            let name = f.path.rsplit('/').next().unwrap_or("").to_string();
            let rel = f.path.strip_prefix("/var/log/").unwrap_or(&f.path);
            let kind = classify_log(&f.path);
            LogFile {
                rotated: crate::cleanup::rotated_base(&name).is_some(),
                protected: crate::cleanup::is_protected(std::path::Path::new(rel)),
                writer: owners.get(&(f.dev, f.ino)).cloned(),
                container: (kind == LogKind::Docker)
                    .then(|| container_name(&f.path))
                    .flatten(),
                kind,
                bytes: f.bytes,
                mtime: f.mtime,
                path: f.path,
                per_min: None,
            }
        })
        .collect();
    logs.sort_by_key(|l| std::cmp::Reverse(l.bytes));
    if sample_secs > 0 {
        let watch: Vec<usize> = logs
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.rotated)
            .map(|(i, _)| i)
            .take(40)
            .collect();
        let before: Vec<Option<u64>> = watch
            .iter()
            .map(|&i| std::fs::metadata(&logs[i].path).ok().map(|m| m.len()))
            .collect();
        std::thread::sleep(std::time::Duration::from_secs(sample_secs));
        for (k, &i) in watch.iter().enumerate() {
            if let (Some(b), Ok(m)) = (before[k], std::fs::metadata(&logs[i].path)) {
                logs[i].per_min =
                    Some(m.len().saturating_sub(b) as f64 * 60.0 / sample_secs as f64);
            }
        }
    }
    let journal_bytes = (root == "/").then(crate::cleanup::journal_usage).flatten();
    Ok(LogReport {
        journal_bytes,
        logs,
        walk_seconds: tree.stats.seconds,
    })
}

// ---------------------------------------------------------------- projects

/// Build output and dependency folders that rebuild themselves.
pub const ARTIFACTS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    ".tox",
    ".parcel-cache",
    ".turbo",
    ".angular",
    "coverage",
    ".cache",
];

/// Never treated as projects: system and package-manager trees.
const NOT_PROJECTS: &[&str] = &[
    "/proc",
    "/sys",
    "/usr",
    "/lib",
    "/lib64",
    "/bin",
    "/sbin",
    "/boot",
    "/snap",
    "/etc",
    "/dev",
    "/run",
    "/var/lib",
    "/var/cache",
    "/root/.cargo",
    "/root/.rustup",
    "/root/go/pkg",
];

#[derive(Clone, Debug)]
pub struct Project {
    pub path: String,
    pub kinds: Vec<&'static str>,
    pub total: u64,
    pub artifacts: u64,
    pub artifact_dirs: Vec<(String, u64)>,
    pub git: u64,
    pub last_activity: i64,
}

impl Project {
    pub fn source(&self) -> u64 {
        self.total.saturating_sub(self.artifacts + self.git)
    }
}

pub fn kinds(m: u16) -> Vec<&'static str> {
    use crate::walker::*;
    let all = [
        (MARK_NODE, "node"),
        (MARK_RUST, "rust"),
        (MARK_GO, "go"),
        (MARK_PYTHON, "python"),
        (MARK_PHP, "php"),
        (MARK_RUBY, "ruby"),
        (MARK_JAVA, "java"),
        (MARK_DOTNET, ".net"),
        (MARK_CMAKE, "c/c++"),
        (MARK_COMPOSE, "compose"),
        (MARK_GIT, "git"),
    ];
    all.iter()
        .filter(|(b, _)| m & b != 0)
        .map(|(_, n)| *n)
        .collect()
}

/// Newest change in a subtree, ignoring build output and .git (those change
/// when tools run, not when people work).
fn activity(n: &DirNode) -> i64 {
    n.children
        .iter()
        .filter(|c| c.name != ".git" && !ARTIFACTS.contains(&c.name.as_str()))
        .map(activity)
        .fold(n.own_change, i64::max)
}

fn collect_artifacts(n: &DirNode, path: &str, depth: usize, out: &mut Vec<(String, u64)>) {
    for c in &n.children {
        let p = format!("{path}/{}", c.name);
        if ARTIFACTS.contains(&c.name.as_str()) {
            out.push((p, c.total_bytes));
        } else if c.name != ".git" && depth < 6 {
            collect_artifacts(c, &p, depth + 1, out);
        }
    }
}

/// Find projects: the top-most folder with a marker file on each branch.
pub fn projects(tree: &Tree) -> Vec<Project> {
    let mut out = Vec::new();
    fn rec(n: &DirNode, path: &str, out: &mut Vec<Project>) {
        if NOT_PROJECTS
            .iter()
            .any(|x| path == *x || path.starts_with(&format!("{x}/")))
            || ARTIFACTS.contains(&n.name.as_str())
        {
            return;
        }
        if n.markers != 0 && path != "/" {
            let mut arts = Vec::new();
            collect_artifacts(n, path, 0, &mut arts);
            arts.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
            out.push(Project {
                path: path.to_string(),
                kinds: kinds(n.markers),
                total: n.total_bytes,
                artifacts: arts.iter().map(|(_, b)| *b).sum(),
                git: n
                    .children
                    .iter()
                    .find(|c| c.name == ".git")
                    .map_or(0, |g| g.total_bytes),
                last_activity: activity(n),
                artifact_dirs: arts,
            });
            return;
        }
        for c in &n.children {
            let p = if path == "/" {
                format!("/{}", c.name)
            } else {
                format!("{path}/{}", c.name)
            };
            rec(c, &p, out);
        }
    }
    let root = tree.root_path.trim_end_matches('/');
    rec(
        &tree.root,
        if root.is_empty() { "/" } else { root },
        &mut out,
    );
    out.sort_by_key(|p| std::cmp::Reverse(p.total));
    out
}

/// Well-known application data folders and every folder under /opt, /srv, /home.
pub fn apps(tree: &Tree) -> Vec<(String, String, u64, i64)> {
    const KNOWN: &[(&str, &str)] = &[
        ("var/lib/mysql", "MySQL / MariaDB"),
        ("var/lib/postgresql", "PostgreSQL"),
        ("var/lib/redis", "Redis"),
        ("var/lib/mongodb", "MongoDB"),
        ("var/lib/elasticsearch", "Elasticsearch"),
        ("var/lib/clickhouse", "ClickHouse"),
        ("var/lib/docker", "Docker"),
        ("var/lib/containerd", "containerd"),
        ("var/lib/snapd", "Snap packages"),
        ("var/lib/flatpak", "Flatpak"),
        ("var/www", "web root"),
        ("var/lib/apt", "apt package lists"),
        ("var/cache", "system caches"),
        ("var/log", "system logs"),
        ("var/lib/xgs", "XGenStack agent"),
        ("var/lib/libvirt", "libvirt VMs"),
        ("var/lib/lxd", "LXD"),
        ("var/mail", "mail"),
        ("var/spool", "spools"),
    ];
    if tree.root_path != "/" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (rel, name) in KNOWN {
        if let Some(n) = tree.root.find(rel) {
            out.push((
                name.to_string(),
                format!("/{rel}"),
                n.total_bytes,
                n.newest_change,
            ));
        }
    }
    for top in ["opt", "srv", "home"] {
        if let Some(t) = tree.root.find(top) {
            for c in &t.children {
                let label = if top == "home" {
                    format!("home of {}", c.name)
                } else {
                    c.name.clone()
                };
                out.push((
                    label,
                    format!("/{top}/{}", c.name),
                    c.total_bytes,
                    c.newest_change,
                ));
            }
        }
    }
    if let Some(r) = tree.root.find("root") {
        out.push((
            "home of root".into(),
            "/root".into(),
            r.total_bytes,
            r.newest_change,
        ));
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.2));
    out
}

/// Does a `pp_plan` title say the project has been idle at least `days`?
pub fn idle_at_least(title: &str, days: u64) -> bool {
    title
        .split("(idle ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|n| n >= days)
}

/// Remove build output in projects idle for at least `stale_days`.
pub fn pp_plan(projects: &[Project], stale_days: u64, now: i64) -> Vec<Item> {
    let mut items = Vec::new();
    for p in projects {
        let idle = (now - p.last_activity).max(0) as u64 / 86_400;
        if idle < stale_days {
            continue;
        }
        for (dir, bytes) in &p.artifact_dirs {
            if *bytes >= 1 << 20 {
                items.push(Item {
                    level: 4,
                    what: format!("build output in {} (idle {idle} days)", p.path),
                    action: Action::RemoveTree { path: dir.into() },
                    frees: *bytes,
                });
            }
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.frees));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dux-insight-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &std::path::Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn log_detection() {
        assert!(is_log(b"/srv/app", b"server.log"));
        assert!(is_log(b"/srv/app", b"server.log.3"));
        assert!(is_log(b"/var/log/nginx", b"access"));
        assert!(is_log(b"/opt/x/logs", b"out"));
        assert!(!is_log(b"/srv/app", b"catalog.db"));
        assert_eq!(
            classify_log("/var/lib/docker/containers/ab/ab-json.log"),
            LogKind::Docker
        );
        assert_eq!(classify_log("/var/log/syslog"), LogKind::System);
        assert_eq!(classify_log("/srv/app/x.log"), LogKind::App);
    }

    #[test]
    fn biggest_and_logs_on_a_tree() {
        let d = tmp("big");
        put(&d.join("a/huge.bin"), 3 << 20);
        put(&d.join("a/app.log"), 2 << 20);
        put(&d.join("b/app.log.1"), 1 << 20);
        put(&d.join("b/tiny.log"), 100);
        let b = biggest(d.to_str().unwrap(), 2, 2, 1 << 20).unwrap();
        assert_eq!(b.files.len(), 2);
        assert!(b.files[0].path.ends_with("a/huge.bin"));
        let r = logs(d.to_str().unwrap(), 2, 1 << 20, 0).unwrap();
        let names: Vec<&str> = r
            .logs
            .iter()
            .map(|l| l.path.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(names, vec!["app.log", "app.log.1"]);
        assert!(!r.logs[0].rotated && r.logs[1].rotated);
        assert_eq!(r.total(Some(LogKind::App)), r.total(None));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn projects_split_source_artifacts_and_git() {
        let d = tmp("pp");
        put(&d.join("web/package.json"), 100);
        put(&d.join("web/src/index.js"), 200_000);
        put(&d.join("web/node_modules/lib/index.js"), 3 << 20);
        put(&d.join("web/node_modules/lib/package.json"), 50);
        put(&d.join("web/.git/objects/pack"), 1 << 20);
        put(&d.join("api/Cargo.toml"), 50);
        put(&d.join("api/target/debug/api"), 2 << 20);
        put(&d.join("notes/readme.txt"), 10);
        let (t, _) = walker::walk_with(d.to_str().unwrap(), 2, WalkOptions::default()).unwrap();
        let ps = projects(&t);
        let names: Vec<String> = ps
            .iter()
            .map(|p| p.path.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["web", "api"],
            "nested node_modules package.json is not a project; notes has no marker"
        );
        let web = &ps[0];
        assert_eq!(web.kinds, vec!["node", "git"]);
        assert!(web.artifacts >= 3 << 20 && web.git >= 1 << 20);
        assert!(web.source() >= 200_000 && web.source() < 1 << 20);
        // Fresh projects are never planned for cleanup; idle ones are.
        let now = crate::util::now_secs();
        assert!(pp_plan(&ps, 30, now).is_empty());
        let items = pp_plan(&ps, 30, now + 40 * 86_400);
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&items[0].action, Action::RemoveTree { path } if path.ends_with("node_modules"))
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
