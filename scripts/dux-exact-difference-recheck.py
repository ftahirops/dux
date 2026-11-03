#!/usr/bin/env python3
"""Recheck recorded exact-audit differences against a later live FS and index.

This classifies old differences as absent on both sides, still present on both,
or persistent one-sided/identity/allocation mismatches. It is diagnostic only:
the later check is not a replacement for a stable whole-root exact audit.
"""
import argparse
from collections import Counter
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import stat


def recheck(source, db_path, root):
    exact = source['snapshot']['exact_filesystem']
    details = exact['difference_details']
    conn = sqlite3.connect(f'file:{db_path}?mode=ro', uri=True, timeout=10)
    conn.execute('PRAGMA query_only=ON')
    conn.execute('BEGIN')
    meta = dict(conn.execute("SELECT key,value FROM meta WHERE key IN ('root_dev','root_inode')"))
    root_id = (int(meta['root_dev']), int(meta['root_inode']))
    root_bytes = os.fsencode(root)
    path_cache = {root_id: root_bytes}

    observer_path = Path(__file__).with_name('dux-live-observer.py')
    spec = importlib.util.spec_from_file_location('dux_live_observer_for_recheck', observer_path)
    observer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(observer)

    def inode_path(identity):
        """Resolve the indexed prime path for an inode, stopping at this root."""
        identity = tuple(identity)
        if identity in path_cache:
            return path_cache[identity]
        chain = []
        seen = set()
        current = identity
        while current not in path_cache:
            if current in seen or len(seen) > 4096:
                return None
            seen.add(current)
            row = conn.execute('''SELECT parent_dev,parent_inode,name FROM dirents
                WHERE dev_id=? AND inode=? AND prime=1 LIMIT 1''', current).fetchone()
            if row is None:
                return None
            parent = (row[0], row[1])
            chain.append((current, parent, bytes(row[2])))
            current = parent
        path = path_cache[current]
        for child, _parent, name in reversed(chain):
            path = os.path.join(path, name)
            path_cache[child] = path
        return path_cache.get(identity)

    def path_state(parent_dev, parent_inode, name_hex, expected=None):
        name = bytes.fromhex(name_hex)
        parent_path = inode_path((parent_dev, parent_inode))
        if parent_path is None:
            state = ('parent_absent_from_current_fs_and_index'
                     if (parent_dev, parent_inode) in unresolved_parents
                     else 'parent_not_indexed')
            return {'state': state}
        path = os.path.join(parent_path, name)
        try:
            st = os.lstat(path)
        except FileNotFoundError:
            st = None
        except OSError as error:
            return {'state': 'stat_error', 'errno': error.errno}
        row = conn.execute('''SELECT dev_id,inode FROM dirents
            WHERE parent_dev=? AND parent_inode=? AND name=?''',
            (parent_dev, parent_inode, sqlite3.Binary(name))).fetchone()
        if st is None and row is None:
            state = 'absent_from_both'
        elif st is not None and row is not None:
            current = (st.st_dev, st.st_ino)
            indexed = (row[0], row[1])
            if current != indexed:
                state = 'identity_mismatch_now'
            elif expected and current != (expected[0], expected[1]):
                state = 'identity_changed_but_consistent_now'
            else:
                state = 'present_and_indexed'
        elif st is not None:
            state = 'present_missing_from_index'
        else:
            state = 'stale_index_entry'
        return {'state': state}

    categories = {}
    recorded_paths = [row for key in ('missing_from_index', 'stale_in_index',
                                       'identity_mismatch')
                      for row in details.get(key, {}).get('rows', [])]
    parent_counts = Counter((row['parent_dev'], row['parent_inode'])
                            for row in recorded_paths)
    unresolved_parents = {
        (row['parent_dev'], row['parent_inode']) for row in recorded_paths
        if inode_path((row['parent_dev'], row['parent_inode'])) is None
    }
    directory_walk = {'directories_seen': 0, 'errors': 0, 'unresolved_after_walk': 0}
    if unresolved_parents:
        mounts, root_is_pseudo = observer.mount_policy(
            root, observer.parse_mountinfo(Path('/proc/self/mountinfo').read_text()))

        def walk_error(_error):
            directory_walk['errors'] += 1

        visited = {root_id}
        for directory, dirs, _files in os.walk(root, topdown=True, followlinks=False,
                                                onerror=walk_error):
            directory_walk['directories_seen'] += 1
            current_path = os.fsencode(directory)
            try:
                st = os.lstat(directory)
            except OSError:
                dirs[:] = []
                directory_walk['errors'] += 1
                continue
            identity = (st.st_dev, st.st_ino)
            if identity in unresolved_parents:
                path_cache[identity] = current_path
                unresolved_parents.remove(identity)
            for name in list(dirs):
                child_path = Path(directory) / name
                if observer.index_artifact(child_path, db_path) or child_path in mounts:
                    dirs.remove(name)
                    continue
                try:
                    child_st = os.lstat(child_path)
                except OSError:
                    dirs.remove(name)
                    directory_walk['errors'] += 1
                    continue
                if not stat.S_ISDIR(child_st.st_mode):
                    dirs.remove(name)
                    continue
                if identity == root_id and root_is_pseudo:
                    dirs.remove(name)
                    continue
                child_identity = (child_st.st_dev, child_st.st_ino)
                if child_identity in visited:
                    dirs.remove(name)
                else:
                    visited.add(child_identity)
        directory_walk['unresolved_after_walk'] = len(unresolved_parents)
    for category in ('missing_from_index', 'stale_in_index', 'identity_mismatch'):
        detail = details.get(category, {'rows': [], 'count': 0, 'truncated': False})
        counts = {}
        examples = []
        for row in detail.get('rows', []):
            expected = None
            if category == 'missing_from_index':
                expected = (row['filesystem_dev'], row['filesystem_inode'])
            elif category == 'stale_in_index':
                expected = (row['indexed_dev'], row['indexed_inode'])
            else:
                expected = (row['filesystem_dev'], row['filesystem_inode'])
            result = path_state(row['parent_dev'], row['parent_inode'],
                                row['name_hex'], expected)
            counts[result['state']] = counts.get(result['state'], 0) + 1
            if len(examples) < 20:
                examples.append({**{k: row[k] for k in
                    ('parent_dev', 'parent_inode', 'name_hex')}, **result})
        categories[category] = {
            'source_count': detail.get('count', len(detail.get('rows', []))),
            'rechecked_count': len(detail.get('rows', [])),
            'source_truncated': detail.get('truncated', False),
            'states': counts,
            'examples': examples,
        }

    detail = details.get('allocation_mismatch', {'rows': [], 'count': 0, 'truncated': False})
    counts = {}
    examples = []
    for row in detail.get('rows', []):
        identity = (row['dev'], row['inode'])
        indexed = conn.execute('SELECT blocks FROM inodes WHERE dev_id=? AND inode=?', identity).fetchone()
        path = inode_path(identity)
        if path is None or indexed is None:
            state = 'inode_not_indexed_now'
            current_bytes = None
        else:
            try:
                st = os.lstat(path)
                if (st.st_dev, st.st_ino) != identity:
                    state, current_bytes = 'path_identity_changed', None
                else:
                    current_bytes = st.st_blocks * 512
                    state = ('allocation_matches_now' if indexed[0] == current_bytes
                             else 'allocation_mismatch_now')
            except FileNotFoundError:
                state, current_bytes = 'inode_path_absent_now', None
            except OSError as error:
                state, current_bytes = f'stat_error_{error.errno}', None
        counts[state] = counts.get(state, 0) + 1
        if len(examples) < 20:
            examples.append({'dev': row['dev'], 'inode': row['inode'], 'state': state,
                             'old_indexed_bytes': row['indexed_bytes'],
                             'old_filesystem_bytes': row['filesystem_bytes'],
                             'current_indexed_bytes': indexed[0] if indexed else None,
                             'current_filesystem_bytes': current_bytes})
    categories['allocation_mismatch'] = {
        'source_count': detail.get('count', len(detail.get('rows', []))),
        'rechecked_count': len(detail.get('rows', [])),
        'source_truncated': detail.get('truncated', False),
        'states': counts,
        'examples': examples,
    }
    parent_rows = []
    parent_summary = Counter()
    for (dev, inode), old_child_paths in sorted(parent_counts.items()):
        path = inode_path((dev, inode))
        inode_row = conn.execute('SELECT kind FROM inodes WHERE dev_id=? AND inode=?',
                                 (dev, inode)).fetchone()
        child_count = conn.execute('SELECT COUNT(*) FROM dirents WHERE parent_dev=? AND parent_inode=?',
                                   (dev, inode)).fetchone()[0]
        current = None
        try:
            current = os.lstat(path) if path is not None else None
            mode = current.st_mode if current else None
            current_identity = [current.st_dev, current.st_ino] if current else None
        except OSError as error:
            mode, current_identity = None, None
            state = f'path_stat_error_{error.errno}'
        else:
            if path is None or inode_row is None:
                state = 'not_indexed_now'
            elif current is None:
                state = 'path_missing_now'
            elif not stat.S_ISDIR(current.st_mode):
                state = 'inode_reused_as_non_directory'
            elif inode_row[0] == 'd':
                state = 'directory_present_now'
            else:
                state = 'index_kind_mismatch_now'
        parent_summary[state] += old_child_paths
        parent_rows.append({'dev': dev, 'inode': inode, 'source_child_path_count': old_child_paths,
                            'current_path': os.fsdecode(path) if path is not None else None,
                            'current_identity': current_identity,
                            'current_index_kind': inode_row[0] if inode_row else None,
                            'current_filesystem_mode': oct(mode) if mode is not None else None,
                            'current_indexed_child_dirents': child_count, 'state': state})
    conn.commit()
    conn.close()
    return {
        'captured_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'source_audit_captured_at_utc': source.get('captured_at_utc'),
        'source_audit_sha256': None,
        'database': str(db_path), 'root': str(root),
        'interpretation': 'later live recheck only; cannot prove the source snapshot was stable',
        'directory_walk': directory_walk,
        'source_parent_inodes': {'unique_count': len(parent_rows),
                                 'old_child_path_state_counts': dict(parent_summary),
                                 'rows': parent_rows},
        'categories': categories,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-audit', type=Path, required=True)
    parser.add_argument('--db', type=Path, required=True)
    parser.add_argument('--root', type=Path, default=Path('/'))
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    source_bytes = args.source_audit.read_bytes()
    source = json.loads(source_bytes)
    report = recheck(source, args.db, args.root)
    report['source_audit_sha256'] = hashlib.sha256(source_bytes).hexdigest()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({key: value['states'] for key, value in report['categories'].items()}, indent=2))


if __name__ == '__main__':
    main()
