# r132 post-observation hint runtime qualification — 2026-11-10

This qualification exercises the scan-first race where a filesystem hint is
counted after a completed walk but before the walk's pending accounting check
can clear dirty state. The candidate was built from the current working tree;
it was not packaged, installed, or used to replace the VPS2 service.

## Change under test

The scan-first fanotify reader drains and counts records without resolving
paths. Its event and overflow counters previously marked the index uncertain,
but did not invalidate `pending_reconciliation_check`. An accounting check for
the older walk could therefore clear the dirty marker after a newer event had
already been observed.

The watcher now invalidates a pending reconciliation accounting check when
either counter advances after the observation. It cancels the verifier when
possible and changes the reason to `filesystem changes await the next full
observation`, so a successful check of the old snapshot cannot clear dirty
state.

## Build and test evidence

- Production build: `cargo build --locked --release --bin dux`
- Fault-injection build: `cargo build --locked --release --features fault-injection --bin dux`
- Both report working-tree source SHA-256
  `9a437f3f77b10f46d13480720004dea18cde1b90403422669079d06ddc848220`.
- Production binary SHA-256:
  `cb298771272323ee08fb72e2c93c6792c6e51305b29a84d0953dbec3f9844b85`.
- Fault-injection binary SHA-256:
  `8933ada608c51fad63ccb01d5132d14a73834e80b5bddd9a172f54564c19c294`.
- `--version` identified the executables as `[production]` and
  `[fault-injection]`, respectively.
- Rust suite: **341 passed, 0 failed, 2 ignored** with all features.
- Python readiness harness: **61 passed**.
- Strict Clippy, `cargo fmt --check`, Python byte compilation, and
  `git diff --check` passed.
- LLVM coverage: **72.61% lines** (19,075/26,270), **71.44% regions**
  (30,309/42,428), and **76.19% functions** (1,331/1,747). LLVM exposed no
  branch counters.

## VPS2 runtime result

The `postscan-hint` readiness suite passed on the marked KVM guest
`192.168.5.106`, kernel `6.8.0-147-generic`. It ran in a transient systemd unit
with a private mount namespace, 25% CPU quota, 4 GiB memory limit, and a
disposable 768 MiB ext4 loop image backed by `/dev/shm`. The fixture had 256
seed files plus its sparse-file, hardlink, and non-UTF-8 cases. The index was
kept on tmpfs outside the observed ext4 filesystem to avoid database writes
becoming unrelated filesystem hints.

The fault-injection build paused immediately after the accounting check
started. The test created a file after the walk, then resumed the daemon. A
test-only 2-second accounting delay allowed the reader to drain the queued
event before the old verification result could be consumed. The assertions
proved that:

1. the post-observation event changed the dirty reason and kept `dirty_since`;
2. the old accounting result did not clear dirty status;
3. a later full observation indexed the new path;
4. the final indexed filesystem matched the fixture exactly and passed
   `integrity`.

The suite took 5.235 seconds; the systemd unit ran for 6.536 seconds. The
installed r121 service remained active with zero restarts. The fixture was
removed by the harness. Machine-readable evidence is in
[`passed-report.json`](r132-postscan-hint-runtime-20261110/passed-report.json);
coverage output is in
[`llvm-cov-summary.json`](r132-postscan-hint-runtime-20261110/llvm-cov-summary.json).

Four exploratory runtime attempts preceded the pass. The first two did not
converge with the test database on the observed ext4 image; moving the
database to tmpfs removed that noise. The next two timed out before the
asserted dirty-reason transition. Reviewing the scan-first path showed that its
counter-only reader did not call the pending-check invalidation helper. The
fourth attempt's intermediate report was overwritten by the final run; the
other three reports are preserved in this evidence directory. The final test
isolates the database and adds a fault-injection-only delay so the queued event
is processed before the accounting result. No delay is present in the
production binary.

## Limits and remaining gates

This is one deterministic race regression on ext4 and one kernel. It is not a
long-duration production canary, overload throughput test, crash/power-loss
test, cross-filesystem matrix, package upgrade/rollback test, or large-index
performance qualification. The 72-hour soak, 24-hour canary, package and
privilege review, and broader filesystem/kernel matrix remain open. This
result does not constitute production-readiness sign-off.
