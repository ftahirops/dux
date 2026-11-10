# Dux production readiness

**Decision: NOT READY for production rollout.** The installed VPS1 daemon is
still r66 with a dirty, stale index. The r132 candidate has passed focused
correctness and package checks, but the long soak, Rocky RPM lifecycle,
publisher authentication, production-scale qualification, and canary gates are
not complete.

Status snapshot: **15:10 UTC, 2026-11-10**. VPS1 was queried directly at
15:02 UTC; VPS2 service and soak were queried through 15:10 UTC. The
latest evidence is in the status log and
[`current-metrics.json`](docs/qualification/current-metrics.json).
The detailed, append-only record is [`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md).

## Candidate and live production

| Item | Current evidence |
|---|---|
| Candidate | r132, source `f02ba7a`; binary SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` |
| VPS1 production | `dux 0.6.3` build `e0cc915f`, active as root, 4 historical restarts. At 15:02 UTC it was dirty and 69.5 hours behind, with 766 kernel overflows, 2,518,862 dropped updates, 9 pending updates, 1,299,525 indexed nodes, and a 128-MiB WAL; last full scan was 5.09 days ago. |
| VPS2 installed service | `dux 0.6.3` build `8b02799b`, scan-first mode, active with zero restarts. Five observations completed with zero coverage issues and passed accounting verification. Completion gaps: 13m48s, 14m51s, 15m39s (missed by 39s), and 14m47s. At 15:10 UTC the sixth observation had just started; zero overflows/drops so far. |
| Disk | At 15:02 UTC: `/` 185 GiB total, 59 GiB available (67% used); workspace 4.2 GiB (`target/readiness` 1.4 GiB); `/var/lib/dux` 3.4 GiB; `/var/log` 2.0 GiB. Disk is not currently near capacity. |
| Current root host admission | At 15:10 UTC, the local guard reported load1 16.34, CPU PSI some 58.08%, and active Go build/test/vet jobs. No VM start is safe. A prior guest was stopped by the runtime guard before SSH/cloud-init; no package commands ran. |

## Qualification gates

| Gate | Result | Remaining evidence |
|---|---|---|
| Rust correctness | 341 passed, 0 failed, 2 ignored on the recorded r132 source; strict Clippy/format passed | Ignored scale/ext4 cases are separate; full candidate runtime coverage remains incomplete. |
| Python qualification harnesses | 381 passed, 0 failed, 1 skipped across 25 modules | Includes the 75-case package lifecycle/security suite. Standard-library statement coverage is 53% for the package lifecycle harness; this is not daemon coverage, and branch coverage is unavailable. One signing integration case is skipped without its dedicated RPM fixture. |
| Instrumented coverage | 72.61% lines, 71.44% regions, 76.19% functions; branch counters unavailable | Critical untested paths and filesystem/platform coverage remain. |
| r132 churn | 100k-file ext4 run passed 20k mutations, exact comparisons and SQLite integrity | Not a 1.9M-node production-scale benchmark; convergence ranged 92–323 seconds. |
| 72-hour soak | Running on VPS2 with the expected candidate SHA-256; at 15:10 UTC it had run 21,901.5 seconds (6h5m), 507,264 operations, 505 samples, 6 mutation bursts, 1 held-reader phase, and zero worker restarts | The 300-second held-reader/WAL-reclaim phase passed. Controlled crashes and terminal exactness/history/SQLite checks are pending. This is about 8.4% of the required duration. |
| Non-root service hardening | Exact r132 binary passed a bounded `User=dux` ext4 runtime probe; all capability masks matched, fanotify observed a new file, rescan/accounting and SQLite checks passed. A mode-000 file was read through `open_by_handle_at` as UID `dux` (direct kernel API probe). | Temporary packages are unsigned and not release outputs. Package install/upgrade/rollback, Dux's integrated handle decoder/replay, full packaged sandbox, production-scale and multi-day behavior remain unqualified. See [runtime evidence](docs/qualification/nonroot-runtime-r132-vps2-20261110/README.md). |
| Ubuntu DEB lifecycle | An earlier r132 lifecycle run passed; active-service upgrade/rollback protection has since changed and is not VM-qualified yet | Local package-script regressions pass, but the new DEB `preinst`/rollback path must be rerun on a disposable Ubuntu VM. This does not qualify RPM behavior or production rollout. |
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
