# VPS2 qualification for release 106

## Artifact provenance

Release 106 was built by `scripts/build-release.sh` from clean commit
`409a2f4db44ab38d8023fcaa927a9bd9b4feacca` (version 0.6.3), target
`x86_64-unknown-linux-musl`. Source-tree SHA-256:
`1169366a051e6e6ce90557eeea711ac4ca97f718382a9e75534665376a12ad34`.

- Static executable SHA-256: `0f2c14df15cbcaec0c934d409703843fc03e35fd089287234653e01be250a556`.
- DEB SHA-256: `50b1553b2b3bd0c894b9478885275e8d90a2d4358d256c5d59ce5c5727eb624a`.
- RPM SHA-256: `b0e4e621d61924d10eacf568490baa03794ba05293e4655ba62eb03f49855a23`.
- Release manifest reports a clean worktree and zero known lockfile advisories
  in the RustSec database snapshot dated 2026-11-06.

All eight artifacts listed by `SHA256SUMS` verified locally. The binary embeds
the expected commit and source digest, is static PIE, and the DEB/RPM payload
hashes match it. The candidate's read-only `containers --json` command ran on
VPS2 and returned 29 containers with valid JSON.

## VPS2 install and rollback protection

VPS2 is the authorized disposable KVM qualification guest at
`192.168.5.106`; production VPS1 was not changed. Before installation, a
SQLite online backup of `/var/lib/dux/dux.db` was made with schema version 7,
`quick_check=ok`, 602,824,704 bytes, and SHA-256
`9eec2e0c64d26ebff91bb0cfa6ef8889d0e458c4911af5bce016483956d5f282`. Its
manifest is in [`live/preinstall-backup-manifest.json`](live/preinstall-backup-manifest.json).
The old executable SHA was
`d7e41c5361011bc6acb2caa9b716488b3c5354f17b7c9b7bcc69ce73b2127b7e`; the old
unit SHA was `e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.
Both files were retained on the guest in the private
`/root/dux-canary-409a2f4/` directory.

The package upgraded installed revision 103 to 106. Its maintainer script
used `systemctl try-restart`; because the old daemon had been stopped for the
upgrade, that left the service inactive. It was explicitly started, then the
running `/proc/PID/exe` and `/usr/bin/dux` hashes both matched release 106. The
new daemon stayed active with zero restarts.

## Source-matched scan-first observation

The candidate started at 2026-11-08 20:36:40 UTC and completed its first
full-root observation at 20:39:39 (179 seconds): 1,382,202 files, 165,729
directories, 1,547,931 staged path instances, zero unresolved reuse paths,
zero coverage issues, and two present file identities refreshed after inode
reuse. Its in-daemon independent accounting verifier passed at 20:40:36 in
57.351 seconds. The daemon had zero restarts and zero kernel overflow
notifications. The verified root total was 86,670,553,088 allocated bytes
across 1,542,733 unique inodes.

A separate read-only `dux-live-observer.py` snapshot passed SQLite
`quick_check`, v7 path-graph invariants, root-to-inode totals, prime attribution,
orphan checks, and sampled recursive rollups. The result is preserved in
[`live/postscan-accounting.json`](live/postscan-accounting.json). This is
independent structural/accounting verification; it is not an exact full-root
path comparison.

## Additional live test and failed attempts

The 5,000-operation real-fanotify event-apply harness passed on the same
release binary using an isolated ext4 loop fixture in a private mount
namespace. It created 5,764 Go-build-shaped inodes across four 480-directory
bursts, removed all burst trees before convergence, found zero stale build
dirents and zero directory rollup mismatches, and matched its independent
filesystem oracle. The run also passed its exact fixture accounting and 32
SQLite WAL lifecycle cycles in 16.113 seconds. The full result is
[`live/event-mode-live-test.json`](live/event-mode-live-test.json).

Earlier attempts were invalidated by qualification-harness defects, not counted
as passing product tests: the first omitted `--event-apply` while its gate
expected event convergence; a shared-root event-mode run then observed other
host churn and failed closed; the first private ext4 run rejected status
version 5 because the harness accepted only versions 1–4. The harness now
selects event-apply explicitly and accepts version 5. These failures and their
raw reports are retained remotely; the final isolated run passed.

## Canary evidence and observer correction

A 60-second monitor calibration's first sample passed, but its measured
collection time was about 74 seconds and cost about 15 CPU-seconds at a 20%
CPU cap. That collector was stopped to reduce qualification overhead. A
five-minute collector at a 10% cap passed sample 1, then falsely failed sample
2 because it compared `status_before`'s heartbeat to the time a slow accounting
query finished. That sample took about 159 seconds; the heartbeat was fresh
when `status_before` was read and the later `status_after` heartbeat was fresh.
The collector also slept a full interval after each query, which could not
meet its own required 24-hour sample count.

Both monitor issues are fixed in the current source: each heartbeat is checked
against that status read's capture time, and samples use fixed start-relative
deadlines with an explicit cadence-overrun failure. Two regression tests cover
these behaviors. The full Python suite now passes 325 tests, including 29
canary-monitor tests. A fresh 24-hour canary must run with this corrected
collector; the earlier failed evidence is preserved in
[`live/failed-5m-canary-monitor-calibration.jsonl`](live/failed-5m-canary-monitor-calibration.jsonl)
and does not count as a pass.

### Corrected 24-hour canary progress (2026-11-08 22:02 UTC; superseded)

The corrected five-minute canary started at 21:05:37 UTC and remains active.
Its first twelve samples (21:07:13, 21:12:14, 21:17:10, 21:22:33, 21:27:17,
21:32:04, 21:37:21, 21:43:18, 21:47:36, 21:52:26, 21:58:42, and 22:02:06 UTC)
passed with no monitor errors. All samples matched the release binary and unit
hashes; SQLite quick-check, schema-v7 graph, and accounting checks passed. At
sample 12 the service had zero restarts, kernel overflows, userspace drops, and
OOM events. The latest full observation was 535 seconds old, with no scan in
progress. The index was dirty
after filesystem changes. The reported
`behind_seconds` is the
legacy event-pipeline catch-up timer; `last_scan_age_seconds` is the relevant
scan-first freshness measure. Twelve samples are an early checkpoint only, not a
soak pass. Peak Dux memory was about 1.51 GiB against the 4 GiB service limit.
The slowest collection so far took 184.1 seconds, below the 300-second sample
interval. Across sample intervals 1→12, Dux used 1,517.8 CPU-seconds over
3,293.4 wall-seconds (46.1% of one core on average), with interval rates from 16.5% to
86.8%. A cgroup snapshot at 21:34 showed throttling in 8,240 of 31,374 CPU
periods (26.3%) and cumulative service I/O of 7,471,202,304 bytes read and
6,704,386,048 bytes written. These early totals include initial indexing and
must not be projected as steady state. The monitor checks the one-core hard
CPU ceiling and memory limit, but does not enforce a CPU or I/O budget; these
passes therefore do not establish a defined efficiency target. The raw
append-only snapshot through sample 12
is [`live/canary-24h-5m-v2-progress.jsonl`](live/canary-24h-5m-v2-progress.jsonl).

The active collector was not changed during the soak. A newer collector source
now records cgroup-v2 I/O counters separately per device and rejects malformed
or missing counters; its focused tests pass 34/34. A read-only live cgroup check
shows that the same I/O can appear on stacked device rows, so the report keeps
raw per-device values and does not add them together. The active run's monitor
hash is still the previous version and its records cannot establish per-device
I/O rates. The last full Python discovery run (325 tests) predates the new
collector tests; the focused suite was rerun, but full discovery was not.

A fresh `cargo audit --json` at 22:01 UTC found zero known vulnerabilities and
zero warnings across 229 dependencies. Its advisory database was last updated
2026-11-06; the raw result is
[`cargo-audit-20261108-2201.json`](cargo-audit-20261108-2201.json).

This VPS2 qualification is not production sign-off. It covers one KVM guest,
Ubuntu 24.04, Linux 6.8, and ext4. A completed 24-hour candidate canary,
production-host canary, sustained overflow storm, longer soak, exact live
full-root comparison, and other filesystem/kernel/package targets remain open.

### VPS1 read-only backlog update (2026-11-08 21:58 UTC)

The legacy production daemon still reports dirty and not caught up, with 295
cumulative kernel overflows and no userspace drops. Pending updates fell from
23,807 at 21:50 to 181 at 21:58; however, the last full scan was 292,172 seconds
old (about 81.2 hours). The recent queue drain is not recovery of the stale
whole-filesystem view. No production service or database changes were made.

### VPS1 read-only recheck (2026-11-08 22:02 UTC)

The overflow counter rose from 295 to 312 (+17) since 21:58. The legacy daemon
remains dirty and not caught up, with 78 pending updates, no userspace drops,
and a last full scan 292,410 seconds old (about 81.2 hours). Its journal had
248 slow-flush and 17 stale-handle warnings during that interval. The overflow
increase confirms the brief backlog drain was not recovery. No production
service or database changes were made.

### Canary collector v3 progress (updated 2026-11-08 22:35 UTC)

The v2 monitor was stopped after sample 13 so the canary could use the new
collector with per-device cgroup-v2 I/O. The v2 run was partial: 13 samples
passed from 21:07 through 22:07 UTC, but this is not a 24-hour soak. Its final
sample remains preserved in
[`live/canary-24h-5m-v2-progress.jsonl`](live/canary-24h-5m-v2-progress.jsonl).

The replacement transient unit `dux-canary-409a2f4-v3.service` is active on
VPS2. Samples 1–6 passed at 22:10:34, 22:14:58, 22:19:24, 22:25:10, 22:30:06,
and 22:35:35 UTC. Collection took 169.6, 133.6, 99.1, 145.0, 141.7, and 170.0
seconds, with no monitor errors. The expected binary, service unit, and monitor hashes
were verified by the collector. Dux had zero restarts, kernel overflows,
userspace drops, and OOM events across these samples. SQLite, schema-v7 graph,
and accounting checks passed. The
collector recorded cgroup I/O for device rows `252:0` and
`8:0`; they are retained separately because they are stacked devices and must
not be summed as independent physical I/O.

These samples are not a completed soak. At sample 1 the index was dirty and a
periodic full observation was in progress; at sample 2 it was dirty with no
scan in progress. The last completed observation was 1,042, 407, 672, 1,018,
415, and 743 seconds old across samples 1–6. These readings met the monitor's
bounded scan-first freshness gate but do not establish real-time freshness.
Dux remained below its 4 GiB memory cap; peak memory was about 1.51 GiB. There
is no CPU or I/O efficiency budget defined yet, so these samples do not
establish that overhead is low enough. Between samples 3–5, a cleanup removed
about 6.9 GiB of stale Rust build outputs under `/var/tmp` while the daemon was
active. The Dux event counter rose by about 12.7 million during those two
intervals, with no reported kernel overflow or userspace drop. Afterwards,
Docker's unused build cache was pruned: Docker reported 2.288 GiB reclaimable,
while filesystem availability improved by about 1 GiB (13 to 14 GiB free).
The remaining 6.402 GiB of reclaimable images, 1.955 GiB of stopped containers,
and 366.8 MiB of dangling volumes were left untouched because their roles and
rollback/data value are not established. This was an
uncontrolled cleanup workload, not a repeatable event-rate stress test; the
coincidence does not establish that all of the events came from the cleanup.

The v3 collector focused suite passed 34/34 tests. Full Python discovery was
rerun after this change at 22:14 UTC: **330 tests passed**. The v3 JSONL remains
live on VPS2 at `/root/dux-canary-409a2f4/canary-24h-5m-v3.jsonl`; its captured
progress through sample 6 is also preserved in
[`live/canary-24h-5m-v3-progress.jsonl`](live/canary-24h-5m-v3-progress.jsonl).
The v3 canary is still in progress.
