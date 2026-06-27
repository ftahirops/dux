# dux — Verification Audit Report

**Audit baseline:** `CODE_REVIEW.md` findings (C1-C3, H1-H7, M1-M15, L1-L15) against tree `8bbd8a4` (v0.2.1).
**Current audited tree:** `HEAD = dc89d43` (v0.3.0).
**Method:** Every file in `src/` was re-read; verification cites concrete `file:line` evidence from the current code, not commit messages.
**Build status:** `cargo build --release` clean; `cargo test --release` → **9 passed, 0 failed**.

The codebase has materially improved since the review. The two structurally important bug clusters the review identified — **rename semantics across flushes (C1)** and **dirty-state lifecycle (H4/H5/H6/H7)** — are both genuinely resolved with regression tests. A new `guard.rs` resource guardian / self-throttle was added.

---

## (A) Verification of each original finding

### CRITICAL

**C1 — Renames straddling a flush window — FIXED** (via inode pairing, not cookies; regression test passes).
`watch.rs:105-110` introduces `DeferredFrom`, and `flush()` runs a five-phase pipeline (B/C/D/E/F) at `watch.rs:1085-1331`. An unmatched `FAN_MOVED_FROM` is parked in `deferred_from` in Phase C (`watch.rs:1183-1188`); Phase D consumes both in-flush and carry-over sources (`watch.rs:1213-1217`); Phase F expires the rest as a real move-out (`watch.rs:1295-1307`). Regression test `rename_split_across_flushes` (`watch.rs:1725-1781`) asserts the subtree survives a FROM-only flush, follows the TO, and writes zero growth rows.
*Caveat:* the review's literal recommendation — parse `fanotify_event_metadata.cookie` — was *not* taken. The code's comment at `watch.rs:99-100` claims "fanotify carries NO cookie", which is incorrect (the `cookie` field exists and is set for MOVED_FROM/MOVED_TO). The chosen inode-identity pairing is functionally equivalent for the common case but has a theoretical edge: two rapid successive renames of the *same* inode leave `deferred.entry(k).or_insert(...)` keeping the first (older) source path (`watch.rs:1183`), so the second rename can mis-pair. Not exercised by tests; low real-world frequency. See B2 below.

**C2 — `mark_fs` non-UTF-8 mountpoint corruption — FIXED.**
`watch.rs:567-571` now does `use std::os::unix::ffi::OsStrExt; ... CString::new(root.as_os_str().as_bytes())?` — matches `open_dir` (`:537`) and `statfs_fsid` (`:524`). Byte identity preserved.

**C3 — `open_by_handle_at` EPERM silently drops every event — NOT FIXED.**
No startup capability probe exists. `resolve_handle` still returns `None` silently on `dfd < 0` (`watch.rs:709-711`) with no `tracing::error!` and no escalation to `dirty_since`. The only error surfaced is `fanotify_init`'s "need CAP_SYS_ADMIN" hint (`watch.rs:184`); `CAP_DAC_READ_SEARCH` is never mentioned in `src/`, `README.md`, or any service file. A daemon granted only `CAP_SYS_ADMIN` will watch indefinitely with an empty index and no warning.

### HIGH

**H1 — TUI raw-mode leak if `EnterAlternateScreen` fails — FIXED.**
`TermGuard` is constructed (`tui.rs:151`) **before** `enable_raw_mode()?` (`:152`) and `execute!(stdout(), EnterAlternateScreen)?` (`:153`). Comment at `:148-150` documents the ordering. Drop is idempotent (no-op `disable_raw_mode` when not raw).

**H2 — TUI OOB panics on empty `rows` — FIXED.**
All four handlers now use `.get(self.sel)` with an early return: `toggle` (`tui.rs:416-419`), `ascend` (`:432-435`), Right (`:701`), Left (`:711-718`). `rebuild` clamps `sel` to `saturating_sub(1)` (`:292-294`).

**H4 — `dirty_since` never cleared — FIXED (indirectly).**
There is no explicit `DELETE FROM meta WHERE key='dirty_since'`, but the set sites (`watch.rs:232, 395, 620, 1051, 1227`) are all cleared by `rebuild_atomic`, which builds into a brand-new file via `Store::create_fresh` (`store.rs:290-300`, which only sets `schema_version`) and renames it over the old DB (`scan.rs:95`). The fresh file simply has no `dirty_since` row. Both entry points (`dux scan` at `main.rs:205`, SIGHUP at `watch.rs:324`) call `rebuild_atomic`. Test `rescan_clears_dirty` (`watch.rs:1671-1709`) confirms.

**H5 — `would_cycle` skip leaves silent inconsistency — FIXED.**
`watch.rs:1221-1231`: on cycle the `move-to` is skipped (still) but now writes `dirty_since` via `INSERT INTO meta ... ON CONFLICT DO UPDATE` and logs `tracing::warn!`. A subsequent rescan will reconcile.

**H6 — Low-disk pause never recovers — FIXED.**
Replaced the lossy single-shot `dirty_since` pause with a `writes_paused: bool` FSM + `paused_since`/`pause_reason` meta (`watch.rs:277, 411-433`). Recovery: when `pressure != Critical` and `writes_paused` was true, the code logs, `DELETE FROM meta WHERE key IN ('paused_since','pause_reason')`, and clears the flag (`:426-433`). Resumes flushing. `status` (`query.rs:474-484`) and TUI (`tui.rs:498`) surface the *distinct* paused vs dirty states.

**H7 — `pending` unbounded during low-disk pause — FIXED (via backstop).**
The `MAX_PENDING` drop (`watch.rs:390-398`) now explicitly covers the pause case — the comment at `:422-424` states "the MAX_PENDING backstop still bounds memory and escalates to dirty_since if the pause is long and busy." During a pause, events are still read but bounded at 500k; past that, pending + deferred are dropped with `dirty_since`. The review's preferred "skip `read` entirely while paused" was not adopted, but the memory bound is real.

### MEDIUM

**M1 — `by_ext` loads everything into a Rust HashMap — NOT FIXED.**
`query.rs:340-369` unchanged in spirit: still streams all prime-file dirents into a `HashMap<String,(i64,i64)>` (the comment at `:342` was merely clarified to mention hardlink-prime dedup). Memory cost unchanged; the read transaction still held for the whole scan. Documentation/`LIMIT` not added.

**M2 — TUI `update_detail` runs N+1 parent-walk per keypress — FIXED.**
`Row.path` added at `tui.rs:41`, populated by parent-path join in `append_children` (`:369-378`) and `rebuild` (`:256`). `update_detail` is now `fn update_detail(&mut self, _store: &Store)` (`:595-614`) — `_store` is unused; it just clones `r.path`. Zero per-keypress queries.

**M3 — `append_children` re-prepares SQL on every expansion — FIXED.**
`tui.rs:327` uses `store.conn.prepare_cached(&sql)`. Two distinct strings (size vs inodes ORDER BY) cache stably.

**M4 — TUI refresh blocks the event loop — PARTIALLY FIXED.**
No background thread was added; `event_loop` (`tui.rs:617-652`) still runs `rebuild(store)`/`refresh_panels(store)` synchronously. The mitigation is that refreshes are now gated on `last_input.elapsed() >= 250ms` (`:642`) before running, so navigation while actively typing never blocks — the comment at `:622-624` ("navigation never waits on a background refresh") is now actually true. But after ≥250ms of idle, a multi-million-row `rebuild` can still lag the *first* returning keypress. Review's recommendation (background thread + channel) not done.

**M5 — Column width computed in chars, not display columns — PARTIALLY FIXED.**
`fixw` (`tui.rs:807-834`) and `short` (`:842-862`) were rewritten to use `unicode_width::UnicodeWidthChar/Str` and accumulate display columns. **But the footer ellipsis at `tui.rs:1175-1186` still uses `path.chars().count()` and `.chars().rev().take(avail-1)`** — exactly the bug the review asked to fix "Same applies to … the footer ellipsis." A CJK/emoji `detail` path will mis-size `avail` and can overflow the footer line. Also, neither `fixw` nor `short` respects grapheme clusters — a wide trail of a ZWJ emoji sequence can be split (the review noted this as a follow-up). For typical filenames (ASCII, CJK, single emoji, accented Latin) it's correct.

**M7 — `del_subtree` materializes the entire descendant set in RAM — FIXED.**
`watch.rs:770-793` now stages descendants into a `TEMP TABLE _delset` via one recursive CTE INSERT, then issues three set-based DELETEs (`inodes`, `dirents` by child id, `dirents` by parent id) + cleanup. No `Vec<(i64,i64)>` shipped across FFI. FTS rows follow via the `AFTER DELETE` triggers in `store.rs`.

**M8 — 50ms busy-poll — FIXED.**
`watch.rs:349-362` replaced the 50ms `sleep` with `libc::poll(&mut pfd, 1, wait_ms.max(0))` where `wait_ms = flush_every.saturating_sub(last_flush.elapsed())`. Idle ⇒ one wakeup per flush window (2s default); active ⇒ sub-ms. EINTR: `pr < 0` falls through (`pr > 0` guard at `:363`) and the loop's top checks `SHUTDOWN`/`RESCAN` flags, so signals are picked up next iteration — correct, though there is no explicit `EINTR` arm for `poll` (none needed). A theoretical tight-loop risk exists if `fan` ever becomes invalid (`EBADF`) — not reachable in practice since `fan` is held until SHUTDOWN.

**M10 — `reconcile_subtree` budget exhaustion commits partial subtree — PARTIALLY FIXED.**
`watch.rs:1045-1056` still `return Ok(())` after inserting the partial entries and setting `dirty_since`. The review's recommended rollback ("delete the partial entries") or "refuse to insert the parent until the whole subtree fits" was not implemented. `dirty_since` is now a real safety net (cleared on rescan — see H4), so a subsequent `dux scan` reconciles, but a query in the interim still sees an inconsistent partial subtree. The original "silently commits partial" behaviour persists.

**M11 — `growth` cutoff rounds the window *down* — FIXED.**
`query.rs:278-279` now uses `(now - since + GROWTH_BUCKET_SECS - 1) / GROWTH_BUCKET_SECS` (round *up*), with the explanatory comment at `:274-277`. The TUI panel path `refresh_panels` (`tui.rs:528`) and `refresh_growth_map` (`:205`) still floor (they're internal hour-window aggregates, not user-requested durations), so the M11 fix only applies to the `growth`/`--since` path the user-facing window applies; consistent with the review's scope.

**M13 — `find --ext` interprets GLOB metacharacters — FIXED.**
`query.rs:64-77` adds `glob_escape`, applied at `:189` (`let e = glob_escape(e.trim_start_matches('.'))`) before `format!("*.{e}")`. `--ext 'c*'` → `*.[c]*`; `--ext ']'` → `*.]` (literal, valid).

**M12 — `find` GLOB is case-sensitive / short patterns skip trigram — NOT FIXED.**
`query.rs:178-184` still uses bare `name GLOB ?` (case-sensitive) and FTS trigram fallback; the `--name ab`/`--ext c` "no ≥3-char literal" full-scan caveat stands. Review flagged this as "low severity" so consistent with triage, but unchanged.

**M14 — `request_daemon_rescan` re-opens the DB every 500ms — NOT FIXED.**
`main.rs:440-441` still calls `read_last_scan_ts(db)` inside the 500ms poll loop, and `read_last_scan_ts` (`main.rs:382-388`) still does `Store::open_ro(db)` (which runs pragmas + migrate checks) on every iteration — ~3600 opens over the 30-minute timeout. The 30-minute timeout still returns `Ok(())` reporting "still running" silently.

**M15 — `parse_size` f64→i64 rounding — NOT FIXED** (low, acceptable as before).
`main.rs:502-527` still uses `n * mult` then `bytes as i64`.

### LOW

**L1 — `signal` vs `sigaction` — NOT FIXED.** `watch.rs:254-260` and `main.rs:146` still use `libc::signal`. Minor.

**L2 — `statfs_fsid` `transmute` — NOT FIXED.** `watch.rs:531` still `unsafe { std::mem::transmute(v) }`. Review suggested `from_ne_bytes` on a slice. Functionally correct, fragile.

**L9/D9/M9 — `fsfds` not closed on SHUTDOWN — NOT FIXED.** `watch.rs:307` only `libc::close(fan)`; `fsfds` fds still leaked on exit. OS reclaims at process exit, so functionally OK; noted as before. (Also: `fsfds` is not closed on the SIGHUP `rebuild_atomic` reopen path either, but that's intentional since the filesystems don't change — comment was *not* added.)

---

## (B) New bugs / issues introduced by the fixes

### B1. Phase F expiry loses deferred entries on flush failure (narrow race)
`watch.rs:1303-1306`:
```rust
for k in expired {
    if let Some(d) = deferred.remove(&k) {       // <-- Rust mutation NOW
        unlink_dirent(&tx, &mut anc, bucket, d.pdev, d.pino, &d.name)?;  // ? can fail
    }
}
```
`deferred` is mutated *before* the transaction commits. If `unlink_dirent` (or any later `?`) errors, the transaction rolls back (`flush` returns `Err`), `pending` is preserved (cleared only at `:1322` after commit), but the expired `deferred` entry is **gone**. On retry: the FROM was from a *previous* flush and was never in `pending`, so it's now orphaned — the stale dirent at its old path stays in the index while disk has moved it out → index/disk drift, and `dirty_since` is NOT set in this path. Rarity: requires a SQLite error during expiry. Severity: low; would self-heal on the next SIGHUP rescan. The "is the map drained properly" question is answered: drain + commit is not atomic.

### B2. Cookie map (deferred) collision under rapid same-inode renames / incorrect in-code claim
`watch.rs:1183` uses `deferred.entry((cdev,cino)).or_insert(...)` — keyed by inode, not cookie. Two successive renames of the same inode with intermediate flushing keep the *first* source path and discard the second's, so the second `MOVED_TO` can mis-pair with the stale source path. Not a regression (pre-fix the subtree was dropped entirely), but the inode-keyed deferred map is weaker than cookie pairing. And the comment at `:99-100` ("fanotify carries NO cookie") is inaccurate — `fanotify_event_metadata.cookie` exists for rename events; the review explicitly recommended parsing it. Documented limitation or follow-up.

### B3. `oom_score_adj=800` leaks to alert-exec children
`guard.rs:91-93` writes `800` to `/proc/self/oom_score_adj`. The value is inherited by `fork()`/`exec()` children, and `check_alerts` spawns `sh -c $cmd` subprocesses (`watch.rs:1418-1426`). Any user-supplied alert script (especially a long-running one started by accident) inherits the boosted OOM-victim score. Never reverted (not even on shutdown). For a quick-and-die pager this is fine, but the alert exec interface is user-extensible, so this is a subtle footgun.

**Fix:** write `0` (or prior value) back on exit and/or `setrlimit`/`CLOEXEC`-style isolation isn't possible for `oom_score_adj`, so resetting on exit + documenting is the right move.

### B4. Footer ellipsis still uses char count after M5 partial fix
`tui.rs:1175-1186` uses `path.chars().count() > avail` and `.chars().rev().take(avail - 1)`. For a `detail` path with CJK/emoji where the footer width `avail` is in terminal columns, taking `avail-1` *chars* can emit up to `2*(avail-1)` columns and push the footer past the screen edge (wrapping/truncation by ratatui). This is the half of M5 that wasn't completed; it's a *newly visible* inconsistency since `fixw`/`short` were fixed but the footer path wasn't.

### B5. `pressure` double-sample on pause entry
`watch.rs:409` computes `pressure` once, then `:413` calls `crate::guard::sample(&watch_dir).reason(...)` a *second* time (still inside the `if !writes_paused` arm) — re-reading `/proc/meminfo`, `/proc/loadavg`, three PSI files, and `statfs` for a reason that's already implied by the first sample. Cosmetic waste; fix is to keep the first `Health` and call `.reason()` on it.

### B6. `writes_paused` cannot recover while `pending` is empty
The pause/recovery block is entirely inside `if !pending.is_empty()` (`watch.rs:410`). If the host goes Critical (set `writes_paused=true`) then everything drains to zero *without* the host recovering, `pending` becomes empty and the recovery arm (`:425-433`) never runs — `writes_paused` stays true and the `paused_since` meta stays in the DB. Functionally harmless (there's nothing to flush either way) but `status`/TUI will keep saying "WRITES PAUSED" until either an event arrives or the next non-empty cycle sees non-Critical pressure. Worth a tiny unconditional `if writes_paused` clear outside the `pending` guard.

### B7. `paused_since` meta written outside the flush transaction
The `paused_since` meta is written *outside* a flush transaction (`:417-418` via `set_meta` — autocommit). If the daemon is killed between `set_meta("paused_since", …)` and the next flush, the meta persists. Acceptable (status will say "paused" until a non-Critical flush — again B6 territory). No bug; noted.

### B8. Set-based delete preserves the pre-existing hardlink-across-subtree-drop limitation
Not a *new* bug, but the set-based delete preserves the limitation. `del_subtree` (`watch.rs:770-793`) issues `DELETE FROM inodes WHERE (dev_id,inode) IN (SELECT d,i FROM _delset)` and `DELETE FROM dirents WHERE (dev_id,inode) IN (…)` — set semantics identical to the old per-row loop. If a descendant inode has a `dirent` **outside** the subtree (hardlink), that outside dirent is also deleted and the inode row is dropped, even though on disk the outside link survives. `unlink_dirent`'s `remaining == 0` check (`:833-841`) only guards the *root* (cdev,cino) of the delete, not the descendants. This was already true with the per-row implementation, so the M7 fix preserves (does not worsen) the semantics — but document the precondition or add an `EXISTS` guard on the descendant delete. FTS triggers fire correctly (`AFTER DELETE` on `dirents`) and `recursive CTE` descendant identification is correct (`UNION` dedup, depth 4096, `NOT (... self-parent ...)`). Prime-flag handling via `unlink_dirent` (`:853-875`) is unchanged and correct.

### B9. Poll errors other than EINTR would spin silently
`watch.rs:362` ignores negative `poll` return codes — fine for EINTR (intended), but it also means any other `poll` error (e.g. `EBADF` if `fan` were ever closed early, or a future restart-syscall race on a debug build without the `deferred_signal` fix) would spin the CPU with no log. Add a small `{ if EINTR continue; else warn + sleep(1ms) }` arm for defense-in-depth.

### B10. `growth_per_day` uses `delta > 0` filter
`tui.rs:520` filters `WHERE bucket>=? AND delta>0`, so rebalancing writes (deletions in the last hour) are invisible to the "growth/day" extrapolation. Not a new bug, but the new guardian/pause UI surfaced this number more prominently, so the asymmetry is more visible than before.

### B11. No-new-unwraps invariant — preserved ✓
Verified: `grep` for `unwrap()/expect()/panic!/unreachable!/todo!` outside `#[cfg(test)]` returns zero. The new `guard.rs` uses `unwrap_or(0)`/`unwrap_or(0.0)` everywhere; `watch.rs`'s new flush code uses `unwrap_or((0,0))` and `unwrap_or(0)` consistently; `tui.rs` uses `.get(sel)` rather than indexing; `bar`/`fixw`/`short` math is saturating/clamped. `panic=abort` risk remains concentrated in `?`→thread-bubble paths and SQLite error returns, which the fixes handle correctly.

### B12. PSI / OOM-protect robustness — fine
`guard.rs` PSI parsing (`:129-139`) returns 0.0 on any error and only reads the `some` line; `level()` (`:37-53`) treats unknown `mem_avail == 0` as "not critical," so a non-Linux or PSI-disabled host simply classifies as Normal/Elevated from load — gracefully degrades. `MIN_AVAIL_MEM = 256 MiB` mirrors `MIN_FREE_BYTES` correctly. The `Critical && psi_mem10 > 20 || psi_io10 > 40` thresholds aren't tunable but documented in the module header.

---

## (C) Overall assessment

The codebase has materially improved since the review. The two structurally important bug clusters the review identified — **rename semantics across flushes (C1)** and **the dirty-state lifecycle (H4/H5/H6/H7)** — are both genuinely resolved:

- **C1** is fixed by a real deferred-rename mechanism with a passing regression test; the tree no longer vanishes for a flush boundary and zero-delta renames write zero growth rows.
- The **dirty FSM** is now a proper two-state system: `paused_*` (transient, auto-clearing) for host-pressure pauses, and `dirty_since` (sticky until rescan) for genuine event loss; every set site has a matching clear path via `rebuild_atomic` rebuilding from a fresh file.
- **C2/H1/H2/M2/M3/M7/M8/M11/M13** are clean, complete fixes matching the review's recommendations.
- The new **`guard.rs`** is a well-contained, defensively-coded addition (no panics, graceful fallbacks when `/proc/pressure` is absent, sensible thresholds) and the daemon integration correctly gates flushing/WAL-checkpoint/alert-scan on `Pressure::Elevated/Critical`.

### Declared / genuine known gaps remaining

| ID | Status | Gap |
|----|--------|-----|
| **C3** | NOT FIXED | No `open_by_handle_at` capability probe; silent event drop under `CAP_SYS_ADMIN`-only units |
| **M1** | NOT FIXED | `by_ext` still loads the whole prime-file set into a Rust HashMap |
| **M4** | PARTIAL | No background refresh thread; mitigated by 250ms idle gate only |
| **M5** | PARTIAL | `fixw`/`short` fixed; footer ellipsis still char-counted; no grapheme-cluster awareness (see B4) |
| **M10** | PARTIAL | `reconcile_subtree` still commits a partial subtree on budget exhaustion |
| **M12/M14/M15** | NOT FIXED | Low severity, as triaged |
| **L1/L2/L9** | NOT FIXED | Low, as triaged |

### New issues introduced by the fixes (worth queuing a follow-up)

- **B1** — deferred entries lost on flush-expiry error → narrow silent drift (no `dirty_since`); self-heals on SIGHUP rescan.
- **B2** — deferred map keyed by inode, not cookie — rapid same-inode rename edge case; the in-code claim that "fanotify carries NO cookie" is incorrect.
- **B3** — `oom_score_adj=800` inherited by `sh -c $cmd` alert children; never reverted on exit.
- **B4** — footer ellipsis uses char count — the unfinished half of M5 (now a visible inconsistency).
- **B6** — `writes_paused` cannot recover while `pending` is empty (cosmetic but user-visible in `status`/TUI).
- **B5/B9** — minor: double `sample()` on pause entry; `poll` errors other than EINTR would spin silently.
- **B8** — pre-existing limitation (hardlink whose secondary dirent lives in a deleted subtree) is preserved, not worsened, by the set-based delete; worth documenting.

### Net

The audit passed on every CRITICAL and HIGH except **C3** (which is now the most important remaining item — a one-time EPERM probe + README/service-file documentation would close it), and the four HIGH-severity daemon bugs (H4/H5/H6/H7) are genuinely fixed with a regression test covering the rename lifecycle and dirty-clear semantics. The new `guard.rs` introduces thoughtful self-throttling without new panic/unwrap paths. The codebase is in better shape than before; the remaining surface is the M-tier polish and the small set of follow-ups in (B).

---

## Priority next actions (ordered by impact)

1. **C3** — startup EPERM probe + README/service-file docs for `CAP_DAC_READ_SEARCH` (highest-impact remaining gap; complete silent-drop failure)
2. **B2 / B1** — parse the real `fanotify_event_metadata.cookie` for rename pairing; defer expiry in a post-commit step or set `dirty_since` on error. Also fix the incorrect "fanotify carries NO cookie" comment at `watch.rs:99-100`.
3. **B3** — reset `oom_score_adj` (save prior value, restore on exit) to keep alert exec children safe
4. **B4 / B6** — finish M5 on the footer ellipsis; move recovery arm outside the `pending.is_empty()` guard
5. **M1** — push `by_ext` aggregation into SQL or a stored `ext` column
6. **M10** — rollback partial `reconcile_subtree` inserts on budget exhaustion
7. **M4** — background refresh thread feeding the UI via a channel (eliminate even the 250ms-idle first-keypress lag on huge indexes)
8. **B9** — explicit `EINTR` arm in `poll` for defense-in-depth

**Test status:** `cargo test --release` → 9/9 passing, including `rename_split_across_flushes`, `rescan_clears_dirty`, `hardlink_lifecycle`, `non_utf8_names_distinct`, `duplicate_path_prevented`, `pressure_classification`.