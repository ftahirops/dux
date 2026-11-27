//! `dux web`: a small local web dashboard over the same data as `dux ui`.
//!
//! - Listens on 127.0.0.1 by default; every request needs the access token
//!   (`?token=` for page loads, `X-Dux-Token` header for API calls).
//! - Read-only unless started with `--allow-actions`. Applying fixes is a
//!   POST that must carry the token in a header (so other web pages cannot
//!   trigger it), and RISKY items additionally need `"confirm": "delete"`.
//! - Data is gathered in the background on start, every `refresh` seconds,
//!   and on POST /api/refresh.

use crate::fix::Safety;
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
    pub allow_actions: bool,
    pub refresh_secs: u64,
    state: Mutex<Option<Arc<Gathered>>>,
    walking: AtomicBool,
    progress: Arc<AtomicU64>,
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

/// A stable identity for a fix: the same fix keeps its key across regathers,
/// a different fix never shares it, so a stale page cannot apply the wrong one.
pub fn fix_key(x: &crate::fix::Suggestion) -> String {
    let text = format!("{}|{:?}", x.title, x.item.as_ref().map(|i| &i.action));
    // FNV-1a, 64-bit.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
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
    pub fn new(root: String, token: String, allow_actions: bool, refresh_secs: u64) -> Arc<Server> {
        Arc::new(Server {
            root,
            token,
            allow_actions,
            refresh_secs,
            state: Mutex::new(None),
            walking: AtomicBool::new(false),
            progress: Arc::new(AtomicU64::new(0)),
        })
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
pub fn state_json(
    g: Option<&Gathered>,
    walking: bool,
    progress: u64,
    allow_actions: bool,
) -> Value {
    let disks: Vec<Value> = gather::disks()
        .into_iter()
        .map(|d| json!({"path": d.path, "fstype": d.fstype, "total": d.usage.total, "used": d.usage.used, "avail": d.usage.avail, "pct": d.usage.used_pct(), "inode_pct": d.usage.inode_pct()}))
        .collect();
    let Some(g) = g else {
        return json!({"ready": false, "walking": walking, "progress": progress, "disks": disks, "allow_actions": allow_actions});
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
    let logs: Vec<Value> = g.logs.iter().take(200).map(|l| json!({"path": l.path, "bytes": l.bytes, "kind": l.kind.label(), "rotated": l.rotated, "protected": l.protected, "writer": l.writer, "container": l.container, "mtime": l.mtime})).collect();
    let projects: Vec<Value> = g.projects.iter().map(|p| json!({"path": p.path, "kinds": p.kinds, "total": p.total, "build_output": p.artifacts, "git": p.git, "last_activity": p.last_activity})).collect();
    let apps: Vec<Value> = g
        .apps
        .iter()
        .map(|(n, p, b, t)| json!({"app": n, "path": p, "bytes": b, "changed": t}))
        .collect();
    let docker = g.docker.as_ref().map(|d| {
        json!({
            "images_total": d.images_total(), "writable_total": d.writable_total(), "logs_total": d.logs_total(),
            "volumes_total": d.volumes_total(), "build_cache": d.build_cache,
            "containers": d.containers.iter().map(|c| json!({"name": c.name, "state": c.state, "writable": c.writable, "logs": c.log_bytes, "project": c.project})).collect::<Vec<_>>(),
            "images": d.images.iter().map(|i| json!({"name": i.name, "unique": i.unique, "containers": i.containers})).collect::<Vec<_>>(),
            "volumes": d.volumes.iter().map(|v| json!({"name": v.name, "size": v.size, "links": v.links, "project": v.project})).collect::<Vec<_>>(),
            "on_disk": d.on_disk.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
        })
    });
    let fixes: Vec<Value> = g
        .fixes
        .iter()
        .enumerate()
        .map(|(i, x)| json!({"id": i, "key": fix_key(x), "safety": x.safety.label(), "title": x.title, "why": x.why, "undo": x.undo, "frees": x.frees, "applicable": x.item.is_some(), "manual": x.manual}))
        .collect();
    json!({
        "ready": true, "walking": walking, "progress": progress, "allow_actions": allow_actions, "at": g.at,
        "disks": disks, "folders": folders, "logs": logs, "journal_bytes": g.journal_bytes,
        "biggest": g.biggest.iter().map(|f| json!({"path": f.path, "bytes": f.bytes, "mtime": f.mtime})).collect::<Vec<_>>(),
        "projects": projects, "apps": apps, "docker": docker, "fixes": fixes, "errors": g.errors,
    })
}

/// Apply picked fixes by key. RISKY ones need `confirm == "delete"`. A key
/// that is no longer in the current list is refused, never re-matched.
pub fn apply_keys(g: &Gathered, keys: &[String], confirm: Option<&str>) -> Value {
    let mut results = Vec::new();
    let risky_ok = confirm == Some("delete");
    for k in keys {
        let Some((i, x)) = g.fixes.iter().enumerate().find(|(_, x)| fix_key(x) == *k) else {
            results.push(json!({"key": k, "ok": false, "error": "this fix is no longer offered; reload the page"}));
            continue;
        };
        let Some(item) = &x.item else {
            results.push(json!({"id": i, "ok": false, "error": "must be done by hand"}));
            continue;
        };
        if x.safety == Safety::Risky && !risky_ok {
            results.push(json!({"id": i, "ok": false, "error": "RISKY: confirm with delete"}));
            continue;
        }
        let r = crate::cleanup::apply(item);
        results.push(json!({"id": i, "ok": r.is_ok(), "title": x.title, "frees": x.frees, "error": r.err().map(|e| format!("{e:#}"))}));
    }
    json!({"results": results})
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
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
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
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
        body,
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
    let header_token = req
        .headers
        .iter()
        .find(|(k, _)| k == "x-dux-token")
        .map(|(_, v)| v.clone());
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
                    srv.allow_actions,
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
        ("POST", "/api/apply") if header_token.is_some() => {
            if !srv.allow_actions {
                respond(
                    &mut s,
                    "403 Forbidden",
                    "application/json",
                    br#"{"error":"start dux web with --allow-actions to apply fixes"}"#,
                );
                return;
            }
            let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let keys: Vec<String> = v["keys"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let Some(g) = srv.snapshot() else {
                respond(
                    &mut s,
                    "409 Conflict",
                    "application/json",
                    br#"{"error":"data not ready"}"#,
                );
                return;
            };
            let out = apply_keys(&g, &keys, v["confirm"].as_str());
            srv.regather();
            api_json(&mut s, out);
        }
        ("POST", _) => respond(
            &mut s,
            "403 Forbidden",
            "text/plain",
            b"POST requests need the X-Dux-Token header\n",
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
:root{--bg:#f4f6f5;--panel:#fff;--fg:#1b2420;--muted:#5b6a63;--line:#d7ded9;--accent:#0f6b5a;--safe:#0f6b5a;--caution:#8a5a00;--risky:#a3352b}
@media (prefers-color-scheme:dark){:root{--bg:#121715;--panel:#19201d;--fg:#e3eae6;--muted:#98a69f;--line:#2c3632;--accent:#5cc4a8;--safe:#6fd1b4;--caution:#e6b45c;--risky:#f08b80}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,sans-serif}
header{padding:14px 18px;border-bottom:1px solid var(--line);display:flex;gap:16px;align-items:center;flex-wrap:wrap}
h1{font-size:18px;margin:0}nav button{background:none;border:1px solid var(--line);color:var(--fg);padding:6px 12px;border-radius:99px;cursor:pointer}
nav button.on{background:var(--accent);color:var(--bg);border-color:var(--accent)}main{padding:16px 18px;max-width:1200px}
table{border-collapse:collapse;width:100%;background:var(--panel);border:1px solid var(--line)}th,td{text-align:left;padding:6px 10px;border-top:1px solid var(--line);vertical-align:top}
th{font-size:12px;color:var(--muted);text-transform:uppercase;border-top:0}td.n{font-variant-numeric:tabular-nums;white-space:nowrap}
.bar{display:inline-block;height:10px;background:var(--accent);border-radius:2px;vertical-align:middle}.muted{color:var(--muted)}
.SAFE{color:var(--safe)}.CAUTION{color:var(--caution)}.RISKY{color:var(--risky)}.box{overflow-x:auto}
#actions{margin:12px 0;display:flex;gap:8px;flex-wrap:wrap;align-items:center}#actions button{padding:6px 12px;cursor:pointer}a{color:var(--accent);cursor:pointer}
</style></head><body>
<header><h1>dux</h1><nav id="tabs"></nav><span id="status" class="muted"></span><button id="refresh">Refresh</button></header>
<main><div id="view"></div></main>
<script>
const token=new URLSearchParams(location.search).get('token')||'';
let S=null,tab='Disks',folder='',picked=new Set();
const tabs=['Disks','Folders','Biggest','Docker','Logs','Projects','Fixes'];
const H=b=>{if(b==null)return'-';const u=['B','KiB','MiB','GiB','TiB'];let i=0;while(b>=1024&&i<4){b/=1024;i++}return(i?b.toFixed(2):b)+' '+u[i]};
const el=(t,txt,cls)=>{const e=document.createElement(t);if(txt!=null)e.textContent=txt;if(cls)e.className=cls;return e};
function table(head,rows){const box=el('div',null,'box'),t=el('table'),tr=el('tr');head.forEach(h=>tr.appendChild(el('th',h)));t.appendChild(tr);rows.forEach(r=>{const row=el('tr');r.forEach(c=>{const td=el('td');if(c instanceof Node)td.appendChild(c);else{td.textContent=c==null?'':String(c);if(/^[\d.,]+ ?(B|KiB|MiB|GiB|TiB|%)$/.test(td.textContent))td.className='n'}row.appendChild(td)});t.appendChild(row)});box.appendChild(t);return box}
function barc(f){const s=el('span',null,'bar');s.style.width=Math.max(1,Math.round(f*120))+'px';return s}
async function api(p,opt){const r=await fetch(p,Object.assign({headers:{'X-Dux-Token':token,'Content-Type':'application/json'}},opt||{}));return r.json()}
async function load(){S=await api('/api/state');render();if(!S.ready||S.walking)setTimeout(load,2000)}
function render(){const nav=document.getElementById('tabs');nav.textContent='';tabs.forEach(t=>{const b=el('button',t,t===tab?'on':'');b.onclick=()=>{tab=t;render()};nav.appendChild(b)});
document.getElementById('status').textContent=S.walking?('walking... '+S.progress+' entries'):(S.at?('as of '+new Date(S.at*1000).toLocaleTimeString()):'');
const v=document.getElementById('view');v.textContent='';
if(tab==='Disks'){v.appendChild(table(['mount','type','size','free','used','inodes'],S.disks.map(d=>[d.path,d.fstype,H(d.total),H(d.avail),(()=>{const s=el('span');s.appendChild(barc(d.pct/100));s.appendChild(document.createTextNode(' '+d.pct.toFixed(1)+'%'));return s})(),d.inode_pct==null?'-':d.inode_pct.toFixed(1)+'%'])));return}
if(!S.ready){v.appendChild(el('p','Walking the filesystem... '+S.progress+' entries so far.','muted'));return}
if(tab==='Folders'){renderFolder(v);return}
if(tab==='Biggest'){v.appendChild(table(['size','path'],S.biggest.map(f=>[H(f.bytes),f.path])));return}
if(tab==='Docker'){if(!S.docker){v.appendChild(el('p','Docker is not available on this host.','muted'));return}const d=S.docker;
v.appendChild(el('p','images '+H(d.images_total)+' · containers '+H(d.writable_total)+' · logs '+H(d.logs_total)+' · volumes '+H(d.volumes_total)+' · build cache '+H(d.build_cache)));
v.appendChild(el('h3','On disk'));v.appendChild(table(['size','path'],d.on_disk.map(x=>[H(x.bytes),x.path])));
v.appendChild(el('h3','Containers'));v.appendChild(table(['writable','logs','name','state','project'],d.containers.map(c=>[H(c.writable),H(c.logs),c.name,c.state,c.project||''])));
v.appendChild(el('h3','Images'));v.appendChild(table(['unique','name','used by'],d.images.map(i=>[H(i.unique),i.name,i.containers?i.containers+' container(s)':'UNUSED'])));
v.appendChild(el('h3','Volumes'));v.appendChild(table(['size','name','used by','project'],d.volumes.map(x=>[H(x.size),x.name,x.links?x.links+' container(s)':'UNUSED',x.project||''])));return}
if(tab==='Logs'){v.appendChild(el('p','journal '+H(S.journal_bytes)));v.appendChild(table(['size','kind','path','note'],S.logs.map(l=>[H(l.bytes),l.kind,l.path,[l.rotated?'rotated':'',l.protected?'protected':'',l.container||'',l.writer?'written by '+l.writer:''].filter(Boolean).join(' · ')])));return}
if(tab==='Projects'){const now=Date.now()/1000;v.appendChild(table(['total','build output','git','idle','kind','path'],S.projects.map(p=>[H(p.total),H(p.build_output),H(p.git),Math.floor((now-p.last_activity)/86400)+'d',p.kinds.join(','),p.path])));v.appendChild(el('h3','Apps and data'));v.appendChild(table(['size','app','path'],S.apps.map(a=>[H(a.bytes),a.app,a.path])));return}
if(tab==='Fixes')renderFixes(v)}
async function renderFolder(v){const r=await api('/api/tree?path='+encodeURIComponent(folder));const top=r.children.length?r.children[0].bytes:1;
const crumbs=el('p');const up=el('a','⬑ up');up.onclick=()=>{folder=folder.split('/').slice(0,-1).join('/');render()};crumbs.appendChild(el('b',(folder||'/')+'  '));if(folder)crumbs.appendChild(up);v.appendChild(crumbs);
v.appendChild(table(['size','','entries','name'],r.children.map(c=>{const n=c.dirs?el('a',c.name+'/'):el('span',c.name);if(c.dirs)n.onclick=()=>{folder=(folder+'/'+c.name).replace('//','/');render()};return[H(c.bytes),barc(c.bytes/top),c.entries,n]})))}
function renderFixes(v){const tot={SAFE:0,CAUTION:0,RISKY:0};S.fixes.forEach(f=>tot[f.safety]+=f.frees);
v.appendChild(el('p','SAFE '+H(tot.SAFE)+' · CAUTION '+H(tot.CAUTION)+' · RISKY '+H(tot.RISKY)));
const bar=el('div');bar.id='actions';const sum=el('span','','muted');const upd=()=>{let b=0;S.fixes.forEach(f=>{if(picked.has(f.id))b+=f.frees});sum.textContent=picked.size+' picked, '+H(b)};
const pick=(f)=>{picked.clear();S.fixes.forEach(x=>{if(x.applicable&&f(x))picked.add(x.id)});render()};
[['Select all SAFE',x=>x.safety==='SAFE'],['Select SAFE + CAUTION',x=>x.safety!=='RISKY'],['Clear',x=>false]].forEach(([t,f])=>{const b=el('button',t);b.onclick=()=>pick(f);bar.appendChild(b)});
const go=el('button','Apply picked');go.disabled=!S.allow_actions;go.title=S.allow_actions?'':'start dux web with --allow-actions';go.onclick=apply;bar.appendChild(go);bar.appendChild(sum);v.appendChild(bar);
v.appendChild(table(['','rating','frees','fix','what it does','undo'],S.fixes.map(f=>{const c=el('input');c.type='checkbox';c.disabled=!f.applicable;c.checked=picked.has(f.id);c.onchange=()=>{c.checked?picked.add(f.id):picked.delete(f.id);upd()};return[c,el('b',f.safety,f.safety),H(f.frees),f.title,f.why+(f.manual?' By hand: '+f.manual:''),f.undo]})));upd()}
async function apply(){const ids=[...picked];if(!ids.length)return;let b=0,risky=false;S.fixes.forEach(f=>{if(picked.has(f.id)){b+=f.frees;risky=risky||f.safety==='RISKY'}});
let confirmWord=null;if(risky){confirmWord=prompt('Your selection includes RISKY items that can lose data. Type delete to include them:');if(confirmWord!=='delete'){alert('Nothing applied.');return}}
else if(!confirm('Apply '+ids.length+' fixes, freeing about '+H(b)+'?'))return;
const keys=S.fixes.filter(f=>picked.has(f.id)).map(f=>f.key);
const r=await api('/api/apply',{method:'POST',body:JSON.stringify({keys,confirm:confirmWord})});
alert(r.error?r.error:r.results.map(x=>(x.ok?'done  ':'skipped  ')+(x.title||x.id)+(x.error?' ('+x.error+')':'')).join('\n'));picked.clear();setTimeout(load,1500)}
document.getElementById('refresh').onclick=async()=>{await api('/api/refresh',{method:'POST'});load()};
load();
</script></body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::{Action, Item};

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
    fn apply_refuses_risky_without_delete_and_manual_items() {
        let d = std::env::temp_dir().join(format!("dux-web-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("big.iso");
        std::fs::write(&f, b"x").unwrap();
        let g = Gathered {
            fixes: vec![
                crate::fix::delete_file(f.to_str().unwrap(), "test", 1),
                crate::fix::risky("held".into(), "w", "restart".into(), 5),
            ],
            ..Gathered::default()
        };
        let keys: Vec<String> = g
            .fixes
            .iter()
            .map(fix_key)
            .chain(["0000000000000000".to_string()])
            .collect();
        assert_ne!(keys[0], keys[1], "different fixes, different keys");
        let r = apply_keys(&g, &keys, None);
        let res = r["results"].as_array().unwrap();
        assert_eq!(res.len(), 3);
        assert!(res.iter().all(|x| x["ok"] == false));
        assert!(f.exists(), "RISKY file kept without delete confirmation");
        let _ = Item {
            level: 0,
            what: String::new(),
            action: Action::Remove { path: f.clone() },
            frees: 0,
        };
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn end_to_end_auth_and_state() {
        let srv = Server::new(
            std::env::temp_dir().to_string_lossy().into_owned(),
            "t0ken".into(),
            false,
            3600,
        );
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let s2 = srv.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten().take(4) {
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
    }
}
