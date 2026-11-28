//! `dux setup` (spec CFG-1): a guided way to write `/etc/dux/dux.toml`.
//!
//! Asks for alert channels (and sends a test), thresholds, never-touch
//! folders and automatic cleanup. Every answer defaults to the current value,
//! so it can be re-run at any time; Enter keeps a value, `-` clears it.

use crate::config::{self, Kind, Setting, SETTINGS};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

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

    a.say("\n4/4 Automatic cleanup on a critical disk (needs your approval here).");
    a.say("  0 = off (dux only alerts and suggests), 1 = shrink the journal,");
    a.say("  2 = + remove rotated logs, 3 = + package caches and temp files.");
    loop {
        d.ask(a, "cleanup", "auto_level", "Level");
        match d
            .get(find("cleanup", "auto_level"))
            .unwrap_or("0")
            .parse::<u8>()
        {
            Ok(0..=3) => break,
            _ => a.say("    0, 1, 2 or 3"),
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

    struct Script(Vec<&'static str>, Vec<String>);
    impl Ask for Script {
        fn line(&mut self, prompt: &str) -> String {
            self.1.push(prompt.to_string());
            if self.0.is_empty() {
                String::new()
            } else {
                self.0.remove(0).to_string()
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
                "ntfy.sh/x", // not a URL: asked again
                "https://ntfy.sh/x",
                "",   // token
                "",   // slack
                "",   // webhook
                "",   // smtp
                "no", // broadcast
                "n",  // no test send
                "96",
                "95", // warn >= crit: both asked again
                "80",
                "",
                "/srv/db, relative", // never-touch
                "5",
                "1", // auto level
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
        assert_eq!(l.raw["DUX_AUTO_CLEANUP_LEVEL"], "1");
        assert_eq!(l.raw["DUX_WALL"], "false");
        assert!(crate::protect::load_from(&nt)
            .iter()
            .any(|x| x == "/srv/db"));
        // Re-run: Enter everywhere keeps every value; "-" clears one.
        let mut b = Script(vec!["-"], vec![]);
        run(&mut b, &p, &nt).unwrap();
        let l = config::load(&p).unwrap();
        assert!(!l.raw.contains_key("DUX_NTFY_URL"), "cleared");
        assert_eq!(l.raw["DUX_WARN_PCT"], "80", "kept");
        assert_eq!(l.raw["DUX_AUTO_CLEANUP_LEVEL"], "1", "kept");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
