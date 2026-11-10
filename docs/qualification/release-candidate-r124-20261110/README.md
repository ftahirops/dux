# Dux package candidate 124 — 2026-11-10

## Build result

`scripts/build-release.sh target/readiness/production-r124-20261110` completed
successfully with package revision 124. It built the default production feature
set as an auditable **static musl** x86-64 binary, then produced and verified a
Debian package, RPM, debug symbols, and source archive. The release pipeline
also audited `Cargo.lock` and the exact binary. No package was installed and no
service was changed.

- Source revision: `9582e0e8800b`, with the scanner change present in the
  worktree during build.
- Version: `0.6.3-124`.
- Source tree SHA-256: `95431a93bc2d137ba296babba5e42e65bc794bee938a8b7ea0189c9208474979`.
- Target/toolchain: `x86_64-unknown-linux-musl`, Rust 1.99.1.
- DEB SHA-256: `573ae4972a9c0b10c697c2622030325801824384ee0534e9a331901f8c4db8c0`.
- RPM SHA-256: `017183d88e809b5be8aab9c06558314ee14f89c489d24efea25de1f115bd5471`.
- Static binary SHA-256: `3bc0f7d59997a28287375f4ca24c6b5d58acca67a1152f84be6510a04171fe8f`.

The full local artifacts remain under
`/home/dux/target/readiness/production-r124-20261110/`; the repository stores
the manifest and audit evidence, not the generated package files.

## Audit and next gate

The script's lockfile and embedded-binary dependency audits passed. The binary
contains auditable dependency metadata for 202 dependencies. The signed/known
advisory scan is bounded by the local RustSec advisory database and does not
cover application logic. Checksums for all generated outputs are in the local build directory at
`/home/dux/target/readiness/production-r124-20261110/SHA256SUMS`; verify them
from that directory with `sha256sum -c SHA256SUMS`. The detailed manifest is
[`build-manifest.json`](build-manifest.json).

The strict r121-to-r124 Debian lifecycle test is now prepared in an isolated
VM input share and is waiting on the VM runner's host-pressure guard. The live
VPS2 r121 daemon remains unchanged. Even after package lifecycle passes, the
candidate still needs installation only on the disposable qualification guest,
accounting/path checks, and a fresh uninterrupted 24-hour canary. The prior
r121 canary failure remains a failed qualification.
