# dux: disk-full alerts that tell you where, who and what to do

**dux 0.7 watches every disk every 2 seconds, alerts you the moment one starts
filling, and follows up with the folder that grew, the service writing to it
and the safe command that frees the space.** It uses the kernel's own counters
and a fast walker instead of a per-file database, so it costs almost nothing to
run and never falls behind.

```
dux                    # terminal UI: Disks, Activity, Folders, Docker, Logs, Projects, Fixes
dux io                 # live: which files are being written/read, how fast, and by whom
dux web                # local web dashboard (token-protected, read-only by default)
dux doctor /           # disk health report in ~15 s, ending with a fix list
dux docker             # where Docker's space goes, with fixes
dux logs               # every log, its growth and its writer, with fixes
dux pp                 # per project / app: source, build output, git, with fixes
dux biggest            # biggest files and folders
dux fix                # every fix on the host in one rated list
dux sentinel           # the always-on monitor (installed as dux-sentinel.service)
dux where /            # where the space is, and what grew since last time (instant)
```

> **Release status: 0.8.1.** Built and load tested (see [measured results](#measured-results))
> on Ubuntu 24.04 hosts; a multi-day soak on a busy production host is still to come. Not built
> yet: opt-in level 4 cleanup categories and the setup wizard.

---

## New in 0.8.1: live I/O, and who is writing

Plain `dux` now opens the new UI. Its **Activity** tab, and `dux io` on the
command line, refresh every second and show:

- **Disk I/O now:** read and write speed per disk.
- **Who is writing and reading now:** processes with their service or container.
- **Writing now:** each file being written, its write speed, size growth per second, size and writer.
- **Writer details** for the selected file: pid, user, command line, executable,
  working folder, parent process, service or container, how long it has run, and
  the next commands to inspect or stop it (`systemctl status`, `docker logs`, `kill`).
- **Reading now**, **growing now** (last minute) and **biggest changes** since the
  last sentinel snapshot.

It samples `/proc` directly (disk counters, per-process I/O, open file
positions and sizes), so it needs no event stream and costs a few
milliseconds per second while it is open. Container logs are attributed to
the container, not to dockerd. The old 0.6 browser is still available as
`dux tui --legacy`.

## New in 0.8.0: see where space goes, fix it in one pick

Every report now ends with a **numbered fix list**. Each fix is rated
**SAFE** (rebuilds itself or is an old copy), **CAUTION** (recoverable, but
costs a re-download, a rebuild or old log lines) or **RISKY** (can lose
data), with what it does, how much it frees and how to undo it.

```
Fixes for logs: SAFE 9.26 MiB, CAUTION 1.07 GiB, RISKY 0 B
    1. SAFE      7.96 MiB  remove rotated log /var/log/syslog.1
    3. CAUTION 831.55 MiB  truncate .../xgm-postgres-json.log (keep its last 10 MiB)
Choose: numbers (1,3,5-7) | safe | all | truncate | remove | clean | Enter = nothing:
```

| You type | It applies |
|---|---|
| `1,3,5-7` | those items |
| `safe` | every SAFE item |
| `all` | every SAFE and CAUTION item |
| `truncate` / `remove` / `clean` | every log truncation / file or folder removal / cleanup command shown |

RISKY items (deleting one of your big files, an unused Docker volume) can
only be picked by number and need `delete` typed. In scripts use
`--select 1,3`, `--apply-safe` or `--apply-all`; scripts cannot pick RISKY
items. Every action is logged to `/var/log/dux/actions.log`.

| Command | What it shows | Fixes it offers |
|---|---|---|
| `dux docker` | images (unique vs shared, unused), container writable layers and logs, volumes, build cache, real folder sizes under the Docker and containerd roots, per compose project | build cache, dangling and unused images, old stopped containers, oversized container logs, unused volumes (RISKY) |
| `dux logs` | journal, system, Docker and app logs; active vs rotated; growth per minute; the process writing each | rotated copies, truncating big active text logs (keeps the last 10 MiB), journal vacuum |
| `dux pp` | every project found by its marker files (`package.json`, `Cargo.toml`, `go.mod`, `.git`...), split into source, build output and git history, with idle days; app data folders; compose projects | removing build output (`node_modules`, `target`, `.venv`, `dist`...) |
| `dux biggest` | biggest files (owner, age) and folders, plus growth since the last snapshot | deleting a specific big file (RISKY) |
| `dux doctor` | the whole-disk health report | package caches, temp files, core dumps, Docker build cache, deleted-but-open files |
| `dux fix` | all of the above in one list | all of the above |

**`dux ui`** is a terminal UI with tabs for Disks, Folders (drill in with
Enter), Docker, Logs, Projects and Fixes (Space toggles, `s` picks all SAFE,
`a` picks SAFE + CAUTION, Enter applies after a confirm screen).

**`dux web`** serves the same view in a browser. It listens on 127.0.0.1 and
prints a URL with a random access token. It is read-only unless started with
`--allow-actions`. Applying is a POST that must carry the token in a header,
so other websites cannot trigger it.

Binary logs (journald files, sysstat data) are never truncated or removed by
path; the journal is only shrunk with `journalctl --vacuum-*`.

---

## New in 0.7.0: a complete redesign

0.6 kept a live database row for every file, fed by kernel events. On busy
build servers the event queue overflowed hundreds of times a day, every
overflow forced a full rebuild, and the index fell days behind while writing
over 100 GB a day to disk. 0.7 replaces that design:

| | 0.6 (per-file index) | 0.7 (counters first, walk on demand) |
|---|---|---|
| How it knows usage | every file change as an event, one database row per file | the kernel's exact `statvfs` counters, every 2 s |
| How it finds the culprit | queries the per-file database | a fast parallel walk into an in-memory folder tree, hourly and right after growth |
| What it stores | 4.3 GB SQLite database (3.2M files) | 23.5 MB compact folder snapshot |
| On event loss | full rebuild; automatic repair stopped after 2 failures | nothing can be lost: there is no event stream |

### What you get

- **Disk-full, time-to-full and inode alerts in about 1 second**, exact, never stale.
- **A follow-up alert naming the folder that grew** (for example
  `/var/log/app +2.1 GiB`), the service writing (from cgroup I/O counters) and
  the safe fix from a built-in catalog of known culprits: journal, Docker
  logs/images/build cache, package caches, core dumps, temp files, old kernels.
- **Alert channels:** system log, `wall` to every terminal (critical only, rate
  limited), [ntfy](https://ntfy.sh) phone push, Slack, any webhook, and email.
- **Controlled alerting:** alerts fire on entry or when clearly worse, repeat
  after a quiet interval, step down only after holding for 2 minutes (no
  flapping), and respect an hourly cap.
- **`dux doctor`:** biggest folders, known culprits with fixes, folders with the
  most files (drilled down to the crowded one), deleted-but-open files, and who
  is writing right now.
- **`dux where`:** instant answer from the latest snapshot, plus what grew since
  the previous one.
- **`dux cleanup`:** levelled, previewed, audited cleanup (see below).
- **Health probes every minute:** ext4 error counters, software RAID, disk
  latency, I/O and memory pressure (PSI), and SMART on physical disks.
- **Optional emergency mode:** when the disk holding `/var/log` goes critical,
  run cleanup automatically up to a level you choose (off by default).

### Who it helps, and what it resolves

| Role | What dux 0.7 gives you | Core problem it resolves |
|---|---|---|
| **SRE / on-call** | disk-full, inode and time-to-full alerts in about 1 s; a follow-up naming the folder, the writing service and the fix; `dux doctor` in about 15 s; `wall`, ntfy, Slack, email, webhook | "Disk is 96% full at 2 a.m., where and why?" is answered by the alert itself, before anyone logs in |
| **Platform / DevOps** | Docker, containerd, journal, package-cache and core-dump culprits with safe fixes; cgroup writer attribution; levelled cleanup with preview and audit log; optional emergency cleanup | runaway container logs, build caches and journals filling hosts; cleanup that is safe enough to automate |
| **Developers and CI** | `dux walk` sizes a whole filesystem in seconds without an index; `dux where` shows what grew between runs; inode-heavy folders drilled down to the exact directory | build agents running out of space or inodes from `target/`, `node_modules`, caches and test artefacts |
| **Security** | read-only alerts with no per-file database to protect; growth and mass-delete visibility through walk diffs; deleted-but-open file detection; filesystem error and read-only remount alerts; secrets never placed on command lines | noticing abnormal growth or deletion, hidden space held by deleted files, and failing or tampered filesystems |
| **IT / small teams** | one `.deb`, one config file, ntfy on a phone, no database to manage, about 0.1 GB of writes per day | knowing a server is filling up without running a monitoring stack |

### A base for file-based checks

Every walk already visits every file and folder once, at about 2 µs per entry,
and diffs the result against the previous walk. That makes the walker a cheap
base for checks that need to look at all files.

**Available now:**
- largest folders and files;
- growth and shrinkage per folder between walks, with mass deletes visible in the diff;
- folders with the most files;
- the newest change time per folder;
- known-culprit sizes;
- the gap between used space and files found, which points at deleted-but-open files.

**Can be added on the same walk at near-zero cost** (planned, not built yet):
- new or changed set-uid and set-gid binaries;
- world-writable files in system paths;
- files owned by deleted users;
- executables in `/tmp` and `/dev/shm`;
- secret-like file names (`*.pem`, `id_rsa*`, `.env`) outside their usual places;
- large files that appeared since the last walk;
- backup folders whose newest file is too old;
- sudden spikes in renamed or rewritten files.

Each would be a rule evaluated while the walk reads metadata it already has.
No extra pass over the disk and no per-file database are needed.

### Measured results

Measured on a 6 vCPU / 5.8 GB VM with 3.27 million files (46.7 GB), against
0.6 measured on a comparable production host with 3.18 million files.

| | 0.6 | 0.7 |
|---|---|---|
| Disk writes | 118.7 GB/day | **about 0.1 GB/day** |
| Average CPU | 5.7% of one core | **about 0.7%** (0.08% when idle between walks) |
| Memory | 85 MB | **5 MB idle, 95 MB peak during a walk** |
| Stored data | 4.3 GB | **23.5 MB** |
| Index freshness | 2.9 days behind | **disk totals live; folders as of the last walk (labelled)** |
| Full walk of every file | ~280 s | **12.6-15.6 s** (4 threads), total within 0.0015% of `du` |
| Disk-full alert | (behind) | **0.18 s warning, 0.98 s critical** |

Load tests on throwaway filesystems:

- 0.8-1.1 s alert latency with 12 CPU burners plus heavy disk I/O.
- No flapping alerts while usage bounced between 92% and 97% for 3 minutes.
- 10 filesystems going critical at once gave 10 alerts, with `wall` capped at 4 per hour.
- `wall` and ntfy still arrived in 0.18 s while a webhook hung for 30 s.
- A follow-up alert named the right folder 41 s before the disk reached 90%.
- 392 automated tests.

---

## Install

```bash
# Debian/Ubuntu (glibc 2.39+, e.g. Ubuntu 24.04, Debian 13)
curl -LO https://github.com/ftahirops/dux/releases/download/v0.8.1/dux_0.8.1-1_amd64.deb
sudo apt install ./dux_0.8.1-1_amd64.deb
```

The package:
- installs and starts `dux-sentinel.service`;
- stops and disables the 0.6 `dux.service`;
- creates `/etc/dux/notify.conf` (mode 600).

The old 0.6 index (`/var/lib/dux/dux.db`) is left in place. Delete it when you
are ready.

Build from source:

```bash
cargo build --release          # binary at target/release/dux
scripts/build-deb.sh           # .deb at target/deb/
```

## Set up alerts

Edit `/etc/dux/notify.conf` (all keys are documented in
[`packaging/notify.conf.example`](packaging/notify.conf.example)), then:

```bash
sudo systemctl restart dux-sentinel
dux notify-test                       # warning to every channel
dux notify-test --severity critical   # also tests wall
```

| Channel | Settings | Notes |
|---|---|---|
| System log | always on | `journalctl -u dux-sentinel` |
| wall | `DUX_WALL=yes` | critical only; once per disk per 30 min, at most 4 an hour |
| ntfy | `DUX_NTFY_URL`, `DUX_NTFY_TOKEN` | critical alerts use urgent priority |
| Slack | `DUX_SLACK_WEBHOOK` | incoming webhook |
| Webhook | `DUX_WEBHOOK_URL`, `DUX_WEBHOOK_TOKEN` | JSON: source, severity, title, body, key, host, timestamp |
| Email | `DUX_SMTP_URL`, `DUX_SMTP_USER`, `DUX_SMTP_PASS`, `DUX_MAIL_FROM`, `DUX_MAIL_TO` | `smtp://` requires STARTTLS; `smtps://` for TLS |

Default thresholds:
- Warning at 85% used, critical at 95%; the same for inodes.
- Full-in-hours: warning at 24 h, critical at 2 h.
- A burst alert when a disk grows 5 GiB within 10 minutes.

All of these can be changed in the same file. HTTP and SMTP go through `curl`,
with secrets passed on stdin, never on the command line.

## Cleanup levels

`dux cleanup` always shows its plan first, including how much each step frees
and the exact command. Nothing changes without `--apply`.

| Level | What it may do |
|---|---|
| 0 | nothing (monitor only) |
| 1 | shrink the systemd journal to a budget (`--journal-max`, default 1G) |
| 2 | + remove rotated logs oldest first, keeping the newest 3 of each; and, only when the disk is at least 80% full, rotate big active logs with their logrotate config, or keep the last 10 MB and truncate in place |
| 3 | + package caches (`apt-get clean`, `dnf clean packages`), old temp files (`systemd-tmpfiles --clean`), old crash and core dumps (newest 5 kept), Docker build cache older than 7 days |

Safety rules applied to every action:
- Security and accounting logs (`auth.log`, `secure`, `audit/`, `wtmp`, `btmp`, `lastlog`) are never touched.
- Open files are never removed or truncated.
- Symlinks are never followed, and other filesystems are never entered.
- Commands run without a shell.
- `/etc/dux/disable-actions` stops all actions.
- Every applied action is logged to `/var/log/dux/actions.log`.

Emergency mode (`DUX_AUTO_CLEANUP_LEVEL=1-3`, default 0) runs the same plan
automatically when the disk holding `/var/log` goes critical. It stops 10
points below critical and reports what it freed.

## How 0.7 works

```
statvfs every 2 s ──► space / inode / fill / burst / read-only alerts ──► channels
        │                        │
        │ growth or alert        └──► walk scheduler (startup, hourly, after growth; ≥10 min apart)
        ▼                                     │
cgroup io.stat every 10 s                     ▼
 (who is writing)              fast walker: getdents64 + fstatat, 2-4 threads,
        │                      nice 19, idle I/O, one node per folder, no writes
        │                                     │
        └──────────────► diff vs previous tree ──► "growth is in /var/log/app" alert
                                              │
                               snapshot every 6 h + growth history
probes every minute: ext4 errors, RAID, latency, PSI, SMART (hourly)
```

The walker:
- never crosses into another filesystem or a bind mount;
- counts hard links once;
- keeps the five largest files (1 MiB or more) per folder.

Listing the files in one folder is a live `readdir`, so no per-file storage
is needed.

---

## Legacy: the 0.6 per-file index

The commands below (`dux scan`, `dux daemon`, `dux top`, `dux find`,
`dux growth`, the TUI and so on) are still in the binary and documented as
they were. 0.7 no longer installs or starts the per-file daemon.

### Reliability fixes in 0.6.2

- Host load is compared with host CPUs, independently of the daemon's CPU quota.
  Resource pauses require sustained recovery before resuming.
- Failed event batches and deferred renames remain available for retry. Missing
  ancestors are inserted before children; directory allocation and writes that
  follow renames are included in totals.
- Reconciliation builds a bounded staging index on a background thread while
  event ingestion continues. Existing indexes are replaced through a SQLite
  transaction, preserving active readers and their WAL snapshots. Growth history
  is carried into the replacement index rather than discarded.
- The kernel event queue and moved-directory work are bounded. Event loss and
  failed accounting checks mark the index dirty and schedule reconciliation with
  a five-minute cooldown. Partial watch coverage still requires operator action.
- Hardlink target FIDs let the daemon refresh a surviving inode when a temporary
  hardlink is written and removed before its alias path can be indexed. Live
  indexing requires Linux 5.17+ or a vendor backport of `FAN_REPORT_TARGET_FID`.
- Keyboard handlers perform no database or filesystem work. View requests use a
  one-slot mailbox; health refreshes independently. Refresh failures and delayed
  views are visible and retried, and obsolete results cannot overwrite a new view.
- Runtime records are published atomically and scoped to their database. SQLite
  writes use FULL synchronization; `dux --version` includes the source revision.

### New in 0.6.0

#### Clear, answer-first TUI

- Five prominent numbered destinations replace the crowded inline navigation:
  **1 Overview**, **2 Explore**, **3 Activity**, **4 Reclaim**, and
  **5 Large Files**. Number and
  letter shortcuts work globally, and `Ctrl-C` now exits cleanly.
- Overview shows the **top five** largest files, fastest-growing paths, and
  reclaim candidates. Each row explains what the path belongs to (for example
  PostgreSQL, Vault, Cargo build output, a backup, or system logging) instead of
  presenting an unexplained filename.
- Large Files provides a complete paged inventory of indexed files, ranked by
  allocated space or absolute growth over the selected 1h/24h/7d window. Its
  treemap sizes tiles proportionally within the current 100-file page; the
  ranked table pages through the full inventory and shows allocated bytes,
  growth delta, modification age, and path. Use `↑`/`↓` to select, `PgUp`/`PgDn`
  to page, `s` to rank by size, `G` to rank by growth, and `[`/`]` to change
  the history window.
- The layout is responsive: wide terminals get balanced cards and distributions;
  an 80x24 terminal keeps the navigation and all five largest-file rows readable.
- Explore can group indexed usage by application/OS/user data, owner, age, object
  type, or file-size band. Reclaim separates measured facts from estimates and
  labels every suggestion by safety. **Dux advises; it never deletes.**

#### Faster CLI answers with context

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

#### Bounded scanning and a smaller live workload

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

#### Daemon and container reliability

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

#### Bug fixes

- Fixed `Ctrl-C` not closing the TUI.
- Fixed the Overview cards showing only one file instead of useful top-five
  rankings with application relationships.
- Fixed congested navigation and tightly coupled status text.
- Fixed dux reporting its own database/WAL as fast-growing disk consumption.
- Fixed event storms causing excessive per-path work and poorly bounded queues.
- Fixed restart gaps that could leave an apparently live but stale index.
- Fixed pseudo-filesystem and nested container views being scanned or counted as
  independent storage.
- Fixed the static-musl release build on platforms where `statfs.f_type` has an
  unsigned ABI representation.

---

### New in 0.5.2

**CPU/I/O controls for a lower-priority background indexer.** Dux shares host
resources with production workloads. Its controls reduce its impact; they do not
guarantee zero contention or uninterrupted index freshness:

- **Limits event-loop CPU.** It measures the watcher thread and injects
  proportional sleeps to target a duty cycle (`dux daemon --max-cpu`, default
  **25% of one core**). Work outside that thread is bounded by the packaged
  cgroup limits. Large event bursts can still raise short-term load. An idle
  daemon is typically near 0% CPU.
- **Runs at idle I/O priority** (`IOPRIO_CLASS_IDLE`) + low `nice`, so the kernel
  gives it lower I/O scheduling priority. The packaged unit also sets CPU and
  I/O weights and a **100% of one core** CPU ceiling. These controls reduce the
  daemon's resource share under contention, but do not eliminate its impact.
- **Bounds event batches.** Fanotify queues events in a finite kernel queue and
  the watcher flushes bounded batches. If event rates exceed processing capacity,
  the queue can overflow; Dux marks coverage uncertain and requires reconciliation.
  Sustained host pressure can delay that reconciliation, so stale or uncertain
  results can persist until the scan is admitted and completes.
- **Reports lag and uncertainty.** `dux status` and the TUI expose queued work,
  dirty state, and reconciliation progress. Raise `--max-cpu` only when the host
  has measured headroom; the systemd CPU quota still sets a hard ceiling.

Tune it: `dux daemon / --max-cpu 10` for an ultra-quiet host, or higher for
snappier live updates on a machine with headroom.

---

### New in 0.5.0

**SRE/DevOps integration — everything scriptable, everything from the index (no
filesystem re-walk, no added daemon cost):**

- **`--json` on every read command** (`overview`, `top`, `find`, `growth`, `by-owner`,
  `by-ext`, `deleted-open`, `diff`, `du`, `containers`) — pipe straight to `jq`.
- **`dux metrics`** — Prometheus text-exposition output for the node_exporter
  textfile collector: `dux_fs_bytes_used`, `dux_fs_inodes_used`,
  `dux_index_bytes`, `dux_daemon_up`, `dux_last_scan_timestamp_seconds`, and
  `dux_path_bytes{path=…}` for the top directories (label values injection-safe).
  `dux_fanotify_overflow_count_available` distinguishes a known zero from legacy
  watcher status formats that do not publish the overflow counter.
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
Ubuntu 22.04). `scripts/build-release.sh` uses pinned `cargo-auditable 0.7.7`
to embed the Rust dependency list in the executable. Before packaging, it audits
both `Cargo.lock` and the exact binary with `cargo-audit` against the current
RustSec advisory database, rejects incomplete binary inventories, and includes
the audit reports and advisory database identity in the release bundle.
Source-built release bundles also include `dux.debug` with the runtime binary's
matching Build ID. Keep it with the release evidence; to add its symbols to
Linux `perf`, run `perf buildid-cache --add <path-to-release>/dux.debug`. The
installed `.deb`/`.rpm` executable omits DWARF, retains its function symbol
table, and references `dux.debug` by GNU debug link; runtime package size does
not include DWARF data.

**Release qualification:** the current plan, acceptance criteria and outstanding
gates are in [PRODUCTION_READINESS_AUDIT.md](PRODUCTION_READINESS_AUDIT.md).
Passing unit tests or a short churn run does not qualify a release for every
filesystem or workload. The 72-hour endurance and package/platform gates require
separate recorded evidence.

For this release, the recorded crash-durability qualification matrix covers
ext4, XFS and Btrfs on Debian 12 / Linux 6.1. Other kernels and filesystems are
outside that recorded fault-test matrix; verify them in your deployment before
relying on the same crash-recovery behavior.

The isolated Linux failure suite builds a disposable loop filesystem in a
private mount namespace. `--filesystem-type` selects ext4 (default), XFS, or
Btrfs; install the matching filesystem utilities (`e2fsprogs`, `xfsprogs`, or
`btrfs-progs`) before running that matrix. It exercises SIGKILL boundaries, actual fanotify queue
overflow, rejected health-metadata writes, full/read-only storage, unreadable
subtrees and bounded churn. It never opens the installed live index. Requires
root, systemd, loop mounts, Python 3 and e2fsprogs:

```sh
cargo build --locked --release
mkdir -p target/readiness
cp target/release/dux target/readiness/dux-production
cargo build --locked --release --features fault-injection
cp target/release/dux target/readiness/dux-faults
cargo build --locked --release # restore the ordinary build for packaging
sudo python3 scripts/dux-readiness.py --binary target/readiness/dux-production \
  --fault-binary target/readiness/dux-faults --crash-rounds 30 --duration-seconds 120
```

The indexer and optional remote access have separate services. Packages do not
create tunnel identities, enable `dux-tunnel.service`, or start outbound tunnels.
The indexer retains only `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`. Operators who
need remote access must install its SSH dependencies, create a root-owned
`/etc/dux/tunnel.conf` (mode `0600`, parent `/etc/dux` mode `0700`) with one
reviewed `HOST PORT` endpoint per line, and create its identity with
`dux tunnel keygen`. Provision `/etc/dux/known_hosts` (root-owned, mode `0600`)
with the SSH host keys for those endpoints. You may collect candidate keys with
`ssh-keyscan -p PORT HOST`, but verify each fingerprint against an independent
trusted source before installing it; the tunnel refuses unknown or changed
keys. Then enable `dux-tunnel.service`. The executable contains no operator
endpoints; the config, identity, and verified host keys are required. This
service remains separate from the indexer.

Schema v6 keys retained growth history by inode birth time to distinguish rapid
inode reuse; legacy ambiguous history remains hidden rather than misattributed.
An atomic rebuild upgrades v4/v5 while retaining legacy rows as unattributed.
Rollback across
this schema change requires the matching pre-upgrade database backup as well as
the previous executable; retain both before replacing a live installation.

JSON evidence, including executable hashes, is written to
`target/readiness/report.json`. Fault hooks are compiled out of default builds;
never install the explicitly enabled `dux-faults` executable. Run the bounded
isolated Linux failure suite using the qualification procedures in
`docs/QUALIFICATION.md`; no GitHub Actions workflow is currently configured.
`dux metrics --top 0` exposes dirty, paused, scan, backlog and caught-up signals;
alert on `dux_index_dirty=1` and sustained `dux_index_caught_up=0` alongside
`dux_daemon_up`. Treat `dux_fanotify_overflow_count_available=0` as unknown
overflow history, not as a zero count. When writing a node_exporter textfile, refresh it by writing a
temporary file in the collector directory and renaming it into place. Monitor
that file's timestamp or absence too: if `dux metrics` cannot open the index it
cannot emit `dux_up=0`, and a collector may otherwise keep exposing the last
successful snapshot. A readable index alone does not mean its data is current.

**Production hardening history:**

- **Crash/downtime drift is no longer silent.** A daemon resuming after a crash,
  reboot, or clean stop/start flags the index **dirty** (with the downtime gap in
  the log), because fanotify can't see changes while it wasn't running — so
  `status`/TUI now recommend a reconciling `dux scan`. A post-crash scan also
  verifies the heartbeat PID is alive, so it reconciles directly instead of
  failing on a stale heartbeat.
- **Hardlink counter correctness.** Modifying a file through a *non-prime*
  hardlink no longer mis-attributes the byte delta or drives directory totals
  negative; a `MAX(0, …)` floor guarantees no total ever displays negative.
- **WAL checkpointing under load.** The daemon checkpoints under *Elevated* host
  pressure too and retries when the WAL exceeds 256 MiB. This is a soft target:
  long-lived readers can prevent a passive checkpoint from reclaiming pages.
  Reader-held WAL growth remains a release qualification gate.
- **Fuzz-hardened parsers** (a multibyte-input panic in size parsing is fixed;
  **0 panics** across thousands of adversarial inputs), fanotify parser
  compile-time guards, and a poison-tolerant scan cycle-guard lock.
- **TUI stays fresh across a rescan** — it reopens on the index swap instead of
  showing stale data, and no longer pins the old (deleted) index inode.

Verified: 21 unit tests, thousands of fuzz inputs (0 panics), repeated SIGKILL
mid-flush (`integrity_check` ok + exact reconcile every time), and a continuous
create/rename/delete soak (stable RSS, constant fd count — no leak).

---

### New in 0.4.0

- **Buttery-smooth TUI on huge filesystems.** The live tree's expensive periodic
  recompute (recursive growth aggregation, panels, path resolution) now runs on a
  **background thread**, so keyboard navigation never stalls — even on
  multi-million-file indexes. Results are applied as they arrive; the tree stays
  responsive while data refreshes behind it.
- **Correct hardlink deletes.** Deleting a directory that contained one link of a
  file hardlinked **elsewhere** now keeps the file (it still exists via the other
  link) and keeps directory totals exact — verified against a fresh scan.

---

### New in 0.3.0

**A resource guardian that reduces load under pressure.** Dux watches host
pressure (free RAM, free disk, load average, and kernel PSI for memory/I/O/CPU)
and changes its work rate:

- **Critical** (low RAM, low disk, high load, or a PSI spike) → it pauses index
  writes, retains pending userspace changes, and asks SQLite to release caches.
  Fanotify's kernel queue is finite; overflow marks coverage uncertain and
  requires reconciliation. `dux status` and the TUI show `WRITES PAUSED (<reason>)`.
- **Elevated** → it keeps indexing but drops the optional extra work (WAL
  checkpoint, alert scan) to reduce extra load.
- **Normal** → full speed; it resumes automatically when the host recovers.
- The one-off direct scan raises its OOM score to make it a more likely victim
  during memory exhaustion. The long-lived systemd daemon keeps the default OOM
  score; neither setting guarantees which process the kernel will kill.

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

### New in 0.2.0

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
  - **Low-disk protection** — pauses index writes after observing less than
    256 MiB free on the index filesystem. In-flight transactions and other
    writers can still consume remaining space.
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

### Install

The packages ship a **static musl binary** with no shared-library dependencies,
so they run on x86-64 distributions regardless of host glibc. The live fanotify
daemon requires Linux 5.17+ or a vendor backport of `FAN_REPORT_TARGET_FID`, plus
the capabilities granted by its systemd unit. Each URL below always points to
the newest release — no version to look up.

**Debian / Ubuntu**
```bash
curl -L -o dux-latest.deb https://github.com/ftahirops/dux/releases/latest/download/dux-latest.deb
sudo dpkg -i dux-latest.deb
```

**RHEL / Fedora / Rocky / openSUSE**
```bash
curl -L -o dux-latest.rpm https://github.com/ftahirops/dux/releases/latest/download/dux-latest.rpm
sudo rpm -i dux-latest.rpm
```

**Standalone binary** (no package manager, any distro)
```bash
curl -L -o dux https://github.com/ftahirops/dux/releases/latest/download/dux-latest-linux-static
sudo install -m755 dux /usr/local/bin/dux
```

The `.deb`/`.rpm` install `/usr/bin/dux` plus a systemd unit that runs the first
full scan **in the background** (watch it with `dux status`) and then keeps the
index live. So after install you can go straight to `dux`. (Browse every download
on the [latest release page](https://github.com/ftahirops/dux/releases/latest).)

**From source** (Rust toolchain):
```bash
cargo build --release
sudo install -m755 target/release/dux /usr/local/bin/dux
```

### 60-second start

```bash
# index once, then everything is instant
sudo dux scan /
dux                       # five views: Overview · Explore · Activity · Reclaim · Large Files

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

#### Automate & integrate (SRE/DevOps)

```bash
# JSON on every read command → pipe to jq / dashboards
dux top --dirs --json | jq '.[] | {path, bytes}'
dux large --files --explain --json | jq '.[].classification'
dux explain /var/log/auth.log.1 --json

# Prometheus metrics for the node_exporter textfile collector
dux metrics --top 0 > /var/lib/node_exporter/textfile_collector/dux.prom.tmp \
  && mv /var/lib/node_exporter/textfile_collector/dux.prom.tmp \
        /var/lib/node_exporter/textfile_collector/dux.prom
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

#### Run it live (systemd)

```bash
sudo cp packaging/dux.service /etc/systemd/system/
sudo systemctl enable --now dux.service  # initial scan, then realtime daemon
```

For an alert hook on the systemd daemon, run `sudo systemctl edit dux.service`
and add this override:

```ini
[Service]
ExecStart=
ExecStart=/usr/bin/dux daemon / --alert-threshold 1G --alert-window 10m --alert-exec /path/hook.sh
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
```

Then apply the override:

```bash
sudo systemctl daemon-reload
sudo systemctl restart dux.service
```

Alert hooks run as root for compatibility, but their Linux capabilities are
cleared before execution by default. The packaged service keeps only the two
capabilities Dux needs and locks root capability regain off. To let a reviewed
hook inherit those capabilities, add `--alert-exec-keep-capabilities`.
The default systemd unit restricts the daemon to Unix sockets. Expand its
`RestrictAddressFamilies` setting only when a configured alert hook needs IP
network access; it applies to the hook child as well as the daemon. Configure
alerts with a systemd `ExecStart` override so the existing daemon owns the index.
Alert evaluation is skipped while the host pressure guard is Elevated or
Critical. Treat hooks as best-effort notifications: sustained host pressure can
delay or miss a threshold check, so they are not an emergency disk-full channel.

The daemon coalesces repeated changes by path and flushes bounded batches; normal
event updates restat changed paths, while reconciliation scans may traverse the
tree. The watcher CPU governor targets an average duty cycle for its event-loop
thread, not the whole process, and short bursts can exceed that target. The
packaged systemd unit caps aggregate daemon CPU at 100% of one core and assigns
a low CPU weight under contention. Scheduling priorities reduce resource share
but do not eliminate contention or guarantee freshness under sustained load.
`dux status` shows whether it is caught up.

#### Who can read the index

The index lists **every filename on `/`** — including names inside directories a
user could not otherwise traverse. It exposes no file *contents*, but reading it
does bypass directory permissions, so the packaged service keeps it at **`0640
dux:dux`**, not world-readable.

```bash
sudo usermod -aG dux alice     # alice can now run `dux find` / `dux status`
```

Everyone else (and any unprivileged tooling) must go through `sudo` or a
privileged wrapper. To lock it down to root-only, or to restore the old
world-readable behaviour, see the `index exposure` notes at the bottom of
`packaging/dux.service`.

#### Index states — and when to rescan

`dux` never silently claims exact data. When it cannot guarantee the index
matches the filesystem it says so, and `dux status` (and the TUI header) surface
the state:

| State | What it means | What to do |
|---|---|---|
| *(none shown)* | Index is live and believed exact. | Nothing. |
| **DIRTY** | Events were missed, scan coverage is incomplete, or totals failed verification. | Reconciliation runs automatically with a five-minute cooldown when coverage permits. Use `sudo dux scan /` to request it immediately; fix persistent capability, mount, or permission problems. |
| **WRITES PAUSED** (*reason*) | Deliberate self-protection: low memory, low disk, high load, or kernel PSI pressure. The daemon stopped writing and shrank its caches to protect the host. Events keep coalescing in memory. | Nothing — it resumes automatically. Fix the underlying pressure. If it persists under sustained churn the backlog cap trips and the index goes **DIRTY**. |
| **BEHIND** | The pipeline has not fully caught up recently. This measures time since catch-up, not the precise age of each indexed value. | Check pending updates, pause reason, and write errors. CPU budget, event volume, storage latency, and failed writes can all contribute. |

```bash
dux status              # index state, staleness, daemon liveness
dux status --json       # same, for monitoring
```

A live heartbeat means the process is responding; it does not guarantee an exact
index. Check pipeline state and warnings. Persistent DIRTY, write errors, or
refresh failures need investigation even if the process is live.

#### Startup and alert response

On first install or after a restart, the daemon attaches filesystem watches
before it builds the baseline. Queries continue to use the last complete index
while the scan runs; `scan_in_progress` and the scan timestamp distinguish this
normal startup work from a caught-up index.

For an initial alert policy, after the first baseline completes, page
immediately when `dux_index_dirty` is 1 outside a declared scan. During startup,
alert separately if the scan exceeds the expected duration for the indexed
tree. Alert when `dux_daemon_up` is 0, `dux_watch_status_available` is 0 for
more than two monitoring intervals, or the metrics file is missing/stale. Warn
when `dux_index_caught_up` stays 0 for five minutes outside a full scan, or when
`dux_pending_updates` exceeds 80% of its 500,000-entry cap. `dux_up` only
describes a readable index when a metrics scrape succeeds; it is not a
replacement for monitoring scrape failures or file freshness. These are
starting thresholds; tune them against the host's workload and scan duration
before treating them as service-level objectives.

```bash
sudo systemctl status dux --no-pager
sudo journalctl -u dux --since -15m --no-pager
dux status --json
dux metrics --top 0
```

If the service is alive but DIRTY, inspect the journal for missed events,
unreadable paths, capability errors, mount changes, or SQLite failures. Restore
the required capabilities and filesystem availability first, then request a
reconciliation with `sudo dux scan /`; verify that `dirty` clears and the event
pipeline drains. WRITES PAUSED is deliberate resource protection: relieve the
reported memory, disk, load, or PSI pressure and let the daemon resume. If a
refresh fails, the previous complete generation remains queryable; fix the
reported storage or coverage issue before retrying. A readable index without a
fresh watch-status record is a static snapshot, not a live index.

#### Controlled upgrade and rollback

For a production canary, keep the exact previous package and binary hash until
the canary closes. Record the service-enabled state and
`sha256sum /usr/bin/dux`, stop the service cleanly, and make a verified SQLite
online backup of `/var/lib/dux/dux.db`. Store it with mode `0600` in a root-only
directory outside the package state directory, then verify it with
`PRAGMA quick_check`. Do not use a plain copy of a live database and its WAL
files as a rollback snapshot.

After installing the candidate, check `dux status --json`, the service journal,
`dux metrics --top 0`, and indexed totals. Keep the canary under observation for
the agreed window and retain those outputs with the package and binary hashes.
A live daemon or a drained queue alone does not prove correctness; the index
must also be clean and pass reconciliation checks.

If rollback is needed, stop the service first and restore the verified database
snapshot before installing or starting the previous package. This ordering
matters when the candidate has migrated the schema: an older binary may not be
able to open the upgraded database. Reinstall the retained previous package,
restore the recorded service-enabled state, start the service, and verify its
binary hash and health. The restored snapshot predates the canary window, so
request a full scan and confirm the index is clean and caught up before
trusting it. Keep the rollback snapshot until that recovery is complete.

A `dux scan` is also warranted after bulk changes made while the daemon was
stopped, and is safe to run any time — rebuilds are transactional: the old index stays readable until the completed
replacement commits. Existing readers keep their SQLite snapshot.

---

### Trust it: independent verification

`dux` ships with an audit harness that checks the index against ground truth
(`du`/`df`/`find`) and its own internal consistency, and can auto-reconcile:

```bash
sudo scripts/dux-verify.sh audit          # integrity + ground-truth cross-checks
sudo DUX_RECONCILE=1 scripts/dux-verify.sh install-cron   # verify every 3h, self-heal
```

`install-cron` copies the verifier to `/usr/local/sbin/dux-verify`, installs a
fixed root cron entry, and sends output to syslog/journal under the `dux-verify`
tag. `DUX_RECONCILE=1` is an explicit opt-in: on a failed audit, the scheduled
job stops the daemon, performs a full scan, then restarts it. Without it, the
job reports findings but does not reconcile.

---

### How it works

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
