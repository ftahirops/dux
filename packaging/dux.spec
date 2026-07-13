Name:           dux
Version:        0.5.1
Release:        1%{?dist}
Summary:        Persistent realtime disk usage + file search (du/ncdu/locate, indexed & live)

License:        MIT
URL:            https://github.com/ftahirops/dux
BuildArch:      x86_64
# The shipped binary is a static musl build with NO shared-library deps, so it
# runs on any x86_64 Linux regardless of the host glibc. Don't let rpm synthesize
# a glibc requirement from the binary.
AutoReqProv:    no

%description
dux is an indexed, persistent du/ncdu with fast trigram file-name search and an
optional live fanotify daemon. It answers "largest dirs/files", "fastest-growing
paths", inode-usage, and deleted-but-open queries in milliseconds, shows df-style
capacity, and ships an integrity-audit harness. Companion to xtop.

# binary is prebuilt and provided as a SOURCE; no compilation in the rpm.
%global debug_package %{nil}

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{_bindir} %{buildroot}/usr/lib/systemd/system %{buildroot}%{_docdir}/dux
install -m0755 %{_sourcedir}/dux        %{buildroot}%{_bindir}/dux
install -m0644 %{_sourcedir}/dux.service %{buildroot}/usr/lib/systemd/system/dux.service
install -m0644 %{_sourcedir}/README.md  %{buildroot}%{_docdir}/dux/README.md

%files
%{_bindir}/dux
/usr/lib/systemd/system/dux.service
%doc %{_docdir}/dux/README.md

%post
mkdir -p /var/lib/dux
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    systemctl enable dux.service || true
    echo "dux: starting service (initial scan of / runs now, then the live daemon)…"
    systemctl restart dux.service || true
fi
exit 0

%preun
# $1 == 0 on final removal (not upgrade)
if [ "$1" = "0" ] && [ -d /run/systemd/system ]; then
    systemctl stop dux.service || true
    systemctl disable dux.service || true
fi
exit 0

%postun
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
exit 0

%changelog
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

* Tue Jul 08 2026 dux maintainers <root@localhost> - 0.5.0-1
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
  optional work when Elevated, marks itself the preferred OOM victim.
- Perf: poll(2) daemon loop (~0.5 idle wakeups/s), set-based subtree delete,
  unicode-width TUI columns, per-row path cache.

* Sat Jun 27 2026 dux maintainers <root@localhost> - 0.2.2-1
- Code-review pass: pair renames split across a flush boundary (no vanished
  subtree / spurious growth); proper dirty-state FSM (self-clearing low-disk
  pause vs lossy DIRTY, cleared by rescan); mark_fs byte-safe; TUI guards
  (restore-guard ordering, empty-row indexing) + per-row path cache; growth
  --since honors the window; --ext is literal; prepare_cached children query.

* Thu Jun 26 2026 dux maintainers <root@localhost> - 0.2.1-1
- UX: `dux scan` against a live daemon now triggers an in-place atomic rescan
  (SIGHUP) instead of telling the user to stop/start by hand; no downtime.
- TUI: stop labeling a live index "N old" (it's maintained in realtime); the
  age-as-staleness text only shows for an off-daemon snapshot.
- Daemon drains the fanotify queue on SIGTERM before the final flush.

* Thu Jun 26 2026 dux maintainers <root@localhost> - 0.2.0-1
- Schema v4: separate inode/dirent tables; hardlink-aware search (every path
  findable, inode counted once); raw-byte (non-UTF-8) filename support.
- Daemon: exclusive per-db lock, graceful SIGTERM flush, root validation,
  low-disk write protection, bounded event backlog + alert workers, FAN_ATTRIB.
- Terminal-safe output (control-char escaping); operational DIRTY state in
  status/TUI; tightened systemd hardening; SECURITY.md.

* Wed Jun 24 2026 dux maintainers <root@localhost> - 0.1.0-1
- Initial RPM package.
