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

### Canary collector v3 progress (updated 2026-11-08 22:45 UTC)

The v2 monitor was stopped after sample 13 so the canary could use the new
collector with per-device cgroup-v2 I/O. The v2 run was partial: 13 samples
passed from 21:07 through 22:07 UTC, but this is not a 24-hour soak. Its final
sample remains preserved in
[`live/canary-24h-5m-v2-progress.jsonl`](live/canary-24h-5m-v2-progress.jsonl).

The replacement transient unit `dux-canary-409a2f4-v3.service` is active on
VPS2. Samples 1–8 passed at 22:10:34, 22:14:58, 22:19:24, 22:25:10, 22:30:06,
22:35:35, 22:40:17, and 22:44:56 UTC. Collection took 169.6, 133.6, 99.1,
145.0, 141.7, 170.0, 152.5, and 131.3 seconds, with no monitor errors. The
expected binary, service unit, and monitor hashes were verified by the
collector. Dux had zero restarts, kernel overflows, userspace drops, and OOM
events across these samples. SQLite, schema-v7 graph, and accounting checks
passed. The
collector recorded cgroup I/O for device rows `252:0` and
`8:0`; they are retained separately because they are stacked devices and must
not be summed as independent physical I/O.

These samples are not a completed soak. At sample 1 the index was dirty and a
periodic full observation was in progress; at sample 2 it was dirty with no
scan in progress. The observation age was 1,042, 407, 672, 1,018, 415, 743,
1,025, and 405 seconds across samples 1–8. These readings met the monitor's
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

Sample 7 caught the next full observation in progress. The VPS2 service journal
shows it began at 22:38:11 and completed at 22:42:03: 1,353,657 files,
161,163 directories, 1,514,820 staged paths, and zero coverage issues. The
daemon's accounting verification passed at 22:43:13 after 69.7 seconds. Across
41 watcher-pressure records from 22:38:10 to 22:44:53, observed event rates
ranged from 10,939 to 24,383 per second; queue bytes peaked at 1,680, with zero
queue-pressure episodes and zero overflows. Sample 8 then passed with the scan
complete, age 405 seconds, no scan in progress, and the index dirty while
waiting for the next observation. Raw records are in
[`live/vps2-scan-pressure-20261108-2242.log`](live/vps2-scan-pressure-20261108-2242.log)
and the v3 JSONL. This is a useful high-event-rate single-host result, not a
repeatable controlled burst test or proof for other filesystems and kernels.

The scan-plus-accounting interval from samples 7→8 used about 244.3 CPU-seconds
over 278.8 seconds (87.6% of one core) and added about 1.75 GiB read and 1.66
GiB written in the Dux service cgroup counters. The next optimization work must
address this measurable scan cost; the zero-overflow result does not make the
resource cost negligible. One WAL checkpoint warning occurred in this window:
busy=0, 7 log frames, 3 checkpointed, 34.4 MB WAL. That is below the 256 MiB
write-pause guard and did not fail sample 8, but recurrence and WAL trend should
be checked during the soak. The available evidence does not establish whether
the canary observer contributed to the pinned frames.

For a rough process-cost view, cgroup CPU counters from samples 3–6 imply
about 307.5 CPU-seconds over 970.9 wall-seconds (31.7% of one core). This
period includes the scan and cleanup interval: the sample 4→5 interval used
about 67% of one core and the next about 17%. Dux service block-I/O counters
rose by about 1.32 GiB read and 1.63 GiB written during sample 4→5, then did
not move during sample 5→6. Stacked device rows report essentially identical
values and must not be summed. These are short, mixed-workload observations,
not a steady-state benchmark or acceptance result; no CPU/I/O budget exists.

The v3 collector focused suite passed 34/34 tests. Full Python discovery was
rerun after this change at 22:14 UTC: **330 tests passed**. The captured v3
JSONL has since been refreshed from VPS2 through sample 13. Samples 1–12
passed; sample 13 failed because status-before was paused under host I/O
pressure. The error text incorrectly described this as a stale observation.
At sample 13 the daemon was alive with zero restarts, kernel overflows, and
userspace drops, but the sample did not pass freshness. The VPS2 journal then
recorded a reconciliation cancellation under memory pressure followed by a
retry. A follow-up read-only status showed the daemon active and a new scan in
progress; this is recovery evidence, not a passed canary sample. The 24-hour
soak is incomplete. See the complete append-only sample history in
[`live/canary-24h-5m-v3-progress.jsonl`](live/canary-24h-5m-v3-progress.jsonl).

The monitor now reports the specific failed health condition, including a
pause reason, instead of conflating paused status with stale scan age. Its
focused suite passes **35/35** and full Python discovery passes **331/331**.
The current Rust working tree passes all-features release tests on VPS2
(321 passed, 2 ignored), all-features llvm-cov tests (321 passed, 2 ignored),
strict Clippy, formatting, and diff checks. Exact-tree coverage is 72.17% line,
70.99% region, and 76.00% function; stable branch coverage is unavailable.

The scan-order optimization's repeated 250,000-entry release benchmark gave
unchanged-reconcile timings of 11.127 s baseline / 10.915 s candidate in the
first valid pair and 21.111 s / 15.640 s in the second. Although the second
pair favors the candidate by 25.9%, its atomic scan times also shifted from
11.926/11.976 s to 17.540/14.591 s. The VPS2 workload varied enough that these
measurements do not establish a reliable speedup; the optimization is retained
for correctness and removal of a staging pass, with performance still an open
qualification item. All four valid test invocations (two baseline and two
candidate) passed and produced 79,859,712-byte databases. A first retry attempt
with an incorrect exact-name filter ran zero tests and is excluded from these
results.

## Fresh VPS2 soak and dependency audit — 2026-11-08 23:29 UTC

The previous v3 canary remains failed at sample 13 and is stopped. A separate
v4 run started at 23:27:17 UTC with a new JSONL file and the corrected monitor
(SHA-256 `6599d0227bd6ea9cbd5295530672bb35cacf8faea4bd71e67d8f7e76f811ec8d`).
Samples 1–4 passed at 23:28:13, 23:33:07, 23:37:54, and 23:43:21 UTC. Each
sample verified the release-106 service binary and unchanged unit hashes,
SQLite/schema-v7 graph, and accounting. The observation ages were 1,045, 439,
727, and 1,054 seconds; all samples had zero restarts, kernel overflows,
userspace drops, and OOM events. The daemon remained dirty awaiting the next
observation. The service's cumulative memory peak was 2.03 GiB under its 4 GiB
limit. Sample collection took 36.8–64.5 seconds. This is the older installed
service binary, so v4 does not qualify the uninstalled scan optimization. The
24-hour soak is still in progress; four samples do not qualify its gate. The
captured progress is preserved in
[`live/canary-24h-5m-v4-progress.jsonl`](live/canary-24h-5m-v4-progress.jsonl).

`cargo audit --json` passed at 23:29 UTC: zero known vulnerabilities and zero
warnings across 229 locked dependencies. The advisory database snapshot was
updated at 23:03 UTC. Raw result:
[`cargo-audit-20261108-2329.json`](cargo-audit-20261108-2329.json). This is
dependency audit evidence, not a full application security assessment.

## VPS2 v4 canary refresh — 2026-11-08 23:49 UTC

The live VPS2 v4 progress JSONL was refreshed from the active collector at
23:49 UTC. Five samples passed, at 23:28:13, 23:33:07, 23:37:54, 23:43:21,
and 23:48:03 UTC. Every sample reported zero service restarts, kernel
queue-overflow notifications, userspace update drops, and OOM events. SQLite
quick checks, schema-v7 path-graph checks, and accounting checks passed. The
latest sample's completed scan was 435 seconds old and the index was dirty
while awaiting its next periodic observation. The sample duration was 45.7
seconds. The canary service is still active; the 24-hour gate remains open.

A read-only `systemd-analyze security --offline=yes packaging/dux.service` on
the local systemd 255 host scored the checked-in service unit **3.3 OK**. This
is not the policy loaded on VPS2: the live canary verifies its installed unit
hash `e12d1f4a…`, and it runs release 106. Production policy remains a separate
open gate; the last production policy snapshot (Nov 3) was 5.1 MEDIUM and
showed weaker core-dump, SecureBits, and syscall-filter settings than the
checked-in unit. No service policy was changed in this qualification step.
