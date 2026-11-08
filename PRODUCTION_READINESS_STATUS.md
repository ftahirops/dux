# Production readiness status

**Updated:** 2026-11-08 14:00 UTC
**Goal:** production reliability, security, performance, and correctness review
**State:** active; not production-ready sign-off

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

Automated tests on the current Rust source passed **310 tests, 0 failed, 2
ignored** (`cargo test --locked --all-features`); the ignored cases require
the dedicated ext4 or manual scale profile. The complete Python script suite
passed **323 tests**, including **27 focused canary-monitor tests**. Strict
all-target/all-feature Clippy, formatting, and `git diff --check` passed. The
lockfile audit found no known vulnerabilities in 229 dependencies; the static
binary contains auditable metadata for 202 dependencies. Audit limitations and
the advisory database date are documented above. These are automated code and
artifact checks, not proofs of universal runtime correctness.
The Python command was rerun on this worktree; its result is preserved in
`docs/qualification/scan-first-fixed-cadence-20261108/python-suite-final-20261108.log`.

`cargo llvm-cov --locked --all-features --summary-only` measured **71.29% Rust
line coverage overall**, 70.28% region coverage, and 75.08% function coverage.
Key line coverage: scan 92.34%, store 96.10%, query 90.56%, deleted-open
handling 90.57%, JSON emitters 85.08%, watch 72.07%, main/CLI 43.20%, and TUI
39.51%. `fault.rs` remains at 32.65%. Branch coverage was not emitted by this
run. The report is preserved at
`docs/qualification/scan-first-fixed-cadence-20261108/llvm-cov-summary.log`.
Coverage measures executed lines, not whether every behavior was asserted.

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
tests are included in the 323 Python tests); do not add them as unique tests.

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
