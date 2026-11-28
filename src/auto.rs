//! Automatic cleanup (spec section 4.5, AUTO-1..10).
//!
//! Off unless the user turned it on (`dux setup` or `[auto]` in dux.toml),
//! approved the folders it may touch and each kind of action. When a disk
//! reaches the start level, dux runs the approved actions on that disk only,
//! safest first, until the stop level (if set) or until nothing approved is
//! left. Project build output is announced 24 hours before it is deleted.
//! Every action is logged and alerted; logs it emptied stay watched.

use crate::cleanup::{self, Action, Item};
use crate::notify::{Alert, NotifyConfig, Severity};
use crate::util::human;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const NOTICE_SECS: i64 = 24 * 3600;
/// Logs dux emptied are watched this long for fast regrowth (AUTO-9).
pub const WATCH_SECS: i64 = 7 * 86_400;

#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub enabled: bool,
    pub start_pct: f64,
    /// 0 = no stop level: free as much as the approved actions allow.
    pub stop_pct: f64,
    pub folders: Vec<String>,
    pub rotated_logs: bool,
    pub journal: bool,
    pub truncate_logs: bool,
    pub build_caches: bool,
    pub docker_images: bool,
    pub docker_volumes: bool,
    /// Projects idle this many days have their build output removed (A8.4).
    pub idle_days: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            enabled: false,
            start_pct: 90.0,
            stop_pct: 0.0,
            folders: Vec::new(),
            rotated_logs: false,
            journal: false,
            truncate_logs: false,
            build_caches: false,
            docker_images: false,
            docker_volumes: false,
            idle_days: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Journal,
    RotatedLogs,
    TruncateLogs,
    BuildCaches,
    DockerImages,
    DockerVolumes,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Journal => "shrink the journal",
            Kind::RotatedLogs => "remove rotated logs",
            Kind::TruncateLogs => "truncate big active logs",
            Kind::BuildCaches => "remove project build output",
            Kind::DockerImages => "remove unused Docker images",
            Kind::DockerVolumes => "remove unused Docker volumes",
        }
    }
}

impl Policy {
    pub fn from_raw(get: &dyn Fn(&str) -> Option<String>) -> Result<Policy> {
        let mut p = Policy::default();
        let yes = |k: &str| -> bool {
            get(k).is_some_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "1" | "yes" | "true" | "on")
            })
        };
        let pct = |k: &str, d: f64| -> Result<f64> {
            match get(k) {
                Some(v) => {
                    let x: f64 = v
                        .trim_end_matches('%')
                        .parse()
                        .with_context(|| format!("{k}={v}"))?;
                    anyhow::ensure!((0.0..=100.0).contains(&x), "{k} must be 0-100");
                    Ok(x)
                }
                None => Ok(d),
            }
        };
        p.enabled = yes("DUX_AUTO");
        p.start_pct = pct("DUX_AUTO_START_PCT", p.start_pct)?;
        p.stop_pct = pct("DUX_AUTO_STOP_PCT", p.stop_pct)?;
        p.folders = get("DUX_AUTO_FOLDERS")
            .map(|v| {
                v.split(',')
                    .map(|s| crate::protect::normalize(s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        p.rotated_logs = yes("DUX_AUTO_ROTATED_LOGS");
        p.journal = yes("DUX_AUTO_JOURNAL");
        p.truncate_logs = yes("DUX_AUTO_TRUNCATE_LOGS");
        p.build_caches = yes("DUX_AUTO_BUILD_CACHES");
        p.docker_images = yes("DUX_AUTO_DOCKER_IMAGES");
        p.docker_volumes = yes("DUX_AUTO_DOCKER_VOLUMES");
        for f in &p.folders {
            anyhow::ensure!(
                f.starts_with('/'),
                "DUX_AUTO_FOLDERS: {f} is not a full path"
            );
            anyhow::ensure!(
                f != "/",
                "DUX_AUTO_FOLDERS: / cannot be approved as a whole; name the folders"
            );
        }
        if p.stop_pct > 0.0 {
            anyhow::ensure!(
                p.stop_pct < p.start_pct,
                "DUX_AUTO_STOP_PCT must be below DUX_AUTO_START_PCT"
            );
        }
        if p.enabled {
            let any_file_action = p.rotated_logs || p.journal || p.truncate_logs || p.build_caches;
            anyhow::ensure!(
                any_file_action || p.docker_images || p.docker_volumes,
                "DUX_AUTO is on but no action is approved (DUX_AUTO_ROTATED_LOGS, DUX_AUTO_JOURNAL, ...)"
            );
            anyhow::ensure!(
                !any_file_action || !p.folders.is_empty(),
                "DUX_AUTO is on but DUX_AUTO_FOLDERS approves no folder"
            );
        }
        Ok(p)
    }

    pub fn allows(&self, k: Kind) -> bool {
        match k {
            Kind::Journal => self.journal,
            Kind::RotatedLogs => self.rotated_logs,
            Kind::TruncateLogs => self.truncate_logs,
            Kind::BuildCaches => self.build_caches,
            Kind::DockerImages => self.docker_images,
            Kind::DockerVolumes => self.docker_volumes,
        }
    }

    /// Is `p` inside an approved folder?
    pub fn approved(&self, p: &Path) -> bool {
        let s = crate::protect::normalize(&p.to_string_lossy());
        self.folders
            .iter()
            .any(|f| s == *f || s.starts_with(&format!("{}/", f.trim_end_matches('/'))))
    }
}

/// Which approved kind an action is, if any. Anything else never runs here.
pub fn kind_of(item: &Item) -> Option<Kind> {
    match &item.action {
        Action::Run { program, args } => {
            let name = Path::new(program)
                .file_name()?
                .to_string_lossy()
                .into_owned();
            let a = args.join(" ");
            match name.as_str() {
                "journalctl" if a.contains("--vacuum") => Some(Kind::Journal),
                "logrotate" => Some(Kind::TruncateLogs),
                "docker" | "podman" if a.starts_with("image prune") => Some(Kind::DockerImages),
                "docker" | "podman" if a.starts_with("volume rm ") => Some(Kind::DockerVolumes),
                _ => None,
            }
        }
        Action::TailAndTruncate { .. } => Some(Kind::TruncateLogs),
        Action::Remove { path } => {
            let name = path.file_name()?.to_string_lossy().into_owned();
            cleanup::rotated_base(&name).map(|_| Kind::RotatedLogs)
        }
        Action::RemoveTree { .. } => Some(Kind::BuildCaches),
    }
}

/// Every file target inside an approved folder (Docker kinds are approved by
/// kind, not folder). Never-touch and `actions off` are checked again by
/// `cleanup::apply` at run time.
pub fn permitted(p: &Policy, item: &Item) -> Result<Kind> {
    let Some(k) = kind_of(item) else {
        bail!("not an action automatic cleanup may take");
    };
    if !p.allows(k) {
        bail!("{} is not approved", k.label());
    }
    if matches!(k, Kind::DockerImages | Kind::DockerVolumes) {
        return Ok(k);
    }
    // The journal also lives in /run (memory); its disk copy is what counts.
    if k == Kind::Journal {
        if !p.approved(Path::new("/var/log/journal")) {
            bail!("/var/log/journal is outside the approved folders");
        }
        return Ok(k);
    }
    let Some(t) = cleanup::targets(item) else {
        bail!("unknown reach");
    };
    for path in &t {
        if !p.approved(path) {
            bail!("{} is outside the approved folders", path.display());
        }
    }
    Ok(k)
}

// ------------------------------------------------------------------ state

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// Build-output folders announced for deletion: path -> when.
    pub announced: HashMap<String, i64>,
    /// Folders the user chose to keep (`dux auto keep`).
    pub keep: Vec<String>,
    /// Logs dux emptied: path -> (size after, when, bytes freed).
    pub watched: HashMap<String, (u64, i64, u64)>,
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("auto-state.json")
}

impl State {
    pub fn load(dir: &Path) -> State {
        let Ok(t) = std::fs::read_to_string(state_path(dir)) else {
            return State::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
            return State::default();
        };
        let mut s = State::default();
        if let Some(m) = v["announced"].as_object() {
            for (k, t) in m {
                if let Some(t) = t.as_i64() {
                    s.announced.insert(k.clone(), t);
                }
            }
        }
        if let Some(a) = v["keep"].as_array() {
            s.keep = a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
        if let Some(m) = v["watched"].as_object() {
            for (k, w) in m {
                if let (Some(a), Some(b), Some(c)) = (w[0].as_u64(), w[1].as_i64(), w[2].as_u64()) {
                    s.watched.insert(k.clone(), (a, b, c));
                }
            }
        }
        s
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let v = json!({
            "announced": self.announced,
            "keep": self.keep,
            "watched": self.watched.iter().map(|(k, (a, b, c))| (k.clone(), json!([a, b, c]))).collect::<serde_json::Map<_, _>>(),
        });
        let tmp = state_path(dir).with_extension("json.new");
        std::fs::write(&tmp, v.to_string())?;
        std::fs::rename(&tmp, state_path(dir))?;
        Ok(())
    }

    pub fn kept(&self, p: &str) -> bool {
        self.keep
            .iter()
            .any(|k| p == k || p.starts_with(&format!("{k}/")))
    }
}

// ------------------------------------------------------------------- plan

fn same_fs(a: &Path, dev: u64) -> bool {
    let mut p = a.to_path_buf();
    loop {
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            return m.dev() == dev;
        }
        if !p.pop() {
            return false;
        }
    }
}

/// Everything approved that could free space on the filesystem of `mount`,
/// safest first. Build output comes back separately (it needs a notice).
pub fn candidates(p: &Policy, mount: &str, used_pct: f64) -> (Vec<Item>, Vec<Item>) {
    let Ok(dev) = std::fs::metadata(mount).map(|m| m.dev()) else {
        return (Vec::new(), Vec::new());
    };
    let on_disk = |i: &Item| {
        cleanup::targets(i).is_some_and(|t| t.iter().all(|x| same_fs(x, dev)))
            || matches!(kind_of(i), Some(Kind::DockerImages | Kind::DockerVolumes))
                && same_fs(Path::new("/var/lib/docker"), dev)
    };
    let o = cleanup::Options {
        level: 2,
        used_pct,
        // In automatic mode the start level is the user's choice.
        logs_min_used_pct: 0.0,
        ..cleanup::Options::default()
    };
    let mut items: Vec<Item> = Vec::new();
    // The journal and /var/log (and container logs), when approved.
    items.extend(cleanup::plan(&o));
    // Logs in every other approved folder.
    let open = cleanup::open_now();
    for f in &p.folders {
        if f == "/var/log" || f.starts_with("/var/log/") {
            continue;
        }
        items.extend(cleanup::log_items(&o, Path::new(f), None, &open));
    }
    if p.docker_images {
        items.push(Item {
            level: 3,
            what: "unused Docker images older than 30 days".into(),
            action: Action::Run {
                program: "docker".into(),
                args: ["image", "prune", "-a", "-f", "--filter", "until=720h"]
                    .map(String::from)
                    .to_vec(),
            },
            frees: 0,
        });
    }
    if p.docker_volumes {
        if let Ok(r) = crate::docker::collect() {
            for v in r.unused_volumes() {
                let program = if v.runtime == "podman" {
                    "podman"
                } else {
                    "docker"
                };
                items.push(Item {
                    level: 3,
                    what: format!("unused {program} volume {}", v.name),
                    action: Action::Run {
                        program: program.into(),
                        args: vec!["volume".into(), "rm".into(), v.name.clone()],
                    },
                    frees: v.size,
                });
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| {
        permitted(p, i).is_ok() && on_disk(i) && seen.insert(format!("{:?}", i.action))
    });
    let order = |i: &Item| match kind_of(i) {
        Some(Kind::Journal) => 0,
        Some(Kind::RotatedLogs) => 1,
        Some(Kind::TruncateLogs) => 2,
        Some(Kind::DockerImages) => 4,
        Some(Kind::DockerVolumes) => 5,
        _ => 9,
    };
    items.sort_by_key(|i| (order(i), std::cmp::Reverse(i.frees)));
    let mut build = Vec::new();
    if p.build_caches {
        let now = crate::util::now_secs();
        for f in &p.folders {
            if !same_fs(Path::new(f), dev) {
                continue;
            }
            if let Ok(tree) = crate::walker::walk(f, 2) {
                let projects = crate::insight::projects(&tree);
                for i in crate::insight::pp_plan(&projects, p.idle_days, now) {
                    if permitted(p, &i).is_ok() && on_disk(&i) {
                        build.push(i);
                    }
                }
            }
        }
    }
    (items, build)
}

// -------------------------------------------------------------------- run

pub struct Outcome {
    pub done: Vec<(String, u64)>,
    pub announced: Vec<(String, u64)>,
    pub skipped: Vec<(String, String)>,
    pub freed: u64,
    pub used_after: f64,
}

/// Run the approved actions on `mount`. `apply` is `cleanup::apply` in
/// production; tests pass their own.
pub fn run(
    p: &Policy,
    mount: &str,
    state_dir: &Path,
    now: i64,
    apply: &mut dyn FnMut(&Item) -> Result<()>,
    used: &mut dyn FnMut() -> Option<(f64, u64)>,
) -> Outcome {
    let mut st = State::load(state_dir);
    let (before_pct, before_used) = used().unwrap_or((0.0, 0));
    let (items, build) = candidates(p, mount, before_pct);
    let mut out = Outcome {
        done: Vec::new(),
        announced: Vec::new(),
        skipped: Vec::new(),
        freed: 0,
        used_after: before_pct,
    };
    let reached_stop = |used: &mut dyn FnMut() -> Option<(f64, u64)>| {
        p.stop_pct > 0.0 && used().is_some_and(|(pct, _)| pct < p.stop_pct)
    };
    let mut run_one = |i: &Item, out: &mut Outcome, st: &mut State| {
        let size_before = match &i.action {
            Action::TailAndTruncate { path, .. } => std::fs::metadata(path).map(|m| m.len()).ok(),
            _ => None,
        };
        match apply(i) {
            Ok(()) => {
                tracing::info!(what = %i.what, frees = i.frees, "automatic cleanup");
                out.done.push((i.what.clone(), i.frees));
                if let Action::TailAndTruncate { path, .. } = &i.action {
                    let after = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    let freed = size_before.unwrap_or(0).saturating_sub(after);
                    st.watched
                        .insert(path.to_string_lossy().into_owned(), (after, now, freed));
                }
            }
            Err(e) => out.skipped.push((i.what.clone(), format!("{e:#}"))),
        }
    };
    for i in &items {
        if reached_stop(used) {
            break;
        }
        run_one(i, &mut out, &mut st);
    }
    // Build output: announced first, deleted only 24 h later (AUTO-10).
    let current: Vec<String> = build
        .iter()
        .filter_map(|i| match &i.action {
            Action::RemoveTree { path } => Some(path.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    st.announced.retain(|k, _| current.contains(k));
    for (i, path) in build.iter().zip(&current) {
        if st.kept(path) {
            continue;
        }
        match st.announced.get(path) {
            Some(t) if now - t >= NOTICE_SECS => {
                if reached_stop(used) {
                    break;
                }
                run_one(i, &mut out, &mut st);
                st.announced.remove(path);
            }
            Some(_) => {}
            None => {
                st.announced.insert(path.clone(), now);
                out.announced.push((path.clone(), i.frees));
            }
        }
    }
    if let Err(e) = st.save(state_dir) {
        tracing::warn!("automatic cleanup state not saved: {e:#}");
    }
    if let Some((pct, u)) = used() {
        out.used_after = pct;
        out.freed = before_used.saturating_sub(u);
    }
    out
}

/// The alerts for one run (AUTO-7, AUTO-10).
pub fn alerts(host: &str, mount: &str, o: &Outcome) -> Vec<Alert> {
    let mut v = Vec::new();
    if !o.done.is_empty() {
        let mut body = format!("{mount} is now {:.1}% full. Done:\n", o.used_after);
        for (w, b) in o.done.iter().take(15) {
            body.push_str(&format!("- {w} ({})\n", human(*b as i64)));
        }
        if o.done.len() > 15 {
            body.push_str(&format!(
                "... and {} more (see the system log)\n",
                o.done.len() - 15
            ));
        }
        for (w, e) in o.skipped.iter().take(5) {
            body.push_str(&format!("skipped: {w}: {e}\n"));
        }
        v.push(Alert {
            severity: Severity::Warning,
            title: format!("{mount}: dux freed {} automatically", human(o.freed as i64)),
            body: body.trim_end().into(),
            key: format!("auto:{mount}"),
            host: host.into(),
        });
    }
    if !o.announced.is_empty() {
        let mut body = String::from(
            "Build output in idle projects (10+ days) will be deleted in 24 hours if the disk is still full:\n",
        );
        for (p, b) in o.announced.iter().take(20) {
            body.push_str(&format!("- {p} ({})\n", human(*b as i64)));
        }
        body.push_str(
            "To keep one: dux auto keep PATH. To protect it for good: dux never-touch add PATH.",
        );
        v.push(Alert {
            severity: Severity::Warning,
            title: format!("{mount}: build output to be deleted in 24 h"),
            body,
            key: format!("auto-notice:{mount}"),
            host: host.into(),
        });
    }
    v
}

/// Logs dux emptied that grow back fast (AUTO-9): they would regain what was
/// freed within a day. Checked once a minute; alerts once per log.
pub fn regrowth(state_dir: &Path, host: &str, now: i64) -> Vec<Alert> {
    let mut st = State::load(state_dir);
    if st.watched.is_empty() {
        return Vec::new();
    }
    let owners = crate::insight::open_file_owners();
    let mut out = Vec::new();
    let mut drop = Vec::new();
    for (path, (after, t, freed)) in &st.watched {
        let age = now - t;
        if age > WATCH_SECS {
            drop.push(path.clone());
            continue;
        }
        let Ok(m) = std::fs::metadata(path) else {
            drop.push(path.clone());
            continue;
        };
        let grown = m.len().saturating_sub(*after);
        if age < 600 || grown < 10 << 20 {
            continue;
        }
        let per_day = grown as f64 * 86_400.0 / age as f64;
        if per_day >= *freed as f64 {
            let writer = owners
                .get(&(m.dev(), m.ino()))
                .cloned()
                .unwrap_or_else(|| "unknown (not open now)".into());
            out.push(Alert {
                severity: Severity::Warning,
                title: format!("{path} is growing back fast after cleanup"),
                body: format!(
                    "+{} in {} since dux emptied it ({}/day); written by {writer}. Fix the cause: log level, rotation, or a failing job.",
                    human(grown as i64),
                    crate::insight::age_text(*t, now),
                    human(per_day as i64)
                ),
                key: format!("regrow:{path}"),
                host: host.into(),
            });
            drop.push(path.clone());
        }
    }
    if !drop.is_empty() {
        for d in drop {
            st.watched.remove(&d);
        }
        let _ = st.save(state_dir);
    }
    out
}

/// One pass for one mount, with real deletes and alerts.
pub fn run_live(cfg: &NotifyConfig, p: &Policy, host: &str, mount: &str, state_dir: &Path) {
    if cfg.dry_run {
        return; // AUTO-8: never in a dry run.
    }
    let m = mount.to_string();
    let mut used = || {
        crate::sentinel::statvfs(&m)
            .ok()
            .map(|u| (u.used_pct(), u.used))
    };
    let o = run(
        p,
        mount,
        state_dir,
        crate::util::now_secs(),
        &mut cleanup::apply,
        &mut used,
    );
    for a in alerts(host, mount, &o) {
        crate::notify::send(cfg, &a, false, crate::util::now_secs());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol(folders: &[&str]) -> Policy {
        Policy {
            enabled: true,
            folders: folders.iter().map(|s| s.to_string()).collect(),
            rotated_logs: true,
            truncate_logs: true,
            build_caches: true,
            ..Policy::default()
        }
    }

    #[test]
    fn policy_is_off_by_default_and_needs_folders_and_actions() {
        let none = |_: &str| None;
        let p = Policy::from_raw(&none).unwrap();
        assert!(!p.enabled, "AUTO-1: off by default");
        assert_eq!(p.start_pct, 90.0);
        assert_eq!(p.stop_pct, 0.0, "no stop level by default");
        let only_on = |k: &str| (k == "DUX_AUTO").then(|| "true".to_string());
        assert!(
            Policy::from_raw(&only_on).is_err(),
            "on without approved actions"
        );
        let no_folder = |k: &str| match k {
            "DUX_AUTO" | "DUX_AUTO_ROTATED_LOGS" => Some("true".to_string()),
            _ => None,
        };
        assert!(
            Policy::from_raw(&no_folder).is_err(),
            "file actions need folders"
        );
        let root = |k: &str| match k {
            "DUX_AUTO" | "DUX_AUTO_ROTATED_LOGS" => Some("true".to_string()),
            "DUX_AUTO_FOLDERS" => Some("/".to_string()),
            _ => None,
        };
        assert!(Policy::from_raw(&root).is_err(), "/ cannot be approved");
        let stop = |k: &str| match k {
            "DUX_AUTO_START_PCT" => Some("90".to_string()),
            "DUX_AUTO_STOP_PCT" => Some("95".to_string()),
            _ => None,
        };
        assert!(Policy::from_raw(&stop).is_err(), "stop below start");
    }

    #[test]
    fn only_approved_kinds_inside_approved_folders() {
        let p = pol(&["/srv/app/logs"]);
        let rm = |s: &str| Item {
            level: 2,
            what: "x".into(),
            action: Action::Remove { path: s.into() },
            frees: 1,
        };
        assert!(permitted(&p, &rm("/srv/app/logs/a.log.1")).is_ok());
        assert!(
            permitted(&p, &rm("/srv/app/logs-old/a.log.1")).is_err(),
            "path boundary"
        );
        assert!(
            permitted(&p, &rm("/srv/app/data.db")).is_err(),
            "not a rotated log, outside"
        );
        assert!(
            permitted(&p, &rm("/srv/app/logs/data.db")).is_err(),
            "not a rotated log"
        );
        let big = Item {
            level: 4,
            what: "delete a biggest file".into(),
            action: Action::Remove {
                path: "/srv/app/logs/huge.bin".into(),
            },
            frees: 1,
        };
        assert!(permitted(&p, &big).is_err(), "never a biggest-file delete");
        let journal = Item {
            level: 1,
            what: "j".into(),
            action: Action::Run {
                program: "journalctl".into(),
                args: vec!["--vacuum-size=1G".into()],
            },
            frees: 1,
        };
        assert!(permitted(&p, &journal).is_err(), "journal not approved");
        let mut pj = pol(&["/var/log"]);
        pj.journal = true;
        assert!(permitted(&pj, &journal).is_ok());
        let tmp = Item {
            level: 3,
            what: "t".into(),
            action: Action::Run {
                program: "systemd-tmpfiles".into(),
                args: vec!["--clean".into()],
            },
            frees: 0,
        };
        assert!(permitted(&pj, &tmp).is_err(), "not an automatic kind");
        let vol = Item {
            level: 3,
            what: "v".into(),
            action: Action::Run {
                program: "docker".into(),
                args: vec!["volume".into(), "rm".into(), "db".into()],
            },
            frees: 0,
        };
        assert!(
            permitted(&pj, &vol).is_err(),
            "volumes need their own approval"
        );
    }

    #[test]
    fn a_run_frees_logs_announces_build_output_then_deletes_it_a_day_later() {
        let d = std::env::temp_dir().join(format!("dux-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let logs = d.join("app/logs");
        std::fs::create_dir_all(&logs).unwrap();
        for n in 1..=5 {
            std::fs::write(logs.join(format!("api.log.{n}")), vec![b'x'; 2 << 20]).unwrap();
        }
        std::fs::write(logs.join("api.log"), vec![b'y'; 60 << 20]).unwrap();
        let proj = d.join("code/site");
        std::fs::create_dir_all(proj.join("node_modules/x")).unwrap();
        std::fs::write(proj.join("package.json"), b"{}").unwrap();
        std::fs::write(proj.join("node_modules/x/big.js"), vec![1u8; 3 << 20]).unwrap();
        let state = d.join("state");
        let mut p = pol(&[
            d.join("app").to_str().unwrap(),
            d.join("code").to_str().unwrap(),
        ]);
        // Activity is the ctime, which a test cannot age: count any project.
        p.idle_days = 0;
        // Not a log, though big and numbered: never touched outside /var/log.
        std::fs::write(d.join("app/data.db"), vec![0u8; 60 << 20]).unwrap();
        std::fs::write(d.join("app/backup.tar.1"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(d.join("app/backup.tar.2"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(d.join("app/backup.tar.3"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(d.join("app/backup.tar.4"), vec![0u8; 2 << 20]).unwrap();
        let applied = std::cell::RefCell::new(Vec::<String>::new());
        let mut apply = |i: &Item| -> Result<()> {
            applied.borrow_mut().push(format!("{:?}", i.action));
            Ok(())
        };
        let mut used = || Some((95.0, 100));
        let now = crate::util::now_secs();
        let o = run(&p, d.to_str().unwrap(), &state, now, &mut apply, &mut used);
        let a = applied.borrow().join("\n");
        assert_eq!(
            a.matches("Remove { path").count(),
            2,
            "rotated beyond the newest 3: {a}"
        );
        assert!(
            a.contains("TailAndTruncate") && a.contains("api.log\""),
            "{a}"
        );
        assert!(
            !a.contains("RemoveTree"),
            "build output is only announced: {a}"
        );
        assert!(
            !a.contains("data.db") && !a.contains("backup.tar"),
            "not logs: {a}"
        );
        assert_eq!(o.announced.len(), 1);
        let al = alerts("h", "/x", &o);
        assert!(al
            .iter()
            .any(|x| x.title.contains("in 24 h") && x.body.contains("dux auto keep")));
        // 23 h later: still waiting. 25 h later: deleted.
        applied.borrow_mut().clear();
        let _ = run(
            &p,
            d.to_str().unwrap(),
            &state,
            now + 23 * 3600,
            &mut apply,
            &mut used,
        );
        assert!(!applied.borrow().join("").contains("RemoveTree"));
        applied.borrow_mut().clear();
        let o = run(
            &p,
            d.to_str().unwrap(),
            &state,
            now + 25 * 3600,
            &mut apply,
            &mut used,
        );
        assert!(
            applied.borrow().join("").contains("RemoveTree"),
            "{:?}",
            applied.borrow()
        );
        assert!(o.announced.is_empty());
        // A kept folder is never deleted.
        let mut st = State::load(&state);
        st.keep.push(proj.to_string_lossy().into_owned());
        st.save(&state).unwrap();
        applied.borrow_mut().clear();
        let _ = run(
            &p,
            d.to_str().unwrap(),
            &state,
            now + 60 * 3600,
            &mut apply,
            &mut used,
        );
        let _ = run(
            &p,
            d.to_str().unwrap(),
            &state,
            now + 90 * 3600,
            &mut apply,
            &mut used,
        );
        assert!(!applied.borrow().join("").contains("RemoveTree"), "kept");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn stop_level_ends_the_run_and_regrowth_names_the_log() {
        let d = std::env::temp_dir().join(format!("dux-auto-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let logs = d.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        for n in 1..=8 {
            std::fs::write(logs.join(format!("w.log.{n}")), vec![b'x'; 1 << 20]).unwrap();
        }
        let mut p = pol(&[logs.to_str().unwrap()]);
        p.stop_pct = 80.0;
        let mut n = 0;
        let mut apply = |_: &Item| -> Result<()> {
            n += 1;
            Ok(())
        };
        let calls = std::cell::Cell::new(0);
        let mut used = || {
            calls.set(calls.get() + 1);
            Some((if calls.get() > 2 { 79.0 } else { 91.0 }, 1))
        };
        let state = d.join("state");
        run(&p, d.to_str().unwrap(), &state, 1000, &mut apply, &mut used);
        assert_eq!(n, 1, "stopped once below the stop level");
        // Regrowth: a log emptied an hour ago that grew 50 MiB (freed 60 MiB).
        let log = logs.join("w.log");
        std::fs::write(&log, vec![b'z'; 50 << 20]).unwrap();
        let mut st = State::default();
        st.watched
            .insert(log.to_string_lossy().into_owned(), (0, 1000, 60 << 20));
        st.save(&state).unwrap();
        let a = regrowth(&state, "h", 1000 + 3600);
        assert_eq!(a.len(), 1);
        assert!(a[0].title.contains("growing back fast"));
        assert!(regrowth(&state, "h", 1000 + 3700).is_empty(), "alerts once");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
