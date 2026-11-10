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
