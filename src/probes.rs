//! Health probes the sentinel runs once a minute. Each returns findings
//! that go through the same alert logic as space alerts (start, repeat,
//! hold-down, flood control). All reads are tiny files under /proc and /sys;
//! smartctl runs only where it is installed and real disks exist.

use crate::sentinel::Level;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub key: String,
    pub level: Level,
    pub value: f64,
    pub title: String,
    pub body: String,
}

/// ext4 keeps a running error count per filesystem in sysfs.
pub fn ext4_errors(sys_root: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(format!("{sys_root}/fs/ext4")) else {
        return out;
    };
    for e in rd.flatten() {
        let dev = e.file_name().to_string_lossy().into_owned();
        let Ok(t) = std::fs::read_to_string(e.path().join("errors_count")) else {
            continue;
        };
        let n: u64 = t.trim().parse().unwrap_or(0);
        let first = std::fs::read_to_string(e.path().join("first_error_time")).unwrap_or_default();
        out.push(Finding {
            key: format!("fserrors:{dev}"),
            level: if n > 0 { Level::Critical } else { Level::Ok },
            value: n as f64,
            title: if n > 0 { format!("ext4 on {dev} has recorded {n} filesystem errors") } else { format!("ext4 on {dev}: no errors") },
            body: if n > 0 {
                format!("first error at unix time {}; plan an fsck window (never fsck a mounted filesystem) and check the disk", first.trim())
            } else {
                String::new()
            },
        });
    }
    out
}

/// Software RAID state from /proc/mdstat: "[UU]" is healthy, "_" is a missing member.
pub fn mdstat(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some((name, _)) = line.split_once(" : ") {
            if name.starts_with("md") {
                current = Some(name.trim().to_string());
            }
            continue;
        }
        let Some(md) = current.clone() else { continue };
        if let Some(i) = line.rfind('[') {
            let tail = &line[i..];
            if tail.contains('U') || tail.contains('_') {
                let missing = tail.matches('_').count();
                // Only this array's own block (until the next "mdN :" line).
                let block: String = text
                    .lines()
                    .skip_while(|l| !l.starts_with(&format!("{md} :")))
                    .skip(1)
                    .take_while(|l| {
                        !l.split_once(" : ")
                            .is_some_and(|(n, _)| n.starts_with("md"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let rebuilding = block.contains("recovery") || block.contains("resync");
                out.push(Finding {
                    key: format!("raid:{md}"),
                    level: if missing > 0 {
                        Level::Critical
                    } else {
                        Level::Ok
                    },
                    value: missing as f64,
                    title: if missing > 0 {
                        format!("RAID {md} is degraded: {missing} member(s) missing")
                    } else {
                        format!("RAID {md} healthy")
                    },
                    body: if missing > 0 {
                        format!(
                            "{}{}",
                            line.trim(),
                            if rebuilding {
                                "; a rebuild is running"
                            } else {
                                "; replace the failed disk"
                            }
                        )
                    } else {
                        String::new()
                    },
                });
                current = None;
            }
        }
    }
    out
}

/// Average I/O wait per request since the previous sample, per whole disk.
#[derive(Default)]
pub struct Latency {
    prev: HashMap<String, (u64, u64)>, // (ios completed, ms spent)
}

impl Latency {
    pub fn sample(&mut self, diskstats: &str, warn_ms: f64, crit_ms: f64) -> Vec<Finding> {
        let mut out = Vec::new();
        for line in diskstats.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 14 {
                continue;
            }
            let name = f[2];
            // Whole disks only: skip partitions, loop, ram and dm (dm repeats the disk).
            let is_part = name.chars().last().is_some_and(|c| c.is_ascii_digit())
                && !name.starts_with("nvme")
                && !name.starts_with("md")
                || (name.starts_with("nvme") && name.contains('p'));
            if is_part
                || name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("dm-")
                || name.starts_with("sr")
            {
                continue;
            }
            let num = |i: usize| f[i].parse::<u64>().unwrap_or(0);
            let ios = num(3) + num(7);
            let ms = num(6) + num(10);
            if let Some((pi, pm)) = self.prev.insert(name.to_string(), (ios, ms)) {
                let dio = ios.saturating_sub(pi);
                if dio < 50 {
                    // A quiet disk is not a slow disk: report Ok so an earlier
                    // latency alert can recover instead of staying stuck.
                    out.push(Finding {
                        key: format!("latency:{name}"),
                        level: Level::Ok,
                        value: 0.0,
                        title: format!("{name} latency back to normal (disk is quiet)"),
                        body: format!("only {dio} requests in the last minute"),
                    });
                    continue;
                }
                let avg = ms.saturating_sub(pm) as f64 / dio as f64;
                let level = if avg >= crit_ms {
                    Level::Critical
                } else if avg >= warn_ms {
                    Level::Warning
                } else {
                    Level::Ok
                };
                out.push(Finding {
                    key: format!("latency:{name}"),
                    level,
                    value: avg,
                    title: if level == Level::Ok { format!("{name} latency back to {avg:.0} ms") } else { format!("{name} is slow: {avg:.0} ms per request") },
                    body: format!("{dio} requests in the last minute averaged {avg:.1} ms (warning {warn_ms} ms, critical {crit_ms} ms)"),
                });
            }
        }
        out
    }
}

/// Pressure stall information: share of time some task waited on I/O or memory.
pub fn psi(kind: &str, text: &str, warn: f64, crit: f64) -> Option<Finding> {
    let line = text.lines().find(|l| l.starts_with("some"))?;
    let avg60: f64 = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("avg60=")?.parse().ok())?;
    let level = if avg60 >= crit {
        Level::Critical
    } else if avg60 >= warn {
        Level::Warning
    } else {
        Level::Ok
    };
    let what = if kind == "memory" {
        "memory (this can look like a disk problem)"
    } else {
        "disk I/O"
    };
    Some(Finding {
        key: format!("pressure:{kind}"),
        level,
        value: avg60,
        title: if level == Level::Ok {
            format!("{kind} pressure back to {avg60:.0}%")
        } else {
            format!("tasks are waiting on {what} {avg60:.0}% of the time")
        },
        body: format!("PSI {kind} some avg60={avg60:.1}% (warning {warn}%, critical {crit}%)"),
    })
}

/// SMART overall health, only for real disks and only if smartctl exists.
pub fn smart(devices: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();
    for d in devices {
        let Ok(o) = std::process::Command::new("smartctl")
            .args(["-H", "-A", d])
            .output()
        else {
            return out;
        };
        let text = String::from_utf8_lossy(&o.stdout);
        if text.contains("Unavailable") || text.contains("QEMU") || text.contains("Unknown USB") {
            continue;
        }
        let failed = text.contains("FAILED") || text.contains("FAILING_NOW");
        // No health verdict at all (smartctl could not open or read the disk):
        // say nothing rather than report a disk as healthy.
        let passed = text.contains("PASSED") || text.contains("SMART Health Status: OK");
        if !failed && !passed {
            continue;
        }
        let realloc = attr(&text, "Reallocated_Sector_Ct").unwrap_or(0)
            + attr(&text, "Current_Pending_Sector").unwrap_or(0);
        let wear = text
            .lines()
            .find(|l| l.starts_with("Percentage Used:"))
            .and_then(|l| l.split_whitespace().nth(2))
            .and_then(|v| v.trim_end_matches('%').parse::<u64>().ok());
        let level = if failed || realloc > 0 || wear.is_some_and(|w| w >= 95) {
            Level::Critical
        } else if wear.is_some_and(|w| w >= 80) {
            Level::Warning
        } else {
            Level::Ok
        };
        out.push(Finding {
            key: format!("smart:{d}"),
            level,
            value: realloc as f64 + wear.unwrap_or(0) as f64,
            title: if level == Level::Ok {
                format!("{d} SMART healthy")
            } else {
                format!("{d} is failing or worn out")
            },
            body: format!(
                "health {}; reallocated+pending sectors {realloc}; wear {}",
                if failed { "FAILED" } else { "PASSED" },
                wear.map(|w| format!("{w}%"))
                    .unwrap_or_else(|| "n/a".into())
            ),
        });
    }
    out
}

fn attr(text: &str, name: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.contains(name))?
        .split_whitespace()
        .last()?
        .parse()
        .ok()
}

/// Whole disks that look physical (not virtual), for SMART.
pub fn physical_disks() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/block") else {
        return out;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !(n.starts_with("sd") || n.starts_with("nvme") || n.starts_with("hd")) {
            continue;
        }
        let model = std::fs::read_to_string(e.path().join("device/model")).unwrap_or_default();
        if model.contains("QEMU") || model.contains("Virtual") || model.contains("VBOX") {
            continue;
        }
        out.push(format!("/dev/{n}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdstat_degraded_and_healthy() {
        let t = "Personalities : [raid1]\nmd0 : active raid1 sdb1[1] sda1[0]\n      976630336 blocks super 1.2 [2/2] [UU]\n\nmd1 : active raid1 sdc1[0]\n      100 blocks [2/1] [U_]\n      [=>...]  recovery = 8.1%\n";
        let f = mdstat(t);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].key.as_str(), f[0].level), ("raid:md0", Level::Ok));
        assert_eq!(
            (f[1].key.as_str(), f[1].level),
            ("raid:md1", Level::Critical)
        );
        assert!(f[1].body.contains("rebuild is running"));
        assert!(mdstat("Personalities :\nunused devices: <none>\n").is_empty());
    }

    #[test]
    fn latency_from_diskstats_deltas() {
        let mut l = Latency::default();
        let a = "   8       0 sda 1000 0 0 1000 1000 0 0 1000 0 0 0\n   8       1 sda1 10 0 0 10 10 0 0 10 0 0 0\n 252 0 dm-0 9 9 9 9 9 9 9 9 9 9 9\n";
        assert!(
            l.sample(a, 20.0, 100.0).is_empty(),
            "first sample has no delta"
        );
        // +200 requests, +10000 ms -> 50 ms each.
        let b = "   8       0 sda 1100 0 0 6000 1100 0 0 6000 0 0 0\n";
        let f = l.sample(b, 20.0, 100.0);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Warning);
        assert!((f[0].value - 50.0).abs() < 0.01);
    }

    #[test]
    fn psi_parsing() {
        let t = "some avg10=1.00 avg60=35.50 avg300=2.00 total=1\nfull avg10=0 avg60=0 avg300=0 total=0\n";
        let f = psi("io", t, 20.0, 50.0).unwrap();
        assert_eq!(f.level, Level::Warning);
        assert_eq!(f.key, "pressure:io");
        assert!(psi("io", "garbage", 1.0, 2.0).is_none());
    }

    #[test]
    fn ext4_error_counts_from_sysfs() {
        let d = std::env::temp_dir().join(format!("dux-probe-{}", std::process::id()));
        std::fs::create_dir_all(d.join("fs/ext4/sda1")).unwrap();
        std::fs::create_dir_all(d.join("fs/ext4/dm-0")).unwrap();
        std::fs::write(d.join("fs/ext4/sda1/errors_count"), "3\n").unwrap();
        std::fs::write(d.join("fs/ext4/dm-0/errors_count"), "0\n").unwrap();
        let mut f = ext4_errors(d.to_str().unwrap());
        f.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(f[0].level, Level::Ok);
        assert_eq!(f[1].level, Level::Critical);
        assert!(f[1].title.contains("3 filesystem errors"));
        std::fs::remove_dir_all(&d).unwrap();
        // Real sysfs never panics.
        let _ = ext4_errors("/sys");
    }

    #[test]
    fn smart_attribute_parsing() {
        let t = "  5 Reallocated_Sector_Ct   0x0033   100   100   010    Pre-fail  Always       -       8\n";
        assert_eq!(attr(t, "Reallocated_Sector_Ct"), Some(8));
        assert_eq!(attr(t, "Current_Pending_Sector"), None);
    }
}
