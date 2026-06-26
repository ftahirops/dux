Name:           dux
Version:        0.2.1
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
