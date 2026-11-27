#!/usr/bin/env bash
set -Eeuo pipefail

bin=${1:?usage: unprivileged-capability-smoke.sh /path/to/dux-r126}
expected_sha256=9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b
actual_sha256=$(sha256sum "$bin" | awk '{print $1}')
[[ "$actual_sha256" == "$expected_sha256" ]] || {
  printf 'FAIL: unexpected candidate hash: %s\n' "$actual_sha256" >&2
  exit 1
}

exec unshare --mount --propagation private --fork /bin/bash -s -- "$bin" <<'INNER'
set -Eeuo pipefail
bin=$1
test_user=${DUX_TEST_USER:-nobody}
uid=$(id -u "$test_user")
gid=$(id -g "$test_user")
tmp=$(mktemp -d /dev/shm/dux-r126-unpriv.XXXXXX)
img=$tmp/ext4.img
mnt=$tmp/mnt
db=$tmp/index.db
log=$tmp/daemon.log
pid=

cleanup() {
  rc=$?
  trap - EXIT INT TERM
  set +e
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
  if mountpoint -q "$mnt"; then umount "$mnt"; fi
  if [[ -f "$log" ]]; then tail -n 50 "$log"; fi
  rm -rf "$tmp"
  exit "$rc"
}
trap cleanup EXIT INT TERM

chmod 0755 "$tmp"
truncate -s 128M "$img"
mkfs.ext4 -F -q "$img"
mkdir "$mnt"
mount -o loop,nodev,nosuid "$img" "$mnt"
chown "$uid:$gid" "$mnt"
chown "$uid:$gid" "$tmp"

# Match the package's two-capability policy, but run with a non-root UID.
setpriv \
  --bounding-set=-all,+sys_admin,+dac_read_search \
  --inh-caps=+sys_admin,+dac_read_search \
  --ambient-caps=+sys_admin,+dac_read_search \
  --reuid="$uid" --regid="$gid" --groups="$gid" --no-new-privs \
  "$bin" daemon "$mnt" --db "$db" --one-file-system --event-apply \
  --max-cpu 100 --flush-ms 25 >"$log" 2>&1 &
pid=$!

wait_json() {
  condition=$1
  for _ in $(seq 1 60); do
    if ! kill -0 "$pid" 2>/dev/null; then
      printf 'FAIL: unprivileged daemon exited before expected status\n'
      return 1
    fi
    status=$("$bin" status --db "$db" --json 2>/dev/null || true)
    if STATUS_JSON="$status" python3 -c "$condition" 2>/dev/null; then return 0; fi
    sleep 1
  done
  return 1
}

wait_json 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("daemon_live") and d.get("caught_up") and not d.get("dirty") and not d.get("scan_in_progress") else 1)' || {
  printf 'FAIL: unprivileged baseline did not become clean\n'
  exit 1
}

proc_uid=$(awk '/^Uid:/ {print $2}' "/proc/$pid/status")
effective_caps=$(awk '/^CapEff:/ {print $2}' "/proc/$pid/status")
expected_caps=$(printf '%016x' "$(( (1 << 21) | (1 << 2) ))")
[[ "$proc_uid" == "$uid" ]] || { printf 'FAIL: daemon uid=%s expected=%s\n' "$proc_uid" "$uid"; exit 1; }
[[ "$effective_caps" == "$expected_caps" ]] || {
  printf 'FAIL: effective capabilities=%s expected=%s\n' "$effective_caps" "$expected_caps"
  exit 1
}

mkdir "$mnt/events"
for i in $(seq 1 8); do printf 'payload-%s\n' "$i" >"$mnt/events/unpriv-$i.txt"; done
mv "$mnt/events/unpriv-1.txt" "$mnt/events/unpriv-renamed.txt"

found=0
for _ in $(seq 1 30); do
  status=$("$bin" status --db "$db" --json)
  paths=$("$bin" find "$mnt" --db "$db" --name 'unpriv-*' --json --limit 100)
  if STATUS_JSON="$status" PATHS_JSON="$paths" python3 -c 'import json,os,sys; s=json.loads(os.environ["STATUS_JSON"]); p=os.environ["PATHS_JSON"]; sys.exit(0 if s.get("events_resolved",0)>0 and "unpriv-renamed.txt" in p and "unpriv-2.txt" in p and "unpriv-1.txt" not in p and s.get("events_dropped",0)==0 and s.get("kernel_overflow_notifications",0)==0 else 1)'; then
    found=1
    break
  fi
  sleep 1
done
[[ "$found" == 1 ]] || { printf 'FAIL: non-root event/handle resolution did not update the index\n'; exit 1; }
printf 'UNPRIVILEGED_STATUS user=%s uid=%s gid=%s CapEff=%s\n' "$test_user" "$proc_uid" "$gid" "$effective_caps"
printf 'PASS: non-root UID with only CAP_SYS_ADMIN and CAP_DAC_READ_SEARCH initializes fanotify and resolves create/rename events\n'
INNER
