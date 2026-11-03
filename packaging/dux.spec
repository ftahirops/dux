Name:           dux
Version:        0.6.3
Release:        17%{?dist}
Summary:        Persistent realtime disk usage + file search (du/ncdu/locate, indexed & live)

License:        MIT
URL:            https://github.com/ftahirops/dux
BuildArch:      x86_64
# The shipped binary is a static musl build with NO shared-library deps, so it
# runs on any x86_64 Linux regardless of the host glibc. Don't let rpm synthesize
# a glibc requirement from the binary.
AutoReqProv:    no
# The static-musl binary is already stripped and is an identity-bearing
# release artifact. Do not let rpmbuild's generic brp-strip rewrite its bytes.
%global __os_install_post %{nil}
# %pre runs groupadd to create the `dux` group that gates index access.
# AutoReqProv is off, so this dependency must be declared explicitly.
Requires(pre):  shadow-utils
Requires:       python3

%description
dux is an indexed, persistent du/ncdu with fast trigram file-name search and an
optional live fanotify daemon. It answers "largest dirs/files", "fastest-growing
paths", inode-usage, and deleted-but-open queries in milliseconds, shows df-style
capacity, and ships an integrity-audit harness. Companion to xtop.

# binary is prebuilt and provided as a SOURCE; no compilation in the rpm.
%global debug_package %{nil}

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{_bindir} %{buildroot}/usr/lib/dux %{buildroot}/usr/lib/systemd/system %{buildroot}%{_docdir}/dux
mkdir -p %{buildroot}/var/lib/dux
install -m0755 %{_sourcedir}/dux        %{buildroot}%{_bindir}/dux
install -m0755 %{_sourcedir}/secure-index-permissions.sh %{buildroot}/usr/lib/dux/secure-index-permissions.sh
install -m0755 %{_sourcedir}/dux-backup-index.py %{buildroot}/usr/lib/dux/dux-backup-index.py
install -m0644 %{_sourcedir}/dux.service %{buildroot}/usr/lib/systemd/system/dux.service
install -m0644 %{_sourcedir}/dux-tunnel.service %{buildroot}/usr/lib/systemd/system/dux-tunnel.service
install -m0644 %{_sourcedir}/README.md  %{buildroot}%{_docdir}/dux/README.md

%files
%{_bindir}/dux
/usr/lib/dux/secure-index-permissions.sh
/usr/lib/dux/dux-backup-index.py
/usr/lib/systemd/system/dux.service
/usr/lib/systemd/system/dux-tunnel.service
%doc %{_docdir}/dux/README.md
# The index lists every filename on /, so the state dir is root:dux 0750, not 0755.
%dir %attr(0750,root,dux) /var/lib/dux

%pre
# The `dux` group gates read access to the index. Created in %pre so it exists
# before %files ownership is applied and before the unit starts.
if ! getent group dux >/dev/null 2>&1; then
    groupadd -r dux >/dev/null
fi
exit 0

%post
mkdir -p /var/lib/dux
/usr/lib/dux/secure-index-permissions.sh /var/lib/dux dux
# Optional dux-tunnel.service is never enabled or configured by installation.
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    if [ "$1" = "1" ]; then
        systemctl enable dux.service
    fi
    # --no-block so the first-start full scan runs in the BACKGROUND and does not
    # block rpm/dnf for the whole scan. Progress is observable via `dux status`.
    echo "dux: starting service — the initial scan of / runs in the BACKGROUND."
    if [ "$1" = "1" ]; then
        systemctl start --no-block dux.service
    else
        systemctl try-restart --no-block dux.service
        systemctl try-restart --no-block dux-tunnel.service
    fi
    echo "dux: watch scan progress with:  dux status   (or: journalctl -u dux -f)"
fi
echo "dux: the index is readable by root and the 'dux' group (it lists every"
echo "dux: filename on /). Grant a user access with: usermod -aG dux <user>"
exit 0

%preun
# $1 == 0 on final removal (not upgrade)
if [ "$1" = "0" ] && [ -d /run/systemd/system ]; then
    systemctl stop dux.service dux-tunnel.service || true
    systemctl disable dux.service dux-tunnel.service || true
fi
exit 0

%postun
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
exit 0

%changelog
* Fri Oct 23 2026 dux maintainers <root@localhost> - 0.6.3-8
- Verify full directory accounting before reporting reconciliation caught up

* Fri Oct 23 2026 dux maintainers <root@localhost> - 0.6.3-6
- Keep fanotify batches moving across concurrent directory renames

* Fri Oct 23 2026 dux maintainers <root@localhost> - 0.6.3-5
- Optimize full-retained-history growth aggregation

* Fri Oct 23 2026 dux maintainers <root@localhost> - 0.6.3-4
- Fix unit documentation and restrict the optional tunnel's address families

* Fri Oct 23 2026 dux maintainers <root@localhost> - 0.6.3-3
- Keep freshness and systemd status truthful across background reconciliation
  and pending event replay.

* Thu Oct 22 2026 dux maintainers <root@localhost> - 0.6.3-2
- Correct combined fanotify CREATE|DELETE masks by reconciling the final path
  state; preserve exact hardlink paths through rapid replacement churn.

* Thu Oct 22 2026 dux maintainers <root@localhost> - 0.6.3-1
- Fix inode-incarnation and nested rename accounting drift, and start watches
  before the baseline scan so changes during first install and upgrades replay.
- Bound event flush work, publish uncertainty promptly, improve query responsiveness,
  separate optional remote access, and pin/audit production dependencies.

* Sun Aug 30 2026 dux maintainers <root@localhost> - 0.6.0-1
- Rebuilt the TUI around four prominent, numbered destinations: Overview,
  Explore, Activity, and Reclaim, with responsive layouts down to 80x24.
- Overview now ranks five largest files, fastest-growing paths, and reclaim
  candidates, including workload/application relationships rather than an
  unexplained path alone.
- Added indexed CLI dashboards and aliases: overview, large, fastest-growth,
  activity, docker, and explain; detailed file reports include owner, type,
  purpose, importance, and conservative deletion guidance.
- Bounded the initial scan with a fixed queue and disk-backed staging reduction;
  added explicit pipeline depth, catch-up lag, update, and drop telemetry.
- Hardened the always-on daemon with systemd watchdog notification,
  Restart=always, watch-first startup, and automatic low-priority reconciliation.
- Fixed Ctrl-C handling, dux indexing its own database/WAL, duplicate pseudo and
  overlay filesystem views, crowded navigation, one-row overview cards, and
  unbounded work during large filesystem event bursts.

* Mon Jul 13 2026 dux maintainers <root@localhost> - 0.5.2-1
- Daemon CPU/I/O controls reduce its resource share under contention. The
  watcher thread targets a CPU duty cycle (`daemon --max-cpu`, default 25% of
  one core); the systemd unit adds a 100% of one core CPU ceiling and low CPU/I/O
  scheduling weights. Short bursts and work on other threads still consume host
  resources.
- Event batches are bounded and queued in fanotify's finite kernel queue. If
  event rates exceed throughput, the queue can overflow, mark the index
  uncertain, and require reconciliation. Sustained pressure can delay recovery;
  status and TUI expose lag and uncertainty.
- Idle activity is typically near 0% CPU; measured rates depend on the host,
  event stream, and resource limits.

* Sun Jul 12 2026 dux maintainers <root@localhost> - 0.5.1-1
- Perf (daemon CPU): born-and-died event coalescing. A file created AND deleted
  within the same flush window (build temp files, editor swap files — e.g. a Go
  build churning thousands of /tmp/go-build* entries) is now dropped before any
  lstat/DB work, instead of doing an lstat-per-event. Collapses a high-churn /tmp
  storm from ~8% steady CPU (90% spikes) to near-zero — automatically, no config,
  with exact correctness preserved (persistent creates still index; real deletes
  still apply; totals match a fresh scan).
- Perf: `dux du -s` (summarize) now does a single index lookup instead of
  enumerating the whole subtree, so `du -sh /home` and friends return instantly
  on large trees (was a timeout). Byte-exact vs GNU du unchanged.
- Docs: version-agnostic install commands (resolve the newest release via the
  GitHub API) so the instructions never go stale between versions.

* Wed Jul 08 2026 dux maintainers <root@localhost> - 0.5.0-1
- New SRE/DevOps commands (all served from the index — no fs re-walk, no daemon
  cost):
  * `--json` on every read command (top/find/growth/by-owner/by-ext/deleted-open/
    diff/du/containers) for jq + automation.
  * `dux metrics` — Prometheus text-exposition output for the node_exporter
    textfile collector (fs bytes/inodes, index size, daemon_up, last-scan ts,
    dux_path_bytes{path=…}); label values are injection-escaped.
  * `dux diff --since <win>` (alias `since`) — net per-path change over a window,
    ranked by magnitude (surfaces both fills and frees).
  * `dux du` — byte-exact du-compatible output from the index (-s/-a/-h/-m/
    --max-depth), verified block-for-block against GNU du.
  * `dux containers` — per-container writable-layer + log + volume usage for
    Docker and Podman, resolved from on-disk metadata (no socket/CLI). Writable
    layer read from the running container's overlay upperdir via
    /proc/<pid>/mountinfo — works with the classic overlay2 driver AND the
    containerd snapshotter (Docker's default image store since v25).
- Portability: ship a STATIC musl binary (no shared-lib deps) so the .deb/.rpm run
  on any x86_64 Linux regardless of host glibc (0.4.4 hard-pinned GLIBC_2.39 and
  failed on Debian 12 / RHEL 9 / Ubuntu 22.04). Reproducible via
  scripts/build-release.sh.
- Production hardening (audit: races, leaks, unsafe/FFI, crash-safety, fuzz):
  * fix a fuzz-found panic in parse_size on multibyte input.
  * daemon flags the index dirty on resume after a crash/downtime gap (missed
    events are no longer silent); a post-crash `dux scan` verifies the heartbeat
    PID is alive so it reconciles directly instead of failing.
  * hardlink counter drift fixed (modify via a non-prime link no longer drives
    totals negative); MAX(0,…) floor on ancestor totals.
  * WAL checkpoints under Elevated pressure (+256 MiB backstop) — no unbounded
    -wal on a busy host; alert children reaped every loop.
  * in-place SIGHUP rescan closes the writer connection before the atomic swap;
    the TUI reopens on the inode swap and no longer pins the old deleted index.
  * fanotify parser hardening (compile-time struct-size assert, info-record
    offset floor); poison-tolerant scan cycle-guard lock.
- Verified: 21 unit tests, thousands of fuzz inputs (0 panics), repeated SIGKILL
  mid-flush (integrity ok + exact reconcile), and a churn soak (no RSS/fd leak).

* Fri Jul 03 2026 dux maintainers <root@localhost> - 0.4.4-1
- New TUI "Apps/OS Heatmap" panel: groups disk usage by application/OS profile
  (OS, Docker, nginx, Apache, Postgres, MySQL, Redis, Elasticsearch, Mongo,
  journald, logs, cache, users), with docker data-root autodetection.
- Index schema-version guard: an incompatible index is rejected with a clear
  rebuild hint instead of misreading it; path_of no longer panics on an
  unresolved inode.
- Correctness fixes:
  * find --name with a literal '[' no longer silently returns nothing.
  * scan terminates on directory cycles (bind mounts / dir hardlinks) instead
    of looping until OOM; per-directory sizes are now deterministic across runs
    for cross-directory hardlinks.
  * daemon: rename pairing is rollback-safe; non-UTF-8 filenames in live events
    are handled by raw bytes (no dropped deletes / index drift); mount fds are
    closed and the shutdown drain retries on EINTR.
  * deleted-open no longer false-positives on files literally named "(deleted)".
  * parse_size accepts scientific notation; TUI groups panel scrolls its
    selection into view; manual 'r' refresh no longer blocks the UI on a hung
    mount; a few smaller guards.
- Perf: statfs only at mount boundaries during scan (far fewer syscalls on deep
  trees).

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.4.3-1
- O(1) status node count, lower idle TUI CPU, leaner rescan polling, and a
  configurable growth-history retention (--growth-days) to shrink the index on
  high-churn hosts.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.4.2-1
- TUI opens instantly on huge high-churn indexes (bounded growth-heat query,
  async startup, slower worker cadence) — was ~30s to first paint.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.4.1-1
- Throttled background scans (--low-priority caps threads; new --jobs N); the
  service + daemon scans run gently by default.
- Live scan progress in `dux status` (and a clear message before the first
  index exists); status no longer does the ~18s dbstat walk.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.4.0-1
- TUI: background-thread refresh so navigation never blocks on large indexes.
- Correct hardlink deletes: a file hardlinked outside a deleted directory
  survives, with exact totals (matches a fresh scan).

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.3.1-1
- Verification-audit follow-up: capability self-check (warns + marks dirty if
  CAP_DAC_READ_SEARCH is missing); alert-exec children no longer inherit the
  OOM-victim boost; deferred-rename expiry is commit-safe; pause state recovers
  while idle; footer ellipsis is display-width aware; poll error handling.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.3.0-1
- Resource guardian: the daemon self-throttles under host pressure (low RAM,
  low disk, high load, kernel PSI) — pauses its own writes when Critical, drops
  optional work when Elevated. The current systemd daemon keeps the default OOM
  score; only one-off direct scans receive an OOM-victim preference.
- Perf: poll(2) daemon loop (~0.5 idle wakeups/s), set-based subtree delete,
  unicode-width TUI columns, per-row path cache.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.2.2-1
- Code-review pass: pair renames split across a flush boundary (no vanished
  subtree / spurious growth); proper dirty-state FSM (self-clearing low-disk
  pause vs lossy DIRTY, cleared by rescan); mark_fs byte-safe; TUI guards
  (restore-guard ordering, empty-row indexing) + per-row path cache; growth
  --since honors the window; --ext is literal; prepare_cached children query.

* Fri Jun 26 2026 dux maintainers <root@localhost> - 0.2.1-1
- UX: `dux scan` against a live daemon now triggers an in-place atomic rescan
  (SIGHUP) instead of telling the user to stop/start by hand; no downtime.
- TUI: stop labeling a live index "N old" (it's maintained in realtime); the
  age-as-staleness text only shows for an off-daemon snapshot.
- Daemon drains the fanotify queue on SIGTERM before the final flush.

* Fri Jun 26 2026 dux maintainers <root@localhost> - 0.2.0-1
- Schema v4: separate inode/dirent tables; hardlink-aware search (every path
  findable, inode counted once); raw-byte (non-UTF-8) filename support.
- Daemon: exclusive per-db lock, graceful SIGTERM flush, root validation,
  low-disk write protection, bounded event backlog + alert workers, FAN_ATTRIB.
- Terminal-safe output (control-char escaping); operational DIRTY state in
  status/TUI; tightened systemd hardening; SECURITY.md.

* Wed Jun 24 2026 dux maintainers <root@localhost> - 0.1.0-1
- Initial RPM package.
