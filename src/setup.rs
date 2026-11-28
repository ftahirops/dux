//! `dux setup` (spec CFG-1): a guided way to write `/etc/dux/dux.toml`.
//!
//! Asks for alert channels (and sends a test), thresholds, never-touch
//! folders and automatic cleanup. Every answer defaults to the current value,
//! so it can be re-run at any time; Enter keeps a value, `-` clears it.

use crate::config::{self, Kind, Setting, SETTINGS};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Answers come from here: a terminal normally, a script in tests.
pub trait Ask {
    fn line(&mut self, prompt: &str) -> String;
    fn secret(&mut self, prompt: &str) -> String {
        self.line(prompt)
    }
    fn say(&mut self, text: &str) {
        println!("{text}");
    }
}

pub struct Terminal;

impl Ask for Terminal {
    fn line(&mut self, prompt: &str) -> String {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        let _ = std::io::stdin().lock().read_line(&mut s);
        s.trim().to_string()
    }
    fn secret(&mut self, prompt: &str) -> String {
        // No echo while a password or token is typed.
        let fd = libc::STDIN_FILENO;
        // SAFETY: termios is plain data; tcgetattr fills it or fails.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::tcgetattr(fd, &mut t) } == 0;
        let old = t;
        if ok {
            t.c_lflag &= !libc::ECHO;
            // SAFETY: t is a valid termios from tcgetattr.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
        }
        let s = self.line(prompt);
        if ok {
            // SAFETY: restoring the settings read above.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
            println!();
        }
        s
    }
}

fn find(section: &str, key: &str) -> &'static Setting {
    SETTINGS
        .iter()
        .find(|s| s.section == section && s.key == key)
        .expect("setting in table")
}

/// The answers so far: section.key -> raw value (as the DUX_* readers take it).
struct Draft {
    values: HashMap<String, String>,
}

impl Draft {
    fn id(st: &Setting) -> String {
        format!("{}.{}", st.section, st.key)
    }
    fn get(&self, st: &Setting) -> Option<&str> {
        self.values
            .get(&Self::id(st))
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
    fn raw(&self) -> HashMap<String, String> {
        SETTINGS
            .iter()
            .filter_map(|st| self.get(st).map(|v| (st.env.to_string(), v.to_string())))
            .collect()
    }
    /// Ask for one setting; Enter keeps, `-` clears. Re-asks until valid.
    fn ask(&mut self, a: &mut dyn Ask, section: &str, key: &str, label: &str) {
        let st = find(section, key);
        loop {
            let cur = self.get(st).map(str::to_string);
            let shown = match (&cur, st.kind) {
                (Some(_), Kind::Secret) => " [set; Enter keeps]".to_string(),
                (Some(v), _) => format!(" [{v}]"),
                (None, _) if !st.default.is_empty() && st.default != "[]" => {
                    format!(" [{}]", st.default.trim_matches('"'))
                }
                _ => String::new(),
            };
            let prompt = format!("  {label}{shown}: ");
            let v = if st.kind == Kind::Secret {
                a.secret(&prompt)
            } else {
                a.line(&prompt)
            };
            if v.is_empty() {
                return;
            }
            if v == "-" {
                self.values.remove(&Self::id(st));
                return;
            }
            match check(st, &v) {
                Ok(()) => {
                    self.values.insert(Self::id(st), v);
                    return;
                }
                Err(e) => a.say(&format!("    {e}")),
            }
        }
    }
}

/// Validate one typed value for its kind.
fn check(st: &Setting, v: &str) -> std::result::Result<(), String> {
    match st.kind {
        Kind::Percent => match v.trim_end_matches('%').parse::<f64>() {
            Ok(x) if (0.0..=100.0).contains(&x) => Ok(()),
            _ => Err("a number from 0 to 100".into()),
        },
        Kind::Number => v.parse::<f64>().map(|_| ()).map_err(|_| "a number".into()),
        Kind::Bool => match v.to_ascii_lowercase().as_str() {
            "y" | "yes" | "n" | "no" | "true" | "false" | "on" | "off" | "1" | "0" => Ok(()),
            _ => Err("yes or no".into()),
        },
        Kind::Duration => crate::util::parse_duration(v)
            .map(|_| ())
            .map_err(|_| "a duration like 30s, 5m, 1h".into()),
        Kind::Size => crate::sentinel::parse_bytes(v)
            .map(|_| ())
            .map_err(|_| "a size like 2G".into()),
        Kind::Severity => crate::notify::Severity::parse(v)
            .map(|_| ())
            .ok_or_else(|| "info, warning or critical".into()),
        Kind::Text | Kind::Secret | Kind::List => {
            if st.key.ends_with("_url") && !v.contains("://") {
                return Err("a URL like https://...".into());
            }
            if st.key == "smtp_url" && !v.starts_with("smtps://") && !v.starts_with("smtp://") {
                return Err("smtps://host:465 or smtp://host:587 (TLS is required)".into());
            }
            Ok(())
        }
    }
}

fn yes(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "y" | "yes" | "true" | "on" | "1"
    )
}

/// Run the questions and write `path`. `never_touch` is the list file.
pub fn run(a: &mut dyn Ask, path: &Path, never_touch: &Path) -> Result<()> {
    let loaded = config::load(path)?;
    let mut d = Draft {
        values: HashMap::new(),
    };
    // Start from what is configured now (files only, not the environment).
    for st in SETTINGS {
        if let (Some(v), Some(o)) = (loaded.raw.get(st.env), loaded.origin.get(st.env)) {
            if o.line > 0 && !v.trim().is_empty() {
                d.values.insert(Draft::id(st), v.trim().to_string());
            }
        }
    }
    a.say(&format!(
        "dux setup writes {}. Enter keeps the value in [brackets], - clears it.",
        path.display()
    ));

    a.say("\n1/4 Alerts. Leave a channel empty to skip it.");
    d.ask(
        a,
        "alerts",
        "ntfy_url",
        "ntfy topic URL (e.g. https://ntfy.sh/my-servers)",
    );
    if d.get(find("alerts", "ntfy_url")).is_some() {
        d.ask(a, "alerts", "ntfy_token", "ntfy token (optional)");
    }
    d.ask(a, "alerts", "slack_webhook", "Slack webhook URL");
    d.ask(a, "alerts", "webhook_url", "Webhook URL (JSON POST)");
    if d.get(find("alerts", "webhook_url")).is_some() {
        d.ask(
            a,
            "alerts",
            "webhook_token",
            "Webhook bearer token (optional)",
        );
    }
    d.ask(a, "alerts", "smtp_url", "Mail server (smtps://host:465)");
    if d.get(find("alerts", "smtp_url")).is_some() {
        d.ask(a, "alerts", "smtp_user", "Mail user");
        d.ask(a, "alerts", "smtp_pass", "Mail password");
        d.ask(a, "alerts", "mail_from", "From address");
        d.ask(a, "alerts", "mail_to", "To addresses (comma-separated)");
    }
    d.ask(
        a,
        "broadcast",
        "enabled",
        "Broadcast to logged-in terminals when a disk is 97% full? (yes/no)",
    );
    let channels = crate::notify::NotifyConfig::from_map(d.raw()).channels();
    if channels.iter().any(|c| *c != "log" && *c != "wall")
        && !a
            .line("  Send a test alert now? [Y/n]: ")
            .eq_ignore_ascii_case("n")
    {
        let cfg = crate::notify::NotifyConfig::from_map(d.raw());
        let alert = crate::notify::Alert {
            severity: crate::notify::Severity::Warning,
            title: "test alert from dux setup".into(),
            body: format!(
                "If you can read this, this channel works. Configured: {}",
                channels.join(", ")
            ),
            key: "test".into(),
            host: crate::sentinel::hostname(),
        };
        let r = crate::notify::send(&cfg, &alert, false, crate::util::now_secs());
        a.say(&format!("    sent: {}", r.sent.join(", ")));
        for (name, err) in &r.failed {
            a.say(&format!(
                "    FAILED {name}: {err} (fix it and re-run dux setup)"
            ));
        }
    }

    a.say("\n2/4 Thresholds.");
    loop {
        d.ask(a, "disk", "warn_pct", "Warning when a disk is this % full");
        d.ask(a, "disk", "crit_pct", "Critical when a disk is this % full");
        let w: f64 = d
            .get(find("disk", "warn_pct"))
            .unwrap_or("85")
            .trim_end_matches('%')
            .parse()
            .unwrap_or(85.0);
        let c: f64 = d
            .get(find("disk", "crit_pct"))
            .unwrap_or("95")
            .trim_end_matches('%')
            .parse()
            .unwrap_or(95.0);
        if w < c {
            break;
        }
        a.say("    the warning level must be below the critical level");
    }

    a.say("\n3/4 Never-touch folders: dux never changes anything inside them.");
    let current = crate::protect::load_from(never_touch);
    if current.is_empty() {
        a.say("  (none yet)");
    } else {
        for p in &current {
            a.say(&format!("  {p}"));
        }
    }
    let add = a.line("  Add folders (comma-separated, Enter = none): ");
    let mut to_add = Vec::new();
    for p in add.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !p.starts_with('/') {
            a.say(&format!(
                "    {p}: give a full path starting with /; skipped"
            ));
            continue;
        }
        to_add.push(p.to_string());
    }

    a.say("\n4/4 Automatic cleanup. Off unless you turn it on here; each action and");
    a.say("  every folder it may touch needs your approval. Everything it does is alerted.");
    d.ask(a, "auto", "enabled", "Turn on automatic cleanup? (yes/no)");
    if d.get(find("auto", "enabled")).is_some_and(yes) {
        loop {
            d.ask(a, "auto", "start_pct", "Start when a disk is this % full");
            d.ask(
                a,
                "auto",
                "stop_pct",
                "Stop once below this % (0 = free all it may)",
            );
            let start: f64 = d
                .get(find("auto", "start_pct"))
                .unwrap_or("90")
                .parse()
                .unwrap_or(90.0);
            let stop: f64 = d
                .get(find("auto", "stop_pct"))
                .unwrap_or("0")
                .parse()
                .unwrap_or(0.0);
            if stop == 0.0 || stop < start {
                break;
            }
            a.say("    the stop level must be below the start level (or 0)");
        }
        a.say("  Approve each action (yes/no):");
        d.ask(a, "auto", "journal", "Shrink the systemd journal to 1 GiB");
        d.ask(
            a,
            "auto",
            "rotated_logs",
            "Remove rotated log copies (newest 3 kept)",
        );
        d.ask(
            a,
            "auto",
            "truncate_logs",
            "Empty active logs over 50 MiB in place (last 10 MiB kept)",
        );
        d.ask(
            a,
            "auto",
            "build_caches",
            "Remove build output of projects idle 10+ days (alerted 24 h before)",
        );
        d.ask(
            a,
            "auto",
            "docker_images",
            "Remove Docker images no container uses, older than 30 days",
        );
        d.ask(
            a,
            "auto",
            "docker_volumes",
            "Remove Docker volumes no container uses (they may hold databases: say no unless sure)",
        );
        let fl = find("auto", "folders");
        let mut folders: Vec<String> = d
            .get(fl)
            .map(|v| {
                v.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        a.say("  Folders it may touch (nothing outside them is ever changed):");
        for f in suggested_folders() {
            let had = folders.contains(&f);
            let q = format!("    {f} [{}]: ", if had { "yes" } else { "no" });
            let v = a.line(&q);
            if v.is_empty() {
                continue;
            }
            folders.retain(|x| *x != f);
            if yes(&v) {
                folders.push(f);
            }
        }
        let more = a.line("    Other folders (comma-separated full paths, Enter = none): ");
        for f in more.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            if !f.starts_with('/') || f == "/" {
                a.say(&format!("    {f}: a full path below /; skipped"));
            } else if !folders.iter().any(|x| x == f) {
                folders.push(f.to_string());
            }
        }
        if folders.is_empty() {
            d.values.remove(&Draft::id(fl));
        } else {
            d.values.insert(Draft::id(fl), folders.join(","));
        }
        if let Err(e) = crate::auto::Policy::from_raw(&|k| d.raw().get(k).cloned()) {
            a.say(&format!("    {e:#}: automatic cleanup stays off"));
            d.values
                .insert(Draft::id(find("auto", "enabled")), "false".into());
        }
    }

    // Check the whole result before anything is written.
    let raw = d.raw();
    let cfg = crate::notify::NotifyConfig::from_map(raw.clone());
    if let Err(e) = crate::sentinel::Settings::from_config(&cfg) {
        bail!("not written: {e:#}");
    }
    let lits: HashMap<String, String> = SETTINGS
        .iter()
        .filter_map(|st| {
            let v = d.get(st)?;
            let v = if st.kind == Kind::Bool {
                yes(v).to_string()
            } else {
                v.to_string()
            };
            Some((Draft::id(st), config::toml_literal(st, &v)))
        })
        .collect();
    config::write_file(path, &lits)?;
    for p in &to_add {
        match crate::protect::add_to(never_touch, p) {
            Ok(_) => a.say(&format!("  never touch: {p}")),
            Err(e) => a.say(&format!("  {p}: {e:#}")),
        }
    }
    a.say(&format!(
        "\nWrote {} (mode 600). The sentinel applies it within seconds.",
        path.display()
    ));
    let legacy = path.with_file_name("notify.conf");
    if legacy.exists() && path.extension().is_some_and(|e| e == "toml") {
        a.say(&format!(
            "Its settings now include those from {}; that file can be removed.",
            legacy.display()
        ));
    }
    Ok(())
}

/// Folders worth approving (AUTO-3): where logs, container logs and
/// projects usually are on this machine.
pub fn suggested_folders() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: String| {
        if Path::new(&p).is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };
    add("/var/log".into());
    add("/var/lib/docker/containers".into());
    for base in ["/srv", "/opt", "/var/www"] {
        if let Ok(rd) = std::fs::read_dir(base) {
            let mut names: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            names.sort();
            for app in names.into_iter().take(20) {
                for sub in ["logs", "log", "storage/logs"] {
                    add(app.join(sub).to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut homes = vec![PathBuf::from("/root")];
    if let Ok(rd) = std::fs::read_dir("/home") {
        homes.extend(rd.flatten().map(|e| e.path()));
    }
    for h in &homes {
        add(h.join(".pm2/logs").to_string_lossy().into_owned());
    }
    // Projects (for build output).
    for h in homes {
        add(h.to_string_lossy().into_owned());
    }
    add("/srv".into());
    add("/opt".into());
    out
}

pub fn run_terminal(path: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "dux setup asks questions on a terminal. To configure without it, edit {} (every setting is documented in /usr/share/dux/dux.toml.example)",
            path.display()
        );
    }
    run(&mut Terminal, path, Path::new(crate::protect::FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers by prompt text: the first unused (substring, answer) that
    /// matches; Enter otherwise. Records everything shown.
    struct Script(Vec<(&'static str, &'static str)>, Vec<String>);
    impl Ask for Script {
        fn line(&mut self, prompt: &str) -> String {
            self.1.push(prompt.to_string());
            match self.0.iter().position(|(k, _)| prompt.contains(k)) {
                Some(i) => self.0.remove(i).1.to_string(),
                None => String::new(),
            }
        }
        fn say(&mut self, t: &str) {
            self.1.push(t.to_string());
        }
    }

    #[test]
    fn setup_writes_a_valid_file_and_re_runs_from_it() {
        let d = std::env::temp_dir().join(format!("dux-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("dux.toml");
        let nt = d.join("never-touch");
        let mut a = Script(
            vec![
                ("ntfy topic", "ntfy.sh/x"), // not a URL: asked again
                ("ntfy topic", "https://ntfy.sh/x"),
                ("Broadcast", "no"),
                ("test alert", "n"),
                ("Warning when", "96"),
                ("Critical when", "95"), // warn >= crit: both asked again
                ("Warning when", "80"),
                ("Add folders", "/srv/db, relative"),
                ("Turn on automatic", "yes"),
                ("Start when", "90"),
                ("Stop once", "95"), // above start: asked again
                ("Start when", ""),
                ("Stop once", "80"),
                ("rotated log", "yes"),
                ("Docker volumes", "no"),
                ("Other folders", "/srv/app/logs, /"),
            ],
            vec![],
        );
        run(&mut a, &p, &nt).unwrap();
        let out = a.1.join("\n");
        assert!(out.contains("a URL like"), "{out}");
        assert!(out.contains("must be below"), "{out}");
        assert!(out.contains("relative: give a full path"), "{out}");
        let l = config::load(&p).unwrap();
        assert_eq!(l.raw["DUX_NTFY_URL"], "https://ntfy.sh/x");
        assert_eq!(l.raw["DUX_WARN_PCT"], "80");
        assert!(out.contains("stop level must be below"), "{out}");
        assert!(out.contains("/: a full path below /"), "{out}");
        assert_eq!(l.raw["DUX_AUTO"], "true");
        assert_eq!(l.raw["DUX_AUTO_STOP_PCT"], "80");
        assert_eq!(l.raw["DUX_AUTO_ROTATED_LOGS"], "true");
        assert!(l.raw["DUX_AUTO_FOLDERS"]
            .split(',')
            .any(|f| f == "/srv/app/logs"));
        assert!(
            !l.raw.contains_key("DUX_AUTO_DOCKER_VOLUMES")
                || l.raw["DUX_AUTO_DOCKER_VOLUMES"] == "false"
        );
        assert_eq!(l.raw["DUX_WALL"], "false");
        assert!(crate::protect::load_from(&nt)
            .iter()
            .any(|x| x == "/srv/db"));
        // Re-run: Enter everywhere keeps every value; "-" clears one.
        let mut b = Script(vec![("ntfy topic", "-")], vec![]);
        run(&mut b, &p, &nt).unwrap();
        let l = config::load(&p).unwrap();
        assert!(!l.raw.contains_key("DUX_NTFY_URL"), "cleared");
        assert_eq!(l.raw["DUX_WARN_PCT"], "80", "kept");
        assert_eq!(l.raw["DUX_AUTO"], "true", "kept");
        assert!(l.raw["DUX_AUTO_FOLDERS"].contains("/srv/app/logs"), "kept");
        // Turned on with nothing approved: refused, stays off.
        let d2 = d.join("b.toml");
        let mut c = Script(vec![("Turn on automatic", "yes")], vec![]);
        run(&mut c, &d2, &nt).unwrap();
        assert!(c.1.join("\n").contains("automatic cleanup stays off"));
        assert_eq!(config::load(&d2).unwrap().raw["DUX_AUTO"], "false");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
