# dux — Deep Code Review

A comprehensive analysis across all 8 source files (`main.rs`, `scan.rs`, `store.rs`, `query.rs`, `watch.rs`, `tui.rs`, `util.rs`, `deleted.rs`).

**Global context:** `Cargo.toml` sets `panic = "abort"` for the release build. Any panic — from `.unwrap()`, out-of-bounds indexing, integer overflow with `debug-assertions` off — terminates the whole daemon or TUI process with no unwinding. Good news: production code contains essentially no `.unwrap()`/`expect()`/`panic!()`/`todo!()`/`unreachable!` (all such calls are in `#[cfg(test)]`). Risk is therefore concentrated in (a) indexing/slice/arithmetic that can panic, and (b) rusqlite errors that `?` to a thread with no handler.

The codebase is genuinely well-engineered — disciplined schema design, careful BLOB/name identity handling, atomic rebuild, RAII guards, documented trade-offs, and zero unwraps in production. Issues cluster in two areas: **rename semantics (C1)** and **the dirty-state lifecycle (H4–H7)**, both in the daemon, and **TUI responsiveness/width (M2, M4, M5, H1, H2)**.

---

## CRITICAL

### C1. Renames that straddle a flush window are mis-handled (lost subtree + spurious growth)
`src/watch.rs:539-548`, `1042-1103`

The fanotify `event->cookie` (the field that pairs `FAN_MOVED_FROM`/`FAN_MOVED_TO` of one rename) is **completely ignored**. Instead, `flush` heuristically matches a moved-from to a moved-to by inode identity *within a single flush* (`moved_in` is built only from `moved_to` in this batch). If the FROM event lands in one flush and the TO event in the next (very common: default `flush_ms=2000`, and renames frequently cross the boundary), the FROM is treated as an out-of-tree deletion and the TO as a fresh insert:

- For a **directory**, Phase C calls `unlink_dirent` → `del_subtree` (drops the entire subtree), then next flush Phase D `upsert_path` + `reconcile_subtree` re-indexes it. Between the two flushes the whole subtree is **absent from the index** (up to 2s), and two big noise rows are written to `growth` (−bytes then +bytes).
- The growth log now records a deletion and a creation for what was a zero-delta rename, polluting `dux growth`, alerts, and the TUI write-rate.

**Fix:** Parse `fanotify_event_metadata.cookie`. Minimally, key pending renames by cookie and pair them across flushes (carry unmatched cookies forward one flush). Better, use `FAN_RENAME` (kernel ≥ 5.13) which gives an atomic old+new record.

### C2. `mark_fs` corrupts non-UTF-8 mountpoint paths → silent miss
`src/watch.rs:482`

```rust
let cpath = std::ffi::CString::new(root.as_os_str().to_string_lossy().as_bytes())?;
```

`to_string_lossy()` replaces invalid UTF-8 bytes with U+FFFD. A mountpoint containing non-UTF-8 bytes (the codebase *elsewhere* goes to lengths to preserve byte identity — see `open_dir:451`, `statfs_fsid:438`, `unescape_mount:413`, `dirents.name BLOB`) is therefore marked at the **wrong path**, so `fanotify_mark` either fails or watches a non-existent path. That filesystem's events are silently lost.

**Fix:** Use `root.as_os_str().as_bytes()` (import `OsStrExt`) like the sibling functions.

### C3. `open_by_handle_at` requires `CAP_DAC_READ_SEARCH`; every event silently dropped otherwise
`src/watch.rs:612-622`

`open_by_handle_at` needs `CAP_DAC_READ_SEARCH` (or `CAP_DAC_OVERRIDE`). `init_fanotify` only needs `CAP_SYS_ADMIN`. A daemon granted only `CAP_SYS_ADMIN` (e.g. via a bounding set or a non-root systemd unit with `CapabilityBoundingSet=CAP_SYS_ADMIN`) will successfully open fanotify and receive events, but `resolve_handle` returns `None` for **every** event → no path is ever resolvable → the index never updates from live events while `dirty_since` is never set (the `None` path is silent, not the overflow path).

**Fix:** Surface the first `open_by_handle_at` EPERM as a fatal/tracing::error at startup (probe one handle), and document the required capability in `--help`/README/service file.

---

## HIGH

### H1. TUI raw mode left enabled if `EnterAlternateScreen` fails
`src/tui.rs:143-145`

```rust
enable_raw_mode()?;
execute!(stdout(), EnterAlternateScreen)?;
let _guard = TermGuard;
```

`TermGuard` is constructed *after* both side-effectful calls. If `enable_raw_mode()` succeeds but `execute!(EnterAlternateScreen)` errors (broken pipe, closed stdout), the function returns via `?` and the terminal is left in **raw mode** with no alternate screen — the user's shell is unusable until `reset`.

**Fix:** Move `let _guard = TermGuard;` to *before* `enable_raw_mode()` (the guard's `Drop` is idempotent; `disable_raw_mode` when not in raw mode is a no-op).

### H2. TUI index-out-of-bounds panics abort the process
`src/tui.rs:386`, `399`, `662`, `673`

```rust
let r = &self.rows[self.sel];            // toggle, ascend, Right, Left
```

`rebuild` clamps `self.sel` to `rows.len()-1` (saturating to 0 for empty), but `toggle`/`ascend`/`handle_key` Right/Left index `self.rows[self.sel]` without a length check. If the tree is empty (root's `recursive_inodes` ≤ 1, or the index root was deleted live and `rebuild` produced zero rows) `self.sel == 0` and `&self.rows[0]` panics → `panic=abort` kills the TUI and raw mode is restored only by the panic hook (which depends on stdout still being writable).

**Fix:** Add `if self.rows.is_empty() { return Ok(()); }` at the top of these handlers.

### H3. (Downgraded after inspection — no action)

### H4. `dirty_since` is set but never cleared
`src/watch.rs:216`, `340`, `1015`, `query.rs:450`

`dirty_since` is written on fanotify overflow, partial mark coverage, MAX_PENDING drop, and low-disk pause. **Nothing ever clears it.** After the transient cause passes (disk frees, a rescan via SIGHUP), the index remains permanently flagged dirty. On `dux status` and the TUI this forever shows "DIRTY — rescan recommended" even though a successful rebuild explicitly reconciles everything.

**Fix:** `rebuild_atomic`/the SIGHUP path should `store.set_meta("dirty_since", "")` (or `DELETE`) after a clean scan, and the low-disk path should clear it when `fs.avail` recovers.

### H5. Cycle-skip in rename leaves index disagreeing with disk
`src/watch.rs:1126-1129`

When `would_cycle` is true, the move-to is skipped silently with `tracing::debug!`. But the rename *did* happen on disk; the index now has the dirent still at the old path and the new path is missing. The comment says this only happens against already-corrupt state, but a single corrupted parent link suffices, and the daemon will never self-heal it — every subsequent operation on that subtree continues against the wrong path.

**Fix:** At minimum set `dirty_since` so a `dux scan` is recommended, instead of leaving the index silently inconsistent.

### H6. Low-disk pause never recovers automatically
`src/watch.rs:1009-1017`

Below `MIN_FREE_BYTES`, `flush` writes `dirty_since` and returns `Ok(())` *every* flush, preserving `pending`. But there's no transition out: even after `fs.avail` recovers, `dirty_since` stays (per H4) and `pending` keeps accumulating toward `MAX_PENDING` (at which point it's dropped — H7 below).

**Fix:** When `fs.avail >= MIN_FREE_BYTES` again, clear `dirty_since` and resume flushing.

### H7. `pending` grows unbounded between low-disk pause and `MAX_PENDING` drop
`src/watch.rs:335-342`, `1010-1017`

In the low-disk path, `flush` returns before the `MAX_PENDING` check (which is in the main loop, after `parse_events`, *before* the flush call). Sequence: events accumulate → flush called → low disk → return Ok, pending preserved → next loop iteration reads more events → `pending.len() > MAX_PENDING` → drops everything and sets dirty. So under sustained low-disk + activity, you silently lose events that could have been retried after free space returns.

**Fix:** Bound pending inside the low-disk branch (drop oldest / set dirty early) or skip `read` entirely while paused.

---

## MEDIUM

### M1. `by_ext` loads every file dirent into a Rust HashMap (unbounded RAM)
`src/query.rs:320-345`

`SELECT d.name, i.blocks FROM dirents d JOIN inodes i ... WHERE i.kind='f' AND d.prime=1` streams the whole prime-file set into a `HashMap<String,(i64,i64)>`. For a 10M-file index this is ~1 GB+ of heap and holds the read transaction for the whole scan. The same aggregation is trivially expressible in SQL with a virtual table / `instr`/`rtrim`/`substr` (SQLite has these), or by storing a normalized `ext` column at scan time.

**Fix:** Push aggregation into SQL or store an `ext` column; at least add a `LIMIT`-style streaming reduce or document the memory cost.

### M2. TUI `update_detail` runs N+1 parent-walk queries on every keypress
`src/tui.rs:553-563`, `store.rs:341-393`

`update_detail` is called at the end of every keypress (`handle_key:653,696`). For `Focus::Tree` it does `store.path_of(r.dev, r.inode)`, which issues one `SELECT` per ancestor — up to 4096 in the worst case, easily 20-50 for typical deep paths. On every `j`/`k`, that's 20-50 round-trips.

**Fix:** Store the full path on each `Row` when `append_children` builds it (the parent's path is already known there). Eliminates the per-keypress cost entirely.

### M3. TUI `append_children` re-prepares SQL on every expansion
`src/tui.rs:306`

`store.conn.prepare(&sql)` (not `prepare_cached`) compiles the children SELECT each time a directory is expanded/refreshed. The SQL only varies by the `order` column (2 variants for `Metric::Size`/`Inodes`).

**Fix:** Switch to `prepare_cached` (the 2 distinct strings will be cached after first use) — meaningful on large indexes where `rebuild` recurses over many expanded dirs every 1200ms.

### M4. TUI refresh blocks the event loop (input lag on big indexes)
`src/tui.rs:603-611`

`app.rebuild(store)` and `app.refresh_panels(store)` run synchronously on the input thread. `refresh_panels` includes a recursive `SUBTREE_CTE` (scoped TUI) + a `SUM` over `growth` + a `PathResolver` walk of up to 60 rows. On a multi-million-node index this easily exceeds 100ms and freezes keyboard input. The comment "navigation never waits on a background refresh" is contradicted by this.

**Fix:** Move refreshes to a background thread that signals the UI thread via a channel, or at minimum make the inner queries cheaper.

### M5. TUI column width computed in chars, not display columns → CJK/wide-char misalignment
`src/tui.rs:766-777` (`fixw`), `779-793` (`short`), and the footer `…tail` path logic `1101-1109`

`fixw` pads to `w` *chars* and truncates by `chars().take(w)`. A CJK/emoji filename occupies 2 terminal columns per char; the fixed-width columns (value 10 + bar 12 + rate 12) then drift right and the tree indent scatters — exactly the failure `fixw`'s comment claims to prevent.

**Fix:** Use `unicode-width`'s `UnicodeWidthStr::width` for both measuring and truncating (truncation must also respect grapheme clusters, but width is the primary fix). Same applies to `short` and the footer ellipsis.

### M6. `SUBTREE_CTE`-via-`IN` re-runs the recursion per outer row in some plans
`src/query.rs:42-54`, used in `top`, `find`, `growth`, TUI panels

`(dev_id,inode) IN (WITH RECURSIVE ...)` — SQLite materializes the CTE once per query, but for `top`/`find` the outer scan over `inodes`/`dirents` tests each row against the materialized set. That's OK; the concern is the lack of an index on the CTE output. For very large subtrees this is a hash join (fine) but the subtree CTE itself walks all descendants, which for `/` is the whole index. For TUI scope=`root`, `panel_scope` short-circuits (empty clause) — good — but `dux top /some/deep/dir` builds the whole subtree set just to filter `inodes`.

**Fix:** Document it or add a `parent_dev,parent_inode`-driven recursive join.

### M7. `del_subtree` / `collect_descendants` materialize the entire descendant set in RAM
`src/watch.rs:675-690`, `694-710`

`collect_descendants` returns `Vec<(i64,i64)>` of every descendant. Deleting a directory with 1M entries allocates 16MB+ and runs 3 DELETEs per row (plus FTS triggers per dirent) inside one transaction — a multi-second TX that blocks the read loop and risks the fanotify queue backing up (mitigated only by `FAN_UNLIMITED_QUEUE`).

**Fix:** Stream the delete: `DELETE FROM dirents WHERE parent IN (recursive subquery)` directly in SQL, avoiding the materialized Vec.

### M8. Daemon main loop is a 50ms poll, not a blocking wait
`src/watch.rs:323-349`

`FAN_NONBLOCK` + `read` + 50ms `sleep` on EAGAIN = 20 wakeups/s with no events, and a worst-case 50ms latency to detect a flush timer expiry.

**Fix:** Use `poll(2)`/`ppoll` on the fanotify fd with a timeout = `flush_every - elapsed`; then you block until either an event or the next flush, get sub-ms event latency when active, and zero wakeups when idle.

### M9. `fsfds` raw fds are never closed on the success exit path
`src/watch.rs:174-205`, `255-285`

`fsfds: HashMap<(i32,i32), RawFd>` stores per-filesystem dir fds. On `SHUTDOWN`, only `fan` is `libc::close`d; the `fsfds` fds are leaked (the OS reclaims them at process exit, so functionally fine, but with `panic=abort` and long-lived processes this is sloppy and races `RLIMIT_NOFILE` for the alert-exec subprocesses).

**Fix:** Drop them explicitly in the SHUTDOWN block. Also: on the RESCAN path, `fsfds` is correctly retained (filesystems don't change), but if `rebuild_atomic` ever moved the db across filesystems the fds would be stale — not a current bug, worth a comment.

### M10. `reconcile_subtree` budget exhaustion marks dirty but commits the partial subtree
`src/watch.rs:962-972`

When the 1M entry budget runs out, it writes `dirty_since` and `return Ok(())` — but the transaction so far has already inserted the budgeted entries; on return, `flush` commits them. So the index has a **partial** subtree (some children present, the rest absent with no dirent) plus a dirty flag. A subsequent query for `top` under that dir would return inconsistent sizes.

**Fix:** Either roll back the subtree portion (delete the partial entries) or refuse to insert the parent until the whole subtree fits.

### M11. `growth` query cutoff rounds the window *down*, expanding the real window
`src/query.rs:256`, TUI `474` (`hour_ago`), `tui.rs:486`

`cutoff = (now - window) / BUCKET_SECS` integer-divides, so the bucket containing `now-window` is fully included; the effective window can be up to ~5 minutes longer than requested. For `dux growth --since 10m` this returns up to ~15m of activity.

**Fix:** Round *up* (`(now-window + BUCKET_SECS-1)/BUCKET_SECS` or use `>= now - window` on `bucket` via multiplication) to honor the requested window, or document the bucketing.

### M12. `find` GLOB is case-sensitive and trigram-FTS only helps ≥3-char substrings
`src/query.rs:160-167`

`names_fts ... WHERE name GLOB ?` is case-sensitive (unlike `find -iname`). And the trigram index is only used for patterns with a ≥3-char literal run; `dux find --name ab` or `--ext c` falls back to a full FTS scan.

**Fix:** Consider `LOWER(name) GLOB LOWER(?)` for case-insensitive default, and/or detect short patterns and fall back to a direct `dirents.name LIKE`/index scan. Low severity but surprising for a locate replacement.

### M13. `find` / `find --ext` allow GLOB metacharacters in user input
`src/query.rs:163-172`

`*`, `?`, `[`, `]` in `--name`/`--ext` are interpreted as glob patterns (this is documented for `--name`). But `--ext` quietly does `format!("*.{e}")` — a user passing `--ext 'c*'` gets `*.*c**` matching arbitrary things, and `--ext ']'` produces `*.]` which is an invalid glob.

**Fix:** Either escape metacharacters in `--ext` (extensions should be literal) or document it.

### M14. `request_daemon_rescan` polls by re-opening the DB every 500ms
`src/main.rs:419-451`, `read_last_scan_ts:378-384`

`read_last_scan_ts` does `Store::open_ro(db)` (which runs `migrate` checks + pragmas setup each call) twice a second for up to 30 minutes → ~3600 DB opens. Each open is cheap-ish but does file I/O and schema introspection.

**Fix:** Open the store once outside the loop and just re-query `get_meta("last_scan_ts")` in-loop. Also the 30-minute timeout silently returns `Ok(())` reporting the rescan "still running" — fine, but the user has no way to Ctrl-C cleanly (the sleep loop ignores EINTR/interrupts; signals default to terminate so Ctrl-C does work, just with no message).

### M15. `parse_size` accepts fractional units but truncates to `i64`
`src/main.rs:498-524`

`1.5G = 1_610_612_736` bytes works, but `bytes as i64` after `f64` math can lose a byte to rounding for huge values, and `bytes >= i64::MAX as f64` guard uses `f64` comparison which is exact only near the boundary. Not a correctness issue for disk sizes (< 2^63), but consider `i128` intermediate or require integer input. Low.

---

## LOW

### L1. `sigaction` would be safer than `signal`
`src/watch.rs:238-244`, `main.rs:144-146`

`libc::signal` semantics are BSD on Linux glibc (no reset), which is fine, but `sigaction` lets you block other signals during the handler and is the portable recommendation. The handlers only touch atomics so they're already async-signal-safe. Minor.

### L2. `statfs_fsid` `transmute` of `f_fsid`
`src/watch.rs:445`

`unsafe { std::mem::transmute(v) }` from libc's `__kernel_fsid_t` to `[i32;2]` is correct (same layout) but fragile against a libc version change. Read the bytes with `from_ne_bytes` off a slice for resilience.

### L3. `set_low_priority` ignores all return values
`src/scan.rs:522-534` — best-effort and documented, but `setpriority` failure (e.g. `EPERM` in a container) means the scan runs at normal priority silently. Acceptable; consider a one-line `tracing::debug!` on failure.

### L4. `last_alert` debounce map is bounded, `alert_children` reaped — good
`src/watch.rs:1257-1261, 1294-1311` — these are correctly handled (MAX_ALERT_CHILDREN, `retain_mut` reaping, `stale_before` eviction). No action; flagged only as a positive.

### L5. `deleted.rs` aggregates correctly but `read_comm` truncation and "(deleted)" heuristics
`src/deleted.rs:41-58` — `link.to_string_lossy().ends_with(" (deleted)")` is a kernel-display-text heuristic; if the kernel ever changes that suffix the detection breaks silently. There's no kernel-stable API, so this is acceptable, but worth a comment. Also `read_comm` reads only the first 16 chars (comm is truncated to 15 by the kernel) — fine.

### L6. `PathResolver` caches are dropped per-query (good) but `full` cache can grow with result set
`src/store.rs:8-82` — bounded by the result set size, not the index; fine for `limit=50` queries, but `find` with a large `limit` (or no limit) accumulates one full-path string per resolved node. For `dux find --name x --limit 100000` this is ~tens of MB. Acceptable; document.

### L7. `scan.rs` parallel channel is unbounded (documented) and peak RAM = all nodes
`src/scan.rs:406-410` — the comment is honest. For 100M-file trees this is multi-GB. A bounded channel with concurrent drain (consumer thread building `dir_idx` while walkers run) would cut peak RAM, but that's a redesign. Note only.

### L8. `Rebuild` foreign-file rename deltas don't generate a `growth` row (intended)
`src/watch.rs:1160` — comment says "rename has zero net byte delta"; correct. No action.

### L9. `upsert_path` directory own-block change records growth on the *directory* inode
`src/watch.rs:873-884` — semantically a directory's own allocation grew; recording it as the inode's growth is reasonable. No action.

### L10. `display_path`/`display_name`/`escape_controls` — good
`src/util.rs:227-257` — correctly escapes C0, DEL, and C1 controls (OSC 52 etc.). One gap: it does *not* strip ESC (`\x1b`) followed by non-CSI bytes, but since all ESC bytes (0x1b) are matched by the `< 0x20` arm and replaced with `\x1b`, that's fine. No action.

### L11. `main.rs` restores SIGPIPE globally
`src/main.rs:142-146` — correct and intentional. Note that with `panic=abort`, a write to a closed pipe will now terminate the process with SIGPIPE rather than panicking — desired. No action.

### L12. `store.rs` migration refuses to wipe incompatible DBs (good); `needs_rebuild` duplicates the schema check
`src/store.rs:215-285` — `migrate` and `needs_rebuild` both probe `meta`+`inodes` independently. If they ever diverge (someone bumps `SCHEMA_VERSION` in one path but not the other), the daemon could rebuild when `open_rw` would have succeeded, or vice versa.

**Fix:** Factor the version probe into a shared helper returning `Option<i64>`.

### L13. `tui.rs` `bar` and `fixw` use `f64`/`usize` round-trips that are safe
`src/tui.rs:737-747` — `.round() as usize` clamped by `.min(width)`. Fine.

### L14. `watch.rs` RESCAN reopens `Store::open_rw` which re-runs `migrate` (re-creates triggers/indexes)
`src/watch.rs:303-306`, `store.rs:215-249` — harmless (`IF NOT EXISTS`), but wasteful: every SIGHUP-driven rescan re-executes `CREATE INDEX`/trigger batches. Guard with an existence check or skip when v4.

### L15. `scan.rs` hardlink "prime" selection is walker-order-dependent
`src/scan.rs:245-260` — the first-seen link is prime; with rayon's non-deterministic order, the prime path chosen for a hardlinked inode varies between scans. `dux find`/`top` resolve via `prime DESC` so they show a stable-but-arbitrary path. Cosmetic; document.

---

## Per-file summary

**main.rs** — Clean. SIGPIPE restore, atomic-rescan-via-daemon orchestration, and `parse_size` are fine. Main issues: M14 (poll re-opens DB), and it shares H1/H2 via the TUI entry path. No `unsafe` concerns beyond the `signal` calls (L1).

**store.rs** — Solid schema design (separate `inodes`/`dirents`, BLOB names, partial indexes, FTS external-content with triggers). `PathResolver` cycle guard (4096) and `path_of` cycle guard are consistent. No leaks; prepared statements are per-query (not cached) but that's by design for the ad-hoc query layer. L12 is the only drift risk.

**scan.rs** — Good separation (parallel walk → bottom-up totals → batched TX → finalize). `ProgressGuard` RAII correctly joins on every path. Hardlink dedup is correct. `set_low_priority` is best-effort (L3). Unbounded channel is documented (L7). The walker-emits-root-as-child duplicate is correctly filtered (`is_root && !self_parented`).

**query.rs** — All SQL uses bound params (no injection). `lim()` correctly clamps negative-as-unlimited. `by_ext` is the memory outlier (M1). `find` GLOB behavior (M12/M13) is surprising. `status` `COUNT(*) FROM inodes` is a full scan but unavoidable without a counter table.

**watch.rs** (largest, most risk) — The structurally important bugs are C1 (rename across flushes), C2 (mark_fs lossy), C3 (capability), plus H4/H5/H6/H7 (dirty-state lifecycle), M7/M8 (subtree RAM / busy-poll), M9/M10. The FFI struct layouts were verified against `/usr/include/linux/fanotify.h` and are **correct** (`metadata_len` is `__u16`, `fanotify_event_info_fid` = header+fsid+handle+name, matching `resolve_handle`'s offsets). The multi-phase flush ordering (deletes deepest-first, upserts shallowest-first, coalesced ancestor writes) is sound; the atomic "copy don't drain then clear after commit" is sound. The dirty-flag/marking logic is the weakest area.

**tui.rs** — H1 (raw-mode leak), H2 (index OOB), M2/M3 (per-key N+1 + re-prepare), M4 (refresh blocks input), M5 (CJK width) are the real UX bugs. The panic-hook + `TermGuard` dual-restore is the right pattern but mis-ordered (H1). The 120ms poll / burst-drain input loop is good; the idle-refresh throttling is good *if* the queries are cheap.

**util.rs** — Clean. `lock_db` (flock EX|NB) is the real mutual-exclusion guard and is correctly held by both scan and daemon. Heartbeat per-db matching (`daemon_live_for`) correctly prevents the global heartbeat from mis-reporting an unrelated index as live. `escape_controls` is thorough.

**deleted.rs** — Correct aggregation by `(pid, dev, inode)`. L5 only.

---

## Top 10 recommendations (ordered by impact)

1. **C1** — Parse `fanotify` rename cookies (or switch to `FAN_RENAME`) so renames work across flush boundaries; today large directory renames vanish for ~2s and pollute growth history.
2. **C3 + H4/H5/H6/H7** — Make `dirty_since` a proper finite state machine: set on transient cause, clear on recovery/successful rescan; surface `open_by_handle_at` EPERM at startup.
3. **C2** — One-line fix: use `as_bytes()` in `mark_fs`.
4. **H1** — Move `TermGuard` creation before `enable_raw_mode`.
5. **H2** — Guard empty `rows` in TUI key handlers.
6. **M2 + M3** — Cache full path on `Row` during `append_children`; use `prepare_cached`. Eliminates per-keypress N+1.
7. **M5** — Use `unicode-width` for `fixw`/`short`/footer so CJK paths don't scatter the tree.
8. **M7 + M8** — Stream `del_subtree` via SQL; replace 50ms poll with `poll(2)` on the fanotify fd.
9. **M1** — Push `by_ext` aggregation into SQL or a stored `ext` column.
10. **M10** — Don't commit a partial `reconcile_subtree`; roll back or hold the parent.