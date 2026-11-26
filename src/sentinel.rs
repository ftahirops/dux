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
    pub wall_min_gap: Duration,
    pub wall_max_per_hour: usize,
    pub max_alerts_per_hour: usize,
    /// Only these mount points, if set.
    pub only_mounts: Vec<String>,
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
            wall_min_gap: Duration::from_secs(1800),
            wall_max_per_hour: 4,
            max_alerts_per_hour: 30,
            only_mounts: Vec::new(),
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
        s.wall_min_gap = dur("DUX_WALL_MIN_GAP", s.wall_min_gap)?;
        s.wall_max_per_hour = num("DUX_WALL_MAX_PER_HOUR", s.wall_max_per_hour as f64)? as usize;
        s.max_alerts_per_hour =
            num("DUX_MAX_ALERTS_PER_HOUR", s.max_alerts_per_hour as f64)? as usize;
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
fn unescape_mount(s: &str) -> String {
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
        if SKIP_FSTYPES.contains(&fstype.as_str())
            || path.starts_with("/proc")
            || path.starts_with("/sys")
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

// Field widths differ between glibc and musl, so the casts are kept.
#[allow(clippy::unnecessary_cast)]
pub fn statvfs(path: &str) -> Result<Usage> {
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
    #[allow(clippy::too_many_arguments)]
    fn judge(
        &mut self,
        key: String,
        level: Level,
        value: f64,
        worse_is_higher: bool,
        title: String,
        body: String,
        now: i64,
    ) -> Option<Alert> {
        let s = &self.settings;
        let prev = self.conds.get(&key).cloned().unwrap_or(CondState {
            level: Level::Ok,
            last_sent: 0,
            last_value: value,
        });
        let send = if level > prev.level {
            true
        } else if level == Level::Ok {
            false
        } else if level < prev.level {
            // Dropped from critical to warning: tell people it eased.
            true
        } else {
            let repeat = if level == Level::Critical {
                s.crit_repeat
            } else {
                s.warn_repeat
            };
            let worse = if worse_is_higher {
                value >= prev.last_value + s.worsen_pct
            } else {
                value <= prev.last_value / 2.0
            };
            now - prev.last_sent >= repeat.as_secs() as i64 || worse
        };
        let recovered = level == Level::Ok && prev.level > Level::Ok;
        if recovered {
            self.conds.remove(&key);
            return Some(Alert {
                severity: Severity::Recovered,
                title,
                body,
                key,
                host: self.host.clone(),
            });
        }
        if level == Level::Ok {
            return None;
        }
        if send {
            self.conds.insert(
                key.clone(),
                CondState {
                    level,
                    last_sent: now,
                    last_value: value,
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
            // Keep the level, remember the old send time and value.
            self.conds.insert(key, CondState { level, ..prev });
            None
        }
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
        let lvl = self.pct_level(&key, pct, self.settings.warn_pct, self.settings.crit_pct);
        let title = if lvl == Level::Ok {
            format!("{path} is back to {pct:.0}% full")
        } else {
            format!("{path} is {pct:.0}% full")
        };
        if let Some(a) = self.judge(key, lvl, pct, true, title, size_line.clone(), now) {
            out.push(a);
        }

        // inodes
        if let Some(ip) = u.inode_pct() {
            let key = format!("inodes:{path}");
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
            if let Some(a) = self.judge(key, lvl, ip, true, title, body, now) {
                out.push(a);
            }
        }

        // growth-based conditions
        if u.total >= self.settings.min_fs_bytes {
            if let Some((rate, grew, span)) = self.growth(&path, now) {
                // fill
                let key = format!("fill:{path}");
                let hours = if rate > 0.0 {
                    u.avail as f64 / rate / 3600.0
                } else {
                    f64::INFINITY
                };
                let current = self.conds.get(&key).map(|c| c.level).unwrap_or(Level::Ok);
                let lvl = if hours <= self.settings.fill_crit_hours {
                    Level::Critical
                } else if hours <= self.settings.fill_warn_hours
                    || (current >= Level::Warning && hours <= self.settings.fill_warn_hours * 1.5)
                {
                    Level::Warning
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
                if let Some(a) = self.judge(key, lvl, hours, false, title, body, now) {
                    out.push(a);
                }

                // burst
                let key = format!("burst:{path}");
                let lvl = if grew >= self.settings.burst_bytes {
                    Level::Warning
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
                if let Some(a) = self.judge(key, lvl, grew as f64, true, title, body, now) {
                    out.push(a);
                }
            }
        }

        // readonly: only a change from writable to read-only is an alert.
        let was = self.was_readonly.insert(path.clone(), u.readonly);
        let key = format!("readonly:{path}");
        match (was, u.readonly) {
            (Some(false), true) => {
                if let Some(a) = self.judge(
                    key,
                    Level::Critical,
                    1.0,
                    true,
                    format!("{path} has become read-only"),
                    format!("{} ({}) was writable and is now read-only, usually after filesystem errors; check the kernel log", m.device, m.fstype),
                    now,
                ) {
                    out.push(a);
                }
            }
            (Some(true), false) if self.conds.contains_key(&key) => {
                if let Some(a) = self.judge(key, Level::Ok, 0.0, true, format!("{path} is writable again"), String::new(), now) {
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
            if a.severity == Severity::Critical {
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
        out
    }
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
    let host = hostname();
    tracing::info!(
        channels = ?cfg.channels(),
        interval_s = settings.interval.as_secs(),
        warn_pct = settings.warn_pct,
        crit_pct = settings.crit_pct,
        "dux sentinel started"
    );
    let mut sentinel = Sentinel::new(settings, host);
    let mut mount_list = mounts(&sentinel.settings)?;
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
        for m in &mount_list {
            match statvfs(&m.path) {
                Ok(u) => alerts.extend(sentinel.observe(m, &u, now)),
                Err(e) => tracing::debug!("skipping {}: {e:#}", m.path),
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
        assert!(sent(&mut s, &m, &usage(100, 50.0), 0).is_empty());
        assert_eq!(
            sent(&mut s, &m, &usage(100, 86.0), 2),
            vec![(Severity::Warning, "space:/".into())]
        );
        // Same level, not worse enough, too soon: quiet.
        assert!(sent(&mut s, &m, &usage(100, 87.0), 4).is_empty());
        // Two points worse: resend early.
        assert_eq!(sent(&mut s, &m, &usage(100, 88.1), 6).len(), 1);
        // Escalates to critical immediately.
        assert_eq!(
            sent(&mut s, &m, &usage(100, 96.0), 8),
            vec![(Severity::Critical, "space:/".into())]
        );
        // Critical repeats after 15 minutes, not before.
        assert!(sent(&mut s, &m, &usage(100, 96.0), 600).is_empty());
        assert_eq!(sent(&mut s, &m, &usage(100, 96.0), 8 + 900).len(), 1);
        // 93% is inside the 3-point hysteresis band below 95: still critical, no new alert.
        assert!(sent(&mut s, &m, &usage(100, 93.0), 1000).is_empty());
        // 91% drops to warning: one "eased" alert.
        assert_eq!(
            sent(&mut s, &m, &usage(100, 91.0), 1002),
            vec![(Severity::Warning, "space:/".into())]
        );
        // 83% is inside the band below 85: still warning.
        assert!(sent(&mut s, &m, &usage(100, 83.0), 1004).is_empty());
        // 80% clears: one recovery.
        assert_eq!(
            sent(&mut s, &m, &usage(100, 80.0), 1006),
            vec![(Severity::Recovered, "space:/".into())]
        );
        assert!(sent(&mut s, &m, &usage(100, 80.0), 1008).is_empty());
    }

    #[test]
    fn fill_and_burst_need_history_then_fire() {
        let mut s = Sentinel::new(Settings::default(), "h".into());
        let m = mount("/data");
        // 1000 GiB disk, 50% used, growing 6 GiB per 2 minutes = 180 GiB/h.
        let mut used = 500.0;
        let mut keys = Vec::new();
        for t in (0..=120).step_by(2) {
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
        let mut s = Sentinel::new(Settings::default(), "h".into());
        let m = mount("/");
        // 100 GiB disk at 90%: 10 GiB free, growing 0.1 GiB per 2 s = 180 GiB/h -> minutes.
        let mut out = Vec::new();
        let mut used = 90.0;
        for t in (0..=62).step_by(2) {
            out.extend(sent(&mut s, &m, &usage(100, used), t));
            used += 0.1;
        }
        assert!(
            out.contains(&(Severity::Critical, "fill:/".into())),
            "{out:?}"
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
        assert_eq!(
            sent(&mut s, &m, &u, 6),
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
        let d = s.admit(vec![crit("space:/")], 0);
        assert!(d[0].wall);
        // Same mount within 30 min: alert goes out, wall does not.
        let d = s.admit(vec![crit("fill:/")], 600);
        assert!(!d[0].wall);
        assert!(s.admit(vec![crit("space:/")], 1800)[0].wall);
        // Hourly cap of 4 walls across mounts.
        assert!(s.admit(vec![crit("space:/a")], 1801)[0].wall);
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
