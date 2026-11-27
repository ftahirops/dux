#!/usr/bin/env python3
"""Scan one mounted subtree and prove exact path, inode, and allocation parity."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import subprocess
import time


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def walk_filesystem(root):
    found = {}
    errors = []
    root_bytes = os.fsencode(root)
    root_stat = os.lstat(root_bytes)
    found[b''] = (root_stat.st_dev, root_stat.st_ino,
                  root_stat.st_blocks * 512, 'd')
    stack = [(root_bytes, b'')]
    while stack:
        directory, relative = stack.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    name = entry.name if isinstance(entry.name, bytes) else os.fsencode(entry.name)
                    child_rel = name if not relative else relative + b'/' + name
                    child = directory + b'/' + name
                    try:
                        st = os.lstat(child)
                    except OSError as exc:
                        errors.append({'path_hex': child_rel.hex(), 'error': str(exc)})
                        continue
                    kind = ('l' if stat.S_ISLNK(st.st_mode) else
                            'd' if stat.S_ISDIR(st.st_mode) else
                            'f' if stat.S_ISREG(st.st_mode) else 'o')
                    found[child_rel] = (st.st_dev, st.st_ino, st.st_blocks * 512, kind)
                    if kind == 'd':
                        stack.append((child, child_rel))
        except OSError as exc:
            errors.append({'path_hex': relative.hex(), 'error': str(exc)})
    return found, errors


def walk_index(database):
    con = sqlite3.connect(f'file:{database}?mode=ro', uri=True, timeout=30)
    con.execute('PRAGMA query_only=ON')
    con.execute('BEGIN')
    root_dev = int(con.execute("SELECT value FROM meta WHERE key='root_dev'").fetchone()[0])
    root_ino = int(con.execute("SELECT value FROM meta WHERE key='root_inode'").fetchone()[0])
    inodes = {(d, i): (b, k) for d, i, b, k in con.execute('SELECT dev_id,inode,blocks,kind FROM inodes')}
    children = {}
    for pd, pi, name, d, i in con.execute(
            'SELECT parent_dev,parent_inode,name,dev_id,inode FROM dirents '
            'WHERE NOT (parent_dev=dev_id AND parent_inode=inode)'):
        children.setdefault((pd, pi), []).append((bytes(name), d, i))
    indexed = {b'': (root_dev, root_ino, inodes[(root_dev, root_ino)][0], 'd')}
    stack = [(root_dev, root_ino, b'')]
    errors = []
    while stack:
        pd, pi, parent_rel = stack.pop()
        for name, dev, ino in children.get((pd, pi), ()):
            rel = name if not parent_rel else parent_rel + b'/' + name
            inode = inodes.get((dev, ino))
            if inode is None:
                errors.append({'path_hex': rel.hex(), 'error': 'dirent target inode missing'})
                continue
            blocks, kind = inode
            indexed[rel] = (dev, ino, blocks, kind)
            if kind == 'd':
                stack.append((dev, ino, rel))
    return indexed, errors, con


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--db', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=2400)
    parser.add_argument('--jobs', type=int, default=1)
    parser.add_argument('--verify-existing-db', action='store_true',
                        help='verify the filesystem subtree against an already completed scan database')
    parser.add_argument('--index-prefix',
                        help='path prefix within the existing database root, required with --verify-existing-db')
    args = parser.parse_args()
    if args.verify_existing_db and not args.index_prefix:
        parser.error('--verify-existing-db requires --index-prefix')
    started = time.monotonic()
    command = [str(args.binary), '--db', str(args.db), 'scan', str(args.root),
               '--jobs', str(args.jobs), '--quiet']
    run = None
    if not args.verify_existing_db:
        args.db.parent.mkdir(parents=True, exist_ok=True)
        for suffix in ('', '-wal', '-shm', '.new', '.new-wal', '.new-shm'):
            Path(str(args.db) + suffix).unlink(missing_ok=True)
        run = subprocess.run(command, capture_output=True, text=True, timeout=args.timeout)
    filesystem, walk_errors = walk_filesystem(args.root)
    indexed, index_errors, con = walk_index(args.db)
    try:
        if args.verify_existing_db:
            prefix = os.fsencode(args.index_prefix.strip('/'))
            indexed = {
                (b'' if path == prefix else path[len(prefix) + 1:]): value
                for path, value in indexed.items()
                if path == prefix or path.startswith(prefix + b'/')
            }
            index_errors = [
                error for error in index_errors
                if bytes.fromhex(error['path_hex']) == prefix
                or bytes.fromhex(error['path_hex']).startswith(prefix + b'/')
            ]
        missing = sorted(filesystem.keys() - indexed.keys())
        extra = sorted(indexed.keys() - filesystem.keys())
        identity = sorted(p for p in filesystem.keys() & indexed.keys() if filesystem[p][:2] != indexed[p][:2])
        allocation = sorted(p for p in filesystem.keys() & indexed.keys() if filesystem[p][2] != indexed[p][2])
        report = {
            'captured_at_utc': datetime.now(timezone.utc).isoformat(),
            'status': 'passed' if (run is None or run.returncode == 0) and not (walk_errors or index_errors or missing or extra or identity or allocation) else 'failed',
            'binary': str(args.binary), 'binary_sha256': sha256(args.binary),
            'root': str(args.root), 'database': str(args.db),
            'scan_command': command if run is not None else None,
            'scan_returncode': run.returncode if run is not None else None,
            'scan_stdout_tail': run.stdout[-2000:] if run is not None else None,
            'scan_stderr_tail': run.stderr[-2000:] if run is not None else None,
            'verified_existing_database': args.verify_existing_db,
            'index_prefix': args.index_prefix,
            'elapsed_seconds': round(time.monotonic() - started, 3),
            'filesystem_paths': len(filesystem), 'indexed_paths': len(indexed),
            'walk_errors': walk_errors[:32], 'index_errors': index_errors[:32],
            'missing_paths': len(missing), 'extra_paths': len(extra),
            'identity_mismatches': len(identity), 'allocation_mismatches': len(allocation),
            'missing_examples_hex': [p.hex() for p in missing[:32]],
            'extra_examples_hex': [p.hex() for p in extra[:32]],
            'identity_examples_hex': [p.hex() for p in identity[:32]],
            'allocation_examples_hex': [p.hex() for p in allocation[:32]],
        }
    finally:
        con.close()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
