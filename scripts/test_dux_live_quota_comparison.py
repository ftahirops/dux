import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("dux-live-quota-comparison.py")
SPEC = importlib.util.spec_from_file_location("dux_live_quota_comparison", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class QuotaSummaryTests(unittest.TestCase):
    def test_ratio_is_independent_of_execution_order(self):
        first = MODULE.quota_wall_time_summary([
            {"cpu_quota_percent": 50, "systemd_run_seconds": 100.0},
            {"cpu_quota_percent": 100, "systemd_run_seconds": 200.0},
        ])
        reversed_order = MODULE.quota_wall_time_summary([
            {"cpu_quota_percent": 100, "systemd_run_seconds": 200.0},
            {"cpu_quota_percent": 50, "systemd_run_seconds": 100.0},
        ])
        self.assertEqual(first["wall_time_ratio_50_over_100"], 0.5)
        self.assertEqual(reversed_order["wall_time_ratio_50_over_100"], 0.5)

    def test_ratio_above_one_means_higher_quota_was_faster(self):
        summary = MODULE.quota_wall_time_summary([
            {"cpu_quota_percent": 100, "systemd_run_seconds": 50.0},
            {"cpu_quota_percent": 50, "systemd_run_seconds": 100.0},
        ])
        self.assertEqual(summary["wall_time_ratio_50_over_100"], 2.0)
        self.assertEqual(summary["quota_run_order"], [100, 50])

    @mock.patch.object(MODULE.subprocess, "run")
    def test_cancel_stops_the_transient_unit_before_returning(self, run):
        process = mock.MagicMock()
        process.poll.return_value = None
        MODULE.stop_transient_unit("dux-live-quota-test.service", process)
        run.assert_called_once_with(
            ["systemctl", "stop", "dux-live-quota-test.service"],
            capture_output=True,
            timeout=45,
        )
        process.terminate.assert_called_once()
        process.wait.assert_called_once_with(timeout=30)

    @mock.patch.object(MODULE.subprocess, "run", side_effect=OSError("systemd unavailable"))
    def test_cancel_kills_worker_if_systemd_stop_does_not_reap_it(self, _run):
        process = mock.MagicMock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("worker", 30), 0]
        MODULE.stop_transient_unit("dux-live-quota-test.service", process)
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(process.wait.call_count, 2)


if __name__ == "__main__":
    unittest.main()
