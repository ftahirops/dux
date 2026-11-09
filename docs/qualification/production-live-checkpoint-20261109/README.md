# Production and unsafe-code checkpoint — 2026-11-09

## Production VPS1 read-only observation

At 18:02:28 UTC the host was still running legacy Dux `0.6.3`
(`e0cc915f8f37`), PID 1073891, with four systemd restarts. Status showed the
last completed scan was 364,366 seconds old (about 4.2 days), 636 kernel
fanotify overflow notifications, 2,518,862 dropped updates, 26 pending updates,
and a dirty index. The indexed database was about 3.21 GB on disk. Root had
64,322,830,336 bytes available. The service cgroup reported no OOM or OOM-kill
events. This is a read-only snapshot; no production service or database state
was changed.

Raw command results are in `vps1-readonly-status-20261109T1802Z.json`.

## Current unsafe-block documentation lint

On the current source, `cargo clippy --locked --bin dux --all-features -- -W clippy::undocumented_unsafe_blocks`
completed with **47 production binary warnings**. Promoting that lint to an
error in the all-target build (`cargo clippy --locked --all-targets --all-features -- -D warnings -W clippy::undocumented_unsafe_blocks`)
failed: 83 diagnostics were emitted, including test-target sites and duplicate
compilation contexts. These diagnostics identify missing Clippy-recognized
safety comments; they do **not** demonstrate that those blocks are unsound.
The output has not been counted as a passing check. The production-only and
all-target logs are retained here. A manual unsafe-block audit and appropriate
safety documentation remain open; do not silence the lint globally.
