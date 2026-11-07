# Production readiness status

**Updated:** 2026-11-07 18:20 UTC
**Goal:** production reliability, security, performance, and correctness review
**State:** active; not production-ready sign-off

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
latency improvement has yet been measured; the prior VPS2 report predates this
source change.

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

The current source test set passed **294 tests** (2 ignored); the readiness
Python harness passed **36 tests**; strict all-target/all-feature Clippy,
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
