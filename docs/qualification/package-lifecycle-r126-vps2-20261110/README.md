# r126 Debian package lifecycle on VPS2 — 2026-11-10

## Result

The checked-in Debian lifecycle harness completed **passed** on the marked
KVM qualification VM VPS2 (`192.168.5.106`, Ubuntu 24.04, kernel
`6.8.0-147-generic`) in 1,732.768 seconds (28m 52.8s). It exercised the exact
r121 and r126 packages whose SHA-256 values are recorded in the report.

All five lifecycle stages passed:

1. r121 legacy baseline install/runtime observation.
2. Upgrade to r126 while preserving the operator's disabled-service state and
   creating a checked SQLite rollback backup.
3. Roll back to r121, verify its package binary hash, restore the saved index,
   and verify the rollback service runtime.
4. Re-upgrade to r126, then remove and purge the Debian package; remove retained
   package state only at purge.
5. Clean install r126 and verify package/binary/helper hashes, service
   readiness, a complete scan, accounting verification, systemd policy,
   index permissions, module-tree visibility, and absence of a tunnel key.

The clean-install observation staged 1,848,188 path instances (1,652,162 files
and 196,026 directories), reported zero coverage issues, and revalidated four
inode-reuse paths. Accounting verification passed. The final status was
`dirty=true` because scan-first waits for the next full observation;
`caught_up=false` and `behind_seconds_applicable=false` are expected in that
mode. The harness verifies install lifecycle and readiness; this is not proof
of independent path equality, freshness bounds, or the 24-hour canary.

## Reliability observation during the run

At 03:29:13 UTC, while the clean-install scan was running under host churn,
Dux logged `SQLite WAL exceeded its budget; event writes are paused until
checkpointing resumes` at 336,871,424 bytes. The sampled WAL reached
712,994,872 bytes, above the configured guard threshold. By 03:30:18 the WAL
was 134,217,728 bytes. The scan completed without coverage issues, accounting
verification passed, and status reported zero kernel overflows, zero dropped
events, and no write error. This is a bounded recovery observation, while the
WAL guard activation remains a reliability/freshness finding: the lifecycle
harness currently permits a passing package run despite that runtime error.
Do not interpret the lifecycle pass as a clean heavy-churn qualification.

During the clean-install service window, systemd reported its configured
1-core CPU quota, 2 GiB `MemoryHigh`, 4 GiB `MemoryMax`, and cumulative service
I/O of about 6.9 GiB read / 7.4 GiB written. These counters are scoped to that
service lifetime and test host; peak memory and production performance
percentiles were not measured here.

## Test-host restoration

Before running the destructive lifecycle, the previous `/var/lib/dux` was
copied to `/root/dux-lifecycle-preserve-20261110T0301Z/`. The backup's SQLite
`quick_check` passed and its database SHA-256 was
`1735bdbe84fae06d6fa883ae1e7a8fd2a03b21102ad5370517cf1c226e2a4803`.
After the lifecycle passed, r121 was reinstalled, the original index directory
was restored byte-for-byte, and the r121 binary hash matched the pre-test
capture (`b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`).
The service was re-enabled and started. Its post-restore r121 observation
completed with 1,654,904 files and 196,340 directories, zero coverage issues,
zero unresolved or vanished paths, and a passing accounting verification in
85.222 seconds. The service was active and enabled with zero restarts, drops,
or kernel overflows at the final check. A second bounded WAL-guard activation
occurred during this restored r121 scan (269,785,872 bytes); the WAL returned
to 134,217,728 bytes after the scan, with no reported write error. The
temporary preservation copy and lifecycle test files were removed after this
verification. This confirms test-host restoration; it does not qualify r126
runtime under sustained churn.

## Artifacts

- [`dux-r126-lifecycle-report.json`](dux-r126-lifecycle-report.json) — full
  machine-readable pass report and per-stage assertions.
- [`dux-r126-lifecycle.log`](dux-r126-lifecycle.log) — streamed stage progress
  and terminal result.
- [`dux-r126-lifecycle-dux-journal.log`](dux-r126-lifecycle-dux-journal.log) —
  Dux service scan and WAL evidence.
- [`SHA256SUMS`](SHA256SUMS) — checksums of the captured artifacts.

## Remaining qualification gates

This closes only the Debian package lifecycle gate on the tested VM/kernel.
Independent filesystem path/accounting comparison, broader high-churn and
stale-handle qualification, other package formats/kernels/filesystems, and a
fresh uninterrupted 24-hour r126 canary remain open. r126 is not installed on
production VPS1.
