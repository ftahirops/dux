use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the index DB lives. Root-writable system path if we can, else per-user.
pub fn data_dir() -> Result<PathBuf> {
    // Prefer the system path (shared with xtop-style tooling) when writable.
    let system = PathBuf::from("/var/lib/dux");
    if can_use(&system) {
        return Ok(system);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("cannot determine data dir: set HOME or XDG_DATA_HOME")?;
    let dir = base.join("dux");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn can_use(p: &PathBuf) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    // crude writability probe
    let probe = p.join(".dux_write_probe");
    let ok = std::fs::write(&probe, b"1").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("dux.db"))
}

/// Live filesystem capacity for the filesystem containing `path` (via statvfs).
#[derive(Default, Clone, Copy)]
#[allow(dead_code)] // free/inodes_free kept for completeness / future use
pub struct FsStat {
    pub total: i64,
    pub free: i64,  // free to root
    pub avail: i64, // available to unprivileged users
    pub used: i64,
    pub inodes_total: i64,
    pub inodes_free: i64,
    pub inodes_used: i64,
}

impl FsStat {
    /// Percent used (df convention: used / (used + available)).
    pub fn use_pct(&self) -> f64 {
        let denom = self.used + self.avail;
        if denom <= 0 {
            0.0
        } else {
            self.used as f64 / denom as f64 * 100.0
        }
    }
    pub fn inode_pct(&self) -> f64 {
        if self.inodes_total <= 0 {
            0.0
        } else {
            self.inodes_used as f64 / self.inodes_total as f64 * 100.0
        }
    }
}

pub fn fs_stat(path: &std::path::Path) -> Option<FsStat> {
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return None;
    }
    let s = unsafe { s.assume_init() };
    let bs = s.f_frsize as i64;
    let total = s.f_blocks as i64 * bs;
    let free = s.f_bfree as i64 * bs;
    let avail = s.f_bavail as i64 * bs;
    let it = s.f_files as i64;
    let ifree = s.f_ffree as i64;
    Some(FsStat {
        total,
        free,
        avail,
        used: total - free,
        inodes_total: it,
        inodes_free: ifree,
        inodes_used: it - ifree,
    })
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Runtime heartbeat file (tmpfs). Liveness lives here, not in SQLite, so an
/// idle daemon performs zero database/WAL writes. systemd's RuntimeDirectory
/// creates /run/dux and removes it on stop, so the file vanishes when the
/// daemon dies — no stale "live" reading.
pub const HEARTBEAT_PATH: &str = "/run/dux/heartbeat";

/// Stamp the heartbeat file with the current epoch seconds and the absolute
/// path of the DB the daemon is writing (best-effort). Format: "<secs> <db>".
pub fn write_heartbeat(db: &std::path::Path) {
    let _ = std::fs::create_dir_all("/run/dux");
    let db = db
        .canonicalize()
        .unwrap_or_else(|_| db.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::write(HEARTBEAT_PATH, format!("{} {db}", now_secs()));
}

/// (epoch, db_path) of the last heartbeat — lets a scan tell whether the daemon
/// is writing the SAME db it's about to rebuild (per-db guard, not global).
pub fn read_heartbeat_db() -> Option<(i64, String)> {
    let s = std::fs::read_to_string(HEARTBEAT_PATH).ok()?;
    let mut it = s.splitn(2, ' ');
    let secs: i64 = it.next()?.trim().parse().ok()?;
    let db = it.next().unwrap_or("").trim().to_string();
    Some((secs, db))
}

/// True only when a daemon heartbeat is FRESH (≤30s) AND belongs to THIS db.
/// Prevents the global heartbeat from reporting an unrelated index as live.
pub fn daemon_live_for(db: &std::path::Path) -> bool {
    match read_heartbeat_db() {
        Some((secs, hbdb)) => {
            let fresh = now_secs() - secs <= 30;
            let want = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
            fresh && std::path::Path::new(&hbdb) == want.as_path()
        }
        None => false,
    }
}

/// Parse durations like "1h", "30m", "24h", "7d", "90s" into seconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| c.is_alphabetic())
            .context("duration needs a unit, e.g. 1h, 30m, 7d")?,
    );
    let n: i64 = num.trim().parse().context("invalid duration number")?;
    if n < 0 {
        anyhow::bail!("duration must not be negative");
    }
    let mult: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        other => anyhow::bail!("unknown duration unit: {other}"),
    };
    n.checked_mul(mult).context("duration too large")
}

pub fn human(bytes: i64) -> String {
    use humansize::{format_size, BINARY};
    format_size(bytes.max(0) as u64, BINARY)
}

/// Render a raw filename (bytes) for SAFE terminal display: decode lossily, then
/// escape control/escape characters. A local user can otherwise craft a filename
/// containing newlines or ANSI/OSC escape sequences (e.g. OSC 52 clipboard
/// writes) that forge or hijack the terminal of an admin running `dux`.
pub fn display_name(raw: &[u8]) -> String {
    escape_controls(&String::from_utf8_lossy(raw))
}

/// Same escaping for an already-decoded path string (CLI output paths).
pub fn display_path(s: &str) -> String {
    escape_controls(s)
}

fn escape_controls(s: &str) -> String {
    // Fast path: most paths have no control characters.
    if !s
        .chars()
        .any(|c| (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..=0x9f).contains(&(c as u32)))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..=0x9f).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Human-readable "time ago" — coarse (no ticking seconds).
pub fn ago(secs: i64) -> String {
    let d = (now_secs() - secs).max(0);
    if d < 60 {
        "now".to_string()
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86400)
    }
}
