# dux — the disk usage tool that already knows the answer

**Stop running `du` and waiting. `dux` keeps a live index of your filesystem, so
"what's eating my disk?" is answered in milliseconds — with history, growth
rates, and realtime alerts the classic tools can't give you.**

```
dux            # instant tree, sorted by size, live-updating
dux top /var   # biggest dirs under /var — no rescan
dux find /home --name '*.log' --larger 1G --newer 1h
```

---

## The pain (every Linux admin / SRE / DevOps has lived this)

> **02:14 — PagerDuty: `/ at 96%`.**

You SSH in and the tools fight you:

- **`du -sh /*` takes minutes** and hammers the disk you're already trying to
  save — every single time, because it remembers nothing.
- **`ncdu /` rescans from scratch** on every launch. Run it twice, scan twice.
- **`df` says 96% full — but *where*?** It gives you a number, not a culprit.
- **No history.** Something grew 40 GB since yesterday and nothing can tell you
  *what* or *how fast* — by the time you look, the logs already rotated.
- **`locate` is stale** (cron `updatedb` runs once a day) and **`find /` is slow**
  and re-walks the whole tree for every query.
- **A process deleted a 90 GB file but still holds it open** — `df` shows the
  space gone, `du` can't see it, and you're grepping `lsof | grep deleted`.
- **You're out of inodes, not bytes** — millions of tiny files somewhere
  (a runaway cache, `node_modules`, a Go module dir) and *no tool ranks
  directories by file count*.
- **Which container/app is filling the disk right now?** You can't tell from a
  point-in-time snapshot.

Every one of these is "scan again and wait," done at the worst possible moment.

---

## The solution — why `dux` exists

`dux` indexes the filesystem **once**, then keeps that index **live** with a
fanotify daemon. Queries read the index, so they're instant — and because the
index is maintained in realtime, it can answer questions `du`/`ncdu`/`df`/`find`
structurally cannot.

| The pain | How `dux` fixes it |
|---|---|
| `du`/`ncdu` rescan every time | **Scan once, query in milliseconds** — persistent index |
| `df` says full but not *where* | **Drill into the biggest dirs/files instantly**, df-style capacity gauge built in |
| No idea what's growing | **`dux growth`** + live per-directory **write rates** and **ETA-to-full** |
| `locate` stale, `find` slow | **Trigram name search on a live index** — fresh *and* fast |
| Deleted-but-open space leaks | **`dux deleted-open`** — ranks the processes pinning freed space |
| Out of inodes, not bytes | **Inode-usage mode** — rank directories by *file count*, not size |
| Disk fills silently | **Growth alerts** — run a webhook/script when any path grows past a threshold |
| "Is it safe to delete?" guesswork | **WinDirStat-style live tree** — see the hot spots at a glance |

It's the tool you wish you'd had at 2 a.m.: **realtime, indexed, and it shows you
the culprit — not just the symptom.**

---

## Install

**Debian / Ubuntu** — download the `.deb` from the [latest release](https://github.com/ftahirops/dux/releases/latest):
```bash
curl -LO https://github.com/ftahirops/dux/releases/latest/download/dux_0.1.0_amd64.deb
sudo dpkg -i dux_0.1.0_amd64.deb
```

**RHEL / Fedora / openSUSE**:
```bash
curl -LO https://github.com/ftahirops/dux/releases/latest/download/dux-0.1.0-1.x86_64.rpm
sudo rpm -i dux-0.1.0-1.x86_64.rpm
```

Both packages install `/usr/bin/dux` and a systemd unit that builds the index on
first start and then runs the realtime daemon — so after install you can go
straight to `dux`.

**From source** (Rust toolchain):
```bash
cargo build --release
sudo install -m755 target/release/dux /usr/local/bin/dux
```

## 60-second start

```bash
# index once, then everything is instant
sudo dux scan /
dux                       # live tree UI (↑↓ move · → expand · i size⇄inodes · q quit)

# answer the incident
dux top /var --dirs       # biggest directories
dux top --inodes          # dirs with the MOST files (inode exhaustion)
dux find /home --name '*.log' --larger 1G
dux growth /data --since 1h
dux deleted-open          # space held by deleted-but-open files
dux status                # capacity + index freshness
```

### Run it live (systemd)

```bash
sudo cp packaging/dux.service /etc/systemd/system/
sudo systemctl enable --now dux          # initial scan, then realtime daemon

# alert when something fills the disk
dux daemon / --alert-threshold 1G --alert-window 10m --alert-exec /path/hook.sh
```

The daemon coalesces changes in memory and flushes batched updates — **~0% CPU
idle, low single-digit % of one core under heavy write load, zero added read
IOPS.**

---

## Trust it: independent verification

`dux` ships with an audit harness that checks the index against ground truth
(`du`/`df`/`find`) and its own internal consistency, and can auto-reconcile:

```bash
sudo scripts/dux-verify.sh audit          # integrity + ground-truth cross-checks
sudo DUX_RECONCILE=1 scripts/dux-verify.sh install-cron   # verify every 3h, self-heal
```

---

## How it works

Two components, one SQLite WAL file — no server, no second database, no eBPF:

```
dux CLI / TUI  ──reads──►  SQLite WAL index  ◄──writes──  dux daemon (scan + fanotify)
```

The daemon uses fanotify **FID mode** (`open_by_handle_at`) to track
**create / delete / rename / dir-creation / growth** live across every mounted
filesystem.

**Status & limitations** (disk usage = allocated blocks like `du`; live tracking
needs the daemon running; one tree per index; hardlinks counted once) are
documented honestly in **[docs/architecture-analysis-and-roadmap.md](docs/architecture-analysis-and-roadmap.md)**.

> Note: an old X11 tool named `xdu` exists in Debian/Ubuntu — this project is `dux`.

## License

MIT — see [LICENSE](LICENSE).
