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
7.69 on 6 CPUs; CPU PSI 19.62%). The latest accessible capture is the one
included here; SSH to VPS2 subsequently failed with public-key denial, so the
retry's final outcome is unknown. Do not restart the monitor or call this a
passing soak without first checking current service state and completing a new
24-hour run after the scan issue is understood.

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
not been implemented or qualified.

## Required follow-up

1. Regain read-only access to VPS2 and preserve the retry result and current
   canary/service state before changing anything.
2. Add a deterministic race test for reuse during the revalidation phase,
   including a case where another pass can converge and a case where continued
   replacement exhausts the bound and remains incomplete.
3. Implement only after the test demonstrates the current gap; cap extra passes
   to bound scan CPU and filesystem metadata I/O.
4. Run targeted regression tests, the all-features Rust suite, strict Clippy,
   formatting, and exact independent path/accounting comparison on a disposable
   ext4 filesystem.
5. Deploy only to the disposable VPS2 candidate after package lifecycle gates
   pass, then start a fresh uninterrupted 24-hour canary. VPS1 remains on its
   legacy daemon and was not changed by this incident capture.
