# Deep Performance and Architecture Audit

No source changes were made during this audit.

## Critical Findings

1. Test build is currently broken.

`watch::flush` now takes `growth_keep_secs: i64`, but four unit-test call sites
still pass 4 arguments:

- `src/watch.rs:1204`
- `src/watch.rs:1668`
- `src/watch.rs:1687`
- `src/watch.rs:1702`
- `src/watch.rs:1743`

`cargo test --no-run` fails with `E0061`. CI/test verification is currently dead
until those test call sites are updated.

2. Full scan is still memory-bound.

The scanner collects every filesystem entry into memory, then builds additional
maps/vectors over the same cardinality:

- `src/scan.rs:236`
- `src/scan.rs:268`
- `src/scan.rs:277`
- `src/scan.rs:299`
- `src/scan.rs:444`

For tens of millions of files this can exceed memory before SQLite or disk
becomes the limiting factor. The unbounded channel avoids walker deadlock, but
it also means peak memory is unbounded. This conflicts with the packaged
`MemoryMax=4G` setting in `packaging/dux.service`.

3. Daemon event hot path is too expensive for Maildir-scale churn.

Every fanotify event is resolved to a full path before coalescing:

- `src/watch.rs:668`
- `src/watch.rs:685`
- `src/watch.rs:751`
- `src/watch.rs:762`

That means `open_by_handle_at`, `/proc/self/fd` readlink, path allocation, and a
full `PathBuf` key per event. On high-churn Zimbra/Maildir trees, this is likely
the main cause of event pressure, missed events, and early `DIRTY` state.

4. Startup/rebuild has an event-loss window.

The service runs a full scan before daemon watch setup:

- `packaging/dux.service:12`

The daemon also rebuilds before fanotify is initialized and filesystems are
marked:

- `src/watch.rs:179`
- `src/watch.rs:193`

Changes during that scan are not journaled, so a live mail server can produce a
stale index immediately after startup without a known `dirty_since`.

## High-Impact Performance Issues

5. Scoped queries materialize recursive subtree sets.

`SUBTREE_CTE` walks descendants for scoped `top`, `find`, `growth`, and TUI
panels:

- `src/query.rs:42`
- `src/query.rs:111`
- `src/query.rs:206`
- `src/tui.rs:628`

Global `dux top` is fast. Scoped queries over huge subtrees can still have
latency cliffs. A faster subtree model would use nested-set intervals,
materialized ancestry, or per-directory cached rankings.

6. TUI can keep reading an old DB after atomic rescan.

The background worker opens one read-only connection and only reopens if a query
fails:

- `src/tui.rs:126`
- `src/tui.rs:140`

Atomic rename does not necessarily make the old connection fail. The TUI can keep
rendering the unlinked old database after a daemon rescan.

7. `reconcile_subtree` commits partial data when budget is exhausted.

It inserts entries until `budget == 0`, marks dirty, returns `Ok(())`, and the
outer flush commits:

- `src/watch.rs:1156`
- `src/watch.rs:1164`
- `src/watch.rs:1174`
- `src/watch.rs:1448`

Dirty state is surfaced, but queries before a rescan can see a partially indexed
moved-in subtree.

8. `by-ext` is not million-file safe.

It streams all prime file dirents into Rust and aggregates in a `HashMap`:

- `src/query.rs:340`
- `src/query.rs:350`

This is proportional to file count and holds a long read transaction. It should
be precomputed or backed by an indexed extension column.

## Command Smoothness

- `dux top` global: strong; uses precomputed recursive totals and indexes.
- `dux top <large subtree>`: can be slow due recursive CTE.
- `dux status`: improved by root-row count lookup; fallback still full-counts if
  root metadata is missing.
- `dux find --name`: good for trigram-friendly searches; short patterns and broad
  globs can degrade.
- `dux growth`: acceptable if growth table is bounded; high-churn windows can
  still group many rows.
- `dux by-owner`: full scan of `inodes`; okay occasionally, not ideal for
  realtime use at huge scale.
- `dux by-ext`: weakest CLI query for large indexes.
- `dux /` TUI: idle repaint is improved, but background refresh and stale DB
  connection behavior remain.
- `dux scan /`: biggest memory risk.
- `dux daemon /`: biggest realtime risk is full path resolution before
  coalescing.

## Architecture Assessment

Strong choices:

- `inodes` separated from `dirents`.
- `WITHOUT ROWID` on high-cardinality keyed tables.
- Atomic rebuild with rename.
- WAL mode.
- Hardlink-aware accounting.
- Terminal output escaping.
- Bounded pending backlog with dirty-state fallback.

Main architectural bottlenecks:

- Realtime daemon is path-first instead of identity-first.
- Full scan is collect-then-aggregate instead of streaming or externally sorted.
- Scoped queries compute subtree membership at query time.
- Some aggregate commands compute from raw rows instead of maintained summaries.

The next architecture step should be identity-first event handling: key pending
events by parent identity plus raw filename, coalesce there, and only construct
full paths for display.

## Code Quality Notes

- `Store::get_meta` turns all DB errors into `None`, which can hide corruption or
  I/O errors.
- Some comments are stale. Example: scan mentions a single transaction, while
  inserts are batched.
- `root_matches` returns `true` when it cannot determine root identity, which can
  skip a needed rebuild in ambiguous states.
- Unsafe fanotify parsing is mostly contained, but deserves integration tests
  because ABI mistakes would be severe.

## Test Coverage Gaps

Existing tests cover hardlinks, rename split, dirty clear, non-UTF8 names, TUI
generation behavior, and basic scan semantics.

Missing coverage:

- fanotify capability failure.
- fanotify overflow handling.
- startup scan mutation window.
- atomic DB swap while TUI is open.
- large subtree query performance.
- `by-ext` scale behavior.
- WAL growth under long-lived readers.
- memory-bounded scan behavior.
- partial `reconcile_subtree` behavior after budget exhaustion.
