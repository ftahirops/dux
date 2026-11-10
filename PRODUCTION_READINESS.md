# Dux production readiness

**Decision: NOT READY for production rollout.** The installed VPS1 daemon is
still r66 with a dirty, stale index. The r132 candidate has passed focused
correctness and package checks, but the long soak, Rocky RPM lifecycle,
publisher authentication, production-scale qualification, and canary gates are
not complete.

Status snapshot: **16:31 UTC, 2026-11-10**. VPS1 was queried at 15:34 UTC,
disk and host admission at 16:25 UTC, and VPS2/soak through 16:31 UTC. The
latest evidence is in the status log and
[`current-metrics.json`](docs/qualification/current-metrics.json).
The detailed, append-only record is [`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md).

## Candidate and live production

| Item | Current evidence |
|---|---|
| Candidate | r132, source `f02ba7a`; binary SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` |
| VPS1 production | `dux 0.6.3` build `e0cc915f`, active as root, 4 historical restarts. At 15:34 UTC it was dirty from fanotify overflow and 70.1 hours behind, with 774 kernel overflows, 2,518,862 dropped updates, 50,618 pending updates, 1,293,153 indexed nodes, and a 128-MiB WAL; last full scan was 5.11 days ago. |
| VPS2 installed service | `dux 0.6.3` build `8b02799b`, scan-first mode, daemon live with zero restarts. Ten verified observations completed. The 16:19 attempt failed once with an inode-incarnation conflict and retained the prior index; its retry completed at 16:30:21 with zero coverage issues, and accounting passed at 16:31:43. The index is dirty because hints changed during the scan. Four of nine successful-completion gaps exceeded 15 minutes (latest 17m12s). Per-path freshness is not measured. At 16:31:45: zero pending updates, overflows, or drops. systemd reports `reloading (reload-notify)` since 04:23; this long-lived notify state needs review. |
| Disk | At 16:25 UTC: `/` 185 GiB total, 56 GiB available (69% used); workspace 4.2 GiB (`target/readiness` 1.4 GiB); `/var/lib/dux` 3.4 GiB; `/var/log` 2.0 GiB. Disk is not currently near capacity. |
| Current root host admission | At 16:21 UTC, a guarded VM start passed admission, but the pressure guard stopped the guest 27 seconds later when an unrelated Go test binary and `go test` became active. It was stopped before SSH/cloud-init; no package commands ran. At 16:25 UTC, load1 was 15.95 and CPU PSI some avg10 was 47.51%. |

## Qualification gates

| Gate | Result | Remaining evidence |
|---|---|---|
| Rust correctness | 341 passed, 0 failed, 2 ignored on the recorded r132 source; strict Clippy/format passed | Ignored scale/ext4 cases are not passing evidence; full candidate runtime coverage remains incomplete. |
| Python qualification harnesses | 380 passed, 0 failed, 1 skipped across 25 modules (381 total) | Includes the 75-case package lifecycle/security suite. Standard-library statement coverage is 53% for the package lifecycle harness; this is not daemon coverage, and branch coverage is unavailable. One signing integration case is skipped without its dedicated RPM fixture. |
| Instrumented coverage | 72.61% lines, 71.44% regions, 76.19% functions; branch counters unavailable | Critical untested paths and filesystem/platform coverage remain. |
| r132 churn | 100k-file ext4 run passed 20k mutations, exact comparisons and SQLite integrity | Not a 1.9M-node production-scale benchmark; convergence ranged 92–323 seconds. |
| 72-hour soak | Running on VPS2 with the expected candidate SHA-256; at 16:31 UTC it had run 26,803.4 seconds (7h26m), 614,054 operations, 611 exactness samples, 7 mutation bursts, 1 held-reader phase, and zero worker restarts | The 300-second held-reader/WAL-reclaim phase passed. No controlled crashes have run; terminal exactness/history/SQLite checks are pending. Elapsed-average pace is about 22.91 operations/second against a requested 25; this is 10.3% of the required duration. The soak harness is running only its `soak` suite; its other 15 suites are not part of this run. |
| Non-root service hardening | Exact r132 binary passed a bounded `User=dux` ext4 runtime probe; all capability masks matched, fanotify observed a new file, rescan/accounting and SQLite checks passed. A mode-000 file was read through `open_by_handle_at` as UID `dux` (direct kernel API probe). | Temporary packages are unsigned and not release outputs. Package install/upgrade/rollback, Dux's integrated handle decoder/replay, full packaged sandbox, production-scale and multi-day behavior remain unqualified. See [runtime evidence](docs/qualification/nonroot-runtime-r132-vps2-20261110/README.md). |
| Ubuntu DEB lifecycle | An earlier r132 lifecycle run passed; active-service upgrade/rollback protection has since changed and is not VM-qualified yet | Local package-script regressions pass, but the new DEB `preinst`/rollback path must be rerun on a disposable Ubuntu VM. This does not qualify RPM behavior or production rollout. |
| Rocky RPM lifecycle | Not qualified; a disposable-key RPM signature was verified in Rocky | Full install/upgrade/rollback did not start; retry only after guarded host admission. |
| Release authenticity | Open | Root GPG keyring is empty and `DUX_RELEASE_SIGNING_KEY` is unset. Release owner must provide trusted fingerprint/key; no publisher key has been used. |
| Dependency audit | Passed at 16:15 UTC: 229 dependencies, zero findings/warnings | RustSec database refreshed Nov 10; dependency audit is not application security certification. |
| Production canary | Not started | Candidate has not been installed on VPS1; first qualify package, rollback, security and production-scale behavior. |

## Required next actions

1. Let the exact r132 soak reach its scheduled end (about **2026-11-13 09:04 UTC**) and require every terminal assertion to pass.
2. Build and qualify the pending non-root service package change on disposable Ubuntu and Rocky guests, including state migration and rollback.
3. Obtain the release owner's signing identity; sign and verify the RPM and checksum manifest, then complete the guarded Rocky lifecycle.
4. Qualify r132 against a production-sized index and WAL budget, then complete privilege/unit review and a controlled rollback-ready canary.

Do not claim zero-error operation, full production readiness, or a completed
canary until those gates have direct passing evidence.
