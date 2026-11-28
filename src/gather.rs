//! One shared snapshot of everything dux knows about a host, for `dux ui`
//! and `dux web`: disks, the folder tree, logs, biggest files, projects,
//! apps, Docker, and one combined, rated fix list.

use crate::cleanup::{self, Action, Item};
use crate::fix::{self, Safety, Suggestion};
use crate::insight::{self, LogFile, Project};
use crate::walker::{self, FileView, FoundFile, Tree, WalkOptions};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Disk {
    pub path: String,
    pub fstype: String,
    pub usage: crate::sentinel::Usage,
}

#[derive(Default)]
pub struct Gathered {
    pub at: i64,
    pub disks: Vec<Disk>,
    pub tree: Option<Tree>,
    pub logs: Vec<LogFile>,
    pub journal_bytes: Option<u64>,
    pub biggest: Vec<FoundFile>,
    pub projects: Vec<Project>,
    pub apps: Vec<(String, String, u64, i64)>,
    pub docker: Option<crate::docker::Report>,
    pub fixes: Vec<Suggestion>,
    pub services: Vec<ServiceUse>,
    /// Files the cleanup catalog matched by name (leftovers, backups, dumps).
    pub leftovers: Vec<FoundFile>,
    pub errors: Vec<String>,
}

/// Disk use of one running systemd service: its data folders and logs.
#[derive(Clone, Debug, Default)]
pub struct ServiceUse {
    pub name: String,
    pub folders: Vec<(String, u64)>,
    pub logs: u64,
    pub total: u64,
}

fn running_services() -> Vec<String> {
    let Ok(out) = std::process::Command::new("systemctl")
        .args([
            "list-units",
            "--type=service",
            "--state=running",
            "--no-legend",
            "--plain",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| u.ends_with(".service"))
        .map(str::to_string)
        .collect()
}

/// Where a service's data usually lives, by its name ("mysqld" -> mysql).
fn service_bases(unit: &str) -> Vec<String> {
    let base = unit.trim_end_matches(".service");
    let base = base.split('@').next().unwrap_or(base);
    let mut v = vec![base.to_string()];
    for suffix in ["d", "-server", "-agent", "-daemon"] {
        if let Some(b) = base.strip_suffix(suffix).filter(|b| b.len() >= 3) {
            v.push(b.to_string());
        }
    }
    v
}

pub fn services(units: &[String], tree: &Tree, logs: &[LogFile]) -> Vec<ServiceUse> {
    let root = tree.root_path.trim_end_matches('/');
    let mut out = Vec::new();
    for unit in units {
        let bases = service_bases(unit);
        let mut s = ServiceUse {
            name: unit.trim_end_matches(".service").to_string(),
            ..ServiceUse::default()
        };
        for b in &bases {
            for dir in ["/var/lib", "/var/cache", "/srv", "/opt"] {
                let p = format!("{dir}/{b}");
                if s.folders.iter().any(|(q, _)| *q == p) {
                    continue;
                }
                let Some(rel) = p.strip_prefix(root).map(|r| r.trim_start_matches('/')) else {
                    continue;
                };
                if let Some(n) = tree.root.find(rel).filter(|n| n.total_bytes > 0) {
                    s.folders.push((p, n.total_bytes));
                }
            }
        }
        s.logs = logs
            .iter()
            .filter(|l| {
                l.service.as_deref() == Some(unit.as_str())
                    || bases
                        .iter()
                        .any(|b| l.path.starts_with(&format!("/var/log/{b}/")))
            })
            .map(|l| l.bytes)
            .sum();
        s.total = s.folders.iter().map(|(_, b)| b).sum::<u64>() + s.logs;
        if s.total >= 1 << 20 {
            out.push(s);
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.total));
    out
}

pub fn disks() -> Vec<Disk> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    crate::sentinel::parse_mountinfo(&text)
        .into_iter()
        .filter_map(|m| {
            crate::sentinel::statvfs(&m.path).ok().map(|u| Disk {
                path: m.path,
                fstype: m.fstype,
                usage: u,
            })
        })
        .collect()
}

/// Rated fixes for logs: rotated copies (SAFE), big active text logs (CAUTION),
/// an oversized journal (SAFE vacuum). Binary and protected logs are skipped.
pub fn log_fixes(logs: &[LogFile], journal: Option<u64>) -> Vec<Suggestion> {
    let mut s = Vec::new();
    if let Some(j) = journal.filter(|j| *j > 1 << 30) {
        s.push(fix::assess(Item {
            level: 1,
            what: format!(
                "shrink the systemd journal from {} to 1 GiB",
                crate::util::human(j as i64)
            ),
            action: Action::Run {
                program: "journalctl".into(),
                args: vec!["--vacuum-size=1G".into()],
            },
            frees: j - (1 << 30),
        }));
    }
    for l in logs
        .iter()
        .filter(|l| !l.protected && !cleanup::is_binary_log(std::path::Path::new(&l.path)))
    {
        if l.rotated && l.writer.is_none() {
            let item = Item {
                level: 2,
                what: format!("remove rotated log {}", l.path),
                action: Action::Remove {
                    path: l.path.clone().into(),
                },
                frees: l.bytes,
            };
            s.push(fix::suggestion(
                Safety::Safe,
                item.what.clone(),
                "An old rotated copy; the active log is untouched.",
                "Not needed unless investigating that period.",
                l.bytes,
                Some(item),
                None,
            ));
        } else if !l.rotated && l.bytes >= 50 << 20 {
            s.push(fix::assess(Item {
                level: 2,
                what: format!("truncate {} (keep its last 10 MiB)", l.path),
                action: Action::TailAndTruncate {
                    path: l.path.clone().into(),
                    keep: 10 << 20,
                },
                frees: l.bytes.saturating_sub(10 << 20),
            }));
        }
    }
    s
}

/// Gather everything. `progress` counts walked entries for display.
pub fn gather(root: &str, threads: usize, progress: Option<Arc<AtomicU64>>) -> Gathered {
    let mut g = Gathered {
        at: crate::util::now_secs(),
        disks: disks(),
        ..Gathered::default()
    };
    let opts = WalkOptions {
        filter: Some(Box::new(|f: &FileView| {
            (f.bytes >= 10 << 20 && insight::is_log(f.dir, f.name))
                || f.bytes >= 50 << 20
                || crate::catalog::file_rule(f.dir, f.name).is_some()
        })),
        max_found: 300_000,
        progress,
    };
    match walker::walk_with(root, threads, opts) {
        Ok((tree, found)) => {
            let (leftovers, found): (Vec<FoundFile>, Vec<FoundFile>) =
                found.into_iter().partition(|f| {
                    let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
                    crate::catalog::file_rule(dir.as_bytes(), name.as_bytes()).is_some()
                });
            g.leftovers = leftovers;
            let (logs, big): (Vec<FoundFile>, Vec<FoundFile>) = found.into_iter().partition(|f| {
                let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
                f.bytes >= 10 << 20 && insight::is_log(dir.as_bytes(), name.as_bytes())
            });
            g.logs = insight::logs_from_found(logs);
            g.journal_bytes = (root == "/").then(cleanup::journal_usage).flatten();
            let mut big = big;
            big.sort_by_key(|f| std::cmp::Reverse(f.bytes));
            big.truncate(50);
            g.biggest = big;
            g.projects = insight::projects(&tree);
            g.apps = insight::apps(&tree);
            g.services = services(&running_services(), &tree, &g.logs);
            g.tree = Some(tree);
        }
        Err(e) => g.errors.push(format!("walk {root}: {e:#}")),
    }
    match crate::docker::collect() {
        Ok(r) => g.docker = Some(r),
        Err(e) => g.errors.push(format!("docker: {e:#}")),
    }
    // One combined fix list.
    let used = g
        .disks
        .iter()
        .find(|d| d.path == "/")
        .map(|d| d.usage.used_pct())
        .unwrap_or(0.0);
    let mut fixes: Vec<Suggestion> = cleanup::plan(&cleanup::Options {
        level: 3,
        used_pct: used,
        ..cleanup::Options::default()
    })
    .into_iter()
    .filter(|i| {
        !matches!(
            i.action,
            Action::Remove { .. } | Action::TailAndTruncate { .. }
        )
    })
    .map(fix::assess)
    .collect();
    let has_journal_fix = fixes.iter().any(|f| f.title.contains("journal"));
    fixes.extend(
        log_fixes(&g.logs, g.journal_bytes)
            .into_iter()
            .filter(|s| !(has_journal_fix && s.title.starts_with("shrink the systemd journal"))),
    );
    if let Some(r) = &g.docker {
        fixes.extend(
            crate::docker::plan(r, true, true, 100 << 20)
                .into_iter()
                .filter(|i| !matches!(i.action, Action::TailAndTruncate { .. }))
                .map(fix::assess),
        );
        fixes.extend(crate::catalog::volume_suggestions(r, g.at));
    }
    // Project build output, only in projects nothing uses (spec A6.19).
    let in_use = crate::catalog::InUse::collect(g.docker.as_ref());
    let idle: Vec<insight::Project> = g
        .projects
        .iter()
        .filter(|p| !in_use.uses(&p.path))
        .cloned()
        .collect();
    fixes.extend(
        insight::pp_plan(&idle, 10, g.at)
            .into_iter()
            .map(|i| fix::assess(i).with_category(crate::catalog::PROJECTS)),
    );
    let idle10: Vec<insight::Project> = idle
        .iter()
        .filter(|p| g.at - p.last_activity >= 10 * 86_400)
        .cloned()
        .collect();
    if let Some(t) = &g.tree {
        fixes.extend(crate::catalog::pulumi_suggestions(t, &idle10));
    }
    // The catalog: leftovers, old backups, web app caches, tool caches, CI
    // folders, server items (spec A6.19-A6.23).
    fixes.extend(crate::catalog::file_suggestions(
        &g.leftovers,
        g.at,
        &in_use.web_roots,
    ));
    if let Some(t) = &g.tree {
        fixes.extend(crate::catalog::folder_suggestions(t, g.at));
        fixes.extend(crate::catalog::tool_suggestions(t));
    }
    if root == "/" {
        fixes.extend(crate::catalog::server_suggestions(g.at));
    }
    // Big files are information (Disk usage, `dux biggest`), never a delete
    // suggestion: being big says nothing about being deletable.
    let mut held: std::collections::BTreeMap<(i32, String), i64> = Default::default();
    for d in crate::deleted::deleted_open().unwrap_or_default() {
        *held.entry((d.pid, d.process)).or_default() += d.size;
    }
    for ((pid, name), size) in held.into_iter().filter(|(_, s)| *s >= 100 << 20) {
        fixes.push(fix::risky(
            format!("deleted files still held open by pid {pid} ({name})"),
            "Already deleted; the space returns when this process closes them.",
            format!("restart the service of pid {pid} at a quiet time"),
            size as u64,
        ));
    }
    // The same action can come from two planners (e.g. the Docker build-cache
    // prune from both the cleanup plan and the Docker plan): keep one.
    let mut seen = std::collections::HashSet::new();
    fixes.retain(|f| match &f.item {
        Some(i) => seen.insert(format!("{:?}", i.action)),
        None => true,
    });
    // An action that frees nothing measurable is noise, not a suggestion.
    fixes.retain(|x| x.item.is_none() || x.frees > 0);
    // Final guard (spec A6.18): nothing in a never-listed place, whatever
    // produced it.
    fixes.retain(|x| crate::fix::target_path(x).is_none_or(|p| !crate::catalog::never(&p)));
    // Never-touch fixes stay listed (marked by the views), so protecting
    // one can be undone where it was done.
    fixes.sort_by(|a, b| a.safety.cmp(&b.safety).then(b.frees.cmp(&a.frees)));
    g.fixes = fixes;
    g
}

// ------------------------------------------------------------ result cache

/// The last scan's results, so `dux` opens with every view filled at once
/// (owner decision: instant start). Read-only until the fresh scan: cached
/// fixes carry no action.
fn cache_path() -> std::path::PathBuf {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return std::path::PathBuf::from("/var/lib/dux/ui-cache.json");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::Path::new(&home).join(".cache/dux/ui-cache.json")
}

fn leak(s: &str) -> &'static str {
    // A handful of short labels (kinds, categories) per load.
    Box::leak(s.to_string().into_boxed_str())
}

pub fn save_cache(root: &str, g: &Gathered) {
    use serde_json::json;
    let v = json!({
        "root": root, "at": g.at, "journal_bytes": g.journal_bytes,
        "logs": g.logs.iter().map(|l| json!([l.path, l.bytes, l.mtime, l.kind.label(), l.rotated, l.protected, l.writer, l.service, l.container])).collect::<Vec<_>>(),
        "projects": g.projects.iter().map(|p| json!({"path": p.path, "kinds": p.kinds, "total": p.total, "artifacts": p.artifacts, "dirs": p.artifact_dirs, "git": p.git, "last": p.last_activity, "name": p.name, "desc": p.description, "branch": p.branch, "remote": p.remote})).collect::<Vec<_>>(),
        "apps": g.apps,
        "services": g.services.iter().map(|s| json!([s.name, s.folders, s.logs, s.total])).collect::<Vec<_>>(),
        "fixes": g.fixes.iter().map(|x| json!([x.safety.label(), x.title, x.why, x.undo, x.frees, x.manual, fix::group(x)])).collect::<Vec<_>>(),
        "biggest": g.biggest.iter().map(|f| json!([f.path, f.bytes, f.apparent, f.mtime, f.uid])).collect::<Vec<_>>(),
    });
    let p = cache_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.new");
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(&tmp);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
    {
        if f.write_all(v.to_string().as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// The cached results for `root`, if any (at most 7 days old).
pub fn load_cache(root: &str) -> Option<Gathered> {
    let t = std::fs::read_to_string(cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    if v["root"].as_str() != Some(root) {
        return None;
    }
    let at = v["at"].as_i64()?;
    if crate::util::now_secs() - at > 7 * 86_400 {
        return None;
    }
    let s = |x: &serde_json::Value| x.as_str().unwrap_or("").to_string();
    let os = |x: &serde_json::Value| x.as_str().map(str::to_string);
    let u = |x: &serde_json::Value| x.as_u64().unwrap_or(0);
    let mut g = Gathered {
        at,
        journal_bytes: v["journal_bytes"].as_u64(),
        ..Gathered::default()
    };
    for l in v["logs"].as_array()? {
        g.logs.push(LogFile {
            path: s(&l[0]),
            bytes: u(&l[1]),
            mtime: l[2].as_i64().unwrap_or(0),
            kind: match l[3].as_str() {
                Some("docker") => insight::LogKind::Docker,
                Some("system") => insight::LogKind::System,
                _ => insight::LogKind::App,
            },
            rotated: l[4].as_bool().unwrap_or(false),
            protected: l[5].as_bool().unwrap_or(true),
            per_min: None,
            writer: os(&l[6]),
            service: os(&l[7]),
            container: os(&l[8]),
        });
    }
    for p in v["projects"].as_array()? {
        g.projects.push(Project {
            path: s(&p["path"]),
            kinds: p["kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str())
                .map(leak)
                .collect(),
            total: u(&p["total"]),
            artifacts: u(&p["artifacts"]),
            artifact_dirs: p["dirs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| (s(&d[0]), u(&d[1])))
                .collect(),
            git: u(&p["git"]),
            last_activity: p["last"].as_i64().unwrap_or(0),
            name: s(&p["name"]),
            description: s(&p["desc"]),
            branch: os(&p["branch"]),
            remote: os(&p["remote"]),
        });
    }
    for a in v["apps"].as_array().into_iter().flatten() {
        g.apps
            .push((s(&a[0]), s(&a[1]), u(&a[2]), a[3].as_i64().unwrap_or(0)));
    }
    for x in v["services"].as_array().into_iter().flatten() {
        g.services.push(ServiceUse {
            name: s(&x[0]),
            folders: x[1]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| (s(&f[0]), u(&f[1])))
                .collect(),
            logs: u(&x[2]),
            total: u(&x[3]),
        });
    }
    for x in v["fixes"].as_array().into_iter().flatten() {
        let safety = match x[0].as_str() {
            Some("SAFE") => Safety::Safe,
            Some("CAUTION") => Safety::Caution,
            _ => Safety::Risky,
        };
        // No action: a cached suggestion is shown, never applied.
        g.fixes.push(
            fix::suggestion(
                safety,
                s(&x[1]),
                x[2].as_str().unwrap_or(""),
                x[3].as_str().unwrap_or(""),
                u(&x[4]),
                None,
                os(&x[5]),
            )
            .with_category(leak(x[6].as_str().unwrap_or("Other"))),
        );
    }
    for f in v["biggest"].as_array().into_iter().flatten() {
        g.biggest.push(FoundFile {
            path: s(&f[0]),
            bytes: u(&f[1]),
            apparent: u(&f[2]),
            mtime: f[3].as_i64().unwrap_or(0),
            uid: f[4].as_u64().unwrap_or(0) as u32,
            dev: 0,
            ino: 0,
        });
    }
    Some(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_a_small_tree() {
        let d = std::env::temp_dir().join(format!("dux-gather-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("p/node_modules/x")).unwrap();
        std::fs::write(d.join("p/package.json"), b"{}").unwrap();
        std::fs::write(d.join("p/node_modules/x/a.js"), vec![1u8; 2 << 20]).unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("logs/app.log.1"), vec![1u8; 11 << 20]).unwrap();
        // Under the 10 MiB floor (spec A7.1): not listed.
        std::fs::write(d.join("logs/small.log"), vec![1u8; 2 << 20]).unwrap();
        let g = gather(d.to_str().unwrap(), 2, Some(Arc::new(AtomicU64::new(0))));
        assert!(g.tree.is_some());
        assert_eq!(g.logs.len(), 1);
        assert_eq!(g.projects.len(), 1);
        assert!(g.fixes.iter().any(|f| f.title.contains("rotated log")));
        // The project was just created: in use, so its build output is not offered.
        assert!(!g.fixes.iter().any(|f| f.title.contains("node_modules")));
        // Big files are never a delete suggestion.
        assert!(!g.fixes.iter().any(|f| f.title.starts_with("delete ")));
        assert!(!g.disks.is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
