//! Alert delivery: ntfy, Slack, generic webhook, email, the system log, and an
//! urgent `wall` broadcast to every logged-in terminal.
//!
//! HTTP and SMTP go through the system `curl` binary, so dux gains no new
//! dependencies. Secrets (tokens, webhook URLs, SMTP passwords) are passed to
//! curl as a config file on stdin, never as command-line arguments, so they do
//! not show up in `ps` or `/proc/<pid>/cmdline`.
//!
//! Configuration is a `KEY=VALUE` file (default `/etc/dux/notify.conf`), the
//! same format systemd's `EnvironmentFile=` reads. Environment variables with
//! the same names override the file.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const DEFAULT_CONFIG: &str = "/etc/dux/notify.conf";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A condition cleared.
    Recovered,
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Recovered => "RECOVERED",
            Severity::Info => "INFO",
            Severity::Warning => "WARNING",
            Severity::Critical => "CRITICAL",
        }
    }

    /// ntfy priority: 1 (min) .. 5 (urgent).
    fn ntfy_priority(self) -> u8 {
        match self {
            Severity::Recovered | Severity::Info => 3,
            Severity::Warning => 4,
            Severity::Critical => 5,
        }
    }

    fn ntfy_tags(self) -> &'static str {
        match self {
            Severity::Recovered => "white_check_mark",
            Severity::Info => "information_source",
            Severity::Warning => "warning",
            Severity::Critical => "rotating_light",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s.trim().to_ascii_lowercase().as_str() {
            "recovered" => Some(Severity::Recovered),
            "info" => Some(Severity::Info),
            "warning" | "warn" => Some(Severity::Warning),
            "critical" | "crit" => Some(Severity::Critical),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Alert {
    pub severity: Severity,
    /// One line, e.g. "/ is 96% full".
    pub title: String,
    /// Plain-text detail, a few lines at most.
    pub body: String,
    /// Stable identity for de-duplication, e.g. "space:/".
    pub key: String,
    pub host: String,
}

/// Which channels a send reached, for `notify-test` and the log.
#[derive(Debug, Default)]
pub struct SendReport {
    pub sent: Vec<&'static str>,
    pub failed: Vec<(&'static str, String)>,
    pub skipped: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct NotifyConfig {
    pub ntfy_url: Option<String>,
    pub ntfy_token: Option<String>,
    pub slack_webhook: Option<String>,
    pub webhook_url: Option<String>,
    pub webhook_token: Option<String>,
    pub smtp_url: Option<String>,
    pub smtp_user: Option<String>,
    pub smtp_pass: Option<String>,
    pub mail_from: Option<String>,
    pub mail_to: Vec<String>,
    /// Broadcast critical alerts to all terminals with `wall`.
    pub wall: bool,
    /// Lowest severity each remote channel receives.
    pub min_push: Severity,
    pub min_email: Severity,
    pub curl_bin: PathBuf,
    pub wall_bin: PathBuf,
    pub timeout_secs: u64,
    /// Print what would be sent instead of sending.
    pub dry_run: bool,
    /// Every key read, for the sentinel's own settings.
    pub raw: HashMap<String, String>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig {
            ntfy_url: None,
            ntfy_token: None,
            slack_webhook: None,
            webhook_url: None,
            webhook_token: None,
            smtp_url: None,
            smtp_user: None,
            smtp_pass: None,
            mail_from: None,
            mail_to: Vec::new(),
            wall: false,
            min_push: Severity::Warning,
            min_email: Severity::Warning,
            curl_bin: PathBuf::from("curl"),
            wall_bin: PathBuf::from("wall"),
            timeout_secs: 10,
            dry_run: false,
            raw: HashMap::new(),
        }
    }
}

/// Parse `KEY=VALUE` lines. Accepts `export KEY=VALUE`, `#` comments, blank
/// lines and single- or double-quoted values.
pub fn parse_env_file(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        out.insert(k.to_string(), v.to_string());
    }
    out
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl NotifyConfig {
    /// Load the file (missing is fine) and apply environment overrides.
    pub fn load(path: &Path) -> Result<NotifyConfig> {
        let mut raw = match std::fs::read_to_string(path) {
            Ok(text) => parse_env_file(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        for (k, v) in std::env::vars() {
            if k.starts_with("DUX_") {
                raw.insert(k, v);
            }
        }
        Ok(NotifyConfig::from_map(raw))
    }

    pub fn from_map(raw: HashMap<String, String>) -> NotifyConfig {
        let get = |k: &str| {
            raw.get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let mut c = NotifyConfig {
            ntfy_url: get("DUX_NTFY_URL"),
            ntfy_token: get("DUX_NTFY_TOKEN"),
            slack_webhook: get("DUX_SLACK_WEBHOOK"),
            webhook_url: get("DUX_WEBHOOK_URL"),
            webhook_token: get("DUX_WEBHOOK_TOKEN"),
            smtp_url: get("DUX_SMTP_URL"),
            smtp_user: get("DUX_SMTP_USER"),
            smtp_pass: get("DUX_SMTP_PASS"),
            mail_from: get("DUX_MAIL_FROM"),
            mail_to: get("DUX_MAIL_TO")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            wall: get("DUX_WALL").map(|v| truthy(&v)).unwrap_or(false),
            ..NotifyConfig::default()
        };
        if let Some(s) = get("DUX_MIN_PUSH").and_then(|v| Severity::parse(&v)) {
            c.min_push = s;
        }
        if let Some(s) = get("DUX_MIN_EMAIL").and_then(|v| Severity::parse(&v)) {
            c.min_email = s;
        }
        if let Some(p) = get("DUX_CURL") {
            c.curl_bin = PathBuf::from(p);
        }
        if let Some(p) = get("DUX_WALL_BIN") {
            c.wall_bin = PathBuf::from(p);
        }
        if let Some(t) = get("DUX_NOTIFY_TIMEOUT").and_then(|v| v.parse().ok()) {
            c.timeout_secs = t;
        }
        c.raw = raw;
        c
    }

    pub fn email_enabled(&self) -> bool {
        self.smtp_url.is_some() && self.mail_from.is_some() && !self.mail_to.is_empty()
    }

    /// Names of channels that are configured, for status output.
    pub fn channels(&self) -> Vec<&'static str> {
        let mut v = vec!["log"];
        if self.ntfy_url.is_some() {
            v.push("ntfy");
        }
        if self.slack_webhook.is_some() {
            v.push("slack");
        }
        if self.webhook_url.is_some() {
            v.push("webhook");
        }
        if self.email_enabled() {
            v.push("email");
        }
        if self.wall {
            v.push("wall");
        }
        v
    }
}

/// Quote a value for a curl config file: double quotes, with backslash,
/// quote and control characters escaped as curl's parser expects.
pub fn curl_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Header values must be one line and free of control characters.
fn header_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The text people read: title, detail, host.
pub fn plain_text(a: &Alert) -> String {
    let mut s = format!("[{}] {}", a.severity.label(), a.title);
    if !a.body.is_empty() {
        s.push('\n');
        s.push_str(&a.body);
    }
    s.push_str(&format!("\nhost: {}", a.host));
    s
}

pub fn ntfy_curl_config(c: &NotifyConfig, url: &str, a: &Alert) -> String {
    let mut cfg = String::new();
    cfg.push_str(&format!("url = {}\n", curl_quote(url)));
    cfg.push_str(&format!(
        "header = {}\n",
        curl_quote(&format!(
            "Title: {}",
            header_safe(&format!("dux {}: {}", a.host, a.title))
        ))
    ));
    cfg.push_str(&format!(
        "header = {}\n",
        curl_quote(&format!("Priority: {}", a.severity.ntfy_priority()))
    ));
    cfg.push_str(&format!(
        "header = {}\n",
        curl_quote(&format!("Tags: {}", a.severity.ntfy_tags()))
    ));
    if let Some(t) = &c.ntfy_token {
        cfg.push_str(&format!(
            "header = {}\n",
            curl_quote(&format!("Authorization: Bearer {}", header_safe(t)))
        ));
    }
    let body = if a.body.is_empty() {
        a.title.clone()
    } else {
        a.body.clone()
    };
    cfg.push_str(&format!("data-raw = {}\n", curl_quote(&body)));
    cfg
}

pub fn slack_payload(a: &Alert) -> String {
    let icon = match a.severity {
        Severity::Critical => ":rotating_light:",
        Severity::Warning => ":warning:",
        Severity::Info => ":information_source:",
        Severity::Recovered => ":white_check_mark:",
    };
    let text = format!(
        "{icon} *{}* {} — `{}`\n{}",
        a.severity.label(),
        a.title,
        a.host,
        a.body
    );
    serde_json::json!({ "text": text }).to_string()
}

pub fn webhook_payload(a: &Alert, now: i64) -> String {
    serde_json::json!({
        "source": "dux",
        "severity": a.severity.label().to_ascii_lowercase(),
        "title": a.title,
        "body": a.body,
        "key": a.key,
        "host": a.host,
        "timestamp": now,
    })
    .to_string()
}

fn json_post_config(url: &str, payload: &str, bearer: Option<&str>) -> String {
    let mut cfg = String::new();
    cfg.push_str(&format!("url = {}\n", curl_quote(url)));
    cfg.push_str(&format!(
        "header = {}\n",
        curl_quote("Content-Type: application/json")
    ));
    if let Some(t) = bearer {
        cfg.push_str(&format!(
            "header = {}\n",
            curl_quote(&format!("Authorization: Bearer {}", header_safe(t)))
        ));
    }
    cfg.push_str(&format!("data-raw = {}\n", curl_quote(payload)));
    cfg
}

/// RFC 5322 date from a Unix timestamp, in UTC.
pub fn rfc5322_date(ts: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        d,
        MONTHS[(m - 1) as usize],
        y,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

pub fn email_message(c: &NotifyConfig, a: &Alert, now: i64) -> String {
    let from = c.mail_from.clone().unwrap_or_default();
    let subject = header_safe(&format!(
        "[dux {}] {} on {}",
        a.severity.label(),
        a.title,
        a.host
    ));
    let mut m = String::new();
    m.push_str(&format!("From: {}\r\n", header_safe(&from)));
    m.push_str(&format!("To: {}\r\n", header_safe(&c.mail_to.join(", "))));
    m.push_str(&format!("Subject: {subject}\r\n"));
    m.push_str(&format!("Date: {}\r\n", rfc5322_date(now)));
    m.push_str("MIME-Version: 1.0\r\n");
    m.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    m.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    for line in plain_text(a).lines() {
        // SMTP dot-stuffing is done by curl; keep lines short and CRLF-terminated.
        m.push_str(line);
        m.push_str("\r\n");
    }
    m
}

pub fn email_curl_config(c: &NotifyConfig, message_path: &Path) -> String {
    let mut cfg = String::new();
    cfg.push_str(&format!(
        "url = {}\n",
        curl_quote(c.smtp_url.as_deref().unwrap_or(""))
    ));
    cfg.push_str(&format!(
        "mail-from = {}\n",
        curl_quote(c.mail_from.as_deref().unwrap_or(""))
    ));
    for to in &c.mail_to {
        cfg.push_str(&format!("mail-rcpt = {}\n", curl_quote(to)));
    }
    if let Some(u) = &c.smtp_user {
        let pass = c.smtp_pass.clone().unwrap_or_default();
        cfg.push_str(&format!("user = {}\n", curl_quote(&format!("{u}:{pass}"))));
    }
    if c.smtp_url
        .as_deref()
        .is_some_and(|u| u.starts_with("smtp://"))
    {
        // Plain smtp:// upgrades with STARTTLS and refuses to send in clear text.
        cfg.push_str("ssl-reqd\n");
    }
    cfg.push_str(&format!(
        "upload-file = {}\n",
        curl_quote(&message_path.to_string_lossy())
    ));
    cfg
}

/// Text for `wall`. Short: terminals show it as is.
pub fn wall_text(a: &Alert) -> String {
    let mut s = format!("dux {} on {}: {}\n", a.severity.label(), a.host, a.title);
    for line in a.body.lines().take(6) {
        s.push_str("  ");
        s.push_str(line);
        s.push('\n');
    }
    s
}

fn run_curl(c: &NotifyConfig, config_text: &str) -> Result<()> {
    let mut child = Command::new(&c.curl_bin)
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            &c.timeout_secs.to_string(),
            "--connect-timeout",
            "5",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", c.curl_bin.display()))?;
    child
        .stdin
        .take()
        .context("curl stdin")?
        .write_all(config_text.as_bytes())?;
    let out = child.wait_with_output()?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("curl failed ({}): {}", out.status, err)
    }
}

fn run_wall(c: &NotifyConfig, text: &str) -> Result<()> {
    let mut child = Command::new(&c.wall_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", c.wall_bin.display()))?;
    child
        .stdin
        .take()
        .context("wall stdin")?
        .write_all(text.as_bytes())?;
    let out = child.wait_with_output()?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "wall failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

fn send_email(c: &NotifyConfig, a: &Alert, now: i64) -> Result<()> {
    // The message itself is not secret; write it to a private temp file so
    // curl's stdin stays free for the config that carries the password.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("dux-mail-{}-{}.eml", std::process::id(), now));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.write_all(email_message(c, a, now).as_bytes())?;
    }
    let result = run_curl(c, &email_curl_config(c, &path));
    let _ = std::fs::remove_file(&path);
    result
}

/// Send one alert to every configured channel the severity qualifies for.
/// `allow_wall` lets the caller rate-limit wall separately.
pub fn send(c: &NotifyConfig, a: &Alert, allow_wall: bool, now: i64) -> SendReport {
    let mut r = SendReport::default();

    // The system log always gets it (journald under systemd).
    match a.severity {
        Severity::Critical => {
            tracing::error!(key = %a.key, "{}", plain_text(a).replace('\n', " | "))
        }
        Severity::Warning => tracing::warn!(key = %a.key, "{}", plain_text(a).replace('\n', " | ")),
        _ => tracing::info!(key = %a.key, "{}", plain_text(a).replace('\n', " | ")),
    }
    r.sent.push("log");

    let push = a.severity >= c.min_push || a.severity == Severity::Recovered;
    let mut attempt = |name: &'static str, enabled: bool, f: &mut dyn FnMut() -> Result<()>| {
        if !enabled {
            return;
        }
        if c.dry_run {
            r.skipped.push(name);
            return;
        }
        match f() {
            Ok(()) => r.sent.push(name),
            Err(e) => {
                tracing::warn!(channel = name, "alert delivery failed: {e:#}");
                r.failed.push((name, format!("{e:#}")));
            }
        }
    };

    // Local channels first, so a slow network endpoint can't delay them.
    attempt(
        "wall",
        c.wall && allow_wall && a.severity == Severity::Critical,
        &mut || run_wall(c, &wall_text(a)),
    );
    if let Some(url) = c.ntfy_url.clone() {
        attempt("ntfy", push, &mut || {
            run_curl(c, &ntfy_curl_config(c, &url, a))
        });
    }
    if let Some(url) = c.slack_webhook.clone() {
        attempt("slack", push, &mut || {
            run_curl(c, &json_post_config(&url, &slack_payload(a), None))
        });
    }
    if let Some(url) = c.webhook_url.clone() {
        let token = c.webhook_token.clone();
        attempt("webhook", push, &mut || {
            run_curl(
                c,
                &json_post_config(&url, &webhook_payload(a, now), token.as_deref()),
            )
        });
    }
    let email =
        c.email_enabled() && (a.severity >= c.min_email || a.severity == Severity::Recovered);
    attempt("email", email, &mut || send_email(c, a, now));
    r
}

/// Send without blocking the caller; delivery can take up to the timeout per channel.
pub fn send_async(c: &NotifyConfig, a: Alert, allow_wall: bool, now: i64) {
    let c = c.clone();
    let _ = std::thread::Builder::new()
        .name("dux-notify".into())
        .spawn(move || {
            let _ = send(&c, &a, allow_wall, now);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(sev: Severity) -> Alert {
        Alert {
            severity: sev,
            title: "/ is 96% full".into(),
            body: "used 177 GiB of 185 GiB\nfull in about 3 h".into(),
            key: "space:/".into(),
            host: "vps1".into(),
        }
    }

    #[test]
    fn env_file_parsing_handles_quotes_comments_and_export() {
        let m = parse_env_file(
            "# comment\n\nDUX_NTFY_URL=https://ntfy.sh/x\nexport DUX_WALL=yes\nDUX_MAIL_TO=\"a@x, b@y\"\nDUX_SLACK_WEBHOOK='https://hooks.slack.com/s'\nbad line\n9X=1\n",
        );
        assert_eq!(m["DUX_NTFY_URL"], "https://ntfy.sh/x");
        assert_eq!(m["DUX_WALL"], "yes");
        assert_eq!(m["DUX_MAIL_TO"], "a@x, b@y");
        assert_eq!(m["DUX_SLACK_WEBHOOK"], "https://hooks.slack.com/s");
        assert!(!m.contains_key("bad line"));
        let c = NotifyConfig::from_map(m);
        assert!(c.wall);
        assert_eq!(c.mail_to, vec!["a@x", "b@y"]);
        assert!(!c.email_enabled(), "email needs a server and a sender too");
        assert_eq!(c.channels(), vec!["log", "ntfy", "slack", "wall"]);
    }

    #[test]
    fn curl_quoting_escapes_everything_curl_parses() {
        assert_eq!(curl_quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(curl_quote("x\ny\tz"), r#""x\ny\tz""#);
        assert_eq!(curl_quote("bell\u{7}"), r#""bell""#);
    }

    #[test]
    fn ntfy_config_carries_priority_and_keeps_token_off_argv() {
        let c = NotifyConfig {
            ntfy_token: Some("tk_secret\nInjected: x".into()),
            ..NotifyConfig::default()
        };
        let cfg = ntfy_curl_config(&c, "https://ntfy.sh/topic", &alert(Severity::Critical));
        assert!(cfg.contains("Priority: 5"));
        assert!(cfg.contains("Tags: rotating_light"));
        assert!(
            cfg.contains("Authorization: Bearer tk_secret Injected: x"),
            "newline in a token must not start a new header"
        );
        assert_eq!(cfg.matches("header = ").count(), 4);
    }

    #[test]
    fn json_payloads_are_valid_json() {
        let a = alert(Severity::Warning);
        let s: serde_json::Value = serde_json::from_str(&slack_payload(&a)).unwrap();
        assert!(s["text"].as_str().unwrap().contains("WARNING"));
        let w: serde_json::Value =
            serde_json::from_str(&webhook_payload(&a, 1_795_000_000)).unwrap();
        assert_eq!(w["severity"], "warning");
        assert_eq!(w["key"], "space:/");
        assert_eq!(w["timestamp"], 1_795_000_000);
    }

    #[test]
    fn rfc5322_dates() {
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(rfc5322_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
        assert_eq!(
            rfc5322_date(1_795_713_742),
            "Thu, 26 Nov 2026 17:22:22 +0000"
        );
    }

    #[test]
    fn email_message_and_config() {
        let c = NotifyConfig {
            smtp_url: Some("smtp://mail.example.com:587".into()),
            smtp_user: Some("dux".into()),
            smtp_pass: Some("p\"w".into()),
            mail_from: Some("dux@example.com".into()),
            mail_to: vec!["ops@example.com".into(), "me@example.com".into()],
            ..NotifyConfig::default()
        };
        assert!(c.email_enabled());
        let m = email_message(&c, &alert(Severity::Critical), 0);
        assert!(m.starts_with("From: dux@example.com\r\nTo: ops@example.com, me@example.com\r\n"));
        assert!(m.contains("Subject: [dux CRITICAL] / is 96% full on vps1\r\n"));
        assert!(m.contains("\r\n\r\n[CRITICAL] / is 96% full\r\n"));
        let cfg = email_curl_config(&c, Path::new("/tmp/m.eml"));
        assert!(cfg.contains("mail-rcpt = \"ops@example.com\"\nmail-rcpt = \"me@example.com\"\n"));
        assert!(cfg.contains(r#"user = "dux:p\"w""#));
        assert!(cfg.contains("ssl-reqd\n"));
    }

    #[test]
    fn wall_text_is_short() {
        let mut a = alert(Severity::Critical);
        a.body = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let t = wall_text(&a);
        assert_eq!(t.lines().count(), 7);
        assert!(t.starts_with("dux CRITICAL on vps1: / is 96% full\n"));
    }

    #[test]
    fn dry_run_and_severity_gating() {
        let c = NotifyConfig {
            dry_run: true,
            ntfy_url: Some("https://ntfy.sh/x".into()),
            wall: true,
            ..NotifyConfig::default()
        };
        let r = send(&c, &alert(Severity::Critical), true, 0);
        assert_eq!(r.sent, vec!["log"]);
        assert_eq!(r.skipped, vec!["wall", "ntfy"]);
        // Info stays in the log and does not reach push channels or wall.
        let r = send(&c, &alert(Severity::Info), true, 0);
        assert!(r.skipped.is_empty());
        // Wall is withheld when the caller's rate limit says no.
        let r = send(&c, &alert(Severity::Critical), false, 0);
        assert_eq!(r.skipped, vec!["ntfy"]);
    }

    #[test]
    fn real_delivery_through_a_fake_curl_and_wall() {
        let dir = std::env::temp_dir().join(format!("dux-notify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls.log");
        let script = |name: &str| {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!(
                    "#!/bin/sh\n{{ echo \"== {name} $*\"; cat; }} >> {}\n",
                    log.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let c = NotifyConfig {
            curl_bin: script("curl"),
            wall_bin: script("wall"),
            ntfy_url: Some("https://ntfy.sh/topic".into()),
            slack_webhook: Some("https://hooks.slack.com/services/T/B/secret".into()),
            wall: true,
            ..NotifyConfig::default()
        };
        let r = send(&c, &alert(Severity::Critical), true, 0);
        assert_eq!(
            r.sent,
            vec!["log", "wall", "ntfy", "slack"],
            "{:?}",
            r.failed
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            !calls
                .lines()
                .any(|l| l.starts_with("== curl") && l.contains("secret")),
            "secret on argv: {calls}"
        );
        assert!(
            calls.contains("hooks.slack.com/services/T/B/secret"),
            "secret should arrive on stdin"
        );
        assert!(calls.contains("dux CRITICAL on vps1: / is 96% full"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
