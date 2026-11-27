# Bounded inode-reuse revalidation — 2026-11-10

## Change under qualification

`src/scan.rs` now retries staged inode-incarnation validation for at most three
passes. It makes another pass only when a pass removed or refreshed staged
paths. It exits early when conflicts clear or no candidate can be repaired.
Unresolved-path evidence is accumulated across passes, so a later pass cannot
erase an earlier ambiguous-directory result merely because a different path
cleared the SQL conflict group. Remaining conflicts still make the scan
incomplete; the patch never suppresses an identity conflict to force a clean
status.

This is a bounded recovery improvement, not a filesystem snapshot. The sample
108 VPS2 failure was followed by a successful scan using the existing code,
before this patch was installed. Therefore the patch is not yet proven to have
prevented that failure. The sample-108 event's specific remaining path/group
was not captured; the proposed validation-phase race remains a supported
hypothesis, not a proven incident root cause.

## Tests run on the current working tree

All local compile/test commands were run in transient systemd scopes with CPU,
memory, and I/O limits because unrelated Go/Vitest jobs were active on the
production host. No Dux service was restarted or changed.

| Check | Result | Scope |
|---|---:|---|
| Focused inode-reuse tests | **5 passed** | Revalidation/pruning/refresh; injected inter-pass race converges; persistent races stop at the bound; unrepairable paths stop after one pass. |
| All-feature debug suite | **333 passed, 0 failed, 2 ignored** | Final current tree. The two ignored tests were then invoked explicitly. |
| All-feature release suite | **332 passed, 0 failed, 2 ignored** | Production code matches the final tree; the one subsequently added test-only case for early no-progress exit was run in the current debug suite. |
| Ext4 stale-parent integration | **1 passed** | Explicitly ran the ignored test on handle-capable ext4 with real file handles and synthetic fanotify records. |
| 100,000-entry scale smoke | **1 passed** | Explicitly ran on `/dev/shm`: 17.890 s baseline rebuild, 17.900 s unchanged reconciliation, 30,171,136-byte DB. These are measurements on a busy, CPU-limited host, not a production benchmark. |
| Strict Clippy | **Passed** | All targets/features, warnings denied, undocumented unsafe blocks denied, final source including the added test. |
| Formatting and diff check | **Passed** | `cargo fmt --all -- --check`; `git diff --check`. |
| Production executable | **Built** | `cargo build --release --locked --bin dux`; default feature set, version identifies `[production]` and no fault-injection feature. |
| Static package pipeline | **Built and audited** | Release pipeline made DEB/RPM/static-musl binary at revision 124; lockfile and embedded binary audits passed; package checksums verified. |

The all-feature LLVM run explicitly included both ignored tests and passed
**335/335** in 213.10 seconds. The machine-readable export is
[`llvm-cov-all-tests.json`](llvm-cov-all-tests.json), SHA-256
`a9c5b245b527f4142128ab6ecddf05d28a3124f5e0893ff9e72ae30098866bd7`.
The invocation was `cargo llvm-cov test --locked --all-features --bin dux
--json --output-path <report> -- --include-ignored`, with the 100,000-entry
fixture placed on `/dev/shm`.

## Coverage

| Scope | Line | Region | Function | Branch |
|---|---:|---:|---:|---:|
| Whole Rust binary | 73.93% | 72.89% | 77.08% | Not emitted by this LLVM profile |
| `src/scan.rs` | 93.75% | 90.93% | 91.84% | Not emitted |

The overall result is slightly above the previous all-tests profile (73.73%
line, 72.67% region, 76.94% function). It is not proof of comprehensive
coverage: LLVM emits no branch counters here, and several production fanotify
syscall call sites remain unhit by the unit profile as documented in
[`coverage-gaps-20261109.md`](../coverage-gaps-20261109.md). The new inter-pass
tests mutate staged rows deterministically; they do not reproduce actual inode
reuse *during* a validation pass. The separate ext4 test exercises real inode
reuse during a scan, but not the newly added repeated-validation timing.

## Runtime and release gates still open

- The change has not been packaged, installed, or run on VPS2. The current r121
  service remains unchanged. A package has been built, but has not yet passed
  its install/upgrade/rollback lifecycle on the disposable VM.
- The VPS2 24-hour canary remains failed at sample 108. A successful recovery
  scan and accounting check do not make that soak pass; a new uninterrupted
  canary is required after candidate qualification.
- The strict Debian lifecycle rerun remains outstanding; the VM guard has
  stopped one attempt when external host work appeared.
- No isolated before/after benchmark of the exceptional multi-pass path exists.
  The normal no-conflict path does not enter this helper; conflict-path SQL and
  metadata costs still need measurement under a controlled workload.
- Root execution and `CAP_SYS_ADMIN` / `CAP_DAC_READ_SEARCH` remain a material
  privilege risk; this patch does not change the security boundary.

The package build record and audit artifacts are in
[`release-candidate-r124-20261110`](../release-candidate-r124-20261110/README.md).
Its DEB, RPM, and static binary remain in the local readiness output directory
until the guarded lifecycle VM completes.
