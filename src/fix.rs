//! Guided fixes shared by `dux doctor`, `docker`, `logs`, `pp`, `biggest`
//! and `fix`: every suggestion is rated SAFE, CAUTION or RISKY, explained,
//! sized, and picked from one numbered list.
//!
//! On a terminal the user types `1,3,5-7`, `safe`, `all`, `truncate`,
//! `remove`, `clean` or nothing. RISKY items can only be picked by number and
//! need `delete` typed to confirm. Scripts use `--select`, `--apply-safe` or
//! `--apply-all`. Nothing runs without a confirmation or one of those flags.

use crate::cleanup::{Action, Item};
use std::io::{BufRead, IsTerminal, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    /// Rebuilds itself or is an old copy: no real data is lost.
    Safe,
    /// Recoverable, but costs a re-download, a rebuild or old log lines.
    Caution,
    /// Can lose data or break something: only by number, with `delete` typed.
    Risky,
}

impl Safety {
    pub fn label(self) -> &'static str {
        match self {
            Safety::Safe => "SAFE",
            Safety::Caution => "CAUTION",
            Safety::Risky => "RISKY",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub safety: Safety,
    pub title: String,
    pub why: String,
    pub undo: String,
    pub frees: u64,
    /// What dux runs if picked; None means "do this by hand" (`manual`).
    pub item: Option<Item>,
    pub manual: Option<String>,
}

/// Command-line choices shared by every command that offers fixes.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct FixArgs {
    /// Show the cleanup suggestions and pick which to apply
    #[arg(long)]
    pub fix: bool,
    /// Apply these numbers from the fix list without asking, e.g. 1,3,5-7
    #[arg(long, value_name = "LIST")]
    pub select: Option<String>,
    /// Apply every SAFE fix without asking
    #[arg(long)]
    pub apply_safe: bool,
    /// Apply every SAFE and CAUTION fix without asking
    #[arg(long)]
    pub apply_all: bool,
    /// List the fixes but never ask or apply
    #[arg(long, hide = true)]
    pub no_fix: bool,
}

pub fn suggestion(
    safety: Safety,
    title: String,
    why: &str,
    undo: &str,
    frees: u64,
    item: Option<Item>,
    manual: Option<String>,
) -> Suggestion {
    Suggestion {
        safety,
        title,
        why: why.into(),
        undo: undo.into(),
        frees,
        item,
        manual,
    }
}

/// A RISKY finding that must be handled by hand.
pub fn risky(title: String, why: &str, manual: String, frees: u64) -> Suggestion {
    suggestion(
        Safety::Risky,
        title,
        why,
        "May not be possible.",
        frees,
        None,
        Some(manual),
    )
}

/// A RISKY one-file deletion the user can pick by number.
pub fn delete_file(path: &str, why: &str, frees: u64) -> Suggestion {
    let item = Item {
        level: 4,
        what: format!("delete {path}"),
        action: Action::Remove { path: path.into() },
        frees,
    };
    suggestion(
        Safety::Risky,
        format!("delete {path}"),
        why,
        "Only from your own backup.",
        frees,
        Some(item),
        None,
    )
}

/// A RISKY Docker volume removal the user can pick by number.
pub fn delete_volume(name: &str, frees: u64) -> Suggestion {
    let item = Item {
        level: 4,
        what: format!("delete unused Docker volume {name}"),
        action: Action::Run {
            program: "docker".into(),
            args: vec!["volume".into(), "rm".into(), name.into()],
        },
        frees,
    };
    suggestion(
        Safety::Risky,
        format!("delete unused Docker volume {name}"),
        "No container uses it now, but it may hold a database or uploads.",
        "Only from your own backup.",
        frees,
        Some(item),
        None,
    )
}

/// Rate and explain a planned cleanup action from `cleanup`/`docker`/`pp`.
pub fn assess(item: Item) -> Suggestion {
    let (safety, why, undo) = match &item.action {
        Action::Run { program, args } => {
            let a = args.join(" ");
            // Podman takes the same subcommands and carries the same risks.
            let program = if program == "podman" { "docker".to_string() } else { program.clone() };
            match (program.as_str(), a.as_str()) {
                ("journalctl", _) => (Safety::Safe, "Deletes the oldest journal entries beyond the size budget; recent logs stay.", "Not needed: old journal entries are history only."),
                ("apt-get", _) | ("dnf", _) => (Safety::Safe, "Deletes downloaded package files that are already installed.", "Downloaded again automatically when needed."),
                ("systemd-tmpfiles", _) => (Safety::Safe, "Removes temporary files older than the system's age rules; files in use are skipped.", "Not needed: they are temporary."),
                ("docker", a) if a.starts_with("builder prune") => (Safety::Safe, "Removes Docker build cache unused for 7 days.", "Rebuilt on the next docker build (slower once)."),
                ("docker", "image prune -f") => (Safety::Safe, "Removes untagged images no container uses.", "Not needed: nothing runs from them."),
                ("docker", a) if a.starts_with("image prune -a") => (Safety::Caution, "Removes images no container uses, older than 30 days.", "docker pull downloads them again (needs registry access)."),
                ("docker", a) if a.starts_with("rm ") => (Safety::Risky, "Removes a stopped container and the files written inside it (not its volumes); its writable layer may hold the only copy of data.", "Recreate it with docker run or compose up; files written inside it are gone."),
                ("docker", a) if a.starts_with("image rm") => (Safety::Caution, "Removes an image no container uses.", "docker pull downloads it again (needs registry access)."),
                ("docker", a) if a.starts_with("container prune") => (Safety::Risky, "Removes stopped containers older than 7 days and files written inside them.", "Recreate with docker run / compose up; their internal files are gone."),
                ("docker", a) if a.starts_with("volume rm") => (Safety::Risky, "Deletes a Docker volume no container uses. It may hold a database or uploads.", "Only from your own backup."),
                ("logrotate", _) => (Safety::Caution, "Forces the logrotate rotation of this log's config now: every log in that config is rotated and archives beyond its `rotate N` are deleted. Configs that also list security logs are never forced.", "Rotated copies stay as .1/.gz files until their normal expiry."),
                _ => (Safety::Caution, "Runs a cleanup command.", "Depends on the command."),
            }
        }
        Action::Remove { path } => {
            let p = path.to_string_lossy();
            if p.contains("/crash") || p.contains("coredump") {
                (Safety::Safe, "Deletes an old crash dump; the newest 5 are kept.", "Not needed unless debugging that crash.")
            } else {
                (Safety::Safe, "Deletes an old rotated copy of a log; the newest 3 are kept.", "Not needed unless investigating that period.")
            }
        }
        Action::TailAndTruncate { .. } => (Safety::Caution, "Empties a big active log in place; its last 10 MiB are saved as .dux-tail and the app keeps writing.", "Older lines are gone; the saved tail and rotated copies remain."),
        Action::RemoveTree { .. } => (Safety::Caution, "Deletes build output (node_modules, target, .venv...). Source code and git history are untouched.", "Rebuild with the project's tools: npm install, cargo build..."),
    };
    Suggestion {
        safety,
        title: item.what.clone(),
        why: why.into(),
        undo: undo.into(),
        frees: item.frees,
        item: Some(item),
        manual: None,
    }
}

/// Parse "1,3,5-7" into 0-based indexes below `n`.
pub fn parse_numbers(s: &str, n: usize) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in s.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: usize = a.parse().map_err(|_| format!("not a number: {part}"))?;
        let b: usize = b.parse().map_err(|_| format!("not a number: {part}"))?;
        if a == 0 || b < a || b > n {
            return Err(format!("{part} is outside 1-{n}"));
        }
        out.extend((a - 1)..b);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Which suggestions a typed choice picks.
pub fn choose(word: &str, s: &[Suggestion]) -> Result<Vec<usize>, String> {
    let applicable = |i: usize| s[i].item.is_some();
    let by = |f: &dyn Fn(&Suggestion) -> bool| {
        (0..s.len())
            .filter(|&i| applicable(i) && s[i].safety != Safety::Risky && f(&s[i]))
            .collect()
    };
    Ok(match word.trim().to_ascii_lowercase().as_str() {
        "" | "q" | "quit" | "n" | "no" => Vec::new(),
        "safe" | "s" => by(&|x| x.safety == Safety::Safe),
        "all" | "a" => by(&|_| true),
        "truncate" | "t" => by(&|x| {
            matches!(
                x.item.as_ref().map(|i| &i.action),
                Some(Action::TailAndTruncate { .. })
            )
        }),
        "remove" | "r" => by(&|x| {
            matches!(
                x.item.as_ref().map(|i| &i.action),
                Some(Action::Remove { .. } | Action::RemoveTree { .. })
            )
        }),
        "clean" | "c" => {
            by(&|x| matches!(x.item.as_ref().map(|i| &i.action), Some(Action::Run { .. })))
        }
        other => {
            let picked = parse_numbers(other, s.len())?;
            if let Some(&i) = picked.iter().find(|&&i| !applicable(i)) {
                return Err(format!(
                    "{} must be done by hand: {}",
                    i + 1,
                    s[i].manual.clone().unwrap_or_default()
                ));
            }
            picked
        }
    })
}

fn read_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

/// The path a suggestion would change, if any.
pub fn target_path(s: &Suggestion) -> Option<String> {
    match &s.item.as_ref()?.action {
        Action::Remove { path }
        | Action::TailAndTruncate { path, .. }
        | Action::RemoveTree { path } => Some(path.to_string_lossy().into_owned()),
        Action::Run { .. } => None,
    }
}

fn blocked(x: &Suggestion) -> bool {
    x.item
        .as_ref()
        .is_some_and(|i| crate::cleanup::allowed(i).is_err())
}

/// Drop suggestions the never-touch list forbids (same rules as apply time:
/// real locations, parents of listed paths, command scopes; an unreadable
/// list blocks everything). Returns how many were dropped.
/// Keep forbidden suggestions in the list but make them unselectable, so
/// the numbers a user saw in a preview still name the same fixes.
fn mark_protected(s: &mut [Suggestion]) -> usize {
    let mut n = 0;
    for x in s.iter_mut().filter(|x| blocked(x)) {
        x.item = None;
        x.title = format!("[never touch] {}", x.title);
        x.manual = Some(format!(
            "on (or containing a path on) the never-touch list {}; dux will not do this",
            crate::protect::FILE
        ));
        n += 1;
    }
    n
}

/// Print the numbered fix list, then apply what the user picks.
/// Returns bytes the applied actions were expected to free.
pub fn offer(title: &str, mut s: Vec<Suggestion>, args: &FixArgs) -> anyhow::Result<u64> {
    let h = |b: u64| crate::util::human(b as i64);
    if s.is_empty() {
        println!("\nFixes: nothing to suggest for {title}.");
        return Ok(0);
    }
    // Reports stay short: the fix list appears only when asked for.
    let asked = args.fix || args.select.is_some() || args.apply_safe || args.apply_all;
    if !asked {
        if !args.no_fix {
            let total: u64 = s.iter().filter(|x| x.item.is_some()).map(|x| x.frees).sum();
            println!(
                "\nCleanup: {} suggestions could free up to {}. Review them with --fix.",
                s.len(),
                crate::util::human(total as i64)
            );
        }
        return Ok(0);
    }
    s.sort_by(|a, b| a.safety.cmp(&b.safety).then(b.frees.cmp(&a.frees)));
    // An unreadable never-touch list (a normal user, or a damaged file)
    // blocks every change; say that instead of calling everything protected.
    let unreadable = crate::protect::policy_from(std::path::Path::new(crate::protect::FILE)).err();
    if let Some(e) = &unreadable {
        println!("\nNote: {e:#}. Fixes are shown, but none can be applied until the list can be read (run as root).");
    }
    let skipped = if unreadable.is_some() {
        0
    } else {
        mark_protected(&mut s)
    };
    if skipped > 0 {
        println!(
            "\n({skipped} suggestions are on the never-touch list {} and cannot be picked)",
            crate::protect::FILE
        );
    }
    if skipped == s.len() {
        println!("\nFixes: every suggestion for {title} is on the never-touch list.");
        return Ok(0);
    }
    let mut t = [0u64; 3];
    for x in s.iter().filter(|x| !x.title.starts_with("[never touch]")) {
        t[x.safety as usize] += x.frees;
    }
    println!(
        "\nFixes for {title}: SAFE {}, CAUTION {}, RISKY {}",
        h(t[0]),
        h(t[1]),
        h(t[2])
    );
    for (i, x) in s.iter().enumerate() {
        println!(
            "  {:>3}. {:<7} {:>10}  {}",
            i + 1,
            x.safety.label(),
            h(x.frees),
            x.title
        );
        println!("                          {}  Undo: {}", x.why, x.undo);
        if x.item.is_none() {
            if let Some(m) = &x.manual {
                println!("                          by hand: {m}");
            }
        }
    }
    let picked: Vec<usize> = if let Some(list) = &args.select {
        choose(list, &s).map_err(|e| anyhow::anyhow!(e))?
    } else if args.apply_all {
        choose("all", &s).map_err(|e| anyhow::anyhow!(e))?
    } else if args.apply_safe {
        choose("safe", &s).map_err(|e| anyhow::anyhow!(e))?
    } else if args.no_fix || !std::io::stdin().is_terminal() {
        println!("\nPick fixes with --select 1,3 | --apply-safe | --apply-all (or run on a terminal to choose).");
        return Ok(0);
    } else {
        loop {
            let w = read_line("\nChoose: numbers (1,3,5-7) | safe | all | truncate | remove | clean | Enter = nothing: ");
            match choose(&w, &s) {
                Ok(p) => break p,
                Err(e) => println!("  {e}"),
            }
        }
    };
    if picked.is_empty() {
        println!("Nothing applied.");
        return Ok(0);
    }
    let risky: Vec<usize> = picked
        .iter()
        .copied()
        .filter(|&i| s[i].safety == Safety::Risky)
        .collect();
    let interactive = args.select.is_none() && !args.apply_all && !args.apply_safe;
    let mut picked = picked;
    if !risky.is_empty() {
        anyhow::ensure!(
            interactive,
            "RISKY items need an interactive `delete` confirmation; leave them out of --select"
        );
        println!("\nRISKY items picked; these can lose data:");
        for &i in &risky {
            println!("  {}. {}", i + 1, s[i].title);
        }
        if read_line("Type delete to include them: ") != "delete" {
            picked.retain(|&i| s[i].safety != Safety::Risky);
            println!("RISKY items left out.");
        }
    }
    let total: u64 = picked.iter().map(|&i| s[i].frees).sum();
    if interactive
        && !read_line(&format!(
            "Apply {} fixes, freeing about {}? [y/N] ",
            picked.len(),
            h(total)
        ))
        .eq_ignore_ascii_case("y")
    {
        println!("Nothing applied.");
        return Ok(0);
    }
    // SAFETY: geteuid has no preconditions.
    anyhow::ensure!(unsafe { libc::geteuid() } == 0, "applying fixes needs root");
    let mut freed = 0;
    for &i in &picked {
        let x = &s[i];
        let Some(item) = &x.item else { continue };
        match crate::cleanup::apply(item) {
            Ok(()) => {
                freed += x.frees;
                println!("  done    {:>10}  {}", h(x.frees), x.title);
            }
            Err(e) => println!("  skipped {:>10}  {}: {e:#}", h(x.frees), x.title),
        }
    }
    println!(
        "Freed about {}. Audit log: {}",
        h(freed),
        crate::cleanup::AUDIT_LOG
    );
    Ok(freed)
}

/// The group a cleanup suggestion is summed under (spec UI-1 view 4).
pub fn group(x: &Suggestion) -> &'static str {
    let Some(item) = &x.item else {
        return "Explained only";
    };
    match &item.action {
        Action::Run { program, .. } => match std::path::Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
        {
            "journalctl" => "Journal",
            "logrotate" => "Logs",
            "docker" | "podman" => "Docker",
            "apt-get" | "apt" | "dnf" | "yum" => "Packages",
            _ => "System",
        },
        Action::TailAndTruncate { path, .. } if path.to_string_lossy().contains("/containers/") => {
            "Docker"
        }
        Action::TailAndTruncate { .. } => "Logs",
        Action::Remove { .. } if item.level >= 4 => "Big files",
        Action::Remove { path } => {
            let p = path.to_string_lossy();
            if p.contains("/crash") || p.contains("coredump") {
                "System"
            } else {
                "Logs"
            }
        }
        Action::RemoveTree { .. } => "Projects",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(action: Action, frees: u64) -> Item {
        Item {
            level: 3,
            what: "x".into(),
            action,
            frees,
        }
    }

    fn sample() -> Vec<Suggestion> {
        vec![
            assess(it(
                Action::Remove {
                    path: "/var/log/a.1".into(),
                },
                10,
            )),
            assess(it(
                Action::TailAndTruncate {
                    path: "/var/log/big".into(),
                    keep: 1,
                },
                20,
            )),
            assess(it(
                Action::Run {
                    program: "apt-get".into(),
                    args: vec!["clean".into()],
                },
                30,
            )),
            suggestion(
                Safety::Risky,
                "big file".into(),
                "data",
                "backup",
                40,
                Some(it(
                    Action::Remove {
                        path: "/home/a/big.iso".into(),
                    },
                    40,
                )),
                None,
            ),
            suggestion(
                Safety::Risky,
                "held open".into(),
                "restart",
                "-",
                50,
                None,
                Some("systemctl restart x".into()),
            ),
        ]
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_numbers("1,3,5-7", 9).unwrap(), vec![0, 2, 4, 5, 6]);
        assert_eq!(parse_numbers(" 2 2 1 ", 3).unwrap(), vec![0, 1]);
        assert!(parse_numbers("0", 3).is_err());
        assert!(parse_numbers("4", 3).is_err());
        assert!(parse_numbers("3-1", 3).is_err());
    }

    #[test]
    fn words_pick_the_right_actions_and_never_risky() {
        let s = sample();
        assert_eq!(choose("safe", &s).unwrap(), vec![0, 2]);
        assert_eq!(choose("all", &s).unwrap(), vec![0, 1, 2]);
        assert_eq!(choose("truncate", &s).unwrap(), vec![1]);
        assert_eq!(
            choose("remove", &s).unwrap(),
            vec![0],
            "risky removals are not swept up by words"
        );
        assert_eq!(choose("clean", &s).unwrap(), vec![2]);
        assert_eq!(choose("", &s).unwrap(), Vec::<usize>::new());
        assert_eq!(choose("4", &s).unwrap(), vec![3], "risky only by number");
        assert!(
            choose("5", &s).is_err(),
            "manual-only items cannot be applied"
        );
    }

    #[test]
    fn ratings() {
        let run = |p: &str, a: &[&str]| Action::Run {
            program: p.into(),
            args: a.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            assess(it(run("journalctl", &["--vacuum-size=1G"]), 1)).safety,
            Safety::Safe
        );
        assert_eq!(
            assess(it(run("docker", &["image", "prune", "-a", "-f"]), 1)).safety,
            Safety::Caution
        );
        assert_eq!(
            assess(it(run("docker", &["volume", "rm", "v"]), 1)).safety,
            Safety::Risky
        );
        assert_eq!(
            assess(it(
                Action::RemoveTree {
                    path: "/p/target".into()
                },
                1
            ))
            .safety,
            Safety::Caution
        );
    }

    #[test]
    fn without_a_terminal_or_flags_nothing_is_applied() {
        assert_eq!(offer("test", sample(), &FixArgs::default()).unwrap(), 0);
        let no = FixArgs {
            no_fix: true,
            ..FixArgs::default()
        };
        assert_eq!(offer("test", sample(), &no).unwrap(), 0);
        let risky = FixArgs {
            select: Some("4".into()),
            ..FixArgs::default()
        };
        assert!(
            offer("test", sample(), &risky).is_err(),
            "RISKY via --select is refused"
        );
    }
}
