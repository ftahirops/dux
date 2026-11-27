# VPS2 disk cleanup — 2026-11-10 21:50 UTC

## Result

Removed two inactive, reproducible tool caches from `/var/tmp`:

| Path | Before | Last modified | Process references before removal |
|---|---:|---|---|
| `/var/tmp/dux-cargo` | 1,005 MiB | 2026-11-08 23:16 UTC | none |
| `/var/tmp/dux-rustup` | 842 MiB | 2026-11-07 00:40 UTC | none |

These were Cargo registry/advisory caches and a Rust toolchain cache, not
source trees or test reports. The active soak used its prebuilt candidate and
isolated workspace; it remained active with zero supervisor restarts after the
cleanup.

Root usage changed from **90%** (about **9.0 GiB available**) to **89%**
(about **11 GiB available**). `/var/tmp` fell from **4.2 GiB** to **2.4 GiB**.
The system journal remained at 4.1 GiB (**4.0 GiB reported by
`journalctl --disk-usage`**). Its archived boot list spans 2026-08-17 through
2026-11-10. No explicit storage or retention override appeared in the
journald configuration. I left these logs intact because they may contain
incident evidence; reclaiming them should follow an agreed retention policy
and any required export of Dux incident logs. No repositories, qualification
evidence, or soak data were removed.

The r132 soak at 21:49 UTC was still running after 12h44m43s, with 1,029,704
mutations, 1,026 exactness samples, 12 mutation bursts, two held-reader/WAL
reclaim passes, and one controlled crash recovery. Terminal exactness, history,
and SQLite integrity assertions remain pending. See the
[`21:49 soak checkpoint`](r132-72h-soak-vps2-20261110/soak-progress-20261110T2149Z.json).

Reproduction commands:

```sh
df -h /
du -sh /var/tmp /var/log/journal
systemctl show dux-readiness-1304925.service -p ActiveState -p NRestarts
```
