# dux Production Readiness Audit

Date: 2026-07-14
Updated: 2026-07-14 — remediation pass; see "Remediation Status" below.

## Remediation Status

| # | Risk | Status |
|---|---|---|
| 1 | Full scans use high RAM | **Reduced, not eliminated.** Per-node cost cut ~34% (see below). All nodes are still held in memory; the streaming redesign is still open and is a speed/memory tradeoff — see "Open: streaming scan". |
| 4 | Expensive read commands | **Fixed for `du`.** `--max-depth` now prunes the SQL walk instead of filtering after: `du --max-depth=1 /usr` went 1.99s/31MB → 0.02s/6MB. The redundant full-subtree buffer is gone. `by_ext` re-measured — see below. |
| 6 | Filename disclosure | **Fixed.** Index is now `0640 root:dux`, not world-readable. `usermod -aG dux <user>` grants access. Upgrades chmod the existing index. |
| 5 | Operational docs | **Fixed.** README documents DIRTY / WRITES PAUSED / THROTTLED and when to rescan. |
| 2,3,5,7 | Daemon storm/pressure behaviour | Unchanged — audit found these already well handled. |
| 7 | Broad capabilities | **Won't fix.** `CAP_SYS_ADMIN` is required by fanotify; already bounded via `CapabilityBoundingSet`. |

Corrections to the original audit:

- **`by_ext` is not a RAM risk.** It folds rows into a `HashMap` keyed by
  *extension*, so its memory is O(distinct extensions) — a few hundred entries,
  not O(files). It does scan every file row (CPU), but it never buffered the row
  set. The original finding overstated this.
- **`du` was the real unbounded read path**, and worse than described: it built
  the full subtree list *twice* (a tuple `Vec`, then a `DuRow` `Vec`).

Scan memory, per node (the dominant term — every node is resident at once):

| Structure | Before | After |
|---|---|---|
| `RawNode` | 128 B | 104 B (dropped write-only `size`/`gid`/`mode`) |
| `canon` map | ~29 B x every node | removed; 8 B x every *file*, transient |
| `dir_idx` map | ~29 B x every node | ~29 B x every *directory* (~10% of nodes) |

Roughly 230 B/node → ~150 B/node, so the ~4 GB `MemoryMax` ceiling moves from
roughly 17M to roughly 27M files. Verified behaviour-preserving: rescanning
`/usr` (283,812 inodes, 20 hardlinks) yields byte-identical `du` output, and two
consecutive scans are identical (canonicalisation is deterministic).

### Open: streaming scan

The streaming redesign (recommendation 1) is **not** a free win and was left for
an explicit decision. Both blockers are whole-tree reductions:

1. **Bottom-up rollup** — a directory's `recursive_bytes` isn't final until its
   deepest descendant is seen.
2. **Hardlink canonicalisation** — the canonical link is the min
   `(parent_dev, parent_inode, name)` across *all* links, so it needs global
   knowledge per inode.

Pushing both into SQLite trades the current in-memory array rollup (fast: `/usr`
scans in ~12s) for per-depth `UPDATE` passes, which is likely to make scans
markedly slower. Memory-vs-speed is a product call, not a bug fix.

Note: an `nlink > 1` fast path is the obvious cheap fix here and is **unsound** —
a bind-mounted file shows the same `(dev,inode)` at two paths with `nlink == 1`,
so gating on `nlink` would mark both primary and double-count the blocks. The
current sort-based canonicalisation is correct for that case.

## Verdict

`dux` should not be described as "fully prepared for all possible production use cases" or "no bugs." It is fairly mature for a filesystem indexer, and the daemon has real protections against event storms, but there are still scale limits, correctness caveats, and operational risks before it can be called future-proof production-ready.

Validation run:

```text
cargo test
23 passed, 0 failed
```

## Main Risks

1. Full scans can use very high RAM on huge filesystems.

   `src/scan.rs` uses an unbounded channel and then collects all `RawNode`s into memory. The scan then allocates additional vectors and hash maps sized to the full node count. On tens or hundreds of millions of files, this can OOM or hit the packaged `MemoryMax=4G` service limit.

2. Daemon event storms degrade to eventual consistency, not exact realtime.

   This is intentional and mostly well handled. The daemon has `MAX_PENDING`, `FLUSH_BATCH`, CPU throttling, dirty marking, and stale-state reporting. Under massive churn, `dux` should avoid unbounded growth, but it may drop backlog and mark the index dirty. That means "safe for host" is much closer to true than "always exact."

3. Critical pressure pauses writes, but pending events still live in memory.

   Under low memory, low disk, high load, or PSI pressure, writes pause and SQLite memory is shrunk. Pending events remain in memory until recovery or until `MAX_PENDING` is exceeded. This protects the host, but a long critical period plus huge churn can still consume substantial RAM before the backlog is dropped.

4. Some read commands can be expensive on very large indexes.

   `by_ext` scans all prime file dirents in Rust and builds a hash map. `du` collects the full subtree into memory. These read paths are not governed like the daemon and can still cause CPU/RAM spikes on very large indexes.

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
- Status/TUI can surface dirty, paused, and throttled states.
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
- Full scan path: not ready for truly massive file counts without a memory redesign.
- Security posture: acceptable only if filename disclosure and required capabilities are acceptable.
- "No bugs / all possible use cases": no.
- "Pragmatic production beta with host-protection and dirty-state fallback": yes.

## Recommended Hardening Before Calling It Production-Ready

1. Redesign full scan to stream into SQLite or bounded batches instead of holding all nodes in memory.
2. Add load tests for millions of creates, deletes, renames, and modify events.
3. Add benchmark gates for query memory and latency on large indexes.
4. Add a root-only default packaging mode or require an explicit opt-in for world-readable indexes.
5. Add operational documentation for dirty/throttled/paused states and when to run `dux scan`.
6. Consider cgroup memory/CPU limits for manual CLI scans, not just the packaged daemon.
7. Add integration tests around fanotify overflow, permission failure, and real daemon restart gaps.
