# Release 121 scan-completion timestamp qualification — 2026-11-09

## Build and package

- Source commit: `8b02799b41323cee7ed8c0f615a7b89d3aa33f54`.
- Clean source tree, source digest `5d6fed78393396a1bf63f424d6cdf03a26bf8511701499c5fa9c72bb540ded05`.
- Package: `dux 0.6.3-121`, built by `scripts/build-release.sh` as static musl.
- Static binary SHA256: `b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`.
- DEB SHA256: `091930e93fd36dd7a93cdc7983ded6e893c207035b66d5d010cec585bde81d57`.
- RPM SHA256: `c743faf3aef3bfc272108e843417c13b4862b6e5ab891379142c4af2dca2fa0c`.
- The release script verified the embedded source digest, static linkage, package payload identity, checksums, dependency metadata, and both RustSec lockfile and exact-binary audits. The lockfile report has zero vulnerabilities and zero warnings; the binary audit found auditable dependency data for 202 dependencies.
- The first packaging attempt failed its source-digest check because the build log was written inside the checkout while source hashing ran. It produced no package; its initial shell pipeline also masked the script's nonzero exit status because `tee` was used without `pipefail`. The failure was detected from the explicit log error and missing success-path artifact checks. The successful retry used a committed, clean source tree, with build outputs and the live build log outside the checkout. The failed attempt is retained in the neighboring `scan-first-pressure` qualification files.

Artifacts: [`build-manifest.json`](build-manifest.json), [`SHA256SUMS`](SHA256SUMS), [lockfile audit](cargo-audit-lockfile.json), [binary audit](cargo-audit-binary.log), and [successful build log](dux-r121-build.log).

## VPS2 installation and rollback

VPS2 (`192.168.5.106`, Ubuntu 24.04, kernel 6.8, ext4) was running package 0.6.3-120. Before upgrade, a root-only rollback copy was created at `/root/dux-canary-120/rollback-release120/` containing the r120 binary, DEB, service unit, and a SQLite online backup of `/var/lib/dux/dux.db`.

The backup is schema 7, 696,860,672 bytes, SHA256 `ab51ac276398c06800372cf387cac23b7018dc3be2fcd13b494a60fff51b1f54`, and passed SQLite `quick_check`. The rollback binary has SHA256 `67bf2fa27680c014ebc3623adb16b6e435eef83ed5e92dad68eae909cc302806`; the old DEB is also retained. Production VPS1 was not changed.

DEB 0.6.3-121 installed successfully. `/usr/bin/dux` matches the expected static binary hash; systemd is active with zero restarts; the packaged unit hash is `e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`. Post-install root reconciliation covered 1,698,615 inode rows and 1,702,097 path instances. It completed without kernel overflows or dropped userspace events. Independent accounting passed in 58.337 seconds; SQLite and path-graph checks passed. Critically, the status timestamp advanced from the old scan-start value `1794232455` to `1794233078`, which corresponds to the new scan completion, while the scan began at `1794232765`. This is runtime evidence that the timestamp field now records completion rather than start.

`systemd-analyze verify` passed for both packaged units. The installed `dux.service` scored 3.3 (OK) in `systemd-analyze security`; this is policy evidence, not proof of containment after a root-level compromise.

The first scheduled r121 observation started at timestamp `1794233665` and
completed at `1794234017` (about 5m52s). `last_scan_ts` advanced to the
completion timestamp only after publication; it did not retain the start value.
The scan covered 1,524,035 files, 181,489 directories, and 1,705,524 staged
path instances with zero coverage issues. Independent accounting passed in
54.822s. Kernel overflows, userspace drops, and daemon restarts remained zero.
The database returned to its post-scan 129 MiB WAL state and VPS2 had 7.5 GiB
free at 14:21 UTC. See the [scheduled-scan journal](scheduled-scan-1-journal.log)
and [14:21 status snapshot](vps2-status-20261109T1421Z.json).

Rollback manifest: [`rollback-db-manifest.json`](rollback-db-manifest.json).
Post-install evidence: [service journal](postinstall-journal.log) and
[14:10 status snapshot](vps2-status-20261109T1410Z.json), [14:13 status snapshot](vps2-status-20261109T1413Z.json), [14:20 status snapshot](vps2-status-20261109T1420Z.json).

## Fresh VPS2 canary

A new 24-hour read-only canary started at `2026-11-09T14:07:03Z`, using five-minute samples and the intended 1,200-second completed-scan freshness limit. It checks the exact binary and unit hashes, indexed path graph, SQLite integrity/accounting, journal errors, resource state, restarts, overflows, drops, OOM events, and history continuity. The monitor is constrained to 20% of one CPU, 1 GiB RAM, no swap, IOWeight 10, and Nice 19; the daemon retains its packaged 100% CPU / 4 GiB limit.

Samples 1–20 passed at `14:07:48Z`, `14:12:57Z`, `14:19:07Z`, `14:22:57Z`, `14:27:50Z`, `14:33:07Z`, `14:37:55Z`, `14:42:52Z`, `14:48:37Z`, `14:52:56Z`, `14:58:34Z`, `15:03:11Z`, `15:07:54Z`, `15:12:50Z`, `15:18:57Z`, `15:23:37Z`, `15:27:50Z`, `15:33:09Z`, `15:37:56Z`, and `15:43:09Z` (44.888 s, 53.619 s, 123.689 s, 53.491 s, 46.684 s, 63.975 s, 52.298 s, 49.206 s, 93.657 s, 53.294 s, 90.983 s, 68.250 s, 51.385 s, 47.385 s, 114.127 s, 94.389 s, 46.803 s, 65.909 s, 53.293 s, and 65.503 s collection time). Every sample had zero monitor errors, a valid indexed path graph, SQLite `quick_check=ok`, zero kernel overflows, zero userspace drops, zero OOM events, and zero service restarts. The earlier active-scan samples and in-sample completion at sample 6 remain detailed above. Sample 15 overlapped the fourth scheduled scan: it observed ages 823–936 seconds while the scan was active and remained below the 1,200-second ceiling. The scan then completed at 15:19:03 (`1794237543`) after starting at 15:14:26 (`1794237266`), with 1,533,991 files, 182,758 directories, and 1,716,749 staged paths; coverage issues were zero and independent accounting passed in 68.969 seconds. See [`scheduled-scan-4-journal.log`](scheduled-scan-4-journal.log). Sample 16 passed at 274 seconds freshness on the 1,716,749-path graph; daemon memory was 248.5 MiB current and 1.75 GiB peak, with a 4 GiB service limit. Sample 17 passed at 527 seconds; sample 18 passed just after the next scan completed, at 4 seconds freshness on the 1,716,749-path graph. Sample 19 passed at 291 seconds freshness and sample 20 at 604 seconds, both with 1,719,379 paths. The 24-hour run remains open at **20 passing samples**. Prior r120 samples are not reused.

At 15:04 UTC, VPS2 had 7.4 GiB free and a 129 MiB WAL after scan staging
was cleaned. Continue monitoring disk during the soak.

The canary monitor and observer hashes are recorded in the JSONL. Local copy at this checkpoint: [`vps2-canary-progress.jsonl`](vps2-canary-progress.jsonl). The live log remains on VPS2 at `/root/dux-canary-121/canary-24h-5m.jsonl` and must be recopied for final qualification.

## Local validation

The timestamp-fix source was revalidated on 2026-11-09: the release/all-features suite passed 328 tests, 0 failed, and 2 were ignored; strict all-target/all-feature Clippy, formatting, and diff checks passed. The ignored ext4 stale-parent end-to-end test was also explicitly run and passed on the local ext4 host; it exercised deleted indexed parents, inode-generation mismatch, orphaned index rows, and freed never-indexed directories. Logs: [`current-release-tests-20261109.log`](current-release-tests-20261109.log), [`current-static-checks-20261109.log`](current-static-checks-20261109.log), and [`stale-parent-ext4-integration-20261109.log`](stale-parent-ext4-integration-20261109.log).

The explicitly ignored reconciliation scale test passed with 1,000,000 file entries on the local host. It measured a 62.029 s initial build and an 83.181 s unchanged in-place reconciliation; the DB remained 321,794,048 bytes. This demonstrates that no-op reconcile still pays a full tree-walk cost and took about 34% longer than the initial build in this run. This was not a constrained-CPU or same-host VPS2 measurement, so it is a diagnostic result, not a production SLO claim. Full output is in [`scale-1m-20261109.log`](scale-1m-20261109.log). Previously measured LLVM coverage is 72.30% line, 71.15% region, and 76.09% function. Branch coverage is unavailable. Coverage is not 100%.

A completed 24-hour canary is still required. Broader OOM, disk-full, multi-day soak, filesystem/kernel, production-schema migration, security threat-model, and controlled production rollout/rollback gates remain open.

## Follow-up validation — 2026-11-09 15:49 UTC

After the earlier qualification snapshot, `src/watch.rs` received a narrow
resource-safety cleanup: two manually closed file descriptors now use the
existing `RawFdGuard`, and SAFETY comments document ownership and FFI buffer
invariants around the related handle operations. The full release/all-features
suite was rerun against this source, including ignored tests:

```text
cargo test --locked --release --all-features --bin dux -- --include-ignored --test-threads=4 --nocapture
330 passed; 0 failed; 0 ignored; finished in 32.54s
```

This includes the ext4 stale-parent integration and 100,000-entry scale smoke
profiles. The latter reported 4.406s baseline, 3.898s unchanged reconciliation,
and an unchanged 32,505,856-byte database. Strict all-target/all-feature
Clippy (`-D warnings`), `cargo fmt --all -- --check`, and `git diff --check`
passed after the edits. Fresh LLVM coverage over the same 330-test run measured
**73.65% lines, 72.55% regions, and 76.80% functions**; branch coverage is
unavailable in this instrumentation. The weakest modules are `tui.rs` (39.49%
lines), `main.rs` (43.61%), and `containers.rs` (67.25%). Full logs are
[`current-release-tests-all-including-ignored-20261109.log`](current-release-tests-all-including-ignored-20261109.log),
[`current-static-checks-raii-20261109.log`](current-static-checks-raii-20261109.log),
and [`current-coverage-all-including-ignored-20261109.log`](current-coverage-all-including-ignored-20261109.log).

The 24-hour VPS2 canary remains active. At 15:48:09 UTC, sample 21 passed;
scan age was 6 seconds, with zero monitor errors, kernel overflows, userspace
drops, OOMs, and service restarts. VPS2 had 7.2 GiB free. The updated live
JSONL is preserved as [`vps2-canary-progress.jsonl`](vps2-canary-progress.jsonl).
This is about 1h41m of a 24-hour run, not a completed soak.


## Debug, Python, and 1.37M scale follow-up — 2026-11-09 16:00 UTC

The all-features debug suite with ignored tests enabled passed **330/330** in
42.05s. Its 100,000-entry scale smoke measured 8.838s baseline and 10.294s
unchanged reconciliation, with database size unchanged at 31,641,600 bytes.
The complete Python harness discovery also passed **354/354** in 9.861s
(`PYTHONDONTWRITEBYTECODE=1`, scratch under `/dev/shm`).

The current release binary/source then passed the isolated **1,370,000-entry**
scale smoke: baseline 63.017s, unchanged in-place reconciliation 74.441s, and
an unchanged database size of 441,102,336 bytes. Unchanged reconciliation was
about 18.1% slower than the initial baseline in this run. It does not include
constrained-CPU limits or repeat variance, so it is a diagnostic measurement,
not an SLO qualification. The fixture was removed by test cleanup; local free
disk returned to 57 GiB. The 10M profile remains unrun because its real-file
fixture would consume most of available scratch capacity.

At 15:58:04 UTC the VPS2 canary had **23 passing samples**. The latest sample
reported 600s scan age, zero errors/overflows/drops/OOMs/restarts, and the
service remained running. During an active scan-first observation systemd
reports `ActiveState=reloading` by the daemon's existing notify protocol; the
monitor accepted it only because the status proved an observation in progress.
This remains an approximately 2-hour checkpoint, not the required 24-hour
soak. Logs: [`current-debug-tests-all-including-ignored-20261109.log`](current-debug-tests-all-including-ignored-20261109.log),
[`current-python-discovery-20261109.log`](current-python-discovery-20261109.log),
[`current-scale-1370k-20261109.log`](current-scale-1370k-20261109.log), and the
updated canary JSONL above.
