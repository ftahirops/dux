//! The JSON documents of the CLI `--json` output and the web API
//! `/api/v1/...` (spec API-3, API-4): one builder per document, used by both,
//! so they cannot drift apart. Plus `/metrics` in Prometheus text (API-5).

use crate::fix::Suggestion;
use crate::gather::Disk;
use crate::insight::{LogFile, Project};
use crate::walker::{FoundFile, Tree};
use serde_json::{json, Value};
use std::path::Path;

pub fn disks_json(disks: &[Disk]) -> Value {
    Value::Array(
        disks
            .iter()
            .map(|d| {
                json!({"path": d.path, "fstype": d.fstype, "total": d.usage.total, "used": d.usage.used,
                    "avail": d.usage.avail, "pct": d.usage.used_pct(), "inodes_total": d.usage.inodes_total,
                    "inodes_used": d.usage.inodes_used, "inode_pct": d.usage.inode_pct()})
            })
            .collect(),
    )
}

/// `dux biggest --json`: the `top` biggest files (already filtered by size)
/// and the biggest folders one and two levels down.
pub fn biggest_json(tree: &Tree, files: &[FoundFile], top: usize) -> Value {
    let users = crate::insight::users();
    let fl: Vec<Value> = files
        .iter()
        .take(top)
        .map(|f| json!({"path": f.path, "bytes": f.bytes, "apparent": f.apparent, "mtime": f.mtime, "owner": users.get(&f.uid), "what": crate::insight::describe(&f.path)}))
        .collect();
    let dirs: Vec<Value> = crate::walker::largest_at_depth(&tree.root, 2, top)
        .into_iter()
        .map(|(p, b)| json!({"path": p, "bytes": b}))
        .collect();
    let st = &tree.stats;
    json!({"files": fl, "folders": dirs, "entries": st.entries, "file_count": st.files, "folder_count": st.dirs,
        "unreadable": st.errors, "hardlinks_counted_once": st.hardlinks_deduped, "seconds": st.seconds})
}

/// Sub-folders of `path` in the walk, biggest first.
pub fn folders_json(tree: &Tree, path: &str, top: usize) -> Value {
    let root = tree.root_path.trim_end_matches('/');
    let rel = if path == root || path.is_empty() || (root.is_empty() && path == "/") {
        Some("")
    } else if root.is_empty() {
        Some(path.trim_start_matches('/'))
    } else {
        path.strip_prefix(root)
            .filter(|r| r.starts_with('/'))
            .map(|r| r.trim_start_matches('/'))
    };
    let base = if path.is_empty() {
        tree.root_path.as_str()
    } else {
        path
    };
    let node = rel.and_then(|r| {
        if r.is_empty() {
            Some(&tree.root)
        } else {
            tree.root.find(r)
        }
    });
    let mut kids: Vec<Value> = Vec::new();
    if let Some(n) = node {
        let mut v: Vec<&crate::walker::DirNode> = n.children.iter().collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.total_bytes));
        for c in v.into_iter().take(top) {
            kids.push(json!({"path": format!("{}/{}", base.trim_end_matches('/'), c.name), "bytes": c.total_bytes, "entries": c.total_entries}));
        }
    }
    json!({"path": base, "bytes": node.map(|n| n.total_bytes), "folders": kids})
}

pub fn logs_json(logs: &[LogFile], journal: Option<u64>, projects: &[Project]) -> Value {
    let l: Vec<Value> = logs
        .iter()
        .map(|l| {
            let (group, owner) = crate::ui::views::log_owner(l, projects);
            json!({"path": l.path, "bytes": l.bytes, "kind": l.kind.label(), "group": group, "owner": owner,
                "rotated": l.rotated, "protected": l.protected, "bytes_per_min": l.per_min, "writer": l.writer,
                "service": l.service, "container": l.container, "mtime": l.mtime})
        })
        .collect();
    json!({"journal_bytes": journal, "logs": l})
}

pub fn docker_json(r: &crate::docker::Report) -> Value {
    let imgs: Vec<Value> = r.images.iter().map(|i| json!({"name": i.name, "id": i.id, "size": i.size, "unique": i.unique, "shared": i.shared, "containers": i.containers, "created": i.created})).collect();
    let cons: Vec<Value> = r.containers.iter().map(|c| json!({"name": c.name, "image": c.image, "state": c.state, "status": c.status, "writable": c.writable, "log_bytes": c.log_bytes, "project": c.project})).collect();
    let vols: Vec<Value> = r
        .volumes
        .iter()
        .map(|v| json!({"name": v.name, "size": v.size, "links": v.links, "project": v.project}))
        .collect();
    let disk: Vec<Value> = r
        .on_disk
        .iter()
        .map(|(p, b)| json!({"path": p, "bytes": b}))
        .collect();
    let proj: Vec<Value> = r.by_project().into_iter().map(|(k, v)| json!({"project": k, "images": v[0], "writable": v[1], "logs": v[2], "volumes": v[3]})).collect();
    json!({"root": r.root_dir, "images": imgs, "containers": cons, "volumes": vols, "build_cache": r.build_cache,
        "build_cache_reclaimable": r.build_cache_reclaimable, "on_disk": disk, "projects": proj})
}

pub fn projects_json(projects: &[Project], apps: &[(String, String, u64, i64)]) -> Value {
    let ps: Vec<Value> = projects.iter().map(|x| json!({"name": x.name, "type": x.type_label(), "description": x.description, "branch": x.branch, "remote": x.remote, "path": x.path, "kinds": x.kinds, "total": x.total, "source": x.source(), "build_output": x.artifacts, "git": x.git, "last_activity": x.last_activity, "build_output_dirs": x.artifact_dirs})).collect();
    let ap: Vec<Value> = apps
        .iter()
        .map(|(n, p, b, t)| json!({"app": n, "path": p, "bytes": b, "newest_change": t}))
        .collect();
    json!({"projects": ps, "apps": ap})
}

/// Cleanup suggestions as data (never actions): rating, group, what it frees.
pub fn fixes_json(fixes: &[Suggestion]) -> Value {
    let protected = crate::protect::load();
    let mut v: Vec<&Suggestion> = fixes.iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.frees));
    Value::Array(
        v.into_iter()
            .map(|x| {
                let never = crate::fix::target_path(x).is_some_and(|p| crate::protect::covered(&protected, &p));
                json!({"safety": x.safety.label(), "group": crate::fix::group(x), "title": x.title, "why": x.why,
                    "undo": x.undo, "frees": x.frees, "applicable": x.item.is_some() && !never,
                    "never_touch": never, "manual": x.manual})
            })
            .collect(),
    )
}

/// Every action dux took, newest first.
pub fn history_json() -> Value {
    let rows: Vec<Value> = crate::ui::views::load_history()
        .into_iter()
        .take(500)
        .map(|h| json!({"t": h.t, "ok": h.ok, "what": h.what, "bytes": h.bytes, "note": h.note}))
        .collect();
    json!({"rows": rows, "file": crate::cleanup::AUDIT_LOG})
}

/// Folder sizes per snapshot (spec API-8), oldest first, from the
/// sentinel's history: [{t, mount, bytes, top: [[path, bytes], ...]}].
pub fn folder_history_json(dir: &Path, since: i64) -> Value {
    let text = std::fs::read_to_string(dir.join("history.jsonl")).unwrap_or_default();
    Value::Array(
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["t"].as_i64().is_some_and(|t| t >= since))
            .collect(),
    )
}

/// The sentinel's own report (status.json), or why it is missing.
pub fn health_json(dir: &Path, now: i64) -> Value {
    match std::fs::read_to_string(dir.join(crate::sentinel::STATUS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(mut v) => {
            let age = now - v["t"].as_i64().unwrap_or(0);
            v["age_seconds"] = json!(age);
            // Two missed minutes (plus the slowest check) means it stopped.
            v["running"] = json!(age <= 180);
            v
        }
        None => {
            json!({"running": false, "error": "the sentinel has not written its status (is dux-sentinel running?)"})
        }
    }
}

fn label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Prometheus text (API-5, API-8): per mount used/free/inodes and growth,
/// active alerts, sentinel health, and per-folder sizes and growth from the
/// last two snapshots.
pub fn metrics_text(disks: &[Disk], dir: &Path, now: i64) -> String {
    let mut o = String::new();
    let mut put = |name: &str, help: &str, kind: &str, rows: &[(String, f64)]| {
        o.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
        for (l, v) in rows {
            o.push_str(&format!("{name}{l} {v}\n"));
        }
    };
    let m = |d: &Disk| {
        format!(
            "{{mount=\"{}\",fstype=\"{}\"}}",
            label(&d.path),
            label(&d.fstype)
        )
    };
    put(
        "dux_disk_size_bytes",
        "Filesystem size.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.total as f64))
            .collect::<Vec<_>>(),
    );
    put(
        "dux_disk_used_bytes",
        "Space used.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.used as f64))
            .collect::<Vec<_>>(),
    );
    put(
        "dux_disk_free_bytes",
        "Space free for normal users.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.avail as f64))
            .collect::<Vec<_>>(),
    );
    put(
        "dux_disk_used_ratio",
        "Used share, 0-1, as df shows it.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.used_pct() / 100.0))
            .collect::<Vec<_>>(),
    );
    put(
        "dux_disk_inodes_total",
        "Inodes.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.inodes_total as f64))
            .collect::<Vec<_>>(),
    );
    put(
        "dux_disk_inodes_used",
        "Inodes used.",
        "gauge",
        &disks
            .iter()
            .map(|d| (m(d), d.usage.inodes_used as f64))
            .collect::<Vec<_>>(),
    );
    let h = health_json(dir, now);
    let rates: Vec<(String, f64)> = h["growth_bytes_per_second"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((format!("{{mount=\"{}\"}}", label(k)), v.as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    put(
        "dux_disk_growth_bytes_per_second",
        "Growth of used space over the last checks (negative = shrinking).",
        "gauge",
        &rates,
    );
    let alerts: Vec<(String, f64)> = h["alerts"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    (
                        format!(
                            "{{key=\"{}\",severity=\"{}\"}}",
                            label(x["key"].as_str().unwrap_or("")),
                            label(x["severity"].as_str().unwrap_or(""))
                        ),
                        1.0,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    put(
        "dux_alert_active",
        "Conditions the sentinel is alerting about now.",
        "gauge",
        &alerts,
    );
    let up = if h["running"].as_bool() == Some(true) {
        1.0
    } else {
        0.0
    };
    put(
        "dux_sentinel_up",
        "1 when the sentinel reported within the last 3 minutes.",
        "gauge",
        &[(String::new(), up)],
    );
    put(
        "dux_sentinel_last_report_age_seconds",
        "Seconds since the sentinel last reported.",
        "gauge",
        &[(String::new(), h["age_seconds"].as_f64().unwrap_or(-1.0))],
    );
    // Folder sizes and growth from the last two snapshots of each mount.
    let hist = folder_history_json(dir, now - 30 * 86_400);
    let mut last: std::collections::BTreeMap<String, (Option<&Value>, &Value)> = Default::default();
    if let Some(a) = hist.as_array() {
        for v in a {
            let Some(mnt) = v["mount"].as_str() else {
                continue;
            };
            let e = last.entry(mnt.to_string()).or_insert((None, v));
            *e = (Some(e.1), v);
        }
    }
    let mut sizes = Vec::new();
    let mut growth = Vec::new();
    for (mnt, (prev, cur)) in &last {
        let get = |v: &Value| -> Vec<(String, f64)> {
            v["top"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .filter_map(|x| Some((x[0].as_str()?.to_string(), x[1].as_f64()?)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let now_top = get(cur);
        let before: std::collections::HashMap<String, f64> =
            prev.map(get).unwrap_or_default().into_iter().collect();
        let dt = prev
            .and_then(|p| Some((cur["t"].as_i64()? - p["t"].as_i64()?) as f64))
            .filter(|d| *d > 0.0);
        for (p, b) in now_top {
            let l = format!("{{mount=\"{}\",folder=\"{}\"}}", label(mnt), label(&p));
            if let (Some(dt), Some(old)) = (dt, before.get(&p)) {
                growth.push((l.clone(), (b - old) / dt));
            }
            sizes.push((l, b));
        }
    }
    put(
        "dux_folder_bytes",
        "Size of the biggest folders in the last snapshot.",
        "gauge",
        &sizes,
    );
    put(
        "dux_folder_growth_bytes_per_second",
        "Growth of each folder between the last two snapshots.",
        "gauge",
        &growth,
    );
    o
}

/// `dux io --json`: disk I/O now and who reads and writes.
pub fn io_json(l: &crate::live::Live, top: usize) -> Value {
    let w: Vec<Value> = l.writing.iter().take(top).map(|x| json!({"path": x.path, "pid": x.pid, "process": x.process, "unit": x.unit, "write_bps": x.write_bps, "growth_bps": x.growth_bps, "size": x.size})).collect();
    let rd: Vec<Value> = l
        .reading
        .iter()
        .take(top)
        .map(
            |x| json!({"path": x.path, "pid": x.pid, "process": x.process, "read_bps": x.read_bps}),
        )
        .collect();
    let p: Vec<Value> = l.procs.iter().take(top).map(|x| json!({"pid": x.pid, "name": x.name, "unit": x.unit, "read_bps": x.read_bps, "write_bps": x.write_bps})).collect();
    let d: Vec<Value> = l
        .disks
        .iter()
        .map(|x| json!({"disk": x.name, "read_bps": x.read_bps, "write_bps": x.write_bps}))
        .collect();
    json!({"disks": d, "processes": p, "writing": w, "reading": rd})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_valid_prometheus_text_with_escaped_labels() {
        let d = std::env::temp_dir().join(format!("dux-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let now = 10_000;
        std::fs::write(
            d.join(crate::sentinel::STATUS_FILE),
            json!({"t": now - 5, "alerts": [{"key": "space:/", "severity": "WARNING"}], "growth_bytes_per_second": {"/": 12.5}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            d.join("history.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"t": now - 3600, "mount": "/", "bytes": 1, "top": [["/var/lo\"g", 1000]]}),
                json!({"t": now, "mount": "/", "bytes": 1, "top": [["/var/lo\"g", 4600]]})
            ),
        )
        .unwrap();
        let disks = vec![Disk {
            path: "/".into(),
            fstype: "ext4".into(),
            usage: crate::sentinel::Usage {
                total: 100,
                used: 40,
                avail: 60,
                inodes_total: 10,
                inodes_used: 1,
                ..Default::default()
            },
        }];
        let t = metrics_text(&disks, &d, now);
        assert!(
            t.contains("dux_disk_used_bytes{mount=\"/\",fstype=\"ext4\"} 40"),
            "{t}"
        );
        assert!(
            t.contains("dux_alert_active{key=\"space:/\",severity=\"WARNING\"} 1"),
            "{t}"
        );
        assert!(t.contains("dux_sentinel_up 1"), "{t}");
        assert!(
            t.contains("dux_disk_growth_bytes_per_second{mount=\"/\"} 12.5"),
            "{t}"
        );
        assert!(
            t.contains("folder=\"/var/lo\\\"g\"} 1"),
            "growth 3600 bytes per hour = 1/s: {t}"
        );
        // Every sample line is "name{labels} value".
        for l in t.lines().filter(|l| !l.starts_with('#')) {
            let (name, val) = l.rsplit_once(' ').unwrap();
            assert!(val.parse::<f64>().is_ok(), "{l}");
            assert!(name.starts_with("dux_"), "{l}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn folders_json_finds_any_folder_of_the_walk() {
        let d = std::env::temp_dir().join(format!("dux-api-f-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/f"), vec![0u8; 8192]).unwrap();
        let t = crate::walker::walk(d.to_str().unwrap(), 1).unwrap();
        let v = folders_json(&t, d.join("a").to_str().unwrap(), 10);
        assert_eq!(
            v["folders"][0]["path"],
            json!(d.join("a/b").to_str().unwrap())
        );
        let v = folders_json(&t, "/elsewhere", 10);
        assert_eq!(v["folders"], json!([]));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
