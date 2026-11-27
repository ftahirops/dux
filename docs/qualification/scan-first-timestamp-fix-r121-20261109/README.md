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

## Local-host test-impact disclosure — 2026-11-09 16:05 UTC

The 1.37M fixture was mistakenly run on the checkout host rather than VPS2. A
read-only check afterward confirmed that this host runs a legacy `dux daemon /`
under systemd (PID 1073891), which can observe filesystem activity created by
that test. The fixture was removed and local free disk returned to 55 GiB at
the status capture. The daemon remained running with the same PID and four
recorded restarts; its post-run status was already dirty from fanotify overflow
and showed 617 lifetime overflow notifications and 2,518,862 userspace-dropped
updates. Journald contains 27 overflow warnings since 15:50 UTC, overlapping
the test period. There was no exact pre-run counter snapshot, so the amount
caused by the test cannot be isolated and no causal count is claimed.

Treat the local 1.37M timing as **non-isolated diagnostic evidence**, not a
clean performance qualification. The simultaneous 100k `perf` profile also
captured kernel fanotify work from the host service and its generated-file
workload; it cannot attribute the observed CPU to Dux reconciliation. No
service restart, configuration change, or database repair was performed.
Production VPS1 remains unmodified by administrative action, but the test
workload itself was not appropriately isolated. Further churn/scale profiling
must run in a dedicated environment after preserving the VPS2 soak evidence.
Post-run snapshots and relevant journal lines are preserved in
[`local-host-test-impact-20261109/`](local-host-test-impact-20261109/).


### VPS2 canary checkpoint — 2026-11-09 16:03 UTC

Sample 24 passed at 16:03:17 UTC with 914s scan age; zero errors, kernel
overflows, drops, OOMs, or service restarts. Dux was active under its existing
PID, with 7.1 GiB free on VPS2. The 24-hour canary remains in progress. This
checkpoint does not change the local-host test-impact disclosure above.


## Scale-fixture guard and isolated full-suite verification — 2026-11-09 16:19 UTC

The ignored scale test now refuses to generate a large tree on a real
filesystem when `/run/dux/heartbeat` indicates a local Dux service. Set
`DUX_RECONCILE_SCALE_TMPDIR` to an existing pseudo filesystem to opt into an
isolated scratch location. The pure guard test passed. The scale smoke passed
at 100,000 entries on `/dev/shm`: baseline 3.204s, unchanged reconciliation
3.244s, DB 30,117,888 bytes unchanged.

The all-features Rust suite including ignored tests passed **331/331** in
40.71s. It ran inside a private mount namespace using a loopback ext4 fixture
backed by tmpfs; the large scale fixture used `/dev/shm`. This preserved ext4
semantics for the stale-parent test without creating test files on the host's
watched ext4 filesystem. Strict Clippy (`-D warnings`), formatting, and diff
checks passed. An earlier run that put *all* fixtures on tmpfs had 328 passes,
one v6 migration test failure, and two ignored tests; the failure was due to
the test expecting a real filesystem. The ext4-isolated complete rerun passed.

The VPS2 canary had **27 passed samples** at 16:18:45 UTC; latest age 874s,
with zero errors/overflows/drops/OOM/restarts. VPS2 was active, with 7.0 GiB
free. The soak has not completed 24 hours. Logs:
[`rust-suite-isolated-ext4-20261109.log`](rust-suite-isolated-ext4-20261109.log),
[`scale-guard-test-20261109.log`](scale-guard-test-20261109.log),
[`scale-tmpfs-test-20261109.log`](scale-tmpfs-test-20261109.log), and
[`static-checks-scale-guard-20261109.log`](static-checks-scale-guard-20261109.log).

## Isolated constrained 1.37M ext4 profile — 2026-11-09 16:40 UTC

A repeat scale profile ran inside a private mount namespace on an 8 GiB ext4
loop image backed by tmpfs. A private tmpfs covered `/run/dux`, so the host's
live Dux service was not visible to the scale-fixture guard; the host daemon
could not see the private ext4 mount, and its backing image was on `/dev/shm`.
The process ran in a transient scope limited to 50% CPU, 12 GiB memory, zero
swap, `nice 19`, and idle I/O priority. The 1,370,000-entry test passed in
453.19s: baseline 186.730s, unchanged reconciliation 141.158s, and DB size
425,861,120 bytes before and after. The last cgroup memory-event check showed
zero OOMs; observed current memory stayed below 10.8 GiB. This memory-backed
ext4 result is a controlled diagnostic, not block-device I/O qualification or
p95/p99 evidence. It supersedes the earlier non-isolated local timing as the
better performance comparison; more repeated trials are needed.

An initial run with the same image size but mkfs's default inode density failed
with ENOSPC while constructing fixture files, before the scan started. The
fixture cleanup worked. Reformatting with 1,800,000 inodes allowed the full
run to pass. This was a benchmark-image setup issue, not a Dux scan failure.
The setup failure is recorded in
[`scale-inode-setup-failure-20261109.txt`](scale-inode-setup-failure-20261109.txt);
metrics and successful raw output are in
[`scale-1370k-isolated-ext4-metadata-20261109.json`](scale-1370k-isolated-ext4-metadata-20261109.json)
and [`scale-1370k-isolated-ext4-20261109.log`](scale-1370k-isolated-ext4-20261109.log).

After cleanup, `/dev/shm` returned to its prior 1.8 GiB use. A read-only check
showed the local service still active under PID 1073891 with four restarts;
its overflow/drop counters were unchanged across this isolated profile at 636
and 2,518,862. Root filesystem free space was 63 GiB.

The VPS2 canary reached **31 passing samples** at 16:37:57 UTC. Latest scan age
was 135s; errors, overflows, drops, OOMs, and restarts remained zero. VPS2 was
active with 7.0 GiB free. The 24-hour soak remains incomplete.


### Canary interpretation correction — 2026-11-09

The latest archived VPS2 monitor row is labeled `passed` and reports no monitor
errors, kernel overflows, dropped updates, OOM events, or service restarts. Its
`status_after`, however, reports `dirty=true`, `caught_up=false`, and
`behind_seconds=9510.948` (about 2 h 38 min). This is a successful observation
sample against the monitor's checks, not evidence that the indexed view is
current or that the canary has met its reliability/accuracy gate. The sample
count and zero-fault counters must not be presented without this freshness
state. The 24-hour qualification is therefore incomplete and the canary's
index-freshness failure remains open.

### VPS2 read-only checkpoint — 2026-11-09 17:55 UTC

Canary sample 46, captured at 17:53:27 UTC, passed its monitor checks. The
service binary and unit hashes matched release 121; its PID remained 2561224,
with zero restarts, kernel overflow notifications, userspace drops, OOM events,
and pending updates. SQLite `quick_check`, indexed path-graph validation, and
independent accounting passed. The observed scan age after the cycle was 281
seconds. The latest read-only `dux status --json` at 17:55 reported the daemon
active, no scan in progress, zero kernel overflows/drops/pending updates, and
385 seconds since the completed scan. `dirty=true` is the expected wait for the
next scheduled scan; the event-pipeline `behind_seconds` is not applicable in
scan-first mode. Sample 46 is roughly 3h46m into the 24-hour soak, so that gate
remains open. Live sample evidence is retained in
[`vps2-canary-progress.jsonl`](vps2-canary-progress.jsonl).

The scan-first metrics change from source commit `b07fc63` is not in the
installed r121 binary. Revision 122 was packaged and audited separately from
clean commit `24456ed`; it has not been installed on VPS2. See the separate
[revision-122 package qualification](../release-candidate-24456ed-20261109/README.md).


### VPS2 post-scan read-only checkpoint — 2026-11-09 16:49 UTC

A read-only observation caught the 16:44:26 scheduled scan completing. The
service journal records `reconciliation accounting verification passed` in
54.085 seconds at 16:48:52 UTC. At 16:49:40 UTC, the service was active under
the same PID (2561224), with zero restarts, 203,792,384 bytes current memory,
zero kernel overflow notifications, zero userspace drops, and no pending
updates. The last completed scan was 103 seconds old. The status still reports
`dirty=true` / `caught_up=false` while waiting for the next full observation;
its legacy `behind_seconds` field is 10,214.579 and must not be confused with
the 103-second scan age. This is one successful observation/accounting cycle,
not the 24-hour soak or proof of continued freshness.

At the same checkpoint, VPS2 root storage was 93% used (`df -h` rounded to
93G total / 82G used, with 6.9G available). This narrows disk headroom and warrants continued
monitoring; no files were removed. Raw status, service state, journal excerpt,
and `df` output are preserved in
[`vps2-postscan-20261109T1649Z.json`](vps2-postscan-20261109T1649Z.json).


### VPS2 disk inventory — 2026-11-09 16:52 UTC

The read-only root inventory measured 99.0 GB (92.2 GiB) total, 87.1 GB
(81.1 GiB) used, and 7.34 GB (6.84 GiB) available (93% full); inode use
was 29%. `/var` accounts for about
61.3 GiB, including Docker at 22.7 GiB, containerd at 18.9 GiB, `/var/tmp`
at 5.2 GiB, `/var/log` at 4.3 GiB, and `/var/lib/dux` at 0.79 GiB. Journald
reports 3.9 GiB on disk. Docker reports 6.4 GB reclaimable images, 1.96 GB
reclaimable stopped containers, and 367 MB reclaimable volumes, while 20
containers and 10 volumes are active. Docker's reported logical sizes overlap
layer storage and must not be summed with `du` totals.

`/var/tmp` contains multiple Dux test, build, and qualification workspaces,
some of which hold preserved evidence. No files, Docker objects, or logs were
removed: active services and retained test evidence make bulk cleanup unsafe
without item-by-item retention decisions. The detailed snapshot is
[`vps2-disk-inventory-20261109T1652Z.json`](vps2-disk-inventory-20261109T1652Z.json).


### Evidence checksum verification

`SHA256SUMS` uses repository-root-relative paths and covers artifacts present
in this checkout. Five release package/source artifacts referenced by the prior
manifest are not archived here. Their recorded hashes were moved to
[`UNARCHIVED_RELEASE_ARTIFACT_SHA256.txt`](UNARCHIVED_RELEASE_ARTIFACT_SHA256.txt)
and are explicitly not verified against artifact bytes in this checkout.


## Scan-first status/metrics correctness fix — 2026-11-09

Fixed a Prometheus correctness bug: metrics could report
`dux_index_caught_up=1` in scan-first mode when the event queue and pending
count were empty, even though JSON status correctly reports `caught_up=false`
for that mode. The metric now excludes scan-first mode. Metrics now expose
`dux_scan_first_mode`, `dux_behind_seconds_applicable`,
`dux_last_scan_available`, and `dux_last_scan_age_seconds`; the misleading
`dux_behind_seconds` sample is omitted when it is not applicable. JSON status
adds `behind_seconds_applicable` while preserving the existing value for API
compatibility. Operations guidance distinguishes expected scan-first dirty
state from actionable event-apply backlog.

Validation on this source: debug all-features suite **330 passed, 0 failed, 2
ignored**; optimized release all-features suite with ignored tests included
**332 passed, 0 failed** (86.59s), including the 100k scale smoke and ext4
stale-parent integration. Focused metrics tests passed 3/3 and the related
machine-status test passed 1/1. Strict all-target/all-feature Clippy, formatting,
and diff checks passed. The release production executable built successfully
and its CLI version/help/metrics-help smoke checks passed. It is a local
candidate only; it has not been installed on VPS2.

Fresh LLVM coverage ran the same 332 tests, including the ignored profiles:
**73.67% line, 72.57% region, 76.76% function** coverage. Branch coverage was
not collected (the report has zero branch counters). The new metric paths are
exercised by the regression tests, but total coverage remains incomplete;
`main.rs` is 46.43% line-covered. Logs and raw coverage are preserved in
[`scan-first-metrics-release-all-20261109.log`](scan-first-metrics-release-all-20261109.log),
[`scan-first-metrics-full-suite-20261109.log`](scan-first-metrics-full-suite-20261109.log),
[`scan-first-metrics-focused-tests-20261109.log`](scan-first-metrics-focused-tests-20261109.log),
[`scan-first-metrics-status-test-20261109.log`](scan-first-metrics-status-test-20261109.log),
[`scan-first-metrics-clippy-20261109.log`](scan-first-metrics-clippy-20261109.log),
[`scan-first-metrics-fmt-20261109.log`](scan-first-metrics-fmt-20261109.log),
[`scan-first-metrics-diff-check-20261109.log`](scan-first-metrics-diff-check-20261109.log),
[`scan-first-metrics-coverage-run-20261109.log`](scan-first-metrics-coverage-run-20261109.log),
and [`scan-first-metrics-coverage-json-20261109.json`](scan-first-metrics-coverage-json-20261109.json).

The clean-commit release executable is 5,354,616 bytes, SHA-256
`f23b4a07066f8ccd1a6ce470a2038e811db0e9cda35ba8cc4a570e967f7458f6`; it reports
revision `b07fc635b6ce` with no modified-tree marker. `--version`, top-level
help, and metrics help all exited successfully. Build and smoke output is in
[`scan-first-metrics-release-binary-20261109.log`](scan-first-metrics-release-binary-20261109.log).
Remaining: package and lifecycle qualify the candidate, then run it on a canary.
The existing VPS2 process is still the earlier binary and does not verify this
source change.


### VPS2 scan-first runtime checkpoint — 2026-11-09 17:07 UTC

At 17:07:19 UTC, VPS2 was active under PID 2561224 with zero restarts. Dux
reported scan-first mode, no scan in progress, zero kernel overflows, zero
userspace drops, and zero pending updates. The last scan was 233 seconds old;
status remained `dirty=true` awaiting the next periodic observation and
`caught_up=false` by design. The legacy `behind_seconds` value was 11,272.851
and is not applicable to scan-first freshness. The 16:59:26 scan completed
with accounting verification passed at 17:04:17 in 49.529 seconds. Root disk
remained 93% full with 7.29 GB available. This is one more passing observation,
not 24-hour qualification. Raw data is in
[`vps2-runtime-20261109T1706Z.json`](vps2-runtime-20261109T1706Z.json).


### VPS2 scan-first checkpoint — 2026-11-09 17:43 UTC

At 17:43:57 UTC the existing service remained active under PID 2561224, with
zero restarts and 110,206,976 bytes current memory. The 17:14:26 and 17:29:26
observations both completed accounting verification (103.467s and 71.042s). At
capture, scan age was 552s, with zero kernel overflows, userspace drops, and
pending updates. Status remains dirty awaiting the next full observation,
`caught_up=false`, and the old event-pipeline `behind_seconds` counter is
13,472.1s; that counter is inapplicable to scan-first freshness. Root disk was
93% used with 7.23 GB available. The local `b07fc63` metrics fix is not yet
installed on this VM. Evidence: [`vps2-runtime-20261109T1743Z.json`](vps2-runtime-20261109T1743Z.json).
