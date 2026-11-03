import os
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).with_name("dux-live-test.py")


class LiveTestCliTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            capture_output=True,
            text=True,
            env={**os.environ, "DUX_LIVE_DIRECT_DAEMON": "1"},
            timeout=5,
        )

    def test_help_exits_without_starting_a_test(self):
        result = self.run_cli("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--cpu-quota-percent", result.stdout)
        self.assertIn("--max-cpu-percent", result.stdout)

    def test_cpu_quota_out_of_range_is_rejected_before_runtime(self):
        result = self.run_cli("--cpu-quota-percent", "9")
        self.assertEqual(result.returncode, 2)
        self.assertIn("between 10 and 800", result.stderr)

    def test_event_loop_target_out_of_range_is_rejected_before_runtime(self):
        result = self.run_cli("--max-cpu-percent", "0")
        self.assertEqual(result.returncode, 2)
        self.assertIn("between 1 and 100", result.stderr)


if __name__ == "__main__":
    unittest.main()
