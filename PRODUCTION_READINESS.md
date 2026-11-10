# Dux production readiness

**Decision: NOT READY for production rollout.** This snapshot is current to
**2026-11-10 22:51 UTC**. VPS1 was last rechecked at 21:30 UTC and remains on the old event-apply
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
| **VPS1 production** | At 21:30 UTC: Dux 0.6.3 build `e0cc915f`, running as root with four historical systemd restarts; still dirty and not caught up, 833 lifetime overflows, 2,518,862 dropped updates, 957 pending updates, and no scan active. Dirty reason is `fanotify queue overflow`; the last scan was over 5.3 days ago and the index reports about 3.17 days behind. This is not the r132 candidate. Overflow count increased by 48 since the 20:08 sample. See [latest VPS1 status](docs/qualification/vps1-live-status-20261110T2130Z.json). |
| **VPS2 installed service** | Dux 0.6.3 build `8b02799b`, scan-first mode. The latest full observation completed at 21:36:57 over 1,786,223 files and 218,602 directories with 0 coverage issues; accounting passed at 21:38:09. At 21:38 the service had zero kernel overflows, dropped updates, and pending updates. It was dirty because filesystem changes await the next full observation; per-path freshness is not measured. |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, isolated on VPS2. At 22:49, the soak had run 13h44m08s of a 72-hour target (19.08%), with 1,103,829 operations, 1,100 exactness samples, 13 mutation bursts, two held-reader/WAL-reclaim passes, and one controlled crash/restart recovery. Whole-run average is 22.323 ops/s; synchronous scan/accounting/oracle checks pause the pacer, so this does not demonstrate sustained 25/s capacity. Sampled convergence latency: mean 3.422s, median 2.792s, p95 7.839s, max 17.393s. The recovery killed candidate PID 1305044, restarted PID 3117886, and recovered 64 files created during downtime after exact-index and history checks passed. The outer systemd supervisor remains active with zero restarts. Terminal exactness, history, and SQLite checks remain pending. See the [22:49 checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T2249Z.json). |
| **Disk and VM admission** | At 22:49 the qualification host root was 69% used (56.7 GiB available) with 9.3 GiB free in `/dev/shm`. VPS2 root was 88% used (10.6 GiB available) with 2.0 GiB free in `/dev/shm`; its 4.1 GiB journal and last measured 2.4 GiB `/var/tmp` remain preserved. A disposable Ubuntu VM started at 22:34 but its pressure guard stopped it after 43 seconds when four unrelated Go compiler/test processes were detected; the ephemeral overlay was cleaned and no test fixture or matrix test ran. A 22:50 recheck rejected admission on sample 1/3 because two Go test processes were active and load1 was 10.58. No VM or fault matrix was started. See the [22:49 capacity sample](docs/qualification/host-capacity-20261110T2249Z.json), [22:50 admission recheck](docs/qualification/vm-admission-preflight-20261110T2250Z.json), [disk cleanup record](docs/qualification/vps2-disk-cleanup-20261110T2150Z.md), and [22:35 guard event](docs/qualification/vm-admission-20261110T2235Z.json). |

### Recent live reliability and throughput evidence

On the installed VPS2 service, one scan at 18:53 found a conflicting inode
incarnation and was rejected with one coverage issue. The old index was retained.
The bounded retry completed at 19:09 with zero coverage issues; accounting
passed at 19:10. Later observations completed at 19:50, 20:06, 21:23, and
21:36 with zero coverage issues. The 21:23 and 21:36 cycles started 15 minutes
apart; their traversals took 511.8 and 390.0 seconds, followed by independent
accounting checks of 91.1 and 72.3 seconds. These results demonstrate scheduled recovery in this interval,
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
[latest r132 soak checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T2138Z.json), [latest VPS2 reconciliation cycle](docs/qualification/vps2-reconciliation-cycle-20261110T2137Z.json).

## Tests, coverage, and open release gates

| Area | Latest evidence | Still open |
|---|---|---|
| Rust automated tests | Full suite: 341 passed, 0 failed, 2 ignored on source matching r132. Both ignored cases then passed in targeted runs: ext4 stale-parent classification and 100k-entry reconciliation. The ext4 case also passed under a non-root UID with exactly the planned service capability masks. Strict Clippy and formatting passed. | The full suite was not rerun after those focused cases; 100k timing is debug-profile and does not prove large-scale production performance. The non-root test used UID 65534 because the `dux` account is not installed on this host. |
| Python qualification harness | Latest full suite: 381 tests in 17.800s, 0 failures, 1 skipped (the RPM signing fixture); this includes the admission-wrapper regression. A preceding run under a 5% CPU cap had 11 subprocess-timeout errors; every case passed on the normal-priority rerun. | Python statement coverage is 53% for the package lifecycle script only; branch coverage is unavailable. See [rerun record](docs/qualification/python-suite-rerun-20261110T2205Z.md). |
| Rust instrumentation | 72.61% lines, 71.44% regions, 76.19% functions. | Branch counters unavailable; critical syscall and race paths remain incompletely covered. |
| Dependency audit | Fresh `cargo audit -D warnings` at 22:15 passed: 229 locked dependencies, zero advisories, zero warnings. RustSec database had 1,446 advisories at commit `28d73fc9`, which direct `git ls-remote` confirmed matched upstream `main`. | Database head was unchanged since the earlier check; this is dependency hygiene, not a full application security review. See [audit evidence](docs/qualification/cargo-audit-20261110T2215Z/README.md). |
| Package lifecycle | r132 Ubuntu DEB lifecycle passed in an earlier disposable-VM run. | Current package/rollback path is not VM-qualified; Rocky RPM lifecycle is not qualified. Trusted publisher key/fingerprint is unavailable, so release authenticity is open. |
| Fault matrix and crashes | Some individual runtime tests and live recovery observations passed. The admission-wrapper module passed 41/41 and the complete Python suite now passes. A production and fault-injection binary pair for source commit `f02ba7ad` is staged and their embedded source digest matches. | The complete 15-suite guarded matrix remains unrun; the r132 soak has passed one of five scheduled controlled crash-recovery checkpoints. The disposable VM was stopped at 22:35 after four Go build/test processes appeared; no matrix suite ran. See [fault-binary verification](docs/qualification/r132-fault-binary-20261110.json), [admission fix evidence](docs/qualification/vm-admission-wrapper-detection-20261110.md), and the [22:35 guard event](docs/qualification/vm-admission-20261110T2235Z.json). |
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
