import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import call, patch


SPEC = importlib.util.spec_from_file_location(
    "dux_live_observer", Path(__file__).with_name("dux-live-observer.py")
)
observer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observer)


class PipelineCatchupTests(unittest.TestCase):
    def setUp(self):
        self.healthy = {
            "caught_up": True,
            "scan_in_progress": False,
            "dirty": False,
            "paused": False,
            "watch_status_available": True,
            "pending_updates": 0,
        }

    def test_requires_full_machine_health_state(self):
        self.assertTrue(observer.pipeline_caught_up(self.healthy))

    def test_rejects_reconciliation_even_when_event_queue_is_empty(self):
        for field, value in (
            ("caught_up", False),
            ("scan_in_progress", True),
            ("dirty", True),
            ("paused", True),
            ("watch_status_available", False),
            ("pending_updates", 1),
        ):
            with self.subTest(field=field):
                status = dict(self.healthy, **{field: value})
                self.assertFalse(observer.pipeline_caught_up(status))

    def test_legacy_drained_text_is_not_proof_of_catchup(self):
        text = "pipeline: drained; 0 pending / 500000 cap; 0ms since caught up"
        self.assertFalse(observer.pipeline_caught_up(text))


class ObserverUtilityTests(unittest.TestCase):
    def setUp(self):
        self.healthy = {
            "caught_up": True,
            "scan_in_progress": False,
            "dirty": False,
            "paused": False,
            "watch_status_available": True,
            "pending_updates": 0,
        }

    def test_uses_requested_unit_for_status_and_journal(self):
        with patch.object(observer, 'run', side_effect=[{'stdout': 'status'},
                                                         {'stdout': 'journal'}]) as mocked:
            service, journal = observer.service_diagnostics(
                'dux-candidate-root-shadow-orphan-guard-20261024.service')
        self.assertEqual(service['stdout'], 'status')
        self.assertEqual(journal['stdout'], 'journal')
        self.assertEqual(mocked.call_args_list, [
            call('systemctl', 'show', 'dux-candidate-root-shadow-orphan-guard-20261024.service',
                 '-p', 'MainPID', '-p', 'NRestarts', '-p', 'ActiveState', '-p', 'SubState',
                 '-p', 'StatusText'),
            call('journalctl', '-u', 'dux-candidate-root-shadow-orphan-guard-20261024.service',
                 '--since', '10 minutes ago', '--no-pager', '-o', 'short-iso-precise'),
        ])

    def test_status_tail_handles_json_and_legacy_text(self):
        self.assertEqual(observer.status_tail(self.healthy), self.healthy)
        self.assertEqual(observer.status_tail("line 1\nline 2\nline 3"),
                         ["line 1", "line 2", "line 3"])

    def test_legacy_text_status_uses_fresh_process_validated_watch_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            db = Path(directory) / 'index.db'
            with sqlite3.connect(db) as connection:
                connection.executescript('''
                    CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
                    CREATE TABLE inodes(dev_id INTEGER,inode INTEGER,recursive_bytes INTEGER,
                        recursive_inodes INTEGER,PRIMARY KEY(dev_id,inode));
                    INSERT INTO meta VALUES('root_dev','1'),('root_inode','1'),
                        ('last_scan_ts','123');
                    INSERT INTO inodes VALUES(1,1,4096,1);
                ''')
            Path(f'{db}.watch.status').write_text(
                f'1 {int(__import__("time").time())} {os.getpid()} 0 1 '
                '10 10 4 0 0 100\n' + str(db.resolve()))
            with patch.object(observer, 'DB', db), \
                    patch.object(observer, 'run', return_value={
                        'returncode': 0, 'stdout': 'pipeline: drained\n', 'stderr': ''
                    }):
                status = observer.current_status()
            self.assertEqual(status['status_source'], 'legacy_watch_status_v1')
            self.assertTrue(observer.pipeline_caught_up(status))
            self.assertEqual(status['index_nodes'], 1)
            self.assertEqual(status['index_bytes'], 4096)

    def test_exact_audit_wait_requires_an_unchanged_caught_up_window(self):
        class FakeClock:
            value = 0

            def now(self):
                return self.value

            def sleep(self, seconds):
                self.value += seconds

        first = dict(self.healthy, updates_committed=10, index_nodes=20, index_bytes=4096)
        changed = dict(first, updates_committed=11)
        statuses = iter([dict(self.healthy, caught_up=False), first, changed,
                         changed, changed, changed])
        fake = FakeClock()
        progress = []
        status, ready, changed_fields = observer.wait_for_stable_catchup(
            timeout=10, stable_seconds=2, status_reader=lambda: next(statuses),
            now=fake.now, sleep=fake.sleep, progress_callback=progress.append)
        self.assertTrue(ready)
        self.assertEqual(status, changed)
        self.assertEqual(changed_fields, [])
        self.assertEqual(progress[0]['phase'], 'waiting_for_stable_catchup')
        self.assertEqual(progress[-1]['phase'], 'stable_catchup_complete')
        self.assertEqual(progress[-1]['stable_window_seconds_observed'], 2)
        # A metadata-only commit no longer restarts the structural quiet window.
        self.assertEqual(fake.value, 3)

    def test_exact_audit_can_start_immediately_when_caught_up(self):
        status, ready, changed = observer.wait_for_stable_catchup(
            timeout=10, stable_seconds=0,
            status_reader=lambda: self.healthy)
        self.assertTrue(ready)
        self.assertEqual(status, self.healthy)
        self.assertEqual(changed, [])

    def test_ignores_coalesced_events_but_detects_database_changes_during_observation(self):
        before = dict(self.healthy, events_seen=10, updates_committed=3,
                      events_resolved=9, events_dropped=0,
                      index_nodes=20, index_bytes=8192,
                      last_scan_timestamp_seconds=100)
        event_only = dict(before, events_seen=12, events_resolved=11)
        self.assertEqual(observer.snapshot_status_stable(before, event_only), (True, []))
        after = dict(event_only, updates_committed=4)
        stable, changed = observer.snapshot_status_stable(before, after)
        self.assertFalse(stable)
        self.assertEqual(changed, ['updates_committed'])
        self.assertEqual(observer.snapshot_status_stable(before, before), (True, []))

    def test_exact_walk_stability_ignores_commit_activity_but_tracks_index_shape(self):
        before = dict(self.healthy, events_seen=10, updates_committed=3,
                      events_resolved=9, events_dropped=0,
                      index_nodes=20, index_bytes=8192)
        metadata_commit = dict(before, events_seen=12, events_resolved=11,
                               updates_committed=4,
                               last_event_commit_timestamp_seconds=101)
        self.assertEqual(observer.snapshot_index_structure_stable(
            before, metadata_commit), (True, []))
        changed = dict(metadata_commit, index_nodes=21)
        stable, fields = observer.snapshot_index_structure_stable(before, changed)
        self.assertFalse(stable)
        self.assertEqual(fields, ['index_nodes'])

    def test_accounting_snapshot_accepts_metadata_commits_but_requires_caught_up(self):
        before = dict(self.healthy, events_dropped=0, index_nodes=20,
                      index_bytes=8192, root='/')
        after = dict(before, updates_committed=4,
                     last_event_commit_timestamp_seconds=101)
        self.assertEqual(observer.accounting_snapshot_stable(before, after),
                         (True, []))
        behind = dict(after, pending_updates=1, caught_up=False)
        self.assertEqual(observer.accounting_snapshot_stable(before, behind),
                         (False, ['caught_up', 'pending_updates']))


class DirectChildAuditTests(unittest.TestCase):
    def test_indexed_paths_respect_non_root_scan_roots_and_raw_names(self):
        path = observer.indexed_path('/tmp', [b'go-build', b'bad-\xff-name'])
        self.assertEqual(os.fsencode(path), b'/tmp/go-build/bad-\xff-name')
        self.assertEqual(str(observer.indexed_path('/', [b'var', b'tmp'])), '/var/tmp')

    def test_finds_missing_database_child_and_checks_live_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            live = Path(directory) / 'live'
            live.write_bytes(b'x')
            st = os.lstat(live)
            result = observer.audit_direct_children(directory, [
                (b'live', st.st_dev, st.st_ino, 'f', st.st_blocks * 512),
                (b'gone', st.st_dev, st.st_ino + 1, 'd', 4096),
            ], complete=True)
        self.assertEqual(result['checked'], 2)
        self.assertTrue(result['complete'])
        self.assertEqual(result['missing_count'], 1)
        self.assertEqual(result['missing'][0]['path'].rsplit('/', 1)[-1], 'gone')
        self.assertEqual(result['identity_or_kind_mismatch_count'], 0)
        self.assertEqual(result['allocation_mismatch_count'], 0)


class ExactFilesystemTests(unittest.TestCase):
    def compare_normal_filesystem(self, root, conn, progress_callback=None):
        # The regression fixture may live on TMPDIR (often tmpfs in isolated
        # test runs). Keep this comparison test focused on path/inode/allocation
        # reconciliation; mount-policy behavior has its own test above.
        with patch.object(observer, "mount_policy", return_value=(set(), False)), \
                patch.object(observer, "audit_staging_directory", return_value=Path(tempfile.gettempdir())):
            return observer.exact_filesystem(root, conn, progress_callback)

    def test_mount_policy_prunes_nested_pseudo_filesystems(self):
        mountinfo = '''
36 25 0:32 / / rw,relatime - ext4 /dev/root rw
37 36 0:33 / /proc rw,nosuid - proc proc rw
38 36 0:34 / /sys rw,nosuid - sysfs sysfs rw
39 36 0:35 / /dev rw,nosuid - devtmpfs udev rw
40 36 0:32 / /mnt/data\\040archive rw,relatime - ext4 /dev/root rw
'''
        mounts = observer.parse_mountinfo(mountinfo)
        nested, root_pseudo = observer.mount_policy('/', mounts)
        self.assertFalse(root_pseudo)
        self.assertEqual(nested, {Path('/proc'), Path('/sys'), Path('/dev')})
        nested, root_pseudo = observer.mount_policy('/proc', mounts)
        self.assertTrue(root_pseudo)

    def test_compares_every_entry_inode_and_allocation_under_scan_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'tmp-root'
            nested = root / 'go-build' / 'b001'
            nested.mkdir(parents=True)
            file = nested / 'artifact.a'
            file.write_bytes(b'x' * 8192)
            conn = sqlite3.connect(':memory:')
            conn.executescript('''
                CREATE TABLE dirents(parent_dev INTEGER,parent_inode INTEGER,name BLOB,
                                     dev_id INTEGER,inode INTEGER);
                CREATE TABLE inodes(dev_id INTEGER,inode INTEGER,blocks INTEGER);
            ''')
            filesystem_entries = {}
            filesystem_inodes = {}
            for parent, dirs, files in os.walk(root):
                parent_stat = os.lstat(parent)
                parent_id = (parent_stat.st_dev, parent_stat.st_ino)
                filesystem_inodes[parent_id] = parent_stat.st_blocks * 512
                for name in dirs + files:
                    path = Path(parent) / name
                    st = os.lstat(path)
                    identity = (st.st_dev, st.st_ino)
                    filesystem_entries[(parent_id[0], parent_id[1], os.fsencode(name))] = identity
                    filesystem_inodes[identity] = st.st_blocks * 512
            for (pdev, pino, name), (dev, ino) in filesystem_entries.items():
                conn.execute('INSERT INTO dirents VALUES(?,?,?,?,?)', (pdev, pino, name, dev, ino))
            for (dev, ino), blocks in filesystem_inodes.items():
                conn.execute('INSERT INTO inodes VALUES(?,?,?)', (dev, ino, blocks))

            progress = []
            clean = self.compare_normal_filesystem(root, conn, progress.append)
            self.assertTrue(clean['passed'], clean)
            self.assertEqual(clean['filesystem_path_count'], 3)
            self.assertEqual(clean['filesystem_inode_count'], 4)
            self.assertIn('disk-backed SQLite', clean['comparison_storage'])
            self.assertEqual(progress[0]['phase'], 'filesystem_walk')
            self.assertEqual(progress[0]['filesystem_paths'], 0)
            self.assertEqual(progress[-1]['phase'], 'comparison_complete')
            self.assertTrue(progress[-1]['passed'])
            self.assertEqual(progress[-1]['filesystem_paths'], 3)

            parent = os.lstat(nested)
            conn.execute('DELETE FROM dirents WHERE parent_dev=? AND parent_inode=? AND name=?',
                         (parent.st_dev, parent.st_ino, sqlite3.Binary(b'artifact.a')))
            missing = self.compare_normal_filesystem(root, conn)
            details = missing['difference_details']
            self.assertEqual(details['missing_from_index']['count'], 1)
            self.assertFalse(details['missing_from_index']['truncated'])
            self.assertEqual(details['missing_from_index']['rows'][0]['name_hex'],
                             b'artifact.a'.hex().upper())

            root_stat = os.lstat(root)
            conn.execute('INSERT INTO dirents VALUES(?,?,?,?,?)',
                         (root_stat.st_dev, root_stat.st_ino, sqlite3.Binary(b'ghost'),
                          root_stat.st_dev, root_stat.st_ino + 1_000_000))
            stale = self.compare_normal_filesystem(root, conn)
            self.assertEqual(stale['difference_details']['stale_in_index']['count'], 1)
            self.assertEqual(stale['difference_details']['stale_in_index']['rows'][0]['name_hex'],
                             b'ghost'.hex().upper())
            conn.execute('DELETE FROM dirents WHERE name=?', (sqlite3.Binary(b'ghost'),))

            conn.execute('INSERT INTO dirents VALUES(?,?,?,?,?)',
                         (parent.st_dev, parent.st_ino, sqlite3.Binary(b'artifact.a'),
                          st.st_dev, st.st_ino + 1))
            identity = self.compare_normal_filesystem(root, conn)
            self.assertEqual(identity['difference_details']['identity_mismatch']['count'], 1)
            self.assertEqual(
                identity['difference_details']['identity_mismatch']['rows'][0]['filesystem_inode'],
                st.st_ino)
            conn.execute('UPDATE dirents SET inode=? WHERE parent_dev=? AND parent_inode=? AND name=?',
                         (st.st_ino, parent.st_dev, parent.st_ino, sqlite3.Binary(b'artifact.a')))

            # Inode rows are a separate index invariant from path dirents;
            # retain exact identities for later live rechecks too.
            conn.execute('DELETE FROM inodes WHERE dev_id=? AND inode=?', (st.st_dev, st.st_ino))
            missing_inode = self.compare_normal_filesystem(root, conn)
            detail = missing_inode['difference_details']['missing_from_index_inodes']
            self.assertEqual(detail['count'], 1)
            self.assertEqual(detail['rows'][0], {
                'filesystem_dev': st.st_dev, 'filesystem_inode': st.st_ino})
            conn.execute('INSERT INTO inodes VALUES(?,?,?)',
                         (st.st_dev, st.st_ino, st.st_blocks * 512))
            conn.execute('INSERT INTO inodes VALUES(?,?,?)',
                         (st.st_dev, st.st_ino + 2_000_000, 4096))
            stale_inode = self.compare_normal_filesystem(root, conn)
            detail = stale_inode['difference_details']['stale_in_index_inodes']
            self.assertEqual(detail['count'], 1)
            self.assertEqual(detail['rows'][0], {
                'indexed_dev': st.st_dev, 'indexed_inode': st.st_ino + 2_000_000})
            conn.execute('DELETE FROM inodes WHERE dev_id=? AND inode=?',
                         (st.st_dev, st.st_ino + 2_000_000))

            conn.execute('UPDATE inodes SET blocks=blocks+4096 WHERE inode=?', (file.stat().st_ino,))
            changed = self.compare_normal_filesystem(root, conn)
            self.assertFalse(changed['passed'])
            self.assertEqual(changed['allocation_mismatch_count'], 1)
            self.assertEqual(changed['difference_details']['allocation_mismatch']['count'], 1)
            self.assertEqual(changed['difference_details']['allocation_mismatch']['rows'][0]['inode'],
                             st.st_ino)
            conn.close()


class ProgressSidecarTests(unittest.TestCase):
    def test_default_progress_sidecar_is_outside_the_watched_workspace(self):
        output = Path('/home/dux/target/readiness/audit.json')
        path = observer.default_progress_path(output)
        self.assertEqual(path.parent, Path('/run'))
        self.assertIn(output.stem, path.name)
        self.assertIn(str(os.getpid()), path.name)

    def test_progress_sidecar_replacement_is_complete_and_leaves_no_temp_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'nested' / 'observer.progress.json'
            observer.write_progress_file(path, {'phase': 'walk', 'filesystem_paths': 42})
            self.assertEqual(json.loads(path.read_text()), {
                'filesystem_paths': 42, 'phase': 'walk'})
            self.assertFalse(Path(str(path) + '.tmp').exists())


class ProvenanceTests(unittest.TestCase):
    def test_observer_source_hash_is_captured_at_module_load(self):
        source = Path(observer.__file__).read_bytes()
        self.assertEqual(observer.OBSERVER_SHA256,
                         observer.hashlib.sha256(source).hexdigest())


class SnapshotClassificationTests(unittest.TestCase):
    def setUp(self):
        self.clean = {
            'root_matches_inode_aggregate': True,
            'sampled_rollup_mismatches': [],
            'orphan_dirents': 0,
            'invalid_prime_attribution': 0,
            'quick_check': [('ok',)],
        }

    def status(self):
        return {'caught_up': True, 'scan_in_progress': False, 'dirty': False,
                'paused': False, 'watch_status_available': True,
                'pending_updates': 0, 'events_dropped': 0,
                'updates_committed': 3, 'index_nodes': 20,
                'index_bytes': 8192, 'root': '/'}

    def test_clean_snapshots_return_success(self):
        self.assertEqual(observer.classify_snapshots(self.clean, self.clean),
                         ('CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS', 0))

    def test_recovered_later_snapshot_keeps_initial_mismatch_visible(self):
        initial = dict(self.clean, sampled_rollup_mismatches=[{'path': '/tmp/build'}])
        self.assertEqual(observer.classify_snapshots(initial, self.clean),
                         ('CAUGHT_UP_AFTER_TRANSIENT_ACCOUNTING_MISMATCH', 2))

    def test_unstable_initial_filesystem_walk_does_not_fail_stable_recovery(self):
        initial = dict(self.clean, exact_filesystem={'passed': False})
        self.assertEqual(observer.classify_snapshots(initial, self.clean,
                                                     initial_stable=False),
                         ('CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS', 0))

    def test_unstable_exact_walk_never_qualifies_from_database_only_recovery(self):
        initial = dict(self.clean, exact_filesystem={'passed': False})
        self.assertEqual(observer.classify_snapshots(
            initial, self.clean, initial_stable=False, require_exact=True),
            ('SNAPSHOT_UNSTABLE', 1))

    def test_stable_exact_walk_requires_its_path_comparison_to_pass(self):
        initial = dict(self.clean, exact_filesystem={'passed': False})
        self.assertEqual(observer.classify_snapshots(
            initial, self.clean, initial_stable=True, require_exact=True),
            ('CAUGHT_UP_BUT_ACCOUNTING_INVARIANTS_FAIL', 1))
        initial['exact_filesystem'] = {'passed': True}
        self.assertEqual(observer.classify_snapshots(
            initial, self.clean, initial_stable=True, require_exact=True),
            ('CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS', 0))

    def test_exact_recovery_uses_a_database_only_snapshot(self):
        source = Path(observer.__file__).read_text()
        recovery = source.split("recovery_deadline = time.monotonic()", 1)[1]
        self.assertIn("recovery = snapshot(False)", recovery)
        self.assertNotIn("snapshot(args.exact_filesystem", recovery)

    def test_final_snapshot_failure_is_not_hidden_by_clean_initial_snapshot(self):
        recovered = dict(self.clean, orphan_dirents=1)
        self.assertEqual(observer.classify_snapshots(self.clean, recovered),
                         ('CAUGHT_UP_BUT_ACCOUNTING_INVARIANTS_FAIL', 1))

    def test_exact_pass_allows_metadata_commits_when_index_structure_matches(self):
        before = self.status()
        after = dict(before, updates_committed=4,
                     last_event_commit_timestamp_seconds=101)
        result = observer.classify_exact_audit(before, after, self.clean)
        self.assertTrue(result['passed'])
        self.assertEqual(result['audit_result'],
                         'CAUGHT_UP_AND_ACCOUNTING_INVARIANTS_PASS')
        self.assertTrue(result['activity_during_audit'])

    def test_exact_walk_rejects_index_structure_change_as_unstable(self):
        before = self.status()
        after = dict(before, index_nodes=21)
        result = observer.classify_exact_audit(before, after, self.clean)
        self.assertFalse(result['passed'])
        self.assertEqual(result['audit_result'], 'SNAPSHOT_UNSTABLE')
        self.assertEqual(result['snapshot_structure_changed_fields'], ['index_nodes'])


if __name__ == "__main__":
    unittest.main()
