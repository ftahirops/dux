#!/usr/bin/env python3
import ast
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "scripts/dux-readiness.py"
SCALE_HARNESS = ROOT / "scripts/dux-scale.py"
VM_HARNESS = ROOT / "scripts/dux-qualification-vm.py"
READINESS_SPEC = importlib.util.spec_from_file_location("dux_readiness_harness", HARNESS)
READINESS = importlib.util.module_from_spec(READINESS_SPEC)
READINESS_SPEC.loader.exec_module(READINESS)


class ReadinessHarnessTests(unittest.TestCase):
    def test_overflow_suite_rejects_fixture_with_insufficient_recovery_headroom(self):
        required = READINESS.OVERFLOW_RECOVERY_MIN_FREE_BYTES
        self.assertEqual(READINESS.validate_overflow_recovery_headroom(required), required)
        with self.assertRaisesRegex(RuntimeError, "Increase --filesystem-mib; overflow was not injected"):
            READINESS.validate_overflow_recovery_headroom(required - 1)

    def test_overflow_recovery_accepts_reconcile_after_backlog_drains(self):
        log = "\n".join((
            "starting background reconciliation of earlier restart gap",
            "fanotify queue overflow — index may have missed events",
            "index backlog persists — 15s since last caught up",
            "starting background reconciliation of /fixture; event ingestion remains active",
            "rescan complete: 118443 files, 10 dirs (0 errors)",
            "reconciliation accounting verification passed",
            "caught up — live updates resumed",
        ))

        evidence = READINESS.overflow_reconciliation_evidence(log)

        self.assertTrue(evidence['passed'])
        self.assertEqual(evidence['reconciliation_start_reason'], 'backlog_drained_after_persistence')

    def test_overflow_recovery_does_not_accept_reconcile_before_backlog_persists(self):
        log = "\n".join((
            "starting background reconciliation of earlier restart gap",
            "fanotify queue overflow — index may have missed events",
            "index backlog persists — 15s since last caught up",
        ))

        self.assertFalse(READINESS.overflow_reconciliation_evidence(log)['passed'])

    def test_legacy_pipeline_status_is_parsed_without_runtime_marker(self):
        output = (
            "pipeline: drained; 3 pending / 500000 cap; 41 events (92.7% resolved); "
            "38 updates committed; 0 dropped; 25ms since caught up\n"
        )
        self.assertEqual(
            READINESS.parse_legacy_pipeline_status(output),
            {"pending": 3, "capacity": 500000, "drained": True,
             "seen": 41, "committed": 38, "dropped": 0, "uncertain": False},
        )

    def test_legacy_pipeline_status_rejects_unknown_format(self):
        with self.assertRaisesRegex(AssertionError, "does not contain"):
            READINESS.parse_legacy_pipeline_status("daemon: live\n")

    def test_runtime_status_v4_parses_uncertainty_and_kernel_overflows(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            db = base / "index.db"
            runtime = base / "runtime"
            runtime.mkdir()
            status = READINESS.runtime_path(db, runtime, "watch.status")
            status.write_text(
                f"4 100  {os.getpid()}  7 1 42 40 38 2 500 500000 1 123 6\n{db.resolve()}"
            )
            case = SimpleNamespace(db=db, runtime=runtime)
            daemon = READINESS.Daemon.__new__(READINESS.Daemon)
            daemon.case = case
            daemon.process = SimpleNamespace(pid=os.getpid())

            result = daemon.record()

        self.assertEqual(result["pending"], 7)
        self.assertTrue(result["drained"])
        self.assertEqual(result["seen"], 42)
        self.assertEqual(result["resolved"], 40)
        self.assertEqual(result["committed"], 38)
        self.assertEqual(result["dropped"], 2)
        self.assertTrue(result["uncertain"])
        self.assertEqual(result["kernel_overflow_notifications"], 6)

    def test_pressure_suite_requires_fault_binary_before_starting_worker(self):
        result = subprocess.run(
            [sys.executable, str(HARNESS), "--suites", "pressure"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("pressure suite requires --fault-binary", result.stderr)

    def test_retry_suite_requires_fault_binary_before_starting_worker(self):
        result = subprocess.run(
            [sys.executable, str(HARNESS), "--suites", "retry"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("retry suite requires --fault-binary", result.stderr)

    def test_report_checkpoints_are_atomically_published_as_valid_json(self):
        with tempfile.TemporaryDirectory() as directory:
            report_path = Path(directory) / "report.json"
            report = {"status": "running", "current_suite": "overflow", "suites": {}}
            READINESS.write_report(report_path, report)
            self.assertEqual(json.loads(report_path.read_text()), report)
            self.assertEqual(list(Path(directory).iterdir()), [report_path])

    def test_churn_progress_persists_each_completed_batch_atomically(self):
        with tempfile.TemporaryDirectory() as directory:
            progress_path = Path(directory) / "report-churn-progress.json"
            progress = {
                "suite": "churn", "status": "running", "total_batches": 10,
                "completed_batches": [{"batch": 1, "exact_paths_inodes_allocations_rollups": True}],
                "completed_operations": 2000,
            }

            READINESS.publish_churn_progress(progress_path, progress)
            checkpoint = json.loads(progress_path.read_text())

            self.assertEqual(checkpoint["status"], "running")
            self.assertEqual(checkpoint["completed_operations"], 2000)
            self.assertEqual(checkpoint["completed_batches"][0]["batch"], 1)
            self.assertEqual(list(Path(directory).iterdir()), [progress_path])
            self.assertRegex(checkpoint["last_checkpoint_at"], r"^\d{4}-\d{2}-\d{2}T.*Z$")

    def test_cpu_budget_separates_event_production_from_pipeline_catchup(self):
        tree = ast.parse(HARNESS.read_text())
        function = next(
            node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "cpu_event_burst_test"
        )
        source = ast.unparse(function)
        self.assertIn("producer_seconds = time.monotonic() - producer_started", source)
        self.assertIn("event_window_seconds = time.monotonic() - event_window_started", source)
        self.assertIn("cpu_fraction = (process_cpu_seconds(pid) - cpu_before) / event_window_seconds", source)
        self.assertIn("catch_up_seconds = time.monotonic() - catchup_started", source)
        self.assertIn("assert catch_up_seconds < 60", source)
        self.assertIn("event_production_seconds=round(producer_seconds, 3)", source)
        self.assertLess(source.index("producer_started = time.monotonic()"), source.index("for i in range(12000)"))
        self.assertLess(source.index("producer_seconds = time.monotonic() - producer_started"), source.index("event_window_started = time.monotonic()"))
        self.assertLess(source.index("event_window_started = time.monotonic()"), source.index("os.kill(pid, signal.SIGCONT)"))

    def test_cpu_governor_budget_is_below_outer_cgroup_quota(self):
        tree = ast.parse(HARNESS.read_text())
        cpu_test = next(
            node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "cpu_event_burst_test"
        )
        source = ast.unparse(cpu_test)
        self.assertIn("daemon_max_cpu_percent = 10", source)
        self.assertIn("Daemon(case, max_cpu_percent=daemon_max_cpu_percent)", source)
        self.assertIn("cpu_fraction <= 0.12", source)
        self.assertLess(source.index("signal.SIGSTOP"), source.index("for i in range(12000)"))
        self.assertLess(source.index("for i in range(12000)"), source.index("signal.SIGCONT"))
        main = next(
            node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "main"
        )
        main_source = ast.unparse(main)
        self.assertIn("25 <= args.cpu_quota_percent <= 75", main_source)

    def test_cpu_cleanup_exact_checks_cover_measured_full_tree_cost(self):
        tree = ast.parse(HARNESS.read_text())
        function = next(
            node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "cpu_event_burst_test"
        )
        timeouts = [
            node.keywords[0].value.value
            for node in ast.walk(function)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "exact"
            and node.keywords
            and node.keywords[0].arg == "timeout"
            and isinstance(node.keywords[0].value, ast.Constant)
            and isinstance(node.keywords[0].value.value, (int, float))
        ]
        self.assertTrue(timeouts)
        self.assertGreaterEqual(min(timeouts), 120)

    def test_remaining_p0_matrix_has_four_hour_wall_limit(self):
        source = HARNESS.read_text()
        self.assertIn('if (P0_MATRIX_SUITES - {"crash", "overflow"}).issubset(set(args.suites)):', source)
        self.assertIn("limit = max(limit, 14400)", source)

    def test_churn_qualification_runtime_has_an_independent_wall_limit(self):
        source = HARNESS.read_text()
        self.assertIn('if "churn" in args.suites:', source)
        self.assertIn("limit = max(limit, 3600)", source)

    def test_churn_defaults_promote_to_production_fixture_and_reserve(self):
        args = SimpleNamespace(suites={"churn"}, seed_files=256, filesystem_mib=768)
        messages = READINESS.promote_suite_resources(args)
        self.assertEqual(args.seed_files, 100_000)
        self.assertEqual(args.filesystem_mib, 2_048)
        self.assertEqual(len(messages), 1)
        self.assertIn("write-safety reserve", messages[0])

    def test_fixture_workspace_defaults_to_tmpfs_and_checks_capacity(self):
        source = HARNESS.read_text()
        tree = ast.parse(source)
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        workspace_arg = next(
            node for node in ast.walk(main)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr == "add_argument"
            and node.args and isinstance(node.args[0], ast.Constant)
            and node.args[0].value == "--workspace-parent"
        )
        self.assertIn("Path('/run/dux-readiness')", ast.unparse(workspace_arg))
        helper = next(
            node for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name == "validate_workspace_parent"
        )
        helper_source = ast.unparse(helper)
        self.assertIn("available < required", helper_source)
        self.assertIn("path.stat().st_dev == Path('/').stat().st_dev", helper_source)
        self.assertIn("filesystem_type != 'tmpfs'", helper_source)
        self.assertIn("512 * MIB", helper_source)
        self.assertIn('"--workspace-parent", str(args.workspace_parent.resolve())', source)

    def test_scale_workspace_defaults_to_tmpfs_and_only_allows_disk_inside_kvm_fixture(self):
        source = SCALE_HARNESS.read_text()
        tree = ast.parse(source)
        worker = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "worker")
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "validate_workspace_parent")
        self.assertIn("Path('/run/dux-scale')", ast.unparse(main))
        helper_source = ast.unparse(helper)
        self.assertIn("filesystem_type != 'tmpfs'", helper_source)
        self.assertIn("systemd-detect-virt", helper_source)
        self.assertIn("filesystem_type == 'ext4'", helper_source)
        self.assertIn("Path('/mnt/dux-readiness')", helper_source)
        self.assertIn("vm.stdout.strip() == 'kvm'", helper_source)
        self.assertIn("image_bytes + 4 * 1024 * MIB", ast.unparse(worker))
        self.assertIn("tempfile.mkdtemp(prefix='dux-scale-', dir=workspace_parent)", ast.unparse(worker))
        self.assertIn("workspace_filesystem_type=workspace_type", ast.unparse(worker))
        self.assertIn("'--workspace-parent', str(args.workspace_parent.resolve())", ast.unparse(main))
        self.assertIn("parser.add_argument('--disk-workspace', action='store_true'", ast.unparse(main))
        self.assertIn("runargs.append('--disk-workspace')", ast.unparse(main))
        self.assertIn("flush_fixture_pages(image, args.disk_workspace)", ast.unparse(worker))
        self.assertIn("os.posix_fadvise(stream.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)", source)
        self.assertIn("os.open('/proc/sys/vm/drop_caches', os.O_WRONLY)", source)
        self.assertIn("fixture_cache_policy", ast.unparse(worker))

    def test_vm_profiles_stay_within_two_to_four_gib_and_preflight_host_memory(self):
        source = VM_HARNESS.read_text()
        tree = ast.parse(source)
        profiles = next(
            node.value for node in tree.body
            if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id == "VM_PROFILES" for target in node.targets)
        )
        profile_source = ast.unparse(profiles)
        self.assertIn("'standard'", profile_source)
        self.assertIn("'qualification4'", profile_source)
        self.assertIn("'guest_ram_mib': 2048", profile_source)
        self.assertIn("'guest_ram_mib': 4096", profile_source)
        self.assertNotIn("'cpu_quota_percent': 200", profile_source)
        self.assertIn("'cgroup_memory_mib': 8192", profile_source)
        self.assertIn("'minimum_host_free_mib': 12288", profile_source)
        self.assertIn("'cpu_quota_percent': 50", profile_source)
        self.assertIn("on a private tmpfs", source)
        self.assertIn("expected tmpfs", source)
        self.assertNotIn("'guest_ram_mib': 16384", profile_source)
        self.assertNotIn("32768", profile_source)
        self.assertIn("str(profile['guest_ram_mib'])", source)
        self.assertIn("str(profile['vcpus'])", source)

    def test_current_package_lifecycle_guest_is_fresh_and_provisions_sandbox_marker(self):
        source = VM_HARNESS.read_text()
        self.assertIn("'ubuntu24-lifecycle-current': 'ubuntu24'", source)
        self.assertIn("'rocky9-lifecycle-current': 'rocky9'", source)
        self.assertIn("path: /etc/dux-qualification-sandbox", source)
        self.assertIn("dux-qualification-sandbox-v1", source)

    def test_vm_host_tree_shares_are_exported_read_only(self):
        source = VM_HARNESS.read_text()
        self.assertIn("parser.add_argument('--read-only-share'", source)
        self.assertIn("share.is_dir()", source)
        self.assertIn("security_model=none,readonly=on", source)
        self.assertIn("'read_only_share': str(share) if share else None", source)

    def test_scale_worker_resource_envelope_is_configurable_and_reported(self):
        source = SCALE_HARNESS.read_text()
        self.assertIn("'cpu_quota_percent': args.cpu_quota_percent", source)
        self.assertIn("'memory_limit_mib': args.worker_memory_mib", source)
        self.assertIn("f'--property=CPUQuota={args.cpu_quota_percent}%'", source)
        self.assertIn("f'--property=MemoryMax={args.worker_memory_mib}M'", source)
        self.assertIn("'--worker-memory-mib', str(args.worker_memory_mib)", source)
        self.assertIn("'--cpu-quota-percent', str(args.cpu_quota_percent)", source)


if __name__ == "__main__":
    unittest.main()
