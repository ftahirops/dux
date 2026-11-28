# dux: disk-full alerts that say where, who and what to do

dux watches every disk on a Linux server, alerts you when one is filling, and
tells you which folder grew, which process or service is writing it, and the
cleanup that frees the space, rated SAFE, CAUTION or RISKY.

```
dux                 terminal UI: Disk usage, Activity, Usage by group, Cleanup, History
dux setup           guided setup: alerts (with a test), thresholds, never-touch, automatic cleanup
dux doctor /        disk health report, ending with the fixes
dux where /var      where the space is under any folder, and what grew
dux biggest         biggest files and folders (-n 30, --files, --folders, -l)
dux logs            every log by owner, size, growth and writer
dux docker          Docker and Podman: stacks, images, containers, volumes, inside a container
dux projects        projects and apps: source, build output, caches, git
dux fix             every cleanup suggestion, rated; pick and apply (--level 1-3)
dux leaks           deleted files still held open: space you cannot see
dux io              live disk I/O: who writes and reads which files
dux web             read-only web dashboard, JSON API and Prometheus metrics
dux sentinel        the watcher (installed as the dux-sentinel service)
dux auto            automatic cleanup: what is approved, announced and watched
dux actions off     emergency brake: dux refuses every change until `dux actions on`
dux never-touch     paths dux must never change: add, remove, list
```

Reports are short by default; switches show more. Nothing changes on disk
unless you ask (`--fix`, `dux fix`, the Cleanup view) or approve automatic
cleanup. Every read command takes `--json`.

> **Status: 1.0.0 release candidate** on branch `dev/spec-1.0`, built to the
> approved specification [docs/SPEC.md](docs/SPEC.md). The 1.0 gate (a 72-hour
> soak on the final build and arm64 testing) is still open. The latest release
> is [v0.8.4](https://github.com/ftahirops/dux/releases/tag/v0.8.4).

## Install

```
curl -fsSL https://github.com/ftahirops/dux/releases/latest/download/install.sh | sudo sh
```

The script picks the package for your system and CPU (x86_64 or arm64),
checks its SHA-256 against the release's `SHA256SUMS`, and installs it:

| System | Package |
| --- | --- |
| Ubuntu 24.04+, Debian 12+ | `.deb` |
| RHEL, Rocky, Alma 9+, Fedora | `.rpm` |
| anything else (older releases, other distros, any libc) | static binary in `/usr/local/bin` |

Packages need nothing beyond the base system; `curl` is recommended for
network alerts. Install, upgrade, downgrade and remove all work; `apt purge`
(or `rpm -e`) leaves nothing in `/etc/dux` or `/var/lib/dux`.

## Set up

```
sudo dux setup
```

It asks for alert channels and sends a test, then thresholds, never-touch
folders and automatic cleanup. It writes `/etc/dux/dux.toml` (mode 600).
Re-run it any time: every answer defaults to the current value.

Or edit `/etc/dux/dux.toml` yourself (Ansible and the like): every setting is
documented in `/usr/share/dux/dux.toml.example` (`dux setup --print-example`).
Changes apply within seconds, with no restart. A mistake is reported in the
system log with its line and reason, and dux keeps the last good settings. An
old `/etc/dux/notify.conf` is still read; `dux.toml` wins.

## Alerts

The sentinel checks each disk every minute, and every 5 seconds once a disk is
past its warning level (85 % by default; critical at 95 %; inodes too). It
alerts:

- when a disk crosses a level,
- when it is clearly worse (5 more points, at most hourly),
- when it recovers, everywhere the alert went.

There are no reminders unless you set them. Channels: system log, ntfy, Slack,
a webhook (JSON) and email (TLS). At 97 % a broadcast to every logged-in
terminal (`wall`) can be turned on. Alerts include where the space is, what
grew and who writes it. Health checks cover filesystem errors, RAID, disk
latency and SMART.

## Cleanup

`dux fix`, `--fix` on any report, and view 4 of the UI list what can be
cleaned:

| Rating | Meaning | Examples |
| --- | --- | --- |
| SAFE | nothing of value is lost | journal over 1 GiB, rotated logs (newest 3 kept), package caches, old crash dumps, Docker build cache |
| CAUTION | recoverable | big active logs emptied in place (last 10 MiB kept aside), unused images, idle projects' build output |
| RISKY | can lose data; needs a typed confirmation | unused volumes, stopped containers, a biggest file |

Security, database, mail and kernel logs are protected. dux never touches a
never-touch path, never crosses into another filesystem, and never follows a
symlink while deleting.

### Automatic cleanup

Automatic cleanup is off by default and never turned on by an install or
upgrade. If you turn it on, you approve:

- the folders it may touch,
- each action: journal, rotated logs, big active logs, project build output,
  unused Docker images, unused Docker volumes,
- the start level (90 % by default) and an optional stop level.

It runs only on a disk past its start level, does the safest actions first,
and alerts what it did and how much it freed. Project build output is
announced 24 hours before deletion: `dux auto keep PATH` keeps it. Logs it
emptied stay watched; if one grows back fast, dux alerts and names the writer.

## Terminal UI

`dux` opens five views:

1. **Disk usage**: folder browser, biggest first, with a disk selector. Read-only.
2. **Activity**: disk I/O now and who writes.
3. **Usage by group**: Docker, Projects, Services, Logs.
4. **Cleanup**: everything cleanable, summed per group.
5. **History**: every action dux took.

The key bar is htop-style (F1 help, F5 rescan, F10 quit) and clickable. Mouse
works, lists turn pages, and never-touch can be set and unset anywhere (F4).

## Web dashboard, API and metrics

```
dux web --listen 127.0.0.1:8099        # prints the URL with its access token
```

- **Dashboard:** the same five views, read-only, refreshed every 5 minutes.
- **API:** `/api/v1/disks`, `folders`, `biggest`, `logs`, `docker`,
  `projects`, `fixes`, `activity`, `history`, `history/folders` and `health`.
  Each returns the same JSON as the matching CLI `--json`; see
  `/usr/share/doc/dux/openapi.yaml`.
- **Metrics:** `/metrics` in Prometheus format: per-mount size, used, free,
  inodes and growth, active alerts, sentinel health, and folder sizes and
  growth.
- **Access:** every request needs the token, as `X-Dux-Token` or
  `Authorization: Bearer`.

## Platforms

Linux on x86_64 and arm64. Tested on ext4, xfs, btrfs, vfat, tmpfs,
overlayfs, NFS and bind mounts. A dead network mount never hangs dux. The
sentinel runs at the lowest CPU and I/O priority.

## License

dux 1.0 and later: [PolyForm Noncommercial 1.0.0](LICENSE). Free for any
noncommercial use; commercial use needs a license from the author. v0.8.4 and
earlier remain under the MIT license.

History: [CHANGELOG.md](CHANGELOG.md). Specification: [docs/SPEC.md](docs/SPEC.md).
