# Release candidate 123 package qualification — 2026-11-09

Built from clean commit `0cad80e6436202fefc2ff385c18872ce2b4a910c` with
`scripts/build-release.sh` and package revision 123. Build outputs are staged
at `/tmp/dux-prod-candidate-0cad80e`; package archives are not committed here.

## Build and package identity

- Source tree digest:
  `736f003f362185275c1d8e474503210c1bf02948b0b1bfab08c639ffbf96f7e3`;
  manifest says `worktree_dirty=false`.
- Target: `x86_64-unknown-linux-musl`, Rust 1.99.1, cargo-auditable 0.7.7.
- Binary SHA-256:
  `0539487212efc3da1fd144e4aa58f648634c5ba66340ecdab6af3705fa2853a8`.
- DEB `0.6.3-123` SHA-256:
  `3e9b5c61422780d577112d8fc8f4cfb3cf95b88f14b379eb60793c63f6e205c4`.
- RPM `0.6.3-123` SHA-256:
  `54277219569f7e3c9ea755e946c852c3d5ce5686d33407a9c245cf2a737f2e68`.
- Both package executable payloads exactly match the release binary hash.
- `sha256sum -c` passed for all staged release artifacts.

## Security and packaging checks

- `cargo audit --deny warnings`: zero vulnerabilities and zero warnings across
  229 locked dependencies.
- Exact-binary audit recovered auditable dependency metadata for 202 crates;
  it emitted no incomplete-scan warning.
- CLI `--version`, top-level `--help`, and `metrics --help` smoke checks exited
  0. DEB and RPM metadata parsed successfully.
- `systemd-analyze verify` passed for service and tunnel units extracted from
  both packages; corresponding unit hashes match across formats.
- Package lifecycle harness unit tests: **70 passed**.

Full machine evidence is in `build-manifest.json`, the two cargo-audit files,
`release-artifact-checksums.txt`, `systemd-verify-report.txt`, CLI/package
metadata captures, and `dux-r123-package-tests.log`.

## Qualification limits

This is build, metadata, and harness-unit evidence. It does not exercise real
DEB/RPM install, upgrade, re-upgrade, rollback, remove, service startup, schema
migration, or workload behavior for revision 123. VPS2 remains on its active
r121 canary and was not modified. Install/rollback qualification for this exact
package and the full 24-hour soak remain open. Production VPS1 remains on
legacy `0.6.3`; no rollout was performed.
