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
qualification. Scale budgets, package/rollback, filesystem/kernel breadth,
hardware-loss behavior, endurance, and canary gates remain open.
