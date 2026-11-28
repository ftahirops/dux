#!/bin/sh
# dux installer (spec PKG-5): picks the package for this distro and CPU,
# verifies its SHA-256 against the release's SHA256SUMS, then installs it.
#
#   curl -fsSL https://github.com/ftahirops/dux/releases/latest/download/install.sh | sudo sh
#
# DUX_VERSION=1.0.0 picks a release (default: the latest).
# DUX_STATIC=1 installs the static binary instead of a package.
# DUX_BASE_URL=https://mirror/dux/1.0.0 installs from a mirror.
set -eu
REPO="ftahirops/dux"
say() { printf 'dux install: %s\n' "$*"; }
die() { printf 'dux install: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (sudo sh)"
command -v sha256sum >/dev/null || die "sha256sum is needed"
if command -v curl >/dev/null; then
    get() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null; then
    get() { wget -q -O "$2" "$1"; }
else
    die "curl or wget is needed"
fi

case "$(uname -m)" in
    x86_64|amd64) DEB_ARCH=amd64; RPM_ARCH=x86_64; TRIPLE=x86_64-linux-musl ;;
    aarch64|arm64) DEB_ARCH=arm64; RPM_ARCH=aarch64; TRIPLE=aarch64-linux-musl ;;
    *) die "unsupported CPU $(uname -m): dux is built for x86_64 and arm64" ;;
esac

if [ -n "${DUX_BASE_URL:-}" ]; then
    # A mirror (or a test server) with the release assets and SHA256SUMS.
    BASE="$DUX_BASE_URL"
    VER="${DUX_VERSION:-}"
elif [ -n "${DUX_VERSION:-}" ]; then
    BASE="https://github.com/$REPO/releases/download/v$DUX_VERSION"
    VER="$DUX_VERSION"
else
    BASE="https://github.com/$REPO/releases/latest/download"
    VER=""
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
get "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" || die "cannot download SHA256SUMS from $BASE"
[ -n "$VER" ] || VER=$(sed -n 's/.*dux_\([0-9][0-9.]*\)-[0-9]*_amd64\.deb$/\1/p' "$TMP/SHA256SUMS" | head -1)
[ -n "$VER" ] || die "cannot tell the version from SHA256SUMS"

# Which package: .deb on Debian 12+ / Ubuntu 24.04+, .rpm on RHEL-family 9+
# and Fedora, the static binary everywhere else (spec PKG-1..3).
KIND=static
if [ "${DUX_STATIC:-0}" != 1 ] && [ -r /etc/os-release ]; then
    . /etc/os-release
    MAJOR=$(echo "${VERSION_ID:-0}" | cut -d. -f1)
    case " ${ID:-} ${ID_LIKE:-} " in
        *" ubuntu "*) [ "${ID:-}" = ubuntu ] && [ "$MAJOR" -ge 24 ] && KIND=deb ;;
    esac
    case "${ID:-}" in
        debian) [ "$MAJOR" -ge 12 ] && KIND=deb ;;
        fedora) KIND=rpm ;;
        rhel|rocky|almalinux|centos|ol) [ "$MAJOR" -ge 9 ] && KIND=rpm ;;
    esac
    command -v dpkg >/dev/null || [ "$KIND" != deb ] || KIND=static
    command -v rpm >/dev/null || [ "$KIND" != rpm ] || KIND=static
fi

case "$KIND" in
    deb) FILE="dux_${VER}-1_${DEB_ARCH}.deb" ;;
    rpm) FILE="dux-${VER}-1.${RPM_ARCH}.rpm" ;;
    static) FILE="dux-${VER}-${TRIPLE}" ;;
esac
say "installing dux $VER ($FILE)"
get "$BASE/$FILE" "$TMP/$FILE" || die "cannot download $FILE"
( cd "$TMP" && grep " $FILE\$" SHA256SUMS | sha256sum -c --status ) \
    || die "checksum of $FILE does not match SHA256SUMS: not installed"

case "$KIND" in
    deb) dpkg -i "$TMP/$FILE" ;;
    rpm) rpm -Uvh --replacepkgs "$TMP/$FILE" ;;
    static)
        install -m 0755 "$TMP/$FILE" /usr/local/bin/dux
        say "installed /usr/local/bin/dux (static binary; no service installed)."
        say "run the watcher as a service with your init system: dux sentinel --config /etc/dux/dux.toml"
        ;;
esac
say "done. Next: dux setup"
