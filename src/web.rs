//! `dux web`: a read-only web dashboard and API (spec 4.6, A12).
//!
//! - Read-only (API-1): no endpoint changes anything on the server; the
//!   dashboard shows the five views of the terminal UI without actions.
//! - Listens on 127.0.0.1 by default (API-2); every request needs the access
//!   token: `?token=` for page loads, `X-Dux-Token` or `Authorization:
//!   Bearer` for API calls and /metrics.
//! - `/api/v1/...` (API-3/4): disks, folders, biggest, logs, docker,
//!   projects, fixes, history, history/folders, health; each is built by the
//!   same code as the matching CLI `--json` output.
//! - `/metrics` (API-5/8): Prometheus text.
//! - Data is gathered on start, every `refresh` seconds (default 5 min) and
//!   on POST /api/refresh (a rescan, which changes nothing on disk).

use crate::gather::{self, Gathered};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Server {
    pub root: String,
    pub token: String,
    pub refresh_secs: u64,
    state: Mutex<Option<Arc<Gathered>>>,
    walking: AtomicBool,
    progress: Arc<AtomicU64>,
    /// Live I/O for the Activity view, sampled at most once a second.
    live: Mutex<(crate::live::Sampler, Option<std::time::Instant>, Value)>,
}

/// A fresh 128-bit token. Fails instead of returning a guessable one when
/// /dev/urandom cannot be read.
pub fn random_token() -> anyhow::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| anyhow::anyhow!("cannot read /dev/urandom for the access token: {e}"))?;
    anyhow::ensure!(b.iter().any(|x| *x != 0), "/dev/urandom returned zeros");
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Percent-decode a query value (UTF-8; invalid sequences are kept as-is).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Constant-time comparison so the token cannot be guessed byte by byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl Server {
    pub fn new(root: String, token: String, refresh_secs: u64) -> Arc<Server> {
        Arc::new(Server {
            root,
            token,
            refresh_secs,
            state: Mutex::new(None),
            walking: AtomicBool::new(false),
            progress: Arc::new(AtomicU64::new(0)),
            live: Mutex::new((crate::live::Sampler::default(), None, Value::Null)),
        })
    }

    /// Activity: disk I/O now and who writes, as `dux io --json` shows it.
    fn activity(&self) -> Value {
        let mut l = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if l.1.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            let x = l.0.sample();
            l.2 = crate::api::io_json(&x, 15);
            l.1 = Some(std::time::Instant::now());
        }
        l.2.clone()
    }

    fn regather(self: &Arc<Self>) {
        if self.walking.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            // Clears `walking` however this thread ends, even on a panic.
            struct Done<'a>(&'a AtomicBool);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _done = Done(&me.walking);
            me.progress.store(0, Ordering::Relaxed);
            let g = gather::gather(&me.root, 2, Some(me.progress.clone()));
            gather::save_cache(&me.root, &g);
            *me.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(g));
        });
    }

    fn snapshot(&self) -> Option<Arc<Gathered>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// JSON for the dashboard.
pub fn state_json(g: Option<&Gathered>, walking: bool, progress: u64) -> Value {
    let disks: Vec<Value> = gather::disks()
        .into_iter()
        .map(|d| json!({"path": d.path, "fstype": d.fstype, "total": d.usage.total, "used": d.usage.used, "avail": d.usage.avail, "pct": d.usage.used_pct(), "inode_pct": d.usage.inode_pct()}))
        .collect();
    let Some(g) = g else {
        return json!({"ready": false, "walking": walking, "progress": progress, "disks": disks});
    };
    let folders: Vec<Value> = g
        .tree
        .as_ref()
        .map(|t| {
            crate::walker::largest_at_depth(&t.root, 2, 40)
                .into_iter()
                .map(|(p, b)| json!({"path": p, "bytes": b}))
                .collect()
        })
        .unwrap_or_default();
    let logs: Vec<Value> = g
        .logs
        .iter()
        .take(2000)
        .map(|l| {
            let (cat, owner) = crate::ui::views::log_owner(l, &g.projects);
            json!({"path": l.path, "bytes": l.bytes, "kind": l.kind.label(), "category": cat, "owner": owner, "rotated": l.rotated, "protected": l.protected, "binary": crate::cleanup::is_binary_log(std::path::Path::new(&l.path)), "writer": l.writer, "container": l.container, "mtime": l.mtime})
        })
        .collect();
    let projects: Vec<Value> = g.projects.iter().map(|p| json!({"path": p.path, "name": p.name, "type": p.type_label(), "description": p.description, "branch": p.branch, "remote": p.remote, "location": crate::ui::views::proj_location(&p.path), "kinds": p.kinds, "total": p.total, "source": p.source(), "build_output": p.artifacts, "git": p.git, "last_activity": p.last_activity, "build_output_dirs": p.artifact_dirs})).collect();
    let apps: Vec<Value> = g
        .apps
        .iter()
        .map(|(n, p, b, t)| json!({"app": n, "path": p, "bytes": b, "changed": t}))
        .collect();
    let docker = g.docker.as_ref().map(|d| {
        let names = |v: Vec<usize>| v.into_iter().map(|c| d.containers[c].name.clone()).collect::<Vec<_>>();
        let image_users = d.image_users();
        json!({
            "root_dir": d.root_dir,
            "images_total": d.images_total(), "writable_total": d.writable_total(), "logs_total": d.logs_total(),
            "volumes_total": d.volumes_total(), "build_cache": d.build_cache, "build_cache_reclaimable": d.build_cache_reclaimable,
            "stacks": d.stacks().iter().map(|s| json!({
                "name": s.name, "working_dir": s.working_dir, "config_files": s.config_files, "running": s.running,
                "containers": s.containers.iter().map(|&c| d.containers[c].name.clone()).collect::<Vec<_>>(),
                "images": s.images.iter().map(|&i| d.images[i].name.clone()).collect::<Vec<_>>(),
                "volumes": s.volumes.iter().map(|&v| d.volumes[v].name.clone()).collect::<Vec<_>>(),
                "writable": s.writable, "logs": s.logs, "image_bytes": s.image_bytes, "volume_bytes": s.volume_bytes, "total": s.total(),
            })).collect::<Vec<_>>(),
            "containers": d.containers.iter().map(|c| json!({"runtime": crate::docker::program(c.runtime), "name": c.name, "id": c.id, "image": c.image, "state": c.state, "status": c.status, "running": c.running(),
                "writable": c.writable, "logs": c.log_bytes, "log_path": c.log_path, "project": c.project, "service": c.service, "working_dir": c.working_dir,
                "command": c.command, "created": c.created, "ports": c.ports, "networks": c.networks, "mounts": c.mounts, "health": c.health})).collect::<Vec<_>>(),
            "images": d.images.iter().enumerate().map(|(i, im)| json!({"runtime": crate::docker::program(im.runtime), "name": im.name, "id": im.id, "size": im.size, "unique": im.unique, "shared": im.shared,
                "containers": im.containers, "created": im.created, "used_by": names(image_users[i].clone())})).collect::<Vec<_>>(),
            "volumes": d.volumes.iter().enumerate().map(|(i, v)| json!({"runtime": crate::docker::program(v.runtime), "name": v.name, "compose_name": v.compose_name, "size": v.size, "links": v.links,
                "project": v.project, "driver": v.driver, "mountpoint": v.mountpoint, "used_by": names(d.users_of_volume(i))})).collect::<Vec<_>>(),
            "on_disk": d.on_disk.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
        })
    });
    let fixes = crate::api::fixes_json(&g.fixes);
    let services: Vec<Value> = g
        .services
        .iter()
        .map(|x| json!({"name": x.name, "total": x.total, "logs": x.logs, "folders": x.folders}))
        .collect();
    json!({
        "ready": true, "walking": walking, "progress": progress, "at": g.at, "services": services,
        "disks": disks, "folders": folders, "logs": logs, "journal_bytes": g.journal_bytes,
        "biggest": g.biggest.iter().map(|f| json!({"path": f.path, "bytes": f.bytes, "mtime": f.mtime})).collect::<Vec<_>>(),
        "projects": projects, "apps": apps, "docker": docker, "fixes": fixes, "errors": g.errors,
    })
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
}

fn read_request(s: &mut TcpStream) -> Option<Request> {
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > 64 * 1024 {
        return None;
    }
    // Read-only: a body is read to the end (so closing does not reset the
    // connection) but never used.
    let mut got = buf.len() - (head_end + 4);
    while got < len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        got += n;
    }
    let (path, query) = target
        .split_once('?')
        .map(|(p, q)| (p.to_string(), q.to_string()))
        .unwrap_or((target, String::new()));
    Some(Request {
        method,
        path,
        query,
        headers,
    })
}

fn query_param(q: &str, key: &str) -> Option<String> {
    q.split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .map(|v| v.to_string())
}

fn respond(s: &mut TcpStream, code: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

fn handle(srv: &Arc<Server>, mut s: TcpStream) {
    let Some(req) = read_request(&mut s) else {
        return;
    };
    // Browsers ask for this without the token; answer it quietly.
    if req.path == "/favicon.ico" {
        respond(&mut s, "204 No Content", "text/plain", b"");
        return;
    }
    let header_token = req
        .headers
        .iter()
        .find(|(k, _)| k == "x-dux-token")
        .map(|(_, v)| v.clone())
        .or_else(|| {
            req.headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .and_then(|(_, v)| v.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
        });
    let query_token = query_param(&req.query, "token");
    let authed = header_token
        .as_deref()
        .or(query_token.as_deref())
        .is_some_and(|t| same(t, &srv.token));
    if !authed {
        respond(
            &mut s,
            "401 Unauthorized",
            "text/plain",
            b"dux web: add ?token=... from the dux web start message\n",
        );
        return;
    }
    let api_json = |s: &mut TcpStream, v: Value| {
        respond(s, "200 OK", "application/json", v.to_string().as_bytes())
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        ),
        ("GET", "/api/state") => {
            let g = srv.snapshot();
            api_json(
                &mut s,
                state_json(
                    g.as_deref(),
                    srv.walking.load(Ordering::Relaxed),
                    srv.progress.load(Ordering::Relaxed),
                ),
            );
        }
        ("GET", "/api/tree") => {
            let p = percent_decode(&query_param(&req.query, "path").unwrap_or_default());
            let g = srv.snapshot();
            let kids: Vec<Value> = g
                .as_ref()
                .and_then(|g| g.tree.as_ref())
                .and_then(|t| {
                    // Strip the walk root once, at a path boundary.
                    let root = t.root_path.trim_end_matches('/');
                    let rel = if root.is_empty() {
                        Some(p.as_str())
                    } else if p == root {
                        Some("")
                    } else {
                        p.strip_prefix(root).filter(|r| r.starts_with('/'))
                    }?;
                    t.root.find(rel)
                })
                .map(|n| {
                    let mut v: Vec<&crate::walker::DirNode> = n.children.iter().collect();
                    v.sort_by_key(|c| std::cmp::Reverse(c.total_bytes));
                    v.iter().map(|c| json!({"name": c.name, "bytes": c.total_bytes, "entries": c.total_entries, "dirs": !c.children.is_empty()})).collect()
                })
                .unwrap_or_default();
            api_json(&mut s, json!({"path": p, "children": kids}));
        }
        ("POST", "/api/refresh") if header_token.is_some() => {
            srv.regather();
            api_json(&mut s, json!({"ok": true}));
        }
        ("GET", "/api/history") => {
            let rows: Vec<Value> = crate::ui::views::load_history()
                .into_iter()
                .take(500)
                .map(|h| json!({"t": h.t, "ok": h.ok, "what": h.what, "bytes": h.bytes, "note": h.note}))
                .collect();
            api_json(
                &mut s,
                json!({"rows": rows, "file": crate::cleanup::AUDIT_LOG}),
            );
        }
        ("GET", "/api/inside") => {
            // Only containers and volumes in the current scan, by name.
            let kind = query_param(&req.query, "kind").unwrap_or_default();
            let name = percent_decode(&query_param(&req.query, "name").unwrap_or_default());
            let g = srv.snapshot();
            let r = g.as_ref().and_then(|g| g.docker.as_ref());
            let res = match (kind.as_str(), r) {
                ("container", Some(r)) => r
                    .containers
                    .iter()
                    .find(|c| c.name == name)
                    .ok_or_else(|| anyhow::anyhow!("no such container"))
                    .and_then(|c| {
                        let d = crate::docker::upper_dir(c.runtime, &c.id)?;
                        let v = crate::docker::inside(&d, 8)?;
                        Ok((v.clone(), crate::docker::layer_advice(c, &v)))
                    }),
                ("volume", Some(r)) => r
                    .volumes
                    .iter()
                    .find(|v| v.name == name)
                    .ok_or_else(|| anyhow::anyhow!("no such volume"))
                    .and_then(|v| Ok((crate::docker::inside(&v.mountpoint, 8)?, None))),
                _ => Err(anyhow::anyhow!("unknown target")),
            };
            match res {
                Ok((v, advice)) => api_json(
                    &mut s,
                    json!({"rows": v.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(), "advice": advice}),
                ),
                Err(e) => api_json(&mut s, json!({"error": format!("{e:#}")})),
            }
        }
        ("GET", "/api/logtail") => {
            // Only files that are in dux's own log list can be read here.
            let p = percent_decode(&query_param(&req.query, "path").unwrap_or_default());
            let known = srv
                .snapshot()
                .is_some_and(|g| g.logs.iter().any(|l| l.path == p));
            if known {
                api_json(
                    &mut s,
                    json!({"lines": crate::ui::views::tail_lines(&p, 8)}),
                );
            } else {
                respond(
                    &mut s,
                    "404 Not Found",
                    "application/json",
                    br#"{"error":"not a log dux listed"}"#,
                );
            }
        }
        ("GET", "/api/activity") => {
            let v = srv.activity();
            api_json(&mut s, v);
        }
        ("GET", "/metrics") => {
            let dir = crate::sentinel::Settings::default().snapshot_dir;
            let t = crate::api::metrics_text(&gather::disks(), &dir, crate::util::now_secs());
            respond(&mut s, "200 OK", "text/plain; version=0.0.4", t.as_bytes());
        }
        ("GET", p) if p.starts_with("/api/v1/") => {
            let dir = crate::sentinel::Settings::default().snapshot_dir;
            let now = crate::util::now_secs();
            let name = &p["/api/v1/".len()..];
            let g = srv.snapshot();
            let v = match (name, g.as_deref()) {
                ("disks", _) => Some(crate::api::disks_json(&gather::disks())),
                ("health", _) => Some(crate::api::health_json(&dir, now)),
                ("history", _) => Some(crate::api::history_json()),
                ("history/folders", _) => {
                    let days: i64 = query_param(&req.query, "days")
                        .and_then(|d| d.parse().ok())
                        .unwrap_or(30);
                    Some(crate::api::folder_history_json(
                        &dir,
                        now - days.clamp(1, 365) * 86_400,
                    ))
                }
                ("activity", _) => Some(srv.activity()),
                ("folders", Some(g)) => g.tree.as_ref().map(|t| {
                    let p = percent_decode(&query_param(&req.query, "path").unwrap_or_default());
                    let n = query_param(&req.query, "n")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(25);
                    crate::api::folders_json(t, &p, n)
                }),
                ("biggest", Some(g)) => g.tree.as_ref().map(|t| {
                    let n = query_param(&req.query, "n")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(10);
                    crate::api::biggest_json(t, &g.biggest, n)
                }),
                ("logs", Some(g)) => {
                    Some(crate::api::logs_json(&g.logs, g.journal_bytes, &g.projects))
                }
                ("docker", Some(g)) => g.docker.as_ref().map(crate::api::docker_json),
                ("projects", Some(g)) => Some(crate::api::projects_json(&g.projects, &g.apps)),
                ("fixes", Some(g)) => Some(crate::api::fixes_json(&g.fixes)),
                (_, None)
                    if ["folders", "biggest", "logs", "docker", "projects", "fixes"]
                        .contains(&name) =>
                {
                    respond(
                        &mut s,
                        "503 Service Unavailable",
                        "application/json",
                        br#"{"error":"the first scan is still running"}"#,
                    );
                    return;
                }
                _ => None,
            };
            match v {
                Some(v) => api_json(&mut s, v),
                None => respond(
                    &mut s,
                    "404 Not Found",
                    "application/json",
                    br#"{"error":"no such endpoint or no data"}"#,
                ),
            }
        }
        ("POST", _) => respond(
            &mut s,
            "405 Method Not Allowed",
            "text/plain",
            b"dux web is read-only\n",
        ),
        _ => respond(&mut s, "404 Not Found", "text/plain", b"not found\n"),
    }
}

pub fn serve(srv: Arc<Server>, listen: &str) -> Result<()> {
    let l = TcpListener::bind(listen).with_context(|| format!("listening on {listen}"))?;
    srv.regather();
    let me = srv.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(me.refresh_secs.max(60)));
        me.regather();
    });
    let active = Arc::new(AtomicU64::new(0));
    for conn in l.incoming().flatten() {
        if active.load(Ordering::Relaxed) >= 16 {
            continue; // drop excess connections rather than spawn without bound
        }
        let srv = srv.clone();
        let active = active.clone();
        active.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            handle(&srv, conn);
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

const PAGE: &str = r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>dux</title>
<style>
:root{--bg:#f4f6f5;--panel:#fff;--fg:#1b2420;--muted:#5b6a63;--line:#d7ded9;--accent:#0f6b5a;--safe:#0f6b5a;--caution:#8a5a00;--risky:#a3352b;--sel:#e3f1ec}
@media (prefers-color-scheme:dark){:root{--bg:#121715;--panel:#19201d;--fg:#e3eae6;--muted:#98a69f;--line:#2c3632;--accent:#5cc4a8;--safe:#6fd1b4;--caution:#e6b45c;--risky:#f08b80;--sel:#1f3a33}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,sans-serif}
html,body{max-width:100%;overflow-x:hidden}header{padding:12px 16px;border-bottom:1px solid var(--line);display:flex;gap:14px;align-items:center;flex-wrap:wrap}
h1{font-size:18px;margin:0}h3{margin:18px 0 6px;font-size:15px}
nav{display:flex;gap:6px;flex-wrap:wrap}nav button,.sub button{background:none;border:1px solid var(--line);color:var(--fg);padding:5px 12px;border-radius:99px;cursor:pointer}
nav button.on,.sub button.on{background:var(--accent);color:var(--bg);border-color:var(--accent)}
.sub{display:flex;gap:6px;flex-wrap:wrap;margin:0 0 12px}
main{padding:14px 16px;max-width:1400px}
.split{display:grid;grid-template-columns:minmax(0,3fr) minmax(280px,2fr);gap:14px;align-items:start}
@media (max-width:900px){.split{grid-template-columns:minmax(0,1fr)}.split>.panel{order:-1;position:static}}
table{border-collapse:collapse;width:100%;background:var(--panel);border:1px solid var(--line)}th,td{text-align:left;padding:5px 9px;border-top:1px solid var(--line);vertical-align:top;overflow-wrap:anywhere}
th{font-size:12px;color:var(--muted);text-transform:uppercase;border-top:0;white-space:nowrap;overflow-wrap:normal}td.n{font-variant-numeric:tabular-nums;white-space:nowrap}
tr.click{cursor:pointer}tr.click:hover,tr.sel{background:var(--sel)}tr.grp td{font-weight:600}tr.dim td{color:var(--muted)}
.box{overflow-x:auto}.panel{background:var(--panel);border:1px solid var(--line);padding:12px 14px;position:sticky;top:10px}
.panel h4{margin:0 0 8px}.kv{display:grid;grid-template-columns:110px 1fr;gap:2px 10px;font-size:13px}.kv b{color:var(--muted);font-weight:500}
.kv span{overflow-wrap:anywhere}.btns{display:flex;gap:8px;flex-wrap:wrap;margin-top:12px}.btns button{padding:6px 12px;cursor:pointer}
.bar{display:inline-block;height:10px;background:var(--accent);border-radius:2px;vertical-align:middle}.muted{color:var(--muted)}
.SAFE{color:var(--safe)}.CAUTION{color:var(--caution)}.RISKY{color:var(--risky)}.warn{color:var(--caution)}.bad{color:var(--risky)}.ok{color:var(--safe)}
pre{background:var(--bg);padding:8px;overflow-x:auto;font-size:12px;margin:8px 0 0;white-space:pre-wrap}
#actions{margin:12px 0;display:flex;gap:8px;flex-wrap:wrap;align-items:center}#actions button{padding:6px 12px;cursor:pointer}a{color:var(--accent);cursor:pointer}
#modal{position:fixed;inset:0;background:rgba(0,0,0,.45);display:none;align-items:center;justify-content:center;padding:16px}
#modal .card{background:var(--panel);border:1px solid var(--line);max-width:900px;width:100%;max-height:85vh;overflow:auto;padding:16px}
.prog{height:12px;background:var(--line);border-radius:6px;overflow:hidden;margin:8px 0}.prog div{height:100%;background:var(--accent)}
</style></head><body>
<header><h1>dux</h1><nav id="tabs"></nav><span id="status" class="muted"></span><button id="refresh">Refresh</button></header>
<main><div id="view"></div></main>
<div id="modal"><div class="card" id="card"></div></div>
<script>
const token=new URLSearchParams(location.search).get('token')||'';
let S=null,tab='Disk usage',grp='Docker',actTimer=null,folder='',picked=new Set(),dsub='Overview',sel=null,openGroups=new Set(),closedPlaces=new Set(),H_ROWS=null;
const tabs=['Disk usage','Activity','Usage by group','Cleanup','History'];
const groups=['Docker','Projects','Services','Logs'];
const dsubs=['Overview','Stacks','Containers','Images','Volumes','Logs','Storage'];
const H=b=>{if(b==null)return'-';const neg=b<0;b=Math.abs(b);const u=['B','KiB','MiB','GiB','TiB'];let i=0;while(b>=1024&&i<4){b/=1024;i++}return(neg?'-':'')+(i?b.toFixed(2):b)+' '+u[i]};
const el=(t,txt,cls)=>{const e=document.createElement(t);if(txt!=null)e.textContent=txt;if(cls)e.className=cls;return e};
const ago=t=>{const s=Math.max(0,Date.now()/1000-t);return s<3600?Math.floor(s/60)+'m':s<86400?Math.floor(s/3600)+'h':Math.floor(s/86400)+'d'};
function table(head,rows,opts){opts=opts||{};const box=el('div',null,'box'),t=el('table'),tr=el('tr');head.forEach(h=>tr.appendChild(el('th',h)));t.appendChild(tr);
rows.forEach((r,i)=>{const row=el('tr');const cells=r.cells||r;if(r.cls)row.className=r.cls;if(r.onclick){row.classList.add('click');row.onclick=r.onclick}
cells.forEach(c=>{const td=el('td');if(c instanceof Node)td.appendChild(c);else{td.textContent=c==null?'':String(c);if(/^-?[\d.,]+ ?(B|KiB|MiB|GiB|TiB|%)$/.test(td.textContent))td.className='n'}row.appendChild(td)});t.appendChild(row)});box.appendChild(t);return box}
function barc(f){const s=el('span',null,'bar');s.style.width=Math.max(1,Math.round(f*120))+'px';return s}
function kv(pairs){const d=el('div',null,'kv');pairs.forEach(([k,v])=>{d.appendChild(el('b',k));d.appendChild(el('span',v==null||v===''?'-':String(v)))});return d}
function panel(title,pairs,buttons,extra){const p=el('div',null,'panel');p.appendChild(el('h4',title));p.appendChild(kv(pairs));if(extra)p.appendChild(extra);
return p}
function split(left,right){const d=el('div',null,'split');d.appendChild(left);d.appendChild(right||panel('Details',[['','Click a row to see its details.']]));return d}
async function api(p,opt){const r=await fetch(p,Object.assign({headers:{'X-Dux-Token':token,'Content-Type':'application/json'}},opt||{}));try{return await r.json()}catch(e){return{error:'HTTP '+r.status}}}
async function load(){S=await api('/api/state');render();if(!S.ready||S.walking)setTimeout(load,2000)}
function render(){const nav=document.getElementById('tabs');nav.textContent='';tabs.forEach((t,i)=>{const b=el('button',(i+1)+' '+t,t===tab?'on':'');b.onclick=()=>{tab=t;sel=null;render()};nav.appendChild(b)});
document.getElementById('status').textContent=(S.walking?('scanning... '+S.progress+' entries'):(S.at?('as of '+new Date(S.at*1000).toLocaleTimeString()):''))+' · read-only';
const v=document.getElementById('view');v.textContent='';if(actTimer&&tab!=='Activity'){clearTimeout(actTimer);actTimer=null}
if(tab==='Activity'){renderActivity(v);return}
if(tab==='History'){renderHistory(v);return}
if(tab==='Disk usage'){v.appendChild(table(['mount','type','size','free','used','inodes'],S.disks.map(d=>[d.path,d.fstype,H(d.total),H(d.avail),(()=>{const s=el('span');s.appendChild(barc(d.pct/100));s.appendChild(document.createTextNode(' '+d.pct.toFixed(1)+'%'));return s})(),d.inode_pct==null?'-':d.inode_pct.toFixed(1)+'%'])))}
if(!S.ready){v.appendChild(el('p','Scanning the filesystem... '+S.progress+' entries so far.','muted'));return}
if(tab==='Disk usage'){renderFolder(v);v.appendChild(el('h3','Biggest files'));v.appendChild(table(['size','changed','path'],S.biggest.slice(0,25).map(f=>[H(f.bytes),ago(f.mtime),f.path])));return}
if(tab==='Usage by group'){const sb=el('div',null,'sub');groups.forEach(t=>{const b=el('button',t,t===grp?'on':'');b.onclick=()=>{grp=t;sel=null;render()};sb.appendChild(b)});v.appendChild(sb);
if(grp==='Docker')renderDocker(v);else if(grp==='Projects')renderProjects(v);else if(grp==='Services')renderServices(v);else renderLogs(v);return}
if(tab==='Cleanup')renderFixes(v)}
async function renderActivity(v){const a=await api('/api/activity');if(tab!=='Activity')return;v.textContent='';
v.appendChild(el('h3','Disk I/O now'));v.appendChild(table(['disk','read/s','write/s'],(a.disks||[]).map(d=>[d.disk,H(d.read_bps),H(d.write_bps)])));
v.appendChild(el('h3','Processes writing and reading most'));v.appendChild(table(['writes/s','reads/s','pid','process','service / container'],(a.processes||[]).map(p=>[H(p.write_bps),H(p.read_bps),p.pid,p.name,p.unit||''])));
v.appendChild(el('h3','Files being written'));v.appendChild(table(['write/s','growth/s','size','writer','path'],(a.writing||[]).map(w=>[H(w.write_bps),H(w.growth_bps),H(w.size),w.process+' ('+w.pid+')',w.path])));
actTimer=setTimeout(()=>{if(tab==='Activity')render()},5000)}
function renderServices(v){const t=S.services.reduce((a,s)=>a+s.total,0);v.appendChild(el('p',S.services.length+' running services use '+H(t)+' in their data folders and logs.'));
v.appendChild(table(['service','total','data','logs','folders'],S.services.map(s=>[s.name,H(s.total),H(s.folders.reduce((a,f)=>a+f[1],0)),H(s.logs),s.folders.map(f=>f[0]).join(', ')])))}
// ---- Docker
function renderDocker(v){const d=S.docker;if(!d){v.appendChild(el('p','Docker is not available on this host.','muted'));return}
const sb=el('div',null,'sub');dsubs.forEach(t=>{const b=el('button',t,t===dsub?'on':'');b.onclick=()=>{dsub=t;sel=null;render()};sb.appendChild(b)});v.appendChild(sb);
const C=n=>d.containers.find(c=>c.name===n),total=d.images_total+d.writable_total+d.logs_total+d.volumes_total+d.build_cache;
const pick=(k)=>()=>{sel=k;render()};const isSel=k=>sel===k?'sel':'';
if(dsub==='Overview'){const running=d.containers.filter(c=>c.running).length,unusedImg=d.images.filter(i=>!i.used_by.length&&!i.containers),unusedVol=d.volumes.filter(x=>!x.used_by.length&&!x.links);
const row=(k,b,n)=>[k,H(b),(b*100/Math.max(1,total)).toFixed(1)+'%',n];
v.appendChild(el('p','Docker uses '+H(total)+' · data root '+d.root_dir));
v.appendChild(table(['what','size','share','details'],[row('Images',d.images_total,d.images.length+' images · '+unusedImg.length+' unused ('+H(unusedImg.reduce((a,i)=>a+i.unique,0))+')'),
row('Containers (writable layers)',d.writable_total,running+' running, '+(d.containers.length-running)+' stopped'),row('Container logs',d.logs_total,d.containers.filter(c=>c.logs>0).length+' containers log'),
row('Volumes',d.volumes_total,d.volumes.length+' volumes · '+unusedVol.length+' unused'),row('Build cache',d.build_cache,H(d.build_cache_reclaimable)+' reclaimable')]));
v.appendChild(el('h3','Biggest stacks'));v.appendChild(table(['stack','running','uses','source'],d.stacks.slice(0,12).map(s=>({onclick:()=>{dsub='Stacks';sel='s:'+s.name;render()},cells:[s.name,s.running+'/'+s.containers.length,H(s.total),s.working_dir||'-']}))));return}
let left,right=null;
if(dsub==='Stacks'){left=table(['stack','running','total','images','writable','logs','volumes','source'],d.stacks.map(s=>({cls:isSel('s:'+s.name),onclick:pick('s:'+s.name),cells:[s.name,s.running+'/'+s.containers.length,H(s.total),H(s.image_bytes),H(s.writable),H(s.logs),H(s.volume_bytes),s.working_dir||'-']})));
const s=d.stacks.find(x=>'s:'+x.name===sel);if(s){const ex=el('div');ex.appendChild(el('h3','Services'));ex.appendChild(table(['container','status','writable','log','image'],s.containers.map(n=>{const c=C(n)||{};return{onclick:()=>{dsub='Containers';sel='c:'+n;render()},cells:[(c.service?c.service+' · ':'')+n,c.status,H(c.writable),H(c.logs),c.image]}})));
ex.appendChild(el('h3','Images'));ex.appendChild(table(['image'],s.images.map(x=>[x])));ex.appendChild(el('h3','Volumes'));ex.appendChild(table(['volume'],s.volumes.map(x=>[x])));
right=panel('Stack '+s.name,[['source',s.working_dir],['compose',s.config_files.join('  ')],['containers',s.containers.length+' ('+s.running+' running)'],['uses',H(s.total)+': images '+H(s.image_bytes)+' · writable '+H(s.writable)+' · logs '+H(s.logs)+' · volumes '+H(s.volume_bytes)]],
[{label:'Truncate big logs of this stack',cls:'CAUTION',run:()=>act({kind:'stack',name:s.name,act:'truncate'},'Truncate every container log over 10 MiB in stack '+s.name+' (the last 10 MiB of each is kept aside)',false)}],ex)}}
if(dsub==='Containers'||dsub==='Logs'){const list=dsub==='Logs'?d.containers.filter(c=>c.logs>0).sort((a,b)=>b.logs-a.logs):d.containers;
left=table(dsub==='Logs'?['size','container','stack','state','log file']:['container','state','stack','uses','writable','log','mounts','image'],list.map(c=>({cls:isSel('c:'+c.name)+(c.running?'':' dim'),onclick:pick('c:'+c.name),
cells:dsub==='Logs'?[H(c.logs),c.name,c.project||'-',c.state,c.log_path]:[c.name,c.state,c.project||'-',H(c.writable+c.logs),H(c.writable),H(c.logs),c.mounts.length,c.image]})));
const c=d.containers.find(x=>'c:'+x.name===sel);if(c){const mounts=el('div');mounts.appendChild(el('h3','Mounts'));mounts.appendChild(table(['mount','size','on disk'],c.mounts.map(m=>{const exact=d.volumes.find(x=>x.name===m),pre=m.endsWith('…')?d.volumes.filter(x=>x.name.startsWith(m.slice(0,-1))):[],vol=exact||(pre.length===1?pre[0]:null);return vol?['volume '+(vol.compose_name||vol.name),H(vol.size),vol.mountpoint]:[m,'','bind mount']})));
mounts.appendChild(el('pre',(c.runtime||'docker')+' logs --tail 50 '+c.name+'\n'+(c.runtime||'docker')+' inspect '+c.name));
mounts.appendChild(insideBox('container',c.name,'Inside its writable layer (what it writes into itself)'));
const b=[];if(c.logs>10*1048576)b.push({label:'Truncate log',cls:'CAUTION',run:()=>act({kind:'container',name:c.name,act:'truncate'},'Truncate the log of '+c.name+' (its last 10 MiB is kept aside)',false,c.logs-10*1048576)});
if(!c.running)b.push({label:'Remove container (RISKY)',cls:'RISKY',run:()=>act({kind:'container',name:c.name,act:'remove'},'Remove stopped container '+c.name+' and the files written inside it (not its volumes). Its writable layer may hold the only copy of data.',true,c.writable+c.logs)});
right=panel('Container '+c.name,[['id',c.id],['state',c.state+' · '+c.status+(c.health?' · health '+c.health:'')],['stack',(c.project||'-')+' · service '+(c.service||'-')],['source',c.working_dir],['image',c.image],['command',c.command],['created',c.created],['ports',c.ports],['networks',c.networks.join(', ')],['disk','writable '+H(c.writable)+' · log '+H(c.logs)],['log file',c.log_path]],b,mounts)}}
if(dsub==='Images'){left=table(['image','size','unique','shared','used by','created'],d.images.map(i=>({cls:isSel('i:'+i.name)+(i.used_by.length||i.containers?'':' dim'),onclick:pick('i:'+i.name),cells:[i.name,H(i.size),H(i.unique),H(i.shared),i.used_by.length||i.containers?(i.used_by.join(', ')||i.containers+' container(s)'):'UNUSED',i.created]})));
const i=d.images.find(x=>'i:'+x.name===sel);if(i){const used=i.used_by.length>0||i.containers>0;right=panel('Image '+i.name,[['id',i.id],['size',H(i.size)+' · unique '+H(i.unique)+' · shared '+H(i.shared)],['created',i.created],['used by',used?(i.used_by.join(', ')||i.containers+' container(s)'):'no container: unused']],
used?[]:[{label:'Remove image',cls:'CAUTION',run:()=>act({kind:'image',name:i.name,act:'remove'},'Remove unused image '+i.name+' (docker pull brings it back)',false,i.unique)}])}}
if(dsub==='Volumes'){left=table(['volume','size','stack','used by','on disk'],d.volumes.map(x=>({cls:isSel('v:'+x.name)+(x.used_by.length||x.links?'':' dim'),onclick:pick('v:'+x.name),cells:[x.compose_name||x.name.slice(0,28),H(x.size),x.project||'-',x.used_by.length?x.used_by.join(', '):(x.links?x.links+' container(s)':'UNUSED'),x.mountpoint]})));
const x=d.volumes.find(y=>'v:'+y.name===sel);if(x){const used=x.used_by.length>0||x.links>0;const vin=el('div');vin.appendChild(insideBox('volume',x.name,'Inside the volume'));right=panel('Volume '+(x.compose_name||x.name),[['name',x.name],['stack',x.project],['size',H(x.size)],['driver',x.driver],['on disk',x.mountpoint],['mounted by',used?x.used_by.join(', ')||x.links+' container(s)':'no container: unused']],
used?[]:[{label:'Delete volume (RISKY)',cls:'RISKY',run:()=>act({kind:'volume',name:x.name,act:'remove'},'Delete unused volume '+x.name+'. It may hold a database or uploads.',true,x.size)}],vin)}}
if(dsub==='Storage'){const what=p=>p.endsWith('/overlay2')?'image layers and container writable layers':p.endsWith('/volumes')?'volumes':p.endsWith('/containers')?'container config and json logs':p.endsWith('/buildkit')?'build cache':p.includes('snapshot')?'containerd snapshots (image layers)':p.includes('content')?'containerd content store':'';
left=table(['size','folder','what it holds'],d.on_disk.map(x=>[H(x.bytes),x.path,what(x.path)]));
right=panel('Build cache',[['size',H(d.build_cache)],['reclaimable',H(d.build_cache_reclaimable)]],d.build_cache_reclaimable>0?[{label:'Prune build cache',cls:'SAFE',run:()=>act({kind:'storage',name:'',act:'prune'},'Prune Docker build cache unused for 7 days',false,d.build_cache_reclaimable)}]:[])}
v.appendChild(split(left,right))}
// "What's inside" a container layer or volume, loaded on demand.
function insideBox(kind,name,title){const box=el('div');box.appendChild(el('h3',title));const p=el('p','loading...','muted');box.appendChild(p);
api('/api/inside?kind='+kind+'&name='+encodeURIComponent(name)).then(r=>{p.remove();if(r.error){box.appendChild(el('p','not available: '+r.error,'muted'));return}
box.appendChild(table(['size','folder'],r.rows.map(x=>[H(x.bytes),(x.path.split('/').length>2?'    ':'')+x.path])));if(r.advice)box.appendChild(el('p',r.advice,'warn'))});return box}
// ---- Logs
function logGroups(){const m=new Map();S.logs.forEach(l=>{const k=l.category+'\u001f'+l.owner;if(!m.has(k))m.set(k,{key:k,category:l.category,owner:l.owner,files:[],total:0,rotated:0,newest:0});const g=m.get(k);g.files.push(l);g.total+=l.bytes;if(l.rotated)g.rotated+=l.bytes;g.newest=Math.max(g.newest,l.mtime)});return[...m.values()].sort((a,b)=>b.total-a.total)}
function renderLogs(v){const groups=logGroups(),rows=[];const total=S.logs.reduce((a,l)=>a+l.bytes,0);
v.appendChild(el('p',H(total)+' in '+S.logs.length+' logs over 1 MiB, '+groups.length+' groups · journal '+H(S.journal_bytes)));
groups.forEach(g=>{const open=openGroups.has(g.key);rows.push({cls:'grp '+(sel==='g:'+g.key?'sel':''),onclick:()=>{open?openGroups.delete(g.key):openGroups.add(g.key);sel='g:'+g.key;render()},cells:[(open?'▾ ':'▸ ')+g.category,g.owner,H(g.total),g.files.length+' files',g.rotated?H(g.rotated)+' rotated':'',ago(g.newest)]});
if(open)g.files.forEach(l=>rows.push({cls:(sel==='l:'+l.path?'sel':'')+(l.rotated?' dim':''),onclick:()=>{sel='l:'+l.path;render()},cells:['    '+l.path.split('/').pop(),l.writer||'',H(l.bytes),l.protected?'protected':l.rotated?'rotated':'active','',ago(l.mtime)]}))});
const left=table(['belongs to','owner / writer','size','files','rotated','changed'],rows);let right=null;
if(sel&&sel.startsWith('g:')){const g=groups.find(x=>'g:'+x.key===sel);if(g){const dirs=[...new Set(g.files.map(l=>l.path.slice(0,l.path.lastIndexOf('/'))))];const writers=[...new Set(g.files.map(l=>l.writer).filter(Boolean))];
const b=[];if(g.files.some(l=>l.rotated&&!l.protected&&!l.writer))b.push({label:'Remove rotated copies',cls:'SAFE',run:()=>act({kind:'log-group',category:g.category,name:g.owner,act:'remove'},'Remove the rotated copies of '+g.category+' · '+g.owner,false,g.rotated)});
if(g.category==='Journal'&&S.journal_bytes>1073741824)b.push({label:'Vacuum journal to 1 GiB',cls:'SAFE',run:()=>act({kind:'journal',name:'',act:'vacuum'},'Shrink the systemd journal to 1 GiB',false,S.journal_bytes-1073741824)});
right=panel(g.category+' · '+g.owner,[['size',H(g.total)+' in '+g.files.length+' files'],['rotated',H(g.rotated)],['folders',dirs.slice(0,5).join('  ')],['written by',writers.join(', ')||'no process has these open now']],b)}}
if(sel&&sel.startsWith('l:')){const l=S.logs.find(x=>'l:'+x.path===sel);if(l){const pre=el('pre','loading last lines...');api('/api/logtail?path='+encodeURIComponent(l.path)).then(r=>{pre.textContent=r.lines?r.lines.join('\n'):(r.error||'')});
const b=[];if(!l.rotated&&!l.protected&&!l.binary&&l.bytes>10*1048576)b.push({label:'Truncate (keep last 10 MiB)',cls:'CAUTION',run:()=>act({kind:'log',name:l.path,act:'truncate'},'Truncate '+l.path+' in place (its last 10 MiB is kept aside)',false,l.bytes-10*1048576)});
if(l.rotated&&!l.protected&&!l.writer)b.push({label:'Remove this rotated copy',cls:'SAFE',run:()=>act({kind:'log',name:l.path,act:'remove'},'Remove rotated log '+l.path,false,l.bytes)});
right=panel(l.path.split('/').pop(),[['file',l.path],['belongs to',l.category+' · '+l.owner],['size',H(l.bytes)+' · changed '+ago(l.mtime)+' ago'],['state',l.protected?'security or accounting log: dux never touches it':l.binary?'binary journal: use the journal group':l.rotated?'rotated copy: SAFE to remove':'active log'],['writer',l.writer||'no process has it open now']],b,pre)}}
v.appendChild(split(left,right))}
// ---- Projects
function renderProjects(v){const m=new Map();S.projects.forEach(p=>{if(!m.has(p.location))m.set(p.location,[]);m.get(p.location).push(p)});const places=[...m.entries()].sort((a,b)=>b[1].reduce((s,p)=>s+p.total,0)-a[1].reduce((s,p)=>s+p.total,0));
const rows=[];places.forEach(([k,ps])=>{const closed=closedPlaces.has(k);rows.push({cls:'grp',onclick:()=>{closed?closedPlaces.delete(k):closedPlaces.add(k);render()},cells:[(closed?'▸ ':'▾ ')+k,ps.length+' projects',H(ps.reduce((s,p)=>s+p.total,0)),H(ps.reduce((s,p)=>s+p.build_output,0)),H(ps.reduce((s,p)=>s+p.git,0)),'','']});
if(!closed)ps.forEach(p=>rows.push({cls:sel==='p:'+p.path?'sel':'',onclick:()=>{sel='p:'+p.path;render()},cells:['    '+p.name,p.type,H(p.total),H(p.build_output),H(p.git),Math.floor((Date.now()/1000-p.last_activity)/86400)+'d',p.path]}))});
const left=table(['project','type','total','build/cache','git','idle','path'],rows);let right=null;const p=S.projects.find(x=>'p:'+x.path===sel);
if(p){const ex=el('div');ex.appendChild(el('h3','Build output and caches (rebuilt by the project tools)'));ex.appendChild(table(['folder','size'],p.build_output_dirs.map(([dd,b])=>[dd.slice(p.path.length+1)||dd,H(b)])));
ex.appendChild(el('p','Remove build output from the terminal UI (dux, key 6), or from Fixes for idle projects.','muted'));
right=panel(p.name,[['type',p.type],['about',p.description],['path',p.path],['git','branch '+(p.branch||'-')+' · '+(p.remote||'-')],['size','source '+H(p.source)+' · build output '+H(p.build_output)+' · git '+H(p.git)],['idle',Math.floor((Date.now()/1000-p.last_activity)/86400)+' days']],[],ex)}
v.appendChild(split(left,right));v.appendChild(el('h3','Apps and data'));v.appendChild(table(['size','app','path'],S.apps.map(a=>[H(a.bytes),a.app,a.path])))}
// ---- History
async function renderHistory(v){const r=await api('/api/history');if(tab!=='History')return;
const freed=r.rows.filter(h=>h.ok===null).reduce((a,h)=>a+h.bytes,0);v.appendChild(el('p','Every action dux took, newest first ('+r.file+'). Batches freed '+H(freed)+' measured on disk.'));
v.appendChild(table(['when','result','space','action','note'],r.rows.map(h=>[ago(h.t)+' ago',el('b',h.ok===null?'batch':h.ok?'done':'FAILED',h.ok===null?'muted':h.ok?'ok':'bad'),H(h.bytes),h.what,h.note])))}
// ---- Folders
async function renderFolder(v){const r=await api('/api/tree?path='+encodeURIComponent(folder));const top=r.children.length?r.children[0].bytes:1;
const crumbs=el('p');const up=el('a','⬑ up');up.onclick=()=>{folder=folder.split('/').slice(0,-1).join('/');render()};crumbs.appendChild(el('b',(folder||'/')+'  '));if(folder)crumbs.appendChild(up);v.appendChild(crumbs);
v.appendChild(table(['size','','entries','name'],r.children.map(c=>{const n=c.dirs?el('a',c.name+'/'):el('span',c.name);if(c.dirs)n.onclick=()=>{folder=(folder+'/'+c.name).replace('//','/');render()};return[H(c.bytes),barc(c.bytes/top),c.entries,n]})))}
// ---- Fixes
function renderFixes(v){const sums={};S.fixes.filter(f=>f.applicable).forEach(f=>{sums[f.group]=(sums[f.group]||0)+f.frees});
const g=Object.entries(sums).filter(x=>x[1]>0).sort((a,b)=>b[1]-a[1]);
v.appendChild(el('p','Can be cleaned: '+(g.length?g.map(([k,b])=>k+' '+H(b)).join(' · '):'nothing')+'. Read-only here: apply with dux fix or in dux ui (view 4).'));
const by={};S.fixes.forEach(f=>{(by[f.group]=by[f.group]||[]).push(f)});
Object.keys(by).sort((a,b)=>(sums[b]||0)-(sums[a]||0)).forEach(k=>{const d=el('details'),sm=el('summary',k+' · '+H(sums[k]||0)+' · '+by[k].length+' items');d.appendChild(sm);
const big=by[k].filter(f=>f.frees>=10485760||!f.applicable),small=by[k].filter(f=>f.frees<10485760&&f.applicable);
const rows=big.map(f=>({cls:f.never_touch?'muted':'',cells:[el('b',f.safety,f.safety),H(f.frees),(f.never_touch?'[never touch] ':'')+f.title,f.why+(f.manual?' By hand: '+f.manual:''),f.undo]}));
if(small.length)rows.push({cls:'muted',cells:['',H(small.reduce((a,f)=>a+f.frees,0)),small.length+' smaller items (under 10 MiB each)','','']});
d.appendChild(table(['rating','frees','what','what it does','undo'],rows));v.appendChild(d)})}
document.getElementById('refresh').onclick=async()=>{await api('/api/refresh',{method:'POST'});load()};
load();setInterval(load,300000);
</script></body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "ab"));
        assert_eq!(random_token().unwrap().len(), 32);
        assert_ne!(random_token().unwrap(), random_token().unwrap());
        assert_eq!(
            percent_decode("%2Fsrv%2Fcaf%C3%A9%20x+y%26"),
            "/srv/café x y&"
        );
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn query_params() {
        assert_eq!(
            query_param("token=x&path=/var", "path").as_deref(),
            Some("/var")
        );
        assert_eq!(query_param("token=x", "path"), None);
    }

    #[test]
    fn end_to_end_auth_and_state() {
        let srv = Server::new(
            std::env::temp_dir().to_string_lossy().into_owned(),
            "t0ken".into(),
            3600,
        );
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let s2 = srv.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten().take(10) {
                handle(&s2, c);
            }
        });
        let get = |path: &str, hdr: &str| {
            let mut c = TcpStream::connect(addr).unwrap();
            write!(c, "GET {path} HTTP/1.1\r\nHost: x\r\n{hdr}\r\n").unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        assert!(get("/api/state", "").starts_with("HTTP/1.1 401"));
        assert!(get("/api/state?token=wrong", "").starts_with("HTTP/1.1 401"));
        let ok = get("/api/state", "X-Dux-Token: t0ken\r\n");
        assert!(
            ok.starts_with("HTTP/1.1 200")
                && ok.contains("\"ready\":false")
                && ok.contains("\"disks\"")
        );
        let page = get("/?token=t0ken", "");
        assert!(page.contains("<title>dux</title>") && page.contains("X-Frame-Options: DENY"));
        // The API: same documents as the CLI, Prometheus with a bearer token.
        let disks = get("/api/v1/disks", "X-Dux-Token: t0ken\r\n");
        assert!(
            disks.starts_with("HTTP/1.1 200") && disks.contains("\"inode_pct\""),
            "{disks}"
        );
        let m = get("/metrics", "Authorization: Bearer t0ken\r\n");
        assert!(
            m.starts_with("HTTP/1.1 200") && m.contains("# TYPE dux_disk_used_bytes gauge"),
            "{m}"
        );
        assert!(get("/metrics", "").starts_with("HTTP/1.1 401"));
        let early = get("/api/v1/logs", "X-Dux-Token: t0ken\r\n");
        assert!(early.starts_with("HTTP/1.1 503"), "{early}");
        // Read-only: what used to apply fixes is gone.
        let post = |path: &str| {
            let mut c = TcpStream::connect(addr).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nHost: x\r\nX-Dux-Token: t0ken\r\nContent-Length: 2\r\n\r\n{{}}").unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        assert!(post("/api/apply").starts_with("HTTP/1.1 405"));
        assert!(post("/api/action").starts_with("HTTP/1.1 405"));
    }
}
