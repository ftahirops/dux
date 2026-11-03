#!/usr/bin/env python3
"""Prove bind aliases are traversed and v6 coverage remains explicitly incomplete.

Creates a private loop-backed ext4 tree and a bind alias inside a private mount
namespace. The candidate scan/database/runtime are isolated from the installed
Dux daemon. Requires root, CAP_SYS_ADMIN, mkfs.ext4, mount, umount and unshare.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from dux_qualification_sandbox import require_disposable_qualification_vm


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_report_no_clobber(path: Path, result: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as report:
            fd = -1
            json.dump(result, report, indent=2)
            report.write("\n")
            report.flush()
            os.fsync(report.fileno())
        os.link(temporary, path, follow_symlinks=False)
        temporary.unlink()
        directory_fd = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if fd >= 0:
            os.close(fd)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def run(binary: Path, report_path: Path) -> dict:
    for tool in ("mkfs.ext4", "mount", "umount", "unshare", "systemd-detect-virt"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"required tool not found: {tool}")
    require_disposable_qualification_vm()

    runtime = Path(f"/dev/shm/dux-bind-alias-runtime-{os.getpid()}")
    runtime.mkdir(mode=0o700)
    inner = r'''import json, os, pathlib, re, sqlite3, subprocess, sys
image, mountpoint, runtime, binary = sys.argv[1:]
subprocess.run(["mount", "-o", "loop", image, mountpoint], check=True)
alias_mounted = False
try:
    tree = pathlib.Path(mountpoint) / "tree"
    original = tree / "original"
    alias = tree / "alias"
    (original / "nested").mkdir(parents=True)
    alias.mkdir()
    (original / "nested" / "probe.bin").write_bytes(b"same object through two directory paths\n")
    subprocess.run(["mount", "--bind", str(original), str(alias)], check=True)
    alias_mounted = True
    db = pathlib.Path(runtime) / "index.db"
    scan = subprocess.run(
        [binary, "scan", str(tree), "--db", str(db), "--jobs", "1", "--quiet"],
        capture_output=True, text=True, timeout=120,
    )
    if scan.returncode != 0:
        raise RuntimeError(f"candidate scan failed ({scan.returncode}): {scan.stderr[-2000:]}")
    status_run = subprocess.run(
        [binary, "status", "--db", str(db), "--json"],
        capture_output=True, text=True, check=True, timeout=10,
    )
    status = json.loads(status_run.stdout)
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
        indexed_paths = conn.execute("SELECT COUNT(*) FROM dirents WHERE name=?", (b"probe.bin",)).fetchone()[0]
    expected_paths = 2
    if not status.get("dirty"):
        raise AssertionError(f"alias scan reported clean status: {status}")
    if indexed_paths != 1:
        raise AssertionError(f"expected one v6 canonical dirent, found {indexed_paths}")
    reason = status.get("dirty_reason") or ""
    if "repeated directory identities" not in reason:
        raise AssertionError(f"missing alias-specific dirty reason: {reason!r}")
    scan_counts = re.search(r"scanned\s+(\d+) files?,\s+(\d+) dirs?", scan.stderr)
    if not scan_counts:
        raise AssertionError(f"scan result did not report files and directories: {scan.stderr[-1200:]}")
    scan_files, scan_dirs = map(int, scan_counts.groups())
    if scan_files != 2:
        raise AssertionError(
            f"expected the nested file to be traversed through both visible paths, scanned {scan_files} files"
        )
    path_counts = re.search(
        r"(\d+) staged path instances \((\d+) directory alias paths\)", scan.stderr
    )
    if not path_counts:
        raise AssertionError(f"scan result did not report staged path graph counts: {scan.stderr[-1200:]}")
    staged_paths, staged_directory_alias_paths = map(int, path_counts.groups())
    if staged_directory_alias_paths != 2:
        raise AssertionError(
            "expected both the aliased directory and its nested directory as duplicate path instances, "
            f"found {staged_directory_alias_paths}"
        )
    totals = re.search(
        r"visible (\d+) bytes/(\d+) entries, unique (\d+) bytes/(\d+) inodes",
        scan.stderr,
    )
    if not totals:
        raise AssertionError(f"scan result did not report staged root totals: {scan.stderr[-1200:]}")
    visible_bytes, visible_entries, unique_bytes, unique_inodes = map(int, totals.groups())
    if visible_entries != staged_paths or visible_bytes <= unique_bytes:
        raise AssertionError(
            "staged visible/unique root totals do not reflect path aliases: "
            f"paths={staged_paths}, visible=({visible_bytes},{visible_entries}), "
            f"unique=({unique_bytes},{unique_inodes})"
        )
    result = {
        "status": "passed",
        "binary": binary,
        "binary_sha256": None,
        "filesystem": "ext4",
        "fixture": "directory bind alias with one nested probe file",
        "expected_visible_probe_paths": expected_paths,
        "scan_files_including_alias_traversal": scan_files,
        "scan_directories_including_alias": scan_dirs,
        "staged_path_instances": staged_paths,
        "staged_directory_alias_paths": staged_directory_alias_paths,
        "staged_root_visible_bytes": visible_bytes,
        "staged_root_visible_entries": visible_entries,
        "staged_root_unique_bytes": unique_bytes,
        "staged_root_unique_inodes": unique_inodes,
        "alias_subtree_traversal": "passed",
        "schema_v6_indexed_probe_dirents": indexed_paths,
        "scan_returncode": scan.returncode,
        "scan_coverage_issues": int(match.group(1)) if (match := re.search(r"(\d+) coverage issues?", scan.stderr)) else None,
        "dirty": status["dirty"],
        "dirty_reason": reason,
        "stderr_tail": scan.stderr[-1200:],
        "coverage": "correctly reports incomplete; the second visible alias path is unsupported",
    }
finally:
    if alias_mounted:
        subprocess.run(["umount", str(pathlib.Path(mountpoint) / "tree" / "alias")], check=True)
    subprocess.run(["umount", mountpoint], check=True)
print(json.dumps(result, sort_keys=True))
'''

    try:
        with tempfile.TemporaryDirectory(prefix="dux-bind-alias-ext4-", dir="/dev/shm") as directory:
            root = Path(directory)
            image = root / "fixture.img"
            mountpoint = root / "root"
            mountpoint.mkdir(mode=0o700)
            with image.open("wb") as stream:
                stream.truncate(256 * 1024 * 1024)
            subprocess.run(["mkfs.ext4", "-q", "-F", "-m", "0", str(image)], check=True)
            completed = subprocess.run(
                [
                    "unshare", "--mount", "--propagation", "private", "--",
                    shutil.which("python3") or "python3", "-c", inner,
                    str(image), str(mountpoint), str(runtime), str(binary.resolve()),
                ],
                capture_output=True,
                text=True,
                check=True,
                timeout=150,
            )
            result = json.loads(completed.stdout.strip().splitlines()[-1])
            result["binary_sha256"] = sha256(binary)
            write_report_no_clobber(report_path, result)
            return result
    finally:
        shutil.rmtree(runtime, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    result = run(args.binary, args.report)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
