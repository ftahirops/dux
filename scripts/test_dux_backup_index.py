from __future__ import annotations

import hashlib
from contextlib import redirect_stderr
import io
import importlib.util
import json
import os
from pathlib import Path
import signal
import sqlite3
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("dux-backup-index.py")
SPEC = importlib.util.spec_from_file_location("dux_backup_index", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
BACKUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BACKUP)


class DuxBackupIndexTests(unittest.TestCase):
    def create_index_schema(self, conn: sqlite3.Connection, schema_version: int) -> None:
        conn.execute("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        conn.execute(
            "INSERT INTO meta VALUES('schema_version',?)", (str(schema_version),)
        )
        inode_columns = "dev_id INTEGER,inode INTEGER,kind TEXT,blocks INTEGER"
        growth_columns = "bucket INTEGER,dev_id INTEGER,inode INTEGER,delta INTEGER"
        if schema_version >= 5:
            inode_columns += ",birth_ns INTEGER"
        if schema_version >= 6:
            growth_columns = (
                "bucket INTEGER,dev_id INTEGER,inode INTEGER,birth_ns INTEGER,delta INTEGER"
            )
        conn.execute(f"CREATE TABLE inodes({inode_columns})")
        conn.execute(
            "CREATE TABLE dirents(parent_dev INTEGER,parent_inode INTEGER,name BLOB,"
            "dev_id INTEGER,inode INTEGER)"
        )
        conn.execute(f"CREATE TABLE growth({growth_columns})")

    def make_index(self, path: Path) -> sqlite3.Connection:
        conn = sqlite3.connect(path)
        conn.execute("PRAGMA journal_mode=WAL")
        self.create_index_schema(conn, 5)
        conn.execute("CREATE TABLE payload(value TEXT)")
        conn.execute("INSERT INTO payload VALUES('committed')")
        conn.commit()
        return conn

    def test_accepts_supported_v4_v5_v6_schema_shapes(self):
        for schema_version in (4, 5, 6):
            with self.subTest(schema_version=schema_version):
                conn = sqlite3.connect(":memory:")
                try:
                    self.create_index_schema(conn, schema_version)
                    BACKUP._validate_dux_schema(conn, schema_version)
                finally:
                    conn.close()

    def test_live_wal_backup_is_verified_private_and_hash_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            writer = self.make_index(database)
            writer.execute("BEGIN")
            writer.execute("INSERT INTO payload VALUES('uncommitted')")

            output = root / "dux.db.pre-upgrade.sqlite"
            result = BACKUP.create_backup(database, output)
            writer.rollback()

            self.assertEqual(result["schema_version"], 5)
            self.assertEqual(result["quick_check"], "ok")
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(
                hashlib.sha256(output.read_bytes()).hexdigest(), result["backup_sha256"]
            )
            manifest = Path(f"{output}.json")
            self.assertEqual(manifest.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(manifest.read_text())["backup_sha256"], result["backup_sha256"])
            backup = sqlite3.connect(f"file:{output}?mode=ro", uri=True)
            try:
                self.assertEqual(backup.execute("PRAGMA quick_check").fetchone(), ("ok",))
                self.assertEqual(backup.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone(), ("5",))
                self.assertEqual(backup.execute("SELECT value FROM payload").fetchall(), [("committed",)])
            finally:
                backup.close()
                writer.close()

    def test_refuses_to_overwrite_backup_or_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            output = root / "backup.sqlite"
            output.write_bytes(b"preserve me")
            with self.assertRaises(FileExistsError):
                BACKUP.create_backup(database, output)
            self.assertEqual(output.read_bytes(), b"preserve me")

            output.unlink()
            Path(f"{output}.json").write_text("operator data")
            with self.assertRaises(FileExistsError):
                BACKUP.create_backup(database, output)
            self.assertFalse(output.exists())
            conn.close()

    def test_backup_name_is_durable_before_manifest_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            output = root / "backup.sqlite"
            manifest = Path(f"{output}.json")
            events: list[tuple[str, Path]] = []
            publish = BACKUP._publish_no_clobber
            fsync_directory = BACKUP._fsync_directory

            def publish_with_manifest_failure(temporary: Path, destination: Path) -> None:
                events.append(("publish", destination))
                if destination == manifest:
                    raise OSError("injected manifest publication failure")
                publish(temporary, destination)

            def record_directory_fsync(directory: Path) -> None:
                events.append(("fsync", directory))
                fsync_directory(directory)

            with (
                mock.patch.object(
                    BACKUP, "_publish_no_clobber", publish_with_manifest_failure
                ),
                mock.patch.object(BACKUP, "_fsync_directory", record_directory_fsync),
                redirect_stderr(io.StringIO()),
            ):
                with self.assertRaisesRegex(
                    OSError, "injected manifest publication failure"
                ):
                    BACKUP.create_backup(database, output)

            self.assertEqual(
                events,
                [("publish", output), ("fsync", root), ("publish", manifest)],
            )
            self.assertTrue(output.is_file())
            self.assertFalse(manifest.exists())
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            backup = sqlite3.connect(f"file:{output}?mode=ro", uri=True)
            try:
                self.assertEqual(backup.execute("PRAGMA quick_check").fetchone(), ("ok",))
            finally:
                backup.close()

    def test_sigterm_during_backup_hashing_cleans_temporary_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            output = root / "backup.sqlite"
            original_hash = BACKUP._sha256
            previous_handler = signal.getsignal(signal.SIGTERM)

            def interrupt_hash(path: Path) -> str:
                signal.raise_signal(signal.SIGTERM)
                return original_hash(path)

            with (
                mock.patch.object(sys, "argv", [
                    "dux-backup-index.py", "--database", str(database),
                    "--output", str(output),
                ]),
                mock.patch.object(BACKUP, "_sha256", side_effect=interrupt_hash),
                redirect_stderr(io.StringIO()) as stderr,
            ):
                self.assertEqual(BACKUP.main(), 1)

            self.assertIn("backup interrupted by SIGTERM", stderr.getvalue())
            self.assertIs(signal.getsignal(signal.SIGTERM), previous_handler)
            self.assertFalse(output.exists())
            self.assertFalse(Path(f"{output}.json").exists())
            self.assertEqual(list(root.glob(".backup.sqlite.*")), [])
            self.assertEqual(list(root.glob(".backup.sqlite.json.*")), [])

    def test_private_temp_mode_failure_closes_fd_and_removes_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real_mkstemp = BACKUP.tempfile.mkstemp
            created_fds: list[int] = []

            def record_temp(*args, **kwargs):
                fd, path = real_mkstemp(*args, **kwargs)
                created_fds.append(fd)
                return fd, path

            with (
                mock.patch.object(BACKUP.tempfile, "mkstemp", side_effect=record_temp),
                mock.patch.object(BACKUP.os, "fchmod", side_effect=OSError("mode failure")),
            ):
                with self.assertRaisesRegex(OSError, "mode failure"):
                    BACKUP._exclusive_temp(root, ".backup.")

            self.assertEqual(len(created_fds), 1)
            with self.assertRaises(OSError):
                os.fstat(created_fds[0])
            self.assertEqual(list(root.iterdir()), [])

    def test_rejects_symlinked_database_and_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            link = root / "alias.db"
            link.symlink_to(database)
            with self.assertRaisesRegex(ValueError, "regular, non-symlink"):
                BACKUP.create_backup(link, root / "link-backup.sqlite")

            for suffix in ("-wal", "-shm", "-journal"):
                with self.subTest(sidecar=suffix):
                    sidecar = Path(f"{database}{suffix}")
                    sidecar.symlink_to(root / f"missing{suffix}")
                    with self.assertRaisesRegex(ValueError, "SQLite sidecar"):
                        BACKUP.create_backup(
                            database, root / f"sidecar{suffix}.sqlite"
                        )
                    sidecar.unlink()

    def test_refuses_backup_output_that_is_a_sqlite_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()

            for suffix in ("-wal", "-shm", "-journal"):
                with self.subTest(sidecar=suffix):
                    sidecar = Path(f"{database}{suffix}")
                    sidecar.write_bytes(f"preserve {suffix}".encode())
                    with self.assertRaisesRegex(
                        ValueError, "must not replace the database or a SQLite sidecar"
                    ):
                        BACKUP.create_backup(database, sidecar)
                    self.assertEqual(sidecar.read_bytes(), f"preserve {suffix}".encode())

    def test_rejects_missing_or_invalid_du_x_schema_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "not-dux.db"
            conn = sqlite3.connect(database)
            conn.execute("CREATE TABLE payload(value TEXT)")
            conn.commit()
            conn.close()
            with self.assertRaisesRegex(sqlite3.Error, "no such table: meta"):
                BACKUP.create_backup(database, root / "backup.sqlite")

    def test_rejects_sqlite_file_with_forged_schema_version_but_no_dux_tables(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "not-dux.db"
            conn = sqlite3.connect(database)
            conn.execute("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            conn.execute("INSERT INTO meta VALUES('schema_version','5')")
            conn.execute("CREATE TABLE payload(value TEXT)")
            conn.commit()
            conn.close()

            output = root / "backup.sqlite"
            with self.assertRaisesRegex(ValueError, "missing required Dux table"):
                BACKUP.create_backup(database, output)
            self.assertFalse(output.exists())
            self.assertFalse(Path(f"{output}.json").exists())
            self.assertEqual(list(root.glob(".backup.sqlite.*")), [])

    def test_rejects_unsupported_schema_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.execute("UPDATE meta SET value='7' WHERE key='schema_version'")
            conn.commit()
            conn.close()

            with self.assertRaisesRegex(ValueError, "unsupported Dux index schema v7"):
                BACKUP.create_backup(database, root / "backup.sqlite")

    def test_reserves_free_space_before_creating_backup_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            output = root / "backup.sqlite"
            low_space = type("DiskUsage", (), {"free": 0})()
            with mock.patch.object(BACKUP.shutil, "disk_usage", return_value=low_space):
                with self.assertRaisesRegex(ValueError, "insufficient free space"):
                    BACKUP.create_backup(database, output)
            self.assertFalse(output.exists())
            self.assertFalse(Path(f"{output}.json").exists())
            self.assertEqual(list(root.glob(".backup.sqlite.*")), [])

    def test_aborts_if_available_space_falls_during_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            output = root / "backup.sqlite"
            disk_usage = [
                type("DiskUsage", (), {"free": 10**12})(),
                type("DiskUsage", (), {"free": 0})(),
            ]
            with mock.patch.object(
                BACKUP.shutil, "disk_usage", side_effect=disk_usage
            ):
                with self.assertRaisesRegex(ValueError, "insufficient free space during backup"):
                    BACKUP.create_backup(database, output)
            self.assertFalse(output.exists())
            self.assertFalse(Path(f"{output}.json").exists())
            self.assertEqual(list(root.glob(".backup.sqlite.*")), [])

    def test_rejects_group_or_world_writable_output_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "dux.db"
            conn = self.make_index(database)
            conn.close()
            insecure = root / "shared"
            insecure.mkdir(mode=0o700)
            insecure.chmod(0o777)
            with self.assertRaisesRegex(ValueError, "not group/world writable"):
                BACKUP.create_backup(database, insecure / "backup.sqlite")
            self.assertEqual(list(insecure.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
