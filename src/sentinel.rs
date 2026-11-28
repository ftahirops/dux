//! `dux sentinel`: a tiny always-on disk monitor.
//!
//! Every few seconds it calls `statvfs` on each real mounted filesystem. That
//! is one syscall per mount, reads counters the kernel already keeps, and does
//! no disk I/O, so it is exact and never stale. From those numbers it raises:
//!
//! - space:    used % crosses the warning or critical threshold
//! - inodes:   inode use crosses the warning or critical threshold
//! - fill:     at the current growth rate the filesystem is full soon
//! - burst:    the filesystem grew by more than N bytes inside the window
//! - readonly: a filesystem that was writable became read-only
//!
//! Alerts are controlled so they cannot flood: a condition alerts when it
//! starts or gets worse, repeats only after a quiet interval, clears with
//! hysteresis, and a global hourly cap applies. `wall` (a message on every
//! logged-in terminal) is reserved for critical alerts, with its own minimum
//! gap per filesystem and an hourly cap.

use crate::notify::{self, Alert, NotifyConfig, Severity};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::time::Duration;

/// Filesystem types never worth watching: kernel pseudo filesystems and
/// read-only images that are always 100% full by design.
const SKIP_FSTYPES: &[&str] = &[
    "proc",
    "sysfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "pstore",
    "bpf",
    "tracefs",
    "debugfs",
    "securityfs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "nsfs",
    "rpc_pipefs",
    "efivarfs",
    "squashfs",
    "iso9660",
    "udf",
    "overlay",
    "ramfs",
    "devtmpfs",
    "fuse.lxcfs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
    "selinuxfs",
    "nfsd",
];

#[derive(Clone, Debug)]
pub struct Settings {
    pub interval: Duration,
    pub warn_pct: f64,
    pub crit_pct: f64,
    pub inode_warn_pct: f64,
    pub inode_crit_pct: f64,
    /// Percentage points below a threshold before a condition clears.
    pub hysteresis_pct: f64,
    pub fill_warn_hours: f64,
    pub fill_crit_hours: f64,
    /// Growth inside `window` that counts as a burst.
    pub burst_bytes: u64,
    pub window: Duration,
    /// Ignore growth-based alerts on filesystems smaller than this.
    pub min_fs_bytes: u64,
    pub warn_repeat: Duration,
    pub crit_repeat: Duration,
    /// Resend early when used % rises this many points above the last alert.
    pub worsen_pct: f64,
    /// Never resend an unchanged-level alert sooner than this.
    pub worsen_min_gap: Duration,
    /// A level steps down only after holding the lower level this long.
    pub ease_after: Duration,
    /// History needed before a full-in-hours warning (critical needs 1 min).
    pub fill_warn_history: Duration,
    /// History needed before a full-in-hours critical.
    pub fill_crit_history: Duration,
    pub wall_min_gap: Duration,
    pub wall_max_per_hour: usize,
    pub max_alerts_per_hour: usize,
    /// Only these mount points, if set.
    pub only_mounts: Vec<String>,
    /// Alert on projected time-to-full (off by default: noisy on build hosts).
    pub fill_alerts: bool,
    /// Alert on growth bursts (off by default).
    pub burst_alerts: bool,
    /// wall broadcasts only when a disk is at least this full (or out of inodes).
    pub wall_pct: f64,
    /// Walk local filesystems to say which folder is growing.
    pub walk: bool,
    pub walk_every: Duration,
    pub walk_min_gap: Duration,
    /// Growth since the last walk that triggers an early walk.
    pub walk_trigger_bytes: u64,
    pub walk_threads: usize,
    pub snapshot_dir: std::path::PathBuf,
    /// Emergency cleanup level run automatically when the disk holding
    /// /var/log goes critical. 0 = off (default).
    pub auto_cleanup_level: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            interval: Duration::from_secs(2),
            warn_pct: 85.0,
            crit_pct: 95.0,
            inode_warn_pct: 85.0,
            inode_crit_pct: 95.0,
            hysteresis_pct: 3.0,
            fill_warn_hours: 24.0,
            fill_crit_hours: 2.0,
            burst_bytes: 5 << 30,
            window: Duration::from_secs(600),
            min_fs_bytes: 1 << 30,
            warn_repeat: Duration::from_secs(3600),
            crit_repeat: Duration::from_secs(900),
            worsen_pct: 2.0,
            worsen_min_gap: Duration::from_secs(300),
            ease_after: Duration::from_secs(120),
            fill_warn_history: Duration::from_secs(300),
            fill_crit_history: Duration::from_secs(180),
            wall_min_gap: Duration::from_secs(1800),
            wall_max_per_hour: 4,
            max_alerts_per_hour: 30,
            only_mounts: Vec::new(),
            fill_alerts: false,
            burst_alerts: false,
            wall_pct: 97.0,
            walk: true,
            walk_every: Duration::from_secs(3600),
            walk_min_gap: Duration::from_secs(600),
            walk_trigger_bytes: 1 << 30,
            walk_threads: 2,
            snapshot_dir: std::path::PathBuf::from(crate::snapshot::DEFAULT_DIR),
            auto_cleanup_level: 0,
        }
    }
}

/// Parse sizes like "5G", "500M", "1T", "1024" (bytes, binary units).
pub fn parse_bytes(s: &str) -> Result<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid size: {s}"))?;
    let mult: f64 = match unit
        .trim()
        .to_ascii_uppercase()
        .trim_end_matches("IB")
        .trim_end_matches('B')
    {
        "" => 1.0,
        "K" => 1024.0,
        "M" => 1024.0 * 1024.0,
        "G" => 1024.0 * 1024.0 * 1024.0,
        "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => anyhow::bail!("unknown size unit: {other}"),
    };
    anyhow::ensure!(n >= 0.0, "size must not be negative");
    Ok((n * mult) as u64)
}

impl Settings {
    /// Read `DUX_*` keys from the notify config map; missing keys keep defaults.
    pub fn from_config(c: &NotifyConfig) -> Result<Settings> {
        let mut s = Settings::default();
        let get = |k: &str| {
            c.raw
                .get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
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
        let dur = |k: &str, d: Duration| -> Result<Duration> {
            match get(k) {
                Some(v) => Ok(Duration::from_secs(
                    crate::util::parse_duration(&v).with_context(|| format!("{k}={v}"))? as u64,
                )),
                None => Ok(d),
            }
        };
        let num = |k: &str, d: f64| -> Result<f64> {
            match get(k) {
                Some(v) => v.parse().with_context(|| format!("{k}={v}")),
                None => Ok(d),
            }
        };
        s.interval = dur("DUX_SENTINEL_INTERVAL", s.interval)?;
        s.warn_pct = pct("DUX_WARN_PCT", s.warn_pct)?;
        s.crit_pct = pct("DUX_CRIT_PCT", s.crit_pct)?;
        s.inode_warn_pct = pct("DUX_INODE_WARN_PCT", s.inode_warn_pct)?;
        s.inode_crit_pct = pct("DUX_INODE_CRIT_PCT", s.inode_crit_pct)?;
        s.hysteresis_pct = pct("DUX_HYSTERESIS_PCT", s.hysteresis_pct)?;
        s.fill_warn_hours = num("DUX_FILL_WARN_HOURS", s.fill_warn_hours)?;
        s.fill_crit_hours = num("DUX_FILL_CRIT_HOURS", s.fill_crit_hours)?;
        if let Some(v) = get("DUX_BURST") {
            s.burst_bytes = parse_bytes(&v).with_context(|| format!("DUX_BURST={v}"))?;
        }
        s.window = dur("DUX_BURST_WINDOW", s.window)?;
        s.warn_repeat = dur("DUX_WARN_REPEAT", s.warn_repeat)?;
        s.crit_repeat = dur("DUX_CRIT_REPEAT", s.crit_repeat)?;
        s.worsen_pct = num("DUX_WORSEN_PCT", s.worsen_pct)?;
        s.worsen_min_gap = dur("DUX_WORSEN_MIN_GAP", s.worsen_min_gap)?;
        s.ease_after = dur("DUX_EASE_AFTER", s.ease_after)?;
        s.fill_warn_history = dur("DUX_FILL_WARN_HISTORY", s.fill_warn_history)?;
        s.fill_crit_history = dur("DUX_FILL_CRIT_HISTORY", s.fill_crit_history)?;
        s.wall_min_gap = dur("DUX_WALL_MIN_GAP", s.wall_min_gap)?;
        s.wall_max_per_hour = num("DUX_WALL_MAX_PER_HOUR", s.wall_max_per_hour as f64)? as usize;
        s.max_alerts_per_hour =
            num("DUX_MAX_ALERTS_PER_HOUR", s.max_alerts_per_hour as f64)? as usize;
        let yes = |v: &str| matches!(v.to_ascii_lowercase().as_str(), "1" | "yes" | "true" | "on");
        if let Some(v) = get("DUX_FILL_ALERTS") {
            s.fill_alerts = yes(&v);
        }
        if let Some(v) = get("DUX_BURST_ALERTS") {
            s.burst_alerts = yes(&v);
        }
        s.wall_pct = pct("DUX_WALL_PCT", s.wall_pct)?;
        if let Some(v) = get("DUX_WALK") {
            s.walk = matches!(v.to_ascii_lowercase().as_str(), "1" | "yes" | "true" | "on");
        }
        s.walk_every = dur("DUX_WALK_EVERY", s.walk_every)?;
        s.walk_min_gap = dur("DUX_WALK_MIN_GAP", s.walk_min_gap)?;
        if let Some(v) = get("DUX_WALK_TRIGGER") {
            s.walk_trigger_bytes =
                parse_bytes(&v).with_context(|| format!("DUX_WALK_TRIGGER={v}"))?;
        }
        s.walk_threads = (num("DUX_WALK_THREADS", s.walk_threads as f64)? as usize).clamp(1, 16);
        s.auto_cleanup_level = (num("DUX_AUTO_CLEANUP_LEVEL", 0.0)? as u8).min(3);
        if let Some(v) = get("DUX_SNAPSHOT_DIR") {
            s.snapshot_dir = std::path::PathBuf::from(v);
        }
        if let Some(v) = get("DUX_MOUNTS") {
            s.only_mounts = v
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
        }
        anyhow::ensure!(
            s.warn_pct < s.crit_pct,
            "DUX_WARN_PCT must be below DUX_CRIT_PCT"
        );
        anyhow::ensure!(
            s.inode_warn_pct < s.inode_crit_pct,
            "DUX_INODE_WARN_PCT must be below DUX_INODE_CRIT_PCT"
        );
        anyhow::ensure!(
            s.fill_crit_hours < s.fill_warn_hours,
            "DUX_FILL_CRIT_HOURS must be below DUX_FILL_WARN_HOURS"
        );
        anyhow::ensure!(
            s.interval >= Duration::from_secs(1),
            "DUX_SENTINEL_INTERVAL must be at least 1s"
        );
        Ok(s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub path: String,
    pub fstype: String,
    pub device: String,
    pub dev_id: String,
}

/// Unescape the octal escapes mountinfo uses for spaces and the like.
pub(crate) fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') as u32 * 64
                + (b[i + 2] - b'0') as u32 * 8
                + (b[i + 3] - b'0') as u32;
            if let Ok(v) = u8::try_from(v) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Real filesystems from /proc/self/mountinfo, one entry per device (the
/// shortest mount path wins, so bind mounts are not reported twice).
/// `path` is `dir` or inside it (a path-boundary prefix test).
fn under(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut by_dev: HashMap<String, Mount> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let lf: Vec<&str> = left.split(' ').collect();
        let rf: Vec<&str> = right.split(' ').collect();
        if lf.len() < 5 || rf.len() < 2 {
            continue;
        }
        let dev_id = lf[2].to_string();
        let path = unescape_mount(lf[4]);
        let fstype = rf[0].to_string();
        let device = unescape_mount(rf[1]);
        if SKIP_FSTYPES.contains(&fstype.as_str()) || under(&path, "/proc") || under(&path, "/sys")
        {
            continue;
        }
        // tmpfs is watched only where filling it breaks things.
        if fstype == "tmpfs" && !matches!(path.as_str(), "/tmp" | "/run" | "/dev/shm" | "/var/tmp")
        {
            continue;
        }
        let m = Mount {
            path,
            fstype,
            device,
            dev_id: dev_id.clone(),
        };
        match by_dev.get(&dev_id) {
            Some(existing) if existing.path.len() <= m.path.len() => {}
            Some(_) => {
                by_dev.insert(dev_id, m);
            }
            None => {
                order.push(dev_id.clone());
                by_dev.insert(dev_id, m);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|d| by_dev.remove(&d))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub total: u64,
    pub used: u64,
    /// Space available to unprivileged users (what `df` shows as Avail).
    pub avail: u64,
    pub inodes_total: u64,
    pub inodes_used: u64,
    pub readonly: bool,
}

impl Usage {
    /// Used percentage the way `df` computes it: used / (used + avail).
    pub fn used_pct(&self) -> f64 {
        let denom = self.used + self.avail;
        if denom == 0 {
            0.0
        } else {
            self.used as f64 * 100.0 / denom as f64
        }
    }
    pub fn inode_pct(&self) -> Option<f64> {
        (self.inodes_total > 0).then(|| self.inodes_used as f64 * 100.0 / self.inodes_total as f64)
    }
}

/// Mounts whose usage check has not returned yet (a hung network share).
static STATVFS_IN_FLIGHT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Filesystem usage, but never blocking for more than 3 seconds: a dead NFS
/// or CIFS server makes statvfs hang forever, and one hung mount must not stop
/// alerts for every other disk. A mount whose earlier check is still stuck is
/// reported "not responding" at once, so stuck threads never pile up.
pub fn statvfs(path: &str) -> Result<Usage> {
    statvfs_within(path, std::time::Duration::from_secs(3))
}

pub fn statvfs_within(path: &str, limit: std::time::Duration) -> Result<Usage> {
    {
        let flight = STATVFS_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flight.iter().any(|p| p == path) {
            anyhow::bail!("{path} is not responding (an earlier check is still waiting; a network filesystem may be down)");
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let p = path.to_string();
    STATVFS_IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(p.clone());
    let spawned = std::thread::Builder::new()
        .name("dux-statvfs".into())
        .spawn(move || {
            let r = statvfs_now(&p);
            STATVFS_IN_FLIGHT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|x| x != &p);
            let _ = tx.send(r);
        });
    if spawned.is_err() {
        STATVFS_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|x| x != path);
        return statvfs_now(path);
    }
    match rx.recv_timeout(limit) {
        Ok(r) => r,
        Err(_) => anyhow::bail!(
            "{path} is not responding after {}s (a network filesystem may be down)",
            limit.as_secs()
        ),
    }
}

// Field widths differ between glibc and musl, so the casts are kept.
#[allow(clippy::unnecessary_cast)]
fn statvfs_now(path: &str) -> Result<Usage> {
    let c = CString::new(path).context("mount path contains NUL")?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: c is a valid NUL-terminated path; st is written by the kernel on success.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("statvfs {path}"));
    }
    // SAFETY: statvfs returned 0, so the struct is initialized.
    let st = unsafe { st.assume_init() };
    let frsize = if st.f_frsize > 0 {
        st.f_frsize as u64
    } else {
        st.f_bsize as u64
    };
    let blocks = st.f_blocks as u64;
    let bfree = st.f_bfree as u64;
    let bavail = st.f_bavail as u64;
    Ok(Usage {
        total: blocks.saturating_mul(frsize),
        used: blocks.saturating_sub(bfree).saturating_mul(frsize),
        avail: bavail.saturating_mul(frsize),
        inodes_total: st.f_files as u64,
        inodes_used: (st.f_files as u64).saturating_sub(st.f_ffree as u64),
        readonly: (st.f_flag as u64) & (libc::ST_RDONLY as u64) != 0,
    })
}

/// Level of one condition, ordered so higher is worse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warning,
    Critical,
}

impl Level {
    fn severity(self) -> Severity {
        match self {
            Level::Ok => Severity::Recovered,
            Level::Warning => Severity::Warning,
            Level::Critical => Severity::Critical,
        }
    }
}

#[derive(Clone, Debug)]
struct CondState {
    level: Level,
    last_sent: i64,
    last_value: f64,
    /// When the value first fell to a lower level; the level steps down
    /// only after it stays there for `ease_after`.
    lower_since: Option<i64>,
}

/// What counts as "clearly worse" for an early repeat.
#[derive(Clone, Copy, Debug)]
enum Worse {
    /// The value rose by this many points (percentages).
    Points(f64),
    /// The value fell to half (time left).
    Halves,
    /// The value doubled (bytes grown).
    Doubles,
}

/// What the sentinel decided to send this tick.
#[derive(Clone, Debug)]
pub struct Decision {
    pub alert: Alert,
    pub wall: bool,
}

pub struct Sentinel {
    pub settings: Settings,
    host: String,
    history: HashMap<String, VecDeque<(i64, u64)>>,
    conds: HashMap<String, CondState>,
    last_wall: HashMap<String, i64>,
    walls: VecDeque<i64>,
    sent: VecDeque<i64>,
    suppressed: usize,
    /// Alert keys allowed to broadcast with wall this tick: space or inode
    /// alerts on a disk that is critically full.
    wall_eligible: std::collections::HashSet<String>,
    was_readonly: HashMap<String, bool>,
}

fn hours_text(h: f64) -> String {
    if h < 1.0 {
        format!("about {:.0} min", (h * 60.0).max(1.0))
    } else if h < 48.0 {
        format!("about {h:.1} h")
    } else {
        format!("about {:.1} days", h / 24.0)
    }
}

fn h(b: u64) -> String {
    crate::util::human(b.min(i64::MAX as u64) as i64)
}

impl Sentinel {
    pub fn new(settings: Settings, host: String) -> Sentinel {
        Sentinel {
            settings,
            host,
            history: HashMap::new(),
            conds: HashMap::new(),
            last_wall: HashMap::new(),
            walls: VecDeque::new(),
            sent: VecDeque::new(),
            suppressed: 0,
            wall_eligible: Default::default(),
            was_readonly: HashMap::new(),
        }
    }

    /// Bytes per second over the window, using the oldest sample in it.
    fn growth(&self, mount: &str, now: i64) -> Option<(f64, u64, i64)> {
        let hist = self.history.get(mount)?;
        let (t0, u0) = *hist.front()?;
        let (t1, u1) = *hist.back()?;
        let span = t1 - t0;
        // Need at least a minute of history before trusting a rate.
        if span < 60 || t1 != now {
            return None;
        }
        let grew = u1.saturating_sub(u0);
        Some((grew as f64 / span as f64, grew, span))
    }

    /// Decide on one condition. Returns an alert when it should be sent.
    ///
    /// - Entering a worse level alerts at once.
    /// - The same level repeats after `warn_repeat`/`crit_repeat`, or sooner
    ///   when the value gets clearly worse, but never sooner than
    ///   `worsen_min_gap` after the previous alert.
    /// - Stepping down (critical to warning, or clearing) happens only after
    ///   the lower level has held for `ease_after`, so a value bouncing around
    ///   a threshold cannot flap.
    #[allow(clippy::too_many_arguments)]
    fn judge(
        &mut self,
        key: String,
        level: Level,
        value: f64,
        worse: Worse,
        title: String,
        body: String,
        now: i64,
    ) -> Option<Alert> {
        let s = &self.settings;
        let mut st = self.conds.get(&key).cloned().unwrap_or(CondState {
            level: Level::Ok,
            last_sent: 0,
            last_value: value,
            lower_since: None,
        });
        if level >= st.level {
            st.lower_since = None;
        }
        if level < st.level {
            let since = *st.lower_since.get_or_insert(now);
            if now - since < s.ease_after.as_secs() as i64 {
                self.conds.insert(key, st);
                return None;
            }
            let alert = Alert {
                severity: if level == Level::Ok {
                    Severity::Recovered
                } else {
                    level.severity()
                },
                title,
                body,
                key: key.clone(),
                host: self.host.clone(),
            };
            if level == Level::Ok {
                self.conds.remove(&key);
            } else {
                self.conds.insert(
                    key,
                    CondState {
                        level,
                        last_sent: now,
                        last_value: value,
                        lower_since: None,
                    },
                );
            }
            return Some(alert);
        }
        if level == Level::Ok {
            return None;
        }
        let escalated = level > st.level;
        let repeat = if level == Level::Critical {
            s.crit_repeat
        } else {
            s.warn_repeat
        };
        let clearly_worse = match worse {
            Worse::Points(p) => value >= st.last_value + p,
            Worse::Halves => value <= st.last_value / 2.0,
            Worse::Doubles => value >= st.last_value * 2.0,
        };
        let since_sent = now - st.last_sent;
        let worsened = clearly_worse && since_sent >= s.worsen_min_gap.as_secs() as i64;
        if escalated || since_sent >= repeat.as_secs() as i64 || worsened {
            self.conds.insert(
                key.clone(),
                CondState {
                    level,
                    last_sent: now,
                    last_value: value,
                    lower_since: None,
                },
            );
            Some(Alert {
                severity: level.severity(),
                title,
                body,
                key,
                host: self.host.clone(),
            })
        } else {
            self.conds.insert(key, st);
            None
        }
    }

    /// Run a health-probe finding through the same alert logic.
    pub(crate) fn feed(
        &mut self,
        f: crate::probes::Finding,
        doubles: bool,
        now: i64,
    ) -> Option<Alert> {
        let worse = if doubles {
            Worse::Doubles
        } else {
            Worse::Points(1.0)
        };
        self.judge(f.key, f.level, f.value, worse, f.title, f.body, now)
    }

    /// Hysteresis: once at a level, stay there until the value drops below
    /// the threshold by `hysteresis_pct` points.
    fn pct_level(&self, key: &str, pct: f64, warn: f64, crit: f64) -> Level {
        let current = self.conds.get(key).map(|c| c.level).unwrap_or(Level::Ok);
        let hy = self.settings.hysteresis_pct;
        if pct >= crit || (current == Level::Critical && pct > crit - hy) {
            Level::Critical
        } else if pct >= warn || (current >= Level::Warning && pct > warn - hy) {
            Level::Warning
        } else {
            Level::Ok
        }
    }

    /// Feed one sample for one mount; returns alerts to send.
    pub fn observe(&mut self, m: &Mount, u: &Usage, now: i64) -> Vec<Alert> {
        let mut out = Vec::new();
        let path = m.path.clone();

        // History for growth, trimmed to the window.
        let window = self.settings.window.as_secs() as i64;
        let hist = self.history.entry(path.clone()).or_default();
        hist.push_back((now, u.used));
        while hist.front().is_some_and(|&(t, _)| now - t > window) {
            hist.pop_front();
        }

        let pct = u.used_pct();
        let size_line = format!(
            "used {} of {} ({pct:.1}%), {} free",
            h(u.used),
            h(u.total),
            h(u.avail)
        );

        // space
        let key = format!("space:{path}");
        if pct >= self.settings.wall_pct {
            self.wall_eligible.insert(key.clone());
        }
        let lvl = self.pct_level(&key, pct, self.settings.warn_pct, self.settings.crit_pct);
        let title = if lvl == Level::Ok {
            format!("{path} is back to {pct:.0}% full")
        } else {
            format!("{path} is {pct:.0}% full")
        };
        if let Some(a) = self.judge(
            key,
            lvl,
            pct,
            Worse::Points(self.settings.worsen_pct),
            title,
            size_line.clone(),
            now,
        ) {
            out.push(a);
        }

        // inodes
        if let Some(ip) = u.inode_pct() {
            let key = format!("inodes:{path}");
            if ip >= self.settings.wall_pct {
                self.wall_eligible.insert(key.clone());
            }
            let lvl = self.pct_level(
                &key,
                ip,
                self.settings.inode_warn_pct,
                self.settings.inode_crit_pct,
            );
            let title = if lvl == Level::Ok {
                format!("{path} inodes back to {ip:.0}%")
            } else {
                format!("{path} is out of inodes soon: {ip:.0}% used")
            };
            let body = format!(
                "{} of {} inodes used; new files will fail when inodes run out even if space is free",
                u.inodes_used, u.inodes_total
            );
            if let Some(a) = self.judge(
                key,
                lvl,
                ip,
                Worse::Points(self.settings.worsen_pct),
                title,
                body,
                now,
            ) {
                out.push(a);
            }
        }

        // growth-based conditions
        if u.total >= self.settings.min_fs_bytes
            && (self.settings.fill_alerts || self.settings.burst_alerts)
        {
            if let Some((rate, grew, span)) = self.growth(&path, now) {
                // fill
                let key = format!("fill:{path}");
                let hours = if rate > 0.0 {
                    u.avail as f64 / rate / 3600.0
                } else {
                    f64::INFINITY
                };
                let current = self.conds.get(&key).map(|c| c.level).unwrap_or(Level::Ok);
                let enough_history = span >= self.settings.fill_warn_history.as_secs() as i64;
                let crit_history = span >= self.settings.fill_crit_history.as_secs() as i64;
                let lvl = if hours <= self.settings.fill_crit_hours
                    && (crit_history || current == Level::Critical)
                {
                    Level::Critical
                } else if (hours <= self.settings.fill_warn_hours
                    && (enough_history || current >= Level::Warning))
                    || (current >= Level::Warning && hours <= self.settings.fill_warn_hours * 1.5)
                {
                    Level::Warning
                } else {
                    Level::Ok
                };
                let lvl = if self.settings.fill_alerts {
                    lvl
                } else {
                    Level::Ok
                };
                let title = if lvl == Level::Ok {
                    format!("{path} is no longer on track to fill")
                } else {
                    format!("{path} will be full in {}", hours_text(hours))
                };
                let body = format!(
                    "growing {}/h over the last {} min; {size_line}",
                    h((rate * 3600.0) as u64),
                    span / 60
                );
                if let Some(a) = self.judge(key, lvl, hours, Worse::Halves, title, body, now) {
                    out.push(a);
                }

                // burst
                let key = format!("burst:{path}");
                let lvl = if grew >= self.settings.burst_bytes {
                    Level::Warning
                } else {
                    Level::Ok
                };
                let lvl = if self.settings.burst_alerts {
                    lvl
                } else {
                    Level::Ok
                };
                let title = if lvl == Level::Ok {
                    format!("{path} growth burst has stopped")
                } else {
                    format!("{path} grew {} in {} min", h(grew), span / 60)
                };
                let body = format!(
                    "burst threshold {} per {} min; {size_line}",
                    h(self.settings.burst_bytes),
                    window / 60
                );
                if let Some(a) = self.judge(key, lvl, grew as f64, Worse::Doubles, title, body, now)
                {
                    out.push(a);
                }
            }
        }

        // readonly: only a change from writable to read-only is an alert.
        let was = self.was_readonly.insert(path.clone(), u.readonly);
        let key = format!("readonly:{path}");
        match (was, u.readonly) {
            (Some(false), true) => {
                if let Some(a) = self.judge(key, Level::Critical, 1.0, Worse::Points(1.0),
                    format!("{path} has become read-only"),
                    format!("{} ({}) was writable and is now read-only, usually after filesystem errors; check the kernel log", m.device, m.fstype),
                    now,
                ) {
                    out.push(a);
                }
            }
            (_, false) if self.conds.contains_key(&key) => {
                if let Some(a) = self.judge(key, Level::Ok, 0.0, Worse::Points(1.0), format!("{path} is writable again"), String::new(), now) {
                    out.push(a);
                }
            }
            _ => {}
        }
        out
    }

    /// Global flood control and the wall decision.
    pub fn admit(&mut self, alerts: Vec<Alert>, now: i64) -> Vec<Decision> {
        while self.sent.front().is_some_and(|&t| now - t >= 3600) {
            self.sent.pop_front();
        }
        while self.walls.front().is_some_and(|&t| now - t >= 3600) {
            self.walls.pop_front();
        }
        let mut out = Vec::new();
        for mut a in alerts {
            // Criticals always go out; others respect the hourly cap.
            if a.severity != Severity::Critical
                && self.sent.len() >= self.settings.max_alerts_per_hour
            {
                self.suppressed += 1;
                continue;
            }
            if self.suppressed > 0 {
                a.body.push_str(&format!(
                    "\n({} lower-priority alerts were held back in the last hour)",
                    self.suppressed
                ));
                self.suppressed = 0;
            }
            self.sent.push_back(now);
            let mut wall = false;
            if a.severity == Severity::Critical && self.wall_eligible.contains(&a.key) {
                let mount = a
                    .key
                    .split_once(':')
                    .map(|(_, m)| m.to_string())
                    .unwrap_or_default();
                let gap_ok = self
                    .last_wall
                    .get(&mount)
                    .is_none_or(|&t| now - t >= self.settings.wall_min_gap.as_secs() as i64);
                if gap_ok && self.walls.len() < self.settings.wall_max_per_hour {
                    wall = true;
                    self.last_wall.insert(mount, now);
                    self.walls.push_back(now);
                }
            }
            out.push(Decision { alert: a, wall });
        }
        self.wall_eligible.clear();
        out
    }
}

/// Network and FUSE filesystems: one remote disk is often mounted several
/// times, and each mount gets its own device number.
pub fn is_remote(fstype: &str) -> bool {
    fstype.starts_with("fuse")
        || matches!(
            fstype,
            "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "9p" | "ceph" | "glusterfs" | "sshfs"
        )
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn now_secs() -> i64 {
    crate::util::now_secs()
}

fn mounts(settings: &Settings) -> Result<Vec<Mount>> {
    let text =
        std::fs::read_to_string("/proc/self/mountinfo").context("reading /proc/self/mountinfo")?;
    let mut ms = parse_mountinfo(&text);
    if !settings.only_mounts.is_empty() {
        ms.retain(|m| settings.only_mounts.contains(&m.path));
    }
    Ok(ms)
}

/// Whether a mount answers its usage check: a warning while it hangs (a
/// network filesystem whose server is gone), Ok when it answers again.
fn responding(path: &str, error: Option<&str>) -> crate::probes::Finding {
    crate::probes::Finding {
        key: format!("unresponsive:{path}"),
        level: if error.is_some() { Level::Warning } else { Level::Ok },
        value: 0.0,
        title: match error {
            Some(_) => format!("{path} is not responding"),
            None => format!("{path} is responding again"),
        },
        body: error.map_or_else(
            || "usage checks return normally".to_string(),
            |e| format!("{e}. Programs that touch it will hang; check the server or unmount it (umount -l {path})."),
        ),
    }
}

/// One line per mount, like a compact `df`, for `--once`.
pub fn snapshot_report(settings: &Settings) -> Result<String> {
    let mut s = format!(
        "{:<24} {:>10} {:>10} {:>10} {:>6} {:>7}\n",
        "MOUNT", "SIZE", "USED", "AVAIL", "USE%", "INODE%"
    );
    for m in mounts(settings)? {
        match statvfs(&m.path) {
            Ok(u) => s.push_str(&format!(
                "{:<24} {:>10} {:>10} {:>10} {:>5.1}% {:>7}\n",
                m.path,
                h(u.total),
                h(u.used),
                h(u.avail),
                u.used_pct(),
                u.inode_pct()
                    .map(|p| format!("{p:.1}%"))
                    .unwrap_or_else(|| "-".into())
            )),
            Err(e) => s.push_str(&format!("{:<24} error: {e:#}\n", m.path)),
        }
    }
    Ok(s)
}

/// Run until stopped. With `once`, take one sample, alert on thresholds
/// (growth needs history, so it is skipped) and exit.
pub fn run(cfg: &NotifyConfig, settings: Settings, once: bool) -> Result<()> {
    use crate::attrib::{self, WalkScheduler, Writers};
    let host = hostname();
    tracing::info!(
        channels = ?cfg.channels(),
        interval_s = settings.interval.as_secs(),
        warn_pct = settings.warn_pct,
        crit_pct = settings.crit_pct,
        walk = settings.walk,
        "dux sentinel started"
    );
    let mut mount_list = mounts(&settings)?;
    let walk_mounts = |ms: &[Mount]| -> Vec<String> {
        ms.iter()
            .filter(|m| attrib::walkable(&m.fstype))
            .map(|m| m.path.clone())
            .collect()
    };
    let mut sched = (settings.walk && !once && !cfg.dry_run).then(|| {
        let mut s = WalkScheduler::new(
            settings.walk_every.as_secs() as i64,
            settings.walk_min_gap.as_secs() as i64,
            settings.walk_trigger_bytes,
            settings.walk_threads,
            Some(settings.snapshot_dir.clone()),
        );
        s.load_snapshots(&walk_mounts(&mount_list));
        s
    });
    let mut writers = Writers::default();
    let mut last_writers = 0i64;
    let mut latency = crate::probes::Latency::default();
    let disks = crate::probes::physical_disks();
    let (mut last_probe, mut last_smart, mut last_cleanup) = (0i64, 0i64, 0i64);
    let mut sentinel = Sentinel::new(settings, host.clone());
    let mut last_mount_refresh = now_secs();
    crate::util::systemd_notify("READY=1\nSTATUS=watching free space and inodes");
    loop {
        let now = now_secs();
        if now - last_mount_refresh >= 60 {
            if let Ok(ms) = mounts(&sentinel.settings) {
                mount_list = ms;
            }
            last_mount_refresh = now;
        }
        let mut alerts = Vec::new();
        let mut used: Vec<(String, u64)> = Vec::new();
        let mut seen_remote: Vec<(u64, u64, u64, u64)> = Vec::new();
        for m in &mount_list {
            match statvfs(&m.path) {
                Ok(u)
                    if is_remote(&m.fstype) && {
                        let sig = (u.total, u.used, u.inodes_total, u.inodes_used);
                        let dup = seen_remote.contains(&sig);
                        seen_remote.push(sig);
                        dup
                    } =>
                {
                    // Another mount of the same remote filesystem: report it once.
                }
                Ok(u) => {
                    if attrib::walkable(&m.fstype) {
                        used.push((m.path.clone(), u.used));
                    }
                    alerts.extend(sentinel.observe(m, &u, now));
                    alerts.extend(sentinel.feed(responding(&m.path, None), false, now));
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("not responding") {
                        alerts.extend(sentinel.feed(responding(&m.path, Some(&msg)), false, now));
                    } else {
                        tracing::debug!("skipping {}: {msg}", m.path);
                    }
                }
            }
        }
        if now - last_writers >= 10 {
            writers.sample(now);
            last_writers = now;
        }
        if now - last_probe >= 60 {
            last_probe = now;
            let mut findings = crate::probes::ext4_errors("/sys");
            if let Ok(t) = std::fs::read_to_string("/proc/mdstat") {
                findings.extend(crate::probes::mdstat(&t));
            }
            let mut doubling = Vec::new();
            if let Ok(t) = std::fs::read_to_string("/proc/diskstats") {
                doubling.extend(latency.sample(&t, 50.0, 250.0));
            }
            if let Ok(t) = std::fs::read_to_string("/proc/pressure/io") {
                doubling.extend(crate::probes::psi("io", &t, 30.0, 60.0));
            }
            if let Ok(t) = std::fs::read_to_string("/proc/pressure/memory") {
                doubling.extend(crate::probes::psi("memory", &t, 20.0, 50.0));
            }
            if now - last_smart >= 3600 {
                last_smart = now;
                findings.extend(crate::probes::smart(&disks));
            }
            for f in findings {
                alerts.extend(sentinel.feed(f, false, now));
            }
            for f in doubling {
                alerts.extend(sentinel.feed(f, true, now));
            }
        }
        let level = sentinel.settings.auto_cleanup_level;
        // Automatic cleanup deletes files: never in a dry run.
        if level > 0 && !cfg.dry_run && now - last_cleanup >= 600 {
            let log_mount = mount_list
                .iter()
                .filter(|m| m.path == "/" || under("/var/log", &m.path))
                .max_by_key(|m| m.path.len())
                .map(|m| m.path.clone());
            let critical_here = log_mount.as_ref().is_some_and(|lm| {
                alerts
                    .iter()
                    .any(|a| a.severity == Severity::Critical && a.key == format!("space:{lm}"))
            });
            if critical_here {
                last_cleanup = now;
                let cfg2 = cfg.clone();
                let host2 = host.clone();
                let target = sentinel.settings.crit_pct - 10.0;
                if once {
                    // --once exits right after this pass: finish the cleanup first.
                    emergency_cleanup(&cfg2, &host2, level, target);
                } else {
                    let _ = std::thread::Builder::new()
                        .name("dux-cleanup".into())
                        .spawn(move || {
                            emergency_cleanup(&cfg2, &host2, level, target);
                        });
                }
            }
        }
        if let Some(s) = sched.as_mut() {
            for a in &alerts {
                if let Some((_, mount)) = a.key.split_once(':') {
                    s.note_alert(mount, a.severity);
                }
            }
            s.poll(now, &used);
            if let Some((mount, reason, tree, changes)) = s.collect(now) {
                let used_now = used.iter().find(|(m, _)| *m == mount).map(|(_, u)| *u);
                if let Some(a) = attrib::attribution_alert(
                    &host,
                    &mount,
                    reason,
                    &tree,
                    &changes,
                    used_now,
                    &writers.summary(3),
                ) {
                    alerts.push(a);
                }
            }
        }
        for d in sentinel.admit(alerts, now) {
            if once || cfg.dry_run {
                let r = notify::send(cfg, &d.alert, d.wall, now);
                if cfg.dry_run {
                    println!(
                        "{}\n  would send to: {:?}{}",
                        notify::plain_text(&d.alert),
                        r.skipped,
                        if d.wall { " (including wall)" } else { "" }
                    );
                }
            } else {
                notify::send_async(cfg, d.alert, d.wall, now);
            }
        }
        crate::util::systemd_notify("WATCHDOG=1");
        if once {
            return Ok(());
        }
        std::thread::sleep(sentinel.settings.interval);
    }
}

/// Run cleanup up to `level`, safest first, stopping once the disk holding
/// /var/log is at or below `target_pct`; report what was done.
fn emergency_cleanup(cfg: &NotifyConfig, host: &str, level: u8, target_pct: f64) {
    let Ok(before) = statvfs("/var/log") else {
        return;
    };
    let opts = crate::cleanup::Options {
        level,
        used_pct: before.used_pct(),
        ..crate::cleanup::Options::default()
    };
    let mut done = Vec::new();
    for item in crate::cleanup::plan(&opts) {
        if statvfs("/var/log")
            .map(|u| u.used_pct() <= target_pct)
            .unwrap_or(true)
        {
            break;
        }
        match crate::cleanup::apply(&item) {
            Ok(()) => done.push(format!("L{} {}", item.level, item.what)),
            Err(e) => tracing::warn!("emergency cleanup skipped {}: {e:#}", item.what),
        }
    }
    let after = statvfs("/var/log").unwrap_or(before);
    let freed = before.used.saturating_sub(after.used);
    let alert = Alert {
        severity: Severity::Warning,
        title: format!("dux freed {} automatically (level {level})", h(freed)),
        body: format!(
            "disk now {:.1}% full; {} actions:\n{}",
            after.used_pct(),
            done.len(),
            done.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
        ),
        key: "cleanup:/var/log".into(),
        host: host.to_string(),
    };
    notify::send(cfg, &alert, false, now_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn mount(p: &str) -> Mount {
        Mount {
            path: p.into(),
            fstype: "ext4".into(),
            device: "/dev/sda1".into(),
            dev_id: "8:1".into(),
        }
    }

    fn usage(total_gib: u64, used_gib: f64) -> Usage {
        let total = total_gib * GIB;
        let used = (used_gib * GIB as f64) as u64;
        Usage {
            total,
            used,
            avail: total - used,
            inodes_total: 1000,
            inodes_used: 100,
            readonly: false,
        }
    }

    fn sent(s: &mut Sentinel, m: &Mount, u: &Usage, now: i64) -> Vec<(Severity, String)> {
        let a = s.observe(m, u, now);
        s.admit(a, now)
            .into_iter()
            .map(|d| (d.alert.severity, d.alert.key))
            .collect()
    }

    #[test]
    fn mountinfo_skips_pseudo_and_dedupes_bind_mounts() {
        let text = "\
22 1 252:0 / / rw,relatime shared:1 - ext4 /dev/mapper/vg-root rw\n\
23 22 0:21 / /proc rw - proc proc rw\n\
24 22 0:5 / /dev rw - devtmpfs udev rw\n\
25 22 0:30 / /run rw - tmpfs tmpfs rw\n\
26 22 0:31 / /run/user/0 rw - tmpfs tmpfs rw\n\
27 22 7:0 / /snap/core/1 ro - squashfs /dev/loop0 ro\n\
28 22 252:0 /srv /mnt/srv\\040copy rw - ext4 /dev/mapper/vg-root rw\n\
29 22 8:1 / /boot rw - ext4 /dev/sda1 rw\n\
30 22 0:50 / /mnt/nas rw - nfs4 nas:/export rw\n";
        let ms = parse_mountinfo(text);
        let paths: Vec<&str> = ms.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["/", "/run", "/boot", "/mnt/nas"]);
        assert_eq!(unescape_mount("/mnt/srv\\040copy"), "/mnt/srv copy");
    }

    #[test]
    fn used_pct_matches_df() {
        // df: 142G used, 35G avail -> 81%
        let u = Usage {
            total: 185 * GIB,
            used: 142 * GIB,
            avail: 35 * GIB,
            inodes_total: 0,
            inodes_used: 0,
            readonly: false,
        };
        assert!((u.used_pct() - 80.2).abs() < 0.1);
        assert_eq!(u.inode_pct(), None);
    }

    #[test]
    fn parse_sizes() {
        assert_eq!(parse_bytes("5G").unwrap(), 5 * GIB);
        assert_eq!(parse_bytes("500MiB").unwrap(), 500 << 20);
        assert_eq!(parse_bytes("1024").unwrap(), 1024);
        assert_eq!(parse_bytes("1.5k").unwrap(), 1536);
        assert!(parse_bytes("5Q").is_err());
    }

    #[test]
    fn space_alerts_escalate_repeat_and_recover_with_hysteresis() {
        // Growth checks off: this test jumps usage between samples on purpose.
        let st = Settings {
            min_fs_bytes: u64::MAX,
            ..Settings::default()
        };
        let mut s = Sentinel::new(st, "h".into());
        let m = mount("/");
        let one = |s: &mut Sentinel, pct: f64, t: i64| sent(s, &m, &usage(100, pct), t);
        assert!(one(&mut s, 50.0, 0).is_empty());
        assert_eq!(
            one(&mut s, 86.0, 2),
            vec![(Severity::Warning, "space:/".into())]
        );
        assert!(one(&mut s, 87.0, 4).is_empty());
        // Two points worse, but within the 5-minute minimum gap: quiet.
        assert!(one(&mut s, 88.1, 6).is_empty());
        assert_eq!(
            one(&mut s, 88.5, 310).len(),
            1,
            "worse and past the minimum gap"
        );
        // Escalates to critical immediately.
        assert_eq!(
            one(&mut s, 96.0, 312),
            vec![(Severity::Critical, "space:/".into())]
        );
        assert!(one(&mut s, 96.0, 600).is_empty());
        assert_eq!(
            one(&mut s, 96.0, 312 + 900).len(),
            1,
            "critical repeats after 15 min"
        );
        // 93% is inside the 3-point band below 95: still critical.
        assert!(one(&mut s, 93.0, 1300).is_empty());
        // 91% starts the 2-minute step-down hold; bouncing back up is silent.
        assert!(one(&mut s, 91.0, 1302).is_empty());
        assert!(one(&mut s, 96.0, 1350).is_empty(), "no flapping alert");
        assert!(one(&mut s, 91.0, 1400).is_empty());
        assert_eq!(
            one(&mut s, 91.0, 1521),
            vec![(Severity::Warning, "space:/".into())],
            "eased after holding 2 min"
        );
        assert!(one(&mut s, 83.0, 1523).is_empty());
        assert!(
            one(&mut s, 80.0, 1525).is_empty(),
            "recovery waits for the hold too"
        );
        assert_eq!(
            one(&mut s, 80.0, 1700),
            vec![(Severity::Recovered, "space:/".into())]
        );
        assert!(one(&mut s, 80.0, 1702).is_empty());
    }

    #[test]
    fn fill_and_burst_need_history_then_fire() {
        let st = Settings {
            fill_alerts: true,
            burst_alerts: true,
            ..Settings::default()
        };
        let mut s = Sentinel::new(st, "h".into());
        let m = mount("/data");
        // 1000 GiB disk, 50% used, growing 6 GiB per 2 minutes = 180 GiB/h.
        let mut used = 500.0;
        let mut keys = Vec::new();
        for t in (0..=320).step_by(2) {
            keys.extend(sent(&mut s, &m, &usage(1000, used), t));
            used += 0.1;
        }
        // 500 GiB free at 180 GiB/h is under 3 h: fill warning, plus a burst (6 GiB > 5 GiB).
        assert!(
            keys.contains(&(Severity::Warning, "fill:/data".into())),
            "{keys:?}"
        );
        assert!(
            keys.contains(&(Severity::Warning, "burst:/data".into())),
            "{keys:?}"
        );
        assert!(!keys.iter().any(|(_, k)| k.starts_with("space:")));
    }

    #[test]
    fn fill_goes_critical_when_close() {
        let st = Settings {
            fill_alerts: true,
            ..Settings::default()
        };
        let mut s = Sentinel::new(st, "h".into());
        let m = mount("/");
        // 100 GiB disk at 70%: 30 GiB free, growing 0.1 GiB per 2 s = 180 GiB/h -> minutes.
        let mut out = Vec::new();
        let mut used = 70.0;
        for t in (0..=62).step_by(2) {
            out.extend(sent(&mut s, &m, &usage(100, used), t));
            used += 0.1;
        }
        assert!(
            !out.iter().any(|(_, k)| k.starts_with("fill:")),
            "one minute of history is not enough: {out:?}"
        );
        for t in (64..=184).step_by(2) {
            out.extend(sent(&mut s, &m, &usage(100, used), t));
            used += 0.1;
        }
        assert!(
            out.contains(&(Severity::Critical, "fill:/".into())),
            "{out:?}"
        );
    }

    #[test]
    fn remote_fstypes() {
        assert!(is_remote("fuse.sshfs") && is_remote("nfs4") && is_remote("cifs"));
        assert!(!is_remote("ext4") && !is_remote("xfs") && !is_remote("tmpfs"));
    }

    #[test]
    fn mount_skips_use_path_boundaries() {
        assert!(under("/proc", "/proc") && under("/proc/fs/nfsd", "/proc"));
        assert!(!under("/processing", "/proc"));
        assert!(!under("/sysbackup", "/sys"));
        assert!(under("/var/log", "/var") && !under("/var/log", "/var/l"));
    }

    #[test]
    fn wall_only_for_a_critically_full_disk_never_for_projections() {
        // Defaults: fill and burst alerts are off entirely.
        let mut s = Sentinel::new(Settings::default(), "h".into());
        let m = mount("/");
        let mut out = Vec::new();
        let mut used = 70.0;
        for t in (0..=400).step_by(2) {
            out.extend(
                {
                    let a = s.observe(&m, &usage(100, used), t);
                    s.admit(a, t)
                }
                .into_iter()
                .map(|d| (d.alert.key, d.wall)),
            );
            used += 0.02;
        }
        assert!(
            !out.iter()
                .any(|(k, _)| k.starts_with("fill:") || k.starts_with("burst:")),
            "{out:?}"
        );
        // 95% critical: alert, but no broadcast below 97%.
        let d = {
            let a = s.observe(&m, &usage(100, 95.5), 500);
            s.admit(a, 500)
        };
        assert!(d.iter().any(|x| x.alert.key == "space:/"
            && x.alert.severity == Severity::Critical
            && !x.wall));
        // 97%+: broadcast.
        let d = {
            let a = s.observe(&m, &usage(100, 98.0), 1500);
            s.admit(a, 1500)
        };
        assert!(
            d.iter().any(|x| x.alert.key == "space:/" && x.wall),
            "{d:?}"
        );
    }

    #[test]
    fn readonly_transition_is_critical_once() {
        let mut s = Sentinel::new(Settings::default(), "h".into());
        let m = mount("/");
        let mut u = usage(100, 10.0);
        assert!(sent(&mut s, &m, &u, 0).is_empty());
        u.readonly = true;
        assert_eq!(
            sent(&mut s, &m, &u, 2),
            vec![(Severity::Critical, "readonly:/".into())]
        );
        assert!(sent(&mut s, &m, &u, 4).is_empty());
        u.readonly = false;
        assert!(
            sent(&mut s, &m, &u, 6).is_empty(),
            "recovery waits for the hold"
        );
        assert_eq!(
            sent(&mut s, &m, &u, 130),
            vec![(Severity::Recovered, "readonly:/".into())]
        );
        // A filesystem mounted read-only from the start never alerts.
        let mut s = Sentinel::new(Settings::default(), "h".into());
        u.readonly = true;
        assert!(sent(&mut s, &m, &u, 0).is_empty());
    }

    #[test]
    fn wall_is_rate_limited_per_mount_and_per_hour() {
        let mut s = Sentinel::new(Settings::default(), "h".into());
        let crit = |k: &str| Alert {
            severity: Severity::Critical,
            title: "t".into(),
            body: String::new(),
            key: k.into(),
            host: "h".into(),
        };
        let elig = |s: &mut Sentinel, k: &str| {
            s.wall_eligible.insert(k.to_string());
        };
        elig(&mut s, "space:/");
        let d = s.admit(vec![crit("space:/")], 0);
        assert!(d[0].wall);
        // Same mount within 30 min: alert goes out, wall does not.
        let d = s.admit(vec![crit("fill:/")], 600);
        assert!(!d[0].wall);
        elig(&mut s, "space:/");
        assert!(s.admit(vec![crit("space:/")], 1800)[0].wall);
        // Hourly cap of 4 walls across mounts.
        elig(&mut s, "space:/a");
        assert!(s.admit(vec![crit("space:/a")], 1801)[0].wall);
        elig(&mut s, "space:/b");
        assert!(s.admit(vec![crit("space:/b")], 1802)[0].wall);
        assert!(
            !s.admit(vec![crit("space:/c")], 1803)[0].wall,
            "fifth wall within an hour"
        );
        // Warnings never trigger wall.
        let w = Alert {
            severity: Severity::Warning,
            ..crit("space:/e")
        };
        assert!(!s.admit(vec![w], 9000)[0].wall);
    }

    #[test]
    fn hourly_cap_holds_back_warnings_but_never_criticals() {
        let st = Settings {
            max_alerts_per_hour: 2,
            ..Settings::default()
        };
        let mut s = Sentinel::new(st, "h".into());
        let a = |sev, k: &str| Alert {
            severity: sev,
            title: "t".into(),
            body: String::new(),
            key: k.into(),
            host: "h".into(),
        };
        let d = s.admit(
            vec![
                a(Severity::Warning, "x:1"),
                a(Severity::Warning, "x:2"),
                a(Severity::Warning, "x:3"),
            ],
            0,
        );
        assert_eq!(d.len(), 2);
        let d = s.admit(vec![a(Severity::Critical, "x:4")], 1);
        assert_eq!(d.len(), 1);
        assert!(d[0]
            .alert
            .body
            .contains("1 lower-priority alerts were held back"));
    }

    #[test]
    fn settings_from_config_validate() {
        let mut c = NotifyConfig::default();
        c.raw.insert("DUX_WARN_PCT".into(), "80%".into());
        c.raw.insert("DUX_BURST".into(), "2G".into());
        c.raw.insert("DUX_WALL_MIN_GAP".into(), "10m".into());
        c.raw.insert("DUX_MOUNTS".into(), "/, /var".into());
        let s = Settings::from_config(&c).unwrap();
        assert_eq!(s.warn_pct, 80.0);
        assert_eq!(s.burst_bytes, 2 * GIB);
        assert_eq!(s.wall_min_gap, Duration::from_secs(600));
        assert_eq!(s.only_mounts, vec!["/", "/var"]);
        c.raw.insert("DUX_WARN_PCT".into(), "99".into());
        assert!(
            Settings::from_config(&c).is_err(),
            "warn above crit must be rejected"
        );
    }

    #[test]
    fn statvfs_on_root_works() {
        let u = statvfs("/").unwrap();
        assert!(u.total > 0 && u.used <= u.total);
    }
}
