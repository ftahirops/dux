#!/bin/sh
# Build the dux .deb (dux-sentinel design). Usage: scripts/build-deb.sh
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
REV=${DEB_REVISION:-1}
TARGET=${CARGO_TARGET_DIR:-target}
# ARCH=amd64 (default) or arm64. A static (musl) binary runs on any Linux,
# whatever its libc and age.
ARCH=${ARCH:-amd64}
case "$ARCH" in
    amd64) TRIPLE=x86_64-unknown-linux-musl ;;
    arm64) TRIPLE=aarch64-unknown-linux-musl
           # No cross C compiler needed: link with the toolchain's own lld.
           export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld ;;
    *) echo "ARCH must be amd64 or arm64" >&2; exit 2 ;;
esac
cargo build --release --target "$TRIPLE" ${CARGO_FLAGS:-}
BIN="$TARGET/$TRIPLE/release/dux"
OUT="$TARGET/deb/dux_${VERSION}-${REV}_${ARCH}"
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
sed -e "s/@VERSION@/${VERSION}-${REV}/" -e "s/@ARCH@/${ARCH}/" packaging/deb-sentinel/control > "$OUT/DEBIAN/control"
for s in preinst postinst prerm postrm; do
    install -m755 "packaging/deb-sentinel/$s" "$OUT/DEBIAN/$s"
done
dpkg-deb -Zxz --root-owner-group --build "$OUT" "$TARGET/deb/" >/dev/null
DEB="$TARGET/deb/dux_${VERSION}-${REV}_${ARCH}.deb"
sha256sum "$DEB" > "$DEB.sha256"
echo "$DEB"
