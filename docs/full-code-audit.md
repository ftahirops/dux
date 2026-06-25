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

---

# Second Deep Audit of Current HEAD

Date: June 25, 2026

Audited commit: `b25a881`

This second audit reviewed the fixes added after the original report and tested
the current system database. No source-code fixes were made during this pass.

## Updated verdict

The latest code is improved, but it is still not production-safe under sustained
filesystem churn, resource pressure, concurrent administration, or adversarial
filenames.

Several earlier defects were fixed:

- TUI schema-v3 startup failure.
- Destructive schema migration.
- Loss of the complete pending batch after a failed SQLite commit.
- Cross-database status heartbeat mismatch for the common single-daemon case.
- Recursive path-resolution depth guard.
- CLI limit integer overflow.
- TUI column alignment.

However, the live database is already inconsistent, and several architectural
defects remain.

## Critical confirmed findings

### 1. The live index is already internally inconsistent

The active `/var/lib/dux/dux.db` passed SQLite's structural integrity check:

```text
PRAGMA integrity_check = ok
```

But application-level invariants failed in one consistent SQLite read
transaction:

```text
orphan nodes:                  49
leaf recursive_bytes mismatch: 28
root byte drift:              about -23 MiB
root inode drift:             -2,286
duplicate path groups:         52
```

One duplicated path was represented by 49 different inode rows. Two directory
rows had invalid totals, including:

```text
recursive_bytes  = -12288
recursive_inodes = -3
```

SQLite integrity validates database pages and indexes. It does not validate the
filesystem-tree invariants required by this application.

### 2. No exclusive scan or daemon ownership lock exists

Two concurrent scans against the same database were reproduced. Both used the
same `<db>.new` path. One failed with:

```text
no such table: nodes
```

The other scan completed, but success depended on timing.

Relevant code:

- `src/scan.rs:70`
- `src/main.rs:170`
- `src/watch.rs:78`

The heartbeat is advisory and cannot provide mutual exclusion. Every scan and
daemon needs a nonblocking per-database `flock` held for the complete operation.

### 3. The schema allows multiple inodes to occupy one path

The `nodes` table has:

```sql
PRIMARY KEY (dev_id, inode)
```

It does not have:

```sql
UNIQUE (parent_dev, parent_inode, name)
```

Relevant schema:

- `src/store.rs:102`

If a delete or replacement event is missed, a new inode can be inserted at the
same parent/name while the stale row remains. The live database confirms this is
already happening.

### 4. Hardlink representation remains incorrect

A test containing two hardlinks indexed only one arbitrary path. Searching for
the other valid name returned no result.

The watcher explicitly ignores additional links when the inode already exists:

- `src/watch.rs:814`

Consequences:

- File search does not represent all valid paths.
- Deleting the selected path can remove an inode that still exists through
  another link.
- Modifying through an unindexed link can be missed or attributed incorrectly.
- Directory totals cannot correctly express where the shared inode is linked.

Correct support requires separate inode metadata and directory-entry/path
tables.

### 5. Per-operation watcher failures are still silently committed

The pending batch is now preserved if the whole SQLite transaction fails. That
fix is valid.

However, individual delete, move and upsert failures are caught inside the
transaction, logged only at debug level, and processing continues. The
transaction can then commit and the complete pending map is cleared.

Examples:

- `src/watch.rs:629`
- `src/watch.rs:662`
- `src/watch.rs:763`
- `src/watch.rs:839`
- `src/watch.rs:852`

Therefore, one operation can fail while related ancestor updates or other
operations commit. This can permanently create drift.

### 6. Watcher startup and shutdown lose changes

When an index is missing or schema-incompatible, the daemon performs the full
scan before creating fanotify watches:

- `src/watch.rs:85`

Changes during that scan are invisible to the daemon.

On SIGTERM or process termination, pending events are not flushed. Up to one
flush window is lost. There is no startup reconciliation to repair downtime or
shutdown gaps.

### 7. Daemon root is not validated against the indexed root

The daemon accepts a `root` argument independently of the database's
`last_scan_root`, `root_dev`, and `root_inode`.

A database indexed for one tree can therefore be watched using another tree.
That can produce missing-parent rows, partial totals, or irrelevant events.

The daemon must reject mismatched roots unless it performs a new atomic scan.

## Security findings

### Confirmed terminal escape injection

Filenames are printed directly by CLI and TUI output:

- `src/main.rs:393`
- `src/tui.rs:948`

Tests confirmed that filenames containing:

- Newline characters.
- ANSI/OSC control sequences.
- OSC 52 clipboard-setting sequences.

are emitted unchanged.

A local user can create a filename that forges terminal output or attempts to
modify the clipboard of an administrator running `dux`.

All untrusted filenames must be escaped or rendered with control characters
visibly encoded.

### World-readable root filename index

The active system files are:

```text
/var/lib/dux          mode 0755
/var/lib/dux/dux.db   mode 0644
```

This permits every local user to read the complete filename index for the root
filesystem, including filenames inside directories they could not normally
traverse.

This might be a deliberate product decision, but it is a significant
information-disclosure policy and must be documented and configurable.

### Privileged service hardening

The service runs as root with:

```text
CAP_SYS_ADMIN
CAP_DAC_READ_SEARCH
```

Relevant configuration:

- `packaging/dux.service:33`

`CAP_SYS_ADMIN` has broad security impact. The service also lacks several common
hardening controls. Fanotify mount-namespace requirements limit some options,
but the remaining security boundary should be reduced and explicitly analyzed.

### Dependency advisory results

The current `Cargo.lock` was checked against the current RustSec advisory
database.

No direct vulnerability caused audit failure, but two warnings were reported:

1. `lru 0.12.5`
   - RustSec: `RUSTSEC-2026-0002`
   - Soundness issue involving `IterMut`.
   - Patched in `>= 0.16.3`.

2. `paste 1.0.15`
   - RustSec: `RUSTSEC-2024-0436`
   - Unmaintained.

Both are transitive dependencies through `ratatui 0.28.1`.

## Memory, process and stress risks

### Scan memory remains unbounded

The scanner:

- Uses an unbounded channel.
- Completes the walk before draining it.
- Stores all raw nodes in memory.
- Allocates additional hash maps, hash sets and vectors proportional to node
  count.

Relevant code:

- `src/scan.rs:377`

This can exceed the service's 4 GB limit on large filesystems.

### Fanotify and pending operations are unbounded

The daemon requests:

```text
FAN_UNLIMITED_QUEUE
```

and stores pending operations in an unbounded `HashMap`.

If SQLite remains unavailable or slow, pending entries remain and new entries
continue accumulating. This is logical unbounded memory growth even though Rust
does not leak unreachable allocations.

### Alert subprocess lifecycle is unbounded

Alert commands are spawned without:

- A concurrency limit.
- A timeout.
- Waiting/reaping logic.
- A bounded work queue.

Relevant code:

- `src/watch.rs:942`

The `last_alert` map also has no cleanup policy.

### TUI growth query cost

On the current 1.5-million-node database, the recursive one-hour growth query
used approximately:

```text
0.5 CPU-seconds
```

It is refreshed approximately every three seconds while idle. Multiple TUI
sessions can therefore add material CPU and temporary-database activity.

### WAL and database growth

At audit time:

```text
main database: about 201 MiB
WAL:           about 57 MiB
growth rows:   about 54,000
```

`journal_size_limit` does not strictly bound active WAL growth when checkpoints
cannot progress.

Growth retention deletes old rows but does not shrink the main database file
automatically.

## Additional correctness findings

### Invalid UTF-8 filenames collapse

Two distinct filenames containing different invalid UTF-8 bytes were scanned.
Both were stored as the same replacement-character string.

Relevant code:

- `src/scan.rs:450`
- `src/watch.rs:431`
- `src/watch.rs:869`

This destroys path identity and makes reliable lookup, delete, rename and search
impossible.

### Control characters are stored unchanged

Newline, carriage-return and terminal-control characters are accepted into the
database and later printed directly.

### Populated directory moves remain incomplete

A directory moved into the watched tree is inserted as one directory node.
Existing descendants are not scanned:

- `src/watch.rs:741`

### Metadata changes are missed

The fanotify mask does not include `FAN_ATTRIB`:

- `src/watch.rs:305`

Ownership and timestamp-only changes therefore remain stale.

Directory allocation-block changes are also ignored because existing directory
upserts return immediately:

- `src/watch.rs:795`

This contributes to recursive-byte drift.

### Dirty state is not operational

Fanotify queue overflow writes `dirty_since`:

- `src/watch.rs:349`

But:

- Status does not display it.
- The TUI does not display it.
- The daemon continues reporting `live`.
- No reconciliation is scheduled.

### Partial watch coverage is hidden

Failed filesystem marks are silently ignored if at least one filesystem was
successfully marked:

- `src/watch.rs:106`

New mounts appearing after startup are not discovered.

### Atomic rebuild durability is incomplete

The atomic rebuild ignores checkpoint failure:

- `src/scan.rs:80`

It then removes the destination WAL/SHM files and renames the new main database.
No explicit file or parent-directory `fsync` is performed.

The rename is namespace-atomic but not fully crash-durable.

### TUI scope is inconsistent

When the TUI opens at a subtree:

- The tree is scoped.
- The largest-files panel remains global.
- The fastest-growth panel remains global.

Relevant queries:

- `src/tui.rs:426`
- `src/tui.rs:452`

### TUI silently truncates large directories

Only the first 5,000 children are loaded:

- `src/tui.rs:289`

A test directory with 5,100 children confirmed that the remaining entries are
not shown and no truncation warning is displayed.

### Older-schema read behavior is inconsistent

Read-only commands do not validate schema version before executing:

- Some queries work against an old schema.
- Other commands fail with missing-table errors.
- Status can report an old database as live if it owns the heartbeat.

The CLI should perform one explicit schema compatibility check for every command.

## Verification harness problems

The supplied verification script overstates confidence.

Problems include:

- It treats orphan and leaf inconsistencies as potentially transient while the
  daemon is live. SQLite transaction snapshots make committed tree invariants
  atomic, so these are real inconsistencies.
- It checks duplicate `(dev,inode)` rows, which the primary key already forbids,
  but does not check duplicate `(parent_dev,parent_inode,name)` paths.
- Its daemon liveness check still ignores database identity.
- It contains a dead loop for one index-to-disk check.
- It reconstructs paths using shell delimiters that filenames can contain.
- It does not check negative totals.
- It does not check `dirty_since`.
- It can print `ROCK SOLID` after only sampled checks.

Relevant sections:

- `scripts/dux-verify.sh:109`
- `scripts/dux-verify.sh:219`
- `scripts/dux-verify.sh:325`
- `scripts/dux-verify.sh:361`

## Current build and test status

### Passing

- Debug test suite: 3 tests passed.
- Release test suite: 3 tests passed.
- `cargo fmt --check`: passed.
- Basic CLI/TUI smoke tests passed after rebuilding the executable.
- FTS integrity check passed on a SQLite backup of the live database.

### Failing or insufficient

Strict Clippy currently fails:

```text
function `read_heartbeat` is never used
```

Only three automated tests exist. They do not cover:

- Daemon event processing.
- Hardlink lifecycle.
- Duplicate-path prevention.
- Transaction partial failures.
- Concurrent scans or daemons.
- Queue overflow.
- Dirty-state recovery.
- Populated directory moves.
- Startup/shutdown event gaps.
- Terminal control characters.
- Invalid UTF-8 filenames.
- TUI scope.
- Schema compatibility.
- WAL/checkpoint failure.
- Database-full behavior.

## Updated remediation order

### Immediate operational action

1. Stop the daemon.
2. Preserve a copy of the inconsistent database for diagnosis.
3. Perform a fresh atomic scan.
4. Do not advertise the index as trustworthy until drift is prevented.

### Immediate code changes

1. Add a per-database `flock` for every daemon and scan.
2. Add `UNIQUE(parent_dev,parent_inode,name)`.
3. Abort the whole flush transaction when any operation fails.
4. Stop reporting live status when `dirty_since` exists.
5. Validate daemon root and scan options against stored metadata.
6. Escape all terminal output.
7. Replace lossy filename storage with raw-byte-safe storage.
8. Add graceful SIGTERM handling and final flush.
9. Add startup reconciliation.

### Required architectural changes

1. Separate inode records from directory-entry/path records.
2. Model all hardlinks explicitly.
3. Stream scan processing instead of buffering the complete tree.
4. Bound watcher state and define overload behavior.
5. Reconcile moved-in directory trees.
6. Track mount additions and watch failures.
7. Add bounded alert workers.
8. Make atomic rebuild crash-durable.

## Final conclusion

The project is not 100% complete and does not currently meet the stated goal of
remaining harmless and correct under server stress.

No classic reachable-memory leak was identified, but the application has:

- Unbounded scan memory.
- Unbounded watcher state.
- Unbounded alert subprocess creation.
- Unbounded debounce-map growth.
- Potentially unbounded WAL/history storage.
- Confirmed live-index correctness drift.
- Confirmed terminal injection.
- Confirmed path-identity loss.

The next engineering milestone must prioritize invariant correctness, explicit
degraded state, resource bounds and recovery before additional performance or
UI work.

## External references

- Linux fanotify init:
  <https://man7.org/linux/man-pages/man2/fanotify_init.2.html>
- Linux fanotify overview:
  <https://man7.org/linux/man-pages/man7/fanotify.7.html>
- RustSec `lru` advisory:
  <https://rustsec.org/advisories/RUSTSEC-2026-0002.html>
- RustSec `paste` advisory:
  <https://rustsec.org/advisories/RUSTSEC-2024-0436.html>
