#!/usr/bin/env python3
"""Verify dux systemd status tracks scan start, replay, and clean catch-up."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import threading
import tempfile
import time
from dux_qualification_sandbox import require_disposable_qualification_vm


def run(*args, check=True):
    return subprocess.run([str(arg) for arg in args], check=check,
                          capture_output=True, text=True)


def status_text(unit):
    return run("systemctl", "show", unit, "--property=StatusText", "--value").stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("/usr/bin/dux"))
    parser.add_argument("--files", type=int, default=100000)
    parser.add_argument("--event-files", type=int, default=3000)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    assert args.files >= 1000
    try:
        require_disposable_qualification_vm()
    except RuntimeError as error:
        parser.error(str(error))
    args.binary = args.binary.resolve()
    assert subprocess.run(["id", "-u"], capture_output=True, text=True).stdout.strip() == "0"
    unit = f"dux-status-transition-{__import__('os').getpid()}.service"
    workspace = Path(tempfile.mkdtemp(prefix="dux-status-transition-", dir="/var/tmp"))
    tree, db = workspace / "tree", workspace / "index.db"
    tree.mkdir()
    report = {"binary": str(args.binary), "fixture_files": args.files,
              "event_files_during_rescan": args.event_files, "statuses": []}
    try:
        for number in range(args.files):
            (tree / f"fixture-{number:08d}").touch()
        run(args.binary, "--db", db, "scan", tree, "--quiet", "--jobs", "2")
        run("systemd-run", f"--unit={unit}", "--collect", "--property=Type=notify",
            "--property=NotifyAccess=all", "--property=PrivateMounts=yes",
            "--property=CPUQuota=50%", "--property=MemoryMax=768M",
            "--property=TimeoutStopSec=20s", args.binary, "--db", db, "daemon", tree,
            "--flush-ms", "100", "--max-cpu", "50")

        initial_statuses = []
        initial_rebuilding = initial_replay = False
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            value = status_text(unit)
            if value and (not initial_statuses or initial_statuses[-1] != value):
                initial_statuses.append(value)
            initial_rebuilding |= value.startswith("Rebuilding index;")
            initial_replay |= value.startswith("Rescan complete; reconciling queued events")
            if value.endswith("incremental index is caught up"):
                machine = json.loads(run(args.binary, "--db", db, "--json", "status").stdout)
                if machine.get("caught_up") is True and machine.get("scan_in_progress") is False:
                    report["initial_machine_status"] = machine
                    break
            time.sleep(.02)
        assert initial_rebuilding, initial_statuses
        report["initial_statuses"] = initial_statuses

        def mutate_during_rescan():
            time.sleep(.05)
            burst = tree / "events-during-rescan"
            burst.mkdir()
            for number in range(args.event_files):
                (burst / f"event-{number:06d}").write_bytes(b"rescan status transition")

        run("systemctl", "kill", "--kill-whom=main", "--signal=HUP", unit)
        writer = threading.Thread(target=mutate_during_rescan)
        writer.start()
        saw_rebuilding = saw_replay = False
        report["statuses"] = []
        final_status = ""
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            value = status_text(unit)
            if value and (not report["statuses"] or report["statuses"][-1] != value):
                report["statuses"].append(value)
            saw_rebuilding |= value.startswith("Rebuilding index;")
            saw_replay |= value.startswith("Rescan complete; reconciling queued events")
            if value.endswith("incremental index is caught up"):
                final_status = value
                machine = json.loads(run(args.binary, "--db", db, "--json", "status").stdout)
                if machine.get("caught_up") is True and machine.get("scan_in_progress") is False:
                    report["final_machine_status"] = machine
                    break
            time.sleep(.02)
        writer.join(timeout=30)
        assert not writer.is_alive(), "event burst writer did not finish"
        assert saw_rebuilding, report
        assert saw_replay, report
        assert final_status.endswith("incremental index is caught up"), report
        report["recovery_status"] = final_status
        report["status_transitions_passed"] = True
        report["status"] = "passed"
    finally:
        run("systemctl", "stop", unit, check=False)
        shutil.rmtree(workspace, ignore_errors=True)
        report.setdefault("status", "failed")
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
