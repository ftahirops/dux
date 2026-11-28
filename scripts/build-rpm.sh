#!/bin/sh
# Build the dux .rpm from the static binary. Usage: ARCH=x86_64|aarch64 scripts/build-rpm.sh
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
REL=${RPM_RELEASE:-1}
TARGET=${CARGO_TARGET_DIR:-target}
ARCH=${ARCH:-x86_64}
case "$ARCH" in
    x86_64) TRIPLE=x86_64-unknown-linux-musl ;;
    aarch64) TRIPLE=aarch64-unknown-linux-musl
             export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld ;;
    *) echo "ARCH must be x86_64 or aarch64" >&2; exit 2 ;;
esac
cargo build --release --target "$TRIPLE" ${CARGO_FLAGS:-}
SRC="$TARGET/rpm-src"
rm -rf "$SRC" && mkdir -p "$SRC"
cp "$TARGET/$TRIPLE/release/dux" "$SRC/dux"
cp packaging/dux-sentinel.service packaging/dux.toml.example packaging/openapi.yaml README.md LICENSE "$SRC/"
mkdir -p "$TARGET/rpm"
rpmbuild -bb packaging/rpm/dux.spec \
    --define "_topdir $(realpath "$TARGET")/rpmbuild" \
    --define "_rpmdir $(realpath "$TARGET")/rpm" \
    --define "_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" \
    --define "dux_version $VERSION" --define "dux_release $REL" \
    --define "dux_arch $ARCH" --define "dux_src $(realpath "$SRC")" \
    --target "$ARCH" >/dev/null
RPM="$TARGET/rpm/dux-${VERSION}-${REL}.${ARCH}.rpm"
sha256sum "$RPM" > "$RPM.sha256"
echo "$RPM"
