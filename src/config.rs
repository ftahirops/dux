//! The configuration file (spec CFG-2, CFG-3, A15).
//!
//! One TOML file, `/etc/dux/dux.toml`, with documented sections. Each setting
//! maps onto the `DUX_*` key the rest of dux reads, so the old
//! `/etc/dux/notify.conf` (KEY=VALUE) keeps working: it is read first, the TOML
//! file overrides it, and `DUX_*` environment variables override both.
//!
//! Every error names the file, the line and the reason. A bad edit never takes
//! a running sentinel down: it keeps the last good configuration.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_FILE: &str = "/etc/dux/dux.toml";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Secret,
    Bool,
    Number,
    Percent,
    Duration,
    Size,
    List,
    Severity,
}

pub struct Setting {
    pub section: &'static str,
    pub key: &'static str,
    pub env: &'static str,
    pub kind: Kind,
    /// Default as written in the file ("" = not set).
    pub default: &'static str,
    pub doc: &'static str,
}

const fn s(
    section: &'static str,
    key: &'static str,
    env: &'static str,
    kind: Kind,
    default: &'static str,
    doc: &'static str,
) -> Setting {
    Setting {
        section,
        key,
        env,
        kind,
        default,
        doc,
    }
}

use Kind::*;

/// Every setting dux reads, in file order. `dux.toml.example` and
/// `dux setup` are both built from this table.
pub const SETTINGS: &[Setting] = &[
    // [alerts]
    s(
        "alerts",
        "ntfy_url",
        "DUX_NTFY_URL",
        Text,
        "",
        "ntfy topic URL, e.g. https://ntfy.sh/my-servers",
    ),
    s(
        "alerts",
        "ntfy_token",
        "DUX_NTFY_TOKEN",
        Secret,
        "",
        "ntfy access token (optional)",
    ),
    s(
        "alerts",
        "slack_webhook",
        "DUX_SLACK_WEBHOOK",
        Secret,
        "",
        "Slack incoming-webhook URL",
    ),
    s(
        "alerts",
        "webhook_url",
        "DUX_WEBHOOK_URL",
        Text,
        "",
        "Generic webhook: dux POSTs the alert as JSON",
    ),
    s(
        "alerts",
        "webhook_token",
        "DUX_WEBHOOK_TOKEN",
        Secret,
        "",
        "Bearer token sent to the webhook (optional)",
    ),
    s(
        "alerts",
        "smtp_url",
        "DUX_SMTP_URL",
        Text,
        "",
        "Mail server, e.g. smtps://smtp.example.com:465 (TLS required)",
    ),
    s(
        "alerts",
        "smtp_user",
        "DUX_SMTP_USER",
        Text,
        "",
        "Mail server user",
    ),
    s(
        "alerts",
        "smtp_pass",
        "DUX_SMTP_PASS",
        Secret,
        "",
        "Mail server password",
    ),
    s(
        "alerts",
        "mail_from",
        "DUX_MAIL_FROM",
        Text,
        "",
        "Sender address",
    ),
    s("alerts", "mail_to", "DUX_MAIL_TO", List, "[]", "Recipients"),
    s(
        "alerts",
        "min_push",
        "DUX_MIN_PUSH",
        Severity,
        "warning",
        "Lowest severity sent to ntfy, Slack and the webhook: info, warning or critical",
    ),
    s(
        "alerts",
        "min_email",
        "DUX_MIN_EMAIL",
        Severity,
        "warning",
        "Lowest severity sent by email",
    ),
    s(
        "alerts",
        "timeout",
        "DUX_NOTIFY_TIMEOUT",
        Number,
        "10",
        "Seconds to wait for each channel",
    ),
    s(
        "alerts",
        "max_per_hour",
        "DUX_MAX_ALERTS_PER_HOUR",
        Number,
        "30",
        "Alerts per hour at most; more are summarised",
    ),
    // [disk]
    s(
        "disk",
        "warn_pct",
        "DUX_WARN_PCT",
        Percent,
        "85",
        "Warning when a disk is this full",
    ),
    s(
        "disk",
        "crit_pct",
        "DUX_CRIT_PCT",
        Percent,
        "95",
        "Critical when a disk is this full",
    ),
    s(
        "disk",
        "inode_warn_pct",
        "DUX_INODE_WARN_PCT",
        Percent,
        "85",
        "Warning when this share of inodes is used",
    ),
    s(
        "disk",
        "inode_crit_pct",
        "DUX_INODE_CRIT_PCT",
        Percent,
        "95",
        "Critical when this share of inodes is used",
    ),
    s(
        "disk",
        "hysteresis_pct",
        "DUX_HYSTERESIS_PCT",
        Percent,
        "3",
        "A level clears only this many points below it",
    ),
    s(
        "disk",
        "check_every",
        "DUX_SENTINEL_INTERVAL",
        Duration,
        "60s",
        "Check each healthy disk this often",
    ),
    s(
        "disk",
        "check_every_when_warning",
        "DUX_SENTINEL_FAST_INTERVAL",
        Duration,
        "5s",
        "Check a disk past its warning level this often",
    ),
    s(
        "disk",
        "mounts",
        "DUX_MOUNTS",
        List,
        "[]",
        "Only watch these mount points (empty = all real filesystems)",
    ),
    s(
        "disk",
        "repeat_warning",
        "DUX_WARN_REPEAT",
        Duration,
        "0",
        "Remind about a warning this often (0 = never)",
    ),
    s(
        "disk",
        "repeat_critical",
        "DUX_CRIT_REPEAT",
        Duration,
        "0",
        "Remind about a critical disk this often (0 = never)",
    ),
    s(
        "disk",
        "worse_by_pct",
        "DUX_WORSEN_PCT",
        Number,
        "5",
        "Alert again when this many points worse",
    ),
    s(
        "disk",
        "worse_min_gap",
        "DUX_WORSEN_MIN_GAP",
        Duration,
        "1h",
        "...but at most this often",
    ),
    s(
        "disk",
        "ease_after",
        "DUX_EASE_AFTER",
        Duration,
        "2m",
        "A lower level must hold this long before it is reported",
    ),
    // [growth]
    s(
        "growth",
        "fill_alerts",
        "DUX_FILL_ALERTS",
        Bool,
        "false",
        "Alert when a disk is projected to fill",
    ),
    s(
        "growth",
        "fill_warn_hours",
        "DUX_FILL_WARN_HOURS",
        Number,
        "24",
        "Warning when full within this many hours",
    ),
    s(
        "growth",
        "fill_crit_hours",
        "DUX_FILL_CRIT_HOURS",
        Number,
        "2",
        "Critical when full within this many hours",
    ),
    s(
        "growth",
        "fill_warn_history",
        "DUX_FILL_WARN_HISTORY",
        Duration,
        "5m",
        "History needed before a fill warning",
    ),
    s(
        "growth",
        "fill_crit_history",
        "DUX_FILL_CRIT_HISTORY",
        Duration,
        "3m",
        "History needed before a fill critical",
    ),
    s(
        "growth",
        "burst_alerts",
        "DUX_BURST_ALERTS",
        Bool,
        "false",
        "Alert on a sudden burst of writes",
    ),
    s(
        "growth",
        "burst_size",
        "DUX_BURST",
        Size,
        "5G",
        "A burst is this much written...",
    ),
    s(
        "growth",
        "burst_window",
        "DUX_BURST_WINDOW",
        Duration,
        "10m",
        "...within this time",
    ),
    // [broadcast]
    s(
        "broadcast",
        "enabled",
        "DUX_WALL",
        Bool,
        "false",
        "Broadcast critical alerts to every logged-in terminal (wall)",
    ),
    s(
        "broadcast",
        "at_pct",
        "DUX_WALL_PCT",
        Percent,
        "97",
        "Only for a disk at least this full",
    ),
    s(
        "broadcast",
        "min_gap",
        "DUX_WALL_MIN_GAP",
        Duration,
        "30m",
        "At most once per disk this often",
    ),
    s(
        "broadcast",
        "max_per_hour",
        "DUX_WALL_MAX_PER_HOUR",
        Number,
        "4",
        "At most this many broadcasts per hour",
    ),
    // [attribution]
    s(
        "attribution",
        "enabled",
        "DUX_WALK",
        Bool,
        "true",
        "Walk a growing or full disk to find which folders grew",
    ),
    s(
        "attribution",
        "every",
        "DUX_WALK_EVERY",
        Duration,
        "1h",
        "Baseline walk of each disk this often",
    ),
    s(
        "attribution",
        "min_gap",
        "DUX_WALK_MIN_GAP",
        Duration,
        "10m",
        "Never walk one disk more often than this",
    ),
    s(
        "attribution",
        "trigger",
        "DUX_WALK_TRIGGER",
        Size,
        "1G",
        "Walk when a disk grew this much since the last walk",
    ),
    s(
        "attribution",
        "threads",
        "DUX_WALK_THREADS",
        Number,
        "2",
        "Walker threads (1-16)",
    ),
    s(
        "attribution",
        "snapshot_dir",
        "DUX_SNAPSHOT_DIR",
        Text,
        "/var/lib/dux/sentinel",
        "Where folder snapshots and history are kept",
    ),
    // [auto] automatic cleanup (spec 4.5): off unless turned on here or in
    // `dux setup`, with the folders it may touch and each kind of action approved.
    s(
        "auto",
        "enabled",
        "DUX_AUTO",
        Bool,
        "false",
        "Clean up automatically when a disk reaches start_pct",
    ),
    s(
        "auto",
        "start_pct",
        "DUX_AUTO_START_PCT",
        Percent,
        "90",
        "Start when a disk is this full",
    ),
    s(
        "auto",
        "stop_pct",
        "DUX_AUTO_STOP_PCT",
        Percent,
        "0",
        "Stop once below this (0 = free as much as the approved actions allow)",
    ),
    s(
        "auto",
        "folders",
        "DUX_AUTO_FOLDERS",
        List,
        "[]",
        "The only folders it may touch, e.g. /var/log, /srv/app/logs",
    ),
    s(
        "auto",
        "journal",
        "DUX_AUTO_JOURNAL",
        Bool,
        "false",
        "Shrink the systemd journal to 1 GiB (needs /var/log approved)",
    ),
    s(
        "auto",
        "rotated_logs",
        "DUX_AUTO_ROTATED_LOGS",
        Bool,
        "false",
        "Remove rotated log copies, keeping the newest 3",
    ),
    s(
        "auto",
        "truncate_logs",
        "DUX_AUTO_TRUNCATE_LOGS",
        Bool,
        "false",
        "Empty active logs over 50 MiB in place, keeping their last 10 MiB",
    ),
    s(
        "auto",
        "build_caches",
        "DUX_AUTO_BUILD_CACHES",
        Bool,
        "false",
        "Remove build output of projects idle 10+ days (announced 24 h before)",
    ),
    s(
        "auto",
        "docker_images",
        "DUX_AUTO_DOCKER_IMAGES",
        Bool,
        "false",
        "Remove Docker images no container uses, older than 30 days",
    ),
    s(
        "auto",
        "docker_volumes",
        "DUX_AUTO_DOCKER_VOLUMES",
        Bool,
        "false",
        "Remove Docker volumes no container uses (they may hold data)",
    ),
    // [ui]
    s(
        "ui",
        "refresh",
        "DUX_UI_REFRESH",
        Duration,
        "5s",
        "How often the Activity view refreshes (1s-60s); F6 freezes it",
    ),
    // [programs]
    s(
        "programs",
        "curl",
        "DUX_CURL",
        Text,
        "curl",
        "curl used for HTTP and mail",
    ),
    s(
        "programs",
        "wall",
        "DUX_WALL_BIN",
        Text,
        "wall",
        "wall used for broadcasts",
    ),
];

/// Where a value came from, for error messages.
#[derive(Clone, Debug, Default)]
pub struct Origin {
    pub file: PathBuf,
    pub line: usize,
}

/// The merged `DUX_*` keys plus where each came from.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    /// The file asked for (`--config`).
    pub path: PathBuf,
    pub raw: HashMap<String, String>,
    pub origin: HashMap<String, Origin>,
    /// Files read, for change detection.
    pub files: Vec<PathBuf>,
}

impl Loaded {
    /// Rewrite `DUX_X` names in an error into `[section] key (file:line)`.
    pub fn explain(&self, err: &anyhow::Error) -> String {
        let mut msg = format!("{err:#}");
        // Longest names first so DUX_WALL_PCT is not taken for DUX_WALL.
        let mut all: Vec<&Setting> = SETTINGS.iter().collect();
        all.sort_by_key(|s| std::cmp::Reverse(s.env.len()));
        for st in all {
            if !msg.contains(st.env) {
                continue;
            }
            let at = match self.origin.get(st.env) {
                Some(o) if o.line > 0 => format!(" ({}:{})", o.file.display(), o.line),
                Some(o) => format!(" ({})", o.file.display()),
                None => String::new(),
            };
            let name = format!("\u{1}{}.{}{at}", st.section, st.key);
            msg = msg.replace(st.env, &name);
        }
        msg.replace('\u{1}', "")
    }
}

/// Load `path`. A `.toml` path also reads the old notify.conf beside it
/// first; any other path is read as the old KEY=VALUE format. Missing files
/// are fine (built-in defaults). Environment `DUX_*` overrides everything.
pub fn load(path: &Path) -> Result<Loaded> {
    let mut l = Loaded {
        path: path.to_path_buf(),
        ..Loaded::default()
    };
    let is_toml = path.extension().is_some_and(|e| e == "toml");
    let legacy = if is_toml {
        path.with_file_name("notify.conf")
    } else {
        path.to_path_buf()
    };
    if let Some(text) = read_optional(&legacy)? {
        for (n, line) in text.lines().enumerate() {
            let map = crate::notify::parse_env_file(line);
            for (k, v) in map {
                l.origin.insert(
                    k.clone(),
                    Origin {
                        file: legacy.clone(),
                        line: n + 1,
                    },
                );
                l.raw.insert(k, v);
            }
        }
    }
    // Both files are watched, present or not, so creating one is noticed.
    l.files.push(legacy.clone());
    if is_toml {
        if let Some(text) = read_optional(path)? {
            parse_toml(&text, path, &mut l)?;
            warn_if_readable(path, &l);
        }
        l.files.push(path.to_path_buf());
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("DUX_") {
            l.origin.insert(
                k.clone(),
                Origin {
                    file: "environment".into(),
                    line: 0,
                },
            );
            l.raw.insert(k, v);
        }
    }
    Ok(l)
}

fn read_optional(p: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

/// Secrets in a file others can read: say so in the system log.
fn warn_if_readable(p: &Path, l: &Loaded) {
    use std::os::unix::fs::PermissionsExt;
    let has_secret = SETTINGS
        .iter()
        .any(|s| s.kind == Secret && l.raw.get(s.env).is_some_and(|v| !v.is_empty()));
    if let Ok(m) = std::fs::metadata(p) {
        if has_secret && m.permissions().mode() & 0o044 != 0 {
            tracing::warn!(
                "{} holds secrets but others can read it; run: chmod 600 {}",
                p.display(),
                p.display()
            );
        }
    }
}

/// The line of `key` inside `[section]` (1-based), or of the section header.
pub fn line_of(text: &str, section: &str, key: Option<&str>) -> usize {
    let mut in_sec = false;
    let mut header = 0;
    for (n, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if t.starts_with('[') {
            in_sec = t.trim_start_matches('[').trim_end_matches(']').trim() == section;
            if in_sec {
                header = n + 1;
            }
            continue;
        }
        if in_sec {
            if let Some(k) = key {
                let name = t.split('=').next().unwrap_or("").trim().trim_matches('"');
                if t.contains('=') && name == k {
                    return n + 1;
                }
            }
        }
    }
    header
}

fn parse_toml(text: &str, path: &Path, l: &mut Loaded) -> Result<()> {
    // Only the file, line and reason: the parser's own message quotes the
    // offending line, which may hold a token or password.
    let table: toml::Table = toml::from_str(text).map_err(|e| {
        let line = e
            .span()
            .map(|sp| text[..sp.start.min(text.len())].matches('\n').count() + 1)
            .unwrap_or(0);
        anyhow::anyhow!("{}:{line}: {}", path.display(), e.message().trim_end())
    })?;
    for (section, v) in &table {
        let Some(t) = v.as_table() else {
            bail!(
                "{}:{}: `{section}` must be a [section]; settings go inside one, e.g. [disk] warn_pct = 85",
                path.display(),
                line_of(text, section, None).max(1)
            );
        };
        if !SETTINGS.iter().any(|s| s.section == section) {
            bail!(
                "{}:{}: unknown section [{section}]; known: {}",
                path.display(),
                line_of(text, section, None),
                sections().join(", ")
            );
        }
        for (key, val) in t {
            let line = line_of(text, section, Some(key));
            let Some(st) = SETTINGS
                .iter()
                .find(|s| s.section == section && s.key == key)
            else {
                let known: Vec<&str> = SETTINGS
                    .iter()
                    .filter(|s| s.section == section)
                    .map(|s| s.key)
                    .collect();
                bail!(
                    "{}:{line}: unknown setting `{key}` in [{section}]; known: {}",
                    path.display(),
                    known.join(", ")
                );
            };
            let value = to_text(st, val).map_err(|why| {
                anyhow::anyhow!("{}:{line}: [{section}] {key}: {why}", path.display())
            })?;
            l.origin.insert(
                st.env.to_string(),
                Origin {
                    file: path.to_path_buf(),
                    line,
                },
            );
            l.raw.insert(st.env.to_string(), value);
        }
    }
    Ok(())
}

/// A TOML value as the text the `DUX_*` readers expect.
fn to_text(st: &Setting, v: &toml::Value) -> std::result::Result<String, String> {
    use toml::Value as V;
    let want = |what: &str| Err(format!("expected {what}, got {}", v.type_str()));
    match (st.kind, v) {
        (List, V::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or("list items must be text".to_string())
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(|x| x.join(",")),
        (List, V::String(s)) => Ok(s.clone()),
        (List, _) => want("a list like [\"a\", \"b\"]"),
        (Bool, V::Boolean(b)) => Ok(b.to_string()),
        (Bool, _) => want("true or false"),
        (Number | Percent, V::Integer(i)) => Ok(i.to_string()),
        (Number | Percent, V::Float(f)) => Ok(f.to_string()),
        (Number | Percent, _) => want("a number"),
        (Duration, V::Integer(i)) => Ok(format!("{i}s")),
        (Duration, V::String(s)) => crate::util::parse_duration(s)
            .map(|_| s.clone())
            .map_err(|_| format!("`{s}` is not a duration like 30s, 5m, 1h")),
        (Duration, _) => want("a duration like \"5m\" or seconds"),
        (Size, V::Integer(i)) if *i >= 0 => Ok(i.to_string()),
        (Size, V::String(s)) => crate::sentinel::parse_bytes(s)
            .map(|_| s.clone())
            .map_err(|_| format!("`{s}` is not a size like 500M or 2G")),
        (Size, _) => want("a size like \"2G\""),
        (Severity, V::String(s)) if crate::notify::Severity::parse(s).is_some() => Ok(s.clone()),
        (Severity, _) => want("info, warning or critical"),
        (Text | Secret, V::String(s)) => Ok(s.clone()),
        (Text | Secret, _) => want("text in quotes"),
    }
}

pub fn sections() -> Vec<&'static str> {
    let mut out: Vec<&str> = Vec::new();
    for s in SETTINGS {
        if !out.contains(&s.section) {
            out.push(s.section);
        }
    }
    out
}

/// What a TOML line for this value looks like.
pub fn toml_literal(st: &Setting, v: &str) -> String {
    match st.kind {
        Bool | Number | Percent => v.to_string(),
        List => {
            let items: Vec<String> = v
                .trim_matches(['[', ']'])
                .split(',')
                .map(|x| x.trim().trim_matches('"'))
                .filter(|x| !x.is_empty())
                .map(|x| format!("{x:?}"))
                .collect();
            format!("[{}]", items.join(", "))
        }
        _ => format!("{v:?}"),
    }
}

/// The documented example file: every setting, commented out at its default.
pub fn example() -> String {
    let mut out = String::from(
        "# dux configuration. Every setting is shown at its default, commented out.\n\
         # Changes apply within a few seconds; no restart needed. A mistake is\n\
         # reported in the system log with its line, and dux keeps the last good\n\
         # settings. The old /etc/dux/notify.conf is still read; this file wins.\n\
         # Keep this file private if it holds tokens or passwords: chmod 600.\n",
    );
    for sec in sections() {
        out.push_str(&format!("\n[{sec}]\n"));
        for st in SETTINGS.iter().filter(|s| s.section == sec) {
            out.push_str(&format!(
                "# {}\n#{} = {}\n",
                st.doc,
                st.key,
                toml_literal(st, st.default)
            ));
        }
    }
    out
}

/// Write `values` (section.key -> TOML literal) as a complete file, keeping
/// every other setting commented at its default. Mode 0600, atomic rename.
pub fn write_file(path: &Path, values: &HashMap<String, String>) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut out = String::from(
        "# dux configuration, written by `dux setup` (re-run it any time, or edit\n\
         # by hand: changes apply within a few seconds, no restart needed).\n",
    );
    for sec in sections() {
        out.push_str(&format!("\n[{sec}]\n"));
        for st in SETTINGS.iter().filter(|s| s.section == sec) {
            let id = format!("{}.{}", st.section, st.key);
            match values.get(&id) {
                Some(v) => out.push_str(&format!("# {}\n{} = {v}\n", st.doc, st.key)),
                None => out.push_str(&format!(
                    "# {}\n#{} = {}\n",
                    st.doc,
                    st.key,
                    toml_literal(st, st.default)
                )),
            }
        }
    }
    // Never write a file dux itself could not read back.
    let mut check = Loaded::default();
    parse_toml(&out, path, &mut check).context("internal: generated config does not parse")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.dux-new");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Modification stamp of the config files, to notice edits.
pub fn stamp(files: &[PathBuf]) -> Vec<(PathBuf, Option<std::time::SystemTime>, u64)> {
    files
        .iter()
        .map(|f| {
            let m = std::fs::metadata(f).ok();
            (
                f.clone(),
                m.as_ref().and_then(|m| m.modified().ok()),
                m.map_or(0, |m| m.len()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dux-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn toml_overrides_legacy_and_maps_to_dux_keys() {
        let d = tmp("merge");
        std::fs::write(
            d.join("notify.conf"),
            "DUX_WARN_PCT=70\nDUX_NTFY_URL=https://old\n",
        )
        .unwrap();
        std::fs::write(
            d.join("dux.toml"),
            "[disk]\nwarn_pct = 80\ncheck_every = 30\nmounts = [\"/\", \"/data\"]\n\n[alerts]\nmail_to = [\"a@x\", \"b@x\"]\n",
        )
        .unwrap();
        let l = load(&d.join("dux.toml")).unwrap();
        assert_eq!(l.raw["DUX_WARN_PCT"], "80");
        assert_eq!(l.raw["DUX_NTFY_URL"], "https://old", "legacy still read");
        assert_eq!(l.raw["DUX_SENTINEL_INTERVAL"], "30s");
        assert_eq!(l.raw["DUX_MOUNTS"], "/,/data");
        assert_eq!(l.raw["DUX_MAIL_TO"], "a@x,b@x");
        assert_eq!(l.files.len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn errors_name_the_file_line_and_reason() {
        let d = tmp("err");
        let p = d.join("dux.toml");
        for (text, want) in [
            (
                "[disk]\nwarn_pct = \"lots\"\n",
                ":2: [disk] warn_pct: expected a number",
            ),
            (
                "[disk]\n\nwarnpct = 80\n",
                ":3: unknown setting `warnpct` in [disk]",
            ),
            ("[disks]\nwarn_pct = 80\n", ":1: unknown section [disks]"),
            (
                "[disk]\ncheck_every = \"soon\"\n",
                ":2: [disk] check_every: `soon` is not a duration",
            ),
            ("[disk]\nwarn_pct = 80\nwarn_pct = 81\n", ":3: "),
            // A parse error never quotes the line: it may hold a secret.
            ("[alerts]\nsmtp_pass = \"hunter2\n", ":2: "),
        ] {
            std::fs::write(&p, text).unwrap();
            let e = format!("{:#}", load(&p).unwrap_err());
            assert!(e.contains(want), "{text:?}: {e}");
            assert!(!e.contains("hunter2"), "secret leaked: {e}");
        }
        // A value that parses but is invalid together with another one.
        std::fs::write(&p, "[disk]\nwarn_pct = 96\n").unwrap();
        let l = load(&p).unwrap();
        let cfg = crate::notify::NotifyConfig::from_map(l.raw.clone());
        let e = crate::sentinel::Settings::from_config(&cfg).unwrap_err();
        let msg = l.explain(&e);
        assert!(
            msg.contains("disk.warn_pct (") && msg.contains("dux.toml:2"),
            "{msg}"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_example_documents_every_setting_and_round_trips() {
        let ex = example();
        for s in SETTINGS {
            assert!(ex.contains(&format!("#{} = ", s.key)), "{}", s.key);
            assert!(!s.doc.is_empty());
        }
        // Uncommenting every default must be a valid, default-equal config.
        let all: String = ex
            .lines()
            .map(|l| {
                if l.starts_with('#') && !l.starts_with("# ") {
                    &l[1..]
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let d = tmp("ex");
        std::fs::write(d.join("dux.toml"), all).unwrap();
        let l = load(&d.join("dux.toml")).unwrap();
        let cfg = crate::notify::NotifyConfig::from_map(l.raw);
        let got = crate::sentinel::Settings::from_config(&cfg).unwrap();
        let def = crate::sentinel::Settings::default();
        assert_eq!(got.interval, def.interval);
        assert_eq!(got.fast_interval, def.fast_interval);
        assert_eq!(got.warn_pct, def.warn_pct);
        assert_eq!(got.worsen_min_gap, def.worsen_min_gap);
        assert_eq!(got.walk_every, def.walk_every);
        assert_eq!(got.walk_trigger_bytes, def.walk_trigger_bytes);
        assert_eq!(got.wall_pct, def.wall_pct);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_shipped_example_is_current() {
        let shipped = include_str!("../packaging/dux.toml.example");
        assert_eq!(
            shipped,
            example(),
            "regenerate: dux setup --print-example > packaging/dux.toml.example"
        );
    }

    #[test]
    fn written_files_are_private_and_read_back() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("write");
        let p = d.join("dux.toml");
        let mut v = HashMap::new();
        v.insert("alerts.ntfy_url".into(), "\"https://ntfy.sh/x\"".into());
        v.insert("disk.warn_pct".into(), "80".into());
        write_file(&p, &v).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let l = load(&p).unwrap();
        assert_eq!(l.raw["DUX_NTFY_URL"], "https://ntfy.sh/x");
        assert_eq!(l.raw["DUX_WARN_PCT"], "80");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
