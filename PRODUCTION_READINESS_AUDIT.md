# dux Production Readiness Audit

Date: 2026-07-14
Updated: 2026-08-30 — bounded-scan and observability remediation pass.

## Remediation Status

| # | Risk | Status |
|---|---|---|
| 1 | Full scans use high RAM | **Fixed architecturally.** Nodes cross an 8,192-entry bounded queue into a disk-backed staging DB; no whole-tree Rust vectors/maps remain. |
| 4 | Expensive read commands | **Fixed for `du`.** `--max-depth` now prunes the SQL walk instead of filtering after: `du --max-depth=1 /usr` went 1.99s/31MB → 0.02s/6MB. The redundant full-subtree buffer is gone. `by_ext` re-measured — see below. |
| 6 | Filename disclosure | **Fixed.** Index is now `0640 root:dux`, not world-readable. `usermod -aG dux <user>` grants access. Upgrades chmod the existing index. |
| 5 | Operational docs | **Fixed.** README documents DIRTY / WRITES PAUSED / THROTTLED and when to rescan. |
| 2,3,5,7 | Daemon storm/pressure behaviour | Improved: queue depth/capacity, resolution, commits, drops, kernel drain state and lag are visible in status/TUI. |
| 7 | Broad capabilities | **Won't fix.** `CAP_SYS_ADMIN` is required by fanotify; already bounded via `CapabilityBoundingSet`. |

Corrections to the original audit:

- **`by_ext` is not a RAM risk.** It folds rows into a `HashMap` keyed by
  *extension*, so its memory is O(distinct extensions) — a few hundred entries,
  not O(files). It does scan every file row (CPU), but it never buffered the row
  set. The original finding overstated this.
- **`du` was the real unbounded read path**, and worse than described: it built
  the full subtree list *twice* (a tuple `Vec`, then a `DuRow` `Vec`).

### Bounded scan design

Both former whole-tree blockers are now reduced on disk. Parallel stat workers
feed an 8,192-entry bounded channel into a side staging DB. A window function
selects the deterministic minimum `(parent_dev,parent_inode,name)` per inode,
then indexed depth passes roll directory totals from deepest to root. The
staging file is removed before FTS and secondary indexes are built, so it never
bloats the installed index.

Workspace validation: 5,516 paths and 1.10 GiB indexed in 0.31s at 9.1 MiB
maximum RSS. Indexed bytes were exactly 1,177,096,192, identical to GNU `du`.

## Verdict

`dux` should not be described as "fully prepared for all possible production use cases" or "no bugs." It is fairly mature for a filesystem indexer, and the daemon has real protections against event storms, but there are still scale limits, correctness caveats, and operational risks before it can be called future-proof production-ready.

Validation run:

```text
cargo test
32 passed, 0 failed
```

## Main Risks

1. Full scans need temporary disk space.

   Bounded RAM is achieved with a side staging DB next to the destination index.
   A rebuild needs space for staging metadata plus the new atomic index until the
   swap completes. Failure remains safe: the old index is kept.

2. Daemon event storms degrade to eventual consistency, not exact realtime.

   This is intentional and mostly well handled. The daemon has `MAX_PENDING`, `FLUSH_BATCH`, CPU throttling, dirty marking, and stale-state reporting. Under massive churn, `dux` should avoid unbounded growth, but it may drop backlog and mark the index dirty. That means "safe for host" is much closer to true than "always exact."

3. Critical pressure pauses writes, but pending events still live in memory.

   Under low memory, low disk, high load, or PSI pressure, writes pause and SQLite memory is shrunk. Pending events remain in memory until recovery or until `MAX_PENDING` is exceeded. This protects the host, but a long critical period plus huge churn can still consume substantial RAM before the backlog is dropped.

4. Some read commands can be CPU-expensive on very large indexes.

   `by_ext` scans prime file dirents but uses memory proportional only to
   distinct extensions. Bounded-depth `du` is SQL-pruned and no longer
   double-buffers its subtree. Unbounded analytical queries can still consume CPU.

5. Large moved-in directories are bounded but may become dirty.

   A populated directory moved into the watched tree is reconciled with a hard budget of 1,000,000 entries. If the budget is exhausted, the index is marked dirty. This is safe behavior, but not exact for every possible case.

6. The index is a filename disclosure risk.

   The packaged service explicitly notes that `/var/lib/dux/dux.db` can contain the full filename list of the root filesystem, including names under directories users could not otherwise traverse. This is a real concern on multi-user or sensitive systems.

7. The daemon requires broad Linux capabilities.

   The service uses `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`. Capability bounding helps, but `CAP_SYS_ADMIN` is still a large privilege surface.

## What Looks Strong

- Atomic rebuilds avoid half-built indexes.
- A per-DB lock prevents concurrent writers.
- The daemon has CPU throttling and idle I/O priority.
- Pending events and flush batches are bounded.
- Fanotify queue overflow, missing capabilities, partial watch coverage, downtime gaps, and dropped backlog mark the index dirty instead of silently claiming exact data.
- Status/TUI surface dirty, paused, throttled, scan-progress, queue and lag states.
- WAL checkpointing has a size backstop.
- Alert subprocesses are capped and reaped.
- The schema handles hardlinks with separate inode and dirent tables.
- Non-UTF-8 names are stored as raw bytes.
- There are tests for hardlinks, duplicate path reuse, split renames, non-UTF-8 names, dirty clearing, and subtree delete with an outside hardlink.

## Specific High-Churn Scenario

For a scenario like "huge number of files are deleted/opened/updated, then dux goes very high":

- Transient create-delete bursts are coalesced and often produce little DB work.
- Fanotify reads are capped per loop tick.
- Flushes are capped to `FLUSH_BATCH`.
- CPU is throttled by a governor.
- Low disk/memory/load pressure pauses writes.
- Backlog is capped by `MAX_PENDING`.
- If events are lost or dropped, the index is marked dirty and needs a rescan.

So `dux` is designed to protect the host under this case. It is not guaranteed to remain exact in realtime under all such cases.

## Production Readiness Grade

- Daemon steady-state: reasonably production-minded, but not perfect.
- Full scan path: bounded-memory design; very large-scale disk/time benchmarks remain.
- Security posture: acceptable only if filename disclosure and required capabilities are acceptable.
- "No bugs / all possible use cases": no.
- "Pragmatic production beta with host-protection and dirty-state fallback": yes.

## Recommended Hardening Before Calling It Production-Ready

1. Add repeatable load tests for millions of creates, deletes, renames, and modifies.
2. Add benchmark gates for scan/index size and query memory/latency.
3. Add integration tests around real fanotify overflow and permission failure.
4. Consider cgroup memory/CPU limits for manual CLI scans, not just the daemon.
5. Add automatic bounded reconciliation where the missed-event scope is known.
