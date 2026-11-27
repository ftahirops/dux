//! Machine-readable (`--json`) output for the read commands. JSON is emitted to
//! stdout as a single pretty-printed value so it pipes cleanly into `jq`. Query
//! paths are safe UTF-8 display forms: invalid filename bytes and literal
//! backslashes are escaped, and serde handles JSON control-character escaping.

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
    println!("{}", render(v));
}

fn render(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("JSON values serialize")
}

pub fn rows(rows: &[query::Row]) {
    print(&rows_value(rows));
}

fn rows_value(rows: &[query::Row]) -> Value {
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
    json!(v)
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
    print(&classified_rows_value(rows));
}

fn classified_rows_value(rows: &[query::Row]) -> Value {
    let values: Vec<Value> = rows
        .iter()
        .map(|row| insight_value(row, &classify::classify(row)))
        .collect();
    json!(values)
}

pub fn explain(row: &query::Row, info: &classify::Insight) {
    print(&insight_value(row, info));
}

pub fn growth(rows: &[query::GrowthRow]) {
    print(&growth_value(rows));
}

fn growth_value(rows: &[query::GrowthRow]) -> Value {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "path": r.path, "delta_bytes": r.delta }))
        .collect();
    json!(v)
}

pub fn overview(
    dirs: &[query::Row],
    files: &[query::Row],
    growth: &[query::GrowthRow],
    changes: &[query::GrowthRow],
    waste: &containers::Waste,
    window_secs: i64,
) {
    print(&overview_value(
        dirs,
        files,
        growth,
        changes,
        waste,
        window_secs,
    ));
}

fn overview_value(
    dirs: &[query::Row],
    files: &[query::Row],
    growth: &[query::GrowthRow],
    changes: &[query::GrowthRow],
    waste: &containers::Waste,
    window_secs: i64,
) -> Value {
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
    json!({
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
    })
}

pub fn owners(rows: &[query::OwnerRow]) {
    print(&owners_value(rows));
}

fn owners_value(rows: &[query::OwnerRow]) -> Value {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "uid": r.uid, "bytes": r.bytes, "files": r.files }))
        .collect();
    json!(v)
}

pub fn exts(rows: &[query::ExtRow]) {
    print(&exts_value(rows));
}

fn exts_value(rows: &[query::ExtRow]) -> Value {
    let v: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "ext": r.ext, "bytes": r.bytes, "files": r.files }))
        .collect();
    json!(v)
}

pub fn du(rows: &[query::DuRow]) {
    print(&du_value(rows));
}

fn du_value(rows: &[query::DuRow]) -> Value {
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
    json!(v)
}

pub fn containers(rows: &[containers::ContainerRow], waste: &containers::Waste) {
    print(&containers_value(rows, waste));
}

fn containers_value(rows: &[containers::ContainerRow], waste: &containers::Waste) -> Value {
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
    json!({
        "containers": v,
        "reclaimable": {
            "stopped_bytes": waste.stopped_bytes,
            "orphan_volume_bytes": waste.orphan_volume_bytes,
            "orphan_volume_count": waste.orphan_volume_count,
            "build_cache_bytes": waste.build_cache_bytes,
            "total_bytes": waste.total(),
        },
    })
}

pub fn deleted_open(rows: &[deleted::DeletedOpen]) {
    print(&deleted_open_value(rows));
}

fn deleted_open_value(rows: &[deleted::DeletedOpen]) -> Value {
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
    json!(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, kind: char) -> query::Row {
        query::Row {
            path: path.to_owned(),
            size: 4096,
            inodes: 2,
            mtime: 123,
            kind,
            uid: 1000,
        }
    }

    #[test]
    fn json_output_builders_preserve_cli_fields_and_values() {
        let rows = vec![
            row("/tree", 'd'),
            row("/tree/file", 'f'),
            row("/tree/link", 'l'),
            row("/tree/device", 'o'),
        ];
        let encoded_rows = rows_value(&rows);
        assert_eq!(encoded_rows[0]["kind"], "dir");
        assert_eq!(encoded_rows[1]["kind"], "file");
        assert_eq!(encoded_rows[2]["kind"], "symlink");
        assert_eq!(encoded_rows[3]["kind"], "other");
        assert_eq!(encoded_rows[1]["bytes"], 4096);
        assert_eq!(encoded_rows[1]["uid"], 1000);

        let classified = classified_rows_value(&[row("/tmp/unknown", 'f')]);
        assert!(classified[0]["classification"]["safety"].is_string());
        let explain_row = row("/var/lib/postgresql/data", 'f');
        let explanation = insight_value(&explain_row, &classify::classify(&explain_row));
        assert_eq!(explanation["classification"]["safety"], "DO NOT DELETE");
        assert!(explanation["classification"]["recommended_action"].is_string());

        let changes = vec![query::GrowthRow {
            path: "/var/log/app.log".into(),
            delta: 8192,
        }];
        assert_eq!(growth_value(&changes)[0]["delta_bytes"], 8192);

        let waste = containers::Waste {
            stopped_bytes: 8,
            orphan_volume_bytes: 16,
            orphan_volume_count: 2,
            build_cache_bytes: 32,
        };
        let overview = overview_value(
            &[row("/tree", 'd')],
            &[row("/tree/file", 'f')],
            &changes,
            &changes,
            &waste,
            3600,
        );
        assert_eq!(overview["window_seconds"], 3600);
        assert_eq!(overview["docker_reclaimable"]["total_bytes"], 56);
        assert_eq!(overview["fastest_growth"][0]["path"], "/var/log/app.log");

        assert_eq!(
            owners_value(&[query::OwnerRow {
                uid: 1000,
                bytes: 4096,
                files: 2,
            }])[0]["files"],
            2
        );
        assert_eq!(
            exts_value(&[query::ExtRow {
                ext: ".log".into(),
                bytes: 8192,
                files: 1,
            }])[0]["ext"],
            ".log"
        );
        assert_eq!(
            du_value(&[query::DuRow {
                path: "/tree".into(),
                bytes: 8192,
                is_dir: true,
                mtime: 123,
            }])[0]["kind"],
            "dir"
        );

        let containers = containers_value(
            &[containers::ContainerRow {
                runtime: "docker",
                id: "id".into(),
                name: "web".into(),
                image: "example/web".into(),
                running: false,
                writable_bytes: 100,
                log_bytes: 20,
                volume_bytes: 5,
            }],
            &waste,
        );
        assert_eq!(containers["containers"][0]["state"], "stopped");
        assert_eq!(containers["containers"][0]["reclaimable_bytes"], 120);

        let deleted = deleted_open_value(&[deleted::DeletedOpen {
            pid: 42,
            process: "worker".into(),
            uid: 1000,
            size: 2048,
            path: "/tmp/gone".into(),
        }]);
        assert_eq!(deleted[0]["pid"], 42);
        assert_eq!(deleted[0]["bytes"], 2048);
        assert_eq!(render(&deleted).parse::<Value>().unwrap(), deleted);
    }
}
