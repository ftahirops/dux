mod attrib;
mod classify;
mod cleanup;
mod containers;
mod deleted;
mod docker;
mod emit;
mod fault;
mod fix;
mod gather;
mod guard;
mod insight;
mod live;
mod notify;
mod probes;
mod protect;
mod query;
mod scan;
mod sentinel;
mod snapshot;
mod store;
mod tunnel;
mod ui;
mod util;
mod walker;
mod watch;
mod web;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use store::Store;
use util::{ago, human};

/// dux — disk-full alerts that say which folder grew, who is writing and
/// the safe fix. Start with `dux sentinel`, `dux doctor /` and `dux where /`.
#[derive(Parser)]
#[command(name = "dux", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DUX_BUILD_REVISION"), ") [", env!("DUX_BUILD_KIND"), "] source-sha256=", env!("DUX_BUILD_SOURCE_SHA256")), about, long_about = None)]
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

#[derive(Clone, Copy, ValueEnum)]
enum TopSort {
    Size,
    #[value(alias = "age")]
    Oldest,
    Newest,
    Path,
    Owner,
    Safety,
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
    /// One-shot CLI dashboard: largest paths, fastest growth, changes, Docker reclaimable
    #[command(alias = "summary", alias = "dashboard", alias = "report")]
    Overview {
        /// Restrict path answers to this subtree (default: whole index)
        path: Option<PathBuf>,
        /// Growth/change window, e.g. 1h, 24h, 7d
        #[arg(long, default_value = "1h")]
        since: String,
        /// Rows per section
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Largest directories or files (instant, from index)
    #[command(alias = "largest", alias = "large")]
    Top {
        /// Restrict to this path subtree (default: whole index)
        path: Option<PathBuf>,
        #[arg(long, alias = "file")]
        files: bool,
        #[arg(long, alias = "directories")]
        dirs: bool,
        /// Rank by inode/file count instead of size
        #[arg(long)]
        inodes: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Add owner, semantic type, deletion safety, and owning application
        #[arg(long, alias = "details", alias = "context")]
        explain: bool,
        /// Sort the displayed largest-file set
        #[arg(long, value_enum, default_value_t = TopSort::Size)]
        sort: TopSort,
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
    #[command(alias = "fastest-growth", alias = "growing")]
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
    #[command(alias = "since", alias = "changed", alias = "activity")]
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
    #[command(alias = "leaks")]
    DeletedOpen,
    /// Disk usage by owner
    #[command(alias = "owners")]
    ByOwner {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Disk usage by file extension
    #[command(alias = "extensions", alias = "exts")]
    ByExt {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Explain what an indexed file belongs to and whether/how it may be removed
    Explain { path: PathBuf },
    /// Terminal UI (same as plain `dux`): Disks, Activity, Folders, Docker,
    /// Logs, Projects, Fixes
    Tui {
        #[arg(default_value = "/")]
        path: PathBuf,
    },
    /// Paths dux must never delete, truncate or offer as a fix
    /// (/etc/dux/never-touch). `add PATH`, `remove PATH`, or `list`.
    NeverTouch {
        #[arg(value_parser = ["add", "remove", "list"], default_value = "list")]
        op: String,
        path: Option<PathBuf>,
    },
    /// Index status
    Status,
    /// Legacy 0.6 per-file index daemon (not installed by the 0.7 package)
    Daemon {
        /// Filesystem root to watch
        #[arg(default_value = "/")]
        root: PathBuf,
        /// Flush coalesced changes every N ms
        #[arg(long, default_value_t = 2000, value_parser = clap::value_parser!(u64).range(10..=60000))]
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
        /// Let alert hooks inherit the daemon's CAP_SYS_ADMIN and
        /// CAP_DAC_READ_SEARCH capabilities (default: clear them before exec).
        #[arg(long)]
        alert_exec_keep_capabilities: bool,
        /// Seconds between repeat alerts for the same path
        #[arg(long, default_value_t = 300)]
        alert_debounce: i64,
        /// Days of growth history to keep. Lower = smaller index on a high-churn
        /// host (growth dominates index size). Min 1.
        #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(i64).range(1..=3650))]
        growth_days: i64,
        /// Target average CPU duty cycle for the fanotify event-loop thread, as a
        /// percent of ONE core. This does not cap SQLite, scan, or other worker
        /// threads; systemd CPUQuota is the process-wide ceiling. Under event
        /// storms the event loop sleeps and the kernel queue buffers work; overflow
        /// marks coverage uncertain and requires reconciliation. Lower = quieter,
        /// with more lag. The daemon also runs at idle I/O priority.
        #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u32).range(1..=100))]
        max_cpu: u32,
        /// Compatibility mode: apply fanotify events directly to the index.
        /// The default is reconciliation-first: scans define the last-observed
        /// view and fanotify provides lossy hints. Keep this only for controlled
        /// comparison or rollback while migrating existing deployments. The
        /// reconciliation-first default still requires release qualification.
        #[arg(long, conflicts_with = "reconcile_first")]
        event_apply: bool,
        /// Compatibility alias for the former opt-in reconciliation-first mode.
        #[arg(long, hide = true, conflicts_with = "event_apply")]
        reconcile_first: bool,
    },
    /// Watch free space and inodes on every mount every few seconds and send
    /// alerts: system log, ntfy, Slack, webhook, email, and wall for critical
    /// ones. Exact and cheap: one statvfs call per mount, no scanning.
    Sentinel {
        /// KEY=VALUE settings file with channels and thresholds
        #[arg(long, default_value = notify::DEFAULT_CONFIG)]
        config: PathBuf,
        /// Check once: print a df-style summary, alert on thresholds, exit
        #[arg(long)]
        once: bool,
        /// Print the alerts that would be sent instead of sending them
        #[arg(long)]
        dry_run: bool,
    },
    /// Walk one filesystem fast into an in-memory folder tree and show the
    /// largest folders. Needs no index and writes nothing.
    Walk {
        /// Directory to walk (stays on its filesystem)
        #[arg(default_value = "/")]
        path: PathBuf,
        /// Worker threads
        #[arg(long, default_value_t = 4)]
        threads: usize,
        /// Depth below the path to rank folders at
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// How many folders to show
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// One-shot disk health report: walks a filesystem once and shows the
    /// biggest folders, known culprits with their safe fix, folders with the
    /// most files, space held by deleted-but-open files and the top writers.
    Doctor {
        /// Filesystem to check
        #[arg(default_value = "/")]
        path: PathBuf,
        /// Worker threads for the walk
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Where the space is, from the latest sentinel folder snapshot, and
    /// what grew since the snapshot before it. Instant; walks nothing.
    Where {
        /// Mount point
        #[arg(default_value = "/")]
        mount: String,
        /// Snapshot directory
        #[arg(long, default_value = snapshot::DEFAULT_DIR)]
        dir: PathBuf,
        /// How many folders to show
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Safe, levelled cleanup. Shows the plan; changes nothing without --apply.
    /// 1 = shrink the journal; 2 = + rotated logs, and big active logs when the
    /// disk is at least 80% full; 3 = + package caches, old temp files, old core
    /// dumps, Docker build cache.
    Cleanup {
        /// Cleanup level, 0-3
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(0..=3))]
        level: u8,
        /// Carry out the plan (default: preview only)
        #[arg(long)]
        apply: bool,
        /// Journal size budget for level 1
        #[arg(long, default_value = "1G")]
        journal_max: String,
        /// An active log counts as big from this size (level 2)
        #[arg(long, default_value = "50M")]
        big_log: String,
    },
    /// Where Docker's disk space goes: images (unique vs shared), container
    /// writable layers and logs, volumes, build cache, real folder sizes on
    /// disk and a per compose project roll-up, plus a previewed cleanup plan.
    Docker {
        #[command(flatten)]
        fixes: fix::FixArgs,
        /// Container logs at least this big are trimmed to their last 10 MiB
        #[arg(long, default_value = "100M")]
        big_log: String,
        /// Rows per section
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Biggest files and folders on a filesystem, from one fast walk, plus
    /// what grew since the last sentinel snapshot.
    Biggest {
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(long, default_value_t = 25)]
        top: usize,
        /// Only list files at least this big
        #[arg(long, default_value = "50M")]
        min: String,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Every log on a filesystem: journal, system, Docker and application logs,
    /// active vs rotated, live growth per minute and the process writing each.
    Logs {
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(long, default_value_t = 25)]
        top: usize,
        /// Only list logs at least this big
        #[arg(long, default_value = "1M")]
        min: String,
        /// Seconds to sample log growth (0 = skip)
        #[arg(long, default_value_t = 3)]
        sample: u64,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Per project and per app breakdown: source, build output and git
    /// history for every project found, app data folders, Docker compose
    /// projects, and a previewed cleanup of build output in idle projects.
    Pp {
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(long, default_value_t = 25)]
        top: usize,
        /// Projects idle this many days are offered for build-output cleanup
        #[arg(long, default_value_t = 30)]
        stale_days: u64,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Guided cleanup: every suggestion rated SAFE, CAUTION or RISKY, with what
    /// it is, how much it frees and how to undo it. Asks before each step on a
    /// terminal; RISKY items are never done by dux, only explained.
    Fix {
        #[command(flatten)]
        fixes: fix::FixArgs,
        /// Projects idle this many days are offered for build-output cleanup
        #[arg(long, default_value_t = 30)]
        stale_days: u64,
        /// Skip the project and big-old-file walk (faster)
        #[arg(long)]
        quick: bool,
    },
    /// Terminal UI on the new design: Disks, Folders, Docker, Logs, Projects
    /// and Fixes (pick with Space, s, a; apply with Enter).
    Ui {
        #[arg(default_value = "/")]
        path: PathBuf,
    },
    /// Local web dashboard with the same data and fix picker. Listens on
    /// 127.0.0.1 and needs a token; read-only unless --allow-actions.
    Web {
        /// Filesystem to report on
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8099")]
        listen: String,
        /// Access token (default: a new random one, printed at start)
        #[arg(long)]
        token: Option<String>,
        /// Allow applying fixes from the dashboard
        #[arg(long)]
        allow_actions: bool,
        /// Seconds between background refreshes
        #[arg(long, default_value_t = 900)]
        refresh: u64,
    },
    /// Live disk I/O: per-disk speed, the processes writing and reading most,
    /// each file being written (speed, growth, writer) or read, and full
    /// details of the top writers so a culprit can be tracked down.
    Io {
        /// Seconds between samples
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Number of samples to print (0 = until Ctrl-C)
        #[arg(long, default_value_t = 1)]
        count: u32,
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Send a test alert to every configured channel
    NotifyTest {
        /// KEY=VALUE settings file with channels
        #[arg(long, default_value = notify::DEFAULT_CONFIG)]
        config: PathBuf,
        /// info, warning or critical (critical also tests wall)
        #[arg(long, default_value = "warning")]
        severity: String,
    },
    /// Reverse-SSH tunnel management (reach this host behind NAT via a home
    /// endpoint). Run it separately with the optional dux-tunnel.service;
    /// this subcommand can also provision the identity key.
    Tunnel {
        #[command(subcommand)]
        action: TunnelCmd,
    },
}

#[derive(Subcommand)]
enum TunnelCmd {
    /// Run the optional remote-access supervisor, separately from the indexer.
    Run,
    /// Create the tunnel identity key and print the line to authorize on each
    /// home endpoint.
    Keygen {
        /// Replace an existing key instead of refusing.
        #[arg(long)]
        force: bool,
    },
}

/// Log to stderr (the journal under systemd) at info unless RUST_LOG says otherwise.
fn init_alert_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() {
    // Restore default SIGPIPE so `dux ... | head` exits quietly instead of
    // panicking with "Broken pipe" (Rust ignores SIGPIPE by default).
    // SAFETY: signal accepts this constant and SIG_DFL is its documented sentinel.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = real_main() {
        eprintln!("dux: {e:#}");
        std::process::exit(1);
    }
}

fn cli_command() -> clap::Command {
    Cli::command().mut_subcommand("du", |du| {
        // Keep du-compatible `-h` for human sizes and preserve `--help` while
        // avoiding Clap's default `-h` help alias on this one subcommand.
        du.disable_help_flag(true).arg(
            clap::Arg::new("help")
                .long("help")
                .action(clap::ArgAction::Help)
                .help("Print help"),
        )
    })
}

fn daemon_uses_reconciliation_first(event_apply: bool, compatibility_alias: bool) -> bool {
    compatibility_alias || !event_apply
}

fn ensure_scan_complete(stats: &scan::ScanStats) -> Result<()> {
    anyhow::ensure!(
        stats.errors == 0,
        "scan is incomplete: {} coverage issues; index remains marked incomplete",
        stats.errors
    );
    Ok(())
}

fn real_main() -> Result<()> {
    let matches = cli_command().get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    // The 0.7 commands never open the per-file index, so they must not fail
    // when no data directory can be resolved (for example under systemd,
    // where HOME is unset and /var/lib/dux is read-only).
    let index_free = matches!(
        cli.cmd,
        None | Some(
            Cmd::Sentinel { .. }
                | Cmd::NotifyTest { .. }
                | Cmd::Walk { .. }
                | Cmd::Doctor { .. }
                | Cmd::Where { .. }
                | Cmd::Cleanup { .. }
                | Cmd::Docker { .. }
                | Cmd::Biggest { .. }
                | Cmd::Logs { .. }
                | Cmd::Pp { .. }
                | Cmd::Fix { .. }
                | Cmd::Ui { .. }
                | Cmd::Io { .. }
                | Cmd::Tui { .. }
                | Cmd::NeverTouch { .. }
                | Cmd::Web { .. }
        )
    );
    let db = match &cli.db {
        Some(p) => p.clone(),
        None if index_free => PathBuf::from("/var/lib/dux/dux.db"),
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
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "warn".into()),
                )
                .init();
            // If the daemon is live for THIS db, it is the only writer. Ask it
            // to perform the configured observation instead of starting a
            // competing writer. This also reconciles drift and downtime gaps.
            let scanned_db = db.canonicalize().unwrap_or_else(|_| db.clone());
            let hb = util::read_heartbeat_for(&db);
            let daemon_live = match &hb {
                Some((secs, pid, hbdb)) => {
                    // Heartbeat can stay "fresh" (≤30s) for a while AFTER a crash;
                    // verify the PID is actually alive (kill(pid,0)) so a post-crash
                    // `dux scan` reconciles directly instead of trying to SIGHUP a
                    // dead process and failing exactly when it's needed most.
                    let alive = util::process_alive(*pid);
                    util::timestamp_is_fresh(*secs, 30)
                        && std::path::Path::new(hbdb) == scanned_db.as_path()
                        && alive
                }
                None => false,
            };
            if daemon_live && !force {
                anyhow::ensure!(exclude.is_empty() && !include_pseudo && jobs.is_none() && !one_file_system,
                    "a running daemon rescans with its own settings; stop it before changing scan options");
                request_daemon_rescan(&db, &path, &hb)?;
                return Ok(());
            }
            // No live daemon (or --force): scan directly. The exclusive per-db
            // lock is the hard guard — if something really is writing this index,
            // lock_db fails with a clear message rather than corrupting it.
            let _lock = util::lock_db(&db)?;
            // A one-off direct scan buffers metadata in RAM; let it yield first
            // under global OOM. The long-lived daemon uses the neutral systemd
            // score so it is not artificially selected at low resident memory.
            guard::oom_protect_scan();
            let opts = scan::ScanOptions {
                one_file_system,
                exclude,
                low_priority,
                include_pseudo,
                progress: !quiet,
                jobs,
                cancel: None,
                priority_paths: Vec::new(),
                #[cfg(test)]
                test_after_entry_stat: None,
            };
            // Atomic rebuild: scan into a sibling file, then rename over the live
            // index — fragmentation-free, never a half-built or empty index.
            let start = std::time::Instant::now();
            let s = scan::rebuild_atomic(&db, &path, &opts)?;
            eprintln!(
                "scanned {} files, {} dirs, {} staged path instances ({} directory alias paths), visible {} bytes/{} entries, unique {} bytes/{} inodes, {} in {:.1}s ({} coverage issues)",
                s.files,
                s.dirs,
                s.staged_path_instances,
                s.staged_directory_alias_paths,
                s.staged_root_visible_bytes,
                s.staged_root_visible_entries,
                s.staged_root_unique_bytes,
                s.staged_root_unique_inodes,
                human(s.bytes),
                start.elapsed().as_secs_f64(),
                s.errors
            );
            // Keep the coverage count visible in the summary above, and make
            // the process status agree with it so scripts cannot mistake an
            // incomplete observation for a successful exact scan.
            ensure_scan_complete(&s)?;
        }
        Some(Cmd::Overview { path, since, limit }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let secs = util::parse_duration(&since)?;
            let dirs = query::top(&store, true, limit, scope, false)?;
            let extra = limit.saturating_add(8);
            let mut files = query::top(&store, false, extra, scope, false)?;
            let mut growth = query::growth(&store, secs, extra, scope)?;
            let mut changes = query::changed(&store, secs, extra, scope)?;
            filter_index_rows(&mut files, &db, limit);
            filter_index_growth(&mut growth, &db, limit);
            filter_index_growth(&mut changes, &db, limit);
            let containers = containers::list(&store)?;
            let waste = containers::waste(&store, &containers)?;
            if json {
                emit::overview(&dirs, &files, &growth, &changes, &waste, secs);
                return Ok(());
            }
            println!(
                "DUX OVERVIEW  index {} · window {}",
                if query::daemon_live(&db) {
                    "LIVE"
                } else {
                    "SNAPSHOT"
                },
                since
            );
            println!("\nLARGEST DIRECTORIES");
            print_rows(&dirs);
            println!("\nLARGEST FILES");
            print_rows(&files);
            println!("\nFASTEST GROWTH");
            print_growth_rows(&growth, "GROWTH");
            println!("\nBIGGEST CHANGES (fills and frees)");
            print_growth_rows(&changes, "CHANGE");
            println!("\nDOCKER RECLAIMABLE  {}", human(waste.total()));
            println!(
                "  stopped {} · orphan volumes {} ({}) · build cache {}",
                human(waste.stopped_bytes),
                human(waste.orphan_volume_bytes),
                waste.orphan_volume_count,
                human(waste.build_cache_bytes)
            );
            println!(
                "\nMore: dux large --files · dux fastest-growth --since 24h · dux activity --since 24h · dux docker"
            );
        }
        Some(Cmd::Top {
            path,
            files,
            dirs,
            inodes,
            limit,
            explain,
            sort,
        }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let want_dirs = dirs || !files; // default: dirs
            let mut rows = query::top(&store, want_dirs, limit.saturating_add(8), scope, inodes)?;
            filter_index_rows(&mut rows, &db, limit);
            sort_top_rows(&mut rows, sort);
            if json {
                if explain {
                    emit::classified_rows(&rows);
                } else {
                    emit::rows(&rows);
                }
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
            } else if explain {
                print_context_rows(&rows);
            } else {
                print_rows(&rows);
                if files {
                    eprintln!(
                        "\nContext: rerun with `--explain`; inspect one path with `dux explain PATH`."
                    );
                }
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
            let mut rows = query::growth(&store, secs, limit.saturating_add(8), scope)?;
            filter_index_growth(&mut rows, &db, limit);
            if json {
                emit::growth(&rows);
                return Ok(());
            }
            println!("{:<14} PATH", "GROWTH");
            for r in rows {
                let sign = if r.delta >= 0 { "+" } else { "-" };
                println!(
                    "{:<14} {}",
                    format!("{sign}{}", util::human_magnitude(r.delta)),
                    display_growth_path(&r.path)
                );
            }
        }
        Some(Cmd::Sentinel {
            config,
            once,
            dry_run,
        }) => {
            init_alert_logging();
            let mut cfg = notify::NotifyConfig::load(&config)?;
            cfg.dry_run = dry_run;
            let settings = sentinel::Settings::from_config(&cfg)?;
            if once {
                print!("{}", sentinel::snapshot_report(&settings)?);
            }
            sentinel::run(&cfg, settings, once)?;
        }
        Some(Cmd::Walk {
            path,
            threads,
            depth,
            top,
        }) => {
            let tree = walker::walk(&path.to_string_lossy(), threads)?;
            let s = &tree.stats;
            let rows = walker::largest_at_depth(&tree.root, depth, top);
            if json {
                let folders: Vec<_> = rows
                    .iter()
                    .map(|(p, b)| serde_json::json!({"path": p, "bytes": b}))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "path": tree.root_path,
                        "total_bytes": tree.root.total_bytes,
                        "apparent_bytes": tree.root.total_apparent,
                        "entries": s.entries,
                        "dirs": s.dirs,
                        "files": s.files,
                        "errors": s.errors,
                        "hardlinks_deduped": s.hardlinks_deduped,
                        "seconds": s.seconds,
                        "entries_per_second": s.entries_per_second(),
                        "folders": folders,
                    })
                );
            } else {
                println!(
                    "{}: {} in {} entries ({} dirs, {} files), {:.2}s, {:.0} entries/s, {} errors",
                    tree.root_path,
                    human(tree.root.total_bytes as i64),
                    s.entries,
                    s.dirs,
                    s.files,
                    s.seconds,
                    s.entries_per_second(),
                    s.errors
                );
                for (p, b) in rows {
                    println!("{:>12}  {p}", human(b as i64));
                }
            }
        }
        Some(Cmd::Doctor {
            path,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let fs = sentinel::statvfs(&p)?;
            let mut writers = attrib::Writers::default();
            writers.sample(util::now_secs());
            let tree = walker::walk(&p, threads)?;
            std::thread::sleep(std::time::Duration::from_secs(1));
            writers.sample(util::now_secs());
            let deleted = deleted::deleted_open().unwrap_or_default();
            let held: i64 = deleted.iter().map(|d| d.size).sum();
            let st = &tree.stats;
            println!(
                "{p}: {} used of {} ({:.1}%), {} free; inodes {}",
                human(fs.used as i64),
                human(fs.total as i64),
                fs.used_pct(),
                human(fs.avail as i64),
                fs.inode_pct()
                    .map(|x| format!("{x:.1}% used"))
                    .unwrap_or_else(|| "n/a".into())
            );
            println!(
                "walked {} entries in {:.2}s ({:.0}/s); files found: {}",
                st.entries,
                st.seconds,
                st.entries_per_second(),
                human(tree.root.total_bytes as i64)
            );
            println!("\nLargest folders");
            for (f, b) in walker::largest_at_depth(&tree.root, 2, 12) {
                println!("{:>12}  {f}", human(b as i64));
            }
            println!("\nKnown culprits");
            let mut any = false;
            for (c, cpath, b) in attrib::catalog_sizes(&tree) {
                if b >= 100 << 20 {
                    any = true;
                    println!("{:>12}  {cpath}  {}: {}", human(b as i64), c.label, c.fix);
                }
            }
            if !any {
                println!("  none over 100 MiB");
            }
            println!("\nMost files (inodes)");
            for (f, n) in walker::most_entries(&tree.root, 5) {
                println!("{n:>12}  {f}");
            }
            let gap = fs.used.saturating_sub(tree.root.total_bytes);
            println!(
                "\nDeleted but still open: {} in {} files",
                human(held),
                deleted.len()
            );
            for d in deleted.iter().take(5) {
                println!(
                    "{:>12}  pid {} {}  {}",
                    human(d.size),
                    d.pid,
                    d.process,
                    d.path
                );
            }
            if gap >= (1u64 << 30) {
                println!(
                    "  note: {} more is used than all files add up to",
                    human(gap as i64)
                );
            }
            let w = writers.summary(5);
            println!(
                "\nWriting now: {}",
                if w.is_empty() {
                    "nothing significant".to_string()
                } else {
                    w
                }
            );
            let opts = cleanup::Options {
                level: 3,
                used_pct: fs.used_pct(),
                ..cleanup::Options::default()
            };
            let mut s: Vec<fix::Suggestion> =
                cleanup::plan(&opts).into_iter().map(fix::assess).collect();
            if let Ok(r) = docker::collect() {
                s.extend(
                    docker::plan(&r, false, false, 100 << 20)
                        .into_iter()
                        .map(fix::assess),
                );
            }
            for d in deleted.iter().filter(|d| d.size >= 100 << 20) {
                s.push(fix::risky(
                    format!("{} held by deleted file {} (pid {} {})", human(d.size), d.path, d.pid, d.process),
                    "Already deleted but still open; the space returns when that process closes it.",
                    format!("restart the service of pid {} at a quiet time", d.pid),
                    d.size as u64,
                ));
            }
            fix::offer("this disk", s, &fixes)?;
        }
        Some(Cmd::Where { mount, dir, top }) => {
            let cur_path = snapshot::file_for(&dir, &mount);
            let cur = snapshot::load_path(&cur_path)?.with_context(|| {
                format!(
                    "no snapshot for {mount} in {} yet; is `dux sentinel` running?",
                    dir.display()
                )
            })?;
            println!(
                "{mount}: {} in folders, as of {}",
                human(cur.root.total_bytes as i64),
                match util::ago(cur.started_at).as_str() {
                    "now" => "just now".to_string(),
                    a => format!("{a} ago"),
                }
            );
            for (f, b) in walker::largest_at_depth(&cur.root, 2, top) {
                println!("{:>12}  {f}", human(b as i64));
            }
            if let Some(prev) = snapshot::load_path(&cur_path.with_extension("prev.snap"))? {
                let changes = walker::diff(&prev.root, &cur.root, 16 << 20);
                println!(
                    "\nChanged since {} ago (previous snapshot)",
                    util::ago(prev.started_at)
                );
                if changes.is_empty() {
                    println!("  nothing over 16 MiB");
                }
                for c in changes.iter().take(top) {
                    let sign = if c.delta() >= 0 { "+" } else { "-" };
                    println!(
                        "{:>12}  {}",
                        format!("{sign}{}", util::human_magnitude(c.delta())),
                        c.path
                    );
                }
            }
        }
        Some(Cmd::Cleanup {
            level,
            apply,
            journal_max,
            big_log,
        }) => {
            let before = sentinel::statvfs("/var/log").or_else(|_| sentinel::statvfs("/"))?;
            let opts = cleanup::Options {
                level,
                journal_max: sentinel::parse_bytes(&journal_max)?,
                big_log: sentinel::parse_bytes(&big_log)?,
                used_pct: before.used_pct(),
                ..cleanup::Options::default()
            };
            let items = cleanup::plan(&opts);
            let total: u64 = items.iter().map(|i| i.frees).sum();
            println!(
                "level {level}: {} actions, about {} to free (disk {:.1}% full){}",
                items.len(),
                human(total as i64),
                before.used_pct(),
                if apply {
                    ""
                } else {
                    "; preview only, add --apply to run"
                }
            );
            for i in &items {
                let how = match &i.action {
                    cleanup::Action::Run { program, args } => {
                        format!("{program} {}", args.join(" "))
                    }
                    cleanup::Action::Remove { path } => format!("remove {}", path.display()),
                    cleanup::Action::TailAndTruncate { path, keep } => {
                        format!(
                            "keep last {} of {}, truncate",
                            human(*keep as i64),
                            path.display()
                        )
                    }
                    cleanup::Action::RemoveTree { path } => {
                        format!("remove folder {}", path.display())
                    }
                    cleanup::Action::RemoveUserPath { path } => {
                        format!("delete {}", path.display())
                    }
                };
                println!(
                    "  L{} {:>10}  {}  [{how}]",
                    i.level,
                    human(i.frees as i64),
                    i.what
                );
            }
            if apply {
                // SAFETY: geteuid has no preconditions.
                anyhow::ensure!(unsafe { libc::geteuid() } == 0, "--apply needs root");
                let (mut ok, mut failed) = (0, 0);
                for i in &items {
                    match cleanup::apply(i) {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            failed += 1;
                            println!("  skipped: {}: {e:#}", i.what);
                        }
                    }
                }
                let after = sentinel::statvfs("/var/log").or_else(|_| sentinel::statvfs("/"))?;
                println!(
                    "done: {ok} applied, {failed} skipped; freed {} (now {:.1}% full). Audit log: {}",
                    human(before.used.saturating_sub(after.used) as i64),
                    after.used_pct(),
                    cleanup::AUDIT_LOG
                );
            }
        }
        Some(Cmd::Docker {
            fixes,
            big_log,
            top,
        }) => {
            let r = docker::collect()?;
            if json {
                let imgs: Vec<_> = r.images.iter().map(|i| serde_json::json!({"name": i.name, "id": i.id, "size": i.size, "unique": i.unique, "shared": i.shared, "containers": i.containers, "created": i.created})).collect();
                let cons: Vec<_> = r.containers.iter().map(|c| serde_json::json!({"name": c.name, "image": c.image, "state": c.state, "status": c.status, "writable": c.writable, "log_bytes": c.log_bytes, "project": c.project})).collect();
                let vols: Vec<_> = r.volumes.iter().map(|v| serde_json::json!({"name": v.name, "size": v.size, "links": v.links, "project": v.project})).collect();
                let disk: Vec<_> = r
                    .on_disk
                    .iter()
                    .map(|(p, b)| serde_json::json!({"path": p, "bytes": b}))
                    .collect();
                let proj: Vec<_> = r.by_project().into_iter().map(|(k, v)| serde_json::json!({"project": k, "images": v[0], "writable": v[1], "logs": v[2], "volumes": v[3]})).collect();
                println!(
                    "{}",
                    serde_json::json!({"root": r.root_dir, "images": imgs, "containers": cons, "volumes": vols, "build_cache": r.build_cache, "build_cache_reclaimable": r.build_cache_reclaimable, "on_disk": disk, "projects": proj})
                );
                return Ok(());
            }
            let h = |b: u64| human(b as i64);
            let unused_img: u64 = r.unused_images().map(|i| i.unique).sum();
            let unused_vol: u64 = r.unused_volumes().map(|v| v.size).sum();
            println!("Docker at {}", r.root_dir);
            println!(
                "  images      {:>10}  {} images, {} unused ({} reclaimable)",
                h(r.images_total()),
                r.images.len(),
                r.unused_images().count(),
                h(unused_img)
            );
            println!(
                "  containers  {:>10}  writable layers of {} containers ({} running)",
                h(r.writable_total()),
                r.containers.len(),
                r.containers.iter().filter(|c| c.state == "running").count()
            );
            println!("  logs        {:>10}  container logs", h(r.logs_total()));
            println!(
                "  volumes     {:>10}  {} volumes, {} unused ({})",
                h(r.volumes_total()),
                r.volumes.len(),
                r.unused_volumes().count(),
                h(unused_vol)
            );
            println!(
                "  build cache {:>10}  ({} reclaimable)",
                h(r.build_cache),
                h(r.build_cache_reclaimable)
            );
            println!("\nOn disk");
            for (p, b) in r.on_disk.iter().take(top) {
                println!("  {:>10}  {p}", h(*b));
            }
            println!("\nImages (by unique size)");
            for i in r.images.iter().take(top) {
                println!(
                    "  {:>10}  {:<48} {}  {}",
                    h(i.unique),
                    i.name,
                    if i.containers == 0 {
                        "UNUSED".to_string()
                    } else {
                        format!("{} container(s)", i.containers)
                    },
                    i.created
                );
            }
            println!("\nContainers (writable layer + log)");
            for c in r.containers.iter().take(top) {
                println!(
                    "  {:>10} + {:>10} log  {:<32} {:<8} {}",
                    h(c.writable),
                    h(c.log_bytes),
                    c.name,
                    c.state,
                    c.project.as_deref().unwrap_or("")
                );
            }
            println!("\nVolumes");
            for v in r.volumes.iter().take(top) {
                println!(
                    "  {:>10}  {:<40} {}",
                    h(v.size),
                    v.name,
                    if v.links == 0 {
                        "UNUSED (remove by hand: docker volume rm NAME)".to_string()
                    } else {
                        format!(
                            "{} container(s) {}",
                            v.links,
                            v.project.as_deref().unwrap_or("")
                        )
                    }
                );
            }
            println!("\nBy compose project            images   writable       logs    volumes");
            for (k, v) in r.by_project() {
                println!(
                    "  {:<26} {:>9} {:>10} {:>10} {:>10}",
                    k,
                    h(v[0]),
                    h(v[1]),
                    h(v[2]),
                    h(v[3])
                );
            }
            let mut s: Vec<fix::Suggestion> =
                docker::plan(&r, true, true, sentinel::parse_bytes(&big_log)?)
                    .into_iter()
                    .map(fix::assess)
                    .collect();
            s.extend(
                r.unused_volumes()
                    .filter(|v| v.size >= 1 << 20)
                    .map(|v| fix::delete_volume(&v.name, v.size)),
            );
            fix::offer("Docker", s, &fixes)?;
        }
        Some(Cmd::Biggest {
            path,
            top,
            min,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let b = insight::biggest(&p, threads, top, sentinel::parse_bytes(&min)?)?;
            let users = insight::users();
            let now = util::now_secs();
            if json {
                let files: Vec<_> = b.files.iter().map(|f| serde_json::json!({"path": f.path, "bytes": f.bytes, "apparent": f.apparent, "mtime": f.mtime, "owner": users.get(&f.uid)})).collect();
                let dirs: Vec<_> = walker::largest_at_depth(&b.tree.root, 2, top)
                    .into_iter()
                    .map(|(p, b)| serde_json::json!({"path": p, "bytes": b}))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({"files": files, "folders": dirs, "entries": b.tree.stats.entries, "seconds": b.tree.stats.seconds})
                );
                return Ok(());
            }
            println!(
                "{p}: {} in {} entries, walked in {:.1}s",
                human(b.tree.root.total_bytes as i64),
                b.tree.stats.entries,
                b.tree.stats.seconds
            );
            println!("\nBiggest files                     owner        age");
            for f in &b.files {
                println!(
                    "  {:>10}  {:<12} {:>5}  {}",
                    human(f.bytes as i64),
                    users
                        .get(&f.uid)
                        .cloned()
                        .unwrap_or_else(|| f.uid.to_string()),
                    insight::age_text(f.mtime, now),
                    f.path
                );
            }
            println!("\nBiggest folders");
            for (f, s) in walker::largest_at_depth(&b.tree.root, 2, top.min(15)) {
                println!("  {:>10}  {f}", human(s as i64));
            }
            if let Ok(Some(prev)) = snapshot::load(std::path::Path::new(snapshot::DEFAULT_DIR), &p)
            {
                let ch = walker::diff(&prev.root, &b.tree.root, 64 << 20);
                if !ch.is_empty() {
                    println!(
                        "\nGrew since the sentinel snapshot ({} ago)",
                        util::ago(prev.started_at)
                    );
                    for c in ch.iter().filter(|c| c.delta() > 0).take(10) {
                        println!("  +{:>9}  {}", human(c.delta()), c.path);
                    }
                }
            }
            let s: Vec<fix::Suggestion> = b
                .files
                .iter()
                .map(|f| fix::delete_file(&f.path, "One of the biggest files on this disk; dux cannot know whether you still need it.", f.bytes))
                .collect();
            fix::offer("the biggest files", s, &fixes)?;
        }
        Some(Cmd::Logs {
            path,
            top,
            min,
            sample,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let r = insight::logs(&p, threads, sentinel::parse_bytes(&min)?, sample)?;
            let now = util::now_secs();
            if json {
                let logs: Vec<_> = r.logs.iter().map(|l| serde_json::json!({"path": l.path, "bytes": l.bytes, "kind": l.kind.label(), "rotated": l.rotated, "protected": l.protected, "bytes_per_min": l.per_min, "writer": l.writer, "container": l.container, "mtime": l.mtime})).collect();
                println!(
                    "{}",
                    serde_json::json!({"journal_bytes": r.journal_bytes, "logs": logs})
                );
                return Ok(());
            }
            let h = |b: u64| human(b as i64);
            use insight::LogKind;
            println!("Logs on {p} (walked in {:.1}s)", r.walk_seconds);
            if let Some(j) = r.journal_bytes {
                println!("  journal     {:>10}  (journalctl --disk-usage)", h(j));
            }
            println!(
                "  system      {:>10}  /var/log",
                h(r.total(Some(LogKind::System)))
            );
            println!(
                "  docker      {:>10}  container logs",
                h(r.total(Some(LogKind::Docker)))
            );
            println!(
                "  apps        {:>10}  application logs elsewhere",
                h(r.total(Some(LogKind::App)))
            );
            println!(
                "  rotated     {:>10}  old rotated copies (safe to remove: dux cleanup --level 2)",
                h(r.rotated_total())
            );
            println!("\n      size   growth/min  age  kind    path");
            for l in r.logs.iter().take(top) {
                let g = l
                    .per_min
                    .map(|g| {
                        if g > 0.0 {
                            format!("+{}", h(g as u64))
                        } else {
                            "-".into()
                        }
                    })
                    .unwrap_or_default();
                let mut note = String::new();
                if l.rotated {
                    note.push_str(" [rotated]");
                }
                if l.protected {
                    note.push_str(" [protected]");
                }
                if let Some(c) = &l.container {
                    note.push_str(&format!(" [{c}]"));
                }
                if let Some(w) = &l.writer {
                    note.push_str(&format!(" [written by {w}]"));
                }
                println!(
                    "  {:>9}  {:>10}  {:>4}  {:<6}  {}{note}",
                    h(l.bytes),
                    g,
                    insight::age_text(l.mtime, now),
                    l.kind.label(),
                    l.path
                );
            }
            let mut s: Vec<fix::Suggestion> = Vec::new();
            if let Some(j) = r.journal_bytes.filter(|j| *j > 1 << 30) {
                s.push(fix::assess(cleanup::Item {
                    level: 1,
                    what: format!(
                        "shrink the systemd journal from {} to 1 GiB",
                        human(j as i64)
                    ),
                    action: cleanup::Action::Run {
                        program: "journalctl".into(),
                        args: vec!["--vacuum-size=1G".into()],
                    },
                    frees: j - (1 << 30),
                }));
            }
            for l in r
                .logs
                .iter()
                .filter(|l| !l.protected && !cleanup::is_binary_log(std::path::Path::new(&l.path)))
            {
                if l.rotated && l.writer.is_none() {
                    let item = cleanup::Item {
                        level: 2,
                        what: format!("remove rotated log {}", l.path),
                        action: cleanup::Action::Remove {
                            path: l.path.clone().into(),
                        },
                        frees: l.bytes,
                    };
                    s.push(fix::suggestion(
                        fix::Safety::Safe,
                        item.what.clone(),
                        "An old rotated copy; the active log is untouched.",
                        "Not needed unless investigating that period.",
                        l.bytes,
                        Some(item),
                        None,
                    ));
                } else if !l.rotated && l.bytes >= 50 << 20 {
                    s.push(fix::assess(cleanup::Item {
                        level: 2,
                        what: format!("truncate {} (keep its last 10 MiB)", l.path),
                        action: cleanup::Action::TailAndTruncate {
                            path: l.path.clone().into(),
                            keep: 10 << 20,
                        },
                        frees: l.bytes.saturating_sub(10 << 20),
                    }));
                }
            }
            fix::offer("logs", s, &fixes)?;
        }
        Some(Cmd::Pp {
            path,
            top,
            stale_days,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let (tree, _) = walker::walk_with(&p, threads, walker::WalkOptions::default())?;
            let projects = insight::projects(&tree);
            let apps = insight::apps(&tree);
            let now = util::now_secs();
            let h = |b: u64| human(b as i64);
            if json {
                let ps: Vec<_> = projects.iter().map(|x| serde_json::json!({"name": x.name, "type": x.type_label(), "description": x.description, "branch": x.branch, "remote": x.remote, "path": x.path, "kinds": x.kinds, "total": x.total, "source": x.source(), "build_output": x.artifacts, "git": x.git, "last_activity": x.last_activity, "build_output_dirs": x.artifact_dirs})).collect();
                let ap: Vec<_> = apps.iter().map(|(n, p, b, t)| serde_json::json!({"app": n, "path": p, "bytes": b, "newest_change": t})).collect();
                println!("{}", serde_json::json!({"projects": ps, "apps": ap}));
                return Ok(());
            }
            println!(
                "{p}: {} projects found, walked {} entries in {:.1}s",
                projects.len(),
                tree.stats.entries,
                tree.stats.seconds
            );
            println!(
                "\n  {:<22} {:<26} {:>10} {:>10} {:>10} {:>6}  {:<14} path",
                "project", "type", "total", "build-out", "git", "idle", "branch"
            );
            for x in projects.iter().take(top) {
                println!(
                    "  {:<22} {:<26} {:>10} {:>10} {:>10} {:>5}d  {:<14} {}",
                    insight::clean_text(&x.name, 22),
                    insight::clean_text(&x.type_label(), 26),
                    h(x.total),
                    h(x.artifacts),
                    h(x.git),
                    ((now - x.last_activity).max(0) / 86_400),
                    x.branch.as_deref().unwrap_or("-"),
                    x.path
                );
            }
            let reclaim: u64 = projects.iter().map(|x| x.artifacts).sum();
            println!(
                "  build output in all projects: {} (rebuildable)",
                h(reclaim)
            );
            println!("\nApps and data                  size  changed  path");
            for (n, ap, b, t) in apps.iter().take(top) {
                println!(
                    "  {:<24} {:>10} {:>7}  {ap}",
                    n,
                    h(*b),
                    insight::age_text(*t, now)
                );
            }
            if let Ok(r) = docker::collect() {
                println!("\nDocker compose projects       images   writable       logs    volumes");
                for (k, v) in r.by_project() {
                    println!(
                        "  {:<26} {:>9} {:>10} {:>10} {:>10}",
                        k,
                        h(v[0]),
                        h(v[1]),
                        h(v[2]),
                        h(v[3])
                    );
                }
            }
            let mut s: Vec<fix::Suggestion> = insight::pp_plan(&projects, 0, now)
                .into_iter()
                .map(fix::assess)
                .collect();
            for x in &mut s {
                if !insight::idle_at_least(&x.title, stale_days) {
                    x.title.push_str(" [active project: rebuild needed]");
                }
            }
            fix::offer("projects", s, &fixes)?;
        }
        Some(Cmd::Fix {
            fixes,
            stale_days,
            quick,
        }) => {
            let before = sentinel::statvfs("/")?;
            let mut s: Vec<fix::Suggestion> = Vec::new();
            let opts = cleanup::Options {
                level: 3,
                used_pct: before.used_pct(),
                ..cleanup::Options::default()
            };
            s.extend(cleanup::plan(&opts).into_iter().map(fix::assess));
            if let Ok(r) = docker::collect() {
                s.extend(
                    docker::plan(&r, true, true, 100 << 20)
                        .into_iter()
                        .map(fix::assess),
                );
                for v in r.unused_volumes().filter(|v| v.size >= 1 << 20) {
                    s.push(fix::delete_volume(&v.name, v.size));
                }
            }
            let mut held: std::collections::BTreeMap<(i32, String), i64> = Default::default();
            for d in deleted::deleted_open().unwrap_or_default() {
                *held.entry((d.pid, d.process.clone())).or_default() += d.size;
            }
            for ((pid, proc_name), size) in held {
                if size >= 100 << 20 {
                    s.push(fix::risky(
                        format!("deleted files still held open by pid {pid} ({proc_name})"),
                        "The files are already deleted; the space comes back only when this process closes them, so restarting it interrupts that service.",
                        format!("systemctl status {pid}  # find its service, then restart it at a quiet time"),
                        size as u64,
                    ));
                }
            }
            if !quick {
                eprintln!("dux fix: walking / for idle projects and big old files...");
                let now = util::now_secs();
                let opts = walker::WalkOptions {
                    filter: Some(Box::new(move |f: &walker::FileView| {
                        let d = String::from_utf8_lossy(f.dir);
                        f.bytes >= 1 << 30
                            && now - f.mtime >= 180 * 86_400
                            && (d.starts_with("/home")
                                || d.starts_with("/root")
                                || d.starts_with("/srv")
                                || d.starts_with("/opt")
                                || d.contains("backup"))
                    })),
                    max_found: 200,
                    progress: None,
                };
                let (tree, old_big) = walker::walk_with("/", 4, opts)?;
                let projects = insight::projects(&tree);
                s.extend(
                    insight::pp_plan(&projects, stale_days, now)
                        .into_iter()
                        .map(fix::assess),
                );
                for f in old_big {
                    s.push(fix::delete_file(&f.path, &format!("Untouched for {} days; it may be an old backup or download, or something important.", (now - f.mtime) / 86_400), f.bytes));
                }
            }
            let freed = fix::offer("everything", s, &fixes)?;
            if freed > 0 {
                let after = sentinel::statvfs("/")?;
                println!(
                    "/ was {:.1}% full, now {:.1}% ({} actually freed)",
                    before.used_pct(),
                    after.used_pct(),
                    human(before.used.saturating_sub(after.used) as i64)
                );
            }
        }
        Some(Cmd::Ui { path }) => {
            ui::run(&path.to_string_lossy())?;
        }
        Some(Cmd::Web {
            path,
            listen,
            token,
            allow_actions,
            refresh,
        }) => {
            let token = match token {
                Some(t) => t,
                None => web::random_token()?,
            };
            let loopback = listen.starts_with("127.")
                || listen.starts_with("localhost")
                || listen.starts_with("[::1]");
            if !loopback {
                eprintln!("dux web: WARNING listening on {listen}, reachable from other machines; keep the token secret");
            }
            println!("dux web: open http://{listen}/?token={token}");
            println!(
                "dux web: {}",
                if allow_actions {
                    "fixes CAN be applied from the page"
                } else {
                    "read-only (add --allow-actions to apply fixes)"
                }
            );
            let srv = web::Server::new(
                path.to_string_lossy().into_owned(),
                token,
                allow_actions,
                refresh,
            );
            web::serve(srv, &listen)?;
        }
        Some(Cmd::Io {
            interval,
            count,
            top,
        }) => {
            let mut s = live::Sampler::default();
            let _ = s.sample();
            let mut n = 0u32;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
                let l = s.sample();
                n += 1;
                let r = live::rate_text;
                if json {
                    let w: Vec<_> = l.writing.iter().take(top).map(|x| serde_json::json!({"path": x.path, "pid": x.pid, "process": x.process, "unit": x.unit, "write_bps": x.write_bps, "growth_bps": x.growth_bps, "size": x.size})).collect();
                    let rd: Vec<_> = l.reading.iter().take(top).map(|x| serde_json::json!({"path": x.path, "pid": x.pid, "process": x.process, "read_bps": x.read_bps})).collect();
                    let p: Vec<_> = l.procs.iter().take(top).map(|x| serde_json::json!({"pid": x.pid, "name": x.name, "unit": x.unit, "read_bps": x.read_bps, "write_bps": x.write_bps})).collect();
                    let d: Vec<_> = l.disks.iter().map(|x| serde_json::json!({"disk": x.name, "read_bps": x.read_bps, "write_bps": x.write_bps})).collect();
                    println!(
                        "{}",
                        serde_json::json!({"disks": d, "processes": p, "writing": w, "reading": rd})
                    );
                } else {
                    let (tr, tw) = l
                        .disks
                        .iter()
                        .fold((0.0, 0.0), |a, x| (a.0 + x.read_bps, a.1 + x.write_bps));
                    println!(
                        "Disk I/O over {:.1}s: read {}  write {}",
                        l.seconds,
                        r(tr),
                        r(tw)
                    );
                    for x in &l.disks {
                        println!(
                            "  {:<10} read {:>12}  write {:>12}",
                            x.name,
                            r(x.read_bps),
                            r(x.write_bps)
                        );
                    }
                    println!("\nWho is writing and reading       writes        reads  service / container");
                    for p in l.procs.iter().take(top) {
                        println!(
                            "  {:>7} {:<22} {:>12} {:>12}  {}",
                            p.pid,
                            p.name,
                            r(p.write_bps),
                            r(p.read_bps),
                            p.unit
                        );
                    }
                    println!("\nWriting now      write       growth        size  pid process        file");
                    for x in l.writing.iter().take(top) {
                        let g = if x.growth_bps > 0.0 {
                            format!("+{}", r(x.growth_bps))
                        } else {
                            "-".into()
                        };
                        let who = if x.unit.starts_with("docker:") {
                            format!("  [{}]", x.unit)
                        } else {
                            String::new()
                        };
                        println!(
                            "  {:>13} {:>12} {:>11}  {:>6} {:<14} {}{who}",
                            r(x.write_bps),
                            g,
                            human(x.size as i64),
                            x.pid,
                            x.process,
                            x.path
                        );
                    }
                    println!("\nReading now       read  pid process        file");
                    for x in l.reading.iter().take(top) {
                        println!(
                            "  {:>13}  {:>6} {:<14} {}",
                            r(x.read_bps),
                            x.pid,
                            x.process,
                            x.path
                        );
                    }
                    let mut seen = std::collections::BTreeSet::new();
                    let writers: Vec<i32> = l
                        .writing
                        .iter()
                        .map(|x| x.pid)
                        .filter(|p| seen.insert(*p))
                        .take(3)
                        .collect();
                    for pid in writers {
                        if let Some(d) = live::details(pid) {
                            println!(
                                "\nWriter pid {} {}  (user {}, running {}, {} threads, {} memory)",
                                d.pid,
                                d.name,
                                d.user,
                                insight::age_text(
                                    util::now_secs() - d.running_secs as i64,
                                    util::now_secs()
                                ),
                                d.threads,
                                human(d.rss_bytes as i64)
                            );
                            println!("  command  {}", d.cmdline);
                            println!("  exe      {}   cwd {}", d.exe, d.cwd);
                            println!(
                                "  parent   pid {} {}   service/container {}",
                                d.ppid,
                                d.parent,
                                if d.unit.is_empty() { "-" } else { &d.unit }
                            );
                            println!("  next     {}", live::next_steps(&d));
                        }
                    }
                    println!();
                }
                if count != 0 && n >= count {
                    break;
                }
            }
        }
        Some(Cmd::NotifyTest { config, severity }) => {
            init_alert_logging();
            let cfg = notify::NotifyConfig::load(&config)?;
            let severity = notify::Severity::parse(&severity)
                .context("--severity must be info, warning or critical")?;
            let alert = notify::Alert {
                severity,
                title: "test alert from dux".into(),
                body: format!(
                    "If you can read this, this channel works. Configured: {}",
                    cfg.channels().join(", ")
                ),
                key: "test".into(),
                host: sentinel::hostname(),
            };
            let report = notify::send(&cfg, &alert, true, util::now_secs());
            println!("sent: {}", report.sent.join(", "));
            for (name, err) in &report.failed {
                println!("FAILED {name}: {err}");
            }
            if !report.failed.is_empty() {
                anyhow::bail!("{} channel(s) failed", report.failed.len());
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
        Some(Cmd::Explain { path }) => {
            let store = Store::open_ro(&db)?;
            let row = query::inspect(&store, &path)?;
            let insight = classify::classify(&row);
            if json {
                emit::explain(&row, &insight);
            } else {
                print_explanation(&row, &insight);
            }
        }
        Some(Cmd::Diff { path, since, limit }) => {
            let store = Store::open_ro(&db)?;
            let scope = match path {
                Some(p) => query::resolve_scope(&store, &p)?,
                None => None,
            };
            let secs = util::parse_duration(&since)?;
            let mut rows = query::changed(&store, secs, limit.saturating_add(8), scope)?;
            filter_index_growth(&mut rows, &db, limit);
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
                    format!("{sign}{}", util::human_magnitude(r.delta)),
                    display_growth_path(&r.path)
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
                    format!("{}", util::ceil_div_nonnegative(b, 1 << 20))
                } else {
                    format!("{}", util::ceil_div_nonnegative(b, 1024))
                }
            };
            // -s (summarize): the total for the path ONLY — a single index lookup,
            // not a whole-subtree enumeration (which times out on a big tree like
            // /home). Short-circuit before the full du walk below.
            if summarize {
                let (rd, ri) = match scope {
                    Some(x) => x,
                    None => query::index_root(&store)?
                        .ok_or_else(|| anyhow::anyhow!("no index root — run `dux scan` first"))?,
                };
                let bytes = query::subtree_bytes(&store, rd, ri)?;
                let p = store.path_of_display(rd, ri).unwrap_or_else(|_| "/".into());
                if json {
                    emit::du(&[query::DuRow {
                        path: p,
                        bytes,
                        is_dir: true,
                        mtime: 0,
                    }]);
                } else {
                    println!("{}\t{}", fmt(bytes), util::display_path(&p));
                }
                return Ok(());
            }
            // `max_depth` is pushed into the subtree walk itself, so a shallow
            // `--depth` never materialises the deep rows it would only discard.
            let mut rows = query::du(&store, scope, all, max_depth)?;
            // du order: a directory prints AFTER its descendants (post-order); a
            // reverse-lexicographic sort puts deeper paths first and the root last.
            rows.sort_by(|a, b| b.path.cmp(&a.path));
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
            // waste is host-level, so compute it over the FULL list before the
            // display truncation.
            let rows = containers::list(&store)?;
            let waste = containers::waste(&store, &rows)?;
            if json {
                emit::containers(&rows, &waste);
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
                "{:<7} {:<8} {:<18} {:<22} {:>9} {:>8} {:>8} {:>9} {:>9}",
                "RUNTIME",
                "STATE",
                "NAME",
                "IMAGE",
                "WRITABLE",
                "LOGS",
                "VOLUMES",
                "TOTAL",
                "RECLAIM"
            );
            for c in rows.iter().take(limit) {
                let reclaim = c.reclaimable();
                println!(
                    "{:<7} {:<8} {:<18} {:<22} {:>9} {:>8} {:>8} {:>9} {:>9}",
                    c.runtime,
                    c.state(),
                    trunc(&util::display_path(&c.name), 18),
                    trunc(&util::display_path(&c.image), 22),
                    human(c.writable_bytes),
                    human(c.log_bytes),
                    human(c.volume_bytes),
                    human(c.total()),
                    if reclaim > 0 {
                        human(reclaim)
                    } else {
                        "-".into()
                    },
                );
            }
            // totals across ALL containers (not just the shown top-N). Note: a
            // volume shared by N containers is counted once per container here —
            // the accurate reclaimable-volume figure is the orphan line below.
            let (sw, sl, sv): (i64, i64, i64) = rows.iter().fold((0, 0, 0), |a, c| {
                (
                    a.0.saturating_add(c.writable_bytes),
                    a.1.saturating_add(c.log_bytes),
                    a.2.saturating_add(c.volume_bytes),
                )
            });
            println!(
                "{:<7} {:<8} {:<18} {:<22} {:>9} {:>8} {:>8} {:>9} {:>9}",
                "",
                "",
                format!("TOTAL ({})", rows.len()),
                "",
                human(sw),
                human(sl),
                human(sv),
                human(sw.saturating_add(sl).saturating_add(sv)),
                "",
            );
            // reclaimable / waste — what `docker system prune` would free.
            println!("\nRECLAIMABLE (≈ docker system prune --volumes):");
            println!(
                "  stopped container layers+logs   {}",
                human(waste.stopped_bytes)
            );
            println!(
                "  orphaned volumes ({:>3})           {}",
                waste.orphan_volume_count,
                human(waste.orphan_volume_bytes)
            );
            println!(
                "  build cache                     {}",
                human(waste.build_cache_bytes)
            );
            println!("  TOTAL RECLAIMABLE               {}", human(waste.total()));
        }
        Some(Cmd::Tui { path }) => {
            ui::run(&path.to_string_lossy())?;
        }
        Some(Cmd::NeverTouch { op, path }) => {
            // The list stores text: refuse a name that is not valid UTF-8
            // rather than protect a different, lossily converted path.
            let text = |p: &std::path::Path| -> Result<String> {
                let abs = std::path::absolute(p)?;
                abs.to_str().map(str::to_string).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is not valid UTF-8; the never-touch list cannot store it",
                        abs.display()
                    )
                })
            };
            match (op.as_str(), path) {
                ("add", Some(p)) => {
                    let p = text(&p)?;
                    if protect::add(&p)? {
                        println!("dux will never touch {}", protect::normalize(&p));
                    } else {
                        println!(
                            "{} was already on the never-touch list",
                            protect::normalize(&p)
                        );
                    }
                }
                ("remove", Some(p)) => {
                    let p = text(&p)?;
                    if protect::remove(&p)? {
                        println!(
                            "removed {} from the never-touch list",
                            protect::normalize(&p)
                        );
                    } else {
                        anyhow::bail!(
                            "{} is not on the never-touch list; nothing changed",
                            protect::normalize(&p)
                        );
                    }
                }
                ("list", _) => {
                    let l = protect::policy_from(std::path::Path::new(protect::FILE))?;
                    if l.is_empty() {
                        println!("never-touch list ({}) is empty", protect::FILE);
                    }
                    for p in l {
                        println!("{p}");
                    }
                }
                _ => {
                    anyhow::bail!("usage: dux never-touch add|remove PATH, or dux never-touch list")
                }
            }
        }
        Some(Cmd::Status) => {
            // On a fresh box the FIRST scan hasn't produced dux.db yet, so open_ro
            // would hard-fail with a cryptic error. Instead: if a scan is running,
            // show its live progress; if not, print a friendly hint. Only a db that
            // exists but is unreadable is a real error worth propagating.
            match Store::open_ro(&db) {
                Ok(store) => {
                    if json {
                        println!("{}", query::status_json(&store, &db)?);
                    } else {
                        println!("{}", query::status(&store, &db)?);
                    }
                }
                Err(e) => {
                    if let Some(progress) = util::read_scan_progress_for(&db) {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "status_version": 1,
                                    "root": null,
                                    "daemon_live": util::daemon_live_for(&db),
                                    "watch_status_available": false,
                                    "dirty": false,
                                    "paused": false,
                                    "scan_in_progress": true,
                                    "scan_progress": {
                                        "started_at_seconds": progress.started,
                                        "files_seen": progress.files,
                                        "directories_seen": progress.dirs,
                                        "allocated_bytes_seen": progress.bytes,
                                        "indexing": progress.indexing,
                                    },
                                    "caught_up": false,
                                })
                            );
                        } else {
                            let line = query::scan_progress_line(&db).unwrap_or_default();
                            println!("no index yet — initial scan in progress:\n{line}");
                        }
                    } else if !Store::has_index_artifacts(&db)? {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "status_version": 1,
                                    "root": null,
                                    "daemon_live": util::daemon_live_for(&db),
                                    "watch_status_available": false,
                                    "dirty": false,
                                    "paused": false,
                                    "scan_in_progress": false,
                                    "scan_progress": null,
                                    "caught_up": false,
                                    "error": "no index yet",
                                })
                            );
                        } else {
                            println!(
                                "no index yet at {} — the daemon builds one on first start, \
                                 or run `dux scan /`.",
                                db.display()
                            );
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Some(Cmd::Daemon {
            root,
            flush_ms,
            one_file_system,
            alert_threshold,
            alert_window,
            alert_exec,
            alert_exec_keep_capabilities,
            alert_debounce,
            growth_days,
            max_cpu,
            event_apply,
            reconcile_first,
        }) => {
            if alert_exec_keep_capabilities && alert_exec.is_none() {
                anyhow::bail!("--alert-exec-keep-capabilities requires --alert-exec");
            }
            if alert_exec_keep_capabilities && alert_threshold.is_none() {
                anyhow::bail!("--alert-exec-keep-capabilities requires --alert-threshold");
            }
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
                    keep_capabilities: alert_exec_keep_capabilities,
                    debounce: alert_debounce,
                }),
                None => None,
            };
            watch::run_daemon(
                &db,
                &root,
                flush_ms,
                one_file_system,
                alert,
                growth_days,
                max_cpu as f64 / 100.0,
                daemon_uses_reconciliation_first(event_apply, reconcile_first),
            )?;
        }
        Some(Cmd::Tunnel { action }) => match action {
            TunnelCmd::Run => {
                tracing_subscriber::fmt().with_env_filter("info").init();
                tunnel::run()?;
            }
            TunnelCmd::Keygen { force } => tunnel::keygen(force)?,
        },
        None => {
            // Bare `dux` or `dux <path>` opens the 0.7+ terminal UI.
            let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("/"));
            ui::run(&root.to_string_lossy())?;
        }
    }
    Ok(())
}

/// Read the daemon's atomic-rescan marker. Database and metadata errors must
/// remain errors: treating them as an empty marker can make an unchanged old
/// generation look like a completed rescan.
fn read_scan_generation(store: &Store) -> Result<Option<String>> {
    let generation = store.get_meta("scan_generation")?;
    if let Some(value) = generation.as_deref() {
        anyhow::ensure!(!value.is_empty(), "scan_generation metadata is empty");
    }
    Ok(generation)
}

fn scan_generation_advanced(previous: Option<&str>, current: Option<&str>) -> bool {
    current.is_some_and(|value| !value.is_empty() && Some(value) != previous)
}

fn daemon_rescan_is_complete(
    generation_settled: bool,
    dirty: bool,
    paused: bool,
    scan_active: bool,
    watch: Option<&util::WatchStatus>,
) -> bool {
    generation_settled
        && !dirty
        && !paused
        && !scan_active
        && watch
            .is_some_and(|status| status.kernel_empty && status.pending == 0 && !status.uncertain)
}

fn daemon_scan_first_observation_finished(
    attempt_advanced: bool,
    generation_advanced: bool,
    scan_active: bool,
) -> bool {
    !scan_active && (attempt_advanced || generation_advanced)
}

const DAEMON_SCAN_FIRST_SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn daemon_scan_first_should_defer_uncertain_result(
    accounting_pending: bool,
    settle_elapsed: std::time::Duration,
) -> bool {
    accounting_pending || settle_elapsed < DAEMON_SCAN_FIRST_SETTLE_TIMEOUT
}

const DAEMON_RESCAN_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn daemon_rescan_start_timed_out(
    scan_started: bool,
    generation_advanced: bool,
    elapsed: std::time::Duration,
) -> bool {
    !scan_started && !generation_advanced && elapsed >= DAEMON_RESCAN_START_TIMEOUT
}

fn canonical_scan_root(requested: &std::path::Path) -> Result<PathBuf> {
    let root = requested
        .canonicalize()
        .with_context(|| format!("resolving requested scan path {}", requested.display()))?;
    let metadata = std::fs::metadata(&root)
        .with_context(|| format!("inspecting requested scan path {}", root.display()))?;
    anyhow::ensure!(metadata.is_dir(), "{} is not a directory", root.display());
    Ok(root)
}

/// Ask the running daemon to rescan (SIGHUP), then wait for its configured
/// reconciliation to finish. This avoids starting a competing writer.
fn request_daemon_rescan(
    db: &std::path::Path,
    requested: &std::path::Path,
    hb: &Option<(i64, i32, String)>,
) -> Result<()> {
    let want = canonical_scan_root(requested)?;
    // The daemon rescans the tree IT indexes; warn if that's not what was asked.
    let store =
        Store::open_ro(db).context("checking the index before requesting a daemon rescan")?;
    if let Some(root) = crate::store::indexed_root_path(&store.conn)? {
        if root != want {
            eprintln!(
                "dux: note — the daemon indexes {}; it will rescan THAT tree.\n\
                 \x20     To index a different tree, stop the daemon first \
                 (sudo systemctl stop dux), then scan.",
                root.display()
            );
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

    let mut poll_store = Store::open_ro(db)?;
    let scan_first = util::read_watch_status(db).is_some_and(|status| status.scan_first);
    let previous_generation = read_scan_generation(&poll_store)?;
    let previous_attempt_id = poll_store.get_meta("last_reconcile_attempt_id")?;
    // SIGHUP = "rescan now"; the daemon picks it up at the top of its loop.
    // SAFETY: pid was validated positive above and kill takes scalar arguments.
    let rc = unsafe { libc::kill(pid, libc::SIGHUP) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        anyhow::bail!(
            "could not signal the daemon (pid {pid}): {e}\n\
             Stop it, scan, and start it manually if this persists."
        );
    }
    eprintln!("dux: daemon is live — requesting a rescan (no downtime)…");

    // A new generation marks baseline installation, not successful completion:
    // wait for queued event replay and independent accounting verification too.
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(1800);
    let mut last_reopen = std::time::Instant::now();
    let mut generation_advanced = false;
    let mut scan_started = util::read_scan_progress_for(db).is_some();
    let mut generation_observed_at = None;
    let mut scan_first_observation_finished_at = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        anyhow::ensure!(
            util::process_alive(pid),
            "daemon exited before reconciliation completed"
        );
        let generation = read_scan_generation(&poll_store)?;
        generation_advanced |=
            scan_generation_advanced(previous_generation.as_deref(), generation.as_deref());
        let attempt_id = poll_store.get_meta("last_reconcile_attempt_id")?;
        let attempt_advanced =
            scan_generation_advanced(previous_attempt_id.as_deref(), attempt_id.as_deref());
        let scan_active = util::read_scan_progress_for(db).is_some();
        if scan_active {
            // A manual follow-up can start after an earlier observation. Give
            // that scan its own post-publication settle interval.
            scan_first_observation_finished_at = None;
        }
        scan_started |= scan_active || generation_advanced || attempt_advanced;
        if daemon_rescan_start_timed_out(scan_started, generation_advanced, start.elapsed()) {
            anyhow::bail!(
                "daemon did not report starting the requested rescan within {}s; the request may still be queued. Check `dux status` and daemon logs before requesting another scan",
                DAEMON_RESCAN_START_TIMEOUT.as_secs()
            );
        }
        if generation_advanced {
            let observed_at = *generation_observed_at.get_or_insert_with(std::time::Instant::now);
            let dirty = poll_store.get_meta("dirty_since")?.is_some();
            let paused = poll_store.get_meta("paused_since")?.is_some();
            let watch = util::read_watch_status(db);
            if daemon_rescan_is_complete(
                observed_at.elapsed() >= std::time::Duration::from_secs(1),
                dirty,
                paused,
                scan_active,
                watch.as_ref(),
            ) {
                if scan_first {
                    eprintln!(
                        "dux: filesystem observation completed and accounting verified in {:.0}s; scan-first status reports last-observed freshness, not real-time caught-up state.",
                        start.elapsed().as_secs_f64()
                    );
                } else {
                    eprintln!(
                        "dux: rescan reconciled, queue drained, and accounting verified in {:.0}s.",
                        start.elapsed().as_secs_f64()
                    );
                }
                return Ok(());
            }
        }
        if scan_first
            && daemon_scan_first_observation_finished(
                attempt_advanced,
                generation_advanced,
                scan_active,
            )
        {
            let finished_at =
                *scan_first_observation_finished_at.get_or_insert_with(std::time::Instant::now);
            if attempt_advanced {
                let attempt_result = poll_store.get_meta("last_reconcile_attempt_result")?;
                if attempt_result.as_deref() == Some("incomplete") {
                    anyhow::bail!(
                        "daemon observation completed with incomplete coverage; the previous indexed rows were preserved. Check `dux status` and daemon logs."
                    );
                }
                if matches!(attempt_result.as_deref(), Some("failed" | "cancelled")) {
                    anyhow::bail!(
                        "daemon observation did not complete successfully; check `dux status` and daemon logs"
                    );
                }
            }
            let dirty_reason = poll_store.get_meta("dirty_reason")?;
            let accounting_pending =
                dirty_reason.as_deref() == Some("reconciliation accounting verification pending");
            if !daemon_scan_first_should_defer_uncertain_result(
                accounting_pending,
                finished_at.elapsed(),
            ) {
                eprintln!(
                    "dux: observation completed in {:.0}s, but the index remains stale or uncertain due to concurrent changes; see `dux status`.",
                    start.elapsed().as_secs_f64()
                );
                return Ok(());
            }
        }
        // Reconciliation can update or replace the database. Reopen occasionally
        // so this reader observes a newly installed generation without doing an
        // open/schema check twice per second during long rescans.
        if last_reopen.elapsed() >= std::time::Duration::from_secs(5) {
            poll_store = Store::open_ro(db)?;
            last_reopen = std::time::Instant::now();
        }
        if start.elapsed() > timeout {
            anyhow::bail!(
                "rescan did not complete within {}s; check `dux status` and daemon logs",
                timeout.as_secs()
            );
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
    let root_path = crate::store::indexed_root_path(&store.conn)?;
    let ts = query::optional_timestamp(store, "last_scan_ts")?.unwrap_or(0);
    let (nodes, bytes) = query::index_totals(store)?;
    let daemon = util::daemon_live_for(db) as i32;
    let watch = util::read_watch_status(db);
    let scan_first = watch.as_ref().is_some_and(|w| w.scan_first);
    let kernel_overflow_notifications =
        watch.as_ref().and_then(|w| w.kernel_overflow_notifications);
    let dirty =
        store.get_meta("dirty_since")?.is_some() || watch.as_ref().is_some_and(|w| w.uncertain);
    let paused = store.get_meta("paused_since")?.is_some();
    let scanning = util::read_scan_progress_for(db).is_some();
    let caught_up = daemon == 1
        && !dirty
        && !paused
        && !scanning
        && watch
            .as_ref()
            .is_some_and(|w| !w.scan_first && w.kernel_empty && w.pending == 0);

    let mut s = String::new();
    let mut g = |name: &str, help: &str, val: String| {
        s.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {val}\n"
        ));
    };
    g("dux_up", "1 if the dux index is readable.", "1".into());
    g(
        "dux_index_nodes",
        "Inodes tracked in the index.",
        nodes.to_string(),
    );
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
        "dux_last_scan_available",
        "1 if a completed full scan timestamp is present.",
        ((ts > 0) as u8).to_string(),
    );
    g(
        "dux_last_scan_age_seconds",
        "Age of the last full scan; use this to assess scan-first freshness.",
        if ts > 0 {
            util::now_secs().saturating_sub(ts).max(0).to_string()
        } else {
            "0".into()
        },
    );
    g(
        "dux_scan_first_mode",
        "1 if the daemon uses periodic scan-first observations rather than applying event hints to the index.",
        (scan_first as u8).to_string(),
    );
    g(
        "dux_daemon_up",
        "1 if the live watch daemon is running.",
        daemon.to_string(),
    );
    for (name, help, value) in [
        ("dux_index_dirty", "1 if the index is stale or its coverage/accounting is uncertain, including volatile event loss.", dirty),
        ("dux_index_paused", "1 if index writes are paused by the resource guardian.", paused),
        ("dux_scan_in_progress", "1 if a full scan is publishing live progress.", scanning),
        ("dux_index_caught_up", "1 if the live event-apply daemon reports drained queues, no pending changes and no known uncertainty or scan; always 0 in scan-first mode.", caught_up),
        ("dux_watch_status_available", "1 if fresh, process-validated event pipeline status is available.", watch.is_some()),
    ] {
        g(name, help, (value as u8).to_string());
    }
    g(
        "dux_fanotify_overflow_count_available",
        "1 if the current watcher status format includes kernel overflow accounting.",
        (kernel_overflow_notifications.is_some() as u8).to_string(),
    );
    g(
        "dux_behind_seconds_applicable",
        "1 if dux_behind_seconds measures event-apply backlog; it is not applicable in scan-first mode.",
        (watch.as_ref().is_some_and(|w| !w.scan_first) as u8).to_string(),
    );
    if let Some(w) = watch {
        g(
            "dux_last_event_commit_timestamp_seconds",
            "Unix time of the most recent committed live event batch, or 0 if none.",
            w.last_commit_at.to_string(),
        );
        g(
            "dux_pending_updates",
            "Pending path changes including deferred renames.",
            w.pending.to_string(),
        );
        if !w.scan_first {
            g(
                "dux_behind_seconds",
                "Seconds since both the kernel and pending queues were drained in event-apply mode.",
                (w.behind_ms as f64 / 1000.0).to_string(),
            );
        }
    }
    if let Some(fs) = root_path.as_deref().and_then(util::fs_stat) {
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
    if let Some(overflows) = kernel_overflow_notifications {
        s.push_str(&format!(
            "# HELP dux_fanotify_overflow_notifications_total Kernel fanotify queue-overflow notifications observed; each implies unknown event loss.\n\
             # TYPE dux_fanotify_overflow_notifications_total counter\n\
             dux_fanotify_overflow_notifications_total {overflows}\n",
        ));
    }
    Ok(s)
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

fn owner_label(uid: i64) -> String {
    if !(0..=u32::MAX as i64).contains(&uid) {
        return format!("uid:{uid}");
    }
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid as u32))
        .ok()
        .flatten()
        .map(|u| u.name)
        .unwrap_or_else(|| format!("uid:{uid}"))
}

fn sort_top_rows(rows: &mut [query::Row], sort: TopSort) {
    match sort {
        TopSort::Size => rows.sort_by_key(|r| std::cmp::Reverse(r.size)),
        TopSort::Oldest => rows.sort_by_key(|r| r.mtime),
        TopSort::Newest => rows.sort_by_key(|r| std::cmp::Reverse(r.mtime)),
        TopSort::Path => rows.sort_by(|a, b| a.path.cmp(&b.path)),
        TopSort::Owner => rows.sort_by_key(|r| (owner_label(r.uid), std::cmp::Reverse(r.size))),
        TopSort::Safety => {
            rows.sort_by_key(|r| (classify::classify(r).safety, std::cmp::Reverse(r.size)))
        }
    }
}

fn print_context_rows(rows: &[query::Row]) {
    println!(
        "{:<11} {:<6} {:<12} {:<18} {:<14} {:<22} PATH",
        "SIZE", "AGE", "OWNER", "TYPE", "SAFETY", "BELONGS TO"
    );
    for row in rows {
        let info = classify::classify(row);
        println!(
            "{:<11} {:<6} {:<12} {:<18} {:<14} {:<22} {}",
            human(row.size),
            ago(row.mtime),
            trunc(&owner_label(row.uid), 12),
            trunc(info.file_type, 18),
            info.safety.label(),
            trunc(info.belongs_to, 22),
            util::display_path(&row.path),
        );
    }
    println!("\nUse `dux explain PATH` for purpose, importance, reason, and safe action.");
}

fn print_explanation(row: &query::Row, info: &classify::Insight) {
    println!("FILE RELATION & SAFETY");
    println!("  Path        {}", util::display_path(&row.path));
    println!("  Size        {} allocated", human(row.size));
    println!("  Age         {}", ago(row.mtime));
    println!("  Owner       {} (uid {})", owner_label(row.uid), row.uid);
    println!("  Type        {}", info.file_type);
    println!("  Belongs to  {}", info.belongs_to);
    println!("  Purpose     {}", info.purpose);
    println!("  Importance  {}", info.importance);
    println!("  Safety      {}", info.safety.label());
    println!("  Why         {}", info.reason);
    println!("  Action      {}", info.action);
    println!(
        "\nClassification is conservative advice; dux does not delete or execute cleanup commands."
    );
}

fn filter_index_rows(rows: &mut Vec<query::Row>, db: &std::path::Path, limit: usize) {
    rows.retain(|r| !util::is_index_artifact(std::path::Path::new(&r.path), db));
    rows.truncate(limit);
}

fn filter_index_growth(rows: &mut Vec<query::GrowthRow>, db: &std::path::Path, limit: usize) {
    rows.retain(|r| !util::is_index_artifact(std::path::Path::new(&r.path), db));
    rows.truncate(limit);
}

fn display_growth_path(path: &str) -> String {
    path.strip_prefix("inode:")
        .map(|inode| format!("[deleted/unlinked file · inode {inode}]"))
        .unwrap_or_else(|| util::display_path(path))
}

fn print_growth_rows(rows: &[query::GrowthRow], heading: &str) {
    println!("{:<14} PATH", heading);
    if rows.is_empty() {
        println!(
            "{:<14} no changes recorded (the daemon builds this history)",
            "—"
        );
        return;
    }
    for r in rows {
        let sign = if r.delta >= 0 { "+" } else { "-" };
        println!(
            "{:<14} {}",
            format!("{sign}{}", human(r.delta.saturating_abs())),
            display_growth_path(&r.path)
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
    use clap::CommandFactory;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn direct_scan_exit_status_rejects_coverage_issues() {
        assert!(ensure_scan_complete(&scan::ScanStats::default()).is_ok());

        let incomplete = scan::ScanStats {
            errors: 1,
            ..Default::default()
        };
        let error = ensure_scan_complete(&incomplete).unwrap_err();
        assert!(error.to_string().contains("1 coverage issues"));
        assert!(error
            .to_string()
            .contains("index remains marked incomplete"));
    }

    #[test]
    fn max_cpu_help_distinguishes_event_thread_target_from_process_limit() {
        let command = Cli::command();
        let daemon = command.find_subcommand("daemon").unwrap();
        let argument = daemon
            .get_arguments()
            .find(|argument| argument.get_long() == Some("max-cpu"))
            .unwrap();
        let help = argument.get_help().unwrap().to_string();
        assert!(help.contains("event-loop"), "{help}");
        assert!(help.contains("does not cap"), "{help}");
        assert!(help.contains("process-wide ceiling"), "{help}");
    }

    #[test]
    fn du_human_short_flag_and_long_help_do_not_conflict() {
        let matches = cli_command()
            .try_get_matches_from(["dux", "du", "-h"])
            .unwrap();
        let parsed = Cli::from_arg_matches(&matches).unwrap();
        match parsed.cmd.unwrap() {
            Cmd::Du { human, .. } => assert!(human),
            _ => panic!("expected du command"),
        }

        let error = cli_command()
            .try_get_matches_from(["dux", "du", "--help"])
            .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn daemon_scan_request_requires_an_existing_directory() {
        let dir = tmp("scan-root-validation");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert_eq!(
            canonical_scan_root(&dir).unwrap(),
            dir.canonicalize().unwrap()
        );
        assert!(canonical_scan_root(&file)
            .unwrap_err()
            .to_string()
            .contains("not a directory"));
        assert!(canonical_scan_root(&dir.join("missing"))
            .unwrap_err()
            .to_string()
            .contains("resolving requested scan path"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn daemon_rescan_waits_for_queue_drain_and_verified_clean_status() {
        let clean = util::WatchStatus {
            kernel_empty: true,
            pending: 0,
            uncertain: false,
            ..Default::default()
        };
        assert!(daemon_rescan_is_complete(
            true,
            false,
            false,
            false,
            Some(&clean)
        ));
        assert!(!daemon_rescan_is_complete(
            false,
            false,
            false,
            false,
            Some(&clean)
        ));
        assert!(!daemon_rescan_is_complete(
            true,
            true,
            false,
            false,
            Some(&clean)
        ));
        assert!(!daemon_rescan_is_complete(
            true,
            false,
            true,
            false,
            Some(&clean)
        ));
        assert!(!daemon_rescan_is_complete(
            true,
            false,
            false,
            true,
            Some(&clean)
        ));
        assert!(!daemon_rescan_is_complete(true, false, false, false, None));

        let mut pending = clean.clone();
        pending.pending = 1;
        assert!(!daemon_rescan_is_complete(
            true,
            false,
            false,
            false,
            Some(&pending)
        ));
        let mut kernel_not_drained = clean.clone();
        kernel_not_drained.kernel_empty = false;
        assert!(!daemon_rescan_is_complete(
            true,
            false,
            false,
            false,
            Some(&kernel_not_drained)
        ));
        let mut uncertain = clean.clone();
        uncertain.uncertain = true;
        assert!(!daemon_rescan_is_complete(
            true,
            false,
            false,
            false,
            Some(&uncertain)
        ));
    }

    #[test]
    fn scan_first_manual_rescan_completes_on_last_observation_without_claiming_caught_up() {
        assert!(daemon_scan_first_observation_finished(true, false, false));
        assert!(daemon_scan_first_observation_finished(false, true, false));
        assert!(!daemon_scan_first_observation_finished(true, false, true));
        assert!(!daemon_scan_first_observation_finished(false, false, false));
    }

    #[test]
    fn scan_first_cli_waits_for_transient_accounting_status_to_settle() {
        assert!(daemon_scan_first_should_defer_uncertain_result(
            false,
            std::time::Duration::from_secs(1),
        ));
        assert!(daemon_scan_first_should_defer_uncertain_result(
            false,
            DAEMON_SCAN_FIRST_SETTLE_TIMEOUT - std::time::Duration::from_millis(1),
        ));
        assert!(!daemon_scan_first_should_defer_uncertain_result(
            false,
            DAEMON_SCAN_FIRST_SETTLE_TIMEOUT,
        ));
        assert!(daemon_scan_first_should_defer_uncertain_result(
            true,
            DAEMON_SCAN_FIRST_SETTLE_TIMEOUT * 100,
        ));
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-t-{tag}-{}", std::process::id()))
    }

    #[test]
    fn metrics_never_call_volatile_event_loss_caught_up() {
        let db = tmp("metrics-uncertain");
        let store = Store::create_fresh(&db).unwrap();
        util::write_heartbeat(&db);
        util::write_watch_status_with_overflows(
            &db, 0, true, 100, 90, 30, 0, 5, 0, 500000, 2, true,
        );
        let metrics = metrics_text(&store, &db, 0).unwrap();
        assert!(metrics.contains("\ndux_index_dirty 1\n"));
        assert!(metrics.contains("\ndux_index_caught_up 0\n"));
        assert!(metrics.contains("\ndux_behind_seconds_applicable 1\n"));
        assert!(metrics
            .lines()
            .any(|line| line.starts_with("dux_behind_seconds ")));
        assert!(metrics.contains("dux_fanotify_overflow_count_available 1\n"));
        assert!(metrics.contains("dux_fanotify_overflow_notifications_total 2\n"));
        let legacy_status = format!(
            "3 {} {} 0 1 100 90 30 5 0 500000 0 {}\n{}",
            util::now_secs(),
            std::process::id(),
            util::now_secs(),
            db.canonicalize().unwrap_or_else(|_| db.clone()).display()
        );
        std::fs::write(util::runtime_path(&db, "watch.status"), legacy_status).unwrap();
        let metrics = metrics_text(&store, &db, 0).unwrap();
        assert!(metrics.contains("\ndux_index_dirty 0\n"));
        assert!(metrics.contains("\ndux_index_caught_up 1\n"));
        assert!(metrics.contains("dux_fanotify_overflow_count_available 0\n"));
        assert!(!metrics.contains("dux_fanotify_overflow_notifications_total"));
        util::write_watch_status(&db, 0, true, 100, 90, 30, 0, 5, 0, 500000, false);
        let metrics = metrics_text(&store, &db, 0).unwrap();
        assert!(metrics.contains("dux_fanotify_overflow_count_available 1\n"));
        assert!(metrics.contains("dux_fanotify_overflow_notifications_total 0\n"));
        std::fs::remove_file(util::runtime_path(&db, "watch.status")).unwrap();
        let metrics = metrics_text(&store, &db, 0).unwrap();
        assert!(metrics.contains("\ndux_index_caught_up 0\n"));
        std::fs::remove_file(util::runtime_path(&db, "heartbeat")).unwrap();
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn scan_first_metrics_never_report_event_caught_up_or_applicable_behind_time() {
        let db = tmp("metrics-scan-first");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        let store = Store::create_fresh(&db).unwrap();
        util::write_heartbeat(&db);
        let scan_timestamp = util::now_secs().saturating_sub(30);
        store
            .set_meta("last_scan_ts", &scan_timestamp.to_string())
            .unwrap();
        util::write_watch_status_with_mode(
            &db, 0, true, 12, 0, 0, 0, 0, 0, 500_000, 0, false, true,
        );

        let status = query::status_json(&store, &db).unwrap();
        assert_eq!(status["scan_first"], true);
        assert_eq!(status["caught_up"], false);
        assert_eq!(status["behind_seconds_applicable"], false);

        let metrics = metrics_text(&store, &db, 0).unwrap();
        assert!(metrics.contains("\ndux_index_caught_up 0\n"));
        assert!(metrics.contains("\ndux_scan_first_mode 1\n"));
        assert!(metrics.contains("\ndux_last_scan_available 1\n"));
        assert!(metrics.contains("\ndux_behind_seconds_applicable 0\n"));
        assert!(!metrics
            .lines()
            .any(|line| line.starts_with("dux_behind_seconds ")));
        let age = metrics
            .lines()
            .find_map(|line| line.strip_prefix("dux_last_scan_age_seconds "))
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert!((30..=31).contains(&age));

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        let _ = std::fs::remove_file(util::runtime_path(&db, "heartbeat"));
        let _ = std::fs::remove_file(util::runtime_path(&db, "watch.status"));
    }

    #[test]
    fn metrics_reject_malformed_scan_timestamp() {
        let db = tmp("metrics-invalid-scan-ts");
        let store = Store::create_fresh(&db).unwrap();
        store.set_meta("last_scan_ts", "not-a-timestamp").unwrap();
        let error = metrics_text(&store, &db, 0).unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid last_scan_ts timestamp in index metadata"));
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn rescan_generation_requires_a_new_nonempty_marker() {
        assert!(!scan_generation_advanced(None, None));
        assert!(!scan_generation_advanced(Some("old"), Some("old")));
        assert!(!scan_generation_advanced(Some("old"), Some("")));
        assert!(scan_generation_advanced(None, Some("new")));
        assert!(scan_generation_advanced(Some("old"), Some("new")));
    }

    #[test]
    fn daemon_rescan_reports_when_request_never_starts() {
        assert!(!daemon_rescan_start_timed_out(
            false,
            false,
            DAEMON_RESCAN_START_TIMEOUT - std::time::Duration::from_millis(1),
        ));
        assert!(daemon_rescan_start_timed_out(
            false,
            false,
            DAEMON_RESCAN_START_TIMEOUT,
        ));
        assert!(!daemon_rescan_start_timed_out(
            true,
            false,
            DAEMON_RESCAN_START_TIMEOUT * 2,
        ));
        assert!(!daemon_rescan_start_timed_out(
            false,
            true,
            DAEMON_RESCAN_START_TIMEOUT * 2,
        ));
    }

    #[test]
    fn rescan_generation_reads_fail_instead_of_becoming_empty() {
        let db = tmp("scan-generation-errors");
        let store = Store::create_fresh(&db).unwrap();
        assert_eq!(read_scan_generation(&store).unwrap(), None);
        store.set_meta("scan_generation", "").unwrap();
        assert!(read_scan_generation(&store)
            .unwrap_err()
            .to_string()
            .contains("is empty"));
        store.conn.execute_batch("DROP TABLE meta").unwrap();
        assert!(read_scan_generation(&store).is_err());
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
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
    fn fix_commands_parse_with_shared_fix_options() {
        assert!(matches!(
            Cli::try_parse_from(["dux", "docker"]).unwrap().cmd,
            Some(Cmd::Docker { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["dux", "containers"]).unwrap().cmd,
            Some(Cmd::Containers { .. })
        ));
        match Cli::try_parse_from(["dux", "pp", "/srv", "--select", "1,3-4"])
            .unwrap()
            .cmd
        {
            Some(Cmd::Pp { fixes, .. }) => assert_eq!(fixes.select.as_deref(), Some("1,3-4")),
            _ => panic!("pp"),
        }
        match Cli::try_parse_from(["dux", "logs", "--apply-safe"])
            .unwrap()
            .cmd
        {
            Some(Cmd::Logs { fixes, .. }) => assert!(fixes.apply_safe && !fixes.apply_all),
            _ => panic!("logs"),
        }
        assert!(matches!(
            Cli::try_parse_from(["dux", "fix", "--apply-all", "--quick"])
                .unwrap()
                .cmd,
            Some(Cmd::Fix { quick: true, .. })
        ));
        assert!(Cli::try_parse_from(["dux", "biggest", "--no-fix"]).is_ok());
    }

    #[test]
    fn cli_answer_aliases_are_stable() {
        assert!(matches!(
            Cli::try_parse_from(["dux", "large", "--files"])
                .unwrap()
                .cmd,
            Some(Cmd::Top { files: true, .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["dux", "fastest-growth", "--since", "1h"])
                .unwrap()
                .cmd,
            Some(Cmd::Growth { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["dux", "activity"]).unwrap().cmd,
            Some(Cmd::Diff { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["dux", "summary"]).unwrap().cmd,
            Some(Cmd::Overview { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["dux", "large", "--files", "--details", "--sort", "safety"])
                .unwrap()
                .cmd,
            Some(Cmd::Top {
                explain: true,
                sort: TopSort::Safety,
                ..
            })
        ));
    }

    #[test]
    fn daemon_defaults_to_reconciliation_first_with_event_apply_escape_hatch() {
        let default = Cli::try_parse_from(["dux", "daemon"]).unwrap();
        let Some(Cmd::Daemon {
            event_apply,
            reconcile_first,
            ..
        }) = default.cmd
        else {
            panic!("daemon command expected");
        };
        assert!(daemon_uses_reconciliation_first(
            event_apply,
            reconcile_first
        ));

        let legacy = Cli::try_parse_from(["dux", "daemon", "--event-apply"]).unwrap();
        let Some(Cmd::Daemon {
            event_apply,
            reconcile_first,
            ..
        }) = legacy.cmd
        else {
            panic!("daemon command expected");
        };
        assert!(!daemon_uses_reconciliation_first(
            event_apply,
            reconcile_first
        ));

        let compat = Cli::try_parse_from(["dux", "daemon", "--reconcile-first"]).unwrap();
        assert!(matches!(
            compat.cmd,
            Some(Cmd::Daemon {
                reconcile_first: true,
                ..
            })
        ));
        assert!(daemon_uses_reconciliation_first(false, true));
        assert!(
            Cli::try_parse_from(["dux", "daemon", "--reconcile-first", "--event-apply"]).is_err()
        );
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
            // Rust tests use TMPDIR on tmpfs; scan this fixture tree rather than
            // treating its nested directories as pseudo-filesystem mounts.
            include_pseudo: true,
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

        // Exact explanations are served from indexed path records, not a live
        // stat: a file that vanished after this snapshot can still be identified.
        std::fs::remove_file(dir.join("a/real.bin")).unwrap();
        let inspected = query::inspect(&store, &dir.join("a/real.bin")).unwrap();
        assert_eq!(inspected.size, 8192);

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db);
    }
}
