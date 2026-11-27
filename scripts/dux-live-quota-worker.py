#!/usr/bin/env python3
"""Private-mount worker used by dux-live-quota-comparison.py."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
from dux_qualification_sandbox import require_disposable_qualification_vm


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--mountpoint", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--cpu-quota-percent", type=int, required=True)
    parser.add_argument("--max-cpu-percent", type=int, required=True)
    parser.add_argument("--operations", type=int, required=True)
    parser.add_argument("--directory-swaps", type=int, required=True)
    args = parser.parse_args()
    try:
        require_disposable_qualification_vm()
    except RuntimeError as error:
        parser.error(str(error))

    mounted = False
    try:
        subprocess.run(
            ["mount", "-o", "loop,nosuid,nodev", str(args.image), str(args.mountpoint)],
            check=True,
        )
        mounted = True
        env = dict(
            os.environ,
            TMPDIR=str(args.mountpoint),
            DUX_LIVE_DIRECT_DAEMON="1",
            DUX_LIVE_CPU_QUOTA_PERCENT=str(args.cpu_quota_percent),
            DUX_LIVE_MAX_CPU_PERCENT=str(args.max_cpu_percent),
            DUX_LIVE_OPERATIONS=str(args.operations),
            DUX_LIVE_DIRECTORY_SWAPS=str(args.directory_swaps),
            DUX_LIVE_REPORT=str(args.report),
        )
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("dux-live-test.py")),
             str(args.binary), "--cpu-quota-percent", str(args.cpu_quota_percent),
             "--max-cpu-percent", str(args.max_cpu_percent)],
            env=env,
            check=False,
        )
        return result.returncode
    finally:
        if mounted:
            subprocess.run(["umount", str(args.mountpoint)], check=True)


if __name__ == "__main__":
    raise SystemExit(main())
