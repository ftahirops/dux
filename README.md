# dux — persistent, realtime disk usage + file search

A persistent, indexed `du`/`ncdu` with fast name search and an optional live
fanotify daemon. Companion to `xtop` (same stack, same daemon model).

> Name note: the old X11 `xdu` exists in Debian/Ubuntu; this project is `dux`.

## What works today

| Capability | Status |
|---|---|
| Persistent index, instant re-query (`top`, `tui`) | ✅ |
| Parallel scan with live progress | ✅ |
| Path-scoped queries (`top PATH`, `find PATH`, `growth PATH`, `dux PATH`) | ✅ |
| Trigram name search (`find --name/--ext`), faster than `locate` on globs | ✅ |
| Size/time/owner filters (`--larger`, `--newer`, `--uid`) | ✅ |
| Pseudo-fs skipped by default (no fake `/proc/kcore`) | ✅ |
| WinDirStat-style expanding tree TUI with heat colors | ✅ |
| **Inode-usage mode** (rank dirs by file count, not size) — `top --inodes`, TUI `i` | ✅ |
| Live daemon: file **create** + **size-growth** tracked via fanotify | ✅ |
| Growth alerts (`--alert-threshold … --alert-exec …`) | ✅ |
| `deleted-open`, `by-owner`, `by-ext` | ✅ |

## Known limitations (honest)

- **Daemon tracks create, delete, rename/move, dir-creation, and size-growth
  live** (fanotify FID mode, `open_by_handle_at`). The remaining gap is the
  **downtime window** — changes made while the daemon isn't running are missed
  until the next `dux scan`. `status`/TUI show **live** (heartbeat) vs **snapshot**.
- On a very busy whole-`/` watcher, the daemon can briefly **lag** behind bursts
  of system-wide activity (events queue up; `FAN_UNLIMITED_QUEUE` prevents drops),
  so live updates may take a few seconds to appear under heavy load.
- **One tree per index.** Each `scan` resets the index (single-root model).
- **Hardlinks: disk totals are correct** (shared inode counted once, matching
  `du`/`df`), but a hardlinked file is searchable under **one name only** —
  full multi-path modelling needs a separate paths table (roadmap).
- **Disk usage = allocated blocks everywhere** (matches `du`; sparse files
  report their real on-disk footprint, not apparent size).
- Cross-mount scans store `parent_dev` so path reconstruction & scoping work
  across mount points.
- Networked FS (NFS/Ceph): scan works; live watch is limited.
- fanotify daemon needs root (CAP_SYS_ADMIN) + kernel ≥ 5.9, and must **not**
  run in a private mount namespace (uses `FAN_MARK_FILESYSTEM`).
- Scanner buffers all nodes in memory during a scan (~hundreds of MB for a few
  million files); very large filesystems may need a higher `MemoryMax`.

See `docs/architecture-analysis-and-roadmap.md` for the full roadmap.

## Build & install

```bash
cargo build --release
sudo install -m755 target/release/dux /usr/local/bin/dux
```

## Use

```bash
dux scan /                       # build the index (parallel, shows progress)
dux                              # tree TUI at root
dux /var/log                     # tree TUI scoped to /var/log
dux top /var --dirs              # largest dirs under /var
dux top --inodes                 # dirs with the MOST files/inodes (du/ncdu can't)
dux find /home --name '*.log' --larger 100M
dux find --newer 10m             # what changed in the last 10 min (global)
dux growth /data --since 1h
dux deleted-open                 # deleted-but-open files wasting disk
dux by-owner ; dux by-ext
dux status                       # index freshness
```

### TUI keys
`↑↓`/`jk` move · `→`/`⏎` expand · `←` collapse / up · **`i` toggle size⇄inodes** ·
`r` refresh · `q` quit.
Folders expand **inline** beneath their parent (the parent stays visible).
Bars/colors are a heatmap: 🔴 red = dominant within its level → 🟢 green = small.
Press `i` to switch the whole graph between **disk size** and **inode/file count** —
the same heat bars then show which directories hold the most *files* (e.g. a Go
module cache or `node_modules`), which size-based tools never reveal.

## Realtime daemon (systemd)

```bash
sudo cp packaging/dux.service /etc/systemd/system/
sudo systemctl enable --now dux
```

The daemon uses fanotify mount mode and **coalesces size deltas in memory**,
flushing batched ancestor-total updates periodically — never a DB write per
event. Overhead is ~0% idle, low single-digit % of one core under write load.

Alerts:
```bash
dux daemon / --alert-threshold 1G --alert-window 10m --alert-exec /path/hook.sh
# hook env: DUX_PATH, DUX_DELTA, DUX_DELTA_HUMAN, DUX_WINDOW
```

## Architecture

Two components, one SQLite WAL file:
```
dux CLI / TUI  ──reads──►  SQLite WAL  ◄──writes──  dux daemon (scan + fanotify)
```
No socket, no second DB, no eBPF.
