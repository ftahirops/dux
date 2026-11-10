# Dux production readiness

**Decision: NOT READY for production rollout.** The installed VPS1 daemon is
still r66 with a dirty, stale index. The r132 candidate has passed focused
correctness and package checks, but the long soak, Rocky RPM lifecycle,
publisher authentication, production-scale qualification, and canary gates are
not complete.

Status snapshot: **18:36 UTC, 2026-11-10**. VPS2 scan/soak evidence is from
18:36 UTC; disk snapshots are from both vps1 and VPS2; the latest package-VM
attempt remains the guarded 17:38 UTC interruption. The
latest evidence is in the status log and
[`current-metrics.json`](docs/qualification/current-metrics.json).
The detailed, append-only record is [`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md).

## Candidate and live production

| Item | Current evidence |
|---|---|
| Candidate | r132, source `f02ba7a`; binary SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` |
| VPS1 production | `dux 0.6.3` build `e0cc915f`, active as root, 4 historical restarts. At 17:06 UTC it remained dirty from fanotify overflow and 71.6 hours behind, with 775 lifetime kernel overflows, 2,518,862 dropped updates, 30,500 pending updates, 1,334,760 indexed nodes, a 3.30-GiB DB and 128-MiB WAL; last full scan was 5.17 days ago. The service was active with four historical restarts; the installed binary SHA-256 is recorded in the live evidence file. |
| VPS2 installed service | `dux 0.6.3` build `8b02799b`, scan-first mode, daemon live with zero restarts. Eighteen verified observations completed. The latest started at 18:23:55 and finished at 18:31:33 (7m38s), covering 1,756,281 files and 215,521 directories; zero coverage issues, 2,479 vanished reuse paths removed, and 83,561,517,056 unique bytes. Independent accounting passed at 18:33:32 in 119.4s. The completion-to-completion interval was 17m17s; the two scan starts were exactly 15m apart. Eight of seventeen completion gaps exceed 15m, but these are not per-path freshness measurements or proof of scheduler misses. At 18:35:53 there were zero pending updates, kernel overflows, and dropped updates; last scan age was 262s. Dirty means filesystem hints await the next observation. Per-path freshness remains unmeasured. See [live status evidence](docs/qualification/vps2-live-status-20261110T1836Z.json). |
| Disk | vps1 at 18:08 UTC: `/` 184.5 GiB total, 52.6 GiB available (71% used). VPS2 at 18:35 UTC: `/` 92.2 GiB total, 9.45 GiB available (90% used). On VPS2, `/var` was about 59 GiB, `/var/tmp` 5.3 GiB, `/var/log` 4.5 GiB, and systemd journal 4.0 GiB. `/var/tmp` contains old Dux scratch and caches plus an active soak and a dirty source checkout; no cleanup was performed. The full `/var` walk was interrupted due to I/O pressure; see [VPS2 disk inventory](docs/qualification/vps2-disk-inventory-20261110T1806Z.json). |
| VPS2 package-test capacity | KVM is unavailable on VPS2, and its root filesystem is 90% used with 9.45 GiB free. Do not start a nested package VM or large build there. |
| Current root host admission | The 17:38:06 Ubuntu package VM passed admission, then the pressure watchdog stopped it at 17:39:01 when a Go test appeared; SSH never became ready, so no package commands ran. A 17:24 attempt had also been refused at admission. Evidence: [17:38 VM attempt](docs/qualification/vm-admission-20261110T1738Z.json). |

The latest 1.75M-file observation took 7m12s and passed coverage and independent accounting. The preceding completion gap was 15m39s. A 422-second cgroup window from 17:25:52 to 17:32:54 spans only the later scan/index phase and accounting; it recorded 331.1 CPU-seconds, 2.01 GiB read, 1.80 GiB written, a 2.00-GiB cgroup peak, and 204 memory-high events with no OOM. It began 1m58s after scan start and ended 17 seconds after accounting, so it is not the full scan cost. See [resource-counter samples](docs/qualification/live-vps2-scan-resources-20261110.json).

## Qualification gates

| Gate | Result | Remaining evidence |
|---|---|---|
| Rust correctness | 341 passed, 0 failed, 2 ignored on the recorded r132 source; strict Clippy/format passed | Ignored scale/ext4 cases are not passing evidence; full candidate runtime coverage remains incomplete. |
| Python qualification harnesses | 380 passed, 0 failed, 1 skipped across 25 modules (381 total) | Includes the 75-case package lifecycle/security suite. Standard-library statement coverage is 53% for the package lifecycle harness; this is not daemon coverage, and branch coverage is unavailable. One signing integration case is skipped without its dedicated RPM fixture. |
| Instrumented coverage | 72.61% lines, 71.44% regions, 76.19% functions; branch counters unavailable | Critical untested paths and filesystem/platform coverage remain. |
| r132 churn | 100k-file ext4 run passed 20k mutations, exact comparisons and SQLite integrity | Not a 1.9M-node production-scale benchmark; convergence ranged 92–323 seconds. |
| 72-hour soak | Running on VPS2 with the expected candidate SHA-256; at 18:36:00 UTC it had run 34,235.3 seconds (9h30m35s), 779,005 operations, 776 exactness samples, 9 mutation bursts, 1 held-reader phase, and zero worker restarts | The 300-second held-reader/WAL-reclaim phase passed. No controlled crashes have run; terminal exactness/history/SQLite checks are pending. Elapsed-average pace is about 22.82 operations/second against a requested 25; this is 13.21% of the required duration. The soak harness is running only its `soak` suite; its other 15 suites are not part of this run. See [18:36 checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T1836Z.json). |
| Non-root service hardening | Exact r132 binary passed a bounded `User=dux` ext4 runtime probe; all capability masks matched, fanotify observed a new file, rescan/accounting and SQLite checks passed. A mode-000 file was read through `open_by_handle_at` as UID `dux` (direct kernel API probe). | Temporary packages are unsigned and not release outputs. Package install/upgrade/rollback, Dux's integrated handle decoder/replay, full packaged sandbox, production-scale and multi-day behavior remain unqualified. See [runtime evidence](docs/qualification/nonroot-runtime-r132-vps2-20261110/README.md). |
| Ubuntu DEB lifecycle | An earlier r132 lifecycle run passed; active-service upgrade/rollback protection has since changed and is not VM-qualified yet | Local package-script regressions pass, but the new DEB `preinst`/rollback path must be rerun on a disposable Ubuntu VM. This does not qualify RPM behavior or production rollout. |
| Rocky RPM lifecycle | Not qualified; a disposable-key RPM signature was verified in Rocky | Full install/upgrade/rollback did not start; retry only after guarded host admission. |
| Release authenticity | Open | Root GPG keyring is empty and `DUX_RELEASE_SIGNING_KEY` is unset. Release owner must provide trusted fingerprint/key; no publisher key has been used. |
| Dependency audit | Re-run at 18:20 UTC: 229 locked dependencies, zero advisories/warnings | Cached RustSec database timestamp is 13:45 UTC; this run did not refresh it. Dependency audit is not application security certification. |
| Production canary | Not started | Candidate has not been installed on VPS1; first qualify package, rollback, security and production-scale behavior. |

## Unmerged older source checkout

The VPS2 `/var/tmp/claude-dux` checkout is a modified 14-file worktree on base
`52542d9`, with 1,886 additions and 176 deletions. Its patch is preserved in
[qualification evidence](docs/qualification/unmerged-claude-dux-worktree-20261110.patch)
with provenance and hash. `git diff --check` passed in that checkout; no test
run for those dirty changes is verified. A reverse-apply check against the
current branch failed in six files, so these changes remain unintegrated and
must not be counted as tested current code. The checkout is preserved because
it contains uncommitted work.

## Test count and coverage summary

The recorded Rust and Python automated suites total **724 cases**: **721 passed,
0 failed, 2 ignored, and 1 skipped**. Rust had 341 passes and 2 ignored; Python
had 380 passes and 1 skip across 25 modules. The 75 package lifecycle/security
cases are already included in the Python total. Measured Rust coverage is
72.61% lines, 71.44% regions, and 76.19% functions; branch counters were not
available. The lifecycle script has 53% statement coverage, which is not
coverage of the Dux daemon. Separate integration and live exercises include
100k-file ext4 churn, a 1.37M-path scale fixture, forced kernel overflow and
recovery, a non-root service probe, and 17 live VPS2 observations. The full
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
