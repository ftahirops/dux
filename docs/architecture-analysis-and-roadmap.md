# xdu Architecture Analysis and Developer Specification

## Scope of This Review

This document analyzes the repository as it exists now. It is based on the Rust
source in `src/`, `Cargo.toml`, `README.md`, `packaging/xdu.service`, and
`docs/superpowers/specs/2026-06-22-xdu-design.md`. It also uses observed CLI
behavior from the local debug binary after `cargo build`.

Commands run during review:

- `cargo build`
- `cargo test`
- `cargo clippy --all-targets --all-features`
- `target/debug/xdu --help`
- `target/debug/xdu scan --help`
- `target/debug/xdu top --help`
- `target/debug/xdu find --help`
- `target/debug/xdu growth --help`
- temporary scan and query checks using `--db /tmp/xdu-analysis/xdu.db`

No benchmark was run, so this document does not claim measured performance.

## Executive Summary

`xdu` is a compact Rust CLI/TUI prototype for persistent disk usage and file
name search. The core architecture is reasonable: one binary, one SQLite WAL
database, direct read-only queries from CLI/TUI, and write-side updates from
scan or daemon code. The project has a useful foundation, but it does not yet
fully replace `du`, `ncdu`, `find`, `locate`, `mlocate`, or `updatedb`.

Current strengths:

- Simple deployment model: one Rust binary plus optional systemd unit.
- SQLite WAL is a pragmatic storage choice for local indexed queries.
- `nodes`, `changes`, and `names_fts` tables map cleanly to the product idea.
- The full scan path works for a single tree.
- The TUI can browse the indexed tree.
- The daemon contains real fanotify code and coalesces pending writes.

Current blockers:

- Path arguments for several commands are either missing or ignored.
- The index stores only one tree because each scan deletes all existing nodes.
- Live freshness is partial: regular-file create/modify is handled, but delete,
  rename, directory creation, metadata changes, and overflow recovery are not.
- Disk accounting mixes allocated block totals and apparent file sizes.
- Hardlinks and multiple path names for the same inode are not represented
  correctly for locate/find-style use.
- There are no tests; `cargo test` reports zero tests.
- Documentation overstates implemented capabilities.

## Current Architecture

### CLI Layer

`src/main.rs` defines all subcommands with `clap`. Implemented commands are:

- `scan`
- `top`
- `find`
- `growth`
- `deleted-open`
- `by-owner`
- `by-ext`
- `tui`
- `status`
- `daemon`

The CLI opens a `Store` in read-write mode for `scan` and `daemon`, and
read-only mode for query/TUI commands. This is a good separation.

Important issue: some parsed path arguments are discarded. `top`, `growth`,
`tui`, and the bare top-level `[PATH]` accept paths in the CLI grammar, but the
match arms do not pass those values into query or TUI logic. `find` accepts no
path argument at all. This makes the user interface look more capable than it
is.

### Storage Layer

`src/store.rs` creates a SQLite database with:

- `meta`: string key/value metadata.
- `nodes`: inode-oriented file tree rows.
- `changes`: rolling change log for growth queries.
- `names_fts`: FTS5 trigram index over base names.

The database is opened with WAL and read-only query clients use a busy timeout.
That is the right high-level concurrency model for this project.

The main schema risk is the `nodes` primary key:

```sql
PRIMARY KEY (dev_id, inode)
```

This collapses all hardlinked names for one inode into one row. That may be
acceptable for inode accounting, but it is not acceptable for a locate/find
replacement, where each path name matters.

### Scan Layer

`src/scan.rs` recursively walks a single root using `std::fs::read_dir` and
`symlink_metadata`. It skips common pseudo-filesystems unless
`--include-pseudo` is provided. It supports `--one-file-system`, `--exclude`,
and a best-effort low-priority mode.

The scanner computes recursive directory totals bottom-up and inserts rows in a
single transaction. That is simple and robust for an MVP.

The scanner is not yet what the design doc describes:

- It is not threaded.
- It does not use `getdents64` or `statx` directly.
- It deletes the entire existing index at scan start.
- It does not maintain multiple roots or a global updatedb-style database.
- It stores only one name per inode.

### Query Layer

`src/query.rs` implements global queries over the index:

- `top`: largest directories or files.
- `find`: name/ext/newer/larger/uid filters.
- `growth`: sums rows from `changes`.
- `by_owner`: groups file sizes by uid.
- `by_ext`: groups file sizes by extension in Rust.
- `status`: reports root, indexed count, total, and scan age.

The query layer currently has no subtree filtering. For example, `xdu top
/some/path --dirs` should report largest directories under `/some/path`, but
the current query ranks all indexed directories globally.

Search is also limited. It searches base names, not full paths, and cannot
express common `find` predicates such as type, permissions, group, regex,
mtime/ctime/atime range pairs, depth, filesystem boundary, or command actions.

### Daemon Layer

`src/watch.rs` uses fanotify mount marks and records latest pending regular-file
state by inode. On flush, it updates existing rows, inserts newly created files,
updates ancestor recursive totals, logs changes, and updates `names_fts` for new
files.

This is a useful starting point, but it is not a complete live index:

- Only `FAN_MODIFY` and `FAN_CLOSE_WRITE` are marked.
- Deletes are not marked as deleted.
- Renames are not tracked.
- Directory creation is not inserted.
- Directory deletion is not reconciled.
- Metadata-only changes are not handled.
- Queue overflow handling is absent.
- There is no dirty-subtree marker or automatic reconcile path.
- The code does not verify that the daemon root matches the scanned root.

The delta math is also inconsistent. Scan totals use allocated blocks for
recursive disk usage, while modify events use apparent size deltas
(`p.size - old_size`) and new-file events use blocks. This can make growth and
recursive totals drift.

### TUI Layer

`src/tui.rs` builds a ratatui browser using indexed rows. It supports movement,
entering directories, going up, sorting by size/growth/name, and periodic
refresh.

Useful foundation:

- Browses from the indexed root.
- Shows top growth and largest file panels.
- Refreshes periodically.

Limitations:

- It ignores CLI path arguments and always starts at indexed root.
- It reads "live" from the DB, but the DB is only partially live.
- It uses Unicode symbols and emoji-like labels in output; that may be fine for
  terminals, but should be tested across target environments.
- It has no tests or snapshot coverage.

## What It Actually Replaces Today

### `du`

Partially. It can scan one directory tree and later answer largest-directory or
largest-file queries without rescanning. It does not yet match `du` semantics
across all cases because disk accounting is inconsistent, hardlinks are not
handled explicitly, path scoping is missing, and multiple roots are not
supported.

### `ncdu`

Partially. The TUI can browse the indexed tree. It does not yet behave like
`ncdu <path>` because command path arguments are ignored and the TUI always
starts at the indexed root.

### `locate` / `mlocate` / `updatedb`

No. It has a fast name index for one scanned tree, but it is not a global path
database. It does not maintain multiple roots, does not preserve all hardlink
path names, and does not fully reconcile live deletes/renames.

### `find`

No. It implements a small indexed subset of find-like predicates. It is useful
for base-name, extension, size, mtime-window, and uid queries, but it is not a
general `find` replacement.

## Code Quality Assessment

The code is readable and reasonably modular for the current size. The core
files have clear responsibilities and the use of `anyhow::Result` is consistent
enough for a CLI project.

Main quality concerns:

- No automated tests.
- Some CLI options are accepted but not honored.
- Docs and design notes describe features not implemented.
- Some dependencies are unused or not reflected in code paths (`rayon`,
  `crossbeam-channel`, and parts of `nix`).
- `cargo clippy --all-targets --all-features` reports warnings. They are mostly
  style warnings, not architectural blockers.
- Error accounting in scan is intentionally lossy: permission errors increment a
  count but do not preserve paths or reasons.
- Schema versioning stores a version but does not perform actual versioned
  migrations.

## Required Product Decisions

Before fixing implementation details, the project needs firm definitions for:

1. Does `xdu` report apparent size or allocated disk usage by default?
2. Should queries be rooted in a single scanned tree or support a global index?
3. Should hardlinks be counted once for disk usage but listed multiple times for
   path search?
4. Is live freshness best-effort, or must stale regions be marked explicitly?
5. Is the daemon responsible for scanning, or only for updating an already
   scanned database?

Recommended answers:

- Use allocated blocks as the default disk-usage metric.
- Store apparent size separately and expose it with an option later.
- Support multiple scan roots in the schema, even if the first UI only shows one.
- Represent names/paths separately from inode metadata.
- Mark stale/dirty subtrees when live correctness cannot be guaranteed.

## Developer Specification: Correctness-First Architecture

### Storage Model

Replace the single `nodes` concept with separate inode and path concepts:

- `roots(root_id, path, dev_id, root_inode, last_scan_ts, dirty)`
- `inodes(dev_id, inode, kind, size, blocks, uid, gid, mode, mtime, ctime,
  nlink, last_seen, deleted)`
- `paths(root_id, dev_id, inode, parent_path_id, name, full_path or path_key,
  last_seen, deleted)`
- `changes(ts, root_id, dev_id, inode, path_id, blocks_before, blocks_after,
  size_before, size_after, delta_blocks, delta_size, event_type)`
- `names_fts(name, path_id, root_id)` or an FTS table over full paths plus base
  names.

Acceptance criteria:

- Hardlinked files can appear at multiple paths.
- Disk usage can count hardlinked blocks once per device/root where required.
- Search can return every path name.
- A scan of one root does not delete unrelated roots.

### Query Semantics

Every user-facing path argument must be meaningful.

Required behavior:

- `xdu top PATH --dirs`: rank directories under `PATH`.
- `xdu top PATH --files`: rank files under `PATH`.
- `xdu find PATH --name '*.log'`: search only under `PATH`.
- `xdu growth PATH --since 1h`: show growth under `PATH`.
- `xdu tui PATH`: open the TUI at `PATH`, not always at database root.
- `by-owner` and `by-ext` should either accept a path or document that they are
  global.

Acceptance criteria:

- A query under `/a` must not return rows under sibling `/b`.
- A query outside the scanned root should fail with a clear message.
- Help text, README examples, and behavior must match.

### Scanner

The first correctness target is not maximum speed. It is a trustworthy scan.

Required fixes:

- Preserve multiple roots.
- Store all path names for hardlinks.
- Normalize and canonicalize excludes before matching.
- Record scan errors with path and error kind in a table or structured log.
- Keep `last_scan_ts` per root, not only globally.
- Decide and consistently use `blocks` or `size` for every disk-usage total.

Performance work after correctness:

- Add parallel directory traversal only after tests prove single-threaded
  behavior.
- Consider `jwalk`, `ignore`, direct `getdents64`, or rayon-backed traversal
  after profiling.
- Add measured benchmarks before claiming millisecond query latency.

### Daemon and Freshness

Live tracking must never silently imply full freshness when it is partial.

Required fixes:

- Track delete and move events if using fanotify modes that support them on the
  target kernels, or explicitly mark affected roots dirty.
- Insert directories on creation before inserting child files.
- Recompute parentage safely when parent directories are unknown.
- Use consistent `delta_blocks` and `delta_size` fields.
- Detect queue overflow and mark the root or subtree dirty.
- Refuse or warn when daemon root differs from scanned root.
- Add a reconcile command such as `xdu reconcile PATH` or make `scan` update a
  root incrementally.

Acceptance criteria:

- Create, grow, shrink, delete, and rename a file under a scanned root; status
  and queries must either be correct or report dirty/stale state.
- New files under new directories become searchable with a valid path.
- Recursive totals do not drift after repeated write/truncate cycles.

### TUI

Required fixes:

- Honor `xdu tui PATH` and bare `xdu PATH`.
- Show stale/dirty state clearly when the daemon cannot guarantee freshness.
- Avoid saying "live" unconditionally.
- Add basic rendering or state tests for navigation and query selection.

### Documentation

README and design docs must distinguish:

- Implemented now.
- Planned.
- Known limitations.
- Measured performance, if any.

Remove or qualify claims such as:

- "replaces `du`, `ncdu`, `locate`, and `find`"
- "always-fresh"
- "`find PATH`" if path scoping is not implemented
- "threaded walk via getdents64/statx" until it exists
- `Type=notify` in design notes unless the service actually uses it

### Testing Plan

Add tests before large refactors.

Minimum test set:

- Unit tests for `parse_duration` and size parsing.
- Scanner tests using temporary directories.
- Exclude matching tests.
- Path reconstruction tests.
- Hardlink tests.
- Query path scoping tests.
- FTS name search tests.
- Growth delta tests for create/grow/shrink.
- CLI smoke tests using a temp database.

Suggested integration scenarios:

1. Scan a temp tree with `/a` and `/b`; verify `top /a` never returns `/b`.
2. Create hardlinks and verify search returns both names while disk usage does
   not double-count unexpectedly.
3. Scan, modify a file through the daemon, and verify both file and ancestor
   totals.
4. Delete or rename a file; verify either correct reconciliation or dirty-state
   reporting.

## Suggested Implementation Order

1. Fix documentation to reflect current behavior, or mark roadmap features as
   planned.
2. Add integration test harness with temp dirs and temp DBs.
3. Implement path scoping for `top`, `growth`, `tui`, and bare path.
4. Add a path argument to `find` or remove path examples from docs.
5. Normalize disk accounting around `blocks` for disk usage and `size` for
   apparent size.
6. Add hardlink/path schema support.
7. Convert full scan from destructive global reset to per-root update.
8. Add dirty-state tracking for daemon limitations.
9. Expand daemon event coverage and reconciliation.
10. Benchmark scanner and query performance; only then make performance claims.

## Non-Goals for the Next Milestone

Avoid adding eBPF, a web UI, distributed indexing, or a background scheduler
until the local correctness contract is stable. The current project will get
more value from path correctness, schema clarity, and tests than from more
features.

## Bottom Line

The project is a solid prototype, not a finished replacement for classic disk
usage and file search tools. The strongest foundation is the simple SQLite WAL
architecture. The highest-priority fixes are path-scoped queries, truthful docs,
test coverage, consistent disk accounting, hardlink/path modeling, and explicit
freshness state for daemon gaps.
