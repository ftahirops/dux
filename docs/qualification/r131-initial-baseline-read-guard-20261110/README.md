# r131 initial-baseline read guard qualification — 2026-11-10

## Change and regression found

Reviewing r129's empty-index reader guard showed that its emptiness check
opened SQLite before returning the “initial scan in progress” result. A
focused regression assertion reproduced a `-wal` sidecar being created by
that read-only open, which could undermine the sidecar-free atomic rename.

r131 adds an `initial_baseline` bit to the existing scan-progress record. The
scanner writes it before the initial walk and retains it through publication;
old five-field progress records remain readable and default the new bit to
false. Read-only Dux clients check this marker before opening SQLite. Tests
assert the client returns the progress message without creating `-wal`,
`-shm`, or `-journal`, including at the publication handoff after the daemon's
SQLite connection has closed.

## Source and test evidence

- Source commit: `57b4356c0de6e1d2b6a0c0dea008d8747b0bb3e1`.
- Package version: `0.6.3-131`; build manifest reports a clean worktree.
- Focused tests passed:
  - `store::tests::open_ro_reports_initial_scan_for_initialized_empty_schema`
  - `scan::tests::fresh_initialized_index_uses_atomic_file_replacement_gate`
  - `util::tests::scan_progress_preserves_initial_baseline_and_reads_legacy_records`
- `cargo test --locked --all-features`: **340 passed, 0 failed, 2 ignored**.
- LLVM coverage run passed the same **340 tests, 0 failed, 2 ignored**.
  Coverage: **72.66% lines** (19,030/26,190), **71.51% regions**
  (30,262/42,319), **76.23% functions** (1,328/1,742). Branch counters were
  not available. The ignored cases include a manual scale scan and a
  handle-capable-ext4 stale-parent test.
- Strict all-target/all-feature Clippy, formatting, and `git diff --check`
  passed.

## Release package

The auditable release pipeline produced the static binary, DEB, RPM, source
archive, debug symbols, and build manifest. The manifest records a clean
source tree and zero known vulnerabilities in the audited lockfile. Binary
`--version` and `--help` checks passed; all eight entries in `SHA256SUMS`
verified.

- Static binary SHA-256:
  `7bf807bc2003b7de7856a27fb580e8ca8cea901978816df0ef2a1d24645c9d41`.
- Release artifacts and manifest: `target/readiness/production-r131-20261110/`.

## Isolated runtime smoke on VPS2

The exact r131 static binary ran on VPS2 (`192.168.5.106`) in a private mount
namespace using a 128 MiB tmpfs-backed ext4 loop filesystem. The installed
service and database were untouched; the service was active/running with
`NRestarts=0` before and after the smoke.

- Event-apply mode initialized/marked fanotify, resolved handles, indexed the
  create/rename fixture, and reported **11 events seen / 11 resolved**, zero
  drops, and zero kernel overflow notices.
- Scan-first mode recorded the change hint and published the changed file
  after a manual full observation. The final status was still `dirty=true`,
  as scan-first awaits its next scheduled observation; this is not a clean or
  caught-up claim.
- Each fixture observation reported zero coverage issues and passed
  accounting verification. Both modes reported zero drops and overflow
  notices.
- Runtime log SHA-256:
  `bc1da906f8ec266904295f1716f01e27df42fe20774589ccb2bfcea13f5875d6`.

The small fixture does not cross the 256 MiB WAL guard. This smoke does not
establish production-scale WAL behavior, package lifecycle safety, sustained
high-churn convergence, independent filesystem equality, or a 24-hour canary.

## Disk and qualification state

At capture, the local host had 55 GiB free on its 185 GiB root filesystem;
`target/readiness` used 1.3 GiB. VPS2 had 3.8 GiB free on its 93 GiB root
filesystem (96% used) and 2.9 GiB free in `/dev/shm`. Its top temporary/log
consumers included 5.3 GiB under `/var/tmp` and 4.1 GiB of system journal.
Those directories were inspected but not pruned because they contain prior
qualification/build material and retained logs whose ownership/retention needs
review.

The full package lifecycle was not run: the disposable-VM admission guard
refused startup while unrelated Go build/test workloads were active. The
guard was not bypassed. **Release decision remains: not production-ready.**
Large-root WAL sampling, full package lifecycle, high-churn/overflow
qualification, independent path equality, and a 24-hour canary remain open.

## Captured evidence

- `target/readiness/production-r131-20261110/runtime-smoke.log`
- `target/readiness/production-r131-20261110/service-before.txt`
- `target/readiness/production-r131-20261110/service-after.txt`
- `target/readiness/production-r131-20261110/llvm-cov-summary.json`
- `target/readiness/production-r131-20261110/build-manifest.json`
