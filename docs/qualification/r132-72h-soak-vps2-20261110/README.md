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

## Workload and pass criteria

- 25 filesystem mutations per second for 259,200 seconds.
- Independently compare indexed filesystem state every 1,000 mutations.
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
