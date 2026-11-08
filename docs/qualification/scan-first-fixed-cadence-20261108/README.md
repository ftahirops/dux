# Fixed-cadence scan-first overflow qualification — 2026-11-08

Candidate source commit: `559097852ac5333bf575299394758af7b7a9cb3d`.
Platform: disposable VPS2 KVM, Ubuntu 24.04, Linux 6.8, ext4. Production VPS1
was not used or changed.

## Change under test

Before this change, scan-first mode still requested a full-root scan whenever
the fanotify hint ring or resolver overflowed. It also had a dirty-index retry
path every 300 seconds, even though the selected scan-first design specifies a
15-minute observation cadence. Both paths could turn expected hint loss into
repeated whole-root I/O.

Commit `5590978` makes ring loss and resolver loss mark the last-observed view
stale, then leaves the next observation on the configured cadence. Dirty state
alone no longer creates the 300-second retry in scan-first mode. Manual scans,
incomplete scans, and accounting failures keep their explicit or bounded retry
behavior. Legacy event-apply mode keeps its immediate repair behavior.

## Source checks and release artifact

- `cargo test --locked --all-features`: **301 passed, 0 failed, 2 ignored**.
- `cargo fmt --check`, strict all-target/all-feature Clippy, and `git diff
  --check` passed.
- `scripts/build-release.sh` produced static-musl DEB/RPM artifacts at package
  revision 101. Lockfile and executable audits completed with zero findings.
- Candidate binary SHA-256:
  `8f9a6f631f6c08b9df2fad7516d0fe8808ce76d5cb1c5371aee23f0d67e533c1`.
- DEB SHA-256:
  `3ef3b1abb94b674d0ea620ce6eb86d0f53e2872274e6d9cd0462335d44ed345b`.
- Embedded source-tree SHA-256:
  `8b58ae2192ba42777a404e0d7a3578ef623ce324cb2385ec684c981bc25355f8`.
  The build manifest records `worktree_dirty=true` because qualification
  evidence files were untracked during this build; the compiled source commit
  and complete source file digests are recorded in the manifest.

After committing all evidence, a second official static-musl build produced
package revision 102 from commit `59f846c616f2a8e5ee6e9de9e5de26de34f2b2f3`
with `worktree_dirty=false`. Its binary SHA-256 is
`7a914d9464229f11b69635e8401bc0f183472f6d6a3e744fa77aeba7baea9000`, and its
DEB SHA-256 is
`fdb978491b1e8cde584bd6cafd384f625717a9c63d55c6369aae528f8856481a`.
The Rust source is identical to commit `5590978` used by the live runtime
test (`git diff 5590978 59f846c -- src` is empty); the later commit records
qualification documentation and artifacts. Package 102 itself has not been
installed on VPS2; the runtime evidence below is from package 101.

## Before/after runtime evidence

The earlier package 100 run was observed from 11:09:08 to 11:30:09 UTC. The
daemon remained dirty and started repeated root observations at 11:14:09,
11:17:09, 11:22:09, and 11:27:11. It did not restart. At stop, systemd reported
12m43s CPU, 8.9 GiB read, and 15.7 GiB written for the service. This is a
single runtime, not a controlled benchmark.

The package 101 candidate was started at 11:30:52 UTC. While its initial
1.44M-node observation was active, a bounded burst created and deleted 40,000
empty files under `/tmp/dux-pressure.*` in about one second. The trap removed
the temporary directory. The burst overflowed Dux's userspace hint ring and
resolver queue repeatedly; the kernel overflow counter stayed at zero.

The first observation ended at 11:33:07 with one coverage issue and recorded
one bounded retry after 120 seconds. The quiet retry started at 11:35:07 and
completed at 11:37:42 with **zero coverage issues** across 1,294,356 files and
155,553 directories (1,444,711 indexed inodes). Hint-ring overflow warnings
continued after the successful pass. The status remained honestly dirty with
`fanotify hint ring capacity was exceeded`; it did not claim `caught_up`.

There was no new scan at the old 300-second dirty-retry boundary (11:40:07),
or through 11:49:30, despite continued hint-ring warnings. The 15-minute
observation then started at 11:50:07, exactly 15 minutes after the retry scan
started. It completed at 11:53:14 with **155 conflicting inode incarnations**
(inode numbers observed with different birth times), retained dirty status,
and scheduled a bounded retry after 240 seconds. This avoided a false-clean
result, but shows that the current full walk still fails to converge under
the VM's continuing churn. The VM runs ClickHouse and Docker workloads; Dux's
sampled event rate briefly reached about 10,900 events/sec. That identifies a
high-churn environment, but does not prove which workload caused each inode
reuse.

The daemon had zero restarts throughout. At 11:53:40, systemd reported 541
seconds CPU, 4.4 GiB read, and 7.7 GiB written since package 101 started.
These totals cover the initial incomplete observation, its successful retry,
and the scheduled 15-minute observation; I/O remains substantial. The retry
after the 15-minute pass began at 11:57:14 and completed at 12:00:09 with zero
coverage issues across 1,298,114 files and 155,970 directories (1,444,084
indexed inodes). The completion log says filesystem or fanotify hints changed
during the observation, so the index correctly remained stale. At 12:02:23,
Dux was active with zero restarts, but still dirty and not caught up; it
reported 6,274,821 events seen, zero resolved events, zero kernel overflows,
and frequent userspace hint-ring overflow warnings. This confirms the retry
can finish after the inode-incarnation conflicts, but does not prove
convergence while churn continues. Cumulative service counters at that point
were 739.7 CPU seconds, 7.2 GiB read, and 10.8 GiB written since package 101
started. These counters include all scans and runtime work and are not an
isolated performance benchmark. The full snapshot and completion lines are in
`post-retry-runtime-snapshot-1202.log`.

Independent read-only index checks after the successful retry reported:

- `PRAGMA quick_check`: `ok`.
- Indexed inode rows: 1,444,711; non-integer `recursive_bytes` rows: 0.
- The same recursive accounting predicate used by Dux: `true`.

Those read-only checks were taken after the successful 11:35 retry and before
the 11:50 scheduled scan; they do not validate the later incomplete scan's
published view.

The daemon's status freshness intentionally remains stale after hint loss. This
test proves the old 300-second dirty retry was removed for this workload and
that the 15-minute timer fired on schedule. It does not prove zero hint loss,
the 15-minute scope-age SLO, recovery under continuous churn, or production
readiness. The 40k burst caused a real scan error; the scheduled scan later
found 155 inode-incarnation conflicts under continuing VM activity. The
bounded retry completed with zero coverage issues, but the index remained
stale due to continuing hint loss, so convergence during ongoing churn is
still unproven.

The next scheduled observation started at 12:12:14, exactly 15 minutes after
the 11:57 retry began. It completed at 12:15:03 with **45 inode-incarnation
conflicts** across 1,300,583 files and 156,239 directories. The prior successful
scan remains the latest accepted observation; Dux stayed dirty and scheduled
another retry with an 8-minute backoff. At 12:15:12, systemd still showed zero
restarts, while cumulative service I/O had grown to 8.8 GiB read and 13.5 GiB
written (939.9 CPU seconds). This second cadence sample confirms the timer
fires but also reproduces incomplete scans on a busy ext4 VM. The current
scanner deliberately counts any `(device,inode)` group with different birth
times as a coverage error (`count_conflicting_inode_incarnations` in
`src/scan.rs`); suppressing that check would risk publishing stale path or
allocation data because `inodes` is keyed by `(dev_id,inode)`. A safe fix needs
to validate and repair affected paths/scopes or use a stable filesystem
snapshot, not simply ignore the conflict. Snapshot:
`post-cadence-runtime-snapshot-1215.log`.

The bounded retry started at 12:23:03 and completed at 12:26:29 with **35
inode-incarnation conflicts** across 1,302,352 files and 156,451 directories.
Dux kept the previous accepted index dirty and raised the retry backoff to 16
minutes. At 12:30:53, the daemon remained active with zero restarts, but had
seen 12.28M events with zero resolved and continued userspace hint-ring loss;
kernel overflows remained zero. Cumulative service counters since 11:30:52
were 1,213.2 CPU seconds, 11.2 GB read, and 17.5 GB written. This makes three
incomplete scans out of the four observations/retries since 11:50; only the
11:57 retry completed without coverage errors, and it still remained stale
because changes raced with the walk. It does not demonstrate stable
convergence. Full snapshot: `post-retry-runtime-snapshot-1230.log`.

## Artifacts

- `build-manifest-101.json`, `SHA256SUMS-101`, and the two Cargo audit outputs
  identify the release artifacts.
- `prepatch-journal.log` and `prepatch-status.log` preserve package 100's
  repeated-scan run.
- `candidate-journal-101.log`, `candidate-status-samples-101.log`,
  `candidate-five-minute-boundary-101.log`, and
  `candidate-fifteen-minute-cadence-101.log` preserve package 101's runtime.
- `post-retry-runtime-snapshot-1202.log` records the completed bounded retry,
  stale status, service counters, and runtime binary identity.
- `post-cadence-runtime-snapshot-1215.log` records the next scheduled scan,
  its 45 inode-incarnation coverage errors, retry state, and service counters.
- `post-retry-runtime-snapshot-1230.log` records the subsequent retry with 35
  conflicts and the daemon's one-hour cumulative CPU and I/O counters.
- `post-fifteen-minute-scan-db-check.json` records the structural SQLite
  check during the later retry; it explicitly does not claim index freshness.
- On VPS2, `/root/dux-qualification-43a7769/pre-5590978.sqlite` is the verified
  pre-upgrade database backup; the package rollback files from the preceding
  qualification remain in that directory.

## Package 103 and static-release qualification — 2026-11-08

Package revision 103 was built from clean source commit `e90f3c346b8b58883edf275c90a4d81c31c15656` with the official release script. The release manifest records `worktree_dirty=false`, source-tree SHA-256 `bf91e97343b3f95b83aa32aad6eeea00016e45abc90391724d25ac96b32c5b52`, and the static-musl executable SHA-256 `8c107f0ce62565ed5747e8f28c801b236c870cf5c52e652d7ef73d3bb259fe42`. The package lockfile audit found 0 vulnerabilities across 229 dependencies; the embedded binary audit recognized 202 dependencies. The advisory database snapshot was last updated 2026-11-06, so this is not a live audit of later advisories.

The exact static executable passed the real overflow integration suite on the VPS2 KVM guest (Ubuntu 24.04, Linux 6.8, ext4). The harness forced one kernel overflow by creating 18,432 files against a 16,384-event queue, then observed a full scan, independent accounting verification, advanced scan generation, and exact filesystem path/inode/allocation/rollup comparison. Recovery took 7.494 seconds. The index correctly remained marked stale after post-scan hints; exactness passed while `clean_status_after_recovery=false`. The machine-readable result is `forced-overflow-e90f3c3.json`. This is one controlled overflow on one ext4 fixture, not a repeated-storm or multi-day test.

The DEB lifecycle harness completed upgrade to 103, rollback to 102 with a verified SQLite snapshot restore, re-upgrade, purge, and clean install of 103 (total 831.819 seconds). Package hashes and all phase evidence are in `package103-lifecycle.json`. The 102 baseline scan encountered a hint-ring-capacity dirty reason during the walk. The clean install completed a full root observation and passed service sandbox, package payload/hash, filesystem access policy, and no-default-tunnel-identity checks. At the captured instant, daemon status was still dirty with `reconciliation accounting verification pending` and `caught_up=false`; the test intentionally treats a complete last-observed scan as readiness in scan-first mode. A separately run read-only verifier on the preserved clean-install database passed SQLite `integrity_check`, root byte/inode totals, orphan-entry checks, unique prime attribution, and all directory rollup predicates. This proves database structure/accounting, but the clean-install database was not compared against an independent full-root path listing in this lifecycle test.

The lifecycle harness leaves Dux stopped and disabled by design. After capturing its report, the guest's pre-test database was restored from the verified backup, package 103 was retained, and the system service was enabled and started again. The raw clean-install database snapshot remains on VPS2 at `/root/dux-canary-e96e708/index-clean-install-103.db` (SHA-256 `045ac20abbe5961236745983bd667ad6b892aaf416b9e140fe1dc43bbc003841`); it is a qualification artifact, not production state.

The restored package 103 service then completed a full root observation from 14:23:30 to 14:26:12 UTC (162 seconds), scanning 1,322,840 files and 158,662 directories with zero coverage issues. Its independent accounting verifier passed at 14:26:52 UTC (40.6 seconds after scan completion). The service was active with zero restarts, kernel overflows, or userspace update drops. Its view was dirty because filesystem hints arrived after the completed observation; `caught_up=false` is expected for this last-observed model. VPS2 had 9.9 GiB free on `/` after the lifecycle run and restoration.

Still unqualified: repeated overflow storms, exact full-root comparison after a clean package install, completed VPS2 candidate canary, separate production-host canary, 72-hour soak, explicit freshness/scan-duration SLO under continuous churn, larger-scale 10M+ file performance budgets, other filesystems/kernels, power loss/media corruption, and package behavior on an RPM-based host. The lifecycle pass is therefore package-transition evidence, not a production-readiness sign-off.

## VPS2 scan-first candidate canary in progress — started 2026-11-08 14:52:55 UTC

The prior canary collector required `caught_up=true`/`dirty=false`, so it could never qualify scan-first mode, whose UI/API truthfully remains dirty when filesystem hints arrive. The collector now requires a fresh completed observation (20-minute maximum age by default), live watch health, schema-v7 accounting/path graph invariants, unchanged qualified binary/unit hashes, bounded service memory/CPU, no restart/OOM increase, and no accounting/coverage/write errors. Scan-first hint-loss/overflow markers are retained in the evidence but do not by themselves fail the sample while observation freshness and accounting checks pass. Legacy mode still requires clean/caught-up status.

The complete Python script suite passed **323 tests** (27 focused monitor tests). Four short preflight runs exposed and fixed monitoring defects: the first missed nonzero scan-coverage counts in journal lines; the second treated systemd's normal scan-time `reloading` state as inactive; the third prompted a fail-closed check for unclassified critical-event counts; the fourth showed that post-scan accounting verification can remain pending briefly after `scan_in_progress` clears while systemd still reports `reloading`. That exact state is now recognized only when the status reason names the pending verifier. None of these runs counts toward the canary. The final monitor started as `dux-scanfirst-canary-20261108-v5.service` at 14:52:55 UTC. It samples hourly, uses a 1,200-second maximum observation age, and is capped at 20% CPU and 1 GiB memory. Sample 1 passed at 14:53:49 UTC with status dirty as expected, last-scan age 865/919 seconds before/after the snapshot, valid v7 path/accounting checks, and no critical journal events. Monitor SHA-256: `95b68de248f1168b2e1ca5f57a18b12ee139ecf37bd3ba43bab3c25e284bb8bc`; observer SHA-256: `fdc693097afbd3d78861ded24a8515b2c52f8b5529c9e8d6a033f7f7997b0509`. Candidate binary SHA-256: `8c107f0ce62565ed5747e8f28c801b236c870cf5c52e652d7ef73d3bb259fe42`; unit SHA-256: `e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.

This is a candidate qualification run on VPS2, not a canary on the production host. The live JSONL evidence is on VPS2 at `/root/dux-canary-e96e708/scanfirst-canary-103-v5.jsonl`; the 24-hour run has not completed. The initial one-sample checkpoint is `canary-v5-initial.jsonl`; the four discarded preflight runs are `canary-preflight-1.jsonl` through `canary-preflight-4.jsonl`. Hourly sampling means a failure can take up to an hour to be detected. This canary bounds last-observation age and checks database accounting, but does not promise event-by-event delivery or real-time freshness.
