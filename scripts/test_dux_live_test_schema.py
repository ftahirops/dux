from pathlib import Path
import sqlite3
import tempfile
import unittest

from dux_live_test_schema import require_live_test_schema, UnsupportedLiveTestSchema


class LiveTestSchemaTests(unittest.TestCase):
    def create_db(self, path: Path, schema_version: int, include_growth_view: bool):
        with sqlite3.connect(path) as conn:
            conn.executescript(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
                "CREATE TABLE inodes(dev_id INTEGER, inode INTEGER, birth_ns INTEGER);"
            )
            conn.execute(
                "INSERT INTO meta VALUES('schema_version', ?)",
                (str(schema_version),),
            )
            if include_growth_view:
                conn.execute("CREATE TABLE growth(dev_id INTEGER, inode INTEGER, delta INTEGER)")
                conn.execute("CREATE VIEW growth_current AS SELECT * FROM growth")

    def test_accepts_required_growth_and_incarnation_schema(self):
        with tempfile.TemporaryDirectory(prefix="dux-live-schema-ok-") as temp:
            db = Path(temp) / "index.db"
            self.create_db(db, 6, include_growth_view=True)
            self.assertEqual(require_live_test_schema(db), 6)

    def test_rejects_old_schema_before_running_fanotify_workload(self):
        with tempfile.TemporaryDirectory(prefix="dux-live-schema-old-") as temp:
            db = Path(temp) / "index.db"
            self.create_db(db, 5, include_growth_view=False)
            with self.assertRaisesRegex(
                UnsupportedLiveTestSchema,
                r"schema_version=5, missing: growth_current view",
            ):
                require_live_test_schema(db)

    def test_preflight_is_read_only(self):
        with tempfile.TemporaryDirectory(prefix="dux-live-schema-readonly-") as temp:
            db = Path(temp) / "index.db"
            self.create_db(db, 6, include_growth_view=True)
            before = db.read_bytes()
            require_live_test_schema(db)
            self.assertEqual(db.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
