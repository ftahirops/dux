import importlib.util
import json
import pathlib
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch


ARCHIVER = pathlib.Path(__file__).resolve().parents[1] / "scripts/dux-soak-archive.py"
SPEC = importlib.util.spec_from_file_location("dux_soak_archive", ARCHIVER)
ARCHIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ARCHIVE)


def complete_progress(candidate_hash="candidate"):
    return {"status": "passed", "started_at": "2026-10-27T08:45:52Z",
            "elapsed_seconds": 259200,
            "candidate_binary_sha256": candidate_hash, "operations": 1000,
            "exact_check_interval_operations": 100,
            "final_exact_passed": True, "final_history_passed": True,
            "sqlite_integrity": "ok", "mutation_bursts": 71,
            "held_readers": [{"held_seconds": 300, "wal_reclaimed": True,
                              "snapshot_inodes": 42} for _ in range(11)],
            "controlled_crashes": [{"pid": 100 + i, "restart_pid": 200 + i,
                                     "downtime_paths": 64} for i in range(5)],
            "samples": [{"operations": 100, "rss_kib": 1024, "fds": 11,
                         "wal_bytes": 0, "exact_convergence_ms": 50}]}


class ProgressFreshnessTests(unittest.TestCase):
    def test_fresh_progress_inside_limit(self):
        self.assertEqual(ARCHIVE.progress_freshness(1000, 750, 300), ("fresh", 250))

    def test_progress_at_limit_is_fresh(self):
        self.assertEqual(ARCHIVE.progress_freshness(1000, 700, 300), ("fresh", 300))

    def test_progress_past_limit_is_stale(self):
        self.assertEqual(ARCHIVE.progress_freshness(1000, 699.9, 300),
                         ("stale_guest_progress", 300.1))

    def test_material_future_mtime_fails_closed_as_clock_skew(self):
        self.assertEqual(ARCHIVE.progress_freshness(1000, 1031, 300),
                         ("guest_clock_skew", -31))

    def test_short_fresh_pass_cannot_qualify_as_72_hour_soak(self):
        result = ARCHIVE.evaluate_qualification(
            {"status": "passed", "elapsed_seconds": 135,
             "candidate_binary_sha256": "candidate", "samples": [],
             "mutation_bursts": 0, "held_readers": [], "controlled_crashes": []},
            "candidate", {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            "oom 0\noom_kill 0\noom_group_kill 0\n")
        self.assertEqual(result["state"], "not_qualified")
        self.assertIn("required_duration_not_met", result["reasons"])
        self.assertIn("six_hour_reader_phases_incomplete", result["reasons"])

    def test_qualification_duration_cannot_be_lowered_by_cli(self):
        self.assertEqual(ARCHIVE.REQUIRED_DURATION_SECONDS, 259200)
        result = subprocess.run(["python3", str(ARCHIVER), "--help"],
                                capture_output=True, text=True, check=True)
        self.assertNotIn("--required-duration-seconds", result.stdout)

    def test_complete_hash_bound_soak_requires_all_scheduled_phases(self):
        result = ARCHIVE.evaluate_qualification(
            complete_progress(),
            "candidate", {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            "oom 0\noom_kill 0\noom_group_kill 0\n")
        self.assertEqual(result, {"state": "qualified", "reasons": [],
                                  "required_duration_seconds": 259200})

    def test_candidate_mismatch_and_oom_fail_qualification(self):
        result = ARCHIVE.evaluate_qualification(
            {"status": "passed", "elapsed_seconds": 259200,
             "candidate_binary_sha256": "wrong", "samples": [{"operations": 100}],
             "mutation_bursts": 71,
             "held_readers": [{"held_seconds": 300, "wal_reclaimed": True} for _ in range(11)],
             "controlled_crashes": [{} for _ in range(5)]},
            "expected", {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            {"ActiveState": "active", "NRestarts": "0"},
            "oom 0\noom_kill 1\noom_group_kill 0\n")
        self.assertEqual(result["state"], "failed")
        self.assertIn("candidate_binary_hash_missing_or_mismatched", result["reasons"])
        self.assertIn("memory_event_oom_kill_nonzero", result["reasons"])

    def test_passed_soak_accepts_cleanly_exited_worker_and_controller(self):
        progress = complete_progress()
        worker = {"ActiveState": "inactive", "Result": "success",
                  "ExecMainStatus": "0", "NRestarts": "0"}
        controller = {"ActiveState": "inactive", "Result": "success",
                      "ExecMainStatus": "0", "NRestarts": "0"}
        host = {"ActiveState": "active", "NRestarts": "0"}
        result = ARCHIVE.evaluate_qualification(
            progress, "candidate", worker, controller, host,
            "oom 0\noom_kill 0\noom_group_kill 0\n")
        self.assertEqual(result["state"], "qualified")

    def test_collect_fails_stale_progress_even_when_services_are_active(self):
        guest = {
            "guest_time_utc": "1970-01-01T00:21:40Z",
            "guest_time_epoch": 1300,
            "progress_mtime_epoch": 999,
            "progress_sha256": "test-progress-hash",
            "progress_bytes": 10,
            "progress": {"status": "running", "elapsed_seconds": 900,
                         "operations": 900, "samples": []},
            "worker": {"fields": {"ActiveState": "active"}},
            "controller": {"fields": {"ActiveState": "active"}},
            "memory_events": "oom 0\noom_kill 0\n",
        }
        active_vm = {"returncode": 0, "fields": {"ActiveState": "active"}}
        with tempfile.TemporaryDirectory() as temp_dir:
            args = SimpleNamespace(
                identity=pathlib.Path("unused-key"),
                known_hosts=pathlib.Path("unused-known-hosts"),
                port=22,
                output_dir=pathlib.Path(temp_dir),
                once=True,
                next_snapshot=0,
                full_snapshot_interval=900,
                max_progress_age_seconds=300,
            )
            completed = subprocess.CompletedProcess([], 0, json.dumps(guest), "")
            with patch.object(ARCHIVE, "run", return_value=completed), \
                    patch.object(ARCHIVE, "host_unit", return_value=active_vm):
                row = ARCHIVE.collect(args)
            self.assertEqual(row["status"], "stale_guest_progress")
            self.assertEqual(row["progress_freshness"], "stale_guest_progress")
            saved = json.loads((pathlib.Path(temp_dir) / "checkpoints.jsonl").read_text())
            self.assertEqual(saved["status"], "stale_guest_progress")

    def test_collect_uses_run_specific_paths_units_and_host_vm(self):
        guest = {
            "guest_time_utc": "1970-01-01T00:21:40Z",
            "guest_time_epoch": 1300,
            "progress_mtime_epoch": 1200,
            "progress_sha256": "test-progress-hash",
            "progress_bytes": 10,
            "progress": {"status": "running", "elapsed_seconds": 100,
                         "operations": 100, "samples": []},
            "worker": {"fields": {"ActiveState": "active"}},
            "controller": {"fields": {"ActiveState": "active"}},
            "memory_events": "oom 0\noom_kill 0\n",
        }
        active_vm = {"returncode": 0, "fields": {"ActiveState": "active"}}
        with tempfile.TemporaryDirectory() as temp_dir:
            args = SimpleNamespace(
                identity=pathlib.Path("unused-key"),
                known_hosts=pathlib.Path("unused-known-hosts"),
                port=22,
                output_dir=pathlib.Path(temp_dir),
                once=True,
                next_snapshot=0,
                full_snapshot_interval=900,
                max_progress_age_seconds=300,
                progress_path="/root/readiness/new-soak/progress.json",
                worker_unit="dux-readiness-12345.service",
                controller_unit="dux-soak-control-new.service",
                memory_events_path="/sys/fs/cgroup/system.slice/dux-readiness-12345.service/memory.events",
                host_vm_unit="dux-qualification-vm-rocky9.service",
            )
            completed = subprocess.CompletedProcess([], 0, json.dumps(guest), "")
            with patch.object(ARCHIVE, "run", return_value=completed) as remote_run, \
                    patch.object(ARCHIVE, "host_unit", return_value=active_vm) as local_unit:
                row = ARCHIVE.collect(args)
            self.assertEqual(row["status"], "ok")
            self.assertEqual(row["qualification"]["state"], "in_progress")
            local_unit.assert_called_once_with("dux-qualification-vm-rocky9.service")
            command = remote_run.call_args.args[0]
            self.assertIn("DUX_SOAK_PROGRESS_PATH=/root/readiness/new-soak/progress.json", command[-1])
            self.assertIn("DUX_SOAK_WORKER_UNIT=dux-readiness-12345.service", command[-1])
            self.assertIn("DUX_SOAK_CONTROLLER_UNIT=dux-soak-control-new.service", command[-1])
            self.assertIn("DUX_SOAK_MEMORY_EVENTS_PATH=/sys/fs/cgroup/system.slice/dux-readiness-12345.service/memory.events", command[-1])

    def test_collect_records_pass_after_worker_exits_successfully(self):
        guest = {
            "guest_time_epoch": 1300, "progress_mtime_epoch": 1200,
            "progress_sha256": "progress-hash", "progress_bytes": 100,
            "progress": complete_progress(),
            "worker": {"fields": {"ActiveState": "inactive", "Result": "success",
                                    "ExecMainStatus": "0", "NRestarts": "0"}},
            "controller": {"fields": {"ActiveState": "inactive", "Result": "success",
                                         "ExecMainStatus": "0", "NRestarts": "0"}},
            "memory_events": "oom 0\noom_kill 0\noom_group_kill 0\n",
        }
        active_vm = {"returncode": 0, "fields": {"ActiveState": "active", "NRestarts": "0"}}
        with tempfile.TemporaryDirectory() as temp_dir:
            args = SimpleNamespace(
                identity=pathlib.Path("unused-key"), known_hosts=pathlib.Path("unused-known-hosts"),
                port=22, output_dir=pathlib.Path(temp_dir), once=True, next_snapshot=0,
                full_snapshot_interval=900, max_progress_age_seconds=300,
                expected_candidate_sha256="candidate",
            )
            completed = subprocess.CompletedProcess([], 0, json.dumps(guest), "")
            with patch.object(ARCHIVE, "run", return_value=completed), \
                    patch.object(ARCHIVE, "host_unit", return_value=active_vm):
                row = ARCHIVE.collect(args)
        self.assertEqual(row["status"], "ok")
        self.assertEqual(row["qualification"]["state"], "qualified")

    def test_collected_units_require_continuous_healthy_archive_history(self):
        now = ARCHIVE.dt.datetime.now(ARCHIVE.dt.timezone.utc)
        start_time = now - ARCHIVE.dt.timedelta(seconds=120)
        started_at = start_time.strftime("%Y-%m-%dT%H:%M:%SZ")
        last_observation = now - ARCHIVE.dt.timedelta(seconds=60)
        active = {"ActiveState": "active", "NRestarts": "0"}
        host = {"ActiveState": "active", "NRestarts": "0"}
        checkpoint = {
            "observed_at_utc": last_observation.isoformat(),
            "status": "ok", "progress_freshness": "fresh",
            "progress_age_seconds": 51.0,
            "guest": {
                "progress": {"status": "running", "started_at": started_at,
                             "candidate_binary_sha256": "candidate"},
                "worker": {"fields": active},
                "controller": {"fields": active},
                "memory_events": "oom 0\noom_kill 0\noom_group_kill 0\n",
            },
            "host_vm": {"fields": host},
        }
        guest = {
            "guest_time_epoch": 1300, "progress_mtime_epoch": 1200,
            "progress_sha256": "progress-hash", "progress_bytes": 100,
            "progress": {**complete_progress(), "started_at": started_at},
            # --collect removes transient unit records after clean completion.
            "worker": {"returncode": 1, "fields": {}},
            "controller": {"returncode": 1, "fields": {}},
            "memory_events": "oom 0\noom_kill 0\noom_group_kill 0\n",
        }
        active_vm = {"returncode": 0, "fields": host}
        with tempfile.TemporaryDirectory() as temp_dir:
            output_dir = pathlib.Path(temp_dir)
            (output_dir / "checkpoints.jsonl").write_text(json.dumps(checkpoint) + "\n")
            args = SimpleNamespace(
                identity=pathlib.Path("unused-key"), known_hosts=pathlib.Path("unused-known-hosts"),
                port=22, output_dir=output_dir, once=True, next_snapshot=0,
                full_snapshot_interval=900, max_progress_age_seconds=300,
                expected_candidate_sha256="candidate",
            )
            completed = subprocess.CompletedProcess([], 0, json.dumps(guest), "")
            with patch.object(ARCHIVE, "run", return_value=completed), \
                    patch.object(ARCHIVE, "host_unit", return_value=active_vm):
                row = ARCHIVE.collect(args)
        self.assertTrue(row["prior_run_units_healthy"])
        self.assertEqual(row["status"], "ok")
        self.assertEqual(row["qualification"]["state"], "qualified")

    def test_collected_units_without_healthy_history_cannot_qualify(self):
        result = ARCHIVE.evaluate_qualification(
            complete_progress(), "candidate", {}, {},
            {"ActiveState": "active", "NRestarts": "0"},
            "oom 0\noom_kill 0\noom_group_kill 0\n")
        self.assertEqual(result["state"], "not_qualified")
        self.assertIn("guest_controller_not_active_at_capture", result["reasons"])


if __name__ == "__main__":
    unittest.main()
