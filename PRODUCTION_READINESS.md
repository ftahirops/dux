# Dux production readiness

**Decision: NOT READY for production rollout.** This snapshot is current to
**2026-11-10 20:19 UTC**. VPS1 was rechecked at 20:08 UTC and remains on the old event-apply
daemon. The r132 candidate remains
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
| **VPS1 production** | At 20:08 UTC: Dux 0.6.3 build `e0cc915f`, running as root with four historical systemd restarts; dirty and not caught up, 785 lifetime overflows, 2,518,862 dropped updates, 166 pending updates, and last full scan on 2026-11-05. This is not the r132 candidate. Current dirty reason reports non-watched filesystem mounts changed; reconciling visible paths. No OOM events were recorded. See [latest VPS1 status](docs/qualification/vps1-live-status-20261110T2008Z.json). |
| **VPS2 installed service** | Dux 0.6.3 build `8b02799b`, scan-first mode; binary SHA-256 `b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`. At 20:23 it had zero restarts, overflows, drops, and pending updates. The service was active; status was dirty with reconciliation accounting verification pending, and last completed scan age was 106 seconds. Two earlier full observations completed at 19:50 and 20:06 with zero coverage issues; independent accounting passed in 100.4 and 130.3 seconds. Starts were exactly 15 minutes apart. Per-path freshness is not measured. |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, running in an isolated worker on VPS2. It is separate from the installed VPS2 daemon. The 20:22 checkpoint is 11h17m22s into a 72-hour run (15.68%), with 918,550 operations, 915 exactness samples, 11 mutation bursts, one held-reader/WAL-reclaim scenario, and zero controlled crashes. Each exactness sample requests a full scan-first observation and accounting verification, then independently compares the fixture. This is reconciliation-under-churn endurance evidence on a small fixture, not passive event-only or production-scale soak evidence. The scheduled 12-hour crash-recovery phase and terminal exactness, history, and SQLite checks remain pending. VPS2 had 91% disk use and 11.33% CPU PSI some avg10 at 20:23, so the guarded fault VM remains inadmissible. |
| **VPS2 disk** | At 20:23, root was 91% used with 8.78 GB available and CPU PSI some avg10 was 11.33%; an earlier 20:16 sample peaked at 45.29%. ClickHouse was using about 91% CPU at 20:17. The guarded fault VM was not started. Detailed directory attribution is older (18:06); no cleanup is recorded. Do not start nested KVM or a large package build on VPS2. |

### Recent live reliability and throughput evidence

On the installed VPS2 service, one scan at 18:53 found a conflicting inode
incarnation and was rejected with one coverage issue. The old index was retained.
The bounded retry completed at 19:09 with zero coverage issues; accounting
passed at 19:10. Later full observations completed at 19:50 and 20:06 with zero
coverage issues. Their starts were exactly 15 minutes apart; traversal took
322.3 and 388.0 seconds, followed by independent accounting checks of 100.4 and
130.3 seconds. These results demonstrate scheduled recovery in this interval,
not convergence under every sustained-churn condition or a per-path freshness
SLO.

A separate 10m45s watcher-log window recorded 65 samples: median 28,298
fanotify events/s (range 10,437–35,493), maximum queue depth 1,296 bytes, and
zero queue-pressure episodes. This is live-host evidence for the installed
VPS2 service, not a controlled capacity test for r132. Dux reported zero event
resolution in this window, as expected in scan-first mode. The scan-first
`behind_seconds` field is marked inapplicable; it is not a freshness measure.

Evidence: [latest scheduled scans](docs/qualification/vps2-reconciliation-cycle-20261110T2006Z.log), [latest VPS2 status](docs/qualification/vps2-live-status-20261110T2012Z.json), [scan retry and recovery](docs/qualification/vps2-reconciliation-retry-20261110T1911Z.log),
[next scheduled scan](docs/qualification/vps2-reconciliation-cycle-20261110T1922Z.log),
[throughput profile](docs/qualification/vps2-event-throughput-20261110T1925Z/profile.json),
[latest VPS2 status](docs/qualification/vps2-live-status-20261110T1926Z.json),
[latest r132 soak checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T2022Z.json).

## Tests, coverage, and open release gates

| Area | Latest evidence | Still open |
|---|---|---|
| Rust automated tests | Full suite: 341 passed, 0 failed, 2 ignored on source matching r132. Both ignored cases then passed in targeted runs: ext4 stale-parent classification and 100k-entry reconciliation. The ext4 case also passed under a non-root UID with exactly the planned service capability masks. Strict Clippy and formatting passed. | The full suite was not rerun after those focused cases; 100k timing is debug-profile and does not prove large-scale production performance. The non-root test used UID 65534 because the `dux` account is not installed on this host. |
| Python qualification harness | 380 passed, 0 failed, 1 skipped (381 total, 25 modules). | Signing integration is skipped without its RPM fixture. Python statement coverage is 53% for the package lifecycle script only; branch coverage is unavailable. |
| Rust instrumentation | 72.61% lines, 71.44% regions, 76.19% functions. | Branch counters unavailable; critical syscall and race paths remain incompletely covered. |
| Dependency audit | 229 locked dependencies; zero advisories and warnings. Local RustSec database head matched upstream at the 18:55 check. | This is dependency hygiene, not a full application security review. |
| Package lifecycle | r132 Ubuntu DEB lifecycle passed in an earlier disposable-VM run. | Current package/rollback path is not VM-qualified; Rocky RPM lifecycle is not qualified. Trusted publisher key/fingerprint is unavailable, so release authenticity is open. |
| Fault matrix and crashes | Some individual runtime tests and live recovery observations passed. | The complete 15-suite guarded matrix and controlled soak crash scenarios have not run. At 19:50 VPS2 admission showed CPU PSI some avg10 20.58%, load1 7.21, root disk 91% used, and ClickHouse at 90.8% CPU. No VM was started. See [latest VPS2 admission sample](docs/qualification/vm-admission-20261110T1950Z.json). |
| Scale and platform | 100k-file churn, 1.37M-path scale, forced overflow recovery, non-root probe, and repeated VPS2 scans have prior evidence. | 10M-file scale, broader filesystem/kernel matrix, and exact path-level freshness remain unqualified or unmeasured. |
| Systemd sandbox event visibility | Exact r132 binary in a separate mount namespace with package-equivalent capabilities, syscall filters, `ProtectKernelTunables`, and `ProtectControlGroups` indexed host-namespace create, rename, and delete events on ext4; create appeared in 187 ms, with zero test-group overflows or drops. Production service counters did not change during the test. | Actual package-created UID, install/upgrade/rollback lifecycle, other filesystems/kernels, and long-duration packaged service remain open. See [namespace event result](docs/qualification/systemd-hardening-20261110/cross-namespace-event-test-20261110T2004Z.json). |
| Production canary | Not started. | VPS1 rollback-ready canary, service-account migration, and rollback evidence remain required. |

The full Rust and Python runs plus the two targeted Rust cases cover **723 distinct passing cases, 0 failures, and 1 skipped**. The ordinary Rust full-run output still lists its two manual cases as ignored; both were separately executed and passed. See the [focused qualification report](docs/qualification/r132-focused-qualification-20261110/README.md). The 75 package lifecycle/security tests
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
