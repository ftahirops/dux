#!/usr/bin/env bash
set -Eeuo pipefail

bin=/dev/shm/dux-r126-fanotify-test
img=/dev/shm/dux-r126-fanotify-test.img
mnt=/dev/shm/dux-r126-fanotify-mnt
db_event=/dev/shm/dux-r126-fanotify-event.db
db_scan=/dev/shm/dux-r126-fanotify-scan.db
log_event=/dev/shm/dux-r126-fanotify-event.log
log_scan=/dev/shm/dux-r126-fanotify-scan.log
trace_event=/dev/shm/dux-r126-fanotify-event.strace
trace_scan=/dev/shm/dux-r126-fanotify-scan.strace
pid=
child=

stop_daemon() {
  if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then
    kill -TERM "$child" 2>/dev/null || true
  elif [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
  fi
  if [[ -n "$pid" ]]; then wait "$pid" 2>/dev/null || true; fi
  pid=
  child=
}

cleanup() {
  rc=$?
  trap - EXIT INT TERM
  set +e
  stop_daemon
  if mountpoint -q "$mnt"; then
    if ! umount "$mnt"; then
      printf 'CLEANUP ERROR: could not unmount %s; preserving loop image\n' "$mnt"
      exit 1
    fi
  fi
  loopdev=$(losetup -j "$img" 2>/dev/null | cut -d: -f1)
  if [[ -n "$loopdev" ]]; then
    if ! losetup -d "$loopdev"; then
      printf 'CLEANUP ERROR: could not detach %s; preserving loop image\n' "$loopdev"
      exit 1
    fi
  fi
  if [[ -f "$log_event" ]]; then
    printf '\n--- event-apply daemon log ---\n'
    tail -n 40 "$log_event"
  fi
  if [[ -f "$trace_event" ]]; then
    printf '\n--- event-apply fanotify syscalls ---\n'
    grep -E 'fanotify_init|fanotify_mark|open_by_handle_at' "$trace_event" | tail -n 40
  fi
  if [[ -f "$log_scan" ]]; then
    printf '\n--- scan-first daemon log ---\n'
    tail -n 30 "$log_scan"
  fi
  if [[ -f "$trace_scan" ]]; then
    printf '\n--- scan-first fanotify syscalls ---\n'
    grep -E 'fanotify_init|fanotify_mark|open_by_handle_at' "$trace_scan" | tail -n 30
  fi
  rmdir "$mnt" 2>/dev/null || true
  rm -f "$img" \
    "$db_event" "$db_event-wal" "$db_event-shm" "$db_event.heartbeat" "$db_event.watch.status" \
    "$db_scan" "$db_scan-wal" "$db_scan-shm" "$db_scan.heartbeat" "$db_scan.watch.status" \
    "$log_event" "$log_scan" "$trace_event" "$trace_scan"
  exit "$rc"
}
trap cleanup EXIT INT TERM

[[ -x "$bin" ]]
[[ ! -e "$img" && ! -e "$db_event" && ! -e "$db_scan" && ! -e "$mnt" ]]
mkdir "$mnt"
truncate -s 128M "$img"
mkfs.ext4 -F -q "$img"
mount -o loop,nodev,nosuid "$img" "$mnt"

start_daemon() {
  mode=$1
  logfile=$2
  tracefile=$3
  db=$4
  shift 4
  strace -f -qq -e trace=fanotify_init,fanotify_mark,open_by_handle_at -o "$tracefile" \
    "$bin" daemon "$mnt" --db "$db" --one-file-system --max-cpu 100 --flush-ms 100 "$@" \
    >"$logfile" 2>&1 &
  pid=$!
  child=
  for _ in $(seq 1 45); do
    child=$(pgrep -P "$pid" -f "$bin daemon" | head -n 1 || true)
    if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then return 0; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 1
  done
  printf 'FAIL: %s daemon process did not start\n' "$mode"
  return 1
}

wait_json_condition() {
  db=$1
  python_condition=$2
  for _ in $(seq 1 45); do
    if ! kill -0 "$child" 2>/dev/null; then
      printf 'FAIL: daemon exited while waiting for status condition\n'
      return 1
    fi
    status=$("$bin" status --db "$db" --json 2>/dev/null || true)
    if STATUS_JSON="$status" python3 -c "$python_condition" 2>/dev/null; then return 0; fi
    sleep 1
  done
  return 1
}

start_daemon event-apply "$log_event" "$trace_event" "$db_event" --event-apply
wait_json_condition "$db_event" 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("daemon_live") and d.get("caught_up") and not d.get("dirty") and not d.get("scan_in_progress") else 1)' || {
  printf 'FAIL: event-apply baseline did not become clean within 45 seconds\n'
  exit 1
}

mkdir "$mnt/events"
for i in $(seq 1 8); do
  printf -v index '%02d' "$i"
  printf 'payload-%s\n' "$index" >"$mnt/events/r126-integration-$index.txt"
done
mv "$mnt/events/r126-integration-01.txt" "$mnt/events/r126-integration-renamed.txt"

found=0
for _ in $(seq 1 30); do
  status=$("$bin" status --db "$db_event" --json)
  paths=$("$bin" find "$mnt" --db "$db_event" --name 'r126-integration-*' --json --limit 100)
  if STATUS_JSON="$status" PATHS_JSON="$paths" python3 -c 'import json,os,sys; s=json.loads(os.environ["STATUS_JSON"]); p=os.environ["PATHS_JSON"]; sys.exit(0 if s.get("events_resolved",0)>0 and "r126-integration-renamed.txt" in p and "r126-integration-02.txt" in p and "r126-integration-01.txt" not in p else 1)'; then
    found=1
    break
  fi
  sleep 1
done
[[ "$found" == 1 ]] || { printf 'FAIL: create/rename events did not resolve into the live index\n'; exit 1; }
status=$("$bin" status --db "$db_event" --json)
STATUS_JSON="$status" python3 -c 'import json,os; d=json.loads(os.environ["STATUS_JSON"]); assert d.get("scan_first") is False, d; assert d.get("events_resolved",0)>0, d; assert d.get("events_dropped",0)==0, d; assert d.get("kernel_overflow_notifications",0)==0, d; print("EVENT_APPLY_STATUS="+json.dumps({k:d.get(k) for k in ["daemon_live","caught_up","dirty","events_seen","events_resolved","events_dropped","kernel_overflow_notifications"]},sort_keys=True))'
grep -Eq 'fanotify_init\(.*\)[[:space:]]+=[[:space:]]+[0-9]+' "$trace_event"
grep -Eq 'fanotify_mark\(.*\)[[:space:]]+=[[:space:]]+0' "$trace_event"
grep -Eq 'open_by_handle_at\(.*\)[[:space:]]+=[[:space:]]+[0-9]+' "$trace_event"
printf 'PASS: event-apply fanotify init/mark, successful open_by_handle_at, create/rename indexed, zero drops/overflows\n'
stop_daemon

start_daemon scan-first "$log_scan" "$trace_scan" "$db_scan"
wait_json_condition "$db_scan" 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("daemon_live") and d.get("scan_first") and not d.get("scan_in_progress") and not d.get("dirty") else 1)' || {
  printf 'FAIL: default scan-first baseline did not become clean within 45 seconds\n'
  exit 1
}
status_before=$("$bin" status --db "$db_scan" --json)
before=$(STATUS_JSON="$status_before" python3 -c 'import json,os; print(json.loads(os.environ["STATUS_JSON"]).get("last_scan_timestamp_seconds",0))')
sleep 1
printf 'scan-first-payload\n' >"$mnt/events/r126-scan-first-new.txt"
wait_json_condition "$db_scan" 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); sys.exit(0 if d.get("events_seen",0)>0 and d.get("dirty") and not d.get("scan_in_progress") else 1)' || {
  printf 'FAIL: scan-first daemon did not record filesystem change as a hint\n'
  exit 1
}
kill -HUP "$child"
found=0
for _ in $(seq 1 60); do
  status=$("$bin" status --db "$db_scan" --json)
  paths=$("$bin" find "$mnt" --db "$db_scan" --name 'r126-scan-first-new.txt' --json --limit 10)
  if STATUS_JSON="$status" PATHS_JSON="$paths" BEFORE_SCAN="$before" python3 -c 'import json,os,sys; d=json.loads(os.environ["STATUS_JSON"]); old=int(os.environ["BEFORE_SCAN"]); sys.exit(0 if d.get("last_scan_timestamp_seconds",0)>old and not d.get("scan_in_progress") and "r126-scan-first-new.txt" in os.environ["PATHS_JSON"] and d.get("kernel_overflow_notifications",0)==0 and d.get("events_dropped",0)==0 else 1)'; then
    found=1
    break
  fi
  sleep 1
done
[[ "$found" == 1 ]] || { printf 'FAIL: manual scan-first observation did not publish the hinted file within 60 seconds\n'; exit 1; }
status=$("$bin" status --db "$db_scan" --json)
STATUS_JSON="$status" python3 -c 'import json,os; d=json.loads(os.environ["STATUS_JSON"]); assert d.get("scan_first") is True, d; assert d.get("kernel_overflow_notifications",0)==0, d; assert d.get("events_dropped",0)==0, d; print("SCAN_FIRST_STATUS="+json.dumps({k:d.get(k) for k in ["daemon_live","scan_first","dirty","scan_in_progress","events_seen","events_resolved","events_dropped","kernel_overflow_notifications","last_scan_age_seconds"]},sort_keys=True))'
printf 'PASS: default scan-first mode records hints and publishes them after a manual full observation\n'
stop_daemon
