//! Machine-readable (`--json`) output for the read commands. JSON is emitted to
//! stdout as a single pretty-printed value so it pipes cleanly into `jq`. Paths
//! are already lossy-UTF-8 Strings from the query layer; serde escapes them, so
//! no terminal-control sanitising is needed here (that's only for TTY output).

use crate::{classify, containers, deleted, query};
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
                "uid": r.uid,
            })
        })
        .collect();
    print(&json!(v));
}

fn insight_value(row: &query::Row, info: &classify::Insight) -> Value {
    json!({
        "path": row.path,
        "bytes": row.size,
        "inodes": row.inodes,
        "mtime": row.mtime,
        "kind": kind_str(row.kind),
        "uid": row.uid,
        "classification": {
            "type": info.file_type,
            "belongs_to": info.belongs_to,
            "purpose": info.purpose,
            "importance": info.importance,
            "safety": info.safety.label(),
            "reason": info.reason,
            "recommended_action": info.action,
        }
    })
}

pub fn classified_rows(rows: &[query::Row]) {
    let values: Vec<Value> = rows
        .iter()
        .map(|row| insight_value(row, &classify::classify(row)))
        .collect();
    print(&json!(values));
}

pub fn explain(row: &query::Row, info: &classify::Insight) {
    print(&insight_value(row, info));
}

pub fn growth(rows: &[query::GrowthRow]) {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "path": r.path, "delta_bytes": r.delta }))
        .collect();
    print(&json!(v));
}

pub fn overview(
    dirs: &[query::Row],
    files: &[query::Row],
    growth: &[query::GrowthRow],
    changes: &[query::GrowthRow],
    waste: &containers::Waste,
    window_secs: i64,
) {
    let indexed = |rows: &[query::Row]| -> Vec<Value> {
        rows.iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "bytes": r.size,
                    "inodes": r.inodes,
                    "mtime": r.mtime,
                    "kind": kind_str(r.kind),
                    "uid": r.uid,
                })
            })
            .collect()
    };
    let deltas = |rows: &[query::GrowthRow]| -> Vec<Value> {
        rows.iter()
            .map(|r| json!({ "path": r.path, "delta_bytes": r.delta }))
            .collect()
    };
    print(&json!({
        "window_seconds": window_secs,
        "largest_directories": indexed(dirs),
        "largest_files": indexed(files),
        "fastest_growth": deltas(growth),
        "biggest_changes": deltas(changes),
        "docker_reclaimable": {
            "stopped_bytes": waste.stopped_bytes,
            "orphan_volume_bytes": waste.orphan_volume_bytes,
            "orphan_volume_count": waste.orphan_volume_count,
            "build_cache_bytes": waste.build_cache_bytes,
            "total_bytes": waste.total(),
        },
    }));
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

pub fn containers(rows: &[containers::ContainerRow], waste: &containers::Waste) {
    let v: Vec<Value> = rows
        .iter()
        .map(|c| {
            json!({
                "runtime": c.runtime,
                "id": c.id,
                "name": c.name,
                "image": c.image,
                "running": c.running,
                "state": c.state(),
                "writable_bytes": c.writable_bytes,
                "log_bytes": c.log_bytes,
                "volume_bytes": c.volume_bytes,
                "total_bytes": c.total(),
                "reclaimable_bytes": c.reclaimable(),
            })
        })
        .collect();
    print(&json!({
        "containers": v,
        "reclaimable": {
            "stopped_bytes": waste.stopped_bytes,
            "orphan_volume_bytes": waste.orphan_volume_bytes,
            "orphan_volume_count": waste.orphan_volume_count,
            "build_cache_bytes": waste.build_cache_bytes,
            "total_bytes": waste.total(),
        },
    }));
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
