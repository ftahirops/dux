# Dux release candidate 126 — 2026-11-10

## Build and artifact verification

Built from clean commit `2a884a8ed6d5a63a5019322241de418354f560dc` with
`worktree_dirty: false`, source-tree SHA-256
`bcd0dfb114402608dce53d24457d1fac2692ee4ed338578b296239a45b88d6a4`, Rust
1.99.1, and target `x86_64-unknown-linux-musl`. The package script completed
the static auditable production build, Build-ID-matched debug-symbol split,
Cargo.lock and binary audits, and DEB/RPM packaging. The binary reports
`dux 0.6.3 (2a884a8ed6d5) [production]`.

`sha256sum -c SHA256SUMS` passed for all artifacts. DEB metadata is
`dux 0.6.3-126 amd64`; RPM metadata is `dux 0.6.3-126 x86_64`.

| Artifact | SHA-256 |
|---|---|
| DEB | `25e1b79bebf1056a69147c6cfaef68da679cd82354e0065daf7709df7e10a24d` |
| RPM | `e94c95e1ab50f0371dd330cf75580f1d778e4e07b063df45aa50b2e8f0e48562` |
| Static binary | `9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b` |
| Source archive | `4cb4bd825987ef8df1817786ce0597d6d1cf7403d8dd07ffd25c73807a515bad` |

Local artifacts are in
`/home/dux/target/readiness/production-r126-20261110/`. The repository keeps
the build manifest and audit logs, not the package binaries.

## Included correction and tests

This candidate includes the scan-first event-backlog-age reporting fix
documented in
[`scan-first-backlog-age-20261110.md`](../scan-first-backlog-age-20261110.md).
The full all-feature suite, including ignored tests on tmpfs, passed
**338/338** in 217.40 seconds. Strict Clippy, formatting, and diff checks also
passed. A separate inclusive LLVM run also passed **338/338** in 224.85
seconds and produced the profile below.

| Scope | Line | Region | Function | Branch |
|---|---:|---:|---:|---:|
| Whole Rust binary | 74.08% | 72.99% | 77.14% | Not emitted |
| `src/watch.rs` | 75.44% | 76.35% | 84.95% | Not emitted |
| `src/query.rs` | 90.96% | 87.86% | 87.50% | Not emitted |
| `src/scan.rs` | 93.77% | 90.93% | 91.49% | Not emitted |

Counts: 19,183/25,896 lines, 30,518/41,812 regions, and 1,326/1,719
functions covered overall. The machine-readable profile is
[`llvm-cov-338-tests.json`](llvm-cov-338-tests.json), SHA-256
`e0e5428d02532ea2a17a174fa3394d5324c93226664a75306bb684bb8ab59715`. LLVM
reported zero branch counters, so this is not branch coverage. The test profile
still does not instrument successful `fanotify_init`/`fanotify_mark`, the
event-resolution `open_by_handle_at` call site, or capability-negative
startup. A separate privileged runtime smoke now exercises the successful
syscall path; see
[`fanotify-integration-smoke.md`](fanotify-integration-smoke.md). This runtime
trace is not LLVM coverage.

A capability-negative smoke was also run against the exact candidate binary:
as `nobody`, with an empty capability bounding set, startup failed closed when
fanotify marking returned `EPERM` (`no watchable filesystem`). The reproducible
script is [`capability-negative-smoke.sh`](capability-negative-smoke.sh). This
checks missing `CAP_SYS_ADMIN` at startup; it does not yet test missing
`CAP_DAC_READ_SEARCH` during event-handle resolution.

A dedicated missing-`CAP_DAC_READ_SEARCH` event-resolution smoke passed twice
on VPS2 using the exact candidate binary. With `CAP_SYS_ADMIN` retained,
fanotify startup succeeded, `open_by_handle_at` returned `EPERM`, and Dux
reported uncertainty after unresolved events. See the
[`test report`](../production-live-followup-20261110T0224Z/README.md),
[`reproducible harness`](capability-handle-resolution-smoke.sh), and
[`captured output`](capability-handle-resolution-output.txt). This covers only
Ubuntu 24.04.3/kernel 6.8.0-147-generic/ext4; other kernels, filesystems, and
capability combinations remain outside this test.

The lockfile audit found 0 known vulnerabilities among 229 dependencies with
the recorded 1,430-advisory RustSec snapshot, last updated 2026-11-09. The
binary audit found auditable metadata for 202 dependencies. See
[`cargo-audit-lockfile.json`](cargo-audit-lockfile.json),
[`cargo-audit-binary.log`](cargo-audit-binary.log), and the refreshed audit
record in
[`release-candidate-r125-20261110`](../release-candidate-r125-20261110/README.md).
These dependency checks do not prove application-code security.

## Lifecycle status and open gates

The r126 DEB, r121 baseline DEB, and lifecycle script are prepared in a
read-only VM input share. The guarded r126 runner has not yet started the
disposable guest; host admission has repeatedly detected active Go/Vitest
workloads and high pressure. No candidate was installed on VPS1 or VPS2.

Still required: guarded clean install/upgrade/rollback/purge, independent
path-and-accounting comparison, and a fresh uninterrupted 24-hour canary. The
isolated smoke closes the basic privileged success-path gap, but broader
create/delete/attribute/stale-handle churn and independent-walk comparison
remain open. Broader capability-negative handle-resolution tests remain open;
the missing-`CAP_SYS_ADMIN` startup smoke passed on the tested Linux host. The
prior r121 canary remains failed. Root execution and `CAP_SYS_ADMIN` remain
material security exposure.
