//! Resource guardian — dux is a background observability tool and must NEVER be
//! the cause of host problems. This module samples system pressure (free memory,
//! CPU load, kernel PSI for memory/io/cpu, and free disk) and classifies it into
//! Normal / Elevated / Critical so the daemon can self-throttle: back off under
//! Elevated, and fully PAUSE its own writes under Critical. It also marks dux as
//! the preferred OOM victim, so the kernel kills the indexer — never a real
//! workload — if it ever has to reclaim memory.

use std::path::Path;

/// Free-memory floor: at or below this available memory we treat the host as
/// memory-critical and pause writes (mirrors the disk floor in watch.rs).
pub const MIN_AVAIL_MEM: i64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    Elevated,
    Critical,
}

/// A point-in-time snapshot of host resource pressure.
#[derive(Clone, Copy, Default)]
pub struct Health {
    pub mem_avail: i64,  // bytes (MemAvailable), 0 if unknown
    pub disk_avail: i64, // bytes available on the index filesystem
    pub load1: f64,      // 1-minute load average
    pub ncpu: f64,
    pub psi_mem10: f64, // /proc/pressure "some avg10" %, 0 if unavailable
    pub psi_io10: f64,
    pub psi_cpu10: f64,
}

impl Health {
    /// Classify pressure. `disk_floor` is the same low-disk threshold the daemon
    /// uses for writes (bytes). Any single hard breach ⇒ Critical.
    pub fn level(&self, disk_floor: i64) -> Pressure {
        let mem_crit = self.mem_avail > 0 && self.mem_avail < MIN_AVAIL_MEM;
        let disk_crit = self.disk_avail > 0 && self.disk_avail < disk_floor;
        let load_crit = self.ncpu > 0.0 && self.load1 / self.ncpu > 8.0;
        let psi_crit = self.psi_mem10 > 20.0 || self.psi_io10 > 40.0;
        if mem_crit || disk_crit || load_crit || psi_crit {
            return Pressure::Critical;
        }
        let mem_elev = self.mem_avail > 0 && self.mem_avail < MIN_AVAIL_MEM * 4;
        let load_elev = self.ncpu > 0.0 && self.load1 / self.ncpu > 4.0;
        let psi_elev = self.psi_mem10 > 5.0 || self.psi_io10 > 10.0 || self.psi_cpu10 > 20.0;
        if mem_elev || load_elev || psi_elev {
            Pressure::Elevated
        } else {
            Pressure::Normal
        }
    }

    /// Short human reason for the current (Critical) pressure, for logs/status.
    pub fn reason(&self, disk_floor: i64) -> &'static str {
        if self.disk_avail > 0 && self.disk_avail < disk_floor {
            "low disk"
        } else if self.mem_avail > 0 && self.mem_avail < MIN_AVAIL_MEM {
            "low memory"
        } else if self.psi_mem10 > 20.0 {
            "memory pressure"
        } else if self.psi_io10 > 40.0 {
            "io pressure"
        } else if self.ncpu > 0.0 && self.load1 / self.ncpu > 8.0 {
            "high load"
        } else {
            "system pressure"
        }
    }
}

/// Sample current host pressure. `watch_dir` is the index's filesystem (for disk).
pub fn sample(watch_dir: &Path) -> Health {
    Health {
        mem_avail: mem_available(),
        disk_avail: crate::util::fs_stat(watch_dir)
            .map(|f| f.avail)
            .unwrap_or(0),
        load1: loadavg1(),
        ncpu: ncpu(),
        psi_mem10: psi("memory"),
        psi_io10: psi("io"),
        psi_cpu10: psi("cpu"),
    }
}

/// Ask the kernel to kill dux FIRST under memory pressure. A background indexer
/// must never get a real workload OOM-killed in its place; +800 makes dux a
/// strongly preferred victim. Best-effort (needs write access to the proc file).
pub fn oom_protect_self() {
    let _ = std::fs::write("/proc/self/oom_score_adj", "800");
}

fn mem_available() -> i64 {
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("MemAvailable:") {
                return kb(v);
            }
        }
    }
    0
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
        .and_then(|s| s.split_whitespace().next().and_then(|n| n.parse().ok()))
        .unwrap_or(0.0)
}

fn ncpu() -> f64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(1.0)
}

/// Read the "some avg10" percentage from /proc/pressure/<kind> (Linux PSI).
/// Returns 0.0 when PSI is unavailable (older kernels / disabled).
fn psi(kind: &str) -> f64 {
    std::fs::read_to_string(format!("/proc/pressure/{kind}"))
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("some")).and_then(|l| {
                l.split_whitespace()
                    .find_map(|t| t.strip_prefix("avg10=").and_then(|v| v.parse().ok()))
            })
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_classification() {
        let gb = 1024 * 1024 * 1024;
        let floor = 256 * 1024 * 1024;
        // healthy host
        let h = Health {
            mem_avail: 8 * gb,
            disk_avail: 50 * gb,
            load1: 1.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Normal);
        // low memory -> critical
        let h = Health {
            mem_avail: 100 * 1024 * 1024,
            ncpu: 8.0,
            disk_avail: 50 * gb,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Critical);
        assert_eq!(h.reason(floor), "low memory");
        // high load -> critical
        let h = Health {
            mem_avail: 8 * gb,
            disk_avail: 50 * gb,
            load1: 100.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Critical);
        // moderately loaded -> elevated
        let h = Health {
            mem_avail: 8 * gb,
            disk_avail: 50 * gb,
            load1: 40.0,
            ncpu: 8.0,
            ..Default::default()
        };
        assert_eq!(h.level(floor), Pressure::Elevated);
    }
}
