# VPS2 canary resource follow-up — 2026-11-09 19:55 UTC

At 19:53:01 UTC, the read-only inspection found **70 formal five-minute r121
canary samples, all passed**, with no sample errors. The canary unit remained
active. The completed-scan age across the samples ranged from 4 to 936 seconds
(median 476 seconds), against the 1,200-second limit.

Resource values are taken from the service cgroup fields in those monitor
samples, not from a synthetic benchmark. Sampled current memory ranged from
35.1 to 1,827.4 MiB (median 345.6 MiB); the maximum recorded cgroup memory peak
was 2,048.8 MiB. Cumulative CPU use at the latest sample was 2.407 core-hours
since service start. Between-sample CPU utilization had a median of 0.479 and
maximum of 0.957 of one CPU core. This indicates meaningful CPU use during
scans; it does not establish acceptable constrained-host p95/p99 latency or
r123 behavior.

At 19:55:17 UTC, the local host load average was 32.27 and CPU PSI `some
avg10` was 28.06%. The guarded lifecycle driver remained alive and no QEMU
process was running. Its admission gate must remain closed at this pressure;
the r123 package lifecycle has still not started.

Raw monitor aggregation and host/driver snapshot are in
[`resource-followup-20261109T1955Z.json`](resource-followup-20261109T1955Z.json).
