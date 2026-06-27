Name:           dux
Version:        0.4.1
Release:        1%{?dist}
Summary:        Persistent realtime disk usage + file search (du/ncdu/locate, indexed & live)

License:        MIT
URL:            https://github.com/ftahirops/dux
BuildArch:      x86_64
Requires:       glibc

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
