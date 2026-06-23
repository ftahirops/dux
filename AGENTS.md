# Repository Guidelines

## Project Structure & Module Organization

This repository contains `xdu`, a Rust 2021 command-line and TUI disk usage indexer. The binary entry point is `src/main.rs`, with feature modules split by responsibility: `scan.rs` for full indexing, `watch.rs` for fanotify daemon updates, `query.rs` for search and reporting, `store.rs` for SQLite access, `tui.rs` for the ncdu-style interface, and `deleted.rs` for deleted-open file detection. Packaging assets live in `packaging/`, currently `packaging/xdu.service`. Design notes are in `docs/superpowers/specs/`.

## Build, Test, and Development Commands

- `cargo build`: compile the debug binary.
- `cargo build --release`: build the optimized `target/release/xdu` binary used for installation.
- `cargo test`: run the Rust test suite.
- `cargo fmt`: format Rust code using rustfmt.
- `cargo clippy --all-targets --all-features`: run lint checks across binaries and tests.
- `cargo run -- scan <PATH>`: build an index locally for manual testing.
- `cargo run -- top <PATH> --dirs`: inspect indexed directory results.

Some runtime features require Linux-specific APIs. The live daemon uses fanotify and may need root or `CAP_SYS_ADMIN`.

## Coding Style & Naming Conventions

Follow standard Rust formatting with 4-space indentation via `cargo fmt`. Use `snake_case` for modules, functions, variables, and file names; use `PascalCase` for structs and enums. Keep modules focused on one responsibility and prefer returning `anyhow::Result` for fallible command paths, matching existing code. Use short comments only to explain non-obvious filesystem, SQLite, or fanotify behavior.

## Testing Guidelines

There are no dedicated test files yet. Add unit tests near the functions they exercise and integration tests under `tests/` when validating CLI behavior. Prefer deterministic tests that operate on temporary directories and avoid scanning host-sensitive paths such as `/proc`, `/sys`, or `/var`. Run `cargo test` before opening a PR; run `cargo clippy --all-targets --all-features` for changes touching shared logic.

## Commit & Pull Request Guidelines

This repository currently has no commit history, so no project-specific commit convention is established. Use concise, imperative commit subjects such as `Add scan exclusion tests` or `Fix daemon flush interval parsing`. Pull requests should include a short description, testing performed, and any operational impact, especially when changing scan behavior, SQLite schema, Linux capability requirements, or the systemd unit.

## Security & Configuration Tips

Avoid committing generated indexes, local database files, or machine-specific service overrides. Treat paths under `/var/lib/xdu` and user data directories as runtime state, not source artifacts.
