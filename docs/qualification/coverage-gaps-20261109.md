# Coverage review and critical untested paths — 2026-11-09

## Existing profile

The latest complete Rust LLVM profile is the all-feature, 332-test run recorded
in [`production-live-checkpoint-20261109`](production-live-checkpoint-20261109/README.md).
It reports 73.73% line, 72.67% region, and 76.94% function coverage. LLVM did
not provide branch counters. The saved per-file table reports:

| Module | Line coverage | Function coverage | Missed functions |
|---|---:|---:|---:|
| `watch.rs` | 75.16% | 84.62% | 74 of 481 |
| `main.rs` | 46.43% | 60.23% | 35 of 88 |
| `tui.rs` | 39.51% | 46.26% | 122 of 227 |
| `containers.rs` | 67.25% | 49.47% | 48 of 95 |

The test count is not a measure of completeness. A follow-up
`cargo llvm-cov report --text --show-missing-lines` against the saved profile
confirmed that several production fanotify syscalls are not reached by that
unit-test profile:

- `init_fanotify()` at `src/watch.rs:4583` and its `fanotify_init` call at
  `src/watch.rs:4586` have zero line hits.
- `mark_fs()`'s `fanotify_mark` call at `src/watch.rs:4615` has zero line hits.
- The event FID path's `open_by_handle_at` call at `src/watch.rs:2014` has zero
  line hits. Other probe/identity helper calls at lines 5348 and 5398 do have
  hits; they do not exercise this event-resolution call site.

Thus the Rust suite tests parser logic and related helpers, but its coverage
profile does not directly test the daemon's actual fanotify setup and event
handle-resolution path. The r121 canary and scan-cycle observations provide
separate live evidence that a real daemon starts, observes a filesystem, and
completes scans, but they are not instrumented coverage. Follow-up work below
adds deterministic invalid-descriptor tests and an isolated successful syscall
smoke; capability-negative boundaries and broader race coverage remain open.

## Required test work

1. Add deterministic tests for fanotify setup/mark error paths using invalid
   descriptors and malformed path inputs, checking errno and fail-closed
   status behavior. **Done** for the two `fanotify_mark` negative cases below.
2. Expand the privileged disposable-filesystem integration beyond the
   successful create/rename smoke below: generate delete/attribute and stale
   handle cases and compare final rows/accounting with an independent walk.
3. Add capability-negative coverage proving startup fails closed without
   `CAP_SYS_ADMIN` and handle resolution is visibly uncertain without
   `CAP_DAC_READ_SEARCH`; do not treat missing privilege as an empty event
   stream.
4. Keep separate line/region/function coverage reporting, and add branch
   coverage from a compatible instrumenter or explicit branch-focused tests.
   Do not set arbitrary global coverage thresholds before critical-path gaps
   are mapped and justified.

These were coverage gaps rather than confirmed runtime defects when this
review was written. Follow-up test coverage is recorded below; the remaining
gaps still require qualification.

## Follow-up — fanotify mark error paths (2026-11-10)

Two deterministic negative-path tests were added in `src/watch.rs`:

- `fanotify_mark_rejects_invalid_descriptor_with_ebadf` calls the real
  `fanotify_mark` wrapper with descriptor `-1` and verifies that `EBADF` is
  preserved through the `anyhow` error boundary.
- `fanotify_mark_rejects_mount_path_with_embedded_nul` verifies that a
  non-C-string mount path is rejected before entering the kernel syscall.

Both passed with:
`cargo test --locked --all-features --bin dux fanotify_mark_rejects -- --nocapture`
(2 passed, 0 failed). Strict Clippy passed on all targets/features with
warnings and undocumented unsafe blocks denied. Formatting and
`git diff --check` also passed. These tests cover malformed input and the
invalid-descriptor failure path; they do
not cover successful `fanotify_mark`, `fanotify_init`, capability-denied
startup, or actual live event delivery. The privileged disposable-filesystem
integration and capability-negative tests above remain open. The previous
whole-project coverage percentages do not include these additions, and no
branch-coverage claim is made.

## Follow-up — r126 privileged fanotify runtime smoke (2026-11-10)

The r126 binary was run as root in a private mount namespace on VPS2 with a
128 MiB ext4 loop filesystem backed by tmpfs. `strace` verified successful
`fanotify_init`, `fanotify_mark`, and event-path `open_by_handle_at` syscalls.
In compatibility event-apply mode, eight file creates and one rename reached
the index: 11 events seen/resolved, zero reported drops/overflows, renamed path
present, and old path absent. In default scan-first mode, a create was seen as
one hint and zero paths were resolved from it; a manual full observation then
published the new path. Both observations had zero coverage issues and passed
accounting checks in about 100 ms. The test did not alter the live r121 service
or its database, and all temporary mounts/files/databases were cleaned up.

Reproducible harness and detailed results:
[`r126 fanotify integration smoke`](release-candidate-r126-20261110/fanotify-integration-smoke.md).
This closes the basic success-path runtime gap for the tested kernel/filesystem,
not the broader race, deletion, attribute, capability-negative, or independent
walk cases. The LLVM profile still has no line hits for the production syscall
call sites, so this runtime evidence does not change its reported percentages.

## Follow-up — r126 missing-capability startup smoke (2026-11-10)

The exact r126 candidate binary (SHA-256
`9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b`) was
started as `nobody` with an empty capability bounding set and `no_new_privs`.
On the local Linux host, fanotify marking returned `EPERM`; Dux exited with
`no watchable filesystem` rather than staying live without coverage. The test
used a temporary `/dev/shm` directory which was automatically removed. This
closes only the missing-`CAP_SYS_ADMIN` startup smoke for that host. The
missing-`CAP_DAC_READ_SEARCH` event-handle behavior, broader filesystem race
cases, and independent-walk comparison remain open. Reproduce with
[`capability-negative-smoke.sh`](release-candidate-r126-20261110/capability-negative-smoke.sh).

A separate capability boundary test now passes twice: fanotify starts with
`CAP_SYS_ADMIN`, but event-handle resolution lacks `CAP_DAC_READ_SEARCH`; the
candidate logs the loss and marks its index dirty. Results and exact limits:
[`VPS2 observation and capability report`](production-live-followup-20261110T0224Z/README.md).
