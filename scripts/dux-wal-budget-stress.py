#!/usr/bin/env python3
"""Qualify Dux's large-WAL write guard with a held SQLite reader."""
import contextlib
import argparse
import hashlib
import os
from pathlib import Path
import sys
import time

import importlib.util

HARNESS = Path(__file__).with_name("dux-readiness.py")
spec = importlib.util.spec_from_file_location("dux_readiness", HARNESS)
readiness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(readiness)

preflight = argparse.ArgumentParser(add_help=False)
preflight.add_argument("--binary", type=Path, required=True)
preflight.add_argument("--churn-operations", type=int)
pre_args, _ = preflight.parse_known_args()
expected_hash = os.environ.get("DUX_EXPECTED_BINARY_SHA256")
actual_hash = hashlib.sha256(pre_args.binary.read_bytes()).hexdigest()
if expected_hash and actual_hash != expected_hash:
    raise SystemExit(f"candidate binary hash mismatch: expected {expected_hash}, got {actual_hash}")

minimum_inode_reserve = int(os.environ.get("DUX_WAL_STRESS_MAX_FILES", "220000")) + int(
    os.environ.get("DUX_WAL_STRESS_BATCH_FILES", "500")
)
if pre_args.churn_operations is None:
    # The shared fixture builder sizes its ext4 inode table from this value.
    sys.argv.extend(["--churn-operations", str(minimum_inode_reserve)])
elif pre_args.churn_operations < minimum_inode_reserve:
    raise SystemExit(
        "--churn-operations must be at least the WAL stress file limit plus "
        f"one post-guard batch ({minimum_inode_reserve}) to reserve enough ext4 inodes"
    )

MIB = 1024 * 1024
GUARD = "SQLite WAL exceeded its budget"
RECOVERED = "SQLite WAL checkpoint recovered; event writes resumed"


def wal_budget_test(case, _args, evidence):
    # Keep this aligned with MAX_WAL_BYTES in src/watch.rs. The write guard
    # triggers only after the WAL grows strictly larger than this threshold.
    target_bytes = int(os.environ.get("DUX_WAL_STRESS_TARGET_MIB", "256")) * MIB
    max_files = int(os.environ.get("DUX_WAL_STRESS_MAX_FILES", "220000"))
    batch_files = int(os.environ.get("DUX_WAL_STRESS_BATCH_FILES", "500"))
    file_bytes = int(os.environ.get("DUX_WAL_STRESS_FILE_BYTES", "0"))
    daemon = readiness.Daemon(case)
    daemon.exact(timeout=120)
    reader = readiness.snapshot(case.db)
    original_snapshot = reader.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone()
    wal = Path(str(case.db) + "-wal")
    created = 0
    pending_at_guard = None

    def log_text():
        try:
            return (case.base / "daemon.log").read_text(errors="replace")
        except FileNotFoundError:
            return ""

    try:
        while created < max_files:
            stop = min(created + batch_files, max_files)
            for index in range(created, stop):
                (case.tree / f"wal-budget-{index:07d}").write_bytes(b"w" * file_bytes)
            created = stop

            def batch_drained_or_guarded():
                if GUARD in log_text():
                    return True
                status = daemon.record()
                return status["pending"] == 0 and status["drained"]

            daemon.wait(batch_drained_or_guarded, "WAL stress batch drain or write guard", timeout=120)
            if GUARD in log_text():
                break
            if wal.exists() and wal.stat().st_size >= target_bytes:
                daemon.wait(lambda: GUARD in log_text(), "large-WAL write guard", timeout=30)
                break

        log = log_text()
        assert GUARD in log, (
            f"write guard did not activate before {created} files; "
            f"WAL={wal.stat().st_size if wal.exists() else 0}, target={target_bytes}"
        )
        peak = wal.stat().st_size if wal.exists() else 0
        assert peak >= target_bytes, (peak, target_bytes)

        # Capture the live index state after the guard, then create more
        # changes. Require the write guard to remain active while these events
        # queue and verify that committed index state does not move.
        with contextlib.closing(readiness.sqlite3.connect(case.db)) as connection:
            committed_at_guard = connection.execute(
                "SELECT SUM(blocks),COUNT(*) FROM inodes"
            ).fetchone()
        for index in range(batch_files):
            (case.tree / f"wal-after-guard-{index:05d}").write_bytes(b"queued" * 256)

        def guarded_backlog():
            log = log_text()
            status = daemon.record()
            if status["pending"] == 0 and status["drained"]:
                return False
            with contextlib.closing(readiness.sqlite3.connect(case.db)) as connection:
                committed = connection.execute(
                    "SELECT SUM(blocks),COUNT(*) FROM inodes"
                ).fetchone()
            return {
                "status": status,
                "guard_still_active": GUARD in log and RECOVERED not in log,
                "committed_index_unchanged": committed == committed_at_guard,
                "committed_at_guard": committed_at_guard,
                "committed_while_guarded": committed,
            }

        pending_at_guard = daemon.wait(
            guarded_backlog,
            "event backlog while WAL writes are guarded", timeout=30,
        )
        assert pending_at_guard["guard_still_active"], pending_at_guard
        assert pending_at_guard["committed_index_unchanged"], pending_at_guard
        assert reader.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone() == original_snapshot
        assert reader.execute("PRAGMA integrity_check").fetchall() == [("ok",)]

        reader.close()
        reader = None
        daemon.wait(lambda: RECOVERED in log_text(), "WAL checkpoint recovery after reader release", timeout=180)
        daemon.exact(timeout=300)
    finally:
        if reader is not None:
            reader.close()
        daemon.stop()

    with contextlib.closing(readiness.sqlite3.connect(case.db)) as connection:
        checkpoint = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
    assert checkpoint[0] == 0, checkpoint
    final = wal.stat().st_size if wal.exists() else 0
    assert final == 0, f"WAL did not reclaim after reader release: {final}"
    readiness.integrity(case.db)
    case.history()
    evidence.update(
        held_reader_snapshot_survived=True,
        wal_guard_observed=True,
        event_backlog_observed_while_guarded=True,
        checkpoint_recovered_after_reader_release=True,
        exact_catchup_after_release=True,
        peak_wal_bytes=peak,
        wal_guard_target_bytes=target_bytes,
        created_files=created,
        file_bytes=file_bytes,
        pending_status_at_guard=pending_at_guard,
        quiescent_wal_bytes=final,
        wal_budget_stress_script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    )
    print(
        f"PASS held-reader WAL guard/recovery: {peak} WAL bytes, "
        f"{created} files, exact after release, WAL reclaimed",
        flush=True,
    )


readiness.SUITES["wal"] = wal_budget_test
readiness.main()
