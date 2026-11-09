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

Samples 1–7 passed at `14:07:48Z`, `14:12:57Z`, `14:19:07Z`, `14:22:57Z`, `14:27:50Z`, `14:33:07Z`, and `14:37:55Z` (44.888 s, 53.619 s, 123.689 s, 53.491 s, 46.684 s, 63.975 s, and 52.298 s collection time). Sample 3 ran during a full scan. Every sample had zero monitor errors, a 1,702,097-path graph, SQLite `quick_check=ok`, zero kernel overflows, zero userspace drops, zero OOM events, and zero service restarts. Sample 3 completed with freshness age 745–868 seconds during a scan, under the 1,200-second limit. Sample 4 verified the completed scan with age 106–159 seconds and the new 1,705,524-path graph. Sample 6 exercised the in-sample completion case: before status had age 706 seconds and the scan was active; after status showed the new completion timestamp `1794234783` and age 4 seconds. The monitor accepted this newer, independently verified completion under its 1,200-second rule. The daemon's `dirty=true` status reflects scan-first's “filesystem changes await the next full observation” state; the monitor validates completed-observation freshness and independent accounting rather than requiring an empty event queue. Sample 7 observed a changed 1,707,302-path graph and independently verified it with SQLite `quick_check=ok`; freshness was 240–292 seconds. The 24-hour run remains open at **7 passing samples**. Prior r120 samples are not reused.

At 14:21 UTC, VPS2 had 7.5 GiB free and a 129 MiB WAL after scan staging
was cleaned. Continue monitoring disk during the soak.

The canary monitor and observer hashes are recorded in the JSONL. Local copy at this checkpoint: [`vps2-canary-progress.jsonl`](vps2-canary-progress.jsonl). The live log remains on VPS2 at `/root/dux-canary-121/canary-24h-5m.jsonl` and must be recopied for final qualification.

## Local validation

The timestamp-fix source passed 328 Rust release/all-features tests, with 0 failures and 2 ignored; strict Clippy, formatting, and diff checks passed. LLVM coverage was 72.30% line, 71.15% region, and 76.09% function. Branch coverage is unavailable. Coverage is not 100%.

A completed 24-hour canary is still required. Broader OOM, disk-full, multi-day soak, filesystem/kernel, production-schema migration, security threat-model, and controlled production rollout/rollback gates remain open.
