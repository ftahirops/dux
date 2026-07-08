mod containers;
mod deleted;
mod emit;
mod guard;
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

    /// Emit machine-readable JSON instead of a table (read commands).
    #[arg(long, global = true)]
    json: bool,

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
        /// Run gently in the background: low CPU/IO priority AND fewer walker
        /// threads (~¼ of cores) so a production box keeps headroom.
        #[arg(long)]
        low_priority: bool,
        /// Cap walker threads (overrides the default / --low-priority). e.g. 2.
        #[arg(long)]
        jobs: Option<usize>,
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
    /// What changed the disk within a window (net growth AND frees), ranked by
    /// magnitude — the "what filled/freed the disk?" query.
    #[command(alias = "since")]
    Diff {
        /// Restrict to this path subtree (default: whole index)
        path: Option<PathBuf>,
        /// Window to compare over, e.g. 1h, 8h, 24h
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// du-compatible disk usage from the index (fast, no re-walk)
    Du {
        /// Path to summarize (default: whole index)
        path: Option<PathBuf>,
        /// Display only a total for the path (du -s)
        #[arg(short = 's', long)]
        summarize: bool,
        /// Include files, not just directories (du -a)
        #[arg(short = 'a', long)]
        all: bool,
        /// Human-readable sizes (du -h)
        #[arg(short = 'h', long)]
        human: bool,
        /// Size in megabytes (du -m). Default unit is 1 KiB blocks (du/-k).
        #[arg(short = 'm', long = "megabytes")]
        megabytes: bool,
        /// Only show entries up to N levels below the path (du --max-depth=N)
        #[arg(long)]
        max_depth: Option<usize>,
    },
    /// Prometheus text-exposition metrics (for the node_exporter textfile collector)
    Metrics {
        /// Include the top-N largest directories as dux_path_bytes series
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Disk usage by container (Docker/Podman): writable layer, logs, volumes
    Containers {
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
        /// Days of growth history to keep. Lower = smaller index on a high-churn
        /// host (growth dominates index size). Min 1.
        #[arg(long, default_value_t = 7)]
        growth_days: i64,
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
    let json = cli.json;

    match cli.cmd {
        Some(Cmd::Scan {
            path,
            one_file_system,
            exclude,
            low_priority,
            jobs,
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
                Some((secs, pid, hbdb)) => {
                    // Heartbeat can stay "fresh" (≤30s) for a while AFTER a crash;
                    // verify the PID is actually alive (kill(pid,0)) so a post-crash
                    // `dux scan` reconciles directly instead of trying to SIGHUP a
                    // dead process and failing exactly when it's needed most.
                    let alive = *pid > 0 && unsafe { libc::kill(*pid, 0) } == 0;
                    (util::now_secs() - secs) <= 30
                        && std::path::Path::new(hbdb) == scanned_db.as_path()
                        && alive
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
            // A scan buffers node metadata in RAM; make dux the preferred OOM
            // victim so a big scan can never get a real workload killed instead.
            guard::oom_protect_self();
            let opts = scan::ScanOptions {
                one_file_system,
                exclude,
                low_priority,
                include_pseudo,
                progress: !quiet,
                jobs,
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
            if json {
                emit::rows(&rows);
            } else if inodes {
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
            if json {
                emit::rows(&rows);
            } else {
                print_rows(&rows);
            }
        }
        Some(Cmd::Growth { path, since, limit }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let secs = util::parse_duration(&since)?;
            let rows = query::growth(&store, secs, limit, scope)?;
            if json {
                emit::growth(&rows);
                return Ok(());
            }
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
            if json {
                emit::deleted_open(&rows);
                return Ok(());
            }
            if rows.is_empty() {
                println!("no deleted-but-open files found (run as root to see all processes)");
                return Ok(());
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
            let rows = query::by_owner(&store, limit)?;
            if json {
                emit::owners(&rows);
                return Ok(());
            }
            println!("{:<10} {:<12} FILES", "UID", "SIZE");
            for r in rows {
                println!("{:<10} {:<12} {}", r.uid, human(r.bytes), r.files);
            }
        }
        Some(Cmd::ByExt { limit }) => {
            let store = Store::open_ro(&db)?;
            let exts = query::by_ext(&store, limit)?;
            if json {
                emit::exts(&exts);
                return Ok(());
            }
            println!("{:<14} {:<12} FILES", "EXT", "SIZE");
            for r in exts {
                println!(
                    "{:<14} {:<12} {}",
                    util::display_path(&r.ext),
                    human(r.bytes),
                    r.files
                );
            }
        }
        Some(Cmd::Diff { path, since, limit }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let secs = util::parse_duration(&since)?;
            let rows = query::changed(&store, secs, limit, scope)?;
            if json {
                emit::growth(&rows);
                return Ok(());
            }
            if rows.is_empty() {
                println!("no changes recorded in that window (needs a running daemon for history)");
                return Ok(());
            }
            println!("{:<14} PATH", "CHANGE");
            for r in rows {
                let sign = if r.delta >= 0 { "+" } else { "-" };
                println!(
                    "{:<14} {}",
                    format!("{sign}{}", human(r.delta.abs())),
                    util::display_path(&r.path)
                );
            }
        }
        Some(Cmd::Du {
            path,
            summarize,
            all,
            human: h,
            megabytes,
            max_depth,
        }) => {
            let store = Store::open_ro(&db)?;
            let scope = match &path {
                Some(p) => query::resolve_scope(&store, p)?,
                None => None,
            };
            // du size formatting: default 1 KiB blocks (rounded up, like du/-k),
            // -m megabytes (rounded up), -h human. All from ALLOCATED bytes.
            let fmt = |b: i64| -> String {
                if h {
                    human(b)
                } else if megabytes {
                    format!("{}", (b + (1 << 20) - 1) / (1 << 20))
                } else {
                    format!("{}", (b + 1023) / 1024)
                }
            };
            let mut rows = query::du(&store, scope, all)?;
            // du order: a directory prints AFTER its descendants (post-order); a
            // reverse-lexicographic sort puts deeper paths first and the root last.
            rows.sort_by(|a, b| b.path.cmp(&a.path));
            // depth relative to the deepest common root (the queried path's root):
            // the shortest path in the set is the subtree root.
            let root_depth = rows
                .iter()
                .map(|r| r.path.trim_end_matches('/').matches('/').count())
                .min()
                .unwrap_or(0);
            if summarize || max_depth.is_some() {
                let maxd = if summarize { Some(0) } else { max_depth };
                if let Some(md) = maxd {
                    rows.retain(|r| {
                        r.path.trim_end_matches('/').matches('/').count() - root_depth <= md
                    });
                }
            }
            if json {
                emit::du(&rows);
                return Ok(());
            }
            for r in &rows {
                // du format: SIZE<TAB>PATH
                println!("{}\t{}", fmt(r.bytes), util::display_path(&r.path));
            }
        }
        Some(Cmd::Metrics { top }) => {
            let store = Store::open_ro(&db)?;
            print!("{}", metrics_text(&store, &db, top)?);
        }
        Some(Cmd::Containers { limit }) => {
            let store = Store::open_ro(&db)?;
            let mut rows = containers::list(&store)?;
            rows.truncate(limit);
            if json {
                emit::containers(&rows);
                return Ok(());
            }
            if rows.is_empty() {
                println!(
                    "no containers found — Docker/Podman not present, or their storage dir \
                     isn't indexed yet (scan a path that includes /var/lib/docker)."
                );
                return Ok(());
            }
            println!(
                "{:<7} {:<20} {:<26} {:>10} {:>9} {:>9} {:>10}",
                "RUNTIME", "NAME", "IMAGE", "WRITABLE", "LOGS", "VOLUMES", "TOTAL"
            );
            for c in &rows {
                println!(
                    "{:<7} {:<20} {:<26} {:>10} {:>9} {:>9} {:>10}",
                    c.runtime,
                    trunc(&util::display_path(&c.name), 20),
                    trunc(&util::display_path(&c.image), 26),
                    human(c.writable_bytes),
                    human(c.log_bytes),
                    human(c.volume_bytes),
                    human(c.total()),
                );
            }
        }
        Some(Cmd::Tui { path }) => {
            // Resolve the start node, then DROP this connection before the TUI runs
            // — run() owns its own reopenable store, so we must not pin this one for
            // the whole session (it would hold a deleted inode open after a rescan).
            let start = {
                let store = Store::open_ro(&db)?;
                resolve_start(&store, &path)?
            };
            tui::run(&db, start)?;
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
            growth_days,
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
            watch::run_daemon(&db, &root, flush_ms, one_file_system, alert, growth_days)?;
        }
        None => {
            // bare `dux` or `dux <path>` opens the TUI. Drop this connection before
            // the TUI runs (run() owns its own reopenable store — see above).
            let start = {
                let store = Store::open_ro(&db)?;
                match &cli.path {
                    Some(p) => resolve_start(&store, p)?,
                    None => None,
                }
            };
            tui::run(&db, start)?;
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
    let mut poll_store = Store::open_ro(db).ok();
    let mut last_reopen = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let cur_ts = match poll_store.as_ref() {
            Some(s) => s
                .get_meta("last_scan_ts")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            None => read_last_scan_ts(db),
        };
        if cur_ts > prev_ts {
            eprintln!(
                "dux: rescan complete — index rebuilt in {:.0}s.",
                start.elapsed().as_secs_f64()
            );
            return Ok(());
        }
        // The daemon swaps in a new DB by rename. A read connection can keep
        // seeing the old inode, so reopen occasionally while still avoiding a
        // twice-per-second open/schema check loop during long rescans.
        if last_reopen.elapsed() >= std::time::Duration::from_secs(5) {
            poll_store = Store::open_ro(db).ok();
            last_reopen = std::time::Instant::now();
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
/// Truncate a display string to `n` columns (char-approx), ellipsizing.
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Escape a Prometheus label value: backslash, double-quote and newline per the
/// exposition format; any other control char becomes a space so a crafted path
/// can't break the line format.
fn prom_label(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// Prometheus text-exposition output for the node_exporter textfile collector.
fn metrics_text(store: &Store, db: &std::path::Path, topn: usize) -> Result<String> {
    let root = store.get_meta("last_scan_root")?.unwrap_or_default();
    let ts: i64 = store
        .get_meta("last_scan_ts")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (nodes, bytes) = query::index_totals(store);
    let daemon = util::daemon_live_for(db) as i32;

    let mut s = String::new();
    let mut g = |name: &str, help: &str, val: String| {
        s.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {val}\n"
        ));
    };
    g("dux_up", "1 if the dux index is readable.", "1".into());
    g("dux_index_nodes", "Inodes tracked in the index.", nodes.to_string());
    g(
        "dux_index_bytes",
        "Allocated bytes tracked by the index.",
        bytes.to_string(),
    );
    g(
        "dux_last_scan_timestamp_seconds",
        "Unix time of the last full scan.",
        ts.to_string(),
    );
    g(
        "dux_daemon_up",
        "1 if the live watch daemon is running.",
        daemon.to_string(),
    );
    if let Some(fs) = util::fs_stat(std::path::Path::new(&root)) {
        g(
            "dux_fs_bytes_total",
            "Total bytes of the scanned filesystem.",
            fs.total.to_string(),
        );
        g(
            "dux_fs_bytes_used",
            "Used bytes of the scanned filesystem.",
            fs.used.to_string(),
        );
        g(
            "dux_fs_bytes_avail",
            "Bytes available to unprivileged users.",
            fs.avail.to_string(),
        );
        g(
            "dux_fs_inodes_total",
            "Total inodes of the scanned filesystem.",
            fs.inodes_total.to_string(),
        );
        g(
            "dux_fs_inodes_used",
            "Used inodes of the scanned filesystem.",
            fs.inodes_used.to_string(),
        );
    }
    if topn > 0 {
        let rows = query::top(store, true, topn, None, false)?;
        s.push_str(
            "# HELP dux_path_bytes Allocated bytes of a top directory.\n\
             # TYPE dux_path_bytes gauge\n",
        );
        for r in rows {
            s.push_str(&format!(
                "dux_path_bytes{{path=\"{}\"}} {}\n",
                prom_label(&r.path),
                r.size
            ));
        }
    }
    Ok(s)
}

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
    // Split the trailing UNIT letters from the number. Splitting on the FIRST
    // alpha char mishandles scientific notation ("1e3" would break at the 'e');
    // instead take the unit as the trailing run of ASCII-alphabetic chars. Use
    // char_indices (NOT `rfind(..)+1`, which lands mid-char on a multibyte input
    // like "б" and panics in split_at) so the split is always on a char boundary.
    let unit_start = s
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic())
        .last()
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(unit_start);
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
        // scientific notation must not be mis-split at the 'e' (regression: the
        // old first-alpha split turned "1e3" into num="1", unit="e3" -> error).
        assert_eq!(parse_size("1e3").unwrap(), 1000);
        assert_eq!(parse_size("1.5K").unwrap(), 1536);
        assert_eq!(parse_size("2KiB").unwrap(), 2048);
        assert!(parse_size("abc").is_err());
        // multibyte / non-ASCII input must ERROR, never panic in split_at (fuzz
        // regression: "б"/"ε"/"⚡" made the old rfind()+1 split land mid-char).
        for bad in ["б", "ε", "⚡", "1б", "б1", "1.5и", "M⚡K", " ", ""] {
            assert!(parse_size(bad).is_err(), "expected Err for {bad:?}");
        }
    }

    #[test]
    fn prom_label_escapes_injection() {
        // a crafted path must not break the exposition line format.
        assert_eq!(prom_label(r#"/a/b"#), r#"/a/b"#);
        assert_eq!(prom_label("/a\"b\\c"), "/a\\\"b\\\\c");
        assert_eq!(prom_label("/a\nb"), "/a\\nb");
        assert_eq!(prom_label("/a\tb"), "/a b"); // other control -> space
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
