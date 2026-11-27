"""Admission guard for destructive or resource-intensive Dux qualification."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import stat
import subprocess

if not __debug__:
    raise RuntimeError(
        "qualification scripts require assertions enabled; do not use python -O"
    )


SANDBOX_MARKER = Path("/etc/dux-qualification-sandbox")


def require_disposable_qualification_vm(
    *, require_root: bool = True
) -> dict[str, str]:
    """Require root plus an explicit root-owned marker before test side effects."""
    if require_root and os.geteuid() != 0:
        raise RuntimeError("qualification requires root inside its disposable VM")
    try:
        marker = SANDBOX_MARKER.lstat()
    except FileNotFoundError as error:
        marker = None
        marker_error = error
    else:
        marker_error = None
    marker_valid = marker is not None
    if marker_valid and (
        not stat.S_ISREG(marker.st_mode)
        or marker.st_uid != 0
        or stat.S_IMODE(marker.st_mode) != 0o400
    ):
        raise RuntimeError("qualification VM marker must be a root-owned regular 0400 file")
    if marker_valid and SANDBOX_MARKER.read_text().strip() != "dux-qualification-sandbox-v1":
        raise RuntimeError("qualification VM marker has invalid contents")

    vm_type = "unknown"
    if shutil.which("systemd-detect-virt") is not None:
        virtualization = subprocess.run(
            ["systemd-detect-virt", "--vm"], capture_output=True, text=True, check=False
        )
        if virtualization.returncode == 0:
            vm_type = virtualization.stdout.strip()
    if vm_type not in {"kvm", "qemu"} and not marker_valid:
        raise RuntimeError(
            "qualification is restricted to a marked KVM/QEMU guest or explicitly marked runner"
        )
    if not marker_valid:
        raise RuntimeError(
            "qualification VM marker is absent; create this guest with dux-qualification-vm.py"
        ) from marker_error
    return {
        "virtualization": vm_type if vm_type in {"kvm", "qemu"} else "explicit-marker",
        "marker": str(SANDBOX_MARKER),
    }
