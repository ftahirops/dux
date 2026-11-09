# Qualification disk cleanup — 2026-11-09

## Findings

- The previously reported 56 GiB `target/readiness/vms` usage is no longer
  present. A current read-only inventory found that directory at **32 KiB**;
  all of `target/readiness` was **2.7 GiB** before cleanup.
- Eight stale Cargo target trees (`fault-build` and `fault-build-109` through
  `fault-build-115`) each occupied about 240 MiB. No Cargo or Rust compiler
  process was running. Their fault-injection executables were all unique, so
  the executables were retained at their original paths.

## Cleanup performed

Ran `cargo clean --target-dir` on each of the eight named target trees, then
restored each exact `release/dux` fault-injection executable from a temporary
`/dev/shm` copy. Cargo reported **1,164 files / 249.6 MiB removed per tree**,
**1,996.8 MiB total**. All eight restored executables passed both SHA-256 and
`dux --version` identity checks against the pre-cleanup manifest. The temporary
preservation copies were then removed.

The per-binary hashes, source digests, and versions are in
[`disk-cleanup-20261109-fault-binaries.json`](disk-cleanup-20261109-fault-binaries.json).

After cleanup, `target/readiness` measured **869 MiB** and `/` had **57 GiB
free** (68% used). The VM directory remains 32 KiB. Release packages, test
reports, source archives, failure evidence, and the eight fault binaries were
preserved. No production service, database, or VPS2 canary data was changed.

## Remaining disk concern

VPS2 is a separate active canary host and remains at 92% root usage with 7.2
GiB free in the latest check. Its systemd journal occupies 4.0 GiB; Dux's DB
and WAL occupy about 0.82 GiB and the canary log about 0.5 MB. Journals were
not vacuumed during the soak because the monitor's journal window is evidence.
The rest of VPS2's used space still needs a bounded directory-level inventory
after the canary ends.
