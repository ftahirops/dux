#!/usr/bin/env python3
"""Exercise upgrade, rollback, removal, purge and clean install in a disposable VM."""
import argparse
import grp
import hashlib
import json
import os
import platform
import shlex
import shutil
import sqlite3
import stat
import subprocess
import tempfile
import time
from pathlib import Path

if not __debug__:
    raise RuntimeError(
        "package lifecycle qualification requires assertions enabled; do not use python -O"
    )

SANDBOX_MARKER = Path("/etc/dux-qualification-sandbox")
HASH_CHUNK_BYTES = 1024 * 1024


def _sha256_file(path):
    """Hash arbitrarily large qualification artifacts with bounded memory."""
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command(*args, check=True):
    return subprocess.run([str(arg) for arg in args], check=check,
                          capture_output=True, text=True)


def _exec_start_has_option(exec_start, option):
    """Match a real ExecStart argv option, not an option-name substring."""
    _, separator, arguments = exec_start.partition("argv[]=")
    if not separator:
        return False
    # `systemctl show` appends metadata after the argv list. Parse only the
    # command vector and use shell-style quoting to keep each argument intact.
    arguments = arguments.split(" ; ignore_errors=", 1)[0]
    try:
        argv = shlex.split(arguments)
    except ValueError:
        return False
    return any(argument == option or argument.startswith(f"{option}=") for argument in argv)


def systemd_syscall_filter_group_members(groups):
    """Resolve systemd syscall groups so effective expanded filters are verifiable."""
    result = command("systemd-analyze", "syscall-filter", *sorted(groups), check=False)
    assert result.returncode == 0, {
        "property": "systemd syscall-filter group expansion",
        "groups": sorted(groups), "returncode": result.returncode,
        "stderr": result.stderr,
    }
    members = {group: set() for group in groups}
    active_group = None
    for line in result.stdout.splitlines():
        value = line.strip()
        if value.startswith("@"):
            active_group = value.split()[0]
        elif active_group in members and value and not value.startswith("#"):
            members[active_group].add(value.split()[0])
    assert all(members.values()), {
        "property": "systemd syscall-filter group expansion",
        "expected_groups": sorted(groups), "observed": result.stdout,
    }
    return members


def require_disposable_qualification_vm():
    """Refuse package removal/purge unless the VM controller provisioned this guest."""
    assert os.geteuid() == 0, "package lifecycle qualification requires root inside its disposable VM"
    virtualization = command("systemd-detect-virt", "--vm", check=False)
    assert virtualization.returncode == 0 and virtualization.stdout.strip() in {"kvm", "qemu"}, {
        "reason": "package lifecycle is allowed only inside a KVM/QEMU qualification guest",
        "systemd_detect_virt": virtualization.stdout.strip(),
    }
    try:
        marker = SANDBOX_MARKER.lstat()
    except FileNotFoundError as error:
        raise AssertionError(
            "qualification VM marker is absent; create this guest with dux-qualification-vm.py"
        ) from error
    assert stat.S_ISREG(marker.st_mode), marker
    assert marker.st_uid == 0 and stat.S_IMODE(marker.st_mode) == 0o400, marker
    assert SANDBOX_MARKER.read_text().strip() == "dux-qualification-sandbox-v1", (
        "qualification VM marker is missing or invalid"
    )
    return {"virtualization": virtualization.stdout.strip(),
            "marker": str(SANDBOX_MARKER), "marker_mode": oct(stat.S_IMODE(marker.st_mode))}


def installed_version(kind):
    if kind == "deb":
        result = command("dpkg-query", "-W", "-f=${db:Status-Status}\\t${Version}", "dux", check=False)
        if result.returncode != 0:
            return None
        status, separator, version = result.stdout.strip().partition("\t")
        return version if separator and status == "installed" else None
    args = ("rpm", "-q", "--qf", "%{VERSION}-%{RELEASE}", "dux")
    result = command(*args, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def service_state():
    enabled = command("systemctl", "is-enabled", "dux.service", check=False).stdout.strip()
    active = command("systemctl", "is-active", "dux.service", check=False).stdout.strip()
    tunnel_enabled = command("systemctl", "is-enabled", "dux-tunnel.service", check=False).stdout.strip()
    tunnel_active = command("systemctl", "is-active", "dux-tunnel.service", check=False).stdout.strip()
    return {"enabled": enabled, "active": active,
            "tunnel_enabled": tunnel_enabled, "tunnel_active": tunnel_active}


def assert_service_security_policy(proc_root=Path("/proc")):
    """Check the effective installed unit configuration, not only its source file."""
    properties = (
        "User", "Group", "AmbientCapabilities", "CapabilityBoundingSet",
        "SecureBits",
        "NoNewPrivileges", "UMask", "StateDirectory", "StateDirectoryMode",
        "MemoryHigh", "MemoryMax", "CPUQuotaPerSecUSec", "OOMScoreAdjust",
        "IOAccounting", "IOReadBytes", "IOWriteBytes", "LimitCORE", "ProtectKernelModules",
        "SystemCallFilter", "RestrictAddressFamilies", "ExecStart", "MainPID",
    )
    output = command(
        "systemctl", "show", "dux.service",
        *(argument for name in properties for argument in ("-p", name)),
        "--no-pager",
    ).stdout
    observed = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
    expected = {
        "Group": "dux",
        "AmbientCapabilities": {"cap_dac_read_search", "cap_sys_admin"},
        "CapabilityBoundingSet": {"cap_dac_read_search", "cap_sys_admin"},
        "SecureBits": "3",
        "NoNewPrivileges": "yes",
        "UMask": "0027",
        "StateDirectory": "dux",
        "StateDirectoryMode": "0750",
        "MemoryHigh": str(2 * 1024**3),
        "MemoryMax": str(4 * 1024**3),
        "CPUQuotaPerSecUSec": "1s",
        "IOAccounting": "yes",
        "OOMScoreAdjust": "0",
        "LimitCORE": "0",
        "ProtectKernelModules": "no",
    }
    assert observed.get("User", "") in {"", "root"}, observed
    for name, wanted in expected.items():
        value = observed.get(name)
        if name in {"AmbientCapabilities", "CapabilityBoundingSet"}:
            value = set(value.split()) if value is not None else None
        assert value == wanted, {"property": name, "expected": wanted, "observed": value}
    syscall_filter = observed.get("SystemCallFilter", "").split()
    assert syscall_filter and syscall_filter[0].startswith("~"), {
        "property": "SystemCallFilter", "expected": "a deny list for kernel module syscalls",
        "observed": observed.get("SystemCallFilter"),
    }
    denied_syscalls = {syscall_filter[0][1:], *syscall_filter[1:]}
    required_denials = {
        "init_module", "finit_module", "delete_module",
        "mount", "umount", "umount2", "pivot_root", "chroot",
        "swapon", "swapoff", "reboot", "kexec_load", "kexec_file_load",
        "ioperm", "iopl", "pciconfig_iobase", "pciconfig_read", "pciconfig_write",
        "lookup_dcookie", "perf_event_open", "pidfd_getfd", "ptrace",
        "modify_ldt", "vm86", "vm86old",
    }
    group_members = systemd_syscall_filter_group_members({"@clock", "@obsolete"})
    missing_group_members = {
        group: sorted(members - denied_syscalls)
        for group, members in group_members.items()
        if group not in denied_syscalls and not members <= denied_syscalls
    }
    assert required_denials <= denied_syscalls and not missing_group_members, {
        "property": "SystemCallFilter",
        "required_denials": sorted(required_denials),
        "missing_group_denials": missing_group_members,
        "observed": observed.get("SystemCallFilter"),
    }
    address_families = set(observed.get("RestrictAddressFamilies", "").split())
    permitted_families = {"AF_UNIX", "AF_INET", "AF_INET6"}
    assert "AF_UNIX" in address_families and address_families <= permitted_families, {
        "property": "RestrictAddressFamilies",
        "expected": "AF_UNIX and optional IP families",
        "observed": observed.get("RestrictAddressFamilies"),
    }
    alert_exec = _exec_start_has_option(observed.get("ExecStart", ""), "--alert-exec")
    if not alert_exec:
        assert address_families == {"AF_UNIX"}, {
            "property": "RestrictAddressFamilies",
            "expected": "AF_UNIX when the daemon has no alert hook",
            "observed": observed.get("RestrictAddressFamilies"),
        }
    try:
        main_pid = int(observed["MainPID"])
    except (KeyError, ValueError) as error:
        raise AssertionError({"property": "MainPID", "observed": observed.get("MainPID")}) from error
    assert main_pid > 0, {"property": "MainPID", "observed": main_pid}
    process_score_path = Path(proc_root) / str(main_pid) / "oom_score_adj"
    process_score = process_score_path.read_text().strip()
    assert process_score == "0", {
        "property": "process oom_score_adj", "expected": "0",
        "observed": process_score, "path": str(process_score_path),
    }
    observed["ProcessOOMScoreAdjust"] = process_score
    return observed


def assert_service_path_visible(path, main_pid, proc_root=Path("/proc")):
    """Ensure service mount names the same inode as the host-visible path."""
    path = Path(path)
    service_path = Path(proc_root) / str(main_pid) / "root" / path.relative_to("/")
    host_stat = path.stat()
    service_stat = service_path.stat()
    host_id = (host_stat.st_dev, host_stat.st_ino)
    service_id = (service_stat.st_dev, service_stat.st_ino)
    assert service_id == host_id, {
        "path": str(path), "host_identity": host_id,
        "service_identity": service_id, "service_path": str(service_path),
    }
    return {"path": str(path), "device": host_id[0], "inode": host_id[1]}


def assert_index_access_policy(directory=Path("/var/lib/dux"), expected_uid=0, group_name="dux"):
    """Enforce the documented root:dux permissions on the filename index."""
    directory = Path(directory)
    directory_stat = directory.lstat()
    expected_gid = grp.getgrnam(group_name).gr_gid
    assert stat.S_ISDIR(directory_stat.st_mode), directory_stat
    assert (directory_stat.st_uid, directory_stat.st_gid) == (expected_uid, expected_gid), directory_stat
    assert stat.S_IMODE(directory_stat.st_mode) == 0o750, oct(stat.S_IMODE(directory_stat.st_mode))

    database = directory / "dux.db"
    database_stat = database.lstat()
    assert stat.S_ISREG(database_stat.st_mode), database_stat
    checked = {}
    for path, info in [(database, database_stat)]:
        assert (info.st_uid, info.st_gid) == (expected_uid, expected_gid), (str(path), info)
        assert stat.S_IMODE(info.st_mode) == 0o640, (str(path), oct(stat.S_IMODE(info.st_mode)))
        checked[path.name] = oct(stat.S_IMODE(info.st_mode))
    for suffix in ("-wal", "-shm", "-journal"):
        path = Path(f"{database}{suffix}")
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        assert stat.S_ISREG(info.st_mode), (str(path), info)
        assert (info.st_uid, info.st_gid) == (expected_uid, expected_gid), (str(path), info)
        assert stat.S_IMODE(info.st_mode) == 0o640, (str(path), oct(stat.S_IMODE(info.st_mode)))
        checked[path.name] = oct(stat.S_IMODE(info.st_mode))
    return {"directory_owner": "root" if expected_uid == 0 else str(expected_uid), "directory_group": group_name,
            "directory_mode": oct(stat.S_IMODE(directory_stat.st_mode)),
            "index_file_owner": "root" if expected_uid == 0 else str(expected_uid), "index_file_group": group_name,
            "index_files": checked}


def wait_service_active(timeout, stage):
    deadline = time.monotonic() + timeout
    while True:
        state = service_state()
        if state["active"] == "active":
            return state
        if state["active"] == "failed":
            journal = command("journalctl", "-u", "dux.service", "--no-pager", "-n", "30", check=False)
            raise AssertionError({"stage": stage, "service": state,
                                  "journal": journal.stdout, "journal_error": journal.stderr})
        if time.monotonic() >= deadline:
            raise AssertionError({"stage": stage, "service_start_timeout": state,
                                  "timeout_seconds": timeout})
        time.sleep(0.5)


def wait_daemon_caught_up(timeout, stage):
    """Require a clean, complete first reconciliation, not just Type=notify active."""
    deadline = time.monotonic() + timeout
    last_status = None
    while True:
        result = command("/usr/bin/dux", "status", "--json", check=False)
        try:
            last_status = json.loads(result.stdout)
        except (json.JSONDecodeError, TypeError):
            last_status = {
                "status_command_returncode": result.returncode,
                "status_stdout": result.stdout[-4000:],
                "status_stderr": result.stderr[-4000:],
            }
        if (result.returncode == 0 and isinstance(last_status, dict)
                and last_status.get("daemon_live") is True
                and last_status.get("watch_status_available") is True
                and last_status.get("dirty") is False
                and last_status.get("paused") is False
                and last_status.get("scan_in_progress") is False
                and last_status.get("caught_up") is True):
            return last_status
        reason = (last_status.get("dirty_reason") or "") if isinstance(last_status, dict) else ""
        if "automatic retries paused" in reason.lower():
            journal = command("journalctl", "-u", "dux.service", "--no-pager", "-n", "50", check=False)
            raise AssertionError({"stage": stage, "automatic_reconciliation_paused": last_status,
                                  "timeout_seconds": timeout,
                                  "journal": journal.stdout, "journal_error": journal.stderr})
        if time.monotonic() >= deadline:
            journal = command("journalctl", "-u", "dux.service", "--no-pager", "-n", "50", check=False)
            raise AssertionError({"stage": stage, "daemon_not_caught_up": last_status,
                                  "timeout_seconds": timeout,
                                  "journal": journal.stdout, "journal_error": journal.stderr})
        time.sleep(0.5)


def structured_failure_evidence(error):
    """Preserve assertion diagnostics as JSON data as well as a readable error."""
    if isinstance(error, AssertionError) and error.args and isinstance(error.args[0], dict):
        return error.args[0]
    return None


def install(kind, package, downgrade=False):
    if kind == "rpm":
        args = ["rpm", "-Uvh", "--replacepkgs"]
        if downgrade:
            args.append("--oldpackage")
        args.append(package)
    else:
        args = ["dpkg", "-i", package]
    return command(*args)


def package_payload_hash(kind, package, payload_path):
    """Hash a named payload file without installing or executing the package."""
    payload_path = Path(payload_path)
    relative = payload_path.as_posix().lstrip("/")
    with tempfile.TemporaryDirectory(prefix="dux-package-payload-") as directory:
        if kind == "deb":
            command("dpkg-deb", "-x", package, directory)
        else:
            archive = subprocess.run(
                ["rpm2cpio", str(package)], check=True, capture_output=True
            )
            subprocess.run(
                ["cpio", "-id", "--quiet", f"./{relative}"],
                cwd=directory, input=archive.stdout, check=True, capture_output=True,
            )
        payload = Path(directory) / relative
        return _sha256_file(payload)


def package_binary_hash(kind, package):
    """Hash the packaged daemon payload without installing or executing it."""
    return package_payload_hash(kind, package, "/usr/bin/dux")


def package_backup_helper_hash(kind, package):
    """Hash the installed rollback helper from the package payload."""
    return package_payload_hash(kind, package, "/usr/lib/dux/dux-backup-index.py")


def installed_backup_helper_hash(path=Path("/usr/lib/dux/dux-backup-index.py")):
    return _sha256_file(path)


def rollback_backup_path_for_report(report_path):
    """Keep each lifecycle attempt's rollback snapshot isolated from prior runs."""
    report_path = Path(report_path)
    return report_path.with_name(f"{report_path.stem}-rollback.sqlite")


def create_rollback_index_backup(database, backup):
    """Use SQLite's online backup API to preserve a schema-matched rollback index."""
    database, backup = Path(database), Path(backup)
    if not database.is_file():
        return None
    if backup.exists():
        raise FileExistsError(f"refusing to replace an existing rollback backup: {backup}")
    parent_metadata = backup.parent.stat()
    assert parent_metadata.st_uid == os.geteuid() and stat.S_IMODE(parent_metadata.st_mode) & 0o077 == 0, {
        "stage": "rollback_backup_directory_security", "path": str(backup.parent),
        "uid": parent_metadata.st_uid, "mode": oct(stat.S_IMODE(parent_metadata.st_mode)),
    }
    metadata = database.stat()
    source = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    destination = sqlite3.connect(backup)
    try:
        source.backup(destination)
        result = destination.execute("PRAGMA quick_check").fetchone()[0]
        assert result == "ok", {"stage": "rollback_backup_quick_check", "result": result}
    finally:
        destination.close()
        source.close()
    os.chown(backup, metadata.st_uid, metadata.st_gid)
    os.chmod(backup, stat.S_IMODE(metadata.st_mode))
    return {"path": str(backup), "sha256": _sha256_file(backup),
            "mode": oct(stat.S_IMODE(backup.stat().st_mode)), "quick_check": "ok"}


def restore_rollback_index_backup(database, backup):
    """Restore a verified snapshot and discard stale SQLite sidecars."""
    database, backup = Path(database), Path(backup)
    source = sqlite3.connect(f"file:{backup}?mode=ro", uri=True)
    try:
        result = source.execute("PRAGMA quick_check").fetchone()[0]
        assert result == "ok", {"stage": "rollback_restore_quick_check", "result": result}
    finally:
        source.close()
    metadata = database.stat() if database.exists() else backup.stat()
    temporary_fd, temporary_name = tempfile.mkstemp(
        prefix=f".{database.name}.rollback-restore.", dir=database.parent
    )
    os.close(temporary_fd)
    temporary = Path(temporary_name)
    try:
        source = sqlite3.connect(f"file:{backup}?mode=ro", uri=True)
        try:
            destination = sqlite3.connect(temporary)
            try:
                source.backup(destination)
                destination.execute("PRAGMA journal_mode=DELETE")
                result = destination.execute("PRAGMA quick_check").fetchone()[0]
                assert result == "ok", {"stage": "rollback_restore_copy_quick_check", "result": result}
            finally:
                destination.close()
        finally:
            source.close()
        os.chown(temporary, metadata.st_uid, metadata.st_gid)
        os.chmod(temporary, stat.S_IMODE(metadata.st_mode))
        for suffix in ("-wal", "-shm", "-journal"):
            sidecar = Path(f"{database}{suffix}")
            if sidecar.exists() or sidecar.is_symlink():
                sidecar.unlink()
        os.replace(temporary, database)
    finally:
        for suffix in ("", "-wal", "-shm", "-journal"):
            path = Path(f"{temporary}{suffix}")
            try:
                path.unlink()
            except FileNotFoundError:
                pass
    return {"sha256": _sha256_file(database),
            "mode": oct(stat.S_IMODE(database.stat().st_mode)), "quick_check": "ok"}


def baseline_install_needed(installed_version, expected_version, installed_binary_hash,
                            expected_binary_hash):
    """Require the baseline package payload to match, not just its version.

    Disposable guest overlays can survive between qualification runs. A package
    version alone is not an identity: locally rebuilt or previously qualified
    artifacts may share the same version string while containing different code.
    """
    return (installed_version != expected_version
            or installed_binary_hash != expected_binary_hash)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("rpm", "deb"))
    parser.add_argument("old_package")
    parser.add_argument("new_package")
    parser.add_argument("--report", required=True)
    args = parser.parse_args()
    sandbox = require_disposable_qualification_vm()
    old_package, new_package = Path(args.old_package), Path(args.new_package)
    report_path = Path(args.report)
    report = {
        "status": "running", "kind": args.kind,
        "platform": platform.platform(), "kernel": platform.release(),
        "started_at_epoch": time.time(), "steps": [],
        "sandbox": sandbox,
        "old_package_sha256": _sha256_file(old_package),
        "new_package_sha256": _sha256_file(new_package),
    }

    def record(name, evidence):
        report["steps"].append({"name": name, "evidence": evidence})
        report_path.parent.mkdir(parents=True, exist_ok=True)
        temporary = report_path.with_suffix(report_path.suffix + ".tmp")
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(report_path)

    try:
        initial = installed_version(args.kind)
        old_version = installed_version_from_package(args.kind, old_package)
        old_payload_hash = package_binary_hash(args.kind, old_package)
        old_binary = Path("/usr/bin/dux")
        initial_binary_hash = (
            _sha256_file(old_binary)
            if old_binary.is_file() else None
        )
        baseline_reinstall = baseline_install_needed(
            initial, old_version, initial_binary_hash, old_payload_hash
        )
        if baseline_reinstall:
            install(args.kind, old_package,
                    downgrade=initial is not None and initial != old_version)
        old_probe = command(str(old_binary), "--version", check=False)
        assert old_probe.returncode == 0, {
            "stage": "legacy_baseline", "returncode": old_probe.returncode,
            "stdout": old_probe.stdout, "stderr": old_probe.stderr,
        }
        assert _sha256_file(old_binary) == old_payload_hash
        service_after_baseline_install = service_state()
        if service_after_baseline_install["active"] == "failed":
            command("systemctl", "reset-failed", "dux.service", check=False)
        command("systemctl", "start", "--no-block", "dux.service", check=False)
        legacy_running = wait_service_active(300, "legacy_baseline_runtime")
        legacy_baseline = wait_daemon_caught_up(900, "legacy_baseline_reconciliation")
        # Keep optional/missing tunnel units from making systemctl reject the
        # whole multi-unit request before it disables the main service.
        for unit in ("dux.service", "dux-tunnel.service"):
            command("systemctl", "stop", unit, check=False)
            command("systemctl", "disable", unit, check=False)
        record("legacy_baseline", {"initial_version": initial,
                                    "staged_version": installed_version(args.kind),
                                    "initial_binary_sha256": initial_binary_hash,
                                    "baseline_package_reinstalled": baseline_reinstall,
                                    "binary_sha256": _sha256_file(Path("/usr/bin/dux")),
                                    "package_payload_binary_sha256": old_payload_hash,
                                    "service_after_install": service_after_baseline_install,
                                    "service_runtime_verified": legacy_running,
                                    "daemon_caught_up": legacy_baseline,
                                    "service": service_state()})

        rollback_backup = create_rollback_index_backup(
            "/var/lib/dux/dux.db",
            rollback_backup_path_for_report(report_path),
        )
        install(args.kind, new_package)
        upgraded = installed_version(args.kind)
        assert upgraded and upgraded != installed_version_from_package(args.kind, old_package), upgraded
        new_payload_hash = package_binary_hash(args.kind, new_package)
        new_backup_helper_hash = package_backup_helper_hash(args.kind, new_package)
        assert _sha256_file(Path("/usr/bin/dux")) == new_payload_hash
        assert installed_backup_helper_hash() == new_backup_helper_hash, {
            "stage": "upgrade_backup_helper_payload",
            "installed_sha256": installed_backup_helper_hash(),
            "package_sha256": new_backup_helper_hash,
        }
        assert command("systemctl", "is-enabled", "dux.service", check=False).stdout.strip() == "disabled", service_state()
        assert command("systemctl", "is-active", "dux.service", check=False).stdout.strip() != "active", service_state()
        assert command("systemctl", "is-enabled", "dux-tunnel.service", check=False).stdout.strip() in {"disabled", "not-found"}, service_state()
        assert command("systemctl", "is-active", "dux-tunnel.service", check=False).stdout.strip() != "active", service_state()
        command("rpm", "-V", "dux") if args.kind == "rpm" else None
        record("upgrade_preserves_disabled_services", {"version": upgraded,
                                                        "binary_sha256": _sha256_file(Path("/usr/bin/dux")),
                                                        "package_payload_binary_sha256": new_payload_hash,
                                                        "backup_helper_sha256": new_backup_helper_hash,
                                                        "rollback_index_backup": rollback_backup,
                                                        "service": service_state()})

        install(args.kind, old_package, downgrade=True)
        rolled_back = installed_version(args.kind)
        assert rolled_back == installed_version_from_package(args.kind, old_package), rolled_back
        rollback_service_after_install = service_state()
        rollback_binary = Path("/usr/bin/dux")
        rollback_probe = command(str(rollback_binary), "--version", check=False)
        assert rollback_probe.returncode == 0, {
            "stage": "rollback", "returncode": rollback_probe.returncode,
            "stdout": rollback_probe.stdout, "stderr": rollback_probe.stderr,
        }
        assert _sha256_file(rollback_binary) == old_payload_hash
        rollback_restore = None
        if rollback_backup:
            # Rollback follows the documented procedure: restore a verified
            # pre-upgrade SQLite snapshot before starting the older executable.
            command("systemctl", "stop", "dux.service", check=False)
            assert service_state()["active"] in {"inactive", "failed"}, service_state()
            rollback_restore = restore_rollback_index_backup(
                "/var/lib/dux/dux.db", rollback_backup["path"]
            )
        if rollback_service_after_install["active"] == "failed":
            command("systemctl", "reset-failed", "dux.service", check=False)
        command("systemctl", "start", "--no-block", "dux.service", check=False)
        legacy_rollback_service = wait_service_active(300, "rollback_runtime")
        record("rollback", {"version": rolled_back,
                             "binary_sha256": _sha256_file(rollback_binary),
                             "package_payload_binary_sha256": old_payload_hash,
                             "index_restore": rollback_restore,
                             "service_after_install": rollback_service_after_install,
                             "service_runtime_verified": legacy_rollback_service,
                             "legacy_package_service_behavior": legacy_rollback_service})
        # Legacy releases may enable/start the service from their install
        # scriptlet during downgrade. Normalize that test-only state before
        # checking that the new package preserves an operator's disabled choice.
        for unit in ("dux.service", "dux-tunnel.service"):
            command("systemctl", "stop", unit, check=False)
            command("systemctl", "disable", unit, check=False)

        install(args.kind, new_package)
        assert installed_version(args.kind) == upgraded
        assert _sha256_file(Path("/usr/bin/dux")) == new_payload_hash
        assert installed_backup_helper_hash() == new_backup_helper_hash
        assert command("systemctl", "is-enabled", "dux.service", check=False).stdout.strip() == "disabled", service_state()
        assert command("systemctl", "is-active", "dux.service", check=False).stdout.strip() != "active", service_state()
        record("re_upgrade", {"version": installed_version(args.kind),
                               "binary_sha256": new_payload_hash,
                               "package_payload_binary_sha256": new_payload_hash,
                               "backup_helper_sha256": new_backup_helper_hash,
                               "service": service_state()})

        if args.kind == "rpm":
            command("rpm", "-e", "dux")
        else:
            command("dpkg", "-r", "dux")
        assert installed_version(args.kind) is None
        assert not Path("/usr/bin/dux").exists()
        state_retained = Path("/var/lib/dux").exists()
        if args.kind == "deb":
            assert state_retained, "remove must retain the index until purge"
            command("dpkg", "-P", "dux")
            assert not Path("/var/lib/dux").exists(), "purge must remove package state"
        state_reset_for_clean_install = not Path("/var/lib/dux").exists()
        if args.kind == "rpm" and not state_reset_for_clean_install:
            # RPM erase intentionally preserves application state. This is a
            # disposable, controller-marked guest, so remove only this package's
            # retained state to make the following install genuinely clean.
            shutil.rmtree("/var/lib/dux")
            state_reset_for_clean_install = not Path("/var/lib/dux").exists()
        assert state_reset_for_clean_install, "clean-install phase requires an empty package state directory"
        record("remove_and_purge", {"package_removed": True,
                                     "state_retained_after_remove": state_retained,
                                     "purge_supported": args.kind == "deb",
                                     "state_removed_after_purge": not Path("/var/lib/dux").exists() if args.kind == "deb" else None,
                                     "state_reset_for_clean_install": state_reset_for_clean_install,
                                     "service": service_state()})

        # Old packages can leave an optional tunnel key behind. A clean install
        # test starts from clean state so it verifies the current package does
        # not create remote-access identities by default.
        shutil.rmtree("/etc/dux", ignore_errors=True)
        install(args.kind, new_package)
        clean_version = installed_version(args.kind)
        assert clean_version == upgraded, clean_version
        # Type=notify stays in systemd's "reloading" state while dux builds the
        # initial index. Wait for that real readiness transition instead of
        # treating the first scan as a package startup failure.
        clean_service = wait_service_active(300, "clean_install_runtime")
        clean_daemon = wait_daemon_caught_up(180, "clean_install_reconciliation")
        assert clean_service["enabled"] == "enabled" and clean_service["active"] == "active", clean_service
        assert clean_service["tunnel_enabled"] in {"disabled", "not-found"}, clean_service
        assert clean_service["tunnel_active"] != "active", clean_service
        assert not Path("/etc/dux/ed25519").exists(), "clean install unexpectedly created a tunnel identity"
        unit_security = assert_service_security_policy()
        module_path = Path("/usr/lib/modules")
        if module_path.is_dir():
            unit_security["module_tree_visible"] = assert_service_path_visible(
                module_path, unit_security["MainPID"]
            )
        index_access = assert_index_access_policy()
        binary_hash = _sha256_file(Path("/usr/bin/dux"))
        clean_backup_helper_hash = installed_backup_helper_hash()
        assert binary_hash == new_payload_hash, {
            "stage": "clean_install", "installed_binary_sha256": binary_hash,
            "package_payload_sha256": new_payload_hash,
        }
        assert clean_backup_helper_hash == new_backup_helper_hash, {
            "stage": "clean_install_backup_helper",
            "installed_sha256": clean_backup_helper_hash,
            "package_sha256": new_backup_helper_hash,
        }
        assert command("/usr/bin/dux", "--version").returncode == 0
        if args.kind == "rpm":
            command("rpm", "-V", "dux")
        record("clean_install", {"version": clean_version, "binary_sha256": binary_hash,
                                 "package_payload_binary_sha256": new_payload_hash,
                                 "backup_helper_sha256": clean_backup_helper_hash,
                                 "service": clean_service,
                                 "daemon_caught_up": clean_daemon,
                                 "service_security_policy": unit_security,
                                 "index_access_policy": index_access,
                                 "tunnel_identity_created": Path("/etc/dux/ed25519").exists()})

        command("systemctl", "stop", "dux.service", check=False)
        command("systemctl", "disable", "dux.service", check=False)
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = repr(error)
        evidence = structured_failure_evidence(error)
        if evidence is not None:
            report["failure_evidence"] = evidence
        report["installed_version_at_failure"] = installed_version(args.kind)
        report["service_at_failure"] = service_state()
        raise
    finally:
        report["elapsed_seconds"] = round(time.time() - report["started_at_epoch"], 3)
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


def installed_version_from_package(kind, package):
    if kind == "rpm":
        return command("rpm", "-qp", "--qf", "%{VERSION}-%{RELEASE}", package).stdout.strip()
    return command("dpkg-deb", "-f", package, "Version").stdout.strip()


if __name__ == "__main__":
    main()
