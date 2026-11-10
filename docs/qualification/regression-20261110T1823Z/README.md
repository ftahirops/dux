# Current workspace regression run — 2026-11-10 18:21–18:26 UTC

## Results

- `CARGO_BUILD_JOBS=1 cargo test --locked --all-features -- --test-threads=1`:
  **341 passed, 0 failed, 2 ignored**, 104.66 seconds. Both ignored cases
  require a handle-capable ext4 qualification profile; they are not counted as
  passing. The Rust source, `Cargo.toml`, and `Cargo.lock` are identical to the
  r132 release source commit `f02ba7ad235ea841a99de27b69fcd402e3c3927c`.
- `PYTHONPATH=scripts python3 -m unittest discover -s scripts -p 'test_*.py'`:
  **381 ran, 380 passed, 1 skipped, 0 failed**, 24.04 seconds. This is the
  current workspace's qualification tooling, including script changes newer
  than r132. The skipped test is the signing integration requiring its RPM
  fixture.
- `CARGO_BUILD_JOBS=1 cargo clippy --locked --all-features --all-targets --
  -D warnings -D clippy::undocumented_unsafe_blocks`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo audit --json`: 229 locked dependencies, zero advisories, zero
  warnings. The locally cached RustSec database has 1,446 advisories and was
  last updated at `28d73fc95e5972af3403225a217def57dcaba3c4` on
  2026-11-10 13:45:03 UTC. This run did not fetch a newer advisory database.

The Python and Rust suite totals are not added together here as a unique count:
they cover different code and include the same testing session, but are still
reported as 341 Rust and 381 Python cases. Combined case accounting is 721
passed, 0 failed, 2 ignored, and 1 skipped.

## Resource guard note

An initial serial Rust run was interrupted after 51 tests had reported `ok`
when concurrent unrelated Go/security workloads drove CPU pressure up sharply.
It is not counted as a passing run. After those workloads cleared, the complete
serial Rust suite passed as recorded above. No unrelated process was stopped.

## Fault-matrix preparation

The exact r132 production binary was copied from VPS2 and hash-verified. A
matching `[fault-injection]` companion was built from a clean detached worktree
at the same source commit; its embedded source SHA-256 is identical to the
production candidate. The harness then enforced its marked KVM/QEMU guest
requirement, so its first run outside a guest created no fixture and did not
start. A guarded disposable Ubuntu guest start was subsequently refused at
admission because unrelated host Go/Vitest/staticcheck workloads raised load1
to 25.99 and CPU PSI some avg10 to 76.79%. No fault-matrix tests have run yet.

## Scope limits

These checks do not qualify the release package lifecycle, 10M-file scale,
controlled crash recovery, all 15 unrun fault suites, multi-filesystem or
multi-kernel behavior, or production canary. The 72-hour soak remains a
separate active run. Exact binary identity and runtime qualification remain
separate from compiling the Rust source in the workspace.

## Logs

- [`rust-tests.log`](rust-tests.log)
- [`python-tests.log`](python-tests.log)
- [`clippy.log`](clippy.log)
- [`cargo-audit.json`](cargo-audit.json)
