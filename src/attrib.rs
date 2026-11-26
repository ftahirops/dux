//! Attribution: which folder, which writer, which known culprit.
//!
//! - `WalkScheduler` runs walks in the background, one at a time: a baseline
//!   walk every hour (jittered) and an early walk when a mount grows fast or
//!   an alert fires, never more often than `walk_min_gap`.
//! - Each finished walk is diffed against the previous one for that mount,
//!   saved as a snapshot, and summarised in the growth history.
//! - `Writers` samples cgroup v2 `io.stat` to name the services writing most.
//! - `CATALOG` maps well-known paths (journal, Docker, caches, crash dumps...)
//!   to a plain-language label and the safe command that frees them.
//! - A gap between statvfs "used" and the walk total points at deleted files
//!   that are still open.

use crate::notify::{Alert, Severity};
use crate::walker::{self, Change, Tree};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Known places disk space goes, with the safe fix for each.
pub struct Culprit {
    pub path: &'static str,
    pub label: &'static str,
    pub fix: &'static str,
}

pub const CATALOG: &[Culprit] = &[
    Culprit {
        path: "/var/log/journal",
        label: "systemd journal",
        fix: "journalctl --vacuum-size=1G",
    },
    Culprit {
        path: "/var/log",
        label: "system logs",
        fix: "check logrotate; remove old rotated *.gz / *.1 files",
    },
    Culprit {
        path: "/var/lib/docker/containers",
        label: "Docker container logs",
        fix: "set log-opts max-size in /etc/docker/daemon.json; truncate the big *-json.log",
    },
    Culprit {
        path: "/var/lib/docker/overlay2",
        label: "Docker images and layers",
        fix: "docker image prune -a --filter until=720h",
    },
    Culprit {
        path: "/var/lib/docker/volumes",
        label: "Docker volumes (data)",
        fix: "docker volume ls -f dangling=true; remove only unused volumes",
    },
    Culprit {
        path: "/var/lib/docker/buildkit",
        label: "Docker build cache",
        fix: "docker builder prune --filter until=168h",
    },
    Culprit {
        path: "/var/lib/containerd",
        label: "containerd images and snapshots",
        fix: "crictl rmi --prune",
    },
    Culprit {
        path: "/var/cache/apt",
        label: "apt package cache",
        fix: "apt-get clean",
    },
    Culprit {
        path: "/var/cache/dnf",
        label: "dnf package cache",
        fix: "dnf clean packages",
    },
    Culprit {
        path: "/var/crash",
        label: "crash dumps",
        fix: "remove old dumps; keep the newest few",
    },
    Culprit {
        path: "/var/lib/systemd/coredump",
        label: "core dumps",
        fix: "coredumpctl list; remove old dumps",
    },
    Culprit {
        path: "/var/tmp",
        label: "temporary files",
        fix: "systemd-tmpfiles --clean",
    },
    Culprit {
        path: "/tmp",
        label: "temporary files",
        fix: "systemd-tmpfiles --clean",
    },
    Culprit {
        path: "/root/.cache",
        label: "root's caches",
        fix: "pip cache purge; go clean -cache; npm cache clean --force",
    },
    Culprit {
        path: "/var/lib/snapd",
        label: "snap revisions",
        fix: "snap set system refresh.retain=2",
    },
    Culprit {
        path: "/boot",
        label: "kernels",
        fix: "apt-get autoremove --purge (keeps the running kernel)",
    },
];

/// The most specific catalog entry covering `path`, if any.
pub fn culprit_for(path: &str) -> Option<&'static Culprit> {
    CATALOG
        .iter()
        .filter(|c| path == c.path || path.starts_with(&format!("{}/", c.path)))
        .max_by_key(|c| c.path.len())
        .or_else(|| {
            // /home/<user>/.cache and friends.
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() >= 4 && parts[1] == "home" && parts[3] == ".cache" {
                Some(&HOME_CACHE)
            } else {
                None
            }
        })
}

static HOME_CACHE: Culprit = Culprit {
    path: "/home/*/.cache",
    label: "user caches",
    fix: "pip cache purge; go clean -cache; npm cache clean --force",
};

/// Sizes of every catalog location present in a tree, largest first.
pub fn catalog_sizes(tree: &Tree) -> Vec<(&'static Culprit, String, u64)> {
    let root = tree.root_path.trim_end_matches('/');
    let mut out = Vec::new();
    for c in CATALOG {
        let rel = if root.is_empty() {
            c.path.trim_start_matches('/').to_string()
        } else if let Some(r) = c.path.strip_prefix(root) {
            if !r.is_empty() && !r.starts_with('/') {
                continue;
            }
            r.trim_start_matches('/').to_string()
        } else {
            continue;
        };
        if let Some(n) = tree.root.find(&rel) {
            out.push((c, c.path.to_string(), n.total_bytes));
        }
    }
    if let Some(home) = root.is_empty().then(|| tree.root.find("home")).flatten() {
        for u in &home.children {
            if let Some(cache) = u.children.iter().find(|c| c.name == ".cache") {
                out.push((
                    &HOME_CACHE,
                    format!("/home/{}/.cache", u.name),
                    cache.total_bytes,
                ));
            }
        }
    }
    out.sort_by_key(|(_, _, b)| std::cmp::Reverse(*b));
    out
}

// ---------------------------------------------------------------------------
// Writers: cgroup v2 io.stat
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Writers {
    last: HashMap<String, u64>,
    last_at: i64,
    /// Bytes per second written by each cgroup over the last interval.
    pub rates: Vec<(String, f64)>,
}

/// Parse one io.stat file: written bytes, taking the largest device line so a
/// device-mapper volume and its disk are not added together.
pub fn parse_io_stat(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| {
            l.split_whitespace()
                .find_map(|f| f.strip_prefix("wbytes=")?.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0)
}

fn docker_name(id: &str) -> Option<String> {
    let cfg =
        std::fs::read_to_string(format!("/var/lib/docker/containers/{id}/config.v2.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&cfg).ok()?;
    Some(v["Name"].as_str()?.trim_start_matches('/').to_string())
}

/// "system.slice/docker-<id>.scope" -> "docker:<name>", "x.service" -> "x".
pub fn friendly_cgroup(rel: &str) -> String {
    let leaf = rel.rsplit('/').next().unwrap_or(rel);
    if let Some(id) = leaf
        .strip_prefix("docker-")
        .and_then(|s| s.strip_suffix(".scope"))
    {
        return format!(
            "docker:{}",
            docker_name(id).unwrap_or_else(|| id.chars().take(12).collect())
        );
    }
    leaf.trim_end_matches(".service")
        .trim_end_matches(".scope")
        .to_string()
}

impl Writers {
    /// Sample leaf cgroups up to three levels deep.
    pub fn sample(&mut self, now: i64) {
        let base = Path::new("/sys/fs/cgroup");
        let mut cur: HashMap<String, u64> = HashMap::new();
        fn rec(base: &Path, dir: &Path, depth: usize, cur: &mut HashMap<String, u64>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            let mut has_child = false;
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) && depth < 3 {
                    has_child = true;
                    rec(base, &e.path(), depth + 1, cur);
                }
            }
            if !has_child && depth > 0 {
                if let Ok(t) = std::fs::read_to_string(dir.join("io.stat")) {
                    let rel = dir
                        .strip_prefix(base)
                        .unwrap_or(dir)
                        .to_string_lossy()
                        .into_owned();
                    cur.insert(rel, parse_io_stat(&t));
                }
            }
        }
        rec(base, base, 0, &mut cur);
        let dt = (now - self.last_at).max(1) as f64;
        if self.last_at > 0 {
            let mut rates: Vec<(String, f64)> = cur
                .iter()
                .filter_map(|(k, v)| {
                    let prev = *self.last.get(k)?;
                    let d = v.saturating_sub(prev);
                    (d > 0).then(|| (k.clone(), d as f64 / dt))
                })
                .collect();
            rates.sort_by(|a, b| b.1.total_cmp(&a.1));
            rates.truncate(8);
            self.rates = rates;
        }
        self.last = cur;
        self.last_at = now;
    }

    pub fn summary(&self, n: usize) -> String {
        self.rates
            .iter()
            .take(n)
            .filter(|(_, r)| *r >= 64.0 * 1024.0)
            .map(|(k, r)| format!("{} {}/s", friendly_cgroup(k), crate::util::human(*r as i64)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ---------------------------------------------------------------------------
// Walk scheduling
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reason {
    Baseline,
    Growth,
    Alert(Severity),
    Startup,
}

pub struct WalkDone {
    pub mount: String,
    pub reason: Reason,
    pub tree: anyhow::Result<Tree>,
}

pub struct WalkScheduler {
    pub every_secs: i64,
    pub min_gap_secs: i64,
    pub trigger_bytes: u64,
    pub threads: usize,
    pub snapshot_dir: Option<PathBuf>,
    pub snapshot_every_secs: i64,
    prev: HashMap<String, Arc<Tree>>,
    last_start: HashMap<String, i64>,
    next_baseline: HashMap<String, i64>,
    used_at_last_walk: HashMap<String, u64>,
    last_snapshot: HashMap<String, i64>,
    pending_alert: HashMap<String, Severity>,
    running: Option<String>,
    tx: Sender<WalkDone>,
    rx: Receiver<WalkDone>,
    seed: u64,
}

/// Filesystems worth walking: local block filesystems, not network or memory.
pub fn walkable(fstype: &str) -> bool {
    matches!(
        fstype,
        "ext2"
            | "ext3"
            | "ext4"
            | "xfs"
            | "btrfs"
            | "f2fs"
            | "jfs"
            | "reiserfs"
            | "zfs"
            | "bcachefs"
    )
}

impl WalkScheduler {
    pub fn new(
        every_secs: i64,
        min_gap_secs: i64,
        trigger_bytes: u64,
        threads: usize,
        snapshot_dir: Option<PathBuf>,
    ) -> WalkScheduler {
        let (tx, rx) = channel();
        WalkScheduler {
            every_secs,
            min_gap_secs,
            trigger_bytes,
            threads,
            snapshot_dir,
            snapshot_every_secs: 6 * 3600,
            prev: HashMap::new(),
            last_start: HashMap::new(),
            next_baseline: HashMap::new(),
            used_at_last_walk: HashMap::new(),
            last_snapshot: HashMap::new(),
            pending_alert: HashMap::new(),
            running: None,
            tx,
            rx,
            seed: std::process::id() as u64 ^ crate::util::now_secs() as u64,
        }
    }

    /// Load saved snapshots so the first diff after a restart has a baseline.
    pub fn load_snapshots(&mut self, mounts: &[String]) {
        let Some(dir) = self.snapshot_dir.clone() else {
            return;
        };
        for m in mounts {
            if let Ok(Some(t)) = crate::snapshot::load(&dir, m) {
                self.prev.insert(m.clone(), Arc::new(t));
            }
        }
    }

    fn jitter(&mut self, secs: i64) -> i64 {
        // xorshift; spreads baseline walks between 3/4 and 5/4 of the interval.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        let span = (secs / 2).max(1) as u64;
        secs * 3 / 4 + (self.seed % span) as i64
    }

    /// Ask for an early walk because an alert fired on this mount.
    pub fn note_alert(&mut self, mount: &str, sev: Severity) {
        if matches!(sev, Severity::Warning | Severity::Critical) {
            let e = self.pending_alert.entry(mount.to_string()).or_insert(sev);
            if sev > *e {
                *e = sev;
            }
        }
    }

    /// Start at most one walk if one is due. `used` is statvfs used bytes per mount.
    pub fn poll(&mut self, now: i64, used: &[(String, u64)]) {
        if self.running.is_some() {
            return;
        }
        let mut pick: Option<(String, Reason)> = None;
        for (m, u) in used {
            let since = now - self.last_start.get(m).copied().unwrap_or(i64::MIN / 2);
            let gap_ok = since >= self.min_gap_secs;
            let reason = if !self.last_start.contains_key(m) {
                Some(Reason::Startup)
            } else if let Some(&sev) = self.pending_alert.get(m).filter(|_| gap_ok) {
                Some(Reason::Alert(sev))
            } else if gap_ok
                && u.saturating_sub(self.used_at_last_walk.get(m).copied().unwrap_or(*u))
                    >= self.trigger_bytes
            {
                Some(Reason::Growth)
            } else if now >= self.next_baseline.get(m).copied().unwrap_or(i64::MAX) {
                Some(Reason::Baseline)
            } else {
                None
            };
            if let Some(r) = reason {
                let better = matches!(
                    (&pick, r),
                    (None, _)
                        | (
                            Some((_, Reason::Baseline)),
                            Reason::Alert(_) | Reason::Growth
                        )
                        | (Some((_, Reason::Growth)), Reason::Alert(_))
                );
                if better {
                    pick = Some((m.clone(), r));
                }
            }
        }
        let Some((mount, reason)) = pick else { return };
        self.pending_alert.remove(&mount);
        self.last_start.insert(mount.clone(), now);
        if let Some((_, u)) = used.iter().find(|(m, _)| *m == mount) {
            self.used_at_last_walk.insert(mount.clone(), *u);
        }
        let next = now + self.jitter(self.every_secs);
        self.next_baseline.insert(mount.clone(), next);
        self.running = Some(mount.clone());
        let tx = self.tx.clone();
        let threads = self.threads;
        let _ = std::thread::Builder::new()
            .name("dux-walk".into())
            .spawn(move || {
                // Lowest CPU and I/O priority for the walk thread only.
                // SAFETY: setpriority/ioprio_set on the calling thread.
                unsafe {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                    libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
                }
                let tree = walker::walk(&mount, threads);
                let _ = tx.send(WalkDone {
                    mount,
                    reason,
                    tree,
                });
            });
    }

    /// Collect a finished walk, if any: returns (mount, reason, tree, changes vs previous).
    pub fn collect(&mut self, now: i64) -> Option<(String, Reason, Arc<Tree>, Vec<Change>)> {
        let done = self.rx.try_recv().ok()?;
        self.running = None;
        let tree = match done.tree {
            Ok(t) => Arc::new(t),
            Err(e) => {
                tracing::warn!(mount = %done.mount, "walk failed: {e:#}");
                return None;
            }
        };
        let changes = self
            .prev
            .get(&done.mount)
            .map(|p| {
                walker::diff(
                    &p.root,
                    &tree.root,
                    (64 << 20).max(tree.root.total_bytes / 1000),
                )
            })
            .unwrap_or_default();
        if let Some(dir) = &self.snapshot_dir {
            let due = now - self.last_snapshot.get(&done.mount).copied().unwrap_or(0)
                >= self.snapshot_every_secs;
            if due {
                // Keep the previous snapshot as .prev for `dux where`.
                let cur = crate::snapshot::file_for(dir, &done.mount);
                let _ = std::fs::rename(&cur, cur.with_extension("prev.snap"));
                match crate::snapshot::save(dir, &tree) {
                    Ok((p, n)) => {
                        tracing::info!(path = %p.display(), bytes = n, "saved folder snapshot");
                        self.last_snapshot.insert(done.mount.clone(), now);
                    }
                    Err(e) => tracing::warn!("saving snapshot failed: {e:#}"),
                }
            }
            let top = walker::largest_at_depth(&tree.root, 2, 20);
            let _ = crate::snapshot::append_history(dir, &tree, &top);
        }
        tracing::info!(
            mount = %done.mount,
            reason = ?done.reason,
            entries = tree.stats.entries,
            seconds = %format!("{:.2}", tree.stats.seconds),
            rate = %format!("{:.0}/s", tree.stats.entries_per_second()),
            "walk finished"
        );
        self.prev.insert(done.mount.clone(), tree.clone());
        Some((done.mount, done.reason, tree, changes))
    }

    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.running.is_some()
    }
}

/// The follow-up alert that says where growth went and what to do about it.
pub fn attribution_alert(
    host: &str,
    mount: &str,
    reason: Reason,
    tree: &Tree,
    changes: &[Change],
    statvfs_used: Option<u64>,
    writers: &str,
) -> Option<Alert> {
    let severity = match reason {
        Reason::Alert(s) => s,
        Reason::Growth => Severity::Warning,
        Reason::Baseline | Reason::Startup => Severity::Info,
    };
    let growing: Vec<&Change> = changes.iter().filter(|c| c.delta() > 0).take(5).collect();
    let mut body = String::new();
    for c in &growing {
        let hint = culprit_for(&c.path)
            .map(|k| format!("  [{}: {}]", k.label, k.fix))
            .unwrap_or_default();
        body.push_str(&format!(
            "+{} {} (now {}){hint}\n",
            crate::util::human(c.delta()),
            c.path,
            crate::util::human(c.after as i64)
        ));
    }
    if !writers.is_empty() {
        body.push_str(&format!("writing now: {writers}\n"));
    }
    if let Some(used) = statvfs_used {
        let gap = used.saturating_sub(tree.root.total_bytes);
        if gap >= (2u64 << 30).max(used / 20) {
            body.push_str(&format!(
                "{} used but not found in any file: likely deleted files still held open (run `dux deleted-open`)\n",
                crate::util::human(gap as i64)
            ));
        }
    }
    if body.is_empty()
        || (growing.is_empty() && matches!(reason, Reason::Baseline | Reason::Startup))
    {
        return None;
    }
    let title = match growing.first() {
        Some(c) => format!(
            "{mount}: growth is in {} (+{})",
            c.path,
            crate::util::human(c.delta())
        ),
        None => format!("{mount}: where the space is"),
    };
    Some(Alert {
        severity,
        title,
        body: body.trim_end().to_string(),
        key: format!("where:{mount}"),
        host: host.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walker::DirNode;

    #[test]
    fn culprit_lookup_prefers_the_most_specific_entry() {
        assert_eq!(
            culprit_for("/var/log/journal/abc").unwrap().label,
            "systemd journal"
        );
        assert_eq!(culprit_for("/var/log/nginx").unwrap().label, "system logs");
        assert_eq!(
            culprit_for("/var/lib/docker/containers/x").unwrap().label,
            "Docker container logs"
        );
        assert_eq!(
            culprit_for("/home/ann/.cache/pip").unwrap().label,
            "user caches"
        );
        assert!(culprit_for("/srv/data").is_none());
        assert!(
            culprit_for("/var/logs-archive").is_none(),
            "prefix must end at a path boundary"
        );
    }

    #[test]
    fn io_stat_takes_the_largest_device_line() {
        let t = "8:0 rbytes=1 wbytes=500 rios=1 wios=2\n252:0 rbytes=1 wbytes=500 rios=1 wios=2\n7:3 wbytes=20\n";
        assert_eq!(parse_io_stat(t), 500);
        assert_eq!(parse_io_stat(""), 0);
        assert_eq!(friendly_cgroup("system.slice/nginx.service"), "nginx");
        assert_eq!(
            friendly_cgroup("user.slice/user-0.slice/session-3.scope"),
            "session-3"
        );
    }

    #[test]
    fn writers_sampling_does_not_panic() {
        let mut w = Writers::default();
        w.sample(100);
        w.sample(110);
        let _ = w.summary(3);
    }

    fn tree(path: &str, root: DirNode) -> Tree {
        Tree {
            root_path: path.into(),
            root,
            stats: Default::default(),
            started_at: 0,
        }
    }

    fn node(name: &str, own: u64, children: Vec<DirNode>) -> DirNode {
        let total = own + children.iter().map(|c| c.total_bytes).sum::<u64>();
        DirNode {
            name: name.into(),
            own_bytes: own,
            total_bytes: total,
            children,
            ..DirNode::default()
        }
    }

    #[test]
    fn attribution_names_folder_fix_writer_and_deleted_gap() {
        let journal = node("journal", 3 << 30, vec![]);
        let log = node("log", 0, vec![journal]);
        let var = node("var", 0, vec![log]);
        let t = tree("/", node("/", 0, vec![var]));
        let ch = vec![Change {
            path: "/var/log/journal".into(),
            before: 1 << 30,
            after: 3 << 30,
        }];
        let a = attribution_alert(
            "h",
            "/",
            Reason::Alert(Severity::Critical),
            &t,
            &ch,
            Some(20 << 30),
            "docker:app 12 MiB/s",
        )
        .unwrap();
        assert_eq!(a.severity, Severity::Critical);
        assert!(a.title.contains("/var/log/journal (+2 GiB)"), "{}", a.title);
        assert!(a.body.contains("journalctl --vacuum-size=1G"));
        assert!(a.body.contains("writing now: docker:app"));
        assert!(a.body.contains("likely deleted files still held open"));
        // A quiet baseline walk with nothing growing sends nothing.
        assert!(
            attribution_alert("h", "/", Reason::Baseline, &t, &[], Some(3 << 30), "").is_none()
        );
    }

    #[test]
    fn catalog_sizes_finds_known_locations() {
        let cache = node(".cache", 5 << 20, vec![]);
        let ann = node("ann", 0, vec![cache]);
        let home = node("home", 0, vec![ann]);
        let apt = node("apt", 7 << 20, vec![]);
        let vcache = node("cache", 0, vec![apt]);
        let var = node("var", 0, vec![vcache]);
        let t = tree("/", node("/", 0, vec![home, var]));
        let s = catalog_sizes(&t);
        assert_eq!(s[0].1, "/var/cache/apt");
        assert_eq!(s[1].1, "/home/ann/.cache");
    }

    #[test]
    fn scheduler_walks_at_startup_then_on_alert_and_respects_the_gap() {
        let d = std::env::temp_dir().join(format!("dux-sched-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/f"), vec![1u8; 300_000]).unwrap();
        let m = d.to_string_lossy().into_owned();
        let mut s = WalkScheduler::new(3600, 600, 1 << 30, 2, None);
        s.poll(0, &[(m.clone(), 0)]);
        assert!(s.running());
        let mut got = None;
        for _ in 0..200 {
            if let Some(x) = s.collect(1) {
                got = Some(x);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (_, reason, t, ch) = got.expect("walk finished");
        assert_eq!(reason, Reason::Startup);
        assert!(t.root.total_bytes >= 300_000 && ch.is_empty());
        // An alert inside the gap waits; after the gap it triggers a walk.
        s.note_alert(&m, Severity::Critical);
        s.poll(100, &[(m.clone(), 0)]);
        assert!(!s.running());
        s.poll(700, &[(m.clone(), 0)]);
        assert!(s.running());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
