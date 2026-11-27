# r128 fresh-index publication qualification — 2026-11-10

## Candidate

- Source commit: `dd3573d21033e6a8d556dff1fccef338934d3e26`.
- Package version: `0.6.3-128`.
- Build manifest: [`build-manifest.json`](build-manifest.json); it records a
  clean worktree and source digest
  `3c4d162bd4f860e7bb402ff5839160a0afa2a7cfee17b37c942b12f300140209`.
- Static binary SHA-256:
  `876c40feb0d7be653e52d831f37b66ff46b887ef37f31815c690fe1ddbf99610`.
- DEB SHA-256:
  `15f83b0c0e1e002ac4fa1aeeedfecad213c90575734e02b0bb348b1e8325ef12`.
- The release build produced and verified static binary, DEB, RPM, source
  archive, debug symbols, and checksums. Cargo.lock and executable audits
  passed; the recorded lockfile audit found zero known vulnerabilities.

## Local test and coverage results

`cargo test --all-features` passed **338 tests**, with zero failures and two
ignored tests. The fresh-index replacement test verifies the main DB inode
changes, the replacement index contains the scanned path, and no SQLite
sidecars remain attached to the new file. The populated-index reader snapshot,
zero-byte database, recovery-sidecar, and dangling-symlink regression tests
also passed.

The all-features LLVM run passed the same **338 tests**. Coverage for this
revision is **72.68% lines** (18,973/26,106), **71.50% regions**
(30,153/42,171), and **76.24% functions** (1,325/1,738). LLVM reported no
branch counters. The two ignored scale/ext4 tests were not included in this
refresh. The machine-readable totals are in
[`llvm-cov-summary.json`](llvm-cov-summary.json).

Strict all-target/all-feature Clippy, formatting, and `git diff --check`
passed.

## Isolated runtime smoke on VPS2

The exact r128 static binary ran in a private mount namespace on VPS2
(Ubuntu 24.04, Linux 6.8.0-147-generic). The harness created a 128 MiB ext4
loop filesystem backed by `/dev/shm`; the installed r121 service and its
database were left untouched and remained active with zero restarts.

Both event-apply and default scan-first startup passed. Dux initialized and
marked fanotify, performed successful `open_by_handle_at` calls, indexed eight
creates and a rename in event-apply mode, and published a scan-first change
after a full observation. Each baseline/observation reported zero coverage
issues and passed accounting verification. Final event counts were 11 seen /
11 resolved in event-apply and 2 seen / 0 resolved in scan-first, with zero
reported drops and zero kernel overflows in both modes. The full captured
output is [`runtime-smoke.log`](runtime-smoke.log), SHA-256
`f1e9445abd2a90768f1dc163de8782cc24341140577555692098ea1a1ab84b46`. Release
artifact checksums are in the build output directory
`target/readiness/production-r128-20261110/`.

No WAL guard warning appeared in this small smoke. Its index was far below the
256 MiB WAL threshold, so this does **not** prove the r127 717 MB WAL spike is
fixed at production scale. It also does not qualify package install/upgrade,
large-root load, sustained churn, freshness bounds, or a 24-hour canary. The
local VM lifecycle attempt was refused by host-admission checks while unrelated
Go test workloads were active; those checks were not bypassed.

## Decision

This prototype fixes the publication design for an empty initial index and has
passed focused source tests plus an isolated live-daemon smoke. The large-index
WAL gate remains open until a marked qualification guest completes a clean
install with WAL sampling over the 256 MiB threshold and confirms exact
accounting. Do not install r128 on production VPS1 or claim production
qualification from this evidence.
