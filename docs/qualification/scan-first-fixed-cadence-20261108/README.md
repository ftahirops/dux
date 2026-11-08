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
