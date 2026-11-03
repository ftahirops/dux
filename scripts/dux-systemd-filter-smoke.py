#!/usr/bin/env python3
"""Verify Dux's systemd syscall denylist preserves fanotify FID resolution."""

import os
from pathlib import Path
import platform
import subprocess
import sys
import uuid


PROGRAM = r'''
import ctypes
import errno
import socket
import sys

class FileHandle(ctypes.Structure):
    _fields_ = [
        ("handle_bytes", ctypes.c_uint),
        ("handle_type", ctypes.c_int),
        ("f_handle", ctypes.c_ubyte * 128),
    ]

libc = ctypes.CDLL(None, use_errno=True)

def assert_blocked(number, label, *args):
    ctypes.set_errno(0)
    result = libc.syscall(number, *args)
    syscall_errno = ctypes.get_errno()
    if result != -1 or syscall_errno != errno.ENOSYS:
        print(f"{label} was not blocked as expected: result={result} errno={syscall_errno}", file=sys.stderr)
        raise SystemExit(1)

# x86_64 syscall 165 is mount(2). With SystemCallErrorNumber=ENOSYS,
# this error proves the @mount seccomp group blocked the call.
assert_blocked(165, "mount", None, None, None, 0, None)
# Representative no-op/invalid-argument probes for the additional denied
# groups. They must be rejected by seccomp before reaching the kernel.
assert_blocked(168, "swapoff", None)
assert_blocked(169, "reboot", 0, 0, 0, 0, 0)
assert_blocked(173, "ioperm", 0, 1, 1)
assert_blocked(101, "ptrace", 2, -1, 0, 0)
assert_blocked(298, "perf_event_open", None, 0, 0, -1, 0, 0)
assert_blocked(154, "modify_ldt", 1, None, 0)
assert_blocked(227, "clock_settime", 0, None)
assert_blocked(156, "_sysctl", None)

# clock_gettime (syscall 228) is implicitly permitted even though @clock's
# clock-changing syscalls are denied.
class Timespec(ctypes.Structure):
    _fields_ = [("tv_sec", ctypes.c_long), ("tv_nsec", ctypes.c_long)]

monotonic_now = Timespec()
ctypes.set_errno(0)
clock_result = libc.syscall(228, 1, ctypes.byref(monotonic_now))
clock_errno = ctypes.get_errno()
if clock_result != 0 or monotonic_now.tv_sec < 0:
    print(f"clock_gettime was not permitted: result={clock_result} errno={clock_errno}", file=sys.stderr)
    raise SystemExit(1)

# x86_64 syscall 304 is open_by_handle_at(2). The indexer needs it for fanotify
# FID resolution; the tunnel does not, so its unit must deny it explicitly.
handle = FileHandle()
handle.handle_bytes = 1
handle.handle_type = 1
ctypes.set_errno(0)
result = libc.syscall(304, -1, ctypes.byref(handle), 0)
fid_errno = ctypes.get_errno()
expected_errno = errno.EBADF if sys.argv[1] == "allow-fid" else errno.ENOSYS
if result != -1 or fid_errno != expected_errno:
    print(
        f"open_by_handle_at policy mismatch: expected errno={expected_errno} "
        f"result={result} errno={fid_errno}",
        file=sys.stderr,
    )
    raise SystemExit(1)

print("restricted syscall groups denied; clock_gettime allowed; FID policy verified")

allowed_families = set(sys.argv[2].split())
local_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
local_socket.close()
for name, family in (("AF_INET", socket.AF_INET), ("AF_INET6", socket.AF_INET6)):
    try:
        network_socket = socket.socket(family, socket.SOCK_STREAM)
    except OSError as error:
        if name in allowed_families or error.errno not in (errno.EAFNOSUPPORT, errno.EPERM):
            print(f"{name} policy mismatch: allowed={name in allowed_families} errno={error.errno}", file=sys.stderr)
            raise SystemExit(1)
    else:
        network_socket.close()
        if name not in allowed_families:
            print(f"{name} unexpectedly allowed", file=sys.stderr)
            raise SystemExit(1)

print("address-family policy verified")
'''


def main():
    if sys.platform != "linux" or platform.machine() != "x86_64":
        raise SystemExit("this syscall smoke test requires x86_64 Linux")
    if os.geteuid() != 0:
        raise SystemExit("run this systemd syscall test as root")

    root = Path(__file__).resolve().parents[1]
    for unit_name, fid_policy, address_family_policy in (
        ("dux.service", "allow-fid", "AF_UNIX"),
        ("dux-tunnel.service", "deny-fid", "AF_UNIX AF_INET AF_INET6"),
    ):
        unit_file = root / "packaging" / unit_name
        filters = [
            line.partition("=")[2].strip()
            for line in unit_file.read_text().splitlines()
            if line.startswith("SystemCallFilter=")
        ]
        if len(filters) != 1:
            raise SystemExit(f"{unit_file} must define exactly one SystemCallFilter")
        address_families = [
            line.partition("=")[2].strip()
            for line in unit_file.read_text().splitlines()
            if line.startswith("RestrictAddressFamilies=")
        ]
        if address_families != [address_family_policy]:
            raise SystemExit(
                f"{unit_file} address-family policy mismatch: {address_families}"
            )
        unit = f"dux-syscall-filter-smoke-{uuid.uuid4().hex[:12]}"
        command = [
            "systemd-run",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            f"--unit={unit}",
            f"--property=SystemCallFilter={filters[0]}",
            f"--property=RestrictAddressFamilies={address_family_policy}",
            "--property=SystemCallErrorNumber=ENOSYS",
            "--property=SystemCallArchitectures=native",
            "--property=CPUQuota=5%",
            "--property=MemoryMax=128M",
            "--property=MemorySwapMax=0",
            "--property=RuntimeMaxSec=10s",
            "--",
            sys.executable,
            "-c",
            PROGRAM,
            fid_policy,
            address_family_policy,
        ]
        result = subprocess.run(command, capture_output=True, text=True, timeout=30)
        if result.stdout:
            print(f"{unit_name}: {result.stdout}", end="")
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr)
        if result.returncode:
            raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
