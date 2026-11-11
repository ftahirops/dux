# Dux production readiness

**Decision: NOT READY for production rollout.** This report was refreshed at
**2026-11-11 05:59 UTC**. VPS1 still runs the old event-apply 0.6.3 daemon and
is 5.71 days past its last full scan, with 979 lifetime fanotify overflows and
2.52M dropped updates. Pending updates have fallen to 47, but the index remains
dirty. VPS2's older 0.6.3 service remains dirty between scans; at 05:54 it was
again in `reloading` during a scan. The r132 candidate is 20.82 hours into a
72-hour isolated soak; final exactness, history, and SQLite checks remain
pending. The guarded 15-suite fault matrix is still waiting on unrelated Go
build/test workloads and extreme CPU pressure. Release,
scale, package-lifecycle, and canary gates remain open.

The detailed evidence trail is
[`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md); the
latest live host/service sample is
[`live-reconciliation-followup-20261111T0423Z.json`](docs/qualification/live-reconciliation-followup-20261111T0423Z.json),
and the latest consolidated live VPS1/VPS2/soak/admission snapshot is
[`live-progress-20261111T0559Z.json`](docs/qualification/live-progress-20261111T0559Z.json).
The broader aggregate snapshot in
[`current-metrics.json`](docs/qualification/current-metrics.json) is from
03:51 UTC and should be read with these newer samples.

## Live systems and candidate

| System | Latest verified state |
|---|---|
| **VPS1 production** | At 05:48 UTC the installed Dux remained active/running with four historical restarts. It is dirty/not caught up: 979 lifetime overflow notifications, 2,518,862 dropped updates, and 47 pending updates. Last full scan age was 5.71 days; event-mode behind time was 3.51 days. Dux reported 1,268,287 nodes and 118.34 GB indexed bytes; the index database occupied 3.91 GB. Root disk use was 65% (63G available). Low pending work does not establish recovery or exactness. See the [05:49 consolidated snapshot](docs/qualification/live-progress-20261111T0549Z.json). |
| **VPS2 installed service** | At 05:59 UTC the service remained `reloading (reload-notify)` during a scan, with zero historical restarts, pending updates, event drops, and kernel overflows. The scan had run about 8.5 minutes and covered 1,834,260 files and 220,880 directories; last completed scan age was 1,080s. This reproduces the old installed-build service-state reporting defect; the r133 READY-notification fix is not installed. See the [05:59 snapshot](docs/qualification/live-progress-20261111T0559Z.json) and the [05:28 completed cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0528Z.json). |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, isolated on VPS2. At 05:59 it had run 20.91h of 72h (29.04%) with 1,639,435 operations. It has completed 20 mutation bursts, three held-reader/WAL-reclaim checks, and one controlled crash/restart recovery. Across 1,634 exact-convergence samples, p50 was 3.91s, p95 10.34s, and max 20.45s; p50 increased from 1.51s in the first soak quartile to 6.51s in the last. The fixture grows with retained burst files, so this trend is not an isolated daemon regression. Aggregate throughput was 21.783 operations/s against a configured 25/s request, but the aggregate includes burst operations and no explicit throughput SLO is defined. Final exactness, history, and SQLite checks remain pending; four planned crash checkpoints remain. See the [05:59 performance snapshot](docs/qualification/live-progress-20261111T0559Z.json). |
| **Disk and VM admission** | VPS2 root was 90% used with 9.1G available at 05:57; it briefly reached 95% used with 5.66 GB available at 02:28, with allocation source still unconfirmed. The indexed tree ranks ClickHouse data at 20 GiB, containerd at 17 GiB, XGS incidents at 12 GiB, and `/root` qualification/rollback files at 8.5 GiB; indexed totals are not directly comparable with root-device `df` without mount normalization. No files were deleted. The guarded matrix runner remains alive but has started zero suites; at 05:59 host load1 was 52.97 and CPU PSI some avg10 was 80.50%, far above the admission limits. See the [05:59 snapshot](docs/qualification/live-progress-20261111T0559Z.json), [VPS2 admission constraints](docs/qualification/vps2-test-admission-20261111T0525Z.json), [02:28 pressure evidence](docs/qualification/vps2-disk-pressure-20261111T0228Z.json), and [02:05 inventory](docs/qualification/vps2-disk-audit-20261111T0205Z.json). |

At 05:25 UTC, VPS2 was also ruled out as an alternate host for the additional
fault matrix: it has no `/dev/kvm`, its root filesystem is 90% used with 9.6G
available, 2.5G of swap is occupied, I/O PSI full avg10 is 5.47%, and the
installed Dux plus its hash-bound r132 soak are both active. Dux was in a
periodic observation and systemd again showed `reloading`; the service had no
new restarts, drops, or kernel overflows. Starting another runtime workload
there would invalidate the soak's isolation and increase production-host
pressure. Evidence: [VPS2 test admission snapshot](docs/qualification/vps2-test-admission-20261111T0525Z.json).

### Recent live reliability and throughput evidence

At 04:03 and 04:09 UTC on VPS2, full scans each found one inode number
associated with different birth-time incarnations. Both scans failed closed
with one coverage issue and scheduled bounded retries; the status showed zero
fanotify overflows and zero dropped events, so this is a distinct scan-time
inode-reuse race. The third scan completed at 04:20 after 7m42 with 1,815,415
files and zero coverage issues. The 04:43 scan completed in 6m56 and the
04:51 scan in 7m27; both reported zero coverage issues. The latter saw
1,821,622 files and pruned 72 vanished/revalidated 2 inode-reuse paths.
Accounting passed at 04:59:48, 67.135s after that scan completed. Hints changed
during both scans, so the service kept the index dirty. These cycles show that
reconciliation and its independent accounting pass can complete under this
observed churn, but they do not establish a clean current view while hints
continue changing. The daemon
is in scan-first mode: its `caught_up` flag is always false and its
`behind_seconds` field is not applicable; use last-scan age to assess freshness.
Systemd reported `reloading` during both scans on the installed build and
returned to `active/running` afterward. The r133
READY-notification change addresses that service-state reporting defect, but
does not address the inode-reuse conflicts or the need for a fresh observation
after concurrent changes. See the [04:44 observation](docs/qualification/vps2-reconciliation-cycle-20261111T0444Z.json), [05:00 observation](docs/qualification/vps2-reconciliation-cycle-20261111T0500Z.json), and [04:23 live follow-up](docs/qualification/live-reconciliation-followup-20261111T0423Z.json).

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

The latest VPS2 observation started at 02:40:36 and completed at 02:47:40 UTC
(424 s), covering 1,796,215 files and 216,760 directories. It reported zero
coverage issues and 52 vanished inode-reuse paths discarded, with no unresolved
reuse paths. Its scheduled start was deferred briefly at elevated CPU pressure,
then resumed automatically. Independent accounting passed at 02:49:04 in 83.5 s. At 02:49 the
service had zero restarts, overflows, dropped updates, and pending updates, but
was dirty/not caught up because filesystem hints changed during the scan. The
installed unit has no `User=` override, so it still runs as root despite the
candidate package unit's dedicated `dux` account. This is a live correctness
observation and a deployment/security gap, not proof of exact freshness. See
the [02:49 cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0249Z.json).

At 02:49 the isolated r132 soak had run 17h44m (24.63% of its 72-hour target),
with 1,406,747 operations, 1,402 exactness samples, 17 mutation bursts, one
controlled crash recovery, and two held-reader WAL reclaim checks. Sampled
convergence p95 was 9.002 s and maximum 17.393 s. Terminal exactness, history,
and SQLite integrity are still pending; these interim numbers do not qualify
the candidate. See the [02:49 soak checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261111T0249Z.json).

VPS2 root space changed from 5.66 GiB available at 02:28 to 11.34 GiB at
02:49. This confirms a short-lived disk-pressure episode; available evidence
does not identify its allocation source. ClickHouse was highly active during
the peak, but cumulative block-I/O counters do not establish causation. A
roughly 518 MiB ClickHouse part captured in the completed scan was absent by
02:37; that confirms some transient indexed data but cannot explain the full
5.82 GiB space recovery. No cleanup was performed. See the [pressure sample
and follow-up](docs/qualification/vps2-disk-pressure-20261111T0228Z.json).

Latest evidence: [02:49 reconciliation cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0249Z.json), [02:28 disk pressure sample](docs/qualification/vps2-disk-pressure-20261111T0228Z.json), [02:49 soak checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261111T0249Z.json), and [02:26 VM admission refusal](docs/qualification/vm-admission-20261111T0226Z.json). Earlier VPS2 evidence: [02:04 cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0204Z.json) and [02:05 disk audit](docs/qualification/vps2-disk-audit-20261111T0205Z.json).

## Tests, coverage, and open release gates

### Newly found service-state defect (2026-11-11)

At 03:01 UTC, VPS2 systemd reported `dux.service` as
`reloading (reload-notify)` since 04:23 UTC the prior day, while the daemon was
alive and reconciling. By 03:08 UTC it had returned to `active/running` after a
completed observation. The watcher source was sending `RELOADING=1` for
ordinary full observations and rebuilds. That notification means a service
configuration reload to systemd, so normal indexing was reported as a service
reload for the duration of the observation. The workspace now sends `READY=1`
with the same scan status instead;
the added regression test covers both observation modes. On this modified
workspace, the complete Rust suite passed **342 tests, 0 failed, 2 ignored**;
both ignored cases (ext4 stale-parent classification and the 100k-entry scale
smoke on `/dev/shm`) then passed when run explicitly. Strict Clippy and
`cargo fmt --check` also pass. One initial 100k invocation on ext4 was refused
by the fixture safety guard before creating data; the `/dev/shm` rerun passed.

This correction is not included in the staged r132 binaries or installed on
either VPS. A fresh production-mode release binary has been built locally from
the updated workspace, and a matching fault-injection binary now has the same
embedded source digest. Both are staged read-only under `/dev/shm` but have not
been qualified in the package's full sandbox. An A/B smoke under systemd 255
paused the new fault-injection binary during reconciliation: it stayed
`active/running`; the old r132 control entered `reloading` at the same point.
This validates notification behavior, but it did not exercise the package's
full capability/syscall sandbox or the production-mode binary. The guarded VM
stopped before the fault matrix could start when unrelated Go tests returned
on the host. The live production host was not modified.

This workspace regression run is separate from the r132 candidate evidence
below. Its Rust instrumentation coverage was not measured. The existing 72.61%
line coverage is from the earlier r132 instrumented build and must not be
presented as coverage of this fix. Detailed commands and outcomes are recorded
in the [workspace regression evidence](docs/qualification/workspace-regression-20261111T0306Z.json).
The 03:08 VPS2 service and index observation is in
[`current-metrics.json`](docs/qualification/current-metrics.json).
The locally built production artifact and its hash are recorded in the
[workspace release build evidence](docs/qualification/workspace-release-build-20261111T0315Z.json).
The matching current production/fault binary pair is recorded in the
[r133 build-pair evidence](docs/qualification/r133-build-pair-20261111T0319Z.json).
The direct systemd A/B result is in the
[reconciliation-state smoke record](docs/qualification/systemd-reconciliation-state-smoke-20261111T0329Z.json).

| Area | Latest evidence | Still open |
|---|---|---|
| Rust automated tests | Latest workspace full suite: 342 passed, 0 failed, 2 ignored; both ignored cases then passed explicitly (ext4 stale-parent and 100k-entry reconciliation). Strict Clippy and formatting passed. Earlier r132 evidence also passed the ext4 case under UID 65534 with planned service capability masks. | Coverage instrumentation was not rerun for the workspace fix; the 100k timing is debug-profile and is not a production-scale benchmark. The non-root test used UID 65534 because the `dux` account is not installed on this host. |
| Python qualification harness | Latest full suite: 384 tests in 29.597s, 0 failures, 1 skipped (the RPM signing fixture); this includes the three new exact-mismatch status regressions and admission-wrapper tests. A preceding run under a 5% CPU cap had 11 subprocess-timeout errors; every case passed on the normal-priority rerun. | Python statement coverage is 53% for the package lifecycle script only and does not include the new diagnostic helper; branch coverage is unavailable. See [latest focused diagnostic evidence](docs/qualification/scan-first-behind-seconds-diagnostic-regression-20261111T0451Z.json) and [prior full-suite rerun record](docs/qualification/python-suite-rerun-20261110T2205Z.md). |
| Rust instrumentation | 72.61% lines, 71.44% regions, 76.19% functions. | Branch counters unavailable; critical syscall and race paths remain incompletely covered. |
| Dependency audit | Refreshed `cargo audit -D warnings --json` at 03:37 fetched the advisory database and exited 0 for 229 locked dependencies; zero advisories or warnings. The database HEAD remained `28d73fc95e5972af3403225a217def57dcaba3c4`, last commit 2026-11-10 13:45 UTC. `Cargo.lock` SHA-256 is `fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`. | Dependency hygiene only, not a source-code or runtime security review. See [03:37 audit evidence](docs/qualification/cargo-audit-20261111T0337Z.json). |
| Package lifecycle | The current clean commit `6db90925` now has matching static-musl production/fault binaries and DEB/RPM **revision 0.6.3-67**. The manifest and artifact checksum list verify; DEB and RPM payload executable hashes equal the standalone binary; RPM header/payload digests and both packaged service units verify. The embedded source digest matches between production and fault binaries. Revision 67 sorts above the installed 66. The 229-dependency audit reports zero advisories/warnings. See [revision-67 artifact verification](docs/qualification/r133-package-revision-67-20261111T0405Z.json). | Artifacts are unsigned local qualification builds. Upgrade/install/rollback and package-created service-account lifecycle still need the guarded VM; trusted publisher key/fingerprint remains unavailable. |
| Fault matrix and crashes | Individual runtime tests and live recovery observations passed; the admission-wrapper module passed 41/41. Matching current-commit r133 static production/fault binaries are staged with equal embedded source digests. A systemd 255 A/B smoke reproduced old `reloading` behavior and confirmed new `active/running` during a paused reconciliation; the static production binary also started under a transient `Type=notify` unit and remained `active/running`. | The full 15-suite guarded matrix remains unrun; the r132 soak has passed one of five scheduled controlled crash-recovery checkpoints. A further guarded VM attempt at 04:02 was stopped before tests when Go build/test workloads appeared; zero suites started. See the [04:05 revision-67 and admission evidence](docs/qualification/r133-package-revision-67-20261111T0405Z.json), the earlier [03:34 admission record](docs/qualification/vm-admission-20261111T0334Z.json), [systemd A/B smoke](docs/qualification/systemd-reconciliation-state-smoke-20261111T0329Z.json), and [production-binary systemd smoke](docs/qualification/r133-static-production-systemd-smoke-20261111T0351Z.json). |
| Scale and platform | 100k-file churn, 1.37M-path scale, forced overflow recovery, non-root probe, and repeated VPS2 scans have prior evidence. | 10M-file scale, broader filesystem/kernel matrix, and exact path-level freshness remain unqualified or unmeasured. |
| Systemd sandbox event visibility | Exact r132 binary in a separate mount namespace with package-equivalent capabilities, syscall filters, `ProtectKernelTunables`, and `ProtectControlGroups` indexed host-namespace create, rename, and delete events on ext4; create appeared in 187 ms, with zero test-group overflows or drops. r133 A/B notifications are verified, and its static production binary starts as `active/running` under systemd `Type=notify`. | Actual package-created UID, r133 package sandbox behavior, install/upgrade/rollback lifecycle, other filesystems/kernels, and long-duration packaged service remain open. See [namespace event result](docs/qualification/systemd-hardening-20261110/cross-namespace-event-test-20261110T2004Z.json), [notification A/B smoke](docs/qualification/systemd-reconciliation-state-smoke-20261111T0329Z.json), and [production-binary smoke](docs/qualification/r133-static-production-systemd-smoke-20261111T0351Z.json). |
| Production canary | Not started. | VPS1 rollback-ready canary, service-account migration, and rollback evidence remain required. |

The latest full Rust and Python runs plus the two targeted Rust cases cover **727 distinct passing cases, 0 failures, and 1 skipped**: 342 Rust full-suite passes, two separately executed manual Rust cases, and 383 Python passes (384 discovered, one skipped). The ordinary Rust full-run output lists those two manual cases as ignored, so they are counted only once here. The 75 package lifecycle/security tests are included in the Python total. This is a test-case total across those recorded runs, not a statement of complete behavioral coverage. See the [focused qualification report](docs/qualification/r132-focused-qualification-20261110/README.md), the [current workspace regression record](docs/qualification/workspace-regression-20261111T0306Z.json), the [regression evidence](docs/qualification/regression-20261110T1823Z/README.md), and the [coverage gap review](docs/qualification/coverage-gaps-20261109.md).

Since those full runs, a focused regression was added for the exact-mismatch
diagnostic helper. Its three tests pass: two scan-first cases verify that the
inapplicable event-drain age is omitted, with the legacy fallback deriving
scan freshness and native JSON retaining it; one event-apply case verifies
that the numeric backlog age remains available.
The full Python suite was rerun after adding these cases and passed 384 tests
(383 passed, 1 skipped) in 29.597 seconds. Across the recorded full and
focused Rust/Python runs this is **727 distinct passing cases, 0 failures,
and 1 skipped**. See the [focused diagnostic regression evidence](docs/qualification/scan-first-behind-seconds-diagnostic-regression-20261111T0451Z.json).

At 05:27 UTC, the same three focused diagnostic tests were rerun from workspace
commit `7d92ce68`: **3 passed, 0 failed, 0 skipped** in 0.235 seconds. This is
a low-cost regression check while the guarded VM remains inadmissible; it does
not replace or expand the full suite or runtime qualification. See the
[focused rerun record](docs/qualification/exact-mismatch-followup-focused-rerun-20261111T0527Z.json).

At 05:41 UTC, two focused Rust runs on the same source passed **10 distinct
tests, 0 failed, 0 ignored**: the direct totals verifier test rejected three
corruption shapes, and nine rollup tests covered drift detection, recovery,
cross-directory mutation, overflow fallback, and nested artifact churn. This
strengthens correctness evidence for the accounting invariant but does not
measure the full production database's 85-second verification cost. See the
[focused accounting test record](docs/qualification/accounting-integrity-rust-tests-20261111T0542Z.json).

At 05:44 UTC, a separate focused scan-first Rust run passed **16 tests, 0
failed, 0 ignored** in 0.23 seconds. It includes pending-accounting
invalidation after a late hint, fail-closed malformed/unresolvable hint cases,
manual follow-up preservation, fixed-cadence recovery, and scan-first status
reporting. The guarded fault-matrix runner simultaneously entered its required
120-second quiet admission window; no matrix suite had started at the time of
the test record. See the [focused scan-first test record](docs/qualification/scan-first-rust-focused-tests-20261111T0544Z.json).

### Accounting-query optimization experiment — 2026-11-11 05:39 UTC

A low-priority, temporary 20k-file SQLite fixture compared the existing
accounting predicate with and without a proposed covering index on
`dirents(parent_dev,parent_inode,prime,dev_id,inode)`. The interleaved pairs
did not show a repeatable improvement: baseline/indexed medians were
33.05/33.48 ms, then 35.83/32.66 ms. The runs overlapped in range and the
winning direction changed between pairs. A separate same-cardinality fixture
grew by 327,680 bytes after adding the index. This is insufficient evidence to
add the index, especially without measuring production write amplification.
No source, schema, or live database change was made. The 100k fixture expansion
failed its initial synthetic-validity assertion and was discarded; it is not
counted as a product failure or a passing run. Full details and limitations are
in the [microbenchmark evidence](docs/qualification/accounting-covering-index-microbenchmark-20261111T0539Z.json).

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
