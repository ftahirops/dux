# VPS2 live reconciliation observation — 2026-11-10

This is a read-only observation of the **installed r121 daemon**, not a test of
the r131 candidate. It extends the live retry observation recorded during the
r131 qualification work. No service, database, or host configuration was
changed.

## Retry and verification outcome

The first scan ended at 05:48:09 UTC with one coverage issue after observing
conflicting inode incarnations. The host-pressure gate deferred its bounded
retry, which began at 05:49:09. That retry completed at 05:58:50: 1,681,973
files, 199,525 directories, 1,881,498 staged path instances, zero coverage
issues, zero unresolved reuse paths, and zero pending paths to replay. The
retry therefore took about 9 minutes 41 seconds. It is a successful live
reconciliation by r121 under load, after one incomplete attempt; it is not a
candidate r131 qualification.

The daemon then kept the status dirty while its separate accounting verifier
ran. At 06:00:42 the journal recorded `reconciliation accounting verification
passed` after 111,528 ms. The next scheduled full observation began at
06:04:09. At 06:11:01 it was in the indexing phase, with a fresh progress
heartbeat and 1,682,574 files / 199,794 directories observed. Its staging DB
was 707,727,360 bytes at 06:10:28 and unchanged in the 06:11:01 sample. It
completed at 06:11:26 with zero coverage issues and zero paths to replay, in
about 7m17s. The accounting check passed at 06:12:25 after 58,863 ms. At
06:14:01 the daemon remained active with zero restarts, dropped events, or
kernel-overflow notices and 4.1 GiB free on root; status was dirty because
filesystem changes had arrived after the completed observation. An unchanged
staging file alone did not show a stall: the heartbeat remained fresh during
projection.

## Observed state during retry finalization

From 05:49 through 05:57 UTC, the root daemon remained in
`ActiveState=reloading` with `NRestarts=0`. The scan progress record's counters
settled at `1794289749 1681973 199525 89621118976 1` (1,681,973 files,
199,525 directories, indexing flag set) after the tree walk. The record's
first field is the scan start time, not a heartbeat. A later `stat` showed the
progress file mtime refreshing at 05:57:29, confirming the progress publisher
was still alive. The retry had logged walk progress through 1,674,000 files
around 05:52, but had not logged completion by 05:57.

The temporary replacement database reached 710,303,744 bytes at 05:55:22 and
remained that size through 05:56:54. This is not proof of a stall: source
inspection shows the in-place path first walks into a staging database, then
sets the progress `indexing` flag and merges the projection into the live
database. During the observation the daemon used about 64% of one CPU core;
at 05:57:30 its cumulative process I/O was approximately 197 GB
read-character, 28.5 GB written-character, 19.0 GB physical reads, and 20.4
GB physical writes. These are process-lifetime counters, not attributable
solely to this retry. The process remained runnable. The open deleted
`/var/tmp` file descriptors observed during one sample may be SQLite scratch
files; their purpose and size were not established.

Fanotify metrics in the journal showed queue bytes generally near zero (a
brief sample reached 81,528 bytes), zero queue-pressure episodes, and zero
kernel-overflow notices. The host reported 3.5 GiB free on its 93 GiB root
filesystem (97% used) and material CPU and I/O pressure. Systemd continued to
report zero restarts.

## Interpretation and limits

At 05:57 the retry was active, its progress publisher was refreshing, and the
walk counters had entered the indexing phase. This was a long database
projection/finalization phase under host pressure, not a deadlock. It also
shows that zero fanotify overflows alone does not guarantee prompt
reconciliation. The daemon was r121, so this observation neither validates nor
falsifies r131 behavior.

A sample at 05:56:54 UTC still showed the same scan counters and temporary DB
size (710,303,744 bytes), with `ActiveState=reloading` and `NRestarts=0`. At
05:57:29, the progress file mtime was fresh. Queue pressure remained zero.
Root free space had fallen to 3.2 GiB, so disk headroom remains a concern while
the projection runs.

Both observed full scans and their accounting verifications have completed:
the first needed a bounded retry, while the next scheduled scan completed on
its first attempt. The filesystem-to-index view is still marked stale after
later filesystem changes, which the next scheduled observation must absorb.
Independent filesystem-to-index equality has not yet been measured. Do not
remove live scan files or restart the service to accelerate it; that would
interrupt the installed indexer and invalidate this observation.

## Evidence collection

Read-only commands used explicit root SSH on port 22 (bypassing the local SSH
config that maps this host to Forgejo's `git` account on port 2222):

- `systemctl show dux -p MainPID -p ActiveState -p NRestarts`
- `cat /run/dux/scan.progress`
- `stat` on the daemon's `dux.db.scan-*.tmp` file
- `/proc/<pid>/io`, `ps`, process file descriptors, and filesystem free space
- `journalctl -u dux` filtered to the observation window

Captured key readings during the retry: `MainPID=576850`;
`ActiveState=reloading`; `NRestarts=0`; root filesystem 93 GiB total, 3.5 GiB
available at the first sample; `/dev/shm` 3.0 GiB total, 2.9 GiB available.
The later 06:05:45 sample had 4.0 GiB root free. The service/database were not
modified.
