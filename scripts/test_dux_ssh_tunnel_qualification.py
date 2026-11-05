import pathlib
import importlib.util
import subprocess
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
QUALIFICATION = ROOT / "scripts/dux-ssh-tunnel-qualification.py"
SPEC = importlib.util.spec_from_file_location("dux_ssh_tunnel_qualification", QUALIFICATION)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class SshTunnelQualificationTests(unittest.TestCase):
    def test_ci_workflow_removed_by_project_request(self):
        self.assertFalse(WORKFLOW.exists())

    def test_loopback_qualification_covers_pinned_and_rejected_host_keys(self):
        source = QUALIFICATION.read_text()
        self.assertIn('"unknown_host_key_rejected"', source)
        self.assertIn('"mismatched_host_key_rejected"', source)
        self.assertIn('"verified_host_key_installed"', source)
        self.assertIn("StrictHostKeyChecking=yes", source)

    def test_checked_command_failure_includes_captured_diagnostics(self):
        completed = subprocess.CompletedProcess(
            ["ssh", "host"], 255, stdout="out detail", stderr="connection reset"
        )
        with mock.patch.object(MODULE.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "connection reset") as raised:
                MODULE.run_checked(["ssh", "host"], capture_output=True, text=True)
        self.assertIn("out detail", str(raised.exception))

    def test_checked_command_returns_successful_result(self):
        completed = subprocess.CompletedProcess(["ssh", "host"], 0, stdout="ok", stderr="")
        with mock.patch.object(MODULE.subprocess, "run", return_value=completed):
            self.assertIs(MODULE.run_checked(["ssh", "host"]), completed)


if __name__ == "__main__":
    unittest.main()
