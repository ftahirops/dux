# VPS2 r121 canary follow-up — 2026-11-09 22:28 UTC

Read-only monitor sample **101** passed at 22:28:38 UTC with no errors.
SQLite quick-check, schema-v7 path graph, and root/inode accounting passed.
The completed observation age was 437 seconds (monitor limit: 1,200 seconds).
Systemd restart count, kernel overflow notifications, kernel/user event drops,
and pending updates were all zero. Scan-first mode reported its expected
interim dirty reason, `filesystem changes await the next full observation`.

Compared with sample 100 at 22:23:17 UTC, cumulative cgroup
`memory.events.high` remained **1,173**; `MemoryCurrent` was about 310 MB,
`MemoryPeak` remained about 2.00 GiB, and OOM counters remained zero. This
five-minute comparison shows no additional high-threshold events during the
interval. It does not erase the earlier high events or establish a full-soak
memory profile.

This is r121, and the required 24-hour soak remains incomplete. The raw sample
is [`vps2-canary-sample-101.json`](vps2-canary-sample-101.json), SHA-256
`a8f99a6f24f0aba2a12d81e6ab573b09d08fd94917b70e3c45d649667594058f`.
No service, database, or host configuration was changed.
