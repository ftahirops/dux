//! Machine-readable (`--json`) output for the read commands. JSON is emitted to
//! stdout as a single pretty-printed value so it pipes cleanly into `jq`. Paths
//! are already lossy-UTF-8 Strings from the query layer; serde escapes them, so
//! no terminal-control sanitising is needed here (that's only for TTY output).

use crate::{containers, deleted, query};
use serde_json::{json, Value};

fn kind_str(k: char) -> &'static str {
    match k {
        'd' => "dir",
        'f' => "file",
        'l' => "symlink",
        _ => "other",
    }
}

pub fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

pub fn rows(rows: &[query::Row]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "path": r.path,
                "bytes": r.size,
                "inodes": r.inodes,
                "mtime": r.mtime,
                "kind": kind_str(r.kind),
            })
        })
        .collect();
    print(&json!(v));
}

pub fn growth(rows: &[query::GrowthRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "path": r.path, "delta_bytes": r.delta }))
        .collect();
    print(&json!(v));
}

pub fn owners(rows: &[query::OwnerRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "uid": r.uid, "bytes": r.bytes, "files": r.files }))
        .collect();
    print(&json!(v));
}

pub fn exts(rows: &[query::ExtRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "ext": r.ext, "bytes": r.bytes, "files": r.files }))
        .collect();
    print(&json!(v));
}

pub fn du(rows: &[query::DuRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "path": r.path,
                "bytes": r.bytes,
                "kind": if r.is_dir { "dir" } else { "file" },
                "mtime": r.mtime,
            })
        })
        .collect();
    print(&json!(v));
}

pub fn containers(rows: &[containers::ContainerRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|c| {
            json!({
                "runtime": c.runtime,
                "id": c.id,
                "name": c.name,
                "image": c.image,
                "running": c.running,
                "writable_bytes": c.writable_bytes,
                "log_bytes": c.log_bytes,
                "volume_bytes": c.volume_bytes,
                "total_bytes": c.total(),
            })
        })
        .collect();
    print(&json!(v));
}

pub fn deleted_open(rows: &[deleted::DeletedOpen]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "pid": r.pid,
                "process": r.process,
                "uid": r.uid,
                "bytes": r.size,
                "path": r.path,
            })
        })
        .collect();
    print(&json!(v));
}
