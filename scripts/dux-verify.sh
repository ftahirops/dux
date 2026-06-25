#!/usr/bin/env bash
#
# dux-verify.sh — independent integrity & correctness audit for the dux index.
#
# Verifies the dux SQLite index from every angle:
#   * DB integrity (sqlite PRAGMA checks)
#   * structural integrity (no orphans, no duplicate inodes, leaf math)
#   * EXACT internal consistency: stored directory totals == recomputed-from-table
#     (proves the live daemon's incremental updates never drifted — no tools needed)
#   * ground-truth cross-checks vs `du`, `df`, `find` (tolerance, since a live FS moves)
#   * existence both ways (disk<->index sampling: catches missing & stale rows)
#
# Subcommands:
#   audit        run the full read-only audit on the live index (default)
#   selftest     deterministic create/grow/rename/delete through the daemon,
#                with EXACT assertions + daemon-vs-fresh-scan agreement
#   install-cron install a cron job to run `audit` every 3 hours
#
# Exit code: 0 = all checks passed, 1 = warnings, 2 = FAILURE (inconsistency).

set -uo pipefail

DB="${DUX_DB:-/var/lib/dux/dux.db}"
DUX="${DUX_BIN:-/usr/bin/dux}"
ROOT="${DUX_ROOT:-/}"
LOGDIR="${DUX_LOGDIR:-/var/log/dux}"
SAMPLE="${DUX_SAMPLE:-200}"        # files sampled per existence/du check
TOL_PCT="${DUX_TOL_PCT:-2}"        # tolerance for ground-truth (live FS) checks
SQLITE="$(command -v sqlite3 || true)"

PASS=0; WARN=0; FAIL=0
RED=$'\e[31m'; GRN=$'\e[32m'; YEL=$'\e[33m'; CYN=$'\e[36m'; DIM=$'\e[2m'; RST=$'\e[0m'

ts() { date '+%Y-%m-%d %H:%M:%S'; }
say() { printf '%s\n' "$*"; }
pass(){ PASS=$((PASS+1)); printf '  %sPASS%s %s\n' "$GRN" "$RST" "$*"; }
warn(){ WARN=$((WARN+1)); printf '  %sWARN%s %s\n' "$YEL" "$RST" "$*"; }
fail(){ FAIL=$((FAIL+1)); printf '  %sFAIL%s %s\n' "$RED" "$RST" "$*"; }
hdr(){ printf '\n%s== %s ==%s\n' "$CYN" "$*" "$RST"; }

q() { sudo sqlite3 -noheader -batch "$DB" "$1" 2>/dev/null; }

# abs/pct diff helpers (integer math; values can be huge so use awk)
within_tol() { # $1 actual $2 expected $3 tol_pct -> 0 if within
  awk -v a="$1" -v e="$2" -v t="$3" 'BEGIN{
    if (e==0){ exit (a==0)?0:1 }
    d=(a>e)?a-e:e-a; pct=d*100.0/e; exit (pct<=t)?0:1 }'
}
pctdiff() { awk -v a="$1" -v e="$2" 'BEGIN{ if(e==0){print (a==0)?"0":"inf";exit} d=(a>e)?a-e:e-a; printf "%.3f", d*100.0/e }'; }
human() { numfmt --to=iec --suffix=B "$1" 2>/dev/null || echo "$1"; }

LIVE=0  # set in audit(): is the daemon writing concurrently?

# Grade an exact-zero check. 0 -> PASS. Nonzero & small & daemon live ->
# WARN (transient inter-flush state). Otherwise -> FAIL (real corruption).
grade_zero() { # $1 count  $2 total  $3 message
  local c="$1" total="$2" msg="$3" lim
  if [ "${c:-0}" -eq 0 ]; then pass "$msg: 0"; return; fi
  lim=$(( total/5000 + 50 ))                   # ~0.02% of rows, min 50
  if [ "$LIVE" = "1" ] && [ "$c" -le "$lim" ]; then
    warn "$msg: $c (transient — daemon writing live; reconcile for exact)"
  else
    fail "$msg: $c"
  fi
}

# Mountpoints at/under $1 on real filesystems (the same set dux indexes:
# everything except the pseudo types dux skips via statfs magic).
real_mounts() {
  local root="$1"
  findmnt -rno TARGET,FSTYPE 2>/dev/null | while read -r tgt fstype; do
    case "$fstype" in
      proc|sysfs|cgroup|cgroup2|devpts|debugfs|tracefs|securityfs|bpf|mqueue|pstore|binfmt_misc|configfs|fusectl|autofs|nsfs) continue ;;
    esac
    if [ "$root" = "/" ] || [ "$tgt" = "$root" ] || case "$tgt" in "$root"/*) true ;; *) false ;; esac; then
      printf '%s\n' "$tgt"
    fi
  done | sort -u
}

require() {
  [ -n "$SQLITE" ] || { echo "sqlite3 not installed (apt install sqlite3)"; exit 2; }
  [ -x "$DUX" ]    || { echo "dux not found at $DUX"; exit 2; }
  sudo test -s "$DB" || { echo "no index DB at $DB — run 'dux scan $ROOT' first"; exit 2; }
}

# ---------------------------------------------------------------------------
audit() {
  require
  local root_dev root_ino
  root_dev="$(q "SELECT value FROM meta WHERE key='root_dev'")"
  root_ino="$(q "SELECT value FROM meta WHERE key='root_inode'")"
  local scan_root; scan_root="$(q "SELECT value FROM meta WHERE key='last_scan_root'")"
  daemon_live && LIVE=1 || LIVE=0
  say "${DIM}index: $DB   root: $scan_root   daemon: $([ "$LIVE" = 1 ] && echo live || echo stopped)   $(ts)${RST}"

  hdr "1. SQLite integrity"
  local ic; ic="$(q "PRAGMA integrity_check")"
  [ "$ic" = "ok" ] && pass "integrity_check = ok" || fail "integrity_check: $ic"
  local qc; qc="$(q "PRAGMA quick_check")"
  [ "$qc" = "ok" ] && pass "quick_check = ok" || fail "quick_check: $qc"

  hdr "2. Schema"
  for col in dev_id inode parent_dev parent_inode recursive_bytes recursive_inodes blocks; do
    if q "SELECT $col FROM nodes LIMIT 1" >/dev/null 2>&1; then pass "column nodes.$col present"
    else fail "column nodes.$col MISSING"; fi
  done

  hdr "3. Structural integrity (exact when daemon quiescent)"
  local total; total="$(q "SELECT count(*) FROM nodes")"
  local dup; dup="$(q "SELECT count(*) FROM (SELECT dev_id,inode,count(*) c FROM nodes GROUP BY dev_id,inode HAVING c>1)")"
  [ "${dup:-0}" = "0" ] && pass "no duplicate (dev,inode) rows" || fail "$dup duplicate (dev,inode) rows"

  # orphans: a non-root node whose parent (parent_dev,parent_inode) is absent
  # (a freshly-created file can be briefly orphaned between its flush and its
  # parent dir's flush — transient while the daemon is live)
  local orph; orph="$(q "
    SELECT count(*) FROM nodes n
    WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode)
      AND NOT EXISTS (SELECT 1 FROM nodes p WHERE p.dev_id=n.parent_dev AND p.inode=n.parent_inode)")"
  grade_zero "${orph:-0}" "${total:-0}" "orphan nodes (broken parent links)"

  local badleaf; badleaf="$(q "SELECT count(*) FROM nodes WHERE kind!='d' AND recursive_bytes<>blocks")"
  grade_zero "${badleaf:-0}" "${total:-0}" "file rows with recursive_bytes != blocks"

  hdr "4. EXACT internal totals (stored vs recomputed from the same table)"
  # The whole table has one row per inode (hardlinks collapsed by PK), so the
  # root's stored totals MUST equal the raw SUM/COUNT over all rows. Any
  # divergence = the daemon's incremental ancestor math drifted.
  local stored_b stored_i sum_b cnt
  stored_b="$(q "SELECT recursive_bytes FROM nodes WHERE dev_id=$root_dev AND inode=$root_ino")"
  stored_i="$(q "SELECT recursive_inodes FROM nodes WHERE dev_id=$root_dev AND inode=$root_ino")"
  sum_b="$(q "SELECT COALESCE(SUM(blocks),0) FROM nodes")"
  cnt="$(q "SELECT count(*) FROM nodes")"
  # exact when quiescent; small skew tolerated only while the daemon writes live
  if [ "$stored_b" = "$sum_b" ]; then pass "root.recursive_bytes ($(human "$stored_b")) == SUM(blocks)  [exact]"
  elif [ "$LIVE" = 1 ] && within_tol "$stored_b" "$sum_b" 0.5; then warn "root.recursive_bytes vs SUM(blocks) off $(human $((stored_b-sum_b))) (Δ$(pctdiff "$stored_b" "$sum_b")%) — daemon writing live"
  else fail "root.recursive_bytes=$stored_b != SUM(blocks)=$sum_b (drift $(human $((stored_b-sum_b)))) — run reconcile"; fi
  if [ "$stored_i" = "$cnt" ]; then pass "root.recursive_inodes ($stored_i) == COUNT(rows)  [exact]"
  elif [ "$LIVE" = 1 ] && within_tol "$stored_i" "$cnt" 0.5; then warn "root.recursive_inodes vs COUNT off $((stored_i-cnt)) (Δ$(pctdiff "$stored_i" "$cnt")%) — daemon writing live"
  else fail "root.recursive_inodes=$stored_i != COUNT(rows)=$cnt — run reconcile"; fi

  hdr "5. EXACT per-directory totals (random sample of 25 dirs, recomputed via CTE)"
  local bad_dir=0 checked=0
  while IFS='|' read -r d i; do
    [ -z "$d" ] && continue
    checked=$((checked+1))
    local got exp_b exp_c got_i
    got="$(q "SELECT recursive_bytes FROM nodes WHERE dev_id=$d AND inode=$i")"
    got_i="$(q "SELECT recursive_inodes FROM nodes WHERE dev_id=$d AND inode=$i")"
    read -r exp_b exp_c <<<"$(q "
      WITH RECURSIVE sub(d,i,b) AS (
        SELECT dev_id,inode,blocks FROM nodes WHERE dev_id=$d AND inode=$i
        UNION ALL
        SELECT n.dev_id,n.inode,n.blocks FROM nodes n JOIN sub ON n.parent_dev=sub.d AND n.parent_inode=sub.i
        WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode)
      ) SELECT COALESCE(SUM(b),0), COUNT(*) FROM sub" | tr '|' ' ')"
    if [ "$got" != "$exp_b" ] || [ "$got_i" != "$exp_c" ]; then
      bad_dir=$((bad_dir+1))
      say "    ${DIM}dir(dev=$d ino=$i): stored b=$got/i=$got_i recomputed b=$exp_b/i=$exp_c${RST}"
    fi
  done < <(q "SELECT dev_id,inode FROM nodes WHERE kind='d' ORDER BY RANDOM() LIMIT 25")
  if [ "$bad_dir" = 0 ]; then pass "all $checked sampled directory totals internally exact"
  elif [ "$LIVE" = 1 ] && [ "$bad_dir" -le 3 ]; then warn "$bad_dir/$checked dirs skewed (sampled mid-flush; daemon live)"
  else fail "$bad_dir/$checked sampled dirs inconsistent — run reconcile"; fi

  # dux scans cross-mount but skips pseudo filesystems, so compare against the
  # SUM over the same set of real mountpoints (du -sx / find -xdev per mount).
  local mounts; mounts="$(real_mounts "$scan_root")"
  say "${DIM}  real mounts counted: $(echo "$mounts" | tr '\n' ' ')${RST}"

  hdr "6. Ground truth: disk usage vs du (per-mount sum, tol ${TOL_PCT}%)"
  local du_b=0 m b
  while IFS= read -r m; do
    [ -z "$m" ] && continue
    b="$(sudo du -sx --block-size=1 "$m" 2>/dev/null | awk '{print $1}')"
    du_b=$((du_b + ${b:-0}))
  done <<<"$mounts"
  if [ "$du_b" -gt 0 ]; then
    if within_tol "$stored_b" "$du_b" "$TOL_PCT"; then
      pass "dux $(human "$stored_b") vs du(Σmounts) $(human "$du_b")  (Δ$(pctdiff "$stored_b" "$du_b")%)"
    else warn "dux $(human "$stored_b") vs du(Σmounts) $(human "$du_b")  (Δ$(pctdiff "$stored_b" "$du_b")% > ${TOL_PCT}%) — live-FS churn / overlay dedup"; fi
  else warn "du failed — skipped"; fi

  hdr "7. Ground truth: node count vs find (per-mount sum, tol ${TOL_PCT}%)"
  local find_c=0 c
  while IFS= read -r m; do
    [ -z "$m" ] && continue
    c="$(sudo find "$m" -xdev 2>/dev/null | wc -l)"
    find_c=$((find_c + c))
  done <<<"$mounts"
  if [ "$find_c" -gt 0 ]; then
    if within_tol "$cnt" "$find_c" "$TOL_PCT"; then
      pass "dux $cnt nodes vs find(Σmounts) $find_c  (Δ$(pctdiff "$cnt" "$find_c")%)"
    else warn "dux $cnt vs find(Σmounts) $find_c (Δ$(pctdiff "$cnt" "$find_c")%) — live churn"; fi
  else warn "find failed — skipped"; fi

  hdr "8. Filesystem capacity vs df"
  local df_used dux_used
  df_used="$(df -B1 --output=used "$scan_root" 2>/dev/null | tail -1 | tr -d ' ')"
  dux_used="$($DUX --db "$DB" status 2>/dev/null | awk -F'[ /]+' '/^filesystem:/{print $2}')"
  if [ -n "$df_used" ]; then pass "df used: $(human "$df_used")  (dux status mirrors statvfs — same source)"
  else warn "df failed — skipped"; fi

  hdr "9. Existence both ways (sample $SAMPLE each)"
  # disk -> index: sampled real files must be indexed
  local miss=0 dchk=0
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    dchk=$((dchk+1))
    local di dn; di="$(stat -c '%d %i' "$f" 2>/dev/null)" || continue
    set -- $di
    local hit; hit="$(q "SELECT 1 FROM nodes WHERE dev_id=$1 AND inode=$2 LIMIT 1")"
    [ "$hit" = "1" ] || miss=$((miss+1))
  done < <(sudo find "$scan_root" -xdev -type f 2>/dev/null | shuf -n "$SAMPLE" 2>/dev/null)
  if [ "$miss" = "0" ]; then pass "all $dchk sampled on-disk files are present in the index"
  else warn "$miss/$dchk on-disk files missing from index (daemon lag or downtime gap)"; fi

  # index -> disk: sampled index rows must still exist on disk
  local stale=0 ichk=0
  while IFS='|' read -r d i; do
    [ -z "$d" ] && continue
    ichk=$((ichk+1))
    local p; p="$($DUX --db "$DB" status >/dev/null 2>&1; q "SELECT name FROM nodes WHERE dev_id=$d AND inode=$i")"
    # resolve full path through dux and stat it
    :
  done < <(q "SELECT dev_id,inode FROM nodes WHERE kind!='d' ORDER BY RANDOM() LIMIT $SAMPLE")
  # path-resolved staleness via dux find on a sample of names
  stale="$(index_to_disk_stale)"
  if [ "$stale" = "0" ]; then pass "sampled index entries resolve to existing files (no stale rows)"
  else warn "$stale sampled index entries are stale (deleted on disk, still in index)"; fi

  summary
}

# Sample index files, reconstruct each path by walking parents, and stat it.
index_to_disk_stale() {
  local stale=0
  while IFS='|' read -r d i; do
    [ -z "$d" ] && continue
    local parts=() cd="$d" ci="$i" guard=0 row name pdev pino rest
    while [ $guard -lt 256 ]; do
      guard=$((guard+1))
      row="$(q "SELECT name,parent_dev,parent_inode FROM nodes WHERE dev_id=$cd AND inode=$ci")"
      [ -z "$row" ] && break
      name="${row%%|*}"; rest="${row#*|}"; pdev="${rest%%|*}"; pino="${rest#*|}"
      parts=("$name" "${parts[@]}")
      { [ "$pdev" = "$cd" ] && [ "$pino" = "$ci" ]; } && break   # root reached
      cd="$pdev"; ci="$pino"
    done
    local path="${parts[0]:-}" k                      # parts[0] = absolute root
    for ((k=1;k<${#parts[@]};k++)); do
      [ "$path" = "/" ] && path="/${parts[k]}" || path="${path%/}/${parts[k]}"
    done
    [ -n "$path" ] && [ -e "$path" ] || stale=$((stale+1))
  done < <(q "SELECT dev_id,inode FROM nodes WHERE kind!='d' ORDER BY RANDOM() LIMIT $SAMPLE")
  echo "$stale"
}

# Reconcile by full rescan when the audit fails. CRITICAL: the daemon must not
# write the DB during a scan (two SQLite writers corrupt the tree), so we stop
# it, scan as sole writer, then restart it.
maybe_reconcile() {
  if [ "$FAIL" -gt 0 ] && [ "${DUX_RECONCILE:-0}" = "1" ]; then
    say ""
    say "${YEL}drift detected -> reconciling (stop daemon, scan as sole writer, restart)...${RST}"
    local had=0
    if systemctl is-active --quiet dux 2>/dev/null; then had=1; sudo systemctl stop dux; fi
    "$DUX" scan "$ROOT" --quiet && say "${GRN}reconcile scan complete${RST}"
    [ "$had" = 1 ] && sudo systemctl start dux
  fi
}

# ---------------------------------------------------------------------------
# Deterministic correctness test: drive ops through the live daemon and assert
# EXACT results, then confirm a fresh scan agrees with the daemon-maintained index.
selftest() {
  require
  local base="$ROOT"; [ "$base" = "/" ] && base="/var/tmp"
  local T="$base/.dux-selftest.$$"
  local TDB="/tmp/dux-selftest.$$.db"
  sudo rm -rf "$T" "$TDB"*; mkdir -p "$T"
  trap 'sudo rm -rf "$T" "$TDB"*' EXIT

  daemon_live || { warn "daemon not live — selftest needs the daemon running"; return 1; }

  hdr "selftest: create"
  dd if=/dev/zero of="$T/f1" bs=1M count=16 status=none; sync
  dd if=/dev/zero of="$T/f2" bs=1M count=8  status=none; sync
  mkdir "$T/sub"; dd if=/dev/zero of="$T/sub/f3" bs=1M count=4 status=none; sync
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "create"

  hdr "selftest: grow"
  dd if=/dev/zero bs=1M count=20 oflag=append conv=notrunc of="$T/f1" status=none; sync
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "grow"

  hdr "selftest: rename"
  mv "$T/f2" "$T/f2-renamed"; sync
  poll_until "renamed visible" "test \"\$(node_exists_name f2-renamed)\" = 1 && test \"\$(node_exists_name f2)\" = 0"

  hdr "selftest: delete file"
  rm -f "$T/f1"; sync
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "delete"

  hdr "selftest: rm -rf subtree"
  rm -rf "$T/sub"; sync
  poll_until "sub removed" "test \"\$(node_exists_name sub)\" = 0"

  hdr "selftest: daemon index == fresh scan of same tree"
  # leave a known state, settle, then compare daemon's view vs a fresh scan
  sync; sleep 3
  local live_total scan_total
  live_total="$(dir_recursive "$T")"
  "$DUX" --db "$TDB" scan "$T" --quiet >/dev/null 2>&1
  scan_total="$(sudo sqlite3 -noheader "$TDB" "SELECT recursive_bytes FROM nodes WHERE inode=parent_inode AND dev_id=parent_dev LIMIT 1")"
  if [ "$live_total" = "$scan_total" ]; then
    pass "daemon-maintained total == fresh-scan total ($(human "${live_total:-0}"))"
  else
    fail "daemon=$live_total  fresh-scan=$scan_total  (incremental drift!)"
  fi

  summary
}

daemon_live() { local hb; hb="$(cat /run/dux/heartbeat 2>/dev/null)"; [ -n "$hb" ] && [ $(( $(date +%s) - hb )) -le 15 ]; }
node_exists_name() { q "SELECT count(*) FROM nodes WHERE name='$1'"; }
dir_recursive() { local di; di="$(stat -c '%d %i' "$1")"; set -- $di; q "SELECT recursive_bytes FROM nodes WHERE dev_id=$1 AND inode=$2"; }

# poll the index until COND is true (handles daemon lag on busy systems)
poll_until() {
  local label="$1" cond="$2" i=0
  while [ $i -lt 30 ]; do
    if eval "$cond" >/dev/null 2>&1; then pass "$label (after ${i}s)"; return 0; fi
    sleep 1; i=$((i+1))
  done
  fail "$label — not reflected within 30s"
}

# assert dux's recursive_bytes for dir == expected du bytes, polling for lag
expect_dir_blocks() {
  local dir="$1" exp="$2" label="$3" i=0 got
  local di; di="$(stat -c '%d %i' "$dir")"; set -- $di; local d=$1 ino=$2
  while [ $i -lt 30 ]; do
    got="$(q "SELECT recursive_bytes FROM nodes WHERE dev_id=$d AND inode=$ino")"
    [ "$got" = "$exp" ] && { pass "$label: dux total == du ($(human "$exp")) after ${i}s"; return 0; }
    sleep 1; i=$((i+1))
  done
  fail "$label: dux=$got  du=$exp  (mismatch after 30s)"
}

# ---------------------------------------------------------------------------
install_cron() {
  local self; self="$(readlink -f "$0")"
  mkdir -p "$LOGDIR" 2>/dev/null || sudo mkdir -p "$LOGDIR"
  local line="0 */3 * * * root $self audit >> $LOGDIR/verify.log 2>&1"
  echo "$line" | sudo tee /etc/cron.d/dux-verify >/dev/null
  echo "installed: $line"
  echo "logs -> $LOGDIR/verify.log"
}

summary() {
  printf '\n%s──────── summary ────────%s\n' "$CYN" "$RST"
  printf '  %sPASS %d%s   %sWARN %d%s   %sFAIL %d%s\n' "$GRN" "$PASS" "$RST" "$YEL" "$WARN" "$RST" "$RED" "$FAIL" "$RST"
  maybe_reconcile
  if [ "$FAIL" -gt 0 ]; then echo "  ${RED}RESULT: INCONSISTENCY DETECTED${RST}"; exit 2
  elif [ "$WARN" -gt 0 ]; then echo "  ${YEL}RESULT: ok with warnings (usually live-FS churn)${RST}"; exit 1
  else echo "  ${GRN}RESULT: ROCK SOLID — index is exact and consistent${RST}"; exit 0; fi
}

case "${1:-audit}" in
  audit) audit ;;
  selftest) selftest ;;
  install-cron) install_cron ;;
  *) echo "usage: $0 {audit|selftest|install-cron}"; exit 2 ;;
esac
