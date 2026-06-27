# Performance Changes

This note records the targeted performance changes made after reviewing the
0.4.x code paths and the production symptoms from a large Maildir/Zimbra host.

## Changed Files

- `src/query.rs`
  - Made `dux status` avoid `SELECT COUNT(*) FROM inodes` on the normal indexed
    root path.
  - `status` now reads `recursive_inodes` and `recursive_bytes` from the root
    inode row, making the indexed count/size lookup O(1) instead of scanning
    millions of rows.
  - Clarified the filesystem capacity label to `filesystem: root mount ...`,
    because `statvfs("/")` reports only the root mount while the index may cover
    multiple mounted filesystems under `/`.

- `src/tui.rs`
  - Reduced idle TUI redraw overhead.
  - The event loop now redraws only when state changes, or once per second for
    time-based labels, instead of repainting every 120ms while idle.
  - This lowers idle CPU cost for `dux /` while keeping input and background
    refresh results responsive.

- `src/main.rs`
  - Reduced SQLite open churn while waiting for a daemon-triggered rescan.
  - The rescan polling loop now reuses one read connection and reopens it only
    periodically so it can observe atomic DB replacement.
  - This avoids reopening and schema-checking SQLite twice per second during a
    long daemon rescan.

## Verified

- `cargo test` passed: 12 tests.
- `cargo build --release` passed.

## Remaining High-Impact Work

- The daemon still resolves every fanotify event into a full path before
  coalescing. On very high-churn trees, this can be the dominant overhead and
  can contribute to `DIRTY` state from missed events.
- The next major daemon optimization should key pending events by parent
  directory identity plus raw filename, then use fd-relative operations such as
  `openat`/`fstatat` during flush. That avoids full path construction on the
  event hot path.
- Full scans still buffer node metadata in memory before loading SQLite. Scaling
  to tens of millions of files with strict memory bounds needs a streaming scan
  loader or external-sort style bottom-up aggregation.

## Disk Footprint Notes

- Source tree size is small (`src` about 252 KiB).
- Release binary is about 4.0 MiB.
- Debian package is about 1.7 MiB.
- The large local checkout size is Cargo build output under `target/`, not the
  shipped package.
