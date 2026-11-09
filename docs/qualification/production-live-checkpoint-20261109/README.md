# Production and unsafe-code checkpoint — 2026-11-09

## Production VPS1 read-only observation

At 18:02:28 UTC the host was still running legacy Dux `0.6.3`
(`e0cc915f8f37`), PID 1073891, with four systemd restarts. Status showed the
last completed scan was 364,366 seconds old (about 4.2 days), 636 kernel
fanotify overflow notifications, 2,518,862 dropped updates, 26 pending updates,
and a dirty index. The indexed database was about 3.21 GB on disk. Root had
64,322,830,336 bytes available. The service cgroup reported no OOM or OOM-kill
events. This is a read-only snapshot; no production service or database state
was changed.

Raw command results are in `vps1-readonly-status-20261109T1802Z.json`.

## Current unsafe-block documentation lint

On the current source, `cargo clippy --locked --bin dux --all-features -- -W clippy::undocumented_unsafe_blocks`
completed with **47 production binary warnings**. Promoting that lint to an
error in the all-target build (`cargo clippy --locked --all-targets --all-features -- -D warnings -W clippy::undocumented_unsafe_blocks`)
failed: 83 diagnostics were emitted, including test-target sites and duplicate
compilation contexts. These diagnostics identify missing Clippy-recognized
safety comments; they do **not** demonstrate that those blocks are unsound.
The output has not been counted as a passing check. The production-only and
all-target logs are retained here. A manual unsafe-block audit and appropriate
safety documentation remain open; do not silence the lint globally.

## Unsafe-block review progress — 2026-11-09 18:15 UTC

Reviewed each production Clippy unsafe site and added a local `SAFETY:` note
that records the relevant pointer, descriptor, signal, or syscall preconditions.
Test-only unsafe sites were also documented. An equivalent `PR_SET_SECUREBITS`
condition was rewritten as a separate checked call so its safety note is
attached directly to the FFI operation; its behavior is unchanged.

The release all-features suite passed **332/332**, including both ignored
profiles: the ext4 stale-parent integration and 100,000-entry scale smoke. The
scale run measured a 3.505s initial build and 3.243s unchanged reconciliation;
the database remained 29,868,032 bytes. The suite ran in a private mount
namespace with loopback ext4 for the handle/migration fixture and separate
private tmpfs scratch for scale output. A preceding exploratory run put every
test temp directory on tmpfs and failed only the v6-to-v7 migration test; the
same migration test passed when rerun on private loopback ext4, and the complete
ext4-configured run passed. The mismatch run's full stderr was not saved, so its
precise assertion is not claimed here.

All-target/all-feature Clippy passed with both general warnings and
`clippy::undocumented_unsafe_blocks` denied. Formatting and diff checks passed.
The production unsafe-comment warning count fell from 47 to zero; the all-target
unsafe lint also reports zero. Logs are retained in `rust-release-all-332-isolated-ext4.log`,
`clippy-all-targets-deny-unsafe.log`, and `fmt-check.log`.

This is a safety-documentation and reviewability improvement, not proof that
all unsafe blocks are correct or a substitute for memory-safety analysis. The
VPS1 live-risk snapshot above is unchanged; the production daemon remains on
legacy 0.6.3. Existing coverage remains below 100%, and branch coverage is not
available.

## Current-source release test and coverage refresh — 2026-11-09 18:18 UTC

After the safety-note edits, the optimized all-features release suite passed
**332/332**, with ignored tests included, in 14.12 seconds of test execution.
This includes the stale-parent integration on loopback ext4 and the 100,000-file
scale smoke. The scale smoke measured 3.505s initial build and 3.243s unchanged
reconciliation; DB size stayed 29,868,032 bytes. The full log is
`rust-release-all-332-isolated-ext4.log`.

Fresh LLVM coverage on the same source and 332-test profile reports **73.73%
line, 72.67% region, and 76.94% function** coverage. Branch counters are not
provided by this instrumentation. Lowest line coverage remains `tui.rs` 39.51%,
`main.rs` 46.43%, and `containers.rs` 67.25%. The isolated coverage-profile
scale smoke measured 8.517s baseline and 8.413s unchanged reconciliation, with
29,798,400-byte DB unchanged. Coverage command and report are in
`llvm-cov-all-332-isolated.log`.

The exact current source passes all-target/all-feature Clippy with
`-D warnings -D clippy::undocumented_unsafe_blocks`, formatting, and diff
checks. This source supersedes r122's source digest; build a new package before
any deployment. The separate current-source package install/upgrade/rollback
qualification and 24-hour canary remain open.

### Production VPS1 follow-up — 2026-11-09 18:19 UTC

The read-only counters remained at 636 kernel overflows and 2,518,862 dropped
updates; the service remained PID 1073891 with four restarts. The last scan age
was 365,390 seconds (about 4.23 days), the dirty reason remained queue overflow,
and pending updates rose to 97. The DB occupied 3,213,430,784 bytes, with a
128 MiB WAL and 63,422,918,656 bytes available on `/`. No memory cgroup OOM or
OOM-kill events were recorded. This follow-up confirms the index remains stale
and the legacy event backlog is still accumulating; it is a read-only sample.
Raw data: `vps1-readonly-checkpoint-20261109T1819Z.json`.

## Live follow-up — 2026-11-09 18:30 UTC (read-only)

Fresh local VPS1 and remote VPS2 snapshots are retained in
`vps1-readonly-followup-20261109T183047Z.json` and
`vps2-readonly-followup-20261109T183047Z.json`.

- VPS1 remains on the legacy daemon (PID 1073891, four restarts), dirty from
  fanotify overflow. Counters remained at 636 kernel overflow notifications
  and 2,518,862 dropped events; the last full scan was about 4.24 days old.
  Pending updates were 1,213. This is read-only evidence; production was not
  changed.
- VPS2 remains on r121 (PID 2561224, zero restarts since its 13:59 UTC start),
  with zero kernel overflows and zero dropped updates in this snapshot. However,
  systemd reports `reloading`, Dux reports a dirty in-progress observation,
  and its last completed scan was 748 seconds old while the next scan ran.
  `caught_up=false` and the 4.52-hour `behind_seconds` value are not scan-first
  freshness indicators: scan-first mode intentionally does not apply event
  batches and reports freshness using `last_scan_age_seconds`. This r121 status
  output does not include the newer explicit `behind_seconds_applicable:false`
  field, so consumers must use its `scan_first` flag. The current source emits
  the explicit applicability field.
- Journal analysis through 18:34 UTC found 19 scan starts and 18 completed
  observations, each followed by a successful accounting check. Starts were
  900–903 seconds apart; full scans took 210–394 seconds (mean 275 seconds),
  and accounting checks took 49–104 seconds (mean 62 seconds). One admission
  was briefly deferred under elevated pressure at 18:29:27, then the scan
  started at 18:29:30. This is positive evidence that r121 can maintain its
  15-minute observation schedule on VPS2 for this 4.5-hour sample without
  observed queue loss. It does not prove path-level exactness under concurrent
  changes, a 24-hour soak, or r123 runtime behavior. Machine-readable timings
  and scan summaries are in `vps2-scan-cycles-20261109T1832Z.json`.

### VPS2 follow-up — 2026-11-09 18:38 UTC

The scan active at the earlier checkpoint completed at 18:35:53 UTC in 383.320
seconds; independent accounting passed at 18:37:01 in 67.941 seconds. At 18:38,
VPS2 was active (PID 2561224), with zero restarts, zero kernel overflows, zero
dropped events, no pending updates, and `last_scan_age_seconds=123`. The live
24-hour monitor's sample 55 passed with no errors, SQLite quick-check `ok`, and
the indexed path-graph check passing. Its freshness ceiling is 1,200 seconds.
The canary still has only about 4.5 hours of elapsed monitoring, so the soak is
not complete. Full status, sample summary, and all 19 scan/accounting timings
are in `vps2-live-followup-20261109T1837Z.json`.

The r121 canary has not been restarted or upgraded. The 24-hour soak remains
open; the current candidate r123 still lacks real package lifecycle and runtime
qualification. Treat these measurements as encouraging but incomplete live
evidence, not a release pass.

### VPS2 and package-lifecycle follow-up — 2026-11-09 18:47 UTC

Canary sample 56 passed at 18:42:53 UTC: no monitor errors, SQLite quick-check
`ok`, indexed path graph passed, zero restarts/overflows/drops, and last
completed observation age 420 seconds against the 1,200-second monitor limit.
At the 18:47:20 read-only status capture, the next observation was in progress,
age 683 seconds, with zero pending updates or observed overflows. The current
service PID remains 2561224 with zero restarts. See
`vps2-live-followup-20261109T1846Z.json`.

An isolated r121-to-r123 DEB lifecycle qualification was prepared but not
started: the VM manager refused twice before QEMU launch because host Go test
and compiler processes were active; Go tests were still being launched during
the subsequent check. No package or service was changed. Temporary VM inputs
were removed after confirming the VM unit was inactive. The full package
upgrade/rollback/removal/clean-install gate therefore remains open.

### Latest live roll-up — 2026-11-09 18:49 UTC

The monitor reached **57 consecutive passing five-minute samples** between
14:07:48 and 18:48:06 UTC. All samples passed SQLite quick-check and path-graph
validation; none reported errors, kernel overflows, dropped events, service
restarts, or OOM kills. Observed completed-scan ages ranged from 4 to 936
seconds (median 453) against the monitor's 1,200-second limit. At the 18:49:16
snapshot, r121 was active with zero restarts, zero pending updates, and a 20-
second completed-scan age. This is positive, bounded live evidence for r121;
the 24-hour soak is only about 4 hours 41 minutes in and still must finish.

VPS1 remains on its legacy daemon with 636 lifetime overflow notifications,
2,518,862 dropped events, and a last full scan age of 367,192 seconds (about
4.25 days). Pending updates were 49 at this sample; that value fluctuates and
does not clear the stale-index/overflow condition. The full two-host snapshot
and monitor roll-up are in `live-followup-20261109T1849Z.json`.

### r123 lifecycle retry — 2026-11-09 18:57 UTC

Host Go test activity had cleared, so the isolated lifecycle attempt was retried
using the VM runner. The Ubuntu cloud image transfer stalled at 96 KiB from the
primary endpoint, then at 192 KiB from the US mirror after nearly three minutes;
small checksum/HEAD requests worked. The transfer was stopped after verifying
the QEMU unit remained inactive. No VM started, no Dux package or service was
changed, and the partial image and empty staging directories were removed.
Package lifecycle qualification is still unrun. A verified distro image must
be staged and the VM runner's host admission checks must pass before retrying.

### Canary monitor follow-up — 2026-11-09 18:58 UTC

The read-only canary monitor reached **59 passing five-minute samples** through
18:58:45 UTC. All 59 passed SQLite and indexed path-graph checks; there were no
monitor errors, service restarts, kernel overflows, dropped events, or OOM
kills. Completed-scan ages ranged from 4 to 936 seconds (median 453) against the
1,200-second limit. At capture, r121 was active, no scan was running, and the
last completed scan was 596 seconds old. This remains under six hours of the
required 24-hour soak. See `vps2-live-followup-20261109T1857Z.json`.

### VPS2 observation follow-up — 2026-11-09 19:05 UTC (read-only)

A subsequent full observation started at 18:59:31 and completed at 19:04:25
UTC, walking 1,576,330 files and 187,628 directories in about 4m54s with zero
coverage issues. Independent accounting verification passed at 19:05:38 after
72.1 seconds. At 19:05:55, r121 remained active with zero systemd restarts,
kernel overflow notifications, kernel-reported drops, userspace drops, or
pending updates; the last completed observation was 90 seconds old. The status
is dirty because filesystem changes await the next full observation, which is
expected for scan-first mode. This is a read-only observation, not a formal
five-minute canary-monitor sample: no SQLite/path-graph checks were run in this
capture. It adds one successful scan/accounting cycle but does not complete the
24-hour soak or qualify r123. Raw status, selected journal evidence, and the
assessment are in `vps2-live-followup-20261109T1905Z.json`.

### VPS2 read-only database integrity check — 2026-11-09 19:10 UTC

An additional independent read-only SQLite check passed against the same
completed r121 scan generation: `PRAGMA quick_check` returned `ok`; schema was
7; all 1,763,958 path rows had one root, valid directory parents, exactly one
prime path per inode identity, matching directory-total rows, and a root entry
count matching the path count. The SQL checks took 5.199 seconds. At capture,
systemd was active with zero restarts, scan age was 369 seconds, and overflow,
kernel/user drops, and pending-update counters were all zero. This duplicates
the database-integrity portion of the monitor but omits its binary/unit hash,
cgroup, journal-window, history, and independent filesystem-walk checks; it is
not counted as a formal canary sample. Raw results are in
`vps2-readonly-db-check-20261109T1909Z.json`.

### r123 package-lifecycle VM admission retry — 2026-11-09 19:18 UTC

The official Ubuntu 24.04 amd64 cloud image was downloaded from Canonical's
`releases/noble/release` endpoint and matched the published SHA-256
`04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94`.
The verified image is staged in `/dev/shm` with a local image manifest; r121
and r123 DEBs plus the lifecycle harness are staged read-only in a separate
tmpfs share. A retry of the guarded `ubuntu24-lifecycle-ephemeral` VM start was
refused before QEMU launch because the host had an active Go build/test and
load1 above the runner's limit of 10. A later 19:18 admission snapshot still
found a Go race test/compiler and load1 17.27. The VM unit is inactive; no
guest, Dux service, or canary state was changed. Thus this retry is an
admission refusal, not a package-lifecycle test result. Retry only after the
runner's repeated host-pressure/workload checks pass. Captured admission,
image, and package hashes are in `package-lifecycle-admission-20261109.json`.

### VPS2 observation during host build load — 2026-11-09 19:23 UTC

While the local VM admission gate remained closed for concurrent Go tests,
VPS2 completed its next scan-first observation. It started at 19:14:31 and
finished at 19:20:39 UTC (6m08s), walking 1,579,003 files and 187,958
directories with zero coverage issues. Independent accounting verification
passed at 19:22:00 in 81.119 seconds. At 19:23, r121 was active with zero
restarts, kernel overflows, kernel/user event drops, or pending updates; scan
age was 142 seconds. Dirty status means changes await the next observation.
This is one observed scan/accounting cycle, not a formal canary-monitor sample
and not r123 runtime qualification. Evidence is in
`vps2-live-followup-20261109T1923Z.json`.
