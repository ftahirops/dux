from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from types import SimpleNamespace
import stat
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import dux_qualification_sandbox as SANDBOX


SCRIPT = Path(__file__).with_name("dux-bind-alias-qualification.py")
SPEC = spec_from_file_location("dux_bind_alias_qualification", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
QUALIFICATION = module_from_spec(SPEC)
SPEC.loader.exec_module(QUALIFICATION)


class BindAliasQualificationSafetyTests(unittest.TestCase):
    def test_report_creation_is_private_and_refuses_to_overwrite(self):
        with tempfile.TemporaryDirectory(prefix="dux-bind-alias-report-") as temporary:
            report = Path(temporary) / "report.json"
            QUALIFICATION.write_report_no_clobber(report, {"status": "passed"})
            self.assertEqual(stat.S_IMODE(report.stat().st_mode), 0o600)
            self.assertIn('"status": "passed"', report.read_text())

            report.write_text("preserve existing report")
            with self.assertRaises(FileExistsError):
                QUALIFICATION.write_report_no_clobber(report, {"status": "overwritten"})
            self.assertEqual(report.read_text(), "preserve existing report")

    def test_report_creation_refuses_symlink_destination(self):
        with tempfile.TemporaryDirectory(prefix="dux-bind-alias-report-link-") as temporary:
            root = Path(temporary)
            report = root / "report.json"
            target = root / "target.json"
            target.write_text("preserve target")
            report.symlink_to(target)
            with self.assertRaises(FileExistsError):
                QUALIFICATION.write_report_no_clobber(report, {"status": "overwritten"})
            self.assertEqual(target.read_text(), "preserve target")

    def test_report_serialization_failure_leaves_no_partial_artifact(self):
        with tempfile.TemporaryDirectory(prefix="dux-bind-alias-report-failure-") as temporary:
            root = Path(temporary)
            report = root / "report.json"
            with self.assertRaises(TypeError):
                QUALIFICATION.write_report_no_clobber(report, {"invalid": object()})
            self.assertFalse(report.exists())
            self.assertEqual(list(root.iterdir()), [])

    def test_requires_root_before_virtualization_probe(self):
        with patch.object(SANDBOX.os, "geteuid", return_value=1000):
            with patch.object(SANDBOX.subprocess, "run") as run:
                with self.assertRaisesRegex(RuntimeError, "requires root"):
                    QUALIFICATION.require_disposable_qualification_vm()
                run.assert_not_called()

    def test_refuses_unmarked_non_kvm_host(self):
        result = SimpleNamespace(returncode=1, stdout="none\n")
        with patch.object(SANDBOX.os, "geteuid", return_value=0):
            with patch.object(SANDBOX.subprocess, "run", return_value=result):
                with patch.object(SANDBOX, "SANDBOX_MARKER", Path("/missing/dux-marker")):
                    with self.assertRaisesRegex(RuntimeError, "KVM/QEMU"):
                        QUALIFICATION.require_disposable_qualification_vm()

    def test_refuses_kvm_without_controller_marker(self):
        result = SimpleNamespace(returncode=0, stdout="kvm\n")
        with patch.object(SANDBOX.os, "geteuid", return_value=0):
            with patch.object(SANDBOX.subprocess, "run", return_value=result):
                with patch.object(SANDBOX, "SANDBOX_MARKER", Path("/missing/dux-marker")):
                    with self.assertRaisesRegex(RuntimeError, "marker is absent"):
                        QUALIFICATION.require_disposable_qualification_vm()

    def test_environment_cannot_bypass_vm_marker(self):
        result = SimpleNamespace(returncode=1, stdout="none\n")
        with patch.object(SANDBOX.os, "geteuid", return_value=0):
            with patch.dict(
                SANDBOX.os.environ,
                {"GITHUB_ACTIONS": "true", "CI": "true", "GITHUB_WORKSPACE": "/tmp"},
                clear=True,
            ):
                with patch.object(SANDBOX.subprocess, "run", return_value=result):
                    with patch.object(SANDBOX, "SANDBOX_MARKER", Path("/missing/dux-marker")):
                        with self.assertRaisesRegex(RuntimeError, "KVM/QEMU"):
                            SANDBOX.require_disposable_qualification_vm()

    def test_explicit_root_marker_admits_runner_without_environment_claims(self):
        marker = MagicMock()
        marker.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFREG | 0o400, st_uid=0)
        marker.read_text.return_value = "dux-qualification-sandbox-v1\n"
        result = SimpleNamespace(returncode=1, stdout="none\n")
        with patch.object(SANDBOX.os, "geteuid", return_value=0):
            with patch.dict(SANDBOX.os.environ, {}, clear=True):
                with patch.object(SANDBOX.subprocess, "run", return_value=result):
                    with patch.object(SANDBOX, "SANDBOX_MARKER", marker):
                        admission = SANDBOX.require_disposable_qualification_vm()
        self.assertEqual(admission["virtualization"], "explicit-marker")
        self.assertEqual(admission["marker"], str(marker))


if __name__ == "__main__":
    unittest.main()
