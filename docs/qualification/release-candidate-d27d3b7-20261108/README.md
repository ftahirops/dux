# Static release candidate 0.6.3-107

Built from clean commit `d27d3b7ccdaebc31060c57b3425baed25cb5739c` using
`scripts/build-release.sh`, Rust 1.99.1, and target
`x86_64-unknown-linux-musl`. The builder completed successfully, produced
static binary, DEB, RPM, source archive, and debug symbols, and verified all
artifact checksums and the matching binary payloads inside both packages.
The exact release binary SHA-256 is
`c301574f148be8ac42caf1800b21a9d0b66441ce38d25844d7f4ea203add0475`.

The Cargo.lock audit found zero known vulnerabilities and zero warnings across
229 dependencies. The exact binary included auditable dependency metadata for
202 dependencies; the binary audit completed without incomplete-data or warning
messages. Its advisory database snapshot was updated 2026-11-08 23:03 UTC.
The manifest, lockfile report, binary audit output, and checksums are preserved
beside this file. The complete release artifacts remain in the ignored local
directory `target/readiness/release-2068-20261108/` and were not installed.

The static release binary passed an isolated daemon smoke test on the local
ext4 test filesystem. Initial indexing covered 100 files, 5 directories, and
105 staged paths; the in-place observation after modifications covered 102
files, 5 directories, and 107 staged paths, with zero reported coverage
issues. The daemon log confirmed `use_in_place_observation=true` and the
independent accounting verifier passed. `find` returned a newly created path
and a renamed path, and no longer returned the deleted path or old rename
name. `last_scan_timestamp_seconds` advanced and the scan ended at age 0.

The status still reported `dirty=true` with reason “filesystem changes await
the next full observation” because hints arrived during the test. The scan
command explicitly reported that the index remained stale or uncertain due to
concurrent changes. This validates the production artifact and in-place
reconciliation path, but it is not a clean-state or sustained-load qualification.
The smoke report is in [`in-place-smoke-20261108.json`](in-place-smoke-20261108.json).

This package set is a qualification artifact only. It is not installed on the
production host or VPS2, and it does not close the 24-hour runtime soak or the
ignored ext4 and production-scale gates.
