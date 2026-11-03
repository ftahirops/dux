#!/usr/bin/env python3
"""Compare exact real-fanotify behavior at two process-wide CPU quotas.

Each run gets the same private loop-backed ext4 fixture, isolated mount
namespace, Dux binary, event-loop governor, memory limit, and churn workload.
The transient worker service applies the requested systemd CPUQuota; the
installed Dux service and its index are never restarted or opened.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dux_qualification_sandbox import require_disposable_qualification_vm

MIB = 1024 * 1024
GIB = 1024 * MIB


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(MIB), b""):
            digest.update(block)
    return digest.hexdigest()


def atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f"{path.name}.tmp-{os.getpid()}")
    with temporary.open("w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def quota_wall_time_summary(runs: list[dict]) -> dict:
    """Compare by quota value, independent of which run happened first."""
    by_quota = {run["cpu_quota_percent"]: run["systemd_run_seconds"] for run in runs}
    summary = {"quota_run_order": [run["cpu_quota_percent"] for run in runs]}
    if 50 in by_quota and 100 in by_quota:
        # >1 means the 100% run completed faster. This is descriptive only;
        # paired host-load variation must be considered separately.
        summary["wall_time_ratio_50_over_100"] = round(by_quota[50] / by_quota[100], 4)
    return summary


def stop_transient_unit(unit_name: str, process: subprocess.Popen) -> None:
    """Stop the worker before its parent removes the shared fixture image."""
    try:
        subprocess.run(["systemctl", "stop", unit_name], capture_output=True, timeout=45)
    except (OSError, subprocess.SubprocessError):
        pass
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def run_one(args, image: Path, mountpoint: Path, scratch: Path, quota: int) -> dict:
    inner_report = scratch / f"fanotify-{quota}.json"
    worker = Path(__file__).with_name("dux-live-quota-worker.py").resolve()
    unit_name = f"dux-live-quota-{os.getpid()}-{quota}.service"
    command = [
        "systemd-run", f"--unit={unit_name.removesuffix('.service')}",
        "--wait", "--pipe", "--collect",
        "--property=PrivateMounts=yes",
        f"--property=CPUQuota={quota}%",
        f"--property=MemoryMax={args.worker_memory_mib}M",
        "--property=MemorySwapMax=0",
        "--property=CPUAccounting=yes", "--property=MemoryAccounting=yes",
        "--property=IOWeight=10", "--property=Nice=10",
        "--property=AmbientCapabilities=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH",
        "--property=RuntimeMaxSec=1800", "--property=TimeoutStopSec=30s",
        sys.executable, str(worker),
        "--binary", str(args.binary), "--image", str(image),
        "--mountpoint", str(mountpoint), "--report", str(inner_report),
        "--cpu-quota-percent", str(quota),
        "--max-cpu-percent", str(args.max_cpu_percent),
        "--operations", str(args.operations),
        "--directory-swaps", str(args.directory_swaps),
    ]
    started = time.monotonic()
    output_lines = []
    process = subprocess.Popen(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, bufsize=1)
    assert process.stdout is not None
    buffered_chars = 0
    try:
        for line in process.stdout:
            output_lines.append(line)
            buffered_chars += len(line)
            print(line, end="", flush=True)
            if buffered_chars > 24000:
                output_lines = output_lines[-100:]
                buffered_chars = sum(map(len, output_lines))
    except BaseException:
        # A cancelled comparison must not leave the candidate daemon running
        # against a fixture that the parent is about to discard.
        stop_transient_unit(unit_name, process)
        raise
    returncode = process.wait()
    elapsed = round(time.monotonic() - started, 3)
    detail = json.loads(inner_report.read_text()) if inner_report.exists() else None
    output = "".join(output_lines)
    def parsed_seconds(label: str):
        match = re.search(rf"^{re.escape(label)}: ([0-9.]+)s$", output, re.MULTILINE)
        return float(match.group(1)) if match else None

    return {
        "cpu_quota_percent": quota,
        "max_cpu_percent": args.max_cpu_percent,
        "systemd_run_seconds": elapsed,
        "returncode": returncode,
        "status": "passed" if returncode == 0 and detail and detail.get("status") == "passed" else "failed",
        "fanotify_report": detail,
        "systemd_service_runtime_seconds": parsed_seconds("Service runtime"),
        "systemd_reported_cpu_seconds": parsed_seconds("CPU time consumed"),
        "output_tail": output[-6000:],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--quotas", type=int, nargs=2, default=[50, 100],
                        metavar=("BASELINE", "CANDIDATE"),
                        help="CPUQuota percentages to compare (default: 50 100)")
    parser.add_argument("--max-cpu-percent", type=int, default=25,
                        help="same Dux event-loop governor target for both runs")
    parser.add_argument("--operations", type=int, default=1200)
    parser.add_argument("--directory-swaps", type=int, default=1500)
    parser.add_argument("--worker-memory-mib", type=int, default=4096)
    parser.add_argument("--image-mib", type=int, default=512)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.report = args.report.resolve()
    if os.geteuid() != 0:
        parser.error("run as root to create the loop-backed ext4 fixture and transient systemd units")
    try:
        require_disposable_qualification_vm()
    except RuntimeError as error:
        parser.error(str(error))
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error(f"binary is not executable: {args.binary}")
    if len(set(args.quotas)) != 2 or any(not 10 <= q <= 800 for q in args.quotas):
        parser.error("--quotas must contain two distinct values from 10 through 800")
    if not 1 <= args.max_cpu_percent <= 100:
        parser.error("--max-cpu-percent must be between 1 and 100")
    if not 100 <= args.operations <= 100000 or not 100 <= args.directory_swaps <= 10000:
        parser.error("operations must be 100..100000 and directory swaps 100..10000")
    if not 768 <= args.worker_memory_mib <= 32768 or not 256 <= args.image_mib <= 2048:
        parser.error("worker memory must be 768..32768 MiB and image size 256..2048 MiB")
    for tool in ("mkfs.ext4", "mount", "umount", "systemd-run"):
        if shutil.which(tool) is None:
            parser.error(f"required tool not found: {tool}")
    workspace = Path("/dev/shm")
    filesystem = subprocess.check_output(["findmnt", "-T", str(workspace), "-no", "FSTYPE"], text=True).strip()
    if filesystem != "tmpfs":
        parser.error(f"{workspace} is on {filesystem}, expected isolated tmpfs workspace")
    required = args.image_mib * MIB + GIB
    free = shutil.disk_usage(workspace).free
    if free < required:
        parser.error(f"{workspace} has {free} free bytes; need image capacity plus 1 GiB reserve ({required})")

    started_at = datetime.now(timezone.utc).isoformat()
    report = {
        "status": "running",
        "started_at_utc": started_at,
        "binary": str(args.binary),
        "binary_sha256": sha256(args.binary),
        "harness_sha256": sha256(Path(__file__)),
        "worker_sha256": sha256(Path(__file__).with_name("dux-live-quota-worker.py")),
        "live_test_harness_sha256": sha256(Path(__file__).with_name("dux-live-test.py")),
        "kernel": subprocess.check_output(["uname", "-r"], text=True).strip(),
        "workspace": str(workspace),
        "workspace_filesystem": filesystem,
        "workspace_free_bytes_at_start": free,
        "image_mib": args.image_mib,
        "memory_limit_mib": args.worker_memory_mib,
        "max_cpu_percent": args.max_cpu_percent,
        "operations": args.operations,
        "directory_swaps": args.directory_swaps,
        "runs": [],
        "interpretation_limit": "This compares isolated real-fanotify churn; it does not establish production-root catch-up, scale-to-10m, or long-duration overflow rates.",
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    atomic_json(args.report, report)
    try:
        with tempfile.TemporaryDirectory(prefix="dux-live-quota-", dir=workspace) as name:
            scratch = Path(name)
            image, mountpoint = scratch / "fixture.ext4", scratch / "mount"
            mountpoint.mkdir()
            with image.open("wb") as stream:
                stream.truncate(args.image_mib * MIB)
            subprocess.run(["mkfs.ext4", "-q", "-F", "-m", "0", "-I", "256", "-N", "100000", str(image)], check=True)
            for quota in args.quotas:
                print(f"Starting exact fanotify suite at CPUQuota={quota}%", flush=True)
                result = run_one(args, image, mountpoint, scratch, quota)
                report["runs"].append(result)
                report["status"] = "running" if result["status"] == "passed" else "failed"
                atomic_json(args.report, report)
                print(f"CPUQuota={quota}% {result['status']} in {result['systemd_run_seconds']}s", flush=True)
                if result["status"] != "passed":
                    break
        if len(report["runs"]) == len(args.quotas) and all(r["status"] == "passed" for r in report["runs"]):
            report["status"] = "passed"
            report.update(quota_wall_time_summary(report["runs"]))
    except BaseException as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        report["finished_at_utc"] = datetime.now(timezone.utc).isoformat()
        atomic_json(args.report, report)
    print(f"Evidence: {args.report}", flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
