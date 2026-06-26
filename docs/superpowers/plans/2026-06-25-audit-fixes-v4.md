# dux Audit Remediation — Schema v4 + Correctness/Security Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use `- [ ]` tracking.

**Goal:** Resolve the live findings in `docs/full-code-audit.md` — a hardlink-correct, byte-safe data model (schema v4) plus the correctness, security, and resource fixes the audit prioritizes.

**Architecture:** Split the single conflated `nodes` table into `inodes` (one row per `(dev,inode)`, holds metadata + recursive dir totals) and `dirents` (one row per directory entry/path, `BLOB` name, `UNIQUE(parent_dev,parent_inode,name)`). Hardlinks become multiple `dirents` for one `inodes` row; each inode's blocks are counted once, attributed to a single *prime* dirent's ancestor chain. FTS5 (external-content, trigram) sits over `dirents.name` for search. The daemon and all queries are ported to the new model; a per-DB `flock` serializes writers; flushes abort atomically on any op error; `dirty_since` becomes operationally visible; terminal output is escaped.

**Tech Stack:** Rust, rusqlite (bundled SQLite, FTS5), fanotify (libc), jwalk, ratatui, nix (flock).

## Global Constraints
- On-disk schema bumps to **v4**. `Store::migrate` already refuses an incompatible existing DB and the daemon atomically rebuilds — keep that path; never wipe in place.
- Disk usage = allocated `blocks` (`st_blocks*512`); hardlinked inodes counted **once** (matches `du`).
- Path resolution / scope CTEs keep the existing 4096 depth/cycle guard.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` must all pass at the end.

---

## Design: schema v4

```sql
CREATE TABLE inodes (
    dev_id           INTEGER NOT NULL,
    inode            INTEGER NOT NULL,
    kind             TEXT    NOT NULL,          -- 'f','d','l','o'
    blocks           INTEGER NOT NULL,          -- allocated bytes, counted once
    recursive_bytes  INTEGER NOT NULL DEFAULT 0,  -- dirs only
    recursive_inodes INTEGER NOT NULL DEFAULT 1,  -- dirs only
    uid              INTEGER NOT NULL,
    mtime            INTEGER NOT NULL,
    PRIMARY KEY (dev_id, inode)
) WITHOUT ROWID;

CREATE TABLE dirents (
    parent_dev   INTEGER NOT NULL,
    parent_inode INTEGER NOT NULL,
    name         BLOB    NOT NULL,   -- raw filename bytes (identity-preserving)
    dev_id       INTEGER NOT NULL,   -- target inode
    inode        INTEGER NOT NULL,
    prime        INTEGER NOT NULL DEFAULT 1,  -- 1 = carries this inode's block attribution
    UNIQUE (parent_dev, parent_inode, name)   -- one inode per path
);
CREATE INDEX idx_dirents_target ON dirents(dev_id, inode);
CREATE INDEX idx_dirents_parent ON dirents(parent_dev, parent_inode);

-- ranking indexes on inode metadata
CREATE INDEX idx_inodes_lfiles ON inodes(blocks DESC)          WHERE kind<>'d';
CREATE INDEX idx_inodes_ldirs  ON inodes(recursive_bytes DESC) WHERE kind='d';
CREATE INDEX idx_inodes_linode ON inodes(recursive_inodes DESC) WHERE kind='d';

-- search over every path component (external-content over dirents)
CREATE VIRTUAL TABLE names_fts USING fts5(
    name, content='dirents', tokenize='trigram', detail=none, columnsize=0
);
```

**Totals semantics (well-defined + incrementally maintainable):** every inode's `blocks` are attributed to exactly one *prime* dirent. A directory has exactly one dirent (dirs aren't hardlinked) → its chain is unambiguous. Roll-up sums each inode's blocks into the `recursive_bytes` of every ancestor inode reached by walking *prime* dirent parents. This counts each hardlinked inode once (`du` semantics) and is maintainable under live add/remove by promoting a surviving dirent to prime and moving attribution.

**Name handling:** `dirents.name` is `BLOB` (authoritative identity — distinct non-UTF-8 names never collapse, satisfies the audit). FTS indexes `CAST(name AS TEXT)` via triggers; non-UTF-8 names are simply weakly searchable, but identity/uniqueness is preserved by the BLOB key. Display escapes control chars (see security task).

**Path of an inode:** find any dirent for `(dev,inode)`, take `(parent_dev,parent_inode,name)`, recurse on the parent inode until a root inode (its dirent's parent == itself, or `last_scan_root` stored as the root inode's "name"). The root is represented by an `inodes` row plus a self-referential dirent whose `name` is the absolute root path.

---

## Task ordering (each ends building + green where testable)

### Task 1: Schema v4 in `store.rs`
- Replace `SCHEMA_SQL`, `INDEXES_SQL`, `FTS_TRIGGERS_SQL`, `SCHEMA_VERSION=4`.
- `PathResolver` + `path_of` rewritten to resolve via `dirents` (BLOB name → `String::from_utf8_lossy` for display) joined to inode parent chain.
- `migrate`/`needs_rebuild` check for `inodes` table instead of `nodes`.
- Triggers keep `names_fts` in sync with `dirents` (insert/delete/update of name) using `CAST(name AS TEXT)`.

### Task 2: Scan loader (`scan.rs`) writes the two tables
- `RawNode` unchanged from the walk, but `name` captured as raw bytes (`Vec<u8>` via `entry.file_name().as_bytes()`).
- Phase 2 roll-up unchanged in spirit (primary per inode). Emit: one `inodes` row per primary inode; one `dirents` row per node (including extra hardlinks, `prime=0`).
- Root: insert root inode + self dirent (name = absolute root path bytes).
- `create_fresh`/`finalize_bulk` build FTS + indexes after bulk load.
- Keep the existing hardlink unit test (`scan_blocks_scope_hardlink`) green; add a test asserting **both** hardlink names are searchable and the inode counted once.

### Task 3: Queries (`query.rs`) ported to v4
- `top`, `find`, `growth`, `by_owner`, `by_ext`, `status`, `resolve_scope` read `inodes`/`dirents`.
- `SCOPE_PREDICATE` recurses over `dirents(parent_dev,parent_inode)→(dev_id,inode)`.
- `find` joins `names_fts`→`dirents.rowid`→inode; returns one Row per matching dirent (so every hardlink path is findable).
- `status` shows `dirty_since` and stops claiming "live" when dirty (Task 8 ties in).

### Task 4: TUI (`tui.rs`) ported to v4
- `append_children` enumerates `dirents` under a parent → join inode metadata.
- Growth/largest panels join `inodes`. Show a truncation marker when a directory has >5000 children. Scope the largest/growth panels to the open subtree.

### Task 5: Daemon flush (`watch.rs`) ported to v4 + atomic abort
- Each phase maintains `inodes` + `dirents` with prime-attribution rules (create/modify/hardlink/delete/move incl. prime promotion).
- **Abort the whole transaction on any operation error** (`?` instead of `if let Err(e)=r { debug }`), so `pending` is preserved and retried.
- Add `FAN_ATTRIB` to the mark mask; an attrib-only event re-stats (uid/mtime/blocks).
- Names stored as raw bytes.

### Task 6: Per-DB `flock` (scan + daemon) — `util.rs` + `main.rs` + `watch.rs`
- `lock_db(db) -> Result<File>` opens `<db>.lock`, `flock(LOCK_EX|LOCK_NB)`; held for the operation's lifetime. Scan and daemon both acquire it; failure → clear error ("another dux scan/daemon holds this index").

### Task 7: Graceful SIGTERM flush (`watch.rs`/`main.rs`)
- Install a SIGTERM/SIGINT handler setting an `AtomicBool`; the daemon loop checks it, does a final `flush`, then exits 0.

### Task 8: `dirty_since` operational (`watch.rs`/`query.rs`/`tui.rs`)
- On queue overflow, set `dirty_since`. `daemon_live_for` stays, but `status`/TUI render a "DIRTY — missed events, rescan recommended" banner and the daemon does not advertise plain "live" while dirty.

### Task 9: Daemon root validation (`watch.rs`)
- Before watching an existing index, compare `root_dev`/`root_inode` to the requested root; on mismatch, rebuild atomically (already the path when `needs_rebuild`) or bail.

### Task 10: Terminal-output escaping (security) — `main.rs` + `tui.rs`
- `util::display_path(&str) -> String` escaping C0/C1 controls, DEL, ESC, newline as visible `\xNN`/`\n`. Apply to every path print in CLI and TUI rows.

### Task 11: Bounded alerts + durable rebuild + mount-failure reporting
- Cap concurrent alert children; reap them; bound `last_alert` (prune stale). `fsync` the new DB file + parent dir before/after rename in `rebuild_atomic`. Log (and set dirty) when a mount mark fails.

### Task 12: Misc + housekeeping
- Remove unused `read_heartbeat` (clippy). Fix `RUSTSEC` advisories by bumping `ratatui`/`lru` if a compatible release exists, else document. Update `dux-verify.sh` (check `dirents` UNIQUE, negative totals, `dirty_since`, per-db liveness, drop `ROCK SOLID`). Update architecture docs. `cargo fmt`.

### Task 13: Test suite
- Integration tests under `tests/`: hardlink lifecycle, duplicate-path prevention (UNIQUE), partial flush failure preserves pending, dirty-state, non-UTF-8 name identity, terminal escaping, schema-mismatch rebuild.

---

## Notes
- Commit after each task that builds + passes tests.
- The daemon's incremental prime-attribution is the highest-risk piece — test it with deterministic flush-level unit tests that drive `pending` maps directly rather than real fanotify.
