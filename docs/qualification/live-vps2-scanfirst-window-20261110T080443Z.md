# Live VPS2 scan-first observation — 2026-11-10 08:04 UTC

Read-only observation of the installed r121 scan-first daemon on
`192.168.5.106`, kernel 6.8.0-147-generic. It was active with zero restarts and
about 546 MiB memory. The database represented **1,905,848 nodes**; its on-disk
size was 796,520,448 bytes. `index_bytes` was 88,405,340,160 bytes of logical
indexed file sizes, not DB disk usage.

Two samples 30 seconds apart saw **922,605 more fanotify events** (about 30.8k
per second) and no increases in dropped events, userspace drops, or kernel
overflows. In scan-first mode the daemon does not resolve these event records
to paths; the counters are hints for freshness. The service remained active
with `NRestarts=0`.

The status was dirty with `filesystem changes await the next full observation`.
The last observation had completed about 8.6 minutes earlier, so this sample
does not show whether the next 15-minute observation starts and finishes on
cadence. `caught_up=false` is expected for the scan-first status model and does
not mean an event-apply backlog. It does mean this sample alone cannot establish
the maximum freshness lag or exactness after the next observation.

This is encouraging live evidence that scan-first mode can drain a very high
hint rate on a 1.9M-node index without reporting event loss during this short
window. It is r121, not the exact r132 binary; it is not a 24-hour canary,
large-scale synthetic churn test, or soak. The full before/after JSON is in
[`live-vps2-scan-first-window-20261110T080443Z.json`](live-vps2-scan-first-window-20261110T080443Z.json).
