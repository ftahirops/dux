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

## Present-path replacement fix under test — 2026-11-08

The VPS2 failure showed a narrower case than a vanished path: after the scan
staged a file, it disappeared and its inode was reused elsewhere, but the
original filename had already been recreated with a different inode. The old
revalidation pruned paths returning `ENOENT` but retained this present,
mismatched leaf, so the scan stayed incomplete.

The current working tree extends revalidation for a non-directory leaf only
when every ancestor still matches its staged identity and the leaf's current
file type matches. It restats the path and refreshes the staged device, inode,
birth time, mount ID, allocation, owner, and mtime. Directory replacements,
ancestor changes, unrepresentable identities, and permission/I/O errors remain
fail-closed; the code does not infer that a still-present ambiguous subtree is
safe to publish.

The deterministic ext4 test now forces inode reuse at a second path and
recreates the original filename with a different inode. An independent fixture
oracle checks that the index contains exactly the five current paths, each
path's device/inode/birth tuple matches `symlink_metadata`/statx, and root
allocated bytes and unique-inode count match direct filesystem metadata. It
passed 10/10 in the latest repeated run (and 10/10 in the earlier qualification
run). A guard test verifies that an unresolved directory mismatch still marks
the scan incomplete even when refreshing a sibling leaf clears the SQL
conflict group; otherwise the conflict counter alone could give a false-clean
result. The current all-features Rust suite passed 320/320 runnable tests
(2 ignored), the default-feature suite passed 317/317 runnable tests (2
ignored), Python discovery passed 323/323, strict Clippy passed, and format
checks passed. Current-source llvm-cov line coverage is 61.58% overall and
48.61% for `scan.rs`; it must not be treated as complete scanner-path coverage.

This is not yet a fix proven against VPS2: the live `d8ea72d` binary predates
it. The 20:02 scheduled scan failed with one conflict after pruning 48
vanished paths; its next bounded retry also failed. A third retry on the old
binary completed at 20:18:42 with zero coverage issues after pruning 60
vanished paths, but the 5-minute canary had already failed its 1,200-second
freshness limit and accounting verification was still pending at 20:19:14.
At 20:26:34 the old daemon was active with zero restarts and no current scan,
but status remained dirty and not caught up; `behind_seconds` was 7,383. The
new code still needs a clean release build, installation on the disposable
VPS2 with rollback evidence, and exact-oracle runtime checks under the same
workload before this issue can be called resolved. The schema still uses
`(dev,inode)` in the compatibility allocation table, so conflicts that are
not provably refreshable leaf paths must remain incomplete.

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

## Scan-first reader burst test — 2026-11-08

The scan-first reader now counts validated event metadata and kernel overflow
records, then discards the raw chunks. It no longer forwards scan-first events
through the bounded userspace payload ring or file-handle resolver. The full
root walk remains authoritative and visits all paths; event activity still
marks freshness as stale, and kernel overflow remains observable. The
event-applied mode retains path resolution.

A local nonblocking-pipe test delivered 10,000 valid synthetic event records;
the reader counted all records, queued zero payload chunks, and recorded no
drops. A parser test covered self-event filtering and truncated-record
rejection. This validates framing and queue-free behavior under the test input,
not real fanotify throughput or kernel-queue behavior. This revision is not
installed on VPS2; its effect on the prior repeated hint-ring overflow remains
unverified. Production-scale CPU/I/O, kernel overflow, scan freshness under
steady churn, and inode-reuse repair remain open gates. The final local source
passed 305 tests, 0 failed, 2 ignored; strict Clippy, formatting, and locked
release build passed.
