mod deleted;
mod query;
mod scan;
mod store;
mod tui;
mod util;
mod watch;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use store::Store;
use util::{ago, human};

/// dux — persistent, realtime disk usage + file search.
/// du/ncdu/locate/find, but indexed and live. Companion to xtop.
#[derive(Parser)]
#[command(name = "dux", version, about, long_about = None)]
struct Cli {
    /// Path to open in the TUI (when no subcommand is given)
    path: Option<PathBuf>,

    /// Override index DB path
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Full scan of PATH into the index
    Scan {
        path: PathBuf,
        #[arg(long)]
        one_file_system: bool,
        #[arg(long)]
        exclude: Vec<PathBuf>,
        #[arg(long)]
        low_priority: bool,
        /// Include pseudo-filesystems (/proc, /sys, cgroup, …)
        #[arg(long)]
        include_pseudo: bool,
        /// Suppress live progress output
        #[arg(long)]
        quiet: bool,
        /// Scan even if the watch daemon is running (NOT recommended — two
        /// writers corrupt the index)
        #[arg(long)]
        force: bool,
    },
    /// Largest directories or files (instant, from index)
    Top {
        /// Restrict to this path subtree (default: whole index)
        path: Option<PathBuf>,
        #[arg(long)]
        files: bool,
        #[arg(long)]
        dirs: bool,
        /// Rank by inode/file count instead of size
        #[arg(long)]
        inodes: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Ultra-fast file search over the live index (locate/find replacement)
    Find {
        /// Restrict search to this path subtree
        path: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        ext: Option<String>,
        /// Modified within this window, e.g. 10m, 1h, 7d
        #[arg(long)]
        newer: Option<String>,
        /// Larger than, e.g. 1G, 500M
        #[arg(long)]
        larger: Option<String>,
        /// Owner uid
        #[arg(long)]
        uid: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Fastest-growing paths within a window
    Growth {
        /// Restrict to this path subtree (default: whole index)
        path: Option<PathBuf>,
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Deleted-but-open files still consuming disk
    DeletedOpen,
    /// Disk usage by owner
    ByOwner {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Disk usage by file extension
    ByExt {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// ncdu-style interactive browser
    Tui {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Index status
    Status,
    /// Run the realtime watch daemon (for systemd)
    Daemon {
        /// Filesystem root to watch
        #[arg(default_value = "/")]
        root: PathBuf,
        /// Flush coalesced changes every N ms
        #[arg(long, default_value_t = 2000)]
        flush_ms: u64,
        /// Only watch the root's filesystem (match `scan --one-file-system`)
        #[arg(long)]
        one_file_system: bool,
        /// Alert when a path grows more than this within the window (e.g. 1G)
        #[arg(long)]
        alert_threshold: Option<String>,
        /// Alert window, e.g. 1m, 10m, 1h
        #[arg(long, default_value = "10m")]
        alert_window: String,
        /// Command to run on alert (env: DUX_PATH, DUX_DELTA, DUX_DELTA_HUMAN, DUX_WINDOW)
        #[arg(long)]
        alert_exec: Option<String>,
        /// Seconds between repeat alerts for the same path
        #[arg(long, default_value_t = 300)]
        alert_debounce: i64,
    },
}

fn main() {
    // Restore default SIGPIPE so `dux ... | head` exits quietly instead of
    // panicking with "Broken pipe" (Rust ignores SIGPIPE by default).
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = real_main() {
        eprintln!("dux: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let db = match &cli.db {
        Some(p) => p.clone(),
        None => util::db_path()?,
    };

    match cli.cmd {
        Some(Cmd::Scan {
            path,
            one_file_system,
            exclude,
            low_priority,
            include_pseudo,
            quiet,
            force,
        }) => {
            // If the daemon is live for THIS db, it holds the exclusive lock and
            // is the only writer — so instead of making the user stop/scan/start
            // by hand, ask the daemon to rebuild its own index in place (atomic,
            // no downtime). This also reconciles any drift/downtime gap.
            let scanned_db = db.canonicalize().unwrap_or_else(|_| db.clone());
            let hb = util::read_heartbeat_full();
            let daemon_live = match &hb {
                Some((secs, _pid, hbdb)) => {
                    (util::now_secs() - secs) <= 30
                        && std::path::Path::new(hbdb) == scanned_db.as_path()
                }
                None => false,
            };
            if daemon_live && !force {
                request_daemon_rescan(&db, &path, &hb)?;
                return Ok(());
            }
            // No live daemon (or --force): scan directly. The exclusive per-db
            // lock is the hard guard — if something really is writing this index,
            // lock_db fails with a clear message rather than corrupting it.
            let _lock = util::lock_db(&db)?;
            let opts = scan::ScanOptions {
                one_file_system,
                exclude,
                low_priority,
                include_pseudo,
                progress: !quiet,
            };
            // Atomic rebuild: scan into a sibling file, then rename over the live
            // index — fragmentation-free, never a half-built or empty index.
            let start = std::time::Instant::now();
            let s = scan::rebuild_atomic(&db, &path, &opts)?;
            eprintln!(
                "scanned {} files, {} dirs, {} in {:.1}s ({} errors)",
                s.files,
                s.dirs,
                human(s.bytes),
                start.elapsed().as_secs_f64(),
                s.errors
            );
        }
        Some(Cmd::Top {
            path,
            files,
            dirs,
            inodes,
            limit,
        }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let want_dirs = dirs || !files; // default: dirs
            let rows = query::top(&store, want_dirs, limit, scope, inodes)?;
            if inodes {
                println!("{:<12} {:<6} PATH", "INODES", "AGE");
                for r in &rows {
                    let suffix = if r.kind == 'd' && !r.path.ends_with('/') {
                        "/"
                    } else {
                        ""
                    };
                    println!(
                        "{:<12} {:<6} {}{}",
                        r.inodes,
                        ago(r.mtime),
                        util::display_path(&r.path),
                        suffix
                    );
                }
            } else {
                print_rows(&rows);
            }
        }
        Some(Cmd::Find {
            path,
            name,
            ext,
            newer,
            larger,
            uid,
            limit,
        }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let o = query::FindOpts {
                name,
                ext,
                newer_than: newer.map(|s| util::parse_duration(&s)).transpose()?,
                larger_than: larger.map(|s| parse_size(&s)).transpose()?,
                owner_uid: uid,
                limit,
                scope,
            };
            let rows = query::find(&store, &o)?;
            print_rows(&rows);
        }
        Some(Cmd::Growth { path, since, limit }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let secs = util::parse_duration(&since)?;
            let rows = query::growth(&store, secs, limit, scope)?;
            println!("{:<14} PATH", "GROWTH");
            for r in rows {
                let sign = if r.delta >= 0 { "+" } else { "-" };
                println!(
                    "{:<14} {}",
                    format!("{sign}{}", human(r.delta.abs())),
                    util::display_path(&r.path)
                );
            }
        }
        Some(Cmd::DeletedOpen) => {
            let rows = deleted::deleted_open()?;
            if rows.is_empty() {
                println!("no deleted-but-open files found (run as root to see all processes)");
            }
            println!(
                "{:<8} {:<16} {:<8} {:<12} PATH",
                "PID", "PROCESS", "UID", "SIZE"
            );
            for r in rows {
                println!(
                    "{:<8} {:<16} {:<8} {:<12} {} (deleted)",
                    r.pid,
                    r.process,
                    r.uid,
                    human(r.size),
                    util::display_path(&r.path)
                );
            }
        }
        Some(Cmd::ByOwner { limit }) => {
            let store = Store::open_ro(&db)?;
            println!("{:<10} {:<12} FILES", "UID", "SIZE");
            for r in query::by_owner(&store, limit)? {
                println!("{:<10} {:<12} {}", r.uid, human(r.bytes), r.files);
            }
        }
        Some(Cmd::ByExt { limit }) => {
            let store = Store::open_ro(&db)?;
            println!("{:<14} {:<12} FILES", "EXT", "SIZE");
            for r in query::by_ext(&store, limit)? {
                println!(
                    "{:<14} {:<12} {}",
                    util::display_path(&r.ext),
                    human(r.bytes),
                    r.files
                );
            }
        }
        Some(Cmd::Tui { path }) => {
            let store = Store::open_ro(&db)?;
            let start = resolve_start(&store, &path)?;
            tui::run(&store, &db, start)?;
        }
        Some(Cmd::Status) => {
            let store = Store::open_ro(&db)?;
            println!("{}", query::status(&store, &db)?);
        }
        Some(Cmd::Daemon {
            root,
            flush_ms,
            one_file_system,
            alert_threshold,
            alert_window,
            alert_exec,
            alert_debounce,
        }) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let alert = match alert_threshold {
                Some(t) => Some(watch::AlertConfig {
                    threshold: parse_size(&t)?,
                    window: util::parse_duration(&alert_window)?,
                    exec: alert_exec,
                    debounce: alert_debounce,
                }),
                None => None,
            };
            watch::run_daemon(&db, &root, flush_ms, one_file_system, alert)?;
        }
        None => {
            // bare `dux` or `dux <path>` opens the TUI
            let store = Store::open_ro(&db)?;
            let start = match &cli.path {
                Some(p) => resolve_start(&store, p)?,
                None => None,
            };
            tui::run(&store, &db, start)?;
        }
    }
    Ok(())
}

/// Last-scan timestamp stored in the index (0 if unreadable). Used to detect
/// when a daemon-driven rescan has finished (the daemon writes a fresh value).
fn read_last_scan_ts(db: &std::path::Path) -> i64 {
    Store::open_ro(db)
        .ok()
        .and_then(|s| s.get_meta("last_scan_ts").ok().flatten())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Ask the running daemon to rebuild its index in place (SIGHUP), then wait for
/// it to finish. Keeps the user out of the stop/scan/start dance and avoids the
/// two-writer corruption the lock would otherwise reject.
fn request_daemon_rescan(
    db: &std::path::Path,
    requested: &std::path::Path,
    hb: &Option<(i64, i32, String)>,
) -> Result<()> {
    // The daemon rescans the tree IT indexes; warn if that's not what was asked.
    if let Ok(store) = Store::open_ro(db) {
        if let Ok(Some(root)) = store.get_meta("last_scan_root") {
            let want = requested
                .canonicalize()
                .unwrap_or_else(|_| requested.to_path_buf());
            if std::path::Path::new(&root) != want {
                eprintln!(
                    "dux: note — the daemon indexes {root}; it will rescan THAT tree.\n\
                     \x20     To index a different tree, stop the daemon first \
                     (sudo systemctl stop dux), then scan."
                );
            }
        }
    }

    let pid = hb.as_ref().map(|(_, p, _)| *p).unwrap_or(0);
    if pid <= 0 {
        anyhow::bail!(
            "the daemon is running but did not report its PID (older build?).\n\
             Restart it once (sudo systemctl restart dux) so scans can drive it, \
             or stop it, scan, and start it."
        );
    }

    let prev_ts = read_last_scan_ts(db);
    // SIGHUP = "rescan now"; the daemon picks it up at the top of its loop.
    let rc = unsafe { libc::kill(pid, libc::SIGHUP) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        anyhow::bail!(
            "could not signal the daemon (pid {pid}): {e}\n\
             Stop it, scan, and start it manually if this persists."
        );
    }
    eprintln!("dux: daemon is live — triggering an in-place atomic rescan (no downtime)…");

    // Wait for the daemon's last_scan_ts to advance past the old value. prev_ts is
    // the PREVIOUS scan (seconds/minutes ago), so the fresh one is strictly newer.
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(1800);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if read_last_scan_ts(db) > prev_ts {
            eprintln!(
                "dux: rescan complete — index rebuilt in {:.0}s.",
                start.elapsed().as_secs_f64()
            );
            return Ok(());
        }
        if start.elapsed() > timeout {
            eprintln!(
                "dux: rescan still running in the daemon after {}s — check `dux status`.",
                timeout.as_secs()
            );
            return Ok(());
        }
    }
}

/// Resolve a TUI start path to a (dev,inode) start node, or None for the root.
fn resolve_start(store: &Store, path: &std::path::Path) -> Result<Option<(i64, i64)>> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path)
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {e}", path.display()))?;
    let id = (m.dev() as i64, m.ino() as i64);
    let ok = store
        .conn
        .query_row(
            "SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2",
            rusqlite::params![id.0, id.1],
            |_| Ok(()),
        )
        .is_ok();
    if !ok {
        anyhow::bail!(
            "{} is not in the index — run `dux scan` first",
            path.display()
        );
    }
    Ok(Some(id))
}

fn print_rows(rows: &[query::Row]) {
    println!("{:<14} {:<6} PATH", "SIZE", "AGE");
    for r in rows {
        let suffix = if r.kind == 'd' && !r.path.ends_with('/') {
            "/"
        } else {
            ""
        };
        // escape control/escape chars: a crafted filename must not inject ANSI/
        // OSC sequences or newlines into the terminal of whoever runs `dux`.
        println!(
            "{:<14} {:<6} {}{}",
            human(r.size),
            ago(r.mtime),
            util::display_path(&r.path),
            suffix
        );
    }
}

/// Parse sizes like 1G, 500M, 10K, 1024.
fn parse_size(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| c.is_alphabetic()) {
        Some(i) => s.split_at(i),
        None => (s, ""),
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid size"))?;
    if !n.is_finite() || n < 0.0 {
        anyhow::bail!("size must be a non-negative number");
    }
    let mult = match unit.to_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0_f64.powi(4),
        other => anyhow::bail!("unknown size unit: {other}"),
    };
    let bytes = n * mult;
    if bytes >= i64::MAX as f64 {
        anyhow::bail!("size too large");
    }
    Ok(bytes as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-t-{tag}-{}", std::process::id()))
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(util::parse_duration("90s").unwrap(), 90);
        assert_eq!(util::parse_duration("5m").unwrap(), 300);
        assert_eq!(util::parse_duration("1h").unwrap(), 3600);
        assert_eq!(util::parse_duration("7d").unwrap(), 604800);
        assert!(util::parse_duration("12x").is_err());
    }

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_size("1Z").is_err());
    }

    // Scan a temp tree and assert: sparse files count 0 blocks, disk usage uses
    // blocks, path scoping isolates subtrees, hardlinks don't double-count.
    #[test]
    fn scan_blocks_scope_hardlink() {
        let dir = tmp("tree");
        let db = tmp("db.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        // 8 KiB real file
        std::fs::write(dir.join("a/real.bin"), vec![7u8; 8192]).unwrap();
        // sparse: 1 GiB apparent, ~0 allocated
        let f = std::fs::File::create(dir.join("a/sparse.bin")).unwrap();
        f.set_len(1 << 30).unwrap();
        // hardlink pair in b/
        std::fs::write(dir.join("b/orig.dat"), vec![1u8; 4096]).unwrap();
        std::fs::hard_link(dir.join("b/orig.dat"), dir.join("b/link.dat")).unwrap();

        let mut store = Store::open_rw(&db).unwrap();
        let opts = scan::ScanOptions {
            progress: false,
            ..Default::default()
        };
        let stats = scan::scan(&mut store, &dir, &opts).unwrap();
        assert_eq!(stats.errors, 0);
        drop(store);

        let store = Store::open_ro(&db).unwrap();

        // sparse file reports ~0 blocks, NOT 1 GiB
        let files = query::top(&store, false, 50, None, false).unwrap();
        let sparse = files
            .iter()
            .find(|r| r.path.ends_with("sparse.bin"))
            .unwrap();
        assert!(
            sparse.size < 64 * 1024,
            "sparse counted as {} bytes",
            sparse.size
        );
        let real = files.iter().find(|r| r.path.ends_with("real.bin")).unwrap();
        assert_eq!(real.size, 8192);

        // path scoping: a query under a/ must not return b/ entries
        let a_id = {
            let m = std::fs::symlink_metadata(dir.join("a")).unwrap();
            Some((m.dev() as i64, m.ino() as i64))
        };
        let scoped = query::top(&store, false, 50, a_id, false).unwrap();
        assert!(
            scoped.iter().all(|r| !r.path.contains("/b/")),
            "scope leaked into b/"
        );
        assert!(scoped.iter().any(|r| r.path.ends_with("real.bin")));

        // hardlink: disk usage counts the shared inode once (b/ recursive == 4 KiB,
        // not 8 KiB), even though two names exist
        let b_id = {
            let m = std::fs::symlink_metadata(dir.join("b")).unwrap();
            (m.dev() as i64, m.ino() as i64)
        };
        let b_total: i64 = store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                rusqlite::params![b_id.0, b_id.1],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            b_total < 8192 + 8192,
            "hardlink double-counted: b total = {b_total}"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db);
    }
}
