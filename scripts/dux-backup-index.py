#!/usr/bin/env python3
"""Create and verify a no-clobber SQLite online backup of a Dux index."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import signal
import sqlite3
import stat
import subprocess
import sys
import shutil
import tempfile
from urllib.parse import quote


CHUNK_BYTES = 1024 * 1024
BACKUP_PAGES = 1024
MIN_FREE_RESERVE_BYTES = 1024 * 1024 * 1024
SUPPORTED_SCHEMA_VERSIONS = frozenset({4, 5, 6, 7})
REQUIRED_INDEX_COLUMNS = {
    "meta": {"key", "value"},
    "inodes": {"dev_id", "inode", "kind", "blocks"},
    "dirents": {"parent_dev", "parent_inode", "name", "dev_id", "inode"},
    "growth": {"bucket", "dev_id", "inode", "delta"},
}
_INTERRUPTED_SIGNAL: int | None = None


def _record_interrupt(signum: int, _frame: object) -> None:
    """Defer SIGINT/SIGTERM until SQLite and temporary files are cleaned up."""
    global _INTERRUPTED_SIGNAL
    _INTERRUPTED_SIGNAL = signum


def _check_interrupted(operation: str = "backup") -> None:
    if _INTERRUPTED_SIGNAL is not None:
        name = signal.Signals(_INTERRUPTED_SIGNAL).name
        raise InterruptedError(f"{operation} interrupted by {name}")


def _validate_dux_schema(connection: sqlite3.Connection, schema_version: int) -> None:
    if schema_version not in SUPPORTED_SCHEMA_VERSIONS:
        supported = ", ".join(
            str(version) for version in sorted(SUPPORTED_SCHEMA_VERSIONS)
        )
        raise ValueError(
            f"unsupported Dux index schema v{schema_version}; expected one of: {supported}"
        )

    required_columns = {
        name: set(columns) for name, columns in REQUIRED_INDEX_COLUMNS.items()
    }
    if schema_version >= 5:
        required_columns["inodes"].add("birth_ns")
    if schema_version >= 6:
        required_columns["growth"].add("birth_ns")
    if schema_version >= 7:
        required_columns["paths"] = {
            "path_id", "parent_path_id", "name", "dev_id", "inode",
            "birth_ns", "kind", "mount_id", "prime", "cycle",
        }
        required_columns["dir_totals"] = {
            "path_id", "visible_bytes", "visible_entries", "unique_bytes",
            "unique_inodes",
        }

    tables = {
        name: (sql or "")
        for name, sql in connection.execute(
            "SELECT name,sql FROM sqlite_master WHERE type='table'"
        )
    }
    missing_tables = sorted(set(required_columns) - tables.keys())
    if missing_tables:
        raise ValueError(
            "SQLite database is missing required Dux table(s): "
            + ", ".join(missing_tables)
        )

    for table, required in required_columns.items():
        if tables[table].lstrip().upper().startswith("CREATE VIRTUAL TABLE"):
            raise ValueError(f"required Dux table {table!r} must be a regular SQLite table")
        columns = {
            row[1] for row in connection.execute(f'PRAGMA table_info("{table}")')
        }
        missing_columns = sorted(required - columns)
        if missing_columns:
            raise ValueError(
                f"Dux table {table!r} is missing required column(s): "
                + ", ".join(missing_columns)
            )


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _validate_source(path: Path) -> Path:
    try:
        info = path.lstat()
    except FileNotFoundError as error:
        raise ValueError(f"index database does not exist: {path}") from error
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise ValueError(f"index database must be a regular, non-symlink file: {path}")
    canonical = path.resolve(strict=True)
    for suffix in ("-wal", "-shm", "-journal"):
        sidecar = Path(f"{canonical}{suffix}")
        try:
            sidecar_info = sidecar.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(sidecar_info.st_mode) or not stat.S_ISREG(sidecar_info.st_mode):
            raise ValueError(f"SQLite sidecar must be a regular, non-symlink file: {sidecar}")
    return canonical


def _validate_output(path: Path, source: Path) -> tuple[Path, Path]:
    parent = path.parent.resolve(strict=True)
    parent_info = parent.stat()
    if not stat.S_ISDIR(parent_info.st_mode):
        raise ValueError(f"backup parent is not a directory: {parent}")
    if parent_info.st_uid != os.geteuid() or parent_info.st_mode & 0o022:
        raise ValueError(
            "backup parent must be owned by the current user and not group/world writable: "
            f"{parent}"
        )
    output = parent / path.name
    manifest = Path(f"{output}.json")
    if output == source or output in {
        Path(f"{source}-wal"), Path(f"{source}-shm"), Path(f"{source}-journal")
    }:
        raise ValueError("backup output must not replace the database or a SQLite sidecar")
    for candidate in (output, manifest):
        if candidate.exists() or candidate.is_symlink():
            raise FileExistsError(f"refusing to overwrite existing backup artifact: {candidate}")
    return output, manifest


def _exclusive_temp(parent: Path, prefix: str) -> tuple[int, Path]:
    fd, name = tempfile.mkstemp(prefix=prefix, dir=parent)
    try:
        os.fchmod(fd, 0o600)
    except BaseException:
        try:
            os.close(fd)
        except OSError:
            pass
        try:
            os.unlink(name)
        except FileNotFoundError:
            pass
        raise
    return fd, Path(name)


def _publish_no_clobber(temporary: Path, destination: Path) -> None:
    # A hard link is atomic and fails if another process created the destination
    # after the preflight. Both names are in the same directory/filesystem.
    os.link(temporary, destination, follow_symlinks=False)
    temporary.unlink()


def _fsync_directory(directory: Path) -> None:
    dir_fd = os.open(directory, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


def _require_backup_space(directory: Path, bytes_remaining: int, phase: str) -> None:
    available_bytes = shutil.disk_usage(directory).free
    required_bytes = max(0, bytes_remaining) + MIN_FREE_RESERVE_BYTES
    if available_bytes < required_bytes:
        raise ValueError(
            f"insufficient free space {phase} for verified rollback copy while retaining "
            f"the {MIN_FREE_RESERVE_BYTES}-byte reserve: need {required_bytes}, "
            f"have {available_bytes}"
        )


def create_backup(database: Path, output: Path) -> dict[str, object]:
    source = _validate_source(database)
    destination, manifest_path = _validate_output(output, source)
    source_info_before = source.stat()
    source_uri = f"file:{quote(os.fspath(source), safe='/')}?mode=ro"
    source_conn: sqlite3.Connection | None = None
    target_conn: sqlite3.Connection | None = None
    db_fd = -1
    db_temp: Path | None = None
    manifest_fd = -1
    manifest_temp: Path | None = None
    published_db = False
    try:
        db_fd, db_temp = _exclusive_temp(destination.parent, f".{destination.name}.")
        temp_fd = db_fd
        db_fd = -1
        os.close(temp_fd)
        _check_interrupted()
        source_conn = sqlite3.connect(source_uri, uri=True, timeout=10.0)
        source_conn.execute("PRAGMA busy_timeout=10000")
        # Validate that this is a Dux index and record the source schema before
        # taking the snapshot; the backup API reads a transaction-consistent
        # view including committed WAL frames.
        row = source_conn.execute(
            "SELECT value FROM meta WHERE key='schema_version'"
        ).fetchone()
        if row is None or not str(row[0]).isdecimal():
            raise ValueError("index has missing or invalid meta.schema_version")
        schema_version = int(row[0])
        _validate_dux_schema(source_conn, schema_version)
        source_page_count = int(source_conn.execute("PRAGMA page_count").fetchone()[0])
        source_page_size = int(source_conn.execute("PRAGMA page_size").fetchone()[0])
        expected_backup_bytes = source_page_count * source_page_size
        _require_backup_space(
            destination.parent, expected_backup_bytes, "before backup"
        )

        target_conn = sqlite3.connect(db_temp, timeout=10.0)
        target_conn.execute("PRAGMA busy_timeout=10000")
        target_conn.execute("PRAGMA synchronous=FULL")

        def progress(status: int, remaining: int, total: int) -> None:
            _check_interrupted()
            if status not in (
                sqlite3.SQLITE_OK,
                sqlite3.SQLITE_BUSY,
                sqlite3.SQLITE_LOCKED,
                sqlite3.SQLITE_DONE,
            ):
                raise RuntimeError(
                    f"SQLite backup failed (status={status}, remaining={remaining}, total={total})"
                )
            _require_backup_space(
                destination.parent,
                remaining * source_page_size,
                "during backup",
            )

        source_conn.backup(
            target_conn, pages=BACKUP_PAGES, progress=progress, sleep=0.05
        )
        _check_interrupted()
        target_conn.execute("PRAGMA journal_mode=DELETE")
        check = target_conn.execute("PRAGMA quick_check").fetchone()
        if check != ("ok",):
            raise RuntimeError(f"backup quick_check failed: {check!r}")
        backup_schema_row = target_conn.execute(
            "SELECT value FROM meta WHERE key='schema_version'"
        ).fetchone()
        if backup_schema_row != (str(schema_version),):
            raise RuntimeError(
                f"backup schema changed: {backup_schema_row!r} != {schema_version}"
            )
        _validate_dux_schema(target_conn, schema_version)
        page_count = int(target_conn.execute("PRAGMA page_count").fetchone()[0])
        page_size = int(target_conn.execute("PRAGMA page_size").fetchone()[0])
        target_conn.close()
        target_conn = None
        source_conn.close()
        source_conn = None

        source_info_after = source.stat()
        if (source_info_before.st_dev, source_info_before.st_ino) != (
            source_info_after.st_dev,
            source_info_after.st_ino,
        ):
            raise RuntimeError("source database path changed during backup")

        with db_temp.open("rb") as stream:
            os.fsync(stream.fileno())
        backup_hash = _sha256(db_temp)
        _check_interrupted()
        backup_bytes = db_temp.stat().st_size
        result: dict[str, object] = {
            "format": 1,
            "created_at_utc": datetime.now(timezone.utc).isoformat(),
            "database": os.fspath(source),
            "database_device": source_info_after.st_dev,
            "database_inode": source_info_after.st_ino,
            "schema_version": schema_version,
            "sqlite_version": sqlite3.sqlite_version,
            "backup": os.fspath(destination),
            "backup_bytes": backup_bytes,
            "backup_sha256": backup_hash,
            "page_count": page_count,
            "page_size": page_size,
            "quick_check": "ok",
            "permissions_octal": "0600",
        }
        manifest_fd, manifest_name = _exclusive_temp(
            manifest_path.parent, f".{manifest_path.name}."
        )
        manifest_temp = Path(manifest_name)
        with os.fdopen(manifest_fd, "w", encoding="utf-8") as stream:
            manifest_fd = -1
            json.dump(result, stream, sort_keys=True, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())

        _check_interrupted()
        _publish_no_clobber(db_temp, destination)
        published_db = True
        # Make the verified rollback copy's name durable before trying the
        # manifest. If manifest publication fails or the process crashes there,
        # the backup remains a durable recovery artifact on its own.
        _fsync_directory(destination.parent)
        _check_interrupted()
        _publish_no_clobber(manifest_temp, manifest_path)
        manifest_temp = None
        _fsync_directory(destination.parent)
        return result
    finally:
        if target_conn is not None:
            target_conn.close()
        if source_conn is not None:
            source_conn.close()
        if manifest_fd >= 0:
            os.close(manifest_fd)
        if db_fd >= 0:
            os.close(db_fd)
        for temporary in (db_temp, manifest_temp):
            if temporary is not None:
                for candidate in (
                    temporary,
                    Path(f"{temporary}-wal"),
                    Path(f"{temporary}-shm"),
                    Path(f"{temporary}-journal"),
                ):
                    try:
                        candidate.unlink()
                    except FileNotFoundError:
                        pass
        # If manifest publication failed, keep the already-published verified
        # database backup; it is safer to retain an unmanifested backup than
        # delete the only rollback artifact after a partial publish.
        if published_db and not manifest_path.exists():
            print(
                f"warning: backup exists without manifest: {destination}",
                file=sys.stderr,
            )


def _require_daemon_stopped(database: Path) -> None:
    """Refuse replacement while systemd or any process still has the index open."""
    if os.geteuid() != 0:
        raise PermissionError("index restore requires root to preserve database ownership")
    if not Path("/run/systemd/system").is_dir():
        raise RuntimeError("cannot verify Dux is stopped: systemd is not available")
    try:
        result = subprocess.run(
            ["systemctl", "is-active", "dux.service"],
            capture_output=True, text=True, check=False,
        )
    except OSError as error:
        raise RuntimeError(f"cannot verify Dux service state: {error}") from error
    state = result.stdout.strip()
    if result.returncode == 0 and state == "active":
        raise RuntimeError("dux.service is active; stop it before restoring the index")
    if result.returncode != 3 or state not in {"inactive", "failed"}:
        raise RuntimeError(
            f"cannot prove dux.service is stopped (systemctl state={state!r}, "
            f"exit={result.returncode})"
        )
    database = Path(database)
    watched = set()
    for candidate in (
        database,
        Path(f"{database}-wal"),
        Path(f"{database}-shm"),
        Path(f"{database}-journal"),
    ):
        try:
            info = candidate.stat()
        except FileNotFoundError:
            continue
        watched.add((info.st_dev, info.st_ino))
    proc_root = Path("/proc")
    if not proc_root.is_dir():
        raise RuntimeError("cannot verify open index handles: /proc is unavailable")
    for process in proc_root.iterdir():
        if not process.name.isdecimal():
            continue
        fd_directory = process / "fd"
        try:
            descriptors = fd_directory.iterdir()
        except OSError:
            continue
        try:
            for descriptor in descriptors:
                try:
                    info = descriptor.stat()
                except (FileNotFoundError, PermissionError, ProcessLookupError):
                    continue
                if (info.st_dev, info.st_ino) in watched:
                    raise RuntimeError(
                        f"index or SQLite sidecar is still open by pid {process.name}; "
                        "stop that process before restoring"
                    )
        except FileNotFoundError:
            continue


def restore_backup(database: Path, backup: Path) -> dict[str, object]:
    """Atomically restore a manifest-verified Dux backup while the daemon is stopped."""
    backup = _validate_source(backup)
    backup_info_before = backup.stat()
    manifest_path = Path(f"{backup}.json")
    try:
        manifest_info = manifest_path.lstat()
    except FileNotFoundError as error:
        raise ValueError(f"backup manifest does not exist: {manifest_path}") from error
    if (stat.S_ISLNK(manifest_info.st_mode) or not stat.S_ISREG(manifest_info.st_mode)
            or manifest_info.st_uid != os.geteuid()
            or manifest_info.st_mode & 0o077 or manifest_info.st_nlink != 1):
        raise ValueError("backup manifest must be a private, singly linked regular file")
    if (backup_info_before.st_uid != os.geteuid()
            or backup_info_before.st_mode & 0o077
            or backup_info_before.st_nlink != 1):
        raise ValueError("backup must be owned by the current user, private, and singly linked")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read backup manifest: {error}") from error
    if manifest.get("format") != 1 or manifest.get("quick_check") != "ok":
        raise ValueError("unsupported or unverified backup manifest")
    backup_hash = _sha256(backup)
    if manifest.get("backup_sha256") != backup_hash:
        raise ValueError("backup SHA-256 does not match its manifest")

    requested_database = Path(os.path.abspath(database))
    parent = requested_database.parent.resolve(strict=True)
    if parent != requested_database.parent:
        raise ValueError("database parent must not contain symlinks")
    destination = parent / requested_database.name
    if destination == backup:
        raise ValueError("restore source and database destination must differ")
    try:
        destination_info = destination.lstat()
    except FileNotFoundError as error:
        raise ValueError(f"index database does not exist: {destination}") from error
    if stat.S_ISLNK(destination_info.st_mode) or not stat.S_ISREG(destination_info.st_mode):
        raise ValueError(f"index database must be a regular, non-symlink file: {destination}")
    if destination_info.st_nlink != 1:
        raise ValueError("index database must not have hard links during restore")
    for suffix in ("-wal", "-shm", "-journal"):
        sidecar = Path(f"{destination}{suffix}")
        try:
            sidecar_info = sidecar.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(sidecar_info.st_mode) or not stat.S_ISREG(sidecar_info.st_mode):
            raise ValueError(f"SQLite sidecar must be a regular, non-symlink file: {sidecar}")
    _require_daemon_stopped(destination)

    source_uri = f"file:{quote(os.fspath(backup), safe='/')}?mode=ro"
    source_conn: sqlite3.Connection | None = None
    target_conn: sqlite3.Connection | None = None
    temporary: Path | None = None
    try:
        source_conn = sqlite3.connect(source_uri, uri=True, timeout=10.0)
        source_conn.execute("PRAGMA busy_timeout=10000")
        check = source_conn.execute("PRAGMA quick_check").fetchone()
        if check != ("ok",):
            raise ValueError(f"backup quick_check failed: {check!r}")
        schema_row = source_conn.execute(
            "SELECT value FROM meta WHERE key='schema_version'"
        ).fetchone()
        if schema_row is None or not str(schema_row[0]).isdecimal():
            raise ValueError("backup has missing or invalid meta.schema_version")
        schema_version = int(schema_row[0])
        if manifest.get("schema_version") != schema_version:
            raise ValueError("backup schema version does not match its manifest")
        _validate_dux_schema(source_conn, schema_version)
        source_page_count = int(source_conn.execute("PRAGMA page_count").fetchone()[0])
        source_page_size = int(source_conn.execute("PRAGMA page_size").fetchone()[0])
        backup_bytes = source_page_count * source_page_size
        _require_backup_space(parent, backup_bytes, "before restore")
        _check_interrupted("restore")

        fd, temporary = _exclusive_temp(parent, f".{destination.name}.restore.")
        os.close(fd)
        target_conn = sqlite3.connect(temporary, timeout=10.0)
        target_conn.execute("PRAGMA busy_timeout=10000")
        target_conn.execute("PRAGMA synchronous=FULL")

        def progress(status: int, remaining: int, total: int) -> None:
            _check_interrupted("restore")
            if status not in (
                sqlite3.SQLITE_OK,
                sqlite3.SQLITE_BUSY,
                sqlite3.SQLITE_LOCKED,
                sqlite3.SQLITE_DONE,
            ):
                raise RuntimeError(
                    f"SQLite restore failed (status={status}, remaining={remaining}, total={total})"
                )
            _require_backup_space(
                parent, remaining * source_page_size, "during restore"
            )

        source_conn.backup(
            target_conn, pages=BACKUP_PAGES, progress=progress, sleep=0.05
        )
        target_conn.execute("PRAGMA journal_mode=DELETE")
        check = target_conn.execute("PRAGMA quick_check").fetchone()
        if check != ("ok",):
            raise RuntimeError(f"restored database quick_check failed: {check!r}")
        _validate_dux_schema(target_conn, schema_version)
        target_conn.close()
        target_conn = None
        source_conn.close()
        source_conn = None
        _check_interrupted("restore")

        backup_info_after = backup.stat()
        if (backup_info_before.st_dev, backup_info_before.st_ino,
                backup_info_before.st_size, backup_info_before.st_mtime_ns) != (
                backup_info_after.st_dev, backup_info_after.st_ino,
                backup_info_after.st_size, backup_info_after.st_mtime_ns):
            raise RuntimeError("backup path changed during restore")
        with temporary.open("rb") as stream:
            os.fsync(stream.fileno())
        os.chown(temporary, destination_info.st_uid, destination_info.st_gid)
        os.chmod(temporary, stat.S_IMODE(destination_info.st_mode))
        for suffix in ("-wal", "-shm", "-journal"):
            sidecar = Path(f"{destination}{suffix}")
            if sidecar.exists():
                sidecar.unlink()
        os.replace(temporary, destination)
        temporary = None
        _fsync_directory(parent)
        return {
            "database": os.fspath(destination),
            "backup": os.fspath(backup),
            "schema_version": schema_version,
            "backup_sha256": backup_hash,
            "database_sha256": _sha256(destination),
            "quick_check": "ok",
        }
    finally:
        if target_conn is not None:
            target_conn.close()
        if source_conn is not None:
            source_conn.close()
        if temporary is not None:
            for candidate in (
                temporary, Path(f"{temporary}-wal"), Path(f"{temporary}-shm"),
                Path(f"{temporary}-journal"),
            ):
                try:
                    candidate.unlink()
                except FileNotFoundError:
                    pass


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path, default=Path("/var/lib/dux/dux.db"))
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--output", type=Path, help="create a verified backup at this path")
    mode.add_argument("--restore-from", type=Path, help="restore a verified backup created by this tool")
    args = parser.parse_args()
    global _INTERRUPTED_SIGNAL
    _INTERRUPTED_SIGNAL = None
    previous_handlers = {
        signum: signal.signal(signum, _record_interrupt)
        for signum in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        try:
            if args.restore_from is not None:
                result = restore_backup(args.database, args.restore_from)
            else:
                assert args.output is not None
                result = create_backup(args.database, args.output)
        except (OSError, sqlite3.Error, RuntimeError, ValueError) as error:
            operation = "restore" if args.restore_from is not None else "backup"
            print(f"dux {operation}: {error}", file=sys.stderr)
            return 1
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
