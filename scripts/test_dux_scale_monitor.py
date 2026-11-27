#!/usr/bin/env python3
"""Tests for loss-resistant host-side scale progress capture."""
import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("dux-scale-monitor.py")
SPEC = importlib.util.spec_from_file_location("dux_scale_monitor", SCRIPT)
MONITOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MONITOR)


class ScaleMonitorTests(unittest.TestCase):
    def test_terminal_status_matches_remote_exit(self):
        self.assertEqual(MONITOR.terminal_status(0), "complete")
        self.assertEqual(MONITOR.terminal_status(1), "failed")
        self.assertEqual(MONITOR.terminal_status(None), "incomplete")
        self.assertEqual(
            MONITOR.terminal_status(0, interrupted=True), "interrupted_by_operator"
        )

    def test_splits_and_validates_atomic_guest_snapshots(self):
        report, progress = MONITOR.split_snapshot(
            '{"status":"running"}' + MONITOR.DELIMITER + '{"phase":"scan"}'
        )
        self.assertEqual(report, '{"status":"running"}\n')
        self.assertEqual(progress, '{"phase":"scan"}\n')

    def test_rejects_torn_or_invalid_guest_snapshot(self):
        with self.assertRaises(ValueError):
            MONITOR.split_snapshot('{"status":"running"}')
        with self.assertRaises(ValueError):
            MONITOR.split_snapshot('{' + MONITOR.DELIMITER + '{"phase":"scan"}')

    def test_atomic_write_replaces_previous_checkpoint(self):
        with tempfile.TemporaryDirectory(prefix="dux-scale-monitor-") as directory:
            target = Path(directory) / "report.json"
            MONITOR.atomic_write(target, b'{"generation":1}\n')
            MONITOR.atomic_write(target, b'{"generation":2}\n')
            self.assertEqual(target.read_bytes(), b'{"generation":2}\n')
            self.assertEqual(list(Path(directory).iterdir()), [target])


if __name__ == "__main__":
    unittest.main()
