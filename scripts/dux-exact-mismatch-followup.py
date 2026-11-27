#!/usr/bin/env python3
"""Repeatedly recheck selected differences from an unstable exact-root audit."""
import argparse
from collections import Counter
import datetime
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time


def sample(source, db, root, binary):
    exact = source['snapshot']['exact_filesystem']
    detail = exact['difference_details']
    conn = sqlite3.connect(f'file:{db}?mode=ro', uri=True, timeout=5)
    conn.execute('BEGIN')
    meta = dict(conn.execute("SELECT key,value FROM meta WHERE key IN ('root_dev','root_inode')"))
    root_id = (int(meta['root_dev']), int(meta['root_inode']))
    cache = {root_id: os.fsencode(root)}

    def path_for(identity):
        identity = tuple(identity)
        if identity in cache:
            return cache[identity]
        seen, chain, current = set(), [], identity
        while current not in cache:
            if current in seen or len(seen) > 4096:
                return None
            seen.add(current)
            row = conn.execute('''SELECT parent_dev,parent_inode,name FROM dirents
                WHERE dev_id=? AND inode=? AND prime=1 LIMIT 1''', current).fetchone()
            if row is None:
                return None
            parent = (row[0], row[1])
            chain.append((current, bytes(row[2])))
            current = parent
        path = cache[current]
        for child, name in reversed(chain):
            path = os.path.join(path, name)
            cache[child] = path
        return cache[identity]

    identity_rows = []
    for row in detail.get('identity_mismatch', {}).get('rows', []):
        path = path_for((row['parent_dev'], row['parent_inode']))
        path = os.path.join(path, bytes.fromhex(row['name_hex'])) if path else None
        current_index = conn.execute('''SELECT dev_id,inode FROM dirents
            WHERE parent_dev=? AND parent_inode=? AND name=?''',
            (row['parent_dev'], row['parent_inode'],
             sqlite3.Binary(bytes.fromhex(row['name_hex'])))).fetchone()
        try:
            st = os.lstat(path) if path else None
            current_fs = [st.st_dev, st.st_ino] if st else None
        except OSError as error:
            current_fs = None
            state = f'stat_error_{error.errno}'
        else:
            idx = list(current_index) if current_index else None
            state = ('parent_unresolved' if path is None else
                     'absent_from_both' if current_fs is None and idx is None else
                     'filesystem_only' if current_fs is not None and idx is None else
                     'index_only' if current_fs is None else
                     'consistent_now' if current_fs == idx else 'identity_mismatch_now')
        identity_rows.append({'path': os.fsdecode(path) if path else None,
                              'state': state, 'indexed': list(current_index) if current_index else None,
                              'filesystem': current_fs})

    allocation_rows = []
    for row in detail.get('allocation_mismatch', {}).get('rows', []):
        identity = (row['dev'], row['inode'])
        path = path_for(identity)
        indexed = conn.execute('SELECT blocks FROM inodes WHERE dev_id=? AND inode=?', identity).fetchone()
        try:
            st = os.lstat(path) if path else None
            fs_blocks = st.st_blocks * 512 if st else None
            if st and (st.st_dev, st.st_ino) != identity:
                state = 'path_identity_changed'
            elif path is None or indexed is None or st is None:
                state = 'inode_unresolved_or_absent'
            else:
                state = 'consistent_now' if indexed[0] == fs_blocks else 'allocation_mismatch_now'
        except OSError as error:
            state, fs_blocks = f'stat_error_{error.errno}', None
        allocation_rows.append({'dev': identity[0], 'inode': identity[1],
                                'path': os.fsdecode(path) if path else None, 'state': state,
                                'indexed_bytes': indexed[0] if indexed else None,
                                'filesystem_bytes': fs_blocks})

    status_run = subprocess.run([str(binary), '--db', str(db), 'status', '--json'],
                                capture_output=True, text=True, timeout=10)
    status = None
    if status_run.returncode == 0:
        try:
            status = json.loads(status_run.stdout)
        except json.JSONDecodeError:
            pass
    if isinstance(status, dict) and (
            status.get('scan_first') is True
            or status.get('behind_seconds_applicable') is False):
        # Keep diagnostics from presenting the event-drain clock as scan-first
        # freshness. The scan age is intentionally reported by the native
        # status command, but this report only carries the event backlog field.
        status = dict(status)
        status['behind_seconds'] = None
        status['behind_seconds_applicable'] = False
        status['scan_first'] = True
    # Installed legacy builds may print the human status table and return 0
    # even with --json. Reconstruct the small status subset from the daemon's
    # versioned tmpfs heartbeat and the same read-only DB transaction instead
    # of failing the entire path/allocation recheck.
    if not isinstance(status, dict):
        watch_path = (Path('/run/dux/watch.status')
                      if Path(db).resolve() == Path('/var/lib/dux/dux.db')
                      else Path(f'{db}.watch.status'))
        try:
            header, status_db = watch_path.read_text().split('\n', 1)
            fields = header.split()
            version = int(fields[0])
            updated_at, pid, pending = map(int, fields[1:4])
            kernel_empty = fields[4] == '1'
            events_seen, events_resolved, updates_committed, dropped, behind_ms, max_pending = map(
                int, fields[5:11])
            uncertain = fields[11] == '1' if version >= 2 and len(fields) >= 12 else False
            scan_first = fields[14] == '1' if version >= 5 and len(fields) >= 15 else False
            os.kill(pid, 0)
            fresh = 0 <= time.time() - updated_at <= 5
            valid = (version in (1, 2, 3, 4, 5) and fresh
                     and status_db.strip() == str(Path(db).resolve()))
            meta = dict(conn.execute('SELECT key,value FROM meta'))
            scan_path = (Path('/run/dux/scan.progress')
                         if Path(db).resolve() == Path('/var/lib/dux/dux.db')
                         else Path(f'{db}.scan.progress'))
            try:
                scanning = time.time() - scan_path.stat().st_mtime <= 10
            except OSError:
                scanning = False
            if valid:
                dirty, paused = 'dirty_since' in meta, 'paused_since' in meta
                last_scan_ts = int(meta.get('last_scan_ts', '0'))
                status = {
                    'caught_up': (pending == 0 and kernel_empty and behind_ms == 0
                                  and dropped == 0 and not uncertain and not dirty
                                  and not paused and not scanning and not scan_first),
                    'dirty': dirty, 'pending_updates': pending,
                    'events_dropped': dropped,
                    'behind_seconds': None if scan_first else behind_ms / 1000,
                    'behind_seconds_applicable': not scan_first,
                    'scan_first': scan_first,
                    'last_scan_age_seconds': (
                        max(0, int(time.time()) - last_scan_ts) if last_scan_ts > 0 else None
                    ),
                    'updates_committed': updates_committed,
                    'status_source': f'legacy_watch_status_v{version}',
                }
        except (OSError, ValueError, IndexError, ProcessLookupError):
            pass
    if not isinstance(status, dict):
        status = {'status_error': (status_run.stderr or status_run.stdout)[-500:],
                  'returncode': status_run.returncode}
    conn.commit()
    conn.close()
    return {
        'captured_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'status': {key: status.get(key) for key in
                   ('caught_up', 'dirty', 'pending_updates', 'events_dropped',
                    'behind_seconds', 'behind_seconds_applicable', 'scan_first',
                    'last_scan_age_seconds', 'updates_committed')},
        'identity_mismatch': identity_rows,
        'allocation_mismatch': allocation_rows,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-audit', type=Path, required=True)
    parser.add_argument('--db', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--root', type=Path, default=Path('/'))
    parser.add_argument('--samples', type=int, default=5)
    parser.add_argument('--interval-seconds', type=float, default=2)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = args.source_audit.read_bytes()
    source = json.loads(raw)
    report = {'source_audit': str(args.source_audit),
              'source_audit_sha256': hashlib.sha256(raw).hexdigest(),
              'candidate_binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'samples': []}
    for index in range(args.samples):
        report['samples'].append(sample(source, args.db, args.root, args.binary))
        if index + 1 < args.samples:
            time.sleep(args.interval_seconds)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'sample_count': len(report['samples']),
                      'identity_state_counts': [dict(Counter(row['state'] for row in s['identity_mismatch']))
                                                for s in report['samples']],
                      'allocation_state_counts': [
                          {state: sum(1 for row in s['allocation_mismatch'] if row['state'] == state)
                           for state in sorted({row['state'] for row in s['allocation_mismatch']})}
                          for s in report['samples']],
                      'output': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
