#!/usr/bin/env python3
"""Run a scale command in a qualification VM and stream durable host checkpoints.

The guest's progress must not be trapped in a qcow2 image if a test is stopped
for host protection. This wrapper polls the atomic guest report over loopback
SSH, saves every complete snapshot on the host, and preserves the command log.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone


DELIMITER = "\n__DUX_SCALE_PROGRESS_DELIMITER__\n"


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def split_snapshot(payload: str):
    report, separator, progress = payload.partition(DELIMITER)
    if not separator:
        raise ValueError("guest response did not contain both report files")
    # Parse before saving so a transient partial read never replaces a good
    # host-side checkpoint. Guest files themselves are written by atomic rename.
    json.loads(report)
    json.loads(progress)
    return report.rstrip() + "\n", progress.rstrip() + "\n"


def terminal_status(returncode, interrupted=False):
    """Report the remote command's actual terminal state in monitor metadata."""
    if interrupted:
        return "interrupted_by_operator"
    if returncode is None:
        return "incomplete"
    return "complete" if returncode == 0 else "failed"


def make_ssh(state, command):
    ssh = [
        "ssh", "-i", state["identity"], "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new",
        "-o", f"UserKnownHostsFile={state['known_hosts']}", "-p", str(state["port"]),
        "root@127.0.0.1",
    ]
    return ssh + [command]


def fetch_snapshot(state, guest_report):
    guest_progress = guest_report + ".progress.json"
    command = (f"cat -- {shlex.quote(guest_report)}; "
               f"printf %s {shlex.quote(DELIMITER)}; "
               f"cat -- {shlex.quote(guest_progress)}")
    result = subprocess.run(make_ssh(state, command), text=True, capture_output=True,
                            timeout=12, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or f"ssh exited {result.returncode}")
    return split_snapshot(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--guest", default="ubuntu24")
    parser.add_argument("--guest-report", required=True,
                        help="absolute report path written by dux-scale.py in the guest")
    parser.add_argument("--host-report", type=Path, required=True,
                        help="host-side copy, refreshed after every poll")
    parser.add_argument("--interval", type=float, default=15.0)
    parser.add_argument("--log", type=Path,
                        help="host-side command log; defaults beside the host report")
    parser.add_argument("command", nargs=argparse.REMAINDER,
                        help="guest command after --")
    args = parser.parse_args()
    if args.interval < 1:
        parser.error("--interval must be at least one second")
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("provide the guest command after --")

    root = Path(__file__).resolve().parents[1]
    vm_dir = root / "target/readiness/vms" / args.guest
    state_path = vm_dir / "state.json"
    state = json.loads(state_path.read_text())
    local_report = args.host_report.resolve()
    local_progress = Path(str(local_report) + ".progress.json")
    log_path = args.log.resolve() if args.log else local_report.with_suffix(".log")
    meta_path = Path(str(local_report) + ".monitor.json")
    remote = "exec " + " ".join(shlex.quote(str(part)) for part in command)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    metadata = {"status": "running", "guest": args.guest,
                "guest_report": args.guest_report, "started_at": datetime.now(timezone.utc).isoformat(),
                "interval_seconds": args.interval, "polls": 0, "poll_errors": []}
    atomic_write(meta_path, (json.dumps(metadata, indent=2) + "\n").encode())

    with log_path.open("ab", buffering=0) as log:
        process = subprocess.Popen(make_ssh(state, remote), stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while True:
                try:
                    report, progress = fetch_snapshot(state, args.guest_report)
                    atomic_write(local_report, report.encode())
                    atomic_write(local_progress, progress.encode())
                    metadata["polls"] += 1
                    metadata["last_snapshot_at"] = datetime.now(timezone.utc).isoformat()
                    metadata["last_progress"] = json.loads(progress)
                except (OSError, RuntimeError, ValueError, json.JSONDecodeError,
                        subprocess.SubprocessError) as error:
                    metadata["poll_errors"].append({
                        "at": datetime.now(timezone.utc).isoformat(),
                        "error": str(error),
                    })
                    metadata["poll_errors"] = metadata["poll_errors"][-100:]
                metadata["command_returncode"] = process.poll()
                atomic_write(meta_path, (json.dumps(metadata, indent=2) + "\n").encode())
                if process.poll() is not None:
                    # Final fetch after the command exits captures its terminal
                    # report even if the previous poll raced the final rename.
                    try:
                        report, progress = fetch_snapshot(state, args.guest_report)
                        atomic_write(local_report, report.encode())
                        atomic_write(local_progress, progress.encode())
                        metadata["polls"] += 1
                        metadata["last_snapshot_at"] = datetime.now(timezone.utc).isoformat()
                        metadata["last_progress"] = json.loads(progress)
                    except (OSError, RuntimeError, ValueError, json.JSONDecodeError,
                            subprocess.SubprocessError) as error:
                        metadata["final_fetch_error"] = str(error)
                    break
                time.sleep(args.interval)
        except KeyboardInterrupt:
            metadata["status"] = "interrupted_by_operator"
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        finally:
            metadata["command_returncode"] = process.poll()
            metadata["status"] = terminal_status(
                metadata["command_returncode"],
                interrupted=metadata.get("status") == "interrupted_by_operator",
            )
            metadata["finished_at"] = datetime.now(timezone.utc).isoformat()
            atomic_write(meta_path, (json.dumps(metadata, indent=2) + "\n").encode())
    return process.returncode or 0


if __name__ == "__main__":
    raise SystemExit(main())
