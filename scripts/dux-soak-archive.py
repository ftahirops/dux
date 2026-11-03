#!/usr/bin/env python3
"""Read-only host-side archiver for the isolated dux endurance guest."""
import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time

REMOTE = r'''import hashlib,json,os,subprocess,time
p=os.environ.get("DUX_SOAK_PROGRESS_PATH","/root/readiness/soak-progress.json")
worker_unit=os.environ.get("DUX_SOAK_WORKER_UNIT","dux-readiness-641.service")
controller_unit=os.environ.get("DUX_SOAK_CONTROLLER_UNIT","dux-soak-control-e30f4288.service")
memory_events_path=os.environ.get("DUX_SOAK_MEMORY_EVENTS_PATH","/sys/fs/cgroup/system.slice/dux-readiness-641.service/memory.events")
with open(p,"rb") as stream:
 raw=stream.read()
 progress_mtime_epoch=os.fstat(stream.fileno()).st_mtime
try: progress=json.loads(raw)
except Exception as e: progress={"status":"invalid","error":repr(e)}
def show(unit):
 if not unit: return {"returncode":0,"fields":{}}
 r=subprocess.run(["systemctl","show",unit,"-p","ActiveState","-p","Result","-p","ExecMainStatus","-p","MainPID","-p","NRestarts","-p","MemoryCurrent"],capture_output=True,text=True)
 return {"returncode":r.returncode,"fields":dict(line.split("=",1) for line in r.stdout.splitlines() if "=" in line)}
try: events=open(memory_events_path).read()
except Exception as e: events=repr(e)
out={"guest_time_utc":time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime()),"guest_time_epoch":time.time(),"progress_mtime_epoch":progress_mtime_epoch,"progress_sha256":hashlib.sha256(raw).hexdigest(),"progress_bytes":len(raw),"progress":progress,"worker":show(worker_unit),"controller":show(controller_unit),"memory_events":events}
print(json.dumps(out,separators=(",",":")))'''

REQUIRED_DURATION_SECONDS = 72 * 60 * 60


def run(argv, timeout=20):
    return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=timeout)


def utc_stamp():
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def progress_freshness(guest_time_epoch, progress_mtime_epoch, max_age_seconds):
    age = guest_time_epoch - progress_mtime_epoch
    if age < -30:
        return "guest_clock_skew", age
    if age > max_age_seconds:
        return "stale_guest_progress", age
    return "fresh", age


def evaluate_qualification(progress, expected_candidate_sha256, worker_fields,
                           controller_fields, host_fields, memory_events,
                           capture_status="ok", prior_units_healthy=False):
    """Separate a complete, hash-bound 72h pass from healthy archive capture."""
    reasons = []
    if capture_status != "ok":
        reasons.append(f"archive_capture_{capture_status}")
    if not expected_candidate_sha256:
        reasons.append("expected_candidate_hash_not_supplied")
    elif progress.get("candidate_binary_sha256") != expected_candidate_sha256:
        reasons.append("candidate_binary_hash_missing_or_mismatched")

    status = progress.get("status")
    if status != "passed":
        reasons.append(f"soak_status_{status or 'missing'}")
    try:
        elapsed = float(progress.get("elapsed_seconds", 0))
    except (TypeError, ValueError):
        elapsed = 0
    if elapsed < REQUIRED_DURATION_SECONDS:
        reasons.append("required_duration_not_met")

    bursts = progress.get("mutation_bursts", 0)
    if not isinstance(bursts, int) or bursts < 71:
        reasons.append("hourly_mutation_bursts_incomplete")
    readers = progress.get("held_readers")
    if (not isinstance(readers, list) or len(readers) < 11 or
            any(not isinstance(item, dict) or item.get("held_seconds", 0) < 300 or
                item.get("wal_reclaimed") is not True for item in readers)):
        reasons.append("six_hour_reader_phases_incomplete")
    crashes = progress.get("controlled_crashes")
    if (not isinstance(crashes, list) or len(crashes) < 5 or
            any(not isinstance(item, dict) or
                not isinstance(item.get("pid"), int) or
                not isinstance(item.get("restart_pid"), int) or
                item.get("pid") == item.get("restart_pid") or
                not isinstance(item.get("downtime_paths"), int) or
                item.get("downtime_paths") < 64 for item in crashes)):
        reasons.append("twelve_hour_restart_phases_incomplete")
    if (not isinstance(readers, list) or
            any(not isinstance(item, dict) or
                not isinstance(item.get("snapshot_inodes"), int) or
                item.get("snapshot_inodes") < 1 for item in readers)):
        reasons.append("reader_snapshot_evidence_incomplete")
    samples = progress.get("samples")
    if (not isinstance(samples, list) or not samples or
            any(not isinstance(item, dict) or
                not isinstance(item.get("operations"), int) or item.get("operations") < 1 or
                not isinstance(item.get("rss_kib"), int) or item.get("rss_kib") < 1 or
                not isinstance(item.get("fds"), int) or item.get("fds") < 1 or
                not isinstance(item.get("exact_convergence_ms"), (int, float)) or
                item.get("exact_convergence_ms") < 0 for item in samples) or
            any(a["operations"] >= b["operations"] for a, b in zip(samples, samples[1:]))):
        reasons.append("no_exactness_samples")
    if progress.get("final_exact_passed") is not True:
        reasons.append("final_exact_check_missing_or_failed")
    if progress.get("final_history_passed") is not True:
        reasons.append("final_history_check_missing_or_failed")
    if progress.get("sqlite_integrity") != "ok":
        reasons.append("final_sqlite_integrity_missing_or_failed")

    if status == "running" and worker_fields.get("ActiveState") != "active":
        reasons.append("guest_worker_not_active_during_run")
    elif status == "passed":
        worker_active = worker_fields.get("ActiveState") == "active"
        if not worker_active and not (cleanly_exited(worker_fields) or
                                      (prior_units_healthy and not worker_fields)):
            reasons.append("guest_worker_not_active_or_cleanly_exited")
    controller_active = controller_fields.get("ActiveState") == "active"
    if (status == "running" and not controller_active) or (
            status == "passed" and not controller_active and
            not (cleanly_exited(controller_fields) or
                 (prior_units_healthy and not controller_fields))):
        reasons.append("guest_controller_not_active_at_capture")
    for label, fields in (("guest_worker", worker_fields),
                          ("guest_controller", controller_fields),
                          ("host_vm", host_fields)):
        try:
            restarts = int(fields["NRestarts"])
        except (KeyError, TypeError, ValueError):
            if not (label in ("guest_worker", "guest_controller") and
                    status == "passed" and prior_units_healthy and not fields):
                reasons.append(f"{label}_restart_count_unavailable")
        else:
            if restarts != 0:
                reasons.append(f"{label}_restart_count_nonzero")

    events = {}
    if isinstance(memory_events, str):
        for line in memory_events.splitlines():
            parts = line.split()
            if len(parts) == 2:
                try:
                    events[parts[0]] = int(parts[1])
                except ValueError:
                    pass
    for key in ("oom", "oom_kill", "oom_group_kill"):
        if key not in events:
            reasons.append(f"memory_event_{key}_unavailable")
        elif events[key] != 0:
            reasons.append(f"memory_event_{key}_nonzero")

    fatal = (status == "failed" or capture_status != "ok" or
             "candidate_binary_hash_missing_or_mismatched" in reasons or
             any("nonzero" in reason for reason in reasons))
    if fatal:
        state = "failed"
    elif status == "running":
        state = "in_progress"
    elif status == "passed" and not reasons:
        state = "qualified"
    else:
        state = "not_qualified"
    return {"state": state, "reasons": reasons,
            "required_duration_seconds": REQUIRED_DURATION_SECONDS}


def host_unit(unit="dux-qualification-vm-debian12.service"):
    result = subprocess.run(
        ["systemctl", "show", unit,
         "-p", "ActiveState", "-p", "Result", "-p", "MainPID", "-p",
         "NRestarts", "-p", "MemoryCurrent", "-p", "MemoryMax", "-p",
         "OOMScoreAdjust"], capture_output=True, text=True, timeout=5)
    return {"returncode": result.returncode,
            "fields": dict(line.split("=", 1) for line in result.stdout.splitlines()
                            if "=" in line)}


def cleanly_exited(fields):
    return (fields.get("ActiveState") == "inactive" and
            fields.get("Result") == "success" and
            fields.get("ExecMainStatus") == "0")


def memory_oom_counters_zero(raw):
    if not isinstance(raw, str):
        return False
    counters = {}
    for line in raw.splitlines():
        parts = line.split()
        if len(parts) == 2:
            try:
                counters[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return all(counters.get(key) == 0
               for key in ("oom", "oom_kill", "oom_group_kill"))


def prior_run_archive_healthy(output_dir, started_at, candidate_sha256,
                              max_progress_age_seconds, observed_at=None):
    """Verify archived health while transient guest units were still active.

    The soak harness and its worker are launched with systemd-run --collect, so
    systemd may remove both unit records after a successful run. A final
    `passed` progress record can use this history only if every archived row
    from the run start onward was fresh and showed active, unrestarted units
    and zero guest OOM counters.
    """
    if not started_at:
        return False
    try:
        start = dt.datetime.fromisoformat(started_at.replace("Z", "+00:00"))
        end = dt.datetime.fromisoformat(
            (observed_at or dt.datetime.now(dt.timezone.utc).isoformat()).replace(
                "Z", "+00:00"))
        lines = (Path(output_dir) / "checkpoints.jsonl").read_text().splitlines()
    except (OSError, TypeError, ValueError):
        return False
    found = False
    observation_times = []
    for line in lines:
        try:
            row = json.loads(line)
            observed = dt.datetime.fromisoformat(
                row["observed_at_utc"].replace("Z", "+00:00"))
        except (KeyError, TypeError, ValueError, json.JSONDecodeError):
            return False
        if observed < start:
            continue
        found = True
        observation_times.append(observed)
        guest = row.get("guest")
        progress = guest.get("progress") if isinstance(guest, dict) else None
        if (row.get("status") != "ok" or
                row.get("progress_freshness") != "fresh" or
                row.get("progress_age_seconds", max_progress_age_seconds + 1) >
                max_progress_age_seconds or
                not isinstance(progress, dict) or
                progress.get("started_at") != started_at or
                progress.get("candidate_binary_sha256") != candidate_sha256):
            return False
        for unit in (guest.get("worker"), guest.get("controller"),
                     row.get("host_vm")):
            fields = unit.get("fields", {}) if isinstance(unit, dict) else {}
            if (fields.get("ActiveState") != "active" or
                    fields.get("NRestarts") != "0"):
                return False
        if not memory_oom_counters_zero(guest.get("memory_events")):
            return False
    if not found or (observation_times[0] - start).total_seconds() > max_progress_age_seconds:
        return False
    if any((later - earlier).total_seconds() > max_progress_age_seconds
           for earlier, later in zip(observation_times, observation_times[1:])):
        return False
    return (end - observation_times[-1]).total_seconds() <= max_progress_age_seconds


def collect(args):
    output_dir = args.output_dir
    row = {"observed_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
           "host_vm": host_unit(getattr(args, "host_vm_unit", "dux-qualification-vm-debian12.service")), "status": "ok"}
    remote_command = shlex.join([
        "env",
        f"DUX_SOAK_PROGRESS_PATH={getattr(args, 'progress_path', '/root/readiness/soak-progress.json')}",
        f"DUX_SOAK_WORKER_UNIT={getattr(args, 'worker_unit', 'dux-readiness-641.service')}",
        f"DUX_SOAK_CONTROLLER_UNIT={getattr(args, 'controller_unit', 'dux-soak-control-e30f4288.service')}",
        f"DUX_SOAK_MEMORY_EVENTS_PATH={getattr(args, 'memory_events_path', '/sys/fs/cgroup/system.slice/dux-readiness-641.service/memory.events')}",
        "/usr/bin/python3", "-c", REMOTE,
    ])
    remote = ["ssh", "-i", str(args.identity), "-o", "BatchMode=yes",
              "-o", "ConnectTimeout=8", "-o", "StrictHostKeyChecking=yes",
              "-o", f"UserKnownHostsFile={args.known_hosts}", "-p", str(args.port),
              "root@127.0.0.1", remote_command]
    try:
        result = run(remote, timeout=15)
        guest = json.loads(result.stdout)
        row["guest"] = guest
        progress = guest.get("progress", {})
        freshness, progress_age = progress_freshness(
            guest.get("guest_time_epoch", 0), guest.get("progress_mtime_epoch", 0),
            args.max_progress_age_seconds)
        row["progress_age_seconds"] = round(progress_age, 3)
        row["progress_freshness"] = freshness
        row["soak"] = {key: progress.get(key) for key in
                        ("status", "started_at", "elapsed_seconds", "operations",
                         "mutation_bursts", "controlled_crashes", "held_readers")}
        samples = progress.get("samples", [])
        row["sample_count"] = len(samples)
        row["latest_exact_sample"] = samples[-1] if samples else None
        row["progress_sha256"] = guest.get("progress_sha256")
        expected_candidate_sha256 = getattr(args, "expected_candidate_sha256", None)
        prior_units_healthy = prior_run_archive_healthy(
            output_dir, progress.get("started_at"), expected_candidate_sha256,
            args.max_progress_age_seconds, row["observed_at_utc"])
        row["prior_run_units_healthy"] = prior_units_healthy
        worker_fields = guest.get("worker", {}).get("fields", {})
        controller_fields = guest.get("controller", {}).get("fields", {})
        if progress.get("status") == "invalid":
            row["status"] = "invalid_guest_progress"
        elif freshness == "stale_guest_progress":
            row["status"] = "stale_guest_progress"
        elif freshness == "guest_clock_skew":
            row["status"] = "guest_clock_skew"
        elif row["host_vm"]["fields"].get("ActiveState") != "active":
            row["status"] = "host_vm_not_active"
        elif (guest.get("worker", {}).get("fields", {}).get("ActiveState") != "active" and
              not (progress.get("status") == "passed" and
                   (cleanly_exited(worker_fields) or
                    (prior_units_healthy and not worker_fields)))):
            row["status"] = "guest_worker_not_active"
        elif (controller_fields.get("ActiveState") != "active" and
              not (progress.get("status") == "passed" and
                   (cleanly_exited(controller_fields) or
                    (prior_units_healthy and not controller_fields)))):
            row["status"] = "guest_controller_not_active"
        row["qualification"] = evaluate_qualification(
            progress,
            getattr(args, "expected_candidate_sha256", None),
            guest.get("worker", {}).get("fields", {}),
            controller_fields,
            row["host_vm"].get("fields", {}),
            guest.get("memory_events"),
            capture_status=row["status"],
            prior_units_healthy=prior_units_healthy,
        )

        now = time.monotonic()
        if args.once or now >= args.next_snapshot:
            snapshot = output_dir / f"progress-{utc_stamp()}.json.gz"
            temporary = snapshot.with_suffix(snapshot.suffix + ".tmp")
            payload = json.dumps(guest, sort_keys=True,
                                 separators=(",", ":")).encode()
            with temporary.open("wb") as raw_file:
                with gzip.GzipFile(filename="", fileobj=raw_file,
                                   mode="wb", compresslevel=6, mtime=0) as stream:
                    stream.write(payload)
                raw_file.flush()
                os.fsync(raw_file.fileno())
            os.replace(temporary, snapshot)
            row["full_snapshot"] = snapshot.name
            row["full_snapshot_sha256"] = hashlib.sha256(snapshot.read_bytes()).hexdigest()
            args.next_snapshot = now + args.full_snapshot_interval
    except Exception as error:
        row["status"] = "capture_error"
        row["error"] = f"{type(error).__name__}: {error}"
    with (output_dir / "checkpoints.jsonl").open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    print(json.dumps({key: row.get(key) for key in
                      ("observed_at_utc", "status", "progress_freshness",
                       "progress_age_seconds", "soak", "sample_count",
                      "latest_exact_sample", "full_snapshot", "qualification", "error")},
                     sort_keys=True), flush=True)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", type=Path, required=True)
    parser.add_argument("--known-hosts", type=Path, required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--interval", type=int, default=60)
    parser.add_argument("--full-snapshot-interval", type=int, default=900)
    parser.add_argument("--max-progress-age-seconds", type=float, default=300,
                        help="fail the archive checkpoint if guest progress is older than this")
    parser.add_argument("--expected-candidate-sha256",
                        help="candidate hash from the run manifest; required for qualification")
    parser.add_argument("--progress-path", default="/root/readiness/soak-progress.json")
    parser.add_argument("--worker-unit", default="dux-readiness-641.service")
    parser.add_argument("--controller-unit", default="dux-soak-control-e30f4288.service")
    parser.add_argument("--memory-events-path", default="/sys/fs/cgroup/system.slice/dux-readiness-641.service/memory.events")
    parser.add_argument("--host-vm-unit", default="dux-qualification-vm-debian12.service")
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if (args.interval < 10 or args.full_snapshot_interval < args.interval or
            args.max_progress_age_seconds < args.interval):
        parser.error("interval must be >=10s, max progress age >= interval, and full snapshot interval >= interval")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    args.next_snapshot = time.monotonic()
    print(f"archive_dir={args.output_dir} interval={args.interval}s full_snapshot={args.full_snapshot_interval}s", flush=True)
    while True:
        started = time.monotonic()
        collect(args)
        if args.once:
            return 0
        time.sleep(max(1, args.interval - (time.monotonic() - started)))


if __name__ == "__main__":
    sys.exit(main())
