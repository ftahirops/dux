# Scan-first pressure and CLI settle qualification — 2026-11-09

This record covers an isolated ext4 pressure test of Dux release 107 and a
runtime test of a scan-command status race fixed in the current source tree.
It also records the induced failure of the pre-existing release-106 VPS2
soak. The release-106 canary was stopped by its fail-closed monitor and is not
a passing soak.

## Release-107 controlled filesystem burst

On VPS2, a 2 GiB sparse loop image was formatted ext4 and mounted at
`/mnt/dux-r107-stress`. The release-107 static binary
(`c301574f…`, source commit `d27d3b7`) ran in its own transient systemd unit,
watching only that filesystem. The unit had a 100% one-core CPU quota, 1 GiB
memory limit, the packaged capability set and syscall filter, and
`--max-cpu 60`. The host fanotify queue limit was 16,384 events.

The generator performed **11 bursts × 32,000 create/unlink pairs**: 352,000
pairs, about 704,000 filesystem operations. Individual bursts took 0.353–0.620
seconds. Release 107 recorded 975,442 hint events with **zero kernel overflow
notifications**, zero userspace drops, and no daemon restart. Its pressure
telemetry sampled at most 45,949 hint events/s; sampled queue bytes peaked at
24 and no queue-pressure episode was recorded. These are sampled queue-byte
measurements, not proof that the queue never briefly grew between samples.

After the bursts, the candidate's full observation found the expected fixture
(1 file, 13 directories, 14 path instances) with zero coverage issues;
accounting verification passed. The separate release-106 root service saw its
overflow count rise from 0 to 2 during the same test interval. This is a
single-host controlled comparison, not a repeated performance benchmark or a
qualification of every kernel/filesystem.

## Scan CLI settle race

The earlier candidate observation could finish its walk while the watcher
still held a transient uncertain status. `dux scan` then returned “stale or
uncertain” immediately, even though the independent accounting check cleared
the status a few seconds later. The source now allows up to 10 seconds for
that post-observation status to settle, while still returning uncertainty if
the index does not become clean.

The updated source binary (`53529db2…`, source tree hash
`29154f16…`, built from `8b47815` plus the uncommitted source change) was run
under a separate transient systemd unit on the ext4 fixture. With the index
dirty because of a newly created test file, `dux scan` waited through the
observation and accounting check and returned success in **4.50 seconds**.
The final status had `dirty=false`, zero overflows and drops, 15 indexed path
instances, and the new file was returned by `dux find`. No package was
installed and the production service was not touched.

## Release-106 soak interruption

The existing 24-hour release-106 VPS2 canary passed samples 1–8. Sample 9
failed because its service restart count changed and it observed two overflow
notifications. This was a **test-induced canary interruption**, not an
uncontrolled release-106 crash: adding the loop mount changed filesystem
coverage, and at 00:03:03 UTC the daemon exited with status 1 and the explicit
message “filesystem mounts changed; restart daemon to refresh watch coverage
and reconcile”; systemd restarted it at 00:03:33. The collector's sample 8
service snapshot preceded that mount change, so sample 9 correctly detected
the restart. The failed sample is preserved in the neighboring v4 JSONL; no
24-hour claim can be made from it. This mount-change restart is expected
coverage-refresh behavior and the test environment was cleaned afterward.

## Checks and anomalies

- Rust release tests after the source fix: **322 passed, 0 failed, 2 ignored**.
- Strict all-target/all-feature Clippy with warnings denied: passed.
- Formatting check and `git diff --check`: passed.
- One earlier full run had a single `ENOENT` in the ext4 inode-reuse test; its
  focused rerun passed, followed by a clean full-suite rerun. Treat that as a
  test flake to monitor; do not count it as a product failure or silently omit
  it from the history.
- The 10-second CLI settling rule is tested with deterministic boundary tests
  and the ext4 runtime reproduction above. A persistent dirty status still
  returns an uncertainty message after the bounded settle period.
- Current-source release tests were rerun locally: **322 passed, 0 failed,
  2 ignored**. The ignored tests remain filesystem/scale qualification gates.
- Current-source llvm-cov ran 324 tests and reports 72.16% line, 70.98% region,
  and 76.03% function coverage. Stable llvm-cov reports no branch totals.
  Module-level results and the complete test output are in the preserved log.
- Current-source strict Clippy (`--all-targets --all-features -D warnings`),
  formatting, and diff checks passed.

## Qualification harness correction and release-108 candidate

Release 108 was built from clean commit `0fc18fc` as static musl, DEB, and RPM
artifacts. Its build manifest records source digest
`6884e20a…a671a8df7`; the binary embeds the same digest and commit. All eight
published artifact checksums passed, and the release script completed both
the 229-dependency lockfile audit and the exact binary audit. This candidate
was not installed.

The first isolated readiness run failed before completing its first crash
round. The harness required scan-first status to become event-pipeline
“certain,” even though scan-first treats those events as hints and can remain
conservatively uncertain. The fixture's initial observation and accounting
verification had completed, but the harness waited 90 seconds for the legacy
status condition. The failure report and log are preserved; this is a harness
incompatibility and is not counted as a product pass or product data-loss
failure.

The harness now detects scan-first mode, requests a new full observation,
waits for the independent accounting verifier, and compares the full indexed
filesystem truth. The updated harness passed all **331 Python tests**. The
release-108 production binary then passed an isolated crash/restart smoke test
with exact post-recovery filesystem comparison: one crash round passed and
recovered in 1.909 seconds. These results validate the corrected test path;
the broader release qualification matrix remains pending and will use a build
from the corrected harness source.

## Artifacts

- [`release-107-stress-service.log`](live/release-107-stress-service.log)
- [`release-107-scan-settle-service.log`](live/release-107-scan-settle-service.log)
- [`release-106-unit-events.log`](live/release-106-unit-events.log)
- [`release-106-canary-v4.jsonl`](live/release-106-canary-v4.jsonl)
- [`current-source-release-tests.log`](live/current-source-release-tests.log)
- [`current-source-llvm-cov.log`](live/current-source-llvm-cov.log)
- [`current-source-clippy.log`](live/current-source-clippy.log)
- [`release108-old-harness-failure.json`](live/release108-old-harness-failure.json)
- [`release108-old-harness-failure.log`](live/release108-old-harness-failure.log)
- [`release108-scanfirst-crash-smoke.json`](live/release108-scanfirst-crash-smoke.json)
- [`release108-scanfirst-crash-smoke.log`](live/release108-scanfirst-crash-smoke.log)

The temporary loop image, test database, transient candidate units, and copied
test executables were removed after these records were collected.
