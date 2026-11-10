#!/usr/bin/env python3
import grp
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import pwd
import stat
import sqlite3
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "dux_package_lifecycle", Path(__file__).with_name("dux-package-lifecycle.py")
)
LIFECYCLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFECYCLE)


class ProgressOutputTests(unittest.TestCase):
    def test_progress_checkpoint_is_prefixed_json_and_flushed(self):
        class FlushTrackingStream(io.StringIO):
            flushed = False

            def flush(self):
                self.flushed = True
                super().flush()

        stream = FlushTrackingStream()
        event = {"event": "stage_started", "stage": "upgrade"}
        LIFECYCLE.emit_progress(event, stream)
        prefix, payload = stream.getvalue().rstrip("\n").split(" ", 1)
        self.assertEqual(prefix, "DUX_PACKAGE_LIFECYCLE")
        self.assertEqual(json.loads(payload), event)
        self.assertTrue(stream.flushed)

    def test_completed_stage_checkpoint_carries_its_full_evidence(self):
        step = {
            "name": "upgrade",
            "evidence": {
                "version": "0.6.3-89",
                "binary_sha256": "abc123",
                "service": {"enabled": "disabled", "active": "inactive"},
            },
        }
        event = LIFECYCLE.stage_complete_progress_event(step, "running")
        self.assertEqual(event, {
            "event": "stage_complete", "stage": "upgrade",
            "status": "running", "step": step,
        })
        self.assertEqual(json.loads(json.dumps(event))["step"], step)

    def test_terminal_progress_event_contains_a_self_contained_report(self):
        report = {"status": "passed", "steps": [{"name": "upgrade", "evidence": {"ok": True}}]}
        event = LIFECYCLE.lifecycle_outcome_progress_event(report)
        self.assertEqual(event, {"event": "lifecycle_passed", "report": report})

    def test_nonterminal_report_cannot_be_emitted_as_an_outcome(self):
        with self.assertRaisesRegex(ValueError, "not terminal"):
            LIFECYCLE.lifecycle_outcome_progress_event({"status": "running"})


class QualificationInterpreterSafetyTests(unittest.TestCase):
    def test_qualification_harnesses_refuse_optimized_python(self):
        scripts = (
            Path(__file__).with_name("dux-package-lifecycle.py"),
            Path(__file__).with_name("dux-bind-alias-qualification.py"),
            Path(__file__).with_name("dux-live-quota-comparison.py"),
            Path(__file__).with_name("dux-live-test.py"),
            Path(__file__).with_name("dux-readiness.py"),
            Path(__file__).with_name("dux-scale.py"),
            Path(__file__).with_name("dux-systemd-status-test.py"),
            Path(__file__).with_name("dux-tui-latency.py"),
        )
        for script in scripts:
            with self.subTest(script=script.name):
                result = subprocess.run(
                    [sys.executable, "-O", str(script), "--help"],
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("do not use python -O", result.stderr)


class IndexAccessPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dux-package-policy-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / "dux"
        self.directory.mkdir(mode=0o750)
        os.chmod(self.directory, 0o750)
        self.database = self.directory / "dux.db"
        self.database.write_bytes(b"test index")
        os.chmod(self.database, 0o640)
        self.uid = self.directory.stat().st_uid
        self.group_name = grp.getgrgid(self.directory.stat().st_gid).gr_name

    def check_policy(self):
        return LIFECYCLE.assert_index_access_policy(
            self.directory, expected_uid=self.uid, group_name=self.group_name
        )

    def test_accepts_private_index_and_sqlite_sidecars(self):
        for suffix in ("-wal", "-shm", "-journal"):
            sidecar = Path(f"{self.database}{suffix}")
            sidecar.write_bytes(b"sqlite sidecar")
            os.chmod(sidecar, 0o640)

        evidence = self.check_policy()

        self.assertEqual(evidence["directory_mode"], "0o750")
        self.assertEqual(evidence["index_files"], {
            "dux.db": "0o640", "dux.db-wal": "0o640", "dux.db-shm": "0o640",
            "dux.db-journal": "0o640",
        })

    def test_rejects_world_readable_database(self):
        os.chmod(self.database, 0o644)
        with self.assertRaises(AssertionError):
            self.check_policy()

    def test_rejects_permissive_directory(self):
        os.chmod(self.directory, 0o755)
        with self.assertRaises(AssertionError):
            self.check_policy()

    def test_rejects_symlinked_database(self):
        external = Path(self.temporary.name) / "outside.db"
        external.write_bytes(b"outside")
        self.database.unlink()
        self.database.symlink_to(external)
        with self.assertRaises(AssertionError):
            self.check_policy()


class SecureIndexPermissionsHelperTests(unittest.TestCase):
    def setUp(self):
        self.helper = Path(__file__).resolve().parents[1] / "packaging/secure-index-permissions.sh"
        self.group = grp.getgrgid(os.getgid()).gr_name
        self.owner = pwd.getpwuid(os.geteuid()).pw_name

    def run_helper(self, state_dir, owner=None):
        return subprocess.run(
            [str(self.helper), str(state_dir), self.group, owner or self.owner],
            capture_output=True, text=True, check=False,
        )

    def test_sets_private_modes_on_regular_state(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-permissions-") as temporary:
            state = Path(temporary) / "dux"
            state.mkdir(mode=0o700)
            db = state / "dux.db"
            wal = state / "dux.db-wal"
            journal = state / "dux.db-journal"
            db.write_bytes(b"database")
            wal.write_bytes(b"wal")
            journal.write_bytes(b"rollback journal")
            os.chmod(db, 0o644)
            os.chmod(wal, 0o644)
            os.chmod(journal, 0o644)

            result = self.run_helper(state)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(stat.S_IMODE(state.stat().st_mode), 0o750)
            self.assertEqual(stat.S_IMODE(db.stat().st_mode), 0o640)
            self.assertEqual(stat.S_IMODE(wal.stat().st_mode), 0o640)
            self.assertEqual(stat.S_IMODE(journal.stat().st_mode), 0o640)
            self.assertEqual(state.stat().st_uid, os.geteuid())
            self.assertEqual(db.stat().st_uid, os.geteuid())

    @unittest.skipUnless(os.geteuid() == 0, "requires root to observe ownership repair")
    def test_closes_directory_access_before_any_ownership_change(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-order-") as temporary:
            root = Path(temporary)
            state = root / "dux"
            state.mkdir(mode=0o777)
            os.chmod(state, 0o777)
            os.chown(state, 65534, 65534)
            database = state / "dux.db"
            database.write_bytes(b"database")
            os.chown(database, 65534, 65534)

            bindir = root / "bin"
            bindir.mkdir()
            mode_log = root / "directory-modes.log"
            chown_wrapper = bindir / "chown"
            chown_wrapper.write_text(
                "#!/bin/sh\n"
                'printf "%s\\n" "$(stat -c %a "$DUX_STATE_DIR")" >> "$DUX_MODE_LOG"\n'
                'exec /usr/bin/chown "$@"\n'
            )
            chown_wrapper.chmod(0o755)
            env = os.environ.copy()
            env.update({
                "PATH": f"{bindir}:{env.get('PATH', '')}",
                "DUX_STATE_DIR": str(state),
                "DUX_MODE_LOG": str(mode_log),
            })

            result = subprocess.run(
                [str(self.helper), str(state), self.group, "root"],
                capture_output=True, text=True, check=False, env=env,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            observed_modes = mode_log.read_text().splitlines()
            self.assertGreaterEqual(len(observed_modes), 2)
            self.assertEqual(observed_modes[0], "0")
            self.assertEqual(observed_modes[1:], ["700"] * (len(observed_modes) - 1))

    @unittest.skipUnless(os.geteuid() == 0, "requires root to verify ownership repair")
    def test_repairs_untrusted_state_ownership_before_opening_directory(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-owner-") as temporary:
            state = Path(temporary) / "dux"
            state.mkdir(mode=0o777)
            os.chown(state, 65534, 65534)
            os.chmod(state, 0o777)
            db = state / "dux.db"
            db.write_bytes(b"database")
            os.chown(db, 65534, 65534)

            result = self.run_helper(state, owner="root")

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((state.stat().st_uid, state.stat().st_gid), (0, os.getgid()))
            self.assertEqual((db.stat().st_uid, db.stat().st_gid), (0, os.getgid()))
            self.assertEqual(stat.S_IMODE(state.stat().st_mode), 0o750)
            self.assertEqual(stat.S_IMODE(db.stat().st_mode), 0o640)

    def test_rejects_symlinked_state_directory_without_changing_target(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-dir-link-") as temporary:
            root = Path(temporary)
            outside = root / "outside"
            outside.mkdir(mode=0o755)
            link = root / "dux"
            link.symlink_to(outside, target_is_directory=True)
            before = stat.S_IMODE(outside.stat().st_mode)

            result = self.run_helper(link)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected a real directory", result.stderr)
            self.assertEqual(stat.S_IMODE(outside.stat().st_mode), before)

    def test_rejects_symlinked_database_without_changing_target(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-file-link-") as temporary:
            root = Path(temporary)
            state = root / "dux"
            state.mkdir()
            outside = root / "outside.db"
            outside.write_bytes(b"sensitive")
            os.chmod(outside, 0o600)
            (state / "dux.db").symlink_to(outside)
            before = (stat.S_IMODE(outside.stat().st_mode), outside.stat().st_gid)

            result = self.run_helper(state)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symbolic links are not allowed", result.stderr)
            self.assertEqual((stat.S_IMODE(outside.stat().st_mode), outside.stat().st_gid), before)

    def test_rejects_symlinked_sqlite_sidecars_without_changing_target(self):
        for suffix in ("-wal", "-shm", "-journal"):
            with self.subTest(sidecar=suffix), tempfile.TemporaryDirectory(
                prefix="dux-state-sidecar-link-"
            ) as temporary:
                root = Path(temporary)
                state = root / "dux"
                state.mkdir()
                outside = root / "outside"
                outside.write_bytes(b"sensitive sidecar")
                os.chmod(outside, 0o600)
                (state / f"dux.db{suffix}").symlink_to(outside)
                before = (stat.S_IMODE(outside.stat().st_mode), outside.stat().st_gid)

                result = self.run_helper(state)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("symbolic links are not allowed", result.stderr)
                self.assertEqual(
                    (stat.S_IMODE(outside.stat().st_mode), outside.stat().st_gid), before
                )

    def test_rejects_hardlinked_sqlite_sidecars_without_changing_external_file(self):
        for suffix in ("-wal", "-shm", "-journal"):
            with self.subTest(sidecar=suffix), tempfile.TemporaryDirectory(
                prefix="dux-state-sidecar-hardlink-"
            ) as temporary:
                root = Path(temporary)
                state = root / "dux"
                state.mkdir(mode=0o700)
                outside = root / "outside"
                outside.write_bytes(b"external sidecar")
                os.chmod(outside, 0o644)
                os.link(outside, state / f"dux.db{suffix}")
                before = outside.stat()

                result = self.run_helper(state, owner="root")

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("hard-linked files are not allowed", result.stderr)
                after = outside.stat()
                self.assertEqual(after.st_uid, before.st_uid)
                self.assertEqual(after.st_gid, before.st_gid)
                self.assertEqual(stat.S_IMODE(after.st_mode), 0o644)

    def test_rejects_non_regular_sqlite_artifact(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-special-") as temporary:
            state = Path(temporary) / "dux"
            state.mkdir()
            os.mkfifo(state / "dux.db-wal")

            result = self.run_helper(state)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected a regular file", result.stderr)

    @unittest.skipUnless(os.geteuid() == 0, "requires root to verify ownership isolation")
    def test_rejects_hardlinked_index_without_changing_external_file(self):
        with tempfile.TemporaryDirectory(prefix="dux-state-hardlink-") as temporary:
            root = Path(temporary)
            state = root / "dux"
            state.mkdir(mode=0o700)
            external = root / "outside.db"
            external.write_bytes(b"external file")
            os.chmod(external, 0o644)
            os.link(external, state / "dux.db")
            before = external.stat()

            result = self.run_helper(state, owner="root")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("hard-linked files are not allowed", result.stderr)
            after = external.stat()
            self.assertEqual(after.st_uid, before.st_uid)
            self.assertEqual(after.st_gid, before.st_gid)
            self.assertEqual(stat.S_IMODE(after.st_mode), 0o644)

    def test_deb_rpm_and_release_builder_install_the_same_helper(self):
        root = Path(__file__).resolve().parents[1]
        deb = (root / "packaging/deb/postinst").read_text()
        spec = (root / "packaging/dux.spec").read_text()
        builder = (root / "scripts/build-release.sh").read_text()
        self.assertIn("/usr/lib/dux/secure-index-permissions.sh /var/lib/dux dux dux", deb)
        self.assertIn("/usr/lib/dux/secure-index-permissions.sh /var/lib/dux dux dux", spec)
        self.assertIn("owner=${3:-dux}", (root / "packaging/secure-index-permissions.sh").read_text())
        service = (root / "packaging/dux.service").read_text()
        self.assertIn("User=dux", service)
        self.assertIn("adduser --system --ingroup dux", deb)
        self.assertIn("useradd --system --gid dux", spec)
        self.assertIn("%dir %attr(0750,dux,dux) /var/lib/dux", spec)
        deb_stop = deb.index("systemctl stop dux.service")
        deb_migrate = deb.index("secure-index-permissions.sh /var/lib/dux dux dux")
        deb_restart = deb.index('if [ "$was_active" = yes ]')
        self.assertLess(deb_stop, deb_migrate)
        self.assertLess(deb_migrate, deb_restart)
        rpm_stop = spec.index("systemctl stop dux.service")
        rpm_migrate = spec.index("secure-index-permissions.sh /var/lib/dux dux dux")
        rpm_restart = spec.index('if [ "$was_active" = yes ]')
        self.assertLess(rpm_stop, rpm_migrate)
        self.assertLess(rpm_migrate, rpm_restart)
        rpm_pre = spec.split("%pre\n", 1)[1].split("\n%post\n", 1)[0]
        rpm_post = spec.split("%post\n", 1)[1].split("\n%posttrans\n", 1)[0]
        rpm_posttrans = spec.split("%posttrans\n", 1)[1].split("\n%preun\n", 1)[0]
        self.assertIn("/run/dux-rpm-upgrade-was-active", rpm_pre)
        self.assertIn("systemctl stop dux.service", rpm_pre)
        self.assertNotIn("systemctl stop dux.service", rpm_post)
        self.assertIn("secure-index-permissions.sh /var/lib/dux dux dux", rpm_post)
        self.assertIn("/run/dux-rpm-upgrade-was-active", rpm_posttrans)
        self.assertIn('if [ "$was_active" = yes ]', rpm_posttrans)
        self.assertIn("packaging/secure-index-permissions.sh", builder)
        self.assertIn("Depends: adduser, python3", (root / "packaging/deb/control").read_text())
        self.assertIn("Requires:       python3", spec)
        self.assertIn("/usr/lib/dux/dux-backup-index.py", spec)
        self.assertIn("scripts/dux-backup-index.py", builder)
        self.assertIn("deb_payload_backup_helper_sha256", builder)
        self.assertIn("rpm_payload_backup_helper_sha256", builder)


class EffectiveUnitPolicyTests(unittest.TestCase):
    def setUp(self):
        self.proc = tempfile.TemporaryDirectory(prefix="dux-process-policy-")
        self.addCleanup(self.proc.cleanup)
        process_dir = Path(self.proc.name) / "4242"
        process_dir.mkdir()
        (process_dir / "oom_score_adj").write_text("0\n")
        (process_dir / "status").write_text(
            "Uid:\t1001\t1001\t1001\t1001\nGid:\t1002\t1002\t1002\t1002\n"
            "CapEff:\t0000000000200004\nCapPrm:\t0000000000200004\n"
            "CapBnd:\t0000000000200004\nCapInh:\t0000000000200004\n"
            "CapAmb:\t0000000000200004\n"
        )
        self.properties = {
            "User": "dux",
            "Group": "dux",
            "AmbientCapabilities": "cap_dac_read_search cap_sys_admin",
            "CapabilityBoundingSet": "cap_dac_read_search cap_sys_admin",
            "SecureBits": "3",
            "NoNewPrivileges": "yes",
            "UMask": "0027",
            "StateDirectory": "dux",
            "StateDirectoryMode": "0750",
            "MemoryHigh": str(2 * 1024**3),
            "MemoryMax": str(4 * 1024**3),
            "CPUQuotaPerSecUSec": "1s",
            "IOAccounting": "yes",
            "IOReadBytes": "1024",
            "IOWriteBytes": "2048",
            "OOMScoreAdjust": "0",
            "LimitCORE": "0",
            "ProtectKernelModules": "no",
            "SystemCallFilter": (
                "~@clock @obsolete chroot delete_module finit_module init_module kexec_file_load "
                "kexec_load lookup_dcookie modify_ldt mount pciconfig_iobase "
                "pciconfig_read pciconfig_write perf_event_open pidfd_getfd "
                "pivot_root ptrace reboot swapoff swapon umount umount2 ioperm "
                "iopl vm86 vm86old bpf"
            ),
            "RestrictAddressFamilies": "AF_UNIX",
            "ExecStart": (
                "path=/usr/bin/dux ; argv[]=/usr/bin/dux daemon / "
                "; ignore_errors=no ; start_time=[n/a]"
            ),
            "MainPID": "4242",
        }
        self.syscall_group_output = """@clock
    # Change the system time
    adjtimex
    clock_adjtime
    clock_adjtime64
    clock_settime
    clock_settime64
    settimeofday

@obsolete
    # Unusual, obsolete or unimplemented system calls
    _sysctl
    afs_syscall
    bdflush
    break
    create_module
    ftime
    get_kernel_syms
    getpmsg
    gtty
    idle
    lock
    mpx
    prof
    profil
    putpmsg
    query_module
    security
    sgetmask
    ssetmask
    stime
    stty
    sysfs
    tuxcall
    ulimit
    uselib
    ustat
    vserver
"""

    def check_properties(self):
        output = "\n".join(f"{key}={value}" for key, value in self.properties.items())
        def fake_command(*args, **kwargs):
            if args[:2] == ("systemd-analyze", "syscall-filter"):
                return SimpleNamespace(
                    returncode=0, stdout=self.syscall_group_output, stderr=""
                )
            return SimpleNamespace(returncode=0, stdout=output, stderr="")

        with patch.object(LIFECYCLE, "command", side_effect=fake_command), \
             patch.object(LIFECYCLE.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=1001)), \
             patch.object(LIFECYCLE.grp, "getgrnam", return_value=SimpleNamespace(gr_gid=1002)):
            return LIFECYCLE.assert_service_security_policy(proc_root=self.proc.name)

    def use_expanded_syscall_filter(self):
        explicit = set(self.properties["SystemCallFilter"].split())
        explicit.discard("~@clock")
        explicit.discard("@obsolete")
        expanded = {
            line.strip()
            for line in self.syscall_group_output.splitlines()
            if line.strip() and not line.strip().startswith(("@", "#"))
        }
        self.properties["SystemCallFilter"] = "~" + " ".join(
            sorted(explicit | expanded)
        )

    def test_accepts_restricted_effective_service_unit(self):
        observed = self.check_properties()
        self.assertEqual(observed["User"], "dux")
        self.assertEqual(observed["Group"], "dux")
        self.assertEqual(observed["ProcessUID"], 1001)
        self.assertEqual(observed["ProcessGID"], 1002)
        self.assertEqual(observed["ProcessCapabilities"]["CapEff"], "0x200004")

    def test_rejects_root_service_identity(self):
        self.properties["User"] = "root"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_unexpected_runtime_capabilities(self):
        status_path = Path(self.proc.name) / "4242" / "status"
        status_path.write_text(status_path.read_text().replace(
            "CapEff:\t0000000000200004", "CapEff:\t000000000020000c"
        ))
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_unexpected_inheritable_capabilities(self):
        status_path = Path(self.proc.name) / "4242" / "status"
        status_path.write_text(status_path.read_text().replace(
            "CapInh:\t0000000000200004", "CapInh:\t000000000020000c"
        ))
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_accepts_systemd_expanded_clock_and_obsolete_deny_groups(self):
        self.use_expanded_syscall_filter()
        self.assertEqual(self.check_properties()["Group"], "dux")

    def test_rejects_missing_syscall_from_expanded_clock_group(self):
        self.use_expanded_syscall_filter()
        self.properties["SystemCallFilter"] = self.properties["SystemCallFilter"].replace(
            "clock_settime ", ""
        )
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_syscall_from_expanded_obsolete_group(self):
        self.use_expanded_syscall_filter()
        self.properties["SystemCallFilter"] = self.properties["SystemCallFilter"].replace(
            "_sysctl ", ""
        )
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_added_ambient_capability(self):
        self.properties["AmbientCapabilities"] += " cap_sys_ptrace"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_root_capability_regain_policy(self):
        self.properties["SecureBits"] = "0"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_no_new_privileges(self):
        self.properties["NoNewPrivileges"] = "no"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_unrestricted_socket_families_for_default_daemon(self):
        self.properties["RestrictAddressFamilies"] = ""
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_network_families_without_alert_exec(self):
        self.properties["RestrictAddressFamilies"] = "AF_UNIX AF_INET AF_INET6"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_accepts_network_families_with_explicit_alert_exec(self):
        self.properties["RestrictAddressFamilies"] = "AF_UNIX AF_INET AF_INET6"
        self.properties["ExecStart"] = (
            "path=/usr/bin/dux ; argv[]=/usr/bin/dux daemon / "
            "--alert-exec /usr/local/bin/disk-alert ; ignore_errors=no"
        )
        self.assertEqual(self.check_properties()["Group"], "dux")

    def test_accepts_alert_exec_equals_form(self):
        self.properties["RestrictAddressFamilies"] = "AF_UNIX AF_INET AF_INET6"
        self.properties["ExecStart"] = (
            "path=/usr/bin/dux ; argv[]=/usr/bin/dux daemon / "
            "--alert-exec=/usr/local/bin/disk-alert ; ignore_errors=no"
        )
        self.assertEqual(self.check_properties()["Group"], "dux")

    def test_rejects_alert_exec_lookalike_option(self):
        self.properties["RestrictAddressFamilies"] = "AF_UNIX AF_INET AF_INET6"
        self.properties["ExecStart"] = (
            "path=/usr/bin/dux ; argv[]=/usr/bin/dux daemon / "
            "--alert-exec-helper /usr/local/bin/disk-alert ; ignore_errors=no"
        )
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_alert_exec_text_inside_another_argument(self):
        self.properties["RestrictAddressFamilies"] = "AF_UNIX AF_INET AF_INET6"
        self.properties["ExecStart"] = (
            "path=/usr/bin/dux ; argv[]=/usr/bin/dux daemon / "
            "/usr/local/bin/--alert-exec ; ignore_errors=no"
        )
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_preferred_oom_victim_unit_override(self):
        self.properties["OOMScoreAdjust"] = "800"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_core_dump_limit_override(self):
        self.properties["LimitCORE"] = "infinity"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_process_score_override_by_binary(self):
        (Path(self.proc.name) / "4242" / "oom_score_adj").write_text("800\n")
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_module_namespace_that_hides_indexed_tree(self):
        self.properties["ProtectKernelModules"] = "yes"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_module_syscall_filter(self):
        self.properties["SystemCallFilter"] = ""
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_bpf_with_sys_admin_capability(self):
        self.properties["SystemCallFilter"] = self.properties["SystemCallFilter"].replace(
            " bpf", ""
        )
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_extra_denylist_syscalls(self):
        self.properties["SystemCallFilter"] = "~delete_module finit_module init_module"
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_clock_change_deny_group(self):
        self.properties["SystemCallFilter"] = self.properties["SystemCallFilter"].replace("@clock ", "")
        with self.assertRaises(AssertionError):
            self.check_properties()

    def test_rejects_missing_obsolete_syscall_deny_group(self):
        self.properties["SystemCallFilter"] = self.properties["SystemCallFilter"].replace("@obsolete ", "")
        with self.assertRaises(AssertionError):
            self.check_properties()


class ServiceMountVisibilityTests(unittest.TestCase):
    def test_accepts_same_host_path_identity(self):
        with tempfile.TemporaryDirectory(prefix="dux-service-root-") as temporary:
            root = Path(temporary)
            host_path = root / "host" / "usr" / "lib" / "modules"
            host_path.mkdir(parents=True)
            service_root = root / "proc" / "4242" / "root"
            service_path = service_root / host_path.relative_to("/")
            service_path.parent.mkdir(parents=True)
            service_path.symlink_to(host_path, target_is_directory=True)
            observed = LIFECYCLE.assert_service_path_visible(
                host_path, 4242, proc_root=root / "proc"
            )
            self.assertEqual(observed["inode"], host_path.stat().st_ino)

    def test_rejects_masked_service_path(self):
        with tempfile.TemporaryDirectory(prefix="dux-service-root-") as temporary:
            root = Path(temporary)
            host_path = root / "host" / "usr" / "lib" / "modules"
            host_path.mkdir(parents=True)
            service_path = (
                root / "proc" / "4242" / "root" / host_path.relative_to("/")
            )
            service_path.mkdir(parents=True)
            with self.assertRaises(AssertionError):
                LIFECYCLE.assert_service_path_visible(
                    host_path, 4242, proc_root=root / "proc"
                )


class OomRolePolicySourceTests(unittest.TestCase):
    def test_positive_oom_bias_is_limited_to_one_off_scan(self):
        root = Path(__file__).resolve().parents[1]
        guard_source = (root / "src/guard.rs").read_text()
        scan_source = (root / "src/main.rs").read_text()
        daemon_source = (root / "src/watch.rs").read_text()
        self.assertIn("pub fn oom_protect_scan()", guard_source)
        self.assertIn('"/proc/self/oom_score_adj", "800"', guard_source)
        self.assertIn("guard::oom_protect_scan()", scan_source)
        self.assertNotIn("oom_protect_scan", daemon_source)
        self.assertNotIn("oom_protect_self", guard_source + scan_source + daemon_source)


class ServiceRestartPolicySourceTests(unittest.TestCase):
    def test_service_restart_delay_uses_portable_systemd_directives(self):
        root = Path(__file__).resolve().parents[1]
        service = (root / "packaging/dux.service").read_text()
        self.assertIn("Restart=always", service)
        self.assertIn("RestartSec=30s", service)
        self.assertNotIn("RestartSteps=", service)
        self.assertNotIn("RestartMaxDelaySec=", service)
        self.assertIn("TimeoutStopSec=300s", service)

    def test_default_daemon_unit_restricts_network_address_families(self):
        root = Path(__file__).resolve().parents[1]
        service = (root / "packaging/dux.service").read_text()
        address_families = [
            line.partition("=")[2].strip()
            for line in service.splitlines()
            if line.startswith("RestrictAddressFamilies=")
        ]
        exec_start = next(
            line.partition("=")[2]
            for line in service.splitlines()
            if line.startswith("ExecStart=")
        )
        self.assertEqual(address_families, ["AF_UNIX"])
        self.assertNotIn("--alert-exec", exec_start)

    def test_alert_documentation_overrides_the_existing_service(self):
        root = Path(__file__).resolve().parents[1]
        readme = (root / "README.md").read_text()
        self.assertIn("systemctl edit dux.service", readme)
        self.assertIn("ExecStart=/usr/bin/dux daemon / --alert-threshold", readme)
        self.assertIn("RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6", readme)
        self.assertNotIn("\ndux daemon / --alert-threshold", readme)


class TunnelUnitHardeningTests(unittest.TestCase):
    def test_tunnel_requires_operator_identity_config_and_verified_host_keys(self):
        root = Path(__file__).resolve().parents[1]
        service = (root / "packaging/dux-tunnel.service").read_text()
        for path in (
            "/etc/dux/ed25519",
            "/etc/dux/tunnel.conf",
            "/etc/dux/known_hosts",
        ):
            with self.subTest(path=path):
                self.assertIn(f"ConditionPathExists={path}", service)

    def test_optional_tunnel_has_kernel_and_memory_sandboxing(self):
        root = Path(__file__).resolve().parents[1]
        service = (root / "packaging/dux-tunnel.service").read_text()
        for setting in (
            "NoNewPrivileges=yes",
            "PrivateDevices=yes",
            "ProtectClock=yes",
            "ProtectControlGroups=yes",
            "ProtectHostname=yes",
            "ProtectKernelLogs=yes",
            "ProtectKernelModules=yes",
            "ProtectKernelTunables=yes",
            "RestrictNamespaces=yes",
            "RestrictRealtime=yes",
            "SystemCallArchitectures=native",
            "SystemCallFilter=~@module @mount @swap @reboot @clock @obsolete @raw-io @debug @cpu-emulation open_by_handle_at",
            "MemoryDenyWriteExecute=yes",
            "LockPersonality=yes",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            "CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_DAC_OVERRIDE",
        ):
            with self.subTest(setting=setting):
                self.assertIn(setting, service)


class PackagePayloadHashTests(unittest.TestCase):
    def test_reinstalls_same_version_when_binary_payload_differs(self):
        self.assertTrue(LIFECYCLE.baseline_install_needed(
            "0.6.3-16", "0.6.3-16", "stale-binary", "package-binary"
        ))

    def test_keeps_exact_same_version_baseline(self):
        self.assertFalse(LIFECYCLE.baseline_install_needed(
            "0.6.3-16", "0.6.3-16", "package-binary", "package-binary"
        ))

    def test_replaces_missing_or_wrong_version_baseline(self):
        self.assertTrue(LIFECYCLE.baseline_install_needed(
            None, "0.6.3-16", None, "package-binary"
        ))
        self.assertTrue(LIFECYCLE.baseline_install_needed(
            "0.6.3-15", "0.6.3-16", "package-binary", "package-binary"
        ))


class RollbackIndexBackupTests(unittest.TestCase):
    def test_report_directory_must_be_private_and_real(self):
        with tempfile.TemporaryDirectory(prefix="dux-report-private-") as temporary:
            directory = Path(temporary)
            report = LIFECYCLE.validate_report_destination(directory / "attempt.json")
            self.assertEqual(report, directory / "attempt.json")

            public = directory / "public"
            public.mkdir(mode=0o755)
            os.chmod(public, 0o755)
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.validate_report_destination(public / "attempt.json")
            self.assertEqual(raised.exception.args[0]["stage"], "report_directory_security")

            readonly = directory / "readonly"
            readonly.mkdir(mode=0o700)
            os.chmod(readonly, 0o500)
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.validate_report_destination(readonly / "attempt.json")
            self.assertEqual(raised.exception.args[0]["stage"], "report_directory_security")

    def test_report_directory_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="dux-report-link-") as temporary:
            root = Path(temporary)
            private = root / "private"
            private.mkdir(mode=0o700)
            link = root / "link"
            link.symlink_to(private, target_is_directory=True)

            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.validate_report_destination(link / "attempt.json")
            self.assertEqual(raised.exception.args[0]["stage"], "report_directory_security")

    def test_report_and_rollback_destinations_must_be_unique(self):
        with tempfile.TemporaryDirectory(prefix="dux-report-collision-") as temporary:
            directory = Path(temporary)
            report = directory / "attempt.json"
            report.write_text("old report")
            with self.assertRaises(FileExistsError) as raised:
                LIFECYCLE.validate_report_destination(report)
            self.assertEqual(raised.exception.args[0]["stage"], "report_destination_collision")
            report.unlink()

            temporary_path = report.with_suffix(report.suffix + ".tmp")
            temporary_path.symlink_to(directory / "outside")
            with self.assertRaises(FileExistsError) as raised:
                LIFECYCLE.validate_report_destination(report)
            self.assertEqual(raised.exception.args[0]["stage"], "report_temporary_collision")
            temporary_path.unlink()

            backup = LIFECYCLE.rollback_backup_path_for_report(report)
            backup.write_bytes(b"previous rollback snapshot")
            with self.assertRaises(FileExistsError) as raised:
                LIFECYCLE.validate_report_destination(report)
            self.assertEqual(raised.exception.args[0]["stage"], "rollback_backup_collision")

    def test_unsafe_report_preflight_runs_before_guest_or_package_work(self):
        with patch.object(sys, "argv", [
            "dux-package-lifecycle.py", "deb", "old.deb", "new.deb",
            "--report", "/tmp/dux-unsafe-report.json",
        ]), patch.object(LIFECYCLE, "require_disposable_qualification_vm") as require_guest:
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.main()
        self.assertEqual(raised.exception.args[0]["stage"], "report_directory_security")
        require_guest.assert_not_called()

    def test_rollback_backup_path_is_unique_per_report_attempt(self):
        first = LIFECYCLE.rollback_backup_path_for_report(
            "/root/dux-package-lifecycle-attempt1.json"
        )
        second = LIFECYCLE.rollback_backup_path_for_report(
            "/root/dux-package-lifecycle-attempt2.json"
        )
        self.assertEqual(first, Path("/root/dux-package-lifecycle-attempt1-rollback.sqlite"))
        self.assertNotEqual(first, second)

    def test_online_backup_restores_verified_snapshot_and_permissions(self):
        with tempfile.TemporaryDirectory(prefix="dux-rollback-backup-") as temporary:
            directory = Path(temporary)
            database = directory / "dux.db"
            backup = directory / "rollback.sqlite"
            connection = sqlite3.connect(database)
            connection.execute("CREATE TABLE values_table(value TEXT)")
            connection.execute("INSERT INTO values_table VALUES('before-upgrade')")
            connection.commit()
            connection.close()
            os.chmod(database, 0o640)

            with patch.object(
                Path, "read_bytes",
                side_effect=AssertionError("rollback hashing must stream large files"),
            ):
                evidence = LIFECYCLE.create_rollback_index_backup(database, backup)
            self.assertEqual(evidence["quick_check"], "ok")
            self.assertEqual(evidence["mode"], "0o640")
            self.assertEqual(evidence["sha256"], LIFECYCLE._sha256_file(backup))

            connection = sqlite3.connect(database)
            connection.execute("INSERT INTO values_table VALUES('after-upgrade')")
            connection.commit()
            connection.close()
            Path(f"{database}-wal").write_bytes(b"stale wal")
            Path(f"{database}-shm").symlink_to(directory / "missing-shm")
            Path(f"{database}-journal").write_bytes(b"stale rollback journal")

            real_connect = sqlite3.connect
            temporary_modes = []

            def inspect_connect(path, *args, **kwargs):
                connection = real_connect(path, *args, **kwargs)
                candidate = Path(path)
                if candidate.name.startswith(f".{database.name}.rollback-restore."):
                    temporary_modes.append(stat.S_IMODE(candidate.stat().st_mode))
                return connection

            with (
                patch.object(LIFECYCLE.sqlite3, "connect", side_effect=inspect_connect),
                patch.object(
                    Path, "read_bytes",
                    side_effect=AssertionError("rollback hashing must stream large files"),
                ),
            ):
                restored = LIFECYCLE.restore_rollback_index_backup(database, backup)

            connection = sqlite3.connect(database)
            values = connection.execute("SELECT value FROM values_table").fetchall()
            check = connection.execute("PRAGMA quick_check").fetchone()[0]
            connection.close()
            self.assertEqual(values, [("before-upgrade",)])
            self.assertEqual(check, "ok")
            self.assertEqual(restored["mode"], "0o640")
            self.assertEqual(temporary_modes, [0o600])
            self.assertFalse(Path(f"{database}-wal").exists())
            self.assertFalse(Path(f"{database}-shm").exists())
            self.assertFalse(Path(f"{database}-shm").is_symlink())
            self.assertFalse(Path(f"{database}-journal").exists())


class DaemonReadinessTests(unittest.TestCase):
    def test_legacy_baseline_timeout_matches_systemd_start_allowance(self):
        self.assertEqual(
            LIFECYCLE.LEGACY_BASELINE_SCAN_TIMEOUT_SECONDS, 3600
        )
        self.assertEqual(LIFECYCLE.DAEMON_READINESS_POLL_SECONDS, 2.0)

    def test_waits_until_full_scan_is_caught_up(self):
        scanning = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "paused": False, "scan_in_progress": True,
            "caught_up": False,
        }
        ready = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": False, "paused": False, "scan_in_progress": False,
            "caught_up": True,
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(scanning), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(ready), stderr=""),
        ]), patch.object(LIFECYCLE.time, "monotonic", return_value=0), \
                patch.object(LIFECYCLE.time, "sleep") as sleep:
            observed = LIFECYCLE.wait_daemon_caught_up(10, "test")

        self.assertTrue(observed["caught_up"])
        sleep.assert_called_once_with(LIFECYCLE.DAEMON_READINESS_POLL_SECONDS)

    def test_legacy_baseline_records_completed_but_incomplete_scan(self):
        baseline = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "dirty_reason": "incomplete coverage",
            "paused": False, "scan_in_progress": False, "caught_up": False,
            "scan_first": True,
            "last_scan_timestamp_seconds": 123,
        }
        with patch.object(LIFECYCLE, "command", return_value=SimpleNamespace(
            returncode=0, stdout=json.dumps(baseline), stderr=""
        )) as command:
            observed = LIFECYCLE.wait_initial_scan_complete(
                10, "legacy-baseline", not_before_timestamp=100
            )

        self.assertEqual(observed, baseline)
        command.assert_called_once_with("/usr/bin/dux", "status", "--json", check=False)

    def test_clean_install_waits_for_accounting_verification_after_scan(self):
        accounting_pending = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "dirty_reason": "reconciliation accounting verification pending",
            "paused": False, "scan_in_progress": False, "caught_up": False,
            "scan_first": True, "last_scan_timestamp_seconds": 123,
        }
        accounted = {
            **accounting_pending,
            "dirty_reason": "filesystem changes await the next full observation",
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(accounting_pending), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(accounted), stderr=""),
        ]) as command, patch.object(LIFECYCLE.time, "monotonic", return_value=0), \
                patch.object(LIFECYCLE.time, "sleep") as sleep:
            observed = LIFECYCLE.wait_initial_scan_complete(
                10, "clean-install", not_before_timestamp=100,
                require_accounting=True,
            )

        self.assertEqual(observed, accounted)
        self.assertEqual(command.call_count, 2)
        sleep.assert_called_once_with(LIFECYCLE.DAEMON_READINESS_POLL_SECONDS)

    def test_clean_install_rejects_incomplete_and_failed_accounting_states(self):
        incomplete = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True,
            "dirty_reason": "incomplete coverage or changes lost during reconciliation",
            "paused": False, "scan_in_progress": False, "caught_up": False,
            "scan_first": True, "last_scan_timestamp_seconds": 123,
        }
        accounting_failed = {
            **incomplete,
            "dirty_reason": "directory accounting invariant failed; one bounded automatic reconciliation retry remains",
        }
        accounted = {
            **incomplete,
            "dirty": False,
            "dirty_reason": None,
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(incomplete), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(accounting_failed), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(accounted), stderr=""),
        ]) as command, patch.object(LIFECYCLE.time, "monotonic", return_value=0), \
                patch.object(LIFECYCLE.time, "sleep") as sleep:
            observed = LIFECYCLE.wait_initial_scan_complete(
                10, "clean-install", not_before_timestamp=100,
                require_accounting=True,
            )

        self.assertEqual(observed, accounted)
        self.assertEqual(command.call_count, 3)
        self.assertEqual(sleep.call_count, 2)

    def test_legacy_baseline_does_not_accept_dirty_state_without_completed_scan(self):
        stale = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "paused": False, "scan_in_progress": False,
            "caught_up": False, "last_scan_timestamp_seconds": 99,
        }
        scanning = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "paused": False, "scan_in_progress": True,
            "caught_up": False, "last_scan_timestamp_seconds": 100,
        }
        complete = {**scanning, "scan_in_progress": False,
                    "last_scan_timestamp_seconds": 123}
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(stale), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(scanning), stderr=""),
            SimpleNamespace(returncode=0, stdout=json.dumps(complete), stderr=""),
        ]), patch.object(LIFECYCLE.time, "monotonic", return_value=0), \
                patch.object(LIFECYCLE.time, "sleep") as sleep:
            observed = LIFECYCLE.wait_initial_scan_complete(
                10, "legacy-baseline", not_before_timestamp=100
            )

        self.assertFalse(observed["caught_up"])
        self.assertEqual(sleep.call_count, 2)
        sleep.assert_called_with(LIFECYCLE.DAEMON_READINESS_POLL_SECONDS)

    def test_rejects_active_but_dirty_daemon_at_timeout(self):
        dirty = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "paused": False, "scan_in_progress": False,
            "caught_up": False,
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(dirty), stderr=""),
            SimpleNamespace(returncode=0, stdout="scan incomplete", stderr=""),
        ]) as command, patch.object(LIFECYCLE.time, "monotonic", side_effect=[0, 2]), \
                patch.object(LIFECYCLE.time, "sleep"):
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.wait_daemon_caught_up(1, "test")

        self.assertEqual(raised.exception.args[0]["daemon_not_caught_up"], dirty)
        self.assertEqual(command.call_count, 2)

    def test_null_dirty_reason_does_not_break_caught_up_polling(self):
        status = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "dirty_reason": None, "paused": False,
            "scan_in_progress": False, "caught_up": False,
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(status), stderr=""),
            SimpleNamespace(returncode=0, stdout="status unavailable", stderr=""),
        ]) as command, patch.object(LIFECYCLE.time, "monotonic", side_effect=[0, 2]), \
                patch.object(LIFECYCLE.time, "sleep"):
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.wait_daemon_caught_up(1, "null-dirty-reason")

        self.assertIn("daemon_not_caught_up", raised.exception.args[0])
        self.assertEqual(command.call_count, 2)

    def test_fails_immediately_when_reconciliation_retry_circuit_is_paused(self):
        paused = {
            "daemon_live": True, "watch_status_available": True,
            "dirty": True, "dirty_reason": "automatic retries paused after failures",
            "paused": False, "scan_in_progress": False, "caught_up": False,
        }
        with patch.object(LIFECYCLE, "command", side_effect=[
            SimpleNamespace(returncode=0, stdout=json.dumps(paused), stderr=""),
            SimpleNamespace(returncode=0, stdout="scan failed", stderr=""),
        ]) as command, patch.object(LIFECYCLE.time, "monotonic", return_value=0), \
                patch.object(LIFECYCLE.time, "sleep") as sleep:
            with self.assertRaises(AssertionError) as raised:
                LIFECYCLE.wait_daemon_caught_up(180, "test")

        self.assertIn("automatic_reconciliation_paused", raised.exception.args[0])
        self.assertEqual(command.call_count, 2)
        sleep.assert_not_called()

    def test_preserves_structured_failure_evidence_for_machine_readable_reports(self):
        evidence = {"stage": "clean_install_reconciliation", "caught_up": False}
        self.assertEqual(
            LIFECYCLE.structured_failure_evidence(AssertionError(evidence)), evidence
        )
        self.assertIsNone(LIFECYCLE.structured_failure_evidence(RuntimeError("failure")))

    def test_hashes_deb_payload_executable(self):
        def extract_deb(_manager, _extract, _package, directory):
            binary = Path(directory) / "usr/bin/dux"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"candidate executable")
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with patch.object(LIFECYCLE, "command", side_effect=extract_deb):
            observed = LIFECYCLE.package_binary_hash("deb", "candidate.deb")
        self.assertEqual(observed, hashlib.sha256(b"candidate executable").hexdigest())

    def test_hashes_rpm_payload_executable(self):
        def extract_rpm(argv, **kwargs):
            if argv[0] == "rpm2cpio":
                return SimpleNamespace(stdout=b"rpm archive")
            binary = Path(kwargs["cwd"]) / "usr/bin/dux"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"candidate executable")
            return SimpleNamespace(stdout=b"", stderr=b"")

        with patch.object(LIFECYCLE.subprocess, "run", side_effect=extract_rpm):
            observed = LIFECYCLE.package_binary_hash("rpm", "candidate.rpm")
        self.assertEqual(observed, hashlib.sha256(b"candidate executable").hexdigest())

    def test_hashes_deb_backup_helper_payload(self):
        helper_bytes = b"#!/usr/bin/python3\\nbackup helper"

        def extract_deb(_manager, _extract, _package, directory):
            helper = Path(directory) / "usr/lib/dux/dux-backup-index.py"
            helper.parent.mkdir(parents=True)
            helper.write_bytes(helper_bytes)
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with patch.object(LIFECYCLE, "command", side_effect=extract_deb):
            observed = LIFECYCLE.package_backup_helper_hash("deb", "candidate.deb")
        self.assertEqual(observed, hashlib.sha256(helper_bytes).hexdigest())

    def test_hashes_rpm_backup_helper_payload(self):
        helper_bytes = b"#!/usr/bin/python3\\nbackup helper"

        def extract_rpm(argv, **kwargs):
            if argv[0] == "rpm2cpio":
                return SimpleNamespace(stdout=b"rpm archive")
            helper = Path(kwargs["cwd"]) / "usr/lib/dux/dux-backup-index.py"
            helper.parent.mkdir(parents=True)
            helper.write_bytes(helper_bytes)
            return SimpleNamespace(stdout=b"", stderr=b"")

        with patch.object(LIFECYCLE.subprocess, "run", side_effect=extract_rpm):
            observed = LIFECYCLE.package_backup_helper_hash("rpm", "candidate.rpm")
        self.assertEqual(observed, hashlib.sha256(helper_bytes).hexdigest())

if __name__ == "__main__":
    unittest.main()
