#!/usr/bin/env python3
"""Manage resource-limited, disposable KVM guests for dux qualification.

Images must first be downloaded from the distributions and verified against
published checksums; target/readiness/vms/images.json records that evidence.
SSH is forwarded only on loopback. Never changes the installed dux service.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shlex
import shutil
import socket
import stat
import subprocess
import sys
import time

if not __debug__:
    raise RuntimeError(
        "qualification VM management requires assertions enabled; do not use python -O"
    )

from dux_storage import GIB, require_capacity

BASE = Path(__file__).resolve().parents[1] / 'target/readiness/vms'
GUEST_IMAGES = {
    'debian12': 'debian12',
    'debian12-rollback': 'debian12',
    'rocky9': 'rocky9',
    'rocky9-lifecycle-current': 'rocky9',
    'rocky9-lifecycle-ephemeral': 'rocky9',
    'ubuntu24': 'ubuntu24',
    'ubuntu24-lifecycle': 'ubuntu24',
    'ubuntu24-lifecycle-final': 'ubuntu24',
    'ubuntu24-lifecycle-current': 'ubuntu24',
    'ubuntu24-lifecycle-ephemeral': 'ubuntu24',
    'ubuntu24-rollback': 'ubuntu24',
    'ubuntu24-scale-reuse': 'ubuntu24',
    'watch-unit': 'ubuntu24',
}
VM_PROFILES = {
    'standard': {'guest_ram_mib': 2048, 'vcpus': 2, 'cpu_quota_percent': 75,
                 'cgroup_memory_mib': 2560, 'minimum_host_free_mib': 6144},
    'qualification4': {'guest_ram_mib': 4096, 'vcpus': 2, 'cpu_quota_percent': 50,
                       # The guest RAM is 4 GiB. QEMU's cgroup also accounts
                       # for dirty qcow2 pages when its ephemeral overlay lives
                       # on a private tmpfs; 6 GiB killed the 10m scan before its 30m
                       # budget elapsed. Keep enough bounded headroom for that
                       # overlay while preserving strict host admission checks.
                       'cgroup_memory_mib': 8192, 'minimum_host_free_mib': 12288},
}
WATCH_UNIT_RUNTIME_DIRECTORY = Path('/dev/shm/dux-watch-unit-vm')
EPHEMERAL_RUNTIME_DIRECTORY = Path('/dev/shm/dux-qualification-vm')
PRIVATE_OVERLAY_MOUNTPOINT = Path('/run/dux-qualification-overlay')
EPHEMERAL_CONTROL_BYTES = 256 * 1024 * 1024


def filesystem_type_for(path):
    """Find the filesystem type of the deepest mount containing ``path``."""
    path = Path(path).resolve()
    selected = None
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, separator, right = line.partition(' - ')
        fields = left.split()
        if not separator or len(fields) < 5:
            continue
        mountpoint = fields[4]
        for escaped, character in ((r'\040', ' '), (r'\011', '\t'),
                                   (r'\012', '\n'), (r'\134', '\\')):
            mountpoint = mountpoint.replace(escaped, character)
        mount = Path(mountpoint)
        if (path == mount or path.is_relative_to(mount)) and (
            selected is None or len(mount.parts) > len(selected[0].parts)
        ):
            selected = mount, right.split()[0]
    return selected[1] if selected else None


def runtime_directory_for(guest, workspace_directory, run_id=None, ephemeral=False):
    """Keep guest control files in a run-private tmpfs directory."""
    if guest == 'watch-unit':
        return WATCH_UNIT_RUNTIME_DIRECTORY / run_id if run_id else WATCH_UNIT_RUNTIME_DIRECTORY
    if ephemeral:
        if not run_id:
            raise ValueError('ephemeral VM runtime requires a unique run id')
        return EPHEMERAL_RUNTIME_DIRECTORY / guest / run_id
    return workspace_directory


def statefile_for(workspace_directory, ephemeral=False):
    """Keep /dev/shm guest state separate from persistent guest metadata."""
    return workspace_directory / ('ephemeral-state.json' if ephemeral else 'state.json')


def private_overlay_runner_script(image, overlay, qemu_args):
    """Build an ephemeral-VM launcher whose writable disk is a private tmpfs."""
    create_overlay = shlex.join([
        'qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', str(image),
        str(overlay), '20G',
    ])
    launch_qemu = shlex.join([str(argument) for argument in qemu_args])
    return f'#!/bin/sh\nset -eu\n{create_overlay}\nexec {launch_qemu}\n'


def private_overlay_unit_properties(memory_mib):
    """Return systemd mount-namespace properties for the ephemeral qcow2."""
    return [
        '--property=PrivateMounts=yes',
        f'--property=TemporaryFileSystem={PRIVATE_OVERLAY_MOUNTPOINT}:rw,size={memory_mib}M',
    ]


def cleanup_ephemeral_runtime(guest, runtime_directory):
    """Remove only a verified, stopped guest overlay created under our tmpfs root."""
    runtime = Path(runtime_directory)
    expected_parent = (
        WATCH_UNIT_RUNTIME_DIRECTORY if guest == 'watch-unit'
        else EPHEMERAL_RUNTIME_DIRECTORY / guest
    )
    if runtime.parent != expected_parent or not runtime.name.isdecimal():
        raise ValueError(f'refusing unexpected ephemeral guest path: {runtime}')
    try:
        resolved = runtime.resolve(strict=True)
    except FileNotFoundError:
        return False
    if resolved != runtime:
        raise ValueError(f'refusing non-canonical ephemeral guest path: {runtime}')
    info = runtime.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError(f'refusing unexpected ephemeral guest directory metadata: {runtime}')
    if filesystem_type_for(runtime) != 'tmpfs':
        raise ValueError(f'refusing to remove ephemeral guest data outside tmpfs: {runtime}')
    try:
        shutil.rmtree(runtime)
    except FileNotFoundError:
        # The pressure guard and the lifecycle wrapper can both clean the same
        # stopped VM after systemd has terminated it. A competing cleanup may
        # remove the run directory after the validation above but before
        # rmtree's first lstat. Treat an already-absent run directory as
        # successfully cleaned; still surface errors when anything remains.
        if not runtime.exists():
            return False
        raise
    return True


def cleanup_stale_ephemeral_runtimes(guest):
    """Remove orphaned per-run overlays after the guest unit is confirmed stopped."""
    expected_parent = (
        WATCH_UNIT_RUNTIME_DIRECTORY if guest == 'watch-unit'
        else EPHEMERAL_RUNTIME_DIRECTORY / guest
    )
    try:
        candidates = sorted(expected_parent.iterdir())
    except FileNotFoundError:
        return []
    removed = []
    for runtime in candidates:
        if not runtime.name.isdecimal():
            continue
        info = runtime.lstat()
        mode = stat.S_IMODE(info.st_mode)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid():
            raise ValueError(f'refusing unexpected stale ephemeral runtime: {runtime}')
        if mode != 0o700:
            if mode & 0o022:
                raise ValueError(
                    f'refusing group/world-writable stale ephemeral runtime: {runtime}'
                )
            # Older runs could leave root-owned, non-writable 0755 directories.
            # Close access before deleting their guest disk and SSH artifacts.
            os.chmod(runtime, 0o700, follow_symlinks=False)
        if cleanup_ephemeral_runtime(guest, runtime):
            removed.append(runtime)
    return removed


def cleanup_stopped_guest_runtime(guest, statefile, vm_unit):
    """Remove a guarded/stopped guest's private overlay after systemd confirms it stopped."""
    statefile = Path(statefile)
    try:
        state = json.loads(statefile.read_text())
    except FileNotFoundError:
        return False
    if state.get('unit') != vm_unit or not state.get('ephemeral_runtime'):
        return False
    if state.get('ephemeral_runtime_cleaned'):
        return False
    active = subprocess.run(
        ['systemctl', 'is-active', '--quiet', vm_unit],
        capture_output=True, text=True, check=False,
    )
    if active.returncode == 0:
        raise RuntimeError(f'refusing to clean runtime while guest unit is active: {vm_unit}')
    cleanup_ephemeral_runtime(guest, state['runtime_directory'])
    state['ephemeral_runtime_cleaned'] = True
    statefile.write_text(json.dumps(state, indent=2) + '\n')
    return True


def parse_psi_avg10(contents, line_name='some'):
    """Read one PSI avg10 value, returning None when the kernel omits PSI."""
    for line in contents.splitlines():
        fields = line.split()
        if not fields or fields[0] != line_name:
            continue
        for field in fields[1:]:
            if field.startswith('avg10='):
                try:
                    value = float(field.split('=', 1)[1])
                except ValueError:
                    return None
                return value if math.isfinite(value) and value >= 0.0 else None
    return None


def active_host_workloads(proc_root=Path('/proc')):
    """Return short labels for host builds, installs, tests, and scans.

    Pressure is a lagging signal: a whole-module compiler or security scan can
    start between the initial PSI sample and guest boot, then trip the
    independent guard after it has already consumed host CPU or I/O. Detect
    common build/test/package-install drivers, test binaries, compiler workers,
    and repository analyzers directly; omit full command lines so paths and
    arguments do not get copied into guard evidence.
    """
    workloads = []
    with os.scandir(proc_root) as processes:
        for process in processes:
            if not process.name.isdigit():
                continue
            try:
                raw = Path(process.path, 'cmdline').read_bytes()
            except (FileNotFoundError, ProcessLookupError):
                # Processes can exit between scandir and cmdline read. Other
                # read failures are not safe to treat as an empty workload set.
                continue
            argv = [part.decode('utf-8', errors='replace')
                    for part in raw.split(b'\0') if part]
            if not argv:
                continue
            process_title = Path(argv[0]).name.split()
            executable = process_title[0]
            args = process_title[1:] + argv[1:]
            if executable in {'node', 'nodejs'} and args:
                node_script = Path(args[0]).stem.lower()
                if node_script in {'tsc', 'tsc.js'}:
                    workloads.append(f'node tsc pid={process.name}')
                    continue
                node_test_runners = {
                    'ava', 'cypress', 'jasmine', 'jest', 'mocha',
                    'playwright', 'tap', 'vitest',
                }
                if node_script in node_test_runners:
                    workloads.append(f'node {node_script} pid={process.name}')
                    continue
                if '--test' in args:
                    workloads.append(f'node --test pid={process.name}')
                    continue
                node_tools = {
                    'npm-cli.js': 'npm',
                    'pnpm.cjs': 'pnpm', 'pnpm.js': 'pnpm',
                    'yarn.cjs': 'yarn', 'yarn.js': 'yarn',
                }
                node_tool = node_tools.get(Path(args[0]).name)
                if node_tool is not None:
                    executable = node_tool
                    args = args[1:]
            if executable in {'go', 'cargo', 'npm', 'pnpm', 'yarn', 'bun'}:
                value_options = {
                    'go': {'-C'},
                    'cargo': {'--manifest-path', '--color', '--config'},
                    'npm': set(), 'pnpm': set(), 'yarn': set(), 'bun': set(),
                }.get(executable, set())
                subcommands = {
                    'go': {'build', 'test', 'vet', 'list'},
                    'cargo': {'build', 'test', 'check', 'clippy', 'audit', 'deny'},
                    'npm': {'ci', 'install', 'rebuild', 'update'},
                    'pnpm': {'install', 'add', 'rebuild', 'update'},
                    'yarn': {'install', 'add', 'rebuild', 'upgrade'},
                    'bun': {'install', 'add', 'update'},
                }[executable]
                index = 0
                while index < len(args) and args[index].startswith('-'):
                    option = args[index].split('=', 1)[0]
                    index += 2 if option in value_options and '=' not in args[index] else 1
                if index < len(args) and args[index] in subcommands:
                    workloads.append(
                        f'{executable} {args[index]} pid={process.name}'
                    )
                elif (
                    executable in {'npm', 'pnpm', 'yarn', 'bun'}
                    and index + 1 < len(args)
                    and args[index] == 'run'
                    and args[index + 1] in {'build', 'test'}
                ):
                    workloads.append(
                        f'{executable} run {args[index + 1]} pid={process.name}'
                    )
            elif executable.endswith('.test'):
                workloads.append(f'Go test binary pid={process.name}')
            elif (
                executable in {'bash', 'dash', 'sh', 'zsh'}
                and args
                and not args[0].startswith('-')
                and (
                    (Path(args[0]).name.startswith('test-')
                     and Path(args[0]).name.endswith('.sh'))
                    or Path(args[0]).name in {'run-tests.sh', 'test.sh'}
                )
            ):
                workloads.append(f'shell test script pid={process.name}')
            elif '/pkg/tool/' in argv[0] and executable in {
                'compile', 'vet',
            }:
                label = 'Go vet worker' if executable == 'vet' else 'Go compiler'
                workloads.append(f'{label} pid={process.name}')
            elif executable in {'rustc', 'rustdoc', 'cc', 'gcc', 'g++',
                                'clang', 'clang++', 'cc1', 'cc1plus',
                                'ld', 'ld.lld', 'collect2', 'make', 'ninja',
                                'gosec', 'golangci-lint', 'staticcheck',
                                'govulncheck', 'cargo-audit', 'cargo-deny',
                                'semgrep', 'osv-scanner'}:
                workloads.append(f'{executable} pid={process.name}')
    return sorted(workloads)


def host_pressure():
    """Return host PSI avg10 values using the daemon's Elevated thresholds."""
    result = {}
    telemetry_errors = []
    try:
        load1 = float(Path('/proc/loadavg').read_text().split()[0])
        if not math.isfinite(load1) or load1 < 0.0:
            raise ValueError('load average is not a finite non-negative value')
        result['load1'] = load1
    except (OSError, ValueError, IndexError):
        result['load1'] = None
        telemetry_errors.append('/proc/loadavg load1 is unavailable or invalid')
    for kind in ('cpu', 'io', 'memory'):
        try:
            contents = Path(f'/proc/pressure/{kind}').read_text()
        except OSError:
            result[kind] = {'some': None, 'full': None}
            telemetry_errors.append(f'/proc/pressure/{kind} is unavailable')
            continue
        result[kind] = {
            'some': parse_psi_avg10(contents, 'some'),
            'full': parse_psi_avg10(contents, 'full'),
        }
        if result[kind]['some'] is None:
            telemetry_errors.append(
                f'/proc/pressure/{kind} some avg10 is missing or invalid'
            )
        if kind == 'cpu' and result[kind]['full'] is None:
            telemetry_errors.append(
                '/proc/pressure/cpu full avg10 is missing or invalid'
            )
    try:
        result['active_host_workloads'] = active_host_workloads()
    except OSError as error:
        result['active_host_workloads'] = []
        telemetry_errors.append(f'/proc workload scan is unavailable: {error}')
    result['telemetry_errors'] = telemetry_errors
    return result


def host_pressure_blockers(pressure):
    """Gate on aggregate load and PSI impact, not quota-limited CPU wait alone.

    PSI ``some`` can stay elevated when a capped service is throttled even with
    spare host capacity. Permit that case only when aggregate load and CPU
    ``full`` pressure are low; retain hard caps and strict memory/I/O limits.
    """
    blockers = [
        f'host pressure telemetry unavailable: {error}'
        for error in pressure.get('telemetry_errors', [])
    ]
    blockers.extend(
        f'active host build/test/install/analysis workload: {workload}'
        for workload in pressure.get('active_host_workloads', [])
    )
    load1 = pressure.get('load1')
    if load1 is not None and load1 > 10.0:
        blockers.append(f'host load1={load1:.2f} exceeds 10')

    cpu = pressure.get('cpu', {}).get('some')
    cpu_full = pressure.get('cpu', {}).get('full')
    if cpu is not None and cpu > 55.0:
        blockers.append(f'cpu PSI some avg10={cpu:.2f}% exceeds hard admission limit 55%')
    elif cpu is not None and cpu > 35.0 and (
        load1 is None or load1 > 10.0 or cpu_full is None or cpu_full > 5.0
    ):
        blockers.append(
            f'cpu PSI some avg10={cpu:.2f}% requires load1<=10 and cpu PSI full avg10<=5%'
        )
    if cpu_full is not None and cpu_full > 5.0:
        blockers.append(f'cpu PSI full avg10={cpu_full:.2f}% exceeds 5%')

    for kind, limit in {'io': 10.0, 'memory': 5.0}.items():
        some = pressure.get(kind, {}).get('some')
        if some is not None and some > limit:
            blockers.append(f'{kind} PSI some avg10={some:.2f}% exceeds {limit:.0f}%')
    return blockers


def require_stable_host_admission(unit, samples=3, interval_seconds=10):
    """Refuse VM startup unless repeated host samples remain clear.

    A single clear snapshot can race with a new build or test starting while
    QEMU is prepared. Fail immediately on the first blocked sample; callers can
    retry after the host has settled. The independent runtime guard remains
    responsible for stopping a guest if pressure rises after startup.
    """
    for sample in range(1, samples + 1):
        blockers = host_pressure_blockers(host_pressure())
        if blockers:
            raise SystemExit(
                f'refusing to start {unit}: host admission sample '
                f'{sample}/{samples} is blocked: ' + '; '.join(blockers)
            )
        if sample < samples:
            time.sleep(interval_seconds)


def host_pressure_critical_blockers(pressure):
    """Stop guests for aggregate CPU saturation, not quota wait by itself."""
    blockers = [
        f'host pressure telemetry unavailable: {error}'
        for error in pressure.get('telemetry_errors', [])
    ]
    blockers.extend(
        f'active host build/test/install/analysis workload: {workload}'
        for workload in pressure.get('active_host_workloads', [])
    )
    load1 = pressure.get('load1')
    cpu = pressure.get('cpu', {}).get('some')
    cpu_full = pressure.get('cpu', {}).get('full')
    if cpu is not None and cpu > 70.0:
        blockers.append(f'cpu PSI some avg10={cpu:.2f}% exceeds hard critical limit 70%')
    elif cpu is not None and cpu > 40.0 and (
        load1 is None or load1 > 10.0 or cpu_full is None or cpu_full > 5.0
    ):
        blockers.append(
            f'cpu PSI some avg10={cpu:.2f}% with high/unavailable host load or CPU full pressure'
        )
    if cpu_full is not None and cpu_full > 10.0:
        blockers.append(f'cpu PSI full avg10={cpu_full:.2f}% exceeds critical 10%')
    for kind, limit in {'io': 40.0, 'memory': 20.0}.items():
        some = pressure.get(kind, {}).get('some')
        if some is not None and some > limit:
            blockers.append(f'{kind} PSI some avg10={some:.2f}% exceeds critical {limit:.0f}%')
    if load1 is not None and load1 > 12.0:
        blockers.append(f'host load1={load1:.2f} exceeds critical 12')
    return blockers


def run_pressure_guard(guest, directory, vm_unit):
    """Stop only this disposable VM after sustained critical host pressure."""
    consecutive = 0
    while subprocess.run(['systemctl', 'is-active', '--quiet', vm_unit]).returncode == 0:
        pressure = host_pressure()
        blockers = host_pressure_critical_blockers(pressure)
        consecutive = consecutive + 1 if blockers else 0
        if consecutive >= 2:
            event = {
                'time': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                'guest': guest,
                'vm_unit': vm_unit,
                'pressure': pressure,
                'action': 'stopped_disposable_vm',
                'reason': blockers,
            }
            with (directory / 'pressure-guard-events.jsonl').open('a') as stream:
                stream.write(json.dumps(event, sort_keys=True) + '\n')
                stream.flush()
            # Persist the event to journald before stopping the VM. The stopped
            # guest cleanup can race the lifecycle wrapper and remove the
            # runtime directory that also contains the JSONL event file.
            print(json.dumps(event, sort_keys=True), flush=True)
            run('systemctl', 'stop', vm_unit)
            statefile = statefile_for(directory, ephemeral=guest != 'watch-unit')
            cleanup_stopped_guest_runtime(guest, statefile, vm_unit)
            return
        time.sleep(5)


def stop_vm_unit(unit):
    """Stop a guest, accepting a unit already collected by systemd --collect."""
    result = subprocess.run(
        ['systemctl', 'stop', unit], capture_output=True, text=True, check=False
    )
    if result.returncode == 0:
        return
    state = subprocess.run(
        ['systemctl', 'show', unit, '-p', 'LoadState', '-p', 'ActiveState'],
        capture_output=True, text=True, check=False,
    )
    properties = dict(
        line.split('=', 1) for line in state.stdout.splitlines() if '=' in line
    )
    if (state.returncode == 0 and properties.get('LoadState') == 'not-found'
            and properties.get('ActiveState') == 'inactive'):
        return
    raise subprocess.CalledProcessError(
        result.returncode, result.args, output=result.stdout, stderr=result.stderr
    )


def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, text=True, **kwargs)


def report_missing_guest_state(guest, unit, statefile, state):
    """Report an absent/bad guest record without traceback or false liveness claims."""
    result = subprocess.run(
        ['systemctl', 'show', unit, '-p', 'ActiveState', '-p', 'MainPID', '--no-pager'],
        capture_output=True, text=True, check=False,
    )
    properties = dict(
        line.split('=', 1) for line in result.stdout.splitlines() if '=' in line
    )
    print(json.dumps({
        'guest': guest,
        'unit': unit,
        'guest_state': state,
        'state_file': str(statefile),
        'systemd_query_exit_code': result.returncode,
        'systemd': properties,
    }, indent=2, sort_keys=True))


def available_memory_mib():
    for line in Path('/proc/meminfo').read_text().splitlines():
        if line.startswith('MemAvailable:'):
            return int(line.split()[1]) // 1024
    raise RuntimeError('cannot determine host MemAvailable; refusing to start a qualification VM')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=sorted(VM_PROFILES), default='standard',
                        help='qualification guest RAM is limited to 2 GiB standard or 4 GiB full profile')
    parser.add_argument('--cpu-quota-percent', type=int, choices=range(10, 101),
                        help='override the QEMU unit CPU quota for this run (10–100 percent)')
    parser.add_argument('--read-only-share', type=Path,
                        help='export one host directory read-only over virtio-9p to an isolated guest')
    parser.add_argument('--read-only-disk', type=Path,
                        help='attach one existing raw disk image read-only as an additional virtio disk')
    parser.add_argument('--ephemeral-runtime', action='store_true',
                        help='use a unique /dev/shm control directory and a private QEMU tmpfs disk overlay')
    parser.add_argument('action', choices=['start', 'stop', 'status', 'ssh', 'copy', 'fetch', 'guard'])
    parser.add_argument('guest', choices=sorted(GUEST_IMAGES))
    parser.add_argument('args', nargs=argparse.REMAINDER)
    options = parser.parse_args()
    # ``argparse.REMAINDER`` intentionally forwards the arguments after the
    # guest name to ssh/scp, but it also captures a trailing ``--help``.  Handle
    # that explicitly before any directory, disk image, or service is created.
    if options.args and options.args[0] in {'-h', '--help'}:
        parser.print_help()
        return
    directory = BASE / options.guest
    if options.action == 'start':
        directory.mkdir(parents=True, exist_ok=True)
    unit = f'dux-qualification-vm-{options.guest}'
    if options.action == 'start':
        statefile = statefile_for(
            directory,
            ephemeral=options.ephemeral_runtime and options.guest != 'watch-unit',
        )
        active_check = subprocess.run(
            ['systemctl', 'is-active', '--quiet', unit], check=False
        )
        if active_check.returncode == 0:
            raise SystemExit(f'{unit} already running; use status/ssh')
        if active_check.returncode not in (3, 4):
            raise SystemExit(
                f'cannot verify {unit} is stopped (systemctl exit '
                f'{active_check.returncode}); refusing to start a VM'
            )
        available_mib = available_memory_mib()
        profile = dict(VM_PROFILES[options.profile])
        if options.cpu_quota_percent is not None:
            profile['cpu_quota_percent'] = options.cpu_quota_percent
        require_stable_host_admission(unit)
        ephemeral_runtime = options.guest == 'watch-unit' or options.ephemeral_runtime
        if ephemeral_runtime:
            stale_runtimes = cleanup_stale_ephemeral_runtimes(options.guest)
            if stale_runtimes:
                print(
                    f'Removed {len(stale_runtimes)} stale tmpfs runtime '
                    f'directories for {options.guest}',
                    flush=True,
                )
        if options.read_only_share is not None:
            share = options.read_only_share.resolve(strict=True)
            assert share.is_dir(), '--read-only-share must name a directory'
        else:
            share = None
        if options.read_only_disk is not None:
            read_only_disk = options.read_only_disk.resolve(strict=True)
            assert read_only_disk.is_file(), '--read-only-disk must name a regular file'
        else:
            read_only_disk = None
        run_id = str(time.time_ns()) if options.guest == 'watch-unit' or options.ephemeral_runtime else None
        runtime_directory = runtime_directory_for(
            options.guest, directory, run_id, ephemeral=options.ephemeral_runtime)
        runtime_directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        if ephemeral_runtime:
            if not runtime_directory.is_relative_to(Path('/dev/shm')) or runtime_directory != runtime_directory.resolve():
                raise SystemExit(f'refusing non-canonical tmpfs runtime path: {runtime_directory}')
            filesystem_type = filesystem_type_for(runtime_directory)
            if filesystem_type != 'tmpfs':
                raise SystemExit(f'refusing ephemeral runtime on {filesystem_type!r}, expected tmpfs')
            runtime_directory.chmod(0o700)
        # Admission is based on usable bytes/inodes, not qcow2 apparent size.
        # Keep capacity for evidence and recovery while a sparse image grows.
        storage_admission = require_capacity(
            runtime_directory,
            required_bytes=(EPHEMERAL_CONTROL_BYTES if ephemeral_runtime else 2 * GIB),
            reserve_bytes=(GIB if ephemeral_runtime else 8 * GIB),
        )
        minimum_available_mib = profile['minimum_host_free_mib']
        if available_mib < minimum_available_mib:
            raise SystemExit(
                f'refusing to start {unit}: host MemAvailable is {available_mib} MiB; '
                f'at least {minimum_available_mib} MiB is required for the '
                f'{profile["guest_ram_mib"]} MiB {options.profile} guest '
                'plus host and test-worker headroom'
            )
        image_name = GUEST_IMAGES[options.guest]
        recorded = json.loads((BASE / 'images.json').read_text())[image_name]
        image = BASE / Path(recorded['file']).name
        digest = hashlib.new(recorded['digest_algorithm'])
        with image.open('rb') as stream:
            for block in iter(lambda: stream.read(1024*1024), b''):
                digest.update(block)
        assert digest.hexdigest() == recorded['digest'], 'image checksum changed'
        key = runtime_directory / 'identity'
        if not key.exists():
            run('ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', key)
        public = key.with_suffix('.pub').read_text().strip()
        userdata = runtime_directory / 'user-data'
        userdata.write_text('#cloud-config\ndisable_root: false\nssh_pwauth: false\nwrite_files:\n  - path: /etc/dux-qualification-sandbox\n    owner: root:root\n    permissions: "0400"\n    content: dux-qualification-sandbox-v1\nusers:\n  - name: root\n    lock_passwd: true\n    ssh_authorized_keys:\n      - ' + public + '\nruncmd:\n  - [ sh, -c, "for unit in dnf-makecache.timer apt-daily.timer apt-daily-upgrade.timer apt-daily.service apt-daily-upgrade.service apt-news.timer unattended-upgrades.service fwupd-refresh.timer fwupd-refresh.service update-notifier-download.timer motd-news.timer man-db.timer sysstat-collect.timer sysstat-summary.timer fstrim.timer e2scrub_all.timer; do systemctl disable --now \\"$unit\\" >/dev/null 2>&1 || true; done" ]\n')
        metadata = runtime_directory / 'meta-data'
        metadata.write_text(f'instance-id: dux-qualification-{options.guest}\nlocal-hostname: dux-{options.guest}\n')
        overlay = (
            PRIVATE_OVERLAY_MOUNTPOINT / 'disk.qcow2'
            if ephemeral_runtime else runtime_directory / 'disk.qcow2'
        )
        if not ephemeral_runtime and not overlay.exists():
            run('qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', image, overlay, '20G')
        seed = runtime_directory / 'seed.img'
        if not seed.exists():
            run('cloud-localds', seed, userdata, metadata)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        state = {'unit': unit, 'guest': options.guest, 'port': port, 'identity': str(key), 'known_hosts': str(runtime_directory / 'known_hosts'),
                 'image': recorded, 'profile': options.profile, **profile,
                 'read_only_share': str(share) if share else None,
                 'read_only_disk': str(read_only_disk) if read_only_disk else None,
                 'ephemeral_runtime': ephemeral_runtime,
                 'overlay_isolated_tmpfs': ephemeral_runtime,
                 'runtime_directory': str(runtime_directory),
                 'host_mem_available_mib_at_start': available_mib,
                 'storage_admission': storage_admission,
                 'started_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
        qemu_args = [
            'qemu-system-x86_64', '-enable-kvm', '-cpu', 'host', '-smp', str(profile['vcpus']),
            '-m', str(profile['guest_ram_mib']),
            '-display', 'none', '-monitor', 'none', '-no-reboot',
            '-serial', f'file:{runtime_directory / "console.log"}',
            '-drive', f'file={overlay},format=qcow2,if=virtio,cache=none,discard=unmap',
            '-drive', f'file={seed},format=raw,if=virtio,readonly=on',
            '-netdev', f'user,id=network,hostfwd=tcp:127.0.0.1:{port}-:22',
            '-device', 'virtio-net-pci,netdev=network']
        if read_only_disk:
            qemu_args += ['-drive', f'file={read_only_disk},format=raw,if=virtio,readonly=on']
        if share:
            qemu_args += ['-virtfs', f'local,path={share},mount_tag=duxhost,security_model=none,readonly=on']
        if ephemeral_runtime:
            runner = runtime_directory / 'run-qemu.sh'
            runner.write_text(private_overlay_runner_script(image, overlay, qemu_args))
            runner.chmod(0o700)
            qemu_command = [str(runner)]
        else:
            qemu_command = qemu_args
        systemd_args = ['systemd-run', f'--unit={unit}', '--collect',
            f'--property=CPUQuota={profile["cpu_quota_percent"]}%',
            f'--property=MemoryMax={profile["cgroup_memory_mib"]}M', '--property=OOMScoreAdjust=1000',
            '--property=IOWeight=10', '--property=Nice=10',
            '--property=RuntimeMaxSec=356400', '--property=TimeoutStopSec=30s']
        if ephemeral_runtime:
            # The overlay receives the guest's high-rate block writes. Put it
            # on a fresh tmpfs visible only in QEMU's mount namespace; host Dux
            # cannot fanotify-monitor that private superblock. The seed, keys,
            # and small serial log remain in the host run directory.
            systemd_args += private_overlay_unit_properties(profile['cgroup_memory_mib'])
        run(*systemd_args, *qemu_command)
        statefile.write_text(json.dumps(state, indent=2)+'\n')
        guard_unit = f'dux-qualification-pressure-guard-{options.guest}-{time.time_ns()}'
        try:
            run('systemd-run', f'--unit={guard_unit}', '--collect', '--property=CPUQuota=2%',
                '--property=MemoryMax=64M', '--property=IOWeight=1', '--property=Nice=19',
                sys.executable, str(Path(__file__).resolve()), 'guard', options.guest)
        except Exception:
            subprocess.run(['systemctl', 'stop', unit], check=False)
            cleanup_stopped_guest_runtime(options.guest, statefile, unit)
            raise
        print('Started', unit, 'SSH loopback port', port, flush=True)
        return
    persistent_statefile = statefile_for(directory)
    ephemeral_statefile = statefile_for(directory, ephemeral=True)
    if options.guest == 'watch-unit':
        statefile = persistent_statefile
    elif options.ephemeral_runtime:
        statefile = ephemeral_statefile
    elif ephemeral_statefile.exists():
        try:
            ephemeral_state = json.loads(ephemeral_statefile.read_text())
        except (OSError, json.JSONDecodeError):
            ephemeral_state = {}
        statefile = (
            ephemeral_statefile
            if ephemeral_state.get('ephemeral_runtime')
            and not ephemeral_state.get('ephemeral_runtime_cleaned')
            else persistent_statefile
        )
    else:
        statefile = persistent_statefile
    if options.action == 'stop':
        stop_vm_unit(unit)
        if not statefile.exists():
            return
        state = json.loads(statefile.read_text())
        if state.get('ephemeral_runtime') and not state.get('ephemeral_runtime_cleaned'):
            cleanup_ephemeral_runtime(options.guest, state['runtime_directory'])
            state['ephemeral_runtime_cleaned'] = True
            statefile.write_text(json.dumps(state, indent=2) + '\n')
        return
    try:
        state = json.loads(statefile.read_text())
    except FileNotFoundError:
        if options.action == 'status':
            report_missing_guest_state(options.guest, unit, statefile, 'missing')
            return
        raise SystemExit(f'no saved state for {options.guest} at {statefile}; start the guest first')
    except (OSError, json.JSONDecodeError) as error:
        if options.action == 'status':
            report_missing_guest_state(options.guest, unit, statefile, 'invalid')
            return
        raise SystemExit(f'cannot read saved state for {options.guest} at {statefile}: {error}')
    if options.action == 'guard':
        run_pressure_guard(options.guest, directory, state['unit'])
        return
    if options.action == 'status':
        run('systemctl', 'show', unit, '-p', 'ActiveState', '-p', 'MainPID', '-p', 'MemoryCurrent', '-p', 'CPUUsageNSec')
        print(json.dumps(state, indent=2))
        return
    common = ['-i', state['identity'], '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
              '-o', 'StrictHostKeyChecking=accept-new', '-o', f'UserKnownHostsFile={state["known_hosts"]}']
    arguments = options.args[1:] if options.args[:1] == ['--'] else options.args
    if options.action == 'ssh':
        run('ssh', *common, '-p', str(state['port']), 'root@127.0.0.1', *arguments)
    elif options.action == 'copy':
        assert len(arguments) == 2, 'copy requires LOCAL_FILE REMOTE_PATH'
        run('scp', *common, '-P', str(state['port']), arguments[0], f'root@127.0.0.1:{arguments[1]}')
    else:
        assert len(arguments) == 2, 'fetch requires REMOTE_PATH LOCAL_FILE'
        run('scp', *common, '-P', str(state['port']), f'root@127.0.0.1:{arguments[0]}', arguments[1])


if __name__ == '__main__':
    main()
