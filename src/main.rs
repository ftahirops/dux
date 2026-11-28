mod api;
mod attrib;
mod auto;
mod catalog;
mod cleanup;
mod config;
mod deleted;
mod docker;
mod fix;
mod gather;
mod insight;
mod live;
mod notify;
mod probes;
mod protect;
#[cfg(test)]
mod qa;
mod sentinel;
mod setup;
mod snapshot;
mod ui;
mod util;
mod walker;
mod web;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;
use util::human;

/// dux — disk-full alerts that say which folder grew, who is writing and
/// the safe fix. Start with `dux sentinel`, `dux doctor /` and `dux where /`.
#[derive(Parser)]
#[command(name = "dux", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DUX_BUILD_REVISION"), ") [", env!("DUX_BUILD_KIND"), "] source-sha256=", env!("DUX_BUILD_SOURCE_SHA256")), about, long_about = None)]
struct Cli {
    /// Path to open in the TUI (when no subcommand is given)
    path: Option<PathBuf>,

    /// Emit machine-readable JSON instead of a table (read commands).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Deleted files still held open by a process: space you cannot see
    ///
    /// Deleted-but-open files still consuming disk
    #[command(name = "leaks", alias = "deleted-open")]
    DeletedOpen,
    /// Paths dux must never clean: add PATH, remove PATH, list
    ///
    /// Paths dux must never delete, truncate or offer as a fix
    /// (/etc/dux/never-touch). `add PATH`, `remove PATH`, or `list`.
    NeverTouch {
        #[arg(value_parser = ["add", "remove", "list"], default_value = "list")]
        op: String,
        path: Option<PathBuf>,
    },
    /// Background watcher that alerts when a disk fills (runs as a service)
    ///
    /// Watch free space and inodes on every mount (every minute; every 5 s
    /// once a disk is past its warning level) and alert through the system
    /// log, ntfy, Slack, a webhook, email and wall: on crossing a level, when
    /// clearly worse, and on recovery. Runs automatic cleanup when approved.
    Sentinel {
        /// Config file (dux.toml; an old notify.conf beside it is read too)
        #[arg(long, default_value = notify::DEFAULT_CONFIG)]
        config: PathBuf,
        /// Check once: print a df-style summary, alert on thresholds, exit
        #[arg(long)]
        once: bool,
        /// Print the alerts that would be sent instead of sending them
        #[arg(long)]
        dry_run: bool,
    },
    /// Disk health report: biggest folders, known culprits, their fixes
    ///
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
    /// Where the space is under a folder, and what grew (from the last snapshot)
    ///
    /// Where the space is, from the latest sentinel folder snapshot, and
    /// what grew since the snapshot before it. Instant; walks nothing.
    Where {
        /// Any folder (its filesystem's latest snapshot is used)
        #[arg(default_value = "/")]
        mount: String,
        /// Snapshot directory
        #[arg(long, default_value = snapshot::DEFAULT_DIR)]
        dir: PathBuf,
        /// How many folders to show
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Where Docker and Podman use disk: stacks, images, containers, volumes
    ///
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
    /// Biggest files and folders
    ///
    /// Biggest files and folders on a filesystem, from one fast walk, plus
    /// what grew since the last sentinel snapshot.
    Biggest {
        #[arg(default_value = "/")]
        path: PathBuf,
        /// How many of each to show
        #[arg(short = 'n', long, default_value_t = 10)]
        top: usize,
        /// Only the biggest files
        #[arg(long, conflicts_with = "folders")]
        files: bool,
        /// Only the biggest folders
        #[arg(long)]
        folders: bool,
        /// Also show each file's owner and full path
        #[arg(short = 'l', long)]
        long: bool,
        /// Only list files at least this big
        #[arg(long, default_value = "50M")]
        min: String,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Logs by size, who writes them, and how fast they grow
    ///
    /// Every log on a filesystem: journal, system, Docker and application logs,
    /// active vs rotated, live growth per minute and the process writing each.
    Logs {
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(short = 'n', long, default_value_t = 25)]
        top: usize,
        /// Only list logs at least this big
        #[arg(long, default_value = "10M")]
        min: String,
        /// Seconds to sample log growth (0 = skip)
        #[arg(long, default_value_t = 3)]
        sample: u64,
        /// Full paths and who writes each log
        #[arg(short = 'l', long)]
        long: bool,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Projects and apps: source, build output and git history
    ///
    /// Per project and per app breakdown: source, build output and git
    /// history for every project found, app data folders, Docker compose
    /// projects, and a previewed cleanup of build output in idle projects.
    #[command(name = "projects", alias = "pp")]
    Pp {
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(short = 'n', long, default_value_t = 25)]
        top: usize,
        /// Projects idle this many days are offered for build-output cleanup
        #[arg(long, default_value_t = 10)]
        stale_days: u64,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[command(flatten)]
        fixes: fix::FixArgs,
    },
    /// Every cleanup suggestion rated SAFE, CAUTION or RISKY, to pick and apply
    ///
    /// Guided cleanup: every suggestion rated SAFE, CAUTION or RISKY, with what
    /// it is, how much it frees and how to undo it. Asks before each step on a
    /// terminal; RISKY items are never done by dux, only explained.
    Fix {
        #[command(flatten)]
        fixes: fix::FixArgs,
        /// Only suggestions up to this level: 1 = journal; 2 = + logs;
        /// 3 = + package caches, temp files, dumps, Docker, build output
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
        level: Option<u8>,
        /// Projects idle this many days are offered for build-output cleanup
        #[arg(long, default_value_t = 10)]
        stale_days: u64,
        /// Skip the project and big-old-file walk (faster)
        #[arg(long)]
        quick: bool,
    },
    /// Terminal UI (the same as plain `dux`)
    ///
    /// Terminal UI on the new design: Disks, Folders, Docker, Logs, Projects
    /// and Fixes (pick with Space, s, a; apply with Enter).
    Ui {
        #[arg(default_value = "/")]
        path: PathBuf,
    },
    /// Read-only web dashboard, JSON API and Prometheus metrics (token protected)
    ///
    /// The five views of the terminal UI, read-only, plus /api/v1/... (the
    /// same JSON as the CLI --json output) and /metrics. Listens on
    /// 127.0.0.1 and needs the token for every request.
    Web {
        /// Filesystem to report on
        #[arg(default_value = "/")]
        path: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8099")]
        listen: String,
        /// Access token (default: a new random one, printed at start)
        #[arg(long)]
        token: Option<String>,
        /// Seconds between background refreshes
        #[arg(long, default_value_t = 300)]
        refresh: u64,
    },
    /// Live disk I/O: who is writing and reading which files
    ///
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
    /// Turn every change on or off: the emergency brake
    ///
    /// `dux actions off` makes dux refuse every change (UI, CLI and automatic
    /// cleanup) until `dux actions on`. Reports stay available.
    Actions {
        #[arg(value_parser = ["on", "off", "status"], default_value = "status")]
        state: String,
    },
    /// Automatic cleanup: what is approved, announced and watched
    ///
    /// `dux auto` shows the approved folders and actions, build output
    /// announced for deletion, and logs watched after cleanup. `dux auto keep
    /// PATH` keeps announced build output from automatic deletion. `dux auto
    /// allow` lets the sandboxed service change the approved folders (done
    /// by dux setup; needed after editing dux.toml by hand).
    Auto {
        #[arg(value_parser = ["status", "keep", "unkeep", "allow"], default_value = "status")]
        op: String,
        path: Option<PathBuf>,
        #[arg(long, default_value = notify::DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Guided setup: alerts (with a test), thresholds, never-touch, cleanup
    ///
    /// Writes /etc/dux/dux.toml (mode 600). Re-run any time: every answer
    /// defaults to the current value. Without a terminal, edit the file.
    Setup {
        #[arg(long, default_value = config::DEFAULT_FILE)]
        config: PathBuf,
        /// Print the documented example config (every setting) and exit
        #[arg(long)]
        print_example: bool,
    },
    /// Send a test alert to every configured channel
    ///
    /// Send a test alert to every configured channel
    NotifyTest {
        /// Config file (dux.toml; an old notify.conf beside it is read too)
        #[arg(long, default_value = notify::DEFAULT_CONFIG)]
        config: PathBuf,
        /// info, warning or critical (critical also tests wall)
        #[arg(long, default_value = "warning")]
        severity: String,
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
    // Exit codes (spec OUT-7): 0 ok, 1 error, 2 usage (clap), 3 refused for safety.
    if let Err(e) = real_main() {
        eprintln!("dux: {e:#}");
        let refused = e
            .chain()
            .any(|c| c.downcast_ref::<util::Refused>().is_some());
        std::process::exit(if refused { 3 } else { 1 });
    }
}

fn cli_command() -> clap::Command {
    Cli::command()
}

fn real_main() -> Result<()> {
    let matches = cli_command().get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    let json = cli.json;
    // Commands that walk the disk show live progress on a terminal.
    if matches!(
        cli.cmd,
        Some(
            Cmd::Doctor { .. }
                | Cmd::Biggest { .. }
                | Cmd::Logs { .. }
                | Cmd::Pp { .. }
                | Cmd::Fix { .. }
                | Cmd::Docker { .. }
                | Cmd::Auto { .. }
        )
    ) {
        walker::show_progress();
    }

    match cli.cmd {
        Some(Cmd::Sentinel {
            config,
            once,
            dry_run,
        }) => {
            init_alert_logging();
            let (mut cfg, settings) = notify::NotifyConfig::load_checked(&config)?;
            cfg.dry_run = dry_run;
            if once && json {
                println!("{}", api::disks_json(&gather::disks()));
                return Ok(());
            }
            if once {
                print!("{}", sentinel::snapshot_report(&settings)?);
            }
            sentinel::run(&cfg, settings, once)?;
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
            let c = util::Colors::detect();
            let st = &tree.stats;
            let pct = fs.used_pct();
            let level = if pct >= 95.0 {
                c.red
            } else if pct >= 85.0 {
                c.yellow
            } else {
                c.green
            };
            let bar = |frac: f64, w: usize| {
                let n = (frac.clamp(0.0, 1.0) * w as f64).round() as usize;
                format!("{}{}", "█".repeat(n), "░".repeat(w - n))
            };
            println!(
                "\n {}{p}{}  {level}{}{}  {level}{}{pct:.1}%{}   {} used of {} · {}{} free{}",
                c.bold,
                c.reset,
                bar(pct / 100.0, 24),
                c.reset,
                c.bold,
                c.reset,
                human(fs.used as i64),
                human(fs.total as i64),
                c.bold,
                human(fs.avail as i64),
                c.reset
            );
            println!(
                "{}  scanned {} entries in {:.0} s{}",
                c.dim, st.entries, st.seconds, c.reset
            );
            let head = |t: &str| println!("\n {}{}{t}{}", c.bold, c.cyan, c.reset);
            // Inodes only when they are a problem.
            if let Some(ip) = fs.inode_pct().filter(|x| *x >= 85.0) {
                head(&format!(
                    "INODES {ip:.0}% USED: new files fail when they run out"
                ));
                for (f, n) in walker::most_entries(&tree.root, 5) {
                    println!("   {n:>11}  {f}");
                }
            }
            head("BIGGEST FOLDERS");
            let top = walker::largest_at_depth(&tree.root, 2, 5);
            let maxb = top.first().map(|(_, b)| *b).unwrap_or(1).max(1);
            for (f, b) in &top {
                println!(
                    "   {:>11}  {}{}{}  {f}",
                    human(*b as i64),
                    c.blue,
                    bar(*b as f64 / maxb as f64, 12),
                    c.reset
                );
            }
            let mut cul: Vec<_> = attrib::catalog_sizes(&tree)
                .into_iter()
                .filter(|(_, _, b)| *b >= 100 << 20)
                .collect();
            cul.sort_by_key(|(_, _, b)| std::cmp::Reverse(*b));
            if !cul.is_empty() {
                head("KNOWN SPACE USERS");
                for (k, cpath, b) in cul.iter().take(10) {
                    println!(
                        "   {:>11}  {}{}{}  {}{cpath}{}",
                        human(*b as i64),
                        c.bold,
                        k.label,
                        c.reset,
                        c.dim,
                        c.reset
                    );
                    // systemd-tmpfiles removes only old files: say how much.
                    let fix = match cpath.as_str() {
                        "/tmp" | "/var/tmp" => {
                            let days = if cpath == "/tmp" { 10 } else { 30 };
                            let aged = catalog::tmp_aged(cpath, days, util::now_secs());
                            format!(
                                "systemd-tmpfiles --clean frees only files untouched {days}+ days: {} here; the rest is recent or in use (dux where {cpath})",
                                human(aged as i64)
                            )
                        }
                        _ => k.fix.to_string(),
                    };
                    println!("   {:>11}  {}→ {fix}{}", "", c.green, c.reset);
                }
                if cul.len() > 10 {
                    println!(
                        "   {}… {} more (dux where {p}){}",
                        c.dim,
                        cul.len() - 10,
                        c.reset
                    );
                }
            }
            // Deleted-but-open files only when there are some.
            if held >= 1 << 20 {
                head(&format!(
                    "DELETED BUT STILL OPEN: {} in {} files",
                    human(held),
                    deleted.len()
                ));
                for d in deleted.iter().take(3) {
                    println!(
                        "   {:>11}  pid {} {}  {}{}{}",
                        human(d.size),
                        d.pid,
                        d.process,
                        c.dim,
                        d.path,
                        c.reset
                    );
                }
                println!(
                    "   {}→ the space returns when those processes close them (dux leaks){}",
                    c.green, c.reset
                );
            }
            let gap = fs.used.saturating_sub(tree.root.total_bytes);
            if gap >= (1u64 << 30) {
                println!("\n {}note:{} {} more is used than all files add up to (deleted-but-open files, or another mount hides files)", c.yellow, c.reset, human(gap as i64));
            }
            let w = writers.summary(5);
            if !w.is_empty() {
                head("WRITING NOW");
                println!("   {w}");
            }
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
            // Any folder: find the filesystem it is on, open that snapshot,
            // then show this folder's own sub-folders and what grew under it.
            let abs = std::path::absolute(&mount)
                .map(|p| protect::normalize(&p.to_string_lossy()))
                .unwrap_or_else(|_| mount.clone());
            let mounts = sentinel::parse_mountinfo(
                &std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default(),
            );
            let fs = mounts
                .iter()
                .map(|m| m.path.as_str())
                .filter(|m| *m == "/" || abs == *m || abs.starts_with(&format!("{m}/")))
                .max_by_key(|m| m.len())
                .unwrap_or("/")
                .to_string();
            let cur_path = snapshot::file_for(&dir, &fs);
            let cur = snapshot::load_path(&cur_path)?.with_context(|| {
                format!("no snapshot for {fs} in {} yet; is `dux sentinel` running? (or use `dux biggest {abs}`)", dir.display())
            })?;
            let rel = abs.strip_prefix(fs.trim_end_matches('/')).unwrap_or(&abs);
            let node = if rel.is_empty() || rel == "/" {
                Some(&cur.root)
            } else {
                cur.root.find(rel)
            };
            let node = node.with_context(|| format!("{abs} is not in the snapshot of {fs} (taken {} ago); it may be newer, or on another filesystem", util::ago(cur.started_at)))?;
            println!(
                "{abs}: {} in folders, as of {}",
                human(node.total_bytes as i64),
                match util::ago(cur.started_at).as_str() {
                    "now" => "just now".to_string(),
                    a => format!("{a} ago"),
                }
            );
            let base = abs.trim_end_matches('/');
            let mut rows: Vec<(String, u64)> = Vec::new();
            for c in &node.children {
                rows.push((format!("{base}/{}", c.name), c.total_bytes));
                for g in &c.children {
                    rows.push((format!("{base}/{}/{}", c.name, g.name), g.total_bytes));
                }
            }
            rows.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
            let width = util::term_width().max(60);
            for (f, b) in rows.into_iter().take(top) {
                println!(
                    "{:>12}  {}",
                    human(b as i64),
                    util::shorten_middle(&f, width.saturating_sub(14))
                );
            }
            if let Some(prev) = snapshot::load_path(&cur_path.with_extension("prev.snap"))? {
                let inside =
                    |p: &str| base.is_empty() || p == base || p.starts_with(&format!("{base}/"));
                let changes: Vec<_> = walker::diff(&prev.root, &cur.root, 16 << 20)
                    .into_iter()
                    .filter(|c| inside(&c.path))
                    .collect();
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
                        util::shorten_middle(&c.path, width.saturating_sub(14))
                    );
                }
            }
        }
        Some(Cmd::Docker {
            fixes,
            big_log,
            top,
        }) => {
            let r = docker::collect()?;
            if json {
                println!("{}", api::docker_json(&r));
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
                        format!(
                            "UNUSED (remove by hand: {} volume rm NAME)",
                            docker::program(v.runtime)
                        )
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
            files,
            folders,
            long,
            min,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let b = insight::biggest(&p, threads, top, sentinel::parse_bytes(&min)?)?;
            let users = insight::users();
            let now = util::now_secs();
            let folder_rows = || -> Vec<(String, u64)> {
                // Folders one and two levels down, biggest first, without a
                // parent repeating its own biggest child.
                walker::largest_at_depth(&b.tree.root, 2, top)
            };
            if json {
                println!("{}", api::biggest_json(&b.tree, &b.files, top));
                return Ok(());
            }
            let width = util::term_width().max(60);
            let st = &b.tree.stats;
            println!(
                "{p}: {} in {} files and {} folders (walked in {:.0}s){}",
                human(b.tree.root.total_bytes as i64),
                st.files,
                st.dirs,
                st.seconds,
                if st.errors > 0 {
                    format!(", {} entries unreadable", st.errors)
                } else {
                    String::new()
                }
            );
            if !folders {
                println!("\nBiggest files");
                if b.files.is_empty() {
                    println!("  none over {min}");
                }
                for f in &b.files {
                    let owner = if long {
                        format!(
                            "{:<10} ",
                            users
                                .get(&f.uid)
                                .cloned()
                                .unwrap_or_else(|| f.uid.to_string())
                        )
                    } else {
                        String::new()
                    };
                    let head = format!(
                        "  {:>10}  {:>5}  {owner}",
                        human(f.bytes as i64),
                        insight::age_text(f.mtime, now)
                    );
                    let room = width.saturating_sub(head.chars().count()).max(20);
                    let what = match (insight::describe(&f.path), long) {
                        (Some(d), false) => d,
                        (Some(d), true) => format!("{d}  {}", f.path),
                        (None, false) => util::shorten_middle(&f.path, room),
                        (None, true) => f.path.clone(),
                    };
                    println!("{head}{what}");
                }
            }
            if !files {
                println!("\nBiggest folders");
                for (f, s) in folder_rows() {
                    println!(
                        "  {:>10}  {}",
                        human(s as i64),
                        util::shorten_middle(&f, width.saturating_sub(14))
                    );
                }
            }
            if let Ok(Some(prev)) = snapshot::load(std::path::Path::new(snapshot::DEFAULT_DIR), &p)
            {
                let ch: Vec<_> = walker::diff(&prev.root, &b.tree.root, 64 << 20)
                    .into_iter()
                    .filter(|c| c.delta() > 0)
                    .take(5)
                    .collect();
                if !ch.is_empty() {
                    println!(
                        "\nGrew since the last snapshot ({} ago)",
                        util::ago(prev.started_at)
                    );
                    for c in ch {
                        println!(
                            "  +{:>9}  {}",
                            human(c.delta()),
                            util::shorten_middle(&c.path, width.saturating_sub(14))
                        );
                    }
                }
            }
            let s: Vec<fix::Suggestion> = b
                .files
                .iter()
                .map(|f| fix::delete_file(&f.path, "One of the biggest files on this disk; dux cannot know whether you still need it.", f.bytes))
                .collect();
            fix::offer("the biggest files", s, &fixes)?;
            if !fixes.fix && fixes.select.is_none() {
                println!("More: -n 30 · --files · --folders · -l (owners, full paths) · dux where <folder>");
            }
        }
        Some(Cmd::Logs {
            path,
            top,
            min,
            sample,
            long,
            threads,
            fixes,
        }) => {
            let p = path.to_string_lossy().into_owned();
            let r = insight::logs(&p, threads, sentinel::parse_bytes(&min)?, sample)?;
            let now = util::now_secs();
            if json {
                println!("{}", api::logs_json(&r.logs, r.journal_bytes, &[]));
                return Ok(());
            }
            let h = |b: u64| human(b as i64);
            println!(
                "Logs on {p} (walked in {:.1}s, logs of 10 MiB and up)",
                r.walk_seconds
            );
            // Grouped by owner (spec CLI-5): OS, journal, web server, database,
            // container, pm2, .NET, app (by the systemd service writing it).
            let owners: Vec<(&str, String)> = r
                .logs
                .iter()
                .map(|l| ui::views::log_owner(l, &[]))
                .collect();
            let mut groups: Vec<(&str, u64, usize)> = Vec::new();
            if let Some(j) = r.journal_bytes {
                groups.push(("Journal", j, 0));
            }
            for (l, (cat, _)) in r.logs.iter().zip(&owners) {
                if *cat == "Journal" && r.journal_bytes.is_some() {
                    continue;
                }
                match groups.iter_mut().find(|g| g.0 == *cat) {
                    Some(g) => {
                        g.1 += l.bytes;
                        g.2 += 1;
                    }
                    None => groups.push((cat, l.bytes, 1)),
                }
            }
            groups.sort_by_key(|g| std::cmp::Reverse(g.1));
            for (cat, b, _) in &groups {
                println!("  {:<17} {:>10}", cat, h(*b));
            }
            if r.rotated_total() > 0 {
                println!(
                    "  {:<17} {:>10}  old rotated copies (dux logs --fix)",
                    "(rotated)",
                    h(r.rotated_total())
                );
            }
            println!("\n        size  growth/min   age  owner                 path");
            for (l, (_, owner)) in r.logs.iter().zip(&owners).take(top) {
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
                if long {
                    if let Some(w) = &l.writer {
                        note.push_str(&format!(" [written by {w}]"));
                    }
                }
                let path = if long {
                    l.path.clone()
                } else {
                    util::shorten_middle(&l.path, 60)
                };
                println!(
                    "  {:>10}  {:>10}  {:>4}  {:<20}  {path}{note}",
                    h(l.bytes),
                    g,
                    insight::age_text(l.mtime, now),
                    util::shorten_middle(owner, 20),
                );
            }
            if r.logs.len() > top {
                println!("  ... {} more (-n {})", r.logs.len() - top, r.logs.len());
            }
            if !long {
                println!("More: -n 50 · -l (full paths, writers) · --min 1M · --fix");
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
            // Protected logs (spec A7.3): only on a disk at 97 % or more, and
            // only by an interactive answer (RISKY is never applied by
            // --select / --apply-*), for protected logs over 250 MB.
            for l in r.logs.iter().filter(|l| {
                l.protected
                    && !l.rotated
                    && l.bytes > 250_000_000
                    && !cleanup::is_binary_log(std::path::Path::new(&l.path))
            }) {
                let Ok(u) = sentinel::statvfs(&l.path) else {
                    continue;
                };
                if u.used_pct() < 97.0 {
                    continue;
                }
                let item = cleanup::Item {
                    level: 2,
                    what: format!(
                        "truncate protected log {} ({}, disk {:.0}% full; keep its last 10 MiB)",
                        l.path,
                        h(l.bytes),
                        u.used_pct()
                    ),
                    action: cleanup::Action::TailAndTruncate {
                        path: l.path.clone().into(),
                        keep: 10 << 20,
                    },
                    frees: l.bytes.saturating_sub(10 << 20),
                };
                s.push(fix::suggestion(
                    fix::Safety::Risky,
                    item.what.clone(),
                    "A protected log (security, database, mail or kernel): it may be needed for audits or recovery. Offered only because the disk is critically full.",
                    "Older lines are gone; the saved .dux-tail and rotated copies remain.",
                    item.frees,
                    Some(item),
                    None,
                ));
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
                println!("{}", api::projects_json(&projects, &apps));
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
            mut fixes,
            level,
            stale_days,
            quick,
        }) => {
            // The same list as the dashboard and /api/v1/fixes (as data).
            if json {
                let g = gather::gather("/", 4, None);
                println!("{}", api::fixes_json(&g.fixes));
                return Ok(());
            }
            // `dux fix` is the fix list: always show it.
            fixes.fix = true;
            let before = sentinel::statvfs("/")?;
            // The one Cleanup list (UI, web, --json): only rule-based items
            // (spec A6.18-A6.25). --quick skips the walk.
            let mut s: Vec<fix::Suggestion> = if quick {
                let opts = cleanup::Options {
                    level: 3,
                    used_pct: before.used_pct(),
                    ..cleanup::Options::default()
                };
                cleanup::plan(&opts).into_iter().map(fix::assess).collect()
            } else {
                gather::gather("/", 4, None).fixes
            };
            let _ = stale_days;
            if let Some(l) = level {
                // Items carry the cleanup level they belong to; hand-only
                // explanations stay visible at every level.
                s.retain(|x| x.item.as_ref().is_none_or(|i| i.level <= l));
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
            refresh,
        }) => {
            let token = match token {
                Some(t) => {
                    anyhow::ensure!(
                        t.len() >= 16 && !t.chars().any(char::is_whitespace),
                        "--token needs at least 16 characters (no spaces); leave it out for a random one"
                    );
                    t
                }
                None => web::random_token()?,
            };
            let loopback = listen.starts_with("127.")
                || listen.starts_with("localhost")
                || listen.starts_with("[::1]");
            if !loopback {
                eprintln!("dux web: WARNING listening on {listen}, reachable from other machines; keep the token secret");
            }
            println!("dux web: open http://{listen}/?token={token}");
            println!("dux web: read-only dashboard; API at /api/v1/..., Prometheus at /metrics (Authorization: Bearer <token>)");
            let srv = web::Server::new(path.to_string_lossy().into_owned(), token, refresh);
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
                    println!("{}", api::io_json(&l, top));
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
                println!(
                    "{}",
                    serde_json::Value::Array(
                        rows.iter()
                            .map(|r| serde_json::json!({"pid": r.pid, "process": r.process, "uid": r.uid, "size": r.size, "path": r.path}))
                            .collect()
                    )
                );
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
        Some(Cmd::Auto { op, path, config }) => {
            let (_, s) = notify::NotifyConfig::load_checked(&config)?;
            let dir = s.snapshot_dir.clone();
            let mut st = auto::State::load(&dir);
            match (op.as_str(), path) {
                ("keep" | "unkeep", Some(p)) => {
                    let p = protect::normalize(&std::path::absolute(&p)?.to_string_lossy());
                    if op == "keep" {
                        if !st.keep.contains(&p) {
                            st.keep.push(p.clone());
                        }
                        st.announced.remove(&p);
                        println!("automatic cleanup will not delete {p}");
                    } else {
                        st.keep.retain(|k| *k != p);
                        println!("{p} may be cleaned automatically again");
                    }
                    st.save(&dir)?;
                }
                ("keep" | "unkeep", None) => anyhow::bail!("dux auto {op} PATH"),
                ("allow", _) => println!("{}", auto::apply_dropin(&s.auto)?),
                _ => {
                    let a = &s.auto;
                    if !a.enabled {
                        println!("Automatic cleanup: off (turn it on with dux setup)");
                    } else {
                        let stop = if a.stop_pct > 0.0 {
                            format!(", stops below {:.0}%", a.stop_pct)
                        } else {
                            String::new()
                        };
                        println!("Automatic cleanup: on, starts at {:.0}%{stop}", a.start_pct);
                        let kinds = [
                            auto::Kind::Journal,
                            auto::Kind::RotatedLogs,
                            auto::Kind::TruncateLogs,
                            auto::Kind::BuildCaches,
                            auto::Kind::DockerImages,
                            auto::Kind::DockerVolumes,
                        ];
                        let on: Vec<&str> = kinds
                            .iter()
                            .filter(|k| a.allows(**k))
                            .map(|k| k.label())
                            .collect();
                        println!("  approved: {}", on.join(", "));
                        println!("  folders:  {}", a.folders.join(", "));
                    }
                    let now = util::now_secs();
                    for (p, t) in &st.announced {
                        let left = (auto::NOTICE_SECS - (now - t)).max(0);
                        println!(
                            "  announced: {p} (deleted in {}h unless kept: dux auto keep {p})",
                            left / 3600
                        );
                    }
                    for k in &st.keep {
                        println!("  kept: {k}");
                    }
                    for p in st.watched.keys() {
                        println!("  watched after cleanup: {p}");
                    }
                }
            }
        }
        Some(Cmd::Setup {
            config,
            print_example,
        }) => {
            if print_example {
                print!("{}", config::example());
                return Ok(());
            }
            setup::run_terminal(&config)?;
        }
        Some(Cmd::Actions { state }) => {
            let brake = std::path::Path::new(cleanup::KILL_SWITCH);
            match state.as_str() {
                "off" => {
                    if let Some(d) = brake.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    std::fs::write(
                        brake,
                        "dux actions are turned off; remove with: dux actions on\n",
                    )
                    .with_context(|| format!("creating {} (run as root)", brake.display()))?;
                    println!("dux will refuse every change until `dux actions on`.");
                }
                "on" => match std::fs::remove_file(brake) {
                    Ok(()) => println!("dux changes are allowed again."),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        println!("dux changes were already allowed.")
                    }
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("removing {} (run as root)", brake.display()))
                    }
                },
                _ => println!(
                    "{}",
                    if brake.exists() {
                        "actions are OFF: dux refuses every change (dux actions on to allow)"
                    } else {
                        "actions are ON: changes are allowed after confirmation"
                    }
                ),
            }
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
        None => {
            // Bare `dux` or `dux <path>` opens the 0.7+ terminal UI.
            let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("/"));
            // A word that is neither a folder nor a command (a removed or
            // mistyped command) gets a clear answer, not an empty UI.
            anyhow::ensure!(
                root.is_dir(),
                "{} is not a folder or a dux command; see `dux help`",
                root.display()
            );
            ui::run(&root.to_string_lossy())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_commands_parse_with_shared_fix_options() {
        assert!(matches!(
            Cli::try_parse_from(["dux", "docker"]).unwrap().cmd,
            Some(Cmd::Docker { .. })
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
        assert!(Cli::try_parse_from(["dux", "biggest", "--fix", "-n", "30", "--files"]).is_ok());
    }

    #[test]
    fn removed_commands_are_gone_and_kept_ones_parse() {
        for gone in [
            "walk",
            "cleanup",
            "scan",
            "top",
            "find",
            "growth",
            "diff",
            "du",
            "metrics",
            "containers",
            "by-owner",
            "by-ext",
            "explain",
            "status",
            "daemon",
            "tunnel",
            "overview",
            "tui",
        ] {
            // Not a subcommand any more: the word is taken as a path, which
            // real_main refuses when no such folder exists.
            assert!(
                Cli::try_parse_from(["dux", gone])
                    .map(|c| c.cmd.is_none())
                    .unwrap_or(true),
                "{gone} must no longer be a command"
            );
        }
        for kept in [
            "leaks",
            "deleted-open",
            "doctor",
            "where",
            "biggest",
            "logs",
            "docker",
            "pp",
            "fix",
            "cleanup",
            "io",
            "walk",
            "sentinel",
            "never-touch",
            "notify-test",
            "ui",
            "web",
        ] {
            assert!(
                Cli::try_parse_from(["dux", kept]).is_ok(),
                "{kept} must parse"
            );
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

    // Scan a temp tree and assert: sparse files count 0 blocks, disk usage uses
    // blocks, path scoping isolates subtrees, hardlinks don't double-count.
}
