#!/usr/bin/env python3
"""Isolated dux release qualification. Never opens the installed live index.

The parent starts a bounded transient systemd unit in a private mount namespace.
The worker creates a disposable loop filesystem exclusively for these tests. Full-disk
and read-only faults affect only that disposable filesystem. JSON evidence is
written even on failure. See --help for bounded crash and soak workloads.
"""
import argparse
import collections
import contextlib
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import signal
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import threading
import time
from dux_qualification_sandbox import require_disposable_qualification_vm

MIB = 1024 * 1024
OVERFLOW_RECOVERY_MIN_FREE_BYTES = 512 * MIB


def command(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True,
                          text=True, timeout=kwargs.pop("timeout", 90), **kwargs)


def runtime_path(db, directory, kind):
    value = 0xCBF29CE484222325
    for byte in os.fsencode(db.resolve()):
        value = ((value ^ byte) * 0x100000001B3) & ((1 << 64) - 1)
    return directory / f"{value:016x}.{kind}"


def parse_legacy_pipeline_status(output):
    """Read the pipeline counters printed by pre-runtime-status daemons."""
    match = re.search(
        r"(?m)^pipeline:\s*(drained|backlogged);\s*(\d+) pending / (\d+) cap; "
        r"(\d+) events.*?;\s*(\d+) updates committed;\s*(\d+) dropped;",
        output,
    )
    if not match:
        raise AssertionError("legacy status output does not contain a recognized pipeline record")
    state, pending, capacity, seen, committed, dropped = match.groups()
    return {
        "pending": int(pending),
        "capacity": int(capacity),
        "drained": state == "drained",
        "seen": int(seen),
        "committed": int(committed),
        "dropped": int(dropped),
        "uncertain": "uncertain" in output.lower(),
    }


def validate_overflow_recovery_headroom(available_bytes):
    """Reject overflow fixtures that leave too little space for recovery scans."""
    if available_bytes < OVERFLOW_RECOVERY_MIN_FREE_BYTES:
        raise RuntimeError(
            "overflow suite needs at least "
            f"{OVERFLOW_RECOVERY_MIN_FREE_BYTES // MIB} MiB free on the watched "
            f"fixture filesystem for reconciliation; only {available_bytes // MIB} "
            "MiB remains. Increase --filesystem-mib; overflow was not injected."
        )
    return available_bytes


def snapshot(db, immutable=False, attachments=()):
    uri = db.as_uri() + "?mode=ro" + ("&immutable=1" if immutable else "")
    connection = sqlite3.connect(uri, uri=True, timeout=1)
    for alias, filename in attachments:
        assert alias.isidentifier(), alias
        connection.execute(f"ATTACH DATABASE ? AS {alias}", (str(filename),))
    connection.execute("BEGIN")
    return connection


def filesystem_snapshot(tree, output_dir):
    """Materialize filesystem truth on disk so large exact checks stay bounded."""
    descriptor, filename = tempfile.mkstemp(prefix="dux-truth-", suffix=".sqlite", dir=output_dir)
    os.close(descriptor)
    connection = sqlite3.connect(filename)
    try:
        connection.execute("PRAGMA journal_mode=OFF")
        connection.execute("PRAGMA synchronous=OFF")
        connection.execute("CREATE TABLE entries(parent_dev INTEGER,parent_inode INTEGER,name BLOB,dev INTEGER,inode INTEGER,PRIMARY KEY(parent_dev,parent_inode,name)) WITHOUT ROWID")
        connection.execute("CREATE TABLE inodes(dev INTEGER,inode INTEGER,blocks INTEGER,PRIMARY KEY(dev,inode)) WITHOUT ROWID")
        root_stat = tree.lstat()
        connection.execute("INSERT INTO inodes VALUES(?,?,?)", (root_stat.st_dev, root_stat.st_ino, root_stat.st_blocks * 512))
        entry_batch, inode_batch = [], []
        for parent, dirs, files in os.walk(tree):
            parent_stat = os.stat(parent)
            for name in dirs + files:
                info = (Path(parent) / name).lstat()
                entry_batch.append((parent_stat.st_dev, parent_stat.st_ino, os.fsencode(name), info.st_dev, info.st_ino))
                inode_batch.append((info.st_dev, info.st_ino, info.st_blocks * 512))
                if len(entry_batch) >= 2048:
                    connection.executemany("INSERT INTO entries VALUES(?,?,?,?,?)", entry_batch)
                    connection.executemany("INSERT OR REPLACE INTO inodes VALUES(?,?,?)", inode_batch)
                    entry_batch.clear()
                    inode_batch.clear()
        if entry_batch:
            connection.executemany("INSERT INTO entries VALUES(?,?,?,?,?)", entry_batch)
            connection.executemany("INSERT OR REPLACE INTO inodes VALUES(?,?,?)", inode_batch)
        connection.commit()
        connection.close()
        return filename
    except BaseException:
        connection.close()
        Path(filename).unlink(missing_ok=True)
        raise


def check_index(db, tree=None, immutable=False):
    """Verify one read snapshot, not independently changing SQL observations."""
    truth_path = filesystem_snapshot(tree, db.parent.parent) if tree is not None else None
    conn = snapshot(db, immutable, (("truth", truth_path),) if truth_path else ())
    try:
        meta = dict(conn.execute("SELECT key,value FROM meta"))
        root = int(meta["root_dev"]), int(meta["root_inode"])
        stored = conn.execute("SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?", root).fetchone()
        raw = conn.execute("SELECT SUM(blocks),COUNT(*) FROM inodes").fetchone()
        assert stored == raw, f"root accounting: {stored} != {raw}"
        orphan = conn.execute("SELECT COUNT(*) FROM dirents d LEFT JOIN inodes p ON p.dev_id=d.parent_dev AND p.inode=d.parent_inode WHERE p.inode IS NULL").fetchone()[0]
        assert orphan == 0, f"{orphan} orphan directory entries"
        primes = conn.execute("SELECT COUNT(*) FROM (SELECT i.dev_id,i.inode FROM inodes i LEFT JOIN dirents d ON d.dev_id=i.dev_id AND d.inode=i.inode GROUP BY i.dev_id,i.inode HAVING COALESCE(SUM(d.prime),0)!=1)").fetchone()[0]
        assert primes == 0, f"{primes} inodes without exactly one prime attribution"
        rollup_errors = conn.execute("""SELECT i.dev_id,i.inode,i.recursive_bytes,
            i.blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0),
            i.recursive_inodes,1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
            FROM inodes i WHERE i.kind='d' AND (i.recursive_bytes != blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
            OR i.recursive_inodes != 1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0))
            ORDER BY i.dev_id,i.inode LIMIT 32""").fetchall()
        assert not rollup_errors, f"incorrect directory rollups (dev,ino,stored_bytes,expected_bytes,stored_inodes,expected_inodes): {rollup_errors!r}"
        if tree is not None:
            path_mismatch = conn.execute("""SELECT COUNT(*) FROM (
                    SELECT e.parent_dev,e.parent_inode,e.name FROM truth.entries e LEFT JOIN main.dirents d
                    ON d.parent_dev=e.parent_dev AND d.parent_inode=e.parent_inode AND d.name=e.name
                    WHERE d.name IS NULL OR d.dev_id!=e.dev OR d.inode!=e.inode
                    UNION ALL
                    SELECT d.parent_dev,d.parent_inode,d.name FROM main.dirents d LEFT JOIN truth.entries e
                    ON e.parent_dev=d.parent_dev AND e.parent_inode=d.parent_inode AND e.name=d.name
                    WHERE NOT(d.parent_dev=d.dev_id AND d.parent_inode=d.inode) AND e.name IS NULL)""").fetchone()[0]
            if path_mismatch:
                actual = conn.execute("SELECT parent_dev,parent_inode,name,dev_id,inode FROM dirents WHERE NOT(parent_dev=dev_id AND parent_inode=inode) ORDER BY parent_dev,parent_inode,name LIMIT 8").fetchall()
                expected = conn.execute("SELECT parent_dev,parent_inode,name,dev,inode FROM truth.entries ORDER BY parent_dev,parent_inode,name LIMIT 8").fetchall()
                raise AssertionError(f"path/identity mismatch count={path_mismatch}; indexed_sample={actual!r}; filesystem_sample={expected!r}")
            inode_mismatch = conn.execute("""SELECT COUNT(*) FROM (
                    SELECT i.dev_id,i.inode FROM main.inodes i LEFT JOIN truth.inodes f ON f.dev=i.dev_id AND f.inode=i.inode
                    WHERE f.inode IS NULL OR f.blocks!=i.blocks
                    UNION ALL
                    SELECT f.dev,f.inode FROM truth.inodes f LEFT JOIN main.inodes i ON i.dev_id=f.dev AND i.inode=f.inode
                    WHERE i.inode IS NULL)""").fetchone()[0]
            if inode_mismatch:
                actual = conn.execute("SELECT dev_id,inode,blocks FROM inodes ORDER BY dev_id,inode LIMIT 12").fetchall()
                expected = conn.execute("SELECT dev,inode,blocks FROM truth.inodes ORDER BY dev,inode LIMIT 12").fetchall()
                raise AssertionError(f"inode identity/allocation mismatch count={inode_mismatch}; indexed_sample={actual!r}; filesystem_sample={expected!r}")
        return meta, stored
    finally:
        conn.close()
        if truth_path:
            Path(truth_path).unlink(missing_ok=True)


def integrity(db):
    with contextlib.closing(snapshot(db)) as conn:
        result = conn.execute("PRAGMA integrity_check").fetchall()
        assert result == [("ok",)], result
    check_index(db)


class Daemon:
    def __init__(self, case, binary=None, pause=None, prefix=None, max_cpu_percent=50):
        self.case, self.binary = case, binary or case.binary
        self.marker = case.base / "crash.marker"
        self.marker.unlink(missing_ok=True)
        env = dict(case.env)
        if pause:
            env.update(DUX_TEST_PAUSE_AT=pause, DUX_TEST_MARKER=str(self.marker))
        self.log = (case.base / "daemon.log").open("a")
        self.process = subprocess.Popen([*(prefix or []), str(self.binary), "--db", str(case.db), "daemon", str(case.tree),
                                         "--flush-ms", "100", "--max-cpu", str(max_cpu_percent)],
                                        env=env, stdout=self.log, stderr=subprocess.STDOUT)
        case.children.append(self)

    def kill(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=10)
        self.log.close()

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGCONT)
            self.process.terminate()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.kill()
                raise AssertionError("daemon did not stop within 15s")
        self.log.close()

    def record(self):
        status = runtime_path(self.case.db, self.case.runtime, "watch.status")
        try:
            fields = status.read_text().splitlines()[0].split()
        except FileNotFoundError:
            if not hasattr(self, 'unit'):
                # Older supported binaries expose these counters through the
                # status command, but predate the atomic watch.status file.
                result = command(str(self.binary), "--db", str(self.case.db), "status",
                                 env=self.case.env, timeout=10)
                legacy = parse_legacy_pipeline_status(result.stdout)
                legacy.update(updated_at=time.monotonic_ns(), status_mtime_ns=time.monotonic_ns())
                return legacy
            error = FileNotFoundError(status)
            details = command('systemctl', 'show', self.unit, '-p', 'ActiveState', '-p', 'MainPID',
                              '-p', 'Result', '-p', 'NRestarts', '--no-pager').stdout.strip()
            pid = self.process.pid
            commandline = Path(f'/proc/{pid}/cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace') if pid else ''
            tail = subprocess.run(['journalctl', '-u', self.unit, '-n', '30', '--no-pager', '-o', 'cat'],
                                  capture_output=True, text=True).stdout[-3000:]
            raise AssertionError(f'{error}; unit={details}; pid={pid}; cmd={commandline}; log={tail}') from error
        version = fields[0]
        assert version in {"1", "2", "3", "4"}, fields
        assert int(fields[2]) == self.process.pid, "runtime record belongs to another process"
        overflow_notifications = int(fields[13]) if version == "4" else 0
        return {"updated_at": int(fields[1]), "status_mtime_ns": status.stat().st_mtime_ns,
                "pending": int(fields[3]), "drained": fields[4] == "1",
                "seen": int(fields[5]), "resolved": int(fields[6]), "committed": int(fields[7]),
                "dropped": int(fields[8]),
                "uncertain": version in {"2", "3", "4"} and fields[11] == "1",
                "kernel_overflow_notifications": overflow_notifications}

    def wait(self, predicate, description, timeout=60, interval=0.05):
        end, last = time.monotonic() + timeout, "not ready"
        while time.monotonic() < end:
            assert self.process.poll() is None, f"daemon exited: {self.process.returncode}"
            try:
                value = predicate()
                if value:
                    return value
            except (AssertionError, OSError, sqlite3.Error, ValueError, KeyError) as error:
                last = str(error)
            time.sleep(interval)
        raise AssertionError(f"timeout waiting for {description}: {last}")

    def pipeline_ready(self, previous_generation=None):
        record = self.record()
        if record["pending"] or not record["drained"]:
            raise AssertionError(f"pipeline is not caught up: {record}")
        if record["uncertain"]:
            raise AssertionError(f"pipeline reports uncertain event state: {record}")
        with contextlib.closing(snapshot(self.case.db)) as conn:
            meta = dict(conn.execute("SELECT key,value FROM meta"))
        if any(k in meta for k in ("dirty_since", "paused_since", "flush_error")):
            return False
        if previous_generation is not None and meta.get("scan_generation") == previous_generation:
            return False
        return meta, record

    def wait_pipeline(self, previous_generation=None, timeout=90, after_status_mtime_ns=None):
        def ready():
            return self.pipeline_ready(previous_generation)
        first_meta, first_status = self.wait(ready, "pipeline convergence", timeout)

        # A drained status describes the last completed poll, not events that
        # arrived after that status was written. Require a newer daemon cycle
        # after the caller's mutation window before treating the index as caught
        # up; otherwise a delayed poll can make a stale status look current.
        def fresh_ready():
            current = ready()
            minimum_mtime = first_status["status_mtime_ns"]
            if after_status_mtime_ns is not None:
                minimum_mtime = max(minimum_mtime, after_status_mtime_ns)
            if current and current[1]["status_mtime_ns"] > minimum_mtime:
                return current
            return False

        return self.wait(fresh_ready, "fresh post-mutation pipeline convergence", timeout)

    def exact(self, previous_generation=None, timeout=90):
        started = time.monotonic()
        meta, status = self.wait_pipeline(previous_generation, timeout)
        def ready():
            return self.pipeline_ready(previous_generation)
        # A status cycle and an index snapshot can straddle a delayed event from
        # the completed filesystem operation. Check truth again on the next
        # watcher publication, requiring two consecutive exact snapshots. This
        # rejects both transient lag and persistent drift without requiring an
        # empty queue when the materialized index already matches the tree.
        # An exact filesystem walk can itself cross the resource guardian's
        # pressure threshold on a large tree. In that interval the daemon is
        # required to pause writes and expose paused_since; the on-disk index
        # can briefly lag until the guardian resumes and the retained event
        # batch commits. Do not classify that intentional pause as lost data.
        # Keep sampling fresh watcher cycles for the caller's full bounded
        # timeout, and fail only if the mismatch remains after recovery.
        deadline = time.monotonic() + timeout
        consecutive_exact = 0
        exact_checks = 0
        exact_check_seconds = []
        fresh_cycles = 0
        last_mismatch = None
        pause_seen = False
        while True:
            try:
                with contextlib.closing(snapshot(self.case.db)) as conn:
                    meta_now = dict(conn.execute("SELECT key,value FROM meta"))
                if "paused_since" in meta_now:
                    pause_seen = True
                    consecutive_exact = 0
                    if time.monotonic() >= deadline:
                        raise AssertionError(
                            f"pipeline remained paused during exactness recovery: "
                            f"{meta_now.get('pause_reason', 'unknown pressure')}"
                        )
                    time.sleep(0.25)
                    continue
                check_started = time.monotonic()
                check_index(self.case.db, self.case.tree)
                exact_checks += 1
                exact_check_seconds.append(round(time.monotonic() - check_started, 3))
                exact_check_seconds = exact_check_seconds[-4:]
                consecutive_exact += 1
                if consecutive_exact == 2:
                    self.last_exact_convergence_ms = round((time.monotonic() - started) * 1000)
                    return meta
            except AssertionError as error:
                consecutive_exact = 0
                last_mismatch = error
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                with contextlib.closing(snapshot(self.case.db)) as conn:
                    meta_now = dict(conn.execute("SELECT key,value FROM meta"))
                record = self.record()
                if pause_seen or "paused_since" in meta_now:
                    raise AssertionError(
                        f"pipeline did not recover from pause within {timeout}s; "
                        f"reason={meta_now.get('pause_reason')!r} status={record!r}; "
                        f"last exactness mismatch={last_mismatch}"
                    )
                if last_mismatch is not None:
                    raise AssertionError(
                        f"filesystem/index mismatch persisted through {timeout}s of "
                        f"fresh watcher cycles; status={record!r}; meta="
                        f"{ {k: meta_now.get(k) for k in ('dirty_reason','flush_error')}!r}; "
                        f"mismatch={last_mismatch}"
                    )
                raise AssertionError(
                    "watcher did not complete two stable exact snapshots: "
                    f"successful_checks={exact_checks} consecutive={consecutive_exact} "
                    f"recent_check_seconds={exact_check_seconds!r} "
                    f"fresh_cycles={fresh_cycles} pause_seen={pause_seen} "
                    f"status={record!r} meta="
                    f"{ {k: meta_now.get(k) for k in ('dirty_since','paused_since',
                        'pause_reason','flush_error','scan_generation')}!r}"
                )
            previous_status_mtime = status["status_mtime_ns"]

            def next_fresh_ready():
                nonlocal pause_seen
                current = ready()
                if not current:
                    with contextlib.closing(snapshot(self.case.db)) as conn:
                        current_meta = dict(conn.execute("SELECT key,value FROM meta"))
                    if "paused_since" in current_meta:
                        pause_seen = True
                if current and current[1]["status_mtime_ns"] > previous_status_mtime:
                    return current
                return False

            try:
                meta, status = self.wait(
                    next_fresh_ready, "index convergence after fresh watcher cycle",
                    min(2.0, remaining), interval=0.2
                )
                fresh_cycles += 1
            except AssertionError:
                # A short wait can expire while a controlled pressure pause is
                # active. Recheck the same bounded overall deadline above.
                continue

    def paused_at(self, stage):
        self.wait(lambda: self.marker.exists() and self.marker.read_text().strip() == f"{self.process.pid} {stage}", stage)


class Case:
    def __init__(self, base, filesystem_root, runtime, binary, fault_binary):
        self.base, self.binary, self.fault_binary = base, binary, fault_binary
        self.root, self.runtime = filesystem_root, runtime
        self.tree, self.db = self.root / "tree", self.root / "index.db"
        self.tree.mkdir()
        self.children = []
        self.env = dict(os.environ, DUX_RUNTIME_DIR=str(runtime))
        self.env.pop("NOTIFY_SOCKET", None)
        self.history_bucket = int(time.time()) // 300 - 2

    def scan(self, binary=None, check=True):
        result = subprocess.run([str(binary or self.binary), "--db", str(self.db), "scan", str(self.tree), "--quiet"],
                                capture_output=True, text=True, env=self.env, timeout=90)
        if check:
            assert result.returncode == 0, result.stderr
        return result

    def seed(self, count=256):
        print(f"PROGRESS seed start: {count} files", flush=True)
        for i in range(count):
            path = self.tree / f"dir-{i % 8}" / f"file-{i}"
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(bytes([i % 256]) * 4096)
            if (i + 1) % 10000 == 0:
                print(f"PROGRESS seed {i + 1}/{count} files", flush=True)
        # Sparse allocation, a hardlink and a byte filename are permanent fixtures.
        with (self.tree / "sparse").open("wb") as file:
            file.truncate(16 * MIB)
        os.link(self.tree / "dir-0/file-0", self.tree / "hardlink")
        raw = os.fsencode(self.tree) + b"/nonutf8-\xff-name"
        descriptor = os.open(raw, os.O_CREAT | os.O_WRONLY, 0o600)
        os.write(descriptor, b"byte-name")
        os.close(descriptor)
        print(f"PROGRESS seed complete; starting baseline scan of {count} files", flush=True)
        self.scan()
        print("PROGRESS baseline scan complete", flush=True)
        with contextlib.closing(sqlite3.connect(self.db)) as conn, conn:
            conn.execute("INSERT INTO growth VALUES(?,-7,-7,0,104729)", (self.history_bucket,))

    def history(self):
        with contextlib.closing(snapshot(self.db)) as conn:
            value = conn.execute("SELECT delta FROM growth WHERE bucket=? AND dev_id=-7 AND inode=-7", (self.history_bucket,)).fetchone()
        assert value == (104729,), f"committed history was lost or duplicated: {value}"

    def close(self):
        errors = []
        for daemon in self.children:
            try:
                daemon.stop()
            except Exception as error:
                errors.append(str(error))
        if errors:
            raise AssertionError("; ".join(errors))


class DisposableFilesystem:
    def __init__(self, base, size_mib=768, inode_count=0, filesystem_type="ext4"):
        self.base, self.mount = base, base / "filesystem"
        self.size_mib, self.inode_count = size_mib, inode_count
        self.filesystem_type = filesystem_type

    def __enter__(self):
        assert os.readlink("/proc/self/ns/mnt") != os.readlink("/proc/1/ns/mnt"), "refusing mount faults outside a private mount namespace"
        self.mount.mkdir()
        self.image = self.base / "filesystem.img"
        with self.image.open("wb") as image:
            image.truncate(self.size_mib * MIB)
        mkfs = {"ext4": ["mkfs.ext4", "-F", "-q"],
                "xfs": ["mkfs.xfs", "-f", "-q"],
                "btrfs": ["mkfs.btrfs", "-f", "-q"]}[self.filesystem_type]
        if self.filesystem_type == "ext4" and self.inode_count:
            mkfs.extend(["-N", str(self.inode_count)])
        command(*mkfs, self.image)
        command("mount", "-o", "loop,nosuid,nodev", self.image, self.mount)
        return self.mount

    def __exit__(self, exc_type, exc, traceback):
        # A cleanup failure must not replace the qualification failure that
        # caused stack unwinding. In particular, preserving the original exact
        # mismatch is more useful than reporting only a secondary busy-mount
        # error. When the suite itself succeeded, cleanup remains a hard gate.
        remount = subprocess.run(
            ["mount", "-o", "remount,rw", str(self.mount)],
            capture_output=True, text=True, timeout=10,
        )
        unmount = subprocess.run(
            ["umount", str(self.mount)],
            capture_output=True, text=True, timeout=15,
        )
        if (remount.returncode or unmount.returncode) and exc_type is None:
            details = "\n".join(
                part for part in (
                    remount.stderr.strip() if remount.returncode else "",
                    unmount.stderr.strip() if unmount.returncode else "",
                ) if part
            )
            raise RuntimeError(f"private filesystem cleanup failed: {details}")
        if (remount.returncode or unmount.returncode) and exc_type is not None:
            print(
                "WARNING: private filesystem cleanup also failed while handling "
                f"{exc_type.__name__}: "
                f"{(remount.stderr or unmount.stderr).strip()}",
                file=sys.stderr,
                flush=True,
            )


def crash_tests(case, args, evidence):
    randomizer = random.Random(args.seed)
    stages = ["watch.flush_before_commit", "watch.flush_committed", "scan.build_complete",
              "scan.install_started", "scan.install_midway", "scan.install_committed"]
    for round_number in range(args.crash_rounds):
        stage = stages[(round_number // 2) % len(stages)] if args.fault_binary and round_number % 2 == 0 else None
        daemon = Daemon(case, case.fault_binary if stage else case.binary, stage)
        if stage and stage.startswith("scan."):
            daemon.paused_at(stage)  # existing-index startup automatically reconciles
        else:
            daemon.exact()
            (case.tree / "pulse").write_bytes(bytes([round_number % 256]) * (4096 * (round_number % 4 + 1)))
            if stage:
                daemon.paused_at(stage)
            else:
                if round_number % 3 == 0:
                    daemon.process.send_signal(signal.SIGHUP)
                time.sleep(randomizer.uniform(0.001, 0.18))
        daemon.kill()
        integrity(case.db)
        case.history()
        (case.tree / f"downtime-{round_number % 7}").write_bytes(b"changed while down")
        recovery_start = time.monotonic()
        recovered = Daemon(case)
        recovered.exact()
        case.history()
        recovered.stop()
        integrity(case.db)
        evidence.append({"round": round_number + 1, "boundary": stage or "random", "recovery_seconds": round(time.monotonic()-recovery_start, 3)})
        print(f"PASS crash {round_number+1}/{args.crash_rounds}: {stage or 'random timing'}", flush=True)
    with contextlib.closing(sqlite3.connect(case.db)) as conn, conn:
        conn.execute("BEGIN IMMEDIATE")
        conn.execute("INSERT INTO names_fts(names_fts,rank) VALUES('integrity-check',1)")
        conn.rollback()


def overflow_reconciliation_evidence(daemon_log):
    """Prove overflow recovery follows a persistent backlog, including drain-first recovery."""
    overflow_at = daemon_log.rfind('fanotify queue overflow — index may have missed events')
    if overflow_at < 0:
        return {'overflow_observed': False, 'backlog_persisted': False,
                'reconciliation_started_after_backlog': False,
                'scan_completed_after_reconciliation': False,
                'accounting_verified_after_reconciliation': False,
                'caught_up_after_reconciliation': False, 'passed': False}

    backlog_at = daemon_log.find('index backlog persists —', overflow_at)
    backlog_persisted = backlog_at >= 0
    if not backlog_persisted:
        return {'overflow_observed': True, 'backlog_persisted': False,
                'reconciliation_started_after_backlog': False,
                'scan_completed_after_reconciliation': False,
                'accounting_verified_after_reconciliation': False,
                'caught_up_after_reconciliation': False, 'passed': False}

    start_markers = (
        'starting reconciliation with queued changes after persistent backlog',
        'starting background reconciliation of ',
    )
    starts = [(daemon_log.find(marker, backlog_at), marker) for marker in start_markers]
    starts = [(offset, marker) for offset, marker in starts if offset >= 0]
    reconciliation_at, start_marker = min(starts, default=(-1, ''))
    scan_at = daemon_log.find('rescan complete:', reconciliation_at) if reconciliation_at >= 0 else -1
    verified_at = (daemon_log.find('reconciliation accounting verification passed', scan_at)
                   if scan_at >= 0 else -1)
    caught_up_at = (daemon_log.find('caught up — live updates resumed', verified_at)
                    if verified_at >= 0 else -1)
    scan_completed = scan_at >= 0
    accounting_verified = verified_at >= 0
    caught_up = caught_up_at >= 0
    return {
        'overflow_observed': True,
        'backlog_persisted': True,
        'reconciliation_started_after_backlog': reconciliation_at >= 0,
        'reconciliation_start_reason': ('queued_changes' if 'queued changes' in start_marker
                                        else 'backlog_drained_after_persistence'
                                        if start_marker else None),
        'scan_completed_after_reconciliation': scan_completed,
        'accounting_verified_after_reconciliation': accounting_verified,
        'caught_up_after_reconciliation': caught_up,
        'passed': reconciliation_at >= 0 and scan_completed and accounting_verified and caught_up,
    }


def overflow_test(case, _args, evidence):
    maximum = int(Path("/proc/sys/fs/fanotify/max_queued_events").read_text())
    assert maximum <= 65536, "configured kernel queue too large for this bounded local test"
    statvfs = os.statvfs(case.tree)
    validate_overflow_recovery_headroom(statvfs.f_bavail * statvfs.f_frsize)
    daemon = Daemon(case)
    before = daemon.exact()["scan_generation"]
    daemon.process.send_signal(signal.SIGSTOP)
    try:
        with contextlib.closing(sqlite3.connect(case.db)) as conn, conn:
            conn.execute("CREATE TRIGGER block_dirty BEFORE INSERT ON meta WHEN new.key='dirty_since' BEGIN SELECT RAISE(ABORT,'dirty metadata unavailable'); END")
        storm = case.tree / "storm"
        storm.mkdir()
        for i in range(maximum + 2048):
            (storm / f"queued-{i}").touch()
    finally:
        daemon.process.send_signal(signal.SIGCONT)
    daemon.wait(lambda: daemon.record()["uncertain"], "explicit uncertainty after true kernel queue overflow")
    with contextlib.closing(snapshot(case.db)) as conn:
        assert conn.execute("SELECT value FROM meta WHERE key='dirty_since'").fetchone() is None
    status = command(case.binary, "--db", case.db, "status", env=case.env).stdout
    assert "DIRTY" in status, "SQLite metadata failure hid actual event loss"
    metrics = command(case.binary, "--db", case.db, "metrics", "--top", "0", env=case.env).stdout
    assert "\ndux_index_dirty 1\n" in metrics, "monitoring hid actual event loss"
    assert "\ndux_index_caught_up 0\n" in metrics, "monitoring incorrectly reported trustworthy data"
    with contextlib.closing(sqlite3.connect(case.db, timeout=10)) as conn, conn:
        conn.execute("DROP TRIGGER block_dirty")
    daemon.process.send_signal(signal.SIGHUP)
    started = time.monotonic()
    # Recovery includes rebuilding the full overflowed path set, then two
    # exact filesystem snapshots. Under the production cgroup budget the scan
    # and truth walks can trigger the pressure guardian; preserve a generous
    # but finite bound and let exact() report whether delay came from pause,
    # index mismatch, or missing fresh watcher publications.
    daemon.exact(before, timeout=600)
    daemon_log = (case.base / "daemon.log").read_text(errors="replace")
    reconciliation_evidence = overflow_reconciliation_evidence(daemon_log)
    evidence.update(queue_capacity=maximum, created=maximum+2048,
                    recovery_seconds=round(time.monotonic()-started, 3),
                    metadata_write_failure_visible=True,
                    persistent_backlog_reconciliation=reconciliation_evidence)
    persistent_backlog_reconciled = reconciliation_evidence['passed']
    assert persistent_backlog_reconciled, (
        "overflow recovery did not reconcile, verify accounting, and catch up after a "
        f"persistent backlog: {reconciliation_evidence}"
    )
    case.history()
    # Keep later suites inside the disposable filesystem's bounded inode
    # budget. The recovery assertion above includes the full overflow storm;
    # after it passes, remove the storm through the live watcher and confirm
    # that cleanup also reconciles before proceeding.
    removed = 0
    for path in storm.iterdir():
        path.unlink()
        removed += 1
        if removed % 2000 == 0:
            daemon.exact(timeout=180)
            case.history()
    storm.rmdir()
    daemon.exact(timeout=600)
    case.history()
    daemon.stop()
    integrity(case.db)
    print("PASS real queue overflow, failed dirty-metadata write, visible uncertainty and reconciliation", flush=True)


def process_cpu_seconds(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    ticks = int(fields[11]) + int(fields[12])  # utime + stime (Linux stat fields 14 and 15)
    return ticks / os.sysconf("SC_CLK_TCK")


def wait_for_process_state(pid, expected, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        if fields[0] in expected:
            return
        time.sleep(0.01)
    raise AssertionError(f"process {pid} did not enter state {sorted(expected)} within {timeout}s")


def cpu_event_burst_test(case, _args, evidence):
    # The worker cgroup is at least 25%; target 10% so the cgroup ceiling
    # cannot hide a broken in-process governor. Keep a 20% measurement margin.
    daemon_max_cpu_percent = 10
    daemon = Daemon(case, max_cpu_percent=daemon_max_cpu_percent)
    try:
        daemon.exact()
        burst = case.tree / "cpu-burst"
        burst.mkdir()
        daemon.exact()
        before = daemon.record()
        pid = daemon.process.pid
        # Stop Dux while producing events so the Python producer cannot consume
        # the shared worker quota and make an unthrottled daemon look compliant.
        # The burst stays in the finite fanotify queue; queue overflow is a test
        # failure, and cleanup resumes the daemon even if fixture creation fails.
        os.kill(pid, signal.SIGSTOP)
        wait_for_process_state(pid, {"T", "t"})
        producer_started = time.monotonic()
        for i in range(12000):
            (burst / f"event-{i:05d}").write_bytes(b"fanotify catch-up")
        producer_seconds = time.monotonic() - producer_started
        cpu_before = process_cpu_seconds(pid)
        event_window_started = time.monotonic()
        os.kill(pid, signal.SIGCONT)
        catchup_started = time.monotonic()
        daemon.wait_pipeline(timeout=60, after_status_mtime_ns=before["status_mtime_ns"])
        catch_up_seconds = time.monotonic() - catchup_started
        event_window_seconds = time.monotonic() - event_window_started
        after = daemon.record()
        event_count = after["seen"] - before["seen"]
        cpu_fraction = (process_cpu_seconds(pid) - cpu_before) / event_window_seconds
        exact_started = time.monotonic()
        daemon.exact(timeout=120)
        exact_validation_seconds = time.monotonic() - exact_started
        # Persist all measured values before enforcing budgets. CPU measurement
        # starts only after the producer has exited and Dux has resumed.
        evidence.update(event_count=event_count, event_production_seconds=round(producer_seconds, 3),
                        catch_up_seconds=round(catch_up_seconds, 3),
                        event_window_seconds=round(event_window_seconds, 3),
                        exact_validation_seconds=round(exact_validation_seconds, 3),
                        event_cpu_fraction=round(cpu_fraction, 3), daemon_max_cpu_percent=daemon_max_cpu_percent,
                        outer_cgroup_cpu_quota_percent=_args.cpu_quota_percent)
        assert event_count >= 10000, f"only {event_count} fanotify events observed"
        assert catch_up_seconds < 60, (
            f"10k-event catch-up took {catch_up_seconds:.3f}s after "
            f"{producer_seconds:.3f}s event production"
        )
        assert exact_validation_seconds < 120, (
            f"100k-file exact validation took {exact_validation_seconds:.3f}s"
        )
        assert cpu_fraction <= 0.12, (
            f"daemon event CPU used {cpu_fraction:.3f} CPUs for --max-cpu "
            f"{daemon_max_cpu_percent} under a {_args.cpu_quota_percent}% cgroup quota"
        )
        # Cleanup exactness scans the complete 100k-file fixture. A prior
        # consolidated run measured a 69.992s single exact snapshot under the
        # 25% CPU cgroup; the old 60s cleanup deadline could expire after one
        # valid snapshot and before the second fresh watcher publication.
        removed = 0
        for path in burst.iterdir():
            path.unlink()
            removed += 1
            if removed % 2000 == 0:
                daemon.exact(timeout=180)
        burst.rmdir()
        daemon.exact(timeout=180)
        daemon.stop()
        integrity(case.db)
        case.history()
        print(
            f"PASS {event_count} events produced in {producer_seconds:.2f}s, "
            f"pipeline caught up in {catch_up_seconds:.2f}s, exact validation "
            f"in {exact_validation_seconds:.2f}s at {cpu_fraction:.2f} CPU",
            flush=True,
        )
    finally:
        daemon.stop()


def production_churn_test(case, args, evidence):
    """Stress the event reducer on a realistic-sized index, then prove exact truth."""
    randomizer = random.Random(args.seed ^ 0xD0C5)
    groups = [case.tree / f'churn-group-{index:03d}' for index in range(64)]
    slots = []
    for group in groups:
        group.mkdir()
        for index in range(8):
            path = group / f'slot-{index:02d}'
            path.write_bytes(b'initial')
            slots.append(path)
    # Put the event workload on top of a known, large baseline before fanotify
    # starts. This isolates accounting behavior from fixture construction.
    print(f"PROGRESS churn baseline scan start: {args.seed_files} files", flush=True)
    case.scan()
    print("PROGRESS churn baseline scan complete; starting event watcher", flush=True)
    daemon = Daemon(case)
    try:
        daemon.exact()
        print("PROGRESS initial exact validation passed; beginning churn batches", flush=True)
        case.history()
        total_batches = (args.churn_operations + args.churn_batch_size - 1) // args.churn_batch_size
        event_start = daemon.record()
        checkpoints = []
        progress_path = args.report.with_name(f"{args.report.stem}-churn-progress.json")
        progress = {
            "suite": "churn",
            "status": "running",
            "candidate_binary_sha256": hashlib.sha256(case.binary.read_bytes()).hexdigest(),
            "report_path": str(args.report),
            "seed_files": args.seed_files,
            "operations": args.churn_operations,
            "operations_per_batch": args.churn_batch_size,
            "total_batches": total_batches,
            "event_counters_at_start": event_start,
            "completed_batches": [],
            "completed_operations": 0,
        }
        publish_churn_progress(progress_path, progress)
        recent_operations = collections.deque(maxlen=128)
        for batch in range(total_batches):
            operations = min(args.churn_batch_size, args.churn_operations - batch * args.churn_batch_size)

            def mutate_batch():
                for step in range(operations):
                    path = randomizer.choice(slots)
                    operation = randomizer.randrange(6)
                    payload = bytes([step % 251]) * randomizer.choice((0, 1, 4096, 8192))
                    target = None
                    action = ("atomic_replace", "create_delete_reuse", "rename_overwrite",
                              "hardlink_write_unlink", "in_place_write", "transient_create_delete")[operation]
                    if operation == 0:  # atomic replacement over an existing pathname
                        temporary = path.with_name(f'.{path.name}.{batch}.{step}.tmp')
                        temporary.write_bytes(payload)
                        temporary.replace(path)
                    elif operation == 1:  # create/delete and rapid path reuse
                        path.unlink(missing_ok=True)
                        path.write_bytes(payload)
                        path.unlink()
                    elif operation == 2:  # rename-overwrite
                        target = randomizer.choice(slots)
                        if target != path and path.exists():
                            path.replace(target)
                    elif operation == 3:  # hardlink growth through the alias, then unlink
                        alias = path.with_name(f'.{path.name}.alias')
                        alias.unlink(missing_ok=True)
                        if not path.exists():
                            path.write_bytes(payload)
                        os.link(path, alias)
                        alias.write_bytes(payload)
                        alias.unlink()
                    elif operation == 4:  # in-place truncation/growth
                        path.write_bytes(payload)
                    else:  # transient temp file
                        transient = path.with_name(f'.{path.name}.{batch}.{step}.transient')
                        transient.write_bytes(payload)
                        transient.unlink()
                    recent_operations.append({"batch": batch + 1, "step": step + 1,
                                              "kind": action,
                                              "path": str(path.relative_to(case.tree)),
                                              "target": str(target.relative_to(case.tree)) if target else None,
                                              "path_inode": (path.stat().st_ino if path.exists() else None),
                                              "path_blocks": (path.stat().st_blocks * 512 if path.exists() else None)})

            # Force a full reconciliation while a concurrent mutation batch is
            # in flight; the queue must replay onto the installed generation.
            if batch == total_batches // 2:
                thread_errors = []
                def run_concurrent_batch():
                    try:
                        mutate_batch()
                    except BaseException as error:
                        thread_errors.append(error)
                writer = threading.Thread(target=run_concurrent_batch)
                writer.start()
                daemon.process.send_signal(signal.SIGHUP)
                writer.join()
                assert not thread_errors, thread_errors
            else:
                mutate_batch()

            try:
                print(f"PROGRESS churn batch {batch+1}/{total_batches}: mutations complete; checking exact state",
                      flush=True)
                daemon.exact(timeout=180)
                check_index(case.db, case.tree)
            except BaseException as error:
                raise AssertionError(f"churn exactness failed after batch {batch + 1}, "
                                     f"operation {min(args.churn_operations, (batch + 1) * args.churn_batch_size)}; "
                                     f"recent operations={list(recent_operations)!r}; cause={error}") from error
            case.history()
            record = daemon.record()
            assert record['dropped'] == event_start['dropped'], record
            assert not record['uncertain'], record
            with contextlib.closing(snapshot(case.db)) as conn:
                growth_rows = conn.execute('SELECT COUNT(*) FROM growth').fetchone()[0]
            checkpoints.append({'batch': batch + 1, 'operations': min(args.churn_operations, (batch + 1) * args.churn_batch_size),
                                'seen': record['seen'], 'resolved': record['resolved'],
                                'committed': record['committed'], 'growth_rows': growth_rows,
                                'dropped': record['dropped'], 'uncertain': record['uncertain'],
                                'exact_convergence_ms': getattr(daemon, 'last_exact_convergence_ms', None),
                                'exact_paths_inodes_allocations_rollups': True})
            progress.update(
                completed_batches=checkpoints,
                completed_operations=min(args.churn_operations, (batch + 1) * args.churn_batch_size),
                last_event_counters=record,
                last_history_check_passed=True,
                last_exact_check_passed=True,
            )
            publish_churn_progress(progress_path, progress)
            print(f'PASS production-shaped churn batch {batch+1}/{total_batches}', flush=True)

        daemon.exact(timeout=180)
        case.history()
        final = daemon.record()
        assert final['dropped'] == event_start['dropped'], final
        daemon.stop()
        integrity(case.db)
        evidence.update(seed_files=args.seed_files, operations=args.churn_operations,
                        operations_per_batch=args.churn_batch_size,
                        concurrent_rebuild_during_churn=True, batches=checkpoints,
                        exact_paths_inodes_allocations_all_rollups=True,
                        history_continuity=True, kernel_events_seen=final['seen']-event_start['seen'],
                        updates_committed=final['committed']-event_start['committed'], dropped_events=0)
        progress.update(
            status='passed',
            final_exact_passed=True,
            final_history_passed=True,
            sqlite_integrity='ok',
            final_event_counters=final,
            kernel_events_seen=final['seen'] - event_start['seen'],
            updates_committed=final['committed'] - event_start['committed'],
            dropped_events=0,
        )
        publish_churn_progress(progress_path, progress)
        print(f"PASS {args.churn_operations} operations over {args.seed_files} baseline files with exact reconciliation", flush=True)
    except BaseException as error:
        if 'progress_path' in locals():
            progress.update(status='failed', failure=repr(error))
            publish_churn_progress(progress_path, progress)
        raise
    finally:
        daemon.stop()


def allocate_until_full(path):
    """Reserve blocks only on the private image; no host filesystem filling."""
    offset = 0
    with path.open("wb") as file:
        for chunk in (8*MIB, MIB, 4096):
            while True:
                try:
                    os.posix_fallocate(file.fileno(), offset, chunk)
                    offset += chunk
                except OSError as error:
                    if error.errno != 28:
                        raise
                    break
        os.fsync(file.fileno())
    return offset


def disk_tests(case, _args, evidence):
    filler = case.root / "filler"
    daemon = Daemon(case)
    daemon.exact()
    available = os.statvfs(case.root).f_bavail * os.statvfs(case.root).f_frsize
    with filler.open("wb") as file:
        os.posix_fallocate(file.fileno(), 0, available - 128*MIB)
    daemon.wait(lambda: "paused_since" in check_index(case.db)[0], "low-disk pause")
    for i in range(64):
        (case.tree / f"paused-{i}").write_bytes(b"retained during pause")
    daemon.wait(lambda: daemon.record()["pending"] > 0, "retained changes during disk pause")
    filler.unlink()
    daemon.exact()
    case.history()
    daemon.stop()
    # Keep a reader established before filling: even WAL read-only connections
    # may need a sidecar when opening for the first time on a completely full FS.
    reader = snapshot(case.db)
    previous = reader.execute("SELECT COUNT(*) FROM inodes").fetchone()
    try:
        reserved = allocate_until_full(filler)
        failed = case.scan(check=False)
        assert failed.returncode != 0, "scan unexpectedly succeeded on a full test filesystem"
        assert reader.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert reader.execute("SELECT COUNT(*) FROM inodes").fetchone() == previous
    finally:
        filler.unlink(missing_ok=True)
        reader.close()
    integrity(case.db)
    case.history()
    command("mount", "-o", "remount,ro", case.root)
    try:
        failed = case.scan(check=False)
        assert failed.returncode != 0, "scan unexpectedly wrote a read-only filesystem"
        check_index(case.db, case.tree, immutable=True)
    finally:
        command("mount", "-o", "remount,rw", case.root)
    recovered = Daemon(case)
    recovered.exact()
    recovered.stop()
    integrity(case.db)
    evidence.update(low_disk_pause_and_resume=True, full_disk_scan_kept_index=True, read_only_scan_kept_index=True, reserved_bytes=reserved)
    print("PASS low-disk pause/resume, actual ENOSPC, read-only failure, retained readable index", flush=True)


def coverage_test(case, _args, evidence):
    # Drop privileges in the child only; root/CAP_DAC_* would bypass mode 000.
    # The worker tmpfs is deliberately mounted mode 0700. Temporarily add only
    # execute permission for other users to its root so uid 65534 can traverse
    # the workspace. Keep the workspace itself private again after this test.
    workspace_parent = case.base.parent
    parent_mode = stat.S_IMODE(workspace_parent.stat().st_mode)
    base_mode = stat.S_IMODE(case.base.stat().st_mode)
    denied = None
    try:
        workspace_parent.chmod(parent_mode | stat.S_IXOTH)
        case.base.chmod(0o755)
        destination = case.root / "unprivileged"
        destination.mkdir(mode=0o777)
        destination.chmod(0o777)
        denied = case.tree / "denied"
        denied.mkdir()
        (denied / "hidden").write_bytes(b"must not silently disappear")
        denied.chmod(0)
        db = destination / "index.db"
        # Execute from the test ext4 image. The worker tmpfs may be mounted
        # noexec, and must keep that policy. This fixture uses its own
        # executable filesystem and a traversable world-writable parent.
        unprivileged_binary = destination / "dux-unprivileged-scan"
        shutil.copyfile(case.binary, unprivileged_binary)
        unprivileged_binary.chmod(0o755)
        result = subprocess.run([str(unprivileged_binary), "--db", str(db), "scan", str(case.tree), "--quiet"],
                                capture_output=True, text=True, timeout=90, env=case.env,
                                user=65534, group=65534, extra_groups=[])
        assert result.returncode == 0, result.stderr
        meta, _ = check_index(db)
        assert "dirty_since" in meta, "inaccessible directory was silently reported as a clean scan"
        assert "unreadable" in meta.get("dirty_reason", ""), meta
    finally:
        if denied is not None and denied.exists():
            denied.chmod(0o755)
        case.base.chmod(base_mode)
        workspace_parent.chmod(parent_mode)
    case.scan()
    check_index(case.db, case.tree)
    case.history()
    evidence.update(unprivileged_scan_reports_incomplete_coverage=True)
    print("PASS unreadable subtree produces explicit incomplete-coverage state", flush=True)


def soak_test(case, args, evidence):
    randomizer = random.Random(args.seed)
    groups = [case.tree / f'soak-group-{index}' for index in range(4)]
    for group in groups:
        group.mkdir()
    daemon = Daemon(case)
    daemon.exact()
    started = time.monotonic()
    started_at = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    deadline = started + args.duration_seconds
    count, samples, crashes, readers, bursts = 0, [], [], [], 0
    exact_convergence_ms = []
    reader = None
    reader_count, reader_release = None, 0.0
    next_burst = started + 3600
    next_reader = started + 6 * 3600
    next_crash = started + 12 * 3600
    next_checkpoint = started + 60
    recent_operations = collections.deque(maxlen=64)
    progress_path = args.report.with_name('soak-progress.json')
    candidate_binary_sha256 = hashlib.sha256(case.binary.read_bytes()).hexdigest()
    final_checks = {'exact': False, 'history': False, 'sqlite_integrity': None}

    def checkpoint(status):
        progress = {'status': status, 'started_at': started_at,
                    'elapsed_seconds': round(time.monotonic() - started, 1), 'operations': count,
                    'controlled_crashes': crashes, 'held_readers': readers, 'mutation_bursts': bursts,
                    'candidate_binary_sha256': candidate_binary_sha256,
                    'exact_check_interval_operations': args.soak_check_interval,
                    'final_exact_passed': final_checks['exact'],
                    'final_history_passed': final_checks['history'],
                    'sqlite_integrity': final_checks['sqlite_integrity'],
                    'samples': samples, 'recent_operations': list(recent_operations)}
        temporary = progress_path.with_suffix('.json.tmp')
        temporary.write_text(json.dumps(progress, indent=2) + '\n')
        temporary.replace(progress_path)

    checkpoint('running')
    while time.monotonic() < deadline:
        now = time.monotonic()
        if now >= next_burst:
            group = groups[bursts % len(groups)]
            for item in range(500):
                path = group / f'burst-{bursts:04d}-{item:04d}'
                temporary = group / f'.burst-{bursts:04d}-{item:04d}.tmp'
                temporary.write_bytes(b'atomic burst')
                temporary.replace(path)
                if item % 2 == 0:
                    path.unlink()
            bursts += 1
            count += 500
            daemon.exact()
            case.history()
            checkpoint('running')
            next_burst += 3600
        if now >= next_reader and reader is None:
            reader = snapshot(case.db)
            reader_count = reader.execute('SELECT COUNT(*) FROM inodes').fetchone()[0]
            reader_release = now + 300
            next_reader += 6 * 3600
        if reader is not None and now >= reader_release:
            assert reader.execute('SELECT COUNT(*) FROM inodes').fetchone()[0] == reader_count
            reader.close()
            reader = None
            daemon.exact()
            with contextlib.closing(sqlite3.connect(case.db)) as connection:
                checkpoint_result = connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
            assert checkpoint_result[0] == 0, checkpoint_result
            wal = Path(str(case.db) + '-wal')
            assert not wal.exists() or wal.stat().st_size == 0, 'WAL did not reclaim after endurance reader released'
            readers.append({'held_seconds': 300, 'snapshot_inodes': reader_count, 'wal_reclaimed': True})
            checkpoint('running')
        if now >= next_crash:
            generation = check_index(case.db)[0]['scan_generation']
            pid = daemon.process.pid
            daemon.kill()
            for index in range(64):
                (groups[index % len(groups)] / f'downtime-{len(crashes):03d}-{index:03d}').write_bytes(b'written while daemon was down')
            daemon = Daemon(case)
            daemon.exact(generation, timeout=150)
            case.history()
            crashes.append({'pid': pid, 'restart_pid': daemon.process.pid, 'downtime_paths': 64})
            checkpoint('running')
            next_crash += 12 * 3600
        group = groups[randomizer.randrange(len(groups))]
        path = group / f"soak-{randomizer.randrange(128)}"
        operation = randomizer.randrange(4)
        operation_name = ('delete', 'rename', 'atomic_replace', 'write_or_hardlink')[operation]
        target = None
        if operation == 0:
            path.unlink(missing_ok=True)
        elif operation == 1 and path.exists():
            target = groups[randomizer.randrange(len(groups))] / f"soak-{randomizer.randrange(128)}"
            path.replace(target)
        elif operation == 2:
            temporary = group / f'.{path.name}-{count}.tmp'
            temporary.write_bytes(bytes([count % 256]) * randomizer.choice([0, 4096, 8192]))
            temporary.replace(path)
        elif count % 211 == 0 and path.exists():
            operation_name = 'hardlink_create'
            link = group / f'hardlink-{count:09d}'
            link.unlink(missing_ok=True)
            os.link(path, link)
        else:
            operation_name = 'write'
            path.write_bytes(bytes([count % 256]) * randomizer.choice([0, 4096, 8192]))
        count += 1
        recent_operations.append({'operation': count, 'kind': operation_name,
                                  'path': str(path.relative_to(case.tree)),
                                  'target': str(target.relative_to(case.tree)) if target else None})
        if count % args.soak_check_interval == 0:
            try:
                daemon.exact(timeout=180)
                case.history()
                exact_convergence_ms.append(daemon.last_exact_convergence_ms)
                pid = daemon.process.pid
                status = dict(line.split(":", 1) for line in Path(f"/proc/{pid}/status").read_text().splitlines() if ":" in line)
                samples.append({"operations": count, "rss_kib": int(status["VmRSS"].split()[0]), "fds": len(list(Path(f"/proc/{pid}/fd").iterdir())), "wal_bytes": Path(str(case.db)+"-wal").stat().st_size if Path(str(case.db)+"-wal").exists() else 0, "exact_convergence_ms": daemon.last_exact_convergence_ms})
            except BaseException as error:
                raise AssertionError(f"exactness failed after operation {count}; recent operations={list(recent_operations)!r}; cause={error}") from error
        if time.monotonic() >= next_checkpoint:
            checkpoint('running')
            next_checkpoint = time.monotonic() + 60
        # The configured rate is operations per second. Sleeping only once in
        # each 100-operation exactness batch multiplies the actual mutation
        # rate by roughly 100 and makes endurance reports misleading. Pace each
        # mutation so the requested workload is reproducible and comparable.
        time.sleep(1 / args.operations_per_second)
    daemon.exact(timeout=180)
    final_checks['exact'] = True
    case.history()
    final_checks['history'] = True
    if reader is not None:
        reader.close()
        reader = None
        with contextlib.closing(sqlite3.connect(case.db)) as connection:
            checkpoint_result = connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
        assert checkpoint_result[0] == 0, checkpoint_result
        readers.append({'held_seconds': round(time.monotonic() - (reader_release - 300), 3),
                        'snapshot_inodes': reader_count, 'wal_reclaimed': True})
    daemon.stop()
    integrity(case.db)
    final_checks['sqlite_integrity'] = 'ok'
    evidence.update(duration_seconds=args.duration_seconds, operations=count, samples=samples,
                    exact_convergence_ms=exact_convergence_ms,
                    max_exact_convergence_ms=max(exact_convergence_ms, default=0),
                    exact_check_interval_operations=args.soak_check_interval,
                    controlled_crashes=crashes, held_readers=readers, mutation_bursts=bursts)
    checkpoint('passed')
    print(f"PASS bounded soak: {count} operations over {args.duration_seconds}s, {len(crashes)} restarts, {len(readers)} reader holds", flush=True)


class ManagedProcess:
    def __init__(self, unit):
        self.unit = unit

    @property
    def pid(self):
        return int(command('systemctl', 'show', self.unit, '-p', 'MainPID', '--value').stdout.strip())

    def poll(self):
        state = command('systemctl', 'show', self.unit, '-p', 'ActiveState', '--value').stdout.strip()
        return None if state in {'active', 'activating', 'reloading'} else 1

    def send_signal(self, signum):
        command('systemctl', 'kill', f'--signal={signal.Signals(signum).name}', self.unit)


class ManagedDaemon(Daemon):
    def __init__(self, case, purpose):
        self.case, self.binary = case, case.binary
        self.unit = f'dux-recovery-{purpose}-{os.getpid()}'
        self.process = ManagedProcess(self.unit)
        self.marker = case.base / 'unused.marker'
        self.log = (case.base / 'daemon.log').open('a')
        command('systemd-run', f'--unit={self.unit}', '--collect', '--property=Restart=on-failure',
                '--property=RestartSec=2s', '--property=StartLimitIntervalSec=0',
                '--property=MemoryMax=96M', '--property=MemorySwapMax=0', '--property=OOMPolicy=stop',
                '--property=CPUQuota=25%', '--property=TimeoutStopSec=15s', '--property=RuntimeMaxSec=600',
                '--property=StandardOutput=journal', '--property=StandardError=journal',
                f'--setenv=DUX_RUNTIME_DIR={case.runtime}',
                'nsenter', f'--mount=/proc/{os.getpid()}/ns/mnt',
                case.binary, '--db', case.db, 'daemon', case.tree, '--flush-ms', '100', '--max-cpu', '50')
        case.children.append(self)

    def stop(self):
        subprocess.run(['systemctl', 'stop', self.unit], capture_output=True, timeout=25)
        result = subprocess.run(['journalctl', '-u', self.unit, '--no-pager', '-o', 'cat'],
                                capture_output=True, text=True)
        if result.returncode == 0 and result.stdout:
            with (self.case.base / 'daemon.log').open('a') as stream:
                stream.write(result.stdout)
        self.log.close()

    def restarts(self):
        return int(command('systemctl', 'show', self.unit, '-p', 'NRestarts', '--value').stdout.strip())


def capability_test(case, _args, evidence):
    denied = Daemon(case, prefix=['setpriv', '--bounding-set=-sys_admin'])
    assert denied.process.wait(timeout=30) != 0, 'daemon falsely started without fanotify capability'
    denied.stop()
    before = check_index(case.db)[0]['scan_generation']
    daemon = Daemon(case, prefix=['setpriv', '--bounding-set=-dac_read_search'])
    # Baseline writes can already expose missing handle resolution. Do not
    # require a clean pipeline from a deliberately capability-impaired daemon.
    daemon.wait(lambda: check_index(case.db, case.tree)[0].get('scan_generation') != before,
                'baseline completion with unavailable handle capability')
    for i in range(128):
        (case.tree / f'capability-{i}').write_bytes(b'unresolvable handle')
    daemon.wait(lambda: daemon.record()['uncertain'], 'visible lost path-resolution capability')
    generation = check_index(case.db)[0]['scan_generation']
    daemon.process.send_signal(signal.SIGHUP)
    # A capability-impaired daemon may continuously re-enter reconciliation
    # while preserving the dirty state. On the 100k-file profile under the
    # worker's 25% CPU quota, that rebuild can take longer than the generic
    # 60s polling default even though the daemon has started it correctly.
    daemon.wait(lambda: check_index(case.db)[0].get('scan_generation') != generation,
                'rebuild with missing capability', timeout=180)
    # A successful baseline cannot restore an unavailable event-resolution capability.
    time.sleep(0.5)
    assert daemon.record()['uncertain'], 'rebuild hid a known broken event pipeline'
    daemon.stop()
    recovered = Daemon(case)
    recovered.exact()
    recovered.stop()
    integrity(case.db)
    case.history()
    evidence.update(missing_sys_admin_fails_closed=True, missing_handle_capability_visible=True,
                    rebuild_keeps_known_capability_failure_visible=True, restored_capability_converged=True)
    print('PASS capability failure, persistent uncertainty across rebuild, restored-capability recovery', flush=True)


def oom_test(case, _args, evidence):
    daemon = ManagedDaemon(case, 'oom')
    before = daemon.exact()['scan_generation']
    pid = daemon.process.pid
    count = daemon.restarts()
    # Make OOM deterministic: an idle daemon may sit under a lowered memory.max
    # without allocating. Force a real reconciliation of a sizable fixture so
    # its own bounded scan allocations cross the kernel-enforced cgroup limit.
    stress = case.tree / 'oom-scan-stress'
    stress.mkdir()
    for index in range(10000):
        (stress / f'entry-{index:05d}').touch()
    group = command('systemctl', 'show', daemon.unit, '-p', 'ControlGroup', '--value').stdout.strip()
    assert group.startswith('/system.slice/dux-recovery-oom-'), group
    events_path = Path('/sys/fs/cgroup') / group.lstrip('/') / 'memory.events'
    def events():
        return {key: int(value) for key, value in (line.split() for line in events_path.read_text().splitlines())}
    previous = events()
    try:
        command('systemctl', 'set-property', '--runtime', daemon.unit, 'MemoryMax=1M')
        daemon.process.send_signal(signal.SIGHUP)
        deadline = time.monotonic()+15
        while time.monotonic() < deadline:
            if not Path(f'/proc/{pid}').exists():
                break
            time.sleep(0.05)
        assert not Path(f'/proc/{pid}').exists(), 'cgroup pressure did not kill the test daemon'
        try:
            observed = events()
            assert observed.get('oom_kill', 0) > previous.get('oom_kill', 0), observed
        except FileNotFoundError:
            # systemd may remove an empty cgroup before the restart delay ends.
            result = command('systemctl', 'show', daemon.unit, '-p', 'Result', '--value').stdout.strip()
            assert result == 'oom-kill', result
            observed = {'systemd_result': result}
        for i in range(32):
            (case.tree / f'oom-downtime-{i}').write_bytes(b'changed during real OOM')
    finally:
        command('systemctl', 'set-property', '--runtime', daemon.unit, 'MemoryMax=96M')
    daemon.wait(lambda: daemon.restarts() > count and daemon.process.pid != pid, 'automatic systemd restart after OOM')
    daemon.exact(before, timeout=150)
    case.history()
    restarts = daemon.restarts()-count
    daemon.stop()
    integrity(case.db)
    evidence.update(actual_cgroup_oom_kill=True, automatic_restarts=restarts,
                    downtime_changes_recovered=True, previous_memory_events=previous, oom_memory_events=observed)
    print('PASS real scoped cgroup OOM, automatic restart and exact downtime recovery', flush=True)


def pressure_reconciliation_test(case, _args, evidence):
    assert case.fault_binary, "pressure recovery qualification requires the fault binary"
    with contextlib.closing(snapshot(case.db)) as conn:
        before = dict(conn.execute("SELECT key,value FROM meta"))
    elevated_file = case.base / "force-elevated-pressure"
    high_load_elevated_file = case.base / "force-high-load-elevated-pressure"
    cpu_elevated_file = case.base / "force-cpu-elevated-pressure"
    critical_file = case.base / "force-critical-pressure"
    elevated_file.unlink(missing_ok=True)
    high_load_elevated_file.unlink(missing_ok=True)
    cpu_elevated_file.unlink(missing_ok=True)
    critical_file.unlink(missing_ok=True)
    case.env["DUX_TEST_ELEVATED_PRESSURE_FILE"] = str(elevated_file)
    case.env["DUX_TEST_HIGH_LOAD_ELEVATED_PRESSURE_FILE"] = str(high_load_elevated_file)
    case.env["DUX_TEST_CPU_ELEVATED_PRESSURE_FILE"] = str(cpu_elevated_file)
    case.env["DUX_TEST_CRITICAL_PRESSURE_FILE"] = str(critical_file)
    daemon = Daemon(case, case.fault_binary)
    try:
        daemon.exact()
        # Starting against a pre-existing index deliberately triggers a startup
        # reconciliation. Capture the generation only after that baseline is
        # fully established so the assertion below checks that the *cancelled*
        # scan preserves the current live generation, not the stale pre-start
        # generation.
        with contextlib.closing(snapshot(case.db)) as conn:
            previous_generation = dict(conn.execute("SELECT key,value FROM meta"))["scan_generation"]
        daemon.process.send_signal(signal.SIGHUP)
        progress = runtime_path(case.db, case.runtime, "scan.progress")
        staging_files = [
            Path(f"{case.db}.new{suffix}")
            for suffix in ("", "-wal", "-shm", "-journal")
        ]
        daemon.wait(
            lambda: progress.exists() and staging_files[0].exists(),
            "background reconciliation staging database",
            timeout=30,
        )
        # Elevated high load must cancel full-root work while incremental
        # filesystem events continue. Elevated I/O pressure is tested below as
        # a separate band in which an already-running scan should keep going.
        log_path = case.base / "daemon.log"
        elevated_log_offset = log_path.stat().st_size if log_path.exists() else 0
        high_load_elevated_file.touch()

        def cancelled():
            log = log_path.read_bytes()[elevated_log_offset:].decode(errors="replace")
            if "cancelling background reconciliation under elevated or critical host pressure" not in log:
                return False
            if any(path.exists() for path in staging_files):
                return False
            with contextlib.closing(snapshot(case.db)) as conn:
                meta = dict(conn.execute("SELECT key,value FROM meta"))
            if meta.get("dirty_reason") != "reconciliation cancelled while host pressure was elevated":
                return False
            assert meta.get("scan_generation") == previous_generation, meta
            return meta

        cancelled_meta = daemon.wait(
            cancelled, "elevated-pressure cancellation preserving old generation", timeout=30
        )

        elevated_path = case.tree / "created-under-elevated-pressure"
        committed_before_elevated = daemon.record()["committed"]
        elevated_path.write_bytes(b"incremental update continues while elevated")

        def elevated_increment_committed():
            status = daemon.record()
            if status["pending"] or not status["drained"] or status["committed"] <= committed_before_elevated:
                return False
            check_index(case.db, case.tree)
            return status

        elevated_status = daemon.wait(
            elevated_increment_committed,
            "incremental event commit under Elevated pressure",
            timeout=30,
        )
        assert not any(path.exists() for path in staging_files), (
            "full reconciliation restarted while Elevated pressure remained"
        )

        # Critical pressure still pauses writes. Confirm the event remains
        # queued and uncommitted while the Elevated marker remains active.
        critical_log_offset = log_path.stat().st_size
        critical_file.touch()
        daemon.wait(
            lambda: "pausing index writes to protect it" in
            log_path.read_bytes()[critical_log_offset:].decode(errors="replace"),
            "Critical-pressure write pause",
            timeout=15,
        )
        critical_path = case.tree / "created-under-critical-pressure"
        committed_before_critical = daemon.record()["committed"]
        critical_path.write_bytes(b"retained through Critical pressure pause")
        daemon.wait(
            lambda: daemon.record()["pending"] > 0,
            "event retained during Critical pressure pause",
            timeout=15,
        )
        time.sleep(0.5)
        critical_status = daemon.record()
        assert critical_status["pending"] > 0, critical_status
        assert critical_status["committed"] == committed_before_critical, critical_status
        parent_stat = critical_path.parent.stat()
        critical_stat = critical_path.lstat()
        with contextlib.closing(snapshot(case.db)) as conn:
            indexed_critical_path = conn.execute(
                "SELECT 1 FROM dirents WHERE parent_dev=? AND parent_inode=? AND name=? AND dev_id=? AND inode=?",
                (parent_stat.st_dev, parent_stat.st_ino, os.fsencode(critical_path.name),
                 critical_stat.st_dev, critical_stat.st_ino),
            ).fetchone()
        assert indexed_critical_path is None, indexed_critical_path
        assert not any(path.exists() for path in staging_files), (
            "full reconciliation restarted while pressure remained elevated"
        )

        critical_file.unlink()
        def elevated_replay_complete():
            status = daemon.record()
            if status["pending"] or not status["drained"] or status["committed"] <= committed_before_critical:
                return False
            check_index(case.db, case.tree)
            return status

        critical_replay_status = daemon.wait(
            elevated_replay_complete,
            "retained event replay after Critical pressure recedes to Elevated",
            timeout=30,
        )
        assert not any(path.exists() for path in staging_files), (
            "full reconciliation restarted while Elevated pressure remained"
        )

        # With memory and load pressure clear, CPU-only Elevated pressure must
        # still permit the bounded recovery scan. This covers the live-host
        # starvation mode where ordinary CPU contention otherwise keeps an
        # uncertain index stale indefinitely.
        log_path = case.base / "daemon.log"
        cpu_recovery_log_offset = log_path.stat().st_size
        cpu_elevated_file.touch()
        high_load_elevated_file.unlink()
        cpu_reconciliation_started = daemon.wait(
            lambda: "starting background reconciliation of " in
            log_path.read_bytes()[cpu_recovery_log_offset:].decode(errors="replace"),
            "reconciliation start under CPU-only Elevated pressure",
            timeout=90,
        )
        # Under a constrained CPU quota, this 100k-file rebuild can take
        # several minutes after the forced cancellations. Keep the recovery
        # deadline generous enough to cover the scan plus two exact snapshots;
        # otherwise a valid but still-running reconciliation is misreported as
        # an uncertain-pipeline defect at the deadline boundary.
        recovered = daemon.exact(previous_generation=previous_generation, timeout=600)
        assert cpu_elevated_file.exists(), "CPU-only Elevated pressure ended before reconciliation completed"
        status = daemon.record()
        assert recovered["scan_generation"] != previous_generation
        assert elevated_path.read_bytes() == b"incremental update continues while elevated"
        assert critical_path.read_bytes() == b"retained through Critical pressure pause"
        case.history()

        # Moderate I/O PSI can be caused by the low-priority scan itself.
        # Cancelling on that signal discards useful work and can create a
        # repeated cancel/retry loop, so prove the scan completes in that band.
        cpu_elevated_file.unlink()
        io_baseline = recovered["scan_generation"]
        io_log_offset = log_path.stat().st_size
        assert not any(path.exists() for path in staging_files), (
            "staging database survived the completed CPU-pressure reconciliation"
        )
        # Use the daemon's normal restart recovery path to start exactly one
        # fresh rebuild. Sending SIGHUP while a startup retry is already queued
        # can leave a second request deferred under Elevated I/O and make the
        # test wait for work that policy intentionally refuses to start.
        daemon.stop()
        daemon = Daemon(case, case.fault_binary)
        daemon.wait(
            lambda: progress.exists() and staging_files[0].exists(),
            "startup reconciliation staging database before Elevated I/O",
            timeout=30,
        )
        elevated_file.touch()
        def io_scan_completed():
            log = log_path.read_bytes()[io_log_offset:].decode(errors="replace")
            if "cancelling background reconciliation under elevated or critical host pressure" in log:
                raise AssertionError("active reconciliation was cancelled under Elevated I/O pressure")
            if "rescan complete:" not in log:
                return False
            with contextlib.closing(snapshot(case.db)) as conn:
                meta = dict(conn.execute("SELECT key,value FROM meta"))
            if meta.get("scan_generation") == io_baseline:
                return False
            return meta

        io_scan_meta = daemon.wait(
            io_scan_completed,
            "active startup reconciliation completes under Elevated I/O pressure",
            timeout=120,
        )
        assert elevated_file.exists(), "Elevated I/O pressure ended before reconciliation completed"
        # The worker now has no second queued scan; require the post-rebuild
        # accounting check and exact tree comparison while the injected I/O
        # pressure remains active.
        io_recovered = daemon.exact(previous_generation=io_baseline, timeout=600)
        assert io_scan_meta["scan_generation"] != io_baseline
        assert io_recovered["scan_generation"] != io_baseline
        case.history()
        status = daemon.record()
        evidence.update(
            elevated_pressure_cancelled_rebuild=True,
            old_generation_preserved=previous_generation,
            cancelled_dirty_reason=cancelled_meta["dirty_reason"],
            elevated_incremental_update_committed=True,
            critical_pressure_paused_write=True,
            critical_event_retained=True,
            critical_event_replayed_after_pressure_receded=True,
            cpu_only_elevated_reconciliation_started=bool(cpu_reconciliation_started),
            cpu_only_elevated_reconciliation_completed=True,
            elevated_io_reconciliation_started=True,
            elevated_io_reconciliation_completed=True,
            elevated_io_recovered_generation=io_recovered["scan_generation"],
            elevated_committed_updates=elevated_status["committed"],
            post_critical_committed_updates=critical_replay_status["committed"],
            recovered_generation=recovered["scan_generation"],
            zero_pending=status["pending"] == 0,
            zero_dropped=status["dropped"] == 0,
            exact_recovery=True,
            growth_history_preserved=True,
        )
        print("PASS Elevated cancellation/incremental updates, Critical pause, and exact recovery", flush=True)
    finally:
        # This suite runs a daemon specifically to test pressure-triggered
        # cancellation. Always release its database flock before the next suite
        # starts a daemon of its own; otherwise the following systemd-managed
        # mount-recovery service can restart repeatedly against the leaked lock.
        daemon.stop()
        elevated_file.unlink(missing_ok=True)
        high_load_elevated_file.unlink(missing_ok=True)
        cpu_elevated_file.unlink(missing_ok=True)
        critical_file.unlink(missing_ok=True)
        case.env.pop("DUX_TEST_ELEVATED_PRESSURE_FILE", None)
        case.env.pop("DUX_TEST_HIGH_LOAD_ELEVATED_PRESSURE_FILE", None)
        case.env.pop("DUX_TEST_CPU_ELEVATED_PRESSURE_FILE", None)
        case.env.pop("DUX_TEST_CRITICAL_PRESSURE_FILE", None)


def mount_test(case, _args, evidence):
    mountpoint = case.tree / 'mounted'
    mountpoint.mkdir()
    image = case.base / 'extra-filesystem.img'
    with image.open('wb') as stream:
        stream.truncate(64*MIB)
    command('mkfs.ext4', '-F', '-q', image)
    daemon = ManagedDaemon(case, 'mount')
    before = daemon.exact()['scan_generation']
    count = daemon.restarts()
    mounted = False
    try:
        command('mount', '-o', 'loop,nosuid,nodev', image, mountpoint)
        mounted = True
        (mountpoint / 'new-filesystem-file').write_bytes(b'initial mount coverage')
        daemon.wait(lambda: daemon.restarts() > count, 'automatic restart for newly mounted filesystem', timeout=35)
        daemon.exact(before, timeout=150)
        # Prove continuing events are watched, rather than only a fresh baseline.
        (mountpoint / 'after-baseline').write_bytes(b'covered by a new fanotify mark')
        daemon.exact()
        before = check_index(case.db)[0]['scan_generation']
        count = daemon.restarts()
        command('umount', '-l', mountpoint)
        mounted = False
        (mountpoint / 'underlying-filesystem').write_bytes(b'mount disappeared')
        daemon.wait(lambda: daemon.restarts() > count, 'automatic restart after unavailable mount', timeout=35)
        daemon.exact(before, timeout=150)
        case.history()
    finally:
        daemon.stop()
        if mounted:
            command('umount', mountpoint)
    integrity(case.db)
    evidence.update(new_mount_detected_and_watched=True, disappearing_mount_recovered=True)
    print('PASS new and unavailable mount detection, refreshed watch coverage and exact recovery', flush=True)


def wal_test(case, _args, evidence):
    daemon = Daemon(case)
    daemon.exact()
    reader = snapshot(case.db)
    old = reader.execute('SELECT SUM(blocks),COUNT(*) FROM inodes').fetchone()
    wal = Path(str(case.db)+'-wal')
    try:
        for round_number in range(12):
            for i in range(48):
                (case.tree / f'long-reader-{i}').write_bytes(bytes([round_number])*4096*(1+round_number%4))
            daemon.exact()
        peak = wal.stat().st_size
        assert reader.execute('SELECT SUM(blocks),COUNT(*) FROM inodes').fetchone() == old
        assert reader.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
    finally:
        reader.close()
    daemon.stop()
    # Quiescent SQLite can reclaim the retained WAL after the reader releases.
    with contextlib.closing(sqlite3.connect(case.db)) as connection:
        checkpoint = connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
    assert checkpoint[0] == 0, checkpoint
    final = wal.stat().st_size if wal.exists() else 0
    assert final == 0, f'WAL did not reclaim after reader released: {final}'
    integrity(case.db)
    case.history()
    evidence.update(reader_snapshot_survived=True, peak_wal_bytes=peak, quiescent_wal_bytes=final)
    print('PASS long reader keeps a consistent snapshot and WAL reclaims after release', flush=True)


def startup_test(case, args, evidence):
    assert case.fault_binary, "startup qualification requires the explicitly enabled fault binary"
    fresh_base=case.base/'fresh-start'
    fresh_root=case.root/'fresh-start'
    fresh_base.mkdir()
    fresh_root.mkdir()
    fresh=Case(fresh_base,fresh_root,case.runtime,case.binary,case.fault_binary)
    for i in range(128):
        (fresh.tree/f'initial-{i}').write_bytes(b'initial baseline')
    def verify_marks(daemon):
        paths=Path(f'/proc/{daemon.process.pid}/fd').iterdir()
        assert any('fanotify' in os.readlink(path) for path in paths), "initial scan started before filesystem watches were attached"
    daemon=Daemon(fresh,case.fault_binary,'scan.build_complete')
    try:
        daemon.paused_at('scan.build_complete')
        verify_marks(daemon)
        for i in range(64):
            (fresh.tree/f'during-first-scan-{i}').write_bytes(b'event after walk, before install')
        daemon.process.send_signal(signal.SIGCONT)
        daemon.exact()
    finally:
        fresh.close()
    integrity(fresh.db)
    # A failed upgrade scan must leave the legacy generation readable and the
    # daemon alive; it must not try to reopen that old schema as writable and
    # turn one scan error into a restart loop. Clear the fault trigger and let
    # the bounded automatic retry install v6, replaying events collected while
    # the old generation remained read-only.
    with contextlib.closing(sqlite3.connect(fresh.db)) as conn, conn:
        conn.execute("UPDATE meta SET value='4' WHERE key='schema_version'")
    fail_scan_file = fresh.base / 'fail-upgrade-scan'
    fail_scan_file.touch()
    fresh.env.update(
        DUX_TEST_FAIL_SCAN_AT='scan.build_complete',
        DUX_TEST_FAIL_SCAN_FILE=str(fail_scan_file),
    )
    failure_daemon = Daemon(fresh, case.fault_binary)
    retry_event = fresh.tree / 'queued-during-failed-upgrade'
    try:
        failure_daemon.wait(
            lambda: 'upgrade reconciliation failed; legacy index retained and retry scheduled'
            in (fresh.base / 'daemon.log').read_text(errors='replace'),
            'failed legacy-schema upgrade scan with old generation retained',
            timeout=30,
        )
        assert failure_daemon.process.poll() is None, 'daemon exited after upgrade scan failure'
        with contextlib.closing(sqlite3.connect(fresh.db)) as conn:
            legacy_version = conn.execute(
                "SELECT value FROM meta WHERE key='schema_version'"
            ).fetchone()[0]
        assert legacy_version == '4', f'failed upgrade replaced legacy index: {legacy_version}'
        assert failure_daemon.record()['uncertain'], 'failed upgrade was not exposed as uncertain'
        retry_event.write_bytes(b'queued until the legacy upgrade retry succeeds')
        failure_daemon.wait(
            lambda: failure_daemon.record()['pending'] > 0,
            'events queued while legacy generation is read-only',
            timeout=30,
        )
        fail_scan_file.unlink()
        failure_daemon.exact(timeout=150)
        with contextlib.closing(sqlite3.connect(fresh.db)) as conn:
            upgraded_version = conn.execute(
                "SELECT value FROM meta WHERE key='schema_version'"
            ).fetchone()[0]
        assert upgraded_version == '6', f'bounded retry did not install v6: {upgraded_version}'
        assert retry_event.exists(), 'retry fixture event disappeared'
        evidence.update(
            failed_upgrade_scan_kept_legacy_index=True,
            daemon_survived_legacy_upgrade_failure=True,
            queued_events_replayed_after_automatic_retry=True,
            automatic_retry_installed_v6=True,
        )
    finally:
        fail_scan_file.unlink(missing_ok=True)
        fresh.env.pop('DUX_TEST_FAIL_SCAN_AT', None)
        fresh.env.pop('DUX_TEST_FAIL_SCAN_FILE', None)
        failure_daemon.stop()
    integrity(fresh.db)
    # Simulate the previous supported schema, retaining its committed history.
    with contextlib.closing(sqlite3.connect(case.db)) as conn, conn:
        # ALTER TABLE DROP COLUMN needs SQLite 3.35; Debian 12/Rocky 9 system
        # Python may provide 3.34, so build the compatible v4 table explicitly.
        # The v6 view follows table renames; remove it before renaming inodes
        # so it cannot be rewritten to reference the temporary v5 table.
        conn.execute('DROP VIEW IF EXISTS growth_current')
        conn.execute('ALTER TABLE inodes RENAME TO inodes_v5')
        conn.execute("""CREATE TABLE inodes (
            dev_id INTEGER NOT NULL, inode INTEGER NOT NULL, kind TEXT NOT NULL,
            blocks INTEGER NOT NULL, recursive_bytes INTEGER NOT NULL DEFAULT 0,
            recursive_inodes INTEGER NOT NULL DEFAULT 1, uid INTEGER NOT NULL,
            mtime INTEGER NOT NULL, PRIMARY KEY(dev_id,inode)) WITHOUT ROWID""")
        conn.execute("""INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
            SELECT dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime FROM inodes_v5""")
        conn.execute('DROP TABLE inodes_v5')
        conn.executescript('CREATE INDEX idx_inodes_lfiles ON inodes(blocks DESC) WHERE kind<>\'d\'; '
                           'CREATE INDEX idx_inodes_ldirs ON inodes(recursive_bytes DESC) WHERE kind=\'d\'; '
                           'CREATE INDEX idx_inodes_linode ON inodes(recursive_inodes DESC) WHERE kind=\'d\';')
        # Recreate the v4 growth table too. Merely changing schema_version and
        # rebuilding `inodes` leaves the v6 `(bucket,dev,inode,birth_ns)` key in
        # place; collapsing that hybrid table to birth_ns=0 can create duplicate
        # keys after inode reuse and makes this an invalid legacy fixture.
        conn.execute('DROP INDEX IF EXISTS idx_growth_bucket_delta')
        conn.execute('DROP INDEX IF EXISTS idx_growth_inode_bucket_delta')
        conn.execute('ALTER TABLE growth RENAME TO growth_v6')
        conn.execute('''CREATE TABLE growth (
            bucket INTEGER NOT NULL, dev_id INTEGER NOT NULL,
            inode INTEGER NOT NULL, delta INTEGER NOT NULL,
            PRIMARY KEY(bucket,dev_id,inode)
        ) WITHOUT ROWID''')
        conn.execute('''INSERT INTO growth(bucket,dev_id,inode,delta)
            SELECT bucket,dev_id,inode,SUM(delta) FROM growth_v6
            GROUP BY bucket,dev_id,inode''')
        conn.execute('DROP TABLE growth_v6')
        conn.executescript('CREATE INDEX idx_growth_bucket_delta ON growth(bucket,delta DESC,dev_id,inode); '
                           'CREATE INDEX idx_growth_inode_bucket_delta ON growth(dev_id,inode,bucket,delta);')
        conn.execute("UPDATE meta SET value='4' WHERE key='schema_version'")
        # Reproduce the live failure shape on the legacy schema: an inode reused
        # by a regular file still has a stale directory row and child dirent.
        # The migration/startup reconciliation must discard that dead subtree.
        reused = conn.execute("""SELECT i.dev_id,i.inode,i.blocks,d.parent_dev,d.parent_inode,d.name
            FROM inodes i JOIN dirents d USING(dev_id,inode)
            WHERE i.kind='f' AND i.blocks>0 AND d.parent_dev=? AND d.parent_inode=?
              AND (SELECT COUNT(*) FROM dirents x WHERE x.dev_id=i.dev_id AND x.inode=i.inode)=1
            LIMIT 1""", (int(conn.execute("SELECT value FROM meta WHERE key='root_dev'").fetchone()[0]),
                           int(conn.execute("SELECT value FROM meta WHERE key='root_inode'").fetchone()[0]))).fetchone()
        assert reused, 'could not select a single-link root file for stale-inode upgrade fixture'
        dev, ino, blocks, parent_dev, parent_ino, name = reused
        stale_ino = -9_000_000_000_000_000_000
        conn.execute("UPDATE inodes SET kind='d',recursive_bytes=blocks,recursive_inodes=2 WHERE dev_id=? AND inode=?", (dev,ino))
        conn.execute("INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime) VALUES(?,?,'f',4096,4096,1,0,0)", (dev,stale_ino))
        conn.execute("INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?,?,x'7374616c652d6368696c64',?,?,1)", (dev,ino,dev,stale_ino))
        conn.execute("UPDATE inodes SET recursive_bytes=recursive_bytes+4096,recursive_inodes=recursive_inodes+1 WHERE dev_id=? AND inode=?", (parent_dev,parent_ino))
    daemon=Daemon(case,case.fault_binary,'scan.build_complete')
    try:
        daemon.paused_at('scan.build_complete')
        verify_marks(daemon)
        # Queries must still work on the old compatible snapshot during upgrade.
        command(case.binary,'--db',case.db,'metrics','--top','0',env=case.env)
        for i in range(64):
            (case.tree/f'during-upgrade-{i}').write_bytes(b'upgrade window event')
        daemon.process.send_signal(signal.SIGCONT)
        daemon.exact()
        assert check_index(case.db)[0]['schema_version']=='6'
        with contextlib.closing(snapshot(case.db)) as conn:
            assert conn.execute('SELECT 1 FROM inodes WHERE inode=?', (stale_ino,)).fetchone() is None, 'upgrade retained dead inode subtree'
            assert conn.execute('SELECT kind FROM inodes WHERE dev_id=? AND inode=?', (dev,ino)).fetchone() == ('f',), 'reused inode was not restored as a file'

        # Reproduce the live file-to-directory inode-kind mismatch while the
        # candidate watcher is already running. Give it one phantom child, then
        # create a real child to force ensure_ancestors() to upsert the directory
        # from a real fanotify event. The repair must discard the stale subtree
        # and restore the exact full path/allocation graph without a root rescan.
        live_dir = case.tree / 'live-inode-kind-reuse'
        live_dir.mkdir()
        daemon.exact()
        live_stat = live_dir.stat()
        live_dev, live_ino = live_stat.st_dev, live_stat.st_ino
        phantom_ino = -(1 << 62)
        with contextlib.closing(sqlite3.connect(case.db)) as conn, conn:
            conn.execute("UPDATE inodes SET kind='f',recursive_bytes=blocks,recursive_inodes=1 WHERE dev_id=? AND inode=?",
                         (live_dev, live_ino))
            conn.execute("INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns) VALUES(?,?,'f',4096,4096,1,0,0,0)",
                         (live_dev, phantom_ino))
            conn.execute("INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?,?,x'7068616e746f6d',?,?,1)",
                         (live_dev, live_ino, live_dev, phantom_ino))
        (live_dir / 'real-event-child').write_bytes(b'candidate fanotify repair')
        daemon.exact(timeout=30)
        with contextlib.closing(snapshot(case.db)) as conn:
            assert conn.execute('SELECT 1 FROM inodes WHERE dev_id=? AND inode=?',
                                (live_dev, phantom_ino)).fetchone() is None, 'live event retained stale phantom child'
            assert conn.execute('SELECT kind FROM inodes WHERE dev_id=? AND inode=?',
                                (live_dev, live_ino)).fetchone() == ('d',), 'live event did not restore directory kind'
        live_event_repair = True
        case.history()
    finally:
        daemon.stop()
    integrity(case.db)
    evidence.update(first_install_watches_before_scan=True,initial_scan_window_changes_replayed=True,
                    upgrade_watches_before_scan=True,legacy_queries_remain_available=True,
                    upgrade_history_and_window_changes_preserved=True,
                    legacy_stale_directory_inode_repaired=True,
                    live_fanotify_file_to_directory_inode_repair=live_event_repair)
    print('PASS watch-first startup/upgrade and live fanotify file-to-directory inode repair with exact accounting',flush=True)


def reconciliation_retry_test(case, args, evidence):
    """Exercise the persisted retry circuit through the real daemon loop."""
    assert case.fault_binary, "reconciliation retry qualification requires the fault binary"

    def metadata():
        with contextlib.closing(snapshot(case.db)) as connection:
            return dict(connection.execute("SELECT key,value FROM meta"))

    def damage_root_rollup():
        with contextlib.closing(sqlite3.connect(case.db, timeout=5)) as connection, connection:
            root = connection.execute(
                "SELECT value FROM meta WHERE key='root_dev'"
            ).fetchone()
            inode = connection.execute(
                "SELECT value FROM meta WHERE key='root_inode'"
            ).fetchone()
            assert root and inode, "index has no root identity metadata"
            changed = connection.execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes+4096 "
                "WHERE dev_id=? AND inode=?", (int(root[0]), int(inode[0]))
            ).rowcount
            assert changed == 1, "could not inject root accounting mismatch"

    failed_rounds = []
    for expected_failures in (1, 2):
        daemon = Daemon(case, case.fault_binary, "scan.install_committed")
        try:
            daemon.paused_at("scan.install_committed")
            damage_root_rollup()
            log_path = case.base / "daemon.log"
            log_offset = log_path.stat().st_size
            daemon.process.send_signal(signal.SIGCONT)
            expected_log = (
                "bounded reconciliation retry scheduled" if expected_failures == 1
                else "automatic scans paused to protect the host"
            )

            def failure_logged():
                log = log_path.read_bytes()[log_offset:].decode(errors="replace")
                return expected_log in log

            daemon.wait(failure_logged,
                        f"failed reconciliation {expected_failures} recorded", timeout=60)
            current = metadata()
            assert current.get("failed_reconciliation_attempts") == str(expected_failures), \
                f"failure log was emitted but retry counter was not persisted: {current!r}"
            assert "dirty_since" in current, current
            failed_rounds.append({
                "failed_attempts": expected_failures,
                "dirty_reason": current.get("dirty_reason"),
                "circuit_open": expected_failures > 1,
            })
        finally:
            daemon.stop()

    # The next process must honor the persisted open circuit, avoid another
    # automatic scan, and continue receiving ordinary filesystem events.
    generation_before = metadata().get("scan_generation")
    log_path = case.base / "daemon.log"
    log_offset = log_path.stat().st_size
    daemon = Daemon(case, case.fault_binary)
    try:
        daemon.wait(
            lambda: "automatic reconciliation remains paused" in
            log_path.read_bytes()[log_offset:].decode(errors="replace"),
            "persisted circuit state after daemon restart", timeout=15,
        )
        previous_seen = daemon.record()["seen"]
        (case.tree / "event-ingestion-while-paused").write_bytes(b"still watched")
        daemon.wait(lambda: daemon.record()["seen"] > previous_seen,
                    "event ingestion while automatic reconciliation is paused", timeout=15)
        time.sleep(2)
        current = metadata()
        log = log_path.read_bytes()[log_offset:].decode(errors="replace")
        assert current.get("failed_reconciliation_attempts") == "2", current
        assert current.get("scan_generation") == generation_before, current
        assert "starting background reconciliation" not in log, log

        daemon.process.send_signal(signal.SIGHUP)
        daemon.exact(timeout=90)
        current = metadata()
        assert "failed_reconciliation_attempts" not in current, current
        assert "dirty_since" not in current, current
        integrity(case.db)
        case.history()
    finally:
        daemon.stop()

    evidence.update(
        persisted_failures=failed_rounds,
        circuit_survived_restart=True,
        automatic_scan_suppressed=True,
        event_ingestion_continued=True,
        explicit_scan_cleared_circuit=True,
        final_exact_index=True,
    )
    print("PASS bounded reconciliation retry, persisted circuit, live ingestion, explicit recovery", flush=True)


def root_scan_test(case, _args, evidence):
    db = case.base / 'root-scan.db'
    # Keep the CPU work budget constant when the qualification unit is capped:
    # the scan can legitimately take several wall-clock minutes on a large host
    # root, and a fixed 180s timeout made the 25% CI/VM profile fail at 45 CPU-s.
    scan_timeout = max(180, int(180 * 100 / _args.cpu_quota_percent) + 60)
    result = command(case.binary, '--db', db, 'scan', '/', '--quiet', '--one-file-system',
                     '--exclude', case.base, '--jobs', '2', env=case.env, timeout=scan_timeout)
    meta, _ = check_index(db)
    with contextlib.closing(snapshot(db)) as conn:
        counts = conn.execute("SELECT SUM(kind='f'),SUM(kind='d'),COUNT(*) FROM inodes").fetchone()
        entries = conn.execute('SELECT COUNT(*) FROM dirents').fetchone()[0]
    dirty_reason = meta.get('dirty_reason')
    assert counts[0] > 100 and counts[1] > 10 and entries > 100, (counts, entries, result.stdout)
    assert meta['root_dev'] == str(os.stat('/').st_dev)
    # A live host root can contain transient or intentionally unreadable paths.
    # Exact materialized accounting must still hold, and incomplete coverage
    # must remain visible instead of being mistaken for a clean scan.
    if dirty_reason:
        assert 'unreadable entries' in dirty_reason, meta
        assert 'dirty_since' in meta, meta
    integrity(db)
    evidence.update(root_path_scanned=True, files=counts[0], directories=counts[1], dirents=entries,
                    database_excluded=True, one_filesystem=True,
                    coverage_complete=dirty_reason is None, dirty_reason=dirty_reason)
    print(f'PASS scan root /: {counts[0]} files, {counts[1]} dirs, {entries} entries', flush=True)


def validate_workspace_parent(path, image_mib):
    """Require excluded tmpfs with room for the disposable image and index."""
    path = Path(path).resolve()
    path.mkdir(parents=True, exist_ok=True)
    if path.stat().st_dev == Path("/").stat().st_dev:
        raise RuntimeError(
            f"qualification workspace {path} is on the production root filesystem; "
            "choose a tmpfs workspace outside the watched production filesystem"
        )
    mount_type = None
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        left, separator, right = line.partition(" - ")
        fields = left.split()
        if not separator or len(fields) < 5:
            continue
        mountpoint = fields[4]
        for escaped, character in ((r"\040", " "), (r"\011", "\t"),
                                   (r"\012", "\n"), (r"\134", "\\")):
            mountpoint = mountpoint.replace(escaped, character)
        mount_path = Path(mountpoint)
        if path == mount_path or path.is_relative_to(mount_path):
            if mount_type is None or len(mount_path.parts) > len(mount_type[0].parts):
                mount_type = (mount_path, right.split()[0])
    filesystem_type = mount_type[1] if mount_type else None
    if filesystem_type != "tmpfs":
        raise RuntimeError(
            f"qualification workspace {path} resolves to filesystem type {filesystem_type!r}; "
            "a tmpfs is required so the installed watcher cannot observe fixture activity"
        )
    required = image_mib * MIB + 512 * MIB
    available = shutil.disk_usage(path).free
    if available < required:
        raise RuntimeError(
            f"qualification workspace {path} has {available} free bytes; "
            f"need at least {required} to isolate the loop image and database"
        )
    return path, path.stat().st_dev, filesystem_type, available


def promote_suite_resources(args):
    """Use production-scale fixtures with enough reserve for the active suite."""
    messages = []
    if "disk" in args.suites or "churn" in args.suites:
        promoted = False
        if args.seed_files < 100_000:
            args.seed_files = 100_000
            promoted = True
        if args.filesystem_mib < 2_048:
            args.filesystem_mib = 2_048
            promoted = True
        if promoted:
            if "disk" in args.suites:
                messages.append("QUALIFICATION: disk suite promoted to 100000 files and 2048 MiB to reliably exercise ENOSPC across supported filesystems")
            if "churn" in args.suites:
                messages.append("QUALIFICATION: churn suite promoted to 100000 files and 2048 MiB so its watcher retains the configured 256 MiB write-safety reserve")
    return messages


SUITES = {"crash": crash_tests, "overflow": overflow_test, "cpu": cpu_event_burst_test,
          "churn": production_churn_test, "disk": disk_tests, "coverage": coverage_test, "soak": soak_test,
          "capability": capability_test, "oom": oom_test, "pressure": pressure_reconciliation_test,
          "mount": mount_test, "wal": wal_test, "startup": startup_test,
          "retry": reconciliation_retry_test,
          # A read-only root scan can raise host I/O PSI. Run it after the
          # isolated crash/pressure suites so that it cannot cause those tests
          # to exercise their intentional pressure deferral path accidentally.
          "root": root_scan_test}

# The packaged production service uses MemoryMax=4G. The consolidated worker
# includes both the harness and a tmpfs-backed loop fixture, so its cgroup must
# allow the same ceiling; a 768 MiB cap OOM-killed the harness during the
# second 100k-file baseline scan before product behavior could be qualified.
QUALIFICATION_WORKER_MEMORY_MIB = 4096

# These suites are the serial P0 release matrix. They are also run in
# evidenced subsets when earlier expensive suites have already passed; those
# subsets still need the full wall-clock budget for the remaining gates.
P0_MATRIX_SUITES = frozenset({
    "crash", "overflow", "cpu", "churn", "disk", "coverage", "capability",
    "oom", "pressure", "mount", "wal", "startup", "retry", "root",
})


def write_report(path, report):
    """Publish a complete qualification checkpoint without exposing partial JSON."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    payload = json.dumps(report, indent=2) + "\n"
    with temporary.open("w") as output:
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    directory_fd = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def publish_churn_progress(report_path, progress):
    """Durably publish completed churn batches so VM stops do not erase milestones."""
    progress["last_checkpoint_at"] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    write_report(report_path, progress)


def worker(args):
    require_disposable_qualification_vm()
    args.report.parent.mkdir(parents=True, exist_ok=True)
    try:
        workspace_parent, workspace_device, workspace_filesystem_type, workspace_free_bytes = validate_workspace_parent(
            args.workspace_parent, args.filesystem_mib
        )
    except Exception as error:
        failure = {
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "status": "failed",
            "error": f"{type(error).__name__}: {error}",
            "workspace_parent": str(args.workspace_parent),
            "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        }
        write_report(args.report, failure)
        print("Evidence:", args.report, flush=True)
        raise
    report = {"started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "kernel": platform.release(), "platform": platform.platform(),
              "seed": args.seed, "binary": str(args.binary), "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "fault_binary_sha256": hashlib.sha256(args.fault_binary.read_bytes()).hexdigest() if args.fault_binary else None,
              "crash_rounds": args.crash_rounds,
              "filesystem": f"{args.filesystem_type} disposable loop image",
              "workspace_parent": str(workspace_parent),
              "workspace_device": workspace_device,
              "workspace_filesystem_type": workspace_filesystem_type,
              "workspace_free_bytes_at_start": workspace_free_bytes,
              "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "seed_files": args.seed_files, "filesystem_image_mib": args.filesystem_mib,
              "qualification_worker_limits": {"cpu_quota_percent": args.cpu_quota_percent,
                                               "memory_max_mib": QUALIFICATION_WORKER_MEMORY_MIB},
              "not_qualified_by_this_run": ["power loss or media corruption",
                  f"filesystems other than {args.filesystem_type} and other kernels",
                  "multi-day reader-held WAL endurance", "100k/1m/10m performance budgets", "package upgrade/rollback and musl",
                  "dependency and default-privilege release review", "72-hour dedicated soak and 24-hour canary"],
              "suites_not_run": sorted(set(SUITES)-set(args.suites)),
              "suites": {}, "status": "running",
              "current_suite": None,
              "last_checkpoint_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    write_report(args.report, report)
    # Keep fixtures off the production filesystem. Private mount namespaces
    # isolate mounts, but they do not hide ordinary /tmp files from a separate
    # daemon watching the host root; the loop image itself would still create
    # a host-visible file there. Require room for the complete image plus
    # database/WAL headroom instead of silently falling back to /tmp.
    workspace = Path(tempfile.mkdtemp(prefix="dux-readiness-", dir=workspace_parent))
    try:
        base = workspace
        runtime = base / "runtime"
        runtime.mkdir()
        inode_reserve = args.seed_files + max(args.churn_operations, 12_000) + 4096
        with DisposableFilesystem(base, args.filesystem_mib, inode_reserve, args.filesystem_type) as filesystem_root:
            case = Case(base, filesystem_root, runtime, args.binary, args.fault_binary)
            try:
                print(f"PROGRESS fixture creation start: {args.seed_files} baseline files", flush=True)
                case.seed(args.seed_files)
                for name in args.suites:
                    evidence = [] if name == "crash" else {}
                    report["suites"][name] = {"status": "running", "evidence": evidence}
                    started = time.monotonic()
                    report["current_suite"] = name
                    report["current_suite_started_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
                    report["last_checkpoint_at"] = report["current_suite_started_at"]
                    write_report(args.report, report)
                    SUITES[name](case, args, evidence)
                    report["suites"][name].update(status="passed", elapsed_seconds=round(time.monotonic()-started, 3))
                    report["current_suite"] = None
                    report["last_checkpoint_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
                    write_report(args.report, report)
            finally:
                already_failing = sys.exc_info()[0] is not None
                if already_failing and args.keep_failed_workspace:
                    # The qualification runner uses a private mount namespace,
                    # so retaining only its /tmp path loses the loop-mounted
                    # tree as soon as systemd tears the namespace down. Export
                    # a consistent SQLite backup and the exact tree to the
                    # persistent report directory before stopping the daemon.
                    preserved = args.report.parent / f"{args.report.stem}-failure-{os.getpid()}"
                    preserved.mkdir(parents=True, exist_ok=True)
                    preservation_errors = []
                    # Capture the database and watcher publication before any
                    # expensive filesystem walk. A 100k-file copy/identity
                    # inventory can itself trigger the daemon's I/O-pressure
                    # guard, extending a transient mismatch and obscuring the
                    # exact state that caused the assertion.
                    try:
                        with contextlib.closing(snapshot(case.db)) as source_db, \
                                contextlib.closing(sqlite3.connect(preserved / "index.db")) as target_db:
                            source_db.backup(target_db)
                        pre_stop_status = []
                        for status_file in runtime.glob("*.watch.status"):
                            try:
                                pre_stop_status.append(status_file.read_text())
                            except FileNotFoundError:
                                continue
                        if pre_stop_status:
                            (preserved / "watch-status-before-stop.txt").write_text(
                                "\n".join(pre_stop_status)
                            )
                    except Exception as error:
                        preservation_errors.append(f"database/status snapshot: {error}")
                    # Stop services before copying runtime files. watch.status is
                    # atomically replaced while the daemon is live, so copying its
                    # directory concurrently can race a temporary-file rename and
                    # mask the qualification failure we are trying to preserve.
                    stop_error = None
                    try:
                        case.close()
                    except Exception as error:
                        stop_error = str(error)
                    # The workload is frozen now, so the copied tree and
                    # identity map describe the same filesystem state as the
                    # preserved SQLite backup without racing daemon writes.
                    try:
                        # Capture true device/inode identities, paths and block
                        # allocations from the original mounted tree. This is the
                        # authoritative exact tree snapshot; copying files would
                        # assign new inode numbers and duplicate large fixtures.
                        truth_path = Path(filesystem_snapshot(case.tree, preserved))
                        truth_path.replace(preserved / "filesystem-truth.sqlite")
                    except Exception as error:
                        preservation_errors.append(f"filesystem truth snapshot: {error}")
                    try:
                        shutil.copytree(runtime, preserved / "runtime", dirs_exist_ok=True)
                        daemon_log = base / "daemon.log"
                        if daemon_log.exists():
                            shutil.copy2(daemon_log, preserved / "daemon.log")
                    except Exception as error:
                        preservation_errors.append(f"runtime/log copy: {error}")
                    preservation = {
                        "source_workspace_private_path": str(base),
                        "captured_at_epoch": time.time(),
                        "binary_sha256": report["binary_sha256"],
                        "harness_sha256": report["harness_sha256"],
                        "sqlite_backup_captured_before_daemon_stop": True,
                        "filesystem_truth_captured_after_daemon_stop": True,
                        "filesystem_truth_is_authoritative_exact_tree_snapshot": True,
                        "full_file_content_copy_saved": False,
                    }
                    if stop_error:
                        preservation["daemon_stop_error"] = stop_error
                    if preservation_errors:
                        preservation["preservation_errors"] = preservation_errors
                    (preserved / "snapshot.json").write_text(json.dumps(preservation, indent=2) + "\n")
                    report["preserved_workspace"] = str(preserved)
                    if preservation_errors:
                        report["preservation_errors"] = preservation_errors
                try:
                    case.close()
                except Exception as error:
                    report["cleanup_error"] = str(error)
                    if not already_failing:
                        raise
                finally:
                    log = base / "daemon.log"
                    report["log_tail"] = log.read_text()[-12000:] if log.exists() else ""
        report["status"] = "passed"
    except BaseException as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}")
        for suite in report["suites"].values():
            if suite["status"] == "running":
                suite.update(status="failed", error=report["error"])
        raise
    finally:
        report["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        if args.keep_failed_workspace and report["status"] != "passed":
            report.setdefault("preserved_workspace", str(workspace))
        else:
            shutil.rmtree(workspace, ignore_errors=True)
        report["current_suite"] = None
        report["last_checkpoint_at"] = report["finished_at"]
        write_report(args.report, report)
        print("Evidence:", args.report, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/dux"))
    parser.add_argument("--fault-binary", type=Path)
    parser.add_argument("--suites", nargs="+", choices=SUITES, default=[name for name in SUITES if name != "startup"])
    parser.add_argument("--crash-rounds", type=int, default=30)
    parser.add_argument("--duration-seconds", type=int, default=120)
    parser.add_argument("--operations-per-second", type=int, default=25)
    parser.add_argument("--soak-check-interval", type=int, default=100,
                        help="mutations between full-tree exactness checks in the soak suite")
    parser.add_argument("--seed", type=int, default=20261022)
    parser.add_argument("--cpu-quota-percent", type=int, default=25)
    parser.add_argument("--seed-files", type=int, default=256,
                        help="baseline regular files to create before the selected suite")
    parser.add_argument("--filesystem-mib", type=int, default=768,
                        help="size of the private loop image")
    parser.add_argument("--filesystem-type", choices=("ext4", "xfs", "btrfs"), default="ext4",
                        help="filesystem for the disposable loop image")
    parser.add_argument("--churn-operations", type=int, default=20000,
                        help="operations for the production-shaped churn suite")
    parser.add_argument("--churn-batch-size", type=int, default=2000,
                        help="operations between exact full-tree checks")
    parser.add_argument("--keep-failed-workspace", action="store_true",
                        help="preserve the disposable image and database after failures")
    parser.add_argument("--trace-events", action="store_true",
                        help="enable watch debug logs in the isolated worker")
    parser.add_argument("--report", type=Path, default=Path("target/readiness/report.json"))
    parser.add_argument("--workspace-parent", type=Path, default=Path("/run/dux-readiness"),
                        help="filesystem outside the watched production root for disposable loop images and databases")
    parser.add_argument("--private-worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.binary, args.report = args.binary.resolve(), args.report.resolve()
    args.fault_binary = args.fault_binary.resolve() if args.fault_binary else None
    parser.error("the startup suite requires --fault-binary built from the same candidate source") \
        if "startup" in args.suites and not args.fault_binary else None
    parser.error("the pressure suite requires --fault-binary built from the same candidate source") \
        if "pressure" in args.suites and not args.fault_binary else None
    parser.error("the retry suite requires --fault-binary built from the same candidate source") \
        if "retry" in args.suites and not args.fault_binary else None
    parser.error("crash rounds must be 1..1000") if not 1 <= args.crash_rounds <= 1000 else None
    parser.error("duration must be 1..259200 seconds") if not 1 <= args.duration_seconds <= 259200 else None
    parser.error("operation rate must be 1..1000") if not 1 <= args.operations_per_second <= 1000 else None
    parser.error("soak exactness interval must be 100..100000") if not 100 <= args.soak_check_interval <= 100000 else None
    parser.error("CPU quota must be 25..75 percent") if not 25 <= args.cpu_quota_percent <= 75 else None
    parser.error("seed file count must be 1..1000000") if not 1 <= args.seed_files <= 1000000 else None
    parser.error("filesystem image must be 768..8192 MiB") if not 768 <= args.filesystem_mib <= 8192 else None
    parser.error("churn operations must be 100..1000000") if not 100 <= args.churn_operations <= 1000000 else None
    parser.error("churn batch size must be 100..100000") if not 100 <= args.churn_batch_size <= 100000 else None
    if not args.private_worker:
        try:
            require_disposable_qualification_vm()
        except RuntimeError as error:
            parser.error(str(error))
    # The disk suite needs enough staged data to fail reliably on supported
    # filesystems. Churn needs the same production baseline and image headroom
    # so the configured low-disk guard never pauses ordinary event processing.
    for message in promote_suite_resources(args):
        print(message, flush=True)
    if args.private_worker:
        worker(args)
        return
    args.report.parent.mkdir(parents=True, exist_ok=True)
    assert os.geteuid() == 0, "root/systemd is required for isolated filesystem qualification"
    unit = f"dux-readiness-{os.getpid()}"
    forwarded = [str(Path(__file__).resolve()), "--private-worker", "--binary", str(args.binary), "--report", str(args.report),
                 "--crash-rounds", str(args.crash_rounds), "--duration-seconds", str(args.duration_seconds),
                 "--operations-per-second", str(args.operations_per_second), "--seed", str(args.seed),
                 "--soak-check-interval", str(args.soak_check_interval),
                 "--cpu-quota-percent", str(args.cpu_quota_percent), "--suites", *args.suites]
    forwarded.extend(["--seed-files", str(args.seed_files), "--filesystem-mib", str(args.filesystem_mib),
                      "--filesystem-type", args.filesystem_type,
                      "--churn-operations", str(args.churn_operations), "--churn-batch-size", str(args.churn_batch_size),
                      "--workspace-parent", str(args.workspace_parent.resolve())])
    if args.keep_failed_workspace:
        forwarded.append("--keep-failed-workspace")
    if args.fault_binary:
        forwarded.extend(["--fault-binary", str(args.fault_binary)])
    limit = args.duration_seconds + args.crash_rounds * 180 + 600
    # Ten exact 2k-operation churn batches over a 100k-file index can consume
    # more than fifteen minutes on a 25% CPU quota. Bound the complete churn
    # qualification independently of the crash-round count so selecting churn
    # alone cannot kill the worker before it writes its evidence report.
    if "churn" in args.suites:
        limit = max(limit, 3600)
    # The consolidated 100k-file profile includes 30 recovery scans, a
    # 16k+ event overflow storm with exact cleanup, ten full-tree churn
    # checkpoints and the disk/pressure/restart suites. Its measured runtime
    # has already exceeded an hour before reaching those later gates; retain
    # four hours for the full matrix instead of expiring a valid long run.
    if (P0_MATRIX_SUITES - {"crash", "overflow"}).issubset(set(args.suites)):
        limit = max(limit, 14400)
    runner = ["systemd-run", f"--unit={unit}", "--wait", "--pipe", "--collect",
                             "--property=PrivateMounts=yes", f"--property=CPUQuota={args.cpu_quota_percent}%",
                             f"--property=MemoryMax={QUALIFICATION_WORKER_MEMORY_MIB}M",
                             "--property=IOWeight=10", "--property=Nice=10", f"--property=RuntimeMaxSec={limit}",
                             "--property=TimeoutStopSec=20s", "--property=KillMode=control-group"]
    if args.trace_events:
        runner.append("--setenv=RUST_LOG=dux::watch=debug")
    runner.extend([sys.executable, *forwarded])
    result = subprocess.run(runner)
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
