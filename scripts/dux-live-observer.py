#!/usr/bin/env python3
"""Read-only bounded observer for the installed dux accounting recurrence."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time

BINARY = Path('/usr/bin/dux')
SERVICE_BINARY = Path('/usr/bin/dux')
DB = Path('/var/lib/dux/dux.db')
OUTPUT = Path('target/readiness/live-recurrence-observed.json')
PROGRESS_OUTPUT = None
SERVICE_UNIT = 'dux.service'
ROOT = '/'
OBSERVER_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
PSEUDO_FILESYSTEMS = {
    'proc', 'sysfs', 'cgroup', 'cgroup2', 'devtmpfs', 'devpts', 'debugfs', 'tracefs',
    'securityfs', 'bpf', 'mqueue', 'pstore', 'binfmt_misc', 'usbfs',
    'fusectl', 'configfs', 'nsfs', 'hugetlbfs', 'ramfs', 'tmpfs', 'autofs',
    'rpc_pipefs', 'selinuxfs', 'fuse', 'overlay',
}


def default_progress_path(output):
    """Keep observer progress off the filesystem tree being audited.

    Replacing a sidecar under the indexed root generates fanotify events and
    changes the index during the observer's own stable-window check. /run is a
    tmpfs excluded from a root-filesystem walk on the supported systemd hosts.
    Include the PID so concurrent observers do not overwrite one another.
    """
    output = Path(output)
    return Path('/run') / f'dux-live-observer-{output.stem}-{os.getpid()}.progress.json'


def parse_mountinfo(text):
    mounts = {}
    for line in text.splitlines():
        fields = line.split()
        if '-' not in fields:
            continue
        separator = fields.index('-')
        if len(fields) <= separator + 1 or len(fields) < 5:
            continue
        mountpoint = fields[4]
        for escaped, character in (('\\040', ' '), ('\\011', '\t'),
                                   ('\\012', '\n'), ('\\134', '\\')):
            mountpoint = mountpoint.replace(escaped, character)
        mounts[Path(mountpoint)] = fields[separator + 1]
    return mounts


def mount_policy(root, mounts):
    """Return nested mounts to prune and whether the index root is pseudo."""
    root = Path(root)
    nested = {path for path, fs in mounts.items()
              if path != root and path.is_relative_to(root)
              and (fs in PSEUDO_FILESYSTEMS or fs.startswith('fuse.'))}
    containing = [(path, fs) for path, fs in mounts.items() if root.is_relative_to(path)]
    root_fs = max(containing, key=lambda item: len(item[0].parts))[1] if containing else ''
    return nested, root_fs in PSEUDO_FILESYSTEMS or root_fs.startswith('fuse.')


def index_artifact(path, db):
    """Mirror dux's database/sidecar exclusion for independent tree walks."""
    path, db = Path(path), Path(db)
    suffixes = ('-wal', '-shm', '-journal', '.lock', '.new', '.new-wal',
                '.new-shm', '.heartbeat', '.watch.status', '.scan.progress')
    if path == db or any(path == Path(str(db) + suffix) for suffix in suffixes):
        return True
    if path.parent != db.parent:
        return False
    name = path.name
    base = db.name
    return any(name.startswith(base + suffix)
               for suffix in ('.new.scan-', '.heartbeat.', '.watch.status.', '.scan.progress.'))


def run(*args):
    p = subprocess.run(args, capture_output=True, text=True)
    return {'returncode': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr}


def write_progress_file(path, payload):
    """Replace a progress sidecar atomically so readers never see partial JSON."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(str(path) + '.tmp')
    temporary.write_text(json.dumps(payload, sort_keys=True) + '\n')
    temporary.replace(path)


def indexed_path(root, components):
    """Rebuild a filesystem path from raw indexed name bytes under any root."""
    path = Path(root)
    for component in components:
        path = path / os.fsdecode(bytes(component))
    return path


def audit_staging_directory():
    return Path('/run/dux') if Path('/run/dux').is_dir() else Path(tempfile.gettempdir())


def exact_filesystem(root, conn, progress_callback=None):
    """Compare all paths and inode allocations using disk-backed staging.

    A Python dict per path can exceed a gigabyte on a whole-host audit. Stage
    filesystem and candidate rows in a temporary SQLite DB under /run instead.
    """
    root = Path(root)
    errors = []
    error_count = 0
    started = time.monotonic()
    last_progress = started
    walked_paths = 0
    visited_directory_count = 0

    def progress(phase, force=False, **details):
        nonlocal last_progress
        now = time.monotonic()
        if progress_callback is not None and (force or now - last_progress >= 10):
            progress_callback({
                'phase': phase,
                'elapsed_seconds': round(now - started, 3),
                'visited_directories': visited_directory_count,
                'filesystem_paths': walked_paths,
                'walk_errors': error_count,
                **details,
            })
            last_progress = now
    try:
        root_stat = os.lstat(root)
    except OSError as error:
        return {'passed': False, 'root_error': str(error)}
    mountpoints, root_is_pseudo = mount_policy(root, parse_mountinfo(
        Path('/proc/self/mountinfo').read_text()))
    visited_directories = {(root_stat.st_dev, root_stat.st_ino)}
    audit_dir = audit_staging_directory()
    fd, audit_path = tempfile.mkstemp(prefix='dux-exact-audit-', suffix='.sqlite', dir=audit_dir)
    os.close(fd)
    audit = sqlite3.connect(audit_path)
    try:
        audit.executescript('''
            PRAGMA journal_mode=OFF;
            PRAGMA synchronous=OFF;
            PRAGMA temp_store=FILE;
            PRAGMA cache_size=-16384;
            CREATE TABLE fs_entries(parent_dev INTEGER,parent_inode INTEGER,name BLOB,
                                    dev_id INTEGER,inode INTEGER,
                                    PRIMARY KEY(parent_dev,parent_inode,name)) WITHOUT ROWID;
            CREATE TABLE fs_inodes(dev_id INTEGER,inode INTEGER,blocks INTEGER,
                                   PRIMARY KEY(dev_id,inode)) WITHOUT ROWID;
            CREATE TABLE idx_entries(parent_dev INTEGER,parent_inode INTEGER,name BLOB,
                                     dev_id INTEGER,inode INTEGER,
                                     PRIMARY KEY(parent_dev,parent_inode,name)) WITHOUT ROWID;
            CREATE TABLE idx_inodes(dev_id INTEGER,inode INTEGER,blocks INTEGER,
                                    PRIMARY KEY(dev_id,inode)) WITHOUT ROWID;
        ''')
        audit.execute('INSERT INTO fs_inodes VALUES(?,?,?)',
                      (root_stat.st_dev, root_stat.st_ino, root_stat.st_blocks * 512))

        def walk_error(error):
            nonlocal error_count
            error_count += 1
            if len(errors) < 50:
                errors.append({'path': getattr(error, 'filename', None), 'error': str(error)})

        def add_entry(parent, name, current):
            nonlocal walked_paths
            dev, ino = current.st_dev, current.st_ino
            audit.execute('INSERT OR IGNORE INTO fs_entries VALUES(?,?,?,?,?)',
                          (parent[0], parent[1], sqlite3.Binary(os.fsencode(name)), dev, ino))
            audit.execute('INSERT OR IGNORE INTO fs_inodes VALUES(?,?,?)',
                          (dev, ino, current.st_blocks * 512))
            walked_paths += 1

        progress('filesystem_walk', force=True)
        for directory, dirs, files in os.walk(root, topdown=True, onerror=walk_error, followlinks=False):
            visited_directory_count += 1
            try:
                parent_stat = os.lstat(directory)
            except OSError as error:
                walk_error(error)
                dirs[:] = []
                continue
            parent = (parent_stat.st_dev, parent_stat.st_ino)
            for name in list(dirs):
                path = os.path.join(directory, name)
                if index_artifact(path, DB) or Path(path) in mountpoints:
                    dirs.remove(name)
                    continue
                try:
                    current = os.lstat(path)
                except OSError as error:
                    walk_error(error)
                    continue
                is_directory = stat.S_ISDIR(current.st_mode)
                if is_directory and root_is_pseudo:
                    dirs.remove(name)
                    continue
                add_entry(parent, name, current)
                if is_directory:
                    identity = (current.st_dev, current.st_ino)
                    if identity in visited_directories:
                        dirs.remove(name)
                    else:
                        visited_directories.add(identity)
            for name in files:
                path = os.path.join(directory, name)
                if index_artifact(path, DB):
                    continue
                try:
                    current = os.lstat(path)
                except OSError as error:
                    walk_error(error)
                    continue
                add_entry(parent, name, current)
            progress('filesystem_walk')
        audit.commit()
        progress('filesystem_walk_complete', force=True)

        # Copy candidate rows into the disk-backed comparison store.
        progress('copy_index_entries', force=True, copied_rows=0)
        copied_entries = 0
        rows = conn.execute('SELECT parent_dev,parent_inode,name,dev_id,inode FROM dirents '
                            'WHERE NOT(parent_dev=dev_id AND parent_inode=inode)')
        while batch := rows.fetchmany(4096):
            audit.executemany('INSERT INTO idx_entries VALUES(?,?,?,?,?)', batch)
            copied_entries += len(batch)
            progress('copy_index_entries', copied_rows=copied_entries)
        progress('copy_index_inodes', force=True, copied_entries=copied_entries,
                 copied_rows=0)
        copied_inodes = 0
        rows = conn.execute('SELECT dev_id,inode,blocks FROM inodes')
        while batch := rows.fetchmany(4096):
            audit.executemany('INSERT INTO idx_inodes VALUES(?,?,?)', batch)
            copied_inodes += len(batch)
            progress('copy_index_inodes', copied_entries=copied_entries,
                     copied_rows=copied_inodes)
        audit.commit()
        progress('index_copy_complete', force=True, copied_entries=copied_entries,
                 copied_inodes=copied_inodes)

        def count(label, sql):
            progress('comparison', force=True, check=label)
            return audit.execute(sql).fetchone()[0]

        indexed_paths = count('indexed_paths', 'SELECT COUNT(*) FROM idx_entries')
        filesystem_paths = count('filesystem_paths', 'SELECT COUNT(*) FROM fs_entries')
        indexed_inodes = count('indexed_inodes', 'SELECT COUNT(*) FROM idx_inodes')
        filesystem_inodes = count('filesystem_inodes', 'SELECT COUNT(*) FROM fs_inodes')
        missing_paths = count('missing_paths', '''SELECT COUNT(*) FROM fs_entries f LEFT JOIN idx_entries i
                                  USING(parent_dev,parent_inode,name) WHERE i.name IS NULL''')
        stale_paths = count('stale_paths', '''SELECT COUNT(*) FROM idx_entries i LEFT JOIN fs_entries f
                                USING(parent_dev,parent_inode,name) WHERE f.name IS NULL''')
        wrong_paths = count('identity_mismatches', '''SELECT COUNT(*) FROM fs_entries f JOIN idx_entries i
                                USING(parent_dev,parent_inode,name)
                                WHERE f.dev_id!=i.dev_id OR f.inode!=i.inode''')
        missing_inodes = count('missing_inodes', '''SELECT COUNT(*) FROM fs_inodes f LEFT JOIN idx_inodes i
                                   USING(dev_id,inode) WHERE i.dev_id IS NULL''')
        stale_inodes = count('stale_inodes', '''SELECT COUNT(*) FROM idx_inodes i LEFT JOIN fs_inodes f
                                 USING(dev_id,inode) WHERE f.dev_id IS NULL''')
        wrong_allocations = count('allocation_mismatches', '''SELECT COUNT(*) FROM fs_inodes f JOIN idx_inodes i
                                      USING(dev_id,inode) WHERE f.blocks!=i.blocks''')

        def path_samples(table, other, missing):
            join = f'{table} a LEFT JOIN {other} b USING(parent_dev,parent_inode,name)'
            condition = 'b.name IS NULL' if missing else 'a.name IS NULL'
            query = (f'SELECT a.parent_dev,a.parent_inode,hex(a.name),a.dev_id,a.inode '
                     f'FROM {join} WHERE {condition} LIMIT 50')
            return [{'parent_dev': row[0], 'parent_inode': row[1], 'name_hex': row[2],
                     'child': [row[3], row[4]]} for row in audit.execute(query)]

        allocation_samples = [
            {'dev': row[0], 'inode': row[1], 'indexed_bytes': row[2], 'filesystem_bytes': row[3]}
            for row in audit.execute('''SELECT f.dev_id,f.inode,i.blocks,f.blocks
                FROM fs_inodes f JOIN idx_inodes i USING(dev_id,inode)
                WHERE f.blocks!=i.blocks LIMIT 50''')]

        # Counts alone cannot distinguish transient live churn from a durable
        # path-loss defect. Preserve every mismatch path up to a bounded cap so
        # later snapshots can recheck the exact identities without rerunning a
        # full tree walk. Keep the summary counts authoritative when capped.
        detail_limit = 10_000

        def details(sql, columns, total):
            rows = [dict(zip(columns, row)) for row in
                    audit.execute(sql + ' LIMIT ?', (detail_limit,))]
            return {'count': total, 'truncated': total > len(rows), 'rows': rows}

        progress('collecting_mismatch_details', force=True)
        difference_details = {
            'limit_per_category': detail_limit,
            'missing_from_index': details('''SELECT f.parent_dev,f.parent_inode,hex(f.name),
                    f.dev_id,f.inode FROM fs_entries f LEFT JOIN idx_entries i
                    USING(parent_dev,parent_inode,name) WHERE i.name IS NULL''',
                ('parent_dev', 'parent_inode', 'name_hex', 'filesystem_dev', 'filesystem_inode'),
                missing_paths),
            'stale_in_index': details('''SELECT i.parent_dev,i.parent_inode,hex(i.name),
                    i.dev_id,i.inode FROM idx_entries i LEFT JOIN fs_entries f
                    USING(parent_dev,parent_inode,name) WHERE f.name IS NULL''',
                ('parent_dev', 'parent_inode', 'name_hex', 'indexed_dev', 'indexed_inode'),
                stale_paths),
            'identity_mismatch': details('''SELECT f.parent_dev,f.parent_inode,hex(f.name),
                    i.dev_id,i.inode,f.dev_id,f.inode FROM fs_entries f JOIN idx_entries i
                    USING(parent_dev,parent_inode,name)
                    WHERE f.dev_id!=i.dev_id OR f.inode!=i.inode''',
                ('parent_dev', 'parent_inode', 'name_hex', 'indexed_dev', 'indexed_inode',
                 'filesystem_dev', 'filesystem_inode'), wrong_paths),
            'missing_from_index_inodes': details('''SELECT f.dev_id,f.inode
                    FROM fs_inodes f LEFT JOIN idx_inodes i USING(dev_id,inode)
                    WHERE i.dev_id IS NULL''',
                ('filesystem_dev', 'filesystem_inode'), missing_inodes),
            'stale_in_index_inodes': details('''SELECT i.dev_id,i.inode
                    FROM idx_inodes i LEFT JOIN fs_inodes f USING(dev_id,inode)
                    WHERE f.dev_id IS NULL''',
                ('indexed_dev', 'indexed_inode'), stale_inodes),
            'allocation_mismatch': details('''SELECT f.dev_id,f.inode,i.blocks,f.blocks
                    FROM fs_inodes f JOIN idx_inodes i USING(dev_id,inode)
                    WHERE f.blocks!=i.blocks''',
                ('dev', 'inode', 'indexed_bytes', 'filesystem_bytes'), wrong_allocations),
        }
        passed = not (error_count or missing_paths or stale_paths or wrong_paths
                      or missing_inodes or stale_inodes or wrong_allocations)
        progress('comparison_complete', force=True,
                 indexed_paths=indexed_paths, filesystem_paths=filesystem_paths,
                 missing_paths=missing_paths, stale_paths=stale_paths,
                 identity_mismatches=wrong_paths, allocation_mismatches=wrong_allocations,
                 passed=passed)
        return {
            'passed': passed,
            'walk_complete': error_count == 0,
            'indexed_path_count': indexed_paths,
            'filesystem_path_count': filesystem_paths,
            'indexed_inode_count': indexed_inodes,
            'filesystem_inode_count': filesystem_inodes,
            'missing_index_path_count': missing_paths,
            'stale_index_path_count': stale_paths,
            'identity_mismatch_count': wrong_paths,
            'missing_index_inode_count': missing_inodes,
            'stale_index_inode_count': stale_inodes,
            'allocation_mismatch_count': wrong_allocations,
            'path_samples': {
                'missing_from_index': path_samples('fs_entries', 'idx_entries', True),
                'stale_in_index': path_samples('idx_entries', 'fs_entries', True),
            },
            'allocation_samples': allocation_samples,
            'difference_details': difference_details,
            'walk_errors': errors,
            'walk_error_count': error_count,
            'comparison_storage': 'disk-backed SQLite, 16 MiB page cache',
        }
    finally:
        audit.close()
        try:
            os.unlink(audit_path)
        except FileNotFoundError:
            pass


def snapshot(check_filesystem=False, progress_callback=None):
    uri = f'file:{DB}?mode=ro'
    with sqlite3.connect(uri, uri=True, timeout=5) as conn:
        conn.execute('PRAGMA query_only=ON')
        conn.execute('BEGIN')
        meta = dict(conn.execute('SELECT key,value FROM meta'))
        index_root = Path(meta.get('last_scan_root', '/'))
        root = (int(meta['root_dev']), int(meta['root_inode']))
        totals = conn.execute('SELECT SUM(blocks),COUNT(*) FROM inodes').fetchone()
        root_totals = conn.execute('SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?', root).fetchone()
        # Find exact bad directory identities and their stored versus recomputed totals.
        rows = conn.execute('''SELECT i.dev_id,i.inode,i.recursive_bytes,
          i.blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0),
          i.recursive_inodes,
          1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
            WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
          FROM inodes i WHERE i.kind='d' AND (
            i.recursive_bytes != i.blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
              WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
            OR i.recursive_inodes != 1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
              WHERE d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0))
          LIMIT 25''').fetchall()
        mismatches = []
        for dev, ino, sb, eb, si, ei in rows:
            parts = []
            cur = (dev, ino)
            seen = set()
            while cur != root and cur not in seen and len(parts) < 512:
                seen.add(cur)
                edge = conn.execute('SELECT parent_dev,parent_inode,name FROM dirents WHERE dev_id=? AND inode=? AND prime=1 LIMIT 1', cur).fetchone()
                if edge is None:
                    break
                parent_dev, parent_ino, name = edge
                parts.append(bytes(name))
                cur = (parent_dev, parent_ino)
            parts.reverse()
            # Audit every direct prime child for ordinary fanout directories,
            # capped at 10k paths; detailed row samples remain capped at 128.
            child_count = conn.execute(
                'SELECT COUNT(*) FROM dirents WHERE parent_dev=? AND parent_inode=? AND prime=1',
                (dev, ino)).fetchone()[0]
            audit_limit = min(child_count, 10_000)
            audit_rows = conn.execute('''SELECT d.name,i.dev_id,i.inode,i.kind,i.blocks,
                    i.recursive_bytes,i.recursive_inodes,d.prime
                FROM dirents d JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode
                WHERE d.parent_dev=? AND d.parent_inode=? AND d.prime=1
                ORDER BY d.name LIMIT ?''', (dev, ino, audit_limit)).fetchall()
            child_rows = audit_rows[:128]
            directory_path = str(indexed_path(index_root, parts))
            filesystem_audit = audit_direct_children(
                directory_path,
                [(r[0], r[1], r[2], r[3], r[4]) for r in audit_rows],
                complete=child_count <= audit_limit)
            child_samples = []
            for name, cdev, cino, kind, blocks, recursive_bytes, recursive_inodes, prime in child_rows:
                child_name = bytes(name)
                child_path = str(indexed_path(index_root, parts + [child_name]))
                try:
                    st = os.lstat(child_path)
                    fs_sample = {'exists': True, 'dev': st.st_dev, 'inode': st.st_ino,
                                 'kind': ('d' if stat.S_ISDIR(st.st_mode) else
                                          'l' if stat.S_ISLNK(st.st_mode) else
                                          'f' if stat.S_ISREG(st.st_mode) else 'other'),
                                 'allocated_bytes': st.st_blocks * 512}
                except OSError as exc:
                    fs_sample = {'exists': False, 'error': str(exc)}
                child_samples.append({'name_hex': child_name.hex(),
                                      'path': child_path,
                                      'dev': cdev, 'inode': cino, 'kind': kind,
                                      'blocks': blocks, 'recursive_bytes': recursive_bytes,
                                      'recursive_inodes': recursive_inodes, 'prime': prime,
                                      'filesystem_sample': fs_sample})
            mismatches.append({'dev': dev, 'inode': ino, 'stored_bytes': sb, 'expected_bytes': eb,
                               'stored_inodes': si, 'expected_inodes': ei,
                               'path_components_hex': [p.hex() for p in parts],
                               'path': directory_path,
                               'direct_prime_child_count': child_count,
                               'direct_prime_children_truncated': child_count > len(child_rows),
                               'direct_prime_children': child_samples,
                               'direct_prime_child_filesystem_audit': filesystem_audit})
        orphan = conn.execute('SELECT COUNT(*) FROM dirents d LEFT JOIN inodes p ON p.dev_id=d.parent_dev AND p.inode=d.parent_inode WHERE p.inode IS NULL').fetchone()[0]
        primes = conn.execute('SELECT COUNT(*) FROM (SELECT i.dev_id,i.inode FROM inodes i LEFT JOIN dirents d ON d.dev_id=i.dev_id AND d.inode=i.inode GROUP BY i.dev_id,i.inode HAVING COALESCE(SUM(d.prime),0)!=1)').fetchone()[0]
        quick = conn.execute('PRAGMA quick_check').fetchall()
        result = {'schema_version': meta.get('schema_version'), 'scan_generation': meta.get('scan_generation'),
                'root': root, 'inode_totals': totals, 'root_totals': root_totals,
                'root_matches_inode_aggregate': root_totals == totals,
                'sampled_rollup_mismatches': mismatches, 'orphan_dirents': orphan,
                'invalid_prime_attribution': primes, 'quick_check': quick}
        if check_filesystem:
            result['exact_filesystem'] = exact_filesystem(index_root, conn, progress_callback)
        return result


def pipeline_caught_up(status_text):
    if isinstance(status_text, dict):
        # Prefer the versioned machine status: unlike the human pipeline summary,
        # it accounts for a background rebuild, uncertainty, and pause state.
        return (status_text.get('caught_up') is True
                and status_text.get('scan_in_progress') is False
                and status_text.get('dirty') is False
                and status_text.get('paused') is False
                and status_text.get('watch_status_available') is True
                and status_text.get('pending_updates') == 0)
    if "DIRTY since" in status_text or "IN PROGRESS" in status_text or "BEHIND" in status_text:
        return False
    # Legacy text status does not expose whether a full reconciliation is still
    # running. Do not infer catch-up from a drained event queue alone.
    if "Rescan complete; reconciling queued events" in status_text:
        return False
    pipeline = next((line for line in status_text.splitlines() if line.strip().startswith("pipeline:")), "")
    return False


def accounting_snapshot_stable(before, after):
    """Check that the accounting view stayed current across a DB read.

    Commit counters may advance for metadata-only changes; the persisted index
    structure and catch-up indicators must remain unchanged.
    """
    stable, changed = snapshot_index_structure_stable(before, after)
    return pipeline_caught_up(after) and stable, changed


def current_status():
    result = run(str(BINARY), '--db', str(DB), '--json', 'status')
    if result['returncode'] == 0:
        try:
            return json.loads(result['stdout'])
        except json.JSONDecodeError:
            pass
    # Older installed binaries print a status table even with --json. Rebuild a
    # process-validated status from their versioned /run sidecar and DB markers
    # so exact audits can distinguish an idle, stable index from a rebuild.
    watch_path = (Path('/run/dux/watch.status')
                  if Path(DB).resolve() == Path('/var/lib/dux/dux.db')
                  else Path(f'{DB}.watch.status'))
    try:
        content = watch_path.read_text()
        head, status_db = content.split('\n', 1)
        fields = head.split()
        version = int(fields[0])
        updated_at, pid, pending = map(int, fields[1:4])
        kernel_empty = fields[4] == '1'
        events_seen, events_resolved, updates_committed, updates_dropped, behind_ms, max_pending = map(int, fields[5:11])
        uncertain = fields[11] == '1' if version >= 2 and len(fields) >= 12 else False
        last_commit_at = int(fields[12]) if version >= 3 and len(fields) >= 13 else None
        canonical_db = str(Path(DB).resolve())
        os.kill(pid, 0)
        watch_fresh = 0 <= time.time() - updated_at <= 5
        watch_valid = (version in (1, 2, 3) and watch_fresh
                       and status_db.strip() == canonical_db)
    except (OSError, ValueError, IndexError):
        return result['stdout']
    if not watch_valid:
        return result['stdout']

    connection = sqlite3.connect(f'file:{DB}?mode=ro', uri=True, timeout=5)
    try:
        meta = dict(connection.execute('SELECT key,value FROM meta'))
        root = (int(meta['root_dev']), int(meta['root_inode']))
        root_bytes, root_inodes = connection.execute(
            'SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=? AND inode=?', root
        ).fetchone()
        inode_count = connection.execute('SELECT COUNT(*) FROM inodes').fetchone()[0]
    finally:
        connection.close()
    scan_path = (Path('/run/dux/scan.progress')
                 if Path(DB).resolve() == Path('/var/lib/dux/dux.db')
                 else Path(f'{DB}.scan.progress'))
    try:
        scan_in_progress = time.time() - scan_path.stat().st_mtime <= 10
    except OSError:
        scan_in_progress = False
    dirty = 'dirty_since' in meta
    paused = 'paused_since' in meta
    caught_up = (pending == 0 and kernel_empty and behind_ms == 0
                 and updates_dropped == 0 and not uncertain and not dirty
                 and not paused and not scan_in_progress)
    return {
        'status_source': f'legacy_watch_status_v{version}',
        'watch_status_available': True, 'caught_up': caught_up,
        'scan_in_progress': scan_in_progress, 'dirty': dirty, 'paused': paused,
        'pending_updates': pending, 'events_dropped': updates_dropped,
        'events_seen': events_seen, 'events_resolved': events_resolved,
        'updates_committed': updates_committed,
        'last_event_commit_timestamp_seconds': last_commit_at,
        'last_scan_timestamp_seconds': int(meta.get('last_scan_ts', '0')),
        'index_nodes': inode_count, 'index_bytes': root_bytes,
        'root': '/', 'root_recursive_inodes': root_inodes,
        'dirty_reason': meta.get('dirty_reason'),
    }


def service_diagnostics(unit):
    """Capture lifecycle state and recent journal for the observed daemon."""
    service = run('systemctl', 'show', unit, '-p', 'MainPID', '-p', 'NRestarts',
                  '-p', 'ActiveState', '-p', 'SubState', '-p', 'StatusText')
    journal = run('journalctl', '-u', unit, '--since', '10 minutes ago',
                  '--no-pager', '-o', 'short-iso-precise')
    return service, journal


def invariant_passed(observed):
    quick_check = observed["quick_check"]
    exact = observed.get('exact_filesystem', {'passed': True})
    return (observed["root_matches_inode_aggregate"]
            and not observed["sampled_rollup_mismatches"]
            and observed["orphan_dirents"] == 0
            and observed["invalid_prime_attribution"] == 0
            and exact.get('passed') is True
            and len(quick_check) == 1
            and tuple(quick_check[0]) == ("ok",))


def snapshot_status_stable(before, after):
    """Whether index state stayed fixed while a potentially long tree walk ran."""
    if not isinstance(before, dict) or not isinstance(after, dict):
        return False, ['machine_status_unavailable']
    # Event counters may advance for transient/coalesced operations that do not
    # change the committed index. Persisted-state counters and loss/backlog
    # indicators determine whether the DB snapshot moved during the filesystem
    # walk; keep event counts in the report as activity telemetry only.
    fields = ('caught_up', 'dirty', 'paused', 'scan_in_progress', 'pending_updates',
              'events_dropped', 'updates_committed',
              'last_event_commit_timestamp_seconds', 'last_scan_timestamp_seconds',
              'index_nodes', 'index_bytes', 'root')
    changed = [field for field in fields if before.get(field) != after.get(field)]
    return not changed, changed


def snapshot_index_structure_stable(before, after):
    """Whether coverage and materialized path/allocation totals stayed stable.

    Event/commit counters can move for metadata-only operations. An exact path,
    inode and allocation comparison can validate those operations without
    requiring the event counters themselves to freeze.
    """
    if not isinstance(before, dict) or not isinstance(after, dict):
        return False, ['machine_status_unavailable']
    fields = ('caught_up', 'dirty', 'paused', 'scan_in_progress', 'pending_updates',
              'events_dropped', 'index_nodes', 'index_bytes', 'root')
    changed = [field for field in fields if before.get(field) != after.get(field)]
    return not changed, changed


def classify_exact_audit(before, after, observed):
    """Classify one whole-root comparison with before/after pipeline evidence."""
    stable, changed = snapshot_status_stable(before, after)
    structure_stable, structure_changed = snapshot_index_structure_stable(before, after)
    caught_up_after = pipeline_caught_up(after)
    exact_passed = invariant_passed(observed)
    passed = structure_stable and caught_up_after and exact_passed
    if not structure_stable or not caught_up_after:
        result = 'SNAPSHOT_UNSTABLE'
    elif exact_passed:
        result = 'CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS'
    elif stable:
        result = 'CAUGHT_UP_BUT_ACCOUNTING_INVARIANTS_FAIL'
    else:
        # A failed comparison can be caused by a path changing while the
        # walker traverses it. If commits moved, preserve it as inconclusive.
        result = 'SNAPSHOT_UNSTABLE'
    return {'audit_result': result, 'passed': passed,
            'snapshot_stable': stable, 'snapshot_changed_fields': changed,
            'snapshot_structure_stable': structure_stable,
            'snapshot_structure_changed_fields': structure_changed,
            'activity_during_audit': not stable,
            'caught_up_after_snapshot': caught_up_after,
            'accounting_invariants_passed': exact_passed}


def wait_for_stable_catchup(timeout, stable_seconds, status_reader=None,
                            now=time.monotonic, sleep=time.sleep, poll_seconds=1,
                            progress_callback=None):
    """Wait for a continuously caught-up pipeline with unchanged index state."""
    read_status = status_reader or current_status
    started = now()
    deadline = started + timeout
    baseline = None
    stable_since = None
    last_status = None
    last_changed = []
    last_progress_at = None

    def report_progress(current_time, phase, force=False):
        nonlocal last_progress_at
        if progress_callback is None:
            return
        if not force and last_progress_at is not None and current_time - last_progress_at < 10:
            return
        progress_callback({
            'phase': phase,
            'elapsed_seconds': round(current_time - started, 3),
            'stable_window_seconds_required': stable_seconds,
            'stable_window_seconds_observed': (round(current_time - stable_since, 3)
                                               if stable_since is not None else 0),
            'pipeline_caught_up': pipeline_caught_up(last_status),
            'changed_fields': list(last_changed),
            'index_nodes': (last_status.get('index_nodes')
                            if isinstance(last_status, dict) else None),
            'index_bytes': (last_status.get('index_bytes')
                            if isinstance(last_status, dict) else None),
            'pending_updates': (last_status.get('pending_updates')
                                if isinstance(last_status, dict) else None),
            'events_dropped': (last_status.get('events_dropped')
                               if isinstance(last_status, dict) else None),
        })
        last_progress_at = current_time

    while True:
        current = read_status()
        last_status = current
        current_time = now()
        if not pipeline_caught_up(current):
            baseline = None
            stable_since = None
            last_changed = ['pipeline_not_caught_up']
        elif baseline is None:
            baseline = current
            stable_since = current_time
            last_changed = []
            if stable_seconds <= 0:
                report_progress(current_time, 'stable_catchup_complete', force=True)
                return last_status, True, []
        else:
            stable, changed = snapshot_index_structure_stable(baseline, current)
            if stable:
                if current_time - stable_since >= stable_seconds:
                    report_progress(current_time, 'stable_catchup_complete', force=True)
                    return last_status, True, []
            else:
                baseline = current
                stable_since = current_time
                last_changed = changed
        if current_time >= deadline:
            report_progress(current_time, 'stable_catchup_timeout', force=True)
            return last_status, False, last_changed
        report_progress(current_time, 'waiting_for_stable_catchup')
        sleep(min(poll_seconds, max(0, deadline - current_time)))


def classify_snapshots(initial, recovered, initial_stable=True, require_exact=False):
    """Keep a transient initial accounting failure visible to operators."""
    if not invariant_passed(recovered):
        return "CAUGHT_UP_BUT_ACCOUNTING_INVARIANTS_FAIL", 1
    if require_exact:
        exact = initial.get('exact_filesystem', {'passed': False})
        if not initial_stable:
            return 'SNAPSHOT_UNSTABLE', 1
        if not exact.get('passed'):
            return 'CAUGHT_UP_BUT_ACCOUNTING_INVARIANTS_FAIL', 1
        return "CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS", 0
    if initial_stable and not invariant_passed(initial):
        return "CAUGHT_UP_AFTER_TRANSIENT_ACCOUNTING_MISMATCH", 2
    return "CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS", 0


def status_tail(status):
    """Return a concise status excerpt for either JSON or legacy text output."""
    if isinstance(status, dict):
        return status
    return status.splitlines()[-5:]


def audit_direct_children(directory, children, complete):
    """Compare a bounded set of DB children with current path identities."""
    audit = {'checked': 0, 'complete': complete, 'missing': [],
             'identity_or_kind_mismatch': [], 'allocation_mismatch': [],
             'stat_errors': []}
    for name, dev, ino, kind, blocks in children:
        child_name = bytes(name)
        child_path = os.path.join(directory, os.fsdecode(child_name))
        audit['checked'] += 1
        try:
            st = os.lstat(child_path)
        except FileNotFoundError:
            audit['missing'].append({'name_hex': child_name.hex(), 'path': child_path,
                                     'dev': dev, 'inode': ino, 'kind': kind,
                                     'blocks': blocks})
            continue
        except OSError as exc:
            audit['stat_errors'].append({'name_hex': child_name.hex(), 'path': child_path,
                                         'error': str(exc)})
            continue
        fs_kind = ('d' if stat.S_ISDIR(st.st_mode) else
                   'l' if stat.S_ISLNK(st.st_mode) else
                   'f' if stat.S_ISREG(st.st_mode) else 'other')
        fs_blocks = st.st_blocks * 512
        if (st.st_dev, st.st_ino, fs_kind) != (dev, ino, kind):
            audit['identity_or_kind_mismatch'].append(
                {'name_hex': child_name.hex(), 'path': child_path,
                 'db': {'dev': dev, 'inode': ino, 'kind': kind, 'blocks': blocks},
                 'filesystem': {'dev': st.st_dev, 'inode': st.st_ino,
                                'kind': fs_kind, 'blocks': fs_blocks}})
        elif fs_blocks != blocks:
            audit['allocation_mismatch'].append(
                {'name_hex': child_name.hex(), 'path': child_path,
                 'db_blocks': blocks, 'filesystem_blocks': fs_blocks})
    for key in ('missing', 'identity_or_kind_mismatch', 'allocation_mismatch', 'stat_errors'):
        audit[key + '_count'] = len(audit[key])
        audit[key] = audit[key][:200]
    return audit


def main():
    global BINARY, SERVICE_BINARY, DB, OUTPUT, SERVICE_UNIT, PROGRESS_OUTPUT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=BINARY)
    parser.add_argument('--service-binary', type=Path, default=SERVICE_BINARY,
                        help='installed daemon executable to identify in the report')
    parser.add_argument('--service-unit', default='dux.service',
                        help='systemd unit to capture in service/journal evidence')
    parser.add_argument('--db', type=Path, default=DB)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--progress-output', type=Path,
                        help='atomic progress sidecar for long exact filesystem audits '
                             '(default: <output>.progress.json)')
    parser.add_argument('--wait-for-dirty', action='store_true')
    parser.add_argument('--trigger-timeout', type=int, default=600)
    parser.add_argument('--catchup-timeout', type=int, default=600)
    parser.add_argument('--exact-filesystem', action='store_true',
                        help='compare every indexed path, inode, and allocation with a filesystem walk')
    parser.add_argument('--stable-catchup-before-exact', type=float, default=None,
                        metavar='SECONDS',
                        help='for one exact audit, optionally require an unchanged caught-up window (0 starts immediately)')
    args = parser.parse_args()
    BINARY, SERVICE_BINARY, DB, OUTPUT, SERVICE_UNIT = (
        args.binary, args.service_binary, args.db, args.output, args.service_unit)
    PROGRESS_OUTPUT = args.progress_output or default_progress_path(args.output)
    progress_binary_sha256 = None

    def write_progress(state):
        nonlocal progress_binary_sha256
        if progress_binary_sha256 is None:
            progress_binary_sha256 = hashlib.sha256(BINARY.read_bytes()).hexdigest()
        PROGRESS_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        payload = {'updated_at_utc': datetime.datetime.now(
            datetime.timezone.utc).isoformat(), 'binary_sha256': progress_binary_sha256,
            **state}
        write_progress_file(PROGRESS_OUTPUT, payload)

    if args.stable_catchup_before_exact is not None:
        if not args.exact_filesystem:
            parser.error('--stable-catchup-before-exact requires --exact-filesystem')
        status, ready, wait_changed = wait_for_stable_catchup(
            args.catchup_timeout, args.stable_catchup_before_exact,
            progress_callback=write_progress)
        captured = datetime.datetime.now(datetime.timezone.utc).isoformat()
        report = {'captured_at_utc': captured,
                  'binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(),
                  'service_binary_sha256': hashlib.sha256(SERVICE_BINARY.read_bytes()).hexdigest(),
                  'observer_sha256': OBSERVER_SHA256,
                  'service_unit': SERVICE_UNIT,
                  'exact_filesystem_audit': True,
                  'stable_catchup_window_seconds': args.stable_catchup_before_exact,
                  'stable_catchup_observed': ready,
                  'stable_catchup_wait_changed_fields': wait_changed,
                  'status_before_snapshot': status}
        if not ready:
            service, journal = service_diagnostics(SERVICE_UNIT)
            report.update({'audit_result': 'STABLE_CATCHUP_TIMEOUT',
                           'service': service, 'journal_10m': journal})
            OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
            print('STABLE_CATCHUP_TIMEOUT', status_tail(status), flush=True)
            return 1
        observed = snapshot(True, write_progress)
        after = current_status()
        audit = classify_exact_audit(status, after, observed)
        service, journal = service_diagnostics(SERVICE_UNIT)
        report.update({'snapshot': observed, 'status_after_snapshot': after,
                       **audit,
                       'service': service, 'journal_10m': journal,
                       })
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
        print(audit['audit_result'], captured, flush=True)
        return 0 if audit['passed'] else 1

    if args.wait_for_dirty:
        deadline = time.monotonic() + args.trigger_timeout
        while time.monotonic() < deadline:
            status = current_status()
            if ((isinstance(status, dict) and (status.get('dirty') or status.get('scan_in_progress')))
                    or (isinstance(status, str) and ('DIRTY since' in status or 'IN PROGRESS' in status))):
                break
            time.sleep(2)
        else:
            print(f'NO_DIRTY_OR_REBUILD_OBSERVED_WITHIN_{args.trigger_timeout}s', flush=True)
            return 2
    else:
        status = current_status()
    captured = datetime.datetime.now(datetime.timezone.utc).isoformat()
    observed = snapshot(args.exact_filesystem,
                        write_progress if args.exact_filesystem else None)
    initial_status_after_snapshot = current_status()
    initial_snapshot_stable, initial_changed_fields = snapshot_status_stable(
        status, initial_status_after_snapshot)
    service, journal = service_diagnostics(SERVICE_UNIT)
    report = {'captured_at_utc': captured, 'binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(),
              'service_binary_sha256': hashlib.sha256(SERVICE_BINARY.read_bytes()).hexdigest(),
              'observer_sha256': OBSERVER_SHA256,
              'service_unit': SERVICE_UNIT,
              'exact_filesystem_audit': args.exact_filesystem,
              'initial_status_after_snapshot': initial_status_after_snapshot,
              'initial_snapshot_stable': initial_snapshot_stable,
              'initial_snapshot_changed_fields': initial_changed_fields,
              'status': status, 'service': service, 'journal_10m': journal, 'snapshot': observed,
              'initial_accounting_invariants_passed': invariant_passed(observed)}
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'captured_at_utc': captured, 'status': status_tail(status),
                      'sampled_mismatches': observed['sampled_rollup_mismatches'],
                      'root_matches_inode_aggregate': observed['root_matches_inode_aggregate'],
                      'report': str(OUTPUT)}, indent=2), flush=True)
    # A cleared DIRTY flag is insufficient: require zero pending and the
    # explicit caught-up marker, then verify the database invariants again.
    # The expensive filesystem comparison above is a single read-only pass.
    # Repeating a full-root walk each time this two-second status loop observes
    # caught-up state can churn host I/O indefinitely when the index keeps
    # changing; the recovery check below is database-only.
    recovery_deadline = time.monotonic() + args.catchup_timeout
    recovery_checked = False
    while time.monotonic() < recovery_deadline:
        before = current_status()
        if pipeline_caught_up(before) and not recovery_checked:
            recovery_checked = True
            recovery = snapshot(False)
            status = current_status()
            strict_stable, strict_changed_fields = snapshot_status_stable(before, status)
            structure_stable, structure_changed_fields = accounting_snapshot_stable(
                before, status)
            if structure_stable:
                report['caught_up_at_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
                report['caught_up_status'] = status
                report['caught_up_snapshot'] = recovery
                report['caught_up_snapshot_stable'] = True
                report['caught_up_snapshot_changed_fields'] = structure_changed_fields
                report['caught_up_status_strictly_stable'] = strict_stable
                report['caught_up_status_changed_fields'] = strict_changed_fields
                report['accounting_invariants_passed_after_catchup'] = invariant_passed(recovery)
                OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
                result, exit_code = classify_snapshots(
                    observed, recovery, initial_snapshot_stable,
                    require_exact=args.exact_filesystem)
                report['accounting_result'] = result
                OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
                print(result, report['caught_up_at_utc'], flush=True)
                return exit_code
            report['last_unstable_snapshot_changed_fields'] = structure_changed_fields
        time.sleep(2)
    report['catchup_timeout'] = True
    report['last_status'] = current_status()
    OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
    print('CATCHUP_TIMEOUT', status_tail(report['last_status']), flush=True)
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
