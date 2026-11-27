import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import time
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "dux_exact_mismatch_followup",
    Path(__file__).with_name("dux-exact-mismatch-followup.py"),
)
followup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(followup)


class BehindSecondsApplicabilityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.db = Path(self.temp.name) / "index.db"
        with sqlite3.connect(self.db) as connection:
            connection.executescript(
                """
                CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                INSERT INTO meta VALUES('root_dev','1'),('root_inode','1'),
                    ('last_scan_ts','{}');
                CREATE TABLE dirents(
                    dev_id INTEGER, inode INTEGER, parent_dev INTEGER,
                    parent_inode INTEGER, name BLOB, prime INTEGER
                );
                CREATE TABLE inodes(dev_id INTEGER, inode INTEGER, blocks INTEGER);
                """.format(int(time.time()) - 60)
            )
        self.source = {
            "snapshot": {"exact_filesystem": {"difference_details": {}}}
        }

    def sample(self, stdout, sidecar=None):
        if sidecar is not None:
            Path(f"{self.db}.watch.status").write_text(
                sidecar + "\n" + str(self.db.resolve())
            )
        result = type(
            "ProcessResult", (), {"returncode": 0, "stdout": stdout, "stderr": ""}
        )()
        with patch.object(followup.subprocess, "run", return_value=result):
            return followup.sample(self.source, self.db, Path(self.temp.name), Path("dux"))

    def test_legacy_scan_first_sidecar_marks_event_drain_age_inapplicable(self):
        sidecar = "5 {} {} 0 1 12 0 0 0 90000 500000 0 0 0 1".format(
            int(time.time()), os.getpid()
        )
        result = self.sample("legacy human status table\n", sidecar)
        status = result["status"]
        self.assertTrue(status["scan_first"])
        self.assertFalse(status["behind_seconds_applicable"])
        self.assertIsNone(status["behind_seconds"])
        self.assertGreaterEqual(status["last_scan_age_seconds"], 60)
        self.assertLessEqual(status["last_scan_age_seconds"], 62)

    def test_machine_scan_first_json_does_not_expose_misleading_behind_value(self):
        result = self.sample(json.dumps({
            "scan_first": True,
            "behind_seconds": 90000,
            "behind_seconds_applicable": False,
            "last_scan_age_seconds": 17,
        }))
        status = result["status"]
        self.assertTrue(status["scan_first"])
        self.assertFalse(status["behind_seconds_applicable"])
        self.assertIsNone(status["behind_seconds"])
        self.assertEqual(status["last_scan_age_seconds"], 17)

    def test_legacy_event_apply_sidecar_preserves_applicable_behind_seconds(self):
        sidecar = "5 {} {} 0 1 12 0 0 0 90000 500000 0 0 0 0".format(
            int(time.time()), os.getpid()
        )
        result = self.sample("legacy human status table\n", sidecar)
        status = result["status"]
        self.assertFalse(status["scan_first"])
        self.assertTrue(status["behind_seconds_applicable"])
        self.assertEqual(status["behind_seconds"], 90.0)


if __name__ == "__main__":
    unittest.main()
