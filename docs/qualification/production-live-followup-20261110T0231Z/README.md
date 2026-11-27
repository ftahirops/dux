# VPS2 scan retry and candidate capability follow-up — 2026-11-10

This report records two targeted follow-ups. VPS2 was running the installed
r121 service; the isolated capability test ran the r126 candidate binary. This
is evidence, not a release qualification or a 24-hour canary.

## VPS2 observation and bounded retry

The periodic root observation started at 02:31:46 UTC. It staged 1,836,472
path instances (1,641,594 files and 194,878 directories). During the walk,
360 vanished inode-reuse paths were discarded and two paths were revalidated.
One inode was observed with conflicting birth-time incarnations. Dux correctly
reported one coverage issue, left the result incomplete, and scheduled its one
bounded automatic retry. The first retry admission was deferred while host CPU
pressure was elevated (PSI CPU avg10 19.62%).

The retry started at 02:38:47 and completed at 02:45:04 UTC. It staged
1,837,130 path instances (1,642,084 files and 195,046 directories), reported
zero coverage issues and zero unresolved inode-reuse paths, and passed Dux's
accounting verification at 02:46:07 in 63.147 seconds. The
service remained active with zero restarts, kernel overflow notifications,
and dropped updates in the captured samples. This demonstrates recovery from
one observed inode-incarnation conflict; it does not prove arbitrary churn
always converges or independently validate every indexed path.

The staged scan database peaked at 693,379,072 bytes (about 661 MiB) in the
captured retry sample and was removed after successful publication. Root free
space recovered from about 4.6 GiB during staging to about 5.6 GiB afterwards.
Between cgroup samples at 02:37:53 and 02:45:02 (429 seconds), Dux accumulated
about 375.5 CPU-seconds, approximately 87.5% of one core on average. Device
counters increased by about 2.42 GiB read and 1.64 GiB written. The `8:0` and
`252:0` `io.stat` lines represent the same underlying device accounting and
must not be summed. These are short-window host measurements under concurrent
load, not a controlled benchmark or resource ceiling.

Evidence:

- [`host-and-scan-log.txt`](host-and-scan-log.txt)
- [`vps2-status-after-incomplete-scan.json`](vps2-status-after-incomplete-scan.json)
- [`cgroup-and-scan-start.txt`](cgroup-and-scan-start.txt)
- [`cgroup-during-retry.txt`](cgroup-during-retry.txt)

At 02:53:48 UTC a subsequent scheduled observation was in progress. VPS2 must
not be described as currently idle or fully caught up based on this report.

## r126 non-root capability smoke

The exact r126 static candidate (SHA-256
`9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b`) was run
as UID/GID 65534 (`nobody`) with only `CAP_SYS_ADMIN` and
`CAP_DAC_READ_SEARCH`, with `NoNewPrivs` enabled, in a private mount namespace
on an isolated 128 MiB ext4 loop filesystem backed by tmpfs. Fanotify setup,
baseline scan, accounting verification, and create/rename event resolution
passed; no drops or overflows were reported. The temporary mount and loop test
fixture were removed by the harness.

This shows that root UID is not intrinsically required for the tested ext4
fanotify and handle-resolution path. It does **not** qualify running the full
service under a dedicated account: full host tree permissions, package user
creation and ownership migration, StateDirectory behavior, upgrade/rollback,
and other filesystem/kernel combinations remain untested. Do not infer that
the package unit has been changed; it still runs as root with a restricted
capability set.

The captured run and reproducible script are
[`unprivileged-capability-output.txt`](../release-candidate-r126-20261110/unprivileged-capability-output.txt)
and
[`unprivileged-capability-smoke.sh`](../release-candidate-r126-20261110/unprivileged-capability-smoke.sh).

## Release decision

No production service or database was modified by the candidate smoke. The
r126 candidate remains uninstalled. Guarded package lifecycle qualification,
independent path/accounting comparison, broader churn/stale-handle coverage,
and a fresh uninterrupted 24-hour canary are still open. Production readiness
is not established.
