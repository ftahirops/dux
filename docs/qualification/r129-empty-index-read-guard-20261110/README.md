# r129 empty-index publication read guard qualification — 2026-11-10

## Candidate and source checks

- Source commit: `8a8b1e3f6ff4333e2e3cbf7afee1472f303c0ad6`.
- Package version: `0.6.3-129`; release manifest reports a clean source tree.
- The change makes read-only Dux clients refuse an initialized but empty
  unindexed database while the initial scan progress marker is present. This
  prevents a client from opening the target during the empty-index rename
  handoff and creating SQLite sidecars after the watcher checked for them.
- Focused regression test
  `store::tests::open_ro_reports_initial_scan_for_initialized_empty_schema`
  passed.
- `cargo test --all-features`: **339 passed, 0 failed, 2 ignored**.
- Coverage-instrumented all-features run: **339 passed, 0 failed, 2 ignored**.
  Coverage is **72.66% lines** (18,983/26,127), **71.51% regions**
  (30,188/42,218), and **76.25% functions** (1,326/1,739). LLVM branch
  coverage counters were unavailable; ignored tests were not executed.
- Strict Clippy, formatting, and `git diff --check` passed.

## Release artifacts

The release build produced the static binary, DEB, RPM, source archive, debug
symbols, and manifest. The manifest records zero known vulnerabilities in the
audited lockfile. The binary's `--version` and `--help` smoke checks passed;
all eight entries in `SHA256SUMS` verified. Artifact details and checksums
are in `target/readiness/production-r129-20261110/`.

## Isolated runtime smoke on VPS2

The exact r129 static binary ran as root on VPS2 (`192.168.5.106`) inside a
private mount namespace, using a 128 MiB ext4 loop filesystem backed by
`/dev/shm`. The installed service and its database were not modified. Its
systemd state was `reloading` both before and after the smoke, with
`NRestarts=0`; the installed service was not stopped or restarted.

- Event-apply mode initialized and marked fanotify, successfully used
  `open_by_handle_at`, indexed eight file creates and one rename, and reported
  11 events seen / 11 resolved, zero drops, and zero kernel overflow notices.
- Scan-first mode recorded the test change as a hint and published it after a
  manual full observation; it reported one event seen, zero drops, and zero
  kernel overflow notices.
- All three fixture observations reported zero coverage issues and passed
  Dux accounting verification (about 100–107 ms each).
- Captured runtime output SHA-256:
  `b9133d3fa80293fdad56ac0b9a8813e1694ee19792165eb46431a383fc9e045a`.

The fixture is deliberately small. This smoke does not exercise the 256 MiB
WAL guard, production-size publication, package install/upgrade/rollback,
high-churn behavior, exact equality against an independent filesystem walk,
performance under representative load, or a 24-hour canary. It therefore
does not close production qualification.

## Decision and remaining gates

The r129 source regression, local suite, release build, and isolated daemon
smoke passed. **Release decision remains: not production-ready.** The key
remaining gate for the publication change is a marked large-root clean-install
lifecycle with WAL sampled across publication and independent accounting
checks. Sustained churn, overflow recovery, cross-filesystem/kernel coverage,
and 24-hour canary gates also remain open.

Artifacts captured during this run:

- `target/readiness/production-r129-20261110/runtime-smoke.log`
- `target/readiness/production-r129-20261110/service-before.txt`
- `target/readiness/production-r129-20261110/service-after.txt`
- `target/readiness/production-r129-20261110/llvm-cov-summary.json`
- `target/readiness/production-r129-20261110/build-manifest.json`
