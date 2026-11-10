# r132 72-hour dedicated soak on VPS2

## Current state: running

Started **2026-11-10 09:04:48 UTC** on the authorized VPS2. The exact r132
static binary SHA-256 is
`998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76` (source
commit `f02ba7ad235ea841a99de27b69fcd402e3c3927c`). The first progress report
showed 1,441 operations at 60 seconds. This is startup evidence only; the soak
has not passed.

The test runs as transient unit `dux-readiness-1304925.service` with a private
mount namespace, **25% CPU quota**, **4 GiB memory ceiling**, and low I/O weight.
Its 768-MiB disposable ext4 loop image and database are backed by `/dev/shm`;
the persistent report is in
`/var/tmp/dux-r132-soak-20261110/evidence/` on VPS2. The estimated unit runtime
limit is 73h40m, leaving time for final assertions and cleanup after the
72-hour workload. Expected workload completion is approximately
2026-11-13 09:04:48 UTC.

The installed r121 service is not modified by this test. It was active with
zero restarts at soak start. The test can add bounded CPU pressure to the guest
for three days; the dedicated worker's CPU and memory are cgroup-limited.

### Live checkpoint — 2026-11-10 10:03 UTC

The worker and candidate daemon were both still active at the 10:02:21 UTC
read-only check; the systemd unit reported zero restarts. At 10:02:50 UTC, the
durable progress file showed **82,405 mutations** after 3,481.5 seconds, with
82 exact-check samples through operation 82,000. The latest exact comparison
completed in **277 ms**; sampled daemon RSS was **4,376 KiB**, open file
descriptors **14**, and WAL size **4,301,312 bytes**. No mutation burst, held
reader, or controlled crash/recovery event has run yet. Final exactness,
history, and SQLite integrity are therefore still pending. This is progress,
not a soak pass. The captured progress JSON is
[`soak-progress-20261110T1002Z.json`](soak-progress-20261110T1002Z.json).

### Hourly burst checkpoint — 2026-11-10 10:05 UTC

The first scheduled **500-file hourly burst** had completed. At 10:05 UTC,
durable progress recorded `mutation_bursts: 1` after 3,601.6 seconds and 85,758
total operations. The harness increments this counter before `daemon.exact`
and `case.history`, then writes the durable progress checkpoint only after both
assertions succeed. This therefore proves the first burst's exactness and
history checks passed. The latest regular 1,000-operation sample was operation
85,000: exact convergence **890 ms**, RSS **4,388 KiB**, 15 file descriptors,
and a **4,301,312-byte WAL**. The six-hour held-reader and twelve-hour crash
recovery remain pending. See
[`soak-progress-20261110T1005Z.json`](soak-progress-20261110T1005Z.json).

### 100k-operation checkpoint — 2026-11-10 10:16 UTC

At elapsed **4,262.4 seconds** (about 71 minutes), the soak had completed
**101,438 mutations**. The last exact check, at 101,000 operations, passed in
**1,082 ms**; the progress file contained 101 exact-check samples. The worker
remained `running` with zero restarts. One hourly burst had passed, while the
held-reader, controlled crash/recovery, and final exact/history/integrity
assertions remain pending. This is a 100k-operation soak milestone, not a
72-hour pass. The full snapshot is
[`soak-progress-20261110T1016Z.json`](soak-progress-20261110T1016Z.json).

### Early latency and resource profile — 2026-11-10 10:19 UTC

At elapsed **4,442.5 seconds**, the worker had completed **105,689 mutations**
and **105 exact-check samples**; the unit remained active with zero restarts.
Across those checks, end-to-end exact convergence was **229 ms minimum, 796 ms
median, 1,148 ms mean, 2,634 ms p95, and 9,028 ms maximum**. These timings
include requesting a full scan-first observation, waiting for accounting
verification, and the harness's independent tree/index comparison; they are
not scanner-only benchmarks. Sampled daemon RSS ranged from **4,244 to 6,568
KiB**; the largest observed WAL was **4,412,552 bytes**. Systemd's whole-unit
peak was about **68 MiB**, with about **150 CPU-seconds** accumulated under its
25% CPU quota.

The host-pressure sample at 10:19:54 had six logical CPUs, load averages
4.01/4.17/4.24, CPU PSI `some` 5.58% over 10 seconds and 8.55% over five
minutes. The existing VPS2 Dux process and ClickHouse were also active. This
confirms the test ran on a shared, contended host; the co-occurring load cannot
be assigned as the cause of earlier latency outliers because per-check host
pressure was not recorded. The run uses only a small seeded fixture, so these
measurements do not establish production-scale scan latency or overhead.

Evidence: [progress samples](soak-progress-20261110T1019Z.json) and
[host-pressure sample](host-pressure-20261110T1019Z.json).

## Workload and pass criteria

- The mutation pacer targets 25 filesystem mutations per second over 259,200
  seconds. This is not a guaranteed wall-clock average: each exactness gate
  synchronously pauses mutations while a full scan, accounting verification,
  and independent comparison run. The pacer rebases after that pause instead
  of issuing a catch-up burst. At the 2026-11-10 22:05 checkpoint, the run-wide
  average was 22.402 mutations/second. The soak can support correctness and
  endurance at the observed rate; it does not prove a sustained 25/s workload
  or throughput capacity. A continuous concurrent producer is needed for that
  separate performance gate.

At the 22:11 checkpoint, 1,054 exactness samples had accumulated 3,478.5
seconds of measured convergence time (mean 3.300 s, median 2.671 s, p95 7.591
s, maximum 17.393 s). This represents 7.37% of elapsed soak time and excludes
other work around those synchronous gates, such as history assertions. The
recorded timings reinforce that the current soak's wall-clock mutation rate is
reconciliation-limited; they are not a standalone scanner benchmark.
- Every 1,000 mutations, request a full scan-first observation via SIGHUP, wait for scan completion and accounting verification, then independently compare indexed paths and identities against the fixture. Thus this is repeated reconciliation-under-churn endurance testing, not a passive event-only soak; the 256-file seed and small fixture also do not establish production-scale throughput.
- Inject a 500-file mutation burst each hour and verify exactness/history.
- Hold a read-only SQLite snapshot for five minutes after six hours; then
  verify the snapshot stays stable and WAL truncation reclaims space.
- After twelve hours, kill the test daemon, mutate 64 files while it is down,
  restart it, and require exact recovery from the prior generation.
- At completion, require a final exact comparison, history continuity, and
  SQLite `integrity_check` success.

The result qualifies only this exact binary, ext4 fixture, kernel, and 72-hour
test profile. It does not qualify a 24-hour production canary, full 1.9M-node
production-scale throughput, other filesystems/kernels, or storage power-loss
recovery. Final evidence will be copied into this directory after the worker
finishes; until then, the live report and rolling progress file are on VPS2.

### Refreshed progress — 2026-11-10 13:24 UTC

The VPS2 unit was active with zero restarts. The durable progress file records
15,561.5 seconds elapsed, 361,448 mutations, 360 exact-check samples, and four
mutation bursts. The latest exact sample covered operation 361,000 and measured
1,987 ms end-to-end convergence, 4,328 KiB daemon RSS, 14 file descriptors,
and a 4,482,592-byte WAL. No held-reader or controlled-crash phase has run;
final exactness, history continuity, and SQLite integrity remain pending. This
is still progress, not a soak pass. See
[`soak-progress-20261110T1324Z.json`](soak-progress-20261110T1324Z.json).


### Exactness cadence and current checkpoint — 2026-11-10 20:22 UTC

Source inspection of `Daemon.exact` and `request_scan_first_observation`
confirmed that each 1,000-operation exactness sample requests a full scan-first
observation by SIGHUP and waits for a newer completed generation and accounting
verification before checking the tree against the index. The soak log's repeated
“operator requested a full reconciliation” warnings are therefore expected from
its exactness protocol. This measures repeated reconciliation under concurrent
mutations; it does not measure 72 hours of event-only indexing. The 20:22
checkpoint records 40,642.4 seconds, 918,550 operations, 915 exactness samples,
11 hourly bursts, one successful held-reader/WAL-reclaim scenario, and zero
controlled crashes. The scheduled crash-recovery phase begins at 12 hours and
remains pending, as do final exactness, history, and SQLite integrity checks.
The measured average operation rate is about 22.6/s against the requested 25/s.
See [`soak-progress-20261110T2022Z.json`](soak-progress-20261110T2022Z.json).


## 12-hour controlled restart checkpoint — 2026-11-10 21:05 UTC

The live r132 candidate was stopped and relaunched by the soak harness after
12 hours. The checkpoint records candidate PID `1305044`, replacement PID
`3117886`, and 64 files written while the daemon was down. The harness sequence
performs a full exactness check against the pre-crash generation and checks
history continuity before it appends the crash record, so the recorded entry
means both assertions returned successfully. The outer systemd soak unit stayed
active with zero supervisor restarts.

At this checkpoint the run had 43,224 seconds elapsed, 973,838 mutations, and
970 exactness samples. This is one successful controlled restart on the small
ext4 fixture; four more scheduled 12-hour checkpoints and the final exactness,
history, and SQLite integrity gates remain pending. Evidence:
[`soak-progress-20261110T2105Z.json`](soak-progress-20261110T2105Z.json).

### Live checkpoint — 2026-11-10 22:38 UTC

A fresh read-only query of VPS2 at 22:38:11 found the exact-hash candidate and
its worker active; the outer systemd unit had zero restarts. The durable report
records **48,847.6 seconds** (13h34m08s; **18.85%** of 72 hours), **1,091,827
mutations**, **1,088 exactness samples**, 13 hourly bursts, two successful
held-reader/WAL-reclaim scenarios, and one controlled crash/recovery. The
whole-run average is **22.352 mutations/s**, below the requested 25/s because
synchronous exactness gates pause the producer. Across the recorded samples,
end-to-end convergence mean is **3.385 s**, median **2.767 s**, p95 **7.738 s**,
and max **17.393 s**. Sampled candidate RSS peaked at **6,568 KiB** and WAL at
**7,519,032 bytes**. Final exactness/history and SQLite integrity are still
pending. This is continued progress, not a passed soak. Snapshot:
[`soak-progress-20261110T2238Z.json`](soak-progress-20261110T2238Z.json),
SHA-256 `22d006fae545c22abcec1d4a92acc167a210813a2a8b6cd41873a4662332f36c`.

### Live checkpoint — 2026-11-10 22:44 UTC

The 22:44 VPS2 read-only query found the worker active with zero service
restarts. The report records **49,207.9 seconds** (13h40m08s; **18.99%**),
**1,098,981 mutations**, **1,095 exactness samples**, 13 bursts, two
held-reader/WAL-reclaim scenarios, and one controlled crash/recovery. The
whole-run average is **22.333 mutations/s**. Sampled exact convergence latency
is **3.409 s mean, 2.783 s median, 7.839 s p95, and 17.393 s maximum**.
Terminal exactness, history, and SQLite integrity remain pending. This is a
running checkpoint, not a passed soak. Evidence:
[`soak-progress-20261110T2244Z.json`](soak-progress-20261110T2244Z.json),
SHA-256 `6654a73a881f5a121a74deeb3d3bbfb6792063c21e94ca479b56bf9308c535aa`.
