#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import sys
import time
import unittest


SCRIPT = Path(__file__).with_name("dux-canary-monitor.py")
SPEC = importlib.util.spec_from_file_location("dux_canary_monitor", SCRIPT)
MONITOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MONITOR
SPEC.loader.exec_module(MONITOR)


EXPECTED_BINARY = "a" * 64
EXPECTED_UNIT = "b" * 64


def status(**overrides):
    value = {"status_version": 1, "daemon_live": True,
             "watch_status_available": True, "caught_up": True,
             "scan_in_progress": False, "dirty": False, "paused": False,
             "pending_updates": 0, "events_dropped": 0, "write_error": None,
             "watch_status_updated_at_seconds": int(time.time())}
    value.update(overrides)
    return value


def sample():
    accounting = {"schema_version": "7", "root_matches_inode_aggregate": True,
                  "sampled_rollup_mismatches": [], "orphan_dirents": 0,
                  "invalid_prime_attribution": 0,
                  "path_graph": {"passed": True, "path_count": 12},
                  "quick_check": [("ok",)]}
    return {"binary_sha256": EXPECTED_BINARY,
            "service_binary_sha256": EXPECTED_BINARY,
            "process_binary_sha256": EXPECTED_BINARY,
            "unit_sha256": EXPECTED_UNIT,
            "status_before": status(), "status_after": status(),
            "service": {"ActiveState": "active", "NRestarts": "0",
                        "ExecMainStatus": "0", "MainPID": "123",
                        "MemoryCurrent": "100", "MemoryMax": "1000",
                        "CPUQuotaPerSecUSec": "1s", "Restart": "always",
                        "WatchdogUSec": "45s", "DropInPaths": ""},
            "accounting": accounting,
            "history": {"rows": 40, "max_bucket": 500},
            "journal": {"critical_event_count": 0, "critical_events": []},
            "memory_events": {"max": 0, "oom": 0, "oom_kill": 0,
                              "oom_group_kill": 0}}


class CanaryMonitorTests(unittest.TestCase):
    def test_healthy_sample_passes(self):
        self.assertEqual(MONITOR.evaluate_sample(sample(), EXPECTED_BINARY,
                                                  EXPECTED_UNIT), [])

    def test_binary_and_unit_hash_mismatch_fail_closed(self):
        value = sample()
        value["process_binary_sha256"] = "c" * 64
        value["unit_sha256"] = "d" * 64
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("process_binary_sha256 differs from expected candidate", errors)
        self.assertIn("unit_sha256 differs from expected candidate unit", errors)

    def test_dirty_dropped_and_unavailable_status_fail(self):
        value = sample()
        value["status_after"] = status(dirty=True, events_dropped=3,
                                       watch_status_available=False)
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("status_after is not versioned, live, and caught up", errors)

    def test_stale_watch_heartbeat_fails(self):
        value = sample()
        value["status_after"]["watch_status_updated_at_seconds"] = 800
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT,
                                         now_timestamp=1000)
        self.assertIn("status_after watch heartbeat is stale or in the future", errors)

    def test_live_false_clean_status_cannot_override_exact_accounting_mismatch(self):
        value = sample()
        # Reproduce the old installed daemon's live shape: both machine status
        # reads claim clean/caught-up, but a consistent SQLite snapshot has a
        # 61,440-byte / 16-inode root-vs-inode delta and the matching /tmp rollup.
        value["status_before"] = status(caught_up=True, dirty=False,
                                        pending_updates=0, scan_in_progress=False)
        value["status_after"] = status(caught_up=True, dirty=False,
                                       pending_updates=0, scan_in_progress=False)
        value["accounting"].update(
            root_matches_inode_aggregate=False,
            root_totals=(123485155328, 1174955),
            inode_totals=(123485093888, 1174939),
            sampled_rollup_mismatches=[{
                "path": "/tmp", "stored_bytes": 2523144192,
                "expected_bytes": 2523082752, "stored_inodes": 47259,
                "expected_inodes": 47243,
            }],
        )
        self.assertTrue(MONITOR.healthy_pipeline(value["status_before"]))
        self.assertTrue(MONITOR.healthy_pipeline(value["status_after"]))
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("exact accounting snapshot failed", errors)

    def test_balanced_directory_rollup_errors_fail_even_when_root_totals_match(self):
        value = sample()
        # Match the live legacy-daemon shape: an oversized /tmp rollup and an
        # undersized nested Go-build rollup cancel exactly at the root. Global
        # totals and SQLite integrity alone therefore look healthy.
        delta_bytes, delta_inodes = 622_592, 49
        value["accounting"].update(
            root_matches_inode_aggregate=True,
            root_totals=(123_940_270_080, 1_176_663),
            inode_totals=(123_940_270_080, 1_176_663),
            sampled_rollup_mismatches=[
                {"path": "/tmp", "stored_bytes": 2_527_391_744,
                 "expected_bytes": 2_526_769_152,
                 "stored_inodes": 48_126, "expected_inodes": 48_077},
                {"path": "/tmp/go-build4152615103",
                 "stored_bytes": 1_000_000 - delta_bytes,
                 "expected_bytes": 1_000_000,
                 "stored_inodes": 100 - delta_inodes,
                 "expected_inodes": 100},
            ],
        )
        self.assertTrue(MONITOR.healthy_pipeline(value["status_before"]))
        self.assertTrue(MONITOR.healthy_pipeline(value["status_after"]))
        self.assertTrue(value["accounting"]["root_matches_inode_aggregate"])
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("exact accounting snapshot failed", errors)

    def test_old_database_schema_fails(self):
        value = sample()
        value["accounting"]["schema_version"] = "4"
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("database schema '4' differs from candidate schema 7", errors)

    def test_v7_path_graph_corruption_fails_canary(self):
        value = sample()
        value["accounting"]["path_graph"] = {
            "passed": False,
            "path_count": 12,
            "bad_parent_count": 1,
        }
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("exact accounting snapshot failed", errors)

    def test_candidate_schema_version_is_explicit(self):
        self.assertEqual(MONITOR.EXPECTED_SCHEMA_VERSION, "7")

    def test_restart_and_resource_limit_breach_fail(self):
        value = sample()
        value["service"]["NRestarts"] = "1"
        value["service"]["MemoryCurrent"] = "1001"
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT,
                                        previous_restarts=0)
        self.assertIn("service restart count changed during canary", errors)
        self.assertIn("service MemoryCurrent exceeds MemoryMax", errors)

    def test_unit_resource_or_dropin_drift_fails(self):
        value = sample()
        value["service"]["CPUQuotaPerSecUSec"] = "infinity"
        value["service"]["DropInPaths"] = "/etc/systemd/system/dux.service.d/override.conf"
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("service CPU quota differs from the qualified 100% limit", errors)
        self.assertIn("unexpected systemd drop-in overrides the qualified unit", errors)

    def test_growth_history_cannot_move_backwards(self):
        value = sample()
        value["history"]["max_bucket"] = 499
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT,
                                         previous_history_bucket=500)
        self.assertIn("growth history maximum bucket moved backwards", errors)

    def test_cgroup_oom_or_limit_counters_cannot_increase(self):
        value = sample()
        value["memory_events"]["oom_kill"] = 1
        errors = MONITOR.evaluate_sample(
            value, EXPECTED_BINARY, EXPECTED_UNIT,
            previous_memory_events={"max": 0, "oom": 0,
                                    "oom_kill": 0, "oom_group_kill": 0})
        self.assertIn("cgroup memory.events oom_kill counter increased", errors)

    def test_recovered_accounting_error_still_fails_canary(self):
        value = sample()
        value["journal"] = {"critical_event_count": 1,
                            "critical_events": ["directory accounting invariant failed"]}
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("journal contains an accounting, overflow, scan, or write failure", errors)

    def test_materialized_totals_mismatch_journal_marker_fails_canary(self):
        value = sample()
        value["journal"] = {
            "critical_event_count": 1,
            "critical_events": ["materialized index totals disagree with inode rows"],
        }
        errors = MONITOR.evaluate_sample(value, EXPECTED_BINARY, EXPECTED_UNIT)
        self.assertIn("journal contains an accounting, overflow, scan, or write failure", errors)

    def test_systemd_properties_are_parsed(self):
        self.assertEqual(MONITOR.parse_properties("ActiveState=active\nNRestarts=0\n"),
                         {"ActiveState": "active", "NRestarts": "0"})

    def test_24_hour_sample_floor_includes_start_and_finish(self):
        self.assertEqual(MONITOR.minimum_sample_count(3600), 25)
        self.assertEqual(MONITOR.minimum_sample_count(60), 1441)


if __name__ == "__main__":
    unittest.main()
