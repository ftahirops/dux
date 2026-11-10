# Dux production readiness

**Decision: NOT READY for production rollout.** This snapshot is current to
**2026-11-10 19:43 UTC**. VPS1 was last verified running the old event-apply
daemon at 17:06 UTC and has not been rechecked since. The r132 candidate remains
in a separate soak, and release, scale, fault-matrix, and canary gates remain
open. A green unit suite or a successful VPS2 observation does not close those
gates.

The detailed evidence trail is
[`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md); the
machine-readable snapshot is
[`current-metrics.json`](docs/qualification/current-metrics.json).

## Live systems and candidate

| System | Latest verified state |
|---|---|
| **VPS1 production** | Last detailed observation at 17:06 UTC: Dux 0.6.3 build `e0cc915f`, running as root, dirty after fanotify overflow, 775 lifetime overflows, 2,518,862 dropped updates, 30,500 pending updates, and last full scan on 2026-11-05. This is not the r132 candidate. No newer VPS1 observation is recorded here. |
| **VPS2 installed service** | Dux 0.6.3 build `8b02799b`, scan-first mode; binary SHA-256 `b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`. At 19:41:55 it was active with zero restarts. The latest full scan/accounting evidence remains from 19:37–19:38. Its latest full observation completed at 19:37 with zero coverage issues; independent accounting passed in 66.0 seconds. The index was marked dirty because new hints arrived after the scan. Per-path freshness is not measured. |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, running in an isolated worker on VPS2. It is separate from the installed VPS2 daemon. The 19:42 checkpoint is 10h36m48s into a 72-hour run (14.76%), with 866,389 operations, 863 exactness samples, 10 mutation bursts, one held-reader/WAL-reclaim scenario, and zero controlled crashes. At 19:41:55 the installed service was active with zero restarts. Terminal exactness, history, and SQLite checks remain pending. |
| **VPS2 disk** | At 19:41:55, root was 90% used with 9.68 GB available. Detailed directory attribution is older (18:06); no cleanup is recorded. Do not start nested KVM or a large package build on VPS2. |

### Recent live reliability and throughput evidence

On the installed VPS2 service, one scan at 18:53 found a conflicting inode
incarnation and was rejected with one coverage issue. The old index was retained.
The bounded retry completed at 19:09 with zero coverage issues; accounting
passed at 19:10. The next scheduled scan completed at 19:21 with zero coverage
issues and accounting passed at 19:22. These results show recovery in this
observed interval; they do not prove convergence under every sustained-churn
condition.

A separate 10m45s watcher-log window recorded 65 samples: median 28,298
fanotify events/s (range 10,437–35,493), maximum queue depth 1,296 bytes, and
zero queue-pressure episodes. This is live-host evidence for the installed
VPS2 service, not a controlled capacity test for r132. Dux reported zero event
resolution in this window, as expected in scan-first mode. The scan-first
`behind_seconds` field is marked inapplicable; it is not a freshness measure.

Evidence: [latest scheduled scan](docs/qualification/vps2-reconciliation-cycle-20261110T1938Z.log), [latest VPS2 status](docs/qualification/vps2-live-status-20261110T1938Z.json), [scan retry and recovery](docs/qualification/vps2-reconciliation-retry-20261110T1911Z.log),
[next scheduled scan](docs/qualification/vps2-reconciliation-cycle-20261110T1922Z.log),
[throughput profile](docs/qualification/vps2-event-throughput-20261110T1925Z/profile.json),
[latest VPS2 status](docs/qualification/vps2-live-status-20261110T1926Z.json),
[latest r132 soak checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T1942Z.json).

## Tests, coverage, and open release gates

| Area | Latest evidence | Still open |
|---|---|---|
| Rust automated tests | 341 passed, 0 failed, 2 ignored on source matching r132; strict Clippy and formatting passed. | Ignored ext4/scale cases are not passes; candidate runtime matrix remains incomplete. |
| Python qualification harness | 380 passed, 0 failed, 1 skipped (381 total, 25 modules). | Signing integration is skipped without its RPM fixture. Python statement coverage is 53% for the package lifecycle script only; branch coverage is unavailable. |
| Rust instrumentation | 72.61% lines, 71.44% regions, 76.19% functions. | Branch counters unavailable; critical syscall and race paths remain incompletely covered. |
| Dependency audit | 229 locked dependencies; zero advisories and warnings. Local RustSec database head matched upstream at the 18:55 check. | This is dependency hygiene, not a full application security review. |
| Package lifecycle | r132 Ubuntu DEB lifecycle passed in an earlier disposable-VM run. | Current package/rollback path is not VM-qualified; Rocky RPM lifecycle is not qualified. Trusted publisher key/fingerprint is unavailable, so release authenticity is open. |
| Fault matrix and crashes | Some individual runtime tests and live recovery observations passed. | The complete 15-suite guarded matrix and controlled soak crash scenarios have not run. At 19:39 an unrelated Go test was still active, despite CPU PSI some avg10 easing to 1.23%. No VM was started. See [latest host sample](docs/qualification/current-metrics.json) and [earlier high-pressure sample](docs/qualification/host-admission-20261110T1931Z.json). |
| Scale and platform | 100k-file churn, 1.37M-path scale, forced overflow recovery, non-root probe, and repeated VPS2 scans have prior evidence. | 10M-file scale, broader filesystem/kernel matrix, and exact path-level freshness remain unqualified or unmeasured. |
| Production canary | Not started. | VPS1 rollback-ready canary, service-account migration, and rollback evidence remain required. |

The latest full language-suite counts are **721 passed, 0 failed, 2 ignored,
and 1 skipped** across Rust and Python. The 75 package lifecycle/security tests
are already included in the Python total. See the
[regression evidence](docs/qualification/regression-20261110T1823Z/README.md)
and [coverage gap review](docs/qualification/coverage-gaps-20261109.md).

## Next gates

1. Let the exact r132 soak reach 72 hours and require all terminal assertions to
   pass, including SQLite integrity and history/exactness checks.
2. Run the 15-suite fault matrix, controlled crash recovery, current DEB/RPM
   upgrade/rollback tests, and 10M scale in guarded disposable environments.
3. Obtain the release owner's trusted signing identity and verify publisher
   signatures on release artifacts.
4. Refresh VPS1 production evidence, then perform a rollback-ready canary only
   after the package, security, scale, and recovery gates pass.

Do not claim zero-error operation or production readiness until each required
gate has direct passing evidence.
