# Scan-first pressure and CLI settle qualification — 2026-11-09

This record covers an isolated ext4 pressure test of Dux release 107 and a
runtime test of a scan-command status race fixed in the current source tree.
It also records the induced failure of the pre-existing release-106 VPS2
soak. The release-106 canary was stopped by its fail-closed monitor and is not
a passing soak.

## Release-107 controlled filesystem burst

On VPS2, a 2 GiB sparse loop image was formatted ext4 and mounted at
`/mnt/dux-r107-stress`. The release-107 static binary
(`c301574f…`, source commit `d27d3b7`) ran in its own transient systemd unit,
watching only that filesystem. The unit had a 100% one-core CPU quota, 1 GiB
memory limit, the packaged capability set and syscall filter, and
`--max-cpu 60`. The host fanotify queue limit was 16,384 events.

The generator performed **11 bursts × 32,000 create/unlink pairs**: 352,000
pairs, about 704,000 filesystem operations. Individual bursts took 0.353–0.620
seconds. Release 107 recorded 975,442 hint events with **zero kernel overflow
notifications**, zero userspace drops, and no daemon restart. Its pressure
telemetry sampled at most 45,949 hint events/s; sampled queue bytes peaked at
24 and no queue-pressure episode was recorded. These are sampled queue-byte
measurements, not proof that the queue never briefly grew between samples.

After the bursts, the candidate's full observation found the expected fixture
(1 file, 13 directories, 14 path instances) with zero coverage issues;
accounting verification passed. The separate release-106 root service saw its
overflow count rise from 0 to 2 during the same test interval. This is a
single-host controlled comparison, not a repeated performance benchmark or a
qualification of every kernel/filesystem.

## Scan CLI settle race

The earlier candidate observation could finish its walk while the watcher
still held a transient uncertain status. `dux scan` then returned “stale or
uncertain” immediately, even though the independent accounting check cleared
the status a few seconds later. The source now allows up to 10 seconds for
that post-observation status to settle, while still returning uncertainty if
the index does not become clean.

The updated source binary (`53529db2…`, source tree hash
`29154f16…`, built from `8b47815` plus the uncommitted source change) was run
under a separate transient systemd unit on the ext4 fixture. With the index
dirty because of a newly created test file, `dux scan` waited through the
observation and accounting check and returned success in **4.50 seconds**.
The final status had `dirty=false`, zero overflows and drops, 15 indexed path
instances, and the new file was returned by `dux find`. No package was
installed and the production service was not touched.

## Release-106 soak interruption

The existing 24-hour release-106 VPS2 canary passed samples 1–8. Sample 9
failed because its service restart count changed and it observed two overflow
notifications. This was a **test-induced canary interruption**, not an
uncontrolled release-106 crash: adding the loop mount changed filesystem
coverage, and at 00:03:03 UTC the daemon exited with status 1 and the explicit
message “filesystem mounts changed; restart daemon to refresh watch coverage
and reconcile”; systemd restarted it at 00:03:33. The collector's sample 8
service snapshot preceded that mount change, so sample 9 correctly detected
the restart. The failed sample is preserved in the neighboring v4 JSONL; no
24-hour claim can be made from it. This mount-change restart is expected
coverage-refresh behavior and the test environment was cleaned afterward.

## Checks and anomalies

- Rust release tests after the source fix: **322 passed, 0 failed, 2 ignored**.
- Strict all-target/all-feature Clippy with warnings denied: passed.
- Formatting check and `git diff --check`: passed.
- One earlier full run had a single `ENOENT` in the ext4 inode-reuse test; its
  focused rerun passed, followed by a clean full-suite rerun. Treat that as a
  test flake to monitor; do not count it as a product failure or silently omit
  it from the history.
- The 10-second CLI settling rule is tested with deterministic boundary tests
  and the ext4 runtime reproduction above. A persistent dirty status still
  returns an uncertainty message after the bounded settle period.
- Current-source release tests were rerun locally: **322 passed, 0 failed,
  2 ignored**. The ignored tests remain filesystem/scale qualification gates.
- Current-source llvm-cov ran 324 tests and reports 72.16% line, 70.98% region,
  and 76.03% function coverage. Stable llvm-cov reports no branch totals.
  Module-level results and the complete test output are in the preserved log.
- Current-source strict Clippy (`--all-targets --all-features -D warnings`),
  formatting, and diff checks passed.

## Qualification harness correction and release-108 candidate

Release 108 was built from clean commit `0fc18fc` as static musl, DEB, and RPM
artifacts. Its build manifest records source digest
`6884e20a…a671a8df7`; the binary embeds the same digest and commit. All eight
published artifact checksums passed, and the release script completed both
the 229-dependency lockfile audit and the exact binary audit. This candidate
was not installed.

The first isolated readiness run failed before completing its first crash
round. The harness required scan-first status to become event-pipeline
“certain,” even though scan-first treats those events as hints and can remain
conservatively uncertain. The fixture's initial observation and accounting
verification had completed, but the harness waited 90 seconds for the legacy
status condition. The failure report and log are preserved; this is a harness
incompatibility and is not counted as a product pass or product data-loss
failure.

The harness now detects scan-first mode, requests a new full observation,
waits for the independent accounting verifier, and compares the full indexed
filesystem truth. The updated harness passed all **331 Python tests**. The
release-108 production binary then passed an isolated crash/restart smoke test
with exact post-recovery filesystem comparison: one crash round passed and
recovered in 1.909 seconds.

The next run exposed a second harness assumption. It selected fault-injection
stages from the atomic rebuild path (`scan.build_complete` and `scan.install_*`)
even though the candidate had a ready v7 baseline and reconciled in place.
Those callbacks are not reached by in-place reconciliation. The failure is
preserved and did not show an index mismatch. The in-place implementation now
has test-only fault checkpoints at observation start, projection-ready, before
the publication transaction commits, and immediately after commit. The
scan-first crash matrix selects those checkpoints; event-apply mode retains
the existing event-flush and atomic-rebuild stages. The checkpoints compile to
no-ops in production builds.

Release 110 was built from clean commit `6dacdf5`; its static binary, DEB, RPM,
source archive, debug symbols, and all eight checksum entries verified. The
manifest and binary agree on source digest `4b24695c…d9356bf51`; both the
lockfile and exact binary audits passed. On VPS2, the candidate and matching
fault-injection binary passed all **8 crash/restart rounds**, including each of
the four in-place checkpoints and four random-timing rounds. Every recovery
passed SQLite integrity and exact filesystem/index comparison; recovery took
0.736–1.109 seconds. The worker was limited to 25% CPU and 4 GiB memory on a
768 MiB disposable ext4 image. This qualifies the exercised crash boundaries,
not the broader release matrix or long soak.

### Corrected release-110 harness reruns

The combined rerun used the same release-110 product binary and fault binary
with the corrected scan-first CPU gate. It passed **30/30 crash rounds**, the
real kernel-overflow recovery case, and the CPU burst case in one run. The CPU
case observed 12,008 hints, no overflow, and about 0.01 event-thread cores
(rounded by the report); full exact validation completed. The run then stopped
at coverage because that harness assertion expected a successful exit for an
intentionally incomplete unprivileged scan. The scan correctly returned
nonzero and preserved its incomplete-coverage state. The harness now expects
that fail-closed result, and coverage passed on rerun.

The capability case also had an event-apply assumption. Under scan-first, a
full observation with `DAC_READ_SEARCH` removed still matched filesystem truth
and reported a completed reconciliation; Dux conservatively retained the dirty
marker while waiting for a later observation. Restarting with normal
capabilities then passed exact recovery. This establishes exact data for the
tested fixture while retaining visible uncertainty; it does not establish a
clean status without the capability.

The remaining combined runs did not complete. The OOM run was stopped while
the harness was waiting on a scan-first exactness path after restart. The
pressure suite failed at a gate that waits for the atomic-rebuild staging DB;
the in-place scan-first implementation has no such staging file. Those suites
remain unqualified pending mode-specific gates. Disk-full and 100,000-file
churn remain skipped because VPS2 has insufficient free `/dev/shm` for their
required fixtures. Mount, WAL, startup, retry, root, and soak suites were not
completed in the final rerun. No 72-hour soak or production canary is claimed.

The artifacts preserve both the initial failures and corrected reruns:

- [`release110-final-harness-failure.json`](live/release110-final-harness-failure.json)
- [`release110-final-harness-followup2.json`](live/release110-final-harness-followup2.json)
- [`release110-capability-recheck2.json`](live/release110-capability-recheck2.json)
- [`release110-final-independent.json`](live/release110-final-independent.json)
- [`release110-final-rest3-aborted.json`](live/release110-final-rest3-aborted.json)

The final local harness regression run passed **337 Python tests** after these
corrections. Rust product code did not change in this harness-only iteration.
The standard release test invocation passed **319 tests with 2 ignored**;
the all-features test output recorded earlier passed **322 with 2 ignored**.
Strict Clippy, formatting, and diff checks passed. Coverage figures remain the
recorded Rust run: 72.16% lines, 70.99% regions, and 76.03% functions, with no
branch totals from stable `llvm-cov`.

## Artifacts

- [`release-107-stress-service.log`](live/release-107-stress-service.log)
- [`release-107-scan-settle-service.log`](live/release-107-scan-settle-service.log)
- [`release-106-unit-events.log`](live/release-106-unit-events.log)
- [`release-106-canary-v4.jsonl`](live/release-106-canary-v4.jsonl)
- [`current-source-release-tests.log`](live/current-source-release-tests.log)
- [`current-source-llvm-cov.log`](live/current-source-llvm-cov.log)
- [`current-source-clippy.log`](live/current-source-clippy.log)
- [`release108-old-harness-failure.json`](live/release108-old-harness-failure.json)
- [`release108-old-harness-failure.log`](live/release108-old-harness-failure.log)
- [`release108-scanfirst-crash-smoke.json`](live/release108-scanfirst-crash-smoke.json)
- [`release108-scanfirst-crash-smoke.log`](live/release108-scanfirst-crash-smoke.log)
- [`release109-atomic-checkpoint-harness-failure.json`](live/release109-atomic-checkpoint-harness-failure.json)
- [`release109-atomic-checkpoint-harness-failure.log`](live/release109-atomic-checkpoint-harness-failure.log)
- [`release109-inplace-checkpoint-smoke-failure.json`](live/release109-inplace-checkpoint-smoke-failure.json)
- [`release109-inplace-checkpoint-smoke-failure.log`](live/release109-inplace-checkpoint-smoke-failure.log)
- [`release110-crash8.json`](live/release110-crash8.json)
- [`release110-crash8.log`](live/release110-crash8.log)
- [`release110-cpu4.json`](live/release110-cpu4.json)
- [`release110-final-harness-failure.json`](live/release110-final-harness-failure.json)
- [`release110-capability-recheck2.json`](live/release110-capability-recheck2.json)

The temporary loop image, test database, transient candidate units, and copied
test executables were removed after these records were collected.

### Current working-tree verification refresh (2026-11-09)

After the read-only upgrade-source retry fix in `src/watch.rs` was added, the
current working tree passed `cargo test --locked --release --all-features
--bin dux`: **323 passed, 0 failed, 2 ignored** (325 discovered). The ignored
tests are the manual production-shaped scale run and the ext4-handle-specific
stale-parent integration test. The focused real ext4 inode-reuse test also
passed separately. Current Python discovery passed **337/337** tests.

Coverage was refreshed on the same working tree with `cargo llvm-cov --locked
--all-features --summary-only --bin dux`: **72.20% line** (18,309/25,359),
**71.02% region** (29,064/40,923), and **76.05% function** (1,280/1,683).
The stable llvm-cov output reports no branch totals. Clippy with warnings
denied, formatting, and `git diff --check` had passed on this working tree.
At this point release-115 below has rerun the startup-upgrade integration case
against the fixed source. The refresh here records the local source tests and
coverage; it does not qualify a production deployment.

### Release-115 startup and upgrade qualification (2026-11-09)

The static musl release-115 candidate and matching fault-injection build were
run on VPS2 (Ubuntu 24.04, kernel 6.8, ext4 disposable 768 MiB loop image;
worker limits 25% CPU and 4 GiB memory). The startup suite **passed in 63.0 s**.
It proved: failed v4 upgrade retained its readable legacy index; daemon stayed
alive; queued changes survived the automatic retry; retry installed schema v7;
watches were active before initial and upgrade scans; legacy queries and
history remained available; and stale directory inode rows were repaired.
The deliberate live inode-kind corruption was repaired by a complete scan-first
observation, not direct fanotify event application; the report now labels that
behavior accurately.

Artifacts: [`release115-startup.json`](live/release115-startup.json). Candidate
SHA256 `f67ed8531a8891d40b5f1694ff760c748dfe4bcddb9a6c506c23e6f0e57d7135`;
matching fault binary SHA256
`10ac111df349c6c753c3f8d10669d7ca3295e7d16116f403c2487e2ef735954a`;
both embed source digest
`caa8bb3189b6a5057ab21ac5b2fd1b75ac980ca9c8dae0a24330f77fce4f8347`.
The release build generated and checked its binary/package checksums, static
musl artifact, DEB, RPM, and source archive. Cargo audit found **0 known
vulnerabilities** in 229 locked dependencies using 1,422 advisories; the
auditable binary includes metadata for 202 dependencies.

The full current Python discovery now passes **340/340 tests** after the
harness corrections. The current Rust suite remains **323 passed, 0 failed,
2 ignored**, with 72.20% line, 71.02% region, and 76.05% function coverage; no
branch totals are reported. Clippy with warnings denied passed earlier after
the Rust source fix; the subsequent edits were limited to Python harness,
Python tests, and documentation. This startup run closes that integration gate
for the release-115 candidate. It does not satisfy the unrun pressure, disk,
churn, retry/root, long-soak, package lifecycle, or production-canary gates.
The release-114 JSON is retained as a prior successful run whose evidence
label incorrectly described a scan-first observation as fanotify repair;
release-115 is the correctly classified record.

### Release-115 scan-first pressure qualification (2026-11-09)

The release-115 candidate then passed the scan-first pressure suite on VPS2 in
110.9 seconds. It used a disposable 768 MiB ext4 loop filesystem seeded with
20,000 files, under a 25% one-core CPU quota and 4 GiB memory limit. All four
scenarios passed: reconciliation cancelled during synthetic high load and
Critical pressure, then recovered the changed filesystem exactly; it completed
during CPU Elevated and I/O Elevated pressure and included each mutation in the
published generation. The daemon remained under its transient qualification
unit; the production service was not changed.

Evidence: [`release115-pressure.json`](live/release115-pressure.json), SHA256
`94e1c39453d1e2770e6f3e3a1f399b8a8c531da946982edbd50e684270deaea2`.
Candidate SHA256 is the same release-115 binary recorded above. The pressure
harness copied to VPS2 had SHA256
`4c4f350916d79eb9686f296e8fd63420b28773bf1ed6330fe847c4c59171efb8`.
This validates the four exercised pressure policy cases; it is not a kernel
overflow, sustained throughput, disk-full, long-soak, or production-canary
qualification.

The refreshed local Python discovery suite passes **341/341 tests**. The
current Rust all-features suite remains **323 passed, 0 failed, 2 ignored**;
coverage remains 72.20% line, 71.02% region, and 76.05% function. These are
separate suites and must not be added together as unique test cases.

### Release-117 expanded matrix checkpoint and crash-stage cleanup — 2026-11-09 05:52 UTC

Release 117 is running on VPS2 under a disposable ext4 loop image (100,000
seeded files, 2 GiB image, worker quota 25% of one CPU and 4 GiB memory). At
this checkpoint, the 8-round crash suite and real fanotify overflow suite
passed; the CPU suite passed; churn is still running. This is a progress
snapshot, not the final matrix report. The snapshot is
[`release117-p0-matrix-progress-0552.json`](live/release117-p0-matrix-progress-0552.json),
SHA256 `0463e5bb461395f54896cf3f067ec99046349473ed203622333c9d5152c6fb7f`.

Crash recovery passed all 8 rounds, covering each of the four in-place scan
fault boundaries once and four random timings. Recovery took 95.785–209.786
seconds per round. The real overflow case used a 16,384-event queue and created
18,432 files; it observed exactly one overflow, completed reconciliation, and
matched the fixture exactly in 99.055 seconds. Its recorded
`clean_status_after_recovery` is false, so this proves exact accounting after
reconciliation but does not pass a clean-status gate. The CPU case drained
12,000 hints in 0.141 seconds after a 5.898-second production burst, with no
overflow; exact validation took 113.471 seconds against a 120-second limit.
The measured event-thread CPU was 0.014 cores against a 0.10-core setting.

Inspection of the disposable crash-test database found four abandoned
in-place scan stage databases from killed processes (three about 28 MiB and
one 24 KiB). The existing stale-stage cleanup only covered atomic rebuild
stages, so a later in-place observation could leave these files consuming disk.
The current source now runs the existing generated-name and regular-file-only
cleanup before an in-place observation, while holding the database's exclusive
lock. A Rust regression test seeds a stale stage and WAL plus an unrelated
lookalike, then verifies the stale files are removed and the unrelated file is
preserved. The focused Rust regression passed. The crash harness now asserts
that recovery leaves no generated stage files and records that result. These
source and harness changes were made after release 117 was built, so the active
release-117 matrix does not yet qualify this fix.

Current working-tree checks after this fix: **324 Rust tests passed, 0 failed,
2 ignored** (326 discovered); **343 Python tests passed**; strict all-target,
all-feature Clippy, formatting, and diff checks passed. Fresh llvm-cov reports
**72.25% line, 71.09% region, 76.07% function** coverage and no branch totals.
The current coverage table is preserved in
[`current-source-llvm-cov-20261109-recheck.log`](live/current-source-llvm-cov-20261109-recheck.log);
the earlier detailed coverage log remains unchanged for historical comparison.
The local tests include the stale-stage regression; the release-117 remote
binary does not.

Release 117 candidate SHA256 is `f8637b4a06df6bb5fa644c07047e3d0f543bc7b340ec411239b4cf5c8fc425e7`,
with source digest `d574ec1dc785948297fb0d9e9ae3c18be659229ddcff76e874b42ffba956d64e`.
The final matrix has not completed; churn and the remaining disk, coverage,
capability, OOM, pressure, mount, WAL, startup, retry, and root suites are not
qualified by this checkpoint. The report also explicitly excludes media/power
failure, other filesystems and kernels, multi-day WAL endurance, scale budgets,
package upgrade/rollback and musl, release privilege review, and a dedicated
72-hour soak plus 24-hour canary. No production binary or database was changed.

At 06:00 UTC, churn had completed 4/10 batches (8,000/20,000 operations), with
the release-117 worker and candidate daemon still active. The intermediate
batch record is [`release117-p0-matrix-churn-progress-08000.json`](live/release117-p0-matrix-churn-progress-08000.json),
SHA256 `042fdf7fba46731c8d9c184495e93ded3dbe408109ef9ad4d1c3baf8c579624f`.
At 06:02 UTC it had completed 5/10 batches (10,000/20,000 operations); see
[`release117-p0-matrix-churn-progress-10000.json`](live/release117-p0-matrix-churn-progress-10000.json),
SHA256 `6fa4299e1cd3a5c61d478afa78496eda4688cbaae70adff151e232372d24e0ad`.
At 06:13 UTC all 10 batches (20,000/20,000 operations) were complete, while
the final exactness and history checks were still running. Snapshot:
[`release117-p0-matrix-churn-progress-20000.json`](live/release117-p0-matrix-churn-progress-20000.json),
SHA256 `5f5ce456cad18a7023e6c270b4eb9020c9ec7bad2a3a7c923c7b76b87a690497`.
The final churn assertions then passed in 1,623.691 seconds. All 10 batch
oracles matched paths, inodes, allocations, and rollups; the concurrent
rebuild and history-continuity checks passed; 70,795 kernel events were seen
with zero dropped events. Event resolution/commit counters remained zero in
scan-first mode; each batch was validated against an independent exact scan.
The suite still reported conservative uncertainty in every batch, which the
scan-first contract permits while hints are pending. The report checkpoint
after churn is [`release117-p0-matrix-progress-churn-passed.json`](live/release117-p0-matrix-progress-churn-passed.json),
SHA256 `0934bfcffeece108ccfbced434ebcd9eced9c9a672905a924e15692f8feca534`;
the final per-batch progress record is
[`release117-p0-matrix-churn-progress-final.json`](live/release117-p0-matrix-churn-progress-final.json),
SHA256 `3b4b94eae212325d1ea00dc3c9ab3ef2168f73d39dd4b6b0a5003cbcf936fbeb`.
The P0 matrix has advanced to its disk suite; its remaining suites are not yet
qualified.

#### Disk-suite harness correction and rerun — 2026-11-09 06:20 UTC

Release 117 stopped in the disk suite before exercising ENOSPC or read-only
recovery. The gate timed out waiting for `pending > 0` after 64 pause-time
mutations. That counter belongs to event-apply mode; scan-first mode intentionally
does not queue per-path work there. The failure report is retained at
[`release117-p0-matrix-disk-harness-failure.json`](live/release117-p0-matrix-disk-harness-failure.json),
SHA256 `816bfbb2a638a30639538c5b7ef69eca8ef516f71b265b02d57af8ebb8965b57`.
This is a harness contract failure, not evidence that disk-full handling failed.

The disk gate now requires the 64 pause-time hints to appear in a fresh
scan-first status publication with no additional kernel overflow; event-apply
mode retains its pending-work assertion. Two Python regressions cover those
branches. The full Python suite passed **345/345** for this correction; a
subsequent test addition raised it to **346/346**. Python compilation and diff
checks pass. The current harness SHA256 is
`2652ac8a08d3402225d5b1f3aa4b5df4f92e33e3583b13db64b5200559a3f7a1`.

The first release-118 disk rerun failed at the read-only check using
`immutable=True`, reporting 64 missing `paused-*` entries. The post-failure
preservation copied both the SQLite index and an authoritative device/inode
tree snapshot. An independent comparison of those saved databases found zero
path mismatches and zero inode/allocation mismatches; all 64 paused paths were
present and SQLite integrity was `ok`. This points to a read-only immutable
oracle mismatch, not database data loss, but the failed run still does not pass
the disk gate. The report is
[`release118-disk-exactness-failure.json`](live/release118-disk-exactness-failure.json),
SHA256 `a72eb48b4659eec6d8dccdd2bbd0b81e2e8f6a45dd0280bf50c59111fa03203a`.
Forensic comparison data is in
[`release118-disk-failure-forensics.json`](live/release118-disk-failure-forensics.json)
and [`release118-disk-failure-forensics.tar.gz`](live/release118-disk-failure-forensics.tar.gz),
archive SHA256 `92b166185b3d025e656d10af7b89bc143c5a94ea290734644b9c9c47d3a7cb6d`.

The next run fingerprints the main DB and WAL around both failed scans and
uses a normal read-only snapshot for the post-scan exact oracle. It still
requires the complete paths/inodes/allocations comparison to pass. The first
launch was refused before testing because the preserved fixture left only
2,473,582,592 bytes free, below the required 2,684,354,560 bytes. Its report
is [`release118-disk-workspace-insufficient.json`](live/release118-disk-workspace-insufficient.json),
SHA256 `85c522d2b394d03c204ce52fd14c17af5a0a6501faf8b15f402b0e6e51525220`.
After preserving the exact SQLite/truth evidence archive, I removed only the
completed synthetic loop workspace (581 MiB) and started a fresh retry with
failed-workspace retention. The retry is active; no disk-suite result is
available yet. The current harness SHA256 is
`d0f22f14fdd8eda50f40cadb14f0dccdf383fba51c159c9ca695a5e8fd5809f0`.

The disk retry is running in a fresh isolated workspace using release 118, with
a 100,000-file fixture and 2 GiB disposable ext4 image. Product ENOSPC/read-only
behavior remains unqualified until it passes. The candidate includes stale
in-place scan-stage cleanup; the release-117 matrix binary did not. The
release-118 static binary SHA256 is
`f69e20de7a844f27cccdc11eae886e1ac8164aee027dd9290544bc0f6c007ae5`, fault
binary SHA256 `50d1a3d09bcdbca55fdde615fc264c54a06fda17eb3017a7ae43fef3f2ae5f68`,
and both report source digest
`3cdcedd87a744a40e38751db462c605d6be8f6cf5cba9c3faa5ced25a70f548d`.

#### Release-118 disk qualification passed — 2026-11-09 06:48 UTC

The fresh disk suite passed on VPS2/Linux 6.8.0-147-generic. It used the
release-118 static binary above, a disposable 2 GiB ext4 image seeded with
100,000 files, 25% CPU quota, and 4 GiB memory limit. All three disk assertions
passed: low-disk pause/resume, a full-disk scan that kept the index, and a
read-only scan failure that kept the index readable. Reserved-space evidence
was 1,582,514,176 bytes. Suite time was 333.091 seconds; the transient systemd
worker ran 6m31s and peaked at 927.8 MiB RSS. The exact test report is
[`release118-disk-final2.json`](live/release118-disk-final2.json), SHA256
`c77403090b41eb8e79c006dcdc6ddd851d4fdbd59093e7692eb5b4f6149883bf`;
harness SHA256 `d0f22f14fdd8eda50f40cadb14f0dccdf383fba51c159c9ca695a5e8fd5809f0`.

Current local suites: Rust release/all-features **324 passed, 0 failed, 2
ignored**; Python harness/workflow **347/347 passed**; LLVM coverage **72.25%
line, 71.09% region, 76.07% function**. Branch coverage is unavailable.
Strict Clippy, formatting, Python compilation, and `git diff --check` pass.
The separate focused stale-stage-cleanup regression passed. On VPS2, release
117 passed 8/8 crash rounds, one real fanotify overflow scenario, one CPU-burst
scenario, and 10/10 churn batches (20,000 operations; zero drops); earlier
release-115 pressure and startup/upgrade suites passed. These are scenario
counts, not an additive unique-test total, and the matrix reports remain the
source of each detailed metric.

The disk report intentionally lists other suites as not run. Remaining
qualification includes mount/capability/OOM/WAL/retry/root scenarios, scale
budgets at 100k/1m/10m, package upgrade/rollback and musl, security release
review, other filesystems/kernels, power/media loss, multi-day WAL endurance,
72-hour soak, and 24-hour canary. This evidence does not establish 100%
coverage, zero error rate, or production readiness; the installed production
service was not changed.

#### Release-118 crash recovery passed — 2026-11-09 07:18 UTC

The release-118 candidate passed eight crash/restart rounds on VPS2 using a
disposable 2 GiB ext4 image seeded with 100,000 files. The worker had a 25% CPU
quota and 4 GiB memory limit. Fault injection covered each in-place boundary
once (`scan.in_place_started`, `scan.in_place_projection_ready`,
`scan.in_place_before_commit`, `scan.in_place_after_commit`) plus four random
timing kills. Every recovery passed exact filesystem comparison, SQLite
integrity/history checks, and the assertion that no generated scan-stage
artifacts remained. Recovery took 97.592–114.605 seconds; total suite time was
1,548.604 seconds. Worker peak RSS was 956.5 MiB and swap peak was 458.8 MiB.
The installed `dux.service` remained active with zero restarts after the run.

Report: [`release118-crash-8.json`](live/release118-crash-8.json), SHA256
`0e89e7bf29a6eb634db8aeb2fdb0ccf82c946fbe8637ec260d6e7e60e7c23bf5`.
Together with the disk report above, release 118 has passed 8 crash rounds
and 3 disk gates. Its other qualification suites remain unrun; earlier
release-117 overflow/churn/CPU results and release-115 pressure/startup results
are not substitutes for repeating those suites against the release-118
candidate.

#### Release-118 forced fanotify overflow passed — 2026-11-09 07:22 UTC

The candidate passed a real kernel-queue overflow test on VPS2/Linux
6.8.0-147-generic. The disposable ext4 fixture used a 768 MiB image and 256
baseline files; the queue capacity was 16,384 and the harness created 18,432
files while the daemon reader was stopped. Dux observed exactly one overflow,
completed a newer full observation, passed its accounting verifier, matched
the independent filesystem oracle, and cleaned up the storm. Recovery took
13.358 seconds. The index status remained conservatively uncertain after the
scan, which is reported separately from exact data correctness. Candidate
binary SHA256 `f69e20de7a844f27cccdc11eae886e1ac8164aee027dd9290544bc0f6c007ae5`;
harness SHA256 `44a7e4d3c15b2e4c54445f3c09c86ece5b8b2edd6206528de083e1a0ddb9fe7a`.
Report: [`release118-overflow-retry.json`](live/release118-overflow-retry.json),
SHA256 `f6b723dba4900a522ecb69fd130403412d20d9dfc248a11c6bf1758fc8dfcba4`.

The first attempt was a harness false negative: the explicit request raced
with an overflow-triggered observation, and the gate read completion before a
queued follow-up scan ended. The gate now requires a newer scan generation and
attempt, plus no active scan, before accepting completion. The preserved
first-attempt report is
[`release118-overflow-failure.json`](live/release118-overflow-failure.json).
The Python regression suite now passes **348/348**. Release 118 has passed
disk, crash, and overflow qualification; the remaining listed suites are not
covered by those passes.
Latest complete Python test output:
[`current-source-python-tests-20261109.log`](live/current-source-python-tests-20261109.log),
SHA256 `2b974b8270a5205f6af064c13dc95d4e9bf566149768bc6906b9b291e795d6cb`.

#### Release-118 production-shaped churn passed — 2026-11-09 07:58 UTC

The candidate passed 20,000 mixed filesystem operations in ten 2,000-operation
batches against a 100,000-file fixture on a disposable 2 GiB ext4 image. The
worker was limited to 25% CPU and 4 GiB memory. Every exact checkpoint matched
paths, inode identities, allocations, and all rollups. A rebuild concurrent
with churn and history continuity also passed. It observed 70,772 kernel
events with zero drops. Exact convergence per batch took 109.200–202.795
seconds. The index remained conservatively uncertain at every churn checkpoint
while independent exact comparisons passed; continuous-write clean-status and
freshness targets remain unproven. Suite time was 1,977.9 seconds; the unit
runtime was 33m51s, peak RSS 1,019.6 MiB, and peak swap 525.7 MiB. Production
remained active with zero restarts.

Report: [`release118-churn-20k.json`](live/release118-churn-20k.json), SHA256
`aba0b3207a849c1ba872113626603942086a979abb4d2369445045bf81f1a836`.

#### Release-118 CPU event-burst test passed — 2026-11-09 08:00 UTC

The CPU-burst suite passed on VPS2. It generated 12,004 fanotify hints in
3.401 seconds; the scan-first reader observed them in 0.153 seconds; exact
validation took 7.634 seconds. Event-thread CPU measured 0.036 of one core
against a 10% Dux event-thread cap and 25% outer cgroup CPU quota. No kernel
overflow occurred. Suite time was 46.914 seconds. Report:
[`release118-cpu.json`](live/release118-cpu.json), SHA256
`7508d67041eb3de045b999d2f1b760ae7a6df4d77dc23b76e4c009557543bb0e`.

#### Release-118 coverage/fail-closed test passed — 2026-11-09 08:01 UTC

An unprivileged scan over an unreadable subtree exited incomplete and
explicitly preserved incomplete coverage rather than publishing a complete
index. Report: [`release118-coverage.json`](live/release118-coverage.json),
SHA256 `660a15b7428c96c5989149161e3b332777b2cdc295dd8b94da0ee354a251ee15`.

#### Release-118 capability failure/recovery test passed — 2026-11-09 08:02 UTC

The test removed the capability needed for handle-based lookup, verified
fail-closed uncertainty and exact scan-first accounting, then restored the
capability and verified convergence. Report:
[`release118-capability.json`](live/release118-capability.json), SHA256
`154478a1ab51100b8a4836753afa7b7578d0a652cc3ca04c241f67b6ebb92252`.

#### Release-118 mount-coverage test passed — 2026-11-09 08:03 UTC

The candidate detected and began watching a newly mounted filesystem,
detected its disappearance, refreshed coverage, and recovered to an exact
index. Report: [`release118-mount.json`](live/release118-mount.json), SHA256
`7dfad2246bc3ddc0088c31c67991481f56f0edc6ad45f9246b2adc21a6025f65`.

#### Release-118 automatic retry/restart test passed — 2026-11-09 08:14 UTC

The suite injected an accounting failure after an in-place scan commit,
verified that retry state and its 60-second deadline were persisted, then
restarted the daemon. During cooldown it continued receiving events and did
not start another scan early. After cooldown, Dux retried automatically,
completed independent accounting verification, cleared retry metadata, and
matched the filesystem exactly without SIGHUP. The scenario took 62.105
seconds. Earlier harness attempts are retained: the first used an
atomic-install checkpoint absent from scan-first, and a later gate observed
the scan commit before its verifier completed. Another run recorded Dux
deferring retry during elevated host pressure; the final gate waited for
recovery under the host pressure-admission policy.

Report: [`release118-retry.json`](live/release118-retry.json), SHA256
`b910ad45267e90fe93730e12349f520de178d560b3abbcb72c8a5e78e032cae9`.
Harness SHA256 `e3e864fe2379213834b4db8b80c3caab4841ecf970c701cb4fe74eb6c93c9036`.

#### Release-118 cgroup OOM/restart recovery passed — 2026-11-09 08:15 UTC

The isolated test daemon was placed under a 1 MiB memory limit, then forced to
allocate during reconciliation. The kernel killed it (`systemd Result=oom-kill`),
systemd restarted it once, and the daemon exactly recovered 32 filesystem
changes made during downtime. The production service remained active with zero
restarts; the OOM and runtime memory-limit changes were confined to the test
unit. Report: [`release118-oom.json`](live/release118-oom.json), SHA256
`c49dcfca79ab1437119d6c5f92d2e8f7a81a998e72c6f472220fcaa5a409005b`.

#### Release-118 long-reader WAL test passed — 2026-11-09 08:16 UTC

A held SQLite reader retained a consistent snapshot across 12 daemon
observations and 576 filesystem mutations. Peak WAL size was 959,992 bytes;
after releasing the reader, a truncate checkpoint reclaimed it to zero. The
index passed its integrity and exactness checks. Report:
[`release118-wal.json`](live/release118-wal.json), SHA256
`83f9e0150b5d41f20077f692e43c16363956bc6af4d9eea70bffba1c3dde7be8`.

#### Release-118 host-pressure behavior passed — 2026-11-09 08:17 UTC

The pressure suite verified the scan-first policy: high-load and Critical
observations were cancelled, prior state was retained, and a later observation
recovered exactly; CPU Elevated and I/O Elevated observations completed. All
four induced mutations appeared in the recovered index. Report:
[`release118-pressure.json`](live/release118-pressure.json), SHA256
`8b2116cc39ad58ddd0b478e5bc1ba045d44e07ed52a84e731b7eea90c7bcf915`.

#### Release-118 startup/upgrade test passed — 2026-11-09 08:19 UTC

The candidate passed watch-before-scan startup, changes during initial scan,
schema-v4 to v7 upgrade, and injected upgrade-scan failure. The old index
remained queryable and the daemon stayed alive; after automatic retry it
installed v7, replayed downtime changes, preserved history, and repaired an
inode-kind mismatch by full observation. This mode intentionally validates
observation repair rather than per-event inode-kind repair. Report:
[`release118-startup.json`](live/release118-startup.json), SHA256
`b790c68176ba65739985c37ae953b6431c16c11745aab9412958849709c25790`.

### Matrix checkpoint

The P0 matrix now has **14 of 14 scenarios passed across the release-118 and
release-119 builds**. The 13 earlier scenarios used release 118; all Rust
product sources match release 119 except `src/scan.rs` documentation and
visibility narrowing of `reconcile_in_place` (`pub` to `pub(crate)`), which
does not change runtime behavior. The root-scan scenario below used the clean
release-119 artifact built from commit `4fe2730ced1d`.

#### Release-119 full-root scan passed — 2026-11-09

The candidate scanned `/` on VPS2 inside the isolated, CPU/memory-capped
qualification unit, writing its fresh index to `/dev/shm`; the installed
Dux index and service were not used by the test. The scan covered one
filesystem, excluded its own workspace, found **1,430,977 files, 174,668
directories, and 1,640,588 entries**, reported complete coverage, and passed
accounting and SQLite integrity checks. Scan/check duration was 298.673
seconds at a 75% CPU quota; the transient unit ran for about 5 minutes 28
seconds, with 1.1 GiB memory peak and 140 MiB swap peak. The installed service
remained active with `NRestarts=0`; root disk remained at 11 GiB free.

The first attempt at a 25% CPU quota timed out at the harness's 780-second
scan limit. That result is preserved and is not counted as a pass; it shows
that this full-root workload does not meet that budget on this VM. Raising
the bounded qualification quota to 75% completed the same gate. This does
not qualify the production freshness SLO or 1M/10M performance budgets.

Pass report: [`release119-root.json`](live/release119-root.json), SHA256
`d20b360254a90d344b5883d6743045b5263eb065cc093226153759b50f9c710b`.
Timeout report: [`release119-root-25cpu-timeout.json`](live/release119-root-25cpu-timeout.json),
SHA256 `9d4c2eba4feb12fb84d3b8885c04ab0293d86b0f8ada64091e6e4691082ada8f`.
Build identity: [`release119-build-manifest.json`](live/release119-build-manifest.json),
SHA256 `ce6686e874dbf684a5186c478467860d93b094d7de80822718f6a88b23362229`.

Rust release/all-features tests: **324 passed, 0 failed, 2 ignored**.
Python readiness/workflow tests: **351 passed**. Current LLVM coverage:
**72.25% line, 71.09% region, and 76.07% function**; branch totals are not
available from this run. Formatting, strict Clippy, Python compilation, and
`git diff --check` pass.

The passing P0 scenarios are disk pressure/read-only recovery, eight crash
recoveries, real fanotify overflow, 20,000-operation churn, CPU burst,
incomplete coverage, missing capability, mount add/remove, retry across
restart, cgroup OOM recovery, long-reader WAL, host-pressure behavior, and
startup/schema upgrade, and the full-root scan. Earlier failed harness attempts remain archived to
show how the gates were corrected; they are not counted as passes. The P0
scenarios are candidate evidence, not a complete production-readiness
qualification. Scale budgets, RPM and other distro lifecycle, filesystem/kernel breadth,
hardware-loss behavior, endurance, and canary gates remain open.

#### Release-119 Debian lifecycle passed — 2026-11-09

The clean static-musl DEB from commit `4fe2730ced1d` passed the VPS2 KVM
lifecycle against the release-106 baseline: baseline runtime, upgrade to 119,
rollback to 106 with verified SQLite snapshot restore, re-upgrade,
remove/purge, and clean install of 119. The report completed in **1,053.154
seconds**. Package and binary hashes matched at every transition; upgrade
preserved disabled service state; rollback restored an index with SQLite
`quick_check=ok`; remove retained state and purge removed it; clean install
enabled only `dux.service` and created no tunnel identity.

The candidate's clean-install observation covered **1,642,252 indexed nodes**
with no kernel overflow or event drops. At capture, status was still dirty
with accounting verification pending and `caught_up=false`; this does not
prove a clean/freshness SLO. The service cgroup had recorded about 3.51 GiB
read and 6.26 GiB written. This is material I/O cost from one host run, not a
controlled benchmark. The candidate passed effective systemd policy checks
and private index modes (directory `0750`, DB and sidecars `0640`, owner
`root:dux`).

The lifecycle harness left release 119 stopped and disabled as designed. A
restore guard reinstalled the original release-106 package and restored its
pretest database snapshot. The guard's first check saw systemd `reloading`
while the restored daemon performed its normal full scan; it was a verifier
assumption, not a failed restore. The final check after scan completion passed:
release 106 enabled and active, original binary and unit hashes matched,
SQLite `quick_check=ok`, 1,643,390 inode rows, zero restarts, zero event drops,
and 9.7 GiB free. The 1.3 GiB of redundant snapshots and temporary package/
runner files was removed after verification; test reports were retained.

Lifecycle report: [`release119-package-lifecycle.json`](live/release119-package-lifecycle.json),
SHA256 `f92e58299bdfff998aae342f0f0c90aa92fa4a050dda84b6986070df511e7444`.
Final restore check: [`release119-package-final-restore.json`](live/release119-package-final-restore.json),
SHA256 `3fe1dd12de9f9d4f7bf154924f5474c6f62b91fb124720a5f99b5724309cb1df`.
Pretest state and the intermediate `reloading` check are in
[`release119-package-pretest-state.json`](live/release119-package-pretest-state.json)
and [`release119-package-restore-verification.json`](live/release119-package-restore-verification.json).

This qualifies Debian transitions on one Ubuntu 24.04/Linux 6.8/ext4 KVM
guest. RPM lifecycle, other distributions, and upgrades from older
pre-scan-first production databases remain open.

#### Release-119 candidate canary started — 2026-11-09 09:43 UTC

After release-106 completed its scheduled full-root observation on VPS2, the
observation covered **1,474,513 files, 175,623 directories, and 1,650,136 path
instances**, with zero coverage issues. The walk took 290 seconds; independent
accounting verification passed 60.7 seconds later. During the interval the
watcher recorded about 20.9 million hints, with zero kernel overflows and zero
userspace drops; systemd reported zero service restarts. This was a live
baseline observation, not candidate evidence. The old daemon's index remained
dirty between observations as expected in scan-first mode.

Before upgrading, the release-106 package, binary, systemd unit, and an online
SQLite backup were verified. The backup is root-only, 668,336,128 bytes, schema
v7, and passed `PRAGMA quick_check`; SHA-256 is
`2ced1b0b4379fbda37faae719f37d83b180cf65e8f445bf6ba3682d0a215140f`. The
marked VPS2 qualification VM was upgraded to release 119. Its installed binary
matches the clean candidate SHA-256
`e4841e7b75a4785a3fcf2597d4e711b21df18ebca25639f27ec0d96b9dfa293d`, and the
effective unit matches `e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.
VPS1 production was not changed.

A read-only 24-hour canary is active as
`dux-candidate-canary-119.service`, sampling every 300 seconds with a
20-minute maximum full-observation age. The collector is capped at 20% CPU and
1 GiB memory; Dux retains its packaged 100% CPU and 4 GiB memory ceilings. The
first sample passed in 60.766 seconds while a scan was running: exact binary
and unit identities, schema-v7 path graph, root accounting, and SQLite quick
check all passed; no journal failures, restarts, or OOM events were found.
Independent accounting found 1,646,654 inodes and 1,650,136 paths, with zero
orphan dirents, invalid prime attributions, parent errors, or rollup errors.
At the initial checkpoint there was **1 passing sample**, not a completed
soak; see the later checkpoint below for the current count. The pre-upgrade
package and DB backup are retained for rollback.

Initial evidence snapshot:
[`release119-canary-progress-20261109T0943Z.jsonl`](live/release119-canary-progress-20261109T0943Z.jsonl),
SHA-256 `fda22a0a9cf3d8bcc08570ae19fad42eee77d8c3e34c16da8c925dc72d449ea5`.
The append-only live log continues on VPS2 at
`/root/dux-canary-119/candidate-canary.jsonl`; the monitor and observer hashes
are recorded in that log. Preserve subsequent snapshots and record any failed
sample; do not treat the run as passed until the monitor records
`canary_completed` after the full 24 hours.

#### Canary checkpoint — 2026-11-09 09:50 UTC

The release-119 monitor has recorded **2 passing samples out of 289 required**
and remains active. Sample 2 completed in 47.603 seconds; its before/after
status reads showed the same full observation within the 20-minute age bound,
zero kernel overflows and userspace drops, and exact schema-v7 accounting.
Across both samples there are no monitor errors, restarts, OOM events, or
critical journal entries. At sample 2, the systemd Dux memory peak was
1,329,266,688 bytes against the 4 GiB service cap. Cgroup I/O counters for
device `8:0` had reached 1,781,170,176 bytes read and 1,484,582,912 bytes
written since service start; `8:0` and `252:0` are stacked representations and
must not be summed. These are early single-run observations, not an efficiency
SLO or steady-state benchmark.

The current two-sample snapshot is
[`release119-canary-progress-20261109T0950Z.jsonl`](live/release119-canary-progress-20261109T0950Z.jsonl),
SHA-256 `b38cb3ec40d9237d154f270be355a8b3cdc8cfab619eecea28096c13eb28ecc8`.
The complete append-only log remains on VPS2 at
`/root/dux-canary-119/candidate-canary.jsonl`.

#### Canary checkpoint — 2026-11-09 09:54 UTC

The monitor has recorded **3/289 passing samples**. The latest sample took
51.609 seconds; scan ages in the three samples were 575, 375, and 679 seconds.
Every sample passed exact accounting, path-graph and SQLite checks, binary and
unit identity, and health gates. No sample found a service restart, OOM,
kernel overflow, userspace drop, or critical journal entry. This is only about
11 minutes of operation; the required 24-hour interval is still in progress.

Three-sample snapshot:
[`release119-canary-progress-20261109T0954Z.jsonl`](live/release119-canary-progress-20261109T0954Z.jsonl),
SHA-256 `b7099f4ee4784ee8f0ee1d0c985b77181e470dca336171ae1f77ec9187042eec`.

#### VPS1 production-state check — 2026-11-09 10:06 UTC

Read-only inspection confirms VPS1 is still on legacy event-apply release
`0.6.3` commit `e0cc915f8f37`, not the release-119 candidate. Its service is
active with four systemd restarts. The schema-6 database is dirty from an
overflow; its last completed scan is 93.3 hours old and it reports 40.6 hours
behind. There are 435 cumulative kernel overflows and zero userspace drops.
The database is 2.93 GB with a 128 MiB WAL; the root filesystem has roughly 62
GiB available.

From a 30-second production sample, the current event rates were 294/s seen
and 285/s resolved, with no new overflow in that interval. The index remained
dirty. The preceding ten-minute journal sample contained 157
`Stale file handle` resolution warnings and no new explicit scan,
reconciliation, accounting, or overflow failure messages. This does not
establish that the old index is correct or fresh.

The Dux cgroup accumulated approximately 297.1 GB of writes on each of two
stacked device rows since service start. `systemctl` reports about 594.2 GB
because it adds both layers. A 30-second sample added 21.2 MB per row; these
are cgroup counters, not a direct measure of unique physical-media writes.
The production unit's CPU quota is 100%, memory high/max are 2/4 GiB, and the
kernel fanotify queue limit is 65,536 events.

This recheck confirms a migration gap: the actual production DB is schema 6,
while the release-119 package lifecycle only tested schema-7 baseline data.
The current v6-to-v7 regression fixture is a one-file database. Production-
scale v6 migration remains unqualified. No production package, DB, or service
was modified. Raw snapshot:
[`vps1-production-readonly-20261109T1006Z.json`](live/vps1-production-readonly-20261109T1006Z.json),
SHA-256 `876758c55554bc89a693d430d1eeefb48d4222e45ba7a5738e52d44c5c3f2304`.

#### Release-119 canary checkpoint — 2026-11-09 10:06 UTC

The test-VM candidate canary has **5/289 passing samples**. Sample 5 passed
in 81.895 seconds; it showed a 409-second-old completed observation, no scan
in progress, zero overflows/drops, and no monitor errors. It is still far
short of the 24-hour gate. Evidence:
[`release119-canary-progress-20261109T1006Z.jsonl`](live/release119-canary-progress-20261109T1006Z.jsonl),
SHA-256 `de6bd413a904af216fd9fce10e47ac5423c0b8ff8b55a37357b125048b6456a2`.

#### Release-119 canary sample six — 2026-11-09 10:09 UTC

The VPS2 canary remains active with `NRestarts=0`; **6/289 samples pass**.
Sample 6 completed in 48.304 seconds with zero errors, overflows, and user
drops. Exact accounting, path-graph, and SQLite checks passed. The scan age
was 675 seconds and status remained dirty/not caught up between periodic
observations. The 24-hour soak remains open. Raw sample:
[`release119-canary-sample6-20261109T1009Z.jsonl`](live/release119-canary-sample6-20261109T1009Z.jsonl),
SHA-256 `792c7125c9a8ba7e08a682ebd1ad82d9ed933f9437ec7ab3029573f2d4375916`.

### Production-shaped schema-6 migration regression — 2026-11-09

The existing v6-to-v7 test now accepts `DUX_V6_UPGRADE_SCALE_ENTRIES` and
asserts source and migrated counts, zero coverage issues, exact path totals,
schema version, root resolution, and retained growth history. It defaults to
one file for ordinary test runs. The explicit 1,300,000-file run passed in
**3:37.28 elapsed**, at **722,524 KiB peak RSS**, with no swap. The constructed
schema-6 source had 1,300,000 files, 1,301 directories, and 1,301,301 path
instances. The rebuilt schema-7 index had the same counts, no coverage issues,
and exact root-visible entry totals. `cargo test --locked --release
--all-features --bin dux` then passed **324, failed 0, ignored 2**; strict
Clippy, formatting, and diff checks passed. Refreshed LLVM coverage is **72.24%
line, 71.08% region, 76.02% function**; branch totals are unavailable. Raw
coverage output:
[`schema6-current-coverage-20261109.log`](live/schema6-current-coverage-20261109.log),
SHA-256 `6347d9650a07d891f59c56dfa70cfb117f73ffa5a43d13a640ad30be50e5125a`.

This synthetic fixture starts from a schema-7 scan and removes the v7 path
graph while setting schema metadata to 6. It tests production scale and the
known v6 source layout, but does not use a backup of the live production DB or
prove its exact contents and history. Production migration qualification is
therefore improved but remains an open release gate. Complete raw test output:
[`schema6-upgrade-scale-20261109.log`](live/schema6-upgrade-scale-20261109.log),
SHA-256 `deeae8385aff194e8c10e3d6aea733d787e56734ddba5ff9ee40afe548129e74`.

### Release-119 canary checkpoint — 2026-11-09 10:24 UTC

The VPS2 canary is active with zero Dux and monitor restarts and **9/289
passing samples**. Samples 7–9 passed with no monitor errors, overflows, or
userspace drops; sample 9's accounting, path graph, SQLite, binary/unit,
journal, and health checks passed in 43.492 seconds. The scan age was 671
seconds; status remained dirty/not caught up while waiting for the next
periodic observation. This is not a completed 24-hour soak. Evidence for
samples 7–9:
[`release119-canary-progress-20261109T1024Z.jsonl`](live/release119-canary-progress-20261109T1024Z.jsonl),
SHA-256 `7ac4ff7214149b309936c779b193d02109804166c6d6bba74a56cf2939d4f697`.

### Release-119 sample 10 and post-scan check — 2026-11-09 10:34 UTC

Sample 10 passed at 10:29:40 UTC in 61.601 seconds, with no monitor errors,
kernel overflows, userspace drops, or Dux restarts. The sample was collected
while a periodic full scan was running (`ActiveState=reloading`,
`scan_in_progress=true`); the most recent completed scan was then 988 seconds
old. The scan finished at 10:31:38 with 1,482,435 files, 176,659 directories,
1,659,094 path instances, and zero coverage issues. Accounting verification
passed in 54.328 seconds. A separate read-only status check at 10:34 found the
service active/running, zero restarts/overflows/drops, scan age 369 seconds,
and 1,655,612 indexed nodes. It remained dirty/not caught up between periodic
observations. The sample passed, but it does not complete the 24-hour soak.
Raw sample:
[`release119-canary-sample10-20261109T1029Z.jsonl`](live/release119-canary-sample10-20261109T1029Z.jsonl),
SHA-256 `e6cb7a3881c6eefb906b5b6f6ec8420a12746b6fc0250b70baae04d760f9175a`.

### Production-volume growth-history migration and live-source snapshot — 2026-11-09

Read-only SQLite `dbstat` on the actual production schema-6 index counted
**27,184,763 growth rows**. The `growth` table and its two indexes consume
2,557,689,856 bytes (2.38 GiB), about 86% of the 2,958,880,768-byte database.
This is the primary reason the old index is much larger than the schema-7
canary database. VPS2 had only about 10 GiB free. An in-place clone, rebuilt
staging DB, and SQLite destination WAL could approach that limit, so I did not
run a full exact-data migration there.

A production-source online backup attempt used SQLite's backup API with the
source opened read-only and idle I/O priority. While the daemon continued
writing, the backup made little forward progress: the process reported 65.5
GB written while the incomplete destination was about 2.1 GB. This is
consistent with backup restarts when SQLite detects source changes. I
interrupted the attempt, removed only its incomplete destination, and checked
that production remained active with its restart count unchanged. No live DB
or service unit was modified. This attempt is not a pass; a stable source
snapshot is required before an exact production-data migration test.

Instead, the migration regression now accepts
`DUX_V6_UPGRADE_GROWTH_BUCKETS` to generate history at scale without copying
production path names. With 1,300,000 synthetic files and 21 history buckets,
it migrated **27,327,322 growth rows** successfully—close to production's
27,184,763. The test verified file, directory, and path totals, zero scan
coverage issues, schema version 7, path-graph/root resolution, and complete
growth-row count plus a preserved legacy growth row. It completed in **7:32.66**
with **1,009,848 KiB peak RSS** and no swap. The full release/all-features
suite passed **324, failed 0, ignored 2**; strict Clippy, format, and diff
checks passed. Coverage is **72.22% line, 71.08% region, 75.99% function**;
branch totals unavailable. Coverage output:
[`schema6-growth-coverage-20261109.log`](live/schema6-growth-coverage-20261109.log),
SHA-256 `cc4f8d8d9d61c311336934bf9feb967fe85f49918d1a76ff480cc5562feb041d`.
Raw migration log:
[`schema6-production-growth-scale-20261109.log`](live/schema6-production-growth-scale-20261109.log),
SHA-256 `66000b7662fb6b5dee4c89a62c5bcdd5416b1c1ae32f894f8eef827cd0c851fc`.

The canary monitor passed **samples 12–16** with no errors, restarts,
overflows, or user-space drops. Sample 16 took 139.338 seconds; at capture the
last completed scan was 1,066 seconds old. A separate check at 11:02 found the
service active with zero restarts, a new scan in progress, and scan age 1,192
seconds. The index remains dirty between periodic observations; 24-hour soak
is incomplete. Evidence:
[`release119-canary-progress-20261109T1100Z.jsonl`](live/release119-canary-progress-20261109T1100Z.jsonl),
SHA-256 `d481d2b7ea376e6a524b770915890776698257c51cbc0cd054acf249f52220d8`.

### Canary monitor freshness-gate defect and correction — 2026-11-09

The first release-119 monitor run ended at sample 17. The before-status had a
1,227-second-old last scan and a scan in progress. During the 61-second sample,
the scan completed; after-status had age 388 seconds, scan-in-progress false,
valid exact accounting, zero overflow/drop counters, and Dux `NRestarts=0`.
The monitor evaluated before/after independently and failed on the old before
age even though the collection ended with a fresh verified observation.

The monitor now accepts that sample only when it can prove all of these:
before-status contains a valid stale age and active scan start after the
previous scan; after-status proves a newer completed observation within the
freshness bound; and both status heartbeats and the rest of the sample gates
pass. If scanning remains active after collection, timestamps are invalid, or
the final status is unhealthy, the monitor still fails. The actual sample 17
re-evaluates with zero errors. Raw input, original failure event, and
re-evaluation are preserved:
[`release119-canary-sample17-20261109T1104Z.jsonl`](live/release119-canary-sample17-20261109T1104Z.jsonl),
[`release119-canary-failed-event-20261109T1104Z.jsonl`](live/release119-canary-failed-event-20261109T1104Z.jsonl),
[`release119-sample17-monitor-reevaluation.json`](live/release119-sample17-monitor-reevaluation.json).
The sample itself reports no Dux restarts, kernel overflows, or user drops.

Three monitor regression tests cover successful scan completion inside the
sample, an unfinished scan remaining stale, and invalid age not being waived.
Full Python discovery passed **354 tests**; Python compilation and diff checks
passed. Monitor SHA-256:
`ea1d4c98ea7bcf8fe17632095af6d5535852359b691439987480b7fedc043cad`.
Test log SHA-256:
`d76183339b5893ba511b9a15a2c7ea52e9e4eb84753080b77c87316fe04e9d2d`.

The original run is retained as **failed under the old monitor**, not silently
converted into a pass. A new 24-hour run using the corrected monitor started
at 11:13:18 UTC. Sample 1 passed in 60.935 seconds; scan age was 968 seconds,
the next scan was in progress, and overflow/drop/restart counters were zero.
The collector is active at 20% CPU and 1 GiB memory caps. This new run has only
one sample and has not qualified the 24-hour soak. Evidence:
[`release119-monitorfix-v2-sample1-20261109T1114Z.jsonl`](live/release119-monitorfix-v2-sample1-20261109T1114Z.jsonl),
SHA-256 `cf746cabe9873feede7f7769184c173548de51d369ebf35300ba954137a23739`.

At 11:20 UTC the corrected-monitor run has **2/289 passing samples**. Sample
2 passed in 99.598 seconds with scan ages 307/407 seconds, zero overflow and
drop counters, and no Dux or monitor restarts. The canary remains active but
is only minutes into its new 24-hour interval. Evidence:
[`release119-monitorfix-v2-sample2-20261109T1119Z.jsonl`](live/release119-monitorfix-v2-sample2-20261109T1119Z.jsonl),
SHA-256 `2a6334216f7445649b304bea4f3402914fd3ce3c2f508fb76cb3a7f46ad257c2`.

At 11:24 UTC the corrected-monitor run reached **3 passing samples**. Sample
3 completed in 57.621 seconds with schema 7, SQLite `quick_check=ok`, a valid
1,668,477-path graph, zero kernel overflows, zero userspace drops, no OOM
events, and no Dux or monitor restarts. Scan age was 665 seconds at collection;
the database was dirty and not caught up, so this is evidence of a bounded,
accounted observation, not a continuously clean index. The canary remains
active and the 24-hour gate is still open. The captured full JSONL snapshot
contains samples 1–3:
[`release119-monitorfix-v2-progress-20261109T1124Z.jsonl`](live/release119-monitorfix-v2-progress-20261109T1124Z.jsonl),
SHA-256 `b674e11d4ab27e8af4e8e20b18456571632b7a47f16afe7a0caa03a4e581bf1d`.

#### Sample 5 freshness failure and pressure-cancellation fix — 2026-11-09

The corrected-monitor run later **failed closed at sample 5** (11:34:30 UTC).
Its 24-hour gate requires a completed full observation no older than 1,200
seconds; the observation age was 1,278 seconds before and after collection.
This failure is real and is not counted as a passing canary. The raw run,
including samples 1–4 and the failed sample, is preserved at
[`release119-monitorfix-v2-failed-sample5-20261109T1134Z.jsonl`](live/release119-monitorfix-v2-failed-sample5-20261109T1134Z.jsonl),
SHA-256 `212b617f0fa3d54b212ecebfb12ad61387823ff2d8994b622a10e0e964d0dbdf`.

The service did not crash: Dux `NRestarts=0`, the sample had no kernel
overflows, userspace drops, or OOM events, and its exact path-graph/SQLite
checks passed. The previous scan had been cancelled at 11:29:27 during
*elevated* memory PSI (`avg10=5.07%`) despite 3.37 GiB available memory; the
pressure was below the code's critical PSI threshold (>20%). A retry started
at 11:30:28 and completed at 11:34:52—22 seconds after the failed sample. It
observed 1,494,739 files, 178,007 directories, 1,672,746 path instances, and
zero coverage issues. The canary correctly rejected the freshness breach;
completion shortly afterward does not retroactively pass it.

The current source now keeps an **already active** low-priority scan running
under elevated memory PSI when available memory remains above the elevated
floor. It still cancels under critical pressure, genuinely low available
memory, elevated host load, or unavailable telemetry. New scan admission
remains more restrictive. This targets the observed cancel/retry loop without
relaxing the freshness limit. Revision 120 has now exercised the change in its
first root observation and remains in a fresh 24-hour VPS2 canary; the long
runtime gate is still pending.

The focused resource-guard suite passed **21/21 tests**. Full release/all-
features Rust tests passed **326, failed 0, ignored 2**. Strict all-target,
all-feature Clippy, formatting, and diff checks passed. Refreshed coverage is
**72.26% line, 71.11% region, 76.02% function**; branch coverage is unavailable.
Logs:
[`pressure-guard-release-tests-20261109.log`](live/pressure-guard-release-tests-20261109.log),
[`pressure-guard-clippy-20261109.log`](live/pressure-guard-clippy-20261109.log),
[`pressure-guard-llvm-cov-20261109.log`](live/pressure-guard-llvm-cov-20261109.log).

#### Release-120 build and fresh VPS2 canary — 2026-11-09

Package revision **0.6.3-120** was built from commit `5d7cdacb80aa` as static
musl. The build audited the 229-dependency lockfile and exact release binary;
the audit found zero advisories. The binary SHA256 is
`67bf2fa27680c014ebc3623adb16b6e435eef83ed5e92dad68eae909cc302806`; the
DEB SHA256 is
`9f701f0b8eca926765be6bb1536e9168a80c831cac9eeca72639e986ed47fd4d`. The
manifest and all package checksums are preserved in
[`release120-build-manifest.json`](live/release120-build-manifest.json),
SHA256 `ba67af8255ebce73aa16f10c63ba2672e116ea1943a17dea7e8a0b91fc1f636b`,
and [`release120-SHA256SUMS`](live/release120-SHA256SUMS), SHA256
`3893464eeae7e29ddb332a62c01eed29bbdae9e3bc3003ffb03fefaeaf0127de`.

On VPS2, I stopped release 119 after its current observation finished, copied
the schema-7 database, binary, unit, and old package to a root-only rollback
directory, then verified the database with SQLite `quick_check=ok` and
1,672,454 inode rows. Revision 120 is installed and running; its process binary
hash matches the build manifest and its systemd unit hash is unchanged. The
production VPS1 service and database were not touched.

A new 24-hour canary started at **11:54:37 UTC** with 5-minute sampling,
20%-CPU/1-GiB monitor limits, and the existing 1,200-second observation-age
gate. Its first two samples passed. Sample 1 had a scan in progress, zero
overflow/drop/OOM/restart counters, and a valid 1,675,936-path graph. Sample 2
passed after the full root observation completed: 1,677,080 paths, SQLite
integrity and path graph passed, scan age 384 seconds, zero overflows/drops,
and no service or monitor restart. The full observation finished in about
4m37s with zero coverage issues. The index remained dirty between periodic
observations, as expected in scan-first mode. This is only minutes of runtime;
the 24-hour soak is not qualified. Snapshot:
[`release120-canary-progress-20261109T1200Z.jsonl`](live/release120-canary-progress-20261109T1200Z.jsonl),
SHA256 `73b7be431183603c3b4f80d32dc4cdac52913b08ee8923454717a40309358e02`.

At 12:06 UTC the same r120 canary reached **3/289 passing samples**. Sample 3
completed in 79.307 seconds; the observation was 707 seconds old, no scan was
active at collection end, the path graph still had 1,677,080 paths, and
overflow, drop, OOM, and restart counters remained zero. Snapshot containing
samples 1–3:
[`release120-canary-progress-20261109T1206Z.jsonl`](live/release120-canary-progress-20261109T1206Z.jsonl),
SHA256 `68ab24387f38c5cfa2a0be0dda98f6d47fb1c4d9883e3b3d5615eabc4db1d4f6`.

At 12:16 UTC the r120 run reached **5/289 passing samples**. Sample 5 began
with an active scan and a 1,228-second-old prior observation, above the
1,200-second threshold. The scan completed during the 90-second collection;
the after-status showed a newer observation 417 seconds old. The monitor
accepted this because it verifies the completed after-state when a scan
finishes inside a sample. This is a short observed threshold excursion, not
proof of a hard 20-minute maximum. SQLite `quick_check` and the 1,677,080-path
graph passed; the service remained dirty while its separate accounting
verification was pending, which passed 16 seconds after collection. There were
no overflows, drops, OOM events, or restarts. The observation completed at
12:15:17 after 6m08s with zero coverage issues. Full snapshot:
[`release120-canary-progress-20261109T1216Z.jsonl`](live/release120-canary-progress-20261109T1216Z.jsonl),
SHA256 `45c25096ca113c5fcd6a24586d41549682998976f7065994ab2f2b2affae4c17`.

At 12:21 UTC the r120 run reached **6/289 passing samples**. Sample 6 took
88.505 seconds; the latest observation was 716 seconds old with no scan active.
The path graph had advanced to 1,680,006 paths and remained valid. Overflow,
drop, OOM, and restart counters were still zero. This extends the canary by
five minutes only. Snapshot:
[`release120-canary-progress-20261109T1221Z.jsonl`](live/release120-canary-progress-20261109T1221Z.jsonl),
SHA256 `9258733256e0023c7aa65732bc41c2d94afe29da2f9542c05b67131a843ef164`.

At 12:26 UTC the r120 canary reached **7/289 passing samples**. Sample 7 took
121.719 seconds; it saw the next full observation in progress at age 1,049
seconds. The independent path graph remained valid at 1,680,006 paths, with
zero overflows, drops, OOM events, or restarts. The 24-hour gate remains open.
Snapshot:
[`release120-canary-progress-20261109T1226Z.jsonl`](live/release120-canary-progress-20261109T1226Z.jsonl),
SHA256 `6a2305370b2a87c3e35d3a15bc7310091ef6e81635745998f9c99c0e204c92b6`.

The early 5-minute resource windows are variable. Dux averaged 14.9–98.3% of
one core across adjacent sample intervals. Two intervals containing full-root
scan work recorded 1.77 GiB read/1.46 GiB written and 1.09 GiB read/1.28 GiB
written on the root block device. These intervals include scan work and normal daemon
activity; they are not isolated per-scan benchmarks or a steady-state budget.
The current 15-minute cadence and freshness threshold therefore still need a
longer CPU/I/O distribution before any performance SLO or cadence change is
approved.

At 12:31 UTC the r120 canary reached **8/289 passing samples**. Sample 8 took
106.305 seconds, ended with no scan active and scan age 433 seconds, and passed
the 1,680,006-path graph, SQLite, and accounting checks. Overflow, drop, OOM,
and restart counters remained zero. This is still under 40 minutes of a
24-hour qualification. Snapshot:
[`release120-canary-progress-20261109T1231Z.jsonl`](live/release120-canary-progress-20261109T1231Z.jsonl),
SHA256 `bc11ff65218e40c89affd03bef2ff0964d2e852ef71eaafdcb5a688ac9818245`.

At 12:36 UTC the r120 canary reached **9/289 passing samples**. Sample 9 took
65.217 seconds; the observation was 693 seconds old with no scan active. The
independent path graph was valid at 1,683,013 paths. Overflow, drop, OOM, and
restart counters remained zero. Snapshot:
[`release120-canary-progress-20261109T1236Z.jsonl`](live/release120-canary-progress-20261109T1236Z.jsonl),
SHA256 `38370fabe42cce4cde983e19e61efe3dca23f47a92e2797ad2d7a0b3c3f9d924`.

At 12:40:47 UTC, sample 10 passed in 70.003 seconds. It observed a root scan
in progress at age 997 seconds, with a valid 1,683,013-path graph, zero
coverage issues, and zero overflow, userspace-drop, OOM, or restart counters.
The index was dirty and not caught up during the active scan. The 24-hour gate
remains open. The full append-only monitor log is
[`release120-canary-progress-20261109T1241Z.jsonl`](live/release120-canary-progress-20261109T1241Z.jsonl),
SHA256 `bc75b426cfd4e2f1e5ba67adb950ba6a9f9c72b8f806f636bd947d31a0c0007a`.

At 12:46:05 UTC, sample 11 passed in 88.608 seconds. The completed scan was
416 seconds old, and the independently checked graph contained 1,685,917
paths with zero coverage issues. Overflow, userspace-drop, OOM, and restart
counters remained zero. The 24-hour run remains active and incomplete. The
captured full log is
[`release120-canary-progress-20261109T1247Z.jsonl`](live/release120-canary-progress-20261109T1247Z.jsonl),
SHA256 `8c5cfb21c5ab85cdfa633dee122abeab616059eb0c2f0898f84ca047b8c4fb05`.

At 12:50:43 UTC, sample 12 passed in 66.603 seconds. Scan age was 694 seconds,
the independently checked path graph remained valid at 1,685,917 paths, and
overflow, userspace-drop, OOM, and restart counters remained zero. At 12:53
UTC, the monitor was still active and Dux had zero restarts. The full 24-hour
gate is incomplete. Captured log:
[`release120-canary-progress-20261109T1253Z.jsonl`](live/release120-canary-progress-20261109T1253Z.jsonl),
SHA256 `d9690ce7e3f6b3b8e358d07113821f2c7aba1da4c247f493a08bc0ba8fa9642f`.

At 12:56:37 UTC, sample 13 passed in 120.562 seconds. A full scan was active
at age 1,048 seconds; the independent graph remained valid at 1,685,917 paths.
Overflow, userspace-drop, OOM, and restart counters remained zero. Collection
took longer than recent samples but completed within the five-minute cadence.
The 24-hour run remains incomplete. Captured log:
[`release120-canary-progress-20261109T1258Z.jsonl`](live/release120-canary-progress-20261109T1258Z.jsonl),
SHA256 `9c1236b1223902451010e76f3f48f3f35e56f883c1ae9f2970850bcf46d2b05d`.

At 13:01:13 UTC, sample 14 passed in 95.954 seconds. The scan was complete
and 423 seconds old; the independent 1,685,917-path graph passed. Overflow,
userspace-drop, OOM, and restart counters remained zero. The 24-hour gate
remains open. Captured log:
[`release120-canary-progress-20261109T1302Z.jsonl`](live/release120-canary-progress-20261109T1302Z.jsonl),
SHA256 `df614009f4da875254e0afeb80eb5da452b5070472b746f7c074e1362163225d`.

At 13:06:09 UTC, sample 15 passed in 92.128 seconds with scan age 720
seconds. The independently checked path graph passed at 1,689,200 paths, and
overflow, userspace-drop, OOM, and restart counters remained zero. The 24-hour
gate is still open. Captured log:
[`release120-canary-progress-20261109T1307Z.jsonl`](live/release120-canary-progress-20261109T1307Z.jsonl),
SHA256 `9e3da55bf372ae813dea69770720f427dc0d6d01f895a070c3408aa12f4699c4`.

At 13:11:49 UTC, sample 16 passed in 132.004 seconds. A scan was active at
age 1,059 seconds and the independent path graph passed at 1,689,200 paths.
Overflow, userspace-drop, OOM, and restart counters remained zero. This was
the longest canary collection so far, but it completed within the five-minute
sampling interval. The 24-hour gate remains open. Captured log:
[`release120-canary-progress-20261109T1313Z.jsonl`](live/release120-canary-progress-20261109T1313Z.jsonl),
SHA256 `9ee52e4f5a6d2ec71ed8cffe80d608ee72f2463919a4d7329fcec0d0928a4958`.

#### Production VPS1 read-only health samples — 2026-11-09 12:52 UTC

Read-only `dux status --json`, systemd, and binary-hash samples confirm the
production service is still legacy `dux 0.6.3` (`e0cc915f8f37`; binary
SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`).
At 12:49:52 it was dirty from fanotify overflow, the last full scan was
345,629 seconds old, `behind_seconds` was 155,940, and the service had four
restarts. In the next 58.5 seconds, overflow notifications increased from 527
to 539 (+12), `events_seen` increased 1,424,447, and `events_resolved`
increased 866,774. No full scan was active; scan age advanced by 59 seconds.
The userspace drop counter remained 1,511,839. A Go test process was active,
but this time correlation alone does not establish it as the cause. A 12:52:12
follow-up read showed 540 overflows and scan age 345,770 seconds. Production
was not modified. This demonstrates that the currently deployed legacy daemon
is still far behind and overflowing while the r120 candidate is under a
separate VPS2 soak; the candidate's short soak is not grounds to deploy it.

Snapshots:

- [`vps1-readonly-status-20261109T1250Z.json`](live/vps1-readonly-status-20261109T1250Z.json),
  SHA256 `b5a78fa75f40e7dd2498a79ca39f91fb8ece2f2f3ff27bbeb44318d051524864`.
- [`vps1-readonly-status-20261109T1251Z.json`](live/vps1-readonly-status-20261109T1251Z.json),
  SHA256 `8639005b2df3caf0f0319d25235c69810fe871a702b13296da212dcf9d4a6176`.
- [`vps1-readonly-status-20261109T1252Z.json`](live/vps1-readonly-status-20261109T1252Z.json),
  SHA256 `07f46b70502b12b3ee338280159f0b78979a13620929920167b304c1ca4cc3dc`.

At 13:00:26 UTC, a further read-only sample still showed the old daemon dirty,
with no full scan active, scan age 346,264 seconds, and four restarts. The
overflow counter remained 540 from 12:52, although `events_seen` grew by about
241K. Thus overflow activity is bursty, not continuously increasing; the
index still has not reconciled. The live Go security/race test workload
continues to use `/tmp/x.mQn3l`, so shared temporary files were left intact.
Snapshot:
[`vps1-readonly-status-20261109T1300Z.json`](live/vps1-readonly-status-20261109T1300Z.json),
SHA256 `de3dd3ff753da938317b2194b9159e7d6120bafead6104bc5196ebdf30b17ef8`.

At 13:10:27 UTC, the production service still had no full scan active and its
last scan was 346,865 seconds old. Overflow notifications remained at 540
since the 12:52 sample, while `events_seen` increased by 405,571 since 13:00.
The dirty index's reported node count fell by 51,201; these changing counts
are not an independent correctness proof. Dux remained active with four
restarts. Snapshot:
[`vps1-readonly-status-20261109T1310Z.json`](live/vps1-readonly-status-20261109T1310Z.json),
SHA256 `6501d1db471e0a8b7423424898018c34af1f80012f853258403b364ac9257ea3`.

#### Packaged service security policy check — 2026-11-09

The package lifecycle/security policy suite passed **70/70 tests** in a
focused rerun. `systemd-analyze verify` passed for `packaging/dux.service` and
`packaging/dux-tunnel.service`. On VPS2 (systemd 255), the effective running
`dux.service` scored **3.3 (OK)** under `systemd-analyze security`. Runtime
inspection confirmed UID 0/GID `dux`, the expected `CAP_SYS_ADMIN` and
`CAP_DAC_READ_SEARCH` capability sets, `NoNewPrivileges=yes`, seccomp active,
`RestrictAddressFamilies=AF_UNIX`, and no private mount namespace. Keeping the
host mount view is required for the current fanotify coverage model. This
check confirms configured controls, not containment of a root-level daemon
compromise; the daemon retains powerful filesystem/kernel capabilities and
broader threat-model review remains open.
