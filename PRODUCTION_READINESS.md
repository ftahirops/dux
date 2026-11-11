# Dux production readiness

**Decision: NOT READY for production rollout.** This snapshot is current to
**2026-11-11 02:06 UTC**. VPS1 was rechecked at 01:01 UTC and remains on the old event-apply
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
| **VPS1 production** | At 01:01 UTC the installed Dux remains 0.6.3 build `e0cc915f8f37`, running as root with four historical restarts; dirty and not caught up, 892 lifetime overflows, 2,518,862 dropped updates, and 3 pending updates. Dirty reason is `non-watched filesystem mounts changed; reconciling visible paths`; last scan is 5.48 days old and reported behind time is 3.29 days. See the [01:01 VPS1 status](docs/qualification/vps1-live-status-20261111T0101Z.json), [pressure pause/resume sample](docs/qualification/vps1-critical-io-pause-recovery-20261110T2328Z.json), and [unresolved log-volume sample](docs/qualification/vps1-unresolved-log-volume-20261110T2321Z.json). |
| **VPS2 installed service** | Dux 0.6.3 build `8b02799b`, scan-first mode. The 01:55:20–02:02:39 full observation completed in 438 s over 1,789,159 files and 215,822 directories with zero coverage issues or reuse conflicts. Accounting passed at 02:04:01 in 82.5 s. At 02:04:51 the service was active with zero restarts, overflows, drops, and pending updates; it remained dirty because filesystem hints changed during the scan. This is another successful reconciliation, not proof of caught-up status. See [02:04 cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0204Z.json). |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, isolated on VPS2. At 02:04 it had run 16h59m39s of a 72-hour target (23.60%), with 1,351,085 operations and 1,348 exactness samples, 16 mutation bursts, two held-reader/WAL-reclaim passes, and one controlled crash/restart recovery. Whole-run average is 22.08 ops/s; synchronous exactness/accounting checks pause the pacer, so this is not a sustained 25/s capacity result. Sampled convergence latency: mean 3.885s, median 3.360s, p95 8.688s, max 17.393s. Four crash checkpoints and terminal exactness, history, and SQLite checks remain pending. See the [02:05 checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261111T0205Z.json). |
| **Disk and VM admission** | At 02:05 VPS2 root was 88% used with 10.9 GiB available; `/var/lib/dux` was 0.86 GiB. The indexed tree ranks ClickHouse data at 20 GiB, containerd at 17 GiB, XGS incidents at 12 GiB, and `/root` qualification/rollback files at 8.5 GiB. A GlitchTip worker has a 3.96 GiB active Docker `json-file` log; inspected container and daemon settings contain no rotation limits. A 1.58 GiB ClickHouse merge file appeared in the 02:03 top-file sample and was gone 27 seconds later, demonstrating transient merge space. No files were deleted. See [disk audit](docs/qualification/vps2-disk-audit-20261111T0205Z.json). The latest guarded VM attempt at 02:05 was refused before QEMU due to Go tests/shell scripts and I/O PSI 10.68%; no fault suite ran. See [02:05 admission result](docs/qualification/vm-admission-20261111T0205Z.json). The 15-suite matrix remains unrun. VPS2 has no KVM. |

### Recent live reliability and throughput evidence

At 23:14 UTC, a bounded inspection of the latest 500 VPS1 journal messages
found 430 unresolved `open_by_handle_at` failures with `ESTALE`. Of those,
304 carried mask `0x4` (`FAN_ATTRIB`) and a FID-only record; 126 had other
masks, including compound event records. The candidate contains a predicate
that suppresses only a complete, target-only `FAN_ATTRIB` record on a known
handle-capable filesystem, with a focused unit test for its boundaries. This
is relevant evidence for that narrow candidate behavior, but it does not show
that every observed `FAN_ATTRIB` event is harmless, that the candidate would
fix the live index, or that these events caused the historical overflows. The
installed production binary predates the candidate, and a live candidate
comparison has not been run. See the [sample and limitations](docs/qualification/vps1-estale-event-sample-20261110T2314Z.json).

A fresh bounded journal sample from 00:35:00 through 00:39:20 UTC counted 93
`ESTALE` warnings. Eighty-nine were target-only `FAN_ATTRIB` FID records on one
filesystem ID; four were compound events. That ID matches VPS1’s root ext4
filesystem, and `/tmp` has the same filesystem ID. The r132 focused ext4 test
used real handles on `/tmp`, while the full r132 Rust suite passed the stale
classification and startup-probe tests. The candidate predicate still requires
a successful per-filesystem startup handle probe; the old production daemon
has not run that candidate probe. The remaining compound records stay
uncertain. This supports the candidate’s classification logic, but the sample
is not a complete event-rate measurement and does not show these failures
caused the overflow or dirty status. See the [00:39 sample](docs/qualification/vps1-estale-window-20261111T0039Z.json), [focused ext4 qualification](docs/qualification/r132-focused-qualification-20261110/README.md), and [r132 Rust regression log](docs/qualification/regression-20261110T1823Z/rust-tests.log).

A separate 30-second status sample at 23:17:47–23:18:17 showed 132,067 new
events seen, 75,130 resolved, 12,544 updates committed, and a net decrease of
6,045 pending updates. The queue drained in this interval while the index
remained dirty and not caught up; the full scan age continued increasing. This
short interval does not establish a sustained service rate, and the counters
have different meanings. The sample is preserved in
[`vps1-backlog-window-20261110T2318Z.json`](docs/qualification/vps1-backlog-window-20261110T2318Z.json).

At 23:28, host I/O PSI crossed the code’s critical threshold (>40%), and the
installed daemon paused index writes. In four ten-second samples PSI fell from
66.71% to 5.73%; Dux resumed writes and pending work fell from its 9,654 peak
to 5,280, with no increase in the cumulative overflow or dropped-update
counters. The dirty marker and multi-day scan age remained. This demonstrates a
short pressure pause/resume path, not recovery of index exactness. See the
[pressure episode evidence](docs/qualification/vps1-critical-io-pause-recovery-20261110T2328Z.json).

The installed daemon also emits a warning for almost every unresolved event: in
the latest hour sample there were 1,991 such warnings (1,975 `ESTALE`) and no
rate-limit suppression fields. The candidate source has a global one-per-minute
warning limiter with a suppressed-event count, and its focused unit test passed
in the recorded Rust suite. At the observed rate this projects to roughly 97%
fewer warning records, but it has not been measured after deployment. Journald
uses 508.6 MiB overall; that total includes all units and cannot be attributed
to Dux. See the [bounded warning-volume sample and projection
limits](docs/qualification/vps1-unresolved-log-volume-20261110T2321Z.json) and
[test log](docs/qualification/regression-20261110T1823Z/rust-tests.log).

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
| Dependency audit | `cargo audit -D warnings` at 00:07 exited 0 for 229 locked dependencies against the 1,446-advisory RustSec DB at commit `28d73fc95e5972af3403225a217def57dcaba3c4`; zero advisories and warnings. | Dependency hygiene only, not a source-code or runtime security review. See [00:07 audit evidence](docs/qualification/cargo-audit-20261111T0007Z.json). |
| Package lifecycle | r132 Ubuntu DEB lifecycle passed in an earlier disposable-VM run. | Current package/rollback path is not VM-qualified; Rocky RPM lifecycle is not qualified. Trusted publisher key/fingerprint is unavailable, so release authenticity is open. |
| Fault matrix and crashes | Some individual runtime tests and live recovery observations passed. The admission-wrapper module passed 41/41 and the complete Python suite now passes. A production and fault-injection binary pair for source commit `f02ba7ad` is staged and their embedded source digest matches. | The complete 15-suite guarded matrix remains unrun; the r132 soak has passed one of five scheduled controlled crash-recovery checkpoints. A fresh 00:04 preflight cleared, but the guarded VM start refused at sample 1/3 when unrelated Go workloads returned; the 00:07 check also failed admission. No matrix suite ran. The matching r132 production/fault binaries remain staged. See [latest admission result](docs/qualification/vm-admission-20261111T0007Z.json), [fault-binary verification](docs/qualification/r132-fault-binary-20261110.json), [admission fix evidence](docs/qualification/vm-admission-wrapper-detection-20261110.md), and the [22:35 guard event](docs/qualification/vm-admission-20261110T2235Z.json). |
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
