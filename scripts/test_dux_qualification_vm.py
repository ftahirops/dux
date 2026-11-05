#!/usr/bin/env python3
import importlib.util
import io
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "dux_qualification_vm", Path(__file__).with_name("dux-qualification-vm.py")
)
VM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VM)


class PressureGuardTests(unittest.TestCase):
    def test_stop_accepts_a_systemd_collected_inactive_guest(self):
        stop_result = subprocess.CompletedProcess(['systemctl', 'stop', 'dux-vm'], 5)
        show_result = subprocess.CompletedProcess(
            ['systemctl', 'show', 'dux-vm'], 0,
            stdout='LoadState=not-found\nActiveState=inactive\n',
        )
        with patch.object(VM.subprocess, 'run', side_effect=[stop_result, show_result]):
            VM.stop_vm_unit('dux-vm')

    def test_stop_still_fails_when_systemd_reports_guest_active(self):
        stop_result = subprocess.CompletedProcess(['systemctl', 'stop', 'dux-vm'], 5)
        show_result = subprocess.CompletedProcess(
            ['systemctl', 'show', 'dux-vm'], 0,
            stdout='LoadState=loaded\nActiveState=active\n',
        )
        with patch.object(VM.subprocess, 'run', side_effect=[stop_result, show_result]):
            with self.assertRaises(subprocess.CalledProcessError):
                VM.stop_vm_unit('dux-vm')

    def test_vm_manager_refuses_optimized_python(self):
        script = Path(__file__).with_name("dux-qualification-vm.py")
        result = subprocess.run(
            [sys.executable, "-O", str(script), "--help"],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("do not use python -O", result.stderr)

    def test_parses_some_and_full_avg10(self):
        pressure = "some avg10=22.50 avg60=20.00 avg300=18.00 total=1\nfull avg10=0.25 avg60=0.10 avg300=0.05 total=2\n"
        self.assertEqual(VM.parse_psi_avg10(pressure, "some"), 22.5)
        self.assertEqual(VM.parse_psi_avg10(pressure, "full"), 0.25)

    def test_missing_or_malformed_psi_is_unknown(self):
        self.assertIsNone(VM.parse_psi_avg10("", "some"))
        self.assertIsNone(VM.parse_psi_avg10("some avg10=bad", "some"))
        self.assertIsNone(VM.parse_psi_avg10("some avg10=nan", "some"))
        self.assertIsNone(VM.parse_psi_avg10("some avg10=-1", "some"))

    def test_accepts_pressure_at_daemon_elevated_boundary(self):
        pressure = {
            "load1": 1.0,
            "cpu": {"some": 20.0},
            "io": {"some": 10.0},
            "memory": {"some": 5.0},
        }
        self.assertEqual(VM.host_pressure_blockers(pressure), [])

    def test_active_host_workloads_block_vm_admission_and_stop_guard(self):
        pressure = {
            "load1": 1.0,
            "cpu": {"some": 0.0, "full": 0.0},
            "io": {"some": 0.0},
            "memory": {"some": 0.0},
            "active_host_workloads": ["go build pid=42"],
        }
        expected = [
            "active host build/test/install/analysis workload: go build pid=42"
        ]
        self.assertEqual(VM.host_pressure_blockers(pressure), expected)
        self.assertEqual(VM.host_pressure_critical_blockers(pressure), expected)

    def test_host_workload_probe_detects_build_install_test_and_analysis(self):
        with tempfile.TemporaryDirectory(prefix="dux-host-workloads-") as temporary:
            proc_root = Path(temporary)
            processes = {
                "101": ["/usr/local/go/bin/go", "build", "./..."],
                "102": ["/usr/bin/go", "test", "./..."],
                "103": ["/root/.cargo/bin/cargo", "test", "--workspace"],
                "104": ["/root/.rustup/toolchains/stable/bin/rustc", "lib.rs"],
                "105": ["/root/go/pkg/mod/golang.org/toolchain/pkg/tool/linux_amd64/compile", "-o", "/tmp/go-build"],
                "106": ["/usr/bin/python3", "-c", "print('go build')"],
                "107": ["/usr/bin/go", "version"],
                "108": ["/usr/bin/go", "-C", "/home/project", "build", "./..."],
                "109": ["/usr/bin/cargo", "--manifest-path", "/tmp/Cargo.toml", "test"],
                "110": ["/usr/bin/go", "vet", "./..."],
                "111": ["/usr/bin/go", "list", "./..."],
                "112": ["/usr/bin/cargo", "clippy", "--workspace"],
                "113": ["/usr/local/bin/gosec", "-fmt=json", "./..."],
                "114": ["/usr/local/bin/golangci-lint", "run", "./..."],
                "115": ["/usr/bin/npm", "ci"],
                "116": ["/usr/bin/npm", "run", "build"],
                "117": ["/usr/bin/pnpm", "install", "--frozen-lockfile"],
                "118": ["/usr/bin/npm", "--version"],
                "119": ["/usr/bin/node", "/usr/lib/node_modules/npm/bin/npm-cli.js", "ci"],
                "120": ["npm ci", "/usr/lib/node_modules/npm/bin/npm-cli.js", "ci"],
                "121": ["/usr/bin/node", "/home/project/ui/node_modules/.bin/vitest", "run"],
                "122": ["/usr/bin/node", "/home/project/ui/node_modules/jest/bin/jest.js", "--runInBand"],
                "123": ["/usr/bin/node", "/srv/app/server.js"],
                "124": ["/usr/bin/node", "--version"],
                "125": ["/usr/bin/node", "--test", "test/*.test.js"],
                "126": ["/tmp/go-build123/b140/recovery.test", "-test.paniconexit0"],
                "127": ["/bin/bash", "scripts/test-fast.sh"],
                "128": ["/bin/bash", "-c", "echo test"],
            }
            for pid, argv in processes.items():
                process = proc_root / pid
                process.mkdir()
                (process / "cmdline").write_bytes(
                    b"\0".join(part.encode() for part in argv) + b"\0"
                )

            self.assertEqual(
                VM.active_host_workloads(proc_root),
                [
                    "Go compiler pid=105",
                    "Go test binary pid=126",
                    "cargo clippy pid=112",
                    "cargo test pid=103",
                    "cargo test pid=109",
                    "go build pid=101",
                    "go build pid=108",
                    "go list pid=111",
                    "go test pid=102",
                    "go vet pid=110",
                    "golangci-lint pid=114",
                    "gosec pid=113",
                    "node --test pid=125",
                    "node jest pid=122",
                    "node vitest pid=121",
                    "npm ci pid=115",
                    "npm ci pid=119",
                    "npm ci pid=120",
                    "npm run build pid=116",
                    "pnpm install pid=117",
                    "rustc pid=104",
                    "shell test script pid=127",
                ],
            )

    def test_host_workload_probe_fails_closed_when_process_cmdline_is_unreadable(self):
        with tempfile.TemporaryDirectory(prefix="dux-host-workloads-unreadable-") as temporary:
            process = Path(temporary) / "101"
            process.mkdir()
            (process / "cmdline").write_bytes(b"/usr/bin/go\0build\0./...\0")
            with patch.object(Path, "read_bytes", side_effect=PermissionError("denied")):
                with self.assertRaises(PermissionError):
                    VM.active_host_workloads(Path(temporary))

    def test_unavailable_host_workload_probe_blocks_admission(self):
        with patch.object(
            VM, "active_host_workloads", side_effect=PermissionError("denied")
        ):
            pressure = VM.host_pressure()
        self.assertEqual(pressure["active_host_workloads"], [])
        self.assertIn(
            "/proc workload scan is unavailable: denied",
            pressure["telemetry_errors"],
        )
        self.assertIn(
            "host pressure telemetry unavailable: /proc workload scan is unavailable: denied",
            VM.host_pressure_blockers(pressure),
        )

    def test_watch_unit_guest_uses_fresh_ubuntu_base(self):
        self.assertEqual(VM.GUEST_IMAGES["watch-unit"], "ubuntu24")
        self.assertEqual(VM.GUEST_IMAGES["ubuntu24-scale-reuse"], "ubuntu24")

    def test_watch_unit_writable_artifacts_live_on_tmpfs(self):
        workspace = Path('/home/dux/target/readiness/vms/watch-unit')
        self.assertEqual(
            VM.runtime_directory_for('watch-unit', workspace, 'unit-run'),
            Path('/dev/shm/dux-watch-unit-vm/unit-run'),
        )
        self.assertEqual(VM.runtime_directory_for('ubuntu24', workspace), workspace)

    def test_other_guest_can_use_a_unique_ephemeral_runtime(self):
        workspace = Path('/var/lib/dux-test')
        first = VM.runtime_directory_for('rocky9', workspace, 'run-a', ephemeral=True)
        second = VM.runtime_directory_for('rocky9', workspace, 'run-b', ephemeral=True)

        self.assertEqual(first, Path('/dev/shm/dux-qualification-vm/rocky9/run-a'))
        self.assertNotEqual(first, second)
        self.assertEqual(VM.runtime_directory_for('rocky9', workspace), workspace)

    def test_private_overlay_runner_quotes_paths_and_keeps_disk_writes_private(self):
        image = Path('/srv/test images/ubuntu;touch-owned.img')
        overlay = VM.PRIVATE_OVERLAY_MOUNTPOINT / 'disk.qcow2'
        qemu_args = [
            'qemu-system-x86_64', '-drive',
            f'file={overlay},format=qcow2,if=virtio',
            '-serial', 'file:/dev/shm/dux-vm-run/console.log',
        ]

        script = VM.private_overlay_runner_script(image, overlay, qemu_args)
        lines = script.splitlines()

        self.assertEqual(lines[:2], ['#!/bin/sh', 'set -eu'])
        self.assertEqual(
            shlex.split(lines[2]),
            [
                'qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b',
                str(image), str(overlay), '20G',
            ],
        )
        self.assertTrue(lines[3].startswith('exec '))
        self.assertEqual(shlex.split(lines[3][len('exec '):]), qemu_args)

    def test_ephemeral_overlay_uses_private_mount_namespace_and_bounded_tmpfs(self):
        self.assertEqual(
            VM.private_overlay_unit_properties(8192),
            [
                '--property=PrivateMounts=yes',
                '--property=TemporaryFileSystem=/run/dux-qualification-overlay:rw,size=8192M',
            ],
        )

    def test_ephemeral_lifecycle_slots_do_not_reuse_persistent_guest_state(self):
        self.assertEqual(VM.GUEST_IMAGES['ubuntu24-lifecycle-ephemeral'], 'ubuntu24')
        self.assertEqual(VM.GUEST_IMAGES['rocky9-lifecycle-ephemeral'], 'rocky9')
        workspace = Path('/qualification/ubuntu24-lifecycle-ephemeral')
        self.assertNotEqual(VM.statefile_for(workspace), VM.statefile_for(workspace, ephemeral=True))
        self.assertEqual(VM.statefile_for(workspace, ephemeral=True).name, 'ephemeral-state.json')

    def test_ephemeral_cleanup_removes_only_verified_guest_tmpfs_directory(self):
        with tempfile.TemporaryDirectory(prefix='dux-ephemeral-cleanup-') as temporary:
            tmpfs_root = Path(temporary)
            runtime = tmpfs_root / 'ubuntu24-lifecycle-ephemeral' / '123456'
            runtime.mkdir(parents=True, mode=0o700)
            runtime.chmod(0o700)
            (runtime / 'disk.qcow2').write_bytes(b'guest overlay')
            with (
                patch.object(VM, 'EPHEMERAL_RUNTIME_DIRECTORY', tmpfs_root),
                patch.object(VM, 'filesystem_type_for', return_value='tmpfs'),
            ):
                self.assertTrue(VM.cleanup_ephemeral_runtime(
                    'ubuntu24-lifecycle-ephemeral', runtime
                ))
                self.assertFalse(runtime.exists())

    def test_ephemeral_cleanup_rejects_paths_outside_dedicated_guest_slot(self):
        with tempfile.TemporaryDirectory(prefix='dux-ephemeral-cleanup-') as temporary:
            tmpfs_root = Path(temporary) / 'runtime'
            runtime = Path(temporary) / 'valuable-data'
            runtime.mkdir()
            marker = runtime / 'keep'
            marker.write_text('preserve')
            with patch.object(VM, 'EPHEMERAL_RUNTIME_DIRECTORY', tmpfs_root):
                with self.assertRaisesRegex(ValueError, 'unexpected ephemeral guest path'):
                    VM.cleanup_ephemeral_runtime('ubuntu24-lifecycle-ephemeral', runtime)
            self.assertEqual(marker.read_text(), 'preserve')

    def test_stale_ephemeral_cleanup_removes_only_numeric_run_directories(self):
        with tempfile.TemporaryDirectory(prefix='dux-stale-vm-runtimes-') as temporary:
            root = Path(temporary)
            stale = root / '1793321637294526451'
            stale.mkdir(mode=0o700)
            (stale / 'disk.qcow2').write_bytes(b'disposable overlay')
            unrelated = root / 'preserve-me'
            unrelated.mkdir()
            (unrelated / 'evidence.txt').write_text('keep')
            with (
                patch.object(VM, 'WATCH_UNIT_RUNTIME_DIRECTORY', root),
                patch.object(VM, 'filesystem_type_for', return_value='tmpfs'),
            ):
                removed = VM.cleanup_stale_ephemeral_runtimes('watch-unit')
            self.assertEqual(removed, [stale])
            self.assertFalse(stale.exists())
            self.assertEqual((unrelated / 'evidence.txt').read_text(), 'keep')

    def test_stale_ephemeral_cleanup_closes_legacy_directory_permissions(self):
        with tempfile.TemporaryDirectory(prefix='dux-stale-vm-mode-') as temporary:
            root = Path(temporary)
            stale = root / '1793321637294526451'
            stale.mkdir(mode=0o700)
            stale.chmod(0o755)
            (stale / 'disk.qcow2').write_bytes(b'disposable overlay')
            with (
                patch.object(VM, 'WATCH_UNIT_RUNTIME_DIRECTORY', root),
                patch.object(VM, 'filesystem_type_for', return_value='tmpfs'),
            ):
                removed = VM.cleanup_stale_ephemeral_runtimes('watch-unit')
            self.assertEqual(removed, [stale])
            self.assertFalse(stale.exists())

    def test_stale_ephemeral_cleanup_rejects_numeric_symlink(self):
        with tempfile.TemporaryDirectory(prefix='dux-stale-vm-symlink-') as temporary:
            root = Path(temporary) / 'runtime'
            root.mkdir()
            outside = Path(temporary) / 'keep'
            outside.mkdir()
            marker = outside / 'evidence.txt'
            marker.write_text('keep')
            (root / '1793321637294526451').symlink_to(outside, target_is_directory=True)
            with patch.object(VM, 'WATCH_UNIT_RUNTIME_DIRECTORY', root):
                with self.assertRaisesRegex(ValueError, 'unexpected stale ephemeral runtime'):
                    VM.cleanup_stale_ephemeral_runtimes('watch-unit')
            self.assertEqual(marker.read_text(), 'keep')

    def test_quota_pressure_is_allowed_when_host_load_and_full_psi_are_low(self):
        pressure = {
            "load1": 3.0,
            "cpu": {"some": 34.9, "full": 0.0},
            "io": {"some": 0.5},
            "memory": {"some": 0.0},
        }
        self.assertEqual(VM.host_pressure_blockers(pressure), [])

    def test_elevated_cpu_psi_requires_low_load_and_low_full_psi(self):
        pressure = {
            "load1": 3.0,
            "cpu": {"some": 36.0, "full": 5.1},
            "io": {"some": 0.0},
            "memory": {"some": 0.0},
        }
        self.assertEqual(
            VM.host_pressure_blockers(pressure),
            [
                "cpu PSI some avg10=36.00% requires load1<=10 and cpu PSI full avg10<=5%",
                "cpu PSI full avg10=5.10% exceeds 5%",
            ],
        )

    def test_refuses_guest_when_load_or_non_cpu_pressure_is_high(self):
        pressure = {
            "load1": 10.1,
            "cpu": {"some": 20.1},
            "io": {"some": 10.1},
            "memory": {"some": 5.1},
        }
        self.assertEqual(
            VM.host_pressure_blockers(pressure),
            [
                "host load1=10.10 exceeds 10",
                "io PSI some avg10=10.10% exceeds 10%",
                "memory PSI some avg10=5.10% exceeds 5%",
            ],
        )

    def test_unavailable_pressure_files_block_vm_admission(self):
        with (
            patch.object(VM.Path, 'read_text', side_effect=OSError('unavailable')),
            patch.object(VM, 'active_host_workloads', return_value=[]),
        ):
            pressure = VM.host_pressure()
        blockers = VM.host_pressure_blockers(pressure)
        self.assertEqual(len(blockers), 4)
        self.assertTrue(all('telemetry unavailable' in blocker for blocker in blockers))

    def test_missing_host_telemetry_blocks_vm_admission(self):
        pressure = {
            "load1": None,
            "cpu": {"some": None, "full": None},
            "io": {"some": None, "full": None},
            "memory": {"some": None, "full": None},
            "telemetry_errors": [
                "/proc/loadavg load1 is unavailable or invalid",
                "/proc/pressure/cpu is unavailable",
            ],
        }
        self.assertEqual(
            VM.host_pressure_blockers(pressure),
            [
                "host pressure telemetry unavailable: /proc/loadavg load1 is unavailable or invalid",
                "host pressure telemetry unavailable: /proc/pressure/cpu is unavailable",
            ],
        )

    def test_missing_host_telemetry_stops_running_vm_guard(self):
        pressure = {"telemetry_errors": ["/proc/pressure/memory is unavailable"]}
        self.assertEqual(
            VM.host_pressure_critical_blockers(pressure),
            ["host pressure telemetry unavailable: /proc/pressure/memory is unavailable"],
        )

    def test_runtime_guard_uses_critical_not_elevated_thresholds(self):
        pressure = {
            "load1": 2.0,
            "cpu": {"some": 40.0, "full": 0.0},
            "io": {"some": 40.0},
            "memory": {"some": 20.0},
        }
        self.assertEqual(VM.host_pressure_critical_blockers(pressure), [])
        pressure["cpu"]["some"] = 40.1
        self.assertEqual(
            VM.host_pressure_critical_blockers(pressure),
            [],
        )

    def test_runtime_guard_allows_quota_psi_but_stops_when_host_load_rises(self):
        pressure = {"load1": 2.0, "cpu": {"some": 48.0, "full": 0.0}}
        self.assertEqual(VM.host_pressure_critical_blockers(pressure), [])
        pressure["load1"] = 10.1
        self.assertEqual(
            VM.host_pressure_critical_blockers(pressure),
            ["cpu PSI some avg10=48.00% with high/unavailable host load or CPU full pressure"],
        )

    def test_runtime_guard_stops_on_hard_cpu_or_cpu_full_pressure(self):
        self.assertEqual(
            VM.host_pressure_critical_blockers(
                {"load1": 1.0, "cpu": {"some": 70.1, "full": 0.0}}
            ),
            ["cpu PSI some avg10=70.10% exceeds hard critical limit 70%"],
        )
        self.assertEqual(
            VM.host_pressure_critical_blockers(
                {"load1": 1.0, "cpu": {"some": 41.0, "full": 10.1}}
            ),
            [
                "cpu PSI some avg10=41.00% with high/unavailable host load or CPU full pressure",
                "cpu PSI full avg10=10.10% exceeds critical 10%",
            ],
        )

    def test_runtime_guard_stops_on_critical_host_load(self):
        self.assertEqual(
            VM.host_pressure_critical_blockers({"load1": 12.1}),
            ["host load1=12.10 exceeds critical 12"],
        )

    def test_runtime_guard_stops_only_its_vm_after_two_critical_samples(self):
        with tempfile.TemporaryDirectory(prefix="dux-vm-pressure-guard-") as temp:
            directory = Path(temp)
            runtime_root = directory / "tmpfs"
            runtime = runtime_root / "ubuntu24" / "12345"
            runtime.mkdir(parents=True, mode=0o700)
            overlay = runtime / "disk.qcow2"
            overlay.write_bytes(b"private disposable overlay")
            statefile = VM.statefile_for(directory, ephemeral=True)
            statefile.write_text(json.dumps({
                "unit": "dux-vm-guest.service",
                "guest": "ubuntu24",
                "ephemeral_runtime": True,
                "runtime_directory": str(runtime),
            }))
            critical = {"cpu": {"some": 41.0}, "io": {"some": 0.0}, "memory": {"some": 0.0}}
            with (
                patch.object(VM, "EPHEMERAL_RUNTIME_DIRECTORY", runtime_root),
                patch.object(VM, "filesystem_type_for", return_value="tmpfs"),
                patch.object(VM.subprocess, "run", side_effect=[
                    SimpleNamespace(returncode=0),
                    SimpleNamespace(returncode=0),
                    SimpleNamespace(returncode=3),
                ]),
                patch.object(VM, "host_pressure", side_effect=[critical, critical]),
                patch.object(VM, "run") as run,
                patch.object(VM.time, "sleep") as sleep,
                patch("builtins.print") as output,
            ):
                VM.run_pressure_guard("ubuntu24", directory, "dux-vm-guest.service")

            run.assert_called_once_with("systemctl", "stop", "dux-vm-guest.service")
            sleep.assert_called_once_with(5)
            event = json.loads((directory / "pressure-guard-events.jsonl").read_text())
            self.assertEqual(event["action"], "stopped_disposable_vm")
            self.assertEqual(event["vm_unit"], "dux-vm-guest.service")
            output.assert_called_once()
            self.assertFalse(runtime.exists())
            self.assertTrue(json.loads(statefile.read_text())["ephemeral_runtime_cleaned"])

    def test_stopped_guest_cleanup_refuses_an_active_unit(self):
        with tempfile.TemporaryDirectory(prefix="dux-vm-active-cleanup-") as temp:
            directory = Path(temp)
            runtime_root = directory / "tmpfs"
            runtime = runtime_root / "ubuntu24" / "98765"
            runtime.mkdir(parents=True, mode=0o700)
            statefile = VM.statefile_for(directory, ephemeral=True)
            statefile.write_text(json.dumps({
                "unit": "dux-vm-guest.service",
                "guest": "ubuntu24",
                "ephemeral_runtime": True,
                "runtime_directory": str(runtime),
            }))
            with (
                patch.object(VM, "EPHEMERAL_RUNTIME_DIRECTORY", runtime_root),
                patch.object(VM.subprocess, "run", return_value=SimpleNamespace(returncode=0)),
            ):
                with self.assertRaisesRegex(RuntimeError, "while guest unit is active"):
                    VM.cleanup_stopped_guest_runtime(
                        "ubuntu24", statefile, "dux-vm-guest.service"
                    )
            self.assertTrue(runtime.is_dir())


class CommandSafetyTests(unittest.TestCase):
    def test_help_after_start_arguments_does_not_start_a_vm(self):
        with (
            patch.object(
                VM.sys,
                "argv",
                ["dux-qualification-vm.py", "start", "watch-unit", "--help"],
            ),
            patch.object(VM, "run") as run,
            patch("sys.stdout", new_callable=io.StringIO) as output,
        ):
            VM.main()

        run.assert_not_called()
        self.assertIn("usage:", output.getvalue())

    def test_status_reports_missing_guest_state_without_traceback(self):
        with tempfile.TemporaryDirectory(prefix='dux-vm-missing-state-') as temporary:
            with (
                patch.object(VM, 'BASE', Path(temporary)),
                patch.object(VM.sys, 'argv', [
                    'dux-qualification-vm.py', 'status', 'ubuntu24-lifecycle-ephemeral',
                ]),
                patch.object(VM.subprocess, 'run', return_value=SimpleNamespace(
                    returncode=1, stdout='LoadState=not-found\nActiveState=inactive\nMainPID=0\n',
                )) as run,
                patch('sys.stdout', new_callable=io.StringIO) as output,
            ):
                VM.main()

            report = json.loads(output.getvalue())
            self.assertEqual(report['guest_state'], 'missing')
            self.assertEqual(report['systemd']['LoadState'], 'not-found')
            self.assertEqual(report['systemd']['ActiveState'], 'inactive')
            self.assertEqual(list(Path(temporary).iterdir()), [])
            run.assert_called_once_with(
                ['systemctl', 'show', 'dux-qualification-vm-ubuntu24-lifecycle-ephemeral',
                 '-p', 'ActiveState', '-p', 'MainPID', '--no-pager'],
                capture_output=True, text=True, check=False,
            )

    def test_ssh_with_missing_guest_state_fails_with_actionable_error(self):
        with tempfile.TemporaryDirectory(prefix='dux-vm-missing-state-') as temporary:
            with (
                patch.object(VM, 'BASE', Path(temporary)),
                patch.object(VM.sys, 'argv', [
                    'dux-qualification-vm.py', 'ssh', 'ubuntu24-lifecycle-ephemeral',
                ]),
                patch.object(VM.subprocess, 'run') as run,
            ):
                with self.assertRaisesRegex(SystemExit, 'start the guest first'):
                    VM.main()
            run.assert_not_called()
            self.assertEqual(list(Path(temporary).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
