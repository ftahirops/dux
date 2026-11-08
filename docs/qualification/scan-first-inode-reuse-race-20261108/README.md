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
