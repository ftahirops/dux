# Fanotify ingress self-event filtering qualification

**Result:** the patched candidate performed better than the unfiltered candidate
in the tested VPS2 workload. This is a bounded qualification result, not a
production-readiness or rollout approval.

## Change under test

The dedicated fanotify reader now removes well-formed records whose PID is
Dux's own before admitting read chunks to the bounded in-memory ring. This
prevents Dux-generated SQLite/WAL/index activity from filling that ring before
the coordinator can discard those events. Overflow records are retained even
if their PID matches Dux. Malformed/ambiguous data is forwarded unchanged to
the canonical fail-closed parser.

The final source also contains the follow-up-scan latch: a manual SIGHUP during
an observation is remembered, and a follow-up observation is scheduled when
the active observation races. The midpoint case exercised bounded retry and
recovered to an exact comparison.

## Evidence

- Exact final candidate binary SHA-256:
  `f5318e04ecbd7ca460c75a850e369b534ac9fdaa553d70812b1c003290da0d24`.
- Source SHA-256: `351d2df5afbeb8b706abb4eed5c779eaba4aebed344b708543cbffa477b808a8`.
- Readiness harness SHA-256:
  `c36e898f229861c529bcdf7de4961ed17c9b53acff3e1bf1bf8a067a5d7f58e5`.
- Environment: VPS2, Linux 6.8.0-147-generic, ext4 disposable loop image.
- Limits: 25% CPU and 4 GiB memory; 100,000 baseline files; 20,000 mixed
  mutations in ten batches; 2 GiB filesystem image.
- Result: **passed** in 747.388 seconds. All ten batches matched independent
  path, inode, allocation, and rollup comparisons. The harness recorded 50,498
  events, zero dropped events, and no uncertain checkpoints. Per-batch exact
  convergence ranged from 37.917 to 141.997 seconds.
- The midpoint concurrent observation correctly reported itself incomplete and
  scheduled one bounded retry; that retry and the remaining batches converged.
  Therefore this run had a recovered transient observation error; it was not an
  error-free execution trace.
- Final-source Rust suite: **300 passed, 0 failed, 2 ignored**.
- Strict Clippy (`--all-targets --all-features -- -D warnings`) passed, and the
  optimized release build completed.

Artifacts: [final churn report](final-churn-report.json), [progress report](final-churn-progress.json), [daemon log](final-daemon.log), [harness log](final-run.log), [Rust suite](rust-suite-final.log), [Clippy](clippy-final.log), and [release build](release-build-final.log).

The same exact binary then passed the forced-overflow suite. The fixture
overflowed a 16,384-event kernel queue after 18,432 creates; the kernel
overflow counter advanced from 0 to 1. Dux reported uncertainty, started a
full scan, completed accounting verification, advanced the scan generation,
and matched the filesystem oracle with clean status in 7.147 seconds. See
[the final overflow report](final-overflow-report.json). This qualifies one
ext4 overflow-recovery scenario; it does not establish recovery across other
kernels/filesystems or sustained repeated overflow.

A second independent 100k/20k churn run with the same source, binary, seed,
limits, and harness also passed all ten exact checkpoints. It completed in
708.328 seconds; maximum batch convergence was 129.611 seconds, with zero
dropped events and no uncertain checkpoint. See the
[repeat churn report](repeat-churn-report.json). Across both runs, all twenty
checkpoints matched the filesystem oracle; the first run exercised an
incomplete midpoint observation and automatic recovery, while the repeat had
no incomplete checkpoint.

The same exact binary also passed the isolated 1M scale profile on VPS2/Linux
6.8 ext4 at 50% CPU quota and a 2 GiB worker memory cap. The exact scan took
115.384 s at 153,844 KiB peak RSS; one million generated paths matched the
independent inode/allocation/directory-entry oracle. Ten samples per query had
p95 latencies of 2.4–60.2 ms; TUI input and quit p95 were 52.4 ms and 63.6 ms.
The 390 MB database passed the harness's integrity and performance budgets.
The run used an ext4 disk fixture under `/mnt/dux-readiness` in the isolated
KVM guest, with a private mount namespace. This is one 1M profile run and does
not qualify 10M scale, repeated query variance, or the 15-minute freshness
target. Full report: [scale-1m-report.json](scale-1m-report.json).

The scale harness's CLI accepted arbitrary sizes but its scan-time budget map
only accepted the three standard levels. I fixed the lookup and added a
300-second gate for the 1.37M production-count fixture; the four focused scale
measurement tests pass. The exact candidate then passed that 1.37M profile:
scan time 196.253 s, scan RSS 153,180 KiB, and exact path/allocation/inode
counts. Query p95 was at most 65.9 ms and TUI input/quit p95 were 1.3/31.7 ms.
SQLite integrity and all scale performance budgets passed. Cgroup memory peaked
at its 2 GiB cap and used 114 MiB swap; the scanner's measured RSS remained
about 150 MiB. See [scale-1370k-report.json](scale-1370k-report.json).

`cargo audit --deny warnings` passed for the 229 dependencies in `Cargo.lock`
with zero advisories against the RustSec database last updated 2026-11-06; the
raw report is [cargo-audit-lockfile.json](cargo-audit-lockfile.json). A binary
audit of the GNU qualification build warns that it lacks cargo-auditable
metadata and recovers only 43 dependencies. Treat that binary inventory as
incomplete; the current-source static-musl release artifact still needs to be
built and audited.

An unfiltered f5c84b3 control with the same harness and workload failed before
mutations: it remained uncertain after 240 seconds, with 16 userspace ring
overflow warnings in the captured tail and no kernel overflow notices. The
patched candidate passed all ten batches. This comparison supports self-event
pressure as one cause of ring saturation; it does not prove it is the only
cause of the earlier production backlog. See
[the unfiltered control report](unfiltered-f5c84b3-retry240-report.json).

## Scope and remaining gaps

These runs show improvement on bounded ext4 workloads up to 1.37M paths on one
VM and kernel.
They do not prove zero errors under arbitrary load or general production
readiness. The 10M scale, repeated query variance, 15-minute freshness SLO,
other filesystems/kernels, extended WAL
endurance, power-loss behavior, release packaging/upgrade/rollback, privilege
review, a 72-hour soak, and a 24-hour canary remain unqualified. The change
reduces self-generated ring pressure; fanotify remains a lossy hint source and
full reconciliation remains necessary.
