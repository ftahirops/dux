# Dux production readiness

**Decision: NOT READY for production rollout.** The installed VPS1 daemon is
still r66 with a dirty, stale index. The r132 candidate has passed focused
correctness and package checks, but the long soak, Rocky RPM lifecycle,
publisher authentication, production-scale qualification, and canary gates are
not complete.

Status snapshot: **17:18 UTC, 2026-11-10**. VPS2 was queried at 17:18 UTC,
its soak checkpoint is from 17:16 UTC, and prior VPS1/disk and VM evidence
is from 17:06 and 17:04 UTC respectively. The
latest evidence is in the status log and
[`current-metrics.json`](docs/qualification/current-metrics.json).
The detailed, append-only record is [`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md).

## Candidate and live production

| Item | Current evidence |
|---|---|
| Candidate | r132, source `f02ba7a`; binary SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` |
| VPS1 production | `dux 0.6.3` build `e0cc915f`, active as root, 4 historical restarts. At 17:06 UTC it remained dirty from fanotify overflow and 71.6 hours behind, with 775 lifetime kernel overflows, 2,518,862 dropped updates, 30,500 pending updates, 1,334,760 indexed nodes, a 3.30-GiB DB and 128-MiB WAL; last full scan was 5.17 days ago. The service was active with four historical restarts; the installed binary SHA-256 is recorded in the live evidence file. |
| VPS2 installed service | `dux 0.6.3` build `8b02799b`, scan-first mode, daemon live with zero restarts. Thirteen verified observations completed. The latest began at 17:08:54 and finished at 17:15:27 (6m33s), covering 1,743,558 files and 213,882 directories with zero coverage issues, zero unresolved reuse paths, and 82,984,394,752 unique bytes; accounting passed at 17:16:36 in 68.8s. The completion interval was 15m50s, 50 seconds over target; six of twelve measured intervals now exceed 15 minutes. At 17:18 UTC the service remained active with zero pending updates, kernel overflows, and dropped updates; last scan age was 220s. Index dirty state means later filesystem hints await the next full observation. Per-path freshness is not measured. See [live status evidence](docs/qualification/vps2-live-status-20261110T1718Z.json). |
| Disk | At 17:06 UTC: `/` 184.5 GiB total, 50.6 GiB available (72% used); workspace 4.2 GiB (`target/readiness` 1.4 GiB); `/var/lib/dux` 3.4 GiB; `/var/log` 2.0 GiB. Disk is not currently near capacity. |
| Current root host admission | The Ubuntu guest was stopped by the pressure guard at 16:21 before SSH/cloud-init, so no package commands ran. A 16:35 start was refused because Node Vitest was active. The 17:04 guest passed admission, but the pressure watchdog stopped it nine seconds later when Go compiler/test work appeared; SSH reset and no package command ran. The prior 16:47 attempt was also refused during admission. |

The latest 1.74M-file observation took 6m33s and passed coverage and independent accounting. The preceding completion gap was 15m50s. A cgroup sample from 17:13:54 to 17:15:58 spans the final part of indexing and the start of verification: 124 seconds, about 91.2 CPU-seconds, 0.99 GiB read and 0.45 GiB written on the backing disk. Cgroup memory peaked at 1.93 GiB; this sample does not represent the entire scan. See [resource-counter samples](docs/qualification/live-vps2-scan-resources-20261110.json).

## Qualification gates

| Gate | Result | Remaining evidence |
|---|---|---|
| Rust correctness | 341 passed, 0 failed, 2 ignored on the recorded r132 source; strict Clippy/format passed | Ignored scale/ext4 cases are not passing evidence; full candidate runtime coverage remains incomplete. |
| Python qualification harnesses | 380 passed, 0 failed, 1 skipped across 25 modules (381 total) | Includes the 75-case package lifecycle/security suite. Standard-library statement coverage is 53% for the package lifecycle harness; this is not daemon coverage, and branch coverage is unavailable. One signing integration case is skipped without its dedicated RPM fixture. |
| Instrumented coverage | 72.61% lines, 71.44% regions, 76.19% functions; branch counters unavailable | Critical untested paths and filesystem/platform coverage remain. |
| r132 churn | 100k-file ext4 run passed 20k mutations, exact comparisons and SQLite integrity | Not a 1.9M-node production-scale benchmark; convergence ranged 92–323 seconds. |
| 72-hour soak | Running on VPS2 with the expected candidate SHA-256; at 17:16 UTC it had run 29,511.5 seconds (8h11m52s), 675,218 operations, 672 exactness samples, 8 mutation bursts, 1 held-reader phase, and zero worker restarts | The 300-second held-reader/WAL-reclaim phase passed. No controlled crashes have run; terminal exactness/history/SQLite checks are pending. Elapsed-average pace is about 22.88 operations/second against a requested 25; this is 11.4% of the required duration. The soak harness is running only its `soak` suite; its other 15 suites are not part of this run. See [17:16 checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T1716Z.json). |
| Non-root service hardening | Exact r132 binary passed a bounded `User=dux` ext4 runtime probe; all capability masks matched, fanotify observed a new file, rescan/accounting and SQLite checks passed. A mode-000 file was read through `open_by_handle_at` as UID `dux` (direct kernel API probe). | Temporary packages are unsigned and not release outputs. Package install/upgrade/rollback, Dux's integrated handle decoder/replay, full packaged sandbox, production-scale and multi-day behavior remain unqualified. See [runtime evidence](docs/qualification/nonroot-runtime-r132-vps2-20261110/README.md). |
| Ubuntu DEB lifecycle | An earlier r132 lifecycle run passed; active-service upgrade/rollback protection has since changed and is not VM-qualified yet | Local package-script regressions pass, but the new DEB `preinst`/rollback path must be rerun on a disposable Ubuntu VM. This does not qualify RPM behavior or production rollout. |
| Rocky RPM lifecycle | Not qualified; a disposable-key RPM signature was verified in Rocky | Full install/upgrade/rollback did not start; retry only after guarded host admission. |
| Release authenticity | Open | Root GPG keyring is empty and `DUX_RELEASE_SIGNING_KEY` is unset. Release owner must provide trusted fingerprint/key; no publisher key has been used. |
| Dependency audit | Passed at 16:15 UTC: 229 dependencies, zero findings/warnings | RustSec database refreshed Nov 10; dependency audit is not application security certification. |
| Production canary | Not started | Candidate has not been installed on VPS1; first qualify package, rollback, security and production-scale behavior. |

## Test count and coverage summary

The recorded Rust and Python automated suites total **724 cases**: **721 passed,
0 failed, 2 ignored, and 1 skipped**. Rust had 341 passes and 2 ignored; Python
had 380 passes and 1 skip across 25 modules. The 75 package lifecycle/security
cases are already included in the Python total. Measured Rust coverage is
72.61% lines, 71.44% regions, and 76.19% functions; branch counters were not
available. The lifecycle script has 53% statement coverage, which is not
coverage of the Dux daemon. Separate integration and live exercises include
100k-file ext4 churn, a 1.37M-path scale fixture, forced kernel overflow and
recovery, a non-root service probe, and 13 live VPS2 observations. The full
fault matrix, 10M scale, controlled soak crashes, release lifecycle on current
packages, and production canary remain open. The detailed inventory and limits
are in [the qualification log](PRODUCTION_READINESS_STATUS.md#test-inventory-and-measured-coverage).

## Required next actions

1. Let the exact r132 soak reach its scheduled end (about **2026-11-13 09:04 UTC**) and require every terminal assertion to pass.
2. Build and qualify the pending non-root service package change on disposable Ubuntu and Rocky guests, including state migration and rollback.
3. Obtain the release owner's signing identity; sign and verify the RPM and checksum manifest, then complete the guarded Rocky lifecycle.
4. Qualify r132 against a production-sized index and WAL budget, then complete privilege/unit review and a controlled rollback-ready canary.

Do not claim zero-error operation, full production readiness, or a completed
canary until those gates have direct passing evidence.
