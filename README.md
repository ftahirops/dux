# dux — the disk usage tool that already knows the answer

**Stop running `du` and waiting. `dux` keeps a live index of your filesystem, so
"what's eating my disk?" is answered in milliseconds — with history, growth
rates, and realtime alerts the classic tools can't give you.**

```
dux            # instant tree, sorted by size, live-updating
dux overview   # one-shot CLI dashboard: largest, growth, changes, reclaimable
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
| "What filled the disk overnight?" | **`dux diff --since 8h`** — net per-path change (fills *and* frees), from the index |
| Which container is bloating? | **`dux containers`** — writable-layer/log/volume usage per Docker/Podman container |
| Feed dashboards / automation | **`--json` on every command** + **`dux metrics`** (Prometheus exposition) |
| Drop-in for scripts | **`dux du`** — byte-exact `du`-compatible output, but instant (no re-walk) |

It's the tool you wish you'd had at 2 a.m.: **realtime, indexed, and it shows you
the culprit — not just the symptom.**

---

## New in 0.6.0

### Clear, answer-first TUI

- Four prominent numbered destinations replace the crowded inline navigation:
  **1 Overview**, **2 Explore**, **3 Activity**, and **4 Reclaim**. Number and
  letter shortcuts work globally, and `Ctrl-C` now exits cleanly.
- Overview shows the **top five** largest files, fastest-growing paths, and
  reclaim candidates. Each row explains what the path belongs to (for example
  PostgreSQL, Vault, Cargo build output, a backup, or system logging) instead of
  presenting an unexplained filename.
- The layout is responsive: wide terminals get balanced cards and distributions;
  an 80x24 terminal keeps the navigation and all five largest-file rows readable.
- Explore can group indexed usage by application/OS/user data, owner, age, object
  type, or file-size band. Reclaim separates measured facts from estimates and
  labels every suggestion by safety. **Dux advises; it never deletes.**

### Faster CLI answers with context

- Added `dux overview`, `dux large --files`, `dux fastest-growth --since 1h`,
  `dux activity --since 24h`, and `dux docker`. They are friendly aliases for
  the precise indexed reports and retain `--json` support for automation.
- `dux large --files --explain` adds owner, type, safety, application/workload,
  and purpose. Results can be sorted by size, safety, age, path, or owner.
- `dux explain PATH` identifies why an indexed file exists, how important it is,
  and the correct cleanup action. Classification is deliberately conservative:
  databases, swap, container storage, active logs, and unknown application data
  are never presented as blindly safe to remove.
- Explanation uses the index, so it remains instant and can describe a path even
  when the invoking user cannot traverse its parent or the file disappeared
  after the last indexed snapshot.

### Bounded scanning and a smaller live workload

- The initial crawl no longer retains the whole filesystem plus several scratch
  copies in memory. Parallel metadata workers feed a fixed 8,192-entry queue into
  a disk-backed staging database; hardlinks and directory totals are reduced
  before the compact final SQLite index is atomically installed.
- After the initial scan, fanotify sends only changed paths through a bounded,
  coalescing update pipeline. Create/delete bursts are collapsed, commits are
  batched, and overload degrades to an automatic reconcile instead of unbounded
  CPU or RAM growth.
- `dux status` and the TUI expose pending/capacity, events seen and resolved,
  updates committed, dropped events, throttling, scan progress, and catch-up lag.
  You can now distinguish **live and caught up** from scanning or intentionally
  throttled states.
- Dux's own database, WAL, shared-memory, and staging files are excluded from the
  scan, live events, growth reports, and rankings, preventing observer feedback
  and unnecessary index growth.

### Daemon and container reliability

- The packaged service uses `Restart=always`, bounded restart backoff, and a
  45-second application watchdog. It recovers from crashes/OOM kills and from a
  process that is alive but no longer making indexing progress.
- On restart, watch coverage is installed first; a low-priority atomic rebuild
  then reconciles the downtime gap while the previous index remains queryable.
- Nested overlay, tmpfs, namespace, and other non-storage views are filtered to
  avoid double-counting and false coverage warnings.
- Docker/Podman reporting breaks usage down into writable layers, logs, volumes,
  image/content storage, snapshots, and build/cache areas, then provides
  evidence-based cleanup suggestions.

### Bug fixes

- Fixed `Ctrl-C` not closing the TUI.
- Fixed the Overview cards showing only one file instead of useful top-five
  rankings with application relationships.
- Fixed congested navigation and tightly coupled status text.
- Fixed dux reporting its own database/WAL as fast-growing disk consumption.
- Fixed event storms causing excessive per-path work and poorly bounded queues.
- Fixed restart gaps that could leave an apparently live but stale index.
- Fixed pseudo-filesystem and nested container views being scanned or counted as
  independent storage.

---

## New in 0.5.2

**A CPU/I/O governor — dux stays invisible on a production host.** A background
reader must never be the reason a box slows down. The daemon now:

- **Hard-caps its own CPU.** It self-measures its CPU and injects proportional
  sleeps to hold a duty-cycle ceiling (`dux daemon --max-cpu`, default **25% of
  one core**). Under a filesystem event storm it *stays at the cap* instead of
  spiking — verified holding **3–5%** through a 1.8M-file create storm that used
  to pin 16–21%. An idle daemon is still ~0%.
- **Runs at idle I/O priority** (`IOPRIO_CLASS_IDLE`) + low `nice`, so the kernel
  only gives it disk/CPU when nothing else wants it — **zero contention** with
  your workload. The packaged unit adds cgroup backstops (`CPUQuota`, `IOWeight`).
- **Buffers, never spikes.** Bursts wait in the kernel's fanotify queue; the flush
  is bounded per cycle so a storm can't produce one giant spike. If the daemon
  ever can't keep up at its cap, it **degrades to eventual consistency** (marks
  the index for a later reconcile) rather than ever loading the host.
- **Tells you when it's behind.** If the governor is deliberately holding the
  daemon back, `dux status` and the TUI show a **THROTTLED** state explaining the
  index is intentionally stale to protect the host, how stale it is, and that it
  catches up automatically when load eases (raise `--max-cpu` for faster updates).

Tune it: `dux daemon / --max-cpu 10` for an ultra-quiet host, or higher for
snappier live updates on a machine with headroom.

---

## New in 0.5.0

**SRE/DevOps integration — everything scriptable, everything from the index (no
filesystem re-walk, no added daemon cost):**

- **`--json` on every read command** (`overview`, `top`, `find`, `growth`, `by-owner`,
  `by-ext`, `deleted-open`, `diff`, `du`, `containers`) — pipe straight to `jq`.
- **`dux metrics`** — Prometheus text-exposition output for the node_exporter
  textfile collector: `dux_fs_bytes_used`, `dux_fs_inodes_used`,
  `dux_index_bytes`, `dux_daemon_up`, `dux_last_scan_timestamp_seconds`, and
  `dux_path_bytes{path=…}` for the top directories (label values injection-safe).
- **`dux diff --since <window>`** (alias `since`) — the *"what filled or freed the
  disk?"* query: net per-path change over a window, ranked by magnitude.
- **`dux du`** — byte-exact `du`-compatible output (`-s`/`-a`/`-h`/`-m`/
  `--max-depth`) served from the index; verified block-for-block against GNU `du`.
- **`dux containers`** — per-container **writable-layer + log + volume** usage for
  **Docker and Podman**, resolved from on-disk metadata (no daemon socket, no
  `docker` CLI). The writable layer is read from the running container's overlay
  `upperdir` via `/proc/<pid>/mountinfo`, so it works with both the classic
  `overlay2` driver **and** the containerd snapshotter (Docker's default image
  store since v25).

**Portability:** the `.deb`/`.rpm` now ship a **static musl binary** with no
shared-library dependencies, so they run on **any x86-64 Linux** regardless of
host glibc (0.4.x hard-pinned a recent glibc and failed on Debian 12 / RHEL 9 /
Ubuntu 22.04). Reproducible via `scripts/build-release.sh`.

**Production hardening** (from a full audit — races, leaks, unsafe/FFI,
crash-safety — plus fuzzing, chaos, and soak testing):

- **Crash/downtime drift is no longer silent.** A daemon resuming after a crash,
  reboot, or clean stop/start flags the index **dirty** (with the downtime gap in
  the log), because fanotify can't see changes while it wasn't running — so
  `status`/TUI now recommend a reconciling `dux scan`. A post-crash scan also
  verifies the heartbeat PID is alive, so it reconciles directly instead of
  failing on a stale heartbeat.
- **Hardlink counter correctness.** Modifying a file through a *non-prime*
  hardlink no longer mis-attributes the byte delta or drives directory totals
  negative; a `MAX(0, …)` floor guarantees no total ever displays negative.
- **Bounded WAL under load.** The daemon now checkpoints under *Elevated* host
  pressure too (plus a 256 MiB forced-checkpoint backstop), so the write-ahead
  log can't grow without bound on a busy high-churn host.
- **Fuzz-hardened parsers** (a multibyte-input panic in size parsing is fixed;
  **0 panics** across thousands of adversarial inputs), fanotify parser
  compile-time guards, and a poison-tolerant scan cycle-guard lock.
- **TUI stays fresh across a rescan** — it reopens on the index swap instead of
  showing stale data, and no longer pins the old (deleted) index inode.

Verified: 21 unit tests, thousands of fuzz inputs (0 panics), repeated SIGKILL
mid-flush (`integrity_check` ok + exact reconcile every time), and a continuous
create/rename/delete soak (stable RSS, constant fd count — no leak).

---

## New in 0.4.0

- **Buttery-smooth TUI on huge filesystems.** The live tree's expensive periodic
  recompute (recursive growth aggregation, panels, path resolution) now runs on a
  **background thread**, so keyboard navigation never stalls — even on
  multi-million-file indexes. Results are applied as they arrive; the tree stays
  responsive while data refreshes behind it.
- **Correct hardlink deletes.** Deleting a directory that contained one link of a
  file hardlinked **elsewhere** now keeps the file (it still exists via the other
  link) and keeps directory totals exact — verified against a fresh scan.

---

## New in 0.3.0

**A resource guardian so dux always yields to the host.** dux is a background
tool — it must never be the thing that takes your server down. The daemon now
continuously watches host pressure (free RAM, free disk, load average, and
kernel PSI for memory/io/cpu) and self-throttles:

- **Critical** (low RAM, low disk, high load, or a PSI spike) → it **pauses its
  own index writes** (keeping pending changes, losing nothing) and hands SQLite's
  caches back to the OS. `dux status` and the TUI show `WRITES PAUSED (<reason>)`.
- **Elevated** → it keeps indexing but drops the optional extra work (WAL
  checkpoint, alert scan) so it adds no load.
- **Normal** → full speed; it resumes automatically when the host recovers.
- It also marks itself the **preferred OOM victim** (`oom_score_adj`), so if the
  kernel ever must reclaim memory it kills dux — never your real workload.

Plus performance work for very large filesystems:
- Daemon uses `poll(2)` instead of a 50 ms busy-loop — **~0.5 idle wakeups/s**
  (was ~20/s) and sub-millisecond event latency when active.
- Deleting a huge directory is now a **set-based** operation (a few SQL
  statements, not millions of round-trips) — a short transaction even for
  million-entry trees.
- TUI columns measure true terminal width, so **CJK/emoji filenames** no longer
  scatter the tree; full paths are cached per row (no per-keystroke lookups).

(Also rolls up the 0.2.1 / 0.2.2 fixes: seamless live-daemon rescan, rename
pairing across flush boundaries, and the dirty-state lifecycle.)

---

## New in 0.2.0

A rebuilt, hardlink-aware index (schema **v4**) and a hardened daemon. New
capabilities (the index format changed — upgrading rebuilds it automatically on
first start):

- **Hardlink-aware search** — `dux find` now returns **every** name a file is
  linked under, so a search resolves to the exact path you typed. Disk usage
  still counts the shared inode **once** (like `du`).
- **Works with any filename** — non-UTF-8 byte names are preserved (distinct
  names never collapse), and control/escape characters in filenames are safely
  escaped in all CLI/TUI output, so a crafted name can't forge or hijack your
  terminal.
- **Knows when it's stale** — `dux status` and the TUI now show a **DIRTY**
  banner when the index has missed events (fanotify overflow, a filesystem that
  couldn't be watched, low disk, or overload) so you know to rescan instead of
  trusting a degraded index.
- **Safer, host-friendly daemon:**
  - **Exclusive per-database lock** — two writers (a second scan or daemon) can
    no longer race and corrupt the index; you get a clear error instead.
  - **Graceful shutdown** — flushes pending changes on `systemctl stop` / SIGTERM
    instead of dropping the last window.
  - **Low-disk protection** — pauses index writes when the filesystem drops below
    256 MiB free, so dux is never the process that fills your disk.
  - **Bounded under stress** — caps the in-memory event backlog and the number of
    concurrent `--alert-exec` processes (and reaps them), and tracks
    metadata-only changes plus directories moved into the tree.
  - **Root-validated** — refuses to attach live events to a tree it didn't index
    (auto-rebuilds instead).
- **Nicer TUI** — large directories show a "… more entries" marker instead of
  silently truncating, empty directories no longer show a false expand arrow,
  and the largest-files / fastest-growth panels scope to the subtree you opened.
- **Hardened packaging** — tightened systemd unit (capability bounding,
  `NoNewPrivileges`, `MemoryDenyWriteExecute`, …) and a documented security
  posture.

---

## Install

The packages ship a **static musl binary** with no shared-library dependencies,
so they run on **any x86-64 Linux** regardless of host glibc. The commands below
resolve the newest release automatically, so they never go stale between versions.

**Debian / Ubuntu**:
```bash
curl -s https://api.github.com/repos/ftahirops/dux/releases/latest \
  | grep -o 'https://[^"]*_amd64\.deb' | xargs curl -LO
sudo dpkg -i dux_*_amd64.deb
```

**RHEL / Fedora / Rocky / openSUSE**:
```bash
curl -s https://api.github.com/repos/ftahirops/dux/releases/latest \
  | grep -o 'https://[^"]*\.x86_64\.rpm' | xargs curl -LO
sudo rpm -i dux-*.x86_64.rpm
```

**Standalone binary** (no package manager, any distro):
```bash
curl -s https://api.github.com/repos/ftahirops/dux/releases/latest \
  | grep -o 'https://[^"]*-x86_64-linux-static' | xargs curl -L -o dux
sudo install -m755 dux /usr/local/bin/dux
```

The `.deb`/`.rpm` install `/usr/bin/dux` and a systemd unit that builds the index
on first start and then runs the realtime daemon — so after install you can go
straight to `dux`. (Browse all downloads on the
[latest release page](https://github.com/ftahirops/dux/releases/latest).)

**From source** (Rust toolchain):
```bash
cargo build --release
sudo install -m755 target/release/dux /usr/local/bin/dux
```

## 60-second start

```bash
# index once, then everything is instant
sudo dux scan /
dux                       # four views: Overview · Explore · Activity · Reclaim

# answer the incident
dux top /var --dirs       # biggest directories
dux large --files --explain --sort safety  # owner/type/relation/deletion safety
dux explain /swap.img     # purpose, importance, why, and correct cleanup method
dux top --inodes          # dirs with the MOST files (inode exhaustion)
dux find /home --name '*.log' --larger 1G
dux growth /data --since 1h
dux diff --since 8h       # what FILLED (or freed) the disk in the last 8h
dux du -sh /var/log       # byte-exact du, but instant (no re-walk)
dux containers            # writable/log/volume/reclaimable Docker/Podman breakdown
dux deleted-open          # space held by deleted-but-open files
dux status                # capacity + index freshness
```

### Automate & integrate (SRE/DevOps)

```bash
# JSON on every read command → pipe to jq / dashboards
dux top --dirs --json | jq '.[] | {path, bytes}'
dux large --files --explain --json | jq '.[].classification'
dux explain /var/log/auth.log.1 --json

# Prometheus metrics for the node_exporter textfile collector
dux metrics > /var/lib/node_exporter/textfile_collector/dux.prom
#   exposes dux_fs_bytes_used, dux_index_bytes, dux_daemon_up,
#   dux_last_scan_timestamp_seconds, and dux_path_bytes{path=...}

# "what changed the disk overnight?" (needs the daemon running for history)
dux diff --since 12h

# per-container writable-layer / log / volume usage (Docker & Podman;
# works with the classic overlay2 driver AND the containerd snapshotter)
dux containers --json
```

All read commands accept `--json`; every query above is served from the index
(no filesystem re-walk), so it stays instant and adds no daemon overhead.

### Run it live (systemd)

```bash
sudo cp packaging/dux.service /etc/systemd/system/
sudo systemctl enable --now dux          # initial scan, then realtime daemon

# alert when something fills the disk
dux daemon / --alert-threshold 1G --alert-window 10m --alert-exec /path/hook.sh
```

The daemon coalesces repeated changes by path and flushes bounded batches — only
changed paths are restatted, never the full tree. It is **~0% CPU idle**, stays
under its configured one-core duty-cycle ceiling during write storms, and adds
no steady-state scan I/O. `dux status` shows whether it is caught up.

### Who can read the index

The index lists **every filename on `/`** — including names inside directories a
user could not otherwise traverse. It exposes no file *contents*, but reading it
does bypass directory permissions, so the packaged service keeps it at **`0640
root:dux`**, not world-readable.

```bash
sudo usermod -aG dux alice     # alice can now run `dux find` / `dux status`
```

Everyone else (and any unprivileged tooling) must go through `sudo` or a
privileged wrapper. To lock it down to root-only, or to restore the old
world-readable behaviour, see the `index exposure` notes at the bottom of
`packaging/dux.service`.

### Index states — and when to rescan

`dux` never silently claims exact data. When it cannot guarantee the index
matches the filesystem it says so, and `dux status` (and the TUI header) surface
the state:

| State | What it means | What to do |
|---|---|---|
| *(none shown)* | Index is live and believed exact. | Nothing. |
| **DIRTY** | The daemon knows it **missed events** — fanotify queue overflow, a downtime gap, dropped backlog, missing capabilities, partial watch coverage, or a moved-in directory too large to reconcile. Totals may be stale or wrong. | **`sudo dux scan /`** — the only thing that clears it. |
| **WRITES PAUSED** (*reason*) | Deliberate self-protection: low memory, low disk, high load, or kernel PSI pressure. The daemon stopped writing and shrank its caches to protect the host. Events keep coalescing in memory. | Nothing — it resumes automatically. Fix the underlying pressure. If it persists under sustained churn the backlog cap trips and the index goes **DIRTY**. |
| **THROTTLED** | The CPU governor is intentionally holding dux back (default: 25% of one core), so the index is *knowingly behind*. `dux status` reports how stale. | Nothing. Raise `daemon --max-cpu` only if you want dux to catch up faster at the cost of host CPU. |

```bash
dux status              # index state, staleness, daemon liveness
dux status --json       # same, for monitoring
```

Rule of thumb: **DIRTY is the only state that needs you.** Paused and throttled
are dux choosing the host over its own freshness, and both self-recover.

A `dux scan` is also warranted after bulk changes made while the daemon was
stopped, and is safe to run any time — rebuilds are atomic (the live index is
replaced only once the new one is complete, so a failed scan never leaves a
half-built index).

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

Two long-lived components and one compact SQLite WAL index — no server and no
eBPF. A temporary staging DB exists only during a full rebuild:

```
initial crawl ──bounded queue──► staging DB ──atomic build──► SQLite WAL index
                                                               ▲          │
                                             fanotify changes ─┘          ▼
                                                          dux CLI / TUI reads
```

The daemon uses fanotify **FID mode** (`open_by_handle_at`) to track
**create / delete / rename / dir-creation / growth** live across every mounted
filesystem. It therefore needs **two capabilities**: `CAP_SYS_ADMIN` (fanotify)
and `CAP_DAC_READ_SEARCH` (resolve event file-handles to paths). The packaged
`dux.service` grants both; if you run the daemon under a custom unit that drops
`CAP_DAC_READ_SEARCH`, it can receive events but resolves none — dux now detects
this, logs a clear error, and marks the index dirty rather than failing silently.

**Status & limitations**: disk usage = allocated blocks like `du`; live tracking
needs the daemon running; one tree per index; hardlinks counted once for size but
every path is searchable.

> Note: an old X11 tool named `xdu` exists in Debian/Ubuntu — this project is `dux`.

## License

MIT — see [LICENSE](LICENSE).
