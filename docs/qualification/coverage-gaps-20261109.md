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
completes scans, but they are not instrumented coverage and do not substitute
for deterministic regression tests of syscall failures and privilege
boundaries.

## Required test work

1. Add deterministic tests for fanotify setup/mark error paths using invalid
   descriptors and malformed path inputs, checking errno and fail-closed
   status behavior.
2. Add a privileged disposable-filesystem integration test that creates a
   real fanotify mark, generates create/delete/rename/attribute events, resolves
   live and stale handles, and compares final rows/accounting with an
   independent filesystem walk.
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
