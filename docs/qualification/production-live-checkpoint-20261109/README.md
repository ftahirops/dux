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

The r121 canary has not been restarted or upgraded. The 24-hour soak remains
open; the current candidate r123 still lacks real package lifecycle and runtime
qualification. Treat these measurements as encouraging but incomplete live
evidence, not a release pass.
