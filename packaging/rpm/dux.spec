# Built by scripts/build-rpm.sh from the static (musl) binary: no compiler,
# no runtime dependencies beyond the base system (spec PKG-2, PKG-7).
Name:           dux
Version:        %{dux_version}
Release:        %{dux_release}
Summary:        Disk-full alerts, folder attribution and safe cleanup
License:        PolyForm-Noncommercial-1.0.0
URL:            https://github.com/ftahirops/dux
BuildArch:      %{dux_arch}
Recommends:     curl
Recommends:     util-linux
Suggests:       smartmontools
%global debug_package %{nil}
%global __strip /bin/true

%description
dux watches free space and inodes on every disk and alerts through the
system log, ntfy, Slack, a webhook, email and wall: once per level, when
clearly worse, and on recovery. It says which folder grew and who writes it,
finds logs, Docker data, projects and caches, and offers cleanup rated SAFE,
CAUTION or RISKY. Automatic cleanup is off unless you approve it. Includes a
terminal UI and a read-only web dashboard with an API and Prometheus metrics.
Set up with: dux setup

%install
install -Dm755 %{dux_src}/dux %{buildroot}/usr/bin/dux
install -Dm644 %{dux_src}/dux-sentinel.service %{buildroot}/usr/lib/systemd/system/dux-sentinel.service
install -Dm644 %{dux_src}/dux.toml.example %{buildroot}/usr/share/dux/dux.toml.example
install -Dm644 %{dux_src}/README.md %{buildroot}/usr/share/doc/dux/README.md
install -Dm644 %{dux_src}/openapi.yaml %{buildroot}/usr/share/doc/dux/openapi.yaml
install -Dm644 %{dux_src}/LICENSE %{buildroot}/usr/share/licenses/dux/LICENSE

%files
/usr/bin/dux
/usr/lib/systemd/system/dux-sentinel.service
/usr/share/dux/dux.toml.example
%doc /usr/share/doc/dux/README.md
%doc /usr/share/doc/dux/openapi.yaml
%license /usr/share/licenses/dux/LICENSE

%post
install -d -m 0700 /etc/dux
if [ ! -e /etc/dux/dux.toml ] && [ ! -e /etc/dux/notify.conf ]; then
    install -m 0600 /usr/share/dux/dux.toml.example /etc/dux/dux.toml
    echo "dux: created /etc/dux/dux.toml. Set up alerts with: dux setup"
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
    if [ "$1" -eq 1 ]; then
        systemctl enable --now dux-sentinel.service >/dev/null 2>&1 || :
        echo "dux: dux-sentinel is running. Test alerts with: dux notify-test"
    else
        systemctl try-restart dux-sentinel.service >/dev/null 2>&1 || :
    fi
fi

%preun
if [ "$1" -eq 0 ]; then
    # RPM has no purge: erasing removes everything (owner decision); say so.
    cat >&2 <<'WARN'
dux: WARNING: removing dux this way also DELETES everything dux created:
dux:   /etc/dux/dux.toml and notify.conf (your alert channels, tokens, thresholds)
dux:   /etc/dux/never-touch (the folders you protected)
dux:   /var/lib/dux (folder history and snapshots) and the automatic-cleanup drop-in
dux: The audit log /var/log/dux/actions.log is kept.
dux: To keep your settings, copy /etc/dux somewhere safe BEFORE removing,
dux: e.g. cp -a /etc/dux /root/dux-config-backup
WARN
    if [ -d /run/systemd/system ]; then
        systemctl disable --now dux-sentinel.service >/dev/null 2>&1 || :
    fi
fi

%postun
# RPM has no purge: removing the package removes everything dux created
# (spec PKG-6), the way `apt purge` does.
if [ "$1" -eq 0 ]; then
    rm -rf /var/lib/dux/sentinel
    rmdir /var/lib/dux 2>/dev/null || :
    rm -f /etc/dux/dux.toml /etc/dux/dux.toml.dux-new /etc/dux/notify.conf /etc/dux/never-touch /etc/dux/never-touch.lock /etc/dux/web.env /etc/dux/disable-actions
    rmdir /etc/dux 2>/dev/null || :
    rm -f /etc/systemd/system/dux-sentinel.service.d/dux-auto.conf
    rmdir /etc/systemd/system/dux-sentinel.service.d 2>/dev/null || :
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi
