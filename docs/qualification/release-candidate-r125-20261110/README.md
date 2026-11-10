# Dux release candidate 125 — 2026-11-10

## Build provenance and package checks

Release 125 was built from clean commit
`0bcd42e3703e8bdaed2db7abe9b02178f65b4cd5`, which is the pushed branch head.
The build manifest records `worktree_dirty: false`, source-tree SHA-256
`a42c461d7e1e3507c81de5ba81f2ef709207b7ed43a7c5ae56696a2457919d5d`, Rust
1.99.1, and target `x86_64-unknown-linux-musl`. The default production feature
set was built as a statically linked auditable binary.

The release script completed the lockfile and embedded-binary audits, split
Build-ID-matched debug symbols, and built DEB and RPM packages. Independent
artifact checks passed with `sha256sum -c SHA256SUMS`; package metadata reports
DEB `dux 0.6.3-125 amd64` and RPM `dux 0.6.3-125 x86_64`. The binary reports
`dux 0.6.3 (0bcd42e3703e) [production]`.

| Artifact | SHA-256 |
|---|---|
| DEB | `8128f0e5e648380ef8502060505e03e65f713757421a45a0d83aee4bd1aa4d6e` |
| RPM | `c1840842e498fc0eb04ca294f2ee4a7c8cf7b5961ce586379af7cc0d42db2412` |
| Static binary | `1e0479e57db74e5acdf40b870844259a51f8ab73c24897b6fa9a530027abc247` |
| Source archive | `aba590c7ff3e5de1611cc3382f39b6f75945f2a7cf881ee74fa69e804e45ba26` |

The local artifacts are in
`/home/dux/target/readiness/production-r125-20261110/`. The repository stores
the manifest and audit logs, not the package binaries or source archive.

## Security scan evidence

The recorded lockfile audit found **0 known vulnerabilities** across 229
dependencies, using the RustSec database snapshot with 1,430 advisories, last
updated 2026-11-09T17:13:08+01:00. The embedded binary audit found auditable
dependency metadata for 202 dependencies. This is dependency evidence bounded
by that advisory snapshot; it is not an application-code security proof and
does not replace a fresh advisory database scan.

## Lifecycle and rollout gates

No package has been installed by this build. The guarded r125 lifecycle runner
and its read-only VM inputs are prepared, but it has not started a VM because
the host admission guard continues to detect active Go test/build jobs and
high load. The prior r124 runner was stopped before it started a VM. Release
125 still needs a strict install/upgrade/rollback test on the disposable
Ubuntu guest, followed by accounting/path verification and a fresh
uninterrupted 24-hour canary. VPS1 and VPS2 production services remain
unchanged. The previous VPS2 canary remains a failed run.

A fresh read-only VPS2 snapshot is retained in
[`vps2-live-status-20261110T0048Z.json`](vps2-live-status-20261110T0048Z.json).
At capture the r121 daemon was active with PID 2561224 and zero restarts. It
had completed a full observation 59 seconds earlier, but accounting
verification was still pending; the index remained dirty and not caught up,
with reported lag of about 10.8 hours. The same sample showed zero kernel
overflows and dropped events. Since VPS2 is in scan-first mode, `events_resolved`
being zero is expected: fanotify records are hints, and full observations
reconcile the index. This brief live snapshot is not a new canary pass.
At 00:50 UTC a second read-only sample showed the observation had advanced
past accounting verification to `filesystem changes await the next full
observation`; the daemon remained active with zero restarts, overflows, and
dropped events. The index still reported about 10.8 hours since its last
fully-caught-up state, so this is evidence of scan completion, not proof the
lag has recovered. See
[`vps2-live-status-20261110T0050Z.json`](vps2-live-status-20261110T0050Z.json).

The new `src/watch.rs` changes in this revision are test-only. The bounded
inode-reuse production change was already included in release 124; release 125
rebuilds that production code from its committed, clean source and includes
the subsequently committed regression tests in its source archive.
