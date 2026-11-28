mod attrib;
mod cleanup;
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
    /// Web dashboard: token protected, read-only unless --allow-actions
    ///
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
    /// Send a test alert to every configured channel
    ///
    /// Send a test alert to every configured channel
    NotifyTest {
        /// KEY=VALUE settings file with channels
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

    match cli.cmd {
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
                let fl: Vec<_> = b.files.iter().map(|f| serde_json::json!({"path": f.path, "bytes": f.bytes, "apparent": f.apparent, "mtime": f.mtime, "owner": users.get(&f.uid), "what": insight::describe(&f.path)})).collect();
                let dirs: Vec<_> = folder_rows()
                    .into_iter()
                    .map(|(p, b)| serde_json::json!({"path": p, "bytes": b}))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({"files": fl, "folders": dirs, "entries": b.tree.stats.entries, "file_count": b.tree.stats.files, "folder_count": b.tree.stats.dirs, "unreadable": b.tree.stats.errors, "hardlinks_counted_once": b.tree.stats.hardlinks_deduped, "seconds": b.tree.stats.seconds})
                );
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
                let logs: Vec<_> = r.logs.iter().map(|l| serde_json::json!({"path": l.path, "bytes": l.bytes, "kind": l.kind.label(), "rotated": l.rotated, "protected": l.protected, "bytes_per_min": l.per_min, "writer": l.writer, "container": l.container, "mtime": l.mtime})).collect();
                println!(
                    "{}",
                    serde_json::json!({"journal_bytes": r.journal_bytes, "logs": logs})
                );
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
            mut fixes,
            level,
            stale_days,
            quick,
        }) => {
            // `dux fix` is the fix list: always show it.
            fixes.fix = true;
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
