# VPS1 production status follow-up — 2026-11-09 22:30 UTC

This is a read-only snapshot of the production host. No daemon, database, or
configuration was changed.

- Installed daemon: **0.6.3**, source `e0cc915f8f37`; systemd active, PID
  1073891, four lifetime restarts.
- Index state: dirty from `fanotify queue overflow`, not caught up, no scan in
  progress. Last completed full scan was **380,478 seconds old** (about 4.4
  days); reported `behind_seconds` was 190,788 seconds. The index does not
  currently claim exactness.
- Event-loss counters: **649** kernel overflow notifications and **2,518,862**
  dropped updates. The overflow count was 636 at the earlier 18:19 checkpoint;
  dropped updates remain at the previously recorded count. Event counters are
  cumulative, so the 13-notification increase confirms continued overflow but
  does not identify its cause.
- Storage: database **3,332,632,576 bytes** with a **134,217,728-byte WAL**;
  indexed visible size about **124.9 GB** across 1,323,464 nodes. Root has
  **61.7 GB available** (68% used).
- Service memory was about 856 MB current / 1.43 GB peak. No write error or
  scan-in-progress state was reported.

This directly confirms that VPS1 is still on the old binary and its index is
severely stale. It is not evidence that r123 is ready for deployment; the
strict package lifecycle and r123's own 24-hour production-like soak remain
unqualified. Raw command output is in [`vps1-status.json`](vps1-status.json),
SHA-256 `967b9bc34a0569934bda922a666261bf2eae3a240512db6073d7811ed4e8a057`.
