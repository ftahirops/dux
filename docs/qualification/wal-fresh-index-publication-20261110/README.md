# Fresh-index atomic publication work — 2026-11-10

## Change made

The r127 lifecycle showed a 717,609,272-byte WAL during clean-install
publication. The scan worker copied its rebuilt database into the daemon's
initialized-but-unpopulated SQLite database in one online-backup transaction;
PASSIVE checkpoints could not progress during that writer transaction.

The daemon now has a gated path for that narrow initial-install case. The
scanner recognizes only a current v7 schema with no root metadata, indexed
inode/path/dirent/totals rows, or growth history. It avoids opening an
unnecessary read connection to that empty source. At publication, the watcher
closes its database connection, checks that SQLite sidecars are gone, and
allows the already-synced staging database to replace the empty database by
rename. It reopens the new database before returning to its event loop. If the
connection cannot close or sidecars remain, publication falls back to SQLite
backup. Populated indexes retain the backup path and reader snapshot behavior.

## Local verification

- `cargo test --all-features`: **338 passed, 0 failed, 2 ignored**.
- Focused fresh-index rename test passed; it verifies the staged index is
  readable and contains the scanned path after replacement, checks that the
  main database inode changed, and verifies that no WAL/SHM/journal sidecar is
  attached to the new database.
- Existing-reader snapshot test passed and asserts the fresh-index gate is not
  selected for a populated index.
- Zero-byte index, recovery-sidecar refusal, and dangling-database-symlink
  regression tests passed.
- `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt
  --all -- --check`, and `git diff --check` passed.

The two ignored tests include an ext4 handle-capability integration test; they
were not rerun in this worktree. Previous r127 coverage (74.13% lines, 73.05%
regions, 77.15% functions; branch counters unavailable) predates this change
and must not be presented as coverage for it.

## Qualification still required

No package or VPS test has yet exercised this change. The decisive test is a
clean-install lifecycle on the marked VPS2 guest that records WAL size and
checkpoint status throughout publication. The pass condition is no 256 MiB
guard activation, a correct readable index, zero coverage issues, passing
accounting verification, no event drops/overflows, and successful service
reopen. Existing r127 evidence does not establish that result. The source
change is not production-qualified, and VPS1 remains unchanged.
