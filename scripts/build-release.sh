#!/usr/bin/env bash
# Build the release binary + .deb + .rpm for dux.
#
# The binary is a STATIC musl build (no shared-library deps), so it runs on any
# x86_64 Linux regardless of host glibc. A default glibc build hard-pins the
# builder's glibc (e.g. GLIBC_2.39 on Ubuntu 24.04) and fails to load on older
# distros (Debian 12, Rocky 9, Ubuntu 22.04, …). Do NOT ship the glibc build.
#
# Requirements: rustup + `rustup target add x86_64-unknown-linux-musl`,
#               musl-tools (provides musl-gcc), rpmbuild, dpkg-deb.
set -euo pipefail
cd "$(dirname "$0")/.."

VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TARGET=x86_64-unknown-linux-musl
OUT=${1:-dist}
mkdir -p "$OUT"

echo ">> building static musl binary (v$VER)"
CC_x86_64_unknown_linux_musl=musl-gcc \
    cargo build --release --target "$TARGET"
BIN=target/$TARGET/release/dux
file "$BIN" | grep -q 'static' || { echo "ERROR: binary is not static"; exit 1; }

echo ">> .deb"
STAGE=$(mktemp -d)/dux_${VER}_amd64
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/lib/systemd/system" "$STAGE/usr/share/doc/dux"
cp packaging/deb/control "$STAGE/DEBIAN/control"
for s in postinst prerm postrm; do install -m0755 "packaging/deb/$s" "$STAGE/DEBIAN/$s"; done
install -m0755 "$BIN" "$STAGE/usr/bin/dux"
install -m0644 packaging/dux.service "$STAGE/usr/lib/systemd/system/dux.service"
install -m0644 README.md "$STAGE/usr/share/doc/dux/README.md"
printf 'Installed-Size: %s\n' "$(du -k -s "$STAGE/usr" | cut -f1)" >> "$STAGE/DEBIAN/control"
dpkg-deb --build --root-owner-group "$STAGE" "$OUT/dux_${VER}_amd64.deb"

echo ">> .rpm"
TOP=$(mktemp -d)
mkdir -p "$TOP"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
install -m0755 "$BIN" "$TOP/SOURCES/dux"
install -m0644 packaging/dux.service "$TOP/SOURCES/dux.service"
install -m0644 README.md "$TOP/SOURCES/README.md"
cp packaging/dux.spec "$TOP/SPECS/dux.spec"
rpmbuild --define "_topdir $TOP" -bb "$TOP/SPECS/dux.spec"
cp "$(find "$TOP/RPMS" -name '*.rpm')" "$OUT/"

install -m0755 "$BIN" "$OUT/dux-${VER}-x86_64-linux-static"
echo ">> done — artifacts in $OUT/:"
ls -1 "$OUT"
