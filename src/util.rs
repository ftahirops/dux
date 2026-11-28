use anyhow::{Context, Result};
use std::time::{SystemTime, UNIX_EPOCH};

/// Convert the kernel's 512-byte allocated-block count to the signed byte
/// representation used in SQLite. Refuse overflow so disk totals never become
/// negative or silently wrap when a filesystem exposes an unusually large file.
pub fn allocated_bytes(metadata: &std::fs::Metadata) -> Result<i64> {
    use std::os::unix::fs::MetadataExt;
    let bytes = u128::from(metadata.blocks()) * 512;
    i64::try_from(bytes).context("allocated file size exceeds supported range")
}

/// A change dux refused on purpose for safety (never-touch, kill switch,
/// system path, open file...), as opposed to a failure. Exit code 3.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Like `anyhow::ensure!`, but the error is a safety refusal (exit code 3).
#[macro_export]
macro_rules! refuse_unless {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(anyhow::Error::new($crate::util::Refused(format!($($arg)+))));
        }
    };
}

#[cfg(test)]
fn allocated_bytes_from_blocks(blocks: u64) -> Result<i64> {
    let bytes = u128::from(blocks) * 512;
    i64::try_from(bytes).context("allocated file size exceeds supported range")
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Send an sd_notify datagram when systemd supplied NOTIFY_SOCKET. Implemented
/// directly to avoid a runtime dependency and to support both pathname and
/// Linux abstract (@name) UNIX sockets. Best-effort outside systemd.
pub fn systemd_notify(message: &str) {
    use std::os::unix::ffi::OsStrExt;
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let bytes = socket.as_os_str().as_bytes();
    if bytes.is_empty() {
        return;
    }
    // SAFETY: socket takes scalar arguments and returns a new owned descriptor.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return;
    }
    // SAFETY: zero is valid for sockaddr_un; the family and path are filled below.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let max = addr.sun_path.len();
    let (copy, abstract_socket) = if bytes[0] == b'@' {
        (&bytes[1..], true)
    } else {
        (bytes, false)
    };
    let start = usize::from(abstract_socket);
    if copy.len() + start >= max {
        // SAFETY: `fd` is the newly created socket and has not been transferred.
        unsafe { libc::close(fd) };
        return;
    }
    for (i, b) in copy.iter().enumerate() {
        addr.sun_path[start + i] = *b as libc::c_char;
    }
    // One leading NUL for abstract sockets or one trailing NUL for path sockets.
    let path_len = copy.len() + 1;
    let addr_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_len) as libc::socklen_t;
    // SAFETY: `addr` is initialized, `addr_len` is bounded by sun_path, and the
    // message slice remains alive for the duration of the synchronous sendto.
    let _ = unsafe {
        libc::sendto(
            fd,
            message.as_ptr().cast(),
            message.len(),
            libc::MSG_NOSIGNAL,
            (&addr as *const libc::sockaddr_un).cast(),
            addr_len,
        )
    };
    // SAFETY: `fd` is still uniquely owned by this function.
    unsafe { libc::close(fd) };
}

#[repr(C)]
struct InodeStatxTime {
    tv_sec: i64,
    tv_nsec: u32,
    _reserved: i32,
}

// Linux UAPI linux/stat.h. libc's musl bindings do not expose this structure;
// the kernel syscall uses the same fixed 256-byte layout with either libc.
#[repr(C)]
struct InodeStatx {
    stx_mask: u32,
    _blksize: u32,
    _attributes: u64,
    _nlink: u32,
    _uid: u32,
    _gid: u32,
    _mode: u16,
    _spare0: u16,
    stx_ino: u64,
    _size: u64,
    _blocks: u64,
    _attributes_mask: u64,
    _atime: InodeStatxTime,
    stx_btime: InodeStatxTime,
    _ctime: InodeStatxTime,
    _mtime: InodeStatxTime,
    _rdev_major: u32,
    _rdev_minor: u32,
    stx_dev_major: u32,
    stx_dev_minor: u32,
    stx_mnt_id: u64,
    _reserved: [u64; 13],
}
const _: () = {
    assert!(std::mem::size_of::<InodeStatx>() == 256);
    assert!(std::mem::offset_of!(InodeStatx, stx_btime) == 80);
    assert!(std::mem::offset_of!(InodeStatx, stx_dev_major) == 136);
    assert!(std::mem::offset_of!(InodeStatx, stx_mnt_id) == 144);
};
const STATX_INODE: u32 = 0x100;
const STATX_MNT_ID: u32 = 0x1000;

/// Which mount an open file descriptor is on: (device, mount id), using the
/// raw statx syscall so it works with glibc and musl alike. The mount id is
/// 0 on kernels that do not report it.
pub fn fd_mount(fd: libc::c_int) -> Option<(u64, u64)> {
    let mut stat = std::mem::MaybeUninit::<InodeStatx>::zeroed();
    // SAFETY: an empty path with AT_EMPTY_PATH stats the open fd itself; the
    // output storage is sized as InodeStatx; success is checked before reading.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            STATX_INODE | STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return None;
    }
    // SAFETY: a successful statx call initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let dev = (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor);
    let mnt = if stat.stx_mask & STATX_MNT_ID != 0 {
        stat.stx_mnt_id
    } else {
        0
    };
    Some((dev, mnt))
}

/// Parse durations like "1h", "30m", "24h", "7d", "90s" into seconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    // Zero needs no unit ("0" = never, for repeat settings).
    if s == "0" {
        return Ok(0);
    }
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

/// Format the magnitude of a signed delta without overflowing on `i64::MIN`.
pub fn human_magnitude(bytes: i64) -> String {
    use humansize::{format_size, BINARY};
    format_size(bytes.unsigned_abs(), BINARY)
}

/// Same escaping for an already-decoded path string (CLI output paths).
pub fn display_path(s: &str) -> String {
    escape_controls(s)
}

fn escape_controls(s: &str) -> String {
    // Fast path: most paths have no control characters.
    if !s.chars().any(char::is_control) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Human-readable "time ago" — coarse (no ticking seconds).
pub fn ago(secs: i64) -> String {
    let d = now_secs().saturating_sub(secs).max(0);
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

/// Terminal width for laying out lines (COLUMNS, then the tty, then 100).
pub fn term_width() -> usize {
    if let Some(c) = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok()) {
        return c;
    }
    let mut ws = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: TIOCGWINSZ writes a winsize into the provided storage on success.
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) };
    if rc == 0 {
        // SAFETY: ioctl succeeded.
        let ws = unsafe { ws.assume_init() };
        if ws.ws_col > 20 {
            return usize::from(ws.ws_col);
        }
    }
    100
}

/// Shorten text to `max` characters by cutting the middle ("/var/…/file.log").
pub fn shorten_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 8 {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 3;
    let tail = keep - head;
    let a: String = s.chars().take(head).collect();
    let b: String = s.chars().skip(n - tail).collect();
    format!("{a}…{b}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_block_conversion_rejects_overflow() {
        assert_eq!(allocated_bytes_from_blocks(2).unwrap(), 1024);
        assert!(allocated_bytes_from_blocks(u64::MAX)
            .unwrap_err()
            .to_string()
            .contains("allocated file size exceeds supported range"));
    }
}
