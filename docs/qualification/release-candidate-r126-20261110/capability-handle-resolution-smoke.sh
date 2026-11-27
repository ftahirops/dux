#!/usr/bin/env bash
set -Eeuo pipefail

bin=${1:-/home/dux/target/readiness/production-r126-20261110/dux-0.6.3-x86_64-linux-static}
expected_sha256=9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b
actual_sha256=$(sha256sum "$bin" | awk '{print $1}')
[[ "$actual_sha256" == "$expected_sha256" ]] || {
  printf 'FAIL: unexpected candidate hash: %s\n' "$actual_sha256" >&2
  exit 1
}

exec unshare --mount --propagation private --fork /bin/bash -s -- "$bin" <<'INNER'
set -Eeuo pipefail
bin=$1
tmp=$(mktemp -d /dev/shm/dux-r126-nodac.XXXXXX)
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
  if [[ -f "$log" ]]; then tail -n 60 "$log"; fi
  rm -rf "$tmp"
  exit "$rc"
}
trap cleanup EXIT INT TERM

mkdir "$mnt"
truncate -s 128M "$img"
mkfs.ext4 -F -q "$img"
mount -o loop,nodev,nosuid "$img" "$mnt"

# Keep CAP_SYS_ADMIN so fanotify can initialize and mark the mount, while
# removing only CAP_DAC_READ_SEARCH from the daemon's capability bounding set.
setpriv --bounding-set=-dac_read_search --no-new-privs \
  "$bin" daemon "$mnt" --db "$db" --one-file-system --event-apply \
  --max-cpu 100 --flush-ms 25 >"$log" 2>&1 &
pid=$!

wait_json() {
  condition=$1
  for _ in $(seq 1 60); do
    if ! kill -0 "$pid" 2>/dev/null; then
      printf 'FAIL: daemon exited before expected status\n'
      return 1
    fi
    status=$("$bin" status --db "$db" --json 2>/dev/null || true)
    if STATUS_JSON="$status" python3 -c "$condition" 2>/dev/null; then return 0; fi
    sleep 1
  done
  return 1
}

wait_json 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("daemon_live") and d.get("caught_up") and not d.get("dirty") and not d.get("scan_in_progress") else 1)' || {
  printf 'FAIL: initial baseline did not become clean\n'
  exit 1
}

mkdir "$mnt/events"
for i in $(seq 1 80); do printf 'payload-%s\n' "$i" >"$mnt/events/no-dac-$i.txt"; done

wait_json 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("events_seen",0)>=64 and d.get("dirty") else 1)' || {
  printf 'FAIL: 64+ unresolved events did not mark the index dirty\n'
  exit 1
}

grep -Eq 'received [0-9]+ fanotify events but resolved 0 paths' "$log"
status=$("$bin" status --db "$db" --json)
STATUS_JSON="$status" python3 -c 'import json,os; d=json.loads(os.environ["STATUS_JSON"]); assert d.get("scan_first") is False, d; assert d.get("events_seen",0)>=64, d; assert d.get("events_resolved",0)==0, d; assert d.get("dirty") is True, d; print("CAPABILITY_NEGATIVE_STATUS="+json.dumps({k:d.get(k) for k in ["daemon_live","dirty","events_seen","events_resolved","events_dropped","kernel_overflow_notifications"]},sort_keys=True))'
printf 'PASS: fanotify works with CAP_SYS_ADMIN, missing CAP_DAC_READ_SEARCH is surfaced as dirty uncertainty\n'
INNER
