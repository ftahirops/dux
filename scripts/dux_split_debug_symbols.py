#!/usr/bin/env python3
"""Create a separate, Build-ID-matched debug file without changing runtime code."""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def run(*args: str) -> str:
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT)


def build_id(path: Path) -> str:
    output = run("readelf", "-n", str(path))
    match = re.search(r"Build ID: ([0-9a-f]+)", output)
    if not match:
        raise RuntimeError(f"no ELF Build ID found in {path}")
    return match.group(1)


def sections(path: Path) -> str:
    return run("readelf", "-W", "-S", str(path))


def split_debug_symbols(binary: Path, debug_output: Path) -> None:
    binary = binary.resolve()
    debug_output = debug_output.resolve()
    if binary == debug_output:
        raise ValueError("binary and debug output paths must differ")
    for tool in ("objcopy", "strip", "readelf"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"required tool not found: {tool}")
    original_build_id = build_id(binary)
    original_sections = sections(binary)
    if ".debug_info" not in original_sections:
        raise RuntimeError("input binary has no DWARF debug information")

    debug_output.parent.mkdir(parents=True, exist_ok=True)
    with (
        tempfile.TemporaryDirectory(prefix=".dux-runtime-", dir=binary.parent) as runtime_dir,
        tempfile.TemporaryDirectory(prefix=".dux-symbols-", dir=debug_output.parent) as debug_dir,
    ):
        runtime_temp = Path(runtime_dir) / binary.name
        debug_temp = Path(debug_dir) / debug_output.name
        shutil.copy2(binary, runtime_temp)
        run("objcopy", "--only-keep-debug", str(binary), str(debug_temp))
        run("strip", "--strip-debug", str(runtime_temp))
        run("objcopy", f"--add-gnu-debuglink={debug_temp}", str(runtime_temp))

        debug_sections = sections(debug_temp)
        runtime_sections = sections(runtime_temp)
        if ".debug_info" not in debug_sections:
            raise RuntimeError("split debug artifact lost DWARF information")
        if ".debug_info" in runtime_sections or ".symtab" not in runtime_sections:
            raise RuntimeError("runtime binary lost its symbols or retained DWARF information")
        if ".gnu_debuglink" not in runtime_sections:
            raise RuntimeError("runtime binary lacks its debug link")
        if build_id(debug_temp) != original_build_id or build_id(runtime_temp) != original_build_id:
            raise RuntimeError("split artifacts do not retain the original Build ID")

        os.replace(debug_temp, debug_output)
        os.replace(runtime_temp, binary)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("debug_output", type=Path)
    args = parser.parse_args()
    try:
        split_debug_symbols(args.binary, args.debug_output)
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        parser.exit(1, f"ERROR: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
