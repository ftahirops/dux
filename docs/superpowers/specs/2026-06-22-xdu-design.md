# xdu — Persistent Realtime Disk Usage + File Search

**Date:** 2026-06-22
**Status:** Approved, building

## One-liner
Persistent, realtime disk-usage and file-search index. Replaces `du`, `ncdu`,
`locate`, and `find` with one always-fresh tool. Companion to `xtop`.

## Pitch
`locate` speed, `find` freshness, `du` intelligence — in one tool that can run
as a systemd daemon and stay live via fanotify.

## Stack
Rust static binary. `clap`, `ratatui`+`crossterm`, `rusqlite` (WAL),
`nix`/`libc` (statx, fanotify, getdents64), `rayon`, `tracing`.

## Architecture — 2 components, 1 DB
```
xdu CLI / TUI  ──reads──►  ONE SQLite WAL  ◄──writes──  xdu --daemon (scan + fanotify)
                              + in-mem hot cache
```
No socket, no second DB, no eBPF. CLI reads the DB directly (WAL concurrent reads).

## Storage — one file, two tables
- `nodes(dev_id, inode, parent_inode, name, type, size, blocks, recursive_bytes,
  uid, gid, mode, mtime, last_seen, deleted)` — store `name` only; reconstruct
  paths on query (rename = 1 row update, not millions).
- `changes(ts, dev_id, inode, size_before, size_after, delta, event_type)` —
  rolling, default 24h retention. Growth derived from this.
- Indexes: `size DESC`, `mtime DESC`, `parent_inode`, `uid`, and a name index
  (FTS/trigram) for search.
- In-memory: hot directory tree + recent-changes ring, flushed in batches.

## Three load-bearing performance rules
1. **fanotify mount mode** (not inotify-per-dir). Kernel >= 5.9 for dirent
   events (have 6.8). Filter by `dev_id` for `--one-file-system`.
2. **Batched/coalesced ancestor updates** — never per-event DB writes;
   `HashMap<inode, delta>` flushed every N ms in one transaction. Make-or-break.
3. **Notification mode only** — never block the FS; bounded queue, drop-to-
   rescan-dirty on overflow.

## MVP commands (both cores: du + search)
| Command | Purpose |
|---|---|
| `xdu scan PATH` | full threaded scan into index |
| `xdu top PATH --dirs/--files` | largest dirs/files, instant |
| `xdu find PATH --name/--newer/--larger/--owner/--ext` | ultra-fast realtime search |
| `xdu growth PATH --since 1h` | fastest-growing paths |
| `xdu deleted-open` | `/proc/*/fd` deleted-but-open scan |
| `xdu tui PATH` | ncdu-style browser |
| `xdu --daemon` | systemd watcher |

Plus `xdu.service` (`Type=notify`, `Nice=10`, `IOSchedulingClass=idle`,
`MemoryMax=512M`).

## Correctness / trust
Always surface the last-scan timestamp. On event-overflow or crash, mark the
subtree dirty and reconcile by rescan rather than serving wrong numbers. A
confidently-wrong tool is worse than a slow one.

## Out of scope (YAGNI)
LMDB, eBPF, IPC socket, web UI, distributed mode, resume-checkpoint, container
attribution.

## Build order
1. Project scaffold + `IndexStore` (SQLite schema, open/migrate).
2. Scanner (`scan`) — threaded walk via getdents64/statx, bottom-up totals.
3. Query (`top`, `find`, `growth`, `deleted-open`) + CLI.
4. TUI (`tui`).
5. Daemon (`--daemon`) — fanotify watcher with batched ancestor updates.
6. systemd unit + packaging.
