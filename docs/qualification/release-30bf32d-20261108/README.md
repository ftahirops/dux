# Static-musl package revision 105

Built with `scripts/build-release.sh` from clean commit
`30bf32da7e32e15f93c9461bd83df010677992ad` on 2026-11-08. The release
manifest records `worktree_dirty=false`, source-tree SHA-256
`a73f01dcf948a5af67071e216ef55b048e44069e8ca3cb72ddbf6e9613cd310d`, and
Rust 1.99.1 with `cargo-auditable` 0.7.7.

## Artifact verification

- Static-musl executable SHA-256:
  `8f6e8c14307058258b905d7e6b2530bc5465b66fd3d89784fcd55147add82ef4`.
- DEB SHA-256:
  `b19796568dd1b5c3ec55f7318a88fa980dfa1c92407f41ef1421c14d48e1e84d`.
- RPM SHA-256:
  `237d5dc0af3bbcb221c08ec3ad58108504b9b30683f11279b6128122ac8166b0`.
- Embedded executable hashes in both package payloads match the standalone
  executable. All eight checksums in `target/readiness/release-30bf32d/SHA256SUMS`
  passed.
- The standalone executable runs and reports Dux 0.6.3, commit `30bf32d`, and
  the matching source-tree digest. `dpkg-deb --info` and `rpm -qip` recognize
  the packages.
- Lockfile audit: zero known advisories among 229 dependencies. The embedded
  binary audit recognized 202 dependencies. The RustSec database snapshot was
  last updated 2026-11-06.
- The exact standalone release binary was copied to a temporary VPS2 path and
  ran `containers --json` against the live schema-v7 database without
  installation or service changes. It returned valid JSON for 29 Docker
  containers (20 running, 9 stopped), including writable layers, logs,
  volumes, and reclaimable totals. The smoke-result summary is in
  [`release-container-smoke-20261108.json`](release-container-smoke-20261108.json).

Artifacts are retained locally in the ignored directory
`target/readiness/release-30bf32d/`; package files are intentionally not in the
Git history. This proves package construction, payload identity, and one
read-only command integration against live Docker metadata. It does not
qualify the daemon or escaped upperdir paths end to end. Revision 105 has not
been installed on VPS2 or production.
