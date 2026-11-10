# Focused r132 qualification — 2026-11-10

These two test cases were marked ignored by the ordinary Rust suite because
one requires handle-capable ext4 and the other is a manually invoked scale
measurement. Both were run separately against the workspace test binary built
for the 2026-11-10 full Rust regression run. The Rust sources and lockfile at
that run matched candidate source commit `f02ba7ad235ea841a99de27b69fcd402e3c3927c`.
The test binary SHA-256 was
`01729c28f7ee9822c641531e33fdf24a0ad95963cacfcd3c9c06c97e78e90d27`.

## Stale-parent event classification on ext4

`watch::tests::stale_parent_events_reconcile_indexed_paths_and_ignore_freed_unindexed_dirs`
passed twice, with 0 failures. It ran as root using `/tmp`, whose filesystem
was accepted by the test's ext4 and file-handle capability checks. It obtained
real ext4 directory handles with `name_to_handle_at`, deleted test directories,
and passed constructed DFID_NAME records to the event parser. It verified:

- an indexed parent deleted after indexing queues reconciliation for its child;
- a same-inode, different-generation handle remains uncertain and queues no
  unsafe replacement path;
- an orphaned parent with remaining indexed children stays uncertain;
- a transient directory never present in the index can be ignored after it is
  freed;
- a live parent continues to resolve; and
- a filesystem not marked handle-capable retains uncertain/lost-coverage
  behavior.

This exercises the classification logic with real ext4 handles; it does not
exercise live fanotify delivery or kernel queue overflow. The command output is
in [`stale-parent-ext4.log`](stale-parent-ext4.log).

## 100k-entry unchanged reconciliation smoke

`scan::tests::reconciliation_scale_smoke_reports_scan_cost` passed on `/dev/shm`
(tmpfs), with the live Dux heartbeat present. The test's filesystem guard
allowed the pseudo-filesystem fixture. It created 100,000 64-byte files in 100
subdirectories, built the baseline index, then reconciled the unchanged tree.

- Baseline build: **7.521 s**
- Unchanged reconciliation: **8.440 s**
- Database: **30,072,832 bytes** before and after
- Assertion: expected directory-entry row count

This was a debug-profile test binary, so timings are not production performance
estimates. The test does not compare every indexed path against an independent
`find` walk and does not substitute for 1.37M/10M production-shaped scale runs.
The captured command and output are in [`scale-100k.log`](scale-100k.log).

## Combined test accounting

The full Rust run remains recorded as 341 passed, 0 failed, 2 ignored. These two
previously ignored cases then passed separately, so the distinct Rust cases
covered by the full run plus these focused runs are **343 passed, 0 failed**.
Together with the latest Python run, the combined distinct case accounting is
**723 passed, 0 failed, 1 skipped**. The skip is the RPM signing integration
without its fixture. The suite was not rerun in full after the focused runs.
