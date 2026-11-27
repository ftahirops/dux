# VPS2 r121 canary and scan-cycle follow-up — 2026-11-09 22:33 UTC

Canary sample **102** passed at 22:33:12 UTC with no errors. It was captured
while a scan-first observation was active; systemd reported `reloading`, zero
restarts, and 1.36 GiB current / 2.00 GiB peak memory. The monitor explicitly
accepts `reloading` only when a scan-first observation is active. At that
sample, scan age was 711 seconds, below the 1,200-second ceiling; SQLite and
schema-v7 accounting checks passed, with no kernel overflow or dropped events.

The same observation completed at 22:34:07 UTC:

- 1,613,677 files and 192,125 directories; 1,805,802 path instances.
- Zero coverage issues; 80.45 GiB visible bytes.
- Scan duration was about **275 seconds** from its recorded start.
- Independent accounting verification passed at 22:34:58 UTC in **51.258
  seconds**.
- A fresh read-only status after completion showed no scan in progress, age 144
  seconds, zero overflows, drops, or pending updates. Its interim dirty reason
  was the expected `filesystem changes await the next full observation`.

`memory.events.high` remained at 1,173 from sample 101 through sample 102; OOM
counters remained zero. This is positive evidence for the r121 observation
cycle on VPS2, not a candidate-r123 qualification or production sign-off. The
24-hour soak remains incomplete. No service or database state was changed.

Raw evidence: [`sample.json`](sample.json),
[`status_after_observation.json`](status_after_observation.json), and
[`scan-accounting-journal.txt`](scan-accounting-journal.txt).
