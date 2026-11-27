import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    'dux_live_invariant_probe', Path(__file__).with_name('dux-live-invariant-probe.py'))
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


class LiveInvariantProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='dux-invariant-probe-')
        self.db = Path(self.temp.name) / 'index.db'
        with sqlite3.connect(self.db) as conn:
            conn.executescript('''
                CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
                CREATE TABLE inodes(
                    dev_id INTEGER,inode INTEGER,kind TEXT,blocks INTEGER,
                    recursive_bytes INTEGER,recursive_inodes INTEGER,
                    PRIMARY KEY(dev_id,inode));
                CREATE TABLE dirents(
                    parent_dev INTEGER,parent_inode INTEGER,name BLOB,
                    dev_id INTEGER,inode INTEGER,prime INTEGER);
                INSERT INTO meta VALUES('root_dev','1'),('root_inode','1');
                INSERT INTO inodes VALUES(1,1,'d',4096,12288,3);
                INSERT INTO inodes VALUES(1,2,'d',4096,8192,2);
                INSERT INTO inodes VALUES(1,3,'f',4096,4096,1);
                INSERT INTO dirents VALUES(1,1,X'2f',1,1,1);
                INSERT INTO dirents VALUES(1,1,X'746d70',1,2,1);
                INSERT INTO dirents VALUES(1,2,X'66',1,3,1);
            ''')

    def tearDown(self):
        self.temp.cleanup()

    def test_reports_consistent_tree_invariants(self):
        predicates, _ = probe.diagnose(self.db)
        self.assertEqual(predicates['root_ok'], 1)
        self.assertEqual(predicates['bad_directory_rollups'], 0)
        self.assertEqual(predicates['bad_leaf_totals'], 0)
        self.assertEqual(predicates['dangling_children'], 0)
        self.assertEqual(predicates['invalid_parents'], 0)

    def test_distinguishes_internal_rollup_drift_from_root_inode_aggregate(self):
        with sqlite3.connect(self.db) as conn:
            conn.execute('UPDATE inodes SET recursive_bytes=recursive_bytes+4096, '
                         'recursive_inodes=recursive_inodes+1 WHERE inode=2')
        predicates, _ = probe.diagnose(self.db)
        self.assertEqual(predicates['root_ok'], 1)
        self.assertEqual(predicates['root_bytes'], predicates['stored_root_bytes'])
        self.assertEqual(predicates['bad_directory_rollups'], 2)
        self.assertEqual(predicates['bad_leaf_totals'], 0)
        self.assertEqual(predicates['dangling_children'], 0)
        self.assertEqual(predicates['invalid_parents'], 0)

    def test_captures_bad_directory_and_leaf_paths_in_the_same_snapshot(self):
        with sqlite3.connect(self.db) as conn:
            conn.execute('UPDATE inodes SET recursive_bytes=recursive_bytes+4096, '
                         'recursive_inodes=recursive_inodes+1 WHERE inode=2')
            conn.execute('UPDATE inodes SET recursive_bytes=recursive_bytes+1 WHERE inode=3')
        predicates, _ = probe.diagnose(self.db)
        directory = next(row for row in predicates['bad_directory_samples']
                         if row['inode'] == 2)
        leaf = predicates['bad_leaf_samples'][0]
        self.assertEqual(directory['path_components_hex'], ['746d70'])
        self.assertEqual(leaf['path_components_hex'], ['746d70', '66'])

    def test_status_command_requests_json_with_global_options(self):
        class Result:
            returncode = 0
            stdout = '{"dirty_reason": null}'
            stderr = ''

        with patch.object(probe.subprocess, 'run', return_value=Result()) as run:
            self.assertEqual(probe.status(Path('/usr/bin/dux'), self.db),
                             {'dirty_reason': None})
        self.assertEqual(run.call_args.args[0], [
            '/usr/bin/dux', '--db', str(self.db), '--json', 'status'])

    def test_status_falls_back_to_read_only_dirty_metadata_for_legacy_cli(self):
        class Result:
            returncode = 0
            stdout = 'daemon: live; pipeline: caught up\n'
            stderr = ''

        with sqlite3.connect(self.db) as conn:
            conn.execute("INSERT INTO meta VALUES('dirty_since','123')")
            conn.execute("INSERT INTO meta VALUES('dirty_reason','test drift')")
        with patch.object(probe.subprocess, 'run', return_value=Result()):
            observed = probe.status(Path('/usr/bin/dux'), self.db)
        self.assertTrue(observed['dirty'])
        self.assertEqual(observed['dirty_since'], '123')
        self.assertEqual(observed['dirty_reason'], 'test drift')


if __name__ == '__main__':
    unittest.main()
