# Full Engineering Audit

Date: June 25, 2026

## Overall verdict

The code is not production-safe under heavy stress yet. Core scan and query
logic is promising, but several critical correctness and availability defects
can silently make the index inaccurate while reporting it as healthy.

This was a read-only audit. No source files were modified as part of the review.

## Critical findings

### 1. TUI is broken on schema v3

The database replaced `changes` with `growth`, but the TUI still queries
`changes`:

- `src/tui.rs:194`
- `src/tui.rs:413`
- `src/tui.rs:424`

This was reproduced against a freshly created schema-v3 database:

```text
dux: no such table: changes: Error code 1: SQL error or missing database
```

As a result, the primary interactive interface cannot start.

### 2. Upgrading can silently erase the complete index

`Store::migrate()` drops all indexed data when it encounters an older schema:

- `src/store.rs:201`

The systemd pre-start scan only runs when the database file does not exist or is
empty:

- `packaging/dux.service:12`

Therefore, an existing schema-v1 or schema-v2 database can be upgraded to an
empty schema-v3 database without triggering a replacement scan.

This was reproduced with a schema-v2 database. Starting the daemon changed the
schema version to 3 and reduced the `nodes` table from one row to zero before
fanotify initialization failed.

### 3. Failed flushes permanently lose filesystem events

Pending events are removed from memory before the SQLite transaction has
succeeded:

- `src/watch.rs:448`

Any prepare, transaction, commit, disk-full, I/O, corruption, or locking error
therefore discards those events permanently.

Individual operation errors are also caught and ignored while the remaining
transaction is allowed to commit, for example:

- `src/watch.rs:746`

This can leave the database partially updated and guarantee index drift under
some failure conditions.

### 4. No exclusive daemon ownership exists

There is no per-database process lock. Two daemon instances can watch the same
filesystem and apply the same changes twice.

The heartbeat is not a lock and is stored in one global location:

- `src/util.rs:101`

Status checks also ignore which database owns the heartbeat:

- `src/query.rs:399`

This was reproduced: a temporary static database reported its daemon as live
because the heartbeat belonged to `/var/lib/dux/dux.db`.

### 5. Live hardlink handling is incorrect

The database stores only one path per inode. Additional hardlinks are discarded:

- `src/watch.rs:798`

Consequences include:

- Deleting the selected canonical path can remove the inode from the index even
  when another hardlink remains.
- Modifying a file through a different hardlink might not update its indexed
  size.
- Rename and delete events can resolve to the wrong canonical path.
- File search cannot represent every valid path to the inode.

Correct hardlink support requires separate inode and directory-entry/path
records.

### 6. Moving a populated directory into the watched tree loses its contents

A directory moved into the watched tree is inserted as a single node:

- `src/watch.rs:725`

Its existing descendants are not scanned. A move does not necessarily produce
individual create events for all files already inside the directory, so the
subtree can remain permanently incomplete.

## High-severity stress and resource issues

### Unbounded scan memory

The complete filesystem is collected through an unbounded channel and then held
in several vectors, hash maps, and hash sets:

- `src/scan.rs:348`

On a sufficiently large filesystem, memory use can exceed the systemd 4 GB
limit and kill the initial scan. The implementation should stream records into
temporary SQLite tables or an external-sort pipeline.

### Unbounded watcher pressure

The daemon requests `FAN_UNLIMITED_QUEUE` and stores pending events in an
unbounded `HashMap`:

- `src/watch.rs:275`

An event storm can consume substantial kernel and userspace memory. Queue size,
pending state, and processing latency need explicit limits and overload
behavior.

### Scan-to-daemon consistency gap

Changes occurring during the initial scan, or between scan completion and
watcher startup, can be missed permanently. A reliable design needs a watcher
established before the scan, an event journal, or a mandatory reconciliation
pass after startup.

### Growth history can dominate database size

The `growth` table permits one row per changed inode per five-minute bucket:

- `src/store.rs:108`
- `src/watch.rs:838`

For example, 100,000 active files in every bucket over seven days permits about
201 million rows. Retention deletes rows but does not shrink the database file
automatically.

Growth data needs a storage budget, aggregation tiers, incremental vacuuming, or
a bounded ring-style design.

### WAL size is not strictly capped

`journal_size_limit` controls retained WAL size after checkpoints; it does not
prevent WAL growth while readers block checkpoint progress:

- `src/store.rs:193`

Long-lived readers can still permit substantial WAL growth.

### Alert subprocesses are unbounded

Alert commands are spawned without waiting, concurrency limits, timeouts, or
backpressure:

- `src/watch.rs:923`

This can create process storms and unreaped child processes. The in-memory
`last_alert` map also has no cleanup policy.

### Watch coverage can silently degrade

Mount marking failures are ignored as long as at least one filesystem succeeds:

- `src/watch.rs:95`

The service can therefore claim to be live while missing complete filesystems.
Filesystems mounted after daemon startup are also never added.

### Non-UTF-8 filenames are corrupted

Unix filenames are converted with lossy UTF-8 conversion:

- `src/scan.rs:421`
- `src/watch.rs:848`

Distinct byte sequences can collapse into the same replacement-character
representation. Such paths cannot be reliably reconstructed or tracked.

### Dirty state is ineffective

Fanotify queue overflow writes `dirty_since`, but status does not display it and
the daemon does not initiate reconciliation. The index can continue reporting
itself as live after known event loss.

### Path resolution can overflow the stack

`PathResolver::resolve()` recursively follows parents without the 4096-node
cycle/depth guard used elsewhere:

- `src/store.rs:41`

A corrupt parent cycle or extreme path depth can cause unbounded recursion and a
stack overflow.

## Other functional and quality issues

- Scan statistics overcount directories by one. A test tree containing two
  directories was reported as containing three.
- The TUI silently hides children beyond 5,000 without indicating truncation:
  `src/tui.rs:287`.
- All directories are displayed as having children, including empty
  directories.
- Status can print awkward output such as `now ago` or `never ago`:
  `src/query.rs:343`.
- Arbitrarily large CLI limits can cast from `usize` to a negative `i64`,
  causing SQLite `LIMIT` to behave as unlimited.
- The verification script uses the global heartbeat and can associate daemon
  liveness with the wrong database.
- Verification compares path counts with inode counts despite intentionally
  collapsing hardlinks.
- The verification script labels sampled checks as `ROCK SOLID`, which
  overstates the evidence.
- Several database errors are converted into empty or zero values, hiding
  corruption and schema problems from users.
- The architecture document contains stale descriptions of the old `changes`
  schema.

## Database assessment

The schema-v2 and schema-v3 work significantly reduced steady-state database
size through:

- External-content FTS5.
- Removal of unused columns.
- Partial indexes.
- Bucketed growth history.
- Atomic full-scan replacement.

These are useful optimizations, but database growth is not fully bounded.
Important remaining risks are:

- Unbounded cardinality in `growth`.
- WAL growth during blocked checkpoints.
- Free pages retained after history deletion.
- Destructive migrations without an automatic rescan.
- No explicit maximum database size or low-disk-space behavior.

The daemon must stop or enter a clearly reported degraded mode before its own
database consumes the remaining filesystem space.

## UI and TUI assessment

The TUI currently cannot start on the active schema. After correcting that
regression, the following still require attention:

- Queries and rebuilding occur synchronously on the input/render thread.
- Expanded trees can generate large recursive query workloads.
- Errors during periodic refresh are silently ignored.
- The 5,000-child limit is not visible to the user.
- There is no clear stale, dirty, partial-watch, or reconciliation-required
  state.
- Global heartbeat data can incorrectly show an unrelated database as live.

The UI should prioritize correctness state over cosmetic live indicators.

## Validation performed

The following checks were run:

```text
cargo test --all-targets
```

Result: passed, with only three tests.

```text
cargo clippy --all-targets -- -D warnings
```

Result: passed.

```text
cargo fmt --all -- --check
```

Result: failed because committed source formatting differs from `rustfmt`.

Runtime validation included:

- Reproducing the fresh-schema TUI startup failure.
- Reproducing destructive schema-v2 to schema-v3 migration.
- Reproducing incorrect cross-database daemon status.
- Smoke testing scan, status, top, find, and growth commands.
- Running SQLite FTS integrity validation.
- Inspecting subtree query plans.

The current automated tests do not exercise:

- Fanotify parsing or event handling.
- Daemon flush correctness.
- Transaction failure and retry behavior.
- Database-full behavior.
- Queue overflow.
- Hardlink lifecycle behavior.
- Directory moves into or out of the watched tree.
- Schema migrations.
- Multiple daemon instances.
- TUI startup or refresh queries.
- Alert process lifecycle.
- Mount additions and failures.

## Recommended remediation order

### Phase 1: Stop correctness regressions

1. Convert all TUI queries from `changes` to the schema-v3 `growth` table.
2. Replace destructive migration with an explicit incompatible-schema error, or
   automatically perform a successful atomic rescan before installing the new
   schema.
3. Add a per-database `flock` held for the complete daemon lifetime.
4. Make status and scan guards validate the heartbeat's database identity.
5. Preserve pending events until their transaction commits successfully.
6. Treat operation failures as transaction failures unless explicitly proven
   safe.
7. Display dirty and partial-watch states and stop claiming the index is live
   when correctness is uncertain.

### Phase 2: Correct the data model

1. Separate inode metadata from path/directory-entry records.
2. Track every hardlink path while counting inode blocks only once.
3. Reconcile populated directories moved into the tree.
4. Handle directory moves out of and back into scope.
5. Preserve raw Unix filename bytes instead of lossy UTF-8 strings.

### Phase 3: Bound resource use

1. Stream scans instead of retaining every node in RAM.
2. Bound watcher queues and pending state.
3. Add event-rate metrics and overload thresholds.
4. Bound growth-history storage by bytes or rows, not only time.
5. Add incremental vacuum or periodic atomic compaction.
6. Limit concurrent alert commands and enforce timeouts.
7. Clean completed child processes and stale debounce entries.
8. Add low-disk-space protection for database and WAL writes.

### Phase 4: Recovery and operations

1. Start watching before scanning or journal events across the initial scan.
2. Reconcile after queue overflow and daemon downtime.
3. Detect and mark newly mounted filesystems.
4. Fail startup or report degraded status when any required mount cannot be
   watched.
5. Add automatic integrity checks and controlled rebuilding.
6. Ensure daemon restart cannot convert a populated index into an empty index.

### Phase 5: Testing

Add deterministic integration and stress tests for:

- Create, modify, truncate, sparse allocation, rename, delete, and replacement.
- Hardlinks created, modified, renamed, and deleted through every link.
- Populated subtree moves.
- Event coalescing order permutations.
- SQLite busy, I/O, commit, and disk-full failures.
- Fanotify overflow and malformed event buffers.
- Multiple daemon instances.
- Schema upgrades.
- TUI startup against every supported schema.
- Millions of files and high-rate event storms.
- Long-lived readers and WAL checkpoint behavior.

## Engineering target

The realistic target is not that the daemon can literally never fail. The
target should be:

- Bounded CPU, memory, I/O, database, process, and queue use.
- No silent loss of correctness.
- Atomic updates or explicit dirty state.
- Reliable retry and reconciliation.
- Correct hardlink and subtree semantics.
- No destructive migration without a validated replacement index.
- Clear degraded-mode reporting.
- Negligible idle overhead.

Under overload, the daemon should preserve host stability first, stop claiming
freshness, and recover through a controlled reconciliation process.
