# dux — Product Specification

Version: 1.0 · 2026-11-28 · Status: for owner approval

dux tells you, on a Linux server, when a disk is filling, where the space went,
who is writing it, and how to free it safely. It answers those questions in
seconds and never damages the system it protects.

Requirement levels: **MUST** blocks a release; **SHOULD** does not.
Items marked **[decided]** come from the owner. Items marked **[proposed]** are
my defaults and need confirmation (all are listed again in section 11).

---

## 1. Users and goals

dux is a **public, source-available tool** [decided]: anyone may install it on
any supported Linux server for non-commercial use. Defaults must be safe for a
stranger who has read nothing.

**License** [decided]: from the next release, PolyForm Noncommercial 1.0.0. Free
for personal and non-commercial use; any commercial use needs the owner's
written permission. Releases up to v0.8.4 remain under MIT (cannot be
withdrawn). The project keeps the name **dux** [decided].

| User | Primary need |
| --- | --- |
| Server owner / SRE on call | An alert before a disk is full, naming the folder and the process |
| Developer on a shared server | Which projects, caches, containers and logs are big, and a safe way to clean them |
| Automation (scripts, Prometheus) | The same data as JSON and metrics, through a stable API |
| Security / audit | What dux deleted, when, why; space held by deleted-but-open files |

Problems dux must solve:

1. A disk fills and nobody notices until a service fails.
2. Space disappears and nobody can say which folder, container or log took it.
3. Something writes fast and nobody can say which process or file.
4. Cleanup is risky: people delete a live log, a database or Docker's data.

## 2. Scope

| In scope | Out of scope |
| --- | --- |
| Free space and inode monitoring of every mount, alerts | A metrics database or long-term history UI |
| Folder sizes, biggest files, logs, Docker/Podman, projects, live I/O | A per-file index of the whole disk (the 0.6 design) [decided: code deleted] |
| Rated, previewed, confirmed cleanup; never-touch list; audit log | Reverse-SSH tunnel [decided: code deleted] |
| Optional automatic cleanup, off by default, limited to user-approved folders | Any deletion the user did not approve, ever |
| CLI reports, terminal UI, read-only web dashboard, read-only API | Changes through the web dashboard or the API [decided: read-only] |
| Central fleet dashboard | — (not now; possible later) |

## 3. Platforms and packaging [decided]

| Item | Requirement |
| --- | --- |
| PKG-1 MUST | `.deb` for Ubuntu 24.04 and Debian 12 and newer [decided]; older releases use the static binary |
| PKG-2 MUST | `.rpm` for RHEL/Rocky/Alma 9 and newer, Fedora (current) [decided]; older releases use the static binary |
| PKG-3 MUST | One static Linux binary per architecture: runs on any Linux, any libc, any age (the support floor) [decided] |
| PKG-4 MUST | x86_64 and arm64 for every package and binary |
| PKG-5 MUST | One-line install script that detects distro and architecture and installs the right package, with checksum verification |
| PKG-6 MUST | Install, upgrade (from every earlier public release), downgrade, remove and purge work; purge leaves nothing in /etc/dux or /var/lib/dux |
| PKG-7 MUST | Packages have no hard dependency beyond the base system; curl is recommended (for network alerts) |
| PKG-8 SHOULD | Signed checksums for every release asset |

## 4. Functional requirements

### 4.1 Monitoring and alerts (service: `dux-sentinel`)

| ID | Requirement |
| --- | --- |
| MON-1 MUST | Check free space and inodes on every real mount every 60 s, and every 5 s on a disk past its warning level (adaptive) [decided] |
| MON-2 MUST | Warning at 85 % and critical at 95 % used (space or inodes), configurable; recovery after 3 points of hysteresis [decided: keep defaults] |
| MON-3 MUST | A mount that does not answer (dead NFS/CIFS) is reported "not responding" within 3 s and never delays checks of other mounts |
| MON-4 MUST | Each alert names, when known, the folder that grew and the process writing |
| MON-5 MUST | Channels: system log, ntfy, Slack, generic webhook, email (TLS only), terminal broadcast |
| MON-6 MUST | Terminal broadcast only when a disk is critically full right now (97 %), at most 4 per hour, never for a projection |
| MON-7 SHOULD | Fill-time projection and burst alerts, off by default |
| MON-8 SHOULD | Health probes: filesystem errors, RAID degraded, disk latency, I/O and memory pressure, SMART |
| MON-9 MUST | A failing or hanging channel never delays other channels; every failure is logged |
| MON-10 MUST | No repeats: alert once on crossing a level, again only after +5 points (at most hourly), once on recovery (3 points below, held 2 min); at most 30 alerts per hour in total [decided] |

### 4.2 CLI reports

General rules are in section 6. Every report supports `--json`.

| ID | Command | What it must do |
| --- | --- | --- |
| CLI-1 MUST | `dux` | Open the terminal UI on `/` (or the given path) |
| CLI-2 MUST | `dux biggest [PATH]` | Top 10 biggest files and top 10 folders; readable names for container logs, image layers, swap; `-n`, `--files`, `--folders`, `-l` |
| CLI-3 MUST | `dux where [PATH]` | Biggest sub-folders of **any** folder and what grew under it, instantly, from the last snapshot |
| CLI-4 MUST | `dux doctor [PATH]` | One health report: biggest folders, known culprits with their fix, deleted-but-open files, top writers |
| CLI-5 MUST | `dux logs [PATH]` | Every log grouped by owner (OS, journal, web server, database, container, pm2, .NET, app), size, writer, growth |
| CLI-6 MUST | `dux docker` | Docker and Podman: stacks, images, containers, volumes, build cache, what is inside each container's writable layer |
| CLI-7 MUST | `dux projects [PATH]` (was `pp`) | Projects and apps: type, source, build output, caches (incl. Go and Rust caches tied to projects), git |
| CLI-8 MUST | `dux io` | Live I/O: per disk, per process, per file being written or read |
| CLI-9 MUST | `dux fix [--level 1-3]` | Every cleanup suggestion rated SAFE / CAUTION / RISKY, pick and apply; `--level` replaces the old `dux cleanup` |
| CLI-10 MUST | `dux actions off` / `dux actions on` | Emergency brake: refuse every change until turned back on |
| CLI-11 MUST | `dux never-touch add\|remove\|list` | Manage paths dux must never clean |
| CLI-12 SHOULD | `dux leaks` | Deleted files still held open by a process |
| CLI-13 MUST | `dux notify-test` | Send a test alert through every configured channel |
| CLI-14 MUST | `dux setup` | Guided setup (section 4.7) |
| CLI-15 MUST | `dux web` | Read-only dashboard and API (section 4.6) |
| CLI-16 MUST | Removed | scan, overview, top, find, growth, diff, du, metrics, containers, by-owner, by-ext, explain, status, daemon, tunnel, tui (code deleted), walk (merged into biggest), cleanup (merged into fix) [decided] |

### 4.3 Terminal UI

| ID | Requirement |
| --- | --- |
| UI-1 MUST | Views: 1 Disk usage (folder browser on /, disk selector on top, read-only), 2 Activity, 3 Usage by group (Docker, Projects, Services, Logs, each in detail), 4 Cleanup (what can be cleaned, summed per group), 5 History [decided] |
| UI-2 MUST | htop-style key bar (F-keys with names), mouse support, lists biggest first, each list fits one screen with pages; never-touch can be added and removed everywhere [decided] |
| UI-3 MUST | Every action shows exactly what it will do, asks to confirm (RISKY: typed confirmation), runs with live progress, reports space measured as freed |
| UI-4 MUST | Responsive with 1,000,000 entries in one folder and 1,000 containers |
| UI-5 MUST | Renders without crashing at any terminal size |
| UI-6 MUST | A confirmation is bound to the exact item shown; a rescan can never change its target |

### 4.4 Cleanup (manual)

| ID | Requirement |
| --- | --- |
| CLN-1 MUST | Suggestions are rated SAFE (nothing of value lost), CAUTION (recoverable), RISKY (can lose data) |
| CLN-2 MUST | Log sources analysed everywhere: system, journal, Docker container logs, pm2, .NET, web servers, databases, app logs; offer "truncate all possible" with the list shown and the user asked [decided] |
| CLN-3 MUST | Build and tool caches: node_modules, target/, Go build and module cache, Rust/cargo caches, pip, npm, .cache, tied to the projects that use them [decided] |
| CLN-4 MUST | Docker: unused volumes and images not related to any container, offered for cleanup; volumes are RISKY |
| CLN-5 MUST | Truncating a log keeps its last part aside (owner and mode preserved) and empties it in place, never deletes it |

### 4.5 Automatic cleanup [decided]

| ID | Requirement |
| --- | --- |
| AUTO-1 MUST | Off by default. Never enabled by an install, upgrade or any default |
| AUTO-2 MUST | Enabled only by the user, through `dux setup` or the config file |
| AUTO-3 MUST | When enabling, the user approves the folders it may touch: dux offers suggestions (found logs, caches, projects) and the user adds custom folders |
| AUTO-4 MUST | Allowed actions, each approved separately: remove rotated logs, shrink the journal, truncate big active logs, remove build caches |
| AUTO-5 MUST | Docker unused images and unused volumes: suggested only, unless the user explicitly approved automatic removal of each kind in setup |
| AUTO-6 MUST | Trigger level set by the user, default start at 90 % used. No stop level by default: once triggered it frees as much as the approved safe actions allow; the user may set a stop level [decided] |
| AUTO-7 MUST | Every automatic action is logged and alerted (what, where, how much freed) |
| AUTO-8 MUST | Never runs in `--dry-run`; never touches never-touch paths or anything outside approved folders |
| AUTO-10 MUST | Project build caches: an alert lists what will be deleted 24 h before; the user can keep or never-touch them [decided] |
| AUTO-9 MUST | Logs it truncated or whose copies it removed stay watched: if one grows back fast, dux alerts and names the writer [decided] |

### 4.6 Web dashboard and API [decided]

| ID | Requirement |
| --- | --- |
| API-1 MUST | Read-only: no endpoint changes anything on the server |
| API-2 MUST | Listens on 127.0.0.1 by default; another address only by config; every request needs a token |
| API-3 MUST | Versioned JSON endpoints `/api/v1/...`: disks, folders, biggest, logs, docker, projects, fixes (as data), history, health |
| API-4 MUST | The JSON of each endpoint equals the matching CLI `--json` output |
| API-5 MUST | `/metrics` in Prometheus text format: per-mount used/free/inodes, growth rate, active alerts, sentinel health |
| API-6 MUST | The dashboard shows the same views as the terminal UI, read-only |
| API-7 SHOULD | Documented in an OpenAPI file shipped with the package |
| API-8 MUST | Folder history over time from the snapshots: `/api/v1/history/folders` (sizes per top folder per snapshot) and per-folder growth gauges in `/metrics` [decided] |

### 4.7 Setup and configuration [decided: both]

| ID | Requirement |
| --- | --- |
| CFG-1 MUST | `dux setup` asks for: alert channels (with a test), thresholds, never-touch folders, automatic cleanup (section 4.5); writes the config; can be re-run |
| CFG-2 MUST | One TOML file `/etc/dux/dux.toml`, every setting documented; works without the wizard (Ansible etc.); old `notify.conf` still read; changes apply without restart [decided] |
| CFG-3 MUST | Invalid config is reported with the line and the reason; the service keeps running with the last good config |

## 5. Safety requirements (never broken)

| ID | Requirement |
| --- | --- |
| SAFE-1 | Never-touch list: nothing listed, nothing inside it, nothing containing it is changed |
| SAFE-2 | Fail closed: an unreadable never-touch list blocks every change |
| SAFE-3 | Paths are judged at their real location; a symlinked parent or `..` cannot route around a check |
| SAFE-4 | Never deleted: `/`, top-level folders, system folders, container runtime and database data folders, security logs |
| SAFE-5 | Every check is repeated at the moment of the action, not only when it was planned |
| SAFE-6 | A tree delete never crosses into another filesystem or follows a symlink |
| SAFE-7 | A file held open by a process is not removed |
| SAFE-8 | Every change is written to the audit log (time, action, target, result, space); kept 90 days by default, configurable; shown in the History view |
| SAFE-9 | Kill switch (`dux actions off`, or the file /etc/dux/disable-actions) disables all changes immediately |
| SAFE-10 | Non-root users can read reports; every change says it needs root |

## 6. CLI and output rules

| ID | Rule |
| --- | --- |
| OUT-1 MUST | Short by default: one screen; more only by switch (`-n`, `--all`, `-l`) [decided] |
| OUT-2 MUST | No fix list or prompt unless asked (`--fix`, `--select`, `--apply-*`); one hint line otherwise [decided] |
| OUT-3 MUST | Long paths shortened to the terminal width; readable names for known files |
| OUT-4 MUST | `dux help` lists only working commands, one line each |
| OUT-5 MUST | Every report has `--json`, stable and documented |
| OUT-6 SHOULD | Colour only on a terminal; `NO_COLOR` respected |
| OUT-7 MUST | Exit codes: 0 ok, 1 error, 2 usage error, 3 refused for safety |

## 7. Non-functional requirements

| ID | Requirement |
| --- | --- |
| NFR-1 MUST | Walk 20 million files in under 90 s using under 128 MB memory |
| NFR-2 MUST | Sentinel idle CPU under 0.2 % of one core; memory under 200 MB |
| NFR-3 MUST | Filesystems: ext4, xfs, btrfs (incl. subvolumes), vfat, tmpfs, overlayfs, NFS, bind mounts; sizes equal `du` |
| NFR-4 MUST | No hang on a dead network mount (any command) |
| NFR-5 MUST | Background work at lowest CPU and I/O priority |
| NFR-6 MUST | No crash on malformed input (mountinfo, Docker/Podman JSON, configs, manifests); fuzzed |

## 8. Quality gate for 1.0 [decided]

1. Every MUST requirement has a passing automated or scripted test.
2. 72-hour soak: no memory growth, no restarts, no alert storms.
3. Distro and architecture matrix: install, upgrade, downgrade, remove, purge pass on every supported target.
4. Independent code review (OpenCodeReview or CodeRabbit) with no open critical or high findings.

## 9. Test plan (maps to the gate)

| Area | Covers |
| --- | --- |
| Unit and property tests | Parsers, safety rules, ratings, rendering |
| Destructive-path audit | SAFE-1 to SAFE-10 |
| Failure injection | Full disk, read-only fs, dead NFS, Docker down, unreadable folders |
| Filesystem matrix | NFR-3, NFR-4 |
| Scale | NFR-1, UI-4 |
| Distro matrix | PKG-1 to PKG-7 |
| Alert delivery | MON-5, MON-9 |
| Soak | Gate 2 |
| Coverage report | Safety and cleanup code fully exercised |

## 10. Current state against this spec

Dead code from the 0.6 design is removed (staged, not committed): 10 modules,
16 commands, old packaging, 58 scripts, 0.6 docs. Everything else in this spec
is to be built or checked against its ID; Appendix A records every decision.

## 11. Open questions for the owner

None. Every item in this spec and in Appendix A has an owner decision.

---

## Appendix A — Feature inventory with decisions (2026-11-28)

Every feature, setting and default dux had on 2026-11-27, with the owner's
decision. The old 0.6 index code (watch, scan, query, store, guard, tunnel,
containers, emit, classify, fault) is **deleted** [decided, done].

### A1. Disk monitoring (sentinel)
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A1.1 | Check interval | 2 s | adaptive: every 60 s while healthy, every 5 s on a disk past its warning level |
| A1.2 | Space warning / critical | 85 % / 95 % | keep |
| A1.3 | Inode warning / critical | 85 % / 95 % | keep |
| A1.4 | Recovery hysteresis | 3 points | keep |
| A1.5 | Repeat while above a level | warn 60 min, crit 15 min | never repeat: alert on crossing, on getting worse, on recovery |
| A1.6 | "Got worse" re-alert | +2 points, ≤ every 5 min | +5 points, at most once an hour |
| A1.7 | Hold before recovery | 120 s | keep |
| A1.8 | Ignore filesystems smaller than | 1 GiB | keep |
| A1.9 | Which mounts | all real ones | all real ones by default; user may list exact mounts |
| A1.10 | Fill-time projection alert | off | off by default |
| A1.11 | Burst alert (5 GiB in 10 min) | off | off by default |
| A1.12 | Read-only remount alert | on | keep |
| A1.13 | Mount not responding alert (3 s) | on | keep |
| A1.14 | Max alerts per hour | 30 | keep |
| A1.15 | Same remote share reported once | on | keep |

### A2. Folder attribution and snapshots
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A2.1 | Background walk | hourly, min gap 10 min, 2 threads, lowest priority | keep |
| A2.2 | Extra walk after growth | 1 GiB | keep |
| A2.3 | Snapshots | latest + previous | keep |
| A2.4 | Top-folder history | no limit | 30 days, older lines removed |
| A2.5 | "Who is writing" per service | every 10 s | keep, every 60 s |
| A2.6 | Culprit catalog | 16 entries + per-user caches | keep all; add language caches (Go, cargo, npm/yarn/pnpm, pip, Maven/Gradle), pm2/.NET/app logs, Kubernetes/k3s, database logs (MySQL binlogs, PostgreSQL WAL/pg_log, MongoDB; named, never auto-cleaned) |

### A3. Health probes
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A3.1 | ext4 error counters | 60 s | keep |
| A3.2 | RAID degraded / rebuilding | 60 s | keep |
| A3.3 | Disk latency | 50 / 250 ms | keep |
| A3.4 | I/O pressure | 30 % / 60 % | keep |
| A3.5 | Memory pressure | 20 % / 50 % | keep |
| A3.6 | SMART | hourly | keep |

### A4. Alert channels and routing
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A4.1–A4.5 | System log, ntfy, Slack, webhook, email (TLS) | yes | keep |
| A4.6 | Terminal broadcast | ≥ 97 %, min gap 30 min, max 4/hour | keep |
| A4.7 | Severity to push / email | warning+ / warning+ | keep |
| A4.8 | Recovery messages | everywhere | keep: everywhere the alert went |
| A4.9 | Delivery timeout | 10 s | keep |
| A4.10 | More channels (Telegram, Discord, Teams, PagerDuty) | none | none for 1.0 |

### A5. Automatic cleanup
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A5.1 | Emergency cleanup | level setting, off | replaced by section 4.5 (AUTO-1..10) |

### A6. Cleanup catalog
| ID | Item | Was | Rating | Decision |
| --- | --- | --- | --- | --- |
| A6.1 | Shrink journal | 1 GiB / 30 days | SAFE | keep |
| A6.2 | Rotated logs | keep newest 3 | SAFE | keep |
| A6.3 | Truncate big active log | ≥ 50 MiB, disk ≥ 80 %, keep last 10 MiB | CAUTION | keep |
| A6.4 | Force logrotate | security-log configs excluded | CAUTION | keep |
| A6.5 | apt / dnf cache | ≥ 16 MiB | SAFE | keep |
| A6.6 | systemd-tmpfiles | | SAFE | keep |
| A6.7 | Crash / core dumps | keep 5, older than 14 days | SAFE | keep |
| A6.8 | Docker build cache | unused 7 days | SAFE | keep |
| A6.9 | Dangling images | | SAFE | keep |
| A6.10 | Unused images | older than 30 days | CAUTION | keep |
| A6.11 | Stopped containers | 7 days | RISKY | keep |
| A6.12 | Unused volumes | | RISKY | keep (typed confirmation) |
| A6.13 | Container log truncate | > 10 MiB | CAUTION | keep |
| A6.14 | Project build output | idle 30 days | CAUTION | idle 10 days; in automatic mode an alert 24 h before deletion |
| A6.15 | Delete a biggest file | | RISKY | keep (typed confirmation) |
| A6.16 | Deleted-but-open files | explained | RISKY | keep (explained only) |
| A6.17 | Go/cargo/npm/pip caches, pm2 and .NET logs | — | — | add |

### A7. Logs
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A7.1 | What counts as a log | name/folder rules, ≥ 1 MiB | keep rules; only logs ≥ 10 MiB |
| A7.2 | Groups | OS, journal, web, DB, container, app | add pm2, .NET; app logs grouped under the systemd service that writes them |
| A7.3 | Protected logs | auth, secure, wtmp, btmp, lastlog, faillog, audit | protect these plus database, mail and kernel logs. Only when a disk is ≥ 97 % does dux ASK (never automatic) to rotate/truncate protected logs over 250 MB, shown as a list with sizes |
| A7.4 | Binary logs | never by path | keep |
| A7.5 | Growth sampling | on request | keep |
| A7.6 | Writer of each log | yes | keep |

### A8. Projects
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A8.1 | Types | Node, Rust, Go, Python, PHP, Ruby, Java, .NET, C/C++, Compose, git | keep; add Flutter/Dart, Elixir, Swift, Terraform/Pulumi |
| A8.2 | Build/cache folders | 18 names | keep; add build/_build/.build, vendor (PHP always; Go only when not tracked by git), bin+obj (.NET), .terraform, .pulumi |
| A8.3 | Never a project | /, top-level, temp, system | keep |
| A8.4 | Idle threshold | 30 days | 10 days |
| A8.5 | Details | name, description, branch, remote | keep |
| A8.6 | Grouping | by place | keep |

### A9. Docker and Podman
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A9.1–A9.5 | Views, stack source, links, inside a layer, Podman | built | keep all |
| A9.6 | Kubernetes / k3s | none | not for 1.0 |

### A10. Live I/O
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A10.1–A10.5 | Disk speed, per process, files written, writer details, recent growth | built | keep all |

### A11. Terminal UI
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A11.1 | Views | 8 views, Browse first | 1 Disk usage (folder browser on /, disk selector on top, read-only) · 2 Activity · 3 Usage by group (Docker, Projects, Services, Logs, each in detail) · 4 Cleanup (what can be cleaned, summed per group) · 5 History |
| A11.2 | Keys | single letters | htop-style key bar: F-keys with names, mouse-clickable |
| A11.3 | Confirmation | typed D for RISKY | keep for Cleanup actions |
| A11.4 | Sorting | s/n/C/M | biggest first only |
| A11.5 | Long lists | scroll | each list fits one screen, with pages |
| A11.6 | Mouse | no | add mouse support |
| A11.7 | Never-touch | add only (BUG: no undo) | add and remove everywhere |

### A12. Web dashboard and API
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A12.1 | Views | 8 views | same 5 views as the terminal UI, read-only |
| A12.2 | Actions | --allow-actions | removed |
| A12.3 | Refresh | 15 min | 5 min, or on Refresh |
| A12.4 | Listen / token | 127.0.0.1, token | keep |
| A12.5 | API | none | /api/v1, /metrics, folder history |

### A13. Safety
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A13.1 | Never-touch list | file + add | keep; add removal (undo) everywhere |
| A13.2 | Deleting from the folder browser | allowed with refuse list | no deleting in the browser; changes only in the Cleanup view; refuse list still guards every change |
| A13.3 | Kill switch | file | keep; add `dux actions off` / `dux actions on` |
| A13.4 | Audit log | no rotation | user-configurable; default keep 90 days |

### A14. Commands
| ID | Command | Decision |
| --- | --- | --- |
| A14.1 | walk | remove (merged into biggest) |
| A14.2 | doctor | keep |
| A14.3 | where | keep (any folder) |
| A14.4 | cleanup | remove (levels become `fix --level`) |
| A14.5 | docker | keep |
| A14.6 | biggest | keep (short by default, switches) |
| A14.7 | logs | keep |
| A14.8 | pp | rename to `projects` |
| A14.9 | fix | keep |
| A14.10 | io | keep |
| A14.11 | leaks, never-touch, notify-test, sentinel, ui, web | keep; add `setup`, `actions on/off` |

### A15. Configuration
| ID | Feature | Was | Decision |
| --- | --- | --- | --- |
| A15.1 | Config file | /etc/dux/notify.conf, KEY=VALUE | one TOML file /etc/dux/dux.toml; old file still read |
| A15.2 | Settings shown | 71 keys | all settings in the file, each documented |
| A15.3 | Applying changes | restart | applied on change, no restart |
