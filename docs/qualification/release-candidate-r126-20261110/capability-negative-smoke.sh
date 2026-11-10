#!/usr/bin/env bash
set -Eeuo pipefail

bin=${1:-/home/dux/target/readiness/production-r126-20261110/dux-0.6.3-x86_64-linux-static}
expected_sha256=9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b
actual_sha256=$(sha256sum "$bin" | awk '{print $1}')
[[ "$actual_sha256" == "$expected_sha256" ]] || {
  printf 'FAIL: unexpected candidate hash: %s\n' "$actual_sha256" >&2
  exit 1
}

python3 - "$bin" <<'PY'
import grp
import os
import pwd
import subprocess
import sys
import tempfile

binary = sys.argv[1]
nobody = pwd.getpwnam("nobody")
nogroup = grp.getgrnam("nogroup")

with tempfile.TemporaryDirectory(prefix="dux-r126-nocap-", dir="/dev/shm") as tmp:
    os.chown(tmp, nobody.pw_uid, nogroup.gr_gid)
    command = [
        "setpriv", "--reuid=nobody", "--regid=nogroup", "--clear-groups",
        "--inh-caps=-all", "--bounding-set=-all", "--no-new-privs",
        binary, "daemon", tmp, "--db", f"{tmp}/index.db",
        "--one-file-system", "--max-cpu", "100",
    ]
    result = subprocess.run(
        command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        timeout=15, check=False,
    )
    print(f"exit={result.returncode}")
    print(result.stdout, end="")
    if result.returncode == 0:
        raise SystemExit("FAIL: daemon unexpectedly ran without capabilities")
    output = result.stdout.lower()
    if "operation not permitted" not in output or "no watchable filesystem" not in output:
        raise SystemExit("FAIL: startup did not fail closed at fanotify mark")
    print("PASS: candidate fails closed without capabilities")
PY
