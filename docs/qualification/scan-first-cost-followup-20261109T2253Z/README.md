# Second scan-first cost sample — VPS2 r121, 2026-11-09 22:53 UTC

This confirms the same scan/accounting pattern in a second cycle. Dux started
the full observation at 22:44:32 UTC and completed it at 22:48:43.875 in
**251.8 seconds**. It walked 1,617,443 files and 192,444 directories, reported
zero coverage issues, and accounted for 80.49 GiB visible size. Independent
accounting passed at 22:49:57.807 in **73.903 seconds**. Scan plus verification
took about 325.7 seconds, or **36.2%** of the configured 900-second cadence.

Canary sample **106** at 22:52:58 passed with no errors. Scan age was 255
seconds; the scan was no longer in progress. The daemon was active with zero
restarts, overflows, dropped events, pending updates, or OOM events.

The sample-105-to-106 cgroup interval was 299.99 seconds: Dux used 148.38 CPU
seconds (**49.5% of one core**), read 403,599,360 bytes, wrote 1,096,953,856
bytes, and observed 6,981,938 events (23.3k/s). This interval contains scan
completion and accounting; it is not an isolated scan benchmark. Together with
the prior sample, two cycles show scan+accounting around 36% of cadence, but
CPU and I/O vary by cycle. Keep profiling each stage before changing cadence
or CPU controls.

Raw monitor evidence: [`sample-106.json`](sample-106.json), SHA-256
`aa936b9fc55525c1435516197f8114935c7f5911536a14a310b5bfc75710cf7e`.
Matching runtime lines:
[`scan-cycle-journal.txt`](scan-cycle-journal.txt), SHA-256
`79d78cfcd4aaa42bb5b49d7ec6054b462e93275ef0ba32b86a118ab471a9c0c3`.
