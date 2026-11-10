# Production readiness status

## Release authenticity guard implementation — 2026-11-10

The release builder now fails closed when no release signing fingerprint is
configured. A local qualification build must explicitly set
`DUX_ALLOW_UNSIGNED_RELEASE=1`; setting `DUX_RELEASE_SIGNING_KEY` signs the RPM
with `rpmsign`, updates the artifact manifest/checksums, exports the public
key, and detached-signs `SHA256SUMS`. The verifier checks artifact hashes,
manifest identity, the pinned primary-key fingerprint, the detached signature,
and the RPM signature in a temporary RPM database. The production r132 RPM
was not modified; its SHA-256 remains
`bb1adbd0b2bc5a36c30076bcfd3436e424dbd596aa07a26d78d952a24650af78` and it is
still unsigned.

Verification for this change: the signing test module passed its 8 non-opt-in
tests (the real-signing test was skipped in this rerun because its opt-in RPM
environment variable was unset), and the release-builder CLI passed **13/13**.
The real GPG/RPM integration test had passed in the preceding run: it generated
a disposable key, signed and verified a disposable copy of the r132 RPM, then
rejected a tampered copy. No release-owner key was used. Shell syntax, Python
byte-compilation, and `git diff --check` passed. The debug-artifact verifier
suite was not rerun after adding the explicit local unsigned-build override.

The release-authenticity gate remains open until the release owner supplies a
trusted primary-key fingerprint and the actual signed package verifies in the
Rocky lifecycle guest.

## Strict dependency audit — 2026-11-10 10:07 UTC

Fresh `cargo audit` and stricter `cargo audit -D warnings` runs both exited 0
against the current lockfile: **229 dependencies, zero known vulnerabilities,
zero warnings**. The RustSec database snapshot had 1,430 advisories at commit
`d9cb059f3217db87aeccd190b81eaffe8cc29cb9`, last updated
`2026-11-09T17:13:08+01:00`. This is dependency-advisory evidence, not an
application security certification; its scope and raw result are in the
[audit record](docs/qualification/cargo-audit-20261110T1007Z/README.md).

## Release artifact authenticity — open gate

Static inspection of the exact r132 RPM confirmed its payload hashes and
embedded Dux binary hash, but `rpm --checksig -v` showed **no RPM signature**
(`SIGPGP` and `SIGGPG` are both absent). The local SHA256SUMS file is not itself
publisher-authenticated. Before production distribution, sign the RPM and the
checksum manifest using the release owner's key, publish the trusted
fingerprint, and verify both in the Rocky lifecycle guest. The exact artifact
inspection and hashes are recorded in the
[Rocky RPM qualification record](docs/qualification/r132-rocky-rpm-prereq-20261110.md).

## VPS1 live production check — 2026-11-10 10:09 UTC

The installed production binary is still Dux **0.6.3 / r66** (SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`). The
service is active with four historical restarts and about 1.19 GiB current
memory use. Its index remains **dirty and not caught up** from fanotify queue
overflow: 742 cumulative overflow notifications and 2,518,862 dropped updates.
The last full scan was **2026-11-05 12:49:22 UTC** (about 4d21h before this
sample); reported behind time was about 2d16h. In the last five minutes there
were 187 stale-handle resolution warnings and four slow-flush warnings, with
no new queue-overflow log messages. The root filesystem was 73% used with
48.6 GiB available, so disk capacity is not the active cause. This is a fresh
read-only observation; it does not show r132 behavior or recovery. Full status,
unit, binary, disk, and recent-warning counts are in the
[10:09 VPS1 snapshot](docs/qualification/live-vps1-readiness-20261110T1010Z.json).

### VPS1 follow-up — 2026-11-10 10:21 UTC

The service remains active on r66, dirty and not caught up. The last full scan
is still **2026-11-05 12:49:22 UTC**; the daemon reports 423,151 seconds since
that scan and 233,461 seconds behind. Cumulative overflow and dropped-event
counters remain 742 and 2,518,862, while seen/resolved/committed counters
continue to grow. The latest sample has 1,336,872 index nodes and a 3.43-GB
database. In its preceding five minutes it logged 150 stale-handle warnings,
zero slow-flush warnings, and zero new overflow messages. Root is 74% used with
50.3 GB available. The service still has four historical restarts. Full status
and pressure telemetry are in the
[10:21 snapshot](docs/qualification/live-vps1-readiness-20261110T1020Z.json).

## Exact r132 72-hour soak — running since 2026-11-10 09:04:48 UTC

A dedicated soak is running on the authorized VPS2 using the exact r132 static
binary (SHA-256
`998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`). It is
an isolated `soak` readiness suite in transient unit
`dux-readiness-1304925.service`, with a private mount namespace, 25% CPU quota,
4-GiB memory ceiling, and low I/O weight. Its 768-MiB ext4 loop fixture and
database are in `/dev/shm`; progress and the final report are durably written
under `/var/tmp/dux-r132-soak-20261110/evidence/`.

The suite is configured for 72 hours, 25 operations/second, and independent
exact checks every 1,000 mutations. It also schedules an hourly mutation burst,
a held SQLite reader after six hours, and a controlled daemon kill/recovery
with 64 downtime mutations after twelve hours, followed by exactness, history,
and SQLite integrity checks. Its first progress snapshot reported 1,441
mutations at 60 seconds; that is startup progress only and does not count as a
soak pass. The harness estimates a 73h40m maximum unit runtime to allow final
qualification and cleanup. Expected 72-hour completion is 2026-11-13 around
09:04:48 UTC, subject to the final assertions.

The VPS2 r121 service was active with zero restarts when the soak started; it
has not been upgraded or modified. The soak has not yet passed. The live report
and progress file are on VPS2 at the path above; a final copy and qualification
decision will be added here after the worker reaches a terminal result. See
the [soak configuration and gates](docs/qualification/r132-72h-soak-vps2-20261110/README.md).

### Live checkpoint — 2026-11-10 10:03 UTC

The r132 soak remains `active/running`, with **zero restarts** at the 10:02:21
UTC service check. The 10:02 snapshot recorded 82,405 mutations and 82 exact
checks through operation 82,000 (277ms). By 10:05 UTC, the first scheduled
**500-file hourly burst passed exactness and history checks**; progress recorded
one completed burst and 85,758 total operations. The latest regular exact check
(85,000 operations) took 890ms. The six-hour held-reader, twelve-hour
controlled recovery, and final exactness/history/integrity checks remain
pending. This is early progress, not a soak pass. Both progress snapshots are
[preserved here](docs/qualification/r132-72h-soak-vps2-20261110/).

At 10:16 UTC, the exact r132 soak crossed **100,000 operations**: 101,438
mutations after 4,262.4 seconds, with 101 exact-check samples. The 101,000-op
check passed in 1,082ms; the unit remained active with zero restarts. The
six-hour reader, twelve-hour controlled recovery, and final assertions are
still pending. See the
[10:16 progress snapshot](docs/qualification/r132-72h-soak-vps2-20261110/soak-progress-20261110T1016Z.json).

At 10:19 UTC, progress reached 105,689 mutations and 105 exact checkpoints.
Their end-to-end convergence time was 229ms minimum, 796ms median, 1,148ms
mean, 2,634ms p95, and 9,028ms maximum; the metric includes full scan-first
observation, accounting verification, and independent comparison. Sampled
daemon RSS peaked at 6,568 KiB and WAL at 4.41 MB. The host was shared and
contended (six CPUs, load 4.01/4.17/4.24, CPU PSI some 5.58% over 10 seconds);
ClickHouse and the installed VPS2 Dux were active. This cannot be treated as an
isolated performance benchmark, and the 9-second outlier cannot be attributed
to host pressure from the available samples. See the
[10:19 progress and pressure evidence](docs/qualification/r132-72h-soak-vps2-20261110/).

The Python readiness/release harnesses were rerun at low priority: **189 passed,
0 failed** across readiness workflow (62), package lifecycle harness (72),
release builder CLI (13), debug-artifact verifier (1), and qualification VM
harness (41). These test the qualification tooling, not the Dux Rust binary or
the actual Rocky RPM lifecycle. Details are in the
[harness retest report](docs/qualification/r132-harness-retest-20261110T0950Z.md).

## Current production and disk snapshot — 2026-11-10 09:58 UTC

The installed VPS1 production daemon remains **r66** (`0.6.3`, revision
`e0cc915f8f37`), active with 4 historical restarts. It reports dirty status
from fanotify overflow: **742 overflow notifications**, **2,518,862 dropped
updates**, no full scan for about **4d21h**, and `caught_up=false`. It has
1,336,569 indexed nodes and a 3.43-GB SQLite database. Snapshots from 09:38 to
09:58 show the drop and overflow counters unchanged while event counters
continued increasing; this short interval does not repair the stale index.
VPS1 has about 50 GiB available on its 185-GiB root filesystem (72% used), so
production staleness is not explained by disk exhaustion. See the
[09:58 VPS1 snapshot](docs/qualification/live-vps1-readiness-20261110T0958Z.json).

The installed VPS2 **r121** service is separate from the r132 soak. It remains
active with zero restarts and scan-first mode. The latest periodic observation
has completed and its accounting verification settled; the index is now dirty
because filesystem changes await the next full observation. It reports **zero
dropped updates** and **zero kernel overflows**, with 1,866,596 indexed nodes
in an 806-MB database. VPS2 has about 12.5 GB available on `/` (87% used).
See the [09:58 VPS2 snapshot](docs/qualification/live-vps2-readiness-20261110T0958Z.json)
and [post-cleanup disk/service evidence](docs/qualification/vps2-test-disk-cleanup-20261110.json).

### VPS2 disk cleanup — 2026-11-10 09:42 UTC

Before cleanup, the 93-GiB root filesystem was at 97% with 3.1 GiB available.
Docker metadata identified a stopped ClickHouse container from the
`xgs-test` test project, exited since Nov 6, with a Docker-reported 1.95-GB
writable layer. No container in that project was active. I removed that
container, then removed its separate Docker-reported 5.95-GB anonymous volume
after verifying it had no attached containers. The old test database in that
volume was deleted. I preserved the
other Docker images and volumes, all active application data, and the Dux DB.

Afterward, root use fell to **87%** with **12.5 GB available**. Docker reports
17.59 GB of images, 1.61 GB of container layers, and 16.01 GB of volumes. The
installed Dux scan completed during this interval; Dux remains active with zero
restarts, zero loss/overflow counts, and its index marked dirty only because
new changes await the next full observation. Its temporary scan database is
gone, so there is no active scan file to delete. The r132 soak stayed active
with zero restarts. Full before/after details are in the
[disk cleanup record](docs/qualification/vps2-test-disk-cleanup-20261110.json).

At 09:48 UTC, host admission for the Rocky RPM VM was blocked: load1 was 13.32
and multiple Go compiler, test, and vet processes were active. No VM was
started. The guard remains required because concurrent host builds have
repeatedly interrupted guest startup.
At 09:50 UTC, another start was refused on admission sample 2/3 when Go
compiler, test, and vet processes appeared. No VM was started.
At 09:53 UTC, the host still had active Go test processes; no VM was started.
See [the 09:53 admission sample](docs/qualification/r132-rocky-rpm-admission-20261110T095317Z.json).
At 09:57 UTC, Go compiler/test and Node Vitest processes were still active, so
no VM was started. See [the 09:57 admission sample](docs/qualification/r132-rocky-rpm-admission-20261110T095731Z.json).

The Rocky RPM lifecycle is still pending. A guarded VM start at 09:08 UTC was
stopped by its independent host-pressure monitor 11 seconds later when five Go
compiler processes and one Go race-test process appeared. SSH was not ready and
no RPM package operation ran. The recorded interruption is in the
[Rocky prerequisite report](docs/qualification/r132-rocky-rpm-prereq-20261110.md).
At 09:15 UTC another guarded startup attempt was refused before launch because
host load1 was 11.73 against the limit of 10; the guest and package state remain
untouched.
At 09:18 UTC a later start passed admission, but a Go test workload appeared
while Rocky was booting. The pressure guard stopped the guest at 09:19:17 UTC
before SSH was ready; no RPM phase ran. The guard event is recorded in
[`the 09:19 event`](docs/qualification/r132-rocky-rpm-guarded-start-20261110T091917Z.jsonl).
At 09:23 UTC two more guarded starts were refused before VM creation when Go
tests appeared during the three-sample admission window. See the
[Rocky prerequisite report](docs/qualification/r132-rocky-rpm-prereq-20261110.md).
At 09:27 UTC a later guest booted and its RPM hashes verified, but the guard
stopped it at 09:28:20 UTC before package lifecycle began. Another start was
refused at admission at about 09:29:29 UTC.
The 09:31:18 UTC start was also refused before VM creation when a Go test
binary started; the decision is preserved in
[`the 09:31 refusal record`](docs/qualification/r132-rocky-rpm-admission-refusal-20261110T093118Z.jsonl).
At 09:35:06 UTC a further start was refused on sample 3/3 after three Go
compiler processes and a Go test process appeared; no VM was created. See
[`the 09:35 refusal record`](docs/qualification/r132-rocky-rpm-admission-refusal-20261110T093506Z.jsonl).
At 09:38 UTC, host load1 was 10.72 with Go compiler/test processes active, so
the RPM VM was not retried.

## Exact r132 100k-file churn qualification — 2026-11-10 08:19–08:57 UTC

The exact r132 static release binary (SHA-256
`998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, source
commit `f02ba7ad235ea841a99de27b69fcd402e3c3927c`) passed the isolated ext4
production-shaped churn suite on the authorized VPS2. It used a disposable
2-GiB loop image in `/dev/shm`, a private mount namespace, 100,000 baseline
files, and a transient systemd unit limited to 25% CPU and 4 GiB memory. The
unit completed successfully in **38m02s**, used **9m30s CPU**, peaked at about
**773 MiB memory** and **538 MiB swap**. VPS2's root filesystem remained at
95% use with 4.7 GiB free; test data was kept on tmpfs.

The suite performed **20,000 mutations in ten 2,000-operation batches**,
including atomic replacement, rapid delete/recreate, rename-overwrite,
hardlink write/unlink, in-place write, and transient create/delete. All ten
batch checkpoints passed an independent exact comparison of paths, inode
identity, allocation and rollups. The final exact comparison, history
continuity and SQLite integrity check also passed. The candidate observed
**91,013 fanotify hints during the mutation workload**, with **zero dropped
updates and zero kernel overflows**. Scan-first intentionally resolved and
committed zero per-file hint updates; each batch instead forced a full
observation and exact comparison. Checkpoint exact-convergence measurements
were **92.3–323.0 seconds** (median **152.1s**), including the observation and
oracle-check path; these are not isolated scanner benchmarks. The harness used
for this run also sent a `SIGHUP` before calling `daemon.exact()`, which itself
requests the observation, so it performed a redundant extra full scan per
batch. That inflated test time but did not weaken exactness; the duplicate
trigger has since been removed and regression-guarded. Every scan reported
zero coverage issues.

The installed r121 service remained active with zero restarts, and no package
or live database was changed. This materially improves evidence for exact r132
under churn, but it is one ext4/6.8-kernel run at 100k files—not the 1.9M-node
production scale, a multi-day soak, a 72-hour soak, a 24-hour canary, or a
cross-filesystem test. Full machine-readable results and checkpoint data are
in the [qualification report](docs/qualification/r132-100k-churn-vps2-20261110/report.json)
and [progress record](docs/qualification/r132-100k-churn-vps2-20261110/churn-progress.json);
the [test notes](docs/qualification/r132-100k-churn-vps2-20261110/README.md)
include the environment and caveats.

## VPS2 completed scan-first observation — 2026-11-10 08:05–08:14 UTC

The r121 service on VPS2 completed the scheduled full-root observation that
started at 08:05:31 UTC. The filesystem walk completed at 08:11:52 UTC
(approximately **6m21s**) with **1,708,484 files**, **202,663 directories**,
**1,911,147 staged path instances**, zero directory aliases, and **zero
coverage issues**. Dux reported **12 vanished inode-reuse paths**, discarded
from the scan result, with zero unresolved reuse paths. The resulting index
contained 1,907,665 unique inodes and about 82.47 GiB of unique allocated data.

Post-scan accounting verification passed at 08:13:39 UTC in **106.9 seconds**.
At the 08:14:21 UTC follow-up, the daemon remained active with zero restarts,
zero reported dropped updates and kernel overflow notifications, and about
598 MiB memory use. The index was still marked dirty with reason
`filesystem changes await the next full observation`: filesystem hints arrived
while/after the walk, so this is not evidence that the view was caught up to
the continuously changing filesystem. The scan itself completed and its
accounting check passed; the dirty marker correctly communicates freshness.

During the walk, telemetry showed roughly 20k–38k hints/second and no growth
in the drop/overflow counters at the sampled checkpoints. This is useful live
evidence that the installed r121 scan-first service can complete a 1.9M-node
walk on VPS2 under heavy churn, but it does not qualify exact r132, guarantee
zero losses over a long soak, or independently compare every indexed path to
an external filesystem listing. The measured six-minute scan also consumes a
substantial share of the configured 15-minute observation interval. Full
snapshot and intermediate progression data are in
[`the VPS2 scan cycle report`](docs/qualification/live-vps2-scan-first-cycle-20261110T080841Z.md)
and its [machine-readable samples](docs/qualification/live-vps2-scan-first-cycle-20261110T080841Z.json).

## Live VPS1 overflow evidence — 2026-11-10 07:52 UTC

Read-only production sampling found the installed package is still **r66**
(`dux 0.6.3-66`, revision `e0cc915f8f37`), not the r132 candidate. The service
was active with historical `NRestarts=4`, but its index status was dirty from
fanotify overflow: **721 overflow notifications**, **2,518,862 dropped
updates**, last full scan **4d19h** earlier, and `caught_up=false`. In the
preceding five minutes journald recorded 109 stale-handle warnings and 29 slow
flush-phase warnings. Host load was elevated with unrelated Go builds/tests
running; root still had about 57 GiB free, so disk exhaustion did not explain
the stale index. The 126.4 GB `index_bytes` value is the logical sum of indexed
file sizes, not SQLite disk usage; the DB was 3.38 GB and WAL 128 MiB.

This confirms a current production blocker but does not qualify the r132
candidate. r132 has passed focused runtime and Ubuntu DEB lifecycle checks, not
production-scale throughput, sustained churn, or a long canary/soak. Full
counters and caveats are in the
[VPS1 production snapshot](docs/qualification/live-vps1-overflow-20261110T0752Z.md).

A 30-second read-only follow-up at 07:58:28–07:58:58 during host Go test/build
churn saw 9,006 events, 6,237 resolved, and 2,518 committed updates; it added
no drops or overflows during that short interval. The cumulative overflow
counter rose from 721 to 731 between the 07:52 and 07:58 samples, while dropped
updates remained at 2,518,862. Dux remained dirty and its last full scan was
still November 5. This short interval is not a reliability pass. See the
[30-second counter comparison](docs/qualification/live-vps1-overflow-window-20261110T075858Z.json).

## Live VPS2 scan-first comparison — 2026-11-10 08:04 UTC

The separately authorized VPS2 currently runs r121 in scan-first mode. It
indexes 1,905,848 nodes and was active with zero restarts and about 546 MiB
memory. Over a 30-second read-only window it counted 922,605 fanotify hints
(about 30.8k/s) with no increase in dropped updates or kernel overflows. Its
status was dirty because hints arrived after the last observation, completed
about 8.6 minutes earlier; this sample does not yet prove the next 15-minute
observation's timing or completion. It is encouraging evidence for the
scan-first architecture, but it is not the exact r132 binary or a long soak.
Details are in the
[VPS2 scan-first window](docs/qualification/live-vps2-scanfirst-window-20261110T080443Z.md).

## r132 exact-source package and DEB lifecycle qualification — 2026-11-10 07:44 UTC

The exact current source at commit `f02ba7ad235ea841a99de27b69fcd402e3c3927c`
built successfully as a static-musl release (`x86_64-unknown-linux-musl`). The
build was isolated on VPS1 in a private tmpfs under a 20% CPU quota and 6 GiB
memory limit. It completed in 19m20s, consumed 3m52s CPU, and peaked at 1.3 GiB
memory. The installed Dux service stayed active with its historical restart
count unchanged at 4.

The build pipeline passed source-digest and revision matching, static-link and
embedded dependency-metadata checks, exact release-binary RustSec audit, DEB
and RPM construction, payload executable/helper hash comparisons, and manifest
generation. The binary reports source SHA-256
`5d427c2c72dc3fc963fdae8c72006a82390cd00c8bd0b7af9d4e75540cd405ff`; its SHA-256
is `998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`. DEB
SHA-256: `d35236cb0b55cf80ea4750115d6269dea2203b5922942df4f1fec0e4a29d8def`;
RPM SHA-256: `bb1adbd0b2bc5a36c30076bcfd3436e424dbd596aa07a26d78d952a24650af78`.
All entries in the published SHA256SUMS verified; `--version` and `--help`
smokes passed. The lockfile report covered 229 crates with zero known
vulnerabilities and zero warnings; the exact static binary audit recovered
202 dependencies without a warning.

Additional package qualification harness tests passed: lifecycle harness **72**,
release-builder CLI **13**, debug-artifact verifier **1**, and qualification VM
harness **41** (127 total). These are local harness/unit tests; they do not
replace installing packages inside a guest.

The complete r132 DEB lifecycle passed in a disposable Ubuntu 24.04 KVM guest
in **87.943 seconds**: r131 baseline install, r132 upgrade, rollback, re-upgrade,
remove/purge, and clean install. The report verifies package binary and backup
helper hashes, rollback DB backup/restore quick-checks, service enablement,
effective systemd policy, state-directory/database permissions, and that clean
install does not create a tunnel identity. On clean install, Dux was active and
enabled with the tunnel disabled; status was dirty because post-observation
filesystem hints awaited the next full observation, with zero dropped events
and zero kernel overflows. The legacy r131 baseline itself was dirty with
accounting verification pending (`caught_up=false`), so this lifecycle test
does not claim an exact clean legacy baseline or independent filesystem/index
equality. A first attempt was stopped during rollback by the host guard when a
Go race-test workload started; the rerun passed after admission cleared. The
guard was not bypassed or interrupted. The guest used an ephemeral overlay and
was stopped cleanly. No package was installed on a production host. The
complete report is
[`deb-lifecycle-passed-report.json`](docs/qualification/r132-postscan-hint-runtime-20261110/r132-release/deb-lifecycle-passed-report.json); the interrupted attempt and guard evidence is in
[`the DEB lifecycle attempt`](docs/qualification/r132-postscan-hint-runtime-20261110/r132-release/deb-lifecycle-attempt.json).
RPM lifecycle on Rocky remains unqualified; its official image and signed
checksum are verified, but VM admission is still refusing startup during
unrelated Go test/build pressure. See the
[Rocky RPM qualification prerequisite](docs/qualification/r132-rocky-rpm-prereq-20261110.md).
The build manifest, checksums and audit reports are in
[`the r132 package evidence`](docs/qualification/r132-postscan-hint-runtime-20261110/r132-release/).
Package artifact and Ubuntu DEB lifecycle gates pass; production readiness
remains open.

## r132 post-observation accounting race fix — 2026-11-10 06:54 UTC

The deterministic VPS2 regression test exposed that scan-first mode's dedicated
reader counted post-walk hints but did not invalidate the pending accounting
check. The coordinator could therefore clear dirty state using a verification
for an older observation. The scan-first counter path now invalidates that
pending clear for either new events or kernel overflows, cancels the worker
when possible, and retains the reason that a fresh observation is required.

Verification passed: **341 Rust tests passed, 0 failed, 2 ignored**; **61
readiness harness tests passed**; strict Clippy, formatting, byte compilation,
and diff checks passed. LLVM coverage is **72.61% lines, 71.44% regions, and
76.19% functions**; branch counters are unavailable. On VPS2, the focused
runtime race suite passed in 5.235s on a disposable 768 MiB ext4 image. It
proved dirty-state retention, later exact indexing of the new file, and final
integrity. The current-source ignored ext4 stale-parent integration also
passed **1/1** on VPS2 in a private mount namespace. The installed r121 service
stayed active with zero restarts. The candidate was not installed; its exact
source package build is recorded above.
Details, hashes, and machine-readable evidence are in
[the race qualification](docs/qualification/r132-postscan-hint-runtime-20261110.md).

This closes the specific post-observation accounting race gate. It does not
close the 72-hour soak, 24-hour canary, package/rollback, privilege review,
large-scale performance, or cross-filesystem/kernel qualification gates.
Production readiness remains open.

## r132 working-tree runtime follow-up — 2026-11-10 06:26 UTC

Built production and fault-injection release binaries from `fad49686709d`; both
reported the same source SHA-256. The isolated VPS2 `retry` suite passed in
62.1s: persisted failure/cooldown survived restart, event ingestion continued,
automatic retry succeeded, and the final fixture index matched exactly. It ran
in a private mount namespace on a disposable ext4 loop fixture; the installed
r121 service/database were untouched, and the fixture was cleaned up. The
installed service had zero restarts, with its own scheduled scan active at the
post-test sample.

This runtime suite covers bounded retry behavior, not the new post-observation
hint race itself. That guard currently has a focused unit test only; a
deterministic runtime race test is still needed. The current worktree has 341
passing Rust tests, 2 ignored, 72.64% line coverage, 71.49% region coverage,
and 76.26% function coverage. Branch counters are unavailable. Binary hashes,
test evidence, and unqualified gates are documented in [the r132 runtime
qualification](docs/qualification/r132-retry-runtime-20261110.md). No package
has been released or installed; production readiness remains open.

## VPS2 live reconciliation follow-up — 2026-11-10 06:14 UTC

The installed r121 service's bounded retry completed at 05:58:50 after about
9m41s: 1,681,973 files, 199,525 directories, zero coverage issues. Independent
accounting verification passed at 06:00:42 after 111.5s. Its next scheduled
observation began at 06:04:09; at 06:11:01 it was indexing after 1,682,574
files and 199,794 directories. It completed at 06:11:26 with zero coverage
issues and zero paths to replay, after about 7m17s. Accounting verification
passed at 06:12:25 after 58.9s. At 06:14:01 the status was dirty because
filesystem changes had arrived after the observation and another full scan
was due; no events were dropped and no kernel overflow was reported. The
service had zero restarts and 4.1 GiB root free. This is live evidence for
installed r121, not candidate r131; independent filesystem-to-index equality
remains open. See [the VPS2 observation](docs/qualification/reconciliation-live-vps2-20261110.md).

The r131 code also now invalidates a pending reconciliation-accounting clear
when a post-observation in-scope hint arrives before the verifier completes.
It retains dirty status and updates its reason to say a new full observation
is needed, preventing the older snapshot's check from clearing that marker.
The focused regression test passed. After this change,
`cargo test --locked --all-features` passed **341 tests, 0 failed, 2 ignored**;
strict all-target/all-feature Clippy, formatting, and `git diff --check` passed.
Coverage on this worktree: **72.64% lines** (19,061/26,240), **71.49% regions**
(30,295/42,378), **76.26% functions** (1,330/1,744); LLVM reported no branch
counters. Test and coverage logs are in
`target/readiness/r132-working-tree-20261110/`. The candidate has not been
packaged or deployed.

## r131 initial-baseline read guard and runtime qualification — 2026-11-10 05:40 UTC

Review found a regression in r129's read guard: checking whether the database
was empty opened SQLite and created a `-wal` sidecar. r131 adds an
`initial_baseline` bit to scan progress and checks it before opening SQLite.
Legacy five-field progress records remain readable. A regression test now
calls the reader during the publication handoff, after the daemon closes its
connection, and verifies the reader fails with the progress message without
creating SQLite sidecars. The progress-bit compatibility test also passes.

The r131 all-features suite passed **340 tests, 0 failed, 2 ignored**; the
coverage run passed the same tests. Coverage is **72.66% lines, 71.51%
regions, 76.23% functions**; branch counters are unavailable. Strict Clippy,
formatting, and diff checks passed. The release pipeline and artifact checks
passed, and the audited lockfile has zero known vulnerabilities.

The suite's ignored ext4 stale-parent integration case was also run separately
as root on VPS2's ext4 root filesystem and passed. VPS2's installed service
remained active with zero restarts before and after. Only the manual
production-shaped scale test was run separately on tmpfs with 100,000 file
entries: atomic baseline took 12.698s, unchanged in-place reconciliation
13.931s, with the expected rows and a 30,076,928-byte database. This verifies
correctness at 100k but does not show a speedup or measure disk writes; it is
below the ~1.37M-node production tree. The installed service remained active
with zero restarts, drops, and overflow notices during both targeted runs.

The exact r131 binary passed isolated event-apply and scan-first fanotify
smokes on VPS2. Accounting verification passed with zero coverage issues,
drops, or overflow notices. Event-apply resolved 11/11 events. Scan-first
published its hinted file after observation but correctly remained `dirty`
pending the next scheduled observation. VPS2's installed service stayed
active with zero restarts. This 128 MiB fixture does not test the 256 MiB WAL
guard or production-scale behavior. Full lifecycle remains unrun because the
disposable-VM admission guard sees unrelated Go build/test workloads; it was
not bypassed. **Release decision remains: not production-ready.** Details:
[`r131 qualification`](docs/qualification/r131-initial-baseline-read-guard-20261110/README.md).

Disk sample: local root has 55 GiB free (185 GiB total); readiness artifacts
use 1.3 GiB. VPS2 root has 3.8 GiB free (93 GiB total, 96% used); `/var/tmp`
uses 5.3 GiB and system journal uses 4.1 GiB. These VPS2 paths contain old
qualification/build material and retained logs and were not removed without
retention review.

## r129 runtime qualification and refreshed test evidence — 2026-11-10 05:22 UTC

Candidate `8a8b1e3` (`0.6.3-129`) adds a read guard for an initialized empty
index while the initial scan progress marker is active, preventing Dux read
clients from opening the target during the atomic publication handoff. Its
focused regression test passed; the all-features suite and LLVM-instrumented
suite each passed **339 tests, 0 failed, 2 ignored**. Coverage is **72.66%
lines, 71.51% regions, 76.25% functions**; branch coverage was not measured.
Strict Clippy, formatting, and `git diff --check` passed. The release pipeline
produced and verified static, DEB, RPM, source, and debug artifacts; manifest
reports a clean tree and zero known lockfile vulnerabilities. Binary version
and help checks passed, and all eight release checksum entries verified.

The exact r129 static binary passed isolated event-apply and scan-first daemon
smokes on VPS2 using a 128 MiB tmpfs-backed ext4 fixture. Both paths passed
accounting checks with zero coverage issues, drops, or overflow notices;
event-apply resolved 11/11 events, and scan-first indexed its hinted test
change after an observation. The installed VPS2 service remained untouched,
at zero restarts; it was in `reloading` state before and after the smoke. The
fixture is too small to exercise the 256 MiB WAL threshold. No r129 package
lifecycle, production-size WAL sample, independent path-equality comparison,
sustained-churn run, or 24-hour canary has passed. **Release decision remains:
not production-ready.** Full evidence: [`r129 qualification`](docs/qualification/r129-empty-index-read-guard-20261110/README.md).

## r128 isolated daemon smoke and refreshed coverage — 2026-11-10 05:11 UTC

Committed candidate `dd3573d` as package `0.6.3-128`. The static release
pipeline passed; build manifest is clean, dependency audit reports zero known
vulnerabilities, and binary/DEB/RPM/source/checksum verification passed. The
all-feature suite passed **338 tests, 0 failed, 2 ignored**. Refreshed LLVM
coverage for this revision is **72.68% lines, 71.50% regions, 76.24% functions**;
branch counters are unavailable. Strict Clippy, formatting, and diff checks
passed.

The exact candidate binary also passed an isolated ext4 fanotify daemon smoke
on VPS2 in a private mount namespace. Both event-apply and scan-first paths
completed observations with zero coverage issues and passing accounting; the
smoke reported zero drops, zero overflows, and no WAL guard warning. The
installed r121 service stayed active with zero restarts. The fixture was only
128 MiB, so it does not exercise the 256 MiB WAL threshold or establish that
the r127 production-scale spike is fixed. A full clean-install lifecycle and
large-root WAL sampling remain required. The prior local VM lifecycle attempt
was refused by host admission while unrelated Go tests were active; admission
was not bypassed.

Detailed evidence: [`r128 fresh-index publication qualification`](docs/qualification/r128-fresh-publication-20261110/README.md).
**Release decision remains: not production-ready.**

## Fresh-index WAL publication prototype — 2026-11-10 04:54 UTC

Implemented a narrow candidate fix for r127's clean-install WAL spike. For a
current v7 database with no indexed root, inode/path/dirent/totals rows, or
growth history, the scanner no longer holds an unnecessary read connection;
the watcher closes its SQLite connection at publication, verifies sidecars
are absent, and permits atomic rename of the synced staging database. If the
handoff is unavailable or SQLite sidecars remain, the existing online backup
path is used. Populated indexes keep SQLite backup publication and its
concurrent-reader snapshot behavior.

Local all-feature tests passed **338/338**, with two ext4/scale tests ignored;
the fresh-index path, populated-reader fallback, zero-byte database,
recovery-sidecar, and dangling-symlink tests passed. Strict Clippy, formatting,
and `git diff --check` passed. This does **not** yet prove the WAL spike is
fixed under the daemon: no candidate package or VPS lifecycle has exercised
the watcher handoff. The prior r127 coverage percentages predate this change
and are not coverage for it. Details and the required runtime gate are in
[`fresh-index WAL publication qualification`](docs/qualification/wal-fresh-index-publication-20261110/README.md).

**Release decision remains: not production-ready.**

## Latest verified checkpoint — 2026-11-10 04:24 UTC

### Release decision

**Not production-ready; do not claim release qualification.** Candidate r127
(source commit `f0673f5`) passed all 339 inclusive Rust tests, the static
package pipeline, and the Debian r121-to-r127 upgrade/rollback/re-upgrade/
remove/purge/clean-install lifecycle on the marked VPS2 KVM qualification VM.
However, its WAL guard still activated at 717,609,272 bytes during clean-install
publication because SQLite could not checkpoint while the publication writer
was active. The guard recovered after the scan; no drops, overflows, or write
error were reported. r127 is not installed on production VPS1; VPS2 has been
restored to r121. The required fresh 24-hour candidate canary, independent
filesystem path comparison, and sustained high-churn qualification remain
open. The WAL publication burst is an unresolved reliability/freshness issue.

### Production VPS1 — current installed service

Read-only capture at 01:40:51 UTC: `/usr/bin/dux` is package `0.6.3-66`, source
commit `e0cc915f8f37`, not the current candidate. PID 1073891 is active with 4
lifetime restarts and no reconciliation in progress. Status is dirty and not
caught up, with reason `non-watched filesystem mounts changed; reconciling
visible paths`, and no write error. Its last full scan was 2026-11-05 12:49:22
UTC (about 4.5 days earlier); event backlog age was about 56 hours. Lifetime
counters show **649 kernel overflows** and **2,518,862 reported dropped
updates**. The journal at capture time continues to report `open_by_handle_at`
`ESTALE` for transient paths. Root storage is 70% used with 55 GiB free.

At 01:45 UTC, the r126 binary's read-only `status --db` command successfully
opened this same production database. It reported event-apply mode, 2 failed
reconciliation attempts against a limit of 1, and
`automatic_reconciliation_paused=true`; the daemon itself was not paused and
had 31 pending updates in that sample. This identifies the legacy repair
circuit as the immediate reason automatic recovery is no longer proceeding.
The snapshot is
[`production-r126-readonly-status-20261110T0145Z.json`](docs/qualification/production-r126-readonly-status-20261110T0145Z.json),
SHA-256 `7932514a5276fbed865007c8aa85f89c0358bde0112220aee6629cf9aec5a12e`.
The r126 default-mode regression test confirms the new daemon defaults to
reconciliation-first; the installed package 66 remains event-apply. That is a
candidate behavior change whose upgrade and live canary still need the open
lifecycle gates. No service or database changes were made.

### VPS2 — scan-first runtime evidence and capacity

The r121 daemon remained active with zero restarts. A root observation from
01:31:41 to 01:36:33 UTC scanned 1,632,117 files and 193,595 directories,
reported **0 coverage issues**, and revalidated two inode-reuse races. The
accounting verification passed in 69.609 seconds. At 01:39 UTC it had no scan
in progress, write error, kernel overflows, or reported drops; it remained
dirty because changes during the walk await the next observation. This is a
successful observation, not a clean 24-hour canary or proof of freshness under
continuous mutation. The read-only raw journal and status sample are linked in
[`scan-first-backlog-age-20261110.md`](docs/qualification/scan-first-backlog-age-20261110.md).

At 01:59 UTC, the r126 binary also passed a privileged integration smoke in a
private mount namespace on an isolated ext4 loop filesystem backed by tmpfs.
`strace` confirmed successful `fanotify_init`, `fanotify_mark`, and
`open_by_handle_at`. Event-apply mode indexed eight creates and a rename (11
events seen/resolved, no drops or overflows); default scan-first recorded one
event as a hint and published the file after a manual full observation. Both
observations had zero coverage issues and passed accounting checks in about
100 ms. The live r121 service remained active with zero restarts, overflows,
and drops; all test mounts and databases were removed. Details and the
reproducible harness are in
[`fanotify-integration-smoke.md`](docs/qualification/release-candidate-r126-20261110/fanotify-integration-smoke.md).

VPS2 root is still 94% used. Removing one unattached, regenerable Go/Node
`act-toolcache` volume reclaimed about 409 MiB; the latest measured free space
was 6.0 GiB at 01:59 UTC after the tmpfs-backed smoke. ClickHouse telemetry,
XGS incident records, active Docker data, and Dux rollback/qualification
workspaces were preserved. See the
[`VPS2 disk audit`](docs/qualification/vps2-disk-audit-20261110.md).

### r126 code/test evidence

Built from clean code commit `2a884a8`; the full all-feature Rust suite,
including ignored tests on tmpfs, passed **338/338**. The separate inclusive
LLVM run also passed 338/338. Coverage is 74.08% line, 72.99% region, 77.14%
function; LLVM emitted no branch counters. Strict Clippy, formatting,
`git diff --check`, package metadata, and artifact hash verification passed.
Dependency audit: 0 known vulnerabilities in 229 locked packages in the
recorded advisory snapshot. The basic privileged fanotify success path now has
an isolated runtime smoke. A separate run as `nobody` with no capabilities
confirmed startup fails closed at `fanotify_mark` (`EPERM`); broader
`CAP_DAC_READ_SEARCH` handle-resolution behavior, stale-handle churn, and
production-scale qualification remain open. Full details and
artifacts are in the
[`r126 qualification`](docs/qualification/release-candidate-r126-20261110/README.md).

At 02:08 UTC, the missing-capability startup smoke was repeated from the
checked-in harness against the exact candidate SHA-256; it passed on VPS1
(Linux 6.8.0-147-generic). The temporary `/dev/shm` fixture was automatically removed.
This does not qualify capability behavior on other kernels or test
`CAP_DAC_READ_SEARCH` event-handle resolution.

At 02:14 UTC, r126's read-only status command opened the live VPS2 r121 database
and reported `behind_seconds_applicable=false` and no stale throttle marker,
while the installed r121 status on that same database still reported about
12.2 hours of behind time despite a scan about six minutes old. Both samples
showed zero drops/overflows, no pending updates, and no write error. This
confirms the status correction against the production database; it does not
qualify r126 runtime behavior. See
[`VPS2 read-only status comparison`](docs/qualification/production-live-followup-20261110T0214Z/README.md).

At 02:24 UTC, the installed r121 daemon completed an in-place observation of
1,640,606 files and 194,545 directories. It reported 0 coverage issues and 0
unresolved inode-reuse paths after discarding 88 vanished paths. The run took
about 7m24s, used a 659 MiB disk-backed staging DB at the captured midpoint,
and cleaned that file on completion. Root free space recovered from 4.7 to 5.8
GiB; the service remained active with zero restarts, drops, or overflows. The
daemon's accounting verification passed in 75.079 seconds. The follow-up
status showed only the normal “changes await next observation” dirty reason.
An independent filesystem-walk comparison is still required. Details are in
[`VPS2 observation and disk report`](docs/qualification/production-live-followup-20261110T0224Z/README.md).

The r126 missing-`CAP_DAC_READ_SEARCH` smoke then passed twice on a disposable
ext4 loop filesystem in a private mount namespace. `open_by_handle_at` returned
the expected `EPERM`, which Dux surfaced as dirty/uncertain status after 69
unresolved events. Final samples showed 90 and 85 events seen, zero resolved,
dropped, or overflow events. The installed r121 service remained active with
zero restarts and no write error. Test report and captured output are in the
same follow-up folder.

At 02:34 UTC, `systemd-analyze security` on VPS2 scored the installed unit
3.3 (“OK”); the installed unit hash exactly matches the r126 package unit and
there are no drop-in overrides. The main remaining hardening question is
whether the daemon can run as UID `dux` with the same two required capabilities;
that change is not made or claimed without lifecycle and full-tree access
tests. See
[`systemd hardening review`](docs/qualification/systemd-hardening-20261110/README.md).

**Updated:** 2026-11-10 02:34 UTC

### Follow-up: incomplete scan recovery and non-root feasibility — 2026-11-10 02:53 UTC

On VPS2, the r121 scheduled observation that started at 02:31:46 found one
conflicting inode birth-time incarnation. It reported one coverage issue and
scheduled its bounded retry; retry admission was initially deferred under
elevated CPU pressure. The retry started at 02:38:47, completed at 02:45:04
with zero coverage issues/unresolved inode-reuse paths, and passed Dux's
accounting verification at 02:46:07 in 63.147s. It staged 1,837,130 path
instances. The service had zero restarts, drops, or overflow notifications in
the recorded samples. This is evidence that this bounded-retry case recovered,
not proof that all high-churn scans converge. It cost roughly 375.5 CPU-seconds
over 429 seconds and about 2.42 GiB reads / 1.64 GiB writes during a short
overlapping host-load window; peak staged DB was about 661 MiB and was removed
on success. At 02:53:48 a later scheduled observation was in progress, so the
host was not idle at the latest sample. See
[`VPS2 retry and capability follow-up`](docs/qualification/production-live-followup-20261110T0231Z/README.md).

The exact r126 candidate also passed an isolated ext4 smoke as UID `nobody`
with only `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`, `NoNewPrivs` enabled:
fanotify setup, baseline/accounting verification, and create/rename resolution
passed. This establishes feasibility for the tested syscall path only; it does
not validate full-tree access or package/service-account lifecycle. The
installed unit remains root. The captured output and script are in
[`release-candidate-r126-20261110`](docs/qualification/release-candidate-r126-20261110/README.md).

At 02:53:48 UTC, VPS2 r121 remained active with zero restarts, drops, and
kernel overflows; a subsequent scheduled scan was in progress. Root was 94%
used with 5.6 GiB available. The lifecycle driver process is still alive, but
host admission is not clear: a Go compiler workload was active at the latest
local process check. The disposable guest has not launched. r126 remains
uninstalled; the production decision and open gates above are unchanged.

**Updated:** 2026-11-10 02:53 UTC

### r126 package lifecycle and VPS2 restoration — 2026-11-10 03:44 UTC

The Debian package lifecycle harness completed **passed** on the marked KVM
qualification VM VPS2 (Ubuntu 24.04, Linux 6.8.0-147-generic) in 1,732.768
seconds. All five stages passed: r121 baseline, upgrade to r126 with disabled
service-state preservation and rollback backup, rollback to r121 and index
restore, re-upgrade/remove/purge, and clean r126 install. The clean-install
scan staged 1,848,188 path instances (1,652,162 files, 196,026 directories),
reported zero coverage issues, revalidated four inode-reuse paths, and passed
accounting verification. Details and raw artifacts are in
[`r126 Debian lifecycle on VPS2`](docs/qualification/package-lifecycle-r126-vps2-20261110/README.md).

The VM was restored to r121 after the destructive package test. Its follow-up
observation completed with 1,654,904 files, 196,340 directories, zero coverage
issues, and passing accounting verification in 85.222 seconds. Final check:
r121 active and enabled, zero restarts, drops, or overflows. During both the
r126 clean install and r121 restore, SQLite WAL crossed its 256 MiB guard
(sampled peaks 713 MB and 270 MB respectively) and later returned to 128 MiB;
neither run reported dropped events or a write error. The r126 lifecycle
harness passed despite this guard activation, so WAL behavior under sustained
churn remains a reliability/freshness gate rather than a release pass.

**Updated:** 2026-11-10 03:44 UTC

### WAL checkpointing during active reconciliation — 2026-11-10 03:54 UTC

The lifecycle run showed the WAL guard activate during both r126 and the
restored r121 full-root scans. Source review found that the main watcher
explicitly skipped PASSIVE checkpoints whenever a scan worker existed, even
though the incremental reconciler commits bounded batches and SQLite PASSIVE
checkpoints can make progress between writer transactions. The watcher now
attempts checkpoints during a scan when the normal interval is due or the WAL
is oversized/guarded. WAL guard activation is logged as a warning because it
is protective throttling, not itself a database write failure.

The focused concurrent-writer checkpoint test passed. Full all-feature tests
passed **337/337** with two ignored, and the ext4 stale-parent ignored test
passed separately. Inclusive LLVM coverage then passed **339/339**, including
the 100,000-entry reconciliation scale smoke on `/dev/shm` and the ext4
stale-parent integration. Updated totals: **74.13% line, 73.05% region,
77.15% function; branch counters unavailable**. Strict Clippy, formatting,
and `git diff --check` passed.

This is code-level and SQLite concurrency evidence, not yet proof that the
256 MiB guard stays inactive during a real large reconciliation. The changed
source was built as an optimized local production-profile binary and passed
`--version` and `--help` smoke checks (SHA-256
`e06d7c3bc3a8722a414b5a37978afffd7932609d059c1f8a6200a902ac52e5c9`). It has
not yet been rebuilt into a Debian package or exercised through the VPS2
lifecycle/canary. The previously qualified r126 package does not contain this
change. Production remains unqualified; the WAL guard behavior remains an
open runtime gate.

**Updated:** 2026-11-10 03:54 UTC

### r127 lifecycle, WAL finding, and VPS2 restoration — 2026-11-10 04:24 UTC

Candidate r127 was built from clean commit `f0673f5` as an auditable static
musl binary; DEB/RPM packaging, dependency audits, payload/revision/source
checks, and artifact checksums passed. Its Debian lifecycle on VPS2 passed all
five stages in 1,332.532 seconds. The clean-install observation scanned
1,664,114 files / 197,276 directories, reported zero coverage issues, and
revalidated four inode-reuse paths; accounting verification passed. Full
machine-readable evidence is in
[`r127 package lifecycle on VPS2`](docs/qualification/r127-package-lifecycle-vps2-20261110/README.md).

The checkpoint-during-scan source change did not prevent the original WAL
burst. At 04:20:31 UTC, a PASSIVE checkpoint during clean-install publication
returned `busy=1`, `log_frames=-1`, and `checkpointed_frames=-1` while the WAL
was 717,609,272 bytes. Dux paused event writes under its 256 MiB guard; the
scan finished three seconds later with zero coverage issues, and the WAL
returned to 128 MiB. The service reported zero drops, kernel overflows,
restarts, and write errors. This identifies the large publication transaction
as the next design target; the guard still creates a temporary disk and
freshness risk, so it remains an open gate.

VPS2 was restored to the exact pre-test r121 package binary and database. The
saved DB hash matched byte-for-byte and `PRAGMA quick_check` returned `ok`. Its
post-restore scan completed with 1,665,459 files / 197,491 directories, zero
coverage issues, and passing accounting verification in 76.8 seconds; 130
vanished paths were discarded and two inode-reuse paths were revalidated. r121
remains active+enabled with zero restarts, drops, or overflows. Test-only
backups were removed after verification; free space recovered from 2.9 to 4.8
GiB.

**Updated:** 2026-11-10 04:24 UTC

---

## Prior verified checkpoint — 2026-11-09 22:59 UTC

### Clean-install lifecycle readiness gate

Code review of the first lifecycle report showed that merely excluding the
`reconciliation accounting verification pending` reason was insufficient: an
incomplete scan or failed accounting check could then be accepted. The clean
install gate now requires a scan newer than installation and allows only a
clean status or the normal post-accounting hint-dirty reason. It rejects
pending, incomplete, and accounting-failure states; automatic-reconciliation
pause fails with journal diagnostics. Legacy baseline handling remains
separate. Focused lifecycle harness tests pass **72/72**; serial Python
discovery passes **359/359**. Logs and code are in the lifecycle qualification
folder. A real guest lifecycle rerun with this strict gate is still required.

The guarded r123 lifecycle controller remains active but has not launched a
guest. Active Go compiler/test workloads continue to keep it in admission
wait. The guard remains enabled; no VM was launched.

### Coverage gap review

I queried the saved Rust coverage profile without rerunning tests. The 332-test
profile remains 73.73% line / 72.67% region / 76.94% function, with no branch
counters. `watch.rs` has 75.16% line coverage and 74 of 481 functions missed;
the actual `fanotify_init`, `fanotify_mark`, and event-path
`open_by_handle_at` call sites have zero line hits in that unit profile. Live
VPS2 evidence exercises real scans but is not instrumented coverage. These
critical-path gaps and the targeted tests needed to close them are recorded in
[`coverage-gaps-20261109.md`](docs/qualification/coverage-gaps-20261109.md).

### Canary and disk

VPS2 r121 sample **107** at 22:58:11 UTC passed with scan age 568 seconds;
SQLite and path/accounting checks passed. Service restarts, overflows, drops,
pending updates, and OOM events remained zero. The 24-hour canary is about
8 hours 50 minutes in, not release sign-off. `memory.events.high` remains at
1,173. VPS2 root remains 92% used with 7.2 GiB free; its 4 GiB journal is
preserved until the soak finishes. See [sample 107](docs/qualification/production-live-followup-20261109T2258Z/README.md)
and [sample 106 and scan evidence](docs/qualification/scan-first-cost-followup-20261109T2253Z/README.md).

### Measured scan-first overhead

Two consecutive observations completed without coverage issues. One scanned
1.61M files / 192k directories in 275 seconds and verified accounting in
51.3 seconds; the next scanned 1.62M / 192k in 251.8 seconds and verified in
73.9 seconds. Both use about 36% of the 900-second schedule before other Dux
work. Cgroup samples during observation-related intervals used 49–61% of one
core and 0.4–1.6 GB reads plus 0.17–1.74 GB writes per five minutes. A
no-observation interval used 21.4% CPU at 23.6k fanotify events/second. These
are live measurements, not isolated attribution. Reports:
[cycle 1](docs/qualification/scan-first-cost-20261109T2248Z/README.md) and
[cycle 2](docs/qualification/scan-first-cost-followup-20261109T2253Z/README.md).

### Production VPS1

A fresh read-only status at 22:30:40 confirms production still runs legacy
0.6.3 (four lifetime restarts). The index is dirty from fanotify overflow, has
649 lifetime overflow notifications and 2,518,862 dropped updates, and its
last full scan was 380,478 seconds old (about 4.4 days). The database is 3.33
GB with a 128 MiB WAL. `/` is 68% used with 61.7 GB available. No production
state was changed. See [the raw status and assessment](docs/qualification/production-live-followup-20261109T2230Z/README.md).

Local cleanup reclaimed the stale Cargo intermediates while keeping all eight
fault-injection binaries verified. `/` has 58 GiB free, `target/readiness` is
869 MiB, and `target/readiness/vms` is 32 KiB. Cleanup evidence is linked in
[`disk-cleanup-20261109.md`](docs/qualification/disk-cleanup-20261109.md).

**Updated:** 2026-11-09 22:59 UTC

---

## Prior verified checkpoint — 2026-11-09 22:14 UTC

### Local readiness disk cleanup

The stale 56 GiB VM-directory report is not current: `target/readiness/vms` is
32 KiB. Eight inactive fault-test Cargo target trees each held about 240 MiB.
I removed their rebuildable Cargo intermediates with `cargo clean`, while
restoring each unique fault-injection executable at its original path. All
eight binary hashes and version strings match the pre-cleanup inventory. Cargo
reported **1,996.8 MiB removed**; `target/readiness` fell from 2.7 GiB to
**869 MiB**, and `/` now has **57 GiB free**. The temporary preservation copies
were deleted. Details and hashes are in
[`disk-cleanup-20261109.md`](docs/qualification/disk-cleanup-20261109.md).

The guarded r123 lifecycle rerun remains live in admission wait; the host keeps
starting Go compiler/test workloads. No VM is running. VPS2's r121 canary
reached **98 passing samples** at 22:12:57 UTC with zero errors, restarts,
overflows, drops, or pending updates; completed-scan age was 545 seconds. The
24-hour canary remains incomplete. VPS2 root was last checked at 92% usage with
7.2 GiB free; do not vacuum its 4.0 GiB journals before canary completion.

**Updated:** 2026-11-09 22:14 UTC

---

## Prior verified checkpoint — 2026-11-09 22:07 UTC

### Targeted VPS2 disk inspection

At 22:05 UTC, the active canary host had 7.2 GiB free on its 93 GiB root
filesystem (92% used). Targeted file/journal checks found the Dux DB at 744 MB,
WAL at 128 MiB, SHM at 0.6 MiB, canary JSONL at 0.5 MB, and systemd journals
at **4.0 GiB**. These explain only a small part of the 81 GiB used. I did not
vacuum journals or scan the full tree while the canary is active; preserving
the journal window takes precedence until the soak ends.

Canary sample 96 at 22:03:30 UTC remains the latest verified result: pass,
zero errors/restarts/overflows/drops/pending, scan age 887 seconds. The corrected
real lifecycle rerun is still waiting on the local host guard; current checks
see Go test/compiler workloads and load1 above 10.

**Updated:** 2026-11-09 22:07 UTC

---

## Prior verified checkpoint — 2026-11-09 22:05 UTC

### Corrected lifecycle rerun admission and disk

The corrected lifecycle harness and regression are committed and pushed in
`9ec181f`. The focused package lifecycle harness passed **71/71** and serial
repository Python discovery passed **358/358**. A fresh guarded VM controller
is live, but no VM has launched: repeated host Go compiler/test processes have
prevented a 120-second quiet admission window.

The VPS2 r121 canary is active at **96 passing samples** through 22:03:30 UTC.
That sample reported zero errors, service restarts, kernel overflows, dropped
events, and pending updates; scan age was 887 seconds. Its root filesystem is
**92% used with 7.2 GiB free** at 22:05 UTC, down from 7.4 GiB at 21:24. No
cleanup or workload was run on the canary host; storage growth needs review
after the 24-hour soak.

**Updated:** 2026-11-09 22:05 UTC

---

## Prior verified checkpoint — 2026-11-09 21:59 UTC

### Debian lifecycle result and harness gate correction

The r121-to-r123 Debian lifecycle completed on the disposable Ubuntu 24.04 KVM
guest in **88.22 seconds**. All six package stages passed: baseline, upgrade,
rollback, re-upgrade, remove/purge, and clean install. Candidate and installed
binary hashes matched; rollback SQLite quick-check passed; clean install
verified service/unit policy, ownership/modes, disabled tunnel, and no tunnel
identity. The full report is retained at
[`package-lifecycle-r123-20261109/report.json`](docs/qualification/package-lifecycle-r123-20261109/report.json).

Review of that report found clean install returned while status still said
`reconciliation accounting verification pending`. I fixed the lifecycle
harness to wait until that pending state resolves, added a regression test, and
documented the finding in the lifecycle README. The focused lifecycle harness
passes **71/71** and serial full Python discovery passes **358/358**. The
updated harness SHA-256 matches its staged copy. The initial lifecycle run
therefore proves the package operations, but **does not qualify initial
accounting readiness**; repeat the real lifecycle with the corrected gate.

### VPS2 canary

The r121 canary reached **95 passing samples** at 21:58:14 UTC. The sample had
zero errors, service restarts, kernel overflows, event drops, or pending updates;
completed-scan age was 571 seconds. It is still far short of the 24-hour
requirement and does not qualify r123. The VPS2 root-disk check remains 92%
used (7.4 GiB free, last checked at 21:24 UTC); no cleanup or service change
was made there.

The local readiness host has 57 GiB free on `/` and 11 GiB free in `/dev/shm`.
No VM is currently running. The tightened package lifecycle is queued for a
fresh guarded run.

**Updated:** 2026-11-09 21:59 UTC

---

## Prior verified checkpoint — 2026-11-09 21:45 UTC

### Third guarded lifecycle attempt

Attempt 3/5 booted the disposable Ubuntu guest and began the lifecycle
controller's legacy-baseline setup. At 21:41:03 UTC, the runtime guard found a
new Go compiler/test and stopped QEMU. SSH closed before the guest returned a
report; VM cleanup exited successfully. This attempt is **interrupted**, not a
package result, and no lifecycle stage is counted as passed. Attempt 4 has not
started: the controller's renewed admission checks found more Go tests and I/O
pressure before a continuous quiet window completed.

### VPS2 canary

The latest sample checked is **91**, at 21:38:00 UTC, passing with zero
errors, restarts, overflows, dropped events, or pending updates and a 122-second
completed-scan age. This remains an incomplete r121-only 24-hour canary. The
last capacity check still showed VPS2 root at 92% used (7.4 GiB free); no disk
cleanup or service change was performed on the canary host.

**Updated:** 2026-11-09 21:45 UTC

---

## Prior verified checkpoint — 2026-11-09 21:32 UTC

### Guarded lifecycle retry and host load

Guarded attempt 2/5 launched the disposable Ubuntu 24.04 lifecycle VM, but the
runtime pressure guard stopped it **before SSH** when a Go test appeared. The
guard journal records `go test` as the active workload; no package test ran and
no lifecycle result should be counted. The retry controller remains alive.
Its latest admission checks now see repeated Go compiler/test processes,
load1 around 25, and CPU PSI some around 50%, so host admission is correctly
closed. No VM is running at this checkpoint.

### VPS2 canary update

The latest verified r121 canary sample is **89**, captured 21:28:01 UTC. It
passed with zero errors, service restarts, kernel overflows, dropped events,
or pending updates; completed-scan age was 506 seconds. This remains only about
7.4 hours of the required 24-hour canary. VPS2 root remains at 92% used with
7.4 GiB free from the 21:24 UTC disk check; the canary and installed service
were not modified.

**Updated:** 2026-11-09 21:32 UTC

---

## Prior verified checkpoint — 2026-11-09 21:27 UTC

### Full Python qualification suite

After the `/proc` exit-race regression was added, repository Python discovery
passed **357/357 tests** (`PYTHONPATH=scripts python3 -m unittest discover -s
scripts -p 'test_*.py'`). The complete output is retained in
[`test.log`](docs/qualification/python-suite-20261109/test.log). This includes
the focused qualification-helper regression and overlapping package/harness
tests; these totals must not be added to count unique cases. Python compile and
`git diff --check` also passed before commit `cc1870c`.

### VPS2 canary and capacity

The r121 five-minute canary is active. Its latest sample inspected was **88**
at 21:23:07 UTC, passing with zero errors, restarts, kernel overflows, dropped
events, or pending updates; completed scan age was 211 seconds. This is about
7.3 hours of the required 24-hour window and qualifies r121 only. VPS2's root
filesystem was **92% used (7.4 GiB free)** at 21:24 UTC. I did not run further
tree-wide disk scans or delete files while the canary is active. The low free
space is a test-host capacity concern and must be reviewed after the soak.

The local readiness host still has approximately 56 GiB free on `/` and 11 GiB
free in `/dev/shm`; it is not currently disk-blocked. The guarded lifecycle
controller remains active, but repeated Go build/test jobs and elevated I/O
pressure have so far prevented a continuous 120-second admission window. It
has not launched another VM attempt at this checkpoint.

**Updated:** 2026-11-09 21:27 UTC

---

## Prior verified checkpoint — 2026-11-09 21:22 UTC

### Qualification helper `/proc` exit race

The host-workload admission probe now treats `ProcessLookupError` for an
individual `/proc/<pid>/cmdline` read as a process that exited during the scan,
and skips that PID. Other read failures still fail closed and block VM
admission. Regression test added; the focused VM qualification-helper suite
passes **41/41**. This repairs test admission telemetry only; it does not relax
the host-pressure or active-workload guard.

The guarded r121-to-r123 lifecycle retry is still live, but its latest
admission check sees active Go test processes. No VM is running and no package
lifecycle result has been produced by this retry.

**Updated:** 2026-11-09 21:22 UTC

---

## Prior verified checkpoint — 2026-11-09 21:17 UTC

### Architecture and candidate

The selected candidate architecture is scan-first: fanotify supplies lossy
hints; a full-root observation every 15 minutes is the source of index state,
with in-place changed-row writes in schema v7. Traversal remains O(number of
entries). The architecture is implemented in source but is not production
qualified. Candidate r123 is a static-musl package set built from clean source
commit `0cad80e6436202fefc2ff385c18872ce2b4a910c`; later work on this branch has
changed qualification scripts and documents, not that Rust artifact. The best
current size profile is one 1.37M-path synthetic ext4 run at 50% CPU: 196.3 s
and 154 MiB scan RSS. This does not prove the 15-minute freshness SLO,
representative-churn performance, or endurance.

### Tests and security evidence

- Rust all-features release suite: **332/332 passed**, ignored tests included.
  LLVM coverage: **73.73% line / 72.67% region / 76.94% function**; branch
  coverage unavailable. `tui.rs` line coverage is 39.51%, `main.rs` 46.43%,
  and `containers.rs` 67.25%.
- Package lifecycle harness units: **70/70**; current VM qualification helper
  regression suite: **40/40**. The real package lifecycle has **not passed**.
- Latest Cargo audit: **0 known advisories across 229 dependencies**; the
  available advisory database was last updated 16:13 UTC. The packaged systemd
  unit passes `systemd-analyze verify`; offline heuristic exposure is 3.3
  (“OK”), not a security certification. Root plus `CAP_SYS_ADMIN` remains a
  material privilege risk; full threat and unsafe-code review remain open.

### VPS1 production

At **21:17:15 UTC**, the legacy 0.6.3 service was active under PID 1073891 with
four systemd restarts. It remains dirty from fanotify queue overflow; the last
completed scan was **376,073 seconds old (about 4.35 days)**. Status reported
**649** lifetime kernel overflows, **2,518,862** dropped updates, **9** pending
updates, and a **3.07 GiB** database. Since 21:10:40, events seen increased by
119,714 and updates committed by 38,102, while overflow/drop counters were
unchanged. This short uncontrolled interval does not prove recovery or event
producer causation.

### VPS2 canary

The VPS2 service remains on r121, not r123. The formal 24-hour monitor recorded
**86/86 passing five-minute samples** through 21:12:52 UTC. The latest sample
had zero errors, restarts, overflows, drops, or pending updates and a completed-
observation age of 526 seconds. This is about seven hours, not the required
24-hour soak, and qualifies r121 only.

### Package lifecycle and disk

The first real r121-to-r123 Debian lifecycle attempt installed the r121 baseline
and upgraded to r123, but the runtime pressure guard stopped the disposable VM
during rollback when host Go workloads appeared. It is **not** a package
failure or pass. A race in pressure-guard cleanup then lost the guest report;
that qualification harness bug was fixed and its helper regression suite passed
40/40. The guarded retry process remains alive, but admission is closed by
active Go builds/tests and load above threshold; no VM is currently running.
The staged r123 DEB was independently checksum-verified against its build
manifest and lifecycle share.

At 21:17 UTC, `/` had about **57 GiB free** and `/dev/shm` about **11 GiB free**.
Disk is not currently blocking the lifecycle attempt.

### Release decision

**Production sign-off is not met.** Still open: successful exact r123 install,
upgrade, rollback, purge, and clean install; completed 24-hour r121 canary and
r123 runtime qualification; live schema migration/rollback; disk-full, OOM, and
filesystem fault tests; 72-hour endurance; constrained-host p95/p99 performance;
and complete privilege/threat review. Coverage remains below 100%.

The detailed evidence and historical checkpoints remain in
[`PRODUCTION_READINESS_AUDIT.md`](PRODUCTION_READINESS_AUDIT.md) and the linked
qualification folders below.

**Updated:** 2026-11-09 21:17 UTC

**Goal:** production reliability, security, performance, and correctness review

**State:** active; not production-ready sign-off

## Previous recorded checkpoint — 2026-11-09 18:23 UTC

Release candidate package revision 123 was built from clean commit
`0cad80e6436202fefc2ff385c18872ce2b4a910c` as a static-musl binary, DEB, and
RPM. Source identity, package payload identity, checksums, embedded dependency
metadata, and RustSec audits passed; the lockfile audit found zero advisories,
and the exact-binary audit recovered 202 dependencies. The package lifecycle
harness passed **70/70** unit tests; package units passed `systemd-analyze
verify`, offline `systemd-analyze security` scored the service unit 3.3 OK, and
CLI smoke checks passed. The score does not eliminate the root plus
`CAP_SYS_ADMIN` privilege risk. Revision 123 was not installed on VPS2.
Its exact install/upgrade/rollback/runtime qualification remains open. Evidence
is in [`release-candidate-0cad80e-20261109`](docs/qualification/release-candidate-0cad80e-20261109/README.md).

VPS2 remains on release 121 and its active 24-hour canary. Sample 51 at
18:18:25 passed monitor checks. At 18:19 UTC, the service remained under PID
`2561224` with zero restarts, kernel overflows, dropped updates, or pending
updates; last completed scan age was 72 seconds. This is about 4h11m of the
24-hour soak, which remains incomplete. Scan freshness is based on completed-
observation age, not the legacy `behind_seconds` field.

On the updated source, the optimized all-features Rust suite passed **332/332**
with ignored tests included, in a private namespace with loopback ext4 and
separate tmpfs scale scratch. The 100k scale smoke passed; the ext4 stale-parent
integration passed. All-target/all-feature Clippy passed with
`undocumented_unsafe_blocks` denied, as did formatting and diff checks. Fresh
LLVM coverage on this source is **73.73% line / 72.67% region / 76.94% function**.
Branch coverage remains unavailable; lowest line coverage is `tui.rs` 39.51%,
`main.rs` 46.43%, and `containers.rs` 67.25%. Details and limits are in the
[`r121 qualification`](docs/qualification/scan-first-timestamp-fix-r121-20261109/README.md)
and [latest unsafe review](docs/qualification/production-live-checkpoint-20261109/README.md).

Production VPS1 remains on legacy `0.6.3` (`e0cc915f8f37`). A read-only
follow-up at 18:19 UTC showed four service restarts, 636 kernel overflow
notifications, 2,518,862 dropped updates, 97 pending updates, and a last scan
365,390 seconds old (about 4.23 days). The DB occupied 3.21 GB; root had 63.4
GB available. The service cgroup reported no OOM events. No production service
or database state was changed. Details:
[`production live checkpoint`](docs/qualification/production-live-checkpoint-20261109/README.md).

The unsafe-block audit initially found 47 production and 36 test-only blocks
without recognized safety comments. Those blocks now have local safety
explanations, and all-target/all-feature Clippy passes with
`clippy::undocumented_unsafe_blocks` denied. This improves reviewability but
does not prove every unsafe block correct; deeper memory-safety review remains
open.

No production rollout has been performed. Revision-122 canary install/rollback,
the completed 24-hour r121 soak, live schema migration qualification,
disk-full/OOM/filesystem stress, multi-day soak, full security threat review,
and constrained-host p95/p99 performance evidence remain open. Production
sign-off is not met.

**Updated:** 2026-11-09 18:23 UTC
**Goal:** production reliability, security, performance, and correctness review
**State:** active; not production-ready sign-off

## Current status and evidence — 2026-11-09 15:44 UTC

A prior verified product change is commit `5d7cdac`: an active, low-priority
reconciliation is no longer cancelled for *moderate memory PSI alone* when
available memory remains above the elevated-memory floor. Critical pressure,
low available memory, high load, and unavailable telemetry still stop it. This
addresses a verified retry loop: release 119 cancelled a root scan at 5.07%
memory PSI with 3.37 GiB available, then its retry completed 22 seconds after
the canary failed the 1,200-second freshness gate. The failure is preserved;
it is not counted as a pass.

Commit `8b02799` fixes `last_scan_ts` to represent scan completion. Release
0.6.3-121 was built as an audited static-musl package from a clean tree and
installed on VPS2, not production VPS1. The package binary SHA256 is
`b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`; the
installed hash matches. Before upgrade, a verified schema-7 SQLite online
backup (696,860,672 bytes, `quick_check=ok`) and the exact r120 binary, DEB,
and service unit were saved for rollback.

The post-install full root observation completed across 1,698,615 indexed inode
rows and 1,702,097 path instances. Independent accounting passed in 58.337 s,
with zero coverage issues, zero kernel overflows, and zero userspace drops. The
stored scan timestamp advanced from 1794232455 (the old scan-start value) to
1794233078; the new scan started at 1794232765 and completed at 1794233078.
That is runtime evidence that the patched field records completion.

A fresh 24-hour VPS2 canary started at 14:07:03 UTC with five-minute samples
and the intended 1,200-second freshness gate. **20 samples have passed**; the
latest was 15:43:09 UTC. The daemon and monitor remain active with zero restarts.
Sample 3 ran during an active scheduled scan; it completed at 14:20:17 in
about 5m52s, with zero coverage issues and accounting passed in 54.8s.
`last_scan_ts` advanced from scan start 1794233665 to completion 1794234017.
Sample 6 exercised scan completion during collection: its age moved from 706s
with a scan active to 4s after completion, and the monitor accepted the newer
verified graph. Samples 9 and 12 also passed during active scheduled scans;
the scans completed in 260s and 234s respectively, each with zero coverage
issues and accounting passed. The latest scan covered 1,713,383 staged paths. Sample 13 also passed with
that graph at 223–274 seconds freshness. Sample 14 passed at 570 seconds
freshness. Sample 15 overlapped an active scan at 823–936 seconds freshness;
the scan completed in 277 seconds at 15:19:03 with zero coverage issues, and
independent accounting passed in 68.969 seconds. Across all 20 samples there
are zero monitor errors, overflows, drops, OOMs, or restarts. The 24-hour gate
remains open. Sample 16 passed at age 274 seconds on the 1,716,749-path graph;
daemon memory was 248.5 MiB current / 1.75 GiB peak against its 4 GiB limit.
Sample 17 passed at 527 seconds freshness; sample 18 passed after scan
completion with age 4 seconds. Sample 19 passed at 291 seconds with 1,719,379
paths. Sample 20 passed at 604 seconds freshness with zero monitor errors. Daemon peak memory remains 1.75 GiB against its 4 GiB limit.
Monitor limits are 20% CPU, 1 GiB
memory, no swap, IOWeight 10, and Nice 19. Full build, backup, install, and
sample evidence is in
[`release-121 qualification`](docs/qualification/scan-first-timestamp-fix-r121-20261109/README.md).

### Production VPS1 read-only check

Two read-only status captures 58.5 seconds apart show the production service
still running legacy `dux 0.6.3` (`e0cc915f8f37`, binary SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`). It is
dirty from fanotify overflow, with the last full scan about 96 hours old,
`behind_seconds` about 43.3 hours, and four systemd restarts. Kernel overflow
notifications rose **527 → 539 (+12)** in 58.5 seconds while `events_seen`
rose by 1.42M and `events_resolved` by 866,774; no full scan was active and scan age advanced by
59 seconds. The userspace dropped-update counter stayed at 1,511,839. A Go
test process was active during this interval, which is consistent with host
churn but does not prove it caused the overflows. No production service,
database, or configuration was changed. A follow-up read at 12:52 UTC showed
540 overflows and a 345,770-second scan age. At 13:10 UTC the count remained
540, although `events_seen` grew by about 406K since 13:00 and the scan age
reached 346,865 seconds. At 13:14, overflows had risen to 549 in four minutes;
`events_seen` grew another 830K and scan age reached 347,092 seconds. No full
scan was active. These samples confirm bursty overflow recurrence while the
index remains unreconciled and its live counts move.

Raw read-only snapshots:
[`12:49Z`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1250Z.json)
and
[`12:50Z`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1251Z.json),
plus the
[`12:52Z follow-up`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1252Z.json).
The 13:00Z sample is
[`here`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1300Z.json);
the 13:10Z follow-up is
[`here`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1310Z.json).
The 13:14Z follow-up is
[`here`](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-status-20261109T1314Z.json).

### Current local tests

- Rust release/all-features suite on the timestamp-fix worktree: **328 passed, 0 failed, 2 ignored**.
  The ignored tests require the handle-capable ext4 and production-scale profiles.
- The ext4 stale-parent end-to-end test was explicitly run and passed on the
  local ext4 host. The 1M-entry scale test passed: baseline rebuild 62.029 s,
  unchanged in-place reconciliation 83.181 s, DB size unchanged at 321,794,048
  bytes. This unconstrained-host result identifies substantial full-walk cost;
  constrained-host p95/p99 performance remains unqualified. Logs are in the
  [r121 qualification folder](docs/qualification/scan-first-timestamp-fix-r121-20261109/README.md).
- Focused resource-guard suite: **21 passed**; includes moderate memory PSI
  with headroom (continue active scan) and low available memory (cancel).
- Strict all-target/all-feature Clippy, formatting, and `git diff --check`:
  passed.
- LLVM coverage on the timestamp-fix worktree: **72.30% line, 71.15% region, 76.09% function**. Branch
  coverage is unavailable; `tui.rs` and parts of the CLI remain weakly covered.
- Python discovery: **354 passed** for the current canary monitor and harness
  sources; this run predates the Rust guard change and is not additive to the
  Rust test count.
- Package lifecycle/security policy suite: **70 passed** in a focused rerun.
  `systemd-analyze verify` passed for both packaged units. On VPS2's systemd
  255, the installed daemon scored **3.3 (OK)** in `systemd-analyze security`;
  runtime inspection confirmed UID 0/GID `dux`, only `CAP_SYS_ADMIN` and
  `CAP_DAC_READ_SEARCH`, `NoNewPrivileges`, seccomp active, AF_UNIX-only
  sockets, and no private mount namespace (needed for host fanotify visibility).
  This is policy evidence, not proof that root-level compromise is contained.
- Canary monitor suite: **38/38 passed** after replaying v1's failed sample.
  Replay checked the stored field value, which was scan-start time in r120; it
  does not establish that a 1,500-second completed-scan budget is required.
- Fresh online RustSec audit: **0 vulnerabilities and no warnings** across 229
  locked Rust dependencies. Gitleaks history coverage now includes all 654
  commits reachable from local/remote-tracking refs; its three matches were
  verified X.509 signer-label false positives, with no confirmed secrets.
  The packaged service policy and live VPS2 confinement have been checked;
  the broader deployment configuration and root-privilege threat review remain
  open. The Semgrep Registry p/rust fetch still fails TLS verification. A
  pinned local Rust security ruleset ran successfully and produced 133 INFO
  audit reminders (99 generic unsafe-use matches, 34 test temp-directory
  matches); no vulnerability was confirmed by those rules. Unsafe-block review
  and broader application security review remain open. Follow-up review added
  safety comments to eight fanotify parser reads; all eight malformed-input
  tests passed, standard strict Clippy remained clean, and the optional
  undocumented-unsafe lint dropped from 107 to 99 diagnostics.

### Open production-readiness gates

- Produce and audit a static-musl package from the timestamp-fix commit, install
  it on VPS2, then complete a fresh 24-hour
  soak with exact binary/unit identity, accounting, 1,200-second completed-scan
  freshness, restart, overflow, drop, memory, and history checks.
- Production is still on legacy 0.6.3 and the 12:52 UTC snapshot had an
  approximately 96-hour-old full scan, 540 overflow notifications, 1,511,839
  dropped updates, and four restarts. Any rollout of the scan-first candidate requires
  a separate controlled migration/rollback plan; VPS1 has not been changed.
- Measure p95/p99 scan duration and read/write cost against completion-time
  freshness; prior r120 age samples used scan-start timestamps and cannot prove
  completed-scan SLO compliance.
- Qualify an exact migration of the live schema-6 production index. The
  synthetic 1.3M-file/27.3M-growth-row migration passed, but a concurrent-write
  live backup did not make progress and was stopped safely.
- Finish the broader deployment configuration and root-privilege threat
  review; resolve the Semgrep ruleset access gap. The packaged unit and live
  process policy check is recorded, but Dux still runs as root with
  `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH` for filesystem event/handle access.
- Complete OOM, mount/capability, disk-full recovery, WAL endurance, other
  filesystem/kernel, 10M scale, rollback, and multi-day soak profiles.
- Coverage is not 100%; branch coverage is not measured. No evidence supports
  a zero-error or universal-host guarantee.

### Disk snapshot

Local release output was placed in `/tmp/dux-r121-dist`; local root had 55 GiB
free at the last disk check. VPS2 had 7.5 GiB free at 14:10 UTC after the
rollback snapshot and package install. Its WAL peaked near 335 MiB during startup
and stood at 129 MiB at that checkpoint. Continue monitoring disk and preserve
the rollback copy and canary evidence until qualification ends.

## VPS2 canary checkpoint at 22:02 UTC (historical; superseded by sample 13)

The corrected five-minute canary started at 21:05:37 UTC. At this checkpoint,
Its first twelve samples passed (21:07:13, 21:12:14, 21:17:10, 21:22:33,
21:27:17, 21:32:04, 21:37:21, 21:43:18, 21:47:36, 21:52:26, 21:58:42, and
22:02:06 UTC) with no monitor errors.
Candidate and unit hashes, SQLite/schema-v7 graph, and accounting checks passed
in each sample. At sample 12, the service had zero restarts, kernel overflows,
userspace drops, and OOM events. The latest full observation was 535 seconds
old, with no scan in progress. The index remained dirty after filesystem hints. Use
`last_scan_age_seconds` for scan-first freshness; `behind_seconds` is the
legacy event-pipeline catch-up timer. Twelve samples do not qualify a 24-hour
soak. Peak Dux memory observed was about 1.51 GiB against the 4 GiB service
limit. The slowest collection so far took 184.1 seconds, under its 300-second
cadence. Across sample intervals 1→12, Dux used 1,517.8 CPU-seconds over
3,293.4 wall-seconds (46.1% of one core on average); interval rates ranged from 16.5%
to 89.4%. A separate cgroup snapshot at 21:34 showed 1,458 CPU-seconds since
service start, throttling in 8,240 of 31,374 CPU periods (26.3%), and 7,471,202,304
bytes read plus 6,704,386,048 bytes written on the block device. The VM has six
vCPUs; the service hard limit is one core. These early figures include initial
indexing and are not a steady-state extrapolation. The monitor records CPU
usage but does not enforce a CPU or I/O budget, so the passing canary samples
prove health/accounting/freshness gates, not a defined efficiency target. The
append-only evidence through sample 12 is recorded in
[`canary-24h-5m-v2-progress.jsonl`](docs/qualification/release-409a2f4-20261108/live/canary-24h-5m-v2-progress.jsonl).

## Canary I/O instrumentation improvement (2026-11-08)

The source-matched canary collector now captures cgroup-v2 `io.stat` as raw
per-device counters and fails closed on missing or malformed rows. It retains
device identities instead of summing them: a live read-only check found the
same root I/O represented at both `8:0` and `252:0`, so summing rows would
double-count layered block I/O. The monitor source hash is now
`3a32bec1136692e57e7471764f05ce786ddf05d31d909277c9a26ee377d1e77b`; its
focused test file passes **34 tests**.

This telemetry change is not loaded by the active VPS2 canary. Its installed
collector still hashes to `60fd032d34d1deaafd1b686e40357257dafc5eca3c9efe7935eb3d4a32134954`,
and its records do not contain per-device I/O. The existing soak therefore
remains valid for its original health/accounting/freshness gates, but I/O rates
must be measured in a follow-up run. No numeric CPU or I/O acceptance budget is
defined yet.

Latest exact-working-tree coverage from `cargo llvm-cov --locked --all-features
--summary-only --bin dux` (2026-11-08, after the scan-order optimization):

- Line coverage: **72.17%** (18,256 of 25,295 executable lines covered).
- Region coverage: **70.99%**; function coverage: **76.00%**.
- Per-file line coverage: `scan.rs` 92.59%, `store.rs` 96.10%, `query.rs`
  90.56%, `watch.rs` 72.05%, `main.rs` 43.20%, `tui.rs` 39.51%.
- Stable llvm-cov reports zero instrumented branches for these files, so it
  provides no usable branch-coverage measurement; this is not evidence of 0%
  branch coverage.

Coverage is not a correctness probability. UI/CLI paths and watcher paths
still have uncovered executable lines; filesystem-specific and failure-path
qualification remains necessary.
- Fresh `cargo audit --json` at 23:29 UTC exited successfully with zero known
  vulnerabilities and zero warnings across 229 dependencies. The advisory
  database snapshot was updated at 23:03 UTC; the raw report is
  [`cargo-audit-20261108-2329.json`](docs/qualification/release-409a2f4-20261108/cargo-audit-20261108-2329.json).
  This is dependency-vulnerability coverage, not a complete application
  security review.

Coverage is reported with the exact stable and nightly commands recorded in
the run logs. Percentages depend on the compiler/instrumentation mode; compare
results only within the same command and toolchain. The run logs and raw audit
result are in
[`docs/qualification/scan-first-fixed-cadence-20261108/`](docs/qualification/scan-first-fixed-cadence-20261108/):
`llvm-cov-summary.log`, `llvm-cov-branch-summary.log`, and
`cargo-audit-20261108.json`. The branch-coverage report shows all per-module
line, function, and branch totals. No claim of 100% test coverage is made.

## Disk and temporary-cache cleanup (19:03 UTC)

The workspace filesystem now has **72 GiB free (60% used)**. `/tmp` fell from
41 GiB to 8.5 GiB after removing 3.6 GiB of Dux build output and about 32 GiB
of verified inactive Go build caches and old Go build temporary directories.
The current running Go test uses `/tmp/x.7RB3d/go-cache`; that active cache was
preserved. An active PostgreSQL test cluster under `/tmp/pg_virtualenv.*` and
remaining qualification/source artifacts were also preserved. `/tmp` is not
empty; those live or potentially valuable artifacts need a separate cleanup
after their owners finish.

At 21:28 UTC, `/` was 185 GiB with 111 GiB used and 66 GiB free (63% used);
`/tmp` was 8.6 GiB and `/home/dux/target` was 3.3 GiB. `/tmp` included a 2.1
GiB actively changing tree while Go build processes were running, a live 76
MiB PostgreSQL regression cluster, and large XGS/xtop qualification and replay
artifacts. These were preserved because they were active or potentially useful
test evidence. Two `go-build*` temp directories (128 MiB combined) had no open
files; an attempted targeted removal was rejected by shell safety controls.

At 22:02 UTC, `/` had 68 GiB free (62% used), `/tmp` was 8.3 GiB, and
`/home/dux/target` remained 3.3 GiB. Space is not close to exhaustion.

## Live VPS1 production snapshot (2026-11-08 19:29 UTC)

Read-only inspection of the production host found the installed binary is
`dux 0.6.3` from revision `e0cc915f8f37`, still on legacy event-apply mode.
`dux status --json` reports `dirty=true`, reason `fanotify queue overflow`,
235 kernel overflow notifications, 62.88M events seen versus 37.31M resolved,
and the last completed scan 283,218 seconds (78.7 hours) old. The index is not
caught up. The host has seen no userspace update drops in the current status
sample.

Systemd reports the service active, with four restarts in its current manager
counter and `Restart=always`. The journal records each observed exit as
`filesystem mounts changed; restart daemon to refresh watch coverage and
reconcile` followed by exit status 1, then systemd restart. It also repeatedly
logs that automatic reconciliation remains paused after two failed attempts.
This is intentional mount-topology restart behavior, not evidence of a panic;
it still means restarts are part of the live service behavior, and the old
daemon has not repaired the stale index. VPS1 was inspected only; no service,
binary, or database changes were made.

### VPS1 read-only recheck (2026-11-08 21:29 UTC)

The production service remains on `dux 0.6.3` revision `e0cc915f8f37`
(`source-sha256=b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`;
binary SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`).
It is active with four systemd restarts. Read-only status reports dirty from
fanotify overflow, 281 cumulative overflows, 73.96M events seen versus 43.41M
resolved, 10 pending updates, no userspace drops, and `caught_up=false`. The
last completed scan is 290,387 seconds old (about 80.7 hours); the reported
event-pipeline behind time is 100,698 seconds. Recent journal samples contain
repeated `open_by_handle_at` `ESTALE` warnings and slow 128-update flushes taking
257–315 ms. This confirms the legacy production install remains stale under
churn. VPS1 was inspected only; no service, binary, or database changes were
made.

### VPS1 overflow trend recheck (2026-11-08 21:43 UTC)

The daemon remains dirty from fanotify overflow and is not caught up. Its
cumulative overflow counter was 295 both at 21:39 and 21:43 UTC, so no new
overflow notification was counted in that four-minute interval. The index
still had 466 pending updates, 76.32M events seen versus 44.66M resolved, and
the last completed scan was 291,252 seconds old (about 80.9 hours). In the same
interval the journal contained 169 slow-flush warnings. This short interval
does not show recovery; the index remains severely stale. VPS1 was inspected
only.

### VPS1 backlog recheck (2026-11-08 21:45 UTC)

The cumulative overflow counter remained at 295, but pending updates rose from
466 at 21:43 to 48,124 at 21:45. The daemon remained dirty with
`caught_up=false`, no reported userspace drops, and the last completed scan
291,369 seconds old (about 80.9 hours). This is active backlog growth without a
new overflow-counter increment, not recovery.

### VPS1 backlog recheck (2026-11-08 21:58 UTC)

The service remained dirty from fanotify overflow with `caught_up=false` and no
userspace drops. Its cumulative overflow count stayed at 295. Pending updates
fell from 23,807 at 21:50 to 181 at 21:58, but the last full scan was 292,172
seconds old (about 81.2 hours). The journal contained 117 slow-flush warnings
since 21:50. The event backlog drained substantially during this interval, but
the last full observation remains over 81 hours old; this is not index recovery.

### VPS1 recheck (2026-11-08 22:02 UTC)

Since 21:58, the cumulative kernel-overflow count rose from 295 to 312 (+17 in
about four minutes). The daemon remains dirty and not caught up, with 78 pending
updates, no userspace drops, and a last full scan 292,410 seconds old (about
81.2 hours). The journal recorded 248 slow-flush warnings and 17 stale-handle
warnings since 21:58. This shows fresh kernel loss despite the short-lived
pending-update drain. VPS1 was inspected only.

The container writable-layer parser had a verified undercount for OverlayFS
upperdir paths containing escaped comma, whitespace, or backslash bytes. The
kernel emits these through octal `seq_escape` values; Dux previously used the
encoded spelling as a path and could report zero bytes. Source now decodes the
kernel format and returns an error for malformed, empty, or relative upperdir
values, so the caller cannot silently fall back to zero bytes. Focused tests
pass. This change is not in the live VPS2 Dux binary and still needs a real mounted-container
qualification. Details: [OverlayFS path-escape regression](docs/qualification/container-overlay-upperdir-escapes-20261108.md).

The preceding default-feature host binary predates explicit error propagation
for malformed OverlayFS upperdirs and is not the current candidate. A clean,
source-matched static-musl package build and embedded binary audit are pending;
the earlier package attempt was discarded because the source tree changed
during the build and the provenance check correctly rejected it.

### Current live candidate checkpoint

The VPS2 full-root scan-first candidate's first monitored canary attempt ran
from 18:34 UTC until the fail-closed monitor stopped after sample 2 at 19:36.
Sample 1 at 18:35 passed binary/service identity, schema-v7 path/accounting,
SQLite quick-check, freshness, resource, and critical-journal checks. Sample 2
failed because the journal contained a 19:16 full-root scan with one
inode-incarnation coverage issue and a bounded retry scheduled. Dux itself
remained running with zero restarts.

At 19:20 UTC a periodic observation was in progress at about 1.37M files and
164k directories; the prior completed observation was 1,364 seconds old, over
the 1,200-second age bound while the new pass ran. It completed at 19:22:05
with 1,368,963 files, 164,146 directories, and zero coverage issues after
pruning 60 vanished paths; accounting verification passed at 19:22:55.

The retry after the 19:16 conflict completed at 19:36:10 with zero coverage
issues across 1,370,983 files and 164,433 directories; accounting verification
passed at 19:36:52. However, the monitor had already failed at 19:36:12 because
its strict journal gate found the earlier conflict. At that sample the last
completed scan was 196 seconds old; SQLite/path-graph accounting passed, and
there was no service restart/OOM, kernel overflow, or userspace drop. This
demonstrates bounded recovery from this incident but does not erase the failed
canary result or prove 24-hour stability. The failed run's JSONL and filtered
journal are preserved as
[`canary-d8ea72d-failed-sample2-20261108.jsonl`](docs/qualification/scan-first-fixed-cadence-20261108/canary-d8ea72d-failed-sample2-20261108.jsonl)
and
[`canary-d8ea72d-failure-recovery-journal-20261108.log`](docs/qualification/scan-first-fixed-cadence-20261108/canary-d8ea72d-failure-recovery-journal-20261108.log).
After verifying the 19:36 scan and accounting pass, a separately named retry
canary started at 19:45 with the same Dux binary, 20% CPU quota, and 1 GiB
memory cap. Its first hourly sample passed. Because hourly sampling leaves a
blind interval much longer than the 20-minute freshness gate, that collector
was stopped after preserving its first sample. A 15-minute retry also passed
its first sample and was stopped after preserving the record. The current
collector, `dux-scanfirst-canary-d8ea72d-retry4-5m.service`, started at 19:56
with a five-minute sample interval, a 20-minute observation-age ceiling, and
the same resource caps. Its first two samples passed at 19:57:11 and 20:02:55. Sample 3
failed at 20:09:44: the last accepted observation was 1,308 seconds old (over
the 1,200-second limit), a full observation was still in progress, and the
journal contained a scan-coverage failure. That scan (20:02:56–20:05:10)
pruned 48 vanished paths but retained one inode-incarnation conflict. Its
bounded retry also failed at 20:10:57 with one conflict and scheduled another
retry. At 20:15:25 that retry was still scanning and last observation age was
1,649 seconds. It finally completed at 20:18:42 with zero coverage issues
after pruning 60 vanished paths; at 20:19:14 independent accounting was still
pending and last-scan age was 377 seconds. The service remained active with
zero restarts and zero kernel overflows. Cumulative I/O by 20:19 was about
31.0 GB read / 31.8 GB written; peak memory was 2.0 GiB of a 4 GiB cap. The
eventual recovery does not undo the canary failure or 20-minute freshness
breach. At 20:26:34 a fresh VPS2 status check still showed the service active
with zero restarts, zero kernel overflow notifications, and no scan currently
in progress, but the index remained dirty and `caught_up=false`; its reported
`behind_seconds` was 7,383. This is a later runtime snapshot, not a canary pass.
The monitor and journal are preserved in
[`live-retry4-5m/`](docs/qualification/scan-first-fixed-cadence-20261108/live-retry4-5m/).
None of the 24-hour attempts completed; production VPS1 was not changed.

The current working tree now revalidates a still-present non-directory leaf
when every ancestor is stable and `symlink_metadata` proves the path now names
a different current inode incarnation. It refreshes identity, birth time,
mount ID, allocation, owner, and mtime in the staging database; ambiguous
directory replacements still fail closed. A deterministic ext4 test forces
inode reuse under a second name and recreates the original name with a
different inode; the test compares all five paths and their identities plus
root allocation/inode totals with a direct filesystem oracle. It passed 10/10
repeated runs, the three focused inode-reuse tests passed, and the full
all-features suite passed. This fix is not yet built into a release, installed
on VPS2, or validated against the observed live failure.

The 15-minute retry's first sample at 19:54 matched the expected candidate and
unit hashes and passed schema-v7 graph/accounting, SQLite quick-check, service
health, and critical-journal checks. The completed observation immediately
before it covered 1,373,609 files and 164,740 directories with zero coverage
issues; independent accounting verification passed in 68.5 seconds. At sample
time the last observation was 375 seconds old. The index correctly remained
dirty because post-observation filesystem hints had arrived. The service had
zero restarts, kernel overflows, OOM events, or reported userspace drops. This
is one passing sample, not evidence of sustained convergence or bounded total
overhead. A nearby systemd snapshot showed high cumulative read/write I/O and
CPU use, so overhead remains a release gate.

The release-105 artifact was built from clean source commit `30bf32d` with
`worktree_dirty=false`. Its static binary SHA-256 is
`8f6e8c14307058258b905d7e6b2530bc5465b66fd3d89784fcd55147add82ef4`; the DEB
and RPM payload binary hashes match it. All eight release-artifact checksums
passed, the static binary reports the expected source hash, and `dpkg-deb` and
`rpm` recognize the packages. The lockfile audit found zero known advisories
across 229 dependencies; embedded binary metadata identifies 202. Packages
are retained under ignored `target/readiness/release-30bf32d/`; audit and
manifest evidence is in
[`docs/qualification/release-30bf32d-20261108/`](docs/qualification/release-30bf32d-20261108/).
The exact standalone release binary also passed one read-only
`containers --json` integration smoke on VPS2 against 29 real Docker records
(20 running, 9 stopped). It was copied to a qualification path; no package or
service was installed or changed. This does not qualify the daemon or parser
handling of escaped paths end to end, and the release-105 artifact has not
been used by the VPS2 daemon canary.

## Latest fixed-cadence implementation and VPS2 evidence (11:54 UTC)

Commit `559097852ac5333bf575299394758af7b7a9cb3d` removes two causes of
unnecessary full-root scans in scan-first mode: hint-ring/resolver loss no
longer directly requests a scan, and a dirty marker no longer triggers the
legacy 300-second retry. Manual requests and bounded retries for failed scans
or accounting checks remain. The configured 15-minute observation timer remains
the normal repair path.

The exact source passed **301 Rust tests, 0 failures, 2 ignored**, formatting,
strict Clippy, and diff checks. Static-musl DEB/RPM package revision 101 was
built and installed on VPS2. During a 40,000-file create/delete burst, the
candidate reported repeated userspace hint-ring and resolver loss, and zero
kernel overflows. Its first observation had one coverage issue and correctly
scheduled a bounded retry; the retry completed with zero coverage issues.
There was no repeat scan at the old 300-second dirty-retry boundary. The
15-minute observation started at 11:50:07, exactly 15 minutes after the retry
scan began, but finished with 155 inode-incarnation conflicts. Dux kept the
index dirty and scheduled another bounded retry; eventual recovery from those
conflicts remains unverified. The daemon had zero restarts. Before that
scheduled pass, read-only SQLite quick-check, row-type, and recursive-accounting
checks passed. The status remained `caught_up=false` after hint loss.

This is concrete improvement evidence, not a production sign-off. The test
still produced sustained hint-ring overflows, and a full-root scan found 155
inode-reuse conflicts while ClickHouse and Docker were active. The service used
substantial CPU and disk I/O. The 15-minute freshness SLO, sustained/repeated high-rate churn,
72-hour current-candidate soak, other filesystems/kernels, and production
canary remain open. A clean-tree static-musl package 102 is built and audited
from commit `59f846c`; its Rust sources are identical to tested commit
`5590978`, but package 102 has not been installed on VPS2. The live runtime
test used package 101 built from that exact implementation commit.
Full details and logs: [fixed-cadence qualification](docs/qualification/scan-first-fixed-cadence-20261108/README.md).

### VPS2 follow-up snapshot (12:02 UTC)

Recovered the 11:57:14 bounded retry outcome: it completed at 12:00:09 with
zero coverage issues across 1.44M indexed inodes, after the preceding scheduled
scan failed on 155 inode-incarnation conflicts. This confirms retry completion
for that particular conflict sample. However, the scan log says filesystem or
fanotify hints changed during the pass. At 12:02:23, the daemon was active with
zero restarts but remained dirty and not caught up; the status reported 6.27M
events seen, zero resolved events, repeated userspace hint-ring overflow, and
no kernel overflow. Since package 101 started, systemd counters were 739.7s
CPU, 7.2 GiB read, and 10.8 GiB written. This is continuing evidence that the
candidate can complete a retry, but cannot presently converge while hint loss
keeps recurring; the freshness SLO and acceptable overhead remain unproven.
See [the captured runtime snapshot](docs/qualification/scan-first-fixed-cadence-20261108/post-retry-runtime-snapshot-1202.log).

At the next interval, the scheduled scan started at 12:12:14 and completed at
12:15:03 with 45 inode-incarnation coverage errors across about 1.46M staged
paths. It remained dirty and scheduled a bounded retry after 8 minutes; service
restart count stayed zero. Cumulative service counters reached 939.9 CPU
seconds, 8.8 GiB read, and 13.5 GiB written. This repeats the live convergence
failure, while confirming the cadence timer itself fires. Source inspection
traces the fail-closed condition to concurrent `(device,inode)` reuse: v7
paths store birth time, but the compatibility `inodes` table is keyed only by
`(dev_id,inode)`. Do not suppress this check without incarnation-aware
allocation and path revalidation. The reproducible regression gate and design
constraints are recorded in [the architecture decision](docs/ARCHITECTURE_EVOLUTION.md#additional-inode-reuse-requirement-from-vps2-2026-11-08).

The 12:23 bounded retry completed at 12:26:29 with another 35 incarnation
conflicts across 1.30M files and 156k directories. At 12:30:53, the daemon was
still active with zero restarts, but had 12.28M events seen, zero resolved,
and remained dirty with retry backoff raised to 16 minutes. One hour after
package start, service counters were 1,213 CPU seconds, 11.2 GB read, and
17.5 GB written. This is repeated scan non-convergence and substantial
overhead on VPS2; it blocks any production-readiness claim. See the
[12:30 runtime snapshot](docs/qualification/scan-first-fixed-cadence-20261108/post-retry-runtime-snapshot-1230.log).

## Deterministic inode-reuse race regression (12:30 UTC)

Added an ext4-only scan test with a `cfg(test)` pause hook after the old
incarnation has been statted. The test unlinks that file, retains candidate
inodes until ext4 reuses the exact number with a different birth time at a
second path, and resumes the scan. It passed **10/10 consecutive runs** on the
local ext4 host. Every run reported the conflicting incarnation and kept the
prior database rows plus dirty marker; it did not publish the unverified new
path. The full suite now passes **302 tests (2 ignored)**; formatting, strict
Clippy, and an optimized release build also pass.

This gives a deterministic reproduction and confirms the current fail-closed
guard prevents a false clean update for this case. It does **not** fix live
convergence, establish repeated-reuse or forced-hint-loss behavior, or alter
the production binary. Details:
[`inode reuse race qualification`](docs/qualification/scan-first-inode-reuse-race-20261108/README.md).

## Latest exact-candidate evidence (2026-11-08)

Commit `ea642ed` filters well-formed fanotify records from Dux's own PID before
they consume bounded hint-ring capacity. It preserves overflow records and
passes malformed records unchanged to the canonical fail-closed parser. On
VPS2, the exact optimized candidate (SHA-256
`f5318e04ecbd7ca460c75a850e369b534ac9fdaa553d70812b1c003290da0d24`) passed
the final-source Rust suite (300 passed, 2 ignored), strict Clippy, and an
optimized build. Two 100k-file / 20k-mutation ext4 churn runs passed twenty
independent path/inode/allocation/rollup exactness checkpoints at a 25% CPU
quota. One concurrent scan was correctly marked incomplete and recovered
through bounded retry; the repeat passed without an uncertain checkpoint.

The exact same binary also passed a forced kernel-overflow test: 18,432 creates
overflowed a 16,384-event queue, the overflow counter advanced from 0 to 1, and
automatic full reconciliation returned an exact, clean result in 7.147 s.
It also passed isolated 1M- and 1.37M-path scale profiles. At 1.37M, the full
scan took 196.253 s at 153,180 KiB peak scan RSS; exact materialized counts,
queries, TUI latency, and SQLite integrity passed. The worker used a 50% CPU
quota and a 2 GiB memory cap; cgroup peak reached 2 GiB and used 114 MiB swap.
At 1M, scan time was 115.384 s at 153,844 KiB peak scan RSS. These are single
profiles, not repeat variance or a 15-minute scope-age guarantee.

The current lockfile audit also passed `cargo audit --deny warnings`: 229
dependencies and zero advisories against the RustSec database last updated
2026-11-06. A binary-level audit of this GNU qualification binary is
incomplete because it lacks `cargo-auditable` metadata; `cargo audit bin`
recovered only 43 dependencies. The official static-musl build path requires
`cargo-auditable`, but that exact release artifact has not yet been built and
qualified with this source.
Machine-readable evidence and logs are in
[`docs/qualification/scan-first-self-filter-20261108/`](docs/qualification/scan-first-self-filter-20261108/).

This materially improves evidence for churn and overflow recovery on one VPS2
ext4 fixture. It does not resolve the broader release blockers below: the false-clean
failure observed on the older `c323aee` candidate still lacks a reproduced root
cause; the current exact candidate has only two 100k churn runs and one run at
each 1M and 1.37M scale; the 15-minute freshness SLO, 10M scale, sustained/repeated
overflow, other filesystems/kernels, 72-hour soak, package rollback/security
review, and 24-hour production canary remain unqualified. Production remains on its older
installed daemon; nothing was deployed by this work.

## New source change after the exact-commit qualification

Resolved in-root fanotify parent hints now feed a bounded (256-path) priority
set into the next full-root scan. It matches child names at hinted ancestor
directories without constructing candidate full paths; jwalk moves matching
branches to the front of the scheduling batch. The scan still visits the
complete root, and only a completed, verified observation can establish
freshness. Hints received during a scan are retained for the following scan.
The full local suite passes (297 passed, 2 ignored); strict Clippy, formatting,
and diff checks also pass. A same-fixture test confirms the complete indexed
inode and dirent rows match with and without hints. No throughput or hinted-path
latency improvement has yet been measured. Exact-commit churn qualification
for this source change is recorded below and is currently inconclusive.

## Exact-commit c323aee churn qualification (19:01–19:59 UTC)

The optimized `c323aee` candidate was exercised on VPS2 with a 100,000-file
ext4 loop image, 20,000 seeded mutations, a 25% one-core CPU quota, and a 4
GiB memory cap. This is the commit that adds best-effort traversal ordering
from resolved directory hints. Its result is **inconclusive and blocks
release**:

- The first run failed after 6,000 mutations. The independent filesystem
  oracle found 276 path/identity mismatches that remained through 180 seconds
  of watcher cycles, while daemon status reported no dirty reason, uncertainty,
  dropped hints, or kernel overflow. This is a false-clean correctness
  failure, regardless of whether the ordering change caused it.
- Five reruns of the exact same candidate and fixture passed all 20,000
  mutations, with maximum batch convergence of 148.9 s, 134.0 s, 126.9 s,
  125.2 s, and 118.0 s.
- An older `ab8735c` binary passed the same diagnostic-harness fixture, with
  maximum batch convergence of 114.9 s. This is not enough to establish a
  performance or reliability regression: each is a single run with variable
  concurrent load.

All five passing runs, as well as the failed run, recorded zero dropped events
and zero kernel overflow notifications. Because the failure was not retained
with the newer row-level diagnostics and was not reproduced, its cause remains
unknown.
The added harness diagnostics now report actual missing, extra, and
wrong-identity dirents, with a unit test for those cases. Further exact-commit
reproduction is needed before this candidate can be called correct or better.
Reports, run identities, and the failed-run metadata are in
[`docs/qualification/scan-first-churn-c323aee-20261107/`](docs/qualification/scan-first-churn-c323aee-20261107/).
The latest two reruns used the same optimized binary SHA-256
`c8c3e619fad2a3e28a32dff986ff3d81ec414168f45fabeaee75bd91193b2aed`; its
diagnostic harness SHA-256 is `d4ac45bfc97f4ed762ec36805f3c8361a5435782ddc65d3834754a707b682649`.
The last two took 805.7 and 850.0 seconds wall, used 168.9 and 194.2 seconds
CPU, and peaked at 1.1G and 1.2G memory respectively, with no swap peak.
VPS2 `/` had 19 GiB free after the runs; `/dev/shm` was restored to 3 GiB
with the separate soak still active. The failure remains a release blocker
until its cause and false-clean status behavior are explained. Production
remains untouched.

## Latest 100k reconciliation-first qualification (18:17 UTC)

The clean optimized build from pushed commit `ab8735c` passed the
production-shaped churn suite on VPS2 using an isolated disposable ext4 loop
filesystem: **100,000 seeded files, 20,000
mutations in ten 2,000-operation batches, and a full reconciliation forced
during concurrent writes at the midpoint**. Every batch matched the
independent filesystem oracle for paths, inode allocations, and all rollups;
history continuity passed; the harness recorded zero dropped indexed events
and no uncertain status at checkpoints. The qualification unit ran for **874
seconds** wall / **201 seconds** CPU at a 25% one-core quota, with a 4 GiB
cgroup memory limit and 1.2 GiB measured peak. Maximum batch convergence was
**118 seconds**.

The midpoint scan observed transient coverage errors while files were changing;
Dux retained uncertainty, retried after bounded backoff and pressure admission,
then converged exactly after churn stopped. This validates recovery for this
ext4/Linux 6.8 fixture. It does not establish the 15-minute freshness SLO,
10k/sec or larger event-rate behavior, other filesystems, endurance, packaging,
or a production canary. Userspace hint-ring loss warnings still appeared in
the daemon log; this run proves recovery from those losses in this fixture, not
that ring loss is eliminated.

The passing candidate binary SHA-256 is
`2b618673cddd859e08999cffd2af289f5d15d37b8d133f16e616ddfddd2fbba0`, with
embedded source SHA-256
`9d472c81664a1e709f60c280d27a5f2b4a3637811239aa97869f495bbf94373b`.
Harness SHA-256 is
`fd669c3691da97b2fe899b441724ccd01a2839d10105ddc0a8ec4ea47123f2f2`.
Machine-readable result: [exact-commit 100k churn report](docs/qualification/scan-first-churn-20261107/report-ab8735c.json).
An earlier same-code run also passed before the clean-commit repeat:
[first passing report](docs/qualification/scan-first-churn-20261107/report-fixed.json).

This followed a failed 100k startup attempt on the previous candidate. That
run stopped before mutations because repeated hint-ring loss kept status
uncertain after two completed scans. It led to the policy change: hint loss
during a complete, accounting-verified scan no longer triggers an immediate
redundant full scan. The failed attempt and log are retained for comparison.

## Prior 100k reconciliation-first failure (17:32 UTC)

The current GNU candidate (binary SHA-256
`67e4c964a26d5f4ae4f5b25af1adf2e932c89953cef9d1447ceea4b64c376c1e`) was
run on VPS2 against a disposable ext4 loop filesystem with 100,000 seeded
files, a 25% one-core CPU quota, and a 4 GiB worker memory limit. This attempt
**failed before the churn mutations began**: startup reconciliation repeatedly
reported userspace hint-ring loss while scans were active; two full-root
observations completed but the watch status remained uncertain, and the
readiness harness timed out waiting for pipeline convergence. The recorded
fanotify kernel overflow counter was zero, and indexed event counters/drops
were zero. This is a real candidate failure, not a successful churn result.

The harness was changed to request a scan after each mutation batch in
scan-first mode; without that, its 90-second exactness check incorrectly waits
for the candidate's 15-minute cadence. With that fix, the run still failed in
the initial convergence gate, before exercising any mutations. The log shows
repeated **userspace hint-ring** overflow warnings and immediate follow-up
scans. The cause of those ring drops is not proven; filesystem-wide marks also
cover the database/WAL when they share the watched filesystem, so
self-generated write notifications remain a hypothesis, not a confirmed
cause.

Evidence: [attempt report](docs/qualification/scan-first-churn-20261107/attempt-100k-report.json),
[daemon log](docs/qualification/scan-first-churn-20261107/attempt-100k-daemon.log),
and [run metadata](docs/qualification/scan-first-churn-20261107/attempt-100k-metadata.json).
The 72-hour soak remains on an earlier candidate and does not cover this
failure. No production service or index was changed.

## Latest architecture qualification result (17:09 UTC)

Built the current reconciliation-first source on VPS2 with
`cargo build --locked --offline --release --bin dux` (GNU release; not the
packaged static-musl artifact). Binary SHA-256:
`67e4c964a26d5f4ae4f5b25af1adf2e932c89953cef9d1447ceea4b64c376c1e`.
The isolated ext4 overflow suite passed under a 25% CPU quota: it generated
18,432 files against a 16,384-event kernel queue, observed one kernel overflow,
completed automatic full-root reconciliation and accounting verification, and
matched an exact filesystem oracle with clean status. Recovery took **13.063
seconds**. The reader's bounded userspace hint/resolver queues also reported
loss during the storm; the full observation repaired it without operator
intervention. Machine-readable evidence and run identity are in
[`docs/qualification/scan-first-overflow-20261107/`](docs/qualification/scan-first-overflow-20261107/).

The run exposed and drove fixes for stale kernel-queue-empty state after an
idle reader poll and for scan-first accounting being blocked by unrelated
queue activity or unresolved hints arriving after a completed observation.
Active-scan unresolved hints and resolved in-scope changes still invalidate
that observation. Several initial qualification attempts also exposed harness
issues (fault-injection noise and reading the log before recovery completed);
those were corrected before this passing run. This is one bounded ext4 fixture
run, not production-scale or endurance qualification. The packaged static-musl
build, 100k/1m/10m budgets, additional filesystems/kernels, 72-hour soak, and
production canary remain open. No production service or settings were changed.

The prior source test set passed **294 tests** (2 ignored); the readiness
Python harness now has **37 tests**; strict all-target/all-feature Clippy,
formatting, and diff checks passed. The soak process on VPS2 remains separate
and was not restarted or modified by this work.

## Live production evidence

Fresh read-only sample at **17:12 UTC**: `dux.service` is active with **2
lifetime restarts**; its current process started at 16:18:43 UTC. The installed
`/usr/bin/dux` remains 0.6.3 (source hash
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`; binary
SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`).
The index is dirty from fanotify overflow: **32 kernel overflow
notifications**, **0 userspace updates dropped**, **4 pending updates**, and
about **53 min behind**. The last full scan is **52 h 23 min old**. It reports
1,274,678 indexed nodes and 127,961,706,496 indexed bytes; the SQLite file is
2,407,358,464 bytes, WAL about 18.1 MiB. It remains without a scan in progress
and has no write error. These are live production observations, not a
controlled causal test. The service, index, and settings were not changed.
Raw capture: [`production-1712.json`](docs/qualification/live-prod-20261107/production-1712.json).

Earlier read-only sample at **16:00 UTC**: the same installed binary was dirty
with 169 kernel overflows, 505,945 dropped userspace updates, 29 pending
updates, and 13 h 32 m reported behind. The service had one lifetime restart
at that point. The 16:18 restart and counter reset occurred externally; no
restart was initiated by this work. These are uncontrolled production
observations.

Fresh sample at **14:01 UTC**: the production daemon remains active but the
index is dirty from fanotify overflow, about **11 h 32 min behind**, with its
last full scan about **49 h 12 min old**. The kernel overflow counter is now
**131** (up from 118 at 13:56); dropped userspace updates remain **505,945**,
and pending updates are **177**. No vps1 load test ran during that interval;
the increase is an uncontrolled production observation, so its producer is
unknown. The SQLite DB is **2,345,058,304 bytes**, WAL **128 MiB**, and root
free space is **61 GiB**.

Read-only SQLite inspection confirms `/var/lib/dux/dux.db` is **schema v6**;
it has `inodes` and `dirents` but lacks the v7 `paths` and `dir_totals` tables.
The installed daemon is `dux 0.6.3`, source hash
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`, and
systemd still reports “Accounting recovery paused after repeated failures;
operator action required.” This is not the current candidate binary.

Latest read-only sample at **13:44 UTC**: the same daemon remains active, but
the index is still dirty from fanotify overflow, about **11 h 15 min behind**,
with the last full scan about **48 h 55 min ago**. Counters now show **118
overflows**, **505,945 userspace updates dropped**, and **12 pending updates**;
the index has 1,385,212 nodes. The DB remains 2,345,058,304 bytes, the WAL is
128 MiB, and the root has **60 GiB available**. Journal records show one more
overflow at 13:41 after the scale run; its exact causal relationship to the
run is not provable from the log. The daemon is still making event commits but
has not established a clean baseline.

At **13:49 UTC**, it remains dirty and not caught up, with **118 overflows**,
**505,945 dropped userspace updates**, and **7 pending updates**; it had
committed further events since the previous sample. The overflow count has
not increased since 13:41. Index freshness continues to age despite event
commits.

At **13:55 UTC**, overflow and dropped-update totals were unchanged, while
pending updates had risen to **747**. This remains a fluctuating backlog; the
daemon has not rebuilt a trusted baseline.

Read-only sample after the local scale run at **13:38 UTC**:

- The installed `/usr/bin/dux daemon /` (PID 988284) remains active and still
  reports “Accounting recovery paused after repeated failures; operator
  action required.” Its index is dirty due to fanotify overflow, is about
  **11 h 09 min behind**, and the last full scan is about **48 h 48 min** old.
- Status reports **117 kernel overflows**, **505,945 userspace updates
  dropped**, **1,377,799 indexed nodes**, and **158,787 pending updates**. The
  DB is **2,345,058,304 bytes** and its WAL is **128 MiB**. The root filesystem
  has **62 GiB available** (66% used).
- The preceding recorded sample at 13:09 had 83 overflows and zero userspace
  drops. The scale run below created and deleted 1.37 million files under
  `/tmp` on this same host. The service journal names the test process and
  records additional overflow warnings during that run. The run therefore
  materially stressed the live watcher and is not isolated evidence. The
  fixture and staging DB are removed. No service, configuration, or production
  index changes were made. Do not repeat filesystem-churn qualification on
  this host; use an isolated VM or filesystem.

Read-only sample from `/` on the production host at **13:09 UTC**:

- Installed daemon is active (PID 988284), with one lifetime restart. systemd
  enforces a 100% one-core CPU quota and a 4 GiB memory limit.
- The index is **dirty and not caught up**: dirty reason `fanotify queue
  overflow`, 83 kernel overflows, about 10 h 41 min behind, and about 48 h
  20 min since the last scan. It reports 1,412,175 indexed nodes, 83 pending
  updates, no active scan, and no write error.
- Lifetime watcher counters are 36,877,707 events seen and 22,662,116 resolved;
  userspace drops remain zero. The SQLite file is 2,301,034,496 bytes and the
  WAL is 128 MiB.
- `/proc/sys/fs/fanotify/max_queued_events` is 65,536. The root filesystem is
  ext4 with about 60 GiB available. The installed service has not been changed
  by this work.

These observations show a real coverage gap. They do not prove that CPU
throttling alone caused the overflows or that build churn is the only producer.
The last recorded event commit was current at sample time, while the index
remained 9.88 hours behind and its last full scan was 47.54 hours old. The event
path is still making commits without repairing the existing uncertainty: event
progress is not equivalent to reconciliation or a trustworthy baseline. The
overflow count rose from 65 at 11:50 to 80 at 12:11 UTC and stayed at 80 through
12:21, then rose to 83 by 13:09. Pending updates fell from 52,258 at 12:11 to 5
at 12:18, then rose to 83 by 13:09. This is an uncontrolled observation, not a
CPU-throttle A/B test. The
service journal contained 252
unresolvable stale-handle (`ESTALE`) event warnings from 10:20 through 10:26;
this verifies recurring stale-handle cases on the live event path, but not
whether they account for all unresolved events or the original overflow.

## Selected architecture and implementation state

The selected target is now **reconciliation-first**: scans define the
last-observed index, and fanotify is only a bounded prioritization hint. The
15-minute maximum observation age is a target to measure, not a current
guarantee. The architecture and development specification now describe scan
semantics, bounded changed-row updates, overflow recovery, project-quota limits,
and test gates.

The working-tree daemon now defaults to reconciliation-first; `--event-apply`
selects the legacy event-apply path for controlled comparison or rollback.
`--reconcile-first` remains a hidden compatibility alias. The reconciliation
path drains and validates fanotify records as hints, without resolving every
file handle or applying per-event SQLite updates. It requests scans on overflow or
malformed records and performs periodic scans every 15 minutes. After the
initial atomic v7 baseline, complete observations reconcile the existing
database in one publication transaction. Existing path IDs are reused by
parent/name identity; new IDs are monotonic. Value-guarded SQL upserts skip
unchanged rows. Incomplete walks preserve the previous indexed rows and record
a failed attempt. This source default has not been deployed; the installed
production daemon remains unchanged.

**Current implementation work (2026-11-07):** the reconciliation-first path
must not inherit the legacy mode's permanent retry circuit: repeated scan or
accounting failures would otherwise stop the only mechanism that can refresh
its last-observed index. The candidate now keeps retrying this mode with
persisted, bounded backoff, including after a corrupt retry counter; the
legacy event-apply circuit remains unchanged. The hint parser also excludes
notifications emitted by the Dux process itself so index/WAL/staging writes do
not make its own scan look raced. Reconciliation-first is now the source
default; the `--event-apply` escape hatch and default-mode CLI behavior are
covered by tests. The latest source passed 289 Rust tests (2 ignored), strict
all-target/all-feature Clippy, formatting, and diff checks on 2026-11-07. A
disposable tmpfs end-to-end run verified v6-to-v7 atomic upgrade followed by
an in-place observation that indexed a newly created file. A fresh release
executable also started a new daemon with no mode flag and reported
`scan_first=true`, proving the new source default is wired through runtime
dispatch. That tmpfs run also showed the current hint path treats unrelated
activity on the same marked filesystem as a stale hint; the current candidate
now filters resolved hints by indexed-root scope. Each batch validates
DFID_NAME parent handles and coalesces at most 32 distinct parents. Malformed,
stale, overflowed, or saturated hints widen to a full observation. Hints do
not prioritize or reconcile subtrees; scans remain full-root observations.

**Dedicated fanotify intake (2026-11-07):** reconciliation-first now has a
separate reader thread that continuously drains the nonblocking fanotify fd into
a bounded synchronous channel of 128 chunks (32 KiB each, about 4 MiB maximum
payload). The reader does not wait for queue capacity: a full/disconnected ring
sets an explicit loss flag, and the coordinator requests a full observation.
The channel still has a short internal synchronization cost and allocates one
buffer per read; its throughput and CPU cost need load measurement.
The CPU governor still applies to the coordinator, not the intake thread. The
coordinator parses chunks and submits coalesced parent handles to a separate
resolver worker through bounded queues. Parsing and resolution can still fill
the ring under enough pressure. Shutdown stops and joins both workers and
persists a startup-observation requirement instead of racing another consumer.

On VPS2, the current candidate passed 289 Rust tests (2 ignored), strict
all-target/all-feature Clippy, and the two focused reader tests. An ext4 tmpfs-
separate-database E2E run started in default scan-first mode, observed a file
created under the watched root (`events_seen=2`), and later indexed it
(`index_nodes=27`). The daemon remained dirty and repeated observations while
other filesystem activity was present on VPS2; this is runtime evidence for
reader delivery, not a clean steady-state or overflow qualification. A graceful
stop persisted `shutdown requires a fresh reconciliation observation`. The
reader initially failed because its duplicated fd guard was dropped too early;
the E2E exposed that defect, which is now fixed. No production daemon was
changed. The VPS2 E2E debug binary SHA-256 was
`926da2ff3924cd6de9475390d0e24d54ef9f5183d9bf47b8281bdecb4c11b828`; the
tested `src/watch.rs` SHA-256 was
`d28fa00bb36b75f0c5cfc6d424dd656b7a3b4784ccb91bea94b1eaf2a9deb5d4`.

A real-fanotify tmpfs E2E test created 25 files outside the indexed root on the
same filesystem: status remained clean with zero in-scope events. A file
created inside the root marked it stale; the explicit reconciliation indexed
it and cleared the marker. This validated root filtering and scan repair on the
test kernel/tmpfs only. The release executable SHA-256 was
`dbe52df6eea586ddf085e9a148a9cf7d6f430f8fc34cc9aa2f60707f13f8fc59`; embedded
source SHA-256 was
`88e0ae444bf7e171196d77503a20ee8f703ef0c2bf7d1c40b2bdc7dacdef30bc`.

These checks are not production-scale qualification: directory-level hint
prioritization, forced-overflow replay, and concurrent-churn recovery remain
open. There is no production-shaped endurance,
package rollout, or canary evidence. Do not interpret this change as production
readiness or as a production deployment.

**Bounded hint resolution and scan-race policy (2026-11-07):** handle-to-root
resolution now runs on a separate worker behind bounded input/output queues;
the coordinator does not block on `open_by_handle_at`. At most 2,048 event
records are accepted from one 32 KiB reader chunk, keeping result accounting
bounded. Queue saturation is explicit loss and requests a full follow-up scan.
An unresolvable individual scope instead leaves freshness stale for the normal
cadence; ordinary churn during a scan no longer schedules an immediate scan
loop. Actual kernel overflow and userspace pipeline loss still trigger an
immediate follow-up after the current scan. The full VPS2 suite passed **292
tests, 2 ignored** after this change. Strict all-target/all-feature Clippy,
formatting, diff checks, and the exact debug candidate build passed. This is
still a partial scan-first implementation: event
prioritization remains full-root, and forced-overflow replay and concurrent
churn convergence have not yet been qualified. The 72-hour soak is running a
previous candidate build and is not evidence for this source revision.
An exact-source VPS2 ext4/tmpfs E2E run then created 500 files outside the
indexed root while the startup observation ran: it completed one scan with no
immediate-follow-up loop and no overflow. A second run created a file in the
indexed root after baseline and sent the supported `SIGHUP` rescan request; the
candidate completed the follow-up observation with the new file included in
the scan totals. Both runs had zero forced-followup log entries and zero kernel
overflow reports. These are short functional checks, not burst-capacity or
long-duration qualification.

**Dependency audit (2026-11-07):** online `cargo audit -D warnings --json`
against the current `Cargo.lock` reported zero known vulnerabilities and zero
warnings across 229 locked dependencies. The refreshed RustSec database was at
commit `309392b18f6b17022f82c701f300433126901068` (2026-11-06 15:34 UTC), about
24.5 hours before this check. This is a clean audit against that advisory
snapshot; repeat it at release time because the feed changes continuously.

**Status correctness fix (2026-11-07):** human and JSON status now consult the
live watch mode before interpreting persisted retry counters. A legacy counter
above the retry limit no longer falsely says that reconciliation-first scans
are paused; event-apply mode continues to report its actual retry circuit. The
regression test covers both modes and corrupt/over-limit counters. Full Rust
suite: 289 passed, 2 ignored; strict Clippy, formatting, and diff checks pass.

**Isolated scale sample (2026-11-07):** the existing manual reconciliation
scale test passed with 100,000 files on tmpfs, pinned to one CPU core. Initial
atomic scan took **10.118 s**; a subsequent unchanged in-place reconciliation
took **11.064 s**; the DB remained **30,171,136 bytes**. This measures a
one-core tmpfs fixture with no competing workload and does not establish ext4
or VPS performance, peak staging bytes, overflow recovery, churn convergence,
or 15-minute freshness. The harness initially failed because it excluded its
tmpfs fixture as pseudo; it now explicitly enables pseudo traversal for this
isolated test root. The same harness also passed at **1,000,000 files**:
baseline **79.374 s**, unchanged reconciliation **91.153 s**, DB unchanged at
**306,790,400 bytes**. This 1m fixture temporarily raised total `/dev/shm` use
from 1.2 GiB to 5.7 GiB; after the test's cleanup it returned to 1.2 GiB. These
are single-core tmpfs measurements and are not a production-filesystem
capacity or performance guarantee. Both scale runs used source commit
`c683011d75dccb03b78f8a722a66671d9b8e60a8`, Rust 1.99.1, and the debug test
executable SHA-256
`96960437293282ba261a9105702c1cec9f4d8920e8711cead1c530dd3257ec1a`.

In-place reconciliation still walks every entry and builds a disk-backed scan
staging database with graph and rollup projections. It reduces full database
replacement and unchanged-row SQLite writes; it does not reduce traversal/stat
work, eliminate staging-disk use, or provide per-directory salvage after a
partial scan. The 1.37M run observed about 414 MiB of staging data during the
in-place pass, but did not continuously capture its peak or count changed rows.
The mode is not ready for a production canary.

The v7 schema step is now partly implemented: full scans persist the path graph
that was previously staging-only, while retaining v6 inode/dirent tables for
the current watcher and queries. Event updates do not update the v7 graph, so
it represents the last full scan only. The schema bump means a v6 production
index requires an atomic full rebuild on upgrade. Added-path storage and rebuild
space/time overhead have not been measured; this is not ready to deploy.

The current source also fails closed when a v7 database is missing required
path-graph tables, columns, or correctly defined indexes. Normal opens require that schema;
`needs_rebuild` reports damaged v7 graphs, and the rebuild-source reader can
still read the v6-compatible projection without silently recreating an empty
graph. Regression coverage includes a required index name recreated on the
wrong column.

The same working tree contains the stale-parent classification and retry
backoff changes under review. Those are not yet release-qualified. The concise
architecture plan orders work by production impact; detailed historical test
evidence remains in [`PRODUCTION_READINESS_AUDIT.md`](PRODUCTION_READINESS_AUDIT.md)
and [`docs/QUALIFICATION.md`](docs/QUALIFICATION.md).

During review, found and fixed a migration edge case in the retry change: legacy
indexes with an attempt count but no deadline now receive the 60-second base
cooldown instead of the exponential maximum (up to 8.5 hours). Current-source
Rust tests now cover this behavior.

## Verification and open gates

- `cargo fmt --check` and `git diff --check` pass.
- Human-readable status now labels the kernel queue separately from pending
  userspace updates; this avoids saying “pipeline: drained” when the kernel
  queue is empty but work remains pending. The current-source regression passed.
- Retry status now exposes both the retry interval and remaining seconds, plus
  whether retry is due. Current-source tests cover missing, future, expired,
  malformed, and bounded retry-deadline cases.
- Read-only SQLite inspection of the live index at 10:30 UTC found
  `failed_reconciliation_attempts=2`, no persisted retry deadline, and dirty
  reason `fanotify queue overflow`. The installed binary logged at startup that
  automatic reconciliation was paused after two failures. The bounded-retry
  changes are now tested on an isolated checkout but are not deployed.
- Latest local Rust run: **283 passed, 0 failed,
  2 ignored**. The ignored tests require a dedicated ext4 profile and manual
  scale invocation. The new 100k-entry scale smoke passed separately. Strict
  all-features/all-targets Clippy passed with `-D warnings`; formatting and diff
  checks pass. Focused reconciliation tests passed, including zero indexed-row
  writes on an unchanged scan and equality against a fresh full-scan oracle for
  inode, dirent, path, and directory-total projections; an injected SQLite
  publication failure also rolled back all four projections and a later retry
  converged. A current optimized-daemon fixture check exercised the in-place
  runtime path end to end: starting with 1,000 files, a file created after
  daemon startup was indexed by a SIGHUP-requested observation, the manual
  `dux scan` returned successfully, and the SQLite database inode stayed
  unchanged. The daemon reported that concurrent changes leave freshness
  uncertain. This small, quiet fixture verifies control flow and row
  publication only; it does not test overflow, sustained churn, scale, or
  recovery while the tree mutates continuously.
  Python discovery passed **306/306**.
  `cargo audit --no-fetch` exited 0 after loading the local 1,422-advisory DB
  and scanning 229 locked dependencies; this was a source-lockfile scan, not an
  OS-package or application security audit. VPS2 access works on explicit SSH
  port 22 with the authorized root key; the host SSH config's port 2222 caused
  the initial failed attempt.
- The four changed Python suites passed **65/65** (backup, canary, schema
  preflight, and live observer). Formatting and diff checks pass. Exact commands,
  cgroup resource use, production sample, and soak state are recorded in
  [`PRODUCTION_READINESS_AUDIT.md`](PRODUCTION_READINESS_AUDIT.md).
- The earlier broader Python run had **301/304 passed** because three package
  build tests timed out under a 5% CPU quota. The latest unrestricted local
  discovery passed **306/306**; that removes the observed test-timeout blocker
  but does not qualify package installation on all supported target systems.
- RustSec `cargo audit` passed for the 229-dependency source lockfile and for
  the exact running production binary (202 embedded Rust dependencies; binary
  SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`):
  zero known advisories in the database snapshot dated 2026-11-06 15:34 UTC.
  This does not cover OS packages or application logic, and advisories newer
  than that database snapshot are outside the check.
- Release-profile 1.37M-entry scan-first measurement on VPS2 used the exact
  optimized test-harness binary SHA-256
  `5d222e9b06f4748c3aae10c5db4fa017e8d57d73b2576071da8ae53061086a09`. On a
  1.37M-file, 5.66-GB-allocated static ext4 fixture, baseline scan took
  **154.331 s** and the following unchanged in-place scan took **128.755 s**;
  both databases measured **440,315,904 bytes**. This is a Rust test executable
  invoking the in-place function, not the production daemon. The second scan was cache-warm,
  so this is not a controlled A/B performance win. A debug-profile repeat took
  **275.050 s** and **297.325 s**, respectively, with a 440,307,712-byte index.
  Both runs passed. The release run took 5 min 36 sec total, used 4 min 45 sec
  CPU, peaked at the test's 1-GiB memory limit (98.5 MiB swap), and recorded no
  cgroup OOM kills. The unit had a one-core CPU quota, low I/O weight, and low
  priority while the existing soak ran. The temporary fixture was removed and
  free space returned to **20 GiB**. This supports feasibility for a static
  1.37M-entry observation on VPS2; it does not establish mutation correctness
  under concurrent churn, peak disk use across hosts, or a production SLO.
- Current-source 1.37M-entry scale test on vps1 used the debug Rust test
  executable SHA-256
  `d55969f98018381752b0fede6306ed04704db29a3de0f6bb6baa0956cef06655` at
  commit `7999b2c`. On a 1.37M-file static tree, the initial atomic scan took
  **154.204 s** and the following unchanged in-place scan took **146.364 s**;
  both database measurements were **441,020,416 bytes**. A sampled peak of the
  private stage file was **422,600,704 bytes**; this was not continuously
  monitored, and peak RSS was not captured. The fixture test passed and its
  RAII cleanup removed the tree and DB. This is static-tree scale evidence,
  not a controlled performance comparison or safe production-load test: the
  running legacy watcher saw the generated churn, logged stale-handle events,
  and its overflow count increased during the run. Current post-run disk
  availability returned to 62 GiB.
- An initial full Rust-suite attempt placed Cargo output and fixtures in
  `/dev/shm`; it ran **282 passed, 1 failed, 2 ignored** because the
  reconciliation test fixture was pruned as a pseudo filesystem. Updated that
  test alone to pass `include_pseudo: true` for its explicit tmpfs root, then
  reran the full suite with Cargo output and fixtures in tmpfs at low CPU
  priority: **283 passed, 0 failed, 2 ignored**. Strict all-target/all-feature
  Clippy with `-D warnings`, formatting, and diff checks passed. This keeps
  build/test writes off the watched ext4 but does not test ext4-specific
  fanotify or handle behavior. The tmpfs build and fixtures were removed after
  the run; the earlier ext4-backed suite also passed 283 tests.
- Found and fixed a freshness-cadence bug: after a scan-first observation raced
  with normal events, the scheduler reset its 15-minute timer at scan
  completion. This made the interval between observations equal to the
  configured interval plus scan duration. The timer now stays anchored to scan
  start; a regression test verifies the next cycle is due at 15 minutes from
  start. Current-source full Rust tests pass **284 passed, 0 failed, 2
  ignored**; strict all-target/all-feature Clippy, formatting, and diff checks
  pass. These checks used tmpfs fixtures/build output; ext4-specific and
  live-daemon cadence behavior still need an isolated ext4 qualification host.
- Read-only inspection showed the live production DB is still schema v6, while
  `reconcile_in_place` requires v7. Fixed scan-first dispatch to use an atomic
  baseline/rebuild until `baseline_ready` is true, then use changed-row in-place
  observations for full-root scans. One-filesystem mode remains atomic. A
  regression test covers the dispatch matrix, and the existing v6-to-v7
  migration test passes. Current full Rust run: **285 passed, 0 failed, 2
  ignored**; strict Clippy and format checks pass. This does not qualify an
  actual production upgrade or rollback.
- The independent VPS2 soak remains active at the latest archived sample
  (**13:11 UTC**): 40,564 s elapsed, 862,300 operations, 11 mutation bursts, no
  controlled crash rounds, and one 300-second held-reader phase with WAL
  reclaimed. It uses candidate SHA-256
  `952328eedd6ee73c344abddf0bb411b537786f030f3b0e5c282e8f83b2e1c287`, not the
  scan-first test binary. Final exactness/history checks are pending; 14
  qualification suites remain unrun. This is not a completed soak or a
  qualification of the new architecture.
- A read-only SSH check at **13:39 UTC** confirmed that exact soak worker and
  daemon are still running on VPS2 (11 h 45 min elapsed), with the recorded
  binary hash unchanged. VPS2 had **20 GiB disk available** and **2.6 GiB
  memory available**. It remains reserved for the active run and was not used
  for additional testing.
- No controlled queue-ioctl calibration, representative event replay, overflow
  comparison, 24-hour canary, or live rollout has been completed. The VPS2
  72-hour soak remains active; its report still lists 14 other qualification
  suites as not run.
- Implemented in working tree: complete scan-first observations merge changed
  rows in place and incomplete observations preserve prior rows. Next: compare
  all indexed projections to an independent filesystem oracle; add generation/
  freshness per directory; force overflow and churn during scans; benchmark
  scan, write, WAL, CPU, memory, and disk overhead on production-sized fixtures
  before considering a source-matched canary.
  Do not infer deployability from passing unit tests.
- Broader release gates remain open: full current-source test/security/package
  matrix, exact recovery of the live index, schema-v7 path identity, scale and
  sustained-load endurance, supported-kernel/filesystem coverage, and a
  source-matched production canary with tested rollback.

No claim of 100% production readiness is made until those gates have direct,
current evidence and all remaining limits are documented.

## Incomplete-scan work reduction and VPS2 follow-up — 2026-11-08 12:46 UTC

Current source now skips constructing the v6 compatibility projection and
depth-by-depth compatibility rollups when an incremental observation has
already failed its coverage checks. Those results cannot be published; the
failure path only records `last_scan_attempt_ts` and returns the last
published root total. A focused regression test verifies that no discarded
projection tables are built and that the prior total is retained. This limits
avoidable SQLite work on failed observations; it does not repair inode reuse
or make the live index current. The change is local and has not been deployed
to VPS2.

Live VPS2 follow-up: the scheduled scan starting at 12:42:29 UTC completed at
12:45:25 with zero coverage issues across 1,305,598 files and 156,800
directories. Dux stayed active with zero restarts. At 12:45:59, status was
already dirty again because the bounded fanotify hint ring had overflowed;
the same loss reason and repeated overflow warnings were present through
12:46:24. The reported last-scan age was 236 seconds at 12:46:25, while
`caught_up=false` and `dirty=true`. Cumulative service counters at scan
completion were about 1,469 CPU seconds, 13.7 GB read, and 20.7 GB written.
This is evidence that a complete scan can finish under this workload, but
the hint stream currently cannot keep the view fresh between scans. It does
not demonstrate stable convergence or an acceptable I/O budget.

The local change passed its focused incomplete-projection test and ext4
inode-reuse regression; the complete suite passed (303 passed, 0 failed, 2
ignored), strict all-target/all-feature Clippy, formatting, and the exact-source
release build passed. Further release gates remain: reduce or redesign hint handling,
prove bounded freshness under sustained churn, measure the optimization on a
production-shaped workload, and perform a source-matched VPS2 canary with
rollback. No production-readiness claim is made.

## Scan-first hint-path simplification — 2026-11-08

The scan-first watcher no longer needs the raw event ring and file-handle
resolver: they were processing every event only to prioritize a walk that still
visits the entire root. The dedicated reader now validates/counts event
metadata and kernel-overflow notifications, then discards each chunk. It uses
a bounded carry buffer for records split across reads and never blocks on a
userspace queue. Event activity still marks the indexed view stale, while
kernel queue loss remains visible in the overflow counter; the next full walk
is the reconciliation mechanism. This is specific to scan-first mode; the
event-applied mode retains its existing path-resolution behavior.

The reader test passed a 10,000-event synthetic burst without queueing payloads
or recording drops. The metadata parser test covers malformed/truncated input
and self-generated events. These are local pipe-based tests, not a fanotify
kernel throughput benchmark. The full suite passed (305 passed, 0 failed, 2
ignored); strict Clippy, formatting, and release build passed. The following
source-matched VPS2 canary tested the userspace ring behavior; it did not
resolve the independent live-scan inode-reuse conflicts. Bounded freshness
under sustained churn and production-scale I/O budget remain open.

## Source-matched scan-first reader canary — VPS2, 2026-11-08 13:03–13:08 UTC

The pushed candidate `e96e708` was installed manually after verifying a
rollback copy of the prior binary. The first full-root walk completed in 188
seconds across 1,307,547 files and 157,131 directories. During and after that
scan, Dux reported zero kernel overflow notifications and zero userspace
updates dropped; the journal contained zero scan-first hint-ring or resolver
overflow warnings after the candidate started. It had counted 2.07M events by
13:08:30. This supports that removing per-event handle resolution and the
payload queue addresses the specific ring-saturation failure on this VPS2
workload.

The scan itself found 36 conflicting inode incarnations, so Dux retained the
previous index and remained dirty. The service stayed active with zero
restarts; its retry backoff was 1,920 seconds. Therefore the canary does not
prove scan convergence, freshness, or production readiness. The scan used
about 192 CPU seconds, read 1.95 GB, and wrote 2.01 GB; without a controlled
comparison those counters are a single-run cost, not an improvement claim.
Evidence and rollback location are recorded in
[`runtime-canary-e96e708-1303.log`](docs/qualification/scan-first-fixed-cadence-20261108/runtime-canary-e96e708-1303.log).

## Fixed-cadence retry gate and refreshed disk sample — 2026-11-08

Code review of that canary found that `scan_first_observation_due` requested a
15-minute scan, but `reconciliation_scan_ready` still required the exponential
retry deadline. After the canary's 36-conflict failure the deadline was 1,920
seconds, so the next 900-second observation could be blocked. The scheduler
now permits a periodic scan-first observation to satisfy the retry gate when
that cadence is due; an operator-requested retry before cadence still respects
backoff. It also advances the persisted retry deadline at the cadence boundary
so `dux status` reports the same due time as the scheduler. The regression
asserts periodic-due, backoff-not-due admission and confirms event-applied mode
does not inherit this exception. Current-source full suite passed (305 passed,
0 failed, 2 ignored), strict Clippy, formatting, and locked release build passed.
This scheduler change has not been installed on VPS2; its next-cadence behavior
is verified by the deterministic helper test, not by runtime observation.

Read-only disk sample at 13:09:56 UTC: local host root had 39 GiB available
(185 GiB total, 138 GiB used, 79%); `/home/dux/target` used 2.0 GiB. VPS2 root
had 12 GiB available (93 GiB total, 77 GiB used, 87%); `/var/lib/dux` used
538 MiB, and its LVM volume group had 4 GiB free. The scan stage had been
cleaned by the time of this sample. No cleanup was performed. The VM has room
for the canary, but its 4 GiB LVM free space is not evidence that a COW
snapshot will survive ongoing root-filesystem churn.

## Current-source package and live-root qualification — 2026-11-08 10:41 UTC

Commit `43a7769` now has a successful exact-source static-musl release build:
the auditable binary and DEB/RPM payloads were produced, with zero known
RustSec advisories for 229 locked dependencies and embedded binary inventory
for 202 dependencies. VPS2 lacked DNS, so its local audit shim skipped the
network-dependent yanked-crate check; this is not a complete online release
audit. The static-musl build also caught and drove a fix for the ioctl request
type mismatch (`libc::Ioctl`).

The next real Debian lifecycle run **failed** before upgrade/rollback/purge:
the package service remained `reloading` after the harness's 300-second
baseline readiness limit. The legacy index on VPS2 contained 233 REAL-valued
`recursive_bytes` totals among 1,974,271 rows, including values beyond i64;
`dux status --json` errored on that index. A candidate full observation
replaced it and restored status output (1,431,666 indexed nodes), but the index
became dirty from fanotify hint-ring/resolver-backlog saturation during the
root walk and required another observation. At the timeout, the second scan
had visited 815,390 files and 116,331 directories. The daemon had seen 842,240
events with zero kernel-overflow notifications and zero reported userspace
update drops; nevertheless the hint ring overflowed and the index was not
caught up. The daemon also logged failure to persist a dirty marker while its
old index was read-only during reconciliation.

This live-root result overrides any broad inference from bounded fixture
tests, but the 300-second harness cutoff was too short to prove failure to
converge: the unit itself allows up to one hour for startup, and the first
replacement index appeared shortly after the harness exited. At that point
the index was dirty and another full scan was running under VPS2's actual
event stream. A longer runtime observation is underway. The lifecycle attempt
used package revisions 98 and 99 with the same candidate
binary, so no real old-code-to-new-code upgrade was tested. The test service
was stopped and disabled after capturing evidence; its test package and index
remain on the marked VM. Full logs, report, and manifest are in
[`docs/qualification/package-lifecycle-20261108/`](docs/qualification/package-lifecycle-20261108/).

After stopping the daemon, read-only checks of the replacement database found
all 1,431,666 `recursive_bytes` values stored as integers and
`PRAGMA integrity_check=ok`. This confirms that the full observation repaired
the malformed stored totals structurally; it does not establish freshness or
that the daemon reached caught-up state.

An extended VPS2 observation (10:45:30–10:56:01 UTC) then confirmed the
retry-amplification path. The first post-shutdown observation finished in
about 4.5 minutes with no ring/kernel overflow; accounting verification stayed
pending and a new scan began after 301 seconds of backlog. That scan hit the
hint-ring overflow twice at 10:53:20 and immediately launched a third
observation. At 10:56:01 the third scan was still indexing, with 52,965 events
seen and zero kernel-overflow notices or reported userspace update drops.
This was not eventual-convergence evidence; the test was stopped to avoid
continued repeated scans.

Working tree now changes scan-first handling so loss during a completed full
observation defers the next check to the fixed scan cadence instead of
immediately repeating the full walk. It also aligns package lifecycle
readiness with the systemd one-hour startup limit and waits for a completed
last-observed scan instead of `caught_up`, which scan-first mode intentionally
never asserts. The updated source passed **300 Rust tests (0 failed, 2
ignored)**, strict all-target/all-feature Clippy with `-D warnings`, formatting,
and **70 package-lifecycle Python tests** on VPS2. The updated source has not
yet been release-built or runtime-qualified; do not rely on the policy change
until the exact package is installed and observed on VPS2.

Immediate open work: explain and reduce live ring/resolver saturation, prove
convergence or define a bounded non-converged state under sustained events,
test repair of legacy numeric overflow/corruption, and complete an actual
source-distinct package upgrade/rollback before any production canary.

## Disk and active temporary data

Read-only sample at 13:12 UTC: root was **185 GiB total / 116 GiB used / 62 GiB
available**. Dux `target` used **9.7 GiB**. `/tmp` used **11 GiB**, largely Go
build caches and XTOP RCA replay/cache artifacts; no Go or XTOP process was
visible in the process sample, but those directories were not deleted because
their contents may be retained test evidence. The failed local scale-test
fixtures and the VPS2 benchmark fixture were removed after confirming no test
process was using them. No broader caches or readiness artifacts were deleted.
Disk usage is significant but not at an immediate capacity limit.
The broader read-only inventory measured `/var` at **38 GiB**, `/home` at
**29 GiB**, and `/root` at **29 GiB**; `/var/lib` accounts for **30 GiB**,
`/var/backups` **3.3 GiB**, and `/var/log` **2.1 GiB**. These totals are not
proof that the contents are disposable. No broader inventory files were removed. The detailed
inventory and cleanup rationale are in the audit.

## Current scan-first runtime candidate — 2026-11-08 13:17 UTC

Commit `739113c` (`Keep scan-first retries on fixed cadence`) is pushed on
`hardening/stale-dfid-classification-review` and is installed on VPS2 as
`/usr/bin/dux`. Its binary SHA-256 is
`fa7aca762c7aa7e90965c9df449dcce229bb5f7c4cadd8aefd183284c8a3f0c6`; the
embedded source revision is `739113c5c1ac`. Local reruns against this source
passed 305 tests (0 failed, 2 ignored), `cargo fmt --check`, and strict
all-target/all-feature Clippy with `-D warnings`.

VPS2 service was active with zero restarts at 13:17:49 UTC. Dux reported
`scan_first=true`, 0 kernel overflow notifications, 0 userspace update drops,
and 935,979 events seen. It remained dirty with 1,458,363 indexed nodes and
the accepted scan timestamp unchanged. There had been no new scan since the
13:15:38 service restart. The scheduler initializes `last_reconcile` to five
minutes before startup, so this instance's first periodic observation is due
around 13:25:38 UTC, subject to pressure admission and queue-drain readiness.
The earlier 188-second scan on e96e708 rejected publication after finding 36
conflicting inode incarnations; the new scheduler has not yet been observed
through a due scan. This runtime is therefore an in-progress canary, not proof
of convergence or production readiness. Rollback binaries remain in
`/root/dux-canary-e96e708/` on VPS2.

A bounded VPS2 churn probe at 13:19:40 UTC created and removed 20,000 files
under `/tmp` in 1.202 seconds while the candidate remained active. Across the
probe plus background host activity, Dux's `events_seen` rose from 1,965,567
to 2,026,258; `kernel_overflow_notifications` and
`userspace_updates_dropped` remained 0, and systemd reported `NRestarts=0`.
The index correctly remained dirty pending a full observation. This supports
burst draining under this specific 20k-file workload; it does not isolate the
probe's exact event count, test queue-capacity saturation, or prove scan
publication or accounting correctness.

A second bounded probe at 13:21:51 UTC used 100,000 create/delete pairs under
`/tmp` and completed in 6.979 seconds. The event counter rose from 2,915,692 to
3,456,877 (including background activity); kernel overflow notifications and
userspace drops remained zero, and the service stayed active with
`NRestarts=0`. The index remained dirty and no scan was active during this
probe. Like the smaller probe, this supports reader throughput only at the
observed workload and is not a queue-capacity or reconciliation correctness
proof.

## Fixed-cadence VPS2 reconciliation result — 2026-11-08 13:28 UTC

The `739113c` canary passed its first scheduled scan-first observation after
startup. Despite the persisted retry backoff previously showing 1,920 seconds,
the scheduler started a full-root in-place observation at 13:25:38.865 UTC.
It completed at 13:27:59.660 UTC (140.795 seconds): 1,310,823 files, 157,518
directories, 1,468,341 staged path instances, and **zero coverage issues**.
The scan did not report conflicting inode incarnations and advanced the
accepted scan timestamp. Reconciliation accounting verification then passed
at 13:28:39 UTC after 39.541 seconds.

At 13:28:55 UTC VPS2 reported `ActiveState=active`, `NRestarts=0`, zero kernel
overflow notifications, zero userspace drops, and no pending updates. The
service had seen 7,193,804 events since its counters began. Dux remained dirty
because additional filesystem changes arrived after the observation
(`filesystem changes await the next full observation`); this is expected for
the advisory-event/periodic-scan model and means the view is not continuously
current. This is direct evidence that the fixed-cadence retry gate now permits
a successful complete scan and accounting verification on this VM under its
observed load. It does not establish 24-hour stability, zero-lag freshness,
correctness under every inode-reuse race, or behavior under a kernel queue
overflow. Continue the canary through subsequent cadence(s) and test explicit
overflow/recovery before treating the new architecture as production-ready.

A concurrent burst at 13:30:02 UTC used eight producers, each creating 10,000
files before a barrier and then deleting them (80,000 pairs total). It completed
in 7.945 seconds. The Dux event counter rose from 7,946,328 to 8,721,074
including other host activity. After a 15-second drain period, the service was
active with zero restarts, zero reported kernel overflows, and zero userspace
drops. This raises confidence in burst draining with concurrent producers but
still did not force an actual kernel overflow; overflow/recovery remains an
explicit qualification gap.

## Repeat root observation and forced-overflow qualification — 2026-11-08 13:54 UTC

The same installed `739113c` candidate completed a second scheduled root
observation. It started at 13:40:39.005 UTC and completed at 13:43:40.402 UTC
(181.397 seconds): 1,313,256 files, 157,802 directories, 1,471,058 staged path
instances, and zero coverage issues. Independent reconciliation accounting
verification passed 48.736 seconds later. At 13:47:20 UTC the service was
active with zero restarts, zero reported kernel overflows, and zero userspace
drops. The index was again dirty because additional filesystem changes arrived
after the observation; no clean/fresh claim is made between scans.

The exact installed binary then passed the isolated forced-kernel-overflow
suite on a disposable ext4 loop filesystem: binary SHA-256
`fa7aca762c7aa7e90965c9df449dcce229bb5f7c4cadd8aefd183284c8a3f0c6`, harness
SHA-256 `65c6ed92de65625cc14b4b66e2a5cf0c85d09d3769aa0aaa7ba1c9b66c95ec10`.
The test paused the isolated daemon, generated 18,432 file creates against a
16,384-event queue, observed the kernel overflow counter advance from 0 to 1,
then explicitly requested a full observation so the bounded test did not wait
for the 15-minute cadence. The scan completed, accounting verification passed,
and an independent path/inode/allocation/rollup oracle matched the filesystem.
Recovery took 11.98 seconds. Status remained conservatively dirty because
post-observation hints arrived; the test records this as
`clean_status_after_recovery=false`, while the exact tree comparison passed.
The test then requested an observation after removing the storm and completed
cleanup. This proves manual full-observation recovery after real kernel loss on
this ext4 fixture; fixed-cadence observations of the dirty live root were
separately observed at 13:25 and 13:40. It does not cover other kernels/filesystems,
repeated overflow storms, or long-duration production freshness.

The first forced-overflow harness attempt timed out before injecting an
overflow because its precondition required the event-apply meaning of a clean
pipeline. Scan-first correctly reports a stale last-observed view when hints
arrive. The harness now establishes a complete accounted baseline, verifies
the filesystem independently, forces the overflow, explicitly requests the
bounded recovery scan, and reports index exactness separately from clean
status. The final Python readiness-harness suite passed **39 tests**. The
initial attempt report and final successful machine-readable report are
[`forced-overflow-attempt-739113c.json`](docs/qualification/scan-first-fixed-cadence-20261108/forced-overflow-attempt-739113c.json)
and
[`forced-overflow-739113c.json`](docs/qualification/scan-first-fixed-cadence-20261108/forced-overflow-739113c.json).

The third live-root observation began at 13:55:39.006 UTC, exactly 15 minutes
after the previous one began. It completed at 13:58:55.097 UTC (196.091
seconds): 1,315,893 files, 158,096 directories, 1,473,989 staged path
instances, and zero coverage issues. Independent accounting verification
passed at 13:59:41.111 UTC (46.003 seconds). At 14:00 UTC VPS2 remained active
with zero restarts, kernel overflow notifications, or userspace drops; the
index was dirty only because further filesystem changes arrived after the
observation. Across the three observations, each due scan started on schedule,
finished without coverage errors or inode-conflict rejection, and passed the
separate accounting verifier. This materially improves confidence in cadence
and convergence for this VPS2 workload, but the index is still only
last-observed and never claims real-time caught-up status.

Post-test disk sample: VPS2 root remained 93 GiB total / 77 GiB used / 12 GiB
available (87%); `/var/lib/dux` was 560 MiB. The disposable `/dev/shm`
qualification workspace had returned to 647 MiB used with 2.4 GiB available,
and no `dux-readiness-*` units remained. No production index or broad host
cache cleanup was performed.

## Package 103 qualification addendum — 2026-11-08

Built package revision 103 from clean commit `e90f3c3` using the official
static-musl release path. The reproducibility manifest records a clean
worktree and source-tree SHA-256
`bf91e97343b3f95b83aa32aad6eeea00016e45abc90391724d25ac96b32c5b52`.
Static executable SHA-256 is
`8c107f0ce62565ed5747e8f28c801b236c870cf5c52e652d7ef73d3bb259fe42`.
The lockfile audit found no known vulnerabilities across 229 dependencies;
the advisory database snapshot was updated 2026-11-06. The binary audit
recognized embedded metadata for 202 dependencies.

On VPS2 (KVM, Ubuntu 24.04, Linux 6.8, ext4), the exact static binary passed a
forced real kernel overflow test: a 16,384-event queue overflowed once after
18,432 creates; the recovery observation completed; accounting verification
passed; and an independent path/inode/allocation/rollup oracle matched. The
recovery took 7.494 seconds. The view remained marked stale due to later hints,
which is expected in scan-first mode. See
`docs/qualification/scan-first-fixed-cadence-20261108/forced-overflow-e90f3c3.json`.

The DEB lifecycle test passed upgrade 102→103, rollback 103→102 with a
verified database restore, re-upgrade, purge, and clean install (831.819 s).
Clean install completed a root observation and met the package/service sandbox
checks, but daemon status at capture was still dirty with
`reconciliation accounting verification pending`; `caught_up` was false by
design for a last-observed scan. A separate read-only verifier then checked
the preserved clean-install database: SQLite integrity was `ok`, root
accounting was 82,852,466,688 allocated bytes over 1,474,954 inodes, and
orphan entries, bad prime attribution, and directory rollup errors were all
zero. No independent full-root path listing was compared for that clean
install. Full evidence is in
`docs/qualification/scan-first-fixed-cadence-20261108/package103-lifecycle.json`
and `clean-install-103-independent-db-check.json`.

The lifecycle harness intentionally stops and disables the service. After
saving its report, I restored the pre-test index backup, retained package 103,
and re-enabled/restarted the VPS2 service. At the latest check the service
had zero restarts and zero kernel/userspace queue drops, but it was still in a
post-start full observation; its completed post-restore verification is not
claimed here.

Follow-up at 14:27 UTC: the restored package 103 service completed that
post-start root observation from 14:23:30 to 14:26:12 (162 seconds), covering
1,322,840 files and 158,662 directories with zero coverage issues. Its
independent accounting verification passed at 14:26:52 (40.6 seconds after
the scan). The service was `active/running`, with `NRestarts=0`, zero kernel
overflows, and zero userspace update drops. At the status sample, it was
conservatively dirty because new filesystem hints arrived after the observed
scan; `caught_up=false` is expected for this last-observed model. VPS2 had
9.9 GiB available on `/` after the lifecycle run and state restoration.

Remaining qualification gaps: 24-hour VPS2 candidate canary completion (now in
progress), a separate production-host canary (not started), 72-hour soak,
repeated sustained overflow storms, continuous-churn freshness and scan-duration SLO,
large-scale performance budgets, clean-install exact full-tree comparison,
other filesystems/kernels, RPM package lifecycle, and power-loss/media
corruption tests. These results improve evidence but do not establish universal
or 100% production readiness.

## VPS2 scan-first candidate canary started — 2026-11-08 14:52 UTC

The existing canary gate required `caught_up=true` and `dirty=false`, which is
incompatible with scan-first's explicit last-observed semantics. Updated
`scripts/dux-canary-monitor.py` to evaluate that mode against a bounded
full-observation age (default 20 minutes), live/versioned watch state, schema
v7 structural/accounting checks, service/package/unit identity, resource
limits, restart/OOM counters, and journal errors. Scan-first dirty status and
fanotify queue-overflow records are allowed only under that freshness bound;
accounting/coverage/write errors and truncated/unclassified journal warnings
still fail the monitor. Legacy event-apply mode keeps its strict clean/caught-up
requirement.

The monitor has **27 focused tests**; the complete Python script suite passed
**323 tests**. Four short preflight attempts were not counted toward the soak.
The first passed its sample but was stopped after review found the journal
matcher missed nonzero scan-coverage counts. The second failed while systemd
reported its normal scan-time `reloading` state as inactive. The third passed,
but prompted a fail-closed fix for unclassified critical-event counts. The
fourth sample passed but review identified a brief post-scan window where
accounting verification is pending after `scan_in_progress` clears while
systemd still reports `reloading`; that exact state is now allowed only when
the status reason names the pending verifier. All findings have regression
tests. The final monitor, with coverage-count parsing, scan-aware systemd
readiness, and fail-closed journal accounting, started as
`dux-scanfirst-canary-20261108-v5.service` at 14:52:55 UTC. It samples hourly,
allows no more than 1,200 seconds since the last full scan, and is capped at
20% CPU and 1 GiB memory. Sample 1 passed at 14:53:49 UTC; last-scan age was
865/919 seconds in the snapshot, the v7 accounting/path graph passed, and the
journal had no critical events. Candidate binary SHA-256 is
`8c107f0ce62565ed5747e8f28c801b236c870cf5c52e652d7ef73d3bb259fe42`; systemd
unit SHA-256 is
`e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.
Monitor SHA-256 is
`95b68de248f1168b2e1ca5f57a18b12ee139ecf37bd3ba43bab3c25e284bb8bc`; observer
SHA-256 is `fdc693097afbd3d78861ded24a8515b2c52f8b5529c9e8d6a033f7f7997b0509`.
The live append-only JSONL is on VPS2 at
`/root/dux-canary-e96e708/scanfirst-canary-103-v5.jsonl`.

This is an isolated VPS2 candidate canary, not the production-host canary. It
remains **in progress** and has not passed its 24-hour gate. Its
threshold checks recency of the latest full scan, not real-time freshness, and
does not claim that every event hint was delivered. The one-hour sample
interval will report service restarts and OOM counters at sample time; it does
not provide sub-hour failure detection. All four preflight reports are
preserved in the qualification directory and on VPS2; none counts toward the
24-hour duration.

## Canary failure and retry convergence result — 2026-11-08 (updated 17:48 UTC)

The v5 VPS2 candidate canary **failed** on sample 2 at 15:55:14 UTC, after
sample 1 passed. Its fail-closed reasons were both status reads exceeding the
1,200-second last-observation limit and critical accounting/overflow/scan
journal evidence. It is not a completed or passing 24-hour canary.

Live Dux evidence explains the failure. The 14:38:30 observation completed at
14:42:22 with zero coverage issues and accounting verification passed at
14:43:00. Subsequent scheduled/retry observations did not qualify:

| Completion (UTC) | Coverage issues | Reported failure category | Next delay |
| --- | ---: | --- | ---: |
| 14:58:55 | 5 | conflicting inode incarnations | 59 s |
| 15:04:24 | 27 | conflicting inode incarnations | 119 s |
| 15:10:38 | 55 | conflicting inode incarnations | 239 s |
| 15:18:47 | 109 | conflicting inode incarnations | 479 s |
| 15:30:46 | 27 | conflicting inode incarnations | 959 s |
| 15:45:45 | 15 | conflicting inode incarnations | 1,919 s |
| 16:01:03 | 72 | conflicting inode incarnations | 3,839 s |
| 16:17:41 | 0 | scan accepted; accounting verifier passed | cadence reset |
| 16:31:38 | 32 | conflicting inode incarnations | 59 s |
| 16:37:20 | 23 | 13 conflicting incarnations and 10 conflicting edges | 119 s |
| 16:43:35 | 1 | conflicting inode incarnations | 239 s |
| 16:51:46 | 1 | conflicting inode incarnations | 479 s |
| 17:04:01 | 18 | conflicting inode incarnations | 959 s |
| 17:19:23 | 64 | conflicting inode incarnations | 1,919 s |
| 17:32:02 | 13 | conflicting inode incarnations | 3,839 s |
| 17:46:56 | 11 | conflicting inode incarnations | 7,679 s |

The retained VPS2 journal identifies the dominant failure: emitted scan error
records in this window were `conflicting_inode_incarnations` (up to 109 inode
numbers in one pass), with one separate scan also reporting 10 conflicting
inode-parent/name edges. This is a concurrent inode-reuse race during a
non-snapshot filesystem walk. One scan at 16:17 converged with zero issues, but
subsequent scans reproduced the race. Failed attempts reported
`lost_events=true`; the kernel overflow counter was 1 and userspace update
drops remained 0. Host recovery admission also deferred some scans at elevated
pressure (six vCPUs, load around 2.6–5.6 in later samples, CPU PSI around 19%).
At 17:48 UTC the daemon was active with zero restarts, no cgroup OOM events,
and about 8.6 GiB free on `/`. Its last accepted observation was 5,968 seconds
old; failed attempts had reached 8, retry backoff was 7,680 seconds, and the
next retry was about 7,421 seconds away. The view
correctly remained dirty. This is a freshness/convergence failure, not a
daemon crash.

The sample-2 report and journal excerpt are preserved in
`docs/qualification/scan-first-fixed-cadence-20261108/canary-v5-failed.jsonl`
and `live-retry-failure-20261108.log`; later retries are in
`live-retry-failure-late-20261108.log`. This result contradicts a claim that the
scan-first design has already demonstrated reliable recovery under sustained
VPS2 churn. The retry/coverage path needs diagnosis before another canary is
counted.

## Test inventory and coverage summary — current evidence

Automated tests on the current Rust source passed **317 default-feature tests**
and **320 all-feature tests**, with 0 failures and 2 ignored in each run. The
ignored cases require the dedicated ext4 or manual scale profile. The complete
Python script suite passed **325 tests**, including **29 focused canary-monitor
tests**. Strict all-target/all-feature Clippy, formatting, and `git diff
--check` passed. The
lockfile audit found no known vulnerabilities in 229 dependencies; the static
binary contains auditable metadata for 202 dependencies. Audit limitations and
the advisory database date are documented above. These are automated code and
artifact checks, not proofs of universal runtime correctness.
The Python command was rerun on this worktree; its result is preserved in
`docs/qualification/scan-first-fixed-cadence-20261108/python-suite-final-20261108.log`.

The latest current-source `cargo llvm-cov --locked --all-features --summary-only`
run measured **61.58% line, 63.74% region, and 65.20% function coverage**.
Per-file line coverage: scan 48.61%, store 94.35%, query 85.39%, deleted-open
handling 89.62%, JSON emitters 83.49%, watch 69.49%, main/CLI 42.45%, and TUI
37.88%. This run's branch column is uninstrumented/unusable (all listed
branches appear missed); do not interpret it as measured 0% branch coverage.
Coverage measures execution, not whether each behavior was asserted.

Runtime and integration coverage completed so far:

- Four accepted full-root observations of roughly 1.31–1.32 million files on
  VPS2/ext4 completed with zero coverage issues and passed independent
  accounting verification. Later high-churn observations produced 1–109
  coverage issues per failed scan, with one later zero-issue pass before the
  failures recurred and retry backoff escalated; see the table above.
- The static package passed one forced real kernel-overflow integration test:
  one overflow on a 16,384-event queue after 18,432 creates, exact independent
  path/inode/allocation/rollup match, accounting verification, and recovery in
  7.494 seconds. The dirty status after later hints was correctly retained.
- DEB package lifecycle passed upgrade 102→103, rollback with verified index
  restore, re-upgrade, purge, and clean install. Clean install completed a
  scan; its in-daemon accounting verifier was still pending at report capture,
  while a separate structural SQLite/accounting verifier passed. No independent
  full-tree path comparison was made for that clean-install database.
- Bursts of 20,000 and 100,000 create/delete pairs and eight concurrent
  producers performing 80,000 pairs completed with zero observed kernel
  overflows, userspace drops, or restarts in those bounded runs. A 10,000-event
  synthetic pipe-reader test passed; it is not a kernel fanotify throughput
  benchmark.
- Earlier self-event-filter qualification (different binary/source revision)
  passed two 100k/20k churn runs, a 1M fixture, a 1.37M fixture, and one isolated
  forced-overflow fixture. These establish bounded results for that revision,
  not the current package under the live retry failure described above.

All kernel/runtime observations are on one Ubuntu 24.04, Linux 6.8, KVM VPS2
with ext4. Uncovered: other kernels/filesystems, repeated sustained-overflow
convergence, 10M+ scale, RPM lifecycle, power-loss/media corruption, 72-hour
soak, and a production-host canary. Test counts overlap (the focused canary
tests are included in the 325 Python tests); do not add them as unique tests.

## Inode-reuse race diagnosis and bounded revalidation fix — 2026-11-08

The repeated canary failures were traced to concurrent inode-number reuse
during the scan. A path could be staged before an unlink, then ext4 could reuse
that inode number for another path before the walk ended. The candidate then
contained incompatible birth-time incarnations and correctly failed the
compatibility projection. The retained VPS2 journal recorded up to 109 such
conflicts in one pass, plus one scan with 10 conflicting inode-parent/name
edges.

The scanner now revalidates paths only when it detects an inode-incarnation
conflict. It checks the conflicting paths and their staged ancestors. If a
component is demonstrably gone, it removes that absent path and its staged
descendants before recomputing the consistency checks. Permission/I/O errors
and paths that remain present with a replacement or ambiguous identity still
fail closed. Clean scans do not pay the revalidation walk. The scan result now
reports how many staged paths were pruned.

Verification before the CLI exit-status guard: the Rust suite passed **309 tests, 0
failed, 2 ignored**; strict Clippy and formatting passed. The real ext4
inode-reuse regression passed **10 consecutive runs**. Two focused helper tests
prove that a vanished ancestor's subtree is pruned and that present ambiguous
paths remain untouched and fail-closed. The ignored 100,000-entry scale test
was also run explicitly: baseline scan 7.609 s, unchanged in-place
reconciliation 8.298 s, and a 31,629,312-byte index. Rust line coverage was
71.29% overall (70.28% regions, 75.08% functions; no branch coverage reported).

The patched static-musl binary (`3372527b…1a55990a`, embedded source digest
`4aa45d3a…8eb5082`) was also run on VPS2 in a
bounded ext4 test using a separate temporary index. Four concurrent producers
performed create/replace/delete churn while a 40,000-file scan ran; that scan
finished in 2.1 seconds with zero coverage issues. After churn stopped, a
second scan finished in 2.1 seconds and matched an independent filesystem
oracle exactly: 40,041 indexed paths, 1,490,944 allocated bytes, and 40,041
visible entries. The active VPS2 service remained running with zero restarts;
its existing kernel overflow counter remained 1 and userspace update drops 0.
The fixture and temporary index were removed. Evidence is
`docs/qualification/scan-first-fixed-cadence-20261108/vps2-inode-reuse-churn-20261108.log`.

This is a targeted fix supported by the VPS2 failure signature plus a bounded
VPS2 churn run. It is not proof that the live 1.3-million-file workload now
converges: the 24-hour candidate canary has failed, the patched daemon has not
replaced the test host's running service, and sustained high-churn full-root
recovery remains the next runtime gate.

## Current-source VPS2 churn recheck — 2026-11-08

A static-musl candidate built from the current worktree (binary SHA-256
`590b5638770957ce9c0821423b4d5888a91a2bb396484b1e10a49585cc5dacf6`, embedded
source SHA-256 `dfa9ece4c84376c0254e5513e986baa7f12d01e86938a91118c79249fc8f72a5`)
was tested standalone on VPS2 against a 40,000-file ext4 fixture. Four
producers performed 8,796 atomic replacements and ephemeral create/delete
operations during the first scan. That scan reported 180 inode-incarnation
coverage issues; it must be treated as incomplete, despite the command exit
status being successful. After churn stopped, a stable scan completed with 0
coverage issues in 4.95 seconds. An independent oracle that included directory
and root allocated blocks matched exactly: 40,041 paths and 165,363,712
allocated bytes; SQLite integrity check passed. The VPS2 systemd service stayed
active with zero restarts and was not replaced by the candidate. Full evidence:
`docs/qualification/scan-first-fixed-cadence-20261108/vps2-current-source-churn-20261108.log`.

This improves evidence that the current source can recover and reconcile an
exact stable fixture after churn. It also confirms that an observation overlapped
with atomic replacements can remain incomplete. It does not establish clean
scans under continuous production churn, full-root convergence, freshness SLO,
long-duration soak, or universal filesystem correctness.

## Direct scan exit status hardening — 2026-11-08

The current-source VPS2 churn test exposed that `dux scan` printed a nonzero
coverage-issue count but returned exit status 0. This could let automation treat
an incomplete observation as successful. Direct scans now return an error when
`ScanStats.errors` is nonzero, after printing the coverage summary; the
incomplete index remains visibly marked. A unit regression test covers clean
and incomplete scan statuses. This does not alter daemon reconciliation or
intentional scoped scans with no detected coverage issues.

The current static-musl candidate passed a VPS2 ext4 confirmation: during four
producers' 10,168 atomic replacements and ephemeral file operations, its scan
reported 221 coverage issues and exited 1. After churn stopped, a stable scan
reported zero issues and exited 0 in 4.15 seconds, with exact independent path,
allocated-byte, and entry totals plus SQLite integrity `ok`. Candidate SHA-256
`627d82be4e035038d23f7503337b7f24bef1c79e8d1e6263491c5e1283b4ed02`; embedded
source SHA-256 `bdfd711d1a4f4b3d678cfa66fa6509afd0eab0c5d98a1853230644ede4c7118d`.
Full evidence: `docs/qualification/scan-first-fixed-cadence-20261108/vps2-cli-incomplete-exit-20261108.log`.

The VPS2 daemon was left untouched. Its main process was alive with zero
restarts, while systemd continued to show `reloading/reload-notify` for nearly
four hours and logs continued to show full-root reconciliation activity. The
candidate has not been installed there. Full-root convergence remains
unverified.

## Pushed candidate VPS2 canary — started 2026-11-08 18:23 UTC

The exact clean commit `d8ea72d31953b741794ed4b1c62b24eec306a457` was built
as static-musl and installed on the dedicated VPS2 test VM. Binary SHA-256 is
`d7e41c5361011bc6acb2caa9b716488b3c5354f17b7c9b7bcc69ce73b2127b7e`; embedded
source SHA-256 is `b68589d3208409a05c69c0b626a538b54540f0c14ae99c630098180900fcc2b6`.
The old `e90f3c3` executable, systemd unit, and a 580,390,912-byte consistent
SQLite snapshot were saved under `/root/dux-rollback-d8ea72d/` before replacement.
The snapshot passed `PRAGMA integrity_check`, contained 1,495,096 inode rows,
and remains root-only. Its SHA-256 is
`1689b2e8487bc93d358d8e06c0e246a1204e452f44818d34519ca3190748dad3`.

The initial candidate full-root attempt failed closed: it recorded one
inode-incarnation conflict and concurrent hint-loss uncertainty, preserved the
old indexed rows, and scheduled a bounded 60-second retry. The automatic retry
then completed in about 182 seconds over 1,361,780 files and 163,202 directories
(1,524,982 staged path instances), with 0 coverage issues. Visible allocated
bytes were 86,700,855,296; unique bytes were 85,492,142,080. Independent
accounting verification passed after 50.85 seconds. Filesystem hints arrived
during the scan, so the last-observed index remains marked dirty until a later
observation; this is expected in scan-first mode and is not reported as
caught-up. At the latest sample there were no kernel overflows, userspace
drops, daemon restarts, or OOM events. The daemon used about 560 MiB and the
host had about 8 GiB free. The status and log are captured in the qualification
artifacts and the live journal on VPS2.

A new read-only 24-hour candidate canary started at 18:34:32 UTC using the
existing fail-closed monitor at 20% CPU and 1 GiB memory. Its first sample at
18:35:15 passed: exact running binary and systemd unit hashes; fresh completed
observation; schema-v7 path graph and accounting checks; SQLite quick check;
no restart/OOM increase; and no critical journal events. It samples hourly and
requires observation age no greater than 20 minutes. The live monitor and JSONL
are on VPS2 at `/root/dux-canary-d8ea72d/`; sample 1 is preserved at
`docs/qualification/scan-first-fixed-cadence-20261108/canary-d8ea72d-initial-20261108.jsonl`.
This is a candidate canary on the test VM, not production sign-off. The 24-hour
window is still running; continued exact observations and no critical errors
are required for it to pass. Rollback artifacts remain in place during the run.

## Release 106 canary collector v3 — 2026-11-08 22:10 UTC

The corrected five-minute v2 monitor recorded 13 consecutive passing samples
from 21:07 through 22:07 UTC. It was then stopped to replace the observer, not
because Dux failed. This is roughly one hour of evidence, not a 24-hour pass.
The v2 progress JSONL is preserved at
`docs/qualification/release-409a2f4-20261108/live/canary-24h-5m-v2-progress.jsonl`.

The replacement monitor adds strict cgroup-v2 per-device I/O capture. Its
focused Python suite passed 34 tests. Full Python discovery was rerun after the
change at 22:14 UTC: 330 tests passed. The read-only VPS2 cgroup check showed stacked
device rows `252:0` and `8:0`; values are retained individually rather than
summed to avoid double-counting.

The transient VPS2 unit `dux-canary-409a2f4-v3.service` is active. Sample 1
passed at 22:10:34 UTC after 169.6 seconds, with no monitor errors. The running
binary and unit hashes matched expectations; SQLite quick-check, schema-v7
path graph, and accounting checks passed. Dux had zero restarts, kernel
overflows, userspace drops, and OOM events. Its peak memory remained about
1.51 GiB under the 4 GiB limit. The index was dirty with a periodic full
observation in progress; the last completed observation was 1,042 seconds old.
Thus the sample passes the monitor's bounded scan-first gate but does not show
real-time freshness or a completed soak. No CPU or I/O efficiency thresholds
have been defined, so overhead acceptability remains unproven. Sample data is
preserved at
`docs/qualification/release-409a2f4-20261108/live/canary-24h-5m-v3-progress.jsonl`.

The VPS1 production service remains unchanged on its older installed release.
The last read-only production snapshot in this record (22:02 UTC) showed it
dirty and not caught up, with 312 cumulative kernel overflows and a last full
scan about 81.2 hours old. A production-host candidate canary, a 24-hour VPS2
canary using v3, a longer soak, sustained overflow-storm qualification, and
full-root independent live comparison remain release blockers.

At 22:14:58 UTC, v3 sample 2 also passed after 133.6 seconds. The completed
scan age was 407 seconds, with no scan running at the sample; the index was
dirty because filesystem changes awaited the next observation. The sample
again showed zero Dux restarts, kernel overflows, userspace drops, or OOM
events. The latest preserved JSONL then contained both v3 samples. This extends
the canary evidence by one interval only; it does not change the open soak,
freshness-SLO, production-host, or full-root qualification gates.

At 22:19:24 UTC, v3 sample 3 passed after 99.1 seconds. Its last completed
scan was 672 seconds old; Dux had zero restarts, kernel overflows, userspace
drops, or OOM events. The read-only VPS2 JSONL is preserved through sample 3.
Full Python discovery passed 330 tests after the v3 collector change. The
canary remains active and is still far short of its 24-hour and 72-hour soak
gates.

## VPS2 disk and canary recheck — 2026-11-08 22:30 UTC

The VPS2 test VM root filesystem reached 82 GiB used of 93 GiB (93%, 6.3 GiB
free). A low-priority disk survey found 11 GiB under `/var/tmp`, including
multiple old Rust build targets; other large consumers were `/var/lib`
(50 GiB, including Docker/containerd data and the Dux DB), `/var/log` (4.3
GiB, mostly the systemd journal), and `/root` (5.7 GiB, including preserved
rollback and qualification material). No database, container data, logs,
source snapshots, release outputs, or rollback evidence were deleted.

There were no active Cargo/Rust build processes. Cargo dry-runs enumerated
seven stale target directories, then `cargo clean --frozen` removed 19,497
generated build files totalling about 6.9 GiB. VPS2 root usage fell to 76 GiB
of 93 GiB (87%, 13 GiB free). The v3 canary stayed active throughout.

V3 samples 4 and 5 passed at 22:25:10 and 22:30:06 UTC, taking 145.0 and
141.7 seconds. Sample 4 saw a periodic observation in progress; sample 5 saw
the completed scan 415 seconds old. Across both samples Dux had zero restarts,
kernel overflows, userspace drops, and OOM events. The event counter rose from
114,244,697 at sample 3 to 126,957,176 at sample 5 while temporary build
outputs were cleaned. This temporal overlap is useful stress evidence but does
not prove those deletions caused all 12.7 million observed events. The
read-only v3 JSONL is preserved through sample 5 in the release qualification
directory. The 24-hour soak and remaining production qualification gates are
still open.

Docker reported a further 2.288 GiB of unused build cache and zero active
cache records. `docker builder prune --force` reclaimed that cache; actual
filesystem availability rose about 1 GiB, from 13 to 14 GiB. Post-prune
Docker accounting still showed 6.402 GiB reclaimable images, 1.955 GiB of
stopped containers, and 366.8 MiB of dangling volumes. These were retained:
the VM runs unrelated application services, and image/container/volume roles
and rollback value were not established. The systemd journal also uses 3.9
GiB; it was retained to avoid discarding unrelated operational evidence.

V3 sample 6 passed at 22:35:35 UTC after 170.0 seconds. The last completed
scan was 743 seconds old, with no scan in progress. Dux still showed zero
restarts, overflows, userspace drops, and OOM events. The JSONL is preserved
through sample 6. This remains only about 28 minutes of canary operation, not
a 24-hour or 72-hour soak.

The same cgroup counters give an early, non-SLO performance estimate: over
samples 3→6 Dux used about 307.5 CPU-seconds over 970.9 wall-seconds (31.7% of
one core). This includes one full scan and the cleanup interval; individual
intervals ranged from about 15% to 67%. The full-scan interval added about
1.32 GiB reads and 1.63 GiB writes in Dux's block-I/O cgroup counters, followed
by no measured block I/O in the next interval. Per-device stacked rows are not
summed. This is evidence for the cost of this specific mixed workload, not a
steady-state overhead claim or pass against an efficiency budget.

## VPS2 high-event scan observation — 2026-11-08 22:45 UTC

V3 samples 7 and 8 passed at 22:40:17 and 22:44:56 UTC. Sample 7 caught a
periodic full observation in progress (scan age 1,025 seconds); sample 8 saw
it complete (age 405 seconds, no scan in progress). The sample-8 index was
still dirty while awaiting the next observation, but there were zero kernel
overflows, userspace drops, restarts, or OOM events.

The VPS2 service journal confirms a full scan from 22:38:11 to 22:42:03 over
1,353,657 files and 161,163 directories (1,514,820 staged paths), with zero
coverage issues. Dux accounting verification passed at 22:43:13 in 69.7
seconds. Across 41 watcher-pressure records in the captured 6m43s interval,
the reported rate ranged 10,939–24,383 events/s; queue bytes peaked at 1,680,
with no active or cumulative queue-pressure episodes and no overflow. The
sample and journal are preserved under
`docs/qualification/release-409a2f4-20261108/live/`.

This is encouraging evidence on one Ubuntu 24.04/Linux 6.8/ext4 VM under its
current workload. It is not a deterministic event-rate replay, does not show
that the index is caught up between periodic scans, and does not qualify other
kernel/filesystem combinations or the production host.

The scan-plus-accounting window (samples 7→8) used 244.3 Dux CPU-seconds over
278.8 seconds (87.6% of one core), with about 1.75 GiB read and 1.66 GiB
written in the service cgroup counters. This is material cost and remains an
optimization target. One partial WAL checkpoint warning reported busy=0,
7 frames total, 3 checkpointed, and a 34.4 MB WAL. It remained below the 256
MiB write-pause guard and the canary passed, but recurrence and WAL growth
should be tracked; whether the read-only observer contributed is unknown.

## Workspace disk cleanup — 2026-11-08

The local `/` filesystem was 61% used with 70 GiB free. The repository's
regenerable Rust `target/` directory occupied about 3.3 GiB; `cargo clean`
removed 8,653 build artifacts (3.6 GiB reported by Cargo). Afterwards `/` was
60% used with 72 GiB free. Source, qualification logs, and the VPS2 test VM
were not touched by this cleanup.

## Service sandbox policy review — 2026-11-08 23:49 UTC

`systemd-analyze security --offline=yes packaging/dux.service` scored the
checked-in daemon unit **3.3 OK** on the local systemd 255 host. The unit
retains root and `CAP_SYS_ADMIN`/`CAP_DAC_READ_SEARCH` for global fanotify and
file-handle resolution, while denying unrelated capabilities and syscall
groups. The offline score is not a runtime compatibility or complete security
proof. VPS2 still runs the release-106 service and its older unit. The Nov 3
production result of 5.1 MEDIUM is historical and superseded by the Nov 8
effective-unit observation below. Package-based rollout and complete
effective-policy verification remain open.

## Fresh read-only production host snapshot — 2026-11-08 23:51 UTC

The production host (`vps1`) is now running an effective unit with
`LimitCORE=0`, `SecureBits=noroot noroot-locked`, `NoNewPrivileges=yes`, only
`CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`, and a broad syscall deny filter.
`systemd-analyze security dux.service` reports **3.3 OK**. The loaded unit hash
is `dd34d308…`; it is not byte-identical to this worktree's packaged unit
(`e12d1f4a…`). The source package also explicitly denies the `bpf` syscall;
the loaded production unit's filter does not. The systemd version supports
that filter, but the package/config drift must be reconciled and tested before
claiming the production policy matches the candidate.

The active daemon is still **dux 0.6.3, commit `e0cc915f8f37`**, binary SHA-256
`e148692f…`, not the qualified source candidate. Its read-only status at this
sample showed: live and unpaused, 4 lifetime systemd restarts, **326 cumulative
kernel overflow notifications**, zero reported userspace drops, a dirty index
(`fanotify queue overflow`), **3,046 pending updates**, and a completed scan
298,907 seconds (about 3.46 days) old. The index reports 1,289,547 nodes and
125,473,435,648 logical bytes; the on-disk database is about 2.8 GB with a
128 MiB WAL. These values show that the production freshness/overflow issue
persists despite the unit-policy improvement. No production daemon, package,
database, or systemd state was changed during this snapshot.

## Production policy gate, health, and disk recheck — 2026-11-08 23:54 UTC

The package lifecycle's read-only `assert_service_security_policy()` check was
run against the effective production unit and **failed** because `bpf` is not
in its explicit syscall deny list. This is a concrete config drift gate; the
checked-in `packaging/dux.service` denies it. The focused lifecycle policy test
module passed **70/70** tests, including a regression case that rejects a
missing BPF denial with `CAP_SYS_ADMIN`. The live failure was observed without
changing systemd or restarting Dux. `systemd-analyze verify` passed for both
checked-in units.

A second read-only Dux status at 23:54 UTC still showed a live, unpaused
production daemon, zero new reported drops, and no write error, but the index
was dirty from fanotify overflow. The cumulative overflow count remained 326;
pending updates increased from 3,046 to 9,086, and `behind_seconds` from
109,218 to 109,398. The last completed full scan was 299,087 seconds old. The
SQLite main DB was 2,802,872,320 bytes, with a 128 MiB WAL. `/` had 66 GiB
available (63% used). This is no longer just a historic snapshot: the main
freshness failure is still live.

Disk survey at the same time: local `/` was 185 GiB total, 111 GiB used, 66
GiB free; `target/readiness` was 44 MiB, `target/dux-auditable` 579 MiB, the
qualification records 7.9 MiB, and `.git` 395 MiB. VPS2 `/` was 93 GiB total,
76 GiB used, 13 GiB free; its Dux state occupied 660 MiB and the canary
evidence/rollback directory 589 MiB. No cleanup was warranted while the soak
and rollback evidence remain active.

## Production event-rate sample — 2026-11-08 23:56:09–23:56:39 UTC

Two read-only status snapshots 30.0 seconds apart recorded 478,955 more events
seen (**15,964/s**) and 250,093 more resolved (**8,336/s**). The kernel
overflow counter rose by **5** during those same 30 seconds (335→340), while
pending updates fell by 3,916. Main DB and WAL file sizes did not change during
the interval. `/proc/sys/fs/fanotify/max_queued_events` is 65,536. This directly
confirms burst overflow is still occurring on production under a workload with
high event volume, even while the pending-update count can fall. It does not by
itself isolate whether the limiting factor is CPU throttling, a short producer
burst, event-resolution I/O, or queue telemetry error; the installed service is
the older `e0cc915` binary, and queue bytes were not captured in these two
samples.

## Scan CLI settling fix and controlled VPS2 burst — 2026-11-09 00:25 UTC

The source now allows a bounded 10-second settle interval after a scan-first
observation before `dux scan` reports uncertainty. The scan can finish its
walk just before the independent accounting worker and watcher publish their
final clean state; previously the CLI could report “stale or uncertain” in
that brief window. A deterministic boundary test covers the settle rule.

The updated test-only release binary was run on the isolated VPS2 ext4 fixture.
With the index dirty after a file creation, the requested observation and
accounting completed in 4.50 seconds; `dux scan` returned success, final status
was `dirty=false`, and `dux find` returned the new file. Rust tests after the
change passed **322, failed 0, ignored 2**; strict Clippy, formatting, and diff
checks passed. One earlier full run had a single ext4 inode-reuse test ENOENT;
its focused rerun and the next complete suite passed, so this remains a test
flake to watch.

The release-107 static candidate handled 11 controlled bursts of 32,000
create/unlink pairs each (352,000 pairs, about 704,000 filesystem operations)
on the separate ext4 loop mount. It saw 975,442 hints with zero overflow or
userspace drops; the full observation found 1 file, 13 directories, and 14
paths with no coverage issues, then accounting passed. Sampled hint throughput
peaked at 45,949/s; sampled queue bytes peaked at 24, with no queue-pressure
episodes. The concurrently installed release-106 root service counted two
overflows. These results are one controlled VPS2 comparison, not a general
throughput guarantee.

The release-106 v4 canary passed samples 1–8 and failed sample 9 during this
test. It recorded two overflows and a service restart-count change. The restart
was caused by the loop mount changing filesystem coverage: the old daemon
exited with its explicit “filesystem mounts changed” status and systemd
restarted it. This invalidates that soak; it is not evidence of a spontaneous
crash. The exact service event and failed JSONL sample are preserved under
[`scan-first-pressure-20261109`](docs/qualification/scan-first-pressure-20261109/README.md)
and the release-106 v4 live record. The loop mount and its test database were
removed after preserving the logs.

During the same production-host review, a transient read-only collector on
vps1 was found retrying a missing script every 10 seconds; it had reached
11,618 restarts with exit status 2. The transient unit
`dux-soak-archive-vps2.service` was stopped because its `ExecStart` target
`/home/dux/target/readiness/vps2-qualification-20261106/soak-archive/collect.py`
no longer existed. The Dux production daemon and package were not touched.

## Current production and disk snapshot — 2026-11-09 00:45 UTC

Read-only checks show `/usr/bin/dux` is still `0.6.3 (e0cc915f8f37)`, active
with systemd `NRestarts=4` and `ExecMainStatus=0`. `dux status --json` reports
`dirty=true`, reason `fanotify queue overflow`, **340 cumulative kernel
overflow notifications**, zero userspace drops, and a last full scan
**302,148 seconds old** (about 83.9 hours). It reports 88.7M events seen,
51.6M resolved, 9 pending, and a 128 MiB WAL. The recent small pending count
does not make the multi-day-old full observation current. This confirms the
production reliability issue is still unresolved; no package or database was
changed.

The effective service has systemd exposure score 3.3 OK. The loaded unit
SHA-256 is `dd34d308…`; its effective syscall deny list omits explicit `bpf`,
while the checked-in packaged unit includes it. The lifecycle security policy
gate therefore still fails. No live unit policy was changed. The local root
filesystem is 185 GiB total, 112 GiB used, and 65 GiB free (64% used); local
disk exhaustion is not the current production blocker.

## Release-108 build and qualification harness correction — 2026-11-09 01:00 UTC

Clean commit `0fc18fc` built successfully as release 108 (static-musl binary,
DEB, RPM, source archive, debug symbols, and checksums). All eight checksums
passed; the binary embeds the expected commit and source-tree digest. The build
audited the locked 229 dependencies and the exact binary. The candidate was
not installed.

The first isolated readiness attempt stopped before its first crash round
because the old harness waited for the event pipeline to become certain. That
is not the scan-first correctness contract: fanotify events are hints and may
remain uncertain until a full observation. The harness was corrected to force
a new observation, wait for accounting verification, and compare every path
and identity against filesystem truth. It passed all **331 Python tests** and
release 108 passed a one-round crash/restart exactness smoke test, recovering
in 1.909 seconds. The initial failure and smoke evidence are preserved under
[`scan-first-pressure-20261109`](docs/qualification/scan-first-pressure-20261109/README.md).

The expanded matrix is now being prepared against the corrected harness. The
VPS2 `/dev/shm` free space supports a 768 MiB fixture, but not the 2 GiB image
required to promote the dedicated disk-full and 100,000-file churn suites;
those two suites are excluded from this run. The remaining matrix is
unqualified until its completed report is checked.

The 30-round candidate attempt then found that fault hooks for atomic install
are absent from the in-place scan-first path. I added four `fault-injection`
only checkpoints around in-place observation and publication, and updated the
readiness harness to select them only in scan-first mode. The production
feature build compiles these checkpoint calls to no-ops. The updated source
passes the 322-test Rust release suite (2 ignored) and all 332 Python tests;
Clippy with warnings denied, formatting, and diff checks pass. A fresh package
build was required before re-running crash boundaries; that build is now
release 110 from clean commit `6dacdf5`. Its artifacts/checksums, embedded
source digest, lockfile audit, and exact-binary audit pass. The scan-first
crash suite passed 8/8 rounds on VPS2, exercising each of the four in-place
fault boundaries once plus four random-timing restarts. Recovery took
0.736–1.109 seconds, with SQLite integrity and exact filesystem/index equality
passing after every crash. This is targeted recovery evidence; the wider
matrix remains unqualified until its completed report is checked.

## Release-110 expanded scan-first evidence — 2026-11-09 01:47 UTC

On VPS2, the release-110 product binary (`1707a102…568e66`) and its matching
fault-injection binary were rerun with a scan-first-aware harness. One run
passed 30/30 crash/restart rounds, actual kernel queue overflow detection and
reconciliation, and the CPU burst: 12,008 hints, zero overflows, event-thread
CPU reported about 0.01 cores, and exact filesystem/index validation passed.
The CPU figure is a single controlled burst result, not a workload envelope.
The overflow test passed its exact-recovery assertion but did not finish with a
clean status during that fixture, so it does not establish clean-state timing.

Subsequent targeted runs passed the unprivileged unreadable-subtree check and
the capability check. With `DAC_READ_SEARCH` removed, a complete observation
matched filesystem truth while Dux kept its dirty marker visible; after
capability restoration, exact recovery passed. That is evidence for honest
degraded state and exact data on the tested fixture, not for clean status with
missing capabilities.

The expanded matrix remains incomplete. Disk and 100,000-file churn were
excluded because VPS2 `/dev/shm` could not safely hold their required 2 GiB
fixtures. The pressure suite stopped at a harness gate requiring an atomic
rebuild staging file, which scan-first in-place reconciliation does not
produce. The OOM run was interrupted during recovery qualification; do not
count it as passed. Mount, WAL, startup, retry, root, and soak were not
completed in the final attempt. No 72-hour soak or 24-hour canary was run.

The harness-only corrections passed 337 Python tests in the documented local
suite run, plus strict Clippy, formatting, and diff checks. The standard
release Rust invocation passed 319 tests with 2 ignored; the all-features run
recorded earlier passed 322 with 2 ignored. Rust line/region/function coverage
remains 72.16%/70.99%/76.03%, respectively; branch coverage was not reported.
Current-source evidence and failed attempts are retained in
[`scan-first-pressure-20261109`](docs/qualification/scan-first-pressure-20261109/README.md).
Production remains on the older installed binary; none of these candidate
runs installed or modified the production package or index.

### Verification refresh after the retry-state fix — 2026-11-09

The current working tree passes **323/325 Rust release all-features tests**
(2 ignored) and **340/340 Python tests**. A focused ext4 inode-reuse
regression also passed. Updated llvm-cov results are **72.20% line, 71.02%
region, and 76.05% function** coverage; branch totals are unavailable from
this stable llvm-cov run. These supersede the earlier 322-test / 72.16%
figures above.

The new Rust regression covers manual retry metadata cleanup when the legacy
upgrade source is read-only. It passed in the full Rust suite. A fresh release
115 static candidate with the fix passed the VPS2 startup/upgrade integration
suite in 63.0 seconds, including automatic retry from v4 to v7 after an
injected failed upgrade. The test report and hashes are in the qualification
README. This closes that specific integration gate for release 115. The broader
matrix and production deployment gates remain open.

### Pressure suite refresh — 2026-11-09

The release-115 candidate passed four scan-first pressure scenarios on VPS2 in
110.9 seconds: reconciliation cancellation and exact recovery during synthetic
high load and Critical pressure, plus successful reconciliation during CPU
Elevated and I/O Elevated pressure. The disposable ext4 fixture contained
20,000 seeded files; the transient worker was limited to 25% of one CPU and
4 GiB memory. The captured report is
[`release115-pressure.json`](docs/qualification/scan-first-pressure-20261109/live/release115-pressure.json).

Current local results: **323/325 Rust all-features tests passed** (2 ignored),
**341/341 Python tests passed**, and coverage is **72.20% line, 71.02% region,
76.05% function**. These counts are separate suites, not a count of unique
cases. Strict Clippy, formatting, and diff checks passed on the current source
as recorded in the qualification log. Branch coverage is unavailable from the
stable llvm-cov run. The two ignored Rust tests and the broader disk, churn,
overflow, OOM, mount, WAL, package lifecycle, multi-day soak, and canary gates
remain open; this is not a production-readiness or zero-error guarantee.

## Latest evidence checkpoint — 2026-11-09 05:52 UTC

The release-117 P0 matrix is still running on the disposable VPS2 ext4 fixture.
Crash recovery passed 8/8 rounds; real queue overflow recovery passed with
exact fixture equality (99.055 seconds, but final status remained conservatively
not clean); CPU burst passed (12,000 hints, 0.141-second drain, 0.014 measured
event-thread CPU, 113.471-second exact validation against a 120-second limit).
Churn is in progress. Details and the timestamped report snapshot are in the
qualification README. Production remains untouched.

The crash fixture exposed abandoned in-place scan stage databases after killed
processes (four files totaling about 84 MiB). In-place reconciliation now runs
the existing strict stale-stage cleanup before building a new stage. The new
Rust regression passed and proves unrelated lookalike files remain untouched;
the crash harness now checks that recovery leaves no generated stage artifacts.
This fix was made after release 117 was built and needs a rebuilt-candidate
crash qualification.

On the updated local tree, **324 Rust tests passed, 0 failed, 2 ignored**,
**345 Python tests passed**, and strict Clippy, formatting, diff, and Python
syntax checks passed. llvm-cov reports **72.25% line, 71.09% region, and
76.07% function coverage**; branch coverage is unavailable. These results do
not qualify the unfinished P0 suites, the updated candidate, package lifecycle,
other filesystems/kernels, multi-day soak, or production canary.
At 06:00 UTC, the VPS2 churn test had completed 4 of 10 batches (8,000 of
20,000 operations) and was still running; its progress record is linked in the
qualification README.
At 06:02 UTC, churn reached 5/10 batches (10,000/20,000 operations); no final
suite result is available yet.
At 06:13 UTC all 20,000 churn operations across 10 batches were complete, but
the final filesystem exactness and history checks were still pending.
The final churn gate passed in 1,623.691 seconds: all 10 independent exact
comparisons and the concurrent rebuild/history checks passed, with 70,795
kernel hints seen and zero dropped. The index stayed conservatively uncertain
in scan-first mode, so this is exactness evidence, not a clean/freshness SLO.
The P0 matrix has advanced to disk testing.

The release-117 disk suite then stopped at a harness-only timeout: it expected
the event-apply `pending` counter to rise, which scan-first mode does not use.
The run failed before testing ENOSPC or read-only recovery. The harness now
checks for 64 fresh fanotify hints and no new overflow in scan-first mode, while
preserving the old queue assertion for event-apply mode. The full Python suite
passes **347/347** with regressions for both event models, expected stale
prechecks, and database content fingerprints. The first release-118 disk
attempt reported 64 missing `paused-*` entries at the immutable read-only
oracle. A saved post-failure SQLite backup compared against the saved exact
filesystem snapshot has zero path or inode/allocation mismatches, all 64 paths,
and a clean SQLite integrity check. This suggests a read-only oracle mismatch,
not data loss, but the disk gate remains open. A workspace-capacity preflight
rejected one subsequent launch before tests began; the generated ext4 image
workspace was removed only after its SQLite/truth snapshot had been archived.
A fresh retry is now active with main DB/WAL fingerprints and a normal
read-only exact comparison; the prior failure workspace and forensic snapshot
remain documented.

## Latest evidence checkpoint — 2026-11-09 06:48 UTC

Release 118's disk qualification passed on VPS2 (Linux 6.8.0-147-generic),
using a disposable 2 GiB ext4 image seeded with 100,000 files. The worker was
limited to 25% CPU and 4 GiB memory. Low-disk pause/resume, scan while the
image was full, and read-only scan failure all retained a readable, exact
index; the test's reserved-space measurement was 1,582,514,176 bytes. The
suite took 333.091 seconds (systemd worker runtime 6m31s, peak RSS 927.8 MiB).
Report: [`release118-disk-final2.json`](docs/qualification/scan-first-pressure-20261109/live/release118-disk-final2.json),
SHA256 `c77403090b41eb8e79c006dcdc6ddd851d4fdbd59093e7692eb5b4f6149883bf`.

Local source evidence: Rust all-features release tests **324 passed, 0 failed,
2 ignored**; Python workflow/harness suite **347/347 passed**; LLVM coverage
**72.25% lines, 71.09% regions, 76.07% functions** (branch coverage not
available). Strict Clippy, `cargo fmt --all -- --check`, Python compilation,
and `git diff --check` pass. A focused regression for stale in-place scan
stage cleanup also passed. These are separate test suites, not a deduplicated
count of unique test cases.

Release-117 VPS2 evidence also passed 8/8 crash rounds, the real fanotify
overflow recovery scenario, the CPU-burst scenario, and 10/10 exact churn
batches (20,000 operations total, 70,795 observed events, zero dropped).
Pressure and startup/upgrade integration suites passed on earlier candidate
builds. Their reports and qualifications are listed in the qualification
README. Failed harness attempts are retained; they are not counted as product
test passes. Release 118's disk-only report explicitly did not run crash,
churn, CPU, overflow, pressure, startup, mount, OOM, WAL, or other matrix
suites.

This remains short of production-ready: 100k/1m/10m performance budgets,
package upgrade/rollback and musl, privilege/dependency release review,
other filesystems/kernels, media/power loss, multi-day WAL endurance, a 72-hour
soak, and a 24-hour canary are unqualified. Production was not modified.

## Latest evidence checkpoint — 2026-11-09 07:18 UTC

The release-118 crash suite has also passed **8/8 rounds** on the same
100,000-file, 2 GiB ext4 fixture and 25% CPU/4 GiB memory limits. The four
in-place crash boundaries (`in_place_started`, `projection_ready`,
`before_commit`, `after_commit`) each passed once, with four random-time
crashes. Every recovery passed exact filesystem comparison and SQLite
integrity checks; no abandoned scan artifacts remained. Recovery took
97.592–114.605 seconds per round. The suite took 1,548.604 seconds; transient
unit peak RSS was 956.5 MiB and peak swap was 458.8 MiB. The installed
production service remained active with `NRestarts=0`. Report:
[`release118-crash-8.json`](docs/qualification/scan-first-pressure-20261109/live/release118-crash-8.json),
SHA256 `0e89e7bf29a6eb634db8aeb2fdb0ccf82c946fbe8637ec260d6e7e60e7c23bf5`.

The disk and crash passes together provide 11 passing release-118 scenario
checks (three disk gates plus eight crash recoveries). The release-118
overflow, churn, CPU, mount, OOM, WAL, retry, package lifecycle, scale,
endurance, and canary gates remain open.

## Latest evidence checkpoint — 2026-11-09 07:22 UTC

Release 118's isolated overflow scenario passed on VPS2. It forced one real
fanotify overflow by creating 18,432 files against a 16,384-event queue. Dux
observed the overflow, ran a newer full observation, passed independent
accounting verification, matched the filesystem path/inode/allocation oracle,
and cleaned up the storm. Recovery took 13.358 seconds. Status remained
conservatively uncertain after the observation; therefore this proves exact
reconciliation, not an immediate clean-state or freshness SLO. Report:
[`release118-overflow-retry.json`](docs/qualification/scan-first-pressure-20261109/live/release118-overflow-retry.json),
SHA256 `f6b723dba4900a522ecb69fd130403412d20d9dfc248a11c6bf1758fc8dfcba4`.

The first attempt found a harness race: it treated an automatically started
overflow observation as the explicit request's completion while another scan
was still active. The gate now requires a newer generation and attempt, and
waits until any queued follow-up observation has ended. The first report is
retained at
[`release118-overflow-failure.json`](docs/qualification/scan-first-pressure-20261109/live/release118-overflow-failure.json).
The full Python harness suite passes **348/348** after adding regression
coverage for this race. The release-118 disk, crash, and overflow suites have
now passed; churn, CPU, mount, OOM, WAL, retry, package lifecycle, scale,
endurance, and canary remain open.
The latest full Python output is archived at
[`current-source-python-tests-20261109.log`](docs/qualification/scan-first-pressure-20261109/live/current-source-python-tests-20261109.log),
SHA256 `2b974b8270a5205f6af064c13dc95d4e9bf566149768bc6906b9b291e795d6cb`.

## Latest evidence checkpoint — 2026-11-09 07:58 UTC

Release 118's production-shaped churn suite passed on VPS2. It performed
20,000 operations in ten 2,000-operation batches against a 100,000-file
fixture on a disposable 2 GiB ext4 image, with 25% CPU quota and a 4 GiB
memory cap. Every batch matched paths, inode identities, allocations, and all
rollups; concurrent rebuild and history continuity passed. The run saw 70,772
events overall with zero drops. Per-batch exact convergence took
109.200–202.795 seconds. The index remained conservatively uncertain at every
churn checkpoint despite exact oracle matches, so this does not prove a
clean-status/freshness SLO under continuous writes. Suite duration was
1,977.9 seconds; worker runtime was 33m51s, peak RSS 1,019.6 MiB and peak swap
525.7 MiB. Production remained active with `NRestarts=0`. Report:
[`release118-churn-20k.json`](docs/qualification/scan-first-pressure-20261109/live/release118-churn-20k.json),
SHA256 `aba0b3207a849c1ba872113626603942086a979abb4d2369445045bf81f1a836`.

Release 118's CPU-burst suite passed on VPS2: 12,004 fanotify hints produced
in 3.401 seconds, the reader observed them in 0.153 seconds, and exact
validation took 7.634 seconds. Measured event-thread CPU was 0.036 of one core
against a 10% Dux event-thread cap and a 25% outer cgroup quota; no kernel
overflow was observed. Suite time was 46.914 seconds. Report:
[`release118-cpu.json`](docs/qualification/scan-first-pressure-20261109/live/release118-cpu.json),
SHA256 `7508d67041eb3de045b999d2f1b760ae7a6df4d77dc23b76e4c009557543bb0e`.

The release-118 coverage suite also passed: an unprivileged scan over an
unreadable subtree exited incomplete and explicitly preserved the
incomplete-coverage state. Report:
[`release118-coverage.json`](docs/qualification/scan-first-pressure-20261109/live/release118-coverage.json),
SHA256 `660a15b7428c96c5989149161e3b332777b2cdc295dd8b94da0ee354a251ee15`.

Capability qualification passed on release 118: removing the handle-lookup
capability failed closed and preserved uncertainty; restoring it allowed
exact convergence. The scan-first case matched the independent oracle while
the capability was absent, and retained the conservative dirty marker.
Report: [`release118-capability.json`](docs/qualification/scan-first-pressure-20261109/live/release118-capability.json),
SHA256 `154478a1ab51100b8a4836753afa7b7578d0a652cc3ca04c241f67b6ebb92252`.

Mount coverage qualification passed: the candidate detected a newly mounted
filesystem, added watch coverage, detected the mount's disappearance, and
returned to exact accounting. Report:
[`release118-mount.json`](docs/qualification/scan-first-pressure-20261109/live/release118-mount.json),
SHA256 `7dfad2246bc3ddc0088c31c67991481f56f0edc6ad45f9246b2adc21a6025f65`.

The release-118 retry suite passed: a fault after in-place publication caused
an accounting-verifier failure; the bounded retry count and deadline survived
restart; event ingestion remained active during cooldown; then an automatic
retry completed verification, cleared retry state, and restored an exact
index without operator intervention. Duration was 62.105 seconds. The first
attempt selected a nonexistent atomic-install checkpoint; a later harness
attempt accepted the database commit before verification completed. Both
reports are preserved, and the corrected test waits for retry metadata to be
cleared by the verifier. One retry attempt also observed elevated host CPU
pressure; Dux deferred reconciliation until admission criteria allowed it.
Report:
[`release118-retry.json`](docs/qualification/scan-first-pressure-20261109/live/release118-retry.json),
SHA256 `b910ad45267e90fe93730e12349f520de178d560b3abbcb72c8a5e78e032cae9`.
The complete Python suite now passes **351 tests**; output:
[`current-source-python-tests-20261109.log`](docs/qualification/scan-first-pressure-20261109/live/current-source-python-tests-20261109.log),
SHA256 `3ba361f7ad65f419bd8b4195d314fe498d7d10fb5b44212dc0f0a25443e2e907`.

Release 118 also passed the actual cgroup OOM recovery gate. The isolated
daemon hit a 1 MiB memory limit during reconciliation, received a real
`oom-kill`, restarted once under systemd, and exactly recovered 32 filesystem
changes made while it was down. The installed production service stayed
active at zero restarts; the limit change applied only to the test unit.
Report: [`release118-oom.json`](docs/qualification/scan-first-pressure-20261109/live/release118-oom.json),
SHA256 `c49dcfca79ab1437119d6c5f92d2e8f7a81a998e72c6f472220fcaa5a409005b`.

The release-118 long-reader WAL test passed as well. A held SQLite reader
retained its snapshot during 12 observations and 576 file mutations; peak WAL
was 959,992 bytes and fell to zero after reader release and truncate
checkpoint. Final index integrity and exactness passed. Report:
[`release118-wal.json`](docs/qualification/scan-first-pressure-20261109/live/release118-wal.json),
SHA256 `83f9e0150b5d41f20077f692e43c16363956bc6af4d9eea70bffba1c3dde7be8`.

Host-pressure behavior passed on release 118: high-load and Critical
observations cancelled while retaining old state, then recovered exactly;
CPU Elevated and I/O Elevated observations completed. All four induced file
changes appeared after recovery. Report:
[`release118-pressure.json`](docs/qualification/scan-first-pressure-20261109/live/release118-pressure.json),
SHA256 `8b2116cc39ad58ddd0b478e5bc1ba045d44e07ed52a84e731b7eea90c7bcf915`.

Startup/upgrade qualification passed on release 118. The fresh schema-v4
upgrade preserved old-index queries, stayed alive after an injected upgrade
scan failure, retried automatically, installed v7, replayed changes from the
upgrade window, preserved history, and repaired an inode-kind mismatch with
a complete observation. The scan-first candidate reports observation repair;
the live event-apply inode-kind repair gate is not applicable to this mode.
Report:
[`release118-startup.json`](docs/qualification/scan-first-pressure-20261109/live/release118-startup.json),
SHA256 `b790c68176ba65739985c37ae953b6431c16c11745aab9412958849709c25790`.

## Qualification summary — 2026-11-09

On the current working tree, the Rust release/all-features suite passes
**324 tests, 0 failed, 2 ignored**; the Python qualification/workflow suite
passes **351 tests**. Strict Clippy, formatting, Python compilation, and
`git diff --check` pass. LLVM coverage is **72.25% lines, 71.09% regions,
76.07% functions**; this run provides no branch-coverage totals.

The P0 matrix now has **14 of 14 scenarios passed across release 118 and
release 119**. The 13 earlier scenarios ran against release 118. Every Rust
product source matches release 119 except `src/scan.rs`, where the only
difference is documentation and visibility narrowing
(`reconcile_in_place`: `pub` to `pub(crate)`); runtime behavior is unchanged.
The full-root scenario passed against the clean release-119 build from commit
`4fe2730ced1d`: 1,430,977 files, 174,668 directories, and 1,640,588 entries,
complete coverage, accounting/integrity checks passed. It ran for 298.673
seconds at a 75% CPU quota; peak memory was 1.1 GiB. The first 25%-quota run
timed out at 780 seconds and is retained as a performance failure at that
budget, not counted as a pass. See
[`release119-root.json`](docs/qualification/scan-first-pressure-20261109/live/release119-root.json)
and the [qualification matrix](docs/qualification/scan-first-pressure-20261109/README.md).
Earlier harness failures remain preserved; only corrected final reruns count
as passes.

These results qualify important recovery paths on the tested Linux/ext4 VPS2
setup, but do not establish universal correctness or 100% production
readiness. The Debian package lifecycle now passed release 106→119; RPM and
other-distribution package behavior, direct upgrade from older pre-scan-first
production state, large-scale 1M/10M performance, other filesystems/kernels,
hardware-loss behavior, long-duration soak, and canary gates remain open. The
root-scan test did not modify the installed index; the package test restored
the pretest release-106 package and index, then the old daemon completed its
normal scan. Final restore verification passed SQLite integrity, service
active/enabled state, and binary/unit identity checks.

## Scoped security scan — 2026-11-09

`cargo audit` found no advisories across 229 locked Rust dependencies, using
an advisory database last updated 2026-11-08. Trivy found no vulnerabilities
in either Cargo lockfile; it detected no deployment configuration files, so
configuration security remains unqualified. Gitleaks found no secrets in
`src/` or `scripts/`; other workspace paths, generated artifacts, and Git
history were not covered. A Semgrep ruleset scan did not produce a report and
is inconclusive. Detailed evidence and scan limits are in
[`security-review-20261109.md`](docs/qualification/scan-first-pressure-20261109/security-review-20261109.md).

## Current verification and VPS2 candidate canary — 2026-11-09 09:45 UTC

I reran the current checked-out source: Rust release/all-features tests passed
**324/324 (2 ignored)**; Python readiness/workflow tests passed **351/351**;
strict all-target/all-feature Clippy, formatting, Python compilation,
`git diff --check`, and systemd unit verification passed. `systemd-analyze
security --offline=yes packaging/dux.service` reports **3.3 OK**. A refreshed
`cargo audit --json` found **0 advisories across 229 locked dependencies**;
its advisory database was updated at 2026-11-09 09:22 UTC. These checks do not
close the documented branch-coverage, 100%-coverage, or configuration-security
gaps.

On VPS2, the currently installed release-106 baseline finished its 09:35 UTC
full-root observation at 09:39:54, covering 1,474,513 files, 175,623
directories, and 1,650,136 paths with zero coverage issues. The walk took 290
seconds; independent accounting verification passed after 60.7 more seconds.
The host recorded 20.9 million hint events across the observation and
verification interval, with zero kernel overflow notifications, zero
userspace drops, and zero daemon restarts. The index stayed conservatively
dirty between observations. This is a single current-host sample, not an
endurance or throughput benchmark.

After taking and verifying a root-only SQLite backup (668,336,128 bytes,
schema v7, `quick_check=ok`), I upgraded only the marked VPS2 VM to the clean
release-119 package. Candidate binary SHA-256 is
`e4841e7b75a4785a3fcf2597d4e711b21df18ebca25639f27ec0d96b9dfa293d`; the
effective service-unit hash is `e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.
VPS1 production remains untouched. A read-only candidate canary is now active
at five-minute intervals, with a 20-minute full-observation-age gate; its
collector is capped at 20% CPU and 1 GiB memory. Sample 1 passed in 60.766
seconds while a scan was in progress: schema-v7 graph, exact accounting,
SQLite quick check, binary/unit identity, service health, journal, and memory
events all passed. It found 1,646,654 inodes and 1,650,136 path instances,
with zero accounting-graph defects. This is **1 sample of the required 289**;
the 24-hour canary is in progress, not passed. The rollback package and
database snapshot remain on VPS2.

Evidence snapshot:
[`release119-canary-progress-20261109T0943Z.jsonl`](docs/qualification/scan-first-pressure-20261109/live/release119-canary-progress-20261109T0943Z.jsonl),
SHA-256 `fda22a0a9cf3d8bcc08570ae19fad42eee77d8c3e34c16da8c925dc72d449ea5`.
The live append-only monitor log is on VPS2 at
`/root/dux-canary-119/candidate-canary.jsonl`.

### Canary and security scan follow-up — 2026-11-09 09:50 UTC

The release-119 canary has now passed **2 samples of the required 289** and
remains active. Sample durations were 60.766 and 47.603 seconds; both passed
with zero findings. At sample 2, Dux had zero restarts, OOM events, kernel
overflows, userspace drops, or journal-critical failures. Its measured memory
peak was 1,329,266,688 bytes under the 4 GiB cap. Per-device I/O counters had
reached 1,781,170,176 bytes read and 1,484,582,912 bytes written on device
`8:0` since service start. Device `252:0` is a stacked representation of the
same underlying I/O and is not added. This short interval is not a steady-state
performance qualification.

The refreshed Cargo audit still finds zero advisories in 229 dependencies.
Gitleaks found no credentials in `src/`, `scripts/`, or `packaging/`; the three
documentation matches were verified false positives on certificate signer
labels, with matched text redacted in the retained report. Semgrep remains
inconclusive because its remote ruleset fetch failed TLS verification; no
certificate bypass was used. `.github/` is absent from this checkout. Reports
are under
[`security-current-20261109`](docs/qualification/scan-first-pressure-20261109/live/security-current-20261109/),
and details are in the [security scan record](docs/qualification/scan-first-pressure-20261109/security-review-20261109.md).

### Candidate canary checkpoint — 2026-11-09 09:54 UTC

The release-119 VPS2 canary remains active and has **3/289 passing samples**.
The third sample took 51.609 seconds; scan ages across the three samples were
575, 375, and 679 seconds. All had zero reported kernel overflows, userspace
drops, service restarts, OOM events, and monitor errors. VPS2's root filesystem
had 11 GiB free at the latest check. This is about 11 minutes of a required
24-hour run, not a soak pass.

The three-sample log snapshot is
[`release119-canary-progress-20261109T0954Z.jsonl`](docs/qualification/scan-first-pressure-20261109/live/release119-canary-progress-20261109T0954Z.jsonl),
SHA-256 `b7099f4ee4784ee8f0ee1d0c985b77181e470dca336171ae1f77ec9187042eec`.

### Production host and migration gate recheck — 2026-11-09 10:06 UTC

A read-only inspection of the actual VPS1 production host confirms the main
incident remains live. VPS1 runs legacy event-apply Dux `0.6.3` commit
`e0cc915f8f37` (binary SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`), with
four systemd restarts. Its index is dirty from fanotify overflow, the last
completed full scan is **335,854 seconds (93.3 hours) old**, and
`behind_seconds` is **146,165 (40.6 hours)**. It has 435 cumulative kernel
overflows, zero userspace drops, and schema version **6**. The main DB is
2,932,318,208 bytes and the WAL is 128 MiB. VPS1 has about 62 GiB available on
its 185 GiB root filesystem; disk exhaustion is not the current cause.

A 30-second status sample at 09:58 UTC saw no new overflow: 8,829 events seen
and 8,543 resolved, with pending updates falling from 656 to 177. The index
remained dirty and its last-scan age remained over 93 hours. During the latest
10-minute journal window, 157 stale-file-handle resolution warnings appeared;
there were no new explicit overflow, scan-failure, reconciliation-failure, or
accounting-failure log messages. No new overflow in that short sample does not
clear the existing dirty state.

Production I/O counters are also large. Since the service started on Nov 7,
the cgroup records about **297.1 GB written on each of two stacked device
rows**. `systemctl`'s aggregate reports about 594.2 GB because it counts both
rows; that aggregate must not be presented as unique physical writes. A
30-second follow-up measured 21.2 MB written per row (42.3 MB in the aggregate)
and negligible reads. These are cgroup-attributed I/O counters, not a hardware
write-amplification benchmark; no write budget exists yet.

The production DB being schema 6 exposes an unclosed compatibility gate:
release 119's Debian lifecycle test upgraded a schema-7 release-106 DB, and
the v6-to-v7 regression had not previously tested production scale. A new
synthetic 1.3M-file schema-6-layout qualification now passes, but it does not
use the actual production database. Exact production DB migration remains an
open gate. The candidate remains only on VPS2; no VPS1 package, DB, or service
was changed. Full details and raw counters are in
[`vps1-production-readonly-20261109T1006Z.json`](docs/qualification/scan-first-pressure-20261109/live/vps1-production-readonly-20261109T1006Z.json)
(SHA-256 `876758c55554bc89a693d430d1eeefb48d4222e45ba7a5738e52d44c5c3f2304`)
and the [earlier VPS1 snapshot](docs/qualification/scan-first-pressure-20261109/live/vps1-readonly-20261109T1001Z.json).

### Release-119 canary checkpoint — 2026-11-09 10:06 UTC

The VPS2 release-119 canary has **5 of 289 passing samples**. The fifth
completed in 81.895 seconds; it passed exact accounting, path graph, SQLite,
binary/unit, journal, and health gates. At that sample the full scan was 409
seconds old and not running; overflows and userspace drops remained zero. The
canary is still far short of 24 hours and remains in progress. Its five-sample
snapshot is
[`release119-canary-progress-20261109T1006Z.jsonl`](docs/qualification/scan-first-pressure-20261109/live/release119-canary-progress-20261109T1006Z.jsonl),
SHA-256 `de6bd413a904af216fd9fce10e47ac5423c0b8ff8b55a37357b125048b6456a2`.

### Release-119 canary sample six — 2026-11-09 10:09 UTC

The VPS2 canary is active with zero restarts and **6/289 passing samples**.
Sample 6 completed in 48.304 seconds with no monitor errors, no kernel
overflows, and no userspace drops. Accounting/path graph and SQLite checks
passed. Its latest completed scan was 675 seconds old; the index remained
dirty and not caught up while waiting for the next periodic observation. This
is a passing health sample, not proof of fresh/continuous exactness or the
24-hour soak. Sample evidence SHA-256:
`792c7125c9a8ba7e08a682ebd1ad82d9ed933f9437ec7ab3029573f2d4375916`.

### Production-shaped schema-6 migration qualification — 2026-11-09

Expanded the v6-to-v7 regression to generate a configurable tree and assert
source and migrated file/directory/path counts, zero coverage issues, schema
version, root path resolution, and preserved growth history. The explicit
1,300,000-file run passed: schema-6 source had 1,300,000 files, 1,301
directories, and 1,301,301 paths; migration produced the same counts with
zero scan coverage issues. Elapsed 3:37.28, peak RSS 722,524 KiB, no swap.
The fixture is synthetic (the v7 path graph was removed and schema metadata
set to 6); this proves scale/layout behavior, not migration of the actual
production DB or its exact contents. Full run output SHA-256:
`deeae8385aff194e8c10e3d6aea733d787e56734ddba5ff9ee40afe548129e74`.

The full release/all-features suite passed **324 tests, 0 failed, 2 ignored**;
strict Clippy, formatting, and diff checks passed. Refreshed LLVM coverage is
**72.24% line, 71.08% region, and 76.02% function**, with no branch totals.
Coverage output SHA-256:
`6347d9650a07d891f59c56dfa70cfb117f73ffa5a43d13a640ad30be50e5125a`.
The release-119 canary is active at **9/289 passing samples**, with zero
restarts. Sample 9 passed its gates with zero kernel overflows/user drops; the
index remained dirty between observations, with a 671-second-old scan.
Samples 7, 8, and 9 took 143.404, 64.097, and 43.492 seconds. The three-sample
snapshot SHA-256 is
`7ac4ff7214149b309936c779b193d02109804166c6d6bba74a56cf2939d4f697`.

### Release-119 sample 10 and completed observation — 2026-11-09 10:34 UTC

Sample 10 passed at 10:29:40 UTC in 61.601 seconds; it reported no monitor
errors, kernel overflows, userspace drops, or Dux restarts. Its snapshot was
taken during a full scan: systemd reported `reloading`, `scan_in_progress` was
true, and the latest completed scan was 988 seconds old. The daemon then
completed the observation at 10:31:38: 1,482,435 files, 176,659 directories,
1,659,094 path instances, zero coverage issues; accounting verification
passed in 54.3 seconds. A fresh read-only status at 10:34 showed the service
active/running, zero restarts/overflows/drops, scan age 369 seconds, and
1,655,612 indexed nodes. It remains dirty/not caught up between observations.
This is a completed scan and accounting check on the canary VM, not a 24-hour
soak or production deployment. Sample SHA-256:
`e6cb7a3881c6eefb906b5b6f6ec8420a12746b6fc0250b70baae04d760f9175a`.

### Production-scale growth-history migration and canary progress — 2026-11-09 11:02 UTC

Read-only SQLite `dbstat` on the live schema-6 database counted **27,184,763
growth rows**. The `growth` table and its two indexes occupy 2,557,689,856
bytes (2.38 GiB), about 86% of the 2,958,880,768-byte database. This explains
why the production DB is much larger than the schema-7 canary DB. VPS2 has
about 10 GiB free; a full in-place migration clone plus staging and WAL could
approach that space budget, so I did not attempt that risky write there.

I tried a consistent SQLite online backup with the production connection
opened read-only and idle I/O priority. Under live writes it made little
forward progress; the backup process recorded 65.5 GB of writes while its
incomplete destination was only about 2.1 GB, consistent with SQLite backup
restarts as the source changed. I interrupted it, removed only the incomplete
test destination/sidecar, and verified the production service remained active
with its restart count unchanged. No production DB or unit was modified. The
attempt reinforces that a stable snapshot requires a quiescent source or a
filesystem snapshot; it is not counted as a passing backup test.

To exercise the same growth-history volume without production names, the
schema migration regression now optionally seeds history rows by cross
joining the synthetic 1,300,000-file index across 21 buckets. The run passed:
**27,327,322 history rows** survived v6-layout to v7 rebuild, with 1,300,000
files, 1,301 directories, 1,301,301 paths, and zero coverage issues. Runtime
7:32.66, peak RSS 1,009,848 KiB, no swap. This is a strong size/shape test,
but not an exact migration of the live database. Output SHA-256:
`66000b7662fb6b5dee4c89a62c5bcdd5416b1c1ae32f894f8eef827cd0c851fc`.

After the test change, the release/all-features suite passed **324 tests, 0
failed, 2 ignored**; strict Clippy, formatting, and diff checks passed.
Refreshed LLVM coverage: **72.22% line, 71.08% region, 75.99% function**;
branch totals are unavailable. Coverage log SHA-256:
`cc4f8d8d9d61c311336934bf9feb967fe85f49918d1a76ff480cc5562feb041d`.

VPS2 release-119 canary samples **12–16 all passed**; latest sample 16 took
139.338 seconds and showed zero overflows/drops with a 1,066-second-old scan.
At 11:02 the daemon remained active with zero restarts; a new observation was
in progress and scan age was 1,192 seconds. The index was dirty between
observations. The 24-hour soak remains open. Sample log SHA-256:
`d481d2b7ea376e6a524b770915890776698257c51cbc0cd054acf249f52220d8`.

### Canary freshness-gate correction — 2026-11-09 11:14 UTC

The original release-119 monitor stopped at sample 17. Its before-snapshot
reported age 1,227 seconds while a full scan was in progress; by the after
snapshot the scan had completed, age was 388 seconds, exact accounting passed,
and there were zero overflows, user-space drops, or Dux restarts. The monitor
treated the two snapshots independently and failed on the older before value.
This was a monitor false negative, not a daemon restart or failed scan.

Fixed `scripts/dux-canary-monitor.py` to waive only that stale-before error
when the before snapshot proves an active scan and the after snapshot proves
that a newer scan completed with healthy status by the end of collection.
An unfinished scan, invalid age/timestamps, or unhealthy after snapshot still
fails. The captured sample 17 re-evaluates with zero errors under the fix.
The monitor tests now cover completion during the sample, an unfinished scan,
and invalid age. Full Python discovery passed **354 tests**; py_compile and
diff checks passed. Monitor SHA-256:
`ea1d4c98ea7bcf8fe17632095af6d5535852359b691439987480b7fedc043cad`.
Test log SHA-256:
`d76183339b5893ba511b9a15a2c7ea52e9e4eb84753080b77c87316fe04e9d2d`.

The original canary remains recorded as failed under its old monitor and is
not counted as a completed soak. The fixed monitor began a fresh run at
11:13:18 UTC with a new evidence file. Sample 1 passed in 60.935 seconds;
scan age was 968 seconds with a scan in progress, zero overflows/drops, and
zero Dux restarts. Collector unit is active and limited to 20% CPU / 1 GiB.
This new soak is only beginning. Sample-17 corrected evaluation SHA-256:
`61a5616fec7f4c2d42ee1d947a630eafa1ef5781a94666af8a68703894304317`.
New canary sample SHA-256:
`cf746cabe9873feede7f7769184c173548de51d369ebf35300ba954137a23739`.

The latest corrected-monitor run has **2/289 passing samples** as of 11:20
UTC. Sample 2 passed in 99.598 seconds; scan age was 307 seconds before and
407 seconds after, with zero overflow/drop counters. The canary monitor and
Dux remain active with zero restarts. This is only a few minutes of the new
24-hour run. Sample 2 SHA-256:
`2a6334216f7445649b304bea4f3402914fd3ce3c2f508fb76cb3a7f46ad257c2`.

### Follow-up validation — 2026-11-09 15:49 UTC

A further source review replaced two manual descriptor closes in
`src/watch.rs` with the existing `RawFdGuard` and added SAFETY comments for
nearby FID syscalls, FFI output buffers, and descriptor ownership. The latest
release/all-features run included the ignored ext4 and scale profiles and
passed **330/330** in 32.54 seconds. The 100,000-entry smoke reported 4.406 s
baseline, 3.898 s unchanged reconciliation, and an unchanged 32,505,856-byte
database. Strict Clippy (`--all-targets --all-features -- -D warnings`),
formatting, and `git diff --check` passed. Full output is in the r121
qualification folder. Coverage has not yet been regenerated for these latest
source edits; the last measured coverage remains 72.30% line, 71.15% region,
and 76.09% function, with branch coverage unavailable.

A read-only VPS2 refresh at 15:48:09 UTC found **21 passing samples** in the
still-active 24-hour canary. Latest scan age was 6 seconds; errors, overflows,
drops, OOM events, and service restarts remained zero. Free disk was 7.2 GiB.
This does not satisfy the 24-hour gate. Production VPS1 remains unchanged.


### Expanded local validation — 2026-11-09 16:00 UTC

Additional source-matched evidence: all-features debug tests including ignored
profiles passed **330/330** in 42.05s; full Python harness discovery passed
**354/354** in 9.861s. The exact current release source passed a 1,370,000-entry
scale smoke with a 63.017s baseline and 74.441s unchanged in-place observation
(441,102,336-byte DB unchanged). This is approximately 18.1% slower than the
baseline in one unconstrained local run; constrained-host p95/p99 and variance
remain open. Its generated fixture was removed; local free space is 57 GiB.
The 10M real-file profile is not run due to scratch-space consumption risk.

VPS2's canary advanced to **23 passing samples** by 15:58:04 UTC; latest scan
age was 600s, with zero errors, overflows, drops, OOMs, or restarts. systemd
reported `reloading` while scan-first observation was active; Dux remained
running under the same PID, and the monitor accepted this only for an active
scan. The 24-hour gate remains open. Details and logs are in the r121
qualification folder.

### Test isolation disclosure — 2026-11-09 16:05 UTC

The 1.37M scale fixture was mistakenly executed on the checkout host, where a
legacy `dux daemon /` runs and observes filesystem activity. After the fixture
was removed, read-only status showed the same daemon PID (1073891), four
lifetime restarts, dirty state due to fanotify overflow, 617 lifetime kernel
overflows, and 2,518,862 userspace-dropped updates. Journald had 27 overflow
warnings since 15:50 UTC, overlapping the test. No exact pre-run counter
snapshot exists, so causation or the number attributable to the test cannot be
established. The service was not restarted or reconfigured; no database repair
was performed. Local free space was 55 GiB in the captured status.

Accordingly, the 1.37M timing is retained only as **non-isolated diagnostic
evidence**, not a clean performance qualification; the whole-test `perf`
profile is likewise contaminated by host fanotify work. Future stress profiles
must use an isolated test environment. The post-run status and journal evidence
are in the r121 qualification folder under
`local-host-test-impact-20261109/`.


### VPS2 canary checkpoint — 2026-11-09 16:03 UTC

Sample 24 passed at 16:03:17 UTC at 914s scan age. No errors, kernel
overflows, userspace drops, OOM events, or service restarts were reported;
Dux was active under the existing PID and VPS2 had 7.1 GiB free. The 24-hour
gate remains open. The current JSONL is in the r121 qualification folder.


### Scale-test harness safety guard and isolated verification — 2026-11-09 16:19 UTC

The ignored Rust scale test previously created large fixtures in the default
`/tmp`, which can be watched by a live Dux daemon. Added a test-only guard:
when `/run/dux/heartbeat` exists, the scale fixture now refuses real
filesystems and requires `DUX_RECONCILE_SCALE_TMPDIR` to select a pseudo
filesystem such as `/dev/shm`. The target and scratch are resolved before any
large fixture is created. Focused guard test passed. The 100,000-entry scale
fixture passed on `/dev/shm` (3.204s baseline / 3.244s unchanged reconciliation;
DB size unchanged at 30,117,888 bytes).

The complete all-features Rust suite, including both normally ignored profiles,
then passed **331/331** in 40.71s in a private mount namespace with an ext4
loopback fixture backed by tmpfs; scale scratch was separately directed to
`/dev/shm`. This isolates the ext4 stale-parent integration from the host
filesystem. Strict Clippy, formatting, and diff checks passed. A preceding full
suite attempt using tmpfs for every test fixture had one unrelated v6 migration
test failure because that test expects real-filesystem scanning; the isolated
ext4 rerun passed it. Logs are recorded in this folder.

The VPS2 canary reached **27 passing samples** at 16:18:45 UTC, latest scan age
874s, zero monitor errors/overflows/drops/OOMs/restarts. The service is active
with zero restarts and 7.0 GiB free. The 24-hour soak remains incomplete.

### Isolated 1.37M constrained ext4 profile and canary checkpoint — 2026-11-09 16:40 UTC

The 1.37M scale test passed in 453.19s inside a private mount namespace on an
8 GiB ext4 loop image backed by tmpfs, under 50% CPU, 12 GiB memory max, no
swap, `nice 19`, and idle I/O priority. Baseline scan was 186.730s; unchanged
reconciliation was 141.158s; DB stayed 425,861,120 bytes. Last-observed cgroup
OOM counters were zero; current memory observed stayed below 10.8 GiB. This is
controlled memory-backed ext4 diagnostic evidence, not real block-device or
p95/p99 qualification. The earlier scale attempt using mkfs's default inode
density hit ENOSPC while creating files, before Dux scanned; 1.8M inodes fixed
the fixture, and the full scan profile passed. Logs and the setup failure are
recorded in the r121 qualification folder.

The local production daemon remained active under the same PID with four
restarts; its overflow/drop counters remained 636 / 2,518,862 across the
isolated run. `/dev/shm` returned to 1.8 GiB used; the root volume showed
63 GiB free. VPS2 canary sample 31 passed at 16:37:57 UTC with scan age 135s
and zero errors, overflows, drops, OOMs, or restarts; the service was active
with 7.0 GiB free. The 24-hour soak remains in progress.


Canary interpretation: the latest VPS2 sample (sample 31, 2026-11-09 16:37:57
UTC) has zero monitor errors, overflows, drops, OOMs, and restarts, but status
reports `dirty=true`, `caught_up=false`, and 9,510.948 seconds behind. It is a
passing telemetry sample, not a healthy/current-index qualification. The
24-hour soak and freshness gate remain open.


### VPS2 post-scan read-only checkpoint — 2026-11-09 16:49 UTC

The 16:44:26 periodic observation completed with `reconciliation accounting
verification passed` in 54.085 seconds at 16:48:52. At 16:49:40, systemd
showed the same active PID 2561224, zero restarts, and 203,792,384 bytes
current memory. Dux reported zero kernel overflows, userspace drops, and
pending updates; its most recent scan was 103 seconds old. Status remains
`dirty=true` / `caught_up=false` pending the next full observation;
`behind_seconds` (10,214.579) is distinct from scan age in this scan-first
mode. This is a single successful scan/accounting cycle, not soak completion.
VPS2 root disk was 93% used with 6.9 GiB available; nothing was deleted. Raw
evidence: `docs/qualification/scan-first-timestamp-fix-r121-20261109/vps2-postscan-20261109T1649Z.json`.


### VPS2 disk inventory — 2026-11-09 16:52 UTC

Read-only inventory: root 99.0 GB (92.2 GiB) total, 87.1 GB (81.1 GiB)
used, 7.34 GB (6.84 GiB) available (93%); inode use 29%. `/var` is about
61.3 GiB: Docker 22.7 GiB, containerd 18.9 GiB, `/var/tmp` 5.2 GiB,
`/var/log` 4.3 GiB, and
`/var/lib/dux` 0.79 GiB. Journald reports 3.9 GiB. Docker reports 6.4 GB
reclaimable images, 1.96 GB reclaimable stopped containers, and 367 MB
reclaimable volumes; 20 containers and 10 volumes are active. Logical Docker
size/reclaimability overlaps stored image layers and should not be added to `du`
totals. No files, Docker objects, or logs were removed because `/var/tmp`
contains qualification artifacts and the host runs active services. Snapshot:
`docs/qualification/scan-first-timestamp-fix-r121-20261109/vps2-disk-inventory-20261109T1652Z.json`.


### Scan-first observability correctness fix — 2026-11-09

Fixed Prometheus reporting that could label a scan-first daemon caught up when
its queues were empty. `dux_index_caught_up` now excludes scan-first mode. Added
scan-first mode, scan-age/availability, and behind-seconds applicability
metrics; omitted event-apply backlog timing in scan-first mode. JSON status adds
`behind_seconds_applicable`; operations guidance distinguishes scan-first
freshness from event-apply backlog. The all-features debug suite passed 330/330
non-ignored tests; optimized release suite passed 332/332 including both ignored
scale and ext4 stale-parent tests. Focused status/metrics tests, strict Clippy,
formatting, diff check, release binary build and CLI smoke passed. Current LLVM
coverage: 73.67% lines, 72.57% regions, 76.76% functions; branch counters were
not collected and `main.rs` line coverage is 46.43%. Clean-commit release executable `b07fc635b6ce` built successfully (5,354,616
bytes; SHA-256 `f23b4a07066f8ccd1a6ce470a2038e811db0e9cda35ba8cc4a570e967f7458f6`)
and CLI smoke passed. It is not installed on VPS2; package/lifecycle and
runtime-canary verification remain open. Evidence is in `docs/qualification/scan-first-timestamp-fix-r121-20261109/`.


### VPS2 scan-first runtime checkpoint — 2026-11-09 17:07 UTC

At 17:07:19 UTC, VPS2 Dux was active under PID 2561224 with zero restarts,
overflows, userspace drops, and pending updates. The last scan was 233 seconds
old; a scan completed at 17:04:17 with independent accounting verification
passed in 49.529 seconds. Status remained dirty pending its next scheduled
observation and `caught_up=false` as scan-first requires. The legacy
`behind_seconds` value is not an applicable scan-first freshness measure. Root
disk was 93% full with 7.29 GB available. This remains a short runtime
checkpoint, not soak completion. Current local metrics fix is not deployed.


### VPS2 runtime checkpoint — 2026-11-09 17:43 UTC

Read-only status: service active under PID 2561224, zero restarts, 110 MB
current memory. The 17:14 and 17:29 scheduled observations passed accounting
verification in 103.467s and 71.042s. At 17:43:57, last scan age was 552s,
with no kernel overflow, userspace drops, or pending updates. It remains dirty
waiting for the next scan; `caught_up=false` and `behind_seconds` 13,472.1s is
not a scan-first freshness measure. Root disk remains 93% used with 7.23 GB
available. The local `b07fc63` fix has not been installed on VPS2. Raw record:
`docs/qualification/scan-first-timestamp-fix-r121-20261109/vps2-runtime-20261109T1743Z.json`.
