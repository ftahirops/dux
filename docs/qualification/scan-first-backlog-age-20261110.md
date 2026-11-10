# Scan-first backlog metric misclassification — 2026-11-10

## Runtime evidence

Read-only VPS2 journal samples show two successful observations while the
legacy r121 daemon remained active:

| Observation | Walk result | Accounting check |
|---|---|---|
| Completed 00:35:22 UTC | 1,636,462 files; 194,734 directories; 1,831,196 paths; 0 coverage issues; 3 identities revalidated; 0 unresolved | Passed at 00:36:25 in 62.689 s |
| Started 00:41:55, completed 00:47:22 UTC | 1,639,365 files; 195,088 directories; 1,834,453 paths; 0 coverage issues; 2 identities revalidated; 0 unresolved | Passed at 00:49:13 in 111.369 s |

The journal also emitted `index backlog persists — 38876s since last caught
up` immediately after the second successful walk. That time is the event-apply
drain clock. In scan-first mode it is not the filesystem observation age: the
daemon records fanotify events as hints and reconciles with full observations.
The machine status already labels `behind_seconds_applicable=false` for
scan-first, and the operator should use `last_scan_age_seconds` plus dirty
reason and scan progress instead. The old r121 CLI did not expose the
applicability field, which made this stale counter easy to misread as a
10.8-hour freshness failure.

At 00:50 UTC the service was active, PID 2561224, with zero restarts, kernel
overflows, or dropped updates. Its last scan was 85 seconds old and its dirty
reason was `filesystem changes await the next full observation`. This brief
sample is not a canary pass and does not prove a snapshot is atomic under
continuous mutation. The raw journal excerpt is in
[`scan-first-backlog-age-20261110-vps2-journal.txt`](scan-first-backlog-age-20261110-vps2-journal.txt);
two raw status captures are in
[`release-candidate-r125-20261110`](release-candidate-r125-20261110/README.md).

## Code correction

The watcher previously derived `throttled_since` from `last_caught_up` in both
event-apply and scan-first modes. It now classifies this as throttled backlog
only when event application is enabled and no full scan is active. Human and
JSON status suppress any persisted event-mode throttle marker while a live
scan-first daemon is reporting, so old metadata cannot be presented as scan
freshness. The regular scan-first dirty reason and observation timestamp
remain visible.

This corrects misleading diagnostics; it does not change event capture,
observation cadence, or index consistency. The r125 package was built before
this fix. Release 126 was subsequently built from commit
`2a884a8ed6d5a63a5019322241de418354f560dc` and includes it; its guarded package
lifecycle and canary gates remain open. See
[`release-candidate-r126-20261110`](release-candidate-r126-20261110/README.md).

## Tests and coverage

- New predicate regression: scan-first mode never reports the event-apply age
  as throttled backlog.
- Extended machine/human status regression: a stale `throttled_since` marker is
  hidden in scan-first status while `behind_seconds_applicable` remains false.
- The initial inclusive run without a scratch override produced 337 passes and
  one expected test-safety rejection: the 100,000-entry scale test refused to
  create load on a real filesystem while Dux was live.
- Rerun with
  `DUX_RECONCILE_SCALE_TMPDIR=/dev/shm cargo test --locked --all-features --bin dux -- --include-ignored`:
  **338 passed, 0 failed, 0 ignored**, 217.40 seconds. The 100,000-entry fixture
  ran on tmpfs; this is a scale smoke, not a production performance benchmark.
- Strict Clippy passed on all targets/features with warnings and undocumented
  unsafe blocks denied. Formatting and `git diff --check` passed.

No new whole-project coverage profile was collected for this correction.
Previously reported line/region/function percentages should not be presented
as a profile of this exact source revision; branch coverage remains unreported.

## Remaining release gates

- Complete the guarded install/upgrade/rollback test on the disposable Ubuntu
  guest, then compare index paths and accounting with an independent walk.
- Run a fresh uninterrupted 24-hour canary on the candidate. The previous r121
  canary remains failed, and the r126 lifecycle runner has not yet started a VM
  because the host admission guard continues to detect Go/Vitest jobs and
  pressure.
- Complete privileged fanotify success-path and capability-negative tests;
  the two recent fanotify tests cover only invalid descriptor and embedded-NUL
  mount-path failures.
- Root plus `CAP_SYS_ADMIN` remains a material security exposure; this status
  reporting fix does not reduce daemon privilege.
