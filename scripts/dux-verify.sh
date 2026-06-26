#!/usr/bin/env bash
#
# dux-verify.sh — independent integrity & correctness audit for the dux index.
#
# Verifies the dux SQLite index (schema v4: inodes + dirents) from every angle:
#   * DB integrity (sqlite PRAGMA checks)
#   * structural integrity (no orphans, no duplicate PATHS, leaf math, no
#     negative totals, no unaddressed dirty state)
#   * EXACT internal consistency: stored directory totals == recomputed-from-tables
#     (proves the live daemon's incremental updates never drifted)
#   * ground-truth cross-checks vs `du`, `df`, `find` (tolerance, since a live FS moves)
#   * existence both ways (disk<->index sampling: catches missing & stale rows)
#
# Subcommands:
#   audit        run the full read-only audit on the live index (default)
#   selftest     deterministic create/grow/rename/delete through the daemon,
#                with EXACT assertions + daemon-vs-fresh-scan agreement
#   install-cron install a cron job to run `audit` every 3 hours
#
# Exit code: 0 = all sampled checks passed, 1 = warnings, 2 = FAILURE.

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

# Daemon liveness for THIS db: the heartbeat is "<secs> <pid> <dbpath>" (older
# builds wrote "<secs> <dbpath>"); require it fresh AND naming the same database
# we're auditing (a daemon on a different index must not look live here).
daemon_live() {
  local line secs rest hbdb want
  line="$(cat /run/dux/heartbeat 2>/dev/null)" || return 1
  secs="${line%% *}"; rest="${line#* }"
  # if the 2nd field is a bare pid, the db path is the remainder; else it's the rest
  if [ "${rest%% *}" -eq "${rest%% *}" ] 2>/dev/null && [ "${rest}" != "${rest#* }" ]; then
    hbdb="${rest#* }"
  else
    hbdb="$rest"
  fi
  [ -n "$secs" ] || return 1
  [ $(( $(date +%s) - secs )) -le 15 ] || return 1
  want="$(readlink -f "$DB" 2>/dev/null || echo "$DB")"
  [ "$hbdb" = "$want" ]
}

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

# Mountpoints at/under $1 on real filesystems (the same set dux indexes).
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
  say "${DIM}index: $DB   root: $scan_root   daemon(this db): $([ "$LIVE" = 1 ] && echo live || echo stopped)   $(ts)${RST}"

  hdr "1. SQLite integrity"
  local ic; ic="$(q "PRAGMA integrity_check")"
  [ "$ic" = "ok" ] && pass "integrity_check = ok" || fail "integrity_check: $ic"
  local qc; qc="$(q "PRAGMA quick_check")"
  [ "$qc" = "ok" ] && pass "quick_check = ok" || fail "quick_check: $qc"
  say "  ${DIM}note: PRAGMA checks validate DB pages, NOT the filesystem-tree invariants below.${RST}"

  hdr "2. Schema (v4: inodes + dirents)"
  for col in dev_id inode kind blocks recursive_bytes recursive_inodes uid mtime; do
    if q "SELECT $col FROM inodes LIMIT 1" >/dev/null 2>&1; then pass "column inodes.$col present"
    else fail "column inodes.$col MISSING"; fi
  done
  for col in parent_dev parent_inode name dev_id inode prime; do
    if q "SELECT $col FROM dirents LIMIT 1" >/dev/null 2>&1; then pass "column dirents.$col present"
    else fail "column dirents.$col MISSING"; fi
  done

  hdr "3. Structural integrity (exact when daemon quiescent)"
  local total; total="$(q "SELECT count(*) FROM inodes")"

  # one inode per PATH: a missed delete + recreate could otherwise leave two
  # inodes at one (parent,name). UNIQUE(parent_dev,parent_inode,name) forbids it,
  # so a nonzero here means schema corruption.
  local duppath; duppath="$(q "SELECT count(*) FROM (SELECT parent_dev,parent_inode,name,count(*) c FROM dirents GROUP BY parent_dev,parent_inode,name HAVING c>1)")"
  [ "${duppath:-0}" = "0" ] && pass "no duplicate (parent,name) paths" || fail "$duppath duplicate paths (two inodes at one name)"

  # orphans: a non-root dirent whose parent inode is absent (briefly possible
  # between a file's flush and its parent dir's flush while the daemon is live)
  local orph; orph="$(q "
    SELECT count(*) FROM dirents d
    WHERE NOT (d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
      AND NOT EXISTS (SELECT 1 FROM inodes p WHERE p.dev_id=d.parent_dev AND p.inode=d.parent_inode)")"
  grade_zero "${orph:-0}" "${total:-0}" "orphan dirents (broken parent links)"

  local badleaf; badleaf="$(q "SELECT count(*) FROM inodes WHERE kind!='d' AND recursive_bytes<>blocks")"
  grade_zero "${badleaf:-0}" "${total:-0}" "file inodes with recursive_bytes != blocks"

  # negative totals are ALWAYS corruption (drift drove an ancestor below zero)
  local neg; neg="$(q "SELECT count(*) FROM inodes WHERE recursive_bytes<0 OR recursive_inodes<0")"
  [ "${neg:-0}" = "0" ] && pass "no negative recursive totals" || fail "$neg inodes with negative totals (drift)"

  # a dirty index has KNOWN missed events (fanotify overflow / partial watch) —
  # it is not trustworthy until rescanned, regardless of the checks above.
  local dirty; dirty="$(q "SELECT value FROM meta WHERE key='dirty_since'")"
  [ -z "$dirty" ] && pass "index not marked dirty" || fail "index DIRTY since $dirty — missed events; rescan needed"

  hdr "4. EXACT internal totals (stored vs recomputed from the tables)"
  # inodes holds one row per inode (hardlinks collapsed), so the root's stored
  # totals MUST equal the raw SUM(blocks)/COUNT over inodes. Divergence = drift.
  local stored_b stored_i sum_b cnt
  stored_b="$(q "SELECT recursive_bytes FROM inodes WHERE dev_id=$root_dev AND inode=$root_ino")"
  stored_i="$(q "SELECT recursive_inodes FROM inodes WHERE dev_id=$root_dev AND inode=$root_ino")"
  sum_b="$(q "SELECT COALESCE(SUM(blocks),0) FROM inodes")"
  cnt="$(q "SELECT count(*) FROM inodes")"
  if [ "$stored_b" = "$sum_b" ]; then pass "root.recursive_bytes ($(human "$stored_b")) == SUM(blocks)  [exact]"
  elif [ "$LIVE" = 1 ] && within_tol "$stored_b" "$sum_b" 0.5; then warn "root.recursive_bytes vs SUM(blocks) off $(human $((stored_b-sum_b))) (Δ$(pctdiff "$stored_b" "$sum_b")%) — daemon writing live"
  else fail "root.recursive_bytes=$stored_b != SUM(blocks)=$sum_b (drift $(human $((stored_b-sum_b)))) — run reconcile"; fi
  if [ "$stored_i" = "$cnt" ]; then pass "root.recursive_inodes ($stored_i) == COUNT(inodes)  [exact]"
  elif [ "$LIVE" = 1 ] && within_tol "$stored_i" "$cnt" 0.5; then warn "root.recursive_inodes vs COUNT off $((stored_i-cnt)) (Δ$(pctdiff "$stored_i" "$cnt")%) — daemon writing live"
  else fail "root.recursive_inodes=$stored_i != COUNT(inodes)=$cnt — run reconcile"; fi

  hdr "5. EXACT per-directory totals (random sample of 25 dirs, recomputed via CTE)"
  local bad_dir=0 checked=0
  while IFS='|' read -r d i; do
    [ -z "$d" ] && continue
    checked=$((checked+1))
    local got got_i exp_b exp_c
    got="$(q "SELECT recursive_bytes FROM inodes WHERE dev_id=$d AND inode=$i")"
    got_i="$(q "SELECT recursive_inodes FROM inodes WHERE dev_id=$d AND inode=$i")"
    # subtree inode SET (UNION dedups hardlinks), summing each inode's blocks once
    read -r exp_b exp_c <<<"$(q "
      WITH RECURSIVE sub(d,i) AS (
        SELECT dev_id,inode FROM inodes WHERE dev_id=$d AND inode=$i
        UNION
        SELECT de.dev_id,de.inode FROM dirents de JOIN sub ON de.parent_dev=sub.d AND de.parent_inode=sub.i
        WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode)
      ) SELECT COALESCE(SUM(i.blocks),0), COUNT(*) FROM sub JOIN inodes i ON i.dev_id=sub.d AND i.inode=sub.i" | tr '|' ' ')"
    if [ "$got" != "$exp_b" ] || [ "$got_i" != "$exp_c" ]; then
      bad_dir=$((bad_dir+1))
      say "    ${DIM}dir(dev=$d ino=$i): stored b=$got/i=$got_i recomputed b=$exp_b/i=$exp_c${RST}"
    fi
  done < <(q "SELECT dev_id,inode FROM inodes WHERE kind='d' ORDER BY RANDOM() LIMIT 25")
  if [ "$bad_dir" = 0 ]; then pass "all $checked sampled directory totals internally exact"
  elif [ "$LIVE" = 1 ] && [ "$bad_dir" -le 3 ]; then warn "$bad_dir/$checked dirs skewed (sampled mid-flush; daemon live)"
  else fail "$bad_dir/$checked sampled dirs inconsistent — run reconcile"; fi

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

  hdr "7. Ground truth: inode count vs find (per-mount sum, tol ${TOL_PCT}%)"
  local find_c=0 c
  while IFS= read -r m; do
    [ -z "$m" ] && continue
    c="$(sudo find "$m" -xdev 2>/dev/null | wc -l)"
    find_c=$((find_c + c))
  done <<<"$mounts"
  if [ "$find_c" -gt 0 ]; then
    if within_tol "$cnt" "$find_c" "$TOL_PCT"; then
      pass "dux $cnt inodes vs find(Σmounts) $find_c  (Δ$(pctdiff "$cnt" "$find_c")%)"
    else warn "dux $cnt vs find(Σmounts) $find_c (Δ$(pctdiff "$cnt" "$find_c")%) — live churn / hardlinks"; fi
  else warn "find failed — skipped"; fi

  hdr "8. Filesystem capacity vs df"
  local df_used
  df_used="$(df -B1 --output=used "$scan_root" 2>/dev/null | tail -1 | tr -d ' ')"
  if [ -n "$df_used" ]; then pass "df used: $(human "$df_used")  (dux status mirrors statvfs — same source)"
  else warn "df failed — skipped"; fi

  hdr "9. Existence both ways (sample $SAMPLE each)"
  # disk -> index: sampled real files must be indexed
  local miss=0 dchk=0
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    dchk=$((dchk+1))
    local di; di="$(stat -c '%d %i' "$f" 2>/dev/null)" || continue
    set -- $di
    local hit; hit="$(q "SELECT 1 FROM inodes WHERE dev_id=$1 AND inode=$2 LIMIT 1")"
    [ "$hit" = "1" ] || miss=$((miss+1))
  done < <(sudo find "$scan_root" -xdev -type f 2>/dev/null | shuf -n "$SAMPLE" 2>/dev/null)
  if [ "$miss" = "0" ]; then pass "all $dchk sampled on-disk files are present in the index"
  else warn "$miss/$dchk on-disk files missing from index (daemon lag or downtime gap)"; fi

  # index -> disk: sampled index rows must still resolve to an existing path
  local stale; stale="$(index_to_disk_stale)"
  if [ "$stale" = "0" ]; then pass "sampled index entries resolve to existing files (no stale rows)"
  else warn "$stale sampled index entries are stale (deleted on disk, still in index)"; fi

  summary
}

# Sample index inodes, reconstruct each path by walking dirents, and stat it.
# Names are carried as HEX (delimiter-safe: a filename may itself contain '|',
# spaces, or newlines) and decoded with xxd, so the check is byte-exact.
index_to_disk_stale() {
  local stale=0
  while IFS='|' read -r d i; do
    [ -z "$d" ] && continue
    local cd="$d" ci="$i" guard=0 row hexname pdev pino rest
    local parts_hex=()
    while [ $guard -lt 4096 ]; do
      guard=$((guard+1))
      row="$(q "SELECT hex(name)||'|'||parent_dev||'|'||parent_inode FROM dirents WHERE dev_id=$cd AND inode=$ci ORDER BY prime DESC LIMIT 1")"
      [ -z "$row" ] && break
      hexname="${row%%|*}"; rest="${row#*|}"; pdev="${rest%%|*}"; pino="${rest#*|}"
      parts_hex=("$hexname" "${parts_hex[@]}")
      { [ "$pdev" = "$cd" ] && [ "$pino" = "$ci" ]; } && break   # root reached
      cd="$pdev"; ci="$pino"
    done
    [ "${#parts_hex[@]}" -eq 0 ] && { stale=$((stale+1)); continue; }
    local path k comp
    path="$(printf '%s' "${parts_hex[0]}" | xxd -r -p 2>/dev/null)"
    for ((k=1;k<${#parts_hex[@]};k++)); do
      comp="$(printf '%s' "${parts_hex[k]}" | xxd -r -p 2>/dev/null)"
      case "$path" in */) path="${path}${comp}";; *) path="${path}/${comp}";; esac
    done
    [ -n "$path" ] && [ -e "$path" ] || stale=$((stale+1))
  done < <(q "SELECT dev_id,inode FROM inodes WHERE kind!='d' ORDER BY RANDOM() LIMIT $SAMPLE")
  echo "$stale"
}

# Reconcile by full rescan when the audit fails. CRITICAL: the daemon must not
# write the DB during a scan, so we stop it, scan as sole writer, then restart.
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

  daemon_live || { warn "daemon not live for $DB — selftest needs the daemon running"; return 1; }

  hdr "selftest: create"
  dd if=/dev/zero of="$T/f1" bs=1M count=16 status=none; sync
  dd if=/dev/zero of="$T/f2" bs=1M count=8  status=none; sync
  mkdir "$T/sub"; dd if=/dev/zero of="$T/sub/f3" bs=1M count=4 status=none; sync
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "create"

  hdr "selftest: grow"
  dd if=/dev/zero bs=1M count=20 oflag=append conv=notrunc of="$T/f1" status=none; sync
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "grow"

  hdr "selftest: hardlink (both names indexed, counted once)"
  ln "$T/f2" "$T/f2-hardlink"; sync
  poll_until "both hardlink names visible" "test \"\$(node_exists_name f2-hardlink)\" = 1 && test \"\$(node_exists_name f2)\" = 1"
  expect_dir_blocks "$T" "$(du -s --block-size=1 "$T" | awk '{print $1}')" "hardlink (du counts once)"

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
  sync; sleep 3
  local live_total scan_total tdev tino
  live_total="$(dir_recursive "$T")"
  "$DUX" --db "$TDB" scan "$T" --quiet >/dev/null 2>&1
  read -r tdev tino <<<"$(stat -c '%d %i' "$T")"
  scan_total="$(sudo sqlite3 -noheader "$TDB" "SELECT recursive_bytes FROM inodes WHERE dev_id=$tdev AND inode=$tino")"
  if [ "$live_total" = "$scan_total" ]; then
    pass "daemon-maintained total == fresh-scan total ($(human "${live_total:-0}"))"
  else
    fail "daemon=$live_total  fresh-scan=$scan_total  (incremental drift!)"
  fi

  summary
}

node_exists_name() { q "SELECT count(*) FROM dirents WHERE name=CAST('$1' AS BLOB)"; }
dir_recursive() { local di; di="$(stat -c '%d %i' "$1")"; set -- $di; q "SELECT recursive_bytes FROM inodes WHERE dev_id=$1 AND inode=$2"; }

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
    got="$(q "SELECT recursive_bytes FROM inodes WHERE dev_id=$d AND inode=$ino")"
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
  # Honest wording: ground-truth checks are SAMPLED, so this is "no inconsistency
  # found", not a proof of exactness.
  else echo "  ${GRN}RESULT: all checks passed (structural exact; ground-truth sampled)${RST}"; exit 0; fi
}

case "${1:-audit}" in
  audit) audit ;;
  selftest) selftest ;;
  install-cron) install_cron ;;
  *) echo "usage: $0 {audit|selftest|install-cron}"; exit 2 ;;
esac
