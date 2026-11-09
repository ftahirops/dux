# VPS2 canary failure — 2026-11-09 23:04 UTC

## Outcome

The r121 24-hour monitor failed at sample 108 at 23:04:18 UTC. Samples 1–107
passed; the 24-hour gate is invalid and must not be reported as passed. The
monitor stopped after recording the failure. The daemon was not restarted or
modified as part of this capture.

The monitor rejected the sample because the service journal contained a scan,
accounting, overflow, or write failure. The matching scan began at 22:59:32,
walked 1,620,893 files and 192,820 directories, then reported one coverage
issue. Specifically, the staged scan contained one `(device,inode)` with
different birth-time incarnations. The inode-reuse pass refreshed two staged
paths, but the final count still found one conflict. Dux retained dirty status
and scheduled one bounded retry. The monitor's failure record and status/journal
captures are stored beside this file.

No kernel overflow/drop counter or service restart was reported at failure.
The retry was initially deferred because host pressure was elevated (load1
7.69 on 6 CPUs; CPU PSI 19.62%). A subsequent SSH attempt initially failed
because this host's SSH configuration directs `192.168.5.106` to port 2222;
connecting explicitly to port 22 with the existing root key succeeds. The
recovery scan was active at the 23:15:58 UTC follow-up. Its completion and
independent verification are recorded below. Do not call this a passing soak:
recovery does not undo the sample-108 failure or complete a fresh 24-hour run.

## Recovery outcome — 2026-11-09 23:19 UTC

The next scheduled full observation completed at 23:16:27 UTC: 1,622,635 files
and 193,105 directories, zero coverage issues, two paths refreshed after
inode-reuse races, and zero unresolved reuse paths. Dux's independent directory
accounting verification passed at 23:17:32 UTC (64.845 seconds). The service
remained active with PID 2561224 and zero restarts; kernel overflow and
userspace-drop counters remained zero. Dux then reported the normal scan-first
dirty reason, `filesystem changes await the next full observation`, with no
automatic reconciliation pause.

A separate read-only SQLite check passed `PRAGMA quick_check` and the v7 path
graph checks (one root, valid directory parents, one prime per inode identity,
complete directory totals, and root entry count matching the 1,815,740 path
rows). The result is in `independent-db-check-2318Z.json`; raw status and
journal captures are in `recovery-status-2316Z.txt`,
`recovery-status-2318Z.txt`, and the two `recovery-journal-*.txt` files.

This shows the existing bounded retry and later full scan recovered after the
sample-108 failure. It does not prove that repeated validation passes would
have prevented the failed scan, and it does not rescue the canary: the monitor
stopped at sample 108 and the 24-hour run remains failed. The current r121
daemon was not modified.

## Source-level analysis

`scan.rs` takes one SQL snapshot of the currently conflicting inode groups,
revalidates those candidate paths once, and recounts conflicts once. A long,
busy filesystem walk can continue to race with unlink/create activity during
that validation. A path may therefore be restatted before an inode is reused
again, while another path is restatted after reuse. The final recount correctly
leaves the scan incomplete if a conflict remains. This is a plausible
explanation for “two paths refreshed, one conflict remains,” and is consistent
with earlier VPS2 inode-reuse failures. The captured evidence does not identify
which path/group remained conflicting, so this timing explanation is an
inference, not a proven root cause.

The fail-closed outcome is preferable to publishing an observation that cannot
be shown internally consistent. The availability/recovery weakness is that a
single bounded validation pass can reject an otherwise repairable scan under
ongoing churn, while the monitor treats any such scan error as a canary failure.
Simply ignoring the conflict would compromise correctness. A fix needs a
deterministic reproducer that introduces reuse between validation passes, then
must prove bounded convergence when churn stops and continued fail-closed
behavior when it does not. A bounded retry to a fixed point may help, but has
not been qualified. A bounded three-pass implementation and two deterministic
synthetic stage-mutation tests are now present in the working tree; they have
not yet compiled or run. The tests are intended to check convergence after a
validation-phase race and fail-closed behavior after persistent races. They do
not prove that VPS2's sample-108 race had the same cause.

## Required follow-up

1. Run the two new deterministic tests; inspect and fix any compile or logic
   failures before proceeding.
2. Run the all-features Rust suite, strict Clippy, formatting, and exact
   independent path/accounting comparison on a disposable ext4 filesystem;
   measure the added scan cost under a conflict case.
3. Keep the r121 canary monitor stopped. Qualify a rebuilt candidate through
   package lifecycle and runtime checks before starting a fresh uninterrupted
   24-hour canary. VPS1 remains on its legacy daemon and was not changed.
