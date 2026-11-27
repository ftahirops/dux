#!/bin/sh
# Build the dux .deb (dux-sentinel design). Usage: scripts/build-deb.sh
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
REV=${DEB_REVISION:-1}
TARGET=${CARGO_TARGET_DIR:-target}
cargo build --release ${CARGO_FLAGS:-}
OUT="$TARGET/deb/dux_${VERSION}-${REV}_amd64"
rm -rf "$OUT"
install -Dm755 "$TARGET/release/dux" "$OUT/usr/bin/dux"
strip --strip-unneeded "$OUT/usr/bin/dux" 2>/dev/null || true
install -Dm644 packaging/dux-sentinel.service "$OUT/lib/systemd/system/dux-sentinel.service"
install -Dm644 packaging/notify.conf.example "$OUT/usr/share/doc/dux/notify.conf.example"
install -Dm644 README.md "$OUT/usr/share/doc/dux/README.md"
install -Dm644 LICENSE "$OUT/usr/share/doc/dux/copyright"
install -d "$OUT/DEBIAN"
sed "s/@VERSION@/${VERSION}-${REV}/" packaging/deb-sentinel/control > "$OUT/DEBIAN/control"
for s in preinst postinst prerm postrm; do
    install -m755 "packaging/deb-sentinel/$s" "$OUT/DEBIAN/$s"
done
dpkg-deb --root-owner-group --build "$OUT" "$TARGET/deb/" >/dev/null
DEB="$TARGET/deb/dux_${VERSION}-${REV}_amd64.deb"
sha256sum "$DEB" > "$DEB.sha256"
echo "$DEB"
