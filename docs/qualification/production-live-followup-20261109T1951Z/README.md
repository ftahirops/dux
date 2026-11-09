# Production qualification follow-up — 2026-11-09 19:51 UTC

This is a read-only follow-up to the earlier 19:44 checkpoint. VPS2 was
queried over SSH on port 22. The raw summary is
[`vps2-canary-20261109T1951Z.json`](vps2-canary-20261109T1951Z.json).

## VPS2 r121 canary

The formal five-minute monitor is still active. Its output contains **69
samples, all passed**, from 14:07:48 through 19:48:43 UTC, with no sample
errors. Completed-observation ages ranged from 4 to 936 seconds against the
1,200-second limit. About 5h44m of the required 24-hour interval has elapsed;
the soak is incomplete.

At 19:51:18 UTC, `dux.service` was active under PID 2561224 with zero restarts,
kernel overflows, kernel-reported event drops, userspace drops, and pending
updates. Its last completed observation was 125 seconds old. The 19:44:31
observation completed at 19:49:13, walking 1,584,643 files and 188,619
directories with zero coverage issues. Independent accounting passed at
19:50:34 in 80.226 seconds.

The status reports `dirty` because changes await the next full observation;
this is the scan-first mode's expected interim state. `behind_seconds` is
21,084.72 seconds and `caught_up` is false, but neither is the canary freshness
gate. The monitor explicitly checks completed-observation age. Event counters
show 265,822,298 seen and zero resolved/committed; the scan-first design uses
events as observation hints rather than applying per-file event updates.

## r123 package lifecycle

The local guarded VM lifecycle driver (PID 1928589, exec session 2482) remained
active but waiting. At 19:50:28, host load was 14.85 and CPU PSI `some avg10`
was 33.83%; Go compiler/test workloads were active. No QEMU VM was running and
the r123 package lifecycle had not started. Do not interpret this admission
wait as either a package pass or a package failure. The runner remains subject
to its host admission and runtime pressure guards.

No production daemon, database, or configuration was changed. No package was
installed on VPS2 in this follow-up.
