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
or through 11:41:34, over five minutes after the successful scan began. The
daemon had zero restarts. At the end of that observation window, service
counters were 311 seconds CPU, 3.1 GiB read, and 5.8 GiB written. Those totals
include the initial incomplete scan and its required retry; they show that
this is still substantial I/O even after removing the unnecessary repeated
scan trigger.

Independent read-only index checks after the successful retry reported:

- `PRAGMA quick_check`: `ok`.
- Indexed inode rows: 1,444,711; non-integer `recursive_bytes` rows: 0.
- The same recursive accounting predicate used by Dux: `true`.

The daemon's status freshness intentionally remains stale after hint loss. This
test proves the old 300-second dirty retry was removed for this workload; it
does not prove zero queue loss, a 15-minute freshness SLO, recovery under
continuous churn, or production readiness. The 40k burst caused a real scan
coverage issue, and the bounded retry was necessary. The current high event
rate still overflows the userspace hint queues, and a five-minute no-rescan
window does not qualify the full 15-minute cadence or long-term behavior.

## Artifacts

- `build-manifest-101.json`, `SHA256SUMS-101`, and the two Cargo audit outputs
  identify the release artifacts.
- `prepatch-journal.log` and `prepatch-status.log` preserve package 100's
  repeated-scan run.
- `candidate-journal-101.log`, `candidate-status-samples-101.log`, and
  `candidate-five-minute-boundary-101.log` preserve package 101's fault run.
- On VPS2, `/root/dux-qualification-43a7769/pre-5590978.sqlite` is the verified
  pre-upgrade database backup; the package rollback files from the preceding
  qualification remain in that directory.
