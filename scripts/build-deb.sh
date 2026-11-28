#!/bin/sh
# Build the dux .deb (dux-sentinel design). Usage: scripts/build-deb.sh
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
REV=${DEB_REVISION:-1}
TARGET=${CARGO_TARGET_DIR:-target}
# A static (musl) binary runs on any Linux, whatever its glibc version.
cargo build --release --target x86_64-unknown-linux-musl ${CARGO_FLAGS:-}
BIN="$TARGET/x86_64-unknown-linux-musl/release/dux"
OUT="$TARGET/deb/dux_${VERSION}-${REV}_amd64"
rm -rf "$OUT"
install -Dm755 "$BIN" "$OUT/usr/bin/dux"
strip --strip-unneeded "$OUT/usr/bin/dux" 2>/dev/null || true
install -Dm644 packaging/dux-sentinel.service "$OUT/lib/systemd/system/dux-sentinel.service"
install -Dm644 packaging/dux.toml.example "$OUT/usr/share/doc/dux/dux.toml.example"
# Outside /usr/share/doc: minimal systems (Ubuntu images) do not install docs.
install -Dm644 packaging/dux.toml.example "$OUT/usr/share/dux/dux.toml.example"
install -Dm644 README.md "$OUT/usr/share/doc/dux/README.md"
install -Dm644 packaging/openapi.yaml "$OUT/usr/share/doc/dux/openapi.yaml"
install -Dm644 LICENSE "$OUT/usr/share/doc/dux/copyright"
install -d "$OUT/DEBIAN"
sed "s/@VERSION@/${VERSION}-${REV}/" packaging/deb-sentinel/control > "$OUT/DEBIAN/control"
for s in preinst postinst prerm postrm; do
    install -m755 "packaging/deb-sentinel/$s" "$OUT/DEBIAN/$s"
done
dpkg-deb -Zxz --root-owner-group --build "$OUT" "$TARGET/deb/" >/dev/null
DEB="$TARGET/deb/dux_${VERSION}-${REV}_amd64.deb"
sha256sum "$DEB" > "$DEB.sha256"
echo "$DEB"
