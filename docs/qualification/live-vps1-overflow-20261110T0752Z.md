# Live VPS1 overflow snapshot — 2026-11-10 07:52 UTC

This is a read-only observation of the installed production daemon during
active host build/test churn. It is evidence about installed package r66, not
the r132 candidate.

## Installed daemon and index state

- Installed package: `dux 0.6.3-66`; executable revision `e0cc915f8f37`, source
  digest `b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`.
- Service was active with historical `NRestarts=4`; this sample did not change
  the service or its database.
- Index status was `dirty=true`, `caught_up=false`, reason `fanotify queue
  overflow`. Current status counters reported **721 kernel overflow
  notifications**, **2,518,862 dropped updates**, 212,913,050 events seen, and
  122,734,646 resolved. These are counters from the daemon status record; this
  snapshot does not establish their exact accumulation interval.
- The last full scan was at `2026-11-05T12:49:22Z`, **414,201 seconds** before
  capture (about 4 days 19 hours). `behind_seconds` was 224,512 (about 2 days
  14 hours). The daemon was not scanning at capture.
- Five minutes of journald output contained **109 stale-file-handle warnings**
  from `open_by_handle_at` and **29 slow-flush phase warnings**. This shows
  active path-resolution and database-flush pressure in the old event-apply
  implementation; it is not a controlled benchmark.
- `index_bytes` was 126,389,637,120 bytes (logical indexed file sizes), while
  the SQLite database was 3,379,904,512 bytes on disk and its WAL was 128 MiB.
  These are separate measures; `index_bytes` is not disk space consumed by the
  database.
- Root filesystem: 185 GiB total, about 120 GiB used, 57 GiB available. Disk
  exhaustion was not the cause of this dirty state.
- Host load average was 20.19; CPU and I/O pressure were elevated. A separate
  Go build/test workload was active. This is observational stress evidence,
  not an isolated Dux-only load result.

## What this proves, and what it does not

The installed r66 service is not maintaining a fresh, trustworthy index on
this host under the observed workload. It remains alive, but the fanotify
overflow marker correctly prevents it from claiming exactness. The snapshot
supports investigating the difference between old per-event path resolution
and the r132 scan-first candidate, but does not prove r132 will sustain the
production tree or avoid future queue loss.

The r132 candidate was not installed on VPS1. It passed the focused scan-first
race test and the Ubuntu 24.04 DEB lifecycle, but has not passed a 1.3M-node
production-shaped churn/throughput test, an extended canary, or the required
soak. The installed daemon's multi-day stale state remains a release blocker.

Raw machine-readable counters, service properties, host pressure, disk
accounting, and a five-minute log summary are in
[`live-vps1-overflow-20261110T0752Z.json`](live-vps1-overflow-20261110T0752Z.json).
