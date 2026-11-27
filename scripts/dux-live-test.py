#!/usr/bin/env python3
"""Exercise real fanotify updates, reconciliation, and TUI exit responsiveness.

Requires Linux, root, systemd, and the built binary. Uses only a temporary tree
and a uniquely named transient service; never restarts the installed dux service.
Run: sudo python3 scripts/dux-live-test.py target/release/dux
"""
import argparse
import fcntl
import hashlib
import json
import os
import re
from pathlib import Path
import pty
import platform
import select
import signal
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from dux_live_test_schema import require_live_test_schema
from dux_qualification_sandbox import require_disposable_qualification_vm

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", nargs="?", type=Path, default=Path("target/release/dux"))
parser.add_argument("--cpu-quota-percent", type=int, default=int(os.environ.get("DUX_LIVE_CPU_QUOTA_PERCENT", "50")),
                    help="process-wide systemd CPUQuota in percent of one core (10..800)")
parser.add_argument("--max-cpu-percent", type=int, default=int(os.environ.get("DUX_LIVE_MAX_CPU_PERCENT", "50")),
                    help="Dux event-loop CPU target in percent of one core (1..100)")
args = parser.parse_args()
if not 10 <= args.cpu_quota_percent <= 800:
    parser.error("--cpu-quota-percent must be between 10 and 800")
if not 1 <= args.max_cpu_percent <= 100:
    parser.error("--max-cpu-percent must be between 1 and 100")
binary = args.binary.resolve()
cpu_quota_percent = args.cpu_quota_percent
max_cpu_percent = args.max_cpu_percent
unit = f"dux-validation-{os.getpid()}"
direct_daemon = os.environ.get("DUX_LIVE_DIRECT_DAEMON") == "1"
started_at = time.time()
report_path = os.environ.get("DUX_LIVE_REPORT")
operation_count = int(os.environ.get("DUX_LIVE_OPERATIONS", "1200"))
large_deleted_subtree = None
ephemeral_sqlite_wal_cycles = 0
transient_temp_cleanup = None
incident_temp_subtrees = None
recursive_leaf_transition = None
go_build_churn = None
fixture_filesystem = None
PSEUDO_FILESYSTEMS = {
    "proc", "sysfs", "cgroup", "cgroup2", "devpts", "debugfs", "tracefs",
    "securityfs", "bpf", "ramfs", "tmpfs", "autofs", "rpc_pipefs", "selinuxfs",
}
STALE_CHILD_BLOCKS = [6_316_032, 17_956_864] + [4_096] * 60
assert sum(STALE_CHILD_BLOCKS) == 24_518_656
if not 100 <= operation_count <= 100000:
    raise SystemExit("DUX_LIVE_OPERATIONS must be between 100 and 100000")
try:
    require_disposable_qualification_vm()
except RuntimeError as error:
    parser.error(str(error))


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def filesystem_type(path):
    """Return the filesystem containing path using this process's mount view."""
    target = os.fsencode(os.path.realpath(path))
    best = None
    with open("/proc/self/mountinfo", "rb") as mountinfo:
        for line in mountinfo:
            fields, separator, details = line.partition(b" - ")
            if not separator:
                continue
            mount_fields = fields.split()
            if len(mount_fields) < 5:
                continue
            mountpoint = re.sub(
                rb"\\([0-7]{3})",
                lambda match: bytes((int(match.group(1), 8),)),
                mount_fields[4],
            )
            prefix = mountpoint.rstrip(b"/") or b"/"
            if target == prefix or target.startswith(prefix + (b"" if prefix == b"/" else b"/")):
                if best is None or len(prefix) > best[0]:
                    fs_type = details.split(None, 1)[0].decode("ascii", errors="replace")
                    best = (len(prefix), fs_type)
    if best is None:
        raise RuntimeError(f"could not identify fixture filesystem for {os.fsdecode(target)}")
    return best[1]


def runtime_record_path(db, kind):
    runtime_dir = os.environ.get("DUX_RUNTIME_DIR")
    if runtime_dir:
        # Match src/util.rs: FNV-1a over the canonical DB path keeps concurrent
        # candidate runtime records distinct inside the selected scratch dir.
        value = 0xCBF29CE484222325
        for byte in os.fsencode(os.path.realpath(db)):
            value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
        return Path(runtime_dir) / f"{value:016x}.{kind}"
    canonical = Path(db).resolve()
    if canonical == Path("/var/lib/dux/dux.db"):
        return Path("/run/dux") / kind
    return Path(f"{db}.{kind}")


def write_report(status, error=None, tui_quit_ms=None):
    if not report_path:
        return
    report = {
        "status": status,
        "error": error,
        "unit": unit,
        "started_at_epoch": started_at,
        "elapsed_seconds": round(time.time() - started_at, 3),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "cpu_quota_percent": cpu_quota_percent,
        "max_cpu_percent": max_cpu_percent,
        "kernel": platform.release(),
        "platform": platform.platform(),
        "fixture_filesystem": fixture_filesystem,
        "test": "real fanotify exact-path, inode-allocation, directory-rollup and TUI regression",
        "churn_operations": operation_count,
        "reused_directory_incarnation": {
            "stale_child_count": len(STALE_CHILD_BLOCKS),
            "stale_child_allocated_bytes": sum(STALE_CHILD_BLOCKS),
            "stale_directory_recorded_recursive_bytes": 5_996_544,
            "stale_directory_recorded_recursive_inodes": 63,
            "injected_rollup_deficit_bytes": 18_526_208,
        },
        "large_deleted_subtree": large_deleted_subtree,
        "ephemeral_sqlite_wal_cycles": ephemeral_sqlite_wal_cycles,
        "transient_temp_cleanup": transient_temp_cleanup,
        "incident_temp_subtrees": incident_temp_subtrees,
        "recursive_leaf_transition": recursive_leaf_transition,
        "go_build_churn": go_build_churn,
        "checks": [
            "exact parent/name to device/inode mapping",
            "exact inode identity and allocated-byte map",
            "root and all-directory recursive totals",
            "allocated-size growth history follows inode incarnation across rename",
            "real fanotify repairs a stale directory incarnation at a reused inode",
            "real fanotify removes a 799-inode temporary build subtree with exact rollups",
            "real fanotify converges after repeated 480-directory Go-build create/delete churn",
            "real fanotify observes transient SQLite WAL and SHM lifecycle",
            "real fanotify removes transient Go-test, args-file and systemd-private paths",
            "real fanotify removes the exact two 19-inode production temp subtrees",
            "hardlink alias survives source atomic replacement",
            "write and unlink of an ephemeral hardlink refreshes surviving inode allocation",
            "empty-file creation followed by allocation refreshes leaf recursive counters and parent rollups",
            "concurrent nested parent-directory swaps and descendant writes",
            "events during background reconciliation",
            "index has no dirty/flush-error/uncertain state",
            *([] if direct_daemon else ["systemd watchdog and restart count"]),
            "TUI input handling and prompt quit",
        ],
        "tui_quit_ms": tui_quit_ms,
    }
    destination = Path(report_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(destination)


def inspect(db):
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=1) as conn:
        conn.execute("BEGIN")
        meta = dict(conn.execute("SELECT key,value FROM meta"))
        root = (int(meta["root_dev"]), int(meta["root_inode"]))
        stored = conn.execute("SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root).fetchone()
        raw = conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone()
        return meta, stored, raw


def caught_up(db, generation=None, tree=None):
    deadline = time.monotonic() + float(os.environ.get("DUX_LIVE_CATCHUP_TIMEOUT", "120"))
    last = "not ready"
    last_truth_error = None
    while time.monotonic() < deadline:
        try:
            meta, stored, raw = inspect(db)
            record = runtime_record_path(db, "watch.status").read_text().splitlines()[0].split()
            clean = "dirty_since" not in meta and "paused_since" not in meta
            new = generation is None or meta.get("scan_generation") != generation
            last = (meta, stored, raw, record)
            # Version 4 adds the kernel overflow counter after the fields this
            # check consumes; uncertainty and userspace-drop positions are
            # unchanged from versions 1-3.
            trustworthy = record[0] in {"1", "2", "3", "4", "5"} and record[11] != "1" and int(record[8]) == 0
            if clean and new and int(record[3]) == 0 and record[4] == "1" and stored == raw:
                try:
                    if tree is not None:
                        assert_truth(db, tree)
                    if trustworthy:
                        return meta
                    last_truth_error = f"pipeline reports loss/uncertainty: dropped={record[8]} uncertain={record[11]}"
                except AssertionError as error:
                    last_truth_error = str(error)
        except (OSError, sqlite3.Error, KeyError, ValueError):
            pass
        time.sleep(0.1)
    raise AssertionError(f"pipeline did not converge: {last}; last truth error: {last_truth_error}")


def truth(tree):
    entries, inodes, _ = filesystem(tree)
    return sum(inodes.values()), len(inodes)


def filesystem(tree):
    entries, inodes, directories = {}, {}, {}
    root = tree.lstat()
    inodes[(root.st_dev, root.st_ino)] = root.st_blocks * 512
    for directory, dirs, files in os.walk(tree):
        parent = os.stat(directory)
        directories[(parent.st_dev, parent.st_ino)] = str(directory)
        for name in dirs + files:
            path = Path(directory) / name
            st = path.lstat()
            identity = st.st_dev, st.st_ino
            inodes.setdefault(identity, st.st_blocks * 512)
            entries[(parent.st_dev, parent.st_ino, os.fsencode(name))] = identity
    return entries, inodes, directories


def assert_truth(db, tree):
    expected_paths, expected_inodes, directories = filesystem(tree)
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
        conn.execute("BEGIN")
        meta = dict(conn.execute("SELECT key,value FROM meta"))
        root = (int(meta["root_dev"]), int(meta["root_inode"]))
        indexed = conn.execute("SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root).fetchone()
        raw = conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone()
        indexed_paths = {(pd, pi, name): (dev, ino) for pd, pi, name, dev, ino in conn.execute(
            "SELECT parent_dev,parent_inode,name,dev_id,inode FROM dirents WHERE NOT(parent_dev=dev_id AND parent_inode=inode)")}
        observed_inodes = {(dev, ino): blocks for dev, ino, blocks in conn.execute("SELECT dev_id,inode,blocks FROM inodes")}
        bad_rollups = conn.execute("""SELECT COUNT(*) FROM inodes i WHERE kind='d' AND (
            recursive_bytes != blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
            OR recursive_inodes != 1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0))""").fetchone()[0]
    extra, missing = indexed_paths.keys() - expected_paths.keys(), expected_paths.keys() - indexed_paths.keys()
    wrong = [(key, indexed_paths[key], expected_paths[key]) for key in indexed_paths.keys() & expected_paths.keys()
             if indexed_paths[key] != expected_paths[key]]
    extra_i, missing_i = observed_inodes.keys() - expected_inodes.keys(), expected_inodes.keys() - observed_inodes.keys()
    wrong_i = [(key, observed_inodes[key], expected_inodes[key]) for key in observed_inodes.keys() & expected_inodes.keys()
               if observed_inodes[key] != expected_inodes[key]]
    missing_named = [(directories.get((dev, ino), "<unknown-parent>"), name.decode(errors="replace"))
                     for dev, ino, name in list(missing)[:8]]
    extra_named = [(directories.get((dev, ino), "<removed-parent>"), name.decode(errors="replace"))
                   for dev, ino, name in list(extra)[:8]]
    assert indexed_paths == expected_paths, ("path mismatch", extra_named, missing_named, wrong[:8],
                                              "inode mismatch", list(extra_i)[:8], list(missing_i)[:8], wrong_i[:8])
    assert observed_inodes == expected_inodes, ("inode/allocation mismatch", list(extra_i)[:8], list(missing_i)[:8], wrong_i[:8])
    assert indexed == raw == (sum(expected_inodes.values()), len(expected_inodes)), (indexed, raw, truth(tree))
    assert bad_rollups == 0, ("bad directory rollups", bad_rollups)
    assert "flush_error" not in meta, meta


with tempfile.TemporaryDirectory(prefix="dux-live-test-") as workspace:
    base = Path(workspace)
    tree = base / "tree"
    tree.mkdir()
    fixture_filesystem = filesystem_type(tree)
    runtime_dir = None
    if direct_daemon:
        runtime_dir = Path(os.environ.get("DUX_RUNTIME_DIR", f"/dev/shm/dux-live-runtime-{os.getpid()}"))
        runtime_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.environ["DUX_RUNTIME_DIR"] = str(runtime_dir)
        # Keep SQLite writes and runtime heartbeat/status updates off the marked
        # fixture filesystem; otherwise their own I/O can flood the event stream.
        db = runtime_dir / "index.db"
    else:
        db = base / "index.db"
    started = False
    daemon_process = None
    daemon_log = None
    tui_quit_ms = None
    try:
        if fixture_filesystem in PSEUDO_FILESYSTEMS:
            raise RuntimeError(
                f"fanotify qualification fixture is on pseudo filesystem {fixture_filesystem!r}; "
                "the default scanner intentionally prunes nested pseudo-filesystem directories. "
                "Use an isolated ext4, XFS, or Btrfs fixture for this exact-tree suite."
            )
        # This harness asserts event-by-event convergence in its temporary
        # fixture. The product default is reconciliation-first, so select the
        # compatibility event-apply path explicitly rather than silently
        # running a different mode and timing out its catch-up gate.
        daemon_args = [str(binary), "--db", str(db), "daemon", str(tree), "--flush-ms", "100",
                       "--max-cpu", str(max_cpu_percent), "--event-apply"]
        if direct_daemon:
            # Share the caller's mount namespace so its private fixture mount contains
            # both the fixture and the filesystem-wide fanotify mark. This keeps
            # unrelated host churn outside the test's event stream.
            daemon_log = (runtime_dir / "daemon.log").open("w")
            daemon_process = subprocess.Popen(
                daemon_args,
                stdout=daemon_log,
                stderr=subprocess.STDOUT,
                env=os.environ.copy(),
            )
        else:
            unit_args = ["systemd-run", f"--unit={unit}", "--collect", "--property=Type=notify",
                         "--property=WatchdogSec=10s", "--property=TimeoutStopSec=15s",
                         f"--property=CPUQuota={cpu_quota_percent}%",
                         "--property=AmbientCapabilities=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH"]
            if os.environ.get("DUX_TRACE_LINK"):
                unit_args.append("--setenv=DUX_TRACE_LINK=1")
            unit_args.extend(daemon_args)
            run(*unit_args)
        started = True
        caught_up(db, tree=tree)
        schema_version = require_live_test_schema(db)
        print(f"PASS: candidate index schema supports the fanotify harness (v{schema_version})", flush=True)
        reuse_probe = tree / "reused-directory-inode-probe"
        reuse_probe.write_bytes(b"current file at reused inode identity")
        caught_up(db, tree=tree)
        reuse_stat = reuse_probe.stat()
        reuse_identity = (reuse_stat.st_dev, reuse_stat.st_ino)
        if os.environ.get("DUX_TRACE_LINK"):
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
                meta = dict(conn.execute("SELECT key,value FROM meta"))
                root_id = int(meta["root_dev"]), int(meta["root_inode"])
                print("reuse baseline", conn.execute(
                    "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root_id
                ).fetchone(), conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone(), flush=True)
        stale_children = [
            ((reuse_stat.st_dev, (1 << 63) - 100 - index), blocks)
            for index, blocks in enumerate(STALE_CHILD_BLOCKS)
        ]
        stale_child_bytes = sum(blocks for _, blocks in stale_children)
        with sqlite3.connect(db, timeout=10) as conn:
            conn.execute("BEGIN IMMEDIATE")
            root_meta = dict(conn.execute("SELECT key,value FROM meta"))
            root_identity = (int(root_meta["root_dev"]), int(root_meta["root_inode"]))
            current_blocks = reuse_stat.st_blocks * 512
            conn.execute(
                "UPDATE inodes SET kind='d',recursive_bytes=5996544,recursive_inodes=63 "
                "WHERE dev_id=? AND inode=?", reuse_identity)
            for index, (stale_child_inode, child_blocks) in enumerate(stale_children):
                conn.execute(
                    "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns) "
                    "VALUES(?,?,'f',?,?,1,0,0,0)",
                    (*stale_child_inode, child_blocks, child_blocks))
                conn.execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?,?,?, ?,?,1)",
                    (*reuse_identity, f"stale-child-{index}".encode(), *stale_child_inode))
            conn.execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes+?,recursive_inodes=recursive_inodes+? "
                "WHERE dev_id=? AND inode=?",
                (stale_child_bytes, len(stale_children), *root_identity))
            conn.commit()
        if os.environ.get("DUX_TRACE_LINK"):
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
                print("reuse injected", conn.execute(
                    "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root_identity
                ).fetchone(), conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone(), flush=True)
        # The on-disk path is still a regular file. A real MODIFY event must
        # recognize the stale directory incarnation and remove its fake child.
        reuse_probe.write_bytes(b"replacement file event from current incarnation")
        repair_deadline = time.monotonic() + 45
        last_repair_error = None
        last_reported_repair_state = None
        while time.monotonic() < repair_deadline:
            try:
                assert_truth(db, tree)
                last_repair_error = None
                break
            except (AssertionError, sqlite3.Error) as error:
                last_repair_error = error
                if os.environ.get("DUX_TRACE_LINK"):
                    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
                        repair_state = (conn.execute(
                            "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root_identity
                        ).fetchone(), conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone())
                    if repair_state != last_reported_repair_state:
                        print("reuse repair pending", repair_state, repr(error), flush=True)
                        last_reported_repair_state = repair_state
                time.sleep(0.1)
        assert last_repair_error is None, f"real watch did not repair stale inode incarnation: {last_repair_error}"
        repair_record = runtime_record_path(db, "watch.status").read_text().splitlines()[0].split()
        assert repair_record[8] == "0" and repair_record[11] != "1", repair_record
        print("PASS: real fanotify repairs stale directory subtree on reused inode", flush=True)

        history_probe = tree / "growth-history-probe"
        history_probe.write_bytes(b"a" * 4096)
        caught_up(db, tree=tree)
        probe_stat = history_probe.stat()
        probe_identity = (probe_stat.st_dev, probe_stat.st_ino)
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            probe_birth = conn.execute(
                "SELECT birth_ns FROM inodes WHERE dev_id=? AND inode=?", probe_identity
            ).fetchone()[0]
            initial_history = conn.execute(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=? AND inode=?",
                probe_identity).fetchone()[0]
        if probe_birth:
            assert initial_history == probe_stat.st_blocks * 512, (initial_history, probe_stat.st_blocks * 512)
        else:
            assert initial_history == 0, "unknown incarnation must not receive attributed growth"
        history_probe.write_bytes(b"b" * (128 * 1024))
        caught_up(db, tree=tree)
        grown_stat = history_probe.stat()
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            grown_history = conn.execute(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=? AND inode=?",
                probe_identity).fetchone()[0]
        if probe_birth:
            assert grown_history == grown_stat.st_blocks * 512, (grown_history, grown_stat.st_blocks * 512)
        else:
            assert grown_history == 0, "unknown incarnation must not receive attributed growth"
        history_probe.rename(tree / "growth-history-renamed")
        caught_up(db, tree=tree)
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            renamed_history = conn.execute(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=? AND inode=?",
                probe_identity).fetchone()[0]
        if probe_birth:
            assert renamed_history == grown_stat.st_blocks * 512, renamed_history
            print("PASS: growth history follows inode incarnation across writes and rename", flush=True)
        else:
            assert renamed_history == 0
            print("PASS: ambiguous growth is suppressed when birth evidence is unavailable", flush=True)

        # Exercise the precise live stale-leaf shape: an empty inode is indexed,
        # then a later write allocates blocks. Some older daemon builds updated
        # `blocks` but left the leaf's recursive counters at zero, allowing a
        # parent directory to claim caught-up with a one-inode rollup mismatch.
        leaf_probe = tree / "empty-to-allocated-probe"
        leaf_probe.touch()
        caught_up(db, tree=tree)
        leaf_identity = (leaf_probe.stat().st_dev, leaf_probe.stat().st_ino)
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            empty_row = conn.execute(
                "SELECT blocks,recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?",
                leaf_identity).fetchone()
        empty_stat = leaf_probe.stat()
        assert empty_row == (empty_stat.st_blocks * 512, empty_stat.st_blocks * 512, 1), (empty_row, empty_stat.st_blocks)

        leaf_probe.write_bytes(b"allocation after indexed empty-file create")
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        allocated_stat = leaf_probe.stat()
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            allocated_row = conn.execute(
                "SELECT blocks,recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?",
                leaf_identity).fetchone()
        expected_allocated = allocated_stat.st_blocks * 512
        assert allocated_row == (expected_allocated, expected_allocated, 1), (allocated_row, expected_allocated)
        recursive_leaf_transition = {
            "inode": list(leaf_identity),
            "empty_state": {"allocated_bytes": empty_stat.st_blocks * 512, "row": list(empty_row)},
            "allocated_state": {"allocated_bytes": expected_allocated, "row": list(allocated_row)},
            "exact_parent_rollups_passed": True,
        }
        print("PASS: empty-file create then allocated write refreshes recursive leaf counters and exact rollups", flush=True)

        nested = tree / "old" / "nested"
        nested.mkdir(parents=True)
        for i in range(160):
            (nested / f"file-{i}").write_bytes(bytes([i]) * (4096 * (i % 3 + 1)))
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        (tree / "old").rename(tree / "new")
        os.link(tree / "new/nested/file-0", tree / "hardlink")
        (tree / "hardlink").write_bytes(b"expanded" * 4096)
        for i in range(0, 160, 3):
            (tree / f"new/nested/file-{i}").unlink()
        # Reuse a path within a single flush window, then delete it again.
        reused = tree / "new/nested/file-1"
        reused.unlink()
        reused.write_bytes(b"replacement")
        reused.unlink()
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        print("PASS: nested creates, directory rename, hardlink growth, bulk deletes, path reuse", flush=True)

        before = inspect(db)[0]["scan_generation"]
        if direct_daemon:
            assert daemon_process is not None and daemon_process.poll() is None
            pid = daemon_process.pid
        else:
            pid = int(run("systemctl", "show", unit, "-p", "MainPID", "--value").strip())
        errors = []

        def mutate():
            try:
                for i in range(600):
                    path = tree / f"during-scan-{i}"
                    path.write_bytes(b"x" * 4096)
                    if i % 2:
                        path.unlink()
                    time.sleep(0.001)
            except Exception as error:
                errors.append(error)

        writer = threading.Thread(target=mutate)
        writer.start()
        os.kill(pid, signal.SIGHUP)
        writer.join()
        assert not errors, errors
        caught_up(db, before, tree=tree)
        assert_truth(db, tree)
        if direct_daemon:
            assert daemon_process is not None and daemon_process.poll() is None
            print("PASS: real events during background reconciliation, exact totals, isolated daemon remained alive", flush=True)
        else:
            assert run("systemctl", "show", unit, "-p", "NRestarts", "--value").strip() == "0"
            print("PASS: real events during background reconciliation, exact totals, 10s watchdog", flush=True)

        # Reproduce the shape of the recurring production drift: deep temporary
        # build/coverage trees, atomic file replacement, cross-directory moves,
        # hardlinks and rapid path reuse. Exercise the real kernel event stream,
        # not only the in-process event reducer.
        groups = [tree / "tmp/xgs-coverage-db-20261022/coverage",
                  tree / "root/.codex/packages/app-server-daemon",
                  tree / "home/build/artifacts"]
        for group in groups:
            group.mkdir(parents=True)
        # The live old-binary recurrence was an absent /tmp/go-build… path with
        # 799 indexed inodes and a 28.9 MB recursive allocation. Exercise the
        # candidate's real delete stream at that subtree cardinality, then exact
        # check every path, inode allocation, history row and directory rollup.
        stale_build = groups[0].parent / "go-build-live-shape"
        stale_build.mkdir()
        for index in range(798):
            length = 32_769 if index < 674 else 32_768
            (stale_build / f"artifact-{index:03d}").write_bytes(b"x" * length)
        caught_up(db, tree=tree)
        _, build_inodes, _ = filesystem(stale_build)
        assert len(build_inodes) == 799, len(build_inodes)
        large_deleted_subtree = {
            "path": str(stale_build.relative_to(tree)),
            "inodes": len(build_inodes),
            "allocated_bytes": sum(build_inodes.values()),
        }
        shutil.rmtree(stale_build)
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        print(f"PASS: real fanotify deleted {len(build_inodes)}-inode temporary build subtree", flush=True)
        # Match Go's short-lived /tmp/go-build trees. Each iteration creates
        # hundreds of nested package directories and allocated artifacts, then
        # removes the whole tree before explicitly waiting for catch-up. This
        # forces the kernel event queue and userspace coalescer to reconcile
        # ancestor deletes while descendant create/write/delete events overlap.
        go_tmp = temp_root = tree / "tmp"
        go_tmp.mkdir(exist_ok=True)
        churn_rounds = 4
        child_directories = 480
        artifacts_per_directory = 2
        churn_created_inodes = 0
        for round_index in range(churn_rounds):
            build = go_tmp / f"go-build-churn-{round_index:02d}"
            build.mkdir()
            churn_created_inodes += 1
            for child_index in range(child_directories):
                package = build / f"b{child_index + 1:03d}"
                package.mkdir()
                churn_created_inodes += 1
                for artifact_index in range(artifacts_per_directory):
                    artifact = package / f"artifact-{artifact_index}.a"
                    artifact.write_bytes(bytes([round_index + artifact_index]) * (4096 * (artifact_index + 1)))
                    churn_created_inodes += 1
            shutil.rmtree(build)
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            stale_build_rows = conn.execute(
                "SELECT COUNT(*) FROM dirents WHERE name LIKE 'go-build-churn-%'"
            ).fetchone()[0]
            rollup_mismatches = conn.execute("""SELECT COUNT(*) FROM inodes i WHERE i.kind='d' AND (
                recursive_bytes != blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d
                  JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
                  WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1
                    AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
                OR recursive_inodes != 1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d
                  JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
                  WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1
                    AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0))""").fetchone()[0]
        assert stale_build_rows == 0, stale_build_rows
        assert rollup_mismatches == 0, rollup_mismatches
        go_build_churn = {
            "rounds": churn_rounds,
            "child_directories_per_round": child_directories,
            "artifacts_per_directory": artifacts_per_directory,
            "created_inodes": churn_created_inodes,
            "all_trees_removed_before_catchup": True,
            "stale_build_dirents_after_catchup": stale_build_rows,
            "directory_rollup_mismatches_after_catchup": rollup_mismatches,
            "exact_filesystem_truth_passed": True,
        }
        print(f"PASS: exact convergence after {churn_rounds} Go-build-shaped bursts ({churn_created_inodes} created inodes)", flush=True)
        # Exercise common short-lived application files while the watcher
        # observes their containing directory. Keep a WAL reader open while
        # committing writes so SQLite must materialize both -wal and -shm, then
        # close and remove the complete set before the exact-tree assertion.
        for index in range(32):
            sqlite_path = groups[0] / f"sqlite-wal-churn-{index:02d}.db"
            writer = sqlite3.connect(sqlite_path)
            assert writer.execute("PRAGMA journal_mode=WAL").fetchone()[0].lower() == "wal"
            writer.execute("CREATE TABLE events (value INTEGER)")
            writer.commit()
            reader = sqlite3.connect(sqlite_path)
            reader.execute("BEGIN")
            reader.execute("SELECT COUNT(*) FROM events").fetchone()
            writer.execute("INSERT INTO events VALUES (?)", (index,))
            writer.commit()
            assert Path(str(sqlite_path) + "-wal").exists()
            assert Path(str(sqlite_path) + "-shm").exists()
            reader.close()
            writer.close()
            for suffix in ("", "-wal", "-shm"):
                Path(str(sqlite_path) + suffix).unlink(missing_ok=True)
            ephemeral_sqlite_wal_cycles += 1
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        print(f"PASS: real fanotify observed {ephemeral_sqlite_wal_cycles} transient SQLite WAL/SHM lifecycles", flush=True)
        # Recreate the direct-child shapes from the most recent production
        # capture: a Go test directory, a 48 KiB args file, and a systemd-private
        # directory. The live observer found these paths present in SQLite after
        # they had vanished on disk; require exact reconciliation after removal.
        temp_root = tree / "tmp"
        temp_root.mkdir(exist_ok=True)
        go_test_path = temp_root / "TestRecomposeHonoursTimeoutsAndReportsFailures1767114555"
        args_path = temp_root / "args4262786159"
        private_path = temp_root / "systemd-private-test-xgs-control-plane-watchdog.service-test"
        (go_test_path / "nested" / "testdata").mkdir(parents=True)
        for index in range(8):
            (go_test_path / "nested" / "testdata" / f"case-{index}").write_bytes(b"test" * 1024)
        args_path.write_bytes(b"a" * 48_000)
        (private_path / "tmp" / "worker" / "state").mkdir(parents=True)
        for index in range(8):
            (private_path / "tmp" / "worker" / "state" / f"unit-{index}").write_bytes(b"state" * 1024)
        caught_up(db, tree=tree)
        transient_temp_cleanup = {
            "paths": [str(go_test_path.relative_to(tree)), str(args_path.relative_to(tree)),
                      str(private_path.relative_to(tree))],
            "inode_counts_before_delete": {
                "go_test_directory": len(filesystem(go_test_path)[1]),
                "args_file": len(filesystem(args_path)[1]),
                "systemd_private_directory": len(filesystem(private_path)[1]),
            },
        }
        shutil.rmtree(go_test_path)
        args_path.unlink()
        shutil.rmtree(private_path)
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        transient_temp_cleanup["all_paths_removed_from_index"] = True
        print("PASS: real fanotify removed transient Go-test, args-file and systemd-private paths", flush=True)
        # Match the most recent live mismatch exactly: two absent direct /tmp
        # directories whose combined subtree contained 19 inodes and 90,112
        # allocated bytes on the captured filesystem.
        incident_dirs = [
            temp_root / "TestRCARemediatesResourceCapDriftAndRecordsTheAttempt4218169234",
            temp_root / "TestRestoreContainerReadyRequiresHealthyEvidence3538992269",
        ]
        for directory in incident_dirs:
            directory.mkdir()
        incident_files = []
        for dir_index, directory in enumerate(incident_dirs):
            count = 9 if dir_index == 0 else 8
            for index in range(count):
                path = directory / f"case-{index:02d}"
                path.write_bytes(b"x" * 4096)
                incident_files.append(path)
        # st_blocks is measured in 512-byte units, independent of the ext4
        # block size. Keep the fixture portable across 1 KiB/4 KiB filesystems:
        # grow files in allocation-sized increments until the actual allocated
        # byte total matches the incident capture exactly.
        directory_bytes = sum(os.lstat(directory).st_blocks * 512 for directory in incident_dirs)
        target_file_bytes = 90_112 - directory_bytes
        assert target_file_bytes >= len(incident_files) * 4096, (
            "filesystem directory allocation leaves too little room to model "
            "the captured 90,112-byte incident",
            directory_bytes,
        )
        file_index = 0
        while sum(path.stat().st_blocks * 512 for path in incident_files) < target_file_bytes:
            path = incident_files[file_index % len(incident_files)]
            current_size = path.stat().st_size
            path.write_bytes(b"x" * (current_size + 4096))
            file_index += 1
        actual_file_bytes = sum(path.stat().st_blocks * 512 for path in incident_files)
        assert actual_file_bytes == target_file_bytes, (
            "cannot model the captured 90,112-byte allocation on this filesystem",
            {"target_file_bytes": target_file_bytes, "actual_file_bytes": actual_file_bytes,
             "directory_bytes": directory_bytes},
        )
        caught_up(db, tree=tree)
        incident_inodes = sum(len(filesystem(directory)[1]) for directory in incident_dirs)
        incident_allocated = sum(sum(filesystem(directory)[1].values()) for directory in incident_dirs)
        assert incident_inodes == 19, incident_inodes
        assert incident_allocated == 90_112, incident_allocated
        incident_temp_subtrees = {
            "paths": [str(directory.relative_to(tree)) for directory in incident_dirs],
            "inodes": incident_inodes,
            "allocated_bytes": incident_allocated,
        }
        for directory in incident_dirs:
            shutil.rmtree(directory)
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        incident_temp_subtrees["all_paths_removed_from_index"] = True
        print(f"PASS: real fanotify removed exact live temp subtrees ({incident_inodes} inodes)", flush=True)
        probe_source = groups[1] / "hardlink-source"
        probe_alias = groups[1] / "hardlink-alias"
        probe_source.write_bytes(b"source inode")
        caught_up(db, tree=tree)
        os.link(probe_source, probe_alias)
        caught_up(db, tree=tree)
        transient_alias = groups[1] / "hardlink-transient"
        os.link(probe_source, transient_alias)
        transient_alias.write_bytes(b"expanded through transient link" * 512)
        transient_alias.unlink()
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        probe_temp = groups[1] / "hardlink-source.tmp"
        probe_temp.write_bytes(b"replacement inode")
        os.replace(probe_temp, probe_source)
        caught_up(db, tree=tree)
        assert probe_alias.stat().st_ino != probe_source.stat().st_ino
        print("PASS: hardlink-only create and source inode replacement", flush=True)
        for i in range(operation_count):
            group = groups[i % len(groups)]
            target = group / f"artifact-{i % 48:03d}.bin"
            temporary = group / f".artifact-{i % 48:03d}.tmp"
            temporary.write_bytes(bytes([i % 251]) * (512 << (i % 5)))
            os.replace(temporary, target)
            moved = groups[(i + 1) % len(groups)] / f"moved-{i % 32:03d}.bin"
            if i % 4 == 0:
                os.replace(target, moved)
            elif i % 4 == 1 and moved.exists():
                moved.unlink()
            if i % 17 == 0 and target.exists():
                link = group / f"link-{i % 24:03d}"
                link.unlink(missing_ok=True)
                os.link(target, link)
            elif i % 17 == 1:
                (group / f"link-{(i - 1) % 24:03d}").unlink(missing_ok=True)
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        print(f"PASS: {operation_count} real-fanotify build/coverage replacements, moves, hardlinks and path reuse", flush=True)

        # Exercise ancestor geometry changes while descendants are changing.
        # A real fanotify stream can interleave the parent move and child writes
        # across flush batches; flat file replacement churn does not cover this.
        directory_swaps = int(os.environ.get("DUX_LIVE_DIRECTORY_SWAPS", "1500"))
        race_root = tree / "tmp" / "nested-parent-race"
        side_a, side_b, swap_path = race_root / "a", race_root / "b", race_root / "swap"
        for root in (side_a, side_b):
            deep = root / "level" / "deep"
            deep.mkdir(parents=True)
            for index in range(8):
                (deep / f"file-{index}").write_bytes(bytes([index]) * (4096 + index * 128))
        caught_up(db, tree=tree)
        writer_errors = []

        def write_descendants():
            try:
                for index in range(directory_swaps * 2):
                    for side in (side_a, side_b):
                        target = side / "level" / "deep" / f"file-{index % 8}"
                        try:
                            target.write_bytes(bytes([index % 251]) * (4096 + (index % 8) * 512))
                        except FileNotFoundError:
                            # A name can be between the three atomic moves in a swap.
                            continue
            except Exception as error:
                writer_errors.append(repr(error))

        writer = threading.Thread(target=write_descendants, name="dux-descendant-writer")
        writer.start()
        try:
            for index in range(directory_swaps):
                side_a.rename(swap_path)
                side_b.rename(side_a)
                swap_path.rename(side_b)
                if index % 100 == 99:
                    time.sleep(0.02)  # allow multiple fanotify flush windows
        finally:
            writer.join()
        assert not writer_errors, writer_errors
        caught_up(db, tree=tree)
        assert_truth(db, tree)
        print(f"PASS: {directory_swaps} concurrent nested-directory swaps and descendant writes", flush=True)

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        env = dict(os.environ, TERM="xterm-256color")
        ui = subprocess.Popen([str(binary), "--db", str(db)], stdin=slave, stdout=slave, stderr=slave, env=env)
        os.close(slave)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                frame = os.read(master, 65536)
                if frame:
                    break
        os.write(master, b"2\x1b[C\x1b[B\x1b[C\t\r")
        time.sleep(0.1)
        exit_start = time.monotonic()
        os.write(master, b"q")
        ui.wait(timeout=2)
        exit_ms = (time.monotonic() - exit_start) * 1000
        tui_quit_ms = round(exit_ms, 3)
        os.close(master)
        assert ui.returncode == 0, ui.returncode
        print(f"PASS: TUI navigation burst followed by quit in {exit_ms:.0f}ms", flush=True)
    except Exception:
        error = sys.exc_info()[1]
        write_report("failed", repr(error), tui_quit_ms)
        if direct_daemon:
            if daemon_log:
                daemon_log.flush()
            logs = (runtime_dir / "daemon.log").read_text(errors="replace") if runtime_dir and (runtime_dir / "daemon.log").exists() else ""
        else:
            logs = run("journalctl", "-u", unit, "--no-pager", "-o", "cat")
        (base / "journal.log").write_text(logs)
        if os.environ.get("DUX_TRACE_LINK"):
            print(logs, file=sys.stderr)
        else:
            print("\n".join(logs.splitlines()[-30:]), file=sys.stderr)
        if os.environ.get("DUX_KEEP_FAILED_WORKSPACE"):
            destination = Path(os.environ.get("DUX_FAILED_DIR", "/root/readiness/live-failures")) / unit
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(base, destination, dirs_exist_ok=True)
            print(f"FAILED WORKSPACE PRESERVED: {destination}", file=sys.stderr)
        raise
    else:
        write_report("passed", tui_quit_ms=tui_quit_ms)
    finally:
        if direct_daemon:
            if daemon_process is not None and daemon_process.poll() is None:
                daemon_process.terminate()
                try:
                    daemon_process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    daemon_process.kill()
                    daemon_process.wait(timeout=5)
            if daemon_log:
                daemon_log.close()
            if runtime_dir:
                shutil.rmtree(runtime_dir, ignore_errors=True)
        elif started:
            subprocess.run(["systemctl", "stop", unit], capture_output=True)
