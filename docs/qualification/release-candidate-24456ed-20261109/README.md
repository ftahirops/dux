# Release candidate package qualification — 2026-11-09

Built from clean commit `24456ed8978fa33886ab13944e8afaf57762c7b6`
using `scripts/build-release.sh` with package revision 122. The build produced
a static-musl executable, Debian and RPM packages, source archive, debug symbols,
manifest, and audit reports. The artifacts were staged locally at
`/tmp/dux-prod-candidate-24456ed`; they were not installed on VPS2.

## Verified

- Source tree was clean and matched digest
  `90502dc2b0a98436f1ae78ba039ff457eda487bca785267e5ac0f38f79fddb5b`.
- Target: `x86_64-unknown-linux-musl`; compiler: Rust 1.99.1.
- Release executable SHA-256:
  `3c731e2057535363e1751dee671d6f9d0af7133709a10322ca0a4dface6b6c3`.
- DEB `0.6.3-122` SHA-256:
  `7531bea622442aea881965c77e078a158caeea436047f23605438ff270347efd`.
- RPM `0.6.3-122` SHA-256:
  `b95525f9513c1f3ab3b07dfec530b8c0a21fbc115e7b68f666b3783df2a2342b`.
- Both package payload executable hashes exactly match the release binary.
- `cargo audit --deny warnings` reported zero lockfile vulnerabilities and zero
  warnings; exact-binary audit recovered dependency metadata for 202 crates
  without an incomplete-scan warning.
- Package checksums verified with `sha256sum -c`; DEB and RPM metadata parsed;
  executable `--version` reported the expected revision and source digest.
- Package lifecycle harness unit tests: **70 passed**.

The package files remain in the staging directory rather than being committed
to the source repository. Verify them with
`(cd /tmp/dux-prod-candidate-24456ed && sha256sum -c /home/dux/docs/qualification/release-candidate-24456ed-20261109/release-artifact-checksums.txt)`.

Evidence files: `build-manifest.json`, `release-artifact-checksums.txt`, both
audit reports, and `package-lifecycle-tests.log`.

## Not established

This is package-build and harness-unit evidence, not end-to-end installation,
upgrade, re-upgrade, rollback, live migration, or service runtime evidence for
revision 122. VPS2 remains on the separate active r121 canary; that service was
not stopped or modified. The r121 soak must finish, then revision 122 requires
its own controlled install/rollback qualification before production rollout.
