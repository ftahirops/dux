#!/usr/bin/env python3
"""Fail-closed, read-only evidence collector for the 24-hour dux canary."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time


OBSERVER_PATH = Path(__file__).with_name("dux-live-observer.py")
OBSERVER_SPEC = importlib.util.spec_from_file_location("dux_live_observer", OBSERVER_PATH)
OBSERVER = importlib.util.module_from_spec(OBSERVER_SPEC)
OBSERVER_SPEC.loader.exec_module(OBSERVER)


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def parse_properties(stdout):
    result = {}
    for line in stdout.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            result[key] = value
    return result


def unit_properties(unit):
    command = ["systemctl", "show", unit, "-p", "ActiveState", "-p", "NRestarts",
               "-p", "ExecMainStatus", "-p", "MainPID", "-p", "MemoryCurrent",
               "-p", "MemoryMax", "-p", "MemoryPeak", "-p", "CPUUsageNSec",
               "-p", "CPUQuotaPerSecUSec", "-p", "ControlGroup", "-p", "FragmentPath",
               "-p", "DropInPaths", "-p", "Restart", "-p", "WatchdogUSec"]
    result = subprocess.run(command, text=True, capture_output=True, timeout=15)
    if result.returncode:
        raise RuntimeError(f"systemctl show failed: {result.stderr.strip()}")
    return parse_properties(result.stdout)


def memory_events(control_group):
    if not control_group.startswith("/") or ".." in Path(control_group).parts:
        raise RuntimeError(f"invalid systemd ControlGroup path {control_group!r}")
    path = Path("/sys/fs/cgroup") / control_group.lstrip("/") / "memory.events"
    fields = {}
    for line in path.read_text().splitlines():
        key, value = line.split(maxsplit=1)
        fields[key] = int(value)
    required = {"max", "oom", "oom_kill", "oom_group_kill"}
    if not required.issubset(fields):
        raise RuntimeError(f"memory.events lacks required counters: {sorted(required - fields.keys())}")
    return fields


def machine_status(binary, db):
    command = [str(binary), "--db", str(db), "--json", "status"]
    result = subprocess.run(command, text=True, capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"candidate status exited {result.returncode}: {result.stderr[-1000:]}")
    try:
        status = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"candidate status did not return JSON: {error}") from error
    if not isinstance(status, dict):
        raise RuntimeError("candidate status JSON is not an object")
    return status


def running_binary_hash(pid):
    if int(pid) <= 0:
        raise RuntimeError(f"systemd reports invalid MainPID {pid!r}")
    return sha256_file(Path(f"/proc/{int(pid)}/exe"))


def history_state(db):
    uri = f"file:{Path(db).resolve()}?mode=ro"
    with sqlite3.connect(uri, uri=True, timeout=10) as conn:
        conn.execute("PRAGMA query_only=ON")
        conn.execute("BEGIN")
        count, maximum = conn.execute(
            "SELECT COUNT(*),COALESCE(MAX(bucket),0) FROM growth").fetchone()
        return {"rows": int(count), "max_bucket": int(maximum)}


CRITICAL_JOURNAL_MARKERS = (
    "directory accounting invariant failed",
    "materialized index totals disagree with inode rows",
    "fanotify queue overflow",
    "inconsistent directory accounting",
    "could not verify accounting",
    "incomplete coverage",
    "pending backlog exceeded",
    "skip move-to (would cycle)",
    "rescan failed",
    "flush error",
    "write error",
    "panic",
)


def journal_state(unit, since):
    result = subprocess.run(["journalctl", "-u", unit, "--since", since,
                             "--no-pager", "-o", "short-iso-precise"],
                            text=True, capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError(f"journalctl failed: {result.stderr.strip()}")
    lines = result.stdout.splitlines()
    relevant = [line for line in lines
                if any(marker in line.lower() for marker in CRITICAL_JOURNAL_MARKERS)]
    return {"since": since, "sha256": hashlib.sha256(result.stdout.encode()).hexdigest(),
            "line_count": len(lines), "critical_event_count": len(relevant),
            "critical_events": relevant[:100]}


def healthy_pipeline(status):
    return (isinstance(status, dict)
            and status.get("status_version") == 1
            and status.get("daemon_live") is True
            and status.get("watch_status_available") is True
            and status.get("caught_up") is True
            and status.get("scan_in_progress") is False
            and status.get("dirty") is False
            and status.get("paused") is False
            and status.get("pending_updates") == 0
            and status.get("events_dropped") == 0
            and status.get("write_error") is None)


def evaluate_sample(sample, expected_binary_sha256, expected_unit_sha256,
                    previous_restarts=None, previous_history_bucket=None,
                    previous_memory_events=None, now_timestamp=None):
    """Return all fail-closed reasons for one captured canary sample."""
    errors = []
    for key in ("binary_sha256", "service_binary_sha256", "process_binary_sha256"):
        if sample.get(key) != expected_binary_sha256:
            errors.append(f"{key} differs from expected candidate")
    if sample.get("unit_sha256") != expected_unit_sha256:
        errors.append("unit_sha256 differs from expected candidate unit")
    for key in ("status_before", "status_after"):
        status = sample.get(key)
        if not healthy_pipeline(status):
            errors.append(f"{key} is not versioned, live, and caught up")
        else:
            now = time.time() if now_timestamp is None else now_timestamp
            updated = status.get("watch_status_updated_at_seconds")
            try:
                age = now - float(updated)
                if age > 120 or age < -5:
                    errors.append(f"{key} watch heartbeat is stale or in the future")
            except (TypeError, ValueError):
                errors.append(f"{key} watch heartbeat timestamp is missing or invalid")
    if sample.get("service", {}).get("ActiveState") != "active":
        errors.append("systemd service is not active")
    try:
        restarts = int(sample["service"]["NRestarts"])
        main_pid = int(sample["service"]["MainPID"])
        memory_current = int(sample["service"]["MemoryCurrent"])
        memory_max = int(sample["service"]["MemoryMax"])
        quota = sample["service"].get("CPUQuotaPerSecUSec", "")
        if int(sample["service"]["ExecMainStatus"]) != 0:
            errors.append("systemd ExecMainStatus is nonzero")
        if main_pid <= 0:
            errors.append("systemd MainPID is not positive")
        if memory_current > memory_max:
            errors.append("service MemoryCurrent exceeds MemoryMax")
        if quota != "1s":
            errors.append("service CPU quota differs from the qualified 100% limit")
        if sample["service"].get("Restart") != "always":
            errors.append("service restart policy differs from the qualified unit")
        if sample["service"].get("WatchdogUSec") != "45s":
            errors.append("service watchdog differs from the qualified unit")
        if sample["service"].get("DropInPaths"):
            errors.append("unexpected systemd drop-in overrides the qualified unit")
        if previous_restarts is not None and restarts != previous_restarts:
            errors.append("service restart count changed during canary")
    except (KeyError, TypeError, ValueError):
        errors.append("systemd service properties are incomplete or invalid")

    if not isinstance(sample.get("accounting"), dict) or not OBSERVER.invariant_passed(sample["accounting"]):
        errors.append("exact accounting snapshot failed")
    elif sample["accounting"].get("schema_version") != "6":
        errors.append("database schema is not candidate schema 5")
    history = sample.get("history")
    if not isinstance(history, dict):
        errors.append("growth history snapshot is missing")
    elif previous_history_bucket is not None and history.get("max_bucket", -1) < previous_history_bucket:
        errors.append("growth history maximum bucket moved backwards")
    journal = sample.get("journal")
    if not isinstance(journal, dict):
        errors.append("journal sample is missing")
    elif journal.get("critical_event_count", 0) > 0:
        errors.append("journal contains an accounting, overflow, scan, or write failure")
    events = sample.get("memory_events")
    if not isinstance(events, dict):
        errors.append("cgroup memory.events counters are missing")
    elif previous_memory_events is not None:
        for key in ("max", "oom", "oom_kill", "oom_group_kill"):
            if events.get(key, -1) > previous_memory_events.get(key, -1):
                errors.append(f"cgroup memory.events {key} counter increased")
    return errors


def capture_sample(binary, service_binary, db, unit, journal_since):
    status_before = machine_status(binary, db)
    service = unit_properties(unit)
    candidate_hash = sha256_file(binary)
    service_hash = sha256_file(service_binary)
    process_hash = running_binary_hash(service.get("MainPID", "0"))
    fragment = service.get("FragmentPath", "")
    if not fragment or not Path(fragment).is_file():
        raise RuntimeError(f"systemd unit FragmentPath is unavailable: {fragment!r}")
    unit_hash = sha256_file(fragment)
    events = memory_events(service.get("ControlGroup", ""))

    OBSERVER.DB = Path(db)
    accounting = OBSERVER.snapshot()
    history = history_state(db)
    status_after = machine_status(binary, db)
    journal = journal_state(unit, journal_since)
    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    return {"captured_at_utc": now, "binary_sha256": candidate_hash,
            "service_binary_sha256": service_hash, "process_binary_sha256": process_hash,
            "unit_path": fragment, "unit_sha256": unit_hash,
            "service": service, "memory_events": events, "status_before": status_before,
            "accounting": accounting, "history": history, "journal": journal,
            "status_after": status_after}


def append_record(fd, record):
    payload = (json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n").encode()
    offset = 0
    while offset < len(payload):
        offset += os.write(fd, payload[offset:])
    os.fsync(fd)


def minimum_sample_count(interval_seconds):
    return (24 * 60 * 60 + interval_seconds - 1) // interval_seconds + 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("/usr/bin/dux"))
    parser.add_argument("--service-binary", type=Path, default=Path("/usr/bin/dux"))
    parser.add_argument("--db", type=Path, default=Path("/var/lib/dux/dux.db"))
    parser.add_argument("--unit", default="dux.service")
    parser.add_argument("--expected-binary-sha256", required=True)
    parser.add_argument("--expected-unit-sha256", required=True)
    parser.add_argument("--interval-seconds", type=int, default=3600,
                        help="sample interval, 60..3600 seconds")
    parser.add_argument("--output", type=Path, required=True,
                        help="new root-readable evidence file; must not already exist")
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("root is required to read the complete system index and process executable")
    if not 60 <= args.interval_seconds <= 3600:
        parser.error("interval must be 60..3600 seconds")
    for name, expected in (("binary", args.expected_binary_sha256),
                           ("unit", args.expected_unit_sha256)):
        if len(expected) != 64 or any(ch not in "0123456789abcdef" for ch in expected):
            parser.error(f"expected {name} SHA-256 must be 64 lowercase hexadecimal characters")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.output.resolve() == args.db.resolve() or args.db.resolve() in args.output.resolve().parents:
        parser.error("evidence output must be outside the index database directory")
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_APPEND, 0o600)
    os.fchmod(fd, 0o600)
    start = time.monotonic()
    end = start + 24 * 60 * 60
    count = 0
    previous_restarts = None
    previous_history_bucket = None
    previous_memory_events = None
    previous_sample_at = (datetime.datetime.now(datetime.timezone.utc)
                          - datetime.timedelta(minutes=1)).isoformat()
    common = {"format_version": 1, "expected_binary_sha256": args.expected_binary_sha256,
              "expected_unit_sha256": args.expected_unit_sha256, "unit": args.unit,
              "db": str(args.db), "interval_seconds": args.interval_seconds,
              "duration_seconds": 86400,
              "monitor_sha256": sha256_file(__file__),
              "observer_sha256": sha256_file(OBSERVER_PATH)}
    try:
        append_record(fd, {**common, "event": "canary_started",
                           "started_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat()})
        while True:
            try:
                sample = capture_sample(args.binary, args.service_binary, args.db, args.unit,
                                        previous_sample_at)
                errors = evaluate_sample(sample, args.expected_binary_sha256,
                                         args.expected_unit_sha256, previous_restarts,
                                         previous_history_bucket, previous_memory_events)
                restarts = int(sample["service"]["NRestarts"])
                history_bucket = int(sample["history"]["max_bucket"])
                events = sample["memory_events"]
                append_record(fd, {**common, "event": "sample", "sample_number": count + 1,
                                   "result": "passed" if not errors else "failed",
                                   "errors": errors, **sample})
                count += 1
                if errors:
                    append_record(fd, {**common, "event": "canary_failed",
                                       "failed_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                       "sample_number": count, "errors": errors})
                    return 1
                if previous_restarts is None:
                    previous_restarts = restarts
                previous_history_bucket = history_bucket
                if previous_memory_events is None:
                    previous_memory_events = events
                previous_sample_at = sample["captured_at_utc"]
            except Exception as error:
                append_record(fd, {**common, "event": "canary_failed",
                                   "failed_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                   "sample_number": count + 1,
                                   "errors": [f"sample collection failed: {type(error).__name__}: {error}"]})
                return 1

            elapsed = time.monotonic() - start
            if elapsed >= 24 * 60 * 60:
                minimum_samples = minimum_sample_count(args.interval_seconds)
                if count < minimum_samples:
                    errors = [f"only {count} samples recorded; need at least {minimum_samples} over 24 hours"]
                    append_record(fd, {**common, "event": "canary_failed",
                                       "failed_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                       "sample_number": count, "errors": errors})
                    print("FAIL 24-hour canary: insufficient hourly samples; see evidence", args.output, flush=True)
                    return 1
                append_record(fd, {**common, "event": "canary_completed",
                                   "completed_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                   "elapsed_seconds": elapsed, "samples_passed": count,
                                   "minimum_samples": minimum_samples})
                print(f"PASS 24-hour canary: {count} exact health/accounting samples; evidence {args.output}", flush=True)
                return 0
            time.sleep(min(args.interval_seconds, 24 * 60 * 60 - elapsed))
    finally:
        os.close(fd)


if __name__ == "__main__":
    raise SystemExit(main())
