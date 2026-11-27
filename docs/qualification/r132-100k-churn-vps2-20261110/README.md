# r132 100k-file churn qualification on VPS2

## Outcome

**Passed** on 2026-11-10 with the exact r132 static release binary:

- Release source commit: `f02ba7ad235ea841a99de27b69fcd402e3c3927c`
- Source tree SHA-256: `5d427c2c72dc3fc963fdae8c72006a82390cd00c8bd0b7af9d4e75540cd405ff`
- Candidate binary SHA-256: `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`
- VPS2 kernel: `6.8.0-147-generic`
- Filesystem: disposable ext4 loop image, 2 GiB, backed by `/dev/shm`
- Baseline: 100,000 files
- Mutation load: 20,000 operations in ten batches of 2,000
- Runtime: 38m02s transient-unit wall time; 9m30s CPU time
- Resource ceiling: 25% CPU, 4 GiB memory; observed peak 773 MiB memory and
  538 MiB swap

The test used the existing readiness harness in a marked KVM guest and a
private mount namespace. It did not install a package, modify the live Dux
database, or place its fixture on the root filesystem. The installed r121
service remained active with zero restarts. VPS2 root stayed 95% used, with
4.7 GiB available; the test fixture occupied tmpfs.

## Assertions that passed

The 20,000 mutations covered atomic replacement, create/delete and rapid path
reuse, rename-overwrite, hardlink write/unlink, in-place truncation/growth, and
transient create/delete. One batch started a reconciliation while mutations
were running. Every batch then completed an exact filesystem/index comparison
for paths, inode identities, allocated blocks and all rollups. All scans
reported zero coverage issues.

The final exact comparison passed, directory history remained continuous, and
SQLite `PRAGMA integrity_check` returned `ok`. The mutation workload generated
91,013 fanotify hints; the final candidate counters showed zero dropped
updates and zero kernel overflow notifications. Because r132 uses scan-first
mode, the test expects no per-file hint resolution or commits; full
observations establish the indexed state after each batch. The status remained
conservatively uncertain while hints arrived and was not treated as a clean
live view until exact reconciliation.

The ten harness exact-convergence measurements ranged from **92.3s to 323.0s**
with a **152.1s median**. They include triggering/awaiting an observation and
the independent exactness check, so they are end-to-end checkpoint times, not
pure scanner throughput. The harness version used for this qualification
(SHA-256 `2fd5cb894b1e0667fbef00df2ac6aa9aeaa3f8f73e8b025be002e305eb81fb67`)
sent a `SIGHUP` before calling `daemon.exact()`, which itself requests a
scan-first observation. This caused a redundant extra full scan per batch and
inflated run time and convergence measurements; it did not weaken the exact
checks. The duplicate trigger has since been removed, and a regression test
now checks that the harness retains only the deliberate mid-mutation signal
and relies on `daemon.exact()` for the post-batch observation.

## Qualification limits

This is strong evidence for the exact r132 binary under moderate, sustained
churn on a 100k-file ext4 tree. It does not establish production-scale
performance for the approximately 1.9M-node VPS2 tree, 1M/10M budgets,
multi-day WAL endurance, power-loss recovery, other filesystems/kernels, a
72-hour soak, or a 24-hour production canary. Zero drops/overflows applies to
this bounded test run only. The 538 MiB swap peak and long exact-check times
also mean these results should not be presented as a low-overhead performance
benchmark.

## Evidence files

- [`report.json`](report.json): full harness result, all ten checkpoint
  counters and convergence measurements, binary identity, limits and
  unqualified gates.
- [`churn-progress.json`](churn-progress.json): durable final progress and
  event-counter record.
