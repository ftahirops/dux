#!/usr/bin/env python3
"""Bounded scale qualification on a disposable ext4 filesystem in a private namespace.

Run in a dedicated qualification VM. Never reads the installed live index.
Records exact fixture counts, query percentiles, scan CPU/RSS, database/WAL size
and TUI input/exit latency. JSON is retained on failure; incomplete levels stay open.
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import resource
import select
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from datetime import datetime, timezone
from dux_qualification_sandbox import require_disposable_qualification_vm

MIB = 1024*1024


def atomic_json(path, value):
    """Persist a complete checkpoint; a killed worker must not leave torn JSON."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'{path.name}.tmp-{os.getpid()}')
    with temporary.open('w') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0))
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


class ProgressPulse:
    """Publish a live stage heartbeat while a scan/check/query takes time."""
    def __init__(self, report_path, binary_sha256, level, phase):
        self.path = Path(str(report_path) + '.progress.json')
        self.data = {
            'status': 'running', 'binary_sha256': binary_sha256,
            'level_paths': level, 'phase': phase,
            'started_at': datetime.now(timezone.utc).isoformat(),
            'started_monotonic': time.monotonic(),
        }
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, name='dux-scale-progress', daemon=True)

    def _publish(self):
        payload = {k: v for k, v in self.data.items() if k != 'started_monotonic'}
        payload['elapsed_seconds'] = round(time.monotonic() - self.data['started_monotonic'], 3)
        payload['heartbeat_at'] = datetime.now(timezone.utc).isoformat()
        atomic_json(self.path, payload)

    def _run(self):
        while not self.stop.wait(5):
            self._publish()

    def __enter__(self):
        self._publish()
        self.thread.start()
        return self

    def __exit__(self, exc_type, exc, traceback):
        self.stop.set()
        self.thread.join()
        self.data['status'] = 'failed' if exc_type else 'complete'
        if exc is not None:
            self.data['error'] = f'{type(exc).__name__}: {exc}'
        self._publish()


def checkpoint(report_path, report, level, phase):
    report['phase'] = phase
    report['updated_at'] = datetime.now(timezone.utc).isoformat()
    if level is not None:
        level['phase'] = phase
    atomic_json(report_path, report)


def command(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, text=True, capture_output=True, **kwargs)


def validate_workspace_parent(path, required_bytes, disk_workspace=False):
    """Require isolated tmpfs, or explicitly enabled VM-local ext4, with full image capacity."""
    path = Path(path).resolve()
    path.mkdir(parents=True, exist_ok=True)
    if path.stat().st_dev == Path('/').stat().st_dev and not disk_workspace:
        raise RuntimeError(f'scale workspace {path} is on the production root filesystem')
    mounted = None
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, separator, right = line.partition(' - ')
        fields = left.split()
        if not separator or len(fields) < 5:
            continue
        mountpoint = fields[4]
        for escaped, character in ((r'\040', ' '), (r'\011', '\t'),
                                   (r'\012', '\n'), (r'\134', '\\')):
            mountpoint = mountpoint.replace(escaped, character)
        mount_path = Path(mountpoint)
        if path == mount_path or path.is_relative_to(mount_path):
            if mounted is None or len(mount_path.parts) > len(mounted[0].parts):
                mounted = (mount_path, right.split()[0])
    filesystem_type = mounted[1] if mounted else None
    if filesystem_type != 'tmpfs':
        vm = subprocess.run(['systemd-detect-virt', '--vm'], text=True, capture_output=True)
        vm_workspace = (disk_workspace and filesystem_type == 'ext4'
                        and path.is_relative_to(Path('/mnt/dux-readiness'))
                        and vm.returncode == 0 and vm.stdout.strip() == 'kvm')
        if not vm_workspace:
            raise RuntimeError(
                f'scale workspace {path} uses {filesystem_type!r}; use tmpfs or explicitly enable '
                'disk fixtures inside the isolated KVM qualification guest'
            )
    free_bytes = shutil.disk_usage(path).free
    if free_bytes < required_bytes:
        raise RuntimeError(f'scale workspace {path} has {free_bytes} free bytes; need {required_bytes}')
    return path, path.stat().st_dev, filesystem_type, free_bytes


def percentile(values, fraction):
    values = sorted(values)
    return values[max(0, math.ceil(len(values)*fraction)-1)]


def timing_summary(values):
    """Keep the raw sample set beside percentiles so outliers are auditable."""
    return {'sample_count': len(values), 'samples_seconds': list(values),
            'p50_seconds': percentile(values, .5),
            'p95_seconds': percentile(values, .95)}


def process_peak_rss_kib(pid):
    """Read this process's own Linux RSS high-water mark, excluding our parent."""
    try:
        with open(f'/proc/{pid}/status') as status:
            for line in status:
                if line.startswith('VmHWM:'):
                    return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError):
        pass
    return 0


def measured_process(args, environment, log):
    # Linux wait4 can report the launching process's inherited maxrss after fork.
    # The scale worker itself has touched hundreds of MiB generating 10m paths,
    # so that value can falsely become the scan's peak. Sample the scan process's
    # VmHWM while it runs, then use wait4 only when it exceeds the parent's
    # pre-launch high-water (evidence that the child exceeded that baseline).
    parent_peak_rss_kib = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    peak_rss_kib = 0
    sample_stop = threading.Event()
    with log.open('ab') as stream:
        process = subprocess.Popen([str(a) for a in args], env=environment, stdout=stream, stderr=stream)
        peak_rss_kib = process_peak_rss_kib(process.pid)
        def sample_peak():
            nonlocal peak_rss_kib
            while not sample_stop.is_set():
                peak_rss_kib = max(peak_rss_kib, process_peak_rss_kib(process.pid))
                sample_stop.wait(.02)
        sampler = threading.Thread(target=sample_peak, name='dux-rss-sampler', daemon=True)
        sampler.start()
        started = time.monotonic()
        _, status, usage = os.wait4(process.pid, 0)
        peak_rss_kib = max(peak_rss_kib, process_peak_rss_kib(process.pid))
        sample_stop.set()
        sampler.join()
        process.returncode = os.waitstatus_to_exitcode(status)
        assert process.returncode == 0, f'process exited {process.returncode}: {log.read_text()[-3000:]}'
    if usage.ru_maxrss > parent_peak_rss_kib:
        peak_rss_kib = max(peak_rss_kib, usage.ru_maxrss)
    return {'wall_seconds': round(time.monotonic()-started, 4),
            'cpu_seconds': round(usage.ru_utime+usage.ru_stime, 4), 'peak_rss_kib': peak_rss_kib,
            'major_page_faults': usage.ru_majflt, 'read_blocks': usage.ru_inblock, 'written_blocks': usage.ru_oublock}


def flush_fixture_pages(image, drop_vfs_cache=False):
    """Flush guest fixture I/O; optionally reclaim its VFS cache inside the dedicated KVM guest."""
    os.sync()
    with Path(image).open('rb') as stream:
        os.posix_fadvise(stream.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
    if drop_vfs_cache:
        descriptor = os.open('/proc/sys/vm/drop_caches', os.O_WRONLY)
        try:
            os.write(descriptor, b'3\n')
        finally:
            os.close(descriptor)


def tui_sample(binary, db, env):
    sample_started_at = datetime.now(timezone.utc).isoformat()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))
    process = subprocess.Popen([str(binary), '--db', str(db)], stdin=slave, stdout=slave, stderr=slave,
                               env=dict(env, TERM='xterm-256color'))
    os.close(slave)
    done = threading.Event()
    first_frame = threading.Event()
    output = bytearray()
    def drain():
        while not done.is_set():
            try:
                if select.select([master], [], [], 0.05)[0]:
                    data = os.read(master, 65536)
                    if data:
                        output.extend(data)
                        if len(output) > 65536:
                            del output[:-65536]
                        first_frame.set()
            except OSError:
                break
    reader = threading.Thread(target=drain)
    reader.start()
    try:
        assert first_frame.wait(5), 'TUI did not produce its first frame'
        started = time.monotonic()
        os.write(master, b'?')
        deadline = started+1
        while time.monotonic() < deadline and b'dux only advises' not in output:
            time.sleep(.001)
        input_ack = time.monotonic()-started
        assert b'dux only advises' in output, 'TUI did not acknowledge help-key input'
        os.write(master, b'?')
        os.write(master, b'2\x1b[C\x1b[B\x1b[C\t\r')
        time.sleep(0.1)
        started = time.monotonic()
        os.write(master, b'q')
        process.wait(timeout=2)
        elapsed = time.monotonic()-started
        assert process.returncode == 0, process.returncode
        return {'started_at': sample_started_at,
                'input_ack_seconds': input_ack, 'quit_seconds': elapsed}
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        done.set()
        reader.join(timeout=1)
        os.close(master)


def group_path(tree, slot):
    prefix = [(), ("nested", "a", "b"), ("deep", "a", "b", "c", "d", "e")][slot % 3]
    return tree.joinpath(*prefix, f"group-{slot:06d}")


def validate_reused_tree(tree, count, evidence_path):
    """Check read-only provenance and samples; the candidate scan audits every path."""
    tree = Path(tree).resolve(strict=True)
    mount = None
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, separator, right = line.partition(' - ')
        fields = left.split()
        if not separator or len(fields) < 6:
            continue
        point = fields[4].replace(r'\040', ' ').replace(r'\011', '\t').replace(r'\134', '\\')
        candidate = Path(point)
        if tree == candidate or tree.is_relative_to(candidate):
            if mount is None or len(candidate.parts) > len(mount[0].parts):
                mount = candidate, fields[5].split(','), right.split()[0]
    if mount is None or 'ro' not in mount[1]:
        raise RuntimeError(f'reused fixture {tree} is not on a read-only mount')
    if count != 10000000:
        raise RuntimeError('retained fixture evidence only covers exactly 10m paths')
    directories = {group_path(tree, slot) for slot in range((count+511)//512)}
    for group in tuple(directories):
        parent = group.parent
        while parent != tree:
            directories.add(parent)
            parent = parent.parent
    links = count//100
    evidence = json.loads(Path(evidence_path).read_text())
    levels = [level for level in evidence.get('levels', []) if level.get('paths') == count]
    if evidence.get('status') != 'passed' or len(levels) != 1:
        raise RuntimeError('retained fixture report does not contain one passed 10m level')
    truth = levels[0].get('truth', {})
    expected_inodes = count+len(directories)+1-links
    expected_entries = count+len(directories)+1
    if (truth.get('inodes') != expected_inodes or truth.get('directory_entries') != expected_entries
            or levels[0].get('fixture_paths') != count):
        raise RuntimeError(f'retained fixture report counts do not match expected layout: {truth}')
    for index in range(0, count, max(1, count//1000)):
        path = group_path(tree, index//512)/f'artifact-{index:010d}.{["rs", "log", "txt", "bin"][index % 4]}'
        expected_links = 2 if index % 100 in (98, 99) else 1
        if path.is_symlink() or not path.is_file() or path.stat().st_nlink != expected_links:
            raise RuntimeError(f'reused fixture sample differs from its report: {path}')
    return tree, directories, links, {'mountpoint': str(mount[0]), 'filesystem': mount[2],
                                     'mount_options': mount[1], 'files': count,
                                     'directories': len(directories), 'hardlink_pairs': links,
                                     'validation': 'historical_exact_counts_plus_1000_read_only_samples',
                                     'historical_candidate_sha256': evidence['binary_sha256']}


def verify_counts(db, tree, count, dirs, links):
    with contextlib.closing(sqlite3.connect(f'file:{db}?mode=ro', uri=True)) as conn:
        conn.execute('BEGIN')
        meta = dict(conn.execute('SELECT key,value FROM meta'))
        expected = count+len(dirs)+1-links
        actual = conn.execute('SELECT COUNT(*),SUM(blocks) FROM inodes').fetchone()
        root = conn.execute('SELECT recursive_inodes,recursive_bytes FROM inodes WHERE dev_id=? AND inode=?',
                            (int(meta['root_dev']), int(meta['root_inode']))).fetchone()
        entries = conn.execute('SELECT COUNT(*) FROM dirents').fetchone()[0]
        assert actual == root and actual[0] == expected, (actual, root, expected)
        assert entries == count+len(dirs)+1, (entries, count+len(dirs)+1)
        physical = tree.stat().st_blocks*512+sum(path.stat().st_blocks*512 for path in dirs)
        assert actual[1] == physical, (actual[1], physical)
        for index in range(0, count, max(1, count//1000)):
            directory = group_path(tree,index//512)
            ext = ["rs", "log", "txt", "bin"][index % 4]
            name = f"artifact-{index:010d}.{ext}"
            parent, inode = directory.stat(), (directory/name).stat()
            stored = conn.execute("SELECT dev_id,inode FROM dirents WHERE parent_dev=? AND parent_inode=? AND name=?",(parent.st_dev,parent.st_ino,os.fsencode(name))).fetchone()
            assert stored == (inode.st_dev,inode.st_ino), (index,stored)
        assert 'dirty_since' not in meta, meta
        assert conn.execute('SELECT COUNT(*) FROM inodes WHERE birth_ns=0').fetchone()[0] == 0
        assert conn.execute('PRAGMA quick_check').fetchall() == [('ok',)]
        return {'inodes': actual[0], 'allocated_bytes': actual[1], 'directory_entries': entries}


def worker(args):
    report = {'status': 'running', 'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'kernel': platform.release(), 'platform': platform.platform(),
              'cpu_quota_percent': args.cpu_quota_percent,
              'memory_limit_mib': args.worker_memory_mib, 'filesystem': 'ext4',
              'fixture_cache_policy': ('sync+fadvise+drop_caches_each_100k' if args.disk_workspace
                                       else 'sync+fadvise_each_100k'),
              'scan_jobs': args.scan_jobs, 'levels': [],
              'workspace_parent': str(args.workspace_parent.resolve()),
              'calibrated_budgets': {
                  'scan_wall_seconds': {'100000': 30, '1000000': 180, '10000000': 1800},
                  'peak_rss_mib': 256,
                  'query_p95_seconds': {
                      '100000': {'all': 1.0}, '1000000': {'all': 1.0},
                      '10000000': {'top_dirs': 0.5, 'top_files': 0.5, 'find_unique': 3.0,
                                   'du_depth_1': 1.0, 'metrics': 0.5, 'growth': 5.0}},
                  'tui_input_ack_p95_seconds': 0.1, 'tui_quit_p95_seconds': 0.25},
              'not_qualified_by_this_run': ['10k-event catch-up and configured daemon CPU ceiling',
                  'multi-day endurance']}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    checkpoint(args.report, report, None, 'preflight')
    base = None
    try:
        assert os.readlink('/proc/self/ns/mnt') != os.readlink('/proc/1/ns/mnt')
        maximum = max(args.sizes)
        image_bytes = (0 if args.reuse_tree else (8192 if maximum >= 10000000 else 2048 if maximum >= 1000000 else 768)*MIB)
        workspace_parent, workspace_device, workspace_type, workspace_free = validate_workspace_parent(
            args.workspace_parent, image_bytes + 4*1024*MIB, args.disk_workspace
        )
        report.update(workspace_parent=str(workspace_parent), workspace_device=workspace_device,
                      workspace_filesystem_type=workspace_type, workspace_free_bytes_at_start=workspace_free)
        checkpoint(args.report, report, None, 'isolated workspace ready')
        temporary = tempfile.mkdtemp(prefix='dux-scale-', dir=workspace_parent)
        base = Path(temporary)
        try:
            image, mount = base/'filesystem.img', base/'filesystem'
            if args.reuse_tree:
                tree, directories, links, fixture_audit = validate_reused_tree(
                    args.reuse_tree, maximum, args.reuse_evidence)
                report['reused_fixture'] = fixture_audit
                report['fixture_cache_policy'] = 'read_only_retained_ext4_image'
                mount = None
            else:
                mount.mkdir()
                with image.open('wb') as stream:
                    stream.truncate(image_bytes)
                # Small block groups allow >10m 256-byte inodes in a sparse 8GiB image;
                # inode birth time requires the extended inode size.
                mkfs = ['mkfs.ext4', '-F', '-q', '-I', '256', '-g', '4096', '-N', str(maximum*11//10+10000), str(image)]
                command(*mkfs)
                flush_fixture_pages(image, args.disk_workspace)
                report['mkfs'] = mkfs[:-1]
                command('mount', '-o', 'loop,nosuid,nodev', image, mount)
                tree = None
            try:
                tree, db, runtime = (tree if args.reuse_tree else mount/'tree'), base/'index.db', base/'runtime'
                if not args.reuse_tree:
                    tree.mkdir()
                runtime.mkdir()
                env = dict(os.environ, DUX_RUNTIME_DIR=str(runtime))
                env.pop('NOTIFY_SOCKET', None)
                produced = maximum if args.reuse_tree else 0
                directories, links = (directories, links) if args.reuse_tree else (set(), 0)
                for size in args.sizes:
                    level = {'paths': size, 'status': 'running'}
                    report['levels'].append(level)
                    checkpoint(args.report, report, level, 'fixture generation')
                    started = time.monotonic()
                    while produced < size:
                        slot = produced//512
                        directory = group_path(tree,slot)
                        if produced % 512 == 0:
                            directory.mkdir(parents=True, exist_ok=True)
                            current = directory
                            while current != tree:
                                directories.add(current)
                                current = current.parent
                        ext = ['rs', 'log', 'txt', 'bin'][produced % 4]
                        path = directory/f'artifact-{produced:010d}.{ext}'
                        if produced % 100 == 99:
                            previous = produced-1
                            previous_ext = ['rs', 'log', 'txt', 'bin'][previous % 4]
                            source = directory/f'artifact-{previous:010d}.{previous_ext}'
                            # A 512-entry boundary can separate this pair.
                            source = group_path(tree,previous//512)/source.name
                            os.link(source, path)
                            links += 1
                        else:
                            descriptor = os.open(path, os.O_WRONLY|os.O_CREAT|os.O_EXCL, 0o600)
                            os.close(descriptor)
                        produced += 1
                        if produced % 100000 == 0:
                            print(f'fixture: {produced}/{size}', flush=True)
                            if not args.reuse_tree:
                                flush_fixture_pages(image, args.disk_workspace)
                        if produced % 500000 == 0 or produced == size:
                            level['fixture_paths'] = produced
                            checkpoint(args.report, report, level, 'fixture generation')
                    level['fixture_seconds'] = round(time.monotonic()-started, 3)
                    with ProgressPulse(args.report, report['binary_sha256'], size, 'scan'):
                        level['scan'] = measured_process(
                            [args.binary, '--db', db, 'scan', tree, '--quiet', '--jobs', str(args.scan_jobs)],
                            env, base/'scan.log')
                    checkpoint(args.report, report, level, 'exact path/allocation and integrity verification')
                    with ProgressPulse(args.report, report['binary_sha256'], size, 'exact path/allocation and integrity verification'):
                        level['truth'] = verify_counts(db, tree, size, directories, links)
                    checkpoint(args.report, report, level, 'growth fixture and query-plan verification')
                    # Growth table scale is part of the fixture, using real inode keys.
                    with ProgressPulse(args.report, report['binary_sha256'], size, 'growth fixture and query-plan verification'):
                        with contextlib.closing(sqlite3.connect(db)) as conn, conn:
                            conn.execute('INSERT OR IGNORE INTO growth SELECT ?,dev_id,inode,birth_ns,4096 FROM inodes WHERE kind=\'f\' AND birth_ns!=0 LIMIT ?',
                                         (int(time.time())//300, size//4))
                            level['growth_fixture'] = {
                                'rows': conn.execute('SELECT COUNT(*) FROM growth').fetchone()[0],
                                'buckets': conn.execute('SELECT COUNT(DISTINCT bucket) FROM growth').fetchone()[0],
                                'query_plan': [row[3] for row in conn.execute(
                                    'EXPLAIN QUERY PLAN SELECT dev_id,inode,delta FROM growth_current '
                                    'WHERE bucket=? AND delta!=0 ORDER BY delta DESC LIMIT ?',
                                    (int(time.time())//300, 28))],
                            }
                    checkpoint(args.report, report, level, 'query latency samples')
                    sample_name = f'artifact-{min(size-1, 4242):010d}'
                    queries = {'top_dirs': ['top', '--dirs', '--limit', '20'],
                               'top_files': ['top', '--files', '--limit', '20'],
                               'find_unique': ['find', '--name', sample_name, '--limit', '20'],
                               'du_depth_1': ['du', str(tree), '--max-depth', '1'],
                               'metrics': ['metrics', '--top', '0'],
                               'growth': ['growth', '--limit', '20']}
                    results = {}
                    for name, query in queries.items():
                        sample_started_at = []
                        with ProgressPulse(args.report, report['binary_sha256'], size, f'query samples: {name}'):
                            command(args.binary, '--db', db, *query, env=env, timeout=120)
                            timings = []
                            for _ in range(args.samples):
                                sample_started_at.append(datetime.now(timezone.utc).isoformat())
                                started = time.monotonic()
                                command(args.binary, '--db', db, *query, env=env, timeout=120)
                                timings.append(time.monotonic()-started)
                        results[name] = timing_summary(timings)
                        results[name]['sample_started_at'] = sample_started_at
                        level['queries'] = results
                        checkpoint(args.report, report, level, f'query latency samples ({name} complete)')
                    checkpoint(args.report, report, level, 'TUI latency samples')
                    level['queries'] = results
                    tui = []
                    with ProgressPulse(args.report, report['binary_sha256'], size, 'TUI latency samples'):
                        for _ in range(args.samples):
                            tui.append(tui_sample(args.binary, db, env))
                    level['tui_samples'] = tui
                    level['tui_input_ack_p95_seconds'] = percentile([s['input_ack_seconds'] for s in tui], .95)
                    level['tui_quit_p95_seconds'] = percentile([s['quit_seconds'] for s in tui], .95)
                    level['database_bytes'] = db.stat().st_size
                    wal = Path(str(db)+'-wal')
                    level['wal_bytes'] = wal.stat().st_size if wal.exists() else 0
                    assert level['scan']['wall_seconds'] < {100000: 30, 1000000: 180, 10000000: 1800}[size], level['scan']
                    assert level['scan']['peak_rss_kib'] < 256 * 1024, level['scan']
                    assert level['tui_quit_p95_seconds'] < .25, level['tui_quit_p95_seconds']
                    assert level['tui_input_ack_p95_seconds'] < .1, level['tui_input_ack_p95_seconds']
                    if size < 10000000:
                        assert all(value['p95_seconds'] < 1 for value in results.values()), results
                    else:
                        limits = {'top_dirs': .5, 'top_files': .5, 'find_unique': 3.0,
                                  'du_depth_1': 1.0, 'metrics': .5, 'growth': 5.0}
                        assert all(results[name]['p95_seconds'] < limit for name, limit in limits.items()), results
                    level['performance_budgets_passed'] = True
                    level['status'] = 'passed'
                    checkpoint(args.report, report, level, 'complete')
                    print('PASS scale level', size, 'scan seconds', level['scan']['wall_seconds'], 'query p95', results, flush=True)
            finally:
                if mount is not None:
                    command('umount', mount)
        except BaseException:
            if base is not None:
                report['failed_workspace'] = str(base)
            raise
        else:
            shutil.rmtree(base, ignore_errors=True)
        report['status'] = 'passed'
    except BaseException as error:
        report.update(status='failed', error=f'{type(error).__name__}: {error}')
        raise
    finally:
        checkpoint(args.report, report, report['levels'][-1] if report['levels'] else None,
                   'complete' if report['status'] == 'passed' else 'failed')
        print('Evidence:', args.report, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--report', type=Path, default=Path('target/readiness/scale.json'))
    parser.add_argument('--sizes', type=int, nargs='+', default=[100000, 1000000, 10000000])
    parser.add_argument('--samples', type=int, default=50,
                        help='latency samples per query/TUI check (3..50; default 50 for a stable p95)')
    parser.add_argument('--scan-jobs', type=int, default=2,
                        help='parallel filesystem walker threads; recorded in the report')
    parser.add_argument('--worker-memory-mib', type=int, default=768,
                        help='MemoryMax for the complete worker, including fixture page cache')
    parser.add_argument('--cpu-quota-percent', type=int, default=50,
                        help='systemd CPUQuota for the worker; recorded in the report')
    parser.add_argument('--workspace-parent', type=Path, default=Path('/run/dux-scale'),
                        help='tmpfs path outside the watched production filesystem for disposable scale fixtures')
    parser.add_argument('--disk-workspace', action='store_true',
                        help='allow an ext4 fixture workspace under /mnt/dux-readiness inside the isolated KVM qualification VM')
    parser.add_argument('--reuse-tree', type=Path,
                        help='scan an existing fixture on a verified read-only mount')
    parser.add_argument('--reuse-evidence', type=Path,
                        help='historical passed scale report proving the retained fixture counts')
    parser.add_argument('--private-worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        require_disposable_qualification_vm()
    except RuntimeError as error:
        parser.error(str(error))
    args.binary, args.report = args.binary.resolve(), args.report.resolve()
    assert args.sizes == sorted(set(args.sizes)) and all(1000 <= size <= 10000000 for size in args.sizes)
    if args.reuse_tree:
        assert args.sizes == [10000000], 'reused fixtures are currently qualified only at exactly 10m paths'
        assert args.reuse_evidence is not None, '--reuse-tree requires --reuse-evidence'
    assert 3 <= args.samples <= 50
    assert 1 <= args.scan_jobs <= 64
    assert 768 <= args.worker_memory_mib <= 32768
    assert 10 <= args.cpu_quota_percent <= 800
    args.report.parent.mkdir(parents=True, exist_ok=True)
    if args.private_worker:
        worker(args)
        return
    runargs = ['systemd-run', f'--unit=dux-scale-{os.getpid()}', '--wait', '--pipe', '--collect',
               '--property=PrivateMounts=yes', f'--property=CPUQuota={args.cpu_quota_percent}%',
               f'--property=MemoryMax={args.worker_memory_mib}M',
               '--property=IOWeight=10', '--property=Nice=10', '--property=RuntimeMaxSec=14400',
               '--property=TimeoutStopSec=30s', sys.executable, str(Path(__file__).resolve()), '--private-worker',
               '--binary', str(args.binary), '--report', str(args.report), '--samples', str(args.samples),
               '--scan-jobs', str(args.scan_jobs), '--worker-memory-mib', str(args.worker_memory_mib),
               '--cpu-quota-percent', str(args.cpu_quota_percent),
               '--workspace-parent', str(args.workspace_parent.resolve()),
               '--sizes', *map(str, args.sizes)]
    if args.reuse_tree:
        runargs += ['--reuse-tree', str(args.reuse_tree.resolve())]
        runargs += ['--reuse-evidence', str(args.reuse_evidence.resolve())]
    if args.disk_workspace:
        runargs.append('--disk-workspace')
    raise SystemExit(subprocess.run(runargs).returncode)


if __name__ == '__main__':
    main()
