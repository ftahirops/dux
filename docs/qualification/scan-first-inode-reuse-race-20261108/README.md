# Live inode reuse during reconciliation scan

**Purpose:** establish the scanner's fail-closed behavior when ext4 reuses an
inode number during a full in-place observation.

## Test design

`scan::tests::in_place_scan_keeps_previous_index_when_ext4_reuses_inode_mid_walk`
uses a test-only hook immediately after the scanner stats `a/old-entry`. The
hook pauses the walker after it has read the parent directory entries but
before it descends into `a/child`. The test unlinks the old file, creates
bounded candidate files until ext4 reuses the exact `(device,inode)` with a
different birth time, renames that new incarnation to `a/child/new-entry`,
then resumes the walk.

The test checks that the scan reports one conflicting inode incarnation and
nonzero coverage errors. It then checks the previously published index still
contains the old path, does not publish the new path, and retains a dirty
marker. This verifies the current safe failure behavior; it does not make the
scanner converge through concurrent inode reuse.

The pause hook and field are compiled only under `cfg(test)` and do not exist
in release builds.

## Results

Environment: local Linux host, ext4 root filesystem at `/dev/mapper/ubuntu--vg-ubuntu--lv`,
12,320,768 total inodes, 8,192 inodes per group. The test verified ext4 via
filesystem magic and required a nonzero birth-time value. The race test passed
**10/10 consecutive runs** after changing the fixture to keep candidate inodes
allocated; the first ephemeral create/delete fixture was flaky and failed to
reuse the target inode, so it was replaced before recording the passing result.

Final source checks:

- `cargo test --locked --all-features`: **302 passed, 0 failed, 2 ignored**.
- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo build --release --locked`: passed.

The current behavior intentionally leaves the index stale when it cannot prove
that conflicting staged paths are simultaneously valid. A future fix must add
incarnation-aware allocation identity and revalidate affected paths before
publishing. Do not remove the conflict guard based on this test alone.

## Later live follow-up and incomplete-scan optimization — 2026-11-08 12:46 UTC

The VPS2 scan beginning at 12:42:29 completed at 12:45:25 with zero coverage
issues over 1,305,598 files and 156,800 directories. The daemon had zero
restarts. By 12:45:59 the index was dirty again because the fanotify hint ring
overflowed; the same reason and repeated overflow warnings were observed
through 12:46:24. At 12:46:25 status reported `dirty=true`, `caught_up=false`,
and a 236-second last-scan age. Cumulative service counters at scan completion
were 1,469 CPU seconds, 13.7 GB read, and 20.7 GB written. The scan completed,
but the current event-hint path does not preserve freshness between scans.

The source now avoids the v6 compatibility projection and rollup work after an
incremental scan has already failed coverage checks. Such output cannot be
published; the failure path records the attempt and returns the existing
published root total. A regression test verifies that no disposable projection
tables are created and that the previous total is returned. This reduces work
for incomplete observations but does not solve live inode reuse or hint-ring
loss. The change has not been installed on VPS2, so no live performance gain
is claimed. Current-source verification passed: 303 Rust tests passed, 0
failed, 2 ignored; strict all-target/all-feature Clippy, formatting, and the
locked release build passed.
