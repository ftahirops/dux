#!/usr/bin/env python3
"""Read-only capture of the first accounting predicate failing on a live index."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import time


DIAGNOSTIC_SQL = '''
WITH child_totals AS (
 SELECT d.parent_dev,d.parent_inode,SUM(ch.recursive_bytes) bytes,
        SUM(ch.recursive_inodes) items
 FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
 WHERE d.prime=1 AND NOT(d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
 GROUP BY d.parent_dev,d.parent_inode
), inode_totals AS (
 SELECT COALESCE(SUM(blocks),0) bytes,COUNT(*) items FROM inodes
), root AS (
 SELECT r.dev_id,r.inode,r.recursive_bytes,r.recursive_inodes,t.bytes,t.items
 FROM inodes r CROSS JOIN inode_totals t
 WHERE r.dev_id=CAST((SELECT value FROM meta WHERE key='root_dev') AS INTEGER)
   AND r.inode=CAST((SELECT value FROM meta WHERE key='root_inode') AS INTEGER)
)
SELECT
 (SELECT count(*) FROM root WHERE recursive_bytes=bytes AND recursive_inodes=items) root_ok,
 (SELECT bytes FROM root) root_bytes,
 (SELECT recursive_bytes FROM root) stored_root_bytes,
 (SELECT items FROM root) inode_count,
 (SELECT recursive_inodes FROM root) stored_root_count,
 (SELECT count(*) FROM inodes) inode_rows,
 (SELECT count(*) FROM dirents de LEFT JOIN inodes ch
    ON ch.dev_id=de.dev_id AND ch.inode=de.inode WHERE ch.inode IS NULL) dangling_children,
 (SELECT count(*) FROM dirents de LEFT JOIN inodes p
    ON p.dev_id=de.parent_dev AND p.inode=de.parent_inode
    WHERE p.kind IS NULL OR p.kind!='d') invalid_parents,
 (SELECT count(*) FROM inodes d LEFT JOIN child_totals x
    ON x.parent_dev=d.dev_id AND x.parent_inode=d.inode
    WHERE d.kind='d' AND
     (d.recursive_bytes!=d.blocks+coalesce(x.bytes,0) OR
      d.recursive_inodes!=1+coalesce(x.items,0))) bad_directory_rollups,
 (SELECT count(*) FROM inodes leaf WHERE leaf.kind!='d' AND
    (leaf.recursive_bytes!=leaf.blocks OR leaf.recursive_inodes!=1)) bad_leaf_totals,
 (SELECT count(*) FROM dirents) dirents
'''
FIELDS = ('root_ok', 'root_bytes', 'stored_root_bytes', 'inode_count',
          'stored_root_count', 'inode_rows', 'dangling_children',
          'invalid_parents', 'bad_directory_rollups', 'bad_leaf_totals',
          'dirents')


def status(binary, db):
    result = subprocess.run([str(binary), '--db', str(db), '--json', 'status'],
                            capture_output=True, text=True, timeout=15)
    if result.returncode:
        raise RuntimeError(f'status command failed: {result.stderr[-1000:]}')
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        # Older installed releases accept --json but still render a table.
        # Read only the small dirty markers directly so the probe can observe
        # verifier-triggered rebuilds without parsing human-facing text.
        connection = sqlite3.connect(f'file:{db}?mode=ro', uri=True, timeout=5)
        try:
            metadata = dict(connection.execute(
                "SELECT key,value FROM meta WHERE key IN ('dirty_since','dirty_reason')"))
        finally:
            connection.close()
        return {
            'dirty': 'dirty_since' in metadata,
            'dirty_since': metadata.get('dirty_since'),
            'dirty_reason': metadata.get('dirty_reason'),
            'status_text': result.stdout,
        }


def diagnose(db):
    connection = sqlite3.connect(f'file:{db}?mode=ro', uri=True, timeout=30)
    try:
        connection.execute('PRAGMA query_only=ON')
        connection.execute('PRAGMA temp_store=FILE')
        connection.execute('PRAGMA cache_size=-16384')
        connection.execute('BEGIN')
        started = time.monotonic()
        values = connection.execute(DIAGNOSTIC_SQL).fetchone()
        elapsed = time.monotonic() - started
        predicates = dict(zip(FIELDS, values))
        root = (int(connection.execute("SELECT value FROM meta WHERE key='root_dev'").fetchone()[0]),
                int(connection.execute("SELECT value FROM meta WHERE key='root_inode'").fetchone()[0]))

        def components(dev, inode):
            """Resolve a prime dirent chain without leaving this read snapshot."""
            current = (dev, inode)
            names = []
            seen = set()
            while current != root:
                if current in seen or len(names) >= 4096:
                    return None
                seen.add(current)
                row = connection.execute(
                    "SELECT parent_dev,parent_inode,name FROM dirents "
                    "WHERE dev_id=? AND inode=? AND prime=1 "
                    "AND NOT(parent_dev=dev_id AND parent_inode=inode) "
                    "ORDER BY parent_dev,parent_inode,name LIMIT 1", current).fetchone()
                if row is None:
                    return None
                names.append(bytes(row[2]).hex())
                current = (row[0], row[1])
            return list(reversed(names))

        bad_directories = connection.execute('''
            WITH child_totals AS (
              SELECT d.parent_dev,d.parent_inode,SUM(ch.recursive_bytes) bytes,
                     SUM(ch.recursive_inodes) items
              FROM dirents d JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
              WHERE d.prime=1 AND NOT(d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
              GROUP BY d.parent_dev,d.parent_inode
            )
            SELECT d.dev_id,d.inode,d.blocks,d.recursive_bytes,
                   d.blocks+coalesce(x.bytes,0),d.recursive_inodes,
                   1+coalesce(x.items,0)
            FROM inodes d LEFT JOIN child_totals x
              ON x.parent_dev=d.dev_id AND x.parent_inode=d.inode
            WHERE d.kind='d' AND
              (d.recursive_bytes!=d.blocks+coalesce(x.bytes,0) OR
               d.recursive_inodes!=1+coalesce(x.items,0))
            ORDER BY d.dev_id,d.inode LIMIT 16
        ''').fetchall()
        predicates['bad_directory_samples'] = [
            {'dev': row[0], 'inode': row[1], 'stored_bytes': row[3],
             'expected_bytes': row[4], 'stored_inodes': row[5],
             'expected_inodes': row[6],
             'path_components_hex': components(row[0], row[1])}
            for row in bad_directories]

        bad_leaves = connection.execute('''
            SELECT dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes
            FROM inodes WHERE kind!='d' AND
              (recursive_bytes!=blocks OR recursive_inodes!=1)
            ORDER BY dev_id,inode LIMIT 16
        ''').fetchall()
        predicates['bad_leaf_samples'] = [
            {'dev': row[0], 'inode': row[1], 'kind': row[2], 'blocks': row[3],
             'stored_bytes': row[4], 'stored_inodes': row[5],
             'path_components_hex': components(row[0], row[1])}
            for row in bad_leaves]
        return predicates, elapsed
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--db', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=360)
    parser.add_argument('--poll-interval', type=float, default=.5)
    args = parser.parse_args()
    binary_sha = hashlib.sha256(args.binary.read_bytes()).hexdigest()
    started = time.monotonic()
    deadline = started + args.timeout
    latest = None
    while time.monotonic() < deadline:
        latest = status(args.binary, args.db)
        if latest.get('dirty_reason') == 'directory accounting invariant failed':
            query_started = time.monotonic()
            predicates, query_seconds = diagnose(args.db)
            after = status(args.binary, args.db)
            report = {
                'status': 'captured',
                'captured_at_utc': datetime.now(timezone.utc).isoformat(),
                'wait_seconds': round(query_started-started, 3),
                'diagnostic_seconds': round(query_seconds, 3),
                'candidate_reader_sha256': binary_sha,
                'trigger_status': latest,
                'status_after_diagnostic': after,
                'predicates': predicates,
            }
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps(report), flush=True)
            return 0
        time.sleep(args.poll_interval)
    report = {'status': 'trigger_not_observed',
              'wait_seconds': round(time.monotonic()-started, 3),
              'candidate_reader_sha256': binary_sha,
              'last_status': latest}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(report), flush=True)
    return 2


if __name__ == '__main__':
    raise SystemExit(main())
