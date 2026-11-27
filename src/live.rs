//! Live I/O: which disks, processes and files are being read and written
//! right now, and how fast. Sampled from /proc on demand (no event stream):
//!
//! - /proc/diskstats: read and write bytes per second per disk
//! - /proc/<pid>/io: read and write bytes per second per process
//! - /proc/<pid>/fd + fdinfo: for processes doing I/O, each open regular
//!   file's position and size, giving per-file read and write speed and
//!   size growth per second
//!
//! Only processes whose I/O counters moved since the last sample have their
//! open files scanned, so a sample costs a few milliseconds.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct DiskIo {
    pub name: String,
    pub read_bps: f64,
    pub write_bps: f64,
}

#[derive(Clone, Debug, Default)]
pub struct ProcIo {
    pub pid: i32,
    pub name: String,
    pub unit: String,
    pub read_bps: f64,
    pub write_bps: f64,
}

#[derive(Clone, Debug, Default)]
pub struct FileIo {
    pub path: String,
    pub pid: i32,
    pub process: String,
    /// systemd service or Docker container of the writer.
    pub unit: String,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Size change per second (positive = growing).
    pub growth_bps: f64,
    pub size: u64,
}

/// Everything worth knowing about the process behind an I/O, to track a culprit.
#[derive(Clone, Debug, Default)]
pub struct ProcDetails {
    pub pid: i32,
    pub name: String,
    pub user: String,
    pub cmdline: String,
    pub exe: String,
    pub cwd: String,
    pub ppid: i32,
    pub parent: String,
    /// systemd service, Docker container ("docker:name") or session.
    pub unit: String,
    /// Seconds the process has been running.
    pub running_secs: u64,
    pub threads: u32,
    pub rss_bytes: u64,
}

/// Read a process's details from /proc (on demand, for the selected row).
pub fn details(pid: i32) -> Option<ProcDetails> {
    let base = format!("/proc/{pid}");
    let status = std::fs::read_to_string(format!("{base}/status")).ok()?;
    let field = |k: &str| {
        status
            .lines()
            .find_map(|x| x.strip_prefix(k))
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let uid: u32 = field("Uid:")
        .split_whitespace()
        .next()
        .and_then(|u| u.parse().ok())
        .unwrap_or(0);
    let ppid: i32 = field("PPid:").parse().unwrap_or(0);
    let cmd = std::fs::read(format!("{base}/cmdline")).unwrap_or_default();
    let cmdline = String::from_utf8_lossy(&cmd)
        .replace('\0', " ")
        .trim()
        .to_string();
    // starttime is field 22 of /proc/<pid>/stat, in clock ticks since boot.
    let stat = std::fs::read_to_string(format!("{base}/stat")).unwrap_or_default();
    let after = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let start_ticks: u64 = after
        .split_whitespace()
        .nth(19)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|u| u.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0);
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let users = crate::insight::users();
    Some(ProcDetails {
        pid,
        name: field("Name:"),
        user: users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
        cmdline,
        exe: std::fs::read_link(format!("{base}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        cwd: std::fs::read_link(format!("{base}/cwd"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ppid,
        parent: std::fs::read_to_string(format!("/proc/{ppid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string(),
        unit: unit_of(pid),
        running_secs: (uptime - start_ticks as f64 / hz).max(0.0) as u64,
        threads: field("Threads:").parse().unwrap_or(0),
        rss_bytes: field("VmRSS:")
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024,
    })
}

/// What to run next to look at or stop a writer, based on what it is.
pub fn next_steps(d: &ProcDetails) -> String {
    if let Some(c) = d.unit.strip_prefix("docker:") {
        format!("docker logs --tail 50 {c}   |   docker inspect {c}   |   stop: docker stop {c}")
    } else if !d.unit.is_empty() && !d.unit.starts_with("session-") && !d.unit.starts_with("user@")
    {
        format!(
            "systemctl status {0}   |   journalctl -u {0} -n 50   |   stop: systemctl stop {0}",
            d.unit
        )
    } else {
        format!(
            "ps -o pid,ppid,user,etime,cmd -p {0}   |   ls -l /proc/{0}/fd   |   stop: kill {0}",
            d.pid
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Live {
    pub seconds: f64,
    pub disks: Vec<DiskIo>,
    pub procs: Vec<ProcIo>,
    pub writing: Vec<FileIo>,
    pub reading: Vec<FileIo>,
    /// Files that grew most over the last minute: (path, bytes grown, size now).
    pub grown_minute: Vec<(String, i64, u64)>,
}

#[derive(Default)]
pub struct Sampler {
    at: Option<Instant>,
    disks: HashMap<String, (u64, u64)>,
    procs: HashMap<i32, (u64, u64, u64, u64)>,
    fds: HashMap<(i32, u32), (u64, u64)>,
    sizes: HashMap<(u64, u64), u64>,
    /// (time, path, growth) events for the one-minute window.
    window: VecDeque<(Instant, String, i64, u64)>,
}

/// Whole-disk read and write sectors from /proc/diskstats.
pub fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 {
                return None;
            }
            let n = f[2];
            let part = (n.starts_with("sd")
                || n.starts_with("vd")
                || n.starts_with("hd")
                || n.starts_with("xvd"))
                && n.chars().last().is_some_and(|c| c.is_ascii_digit())
                || (n.starts_with("nvme") && n.contains('p'));
            if part
                || n.starts_with("loop")
                || n.starts_with("ram")
                || n.starts_with("sr")
                || n.starts_with("dm-")
                || n.starts_with("zram")
            {
                return None;
            }
            Some((n.to_string(), f[5].parse().ok()?, f[9].parse().ok()?))
        })
        .collect()
}

/// (read_bytes, write_bytes, rchar, wchar) from /proc/<pid>/io.
pub fn parse_proc_io(text: &str) -> Option<(u64, u64, u64, u64)> {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.trim().parse::<u64>().ok())
    };
    Some((
        get("read_bytes:")?,
        get("write_bytes:")?,
        get("rchar:")?,
        get("wchar:")?,
    ))
}

/// (pos, access mode 0=read 1=write 2=read-write) from /proc/<pid>/fdinfo/<fd>.
pub fn parse_fdinfo(text: &str) -> Option<(u64, u32)> {
    let pos = text
        .lines()
        .find_map(|l| l.strip_prefix("pos:")?.trim().parse::<u64>().ok())?;
    let flags = text
        .lines()
        .find_map(|l| u32::from_str_radix(l.strip_prefix("flags:")?.trim(), 8).ok())?;
    Some((pos, flags & 3))
}

fn unit_of(pid: i32) -> String {
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    let path = cg.lines().find_map(|l| l.strip_prefix("0::")).unwrap_or("");
    crate::attrib::friendly_cgroup(path.trim_start_matches('/'))
}

/// The service or container behind a write. Docker container logs are
/// written by dockerd, so name the container instead.
fn writer_unit(path: &str, pid: i32) -> String {
    if path.contains("/docker/containers/") && path.ends_with("-json.log") {
        if let Some(c) = crate::insight::container_name(path) {
            return format!("docker:{c}");
        }
    }
    unit_of(pid)
}

fn interesting(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("/proc")
        && !path.starts_with("/sys")
        && !path.starts_with("/dev")
        && !path.contains(" (deleted)")
}

impl Sampler {
    pub fn sample(&mut self) -> Live {
        let now = Instant::now();
        let dt = self
            .at
            .map(|a| now.duration_since(a).as_secs_f64())
            .unwrap_or(0.0);
        let rate = |d: u64| if dt > 0.0 { d as f64 / dt } else { 0.0 };
        let mut live = Live {
            seconds: dt,
            ..Live::default()
        };

        // Disks.
        let mut disks = HashMap::new();
        for (n, r, w) in
            parse_diskstats(&std::fs::read_to_string("/proc/diskstats").unwrap_or_default())
        {
            if let Some((pr, pw)) = self.disks.get(&n) {
                live.disks.push(DiskIo {
                    name: n.clone(),
                    read_bps: rate(r.saturating_sub(*pr) * 512),
                    write_bps: rate(w.saturating_sub(*pw) * 512),
                });
            }
            disks.insert(n, (r, w));
        }
        self.disks = disks;

        // Processes, then open files of the ones doing I/O.
        let mut procs = HashMap::new();
        // Keep baselines of processes that are idle this round, so their first
        // active second is measured too.
        let mut fds = self.fds.clone();
        let first_round = self.at.is_none();
        let mut files: HashMap<(u64, u64), FileIo> = HashMap::new();
        let mut sizes = self.sizes.clone();
        for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Some(io) = std::fs::read_to_string(e.path().join("io"))
                .ok()
                .and_then(|t| parse_proc_io(&t))
            else {
                continue;
            };
            let prev = self.procs.get(&pid).copied();
            procs.insert(pid, io);
            let (rb, wb, rc, wc) = match prev {
                Some(p) => (
                    io.0.saturating_sub(p.0),
                    io.1.saturating_sub(p.1),
                    io.2.saturating_sub(p.2),
                    io.3.saturating_sub(p.3),
                ),
                None => (0, 0, 0, 0),
            };
            // Scan open files of processes doing I/O, and of every process on
            // the first round so later rounds have baselines.
            if rb + wb + rc + wc == 0 && !(first_round || prev.is_none()) {
                continue;
            }
            let name = std::fs::read_to_string(e.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            if prev.is_some() && rb + wb > 0 {
                live.procs.push(ProcIo {
                    pid,
                    name: name.clone(),
                    unit: unit_of(pid),
                    read_bps: rate(rb),
                    write_bps: rate(wb),
                });
            }
            // Per-file positions and sizes.
            for fd in std::fs::read_dir(e.path().join("fd"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let Some(fdn) = fd.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let path = target.to_string_lossy().into_owned();
                if !interesting(&path) {
                    continue;
                }
                let Ok(m) = std::fs::metadata(fd.path()) else {
                    continue;
                };
                if !m.is_file() {
                    continue;
                }
                let Some((pos, mode)) =
                    std::fs::read_to_string(e.path().join(format!("fdinfo/{fdn}")))
                        .ok()
                        .and_then(|t| parse_fdinfo(&t))
                else {
                    continue;
                };
                let key = (m.dev(), m.ino());
                sizes.insert(key, m.len());
                fds.insert((pid, fdn), (pos, m.ino()));
                let entry = files.entry(key).or_insert_with(|| FileIo {
                    path: path.clone(),
                    pid,
                    process: name.clone(),
                    unit: writer_unit(&path, pid),
                    size: m.len(),
                    ..FileIo::default()
                });
                if let Some(&(ppos, pino)) = self.fds.get(&(pid, fdn)) {
                    if pino == m.ino() && pos >= ppos {
                        let moved = rate(pos - ppos);
                        // Read-write fds count as writes when the process wrote more than it read.
                        if mode == 1 || (mode == 2 && wc > rc) {
                            entry.write_bps += moved;
                        } else {
                            entry.read_bps += moved;
                        }
                    }
                }
            }
        }
        for (key, f) in files.iter_mut() {
            if let Some(&prev) = self.sizes.get(key) {
                f.growth_bps = if dt > 0.0 {
                    (f.size as f64 - prev as f64) / dt
                } else {
                    0.0
                };
                let grown = f.size as i64 - prev as i64;
                if grown != 0 {
                    self.window.push_back((now, f.path.clone(), grown, f.size));
                }
            }
        }
        while self
            .window
            .front()
            .is_some_and(|(t, ..)| now.duration_since(*t).as_secs() > 60)
        {
            self.window.pop_front();
        }
        let mut grown: HashMap<String, (i64, u64)> = HashMap::new();
        for (_, p, g, s) in &self.window {
            let e = grown.entry(p.clone()).or_default();
            e.0 += g;
            e.1 = *s;
        }
        let mut gm: Vec<(String, i64, u64)> = grown
            .into_iter()
            .filter(|(_, (g, _))| *g > 0)
            .map(|(p, (g, s))| (p, g, s))
            .collect();
        gm.sort_by_key(|(_, g, _)| std::cmp::Reverse(*g));
        gm.truncate(30);
        live.grown_minute = gm;

        let all: Vec<FileIo> = files.into_values().collect();
        let mut writing: Vec<FileIo> = all
            .iter()
            .filter(|f| f.write_bps > 0.0 || f.growth_bps > 0.0)
            .cloned()
            .collect();
        writing.sort_by(|a, b| {
            (b.write_bps.max(b.growth_bps)).total_cmp(&a.write_bps.max(a.growth_bps))
        });
        writing.truncate(40);
        let mut reading: Vec<FileIo> = all.into_iter().filter(|f| f.read_bps > 0.0).collect();
        reading.sort_by(|a, b| b.read_bps.total_cmp(&a.read_bps));
        reading.truncate(40);
        live.writing = writing;
        live.reading = reading;
        live.procs
            .sort_by(|a, b| (b.write_bps + b.read_bps).total_cmp(&(a.write_bps + a.read_bps)));
        live.procs.truncate(20);
        fds.retain(|(pid, _), _| procs.contains_key(pid));
        self.procs = procs;
        self.fds = fds;
        self.sizes = sizes;
        self.at = Some(now);
        live
    }
}

pub fn rate_text(bps: f64) -> String {
    if bps <= 0.0 {
        "-".into()
    } else {
        format!("{}/s", crate::util::human(bps as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parsers() {
        let ds = "   8       0 sda 100 0 2000 0 50 0 4000 0 0 0 0\n   8       1 sda1 1 0 2 0 1 0 2 0 0 0 0\n 252 0 dm-0 1 1 1 1 1 1 1 1 1 1 1\n 259 0 nvme0n1 1 0 8 0 1 0 16 0 0 0 0\n 259 1 nvme0n1p1 1 0 8 0 1 0 16 0 0 0 0\n";
        assert_eq!(
            parse_diskstats(ds),
            vec![
                ("sda".to_string(), 2000, 4000),
                ("nvme0n1".to_string(), 8, 16)
            ]
        );
        let io = "rchar: 10\nwchar: 20\nsyscr: 1\nsyscw: 2\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_proc_io(io), Some((4096, 8192, 10, 20)));
        assert_eq!(
            parse_fdinfo("pos:\t123\nflags:\t0100001\nmnt_id:\t29\n"),
            Some((123, 1))
        );
        assert_eq!(parse_fdinfo("pos:\t5\nflags:\t02\n"), Some((5, 2)));
    }

    #[test]
    fn details_of_this_process() {
        let d = details(std::process::id() as i32).unwrap();
        assert!(!d.name.is_empty() && !d.user.is_empty() && !d.exe.is_empty());
        assert!(d.ppid > 0 && d.threads >= 1 && d.rss_bytes > 0);
        assert!(!next_steps(&d).is_empty());
        let docker = ProcDetails {
            unit: "docker:web".into(),
            ..ProcDetails::default()
        };
        assert!(next_steps(&docker).contains("docker stop web"));
        let svc = ProcDetails {
            unit: "nginx".into(),
            ..ProcDetails::default()
        };
        assert!(next_steps(&svc).contains("systemctl stop nginx"));
    }

    #[test]
    fn sampler_sees_a_file_being_written_and_read() {
        let d = std::env::temp_dir().join(format!("dux-live-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = d.join("growing.log");
        let mut f = std::fs::File::create(&w).unwrap();
        let src = d.join("input.bin");
        std::fs::write(&src, vec![7u8; 4 << 20]).unwrap();
        let mut r = std::fs::File::open(&src).unwrap();
        let mut s = Sampler::default();
        let _ = s.sample();
        f.write_all(&vec![1u8; 1 << 20]).unwrap();
        f.flush().unwrap();
        let mut buf = vec![0u8; 1 << 20];
        use std::io::Read;
        r.read_exact(&mut buf).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let live = s.sample();
        let wpath = w.to_string_lossy().into_owned();
        let spath = src.to_string_lossy().into_owned();
        let wf = live
            .writing
            .iter()
            .find(|x| x.path == wpath)
            .expect("written file listed");
        assert!(wf.write_bps > 0.0 && wf.growth_bps > 0.0, "{wf:?}");
        assert!(
            live.reading
                .iter()
                .any(|x| x.path == spath && x.read_bps > 0.0),
            "read file listed"
        );
        assert!(live
            .grown_minute
            .iter()
            .any(|(p, g, _)| *p == wpath && *g == 1 << 20));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
