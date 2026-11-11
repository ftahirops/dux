# Dux production readiness

**Decision: NOT READY for production rollout.** This report was refreshed at
**2026-11-11 03:34 UTC**. VPS1 still runs the old event-apply 0.6.3 daemon and
is more than five days past its last full scan, with accumulated fanotify
overflows and dropped updates. VPS2 still runs the older root-owned 0.6.3
binary; r132 remains isolated in a separate soak. Release, scale, fault-matrix,
and canary gates remain open. A green unit suite or a successful VPS2
observation does not close those gates.

The detailed evidence trail is
[`PRODUCTION_READINESS_STATUS.md`](PRODUCTION_READINESS_STATUS.md); the
machine-readable snapshot is
[`current-metrics.json`](docs/qualification/current-metrics.json).

## Live systems and candidate

| System | Latest verified state |
|---|---|
| **VPS1 production** | At 03:31 UTC, the installed Dux remained 0.6.3 build `e0cc915f8f37`, active/running as root with four historical restarts. It is dirty/not caught up from fanotify overflow: 933 lifetime overflow notifications, 2,518,862 dropped updates, and 460 pending updates. Its last full scan was 5.61 days ago; event-mode behind time is 3.42 days. Dux reports 1,287,209 nodes and 122.28 GB indexed bytes; the index database occupies 3.83 GB. Root disk use is 67% with 63.77 GB available. See the [03:31 VPS1 live status](docs/qualification/vps1-live-status-20261111T0331Z.json) and the [pressure pause/resume sample](docs/qualification/vps1-critical-io-pause-recovery-20261110T2328Z.json). |
| **VPS2 installed service** | At 03:24 UTC, Dux 0.6.3 build `8b02799b` was `active/running` with zero restarts. Its last scan completed at 03:16:18; the 2,014,970-node index was dirty and not caught up, with no scan in progress and zero pending updates, overflow notifications, or drops. Dirty reason: `filesystem changes await the next full observation`. See [03:24 live status](docs/qualification/vps2-live-status-20261111T0324Z.json) and the [02:49 full reconciliation cycle](docs/qualification/vps2-reconciliation-cycle-20261111T0249Z.json) for detailed traversal/accounting results. |
| **r132 soak candidate** | Static candidate SHA-256 `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, isolated on VPS2. At 03:34 it had run 18h30m of the 72-hour target (25.69%), with 1,464,000 operations, 1,460 exactness samples, 18 mutation bursts, three held-reader/WAL-reclaim passes, and one controlled crash/restart recovery. Sampled convergence p95 is 9.243s, max 17.963s. Final exactness, history, and SQLite checks remain pending, as do later crash checkpoints. See the [03:34 checkpoint](docs/qualification/r132-72h-soak-vps2-20261110/soak-live-checkpoint-20261111T0334Z.json). |
| **Disk and VM admission** | At 03:24 VPS2 root was 89% used with 11.08 GB available. It briefly reached 95% used with 5.66 GB available at 02:28; the allocation source remains unconfirmed. The indexed tree ranks ClickHouse data at 20 GiB, containerd at 17 GiB, XGS incidents at 12 GiB, and `/root` qualification/rollback files at 8.5 GiB; Dux index totals are not directly comparable with root-device `df` without mount normalization. No files were deleted. See [03:24 live status](docs/qualification/vps2-live-status-20261111T0324Z.json), [02:28 disk pressure evidence](docs/qualification/vps2-disk-pressure-20261111T0228Z.json), and [02:05 disk inventory](docs/qualification/vps2-disk-audit-20261111T0205Z.json). The guarded VM admission at 03:34 refused before QEMU: load1 24.67, CPU PSI 77.76%, and unrelated Go/Node compiler/test activity. No fault-matrix suite began; see the [admission record](docs/qualification/vm-admission-20261111T0334Z.json). |

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
| Python qualification harness | Latest full suite: 381 tests in 17.800s, 0 failures, 1 skipped (the RPM signing fixture); this includes the admission-wrapper regression. A preceding run under a 5% CPU cap had 11 subprocess-timeout errors; every case passed on the normal-priority rerun. | Python statement coverage is 53% for the package lifecycle script only; branch coverage is unavailable. See [rerun record](docs/qualification/python-suite-rerun-20261110T2205Z.md). |
| Rust instrumentation | 72.61% lines, 71.44% regions, 76.19% functions. | Branch counters unavailable; critical syscall and race paths remain incompletely covered. |
| Dependency audit | `cargo audit -D warnings` at 00:07 exited 0 for 229 locked dependencies against the 1,446-advisory RustSec DB at commit `28d73fc95e5972af3403225a217def57dcaba3c4`; zero advisories and warnings. | Dependency hygiene only, not a source-code or runtime security review. See [00:07 audit evidence](docs/qualification/cargo-audit-20261111T0007Z.json). |
| Package lifecycle | r132 Ubuntu DEB lifecycle passed in an earlier disposable-VM run. | Current package/rollback path is not VM-qualified; Rocky RPM lifecycle is not qualified. Trusted publisher key/fingerprint is unavailable, so release authenticity is open. |
| Fault matrix and crashes | Individual runtime tests and live recovery observations passed; the admission-wrapper module passed 41/41. Matching r133 production/fault binaries are staged with equal embedded source digests. A systemd 255 A/B smoke reproduced old `reloading` behavior and confirmed new `active/running` behavior during a paused reconciliation. | The full 15-suite guarded matrix remains unrun; the r132 soak has passed one of five scheduled controlled crash-recovery checkpoints. A guest admitted at 03:10 was stopped at 03:12 when Go work resumed; a fresh 03:34 attempt was refused before QEMU at load1 24.67/CPU PSI 77.76%. Zero matrix suites started. See the [03:34 admission record](docs/qualification/vm-admission-20261111T0334Z.json), [r133 binary pair](docs/qualification/r133-build-pair-20261111T0319Z.json), and [systemd A/B smoke](docs/qualification/systemd-reconciliation-state-smoke-20261111T0329Z.json). |
| Scale and platform | 100k-file churn, 1.37M-path scale, forced overflow recovery, non-root probe, and repeated VPS2 scans have prior evidence. | 10M-file scale, broader filesystem/kernel matrix, and exact path-level freshness remain unqualified or unmeasured. |
| Systemd sandbox event visibility | Exact r132 binary in a separate mount namespace with package-equivalent capabilities, syscall filters, `ProtectKernelTunables`, and `ProtectControlGroups` indexed host-namespace create, rename, and delete events on ext4; create appeared in 187 ms, with zero test-group overflows or drops. The r133 notification fix also passed a transient-unit A/B state check. | Actual package-created UID, the r133 package sandbox, install/upgrade/rollback lifecycle, other filesystems/kernels, and long-duration packaged service remain open. See [namespace event result](docs/qualification/systemd-hardening-20261110/cross-namespace-event-test-20261110T2004Z.json) and [notification A/B smoke](docs/qualification/systemd-reconciliation-state-smoke-20261111T0329Z.json). |
| Production canary | Not started. | VPS1 rollback-ready canary, service-account migration, and rollback evidence remain required. |

The latest full Rust and Python runs plus the two targeted Rust cases cover **724 distinct passing cases, 0 failures, and 1 skipped**. The ordinary Rust full-run output lists its two manual cases as ignored; both were separately executed and passed. See the [focused qualification report](docs/qualification/r132-focused-qualification-20261110/README.md) and the [current workspace regression record](docs/qualification/workspace-regression-20261111T0306Z.json). The 75 package lifecycle/security tests
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
