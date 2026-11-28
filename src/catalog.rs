//! What Cleanup may offer (spec A6 revision, A6.18-A6.25).
//!
//! Only data a rule here identifies as deletable is ever suggested. Being
//! big never qualifies. Every suggestion carries a category, and the views
//! fold the list by it.

use crate::cleanup::{Action, Item};
use crate::fix::{suggestion, Safety, Suggestion};
use crate::walker::{DirNode, FoundFile, Tree};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const PROJECTS: &str = "Project build output";
pub const TOOL_CACHES: &str = "Tool caches";
pub const LEFTOVERS: &str = "Leftover files";
pub const BACKUPS: &str = "Old backups";
pub const CMS: &str = "Web app caches";
pub const SERVER: &str = "System";
pub const CI: &str = "CI work folders";

const DAY: i64 = 86_400;

// ------------------------------------------------------------ never listed

/// A6.18: never listed, whatever a rule says.
pub fn never(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    const DB_EXT: &[&str] = &[
        ".sqlite",
        ".sqlite3",
        ".db",
        ".db3",
        ".mdb",
        ".accdb",
        ".ldb",
        ".mdf",
        ".ndf",
        ".ibd",
        ".frm",
        ".myd",
        ".myi",
        ".wal",
        ".shm",
        ".db-wal",
        ".db-shm",
        ".sqlite-wal",
        ".sqlite-shm",
        ".kdbx",
        ".realm",
        ".lmdb",
        ".rdb",
        ".aof",
    ];
    if DB_EXT.iter().any(|e| name.ends_with(e)) || name.contains(".sqlite") || name.contains(".db-")
    {
        return true;
    }
    if matches!(name.as_str(), "swap.img" | "swapfile" | "swap") {
        return true;
    }
    // The one file dux may touch under Docker's data root: a container's own
    // log, truncated in place (A6.13).
    if path.starts_with("/var/lib/docker/containers/") && name.ends_with("-json.log") {
        return false;
    }
    const UNDER: &[&str] = &[
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib32",
        "/lib64",
        "/boot",
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/snap",
        "/var/lib/docker",
        "/var/lib/containerd",
        "/var/lib/containers",
        "/var/lib/mysql",
        "/var/lib/postgresql",
        "/var/lib/mongodb",
        "/var/lib/redis",
        "/var/lib/elasticsearch",
        "/var/lib/clickhouse",
        "/var/lib/influxdb",
        "/var/lib/rabbitmq",
        "/var/lib/etcd",
        "/var/lib/kubelet",
        "/var/lib/rancher",
        "/var/lib/snapd",
        "/var/lib/dpkg",
        "/var/lib/rpm",
        "/var/lib/apt",
        "/var/lib/dux",
    ];
    if UNDER
        .iter()
        .any(|u| path == *u || path.starts_with(&format!("{u}/")))
    {
        return true;
    }
    // Git object stores and app state in homes (~/.local/share, ~/.claude...).
    if path.contains("/.git/") || path.ends_with("/.git") {
        return true;
    }
    const STATE: &[&str] = &[
        "/.local/share/",
        "/.local/state/",
        "/.claude/",
        "/.codex/",
        "/.config/",
        "/.ssh/",
        "/.gnupg/",
        "/.mozilla/",
        "/.vscode-server/data/",
        "/.docker/",
        "/.kube/",
        "/.aws/",
        "/.azure/",
        "/.gcloud/",
        "/.terraform.d/",
    ];
    let p = format!("{path}/");
    STATE.iter().any(|s| p.contains(s))
}

// ----------------------------------------------------- leftover file rules

/// A6.21 / A6.22 rules matched by name while walking (cheap), finished later.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum FileRule {
    /// SAFE junk older than 7 days.
    Junk,
    /// CAUTION, older than 30 days, never the newest of its series.
    Backup,
    /// CMS backup archive: CAUTION, 30 days, a newer one must exist.
    CmsBackup,
    /// SQL dump: as CmsBackup; in a web root it is flagged, never deleted.
    Dump,
    /// Duplicator installer leftovers: SAFE.
    Installer,
}

pub fn file_rule(dir: &[u8], name: &[u8]) -> Option<FileRule> {
    let n = String::from_utf8_lossy(name);
    let d = String::from_utf8_lossy(dir);
    let lower = n.to_ascii_lowercase();
    if never(&format!("{d}/{n}")) {
        return None;
    }
    // Inside a dependency or package tree a file belongs to the package
    // (crates ship Cargo.toml.orig): leftover rules never look there.
    const PACKAGES: &[&str] = &[
        "/.cargo/",
        "/.rustup/",
        "/node_modules/",
        "/site-packages/",
        "/dist-packages/",
        "/go/pkg/mod/",
        "/vendor/",
        "/.npm/",
        "/.cache/",
        "/.m2/",
        "/.gradle/",
        "/.pnpm-store/",
        "/.yarn/",
        "/target/",
        "/.venv/",
        "/venv/",
        "/bower_components/",
        "/.nuget/",
        "/.composer/",
        "/.local/lib/",
        "/.pyenv/",
        "/.nvm/",
        "/.sdkman/",
    ];
    let dd = format!("{d}/");
    // System configuration is never cleaned by name rules.
    if PACKAGES.iter().any(|p| dd.contains(p)) || dd.starts_with("/etc/") {
        return None;
    }
    if d.contains("/wp-content/ai1wm-backups") && lower.ends_with(".wpress")
        || d.contains("/wp-content/updraft") && lower.starts_with("backup_")
        || d.contains("/backwpup-") && d.contains("-backups")
        || (d.contains("/wp-content/backups-dup") || d.contains("/wp-snapshots"))
            && (lower.ends_with(".zip") || lower.ends_with(".daf"))
        || d.contains("/com_akeeba/backup")
            && (lower.ends_with(".jpa") || lower.ends_with(".jps") || lower.contains(".j0"))
    {
        return Some(FileRule::CmsBackup);
    }
    if lower.ends_with(".sql")
        || lower.ends_with(".sql.gz")
        || lower.ends_with(".sql.zst")
        || lower.ends_with(".sql.xz")
        || lower.ends_with(".sql.bz2")
        || lower.ends_with(".dump")
        || lower.ends_with(".pgdump")
    {
        return Some(FileRule::Dump);
    }
    if matches!(lower.as_str(), "installer.php" | "installer-backup.php") && !d.contains("/vendor/")
    {
        return Some(FileRule::Installer);
    }
    if n.ends_with('~')
        || lower.ends_with(".orig")
        || lower.ends_with(".rej")
        || n == "core"
        || n.strip_prefix("core.")
            .is_some_and(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
        || (lower.starts_with("hs_err_pid") || lower.starts_with("replay_pid"))
            && lower.ends_with(".log")
        || matches!(n.as_ref(), ".DS_Store" | "Thumbs.db" | "desktop.ini")
    {
        return Some(FileRule::Junk);
    }
    if (lower.ends_with(".bak")
        || lower.ends_with(".old")
        || lower.ends_with(".tmp")
        || lower.ends_with(".swp")
        || lower.ends_with(".swo"))
        && !d.starts_with("/etc")
        && !d.starts_with("/var/log")
    {
        return Some(FileRule::Backup);
    }
    None
}

/// The series a backup belongs to: same folder, name without dates,
/// numbers and backup extensions ("site-2026-01-02.sql.gz" -> "site-.sql").
fn series_key(path: &str) -> String {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut n = name.to_ascii_lowercase();
    for e in [
        ".gz", ".zst", ".xz", ".bz2", ".bak", ".old", ".tmp", ".swp", ".swo",
    ] {
        if let Some(s) = n.strip_suffix(e) {
            n = s.to_string();
        }
    }
    let n: String = n.chars().filter(|c| !c.is_ascii_digit()).collect();
    format!("{dir}/{n}")
}

fn age_ok(mtime: i64, now: i64, days: i64) -> bool {
    now - mtime >= days * DAY
}

/// Leftover files, old backups and dumps found by the walk.
pub fn file_suggestions(found: &[FoundFile], now: i64, web_roots: &[String]) -> Vec<Suggestion> {
    let open = crate::cleanup::open_now();
    let mut out = Vec::new();
    // A series is one owner's files: another user cannot make someone's
    // backup look old by planting a "newer" one beside it.
    let mut series: HashMap<(FileRule, String, u32), Vec<&FoundFile>> = HashMap::new();
    for f in found {
        let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
        let Some(rule) = file_rule(dir.as_bytes(), name.as_bytes()) else {
            continue;
        };
        if open.contains(&(f.dev, f.ino)) {
            continue;
        }
        match rule {
            FileRule::Junk if age_ok(f.mtime, now, 7) => out.push(remove(
                LEFTOVERS,
                Safety::Safe,
                &f.path,
                f.bytes,
                "Editor backup, patch leftover, crash core or OS junk, untouched for 7+ days.",
                "Not needed: nothing uses these files.",
            )),
            FileRule::Installer if age_ok(f.mtime, now, 7) => out.push(remove(
                CMS,
                Safety::Safe,
                &f.path,
                f.bytes,
                "Duplicator installer left behind after a migration; Duplicator says to remove it (it is also a security risk).",
                "Not needed after the migration.",
            )),
            FileRule::Dump if web_roots.iter().any(|w| f.path.starts_with(&format!("{w}/"))) => {
                out.push(suggestion(
                    Safety::Risky,
                    format!("database dump inside a web root: {}", f.path),
                    "Anyone who can reach the site may be able to download it. dux does not delete it: move it out of the web root.",
                    "-",
                    f.bytes,
                    None,
                    Some(format!("mv {} /root/  # or delete it if a newer backup exists", shell_quote(&f.path))),
                ).with_category(BACKUPS));
            }
            // A future time (clock skew, or set on purpose) proves nothing.
            FileRule::Backup | FileRule::CmsBackup | FileRule::Dump if f.mtime <= now => {
                series.entry((rule, series_key(&f.path), f.uid)).or_default().push(f);
            }
            _ => {}
        }
    }
    for ((rule, _, _), mut files) in series {
        // The newest of a series is never offered; older ones after 30 days.
        files.sort_by_key(|f| std::cmp::Reverse(f.mtime));
        for f in files
            .into_iter()
            .skip(1)
            .filter(|f| age_ok(f.mtime, now, 30))
        {
            let (cat, why) = match rule {
                FileRule::CmsBackup => (BACKUPS, "An older site backup; a newer one of the same site is kept. Check it is not your only off-site copy."),
                FileRule::Dump => (BACKUPS, "An older database dump; a newer dump of the same database is kept."),
                _ => (LEFTOVERS, "An old .bak/.old/.tmp/.swp copy (30+ days); a newer one of the same file is kept."),
            };
            out.push(remove(
                cat,
                Safety::Caution,
                &f.path,
                f.bytes,
                why,
                "Only from your own backup.",
            ));
        }
    }
    out
}

fn remove(
    cat: &'static str,
    s: Safety,
    path: &str,
    bytes: u64,
    why: &str,
    undo: &str,
) -> Suggestion {
    let item = Item {
        level: 4,
        what: format!("remove {path}"),
        action: Action::Remove { path: path.into() },
        frees: bytes,
    };
    suggestion(s, item.what.clone(), why, undo, bytes, Some(item), None).with_category(cat)
}

fn remove_dir(
    cat: &'static str,
    s: Safety,
    path: &str,
    bytes: u64,
    why: &str,
    undo: &str,
) -> Suggestion {
    let item = Item {
        level: 4,
        what: format!("remove {path}"),
        action: Action::RemoveTree { path: path.into() },
        frees: bytes,
    };
    suggestion(s, item.what.clone(), why, undo, bytes, Some(item), None).with_category(cat)
}

// ------------------------------------------------------- known folders

/// Folders a rule here may remove (beyond project build output): used by
/// `cleanup::remove_tree` to allow them, checked again on disk.
pub fn known_dir(path: &Path) -> bool {
    let p = path.to_string_lossy();
    if never(&p) {
        return false;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    cms_cache_dir(&p) || tool_dir(&p).is_some() || old_browser_dir(&parent, &name) || ci_dir(path)
}

/// A web app's cache folder, only next to that app's own marker files.
fn cms_cache_dir(p: &str) -> bool {
    let has = |root: &str, marker: &str| Path::new(root).join(marker).exists();
    for (tail, marker) in [
        ("/wp-content/cache", "wp-config.php"),
        ("/wp-content/upgrade", "wp-config.php"),
        ("/storage/framework/cache/data", "artisan"),
        ("/storage/framework/views", "artisan"),
        ("/var/page_cache", "bin/magento"),
        ("/var/view_preprocessed", "bin/magento"),
    ] {
        if let Some(root) = p.strip_suffix(tail) {
            return !root.is_empty() && has(root, marker);
        }
    }
    if let Some(root) = p.strip_suffix("/var/cache") {
        return !root.is_empty() && (has(root, "bin/console") || has(root, "bin/magento"));
    }
    // Drupal: <root>/sites/<site>/files/{css,js,php}, <root>/core present.
    let parts: Vec<&str> = p.rsplitn(5, '/').collect();
    if let [leaf, "files", _site, "sites", root] = parts.as_slice() {
        return matches!(*leaf, "css" | "js" | "php") && !root.is_empty() && has(root, "core");
    }
    false
}

/// Single-quote for a shell, so a copied hint cannot run anything else.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Tool caches removed as folders (no reliable command of their own).
fn tool_dir(p: &str) -> Option<(&'static str, Safety)> {
    const T: &[(&str, &str, Safety)] = &[
        (
            "/.cargo/registry/cache",
            "cargo download cache",
            Safety::Safe,
        ),
        (
            "/.cargo/registry/src",
            "cargo unpacked sources",
            Safety::Safe,
        ),
        ("/.cargo/git/checkouts", "cargo git checkouts", Safety::Safe),
        ("/.gradle/caches", "Gradle caches", Safety::Safe),
        ("/.cache/node-gyp", "node-gyp headers", Safety::Safe),
        ("/.cache/electron", "Electron downloads", Safety::Safe),
        (
            "/.cache/electron-builder",
            "electron-builder cache",
            Safety::Safe,
        ),
        (
            "/.cache/huggingface/hub",
            "Hugging Face models (large to download again)",
            Safety::Caution,
        ),
        (
            "/.m2/repository",
            "Maven repository (may hold artifacts built here)",
            Safety::Caution,
        ),
    ];
    T.iter()
        .find(|(s, _, _)| p.ends_with(s))
        .map(|(_, l, s)| (*l, *s))
}

/// An old browser build of Playwright/Puppeteer/Cypress: a newer one of
/// the same browser sits beside it.
fn old_browser_dir(parent: &str, name: &str) -> bool {
    let browsers = parent.ends_with("/.cache/ms-playwright")
        || parent.contains("/.cache/puppeteer/")
        || parent.ends_with("/.cache/Cypress");
    if !browsers {
        return false;
    }
    let (base, ver) = split_version(name);
    let Ok(rd) = std::fs::read_dir(parent) else {
        return false;
    };
    rd.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        let (b, v) = split_version(&n);
        b == base && cmp_version(&v, &ver) == std::cmp::Ordering::Greater
    })
}

fn split_version(n: &str) -> (String, String) {
    match n.rsplit_once('-') {
        Some((b, v)) if v.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            (b.to_string(), v.to_string())
        }
        _ if n.chars().next().is_some_and(|c| c.is_ascii_digit()) => (String::new(), n.to_string()),
        _ => (n.to_string(), String::new()),
    }
}

fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let p = |s: &str| {
        s.split(['.', '_', '-'])
            .map(|x| x.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    p(a).cmp(&p(b))
}

fn ci_dir(path: &Path) -> bool {
    let p = path.to_string_lossy();
    let parent = path
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    // GitHub Actions: <runner>/_work/<repo>, with <runner>/.runner present.
    let gh = parent.ends_with("/_work")
        && path
            .parent()
            .and_then(|x| x.parent())
            .is_some_and(|r| r.join(".runner").exists())
        && !matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("_tool" | "_actions" | "_temp" | "_PipelineMapping")
        );
    let gitlab =
        parent.starts_with("/home/gitlab-runner/builds/") && parent.matches('/').count() == 4;
    let jenkins = parent.ends_with("/jenkins/workspace") || parent.ends_with("/.jenkins/workspace");
    gh || gitlab || jenkins || p.ends_with("/_work/_temp")
}

// -------------------------------------------------------- folder scanning

fn path_of(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Walk the tree for CMS caches, tool caches, old browser builds and CI
/// folders. Returns (category, safety, path, bytes, why, undo).
pub fn folder_suggestions(tree: &Tree, now: i64) -> Vec<Suggestion> {
    let mut out = Vec::new();
    fn rec(n: &DirNode, path: &str, depth: usize, now: i64, out: &mut Vec<Suggestion>) {
        if depth > 12 || never(path) {
            return;
        }
        for c in &n.children {
            let p = path_of(path, &c.name);
            if never(&p) || c.total_bytes < 1 << 20 {
                continue;
            }
            if cms_cache_dir(&p) {
                out.push(remove_dir(CMS, Safety::Safe, &p, c.total_bytes, "A web app's cache (WordPress, Laravel, Symfony, Magento, Drupal); the app rebuilds it on demand.", "Rebuilt automatically (first page loads are slower)."));
                continue;
            }
            if let Some((label, s)) = tool_dir(&p) {
                out.push(remove_dir(
                    TOOL_CACHES,
                    s,
                    &p,
                    c.total_bytes,
                    &format!("{label}: downloaded again when needed."),
                    "Downloaded again on the next build.",
                ));
                continue;
            }
            if old_browser_dir(path, &c.name) {
                out.push(remove_dir(
                    TOOL_CACHES,
                    Safety::Safe,
                    &p,
                    c.total_bytes,
                    "An old browser build of Playwright/Puppeteer/Cypress; the newest is kept.",
                    "Installed again by the tool if a project still pins this version.",
                ));
                continue;
            }
            if ci_dir(Path::new(&p)) && now - c.newest_change >= 7 * DAY {
                out.push(remove_dir(CI, Safety::Caution, &p, c.total_bytes, "A CI runner's work folder untouched for 7+ days; the runner checks it out again on the next job.", "The next CI job fetches it again (slower once)."));
                continue;
            }
            rec(c, &p, depth + 1, now, out);
        }
    }
    let root = tree.root_path.trim_end_matches('/');
    rec(
        &tree.root,
        if root.is_empty() { "/" } else { root },
        0,
        now,
        &mut out,
    );
    out
}

// ---------------------------------------------------------- tool commands

/// Per-user caches with a cleaner of their own (A6.20): (relative folder,
/// tool, args, label, safety).
const TOOL_CMDS: &[(&str, &str, &[&str], &str, Safety)] = &[
    (
        ".cache/pip",
        "pip3",
        &["cache", "purge"],
        "pip download cache",
        Safety::Safe,
    ),
    (
        ".npm/_cacache",
        "npm",
        &["cache", "clean", "--force"],
        "npm download cache",
        Safety::Safe,
    ),
    (
        ".cache/yarn",
        "yarn",
        &["cache", "clean"],
        "yarn cache",
        Safety::Safe,
    ),
    (
        ".cache/go-build",
        "go",
        &["clean", "-cache"],
        "Go build cache",
        Safety::Safe,
    ),
    (
        "go/pkg/mod",
        "go",
        &["clean", "-modcache"],
        "Go module cache (large to download again)",
        Safety::Caution,
    ),
    (
        ".cache/composer",
        "composer",
        &["clear-cache"],
        "Composer cache",
        Safety::Safe,
    ),
];

fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(tool))
                .find(|x| x.is_file())
        })
        .unwrap_or(None)
        .or_else(|| {
            ["/usr/local/bin", "/usr/bin", "/usr/local/go/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|x| x.is_file())
        })
}

/// Home folders: (user, home).
pub fn homes() -> Vec<(String, String)> {
    let text = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 7 && (f[5] == "/root" || f[5].starts_with("/home/")))
                .then(|| (f[0].to_string(), f[5].to_string()))
        })
        .collect()
}

pub fn tool_suggestions(tree: &Tree) -> Vec<Suggestion> {
    let root = tree.root_path.trim_end_matches('/');
    let mut out = Vec::new();
    for (user, home) in homes() {
        for (rel, tool, args, label, safety) in TOOL_CMDS {
            let p = format!("{home}/{rel}");
            let r = p.strip_prefix(root).unwrap_or(&p).trim_start_matches('/');
            let Some(n) = tree.root.find(r) else { continue };
            if n.total_bytes < 1 << 20 || never(&p) {
                continue;
            }
            let Some(bin) = which(tool) else { continue };
            // Run as the cache's owner, with its own HOME.
            let mut a: Vec<String> = vec![
                "-u".into(),
                user.clone(),
                "--".into(),
                "env".into(),
                format!("HOME={home}"),
                bin.to_string_lossy().into_owned(),
            ];
            a.extend(args.iter().map(|s| s.to_string()));
            let item = Item {
                level: 3,
                what: format!("{label} of {user} ({p})"),
                action: Action::Run {
                    program: "runuser".into(),
                    args: a,
                },
                frees: n.total_bytes,
            };
            out.push(
                suggestion(
                    *safety,
                    item.what.clone(),
                    &format!(
                        "{label}: cleared with `{tool} {}` as {user}.",
                        args.join(" ")
                    ),
                    "Downloaded again when needed.",
                    n.total_bytes,
                    Some(item),
                    None,
                )
                .with_category(TOOL_CACHES),
            );
        }
    }
    out
}

// ------------------------------------------------------------- live apps

/// Paths that show a project is in use (A6.19): process cwd/exe/open
/// files, systemd units, pm2 apps, Docker bind mounts, web roots.
pub struct InUse {
    pub paths: Vec<String>,
    pub web_roots: Vec<String>,
}

impl InUse {
    pub fn collect(docker: Option<&crate::docker::Report>) -> InUse {
        let mut paths: HashSet<String> = HashSet::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for p in rd.flatten() {
                let pid = p.file_name().to_string_lossy().into_owned();
                if !pid.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                for l in ["cwd", "exe"] {
                    if let Ok(t) = std::fs::read_link(p.path().join(l)) {
                        paths.insert(t.to_string_lossy().into_owned());
                    }
                }
                if let Ok(fds) = std::fs::read_dir(p.path().join("fd")) {
                    for fd in fds.flatten() {
                        if let Ok(t) = std::fs::read_link(fd.path()) {
                            let t = t.to_string_lossy();
                            if t.starts_with('/') {
                                paths.insert(t.into_owned());
                            }
                        }
                    }
                }
            }
        }
        // systemd units: WorkingDirectory and paths in ExecStart.
        if let Ok(o) = std::process::Command::new("systemctl")
            .args([
                "show",
                "--type=service",
                "--all",
                "-p",
                "WorkingDirectory",
                "-p",
                "ExecStart",
                "*",
            ])
            .stderr(std::process::Stdio::null())
            .output()
        {
            for tok in String::from_utf8_lossy(&o.stdout)
                .split(|c: char| c.is_whitespace() || c == ';' || c == '=')
            {
                if tok.starts_with('/') && tok.len() > 1 {
                    paths.insert(tok.to_string());
                }
            }
        }
        // pm2 apps.
        for (_, home) in homes() {
            if let Ok(t) = std::fs::read_to_string(format!("{home}/.pm2/dump.pm2")) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    for a in v.as_array().into_iter().flatten() {
                        for k in ["pm_cwd", "pm_exec_path"] {
                            if let Some(p) = a[k].as_str() {
                                paths.insert(p.to_string());
                            }
                        }
                    }
                }
            }
        }
        // Docker bind mounts.
        if docker.is_some() {
            if let Ok(o) = std::process::Command::new("docker")
                .args(["ps", "-aq"])
                .stderr(std::process::Stdio::null())
                .output()
            {
                let ids: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect();
                if !ids.is_empty() {
                    let mut a = vec![
                        "inspect".to_string(),
                        "--format".into(),
                        "{{range .Mounts}}{{if eq .Type \"bind\"}}{{.Source}}\n{{end}}{{end}}"
                            .into(),
                    ];
                    a.extend(ids);
                    if let Ok(o) = std::process::Command::new("docker")
                        .args(&a)
                        .stderr(std::process::Stdio::null())
                        .output()
                    {
                        for l in String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .filter(|l| l.starts_with('/'))
                        {
                            paths.insert(l.to_string());
                        }
                    }
                }
            }
        }
        let web_roots = web_roots();
        paths.extend(web_roots.iter().cloned());
        InUse {
            paths: paths.into_iter().collect(),
            web_roots,
        }
    }

    /// Does anything in use sit inside `dir`, or does `dir` sit inside a
    /// web root or service folder?
    pub fn uses(&self, dir: &str) -> bool {
        let d = format!("{}/", dir.trim_end_matches('/'));
        self.paths.iter().any(|p| {
            p.starts_with(&d)
                || *p == dir
                || (d.starts_with(&format!("{}/", p.trim_end_matches('/')))
                    && p.len() > 1
                    && !matches!(
                        p.as_str(),
                        "/root" | "/home" | "/srv" | "/opt" | "/var" | "/tmp"
                    ))
        })
    }
}

/// nginx root/alias and Apache DocumentRoot paths.
pub fn web_roots() -> Vec<String> {
    let mut out = Vec::new();
    let mut files = Vec::new();
    for d in ["/etc/nginx", "/etc/apache2", "/etc/httpd"] {
        collect_conf(Path::new(d), 0, &mut files);
    }
    for f in files {
        let Ok(t) = std::fs::read_to_string(&f) else {
            continue;
        };
        for l in t.lines() {
            let l = l.trim();
            let mut w = l.split_whitespace();
            let k = w.next().unwrap_or("").to_ascii_lowercase();
            if matches!(k.as_str(), "root" | "alias" | "documentroot") {
                if let Some(v) = w.next() {
                    let v = v.trim_matches(['"', '\'', ';']);
                    if v.starts_with('/') && v.len() > 1 {
                        out.push(v.trim_end_matches('/').to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_conf(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_conf(&p, depth + 1, out);
        } else if p.is_file() && std::fs::metadata(&p).is_ok_and(|m| m.len() < 1 << 20) {
            out.push(p);
        }
    }
}

// --------------------------------------------------------------- server

/// A6.23 server items not in cleanup::plan: old snap revisions, old
/// kernels, aged /tmp and /var/tmp.
pub fn server_suggestions(now: i64) -> Vec<Suggestion> {
    let mut out = Vec::new();
    // Snap: disabled revisions.
    if let Ok(o) = std::process::Command::new("snap")
        .args(["list", "--all"])
        .stderr(std::process::Stdio::null())
        .output()
    {
        for l in String::from_utf8_lossy(&o.stdout).lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 6 && f.last().is_some_and(|n| n.contains("disabled")) {
                let (name, rev) = (f[0], f[2]);
                let bytes = std::fs::metadata(format!("/var/lib/snapd/snaps/{name}_{rev}.snap"))
                    .map(|m| m.len())
                    .unwrap_or(0);
                let item = Item {
                    level: 3,
                    what: format!("old snap revision {name} {rev}"),
                    action: Action::Run {
                        program: "snap".into(),
                        args: vec!["remove".into(), name.into(), format!("--revision={rev}")],
                    },
                    frees: bytes,
                };
                out.push(
                    suggestion(
                        Safety::Safe,
                        item.what.clone(),
                        "A disabled (older) revision of a snap; the current one is kept.",
                        "snap revert is no longer possible to this revision.",
                        bytes,
                        Some(item),
                        None,
                    )
                    .with_category(SERVER),
                );
            }
        }
    }
    out.extend(old_kernels());
    // /tmp older than 10 days, /var/tmp older than 30 (systemd defaults).
    let mut tmp_bytes = 0u64;
    for (d, days) in [("/tmp", 10), ("/var/tmp", 30)] {
        for (p, m) in crate::cleanup::files_under_pub(Path::new(d), 4) {
            let newest = m.mtime().max(m.atime()).max(m.ctime());
            let ps = p.to_string_lossy();
            if now - newest >= days * DAY
                && !ps.contains("systemd-private-")
                && !ps.contains(".X11-unix")
                && !ps.contains("snap-private-tmp")
            {
                tmp_bytes += m.blocks() * 512;
            }
        }
    }
    if tmp_bytes >= 1 << 20 {
        let item = Item {
            level: 3,
            what: "files in /tmp older than 10 days and in /var/tmp older than 30 days".into(),
            action: Action::Run {
                program: "systemd-tmpfiles".into(),
                args: vec!["--clean".into()],
            },
            frees: tmp_bytes,
        };
        out.push(suggestion(Safety::Caution, item.what.clone(), "Removed by systemd's own age rules; files in use, sockets and service-private folders are kept.", "Not needed: they are temporary.", tmp_bytes, Some(item), None).with_category(SERVER));
    }
    out
}

/// Old kernels (Debian/Ubuntu): every installed linux-image except the
/// running one and the newest other one.
fn old_kernels() -> Vec<Suggestion> {
    let Ok(o) = std::process::Command::new("dpkg-query")
        .args(["-W", "-f", "${Package} ${Status}\n", "linux-image-[0-9]*"])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut v: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| l.ends_with("install ok installed"))
        .filter_map(|l| {
            l.split_whitespace()
                .next()?
                .strip_prefix("linux-image-")
                .map(str::to_string)
        })
        .collect();
    v.sort_by(|a, b| cmp_version(b, a));
    let mut keep = 0;
    let mut out = Vec::new();
    for ver in v {
        if ver == running {
            continue;
        }
        if keep == 0 {
            keep = 1; // the newest other kernel stays as a fallback
            continue;
        }
        let bytes = crate::cleanup::dir_bytes_pub(Path::new(&format!("/lib/modules/{ver}")))
            + std::fs::metadata(format!("/boot/vmlinuz-{ver}"))
                .map(|m| m.len())
                .unwrap_or(0)
            + std::fs::metadata(format!("/boot/initrd.img-{ver}"))
                .map(|m| m.len())
                .unwrap_or(0);
        let item = Item {
            level: 3,
            what: format!("old kernel {ver}"),
            action: Action::Run {
                program: "apt-get".into(),
                args: vec![
                    "-y".into(),
                    "purge".into(),
                    format!("linux-image-{ver}"),
                    format!("linux-modules-{ver}"),
                ],
            },
            frees: bytes,
        };
        out.push(suggestion(Safety::Caution, item.what.clone(), &format!("Not running (running: {running}); the newest other kernel is kept as a fallback."), "Reinstall it with apt if needed.", bytes, Some(item), None).with_category(SERVER));
    }
    out
}

// ---------------------------------------------------------------- pulumi

pub const PULUMI_WARNING: &str = "This folder can be the ONLY copy of your Pulumi stack state. If the stack uses the local backend (`pulumi login file://...`), deleting it makes Pulumi forget every cloud resource it created: they keep running and costing money, and Pulumi can no longer update or destroy them. Before deleting, run `pulumi whoami -v` in the project: delete only if the backend URL is Pulumi Cloud, s3://, gs:// or azblob://, never file://. Ask whoever owns this project first.";

/// `.pulumi` folders of idle projects: RISKY, typed confirmation, with the
/// full warning (owner decision 2026-11-28).
pub fn pulumi_suggestions(tree: &Tree, projects: &[crate::insight::Project]) -> Vec<Suggestion> {
    let root = tree.root_path.trim_end_matches('/');
    let mut out = Vec::new();
    for p in projects.iter().filter(|p| p.kinds.contains(&"pulumi")) {
        let dir = format!("{}/.pulumi", p.path);
        let rel = dir
            .strip_prefix(root)
            .unwrap_or(&dir)
            .trim_start_matches('/');
        if let Some(n) = tree.root.find(rel).filter(|n| n.total_bytes >= 1 << 20) {
            out.push(remove_dir(
                PROJECTS,
                Safety::Risky,
                &dir,
                n.total_bytes,
                PULUMI_WARNING,
                "None if this was the only copy of the state: restore it from your own backup.",
            ));
        }
    }
    out
}

// ---------------------------------------------------------- docker volumes

/// A6.24: anonymous volumes unused for 30+ days are offered; named unused
/// volumes are information only.
pub fn volume_suggestions(r: &crate::docker::Report, now: i64) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let anon = |n: &str| n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit());
    let mut named = (0usize, 0u64);
    for (i, v) in r.volumes.iter().enumerate() {
        if v.links > 0 || !r.users_of_volume(i).is_empty() {
            continue;
        }
        if !anon(&v.name) {
            named.0 += 1;
            named.1 += v.size;
            continue;
        }
        // An unknown age is not "old": such a volume is not offered.
        if v.created == 0 || now - v.created < 30 * DAY {
            continue;
        }
        let mut s = crate::fix::delete_volume(&v.name, v.size);
        s.title = format!(
            "unused anonymous volume {} ({} days old)",
            &v.name[..12],
            if v.created > 0 {
                (now - v.created) / DAY
            } else {
                0
            }
        );
        out.push(s.with_category("Docker volumes"));
    }
    if named.0 > 0 {
        out.push(
            suggestion(Safety::Risky, format!("{} unused named volumes ({}): information only", named.0, crate::util::human(named.1 as i64)), "Named volumes usually hold data (databases, uploads); dux never offers them. Inspect them in the Docker view.", "-", 0, None, Some("docker volume ls -f dangling=true".into()))
                .with_category("Docker volumes"),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_lists_databases_binaries_state_and_internals() {
        for p in [
            "/root/.codex/thread_history_1.sqlite",
            "/root/.local/share/rtk/history.db",
            "/swap.img",
            "/usr/bin/vault",
            "/var/lib/containerd/x/blob",
            "/var/lib/postgresql/16/main/base/1",
            "/root/.local/share/claude/versions/2.1.3",
            "/root/vst/.git/objects/pack/p.pack",
            "/srv/app/data.db-wal",
            "/home/a/.claude/projects/x.jsonl",
        ] {
            assert!(never(p), "{p}");
        }
        assert!(!never("/srv/app/logs/old.log.1"));
        assert!(
            !never("/var/lib/docker/containers/abc/abc-json.log"),
            "container logs may be truncated"
        );
        assert!(never("/var/lib/docker/containers/abc/config.v2.json"));
    }

    #[test]
    fn file_rules() {
        let r = |d: &str, n: &str| file_rule(d.as_bytes(), n.as_bytes());
        assert_eq!(r("/srv/x", "main.c~"), Some(FileRule::Junk));
        assert_eq!(r("/srv/x", "core.1234"), Some(FileRule::Junk));
        assert_eq!(r("/srv/x", "core.c"), None);
        assert_eq!(r("/srv/x", "hs_err_pid99.log"), Some(FileRule::Junk));
        assert_eq!(r("/srv/x", "site.conf.bak"), Some(FileRule::Backup));
        assert_eq!(r("/etc/nginx", "site.conf.bak"), None, "never under /etc");
        assert_eq!(
            r("/var/www/wp-content/ai1wm-backups", "site-2026.wpress"),
            Some(FileRule::CmsBackup)
        );
        assert_eq!(
            r("/root/backups", "shop-2026-01-01.sql.gz"),
            Some(FileRule::Dump)
        );
        assert_eq!(r("/root/.local/share/x", "a.bak"), None, "app state never");
        assert_eq!(r("/srv/x", "notes.db.bak"), Some(FileRule::Backup));
        assert_eq!(r("/srv/x", "data.sqlite"), None);
        assert_eq!(
            r("/root/.cargo/registry/src/i/clap-4/", "Cargo.toml.orig"),
            None,
            "part of a package"
        );
        assert_eq!(r("/srv/app/node_modules/x", "a.js.bak"), None);
        assert_eq!(
            r("/etc/apt/sources.list.d", "ubuntu.sources.orig"),
            None,
            "never in /etc"
        );
    }

    #[test]
    fn newest_of_a_series_is_kept_and_young_files_wait() {
        let now = 1_000 * DAY;
        let f = |p: &str, age: i64| FoundFile {
            path: p.into(),
            bytes: 5 << 20,
            apparent: 5 << 20,
            mtime: now - age * DAY,
            uid: 0,
            dev: 0,
            ino: p.len() as u64 + age as u64,
        };
        let found = vec![
            f("/root/dumps/shop-2026-01-01.sql.gz", 90),
            f("/root/dumps/shop-2026-02-01.sql.gz", 60),
            f("/root/dumps/shop-2026-03-01.sql.gz", 2),
            f("/root/dumps/blog-2026-01-01.sql.gz", 90),
            f("/srv/x/a.c~", 8),
            f("/srv/x/b.c~", 2),
            f("/var/www/html/dump.sql", 90),
        ];
        let s = file_suggestions(&found, now, &["/var/www/html".into()]);
        let t: Vec<&str> = s.iter().map(|x| x.title.as_str()).collect();
        assert!(
            t.iter().any(|x| x.contains("shop-2026-01-01"))
                && t.iter().any(|x| x.contains("shop-2026-02-01")),
            "{t:?}"
        );
        assert!(
            !t.iter().any(|x| x.contains("shop-2026-03-01")),
            "newest kept: {t:?}"
        );
        assert!(
            !t.iter().any(|x| x.contains("blog-")),
            "the only dump of a series: {t:?}"
        );
        assert!(
            t.iter().any(|x| x.contains("a.c~")) && !t.iter().any(|x| x.contains("b.c~")),
            "{t:?}"
        );
        let web = s
            .iter()
            .find(|x| x.title.contains("/var/www/html/dump.sql"))
            .unwrap();
        assert!(
            web.item.is_none(),
            "a dump in a web root is flagged, never deleted"
        );
    }

    #[test]
    fn cms_caches_need_their_app_and_hints_are_quoted() {
        let d = std::env::temp_dir().join(format!("dux-cms-{}", std::process::id()));
        let wp = d.join("site");
        std::fs::create_dir_all(wp.join("wp-content/cache")).unwrap();
        let c = wp.join("wp-content/cache").to_string_lossy().into_owned();
        assert!(!cms_cache_dir(&c), "no wp-config.php: not WordPress");
        std::fs::write(wp.join("wp-config.php"), b"").unwrap();
        assert!(cms_cache_dir(&c));
        assert!(
            !cms_cache_dir(&format!("{}/any/var/cache", d.display())),
            "no Symfony/Magento marker"
        );
        std::fs::create_dir_all(d.join("dr/core")).unwrap();
        assert!(cms_cache_dir(&format!(
            "{}/dr/sites/default/files/css",
            d.display()
        )));
        assert!(!cms_cache_dir(&format!(
            "{}/x/sites/default/files/css",
            d.display()
        )));
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn a_series_is_one_owners_files() {
        let now = 1_000 * DAY;
        let f = |p: &str, age: i64, uid: u32| FoundFile {
            path: p.into(),
            bytes: 5 << 20,
            apparent: 5 << 20,
            mtime: now - age * DAY,
            uid,
            dev: 0,
            ino: age as u64 + uid as u64,
        };
        // Someone else plants a "newer" dump beside a user's only dump.
        let found = vec![
            f("/srv/share/db-2026-01.sql", 90, 1000),
            f("/srv/share/db-2026-09.sql", 1, 1001),
        ];
        assert!(file_suggestions(&found, now, &[]).is_empty());
        // A future time never makes another file look old.
        let found = vec![
            f("/srv/share/db-2026-01.sql", 90, 1000),
            f("/srv/share/db-2026-09.sql", -400, 1000),
        ];
        assert!(file_suggestions(&found, now, &[]).is_empty());
    }

    #[test]
    fn pulumi_state_is_risky_with_its_warning_never_build_output() {
        let d = std::env::temp_dir().join(format!("dux-pul-{}", std::process::id()));
        let p = d.join("infra");
        std::fs::create_dir_all(p.join(".pulumi/stacks")).unwrap();
        std::fs::write(p.join("Pulumi.yaml"), b"name: x").unwrap();
        std::fs::write(p.join(".pulumi/stacks/dev.json"), vec![1u8; 2 << 20]).unwrap();
        let t = crate::walker::walk(d.to_str().unwrap(), 1).unwrap();
        let projects = crate::insight::projects(&t);
        assert!(
            projects[0]
                .artifact_dirs
                .iter()
                .all(|(x, _)| !x.ends_with(".pulumi")),
            "never plain build output"
        );
        let s = pulumi_suggestions(&t, &projects);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].safety, Safety::Risky);
        assert!(s[0].why.contains("ONLY copy") && s[0].why.contains("pulumi whoami -v"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn old_browser_builds_keep_the_newest() {
        let d = std::env::temp_dir().join(format!("dux-cat-{}", std::process::id()));
        let pw = d.join(".cache/ms-playwright");
        for n in ["chromium-1208", "chromium-1223", "firefox-1490"] {
            std::fs::create_dir_all(pw.join(n)).unwrap();
        }
        let p = pw.to_string_lossy();
        assert!(old_browser_dir(&p, "chromium-1208"));
        assert!(!old_browser_dir(&p, "chromium-1223"));
        assert!(!old_browser_dir(&p, "firefox-1490"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
