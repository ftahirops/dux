# Dux production readiness

**Decision: NOT READY for production rollout.** The installed VPS1 daemon is
still r66 with a dirty, stale index. The r132 candidate has passed focused
correctness and package checks, but the long soak, Rocky RPM lifecycle,
publisher authentication, production-scale qualification, and canary gates are
not complete.

Status snapshot: **2026-11-10 11:57 UTC**. Machine-readable values and evidence
timestamps are in [`current-metrics.json`](docs/qualification/current-metrics.json).
The detailed, append-only record is [`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md).

## Candidate and live production

| Item | Current evidence |
|---|---|
| Candidate | r132, source `f02ba7a`; binary SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` |
| VPS1 production | r66, active as root with 4 historical restarts; at 11:54 UTC it was dirty and about 66.4 hours behind, with 752 overflows and 2,518,862 dropped updates; last full scan Nov 5 12:49 UTC |
| Disk | At 11:54 UTC: `/` 185 GiB total, 56 GiB free. Disk is not the current blocker. |
| Current root host load | At 11:54 UTC: load average 17.13/27.03/20.67. Rocky guest was not started. |

## Qualification gates

| Gate | Result | Remaining evidence |
|---|---|---|
| Rust correctness | 341 passed, 0 failed, 2 ignored on the recorded r132 source; strict Clippy/format passed | Ignored scale/ext4 cases are separate; full candidate runtime coverage remains incomplete. |
| Python qualification harnesses | 376 passed, 0 failed, 1 skipped across 25 modules | Includes the 74-case package lifecycle/security suite. One signing integration case is skipped when its dedicated RPM fixture is not configured. |
| Instrumented coverage | 72.61% lines, 71.44% regions, 76.19% functions; branch counters unavailable | Critical untested paths and filesystem/platform coverage remain. |
| r132 churn | 100k-file ext4 run passed 20k mutations, exact comparisons and SQLite integrity | Not a 1.9M-node production-scale benchmark; convergence ranged 92–323 seconds. |
| 72-hour soak | Running on VPS2, zero restarts at 11:53 UTC; 10,091.7 seconds, 236,237 operations, 235 periodic samples, 2 bursts | Held-reader, crash/recovery, terminal exactness/history/integrity are pending. This run uses an older pacing harness; latest exact convergence sample was 7,718 ms. |
| Non-root service hardening | Source changes, 376 Python tests, temporary DEB/RPM builds and payload inspection pass | Temporary artifacts are unsigned and not release outputs. Package install/upgrade/rollback, fanotify and handle access under UID `dux` remain untested. See [follow-up evidence](docs/qualification/systemd-hardening-20261110/nonroot-service-followup.md). |
| Ubuntu DEB lifecycle | Passed for r132 | Does not qualify RPM behavior or production rollout. |
| Rocky RPM lifecycle | Not qualified; a disposable-key RPM signature was verified in Rocky | Full install/upgrade/rollback did not start; retry only after guarded host admission. |
| Release authenticity | Open | Root GPG keyring is empty and `DUX_RELEASE_SIGNING_KEY` is unset. Release owner must provide trusted fingerprint/key; no publisher key has been used. |
| Dependency audit | Passed: 229 dependencies, zero findings/warnings | RustSec DB snapshot is dated Nov 9; dependency audit is not application security certification. |
| Production canary | Not started | Candidate has not been installed on VPS1; first qualify package, rollback, security and production-scale behavior. |

## Required next actions

1. Let the exact r132 soak reach its scheduled end (about **2026-11-13 09:04 UTC**) and require every terminal assertion to pass.
2. Build and qualify the pending non-root service package change on disposable Ubuntu and Rocky guests, including state migration and rollback.
3. Obtain the release owner's signing identity; sign and verify the RPM and checksum manifest, then complete the guarded Rocky lifecycle.
4. Qualify r132 against a production-sized index and WAL budget, then complete privilege/unit review and a controlled rollback-ready canary.

Do not claim zero-error operation, full production readiness, or a completed
canary until those gates have direct passing evidence.
