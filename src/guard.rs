//! Resource guardian — dux is a background observability tool and should limit
//! its resource impact. This module samples system pressure (free memory,
//! CPU load, kernel PSI for memory/io/cpu, and free disk) and classifies it into
//! Normal / Elevated / Critical so the daemon can self-throttle: back off under
//! Elevated, and fully PAUSE its own writes under Critical. A one-off direct
//! scan opts in as a preferred OOM victim; the long-lived daemon does not
//! override systemd's score, so its memory is weighed normally.

use std::path::Path;
use std::time::{Duration, Instant};

/// Free-memory floor: at or below this available memory we treat the host as
/// memory-critical and pause writes (mirrors the disk floor in watch.rs).
pub const MIN_AVAIL_MEM: i64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    Elevated,
    Critical,
}

/// CPU-only or moderate-I/O Elevated pressure may schedule a low-priority
/// recovery scan. Otherwise a persistently busy host can leave an uncertain
/// index stale forever even when memory and storage have ample headroom.
pub fn may_start_reconciliation_with_health(
    pressure: Pressure,
    health: &Health,
    disk_floor: i64,
) -> bool {
    if health.telemetry_unavailable || health.level(disk_floor) == Pressure::Critical {
        return false;
    }
    match pressure {
        Pressure::Normal => true,
        Pressure::Elevated => {
            health.level(disk_floor) == Pressure::Elevated
                && matches!(health.reason(disk_floor), "cpu pressure" | "io pressure")
        }
        Pressure::Critical => false,
    }
}

/// CPU or moderate I/O PSI can rise because the low-priority reconciliation
/// itself is being throttled. Moderate memory PSI can also rise while the host
/// still has ample available memory. Cancelling in either case discards scan
/// work and can create a repeated scan/cancel loop. Keep an active scan running
/// for those signals: the scanner is nice'd and uses the idle I/O class, while
/// optional work is already suppressed. Low available memory, high load, and
/// hard Critical pressure still stop it. Starting a new scan remains more
/// restrictive (`may_start_reconciliation_with_health`).
pub fn should_cancel_reconciliation(health: &Health, disk_floor: i64) -> bool {
    match health.level(disk_floor) {
        Pressure::Normal => false,
        Pressure::Elevated => {
            let reason = health.reason(disk_floor);
            !matches!(reason, "cpu pressure" | "io pressure")
                && !(reason == "memory pressure"
                    && health.mem_avail > health.elevated_memory_floor())
        }
        Pressure::Critical => true,
    }
}

/// A point-in-time snapshot of host resource pressure.
#[derive(Clone, Copy)]
pub struct Health {
    pub mem_avail: i64,  // bytes (MemAvailable), 0 if unknown
    pub mem_total: i64,  // bytes (MemTotal), 0 if unknown
    pub disk_avail: i64, // bytes available on the index filesystem
    pub load1: f64,      // 1-minute load average
    pub ncpu: f64,
    pub psi_mem10: f64, // /proc/pressure "some avg10" %, 0 if unavailable
    pub psi_io10: f64,
    pub psi_cpu10: f64,
    /// Required host metrics could not be read or parsed. Unknown capacity or
    /// load must never be interpreted as a healthy host for a full scan.
    pub telemetry_unavailable: bool,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            mem_avail: -1,
            mem_total: -1,
            disk_avail: -1,
            load1: 0.0,
            ncpu: 1.0,
            psi_mem10: 0.0,
            psi_io10: 0.0,
            psi_cpu10: 0.0,
            telemetry_unavailable: false,
        }
    }
}

impl Health {
    fn elevated_memory_floor(&self) -> i64 {
        if self.mem_total > 0 {
            (self.mem_total / 16).clamp(MIN_AVAIL_MEM, MIN_AVAIL_MEM * 4)
        } else {
            MIN_AVAIL_MEM * 4
        }
    }

    /// Classify pressure. `disk_floor` is the same low-disk threshold the daemon
    /// uses for writes (bytes). Any single hard breach ⇒ Critical.
    pub fn level(&self, disk_floor: i64) -> Pressure {
        if self.required_telemetry_unavailable()
            || disk_floor < 0
            || [self.psi_mem10, self.psi_io10, self.psi_cpu10]
                .iter()
                .any(|value| !value.is_finite() || *value < 0.0 || *value > 100.0)
        {
            return Pressure::Critical;
        }
        let mem_crit = self.mem_avail >= 0 && self.mem_avail <= MIN_AVAIL_MEM;
        let disk_crit = self.disk_avail >= 0 && self.disk_avail <= disk_floor;
        let load_crit = self.ncpu > 0.0 && self.load1 / self.ncpu > 8.0;
        let psi_crit = self.psi_mem10 > 20.0 || self.psi_io10 > 40.0;
        if mem_crit || disk_crit || load_crit || psi_crit {
            return Pressure::Critical;
        }
        let mem_elev = self.mem_avail >= 0 && self.mem_avail <= self.elevated_memory_floor();
        let load_elev = self.ncpu > 0.0 && self.load1 / self.ncpu > 4.0;
        let psi_elev = self.psi_mem10 > 5.0 || self.psi_io10 > 10.0 || self.psi_cpu10 > 20.0;
        if mem_elev || load_elev || psi_elev {
            Pressure::Elevated
        } else {
            Pressure::Normal
        }
    }

    /// Short human reason for the current pressure, for logs/status.
    pub fn reason(&self, disk_floor: i64) -> &'static str {
        if self.required_telemetry_unavailable()
            || disk_floor < 0
            || [self.psi_mem10, self.psi_io10, self.psi_cpu10]
                .iter()
                .any(|value| !value.is_finite() || *value < 0.0 || *value > 100.0)
        {
            "host pressure telemetry unavailable"
        } else if self.disk_avail >= 0 && self.disk_avail <= disk_floor {
            "low disk"
        } else if self.mem_avail >= 0 && self.mem_avail <= MIN_AVAIL_MEM {
            "low memory"
        } else if self.psi_mem10 > 20.0 {
            "memory pressure"
        } else if self.psi_io10 > 40.0 {
            "io pressure"
        } else if self.ncpu > 0.0 && self.load1 / self.ncpu > 8.0 {
            "high load"
        } else if self.mem_avail >= 0 && self.mem_avail <= self.elevated_memory_floor() {
            "low available memory"
        } else if self.psi_mem10 > 5.0 {
            "memory pressure"
        } else if self.psi_io10 > 10.0 {
            "io pressure"
        } else if self.ncpu > 0.0 && self.load1 / self.ncpu > 4.0 {
            "high load"
        } else if self.psi_cpu10 > 20.0 {
            "cpu pressure"
        } else {
            "system pressure"
        }
    }

    /// Keep classification fail-closed even for callers that construct a
    /// Health value directly instead of using `sample()`.
    fn required_telemetry_unavailable(&self) -> bool {
        self.telemetry_unavailable
            || self.mem_avail < 0
            || self.mem_total <= 0
            || self.disk_avail < 0
            || !self.load1.is_finite()
            || self.load1 < 0.0
            || !self.ncpu.is_finite()
            || self.ncpu <= 0.0
    }
}

/// Sample current host pressure. `watch_dir` is the index's filesystem (for disk).
pub fn sample(watch_dir: &Path) -> Health {
    let mem_avail = mem_available();
    let mem_total = mem_total();
    let disk_avail = crate::util::fs_stat(watch_dir)
        .map(|f| f.avail)
        .unwrap_or(-1);
    let load1 = loadavg1();
    let stat = std::fs::read_to_string("/proc/stat").ok();
    let host_cpu_count = stat.as_deref().and_then(host_cpu_count);
    let mut health = Health {
        mem_avail,
        mem_total,
        disk_avail,
        load1,
        ncpu: host_cpu_count.unwrap_or(1.0),
        psi_mem10: psi("memory"),
        psi_io10: psi("io"),
        psi_cpu10: psi("cpu"),
        telemetry_unavailable: required_telemetry_unavailable(
            mem_avail,
            mem_total,
            disk_avail,
            load1,
            host_cpu_count,
        ),
    };
    if crate::fault::force_critical_pressure() {
        health.mem_avail = 0;
    } else if crate::fault::force_elevated_pressure() {
        health.psi_io10 = 11.0;
    } else if crate::fault::force_cpu_elevated_pressure() {
        health.psi_cpu10 = 25.0;
    } else if crate::fault::force_high_load_elevated_pressure() {
        // Exercise the cancellable Elevated band without depending on ambient
        // VM load. Keep the synthetic load above the Elevated threshold and
        // below the Critical threshold.
        health.load1 = health.ncpu * 5.0;
    }
    health
}

/// Make a one-off direct scan a preferred victim under global OOM. Daemons
/// deliberately do not call this: their memory is bounded by systemd and an
/// artificial +800 score repeatedly killed the live indexer even at low RSS.
/// Best-effort (needs write access to the proc file).
pub fn oom_protect_scan() {
    let _ = std::fs::write("/proc/self/oom_score_adj", "800");
}

fn mem_available() -> i64 {
    meminfo_value("MemAvailable:")
}

fn mem_total() -> i64 {
    meminfo_value("MemTotal:")
}

fn meminfo_value(key: &str) -> i64 {
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix(key) {
                return kb(v);
            }
        }
    }
    -1
}

fn kb(s: &str) -> i64 {
    s.split_whitespace()
        .next()
        .and_then(|n| n.parse::<i64>().ok())
        .unwrap_or(0)
        * 1024
}

fn loadavg1() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(parse_nonnegative_finite)
        })
        .unwrap_or(f64::INFINITY)
}

#[cfg(test)]
fn host_cpus(stat: &str) -> f64 {
    host_cpu_count(stat).unwrap_or(1.0)
}

fn host_cpu_count(stat: &str) -> Option<f64> {
    let count = stat
        .lines()
        .filter(|line| {
            line.split_whitespace().next().is_some_and(|name| {
                name.strip_prefix("cpu")
                    .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .count();
    (count > 0).then_some(count as f64)
}

fn parse_nonnegative_finite(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn required_telemetry_unavailable(
    mem_avail: i64,
    mem_total: i64,
    disk_avail: i64,
    load1: f64,
    cpu_count: Option<f64>,
) -> bool {
    mem_avail < 0
        || mem_total <= 0
        || disk_avail < 0
        || !load1.is_finite()
        || load1 < 0.0
        || !cpu_count.is_some_and(|count| count.is_finite() && count > 0.0)
}

/// Latch elevated/critical pressure until pressure stays lower for a full PSI
/// avg10 window. Three fast watcher iterations were not enough: at a 100ms
/// flush cadence, a noisy threshold could start and cancel full scans every few
/// hundred milliseconds. Elevated pressure still permits incremental writes;
/// Critical pressure pauses them as well.
const PRESSURE_RECOVERY_WINDOW: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct PressureGate {
    latched: Option<Pressure>,
    lower_since: Option<Instant>,
}

impl PressureGate {
    pub fn observe(&mut self, pressure: Pressure) -> Pressure {
        self.observe_at(pressure, Instant::now())
    }

    fn observe_at(&mut self, pressure: Pressure, now: Instant) -> Pressure {
        match pressure {
            Pressure::Critical => {
                self.latched = Some(Pressure::Critical);
                self.lower_since = None;
            }
            Pressure::Elevated => match self.latched {
                Some(Pressure::Critical) => {
                    self.record_lower_pressure(now, Pressure::Elevated);
                }
                Some(Pressure::Elevated) => self.lower_since = None,
                Some(Pressure::Normal) => unreachable!(),
                None => {
                    self.latched = Some(Pressure::Elevated);
                    self.lower_since = None;
                }
            },
            Pressure::Normal => {
                if self.latched.is_some() {
                    self.record_lower_pressure(now, Pressure::Normal);
                }
            }
        }
        match self.latched {
            Some(Pressure::Critical) => Pressure::Critical,
            Some(Pressure::Elevated) => Pressure::Elevated,
            Some(Pressure::Normal) | None => pressure,
        }
    }

    fn record_lower_pressure(&mut self, now: Instant, lower: Pressure) {
        let since = self.lower_since.get_or_insert(now);
        if now.saturating_duration_since(*since) >= PRESSURE_RECOVERY_WINDOW {
            self.latched = match (self.latched, lower) {
                (Some(Pressure::Critical), Pressure::Elevated) => Some(Pressure::Elevated),
                (Some(Pressure::Critical | Pressure::Elevated), Pressure::Normal) => None,
                (latched, _) => latched,
            };
            self.lower_since = None;
        }
    }
}

/// Read the "some avg10" percentage from /proc/pressure/<kind> (Linux PSI).
/// Returns 0.0 when PSI is unavailable (older kernels / disabled).
fn parse_psi_avg10(contents: &str) -> Option<f64> {
    let line = contents
        .lines()
        .find(|line| line.split_whitespace().next() == Some("some"))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?;
    parse_nonnegative_finite(value).filter(|value| *value <= 100.0)
}

fn psi_value(contents: &str) -> f64 {
    parse_psi_avg10(contents).unwrap_or(f64::INFINITY)
}

fn psi(kind: &str) -> f64 {
    match std::fs::read_to_string(format!("/proc/pressure/{kind}")) {
        // PSI is not available on every supported kernel. The independent
        // memory/load/disk checks still gate work on those hosts.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0.0,
        Err(_) => f64::INFINITY,
        Ok(contents) => psi_value(&contents),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_parsers_reject_nonfinite_negative_and_out_of_range_values() {
        for malformed in ["nan", "inf", "-1", "101"] {
            let contents = format!("some avg10={malformed} avg60=0 avg300=0 total=0\n");
            assert_eq!(parse_psi_avg10(&contents), None, "accepted {malformed}");
            assert!(psi_value(&contents).is_infinite(), "accepted {malformed}");
        }
        for malformed in ["nan", "inf", "-1"] {
            assert_eq!(
                parse_nonnegative_finite(malformed),
                None,
                "accepted {malformed}"
            );
        }
        assert_eq!(
            parse_psi_avg10("some avg10=0 avg60=0 avg300=0 total=0\n"),
            Some(0.0)
        );
        assert_eq!(
            parse_psi_avg10("some avg10=100 avg60=100 avg300=100 total=1\n"),
            Some(100.0)
        );
    }

    #[test]
    fn required_pressure_telemetry_rejects_each_missing_or_invalid_metric() {
        let healthy = (8 << 30, 16 << 30, 20 << 30, 0.5, Some(12.0));
        assert!(!required_telemetry_unavailable(
            healthy.0, healthy.1, healthy.2, healthy.3, healthy.4
        ));
        let invalid = [
            (-1, healthy.1, healthy.2, healthy.3, healthy.4),
            (healthy.0, 0, healthy.2, healthy.3, healthy.4),
            (healthy.0, healthy.1, -1, healthy.3, healthy.4),
            (healthy.0, healthy.1, healthy.2, f64::NAN, healthy.4),
            (healthy.0, healthy.1, healthy.2, healthy.3, None),
            (
                healthy.0,
                healthy.1,
                healthy.2,
                healthy.3,
                Some(f64::INFINITY),
            ),
        ];
        for (mem_avail, mem_total, disk_avail, load1, cpu_count) in invalid {
            assert!(required_telemetry_unavailable(
                mem_avail, mem_total, disk_avail, load1, cpu_count
            ));
        }
    }

    #[test]
    fn missing_required_host_telemetry_fails_pressure_admission_closed() {
        let health = Health {
            telemetry_unavailable: true,
            mem_avail: -1,
            mem_total: -1,
            disk_avail: -1,
            ..Default::default()
        };
        assert_eq!(health.level(256 << 20), Pressure::Critical);
        assert_eq!(
            health.reason(256 << 20),
            "host pressure telemetry unavailable"
        );
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &health,
            256 << 20
        ));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Normal,
            &health,
            256 << 20
        ));
    }

    #[test]
    fn directly_constructed_invalid_telemetry_fails_pressure_admission_closed() {
        let healthy = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 8.0,
            ..Default::default()
        };
        let invalid = [
            Health {
                mem_avail: -1,
                ..healthy
            },
            Health {
                mem_total: 0,
                ..healthy
            },
            Health {
                disk_avail: -1,
                ..healthy
            },
            Health {
                load1: f64::NAN,
                ..healthy
            },
            Health {
                load1: -1.0,
                ..healthy
            },
            Health {
                ncpu: 0.0,
                ..healthy
            },
            Health {
                ncpu: f64::INFINITY,
                ..healthy
            },
        ];
        for health in invalid {
            assert_eq!(health.level(256 << 20), Pressure::Critical);
            assert_eq!(
                health.reason(256 << 20),
                "host pressure telemetry unavailable"
            );
            assert!(!may_start_reconciliation_with_health(
                Pressure::Normal,
                &health,
                256 << 20
            ));
        }
        assert_eq!(
            Health::default().level(256 << 20),
            Pressure::Critical,
            "the default unknown snapshot must not be admitted as healthy"
        );
    }

    #[test]
    fn invalid_psi_cannot_be_admitted_as_cpu_only_elevated_pressure() {
        let health = Health {
            psi_cpu10: f64::INFINITY,
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(health.level(256 << 20), Pressure::Critical);
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &health,
            256 << 20
        ));
    }

    #[test]
    fn pressure_classification() {
        let gb = 1024 * 1024 * 1024;
        let floor = 256 * 1024 * 1024;
        // healthy host
        let h = Health {
            mem_avail: 8 * gb,
            mem_total: 16 * gb,
            disk_avail: 50 * gb,
            load1: 1.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Normal);
        // low memory -> critical
        let h = Health {
            mem_avail: 100 * 1024 * 1024,
            mem_total: 16 * gb,
            ncpu: 8.0,
            disk_avail: 50 * gb,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Critical);
        assert_eq!(h.reason(floor), "low memory");
        // high load -> critical
        let h = Health {
            mem_avail: 8 * gb,
            mem_total: 16 * gb,
            disk_avail: 50 * gb,
            load1: 100.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Critical);
        // moderately loaded -> elevated
        let h = Health {
            mem_avail: 8 * gb,
            mem_total: 16 * gb,
            disk_avail: 50 * gb,
            load1: 40.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Elevated);
    }

    #[test]
    fn elevated_memory_threshold_scales_down_for_small_hosts() {
        let mib = 1024 * 1024;
        let floor = 256 * mib;
        let small_host = Health {
            mem_avail: 700 * mib,
            mem_total: 1024 * mib,
            disk_avail: 20 << 30,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(small_host.level(floor), Pressure::Normal);

        let large_host = Health {
            mem_avail: 700 * mib,
            mem_total: 16 * 1024 * mib,
            disk_avail: 20 << 30,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(large_host.level(floor), Pressure::Elevated);
        assert_eq!(large_host.reason(floor), "low available memory");

        let psi_pressure = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            psi_mem10: 8.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(psi_pressure.level(floor), Pressure::Elevated);
        assert_eq!(psi_pressure.reason(floor), "memory pressure");
    }

    #[test]
    fn host_pressure_ignores_daemon_quota() {
        let stat = "cpu 1 2 3\ncpu0 1 2 3\ncpu1 1 2 3\ncpu2 1 2 3\ncpu3 1 2 3\nintr 12\n";
        assert_eq!(host_cpus(stat), 4.0);
        assert_eq!(host_cpus(""), 1.0);
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 20.0,
            ncpu: 12.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Normal);
    }

    #[test]
    fn critical_pressure_requires_sustained_recovery() {
        let mut gate = PressureGate::default();
        let now = Instant::now();
        assert_eq!(gate.observe_at(Pressure::Critical, now), Pressure::Critical);
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(9)),
            Pressure::Critical
        );
        assert_eq!(
            gate.observe_at(Pressure::Critical, now + Duration::from_secs(10)),
            Pressure::Critical
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(11)),
            Pressure::Critical
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(21)),
            Pressure::Normal
        );
    }

    #[test]
    fn critical_pressure_steps_down_to_elevated_only_after_ten_seconds() {
        let mut gate = PressureGate::default();
        let now = Instant::now();
        assert_eq!(gate.observe_at(Pressure::Critical, now), Pressure::Critical);
        assert_eq!(gate.observe_at(Pressure::Elevated, now), Pressure::Critical);
        assert_eq!(
            gate.observe_at(Pressure::Elevated, now + Duration::from_secs(10)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(11)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(21)),
            Pressure::Normal
        );
    }

    #[test]
    fn default_unknown_health_cannot_start_reconciliation() {
        let health = Health::default();
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &health,
            256 << 20
        ));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Elevated,
            &health,
            256 << 20
        ));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Normal,
            &health,
            256 << 20
        ));
    }

    #[test]
    fn cpu_only_pressure_does_not_cancel_an_active_reconciliation() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_cpu10: 25.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "cpu pressure");
        assert!(may_start_reconciliation_with_health(
            Pressure::Elevated,
            &h,
            256 << 20
        ));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &h,
            256 << 20
        ));
        assert!(!should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn elevated_io_pressure_admits_a_low_priority_reconciliation() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_io10: 20.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "io pressure");
        assert!(may_start_reconciliation_with_health(
            Pressure::Elevated,
            &h,
            256 << 20
        ));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &h,
            256 << 20
        ));
        assert!(!should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn critical_io_pressure_still_blocks_reconciliation_admission() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_io10: 41.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Critical);
        assert!(!may_start_reconciliation_with_health(
            Pressure::Critical,
            &h,
            256 << 20
        ));
    }

    #[test]
    fn elevated_io_pressure_does_not_cancel_an_active_reconciliation() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_io10: 20.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "io pressure");
        assert!(!should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn moderate_memory_psi_with_available_headroom_does_not_cancel_active_scan() {
        let h = Health {
            mem_avail: 3 << 30,
            mem_total: 8 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 6.0,
            psi_mem10: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "memory pressure");
        assert!(!may_start_reconciliation_with_health(
            Pressure::Elevated,
            &h,
            256 << 20
        ));
        assert!(!should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn elevated_low_available_memory_still_cancels_active_scan() {
        let h = Health {
            mem_avail: 512 << 20,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "low available memory");
        assert!(should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn elevated_high_load_cancels_an_active_reconciliation() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 5.0,
            ncpu: 1.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Elevated);
        assert_eq!(h.reason(256 << 20), "high load");
        assert!(should_cancel_reconciliation(&h, 256 << 20));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Elevated,
            &h,
            256 << 20
        ));
    }

    #[test]
    fn critical_io_pressure_still_cancels_an_active_reconciliation() {
        let h = Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_io10: 41.0,
            ..Default::default()
        };
        assert_eq!(h.level(256 << 20), Pressure::Critical);
        assert!(should_cancel_reconciliation(&h, 256 << 20));
    }

    #[test]
    fn memory_pressure_still_cancels_an_active_reconciliation() {
        let h = Health {
            mem_avail: 128 << 20,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            ..Default::default()
        };
        assert!(should_cancel_reconciliation(&h, 256 << 20));
        assert!(!may_start_reconciliation_with_health(
            Pressure::Elevated,
            &h,
            256 << 20
        ));
    }

    #[test]
    fn elevated_pressure_requires_ten_seconds_of_continuous_normal_before_rebuilds_resume() {
        let mut gate = PressureGate::default();
        let now = Instant::now();
        assert_eq!(gate.observe_at(Pressure::Elevated, now), Pressure::Elevated);
        assert_eq!(gate.observe_at(Pressure::Normal, now), Pressure::Elevated);
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(9)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Elevated, now + Duration::from_secs(9)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(10)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(19)),
            Pressure::Elevated
        );
        assert_eq!(
            gate.observe_at(Pressure::Normal, now + Duration::from_secs(20)),
            Pressure::Normal
        );
    }
}
