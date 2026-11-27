# VPS2 observation and disk impact — 2026-11-10

## Completed scan-first reconciliation

The installed r121 daemon deferred a full reconciliation at 02:16:41 UTC when
host pressure was elevated (CPU PSI avg10 18.52%, available disk 6.27 GB),
then began an in-place observation at 02:16:46 after admission recovered. The
scan completed at 02:24:10, taking about 7 minutes 24 seconds. Its journal
reported:

- 1,640,606 files and 194,545 directories;
- 1,835,151 staged path instances;
- 81.70 GiB visible allocation and 81.53 GiB unique allocation;
- 0 coverage issues and 0 unresolved inode-reuse paths;
- 88 vanished paths discarded after inode-reuse checks.

The `dux.db.scan-...tmp` staging database reached 690,892,800 bytes (about
659 MiB) during the run. The 727 MiB main database and 128 MiB WAL remained in
place. At completion the scan-stage file was gone and root free space recovered
from 4.7 to 5.8 GiB. This confirms cleanup on successful completion; the
staging file is an expected temporary artifact of in-place observation, not an
orphan. The dirty status after the scan was the normal “filesystem changes
await the next full observation” hint; no writes were pending and no
coverage/drop error was reported.

At 02:25:27 UTC the service was active with zero lifetime restarts, no scan in
progress, zero pending updates, zero kernel overflows/dropped events, and no
write error. Root storage was 94% used with 5.8 GiB available. Status and host
evidence are in
[`vps2-status-after-observation.json`](vps2-status-after-observation.json) and
[`host-observation.txt`](host-observation.txt).
The daemon's reconciliation accounting verification passed at 02:25:25 in
75.079 seconds. This is the daemon's internal accounting check; an independent
filesystem-walk comparison is still required for release qualification.

## Missing `CAP_DAC_READ_SEARCH` event-resolution smoke

The r126 candidate (commit `2a884a8`, SHA-256
`9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b`) passed
two runs on VPS2, Ubuntu 24.04.3, kernel `6.8.0-147-generic`. Each run used a
private mount namespace and a 128 MiB ext4 loop filesystem backed by tmpfs;
the daemon retained `CAP_SYS_ADMIN` but had `CAP_DAC_READ_SEARCH` removed.

Both runs started and marked the filesystem, then received `EPERM` from
`open_by_handle_at`. Dux logged its capability warning and marked the index
dirty instead of claiming successful live updates. The two final status
samples showed 90/85 events seen, 0 resolved, 0 dropped, and 0 overflows. The
script verified the warning after 69 unresolved events. Test filesystems,
databases, loop devices, and temporary binary/script staging were removed
automatically. After each run the installed r121 service remained active with
zero restarts, no overflow/drop/write error, and 5.8 GiB free.

Reproduce with
[`capability-handle-resolution-smoke.sh`](../release-candidate-r126-20261110/capability-handle-resolution-smoke.sh);
captured output from the repeated run is
[`capability-handle-resolution-output.txt`](../release-candidate-r126-20261110/capability-handle-resolution-output.txt).
This exercises one Linux kernel and ext4 configuration; it does not replace
package lifecycle qualification, a sustained canary, or testing other
filesystems and capability combinations.
