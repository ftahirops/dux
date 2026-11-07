"""Read-only schema compatibility checks for the real-fanotify harness."""

from pathlib import Path
import sqlite3


class UnsupportedLiveTestSchema(RuntimeError):
    """Raised before the workload when the candidate lacks required schema."""


def require_live_test_schema(db: Path) -> int:
    """Require the schema features used by exact growth/inode assertions."""
    with sqlite3.connect(f"file:{Path(db)}?mode=ro", uri=True, timeout=2) as conn:
        conn.execute("BEGIN")
        meta = dict(conn.execute("SELECT key, value FROM meta"))
        schema_version = int(meta.get("schema_version", "0"))
        objects = {
            name: kind
            for kind, name in conn.execute(
                "SELECT type, name FROM sqlite_master WHERE type IN ('table','view')"
            )
        }
        inode_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(inodes)")
        }
    path_columns = {row[1] for row in conn.execute("PRAGMA table_info(paths)")}
    total_columns = {row[1] for row in conn.execute("PRAGMA table_info(dir_totals)")}

    missing = []
    if schema_version < 7:
        missing.append("schema version 7")
    if objects.get("growth_current") != "view":
        missing.append("growth_current view")
    if "birth_ns" not in inode_columns:
        missing.append("inodes.birth_ns column")
    if schema_version >= 7:
        missing_path_columns = {
            "path_id", "parent_path_id", "name", "dev_id", "inode", "birth_ns",
            "kind", "mount_id", "prime", "cycle",
        } - path_columns
        missing_total_columns = {
            "path_id", "visible_bytes", "visible_entries", "unique_bytes",
            "unique_inodes",
        } - total_columns
        if missing_path_columns:
            missing.append("paths columns: " + ", ".join(sorted(missing_path_columns)))
        if missing_total_columns:
            missing.append("dir_totals columns: " + ", ".join(sorted(missing_total_columns)))
    if missing:
        details = ", ".join(missing)
        raise UnsupportedLiveTestSchema(
            f"real-fanotify harness requires current index schema features; "
            f"binary created schema_version={schema_version}, missing: {details}"
        )
    return schema_version
