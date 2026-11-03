#!/usr/bin/env python3
"""Re-measure 10m query/TUI budgets on a preserved KVM-only scale fixture.

This is for a completed scan whose query phase failed or was interrupted. It
never builds or modifies the fixture image or base SQLite database: the ext4
image is mounted read-only with journal replay disabled, and the base database
identity is checked afterward. SQLite may create transient WAL/SHM sidecars.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
from dux_qualification_sandbox import require_disposable_qualification_vm


QUERY_BUDGETS = {
    "top_dirs": 0.5,
    "top_files": 0.5,
    "find_unique": 3.0,
    "du_depth_1": 1.0,
    "metrics": 0.5,
    "growth": 5.0,
}

QUERIES = {
    "top_dirs": ["top", "--dirs", "--limit", "20"],
    "top_files": ["top", "--files", "--limit", "20"],
    "find_unique": ["find", "--name", "artifact-0000004242", "--limit", "20"],
    "du_depth_1": None,  # Filled with the read-only mounted fixture tree.
    "metrics": ["metrics", "--top", "0"],
    "growth": ["growth", "--limit", "20"],
}


def summarize(values):
    if not values:
        raise ValueError("at least one timing sample is required")
    ordered = sorted(values)
    return {
        "sample_count": len(values),
        "samples_seconds": list(values),
        "p50_seconds": ordered[max(0, (len(ordered) + 1) // 2 - 1)],
        "p95_seconds": ordered[max(0, (95 * len(ordered) + 99) // 100 - 1)],
    }


def budget_failures(queries, tui_input_p95, tui_quit_p95):
    failures = []
    for name, limit in QUERY_BUDGETS.items():
        result = queries.get(name)
        if result is None or result["p95_seconds"] >= limit:
            failures.append(f"{name} p95 must be below {limit}s")
    if tui_input_p95 >= 0.1:
        failures.append("TUI input acknowledgement p95 must be below 0.1s")
    if tui_quit_p95 >= 0.25:
        failures.append("TUI quit p95 must be below 0.25s")
    return failures


def p95_failure_is_proven(samples, budget, required_samples):
    """Return true once no possible remaining samples can bring p95 in budget.

    Percentiles use nearest rank: ceil(0.95 * N). Stop expensive repeated
    queries only after enough observed samples already force that rank over the
    strict budget. For N=50, three over-budget samples prove failure.
    """
    rank = math.ceil(0.95 * required_samples)
    allowed_over_budget = required_samples - rank
    return sum(value >= budget for value in samples) > allowed_over_budget


def write_report(path, report):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    os.replace(temporary, path)


def command(*args, timeout=120, env=None):
    return subprocess.run(
        [str(arg) for arg in args], check=True, timeout=timeout, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
    )


def load_scale_module(path):
    spec = importlib.util.spec_from_file_location("dux_scale_recheck_source", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load scale helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--scan-binary", type=Path,
        help="exact binary used for the preserved scan (defaults to --binary)",
    )
    parser.add_argument("--scale-harness", type=Path, required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--source-report", type=Path, required=True)
    parser.add_argument(
        "--reuse-exactness-report", type=Path,
        help="reuse prior exactness only when source report, image, DB size, and truth all match",
    )
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=50)
    args = parser.parse_args()
    if args.samples < 50:
        parser.error("the full 10m query gate requires at least 50 samples")
    args.binary = args.binary.resolve(strict=True)
    args.scan_binary = (args.scan_binary or args.binary).resolve(strict=True)
    args.scale_harness = args.scale_harness.resolve(strict=True)
    args.workspace = args.workspace.resolve(strict=True)
    args.source_report = args.source_report.resolve(strict=True)
    if args.reuse_exactness_report is not None:
        args.reuse_exactness_report = args.reuse_exactness_report.resolve(strict=True)
    allowed = Path("/mnt/dux-readiness/scale-space").resolve()
    if not args.workspace.is_relative_to(allowed):
        raise RuntimeError("preserved fixture must be under /mnt/dux-readiness/scale-space in KVM")
    require_disposable_qualification_vm()
    if os.readlink("/proc/self/ns/mnt") == os.readlink("/proc/1/ns/mnt"):
        raise RuntimeError("query recheck must run in a private mount namespace")

    source_report = json.loads(args.source_report.read_text())
    source_hash = hashlib.sha256(args.source_report.read_bytes()).hexdigest()
    binary_hash = hashlib.sha256(args.binary.read_bytes()).hexdigest()
    scan_binary_hash = hashlib.sha256(args.scan_binary.read_bytes()).hexdigest()
    if source_report.get("binary_sha256") != scan_binary_hash:
        raise RuntimeError("preserved scan report and scan-binary hashes differ")
    level = next((item for item in source_report.get("levels", [])
                  if item.get("paths") == 10_000_000), None)
    if level is None or level.get("fixture_paths") != 10_000_000:
        raise RuntimeError("source report does not contain the completed 10m fixture")
    if level.get("scan", {}).get("wall_seconds", float("inf")) >= 1800:
        raise RuntimeError("source scan did not meet the 10m scan wall budget")
    if level.get("scan", {}).get("peak_rss_kib", float("inf")) >= 256 * 1024:
        raise RuntimeError("source scan did not meet the 256 MiB RSS budget")

    image = args.workspace / "filesystem.img"
    mountpoint = args.workspace / "filesystem"
    tree = mountpoint / "tree"
    db = args.workspace / "index.db"
    if not all(path.is_file() for path in (image, db)) or not mountpoint.is_dir():
        raise RuntimeError("preserved image/index/mountpoint is incomplete")

    def file_identity(path):
        st = path.stat()
        return {
            "device": st.st_dev,
            "inode": st.st_ino,
            "size_bytes": st.st_size,
            "allocated_bytes": st.st_blocks * 512,
            "mtime_ns": st.st_mtime_ns,
            "ctime_ns": st.st_ctime_ns,
        }

    # Hashing a multi-gigabyte sparse image would add substantial read I/O and
    # distort the very query latency being measured. Bind this read-only rerun
    # to the preserved image's stable filesystem identity instead, and verify
    # that identity again after unmount.
    image_identity = file_identity(image)

    report = {
        "status": "running",
        "started_at": datetime.now(timezone.utc).isoformat(),
        "binary_sha256": binary_hash,
        "query_binary_sha256": binary_hash,
        "scan_binary_sha256": scan_binary_hash,
        "source_scan_report_sha256": source_hash,
        "source_scan_report_path": str(args.source_report),
        "source_scan": level["scan"],
        "source_truth": level["truth"],
        "workspace": str(args.workspace),
        "filesystem_image_identity": image_identity,
        "index_identity_before": file_identity(db),
        "index_bytes_before": db.stat().st_size,
        "samples": args.samples,
        "platform": platform.platform(),
        "kernel": platform.release(),
        "read_only_mount": "loop,ro,noload,nosuid,nodev",
        "queries": {},
    }
    write_report(args.report, report)
    mounted = False
    runtime = Path(tempfile.mkdtemp(prefix="dux-scale-query-runtime-"))
    env = dict(os.environ, DUX_RUNTIME_DIR=str(runtime))
    env.pop("NOTIFY_SOCKET", None)
    started = time.monotonic()
    try:
        command("mount", "-o", "loop,ro,noload,nosuid,nodev", image, mountpoint)
        mounted = True
        if not tree.is_dir():
            raise RuntimeError("read-only image does not contain the expected tree")

        scale = load_scale_module(args.scale_harness)
        expected_growth = level.get("growth_fixture", {}).get("rows")
        if args.reuse_exactness_report is not None:
            exactness = json.loads(args.reuse_exactness_report.read_text())
            if exactness.get("source_scan_report_sha256") != source_hash:
                raise RuntimeError("reused exactness evidence binds another scan report")
            if exactness.get("workspace") != str(args.workspace):
                raise RuntimeError("reused exactness evidence binds another workspace")
            if exactness.get("filesystem_image_identity") != image_identity:
                raise RuntimeError("preserved fixture image identity changed since exactness check")
            if exactness.get("index_bytes_before") != db.stat().st_size:
                raise RuntimeError("preserved base index size changed since exactness check")
            if not exactness.get("filesystem_image_unchanged"):
                raise RuntimeError("prior exactness evidence did not prove image stability")
            truth = exactness.get("truth_recheck")
            growth = exactness.get("growth_recheck")
            if not truth or truth != level.get("truth"):
                raise RuntimeError("reused exactness truth differs from the scan report")
            if not growth or growth.get("rows") != expected_growth:
                raise RuntimeError("reused exactness growth coverage is missing or changed")
            report["truth_recheck"] = truth
            report["growth_recheck"] = growth
            report["exactness_reused_from_sha256"] = hashlib.sha256(
                args.reuse_exactness_report.read_bytes()
            ).hexdigest()
            report["exactness_reuse_basis"] = (
                "same scan-report hash, workspace, read-only image identity, base-index size, "
                "exact path/allocation truth, and growth-index evidence"
            )
        else:
            directories = set()
            for parent, child_dirs, _files in os.walk(tree):
                directories.update(Path(parent) / name for name in child_dirs)
            report["truth_recheck"] = scale.verify_counts(
                db, tree, 10_000_000, directories, 100_000
            )
            import sqlite3
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
                conn.execute("BEGIN")
                growth_rows, growth_buckets = conn.execute(
                    "SELECT COUNT(*),COUNT(DISTINCT bucket) FROM growth"
                ).fetchone()
                growth_plan = [row[3] for row in conn.execute(
                    "EXPLAIN QUERY PLAN SELECT dev_id,inode,delta FROM growth_current "
                    "WHERE bucket=? ORDER BY delta DESC LIMIT 20", (0,)
                )]
            if expected_growth is None or growth_rows != expected_growth:
                raise RuntimeError(
                    f"growth row count changed: expected {expected_growth}, got {growth_rows}"
                )
            report["growth_recheck"] = {
                "rows": growth_rows,
                "buckets": growth_buckets,
                "query_plan": growth_plan,
            }
        report["filesystem_image_read_only"] = True
        write_report(args.report, report)

        query_set = dict(QUERIES)
        query_set["du_depth_1"] = ["du", str(tree), "--max-depth", "1"]
        results = {}
        for name, query in query_set.items():
            warmup_started = time.monotonic()
            command(args.binary, "--db", db, *query, timeout=120, env=env)
            warmup_seconds = time.monotonic() - warmup_started
            samples = []
            starts = []
            results[name] = {
                "sample_count": 0,
                "samples_seconds": [],
                "sample_started_at": [],
                "warmup_seconds": warmup_seconds,
            }
            for _ in range(args.samples):
                starts.append(datetime.now(timezone.utc).isoformat())
                before = time.monotonic()
                command(args.binary, "--db", db, *query, timeout=120, env=env)
                samples.append(time.monotonic() - before)
                results[name] = summarize(samples)
                results[name]["sample_started_at"] = list(starts)
                results[name]["warmup_seconds"] = warmup_seconds
                report["queries"] = results
                report["phase"] = f"query samples: {name} {len(samples)}/{args.samples}"
                write_report(args.report, report)
                budget = QUERY_BUDGETS[name]
                if p95_failure_is_proven(samples, budget, args.samples):
                    report["status"] = "failed"
                    report["failure"] = (
                        f"{name} p95 cannot meet the strict {budget}s budget; "
                        "three or more observed samples already exceed it"
                    )
                    raise RuntimeError(report["failure"])
            report["queries"] = results
            report["phase"] = f"query samples: {name} complete"
            write_report(args.report, report)

        tui = []
        for index in range(args.samples):
            tui.append(scale.tui_sample(args.binary, db, env))
            report["tui_samples"] = tui
            report["phase"] = f"TUI samples {index + 1}/{args.samples}"
            write_report(args.report, report)
            for metric, budget in (("input_ack_seconds", 0.1), ("quit_seconds", 0.25)):
                if p95_failure_is_proven([item[metric] for item in tui], budget, args.samples):
                    report["status"] = "failed"
                    report["failure"] = (
                        f"TUI {metric} p95 cannot meet the strict {budget}s budget"
                    )
                    raise RuntimeError(report["failure"])
        tui_input = summarize([item["input_ack_seconds"] for item in tui])
        tui_quit = summarize([item["quit_seconds"] for item in tui])
        failures = budget_failures(results, tui_input["p95_seconds"], tui_quit["p95_seconds"])
        report.update({
            "tui_samples": tui,
            "tui_input_ack": tui_input,
            "tui_quit": tui_quit,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "index_identity_after": file_identity(db),
            "index_bytes_after": db.stat().st_size,
            "wal_bytes_after": Path(str(db) + "-wal").stat().st_size
                if Path(str(db) + "-wal").exists() else 0,
            "budget_failures": failures,
            "status": "failed" if failures else "passed",
        })
        if failures:
            raise RuntimeError("10m query/TUI budgets failed: " + "; ".join(failures))
    except BaseException as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        raise
    finally:
        if mounted:
            subprocess.run(["umount", str(mountpoint)], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        report["filesystem_image_unchanged"] = file_identity(image) == image_identity
        if not report["filesystem_image_unchanged"]:
            report["status"] = "failed"
            report["error"] = "preserved filesystem image identity changed during read-only recheck"
        report["index_identity_unchanged"] = (
            file_identity(db) == report["index_identity_before"]
        )
        if not report["index_identity_unchanged"]:
            report["status"] = "failed"
            report["error"] = "base SQLite index identity changed during query recheck"
        report["finished_at"] = datetime.now(timezone.utc).isoformat()
        write_report(args.report, report)
        try:
            runtime.rmdir()
        except OSError:
            pass
    print(json.dumps(report, indent=2), flush=True)
    if report.get("status") != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
