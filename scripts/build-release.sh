#!/usr/bin/env bash
# Build the release binary + .deb + .rpm for dux.
#
# The binary is a STATIC musl build (no shared-library deps), so it runs on any
# x86_64 Linux regardless of host glibc. A default glibc build hard-pins the
# builder's glibc (e.g. GLIBC_2.39 on Ubuntu 24.04) and fails to load on older
# distros (Debian 12, Rocky 9, Ubuntu 22.04, …). Do NOT ship the glibc build.
#
# Requirements: rustup + `rustup target add x86_64-unknown-linux-musl`,
#               cargo-auditable 0.7.7, cargo-audit with a current RustSec DB,
#               musl-tools (provides musl-gcc),
#               readelf, rpmbuild, rpm2cpio, cpio, dpkg-deb.
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: scripts/build-release.sh [OUTPUT_DIRECTORY]

Build and verify the static musl release binary, DEB, RPM, source archive,
RustSec audit reports, and SHA256SUMS in OUTPUT_DIRECTORY (default: dist).
Set DUX_RELEASE_BINARY to package an already-built production binary.
Its embedded version, Git revision, and source-tree digest must match this checkout.
Set DUX_PACKAGE_RELEASE to choose the numeric DEB/RPM package revision.
Set DUX_RELEASE_SIGNING_KEY to a full GPG primary-key fingerprint to sign the
RPM and SHA256SUMS. Release builds require signatures by default. Set
DUX_ALLOW_UNSIGNED_RELEASE=1 only for local, non-distribution qualification
builds that have no release signing key.
USAGE
}

case ${1:-} in
    -h|--help)
        usage
        exit 0
        ;;
    --*)
        printf 'ERROR: unknown option: %s\n' "$1" >&2
        usage >&2
        exit 2
        ;;
esac
if (( $# > 1 )); then
    printf 'ERROR: expected at most one output directory\n' >&2
    usage >&2
    exit 2
fi

ALLOW_UNSIGNED_RELEASE=${DUX_ALLOW_UNSIGNED_RELEASE:-0}
if [[ $ALLOW_UNSIGNED_RELEASE != 0 && $ALLOW_UNSIGNED_RELEASE != 1 ]]; then
    echo 'ERROR: DUX_ALLOW_UNSIGNED_RELEASE must be 0 or 1' >&2
    exit 2
fi
if [[ -z ${DUX_RELEASE_SIGNING_KEY:-} && $ALLOW_UNSIGNED_RELEASE != 1 ]]; then
    echo 'ERROR: DUX_RELEASE_SIGNING_KEY is required; set DUX_ALLOW_UNSIGNED_RELEASE=1 only for local qualification builds' >&2
    exit 2
fi

cd "$(dirname "$0")/.."

SOURCE_TREE_SHA256=$(python3 - <<'PY'
import pathlib
import sys
sys.path.insert(0, 'scripts')
from dux_source_archive import collect_source_entries
_, digest = collect_source_entries(pathlib.Path.cwd())
print(digest)
PY
)

VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
PKGREL=${DUX_PACKAGE_RELEASE:-22}
if [[ ! $PKGREL =~ ^[0-9]+$ ]]; then
    echo "ERROR: DUX_PACKAGE_RELEASE must be a numeric package revision" >&2
    exit 2
fi
DEB_VER=${VER}-${PKGREL}
TARGET=x86_64-unknown-linux-musl
OUT=${1:-dist}
mkdir -p -- "$OUT"
OUT=$(realpath "$OUT")
TMPROOT=$(mktemp -d)
PUBLISH_BACKUP=
PUBLISH_ACTIVE=0
PUBLISHED_ARTIFACTS=()
BACKED_UP_ARTIFACTS=()
cleanup() {
    local exit_code=$?
    if (( PUBLISH_ACTIVE )); then
        set +e
        for name in "${PUBLISHED_ARTIFACTS[@]}"; do
            rm -f -- "$OUT/$name"
        done
        for name in "${BACKED_UP_ARTIFACTS[@]}"; do
            mv -f -- "$PUBLISH_BACKUP/$name" "$OUT/$name"
        done
        set -e
    fi
    rm -rf -- "$TMPROOT"
    if [[ -n $PUBLISH_BACKUP ]]; then
        rm -rf -- "$PUBLISH_BACKUP"
    fi
    return "$exit_code"
}
trap cleanup EXIT
STAGING_OUT=$TMPROOT/output
mkdir -p "$STAGING_OUT"

if [[ -n ${DUX_RELEASE_BINARY:-} ]]; then
    BIN=$DUX_RELEASE_BINARY
    CARGO_AUDITABLE_VERSION='external binary (not built here)'
    CARGO_AUDIT_VERSION=
    echo ">> packaging supplied, already-qualified binary (v$VER)"
else
    if ! command -v cargo-auditable >/dev/null 2>&1; then
        echo 'ERROR: cargo-auditable 0.7.7 is required for release builds' >&2
        exit 2
    fi
    # cargo-auditable is a Cargo subcommand. Its binary intentionally prints
    # an invocation hint when called directly, and `cargo auditable --version`
    # is intercepted by Cargo itself. Read the installed package version from
    # Cargo's authoritative installation inventory instead.
    AUDITABLE_INSTALL_ROOT=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-}}
    if [[ -z $AUDITABLE_INSTALL_ROOT ]]; then
        if [[ -n ${HOME:-} ]]; then
            AUDITABLE_INSTALL_ROOT=$HOME/.cargo
        else
            AUDITABLE_INSTALL_ROOT=$(dirname "$(dirname "$(command -v cargo-auditable)")")
        fi
    fi
    mapfile -t CARGO_AUDITABLE_VERSIONS < <(
        cargo install --list --root "$AUDITABLE_INSTALL_ROOT" |
            sed -n 's/^cargo-auditable v\([^:]*\):$/\1/p'
    )
    if (( ${#CARGO_AUDITABLE_VERSIONS[@]} != 1 )); then
        CARGO_AUDITABLE_VERSION='missing or ambiguous Cargo installation entry'
    else
        CARGO_AUDITABLE_VERSION=${CARGO_AUDITABLE_VERSIONS[0]}
    fi
    if [[ $CARGO_AUDITABLE_VERSION != 0.7.7 ]]; then
        echo "ERROR: expected cargo-auditable 0.7.7, found: $CARGO_AUDITABLE_VERSION" >&2
        exit 2
    fi
    echo ">> building auditable static musl binary (v$VER) with cargo-auditable $CARGO_AUDITABLE_VERSION"
    # Use a fresh isolated target directory. Cargo otherwise reuses artifacts
    # produced by a prior plain `cargo build`, which would omit the audit note.
    AUDIT_TARGET=${CARGO_TARGET_DIR:-target}/dux-auditable
    # Keep DWARF in the temporary build so a matching sidecar can be published.
    # The runtime/package binary is stripped below after extracting that sidecar.
    CARGO_TARGET_DIR=$AUDIT_TARGET \
        CARGO_PROFILE_RELEASE_DEBUG=1 \
        CARGO_PROFILE_RELEASE_STRIP=none \
        CC_x86_64_unknown_linux_musl=musl-gcc \
        cargo auditable build --locked --release --target "$TARGET"
    BIN=$AUDIT_TARGET/$TARGET/release/dux
fi
BIN=$(realpath "$BIN")
DEBUG_ARTIFACT=
if [[ -z ${DUX_RELEASE_BINARY:-} ]]; then
    DEBUG_ARTIFACT=$STAGING_OUT/dux.debug
    echo ">> splitting Build-ID-matched debug symbols"
    python3 scripts/dux_split_debug_symbols.py "$BIN" "$DEBUG_ARTIFACT"
fi
BIN_VERSION=$("$BIN" --version)
if [[ $BIN_VERSION != "dux $VER "* ]]; then
    echo "ERROR: binary version does not match source package version $VER: $BIN_VERSION" >&2
    exit 1
fi
EXPECTED_REV=$(git rev-parse --short=12 HEAD)
if [[ $BIN_VERSION != "dux $VER ($EXPECTED_REV"* ]]; then
    echo "ERROR: binary revision does not match packaging checkout $EXPECTED_REV: $BIN_VERSION" >&2
    exit 1
fi
if [[ $BIN_VERSION != *" source-sha256=$SOURCE_TREE_SHA256" ]]; then
    echo "ERROR: binary source digest does not match packaging checkout $SOURCE_TREE_SHA256: $BIN_VERSION" >&2
    exit 1
fi
[[ $BIN_VERSION == *"[production]"* ]] || { echo "ERROR: refusing a non-production binary" >&2; exit 1; }
file "$BIN" | grep -q 'static' || { echo "ERROR: binary is not static"; exit 1; }
command -v readelf >/dev/null 2>&1 || {
    echo 'ERROR: readelf (binutils) is required to verify embedded dependency metadata' >&2
    exit 2
}
if ! readelf -W -S "$BIN" | grep -Eq '[[:space:]]\.dep-v0[[:space:]]'; then
    echo 'ERROR: binary is missing embedded Rust dependency metadata (.dep-v0)' >&2
    exit 1
fi

# Audit both the lockfile and exact executable being packaged. Do not let the
# binary scanner silently fall back to partial dependency recovery. Preserve
# the machine-readable lockfile result and binary scan log with the artifacts.
if ! command -v cargo-audit >/dev/null 2>&1; then
    echo 'ERROR: cargo-audit is required to audit release dependencies' >&2
    exit 2
fi
CARGO_AUDIT_VERSION=$(cargo audit --version)
if [[ -z $CARGO_AUDIT_VERSION ]]; then
    echo 'ERROR: cargo-audit did not report its version' >&2
    exit 2
fi
LOCK_AUDIT_REPORT=$STAGING_OUT/cargo-audit-lockfile.json
BINARY_AUDIT_LOG=$STAGING_OUT/cargo-audit-binary.log
echo ">> auditing Cargo.lock with $CARGO_AUDIT_VERSION"
cargo audit --deny warnings --file Cargo.lock --format json > "$LOCK_AUDIT_REPORT"
python3 - "$LOCK_AUDIT_REPORT" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text())
vulnerabilities = report.get('vulnerabilities', {})
if vulnerabilities.get('found') is not False or vulnerabilities.get('count') != 0:
    raise SystemExit('ERROR: Cargo.lock audit report contains RustSec vulnerabilities')
if report.get('warnings') != {}:
    raise SystemExit('ERROR: Cargo.lock audit report contains warnings')
if not report.get('database', {}).get('last-commit'):
    raise SystemExit('ERROR: Cargo.lock audit report has no advisory database commit')
PY
echo '>> auditing exact release binary'
cargo audit bin "$BIN" > "$BINARY_AUDIT_LOG" 2>&1
if grep -Eiq 'warning:|report will be incomplete|not built with .cargo auditable.' "$BINARY_AUDIT_LOG"; then
    echo 'ERROR: binary dependency audit emitted a warning or was incomplete' >&2
    cat "$BINARY_AUDIT_LOG" >&2
    exit 1
fi

echo ">> .deb"
STAGE=$TMPROOT/dux_${DEB_VER}_amd64
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/lib/dux" "$STAGE/usr/lib/systemd/system" "$STAGE/usr/share/doc/dux"
cp packaging/deb/control "$STAGE/DEBIAN/control"
[[ $(grep -c '^Version:' "$STAGE/DEBIAN/control") -eq 1 ]] || {
    echo "ERROR: DEB control template must contain exactly one Version field" >&2
    exit 1
}
sed -i "s/^Version:.*/Version: ${DEB_VER}/" "$STAGE/DEBIAN/control"
for s in postinst prerm postrm; do install -m0755 "packaging/deb/$s" "$STAGE/DEBIAN/$s"; done
install -m0755 "$BIN" "$STAGE/usr/bin/dux"
install -m0755 packaging/secure-index-permissions.sh "$STAGE/usr/lib/dux/secure-index-permissions.sh"
install -m0755 scripts/dux-backup-index.py "$STAGE/usr/lib/dux/dux-backup-index.py"
install -m0644 packaging/dux.service "$STAGE/usr/lib/systemd/system/dux.service"
install -m0644 packaging/dux-tunnel.service "$STAGE/usr/lib/systemd/system/dux-tunnel.service"
install -m0644 README.md "$STAGE/usr/share/doc/dux/README.md"
printf 'Installed-Size: %s\n' "$(du -k -s "$STAGE/usr" | cut -f1)" >> "$STAGE/DEBIAN/control"
dpkg-deb --build --root-owner-group "$STAGE" "$STAGING_OUT/dux_${DEB_VER}_amd64.deb"
DEB_PAYLOAD=$TMPROOT/deb-payload
mkdir -p "$DEB_PAYLOAD"
dpkg-deb --extract "$STAGING_OUT/dux_${DEB_VER}_amd64.deb" "$DEB_PAYLOAD"

echo ">> .rpm"
TOP=$TMPROOT/rpm
mkdir -p "$TOP"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
install -m0755 "$BIN" "$TOP/SOURCES/dux"
install -m0755 packaging/secure-index-permissions.sh "$TOP/SOURCES/secure-index-permissions.sh"
install -m0755 scripts/dux-backup-index.py "$TOP/SOURCES/dux-backup-index.py"
install -m0644 packaging/dux.service "$TOP/SOURCES/dux.service"
install -m0644 packaging/dux-tunnel.service "$TOP/SOURCES/dux-tunnel.service"
install -m0644 README.md "$TOP/SOURCES/README.md"
cp packaging/dux.spec "$TOP/SPECS/dux.spec"
sed -i "s/^Release: *.*/Release:        ${PKGREL}%{?dist}/" "$TOP/SPECS/dux.spec"
grep -q "^Release: *${PKGREL}%{?dist}$" "$TOP/SPECS/dux.spec" || {
    echo "ERROR: could not set RPM release revision ${PKGREL}" >&2
    exit 1
}
rpmbuild --define "_topdir $TOP" -bb "$TOP/SPECS/dux.spec"
cp "$(find "$TOP/RPMS" -name '*.rpm')" "$STAGING_OUT/"
RPM_ARTIFACT=$(find "$TOP/RPMS" -name '*.rpm' -print -quit)
RPM_PAYLOAD=$TMPROOT/rpm-payload
mkdir -p "$RPM_PAYLOAD"
(cd "$RPM_PAYLOAD" && rpm2cpio "$RPM_ARTIFACT" | cpio -id --quiet ./usr/bin/dux ./usr/lib/dux/dux-backup-index.py)

install -m0755 "$BIN" "$STAGING_OUT/dux-${VER}-x86_64-linux-static"
python3 - "$VER" "$PKGREL" "$BIN" "$STAGING_OUT" "$SOURCE_TREE_SHA256" "$DEB_PAYLOAD/usr/bin/dux" "$RPM_PAYLOAD/usr/bin/dux" "$CARGO_AUDITABLE_VERSION" "$CARGO_AUDIT_VERSION" "$LOCK_AUDIT_REPORT" "$BINARY_AUDIT_LOG" "$DEBUG_ARTIFACT" "$DEB_PAYLOAD/usr/lib/dux/dux-backup-index.py" "$RPM_PAYLOAD/usr/lib/dux/dux-backup-index.py" scripts/dux-backup-index.py <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path.cwd() / 'scripts'))
from dux_source_archive import (
    SOURCE_TREE_EXCLUDES,
    collect_source_entries,
    source_tree_dirty,
    write_source_archive,
)

version, package_release = sys.argv[1], sys.argv[2]
binary, out = pathlib.Path(sys.argv[3]), pathlib.Path(sys.argv[4])
expected_source_digest = sys.argv[5]
deb_payload_binary, rpm_payload_binary = pathlib.Path(sys.argv[6]), pathlib.Path(sys.argv[7])
cargo_auditable_version = sys.argv[8]
cargo_audit_version = sys.argv[9]
lock_audit_report, binary_audit_log = map(pathlib.Path, sys.argv[10:12])
debug_artifact = pathlib.Path(sys.argv[12]) if sys.argv[12] else None
deb_backup_helper, rpm_backup_helper, source_backup_helper = map(pathlib.Path, sys.argv[13:16])
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

source_entries, source_tree_digest = collect_source_entries(pathlib.Path.cwd())
if source_tree_digest != expected_source_digest:
    raise SystemExit(
        f'source tree changed during packaging: {source_tree_digest} != {expected_source_digest}'
    )

# Preserve the exact source inputs next to the packages. A single aggregate
# hash cannot reveal which files were included if the worktree changes later.
source_archive = out / 'source.tar.gz'
write_source_archive(pathlib.Path.cwd(), source_entries, source_archive)

packages = [out / f'dux_{version}-{package_release}_amd64.deb', out / f'dux-{version}-{package_release}.x86_64.rpm',
            out / f'dux-{version}-x86_64-linux-static']
rpm_candidates = list(out.glob(f'dux-{version}-*.rpm'))
if len(rpm_candidates) == 1:
    packages[1] = rpm_candidates[0]
assert all(path.is_file() for path in packages), packages
for package_name, payload_binary in [('DEB', deb_payload_binary), ('RPM', rpm_payload_binary)]:
    assert sha(payload_binary) == sha(binary), (
        f'{package_name} payload executable differs from the qualified static-musl binary: '
        f"{sha(payload_binary)} != {sha(binary)}")
for package_name, payload_helper in [('DEB', deb_backup_helper), ('RPM', rpm_backup_helper)]:
    assert sha(payload_helper) == sha(source_backup_helper), (
        f'{package_name} backup helper differs from the source-qualified helper: '
        f'{sha(payload_helper)} != {sha(source_backup_helper)}')
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1']))
lock_audit = json.loads(lock_audit_report.read_text())
licenses = sorted({p.get('license') or 'UNSPECIFIED' for p in metadata['packages']})
manifest = {
    'version': version,
    'package_release': package_release,
    'commit': subprocess.run(['git', 'rev-parse', 'HEAD'], text=True, capture_output=True).stdout.strip(),
    'worktree_dirty': source_tree_dirty(pathlib.Path.cwd()),
    'source_tree_sha256': source_tree_digest,
    'source_file_count': len(source_entries),
    'source_tree_excludes': sorted(SOURCE_TREE_EXCLUDES),
    'source_files': source_entries,
    'source_archive': {
        'path': source_archive.name,
        'sha256': sha(source_archive),
    },
    'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
    'cargo_auditable': cargo_auditable_version,
    'cargo_audit': cargo_audit_version,
    'dependency_audit': {
        'advisory_database': lock_audit['database'],
        'lockfile_sha256': sha(pathlib.Path('Cargo.lock')),
        'lockfile_vulnerability_count': lock_audit['vulnerabilities']['count'],
        'lockfile_warnings': lock_audit['warnings'],
        'lockfile_report': {
            'path': lock_audit_report.name,
            'sha256': sha(lock_audit_report),
        },
        'binary_scan_log': {
            'path': binary_audit_log.name,
            'sha256': sha(binary_audit_log),
        },
    },
    'target': 'x86_64-unknown-linux-musl',
    'binary_sha256': sha(binary),
    'debug_symbols': ({
        'path': debug_artifact.name,
        'sha256': sha(debug_artifact),
        'build_id': subprocess.check_output(
            ['readelf', '-n', str(debug_artifact)], text=True,
            stderr=subprocess.STDOUT,
        ).split('Build ID: ', 1)[1].splitlines()[0],
    } if debug_artifact else None),
    'deb_payload_binary_sha256': sha(deb_payload_binary),
    'rpm_payload_binary_sha256': sha(rpm_payload_binary),
    'backup_helper_sha256': sha(source_backup_helper),
    'deb_payload_backup_helper_sha256': sha(deb_backup_helper),
    'rpm_payload_backup_helper_sha256': sha(rpm_backup_helper),
    'licenses': licenses,
    'artifacts': {p.name: sha(p) for p in packages},
}
(out / 'build-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
with (out / 'SHA256SUMS').open('w') as sums:
    evidence = packages + [source_archive, out / 'build-manifest.json',
                           lock_audit_report, binary_audit_log]
    if debug_artifact:
        evidence.append(debug_artifact)
    for path in evidence:
        sums.write(f'{sha(path)}  {path.name}\n')
PY

if [[ -n ${DUX_RELEASE_SIGNING_KEY:-} ]]; then
    echo '>> signing RPM and checksum manifest with the configured release key'
    python3 scripts/dux_release_signing.py sign "$STAGING_OUT" \
        --fingerprint "$DUX_RELEASE_SIGNING_KEY"
else
    echo 'WARNING: building unsigned qualification artifacts by explicit DUX_ALLOW_UNSIGNED_RELEASE=1 override' >&2
fi

# Keep the previous release files untouched until every payload, source input,
# and manifest assertion succeeds. Back up same-named files and journal each
# destination before the rename so EXIT cleanup can roll back a partial publish.
PUBLISH_BACKUP=$(mktemp -d "$OUT/.dux-release-backup.XXXXXX")
PUBLISH_ACTIVE=1
for artifact in "$STAGING_OUT"/*; do
    name=${artifact##*/}
    if [[ -e $OUT/$name || -L $OUT/$name ]]; then
        mv -f -- "$OUT/$name" "$PUBLISH_BACKUP/$name"
        BACKED_UP_ARTIFACTS+=("$name")
    fi
    PUBLISHED_ARTIFACTS+=("$name")
    mv -f -- "$artifact" "$OUT/$name"
done
PUBLISH_ACTIVE=0
rm -rf -- "$PUBLISH_BACKUP"
PUBLISH_BACKUP=
echo ">> done — artifacts in $OUT/:"
find "$OUT" -maxdepth 1 -type f \( -name 'dux_*_amd64.deb' -o -name 'dux-*.rpm' -o -name 'dux-*-x86_64-linux-static' -o -name 'dux.debug' -o -name 'source.tar.gz' -o -name 'build-manifest.json' -o -name 'SHA256SUMS' -o -name 'SHA256SUMS.asc' -o -name 'release-signing-key.asc' \) -printf '%f\n' | sort
