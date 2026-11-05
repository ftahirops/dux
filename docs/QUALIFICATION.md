# Release qualification operations

## Active VM resource policy — 2026-10-27

Qualification guests must use 2–4 GiB RAM. The VM manager exposes a 2 GiB
`standard` profile and a 4 GiB `qualification4` profile; it refuses to start
them unless host `MemAvailable` is at least 6 GiB and 12 GiB respectively.
Their QEMU cgroups are capped at 2.5 GiB / 75% CPU and 6 GiB / 50% CPU. These
cgroup limits include QEMU overhead; guest RAM stays at the selected 2 or 4
GiB. Host admission also requires load1<=10, I/O PSI some avg10<=10%, and
memory PSI some avg10<=5%. CPU PSI some avg10 up to 35% is admitted only when
aggregate load1<=10 and CPU PSI full avg10<=5%; PSI some above 55% always
blocks admission. This distinguishes quota-related runnable wait from broad
host saturation while retaining hard load, memory, and I/O gates. During a
run, two consecutive samples above CPU PSI some 40% stop the guest when
load1>10 or CPU PSI full>5%; CPU PSI some above 70%, CPU PSI full above 10%,
load1>12, I/O PSI above 40%, or memory PSI above 20% always stop only the
disposable guest. For P0 on a busy shared host,
override the qualification4 CPU quota to 25%; keep guest RAM at 4 GiB and its
6 GiB cgroup memory ceiling. Do not use historical measurements from larger
guests as release qualification. Run the complete 100k/1m/10m suite within the
4 GiB guest and its explicit worker limits, even when that takes longer.

The `watch-unit` guest name provisions a fresh Ubuntu 24.04 instance for Rust
watcher regressions. Its per-run qcow2 overlay, cloud-init seed, SSH identity,
and console log live in a unique `/dev/shm/dux-watch-unit-vm/<run-id>` directory,
outside the host's watched workspace. Use the `standard` 2 GiB profile; the
guest's ext4 `/tmp` preserves allocation-sensitive test semantics. The guest
state manifest remains in `target/readiness/vms/watch-unit/state.json` and
records the exact per-run tmpfs directory. Treat tmpfs guest state as ephemeral;
persist test logs and exact binaries separately before stopping the guest.

Keep worker limits separate from the VM limits. The consolidated P0 worker is
capped at 4 GiB, has swap disabled, and uses a 25–75% CPU quota. The calibrated
scale worker uses a 3 GiB cgroup, one scan job, and the measured profile's
explicit CPU quota. Never raise the guest RAM or its QEMU cgroup limit to make
a test pass.

The scale fixture defaults to isolated tmpfs. The 10m image requires 12 GiB of
workspace headroom, which exceeds the 4 GiB guest's tmpfs budget. In the
dedicated KVM guest only, `--disk-workspace` permits an ext4 fixture under
`/mnt/dux-readiness`; it refuses this mode outside KVM or elsewhere on disk.
The scan process remains independently measured and must meet the 256 MiB RSS
budget. The guest's isolated virtual disk holds the large fixture while guest
RAM remains at 4 GiB.
Fixture creation syncs and advises completed image pages away every 100,000
paths, then drops page, dentry, and inode caches in the disposable guest. The
scale memory cap therefore measures live working set instead of retaining the
large fixture as guest cache. This cache drop is never enabled for ordinary
tmpfs runs or outside the dedicated KVM guest.

Run scale jobs through `scripts/dux-scale-monitor.py` from the host. It polls
the guest's atomically written report and heartbeat over loopback SSH and
persists each complete snapshot beside the host evidence, so a safety stop or
guest failure does not erase the last measured phase. Preserve the host `.json`,
`.progress.json`, `.monitor.json`, and `.log` together. Example, after copying
the candidate and harness into the guest:

```sh
python3 scripts/dux-scale-monitor.py \
  --guest ubuntu24 \
  --guest-report /root/dux-scale-current.json \
  --host-report target/readiness/scale-current.json \
  --interval 15 -- \
  python3 /root/dux-scale.py --private-worker \
    --binary /root/dux-candidate --report /root/dux-scale-current.json \
    --sizes 10000000 --samples 10 --scan-jobs 1 \
    --worker-memory-mib 3072 --cpu-quota-percent 100 \
    --workspace-parent /mnt/dux-readiness --disk-workspace
```

The wrapper does not make an interrupted run pass: only the guest's terminal
report and all budget assertions can qualify a level. A missing final fetch,
stale heartbeat, nonzero remote exit, or lost VM is recorded as incomplete and
keeps the gate open.

## Current candidate checkpoint — 2026-10-26 10:38 UTC

### Live continuation — 2026-10-26 10:46 UTC

The 10m scale fixture is generated and the exact candidate scan is running in
the isolated Ubuntu guest on ext4. At 10:46 UTC it had run for 1m55s under the
50% CPU quota; cgroup memory current/peak were 10.15/10.25 GiB with a 12 GiB
live cap and zero pressure/OOM events. Its report remains at the fixture phase
until the scan exits, so no 10m budget is passed yet.

The installed legacy daemon reproduced the accounting failure again at
10:44:58 UTC and started a whole-root reconciliation. At 10:46:07 it showed
2,670 pending paths, DIRTY state, 78 seconds since catch-up and zero dropped
events while scanning 1,121,148 files. The read-only candidate exact observer
is tracking this recovery but has not obtained a stable caught-up window or
completed an exact filesystem comparison. Live accounting remains a release
blocker.

That observer reached its original 600-second deadline while the daemon was
still reconciling; it recorded `STABLE_CATCHUP_TIMEOUT` with 2,964 pending paths,
zero dropped events, and the scan active. The report and progress sidecar are
preserved in `target/readiness/live-accounting-candidate-stable-timeout-20261026T1047Z.json`
and its `.progress.json` companion. A follow-up uses the same 15-second stable
gate with a 1,800-second timeout; the longer wait does not relax the exactness
criteria.

### Scale follow-up — 2026-10-26 10:59 UTC

The first full 100k/1m/10m run is terminal **failed** at the 10m RSS gate.
The 10m scan took 673.431s and passed its reported exact aggregate and sample
checks, while the query and TUI p95s shown in the report met their limits. Peak
RSS was 909,528 KiB against the 256 MiB budget, so the level does not qualify.
The complete report is `target/readiness/scale-ceba3c3-100k-1m-10m-20261026.json`
(SHA-256 `431c03941b2eca12b66c520388be5dcee555d4ea055a20ba3f5760fc44073cf3`).
A one-job RSS-profile rescan is running over the same preserved ext4 fixture to
identify whether walker parallelism is the dominant memory cost. It is
diagnostic evidence only; the passing scale qualification still requires all
budgets, including memory, at the intended four-job configuration.

The 11:00:58 live capture localized the recurring failure before the atomic
rebuild replaced the old index: `/tmp` recursive totals exceeded its prime
children by 573 inodes and 15,183,872 bytes, while
`/root/.codex/packages/app-server-daemon/auto-update-version` was short by
12,288 bytes. The root aggregate was therefore high by 573 inodes and
15,171,584 bytes. No dangling child, invalid parent or bad leaf predicate was
present; exactly two directory rollups and the root aggregate failed. Captured
predicate report SHA-256 is
`1404fe74d93bd4c7728a3f8b66208de0665d35fefbe584bcababc8307b97e24a`; directory
detail SHA-256 is
`88959c1dde3679b69b6ee8b983c68ba5dd01bed888fff98b576c01c9eb1092c7`. The
subsequent full scan restored caught-up status, but this does not identify the
event sequence or prove a candidate fix. Exact comparison against the live
filesystem remains open.

The candidate binary pair remains production SHA-256
`ceba3c3c2b1d830ef7d7869dfac2a1376ddd991fe4701313cb6aab07c9aa5a93` and
fault-injection SHA-256
`5fe81f81c6585b99dd57f980a156be29a4e52922942cbc3a98b995e16388b5aa`. The
readiness harness now has SHA-256
`86d81df3cf84c35e10d408fb3a3c08d07876efbdfd09d746b3216c8de42d1226`.

A full P0 matrix using the prior harness passed its first nine suites but timed
out the pressure-recovery check at 180 seconds. The 100k-file reconciliation
finished about two seconds after that deadline, before the test could verify
exact recovery. Raising that suite deadline to 600 seconds yielded an isolated
pressure pass: cancellation preserved the old generation, Elevated pressure
continued incremental commits, Critical pressure retained and replayed events,
and exact recovery ended with zero pending/dropped events and intact growth
history. Report SHA-256 is
`b1b1721a9eb0dfc54afe937f7cfe33e861568fbfa58a1ac91d3c5040b74aa595`.

A separate four-suite run passed mount recovery, long-reader WAL reclamation,
watch-first startup plus v4 upgrade, and root scan (69,630 files, 9,249
directories, 85,394 dirents, complete coverage). Final report SHA-256 is
`425a59969f429669241e38100443af334c20340bc3aaa9057fdfbfdc571fffe6`.

A consolidated 14-suite P0 run completed crash (30/30), overflow, and CPU,
then was OOM-killed during the churn suite's second 100k-file baseline scan.
The worker cgroup was capped at 768 MiB and systemd recorded a 768 MiB peak;
the scan progress marker shows 31,013 files / 73 directories / 130,199,552
allocated bytes at interruption, before churn assertions. This is a
qualification-worker resource failure, not evidence of a candidate accounting
defect. Preserved failure artifacts are under
`target/readiness/p0-churn-oom-failure-20261026/`.

The last persisted interim report SHA-256 is
`9271dec2673fdf761b7696ef04cda7a090c6995a3c0a055a437ceaf824fd3c64`.
Crash passed in 1,847.137s; overflow passed in 1,595.101s (1,557.262s
recovery); CPU passed with 12,142 events, 5.706s production, 39.514s catch-up,
107.198s exact validation, and 0.209 CPU fraction. The CPU suite recovered
from an Elevated-pressure scan cancellation while retaining the previous
index.

The P0 qualification has pass evidence for all 13 defined suites across two
reports. The first run passed ten suites but failed at mount recovery because
pressure-suite cleanup leaked a daemon holding the database flock. Failure report
`target/readiness/p0-full-4g-20261026-mount-failure.json`, SHA-256
`8ef611ad4a3d3cbea209c31ab5275a9ea8d829afbb09ecda4ee606b2c5eb6b6e`. Fixed
`pressure_reconciliation_test` to stop its daemon in `finally`; updated harness
SHA-256 `e6b28346f741873066f4e59473538f62572299deb5e91d95cdc74971118f4861`.
`py_compile`, readiness workflow tests (9/9), and `git diff --check` pass. The
focused rerun passed pressure, mount, WAL, root, and startup in 10m 1.920s.
Report `target/readiness/p0-tail-cleanup-20261026.json`, SHA-256
`53db21e6f61bd454d3a6fb838875f3d6a637b7b047974c6012b77c589daa515f`. All 13
suites have pass evidence for the same candidate/fault binaries, but not as one
consolidated report.

The scale harness now lets the run specify and records worker MemoryMax and
CPUQuota instead of hard-coding 768 MiB. Workflow tests pass 10/10. Current
candidate scale run is active as `dux-scale-51395.service` in the Ubuntu 24.04
scale16 guest: binary SHA-256
`ceba3c3c2b1d830ef7d7869dfac2a1376ddd991fe4701313cb6aab07c9aa5a93`, harness
SHA-256 `3db4139a81a875c7b28d53c1d7f56cb46bf39016ef9755a2bb19c95a4e381a35`,
10,240 MiB worker cap, 50% CPU quota, four scan jobs. The 100k and 1m levels
passed scan, exact, query and TUI budgets. At 1m, scan took 42.960s / 144,080
KiB peak RSS; exact state contained 991,964 inodes, 56,188,928 allocated bytes
and 1,001,964 entries. The 10m fixture reached 3.5m/10m at 10:36:46 UTC; scan
and exact/query/TUI checks remain pending. Progress report
`target/readiness/scale-ceba3c3-100k-1m-10m-20261026-progress.json`.

The installed legacy daemon remains unchanged and still rebuilds after accounting
invariant failures logged at 10:29:53 and 10:34:54 UTC. At 10:36:19 it remained
in `reload-notify` with 1,120,558 files scanned, 1,424 pending, DIRTY and zero
dropped events. A read-only exact audit is waiting for 15 stable caught-up
seconds. Report target
`/run/dux/live-accounting-stable-exact-20261026T1035Z.json`; progress sidecar
`/run/dux-live-observer-live-accounting-stable-exact-20261026T1035Z-1544020.progress.json`.
No stable exact result is available yet. This reproduces the stale-data issue
and keeps the live accounting gate open.

Package lifecycle/security/rollback, 72-hour endurance, and 24-hour canary also
remain open. Production readiness is not qualified.

### Updated measured evidence — 2026-10-26 11:18 UTC

The four-job 10m profile completed in 673.431s but peaked at 909,528 KiB, so it
failed the 256 MiB RSS budget. A diagnostic rescan of the preserved ext4 fixture
with one walker job passed in 18m13.86s and peaked at 170,348 KiB (166.4 MiB),
with zero swaps. The production daemon's background rebuild uses the
low-priority worker count, which resolves to one job on the four-vCPU
qualification guest. The diagnostic is not a complete scale pass because it did
not rerun all level-specific exactness, query and TUI gates together. Run the
full `dux-scale.py` profile with `--scan-jobs 1`, preserve its report, and
require every budget to pass before closing scale qualification.

The legacy live daemon failed the accounting invariant again at 11:15:06 UTC.
The captured rows all resolve to existing `/tmp/xgs-staticcheck-cache` paths;
two non-directory inode rows had nonzero allocation but zero cached recursive
totals. Reconciliation restored the internal SQL invariants by 11:18 UTC, then
the index changed during the capture window, so path-level equivalence remains
unproven. A candidate unit regression now injects that zero-total leaf shape
and passes. The candidate remains uninstalled.

A corrected consolidated 14-suite P0 run is active in the isolated Ubuntu
guest as `dux-readiness-full-e6-20261026.service`. It uses the corrected
pressure-suite cleanup harness and the existing production/fault binary hashes.
Wait for the final guest report before treating the matrix as passed. The same
guest completed the one-job memory diagnostic before the P0 rerun began; no
scale rerun is running concurrently with P0.

At 11:20:45 UTC the installed legacy watcher reported fanotify queue overflow
and started another reconciliation. At 11:22 UTC it was still dirty, scanning,
about 100 seconds behind, with 12,257 pending paths. The concurrent exact audit
walk finished with one filesystem walk error and differences, but its starting
index changed during the audit and the daemon was rebuilding. Preserve those
counts as unstable evidence only; do not label them persistent drift or a pass.
The 15-second stable-catch-up observer timed out because the service remained
in reconciliation. A new live exact comparison can be meaningful only after
the queue drains, dirty state clears and a stable window is obtained.

The next post-reconciliation check at 11:24 UTC found a false-clean interval:
the candidate-readable status said caught-up with zero pending/dropped events,
but read-only SQL found the root rollup 59,375,616 bytes below inode totals and
five bad directory rollups. The legacy v4 index resolves 28 existing regular
files under `/tmp/xgs-staticcheck-cache` as directories with matching
device/inode identities; that schema has no `birth_ns` field. The candidate
already detects kind/incarnation changes in `upsert_path`, and its focused
stale-directory-reuse regression passes. At 11:25:08 the legacy periodic
verifier marked the index dirty and began another rebuild. This explains why a
simple caught-up status can disagree with the indexed tree; the candidate fix
still needs the consolidated high-churn qualification and deployment gates.

### Current-source release artifact qualification — 2026-10-26 11:35 UTC

Built `0.6.3-14` static-pie musl binary, DEB and RPM under
`target/readiness/release-20261026/`. Their shared payload binary SHA-256 is
`65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e6`; the
manifest binds source tree SHA-256
`ff2f3a942ffe51d255145516ceb6184bc19ddc6c474b6fdf0f828e72f49743a8` and
records the worktree as dirty. Verify those hashes before using the artifacts.

The DEB lifecycle is running in a newly provisioned, controller-marked Ubuntu
24.04 guest against `0.6.3-12` → `0.6.3-14`. Its interim record confirms the
static v14 payload is installed after upgrade and that an operator-disabled
service stays disabled. Rollback, remove/purge, clean install, default tunnel
state and effective security policy remain pending until the final lifecycle
report says `passed`. The first attempt against an older guest was rejected at
the marker precondition before package mutation; a fresh guest alias was added
and its workflow tests pass 11/11.

The consolidated P0 run is at crash round 14/30 on dynamic candidate SHA
`ceba3c3c2b1d830ef7d7869dfac2a1376ddd991fe4701313cb6aab07c9aa5a93`. Since the
release package contains static musl SHA `65c635c4…`, its P0 and complete scale
gates must also be run against that exact packaged binary before release.
Latest live observation at 06:16 UTC: installed legacy PID 900 has three more
`materialized index totals disagree with inode rows` triggers (06:03:57,
06:08:57, 06:13:59). The first two zero-error scans each replayed 29k/42k paths
and later reported caught-up; a third root reconciliation was still active at
the checkpoint with 40,639 pending paths. No exact live filesystem comparison
was run during this active reconciliation. This is recurring production
evidence, not a candidate qualification result.

At 06:19:01 UTC the invariant failed again. A read-only SQLite snapshot during
that rebuild found the root inode 11,501,568 bytes / 2,232 inodes above the
inode-row aggregates, four bad directory rollups, one bad leaf rollup, and no
dangling dirents. The stale rows form `/tmp/go-build3697154896` and its
`b001`/`b142` descendants, all absent on disk; the root of that subtree has a
stale recursive total of 151,605,248 bytes / 293 inodes versus 162,304,000
bytes / 1,107 prime-child inodes. Read-only query artifact:
`/run/dux/live-rollup-mismatch-20261026T0620Z.json`, SHA-256
`2666050b14987ee99777a1803aba5e8df6372be495d5f09ac92827a072aa111b`. This
was captured during an active dirty rebuild, so it reproduces the mismatch
shape but is not a stable post-recovery exact audit. Candidate Rust regression
`deleting_live_sized_stale_build_tree_restores_exact_rollups` models this
deleted Go-build subtree; candidate source tests passed earlier, but the
installed legacy service has not been changed.

A subsequent read-only exact audit could not start its filesystem walk because
the required 15-second stable caught-up window did not occur within 180 seconds:
the indexed allocated-byte total continued to change despite zero pending
events and a caught-up status. Report
`target/readiness/live-exact-recovery-20261026T0621Z.json`, SHA-256
`52e3c71b100f2d3c1801b2050f0ae56fd79cc64e91962265f99afaba748b2194`, records
`STABLE_CATCHUP_TIMEOUT`; it is not an exact pass or comparison result.

The 06:19 rebuild did complete. A read-only SQLite snapshot at 06:26:12 UTC
then found internal accounting totals consistent and the previously stale
Go-build inode removed. Report
`target/readiness/live-rollup-after-reconciliation-20261026T0625Z.json`,
SHA-256 `0983fadf2ac84da0ba795b6abc1c3ba78598cc9616ac575f6bbdd6d1e0c814eb`.
This confirms the rebuild repairs that snapshot; the recurrent cause and an
exact stable filesystem comparison remain unverified.

A single low-priority whole-root snapshot completed from 06:27:57 to 06:31:14
UTC but overlapped active `/tmp` churn. The indexed structure changed during
the audit, and the filesystem walker saw two `ENOENT` races on temporary test
paths. It observed 1,217,077 filesystem paths versus 1,195,595 indexed paths,
22,287 missing paths, 805 stale paths, one identity mismatch, and 93 allocation
mismatches. Report
`target/readiness/live-exact-single-snapshot-20261026T0627Z.json`, SHA-256
`1f6180bc75b772ede0e343cb7ed13bfc096124bd8f59572457d8b502c3675de4`, records
the full samples and `CATCHUP_TIMEOUT`. These are transient diagnostic counts,
not a stable exactness verdict.

The consolidated P0 attempt is terminal and failed in CPU-suite cleanup after
1h02m19s. Preserved report `target/readiness/p0-full-final-failure-20261026/report.json`
SHA-256 `41c34d65cda7e1a7b79fc270fd6a29e398c8e740157583ff0d3c193bb1408045`
records crash 30/30 and real overflow as passed; overflow took 1,376.9s. CPU
production/catch-up/primary exactness met their budgets, but cleanup had a
60-second deadline while one full exact snapshot itself took 69.992s. The
preserved frozen CPU-cleanup index and filesystem truth compare with zero path,
inode/allocation, rollup, orphan and prime-attribution mismatches; SQLite
integrity is `ok`. Details and hashes are in
`target/readiness/p0-full-final-failure-20261026/exactness-verification.json`.

The harness cleanup bound is now 180s and the complete matrix wall limit is
four hours. New local/guest harness SHA-256 is
`dd5700981a7a085381098a86a4db9a647b032f0c1c35b226aee24ec7dad3a3c9`.
Compilation, whitespace validation and workflow checks pass (9/9). A first
CPU-only rerun passed on the candidate but provenance showed the guest still
had the old harness; it does not verify the timeout fix. A second run with the
corrected, hash-verified harness is active as `dux-readiness-29417.service`,
outer session 99351. The full P0 rerun awaits this focused result. Live exact
accounting, calibrated scale, package/security/rollback, 72-hour endurance and
24-hour canary remain open.

At 06:33:59 UTC the installed legacy daemon logged
`cannot resolve ancestor parent`, then reported a materialized-total invariant
failure and started another root reconciliation. The zero-error scan completed
at 06:36:34 with 1,104,049 files / 104,791 directories / 118.19 GiB, but
replayed 71,031 pending paths and immediately reported a persistent backlog.
The candidate source's `ensure_ancestors` now captures parent identities during
the shallow-first walk and installs from those captured identities to avoid a
concurrent-rename re-stat race. This aligns with the legacy error path, but the
active candidate run has not yet completed and a focused race regression is
still desirable; no live fix is claimed.

## Earlier candidate checkpoint — 2026-10-26 02:42 UTC

The 02:37 exact whole-root comparison completed its filesystem walk (1,199,093
paths, zero traversal errors) but did not pass: the index changed during the
walk, accounting invariants were false, and the transient comparison recorded
18,352 missing paths, 408 stale paths, 6 identity mismatches, and 1,411
allocation mismatches. These counts are not a stable exactness verdict. The
four-minute post-walk wait returned `CATCHUP_TIMEOUT`; no second walk was
started. At timeout the service was again reconciling after the 02:43:19
invariant failure, with 6,535 pending updates and 34.2 seconds behind. Report
SHA-256 is
`0de439946ea12438fed1471968ba8d2f36f6cc4447d26ca98f3bc41e927e5136`. During reconciliation, the installed service logged a further
fanotify overflow at 02:40:22. Its 02:40:52 scan completed and it logged
caught-up at 02:40:59, but candidate-readable status is again dirty with zero
pending/dropped and reason `incomplete coverage or changes lost during
reconciliation`. The exact report is
`target/readiness/live-exact-current-20261026T0237Z.json`; installed service
remains the legacy binary and has not been modified.

## Earlier candidate checkpoint — 2026-10-26 02:36 UTC

The latest Rust source includes periodic watch-status refreshes during long
flush transactions. `cargo fmt --all` and `cargo test` pass (106 Rust tests).
Current production and fault-injection build hashes are respectively
`ceba3c3c2b1d830ef7d7869dfac2a1376ddd991fe4701313cb6aab07c9aa5a93` and
`5fe81f81c6585b99dd57f980a156be29a4e52922942cbc3a98b995e16388b5aa`.

The full P0 run `dux-readiness-136755.service` was stopped after 10/30
crash/recovery rounds, before any suite completed. Partial evidence is
`target/readiness/p0-interrupted-20261026.json`, SHA-256
`e4c67231c27670c4b010bd51ff92c6e66ceddcf5d350587c439f364e0188fdb3`. It used
the previous `/tmp` harness and cannot qualify the corrected harness.

The installed legacy service logged eight materialized-accounting invariant
events at 01:58, 02:03, 02:08, 02:13, 02:18, 02:23, 02:28 and 02:33 UTC, plus
two fanotify overflows at 02:25 and 02:27. Following the first overflow, the
02:26:33 scan saw 1,082,249 files / 103,342 directories / 114.81 GiB, replayed
30,148 paths and caught up at 02:26:54. The second overflow occurred after the
P0 worker stopped and before an exact observer began. After the 02:28:12
invariant failure, candidate-readable status at 02:28:45 reported dirty state
(`directory accounting invariant failed`), scan in progress and 950 pending
paths. At 02:30:21 it was still dirty and scanning, with 9,268 pending, 128.2
seconds behind, and 1,088,632 files / 102,871 directories visited. It briefly
returned clean/caught-up at 02:30:51, then failed the invariant again at
02:33:16. At 02:36:22 it remained dirty with 51,364 pending and 201.7 seconds
behind. These are recurring events, not eight proven distinct root causes.

A read-only exact diagnostic walk was stopped during reconciliation. Partial
report `target/readiness/live-exact-diagnostic-interrupted-20261026T0228Z.json`,
SHA-256 `22886a8d53cfc98630ed9a03025b23dcb5e443446ad82d584ba1afec1447a9ea`,
records 295,368 paths / 31,212 directories / zero walk errors, but no exact
path/allocation comparison completed. A second attempt,
`target/readiness/live-exact-stable-window-20261026T0232Z.json` (SHA-256
`89c683ac4ea61d8b46b8bb7cc7386141ea4e486f9defef1935e551b66203957e`), returned
`STABLE_CATCHUP_TIMEOUT` after 240 seconds. It recorded dirty state, reason
`directory accounting invariant failed`, 51,071 pending, 191.7 seconds behind;
the full filesystem walk never started. Exact live accounting remains open.

Both qualification harnesses require disposable workspaces on a `tmpfs`
filesystem separate from `/`, determined from mountinfo. They record workspace
identity and reject insufficient space instead of falling back to the watched
root. The readiness harness requires image size plus 512 MiB headroom; the
scale harness requires image size plus 4 GiB. Targeted workflow tests pass
7/7. Readiness harness SHA-256 is
`ff18df02ae1815a20d99ea6f3b3af5c5ca7b2801acfe1893b3e14412c66ce7f3`; scale
harness SHA-256 is
`6030cb58ddee2b930471a3ec520932f45ae1895d9fc2e24475e13f19f2766f20`. `/run`
is tmpfs on device 26, separate from root device 64512, with about 3.35 GB
free; that is insufficient for the scale harness's 4.75 GiB minimum even at
100k files. The stopped P0 worker used old harness SHA
`3cfecb63291246bf5b07b11646ea6efb2365d0cbb86eac6e7b325f631828c48e`; its
partial results do not qualify the updated harness.

The VM manager is now constrained to `standard` (2 GiB) and `qualification4`
(4 GiB) guests, with vCPU count, cgroup limit, and minimum host memory
recorded. Run 1m and 10m calibration in `qualification4`; prior scale16/scale32
guidance and results are historical only. VM manager SHA-256 is refreshed with
the resource-policy change in the current audit entry.

The status-refresh change has build and unit evidence but still needs a
dedicated long-transaction test. Current P0, scale budgets, package lifecycle,
security review, 72-hour endurance, and 24-hour canary remain open.

This runbook covers the isolated release qualification guests and their host-side
evidence archiver. It supplements `OPERATIONS.md`; it does not authorize a
production rollout. Qualification uses an exact candidate binary in a
disposable guest, separate from the installed production service.

## Candidate evidence and remaining gates

Current host observation, 2026-10-25 10:16 UTC: the installed legacy service
has repeatedly failed its materialized accounting invariant check (five times
from 09:46 to 10:11), reconciled the root each time, and logged fanotify queue
overflow after the 10:13 scan. Its systemd restart count is zero, but its
accounting state is DIRTY; zero pending/dropped counters do not override that
state. A read-only snapshot report
`target/readiness/live-accounting-repro-20261025T1014Z.json` (SHA-256
`da8a0bf8495ed223bada4f415bc262159db8a9537056e4c2501c0b23422f71dd`) recorded
root totals 135,644,311,552 bytes / 1,144,749 entries versus inode aggregate
135,652,413,440 bytes / 1,144,748 unique inode rows. SQLite integrity passed,
with zero orphan dirents and invalid prime attribution, but six sampled
directory rollups disagreed. The samples were stale entries under `/tmp` from
temporary qualification/test trees that no longer exist. Because the snapshot
was taken during DIRTY state immediately after fanotify overflow, it is a
confirmed live accounting mismatch under churn, not proof of persistent drift
after recovery. Leave the installed service and DB unchanged; wait for its
autonomous recovery, then require a stable caught-up exact path/inode/allocation
audit before deciding whether a product defect remains.

Current checkpoint, 2026-10-25 10:15 UTC: all currently defined P0 suite
categories have individual pass evidence on the same production/fault binary
pair, across several bounded runs; a single uninterrupted full-matrix run has
not passed. The late residual run passed mount, WAL, startup, and root with
current harness hash `3cfecb63291246bf5b07b11646ea6efb2365d0cbb86eac6e7b325f631828c48e`.
It scanned 1,042,647 root files, 104,540 directories, and 1,168,004 entries;
the report is `target/readiness/p0-late-suites-20261025.json`, SHA-256
`f9824b30c9967ff7d2490fdfb620b58bffb1cd13251a8e66e7e42aaee906bef1`. The
pressure suite also passes after two harness defects were fixed. Its report is
`target/readiness/pressure-repro-fixed2-20261025.json`, SHA-256
`37c352984706513b8eacfeb6b4fc901103f3b315f8f649d407907d085a9ea9ab`; it
records Elevated cancellation with the current generation retained,
incremental update under Elevated pressure, Critical write pause and event
replay, exact recovery, and preserved growth history. The cancellation test
now snapshots its baseline generation only after startup reconciliation; the
recovery check now consumes `Daemon.exact()`'s metadata return and reads watch
status separately.

The 100k-file CPU catch-up suite separately passed at 12,181 events, 4.40s
production, 58.49s exact catch-up, daemon CPU fraction 0.15. Crash recovery
passed 30/30 and the fanotify overflow/dirty-metadata failure suite passed in
the earlier P0 run. The combined P0 run before the pressure correction passed
churn, disk, coverage, bounded soak, capability, and cgroup OOM; the rerun
after fixing pressure was interrupted by disk capacity constraints (the root
filesystem reached 100% during qualification). Therefore do not describe this
as a one-run complete P0 pass. Free substantial host space, then perform one
full candidate-bound matrix or retain the individual reports as component
evidence. The exact production candidate pair is SHA-256
`389fc316b9fc523ed8603d85fe89d273c5b02503683830c7c40c578ba6edd062` and
`6337328efdc319b6612d74bbf4bd57c6cf99be45b5d0a6ac20fdb53882649e5c`.
There is no current 72-hour soak or 24-hour canary. The 09:24 source manifest
predates the latest harness changes and does not bind the current harness.

Historical checkpoint, 2026-10-25 09:05 UTC: pressure suite then passed on a
100k-file ext4 fixture after two harness defects were fixed. The cancellation
test now snapshots its baseline generation only after startup reconciliation;
the recovery check now consumes `Daemon.exact()`'s metadata return and reads
watch status separately. Report
`target/readiness/pressure-repro-fixed2-20261025.json`, SHA-256
`37c352984706513b8eacfeb6b4fc901103f3b315f8f649d407907d085a9ea9ab`, records
Elevated cancellation with the current generation retained, incremental update
under Elevated pressure, Critical write pause and event replay, exact recovery,
and preserved growth history. This is an isolated suite pass, not a full P0
matrix pass.

Production candidate SHA-256
`389fc316b9fc523ed8603d85fe89d273c5b02503683830c7c40c578ba6edd062` and
fault-injection candidate SHA-256
`6337328efdc319b6612d74bbf4bd57c6cf99be45b5d0a6ac20fdb53882649e5c` are built
from the current Rust source. The 100k-file crash suite passed 30/30 rounds and
the real fanotify overflow/dirty-metadata failure suite passed. The full P0
matrix stopped at the 10k-event performance case: its 62.306s result included
serial creation of 12,000 fixture files, so it did not isolate catch-up
latency. The harness now records producer and catch-up time separately; the
corrected isolated CPU suite is pending. Report
`target/readiness/p0-cpu-recovery-4b729cb4-20261025.json` preserves the run and
its evidence. No current 72-hour soak or 24-hour canary is active. Older
qualification reports and soak archives belong to different binaries and do
not count toward this candidate's gates.

That qualification statement applies only to the binary and source revision
recorded by those reports. The current worktree has since changed: it adds
critical-pressure scan deferral and cooperative cancellation, a fault-build
pressure override, a pressure cancellation/recovery suite, and a TUI freshness
message correction, and removal of the daemon's artificial `oom_score_adj=800`
override. The package policy now checks both effective unit `OOMScoreAdjust=0`
and the running process's `/proc/<MainPID>/oom_score_adj`; the next package
revision is 14. The earlier reports do **not** qualify these changes. The
current worktree passes 106 Rust tests, 65 lightweight Python harness tests,
formatting, Python bytecode compilation, whitespace validation, and systemd
unit verification. The complete P0 matrix is not qualified: it stopped at the
performance-budget assertion before pressure, churn, storage, permission,
cgroup OOM, capability, mount, WAL, startup, and root suites ran. Rebuild both
release binaries after the harness timing change, rerun the isolated CPU
scenario, then complete the P0 matrix. Keep exact source, harness, and binary
hashes with every report.
The `pressure` scenario verifies cancellation and staging cleanup under
Elevated high-load pressure, continued incremental event commits in that band,
and that a running scan continues under Elevated I/O pressure. It also verifies
write retention under Critical pressure, replay after pressure recedes, and
final exact path/inode/allocation/rollup/history recovery. Injected CPU-only
Elevated pressure remains active while an automatic retry starts and completes
a full reconciliation, proving CPU contention alone cannot starve recovery.

The crash-recovery scenario also qualifies an important CPU-pressure case:
CPU-only Elevated pressure may run a low-priority rebuild and must not
repeatedly cancel and restart it. Other Elevated pressure defers new rebuilds
and cancels active ones; active rebuilds are also cancelled for Critical
pressure.

The packaged service includes systemd restart ramping (10 seconds up to a
one-minute cap) on systemd 254 and newer; older releases ignore the ramp
settings and use the fixed 10-second fallback. Verify the effective restart
policy in the qualification guest and retain its unit hash.

Before starting a dedicated soak, close and record the current release
preconditions: exact live accounting, calibrated 100k/1m/10m scale results,
Debian and Rocky package lifecycle, and final security/default-privilege
review. Start only with the exact production candidate hash from the release
manifest, a separate disposable guest, and a run-specific evidence directory.

Run package install/upgrade/rollback/remove/purge checks only inside a disposable
guest created by `scripts/dux-qualification-vm.py`. The controller provisions
`/etc/dux-qualification-sandbox`; `scripts/dux-package-lifecycle.py` refuses to
run without that root-owned, read-only marker and KVM/QEMU virtualization. VM
overlays created before this marker was added must be recreated from the
controller rather than manually marked. The lifecycle runner removes package
state during its purge phase, so never bypass this guard or point it at a
production host.

The stress and mount-qualification entry points use the shared
`scripts/dux_qualification_sandbox.py` guard and also require this controller
marker before starting loop mounts, transient daemons, scale workloads, or TUI
latency sampling. This applies to `dux-bind-alias-qualification.py`,
`dux-live-test.py`, `dux-live-quota-comparison.py`, `dux-live-quota-worker.py`,
`dux-scale.py`, `dux-scale-query-recheck.py`, and `dux-tui-latency.py`.
The heavier `dux-readiness.py` matrix also runs on the repository's ephemeral
GitHub Actions runner. Its workflow explicitly creates the same root-owned,
read-only marker before qualification; the bounded worker checks that marker
again before creating its fixture. CI environment variables alone never grant
admission. The direct `dux-systemd-status-test.py` scan/replay fixture requires
the marked KVM guest.

## Monitor a dedicated soak

Record the run-specific archive directory and systemd unit in its qualification
manifest. The tracked, parameterized collector is
`scripts/dux-soak-archive.py`. Check the collector and latest checkpoint using
the names recorded in that manifest:

```sh
archive_unit=dux-soak-archive-RUN_ID.service
archive_dir=target/readiness/soak-archive-RUN_ID
sudo systemctl show "$archive_unit" \
  -p ActiveState -p MainPID -p NRestarts -p ExecMainStatus
tail -n 1 "$archive_dir/checkpoints.jsonl" |
  python3 -c 'import json,sys; d=json.load(sys.stdin); print({k:d.get(k) for k in ("observed_at_utc","status","progress_freshness","progress_age_seconds","soak","qualification")}); g=d.get("guest",{}); print("worker:",g.get("worker",{}).get("fields")); print("controller:",g.get("controller",{}).get("fields")); print("memory events:",g.get("memory_events"))'
```

Start the collector with `--expected-candidate-sha256` set to the production
binary hash in the qualification manifest. The soak progress record binds its
candidate binary hash. Archive `status=ok` means collection succeeded; it does
not mean the endurance gate passed. Require `qualification.state=qualified`,
which checks the expected binary hash, at least 259,200 recorded seconds,
scheduled mutation bursts, reader/WAL phases, controlled restarts, exactness
samples, explicit final exact/history/SQLite pass markers, zero unit restarts,
and zero cgroup OOM counters. Reader samples must include an inode snapshot and
successful WAL reclamation; every restart record must bind distinct old/new
PIDs and at least 64 downtime writes; exactness samples must include ordered
operation counts, process RSS/FDs, and convergence time. While progress says
`running`, the worker must be active. Once progress says `passed`, the worker
may have exited normally; require `ActiveState=inactive`, `Result=success`,
`ExecMainStatus=0`, and `NRestarts=0`. Keep the VM and soak controller active
through the final archive checkpoint.
`ActiveState=active` alone is not a health result. Also require archive `status=ok`,
`progress_freshness=fresh`, a recent checkpoint, the appropriate worker state
above, controller active with zero restarts, and zero cgroup `oom`, `oom_kill`,
and `oom_group_kill` counters. The guest updates progress at least once per minute;
the collector marks progress older than 300 seconds stale and rejects material
guest clock skew. A stale or invalid report is a failed observation even when
systemd still shows the guest worker as active.

If the collector fails or its checkpoint goes stale, preserve the last
checkpoint and journal, restore connectivity or collector operation, then
verify a new checkpoint is fresh. The collector can be restarted independently;
do not restart the guest worker to repair an archive-only failure. A gap in
archival evidence must be investigated and documented before the soak can pass.

## Soak gates

The dedicated endurance run is 72 continuous hours on the exact recorded
candidate hash. The runner checks the complete path/inode/allocation state and
growth history every 100 mutations. It also schedules 500-file atomic mutation
bursts each hour, holds a SQLite reader snapshot for five minutes every six
hours starting at hour six, and performs a controlled worker restart every
twelve hours starting at hour twelve while 64 files are changed during
downtime. Each reader phase must prove snapshot
stability and reclaim the WAL after release. The restart phase must reconcile
all downtime writes and preserve history.

Do not count the soak as passed based on elapsed wall time or a live systemd
unit. Require the guest report to say `passed`, at least 259,200 seconds of
recorded run time, successful final exactness and SQLite integrity checks, the
scheduled reader and controlled restart records, and complete host archive
evidence for the candidate hash. Any exactness failure, unrecorded phase,
unexpected worker restart, OOM event, stale archive gap, or candidate hash
mismatch against the qualification manifest keeps the gate open. Preserve
failed workspaces and reports; do not silently resume or reset a failed run and
count the combined durations.

The 24-hour production canary is a separate, later gate. Start it only when the
release manifest's preconditions pass, including a fresh exact live accounting
baseline immediately before deployment. Keep the candidate and rollback package
hashes, verified database backup and restore procedure, monitor output, and
rollback result together in the canary evidence bundle. A passing disposable
guest rehearsal does not count as a production canary.

## Evidence refresh — 2026-10-26 11:59 UTC

- Current static release candidate: `0.6.3-14`, binary SHA-256 `65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e6`.
- DEB and RPM lifecycle runs each passed in fresh marker-provisioned Ubuntu 24.04 and Rocky Linux 9 guests. Reports are `target/readiness/package-lifecycle-deb-v14-20261026.json` and `target/readiness/package-lifecycle-rpm-v14-20261026.json`.
- The installed legacy host has a one-snapshot SQL invariant pass, but an exact filesystem comparison found its v4 index omitted all of `/usr/lib/modules` (15,490 paths including that directory). The current static candidate's one-off root scan included that complete subtree. The legacy exact audit was unstable because bytes and commits changed during the walk; do not use either one-snapshot result as a canary baseline.
- The consolidated P0 run on dynamic candidate `ceba3c3c` has passed 30/30 crash-recovery rounds and true fanotify queue overflow with dirty-metadata write failure. Other suites are running. It does not qualify static release hash `65c635c4`; repeat P0 against the static artifact.
- The 72-hour endurance and 24-hour canary gates remain unstarted. Existing interrupted or shorter soaks do not count toward either duration. Preserve a fresh exact baseline and candidate/rollback hashes before any future canary deployment.

## Packaged-static 4 GiB scale result — 2026-10-26 15:27 UTC

The current packaged static binary passed the complete 100k and 1m scale
levels. At 10m, the scan completed in 1,042.7081s, under the 1,800s wall-time
limit; fixture totals, sampled paths, SQLite integrity, query p95s and TUI
responsiveness all passed. Peak scan RSS was 909,936 KiB, above the 256 MiB
limit, so the complete scale report is **failed**. Report:
`target/readiness/scale-static-e6-20261026-4g-1job-r3-final.json`, SHA-256
`344f95958e43f3357f3e38ed0d52316271a4a3746a151a47a4d1a5f1576d852e`; packaged
binary SHA-256 `65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e`.

## Evidence refresh — 2026-10-26 17:30 UTC

- The false RSS failure above was traced to `wait4()` returning the fixture
  worker's inherited high-water mark. The sampler now records the Dux child
  process `VmHWM`; its focused measurement regression passed 1/1.
- Corrected packaged scale rerun r5 passed the 100k, 1m, and 10m levels in a
  4 GiB Ubuntu 24.04 guest, one scan job, 3 GiB worker memory cap, and zero
  observed cgroup OOM/OOM-kill events. The 10m scan took 1,029.9497s and peaked
  at 153,612 KiB RSS. The report records truth totals, SQLite integrity,
  sampled path checks, all query p95s, and TUI response budgets passing.
  Report: `target/readiness/scale-static-e6-20261026-4g-1job-r5-final.json`,
  SHA-256 `53c191bbcb6641582f813b751f4b0a43de2ea26e978b85cd5759fbeb5267418b`;
  binary SHA-256 `65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e6`.
- An isolated one-job root scan on the same Ubuntu guest using both the exact
  installed legacy host binary (`ccdffd4a…`) and the packaged static candidate
  completed with zero errors. Both indexed `/usr/lib/modules` with matching
  device/inode identity and 1,310 recursive inodes. This rules out a universal
  exclusion of that path by either binary on this guest; it does not reproduce
  the host-specific live omission or prove whole-root equality.
- The running 4 GiB guest's writable qcow2 overlay was moved to `/dev/shm` to
  stop test disk growth from changing the host's watched tree. The original
  overlay is preserved at
  `target/readiness/vms/ubuntu24/disk.qcow2.host-root-backup`, and both copies
  had SHA-256 `b976d6bd262c849ea01f713b01482634029fda2745e7073f6257a6db75c9e759`
  before guest restart. Future qualification runs must continue using the
  tmpfs-backed symlink while it is available.
- The installed legacy service triggered another materialized-total invariant
  failure at 17:26:33 UTC and completed a zero-walk-error rebuild at 17:28:55.
  A read-only exact observer could not obtain five seconds of stable caught-up
  index structure: after the rebuild it observed `pending_updates=0` while
  `index_nodes` and `index_bytes` kept changing, then timed out. This remains a
  live production blocker; no candidate deployment or endurance/canary run is
  claimed. The 72-hour runbook precondition of a fresh exact live baseline has
  not been met.

A one-job diagnostic rescan of the preserved, read-only ext4 fixture completed
in 999.1s and reported 153,452 KiB maximum RSS, with zero swaps. Its process
samples and `/usr/bin/time -v` output are retained in
`target/readiness/scale-static-e6-20261026-rss-profile-samples.txt` and
`target/readiness/scale-static-e6-20261026-rss-profile-time.txt`. It used a new
destination DB and atomic rename installation. The failed scale run already
had an existing destination DB from its 1m level, and therefore used SQLite
online backup to install the 10m index. That difference is the leading
testable explanation for the observed 909,936 KiB peak; it is not confirmed.
The failed workspace is preserved. A second diagnostic scanned into a copy of
its existing DB using the exact packaged binary and `wait4` measurement path;
the helper is `target/readiness/scale-static-e6-20261026-rss-profile.py`
(SHA-256 `3d8b6b61bb5953c71cb6098cdc8be895f7b47eee5000a9c5300e63fdd4ae482e`).
It exercised online-backup installation and completed at 154,092 KiB peak RSS
with zero OOM events. This diagnostic is not a qualification pass. The fresh
full rerun is active in the 4 GiB guest with one scan job and a 3 GiB worker
cap. Its 100k level passed end-to-end (3.771s scan, 33,828 KiB peak RSS), and
1m passed end-to-end (46.1762s, 122,328 KiB); exact aggregate/sample checks,
query p95s and TUI checks passed at both levels. The 10m fixture is complete
and its scan is active. The latest progress sidecar reports 328.891s elapsed,
4,689,993 files, and ~24 MiB instantaneous RSS; this is not a final peak. Progress artifact
`target/readiness/scale-static-e6-20261026-4g-1job-r4-progress.json` SHA-256
`7b0cff5efe0d7c75cb834b3e99db8d7a3c3246dd76d766a860750ab89b7c2191`. The
complete three-level suite still must pass the 256 MiB RSS gate.

## Evidence refresh — 2026-10-26 17:37 UTC

- The corrected r5 scale run supersedes the failed r3 report and intermediate
  r4/r5 progress notes above: packaged static binary
  `65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e6` passed
  the full 100k/1m/10m suite. Final report SHA-256 is
  `53c191bbcb6641582f813b751f4b0a43de2ea26e978b85cd5759fbeb5267418b`.
- A later legacy accounting invariant snapshot during an active reconciliation
  found 63 dirents with non-directory parents, 4 bad directory rollups and one
  bad leaf; stored root totals were high by 14,979,072 bytes and 278 inodes.
  The sample bad leaf was indexed as a file with descendants at
  `/root/.local/share/Trash/files/xgs-router-smoke.aTRjAU/data/global`, while
  a later filesystem stat found a directory at that path. The `/tmp` rollup was
  high by 15,093,760 bytes and 278 inodes. Evidence is
  `target/readiness/live-invariant-probe-detailed-20261026T1736Z.json`,
  SHA-256 `95971d7774dd0985cbf02c87a4caf827cf176c2d8cc5438e8b62692aca0f5bba`.
  No user Trash contents were changed. The finding is consistent with a stale
  directory/file inode incarnation and strengthens the candidate fix
  hypothesis, but a full exact stable live comparison and same-event candidate
  reproduction remain open.
- The invariant probe now includes bad directory/leaf path samples from the
  same read transaction. Its five focused regression tests passed in the
  isolated 4 GiB guest; probe SHA-256
  `8c196ad5f05a490553a6fb3ac9e22cdc514fd68a11b9361b79f5a6223cd5b58d`, test
  SHA-256 `d50fb55b5758874ea0a214e07f165956d9eb5184cdc6fc0b54ba1985115b60a7`.
- Added the reverse-direction inode reuse regression: the filesystem has a
  directory while the stale index calls the same identity a leaf with 43 old
  children. It removes the stale subtree and restores exact fixture accounting.
  The full Rust unit suite then passed 107/107 in the 4 GiB guest with a 3 GiB
  test-unit cap. After formatting, the exact source was rebuilt and rerun:
  107/107 passed. Test binary SHA-256
  `2fea12322e36e3bde30232674eb4e59f93acd771ec45bcfcf180b5ee3f1de7eb`; output
  SHA-256 `e7861174b6c18db4ae1eddd231ba7e3a831be7ba0bc525c08e2b815a6abc0185`.
- The post-overlay exact observer still cannot get a stable baseline. It timed
  out after 120 seconds when the next invariant failure started reconciliation
  and pending updates rose to 4,438. Report
  `target/readiness/live-exact-post-overlay-r2-20261026.json`, SHA-256
  `f325375615f54f6e56ccd602e1bf6b9f4ab8d5d3cdd1822930490e550ba0300c`.
  Endurance and canary remain unstarted because the required exact-live
  precondition is unmet.

## Focused startup and inode reuse rerun — 2026-10-26 17:47 UTC

Rebuilt the exact current source in tmpfs and ran the focused startup suite in
the isolated Ubuntu 24.04 4 GiB guest. Candidate binary SHA-256:
`be1e7fca34a51804aeb947eca21e036882b522e7173c9e2ff927bd72b52829d0`; fault
binary SHA-256:
`c475d666c8fd301cdd1103aec3c716330d79bb48d2860c14756d9d258ec7dedb`.
The suite passed on a disposable 768 MiB ext4 loop image in 1.155 seconds,
under a 50% CPU quota and 4 GiB worker limit. It verified fanotify watches
before initial scan and schema upgrade, exact replay of 64 startup-window
changes, old-reader query availability, history continuity, and cleanup of a
deliberately stale directory inode with a phantom child. Report:
`target/readiness/dux-startup-live-shape-20261026.json`, SHA-256
`ad07b54aaa7ccec285c9ade36705e8d2cf54c1d3777c85aac055f748ddbf9d55`.
Harness SHA-256:
`d3793396e72f667eb42f04cbc6f18ba1e6d04cb6d54832a334c379bd914df63e`.

This is isolated candidate evidence. It does not prove an exact replay of the
production fanotify race, stable full-live accounting, the endurance run, or
the canary. Those release gates remain open.

### Live fanotify reproduction added — 2026-10-26 17:49 UTC

Extended and reran that focused suite with a runtime event-path reproduction.
While the candidate watcher was active, the disposable index was changed so an
actual directory inode appeared as a file and had a phantom child dirent. A
real child creation generated fanotify activity. The candidate repaired the
inode kind and stale subtree, and the harness required two fresh snapshots to
match all paths, inode allocations, root totals, and directory rollups. It
passed without relying on a periodic full-root integrity sweep.

Report `target/readiness/dux-live-fanotify-inode-reuse-20261026.json`, SHA-256
`f619bfaf8e8cf54ecee072b1ef6ff105d96af0b9b99b37209620d94ffd4b4a31`.
Candidate binary SHA-256:
`be1e7fca34a51804aeb947eca21e036882b522e7173c9e2ff927bd72b52829d0`;
harness SHA-256:
`102a4059f22b73b53eb8b212bf3386c1b0efd4f0d6a9ab41c6e11c9eaef2d735`.
This directly exercises the candidate repair path for the observed mismatch
class. It does not establish that every live invalid-parent row had that cause,
or prove stable full-live accounting on the installed legacy service.

## Fresh live accounting and exact-window attempt — 2026-10-26 17:55 UTC

A read-only same-snapshot diagnostic of the installed legacy index found root
totals high by 8,192 bytes and 3 inodes. The only bad directory rollup was
`/tmp`, high by the same 8,192 bytes and 3 inodes; there were no invalid parent
dirents, dangling children, or bad leaf totals. A separate recursive `/tmp`
walk found all current paths, identities, and allocations matched, consistent
with a stale cached rollup. Diagnostic report:
`target/readiness/live-accounting-current-20261026T1750Z.json`, SHA-256
`97a97568b02a71678d2d9183a82a96ebb78e447931f5b0f45cb5e1bc192b1147`.

A bounded read-only observer then required 30 seconds of stable caught-up state
before a whole-root exact comparison. It timed out after 180 seconds because
index nodes/bytes continued changing; the whole-root walk did not start. At
timeout, the status fallback reported zero pending updates and zero dropped
events even though the service status still said it was reconciling. Report:
`target/readiness/live-exact-current-r3-20261026.json`, SHA-256
`ee5c8e155ca913348c2cf99b7d686a32ee0a8b722cc22567a0faca72f2b16d38`; observer
SHA-256
`23aa9a9352c9f61e5cb64f4e6abd7de8be6a5af2290d7d683595b591bcba5cf4`.
This is a failed stable-window precondition, not a full exact audit. The
installed legacy service remains active; full-live exactness, endurance, and
canary gates remain open.

## Candidate heavy churn qualification — 2026-10-26 18:08 UTC

The current candidate passed the production-shaped churn suite in the Ubuntu
24.04 4 GiB guest with a 50% CPU quota. It used a 100,000-file baseline on a
disposable 2 GiB ext4 image with a 3 GiB tmpfs workspace, then completed 20,000
operations across ten exactness checkpoints. The mix included atomic replace,
rapid create/delete path reuse, rename overwrite, hardlink writes and unlink,
in-place size changes, and transient files. A full rebuild was forced during
the midpoint mutation batch. Every checkpoint verified paths, inode identity
and allocation, root totals, and all directory rollups; growth history remained
continuous and zero events were dropped. It completed in 556.047 seconds with
1.1 GiB peak worker memory.

Report `target/readiness/dux-churn-100k-20k-r2-20261026.json`, SHA-256
`01874f53210deb058f8afec465d584b568cfa9f0bf183c1ba9cb812a80c633c9`.
Candidate binary SHA-256:
`be1e7fca34a51804aeb947eca21e036882b522e7173c9e2ff927bd72b52829d0`;
harness SHA-256:
`88dff12be778e9f3709400deffb92dfd7bd3b70beb5689d9218143f7c2194fe4`.
The harness now automatically promotes churn to a 100k baseline and 2 GiB
image so normal churn does not cross Dux's 256 MiB low-disk reserve. The
workflow regression suite passed 13/13 after this guard was added.

This candidate test does not establish exact full-live accounting on the
installed legacy daemon. Endurance and canary remain unstarted pending that
gate.

## Live exact walk and observer correction — 2026-10-26

A read-only full filesystem walk completed across 1,260,007 paths with no walk
errors. It was not a passing exact audit: the installed index changed during
the walk (`index_nodes` moved from 1,241,391 to 1,242,472), and the one-snapshot
comparison reported 16,571 paths missing from the index, 2 identity
mismatches, and 23 allocation mismatches. The largest samples included a
transient `/tmp/go-build2675857445` tree. A later sample observed 8,919 paths
present without index entries, but the whole temporary tree was gone at a
subsequent stat; 11 of 23 allocation samples still differed in that later
point check. These changing snapshots cannot distinguish persistent index
drift from transient build churn.

The read-only observer had a retry flaw: after the first full walk, it could
repeat another million-path comparison each time its two-second recovery loop
saw the old service caught up. It now performs only one full filesystem walk
and uses a database-only snapshot for recovery. It reports an exact audit as
unstable unless that original path comparison passed on a structurally stable
snapshot. The live observer and readiness workflow unit suites passed 40/40;
`py_compile` and `git diff --check` passed. Observer SHA-256:
`7159d8ca8f4c5c1044f5d35dc4d39f318ffd16e808f031ee7bf2465f2950d404`.

Evidence reports:
`target/readiness/live-exact-r5-unstable-20261026.json` (SHA-256
`ff1815cb434def5b8898072a145617d7e6217368b62b1a1702283fd2a742a317`) and
`target/readiness/live-exact-r5-recheck-20261026.json` (SHA-256
`76d904fa80b170d99c0021b41e64ab460cfa1748f711df9d8e4193c62f6b968b`).
Neither report qualifies the installed live index. The endurance and canary
gates remain open.

With the retry amplification removed, a bounded 120-second audit then waited
for 20 seconds of unchanged caught-up state. It timed out because `index_bytes`
continued to change, despite zero pending and dropped events; it did not start
another full walk. Report
`target/readiness/live-exact-after-observer-fix-r6-20261026.json`, SHA-256
`c4d79c8b3eac950c09272f73ef7bf9c9a2c192553a7fce4be4b7dd8fd3e55528`.

A separate read-only internal invariant snapshot at 18:26 UTC passed: root
totals exactly matched 1,241,403 inode rows at 140,135,067,648 allocated
bytes, with no bad directory rollups, bad leaves, invalid parents, or dangling
children. This does not establish path or inode-allocation equality with the
filesystem. Full-live exactness, endurance, and canary remain open.

### Bind-mount alias coverage limitation

A 4 GiB Ubuntu guest repro created a bind alias to a directory and a bind mount
cycle back to the scan root. A temporary per-branch cycle guard let the scanner
reach both aliases but failed on schema v5's unique
`(parent_dev,parent_inode,name)` key with SQLite error 2067. The scan change was
reverted. Dux currently cannot represent separate path instances beneath two
bind-mounted names of the same directory inode; a correct full-path solution
needs path-instance-aware dirent parents (and a migration) or a documented
alias policy. This does not establish the cause of the independently observed
missing `/usr/lib/modules` subtree.

Evidence: `target/readiness/scan-bind-cycle-test-20261026.json` (SHA-256
`1892588476d9d53914c3023f059e3c271b46e9f48aeaff0b7e472024e45cd6fc`) and
`target/readiness/scan-bind-cycle-test-20261026.sh` (SHA-256
`fb1439ed2bf38518b9b702598dd54dace07349ccc7175d21dd013759c53b8eb1`).

The tracked bind-alias harness creates loop-backed filesystems and bind mounts.
It refuses to run unless the caller is root inside a KVM/QEMU guest carrying the
controller-provisioned `/etc/dux-qualification-sandbox` marker. Its report uses
exclusive creation so a typo cannot overwrite an existing artifact. Run it only
with a candidate binary and evidence path dedicated to that qualification run.

### Live host subtree scanner comparison

The VM manager can export a selected host directory read-only over virtio-9p to
a 2–4 GiB qualification guest. A write probe in the guest confirmed the host
export rejects writes. Against the actual host `/usr/lib/modules` subtree, the
packaged static candidate scanned 13,034 files and 2,456 directories with zero
errors. Exact verification matched all 15,490 paths and their device/inode and
allocated-byte values, with no extras or traversal errors. This proves direct
subtree scanning works for the candidate; it does not prove a full-root scan
will include it or explain the existing live omission.

Report `target/readiness/live-modules-candidate-exact-20261026.json`, SHA-256
`46d2c6bb5665036f07aa6667bf872d11a35596dac5b67cf6cc7b05c06fe43d0f`; binary
SHA-256 `65c635c45feac49f15793015bdb0666107740b04f6299e1b491ddd42b65718e6`.
The broader installed-legacy `/usr` scan was still active at the latest
checkpoint and remains unqualified until its exact report is complete.

The installed legacy 0.6.2 binary was then scanned over the complete real host
`/usr` subtree in a separate guest database: 158,175 files, 20,342 directories,
zero scan errors. An exact comparison of the existing database's `lib/modules`
branch against the read-only host subtree matched all 15,490 paths, identities,
and allocations, with zero omissions/extras/errors. Report
`target/readiness/live-modules-legacy-fullusr-exact-20261026.json`, SHA-256
`f06f12a4e2308e6f89de11a97924bcaece2de2c702ac98afde75835e9477e846`.
This confirms the legacy scanner handles the subtree under a `/usr` scan root;
the omission remains reproducible only in the installed index built by a `/`
root scan. The first all-`/usr` verifier was stopped after scan completion due
slow 9p metadata reads, so it is not claimed as a full `/usr` exact audit.

## Qualification state refresh — 2026-10-27 02:25 UTC

This section supersedes the older historical statements above that no 72-hour
run is active. The hash-bound R16 endurance run started at 2026-10-27 01:59:30
UTC in the dedicated Ubuntu 24.04 4 GiB guest. At 02:23:42 UTC, its host archive
reported fresh progress at 1,440.6 seconds and 31,058 operations; the candidate
binary SHA-256 is
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, worker and
controller were active with zero restarts, and all guest OOM counters were
zero. No 100,000-operation exactness sample has completed yet, and none of the
72-hour duration, scheduled hourly burst, held-reader, or controlled-restart
gates is complete. This is an active run, not a passed endurance gate.

The installed legacy service still has recurring accounting-invariant failures.
The latest point-in-time capture was taken during a full reconciliation and
found incorrect directory and leaf totals; the full-root exact observer has
not produced a stable filesystem comparison. Keep the canary unstarted until a
fresh stable exact baseline is established immediately before deployment, as
required above. Current all-suite regression evidence (108 Rust tests and 94
Python harness tests) is recorded in `PRODUCTION_READINESS_AUDIT.md`; these
unit/harness passes do not substitute for live exactness, endurance, or canary
qualification.

### Live event-loss incident update — 2026-10-27 02:25 UTC

At 02:24:30 UTC the installed legacy daemon logged an accounting-invariant
failure, began a root reconciliation, and logged `fanotify queue overflow` 26ms
later. The read-only status snapshot showed the reconciliation active and 306
pending updates; its legacy `events_dropped=0` field does not override the
explicit kernel overflow warning. Incident evidence is
`target/readiness/live-overflow-20261027T022430Z.json` (SHA-256
`153dc09935ab67324ec9d41e6c7b061eef0aa775724837528a1e754f715d69e5`). This
makes live event coverage and exactness an active release blocker. Do not start
the production canary until recovery is established and a fresh exact baseline
passes.

### Post-overflow accounting remains dirty — 2026-10-27 02:31 UTC

The 02:24:30 live overflow's reconciliation finished at 02:28:08 and replayed
16,300 pending paths, but the daemon remained dirty after pending reached zero.
A bounded read-only SQL check found four incorrect directory rollups and an
8,077,312-byte root allocation deficit. Its deepest sampled row was
`/tmp/go-build2387714199/b047/tmp`; that Go-build directory was absent from the
filesystem when `lstat` rechecked it, while its indexed row remained. See
`target/readiness/live-postoverflow-sql-audit-20261027T0228.json` (SHA-256
`b5e74c5c454fc82c1ec43e9a3172e2fbaf703d6a0c4c87cc5de5e8141b301dd2`). The
post-overflow live recovery and exact full-root baseline therefore remain
failed/open; the canary precondition is still unmet.

### Endurance checkpoint — 2026-10-27 02:31 UTC

The same hash-bound 4 GiB guest run was still healthy at 02:30:42 UTC: fresh
archive status, 1,861 seconds elapsed, 39,697 operations, active worker, zero
restarts, and zero guest OOM counters. It has not reached its first 100,000-
operation exactness sample, so it remains in progress and unqualified.

### Host reboot invalidated the active endurance run — 2026-10-27 05:01 UTC

The operator force-rebooted the host after load average became extremely high.
The Dux endurance guest and archive collector were stopped at 04:49:02 UTC.
Their last fresh checkpoint, 04:48:42, records 10,115.9 seconds elapsed, 210,444
operations and two exact samples; it does not satisfy the uninterrupted 72-hour
gate. Preserve this partial archive and start a new full-duration run only after
the production Dux scan loop and host load are stabilized.

After reboot the installed legacy daemon completed a startup root scan, then
repeatedly failed its accounting invariant and initiated additional root scans
at 04:55:04 and 05:00:04. During one scan a 10-second measurement recorded up
to 10,025 device reads/s, 421 writes/s and 82.5% device utilization; Dux PID
samples reached 225,324 KiB/s read and 247,596 KiB/s written. I/O dropped
sharply after a scan finished, then rose again when the next began. The host
was responsive at 05:00:55 (12 CPUs, load 0.70/1.75/1.21, 27 GiB available
RAM, 39 GiB disk free), but Dux's recurring accounting mismatch and scan loop
remain production blockers. See
`target/readiness/server-health-after-reboot-20261027T050055Z.json` (SHA-256
`84f81b78c507b15abde20f027cbea232d9038a089d33e9a92b53ee15bc7522c2`).

### Post-reboot scan loop rechecked — 2026-10-27 05:06 UTC

A third successful whole-root reconciliation after boot still did not hold:
Dux reported caught up at 05:03:35, then failed the same accounting invariant
at 05:05:06 and began another full scan. The first two seconds of that scan
showed 1,638–1,775 disk reads/s, 157–868 writes/s, and 33–44% device utilization
while host load remained about 1.04/1.45/1.23. The R16 run remains stopped after
the reboot interruption; it was not restarted during this I/O incident check.
Evidence: `target/readiness/live-reconcile-loop-20261027T050511Z.json`,
SHA-256 `67f6d6debd949ce13467be7c38a8a828a1451189be836dc82f39aebccd6d7e1d`.

### Candidate churn gate passed in isolated 4 GiB guest — 2026-10-27 05:33 UTC

A fresh static-musl build from the current worktree exactly matches candidate
SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`.
In Ubuntu 24.04 KVM limited to 4 GiB RAM / 2 vCPUs, the candidate passed the
production-shaped churn suite on a disposable 2 GiB ext4 image with 100,000
baseline files and 20,000 mixed operations. All ten 2,000-operation batches,
including concurrent reconciliation, passed exact paths, inodes, allocations,
directory rollups and history; 38,671 kernel events were observed, 14,788
updates committed and zero events dropped. Worker runtime was 20m28.6s, memory
peak 1.1 GiB, with no swap, restart or OOM. Evidence:
`target/readiness/r16-100k-20k-churn-20261027T0533Z.json` (SHA-256
`b414fe4ca5872910ed9c5b774a242515345a67380ca186a6f8a22f293708cc2f`).

This closes one isolated churn gate only. The host exactness check, remaining
fault suites, 100k/1m/10m calibrated performance budgets, package lifecycle,
security/default-privilege review, uninterrupted 72-hour soak and 24-hour
canary remain open. The installed legacy service is still stopped while these
gates proceed; no production canary is claimed.

### Candidate root-scan gate passed — 2026-10-27 05:34 UTC

The same hash-bound candidate also passed a read-only one-filesystem scan of
`/` in the 4 GiB Ubuntu 24.04 guest. It indexed 69,806 files, 9,288 directories
and 85,609 dirents with exact materialized accounting, complete coverage, no
dirty reason, and SQLite integrity. Runtime was 36.781s under the 25% CPU
worker quota. Evidence:
`target/readiness/r16-ubuntu24-root-scan-20261027T0534Z.json` (SHA-256
`b5ee3d341a43c660943fdc24e36f33840c2c1c9ebfc363a9761aa9550de50329`).

Together with the 100k/20k churn result above, these are two current isolated
passes. The production root has over 1.1 million files and still lacks a stable
exact candidate comparison; all release, long-duration soak, and canary gates
remain open. The host legacy service remains stopped.

### R17 background accounting check and ext4 churn — 2026-10-27 06:25 UTC

R17 moves periodic and post-reconciliation full accounting verification off the
watcher thread. The read-only worker has a 10%-of-one-CPU duty budget and is
interrupted when new queue/backlog work, a rescan request, or Critical host
pressure returns. Post-scan status stays dirty until the background consistency
check passes. The source SHA-256 is
`713eb5e0cbca4cf5c197e86797478b46fde861f4f216bd18333fbd82c48ea7ce`; its static
musl candidate SHA-256 is
`e34a5c8759d004102de634b4b8dbe1766dfe8f48755b9e4d981258b2ba98d970`.

On Ubuntu 24.04 limited to 4 GiB RAM / 2 vCPUs, this binary passed 100,000-file
and 20,000-operation churn on a disposable 2 GiB ext4 image. All ten exact
batch checks passed for paths, inodes, allocated blocks, directory rollups and
history; a concurrent rebuild also completed with two post-reconciliation
accounting checks passing. The run saw 38,223 events, committed 14,541 updates,
dropped zero, took 1,115.493 seconds, peaked at 1.1 GiB memory, used no swap,
and had no restart/OOM. Report:
`target/readiness/r17-source-candidate/100k-20k-churn.json`, SHA-256
`b697f80c9b7965cd67d8069f7a43e4ea6242cfb4d6ae4986187165d9529c93bc`.

This is not a release qualification. R17 has no DEB/RPM package yet. The full
Rust watcher suite still needs an ext4 run; six allocation-sensitive tests
cannot be meaningfully evaluated on tmpfs, where `st_blocks` is zero. Periodic
check cancellation, the full fault suites, scale budgets, package lifecycle,
security/default-privilege review, live-host exactness, 72-hour endurance and
24-hour canary remain open. R16 remains installed, active, and not caught up;
no production canary or host-exactness result is claimed.

### R17 package and lifecycle gates — 2026-10-27 06:30 UTC

Built R17 release 17 DEB and RPM packages from the tested static-musl binary.
Manifest `target/readiness/r17-source-candidate/package/build-manifest.json`
(SHA-256 `cf612a612c4bc4ed77c15e6d84e565ee9edaf287870666330312d4e37136cda1`)
binds source tree SHA-256
`e9925c956d85f8bb336c201f81a8934fe4831f349db7e76fc93d1ccabd5c6ab3`, binary
SHA-256 `e34a5c8759d004102de634b4b8dbe1766dfe8f48755b9e4d981258b2ba98d970`,
DEB SHA-256 `09ee2f4bf5593368e9dd9da61e08e4a65dc2c8ed11d44a79885367aac53f2557`,
and RPM SHA-256 `6704b314db98448f51c4e9ee8d9fb2ce77630f629200a3ff97aedf70ebd13981`.
All artifact checksum entries pass.

Both packages passed the six-step lifecycle in their disposable qualification
guests: legacy baseline, upgrade preserving disabled services, rollback,
re-upgrade, remove/purge, and clean install. The DEB ran on Ubuntu 24.04 in
11.097s (report SHA-256
`2d3a9fa5f229d33e85683fb06efaca3e0bafd5d4be206c8b129592aa552d54fd`); the RPM
ran on Rocky 9 in 20.245s (report SHA-256
`7ddb56c0ee0882292a08bdfdbd80fbe07df377a90d0088be972f8d5296b43efe`). The
reports verify the service security properties, disabled tunnel state, and
root:dux 0640 database/WAL/SHM permissions. These are package lifecycle passes,
not overall release approval. R17 fault-matrix and performance reruns,
dependency/security review, live-host exactness, runbook closure, 72-hour
endurance and 24-hour canary remain open. The installed service remains R16 and
is dirty/not caught up after mount-topology changes; all qualification VMs are
stopped.

### R17 active verifier cancellation and current package hashes — 2026-10-27 06:46 UTC

Added an integration regression that cancels the background verifier while it
is processing a 500,000-row SQLite snapshot. It passed in 16.58s. The complete
current-worktree Rust test binary passed 111/111 tests in 63.84s in a 2 GiB,
2-vCPU Ubuntu 24.04 guest with no swap. Test binary SHA-256:
`63b9bec2f376f9a03c676bb6bffaff6c660a51c7b0bf3605e898497b01b36888`. Result
summary: `target/readiness/r17-source-candidate/rust-tests-ubuntu24-2g-ext4.json`
(SHA-256 `297c9937807d4519638ab01c3b55359b325ba18597a43bc1b78eb87c00cd2f2c`).

Rebuilt release-17 packages after the test-only source addition; the release
binary payload remains the previously qualified R17 candidate. The updated
build manifest has SHA-256
`c2f47b8c7f38d80d18a1cce78f975c7956952cec6104e86856c7946612a1dbee` and source
tree SHA-256 `12cf565e10aa5a4b5cc01c5d5067537f0921bd20ad84cb03d782a256cf9f3309`.
New DEB SHA-256 `8e894c495fafb565d54f9eabf82689acaa7ea88efc7b3a86e727a4e2bec6104e`;
new RPM SHA-256 `4c435d5fa14305436c8398a36683775597bc110f3f29c7517c3ae22f1cae4384`.
All six lifecycle steps passed again against those exact hashes in 2 GiB
Ubuntu 24.04 and Rocky 9 guests. Reports:
`target/readiness/r17-source-candidate/deb-lifecycle-updated.json` (SHA-256
`378ea61b380e089494c437e35faddd77db3e261b568941e4753d43377b3463b9`) and
`target/readiness/r17-source-candidate/rpm-lifecycle-updated.json` (SHA-256
`9d8c19c5589d5e8ca34993a367323606a3a130531336708aec5f3e27ad69e30f`).
Neither guest remained active after qualification. R16 remains the installed
production service; live exactness, the R17 fault matrix, performance budgets,
security review, runbooks, 72-hour endurance and 24-hour canary are still open.

### Current R17 gates — 2026-10-27 08:49 UTC

The current R17 source candidate has a hash-bound 13/13 selected-suite P0
matrix pass. Its soak suite was not included. The refreshed Cargo.lock advisory
scan covered all 231 packages against 1,378 advisories and found no known
vulnerabilities. `cargo audit bin` recovered only 44 dependencies because the
binary lacks embedded audit metadata; this is not a complete binary inventory.
The unit security analyzer reports 5.1 MEDIUM exposure because the daemon
requires root and fanotify capabilities for host-wide indexing.

A separate 72-hour R17 endurance run is in progress in a 4 GiB Ubuntu 24.04
qualification guest. It uses bounded host/worker CPU and memory, a 100,000-file
ext4 fixture, exactness checks every 100,000 mutations, and archived resource,
restart, OOM, WAL-reader and recovery evidence. Its recorded start is
2026-10-27 08:45:52 UTC; the latest checkpoint at 08:48:45 reports 180 seconds
and 3,766 operations with zero restarts/OOMs. This is early progress, not a
soak pass. R17 1m/10m scale evidence, stable live host exactness, and the
separate 24-hour canary remain open. Production still runs R16 and remains
behind/dirty; do not interpret the candidate's isolated results as live-host
recovery.

### Live host pressure and queue follow-up — 2026-10-27 08:51 UTC

At 08:50 UTC both the installed R16 client and the R17 status reader report
64,529 pending paths and a dirty reason of `non-watched filesystem mounts
changed; reconciling visible paths`. No scan is active. The pending count rose
by 3,478 between 08:47 and 08:50. A two-second per-process sample shows Dux
using its 50%-of-one-core systemd quota, with zero physical block I/O in that
sample. Host load is 4.68 on 12 CPUs, storage utilization is low in sampled
intervals, and CPU PSI remains elevated without full CPU pressure. The index is
still behind/dirty and is not exactness evidence. `docs/OPERATIONS.md` now has a
repeatable host pressure capture procedure. The 72-hour R17 endurance remains
in progress and has not yet reached its first 100,000-operation exact sample.

At 08:51:43 UTC, the same R17 endurance run is active at 300.2 seconds and
6,260 operations. Its archive is fresh; worker/controller/VM restart counts and
guest OOM counters remain zero. At 08:51:50, host load is 3.45 on 12 CPUs and
I/O PSI is below 1%; CPU PSI has receded from a transient elevated sample.
There are no exact endurance samples yet, and no duration gate is passed.

### R21 qualification checkpoint — 2026-10-28 05:15 UTC

The R21 static-musl candidate (`0e61da726b784b9b7d75299bb864758284097106d7741c370c3c360cb8b4e3ab`)
now has all 14 selected P0 recovery suites passed across two bound reports,
including 30 crash boundaries, overflow, 100k-file churn with 20,000
mutations, storage/permission/capability/OOM/pressure/mount/WAL/startup/retry,
and root recovery. Exact DEB and RPM lifecycle runs also passed all six stages
on Ubuntu 24.04 and Rocky 9 guests with 2 GiB RAM. Candidate dependency audit
found zero known vulnerabilities across 231 locked dependencies. The R21
refreshed 100k and 1m scale tiers passed under a 4 GiB guest and 50-sample query
measurements (scan 10.68s / 154.97s; peak RSS 27.6 / 114.0 MiB).

These results do not close the full 10m scale gate: its prior full report failed
the `du --max-depth 1` p95 budget, and the corrected 50-sample full rerun remains
open. Production remains the unchanged R16 daemon and is still stale/uncertain;
a fresh CPU-only no-progress interval was recorded. The Rocky clean-install
runtime also exposed duplicate `/efi` and `/boot/efi` mount-root identity that
correctly leaves coverage uncertain. Exact live production convergence,
duplicate-mount path semantics, final security risk acceptance, 72-hour
endurance, and 24-hour canary remain open. The qualification evidence and
individual hashes are in `PRODUCTION_READINESS_AUDIT.md` and
`target/readiness/r21-incremental-rollup/`.

### Later artifact reconciliation — 2026-10-28 13:19 UTC

The R21 checkpoint above predates later archived evidence and is incomplete as
a summary. R25 DEB and RPM lifecycle reports both record `status=passed` for the
six lifecycle stages (legacy baseline, upgrade preserving service state,
rollback, re-upgrade, remove/purge, and clean install). They bind binary SHA-256
`51ba80db68646b948695ecf868a5fc74bf3d4801c1a954a0147d0c218f87e5b8` and source
tree SHA-256 `d0235e6d81a600ebcaaa45aa32016c821be39b7d15e9815dae7a0631bb88ec7e`.
Reports:
`target/readiness/r25-startup-recovery/package-lifecycle-r25-deb-20261028.json`
(SHA-256 `7ea6ee23eb30cb49d118c76229f6ef6f0534a6a2896b9af42a4840fc3b403f7c`)
and `target/readiness/r25-startup-recovery/package-lifecycle-r25-rpm-20261028.json`
(SHA-256 `05aa0b74bf9ec80d9fe002e4b60f01aff0b311f19ac3fa74d2fc3c83e9d0f306`).
These are package lifecycle results; they do not qualify runtime scan coverage
or fix the duplicate `/efi` and `/boot/efi` identity issue.

The 100k and 1m tiers remain passed on R21 binary SHA-256
`0e61da726b784b9b7d75299bb864758284097106d7741c370c3c360cb8b4e3ab`. A later
R23 query/TUI recheck also records `status=passed`, 50 samples per query, no
budget failures, and exact truth/history rechecks over the preserved 10m index.
It reused exactness evidence from the R21 scan and ran the R23 query binary
`d4c50683a9dc7229e5caa1eb8a0680d4358770261607796503d05ea9557e32e4`; its
source tree SHA differs from R21's. This is query/TUI evidence for R23 against
the immutable R21 index, but it does not replace a complete single-source,
single-binary 10m qualification. The R21 full rerun remains `status=failed` on
its depth-1 `du` query budget; repeat the complete 10m tier on the exact
source/binary intended for release. Recheck report SHA-256
`2d828681dab2297a4f77b0caa36486319683267d53e95f7097730d9d93555112`.

At this checkpoint, only the pressure-gated static-musl build controller is
running; no 72-hour endurance worker or 24-hour canary is active. The installed
production service remains R16 and reports an uncertain, nonempty watcher
backlog. Current-source release build, exact live convergence, duplicate-mount
coverage semantics, a same-binary 10m result, the new-source fault matrix,
endurance, and canary are still open.

### R27/R28 mount-identity follow-up — 2026-10-28 13:24 UTC

The latest packaged artifact found during this review is R28, not R25. R27's
DEB lifecycle passed, but R28 explicitly does not inherit that pass. R28 adds
`statx` mount-ID diagnostics when the v5 scanner encounters the same directory
identity through multiple paths; it does not change the v5 path identity model
and the summary explicitly says alias correctness is not fixed. Its build used
`MemoryMax=2G`, `CPUQuota=20%`, no swap, and peaked at 458,700,000 bytes. Two
focused tests passed (real mount-ID identity and scanner diagnostic wiring),
but R28 package lifecycle and guest scan are both marked not run.

R28 binary SHA-256 is
`651c1cf3b8de7f76b5e6471f6eff56f35dd70160a3cc88b706bf30d2721b8f2b`; source
tree SHA-256 is `ce4981cbd34944a9026f89c1fbbf8d98957ad4cd51abbc35a23d3e716f4fb453`.
Build manifest SHA-256 is
`a8b604d6b4fb236671f895aa87f5b63d68f500625233ef204ff2601f3247fd98`, and
build summary SHA-256 is
`01586b80c374eec1ec851964b7f80c1e37728fb510a62533d64456e5e89e6eea`. This
source tree is not the current worktree: R28's `src/watch.rs` hash is
`675e2ff6cb43d7cd8e8aced8e8a801336242d4952084c3a3a8ecbe700bccac16`, while
current `src/watch.rs` is `dc8481937a17d75c97fb69b0227a8c21c9ef1b8859634a37745a66108bb54224`.
Do not treat R28 as current-source release qualification.

## Current release checkpoint — 2026-11-01 09:37 UTC

The source worktree has no completed current-source R22 release artifact yet.
The version-probe regression is fixed and the full Python harness passes
194/194, but two capped native build attempts on the production host were
stopped while compiling bundled SQLite because host CPU pressure was high and
the worker made little progress. The latest worker was capped at 15% CPU,
6 GiB memory, no swap, I/O weight 10, Nice 19, and one Cargo job; it ran for
2m54s, used 26.1 CPU-seconds, peaked at 443.4 MiB, and produced no package or
binary. Its transient unit is inactive. Resume release building on a separate
builder or during a measured lower-contention window; do not treat preflight or
mocked tests as package qualification.

The active production daemon is still the older installed build, dirty and not
caught up: the 09:37 UTC sample has 32,681 pending updates, 166,282 seconds
behind, no scan active, and 1,001,079 cumulative dropped events. The live
index is not a stable exactness baseline. The published index schema remains
v6; the path-instance graph is private scan staging only, so duplicate mount
aliases remain incomplete and the Rocky `/efi` plus `/boot/efi` behavior is not
closed.

For current release planning, nine major gate groups remain open: live exact
recovery; published path-instance/schema-v7 semantics; signed, provenance-bound
auditable release plus binary dependency scan; current-source full fault and
recovery matrix; same-binary 100k/1m/10m performance qualification; formal
current-package Debian/Ubuntu and Rocky lifecycle/readiness; residual
root/`CAP_SYS_ADMIN` security risk acceptance or qualified redesign; the full
hash-bound 72-hour endurance run; and the 24-hour production canary with stable
exact baseline and rollback evidence. These are release gate groups rather
than a count of individual bugs. Older binary tests and historical package
lifecycles do not close current-source gates.

### Current-source Rust regression update — 2026-11-01 09:50 UTC

The governor regression was made deterministic under strict CPU quotas by
injecting synthetic cycle debt instead of busy-spinning. The focused test
passed 1/1, then the rebuilt all-features Rust suite passed 241/241 under a
10% CPU / 2 GiB memory / no-swap cap. All-target/all-feature Clippy with
warnings denied, formatting, and whitespace checks passed. This closes the
local Rust unit/lint check for this source snapshot only; it does not close the
current release artifact, production recovery, alias-schema, package, scale,
security-risk acceptance, endurance, or canary gates recorded above.

## Live readiness refresh — 2026-11-01 13:40 UTC

The production executable is Debian package `dux 0.6.3-16`, PID 4093354,
SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`.
Its embedded revision prefix matches the current Git `HEAD`, but it reports
`+modified`; the exact source tree used to build it is not hash-bound to this
worktree. The worktree itself has extensive uncommitted changes and no completed
current-source release package. Do not describe this as a simple old-version vs
new-version comparison.

The live database is schema v5 (`PRAGMA quick_check` returned `ok`); current
source declares schema v6. Its v5-to-v6 path requires an atomic full-root
rebuild, not a metadata-only migration. The proposed path-instance graph is
still private staging state, so schema-v7 mount-alias semantics are not
published. Production remains dirty and not caught up: the 13:39 sample has
92,559 pending updates, about 115 minutes of lag, no reconciliation scan, and
the reported dirty reason is `non-watched filesystem mounts changed;
reconciling visible paths`. Counters progressed during one longer interval,
then remained flat across several short samples while lag increased. The index
is not an exact baseline and no production package, service, database, or index
was changed.

Current-source qualification is incomplete. The all-features test binary
reported 121 of 248 tests passing; 127 were not verified after the full run was
stopped to limit impact on the production host. `cargo check --locked
--all-features` passed, but no current-source release artifact/package,
current-binary dependency inventory, exact schema upgrade, or separate-host
runtime qualification has been completed. No more stress or qualification
tests should run on this production host after the observed fanotify queue
overflows and growing event backlog.

The current planning count remains **nine open release-gate groups**, not nine
individual source defects:

1. Exact live convergence and recovery.
2. Published path-instance/mount-alias semantics and schema v7.
3. Current-source auditable artifact, complete binary dependency inventory,
   provenance, and cryptographic signing.
4. Complete current-binary crash, overflow, OOM, WAL, pressure, coverage, and
   retry fault matrix.
5. Same-binary 100k/1m/10m scale and performance budgets.
6. Current-package Debian/Ubuntu and Rocky lifecycle/readiness.
7. Acceptance or qualified reduction of root and `CAP_SYS_ADMIN` privilege
   exposure.
8. Hash-bound 72-hour endurance with exactness, readers, mutations, restarts,
   and integrity phases.
9. A 24-hour production canary with stable exact baseline and rollback
   evidence.

The count and scopes follow QUAL-78 in `PRODUCTION_READINESS_AUDIT.md`; gates
remain open until evidence is tied to the exact source and package under test.

## Live readiness refresh — 2026-11-01 14:06 UTC

The production service remains package `dux 0.6.3-16`, PID `4093354`, executable
SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. The
production database is schema v5 and passed SQLite `quick_check`, but it is
dirty and is not a verified semantic baseline. At 14:06:35 UTC, status showed
**115,064 pending updates**, **1,660,171 seen / 396,718 resolved / 13,184
committed**, **8,510 seconds (~142 minutes) behind**, no active scan, and dirty
reason `non-watched filesystem mounts changed; reconciling visible paths`.
The daemon's main thread remains at approximately 99% of one CPU. A fanotify
queue overflow was logged at 13:54:35 UTC; no later overflow appeared in the
sampled journal window, but this does not prove that all earlier lost events
have been recovered.

A fresh online rollback snapshot was created at
`/var/backups/dux-index/dux.db.schema5.pre-rescan-20261101T1351Z.sqlite` and
verified as schema v5 with `quick_check=ok`, mode 0600, and SHA-256
`99cc9874f1fe89ccccebd79055a75504a47a37dd86968deab63816d502faccf9`. One
documented `dux scan /` request was sent to the live daemon. The CLI remained
waiting for over two minutes while status showed no active scan progress; the
generation value was not captured in the read-only observations, so the exact
trigger disposition is unresolved. The wait-only client was stopped; the
daemon was not restarted and the request was not repeated. Do not infer that
current source has the same recovery-trigger behavior as this dirty-source
installed binary.

The current worktree adds a 60-second daemon-rescan start deadline and focused
unit coverage in `src/main.rs`; the operational behavior is documented in
`docs/OPERATIONS.md`. That change remains **uncompiled and untested**. Two
resource-capped attempts to compile just the focused unit on the production
host were stopped after host CPU PSI crossed 20%; neither reached test
execution. The partial targets were removed. `git diff --check` and Rust
formatting pass only. The configured SSH alias `192.168.5.106:2222` is
unreachable, no verified guest image exists for the local QEMU/KVM manager, and
the available Docker containers are unrelated workloads. Current-source
runtime/package qualification therefore still needs a reachable isolated
builder/guest. The repository's push/PR CI workflow includes formatting,
all-target/all-feature Clippy, default and all-feature Rust tests, Python
qualification regressions, unit/security checks, release builds, and package
artifact verification. No CI run for this uncommitted worktree is recorded.
Given the two PSI-triggered stops, do not start another build or test on the
production host.

The nine release-gate groups listed above remain open: exact live convergence
and recovery; production path-instance/schema-v7 publication; provenance-bound
signed artifact and binary dependency audit; current-source fault/recovery
matrix; same-binary 100k/1m/10m scale; Debian/Ubuntu/Rocky package lifecycle;
root/`CAP_SYS_ADMIN` risk signoff or qualified reduction; 72-hour hash-bound
endurance; and the 24-hour production canary. The older 241/241 Rust result
recorded at 09:50 predates the current scan-start change and does not qualify
it. Historical package and scale reports remain tied to their stated source
and binary hashes.

## Live readiness refresh — 2026-11-01 14:13 UTC

Read-only production status at **14:13:21 UTC** reports the service active
without restarts, but the live index remains dirty and not caught up:
**115,707 pending updates**, **1,688,867 events seen / 409,159 resolved /
13,696 committed**, **8,914 seconds (~148.6 minutes) behind**, no scan in
progress, and no reported write error. `events_dropped` currently reports
zero, but the previously logged fanotify overflow means this does not establish
that all lost events have been recovered. The daemon process remains at **99%
of one CPU**. Root is **93% used with 14 GiB available**; current host CPU PSI
some avg10 is **0.56%**, and I/O PSI some/full avg10 is **0.11%/0.10%**. No
production service, executable, database, index, or configuration was changed.

This is a newer observation of the existing live-recovery blocker, not a new
confirmed source defect. The release accounting remains **nine open major
gate groups** and **14 tracked blockers**, with the latter explicitly not a
count of proven bugs. Current source remains an unqualified candidate: it has
uncommitted changes and the scan-start timeout adjustment has not compiled or
run. No production-host build/test was attempted in this refresh.

### Qualification VM pressure guard hardening — 2026-11-01 14:15 UTC

The disposable qualification-VM controller previously treated unavailable or
malformed host load/PSI telemetry as no pressure blockers. It now records
missing/invalid `/proc/loadavg` and required `/proc/pressure/{cpu,io,memory}`
readings as blockers, refuses VM admission when those readings are absent, and
stops only its own VM after the existing two-sample critical-pressure rule if
telemetry disappears during a run. PSI and load values must be finite and
non-negative. This changes only qualification orchestration, not the live Dux
service.

The focused VM-guard suite passes **23/23**; the repository's full Python
qualification regression suite passes **221/221** in **5.75 seconds**. The
tests use temporary files and mocked systemd/service operations. `git diff
--check` passes. Rust build/tests, VM startup, package tests, and live Dux
recovery remain unqualified. A post-suite read-only check at **14:15 UTC** still
showed production dirty/not caught up, **121,282 pending**, **9,045 seconds
behind**, no active scan, and zero reported write errors; the service remained
active without restarts. The test run did not change production state.

### Current static and dependency checks — 2026-11-01 14:16 UTC

`cargo fmt --all -- --check`, `systemd-analyze verify` for both packaged
units, and `git diff --check` pass. `cargo audit --no-fetch --deny warnings`
passes for the **229 locked crate dependencies** against the locally cached
RustSec advisory database (commit `2295466c`, last updated **2026-10-30**); it
found no advisory affecting the lockfile in that snapshot. This is not a
current online advisory refresh or a binary dependency scan. No verified
`images.json` or qcow2 guest image exists under `target/readiness/vms`, so
isolated VM qualification is still unavailable here. Root storage remains
**14 GiB free / 93% used**. No release build or package qualification ran.

### Live CPU-bound no-progress recheck — 2026-11-01 14:17 UTC

The latest read-only status still reports **121,282 pending updates**, dirty and
not caught up, no active scan, and no write error. A five-second service cgroup
sample measured **4.876 CPU-seconds consumed in 5 seconds**, with **1 of 50
periods throttled**. The sole Dux thread was runnable (`wchan` reports running),
and CPU PSI remained low; this is evidence of a userspace CPU-bound no-progress
condition, not proof of the internal hot function. No safe source-matched
profiler or symbols are available for the installed executable. No signal,
restart, profile, or index mutation was attempted.

An online `cargo audit --deny warnings` attempt exited successfully and found no
affected locked crates, but the local RustSec DB commit did not change from
`2295466c` dated October 30. Treat the result as an October 30 advisory
snapshot; the remote database refresh remains unverified.

### Current-source Rust suite attempt deferred at host-pressure threshold — 2026-11-01 14:19 UTC

A current-source `cargo test --offline --locked --all-features` was started in a
systemd scope capped at **10% CPU, 2 GiB memory, no swap, and IOWeight 10**, with
build/temp output under `/dev/shm`. A concurrent unrelated Go race-test/link
workload was present; host CPU PSI rose to **22.79% avg10**, crossing the
predeclared **20% stop threshold**. I stopped only the Rust test scope; it exited
143 after compiling dependencies and before compiling the Dux crate or running
any tests. The attempt's partial target directory was removed. This is
**incomplete, not a test failure or pass**. Production remained active (PID
4093354, zero restarts), dirty/not caught up with **121,521 pending**, **9,297
seconds behind**, no scan, and no write error. Root remained **14 GiB free**;
CPU PSI fell to **12.85% avg10** in the immediate post-stop sample. The current
Rust source remains unqualified.

### Orphaned qualification-VM tmpfs cleanup — 2026-11-01 14:23 UTC

Inspection found **30** numeric per-run VM directories under the dedicated
`/dev/shm/dux-watch-unit-vm` and `/dev/shm/dux-qualification-vm` roots, all from
October 29–30. No QEMU process or corresponding active systemd guest unit was
running. Several legacy directories were root-owned mode **0755**, so the
existing strict cleanup helper correctly refused them. The VM controller now
requires `systemctl is-active` to confirm the guest is stopped before a new
ephemeral run, then removes only numeric per-run directories under its exact
tmpfs roots. It closes legacy root-owned directories to mode 0700 first and
fails closed on writable, symlinked, non-directory, or foreign-owned entries.

Focused qualification-VM tests pass **26/26**, including preservation of
non-numeric evidence, symlink rejection, and legacy permission handling. Using
the same verified cleanup helper after confirming no QEMU or active guest unit
removed the **30 stale run directories**, reclaiming **2,979,688,448 bytes** of
tmpfs (free space rose from **2,777,587,712** to **5,757,276,160 bytes**).
`/dev/shm` now reports **59% used**. The root filesystem also changed from 14
GiB/93% used to 16 GiB/92% used during this interval; attribution for that root
change is unknown, since tmpfs cleanup alone does not explain it.

The full Python suite was not rerun after these changes. A fresh Rust suite
attempt remains stopped before compiling Dux because host CPU PSI exceeded the
20% guard during a concurrent unrelated Go race test. The production daemon
remains active, dirty and not caught up; cleanup did not touch its database or
service.

### Second capped Rust-suite retry — 2026-11-01 14:25 UTC

After CPU PSI temporarily fell below the threshold, retried
`cargo test --offline --locked --all-features` with **10% CPU, 2 GiB memory,
no swap, IOWeight 10**, one Cargo job, and output in a separate `/dev/shm`
directory. CPU PSI rose to **23.78% avg10** before the Dux crate compiled; the
scope was stopped and exited **143**. This remains an incomplete build, not a
Rust test result. The scope is inactive, no Cargo/rustc child remains, and only
a **12 MiB** partial dependency cache remains at
`/dev/shm/dux-current-source-rust-20261101b` for a possible later retry. The
production service remains active with no restart, dirty/not caught up, **125,640
pending**, no active scan, and no write error. The host CPU PSI sample after
stopping was **25.76% avg10**; no more compilation was started.

### Live host pressure and Dux backlog recheck — 2026-11-01 14:26 UTC

The specific concurrent workload `go test -race -count=1 ./agent/core
./agent/tunnel` is still active (PID 477166) and has spawned multiple Go compiler
processes. At **14:26:51 UTC**, host load average was **33.70 on 12 CPUs** and
CPU PSI some avg10/avg60 was **73.95% / 59.78%**. Production remained active
without restart, but status was dirty/not caught up with **125,934 pending**,
**9,726 seconds (~162 minutes) behind**, no scan, and no write error. Counters
continued to move; this does not establish exactness. The Rust scope is
inactive. No compilation or test was started under this pressure.

### Major production-readiness blockers — 14 active blockers and 1 resolved item across nine gates

These are release blockers, **not 15 confirmed source bugs**. The installed
binary's exact source snapshot is unavailable, so its CPU and scan-trigger
failures cannot be assigned to the current worktree without further evidence.

| ID | Blocker and evidence | Exit condition |
|---|---|---|
| B01 | **Live index is untrustworthy.** At 15:01:03 UTC on Nov. 2 production was dirty/not caught up with **321,287 pending**, **98,177.356 seconds behind**, and **502,309 userspace updates dropped**. JSON reports **no scan in progress**, `write_error=null`, and last scan age **99,358 seconds**. Since 14:56:36, pending rose **3,587** while **896** additional updates were committed; behind time increased about **265 seconds** over 267 seconds, so the daemon remains behind its live event stream. `watch.status` v3 has no kernel-overflow counter; prior candidate code incorrectly exposed a compatibility default of zero. Current source now represents the v3 count as unavailable, but release 0.6.3-62 still needs a rebuilt binary with this fix. Journal review found **57 explicit fanotify-overflow warnings today**, latest at **14:21:11 UTC**. No stable exact filesystem comparison exists. The SQLite `meta.schema_version` is **5**. | Recover, drain, independently compare the complete indexed tree/accounting, and qualify distinct user-space drop and kernel-overflow recovery paths. |
| B02 | **Kernel event loss has recurred.** Journal review found **57 explicit fanotify queue-overflow warnings since midnight UTC Nov. 2**, latest at **14:21:11 UTC**. The v3 watcher status cannot count overflow episodes; separately, it records **502,309 known userspace updates dropped**. Exact recovery after either loss path is not established. | Demonstrate overflow visibility and exact recovery after fault-injected queue overflow on the final binary; reestablish an exact live baseline. |
| B03 | **Installed CPU behavior is source-unmatched and contradicts its help.** `pidstat` measured R16 at **100% of one CPU averaged over five seconds at 13:44 UTC**; at **14:46:02 UTC**, Dux used **94.2% process CPU** while no scan was active and Go compilation was concurrently using up to **387% CPU**. A five-second `/proc` thread sample beginning **14:46:51 UTC** measured the daemon main thread at **99.8% of one CPU**; status still showed no active scan and 317,486 pending. This confirms the event/main thread is CPU-saturated on the installed binary, but the source mismatch prevents attribution to current code. A `perf stat` sample at 14:47:27 measured 3.005 seconds of task-clock across 3.002 seconds elapsed (1.001 CPUs), 11.51 billion cycles and 21.39 billion instructions. A 49 Hz five-second profile lost no samples but reported only raw addresses because the installed static-PIE executable is stripped and has no `.symtab`/debug sections; the hot function cannot be identified from this artifact. The standard build-ID debug path is absent, and none of the five retained `dux.debug` files has the installed build ID `2e2d4557ac578ce614c1b48cfb6b53a389b27330`. Earlier samples also showed 100%. Systemd's control drop-in sets `CPUQuota=100%` (the packaged unit says 50%). Installed `/usr/bin/dux` is 0.6.3 R16, SHA-256 `9433d56d…4e87f64`; its help explicitly promises a hard cap with a 25% default, but measured usage reached the cgroup ceiling. Exact installed source is unavailable. Current source has a different governor contract; its exact-binary event-burst behavior has not been requalified. | Reproduce with an exact binary/source identity; establish whether the help or governor is wrong, then pass event-burst, backlog, and overflow budgets with matching symbols on an isolated host. Reconcile configured quota and intended cap. |
| B04 | **Automatic recovery remains unresolved.** At 14:35:36 systemd reported “Reconciliation deferred; waiting for normal host pressure” and no scan was active. At 14:36 host load was about **2 on 12 CPUs**, available memory **15.3 GiB**, root free space **11 GiB**, CPU PSI some avg10 **0.54%**, I/O PSI some avg10 **0.28%**, and memory PSI some avg10 **0.28%**; these readings are below current-source admission thresholds. By 14:38, an unrelated XGen Go compile/vet workload was active and host CPU PSI some avg10 reached **61.68%**, so a later deferral would be expected. No scan was active and status remained deferred; the last Dux deferral log was at **14:34:44**, so the systemd status text may be stale and does not reveal the current internal pressure reason. Installed R16 is source-unmatched. The unreadable `/run/dux-ext4-inspection` test mount remains a possible scan contaminant; causality is unproven. | Reproduce with exact binary/source identity, remove or deliberately exclude stale test mounts, observe request → scan start → install → queue replay, and verify error/timeout behavior without repeating requests blindly. |
| B05 | **Production schema migration is unqualified.** The live DB is v5; current source declares v6, which requires a full-root atomic rebuild. A protected online backup of the live v5 DB was created at `/root/dux-prequalification-20261102T1503Z.sqlite`: **755,593,216 bytes**, **184,471 pages**, `quick_check=ok` and later read-only `PRAGMA integrity_check=ok`, SHA-256 `21bc898fd23496fe257ab20d28a9a2b9de55e6cb86a94781ab43e556f0a28425`, file/manifest mode 0600. Its source was still dirty with **321,482 pending**, **502,309 drops**, and no active scan; this is a transaction-consistent DB snapshot, not an exact filesystem baseline. A previous v5 backup with **2,151,205 inode rows** passed integrity; release 0.6.3-61 rebuilt a tmpfs copy against the small, different `/home/dux/.github` root, producing v6 with exact four-edge coverage. Neither measures a `/` scan, same-root growth-history transfer, migration space/runtime, rollback, or exact recovery. | Rehearse v5→candidate migration from a verified backup on a production-sized isolated host, including disk, runtime, interruption, rollback, and exactness checks. |
| B06 | **Mount aliases are not represented in the live schema.** The v7 path graph is built only in private scan staging; queries, event updates, and migration still use v6 inode-parent edges. | Publish and qualify path-instance identities, directory totals, event semantics, queries, cycles, and duplicate-mount exactness as one schema-v7 design. |
| B07 | **No production release is qualified.** On pinned Rust **1.99.1**, the current source with the telemetry fix passed the all-feature Rust suite **256/256** at 14:14 UTC. Full Python discovery passed **249/249** on Nov. 2 in 97.799 seconds under a 5% CPU / 1 GiB / no-swap scope, including 46 package lifecycle tests and 12 backup-helper tests. Static-musl release **0.6.3-62** at `target/readiness/current-source-release-20261102T1320Z/` is bound to source SHA-256 `d0bfef3c0838fc3f29916d60e61d1dc93a4900650a0bf42b86c53739969c4cce`; executable SHA-256 `e878cb4c5e82c1055977cc4af1cf60dc16a41e4ca4c3f7418920e8976384a43f`. Source handoff `current-source-handoff-20261102T1416Z` captured tree SHA-256 `0d130f999fdc03fe0dda6190660f24c3d993777414ac3182f8c309feb6a0c9b9`; it differs from release-62 in `src/util.rs`, `src/main.rs`, `src/query.rs`, `src/tui.rs`, and documentation. Its member hashes verify, but newer documentation updates may make it an older snapshot. Release-62 predates the legacy-overflow telemetry fix and is stale for promotion. DEB/RPM payloads match the manifest's helpers and service units, the standalone candidate hash matches both package binaries, and package checksums verify. Candidate E2E scans matched four nested normal-filesystem edges exactly; the pseudo-filesystem hardlink fixture passed with `--include-pseudo`. After the telemetry fix, the current all-feature Rust test suite passed **256/256** on Nov. 2 at 14:14 UTC (99.45 seconds test time, one build job, 20% CPU quota, 2 GiB memory cap, no swap). This is debug unit-test evidence; it does not close release, package, migration, scale, endurance, or canary gates. The latest upstream advisory database scan reported zero lockfile vulnerabilities and zero candidate-binary findings. No installed-package lifecycle, exact-candidate event/overflow/scale matrix, 72-hour endurance, 24-hour canary, signing, or owner acceptance is complete. As of 15:02 UTC there is no active QEMU/libvirt guest and no qcow image under `target/readiness`; `/dev/kvm` and QEMU binaries are present, but qualification guest directories in `/dev/shm` are empty and only 1.7 GiB is free there. A same-source **fault-enabled binary with a different hash** survived two test-injected kill boundaries against disposable copies of a v5 backup; this is limited crash evidence, not a release gate. Root had **7.4 GiB free / 96% used** at 13:38:41 UTC, but space and host load fluctuate. | Rebuild and requalify from a source snapshot that includes current docs, then qualify exact package install/upgrade/rollback/removal, candidate fault and scale gates, production-root migration, 72-hour endurance, and 24-hour canary; add signing and owner acceptance. |
| B08 | **Installed artifact provenance is partial, not absent.** The retained 0.6.3-16/R16 build manifest binds the live binary SHA-256 to source-tree digest `3fa91992…78224cc8` and records 57 inputs from a dirty worktree; its package payload matches `/usr/bin/dux`, but the exact source archive and matching symbols are unavailable. The latest hash-verified unqualified source handoff is recorded in the audit log; it contains no binary. New source-bound 0.6.3-59 artifacts include matching symbols and an audited build manifest, but they predate QUAL-231 and do not reconstruct the installed R16 source. | Retain/reconstruct the exact installed binary source/symbols, and retain signed/reproducible provenance for the final qualified candidate. |
| B09 | **Current-binary fault matrix is incomplete.** A same-source fault-enabled binary (SHA-256 `18d43c094eb1e9378fb312340dec84c673bc0b9d71f2eb6239cf17f9de20537f`) was SIGKILLed at `scan.install_started` and after one page at `scan.install_midway`; both disposable v5 DB copies reopened with all **2,151,205 inode rows**, schema v5, and `integrity_check=ok`. This does not qualify the production binary/package and does not cover overflow, OOM, WAL, pressure, coverage, retries, or full-root recovery. | Pass the complete fault/recovery matrix against the exact final binary and package. |
| B10 | **Same-binary scale/performance budgets are open.** Historical 100k/1m/10m results use different source or binary hashes; no complete current-candidate result exists. | Pass the full 100k/1m/10m correctness, CPU, memory, query, and TUI budgets on one hash-bound candidate. |
| B11 | **Installed package lifecycle is not qualified.** The current package-helper/lifecycle Python module passed **46/46**, and the full Python harness passed **249/249** on Nov. 2; the full run used a 5% CPU, 1 GiB, no-swap transient scope and peaked at 185 MiB. Coverage includes mode-000 ownership transfer and SQLite sidecar symlink/hardlink rejection. Installed-package upgrade/rollback/removal remains untested. The only 0.6.3-59 DEB/RPM predate those helper fixes and are stale. A 14:49 UTC effective-unit recheck confirms live drift from the current packaged source: `SecureBits=0`, `LimitCORE=infinity` / soft 0, a narrower syscall filter, and effective `CPUQuota=100%`; the current package unit syntax passes `systemd-analyze verify`. | Rebuild current DEB/RPM; pass clean install, upgrade, rollback, permissions, readiness, and removal tests on Debian/Ubuntu and Rocky; verify effective unit properties after installation. |
| B12 | **Residual privilege needs owner sign-off or reduction.** Rechecked at 14:49 UTC: the live process runs as root with exactly `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`; its effective systemd unit scores **5.1 MEDIUM**. It has `NoNewPrivs=1`, `SecureBits=0`, `LimitCORE=infinity` (soft 0), and only a three-syscall deny filter; the checked-in unit has stronger `SecureBits`, core-limit, and syscall-group settings. The database is 0640 root:dux. These controls do not remove the inherent root/`CAP_SYS_ADMIN` risk. | Obtain explicit risk acceptance or qualify a design that reduces root/capability exposure without losing filesystem coverage. |
| B13 | **72-hour endurance is not complete for the final candidate.** Interrupted or shorter historical soaks do not count. | Complete the hash-bound 72-hour run with exactness, reader, mutation, restart, integrity, and resource checks. |
| B14 | **24-hour production canary has not started.** The live index is not a stable exact baseline and rollback/candidate evidence is incomplete. Root had **11 GiB free / 94% used** at 15:01:03 UTC. This remains below the documented **10 GiB workspace + 8 GiB reserve** qualification preflight. Headroom fluctuates by multiple GiB within minutes and must be rechecked immediately before any build. | Only after preceding gates pass and disk headroom meets the measured workspace-plus-reserve requirement, run the 24-hour canary with exact baseline, monitored rollback, and owner sign-off. |
| B15 | **RESOLVED 2026-11-03: orphaned test filesystem removed.** The read-only `/run/dux-ext4-inspection` mount on `/dev/loop0` came from the deleted sparse test image `/tmp/dux-scale-f2azn2f8/filesystem.img`; Dux was its only process holder. Dux shut down gracefully and flushed before a normal `umount`; `/dev/loop0` was then normally detached. `findmnt` now resolves the path to `/run` tmpfs and the loop mapping is absent. On restart Dux reported two filesystems (previously three) and completed a fresh scan of **1,234,823 files, 111,289 directories, 89.64 GiB, zero errors**. The current index is still not caught up: at 12:00:14 it had **841 pending paths**, so this closes the stale-mount item but not B01/B02 exactness gates. No lazy unmount or forced detach was used. | Closed for the orphan test mount; retain B01/B02 recovery and exactness verification as separate open gates. |

**Live evidence refresh — 2026-11-02 15:33 UTC:** the R16 status remained dirty
and not caught up, with **337,556 pending updates**, **100,145.736 seconds
behind**, **502,309 userspace updates dropped**, `write_error=null`, no active
scan, and no changes in the event-seen/resolved/committed counters since
15:32. Journal entries record additional fanotify overflow and memory-pressure
pause warnings at **15:25:04 UTC**. This refresh supersedes the older B01/B02
time samples above; the exit conditions are unchanged.

### Fail closed on future scan-progress mtimes — 2026-11-01 14:28 UTC

Code review found that `read_scan_progress_for` treated a future file mtime as
age zero: `SystemTime::elapsed()` returned an error and the old fallback
converted that to a fresh scan. Missing mtimes were handled the same way. The
reader now returns no progress unless it can compute a non-negative age of at
most ten seconds. Added a regression that writes valid progress then moves its
mtime sixty seconds into the future and requires it to be ignored.

`rustfmt +1.99.1 --check --edition 2021 src/util.rs` and `git diff --check`
pass. The Rust regression is **not compiled or run**: host load was **13.97 on
12 CPUs**, CPU PSI avg60 was **22.55%**, and an unrelated Go race test remained
active. Production status at 14:28 was dirty/not caught up with **125,986
pending**, **9,796 seconds behind**, no scan, and no write error. The installed
daemon was not changed; current source remains unqualified.

The nine gate groups remain the release accounting categories; this 15-item
register splits their distinct observed failures and required evidence. No item
is closed by a unit-test count or a historical artifact from another hash.

### Recheck current artifact identity, live recovery, and build headroom — 2026-11-01 14:28 UTC

The installed service is package `dux 0.6.3-16`, running `/usr/bin/dux` with
SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`.
Its version string reports commit prefix `a42739776bbd+modified`; the exact
source snapshot and matching symbols for that installed binary are unavailable.
The installed database is schema v5; the current worktree declares schema v6.

The latest read-only `dux --json status` sample reports dirty/not caught up,
**126,022 pending**, **1,878,904 seen / 446,851 resolved / 14,976 committed**,
**9,864 seconds behind**, no active scan, and `write_error=null`. The service
status is “Reconciliation deferred; waiting for normal host pressure.” Its log
contains additional fanotify overflow warnings at 13:27, 13:32, and 14:22 UTC.
The main process remains runnable at about **99% of one CPU**. This is live
recovery evidence, not a source-matched diagnosis or exactness proof.

The already-built `0.6.3-58` artifact is not the current worktree: its manifest
binds commit `a42739776bbd6ef30441936e4d5b28574b971344`, source-tree SHA-256
`b9b82da73beb85eed65d08a39bc0399546dc700993cfcf375084977f1ffbde7d`, and
binary SHA-256 `85009675f502c232b80374a524eb9d98eed97f96d1b603c7e26e848aae89104d`.
A direct manifest-to-worktree comparison finds **23 changed source files**,
including runtime files `main.rs`, `scan.rs`, `store.rs`, `tunnel.rs`, `util.rs`,
and `watch.rs`. Therefore this older package cannot qualify the present source.

At the sample time, root had **17 GiB free** and `/dev/shm` **5.4 GiB free**;
`target/` already occupied **15 GiB**, including **8.6 GiB** of retained
readiness evidence. Host CPU PSI some avg10 was **11.43%**, while an unrelated Go
test/compiler workload and the production daemon were active. No current-source
build was started: earlier similarly capped attempts crossed the established
20% PSI stop threshold, and available evidence does not establish a safe,
source-matched output space. The isolated VM image/manifest and reachable VPS
are still unavailable. No production service, executable, database, or index
was changed. Current-source Rust build/tests and release qualification remain
open.

### Current-source all-features test stopped after PSI guard — 2026-11-01 14:31 UTC

A third current-source attempt used the existing Cargo cache with
`cargo test --offline --locked --all-features`, one Cargo job, and a dedicated
systemd scope limited to **10% CPU**, **2 GiB memory**, no swap, and IOWeight 10.
It reached `Compiling dux v0.6.3 (/home/dux)`, but host CPU PSI some avg10 rose
from **3.42%** before start to **34.47%**, above the predeclared **20% stop
threshold**. I stopped only `dux-current-source-tests-c.scope`; the command
exited **143**. The Dux crate compile did not complete and no Rust tests ran.
This is an interrupted attempt, not a failure or pass. The scope is inactive,
with no Cargo or rustc process remaining.

A separate Go migration/test workload became CPU-heavy during the attempt; the
production Dux daemon also remained active at about one CPU. The post-stop host
sample had CPU PSI some avg10 **37.53%**. Root had **17 GiB free** and `/dev/shm`
**5.4 GiB free**. Production remained active with zero restarts, dirty/not
caught up, **126,144 pending**, **9,986 seconds behind**, no scan, and no write
error. Its systemd status read “Mount topology changed; reconciling visible
paths.” No production service, executable, database, or index was changed.
The current-source binary and all test results remain unqualified.

### Fourth current-source all-features test attempt stopped at PSI guard — 2026-11-01 14:34 UTC

Retried `cargo test --offline --locked --all-features` with the warm target,
one Cargo job, and a dedicated systemd scope capped at **5% CPU**, **2 GiB
memory**, no swap, and IOWeight 10. Host CPU PSI some avg10 was **6.71%** before
start; it rose to **26.97%** within ten seconds, exceeding the established
**20% stop threshold**. Only `dux-current-source-tests-d.scope` was stopped;
the command exited **143** without reporting Dux compilation completion or
starting tests. This is incomplete, not a failure or pass. The scope is
inactive with no Cargo or rustc process remaining.

At **14:34:32 UTC**, after the stop, CPU PSI some avg10 remained **26.17%**.
Production stayed active with zero restarts but remained dirty/not caught up:
**127,201 pending**, **10,184 seconds behind**, no active scan, and
`write_error=null`. Event totals were **1,944,891 seen / 455,124 resolved /
15,616 committed**. The service status again read “Reconciliation deferred;
waiting for normal host pressure.” Root had **16 GiB free** and `/dev/shm`
**5.4 GiB free**. No production service, executable, configuration, database,
or index was changed. Current-source Rust build/tests and live exact recovery
remain open.

### Production event backlog surge during external Vitest run — 2026-11-05 02:35 UTC

At **02:35:11 UTC**, read-only revision 73 status reported **46,806 pending
updates**, up from **950** at 02:33:56, while the external test runner advanced
from the Go suite to `npm exec vitest run`. Over that interval, Dux's event
counter advanced by about **161,000**, resolved events by about **82,000**, and
committed updates by about **12,900**; indexed nodes fell by about **36,000**.
The overflow counter remained at **215**, but the index was still dirty and
about **25,731 seconds** behind with the automatic retry circuit open. Load1
was **7.20**, CPU PSI `some avg10` **2.41%**, and I/O PSI `full avg10` **3.75%**.
The host test chain was still active. This shows substantial backlog and index
churn during concurrent tests even without a new overflow notification in that
sample; it does not isolate the test runner as the sole cause. Evidence SHA-256
`10af8443fdd21c526d46bd70dd528ce2bebe785b74c2915f8b91776763ce17e9`:
`target/readiness/current-source-qual-20261105-c71e822/live-backlog-surge-0235Z.json`.
No service, package, or database change was made. Recovery scan and exact-HEAD
release build remain deferred until host test activity ends and both event
rate and backlog settle.

### Production rescan attempt and bounded-retry outcome — 2026-11-05 02:48 UTC

After the Go/Vitest test process appeared idle and the overflow count had held
at **215** for several minutes, the installed CLI issued the documented
single-writer command `/usr/bin/dux scan /` at **02:38:49 UTC**. The daemon
stayed active as PID **423219**; no restart or second writer was used. The
first baseline finished at 02:42 with **1,253,196 files**, **116,925
directories**, **1,370,121 path instances**, **110.71 GiB** visible bytes, and
zero traversal coverage issues. It nevertheless recorded `lost_events=true`
and scheduled the one bounded retry because unresolved fanotify identities
arrived during the walk. During that attempt the live WAL reached
**358,596,592 bytes**, over its 256 MiB budget; event writes paused, then
resumed after checkpoint recovery at **134,217,728 bytes**.

The retry started at 02:43 and finished at **02:45:43 UTC** with **1,269,578
files**, **117,986 directories**, **1,387,564 path instances**, **112.49 GiB**
visible bytes, and zero traversal coverage issues. It also recorded
`lost_events=true`; the automatic circuit opened after **2** failed scans.
Its WAL reached **271,520,392 bytes**, and writes resumed after checkpoint
recovery to **134,217,728 bytes**. At 02:47:45, read-only status still showed
`dirty=true`, `automatic_reconciliation_paused=true`, **878 pending updates**,
about **26,485 seconds** behind, **236** kernel overflow notifications, no
active scan, and no write error. An external Go test was still active. The
scan client was interrupted after the daemon had definitively opened its
circuit; that affected only the waiting CLI, not the daemon. Evidence bundle
SHA-256:
`accd923aa3aaac69143054a8ffd888ca4acf8f51a88fd9d4a433af0fa1d9d720` at
`target/readiness/current-source-qual-20261105-004c267/production-rescan-two-failures-0246Z.json`.

This recovers and installs a fresh baseline but does **not** certify current
index exactness: event loss recurred during both scans and later overflows
raised the count from 215 to 236. Production recovery remains incomplete.
Wait for the external churn to end, then require stable queue and overflow
samples before the next operator scan. Package lifecycle, current-source
release rebuild, and post-recovery exact filesystem comparison remain open.

### Fifth current-source all-features test attempt stopped at PSI guard — 2026-11-01 14:35 UTC

A fifth attempt used `cargo test --offline --locked --all-features` with the
warm target, one Cargo job, and a dedicated scope capped at **5% CPU**, **2 GiB
memory**, no swap, and IOWeight 10. CPU PSI some avg10 was **2.37%** immediately
before start and rose to **23.15%** within ten seconds. I stopped only
`dux-current-source-tests-e.scope` at the established **20%** threshold; it
exited **143** without output indicating Dux compilation or test execution.
This is not a build/test failure or pass. The scope is inactive with no Cargo
or rustc process remaining. PSI remained **24.17% avg10** after the stop, when
no Go compiler was present in the process list; Dux itself remained near one
CPU. The cause of the host-wide PSI rise is not attributed to the test alone.

At **14:35:31 UTC**, production was active with zero restarts, dirty/not caught
up, **127,572 pending**, **10,245 seconds behind**, no scan, and
`write_error=null`. Event totals were flat from the 14:34 sample; the daemon
was still runnable near one CPU. Root had **18 GiB free** and `/dev/shm` **5.4
GiB free**. Its database and service were not changed. Current-source build,
Rust tests, exact recovery, and isolated release qualification remain open.

### Current-source all-features Rust unit suite passed — 2026-11-01 14:39 UTC

The current worktree compiled successfully with
`cargo test --offline --locked --all-features`, one Cargo job, under a dedicated
systemd scope. The initial Cargo-driven run used CPU 50%, memory 2 GiB, swap
disabled, IOWeight 10, and CPU affinity 7; it built the test binary in **11.19
seconds** and discovered **250 tests**, but host CPU PSI later reached 31.46%
avg10 during test execution, so that scope was stopped before its summary.

After PSI recovered, ran the already-compiled test binary directly on CPU 7 with
one test thread under the same memory/swap/IO constraints. Result:
**250 passed, 0 failed, 0 ignored**, **64.80 seconds**. Binary SHA-256 is
`b1fd43033c645b1ea313865b1cfa0bdd204164408fe9b9dedf2bc54e3ab5e9a5`. This
includes regressions for future scan-progress timestamps, daemon-rescan timeout,
inode-number reuse, repeated identities without birth time, v5 history migration,
path aliases/cycles, fault/pressure handling, subprocess paths, and accounting
rollback. Host CPU PSI avg10 was 0.38% at completion; the systemd test scope is
inactive. This qualifies the current Rust **unit** suite only; it does not close
integration, package, scale, soak, production exactness, or canary gates.

### Current-source release packaging attempt stopped at PSI guard — 2026-11-01 14:40 UTC

Started `scripts/build-release.sh target/readiness/current-source-r59-20261101`
for package revision 59 with `CARGO_NET_OFFLINE=true`, one job, CPU affinity 7,
CPUQuota 50%, MemoryMax 4 GiB, swap disabled, and IOWeight 10. The script began
building the auditable static-musl release and compiled several dependencies.
Host CPU PSI some avg10 reached **20.92%**, above the 20% stop threshold. Only
the release-build scope was stopped; exit 143 while still compiling dependencies,
before the Dux crate or any package artifact was produced. Its isolated cache
at `target/dux-auditable` now occupies **488 MiB** and can support a later retry.
This attempt is incomplete, not a build failure or pass. No production service,
binary, database, or index changed.

At **14:40:30 UTC**, production remained dirty/not caught up with **130,610
pending**, **10,546 seconds behind**, no active scan, and `write_error=null`.
Current-source release packaging, exact live recovery, scale/fault package
qualification, endurance, and the production canary remain open.

### Third current-source static-musl release build stopped at PSI guard — 2026-11-01 14:43 UTC

Retried the complete `scripts/build-release.sh` for package revision 59 with
`CARGO_NET_OFFLINE=true`, one Cargo job, CPU affinity 7, CPUQuota **10%**,
MemoryMax **4 GiB**, swap disabled, and IOWeight 10. The partial
`target/dux-auditable` dependency cache was reused. Host CPU PSI some avg10 rose
to **27.21%** during the attempt, above the 20% stop threshold. I stopped only
the release-build scope; it exited **143** while compiling dependencies
(`signal-hook-mio`, `digest`, and `derive_more` were among the current jobs),
before compiling Dux or producing any package. No release output exists in
`target/readiness/current-source-r59-20261101/`; the reusable partial dependency
target is now **519 MiB**. The attempt is incomplete, not a build failure/pass.

At **14:43:07 UTC**, production remained active with zero restarts, dirty/not
caught up, **131,914 pending**, **10,702 seconds behind**, no scan, and
`write_error=null`. Host PSI was still **27.76% avg10** after stopping. Root had
**18 GiB free**, `/dev/shm` **5.4 GiB free**. No production service, executable,
configuration, database, or index was touched. A reachable isolated builder is
still needed to finish static-musl packaging and test the package lifecycle.

### Refreshed Rust dependency and binary audit — 2026-11-01 14:44 UTC

- Ran `cargo audit --deny warnings` against Cargo.lock SHA-256
  `fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`.
  `cargo-audit 0.22.2` exited **0**, scanning **229 dependencies**, with **0
  vulnerabilities and no warnings**. Confirmed local RustSec DB HEAD
  `2295466c4e9df26647fd67d8fa1c705ac2fc7f81` equals upstream GitHub `main` at
  the time of the check; the upstream database's latest commit is dated Oct 30
  and contains 1,395 advisories. The fresh-fetch command returned success, and
  a subsequent `--no-fetch` JSON scan against the verified same commit also
  returned success.
- Audited installed `/usr/bin/dux`, SHA-256
  `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. It has
  no `.dep-v0` section and was not built with cargo-auditable. The binary scan
  recovered only **44 dependencies** and found no known advisories among those;
  Cargo Audit explicitly warns the report is incomplete. This is not a full
  installed-binary dependency audit.
- The older R58 static candidate SHA-256
  `85009675f502c232b80374a524eb9d98eed97f96d1b603c7e26e848aae89104d` has
  embedded cargo-auditable metadata. Its scan recovered **202 dependencies**
  with no known advisories and a complete inventory, but R58 is stale against
  current source and does not qualify the present worktree.
- Machine-readable evidence is in `target/readiness/security-audit-20261101/`:
  lockfile JSON, per-binary JSON/terminal reports, and metadata binding binary
  hashes, advisory DB HEAD, and lockfile hash. These results do not cover OS
  packages, source-level flaws, or the unbuilt current release artifact.

At the same **14:44 UTC** read-only production sample, Dux remained active with
zero restarts but dirty/not caught up: **132,148 pending**, **10,796 seconds
behind**, no scan, `write_error=null`. No production files, service, database,
or index changed.

### QUAL-143 Paired read-only production progress and pressure sample — 2026-11-01 14:50 UTC

- Captured two samples 15 seconds apart in `target/readiness/live-host-samples-20261101T1450Z.txt` (SHA-256 `9a28071f7378a26fce6cd9fff66223efe684a1f55162e12fe524c17b1f1821ff`). Host load1 fell **11.35 → 10.12** on 12 CPUs and host CPU PSI some avg10 fell **27.81% → 11.09%**; competing Go compilation/test processes were active. I/O PSI some/full avg10 rose **2.00%/0.69% → 4.83%/3.86%**.
- Production PID **4093354** remained active with zero restarts and one-core CPU quota. Its cgroup consumed about **15.0 CPU-seconds** across the 15-second interval; Dux used about **98.6% CPU**. The status counters remained flat at **2,215,922 seen / 513,193 resolved / 17,536 committed / 138,525 pending**, with **11,125 seconds** behind at the last sample, no scan, and no write error. The commit timestamp advanced by 14 seconds even though the counters did not; this short interval does not establish durable pipeline progress or a root cause.
- The systemd status remains `Reconciliation deferred; waiting for normal host pressure`. The runbook prohibits another scan/restart while queue overflows or host pressure persist. No production service, executable, configuration, database, or index was changed. Keep live recovery/exactness open; continue read-only monitoring and do not treat transiently falling CPU PSI as a recovery gate.

### QUAL-144 Classify the production daemon's CPU-bound no-progress interval — 2026-11-01 14:51 UTC

- A further 25-second read-only sample is preserved at `target/readiness/dux-stall-followup-20261101T1451Z.jsonl` (SHA-256 `eedaf0cbeeb5253dba571edfb0f56425ec68bd3f365dd9d26c103a5a9cff16f6`). Host load1 fell **5.97 → 4.41**; host CPU PSI some avg10 fell **0.33% → 0.02%**. Production PID **4093354** consumed **25.0 CPU-seconds** under its one-core quota. The Dux cgroup's `io.stat` counters did not change.
- Dux status counters remained exactly **2,215,922 seen / 513,193 resolved / 17,536 committed / 138,525 pending**; the last-event-commit timestamp remained fixed while `behind_seconds` grew **11,177 → 11,203**. It remained dirty, not caught up, with no active scan and `write_error=null`. Together with the no-I/O interval this meets the operations runbook's **CPU-bound no-progress incident** classification; it does not identify the loop's source-level cause.
- `/usr/bin/dux` remains SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, Build-ID `2e2d4557ac578ce614c1b48cfb6b53a389b27330`, with no useful Dux function symbols. Kernel `perf_event_paranoid=4` disallows the runbook's short `perf` profile, so profiling was not attempted. The installed artifact's exact source and matching symbols remain unavailable; the current worktree cannot be used to attribute this live binary's CPU loop.
- No production state changed. The runbook says preserve status/checksum/counters and keep the incident open until watcher progress and exact accounting recover. Do not raise quota, scan, restart, or claim recovery from low host PSI alone. This strengthens B03/B04 diagnosis but closes no release gate.

### QUAL-145 Confirm intermittent ingestion progress, backlog growth, and effective CPU quota drift — 2026-11-01 14:53 UTC

- Read-only paired service/host samples 30 seconds apart are preserved in `target/readiness/dux-recovery-followup-20261101T1452Z.jsonl` (SHA-256 `83b431f42b1441a83ee57d1730b310ef8a007f12b43c876a63b3bfa25a6cfc0b`). Host CPU PSI some avg10 rose **27.37% → 33.21%** while unrelated work resumed. Dux consumed **28.3 CPU-seconds** over 30 seconds, with both cgroup I/O counters and watcher counters advancing.
- Over the interval, events seen rose **7,082**, resolved **4,592**, updates committed **128**, and pending increased **3,246** (**138,935 → 142,181**); `behind_seconds` increased **11,267 → 11,297**. The earlier CPU-bound no-progress interval was transient. Current service behavior is **intermittent progress without convergence**; dirty/not-caught-up remains true, there is no active scan, and `write_error=null`.
- Effective cgroup quota is **100%** (`cpu.max=100000 100000`; systemd `CPUQuotaPerSecUSec=1s`), while the installed host fragment `/usr/lib/systemd/system/dux.service` says **50%**. The current worktree package definition `packaging/dux.service` says **100%**, matching the effective systemd control drop-in; this is installed-base/worktree drift, not a mismatch between the current worktree quota and effective quota. `/etc/systemd/system.control/dux.service.d/50-CPUQuota.conf`, timestamped 11:13:50 UTC, is a systemd `set-property` control drop-in setting 100%; no audit entry or evidence here identifies who set it or why. The installed CLI reports `--max-cpu` default **25% of one core**, yet the live process is near **99% of one CPU**. This confirms the production CPU budget is not behaving as its documented in-process target, but the installed binary's exact source is unavailable, so the implementation cause is unresolved.
- No quota, service, configuration, executable, database, or index change was made. Do not remove/alter the control drop-in during this recovery incident; no authority or evidence establishes its intended owner. B03 is confirmed as a production behavior gap; the root cause and remediation still require a source-bound candidate test.


### QUAL-146 Correct quota provenance and observe recovery after the latest fanotify overflow — 2026-11-01 14:56 UTC

- Corrected QUAL-145's ambiguous phrase “packaged unit file”: the installed host fragment `/usr/lib/systemd/system/dux.service` declares **50%**, the current worktree package definition `packaging/dux.service` declares **100%**, and `/etc/systemd/system.control/dux.service.d/50-CPUQuota.conf` sets the effective cgroup quota to **100%**. Thus the effective quota matches the current worktree definition, while the installed base unit differs. The drop-in's creator/rationale and the installed binary's source remain unknown. The installed CLI reports a 25% in-process default; runtime CPU use near 100% remains unexplained and unqualified.
- Journal confirms a fanotify queue overflow at **14:55:13 UTC**, which matches `dirty_since`. Paired samples at **14:56:12** and **14:56:32** are preserved at `target/readiness/dux-overflow-followup-20261101T1456Z.jsonl` (SHA-256 `cde9f50bf9cc86dcd4bb210be718c882ca42b7fc32aedb73240b8a7a9d8511a7`). In the first sample counters were still flat since the overflow. Over the next 20 seconds Dux consumed **19.9 CPU-seconds**, read about **356 KiB**, wrote about **34 MiB**, and processed **4,485 seen / 3,830 resolved / 128 committed** events; pending grew **68** and lag grew **20 seconds**. Host CPU PSI avg10 remained below **0.25%**. This confirms temporary post-overflow no-progress followed by intermittent processing, not exact recovery or convergence.
- Production remains active, dirty/not caught up, with **143,631 pending**, no active scan, `write_error=null`, and zero restarts at 14:56:32. No service/configuration/quota/database/index action was taken. The no-scan/no-restart runbook gate remains applicable; exact live accounting and recovery are still open.

### QUAL-147 Fail closed on missing or malformed daemon pressure telemetry — 2026-11-01 14:58 UTC

- Static review found a verified admission fail-open in `guard::sample`/`Health::level`: unreadable `MemAvailable`/filesystem capacity were represented as negative values that the classifier ignored; missing `/proc/loadavg` defaulted to 0; and floating-point parsers accepted `NaN`, infinity, and negative PSI values, whose threshold comparisons could all be false. An inconsistent caller could also pass `Pressure::Normal` to reconciliation admission despite invalid current health.
- Updated the current worktree so missing required memory/disk/load/CPU-count telemetry marks host health unavailable and classifies it Critical; reconciliation admission independently rejects this state. Load and PSI parsers reject non-finite/negative values; PSI percentages above 100 are invalid and map to Critical. Missing PSI files (`NotFound`) remain optional for older/disabled-PSI kernels, while unreadable or malformed existing PSI files fail closed. This changes current source only; the installed production daemon is untouched.
- Added four Rust regressions for numeric parsing, each required telemetry field, fail-closed classification/admission (including a stale Normal argument), and malformed CPU PSI. **These Rust tests have not been compiled or run.** `cargo fmt --all -- --check`, `rustfmt +1.99.1 --check --edition 2021 src/guard.rs`, and `git diff --check` pass. The analogous Python VM pressure-guard tests pass **26/26 in 0.009 seconds**; they validate the existing VM controller, not the new Rust behavior.
- Release build, Rust regression execution, and broader fault/pressure qualification remain open pending an isolated builder. No production service, binary, index, or database changed.

### QUAL-148 Stop Rust regression build after CPU-pressure guard; production overflow observed concurrently — 2026-11-01 15:03 UTC

- Attempted to compile/run `guard::tests` from the current source using a transient scope with `CPUQuota=5%`, `MemoryMax=2 GiB`, swap disabled, and `CARGO_TARGET_DIR=/dev/shm/dux-guard-target-20261101`. The target is only **15 MiB**; the scope reached about **3.5 CPU-seconds / 248 MiB** and had compiled dependencies but not Dux. It did **not** compile or execute the new Rust tests.
- Stopped the exact scope after host CPU PSI some avg10 reached **20.86%** (15:02:55 UTC); systemd reports it inactive and no cargo/rustc worker remains. Host CPU PSI fell to **7.78%** afterward. Memory PSI stayed at 0%; `/dev/shm` still had **5.4 GiB free**.
- Production journal recorded `fanotify queue overflow — index may have missed events` at **15:02:52 UTC**, during the build; `dirty_since` matches that timestamp. An unrelated `safefs.test` process used ~180% CPU and PostgreSQL work was also active. The evidence does not establish whether the test's tmpfs output, competing workload, or another source caused this overflow. Therefore `/dev/shm` output cannot be assumed safe from this installed daemon's watcher, despite being intended as an isolated path in current qualification procedures. Do not build/test on this production host again during this incident.
- Production remained active with zero restarts but dirty/not caught up, **148,009 pending**, **11,922 seconds behind**, no scan and no write error. No production executable, unit, database, or index was modified. Current-source Rust build/tests require the offered isolated VPS; all release gates remain open.

### QUAL-149 Run exact guard-module Rust tests without Cargo — 2026-11-01 15:04 UTC

- Compiled the exact current `src/guard.rs` with Rust **1.99.1** via `rustc --test` in a 508-byte standalone harness under `/dev/shm`; test output binary also stayed in tmpfs. All **18/18** guard-module tests passed in **0.10 seconds**, including four regressions added in QUAL-147. The isolated scope had CPUQuota **5%**, MemoryMax **512 MiB**, no swap; it exited successfully with host CPU PSI avg10 **4.34%** and memory PSI **0%** after completion.
- Hash-bound report: `target/readiness/current-source-guard-module-tests-20261101.json`, SHA-256 `1b9a10b527a32e06df00db53f694afca00555853ef5f4e5a06eeaef0a100758a`. It records guard source SHA-256 `33ac7776d9242b1874aa209b04e30ed490f55e962a96e0358d3b74f9de946fe3`, harness SHA-256 `f663e98aed2c2773adb231c509bc8956d91541c9bc183984c6e70f6a99b2907f`, and test-binary SHA-256 `9ee0c78f598cc566cd879cd4e3692cd4b37037c0587c2abf6387a68c5a3e1fd6`.
- Scope limit: this compiles the real guard source module but stubs `crate::util::fs_stat` and four fault-injection functions; tests do not call those interfaces. It does **not** compile the full Dux crate, run integration tests, qualify package/runtime behavior, or replace the still-incomplete Cargo/release build. The separate Cargo attempt in QUAL-148 remains incomplete; no further Cargo build was run.
- Production remains active but dirty/not caught up: **149,603 pending**, **12,008 seconds behind**, no scan, no write error, zero restarts at the latest read-only sample. No production service or index mutation occurred.


### QUAL-150 Record another overflow after test workloads ended and refresh live blockers — 2026-11-01 15:06 UTC

- At **15:06:18 UTC**, the production journal logged another `fanotify queue overflow — index may have missed events`; persisted `dirty_since=1793545578` matches that second. The event occurred 3m23s after the isolated guard-module test ended and over three minutes after the CPU-capped Cargo attempt had been stopped. No Dux build/test process was active then. This weakens—but does not eliminate—the hypothesis that our test activity caused the earlier overflow; attribution remains unknown.
- At **15:06:32 UTC**, service PID **4093354** was active with zero restarts, dirty/not caught up, **151,637 pending**, **12,106 seconds behind**, no scan, and `write_error=null`. Counters: **2,600,287 events seen / 549,665 resolved / 19,584 committed**. Last commit timestamp equals the latest overflow second, so activity was intermittent; exactness and convergence remain unproven. No scan, restart, quota change, DB/index change, or executable replacement occurred.
- Refreshed B01/B02/B07 in the 14-blocker register: B01 uses this live status; B02 includes repeated overflow times; B07 records that only the guard module has standalone test evidence, while the full crate/package remain unbuilt. No release gate closed.

### QUAL-151 Refresh production status without touching the service — 2026-11-01 15:07 UTC

- At **15:07:58 UTC**, `dux --json status` remained dirty/not caught up with **152,049 pending updates**, **12,193 seconds behind**, **2,149,864 indexed nodes**, **755,593,216 bytes on disk**, **128 MiB WAL**, no active scan, zero reported dropped events, and `write_error=null`. The status counters were unchanged from the 15:06:32 sample while lag increased by 87 seconds; this interval shows no counter progress, but is too short alone to establish a new CPU-bound incident. Service PID **4093354** remained active with **zero restarts** and was consuming about **99% of one CPU**.
- The latest journal still shows fanotify overflows at **15:02:52** and **15:06:18 UTC**. Root had **15 GiB free (92% used)**; `/dev/shm` had **5.4 GiB free**. No scan, restart, build, backup, quota change, or index mutation was performed. B01 is refreshed; B02/B03 remain open. The prior verified v5 backups remain in `/var/backups/dux-index`; no additional copy was made because a recovery snapshot already exists and additional read/write load is not justified during the active incident.

### QUAL-152 Validate pressure telemetry at the classifier boundary — 2026-11-01 15:10 UTC

- Follow-up source review found that `Health::sample` marked missing required telemetry unavailable, but `Health::level` trusted that marker and did not independently validate public fields. A directly constructed/default `Health` with unknown memory/disk (`-1`), NaN load, or invalid CPU count could therefore classify as Normal and allow a reconciliation if paired with a stale `Pressure::Normal` argument. Updated `src/guard.rs` so classification and its reason string fail closed on missing/invalid required memory, disk, load, and CPU-count values, and on an invalid disk threshold. Added regressions for the default unknown snapshot and seven malformed direct constructions. This closes the interface-level fail-open; the production call path still requires full-crate verification.
- Exact current `src/guard.rs` was compiled with `rustc +1.99.1 --test` using the existing 508-byte harness, which stubs `util::fs_stat` and fault flags. **19/19 tests passed** in 0.14s with `--test-threads=1`, under a transient scope capped at **5% CPU / 512 MiB memory / no swap**. Current source SHA-256: `bda0e7c651655031c9c9556efec3c429b82f74a9925b9f8da7d3b2f5d5d3f8a0`; harness SHA-256 unchanged at `f663e98aed2c2773adb231c509bc8956d91541c9bc183984c6e70f6a99b2907f`; test binary SHA-256 `e0764a0f0789f034309a5ef08a9ad31d25c8daef34f64720eb86a20f41618008`. `rustfmt +1.99.1 --check --edition 2021 src/guard.rs` and `git diff --check` pass.
- Scope remains module-only: the full Dux crate, Cargo tests, integration behavior, package, and live production behavior are not verified by this harness. Production remained active (PID 4093354, zero restarts), dirty/not caught up, with **156,003 pending**, **12,326 seconds behind**, no scan, and `write_error=null` at 15:10:10 UTC. Status counters advanced during the interval; a new dirty timestamp at 15:08:56 was attributed by status to changed mount topology. No build, scan, restart, or service/index mutation was made. B01/B07 refreshed; no release gate closed.

### QUAL-153 Attribute the current CPU ceiling symptom and correct stale quota comments — 2026-11-01 15:11 UTC

- At **15:11:29 UTC**, `ps -L` showed the installed daemon's sole thread (TID 4093354) at **98.7% CPU**. Effective systemd `CPUQuotaPerSecUSec=1s` is **100% of one CPU**; the service remained active with zero restarts and no scan. Between 15:10:10 and 15:11:29, watcher counters and pending work were unchanged (**156,003 pending**) while reported lag rose from **12,326 to 12,404 seconds**. This confirms a contemporaneous main-thread CPU/no-progress interval in the installed service, while its exact source remains unavailable; do not assign the cause to current source based on this observation.
- Current worktree documentation said the packaged CPU quota was 50% in two `src/watch.rs` comments, but `packaging/dux.service` and current README specify a 100% process-wide ceiling. Corrected those comments to say the event-thread target is separate and scan/accounting workers share the one-CPU cgroup limit. This is comment-only; behavior is unchanged. `rustfmt +1.99.1 --check --edition 2021 src/watch.rs` and `git diff --check` pass. No full crate build or test was run.
- B03 remains open: source-matched diagnosis and a controlled event-storm/backlog/overflow budget are still required. No service, quota, binary, database, or index change was made.

### QUAL-154 Rescan current source snapshot for secrets — 2026-11-01 15:12 UTC

- Rebuilt a deterministic snapshot of **124 tracked and non-ignored untracked source files**, then scanned its extraction with Gitleaks default rules (`dir`, full redaction, 5 MiB file cap, 60-second timeout). Result: exit 0, **no leaks found**; 2.30 MB scanned in 547 ms. This source snapshot was captured immediately before adding this qualification entry. Tree SHA-256 `cf0b71ee8f8fbd580318a724ff3bddb5172ea685feba13f8446a76e9b6162a8e`; archive SHA-256 `cf6b22f5cae6bf18eb5a5398131565722ffd8cb948256171b91e03a0281a4acd` (577,932 bytes); empty report SHA-256 `37517e5f3dc66819f61f5a7bb8ace1921282415f10551d2defa5c3eb0985b570`. Preserved archive, manifest, and report under `target/readiness/source-security-review-20261101T1512Z/`; SHA-256 checksums were reverified after copying.
- Scope excludes Git history, ignored files, `PRODUCTION_READINESS_AUDIT.md` (explicitly excluded from release inputs), host configuration, installed binaries, and production secrets outside the source snapshot. This is source-snapshot secret scanning only, not a complete security assessment. No production service or index changes occurred.

### QUAL-155 Run focused backup and source-archive regression suites — 2026-11-01 15:13 UTC

- Python **3.12.3** test suites passed: `test_dux_backup_index.py` **7/7** in 0.200s (including WAL-aware online backup, integrity/hash/mode verification, no-clobber publication, unsafe parent/database/sidecar rejection, schema validation, and disk reserve); `test_dux_source_archive.py` **4/4** in 1.500s (including deterministic bytes/normalized metadata, source-change detection, tree digest binding, and symlink-escape rejection). Total **11/11**. Both ran separately under transient limits **CPUQuota=5%, MemoryMax=512 MiB, MemorySwapMax=0**, with `TMPDIR=/dev/shm/dux-focused-tests-20261101`.
- Exact test inputs: `scripts/dux-backup-index.py` SHA-256 `8ed6ed1b5befa2c6381821b610bf6e3faad37547a296c0c62e0a5f8d217ef726`; its test SHA-256 `fdaaaf8f11b5dc5f3c7a77cf4f9fe29a6a1db065b5c507cd82e28e0d5fc1075f`; `scripts/dux_source_archive.py` SHA-256 `c89f01d8920f1235764e5b0b102c192d3c8f10c690b4737dd0ada0c7bb2f28da`; its test SHA-256 `e2c77affcc7e1b28736ddd8158833715605daa650ee0b7119f8a19413bf9cd00`.
- Post-test read-only check at **15:13:28 UTC**: production PID 4093354 active, zero restarts, dirty/not caught up, **156,317 pending**, **12,524 seconds behind**, no scan, and no write error. Counters did not move during the tests; CPU PSI avg10 was **3.55%**, memory PSI **0%**, and I/O PSI **0.01%**. Disk had **17 GiB free (91% used)**; `/dev/shm` had **5.4 GiB free**. No production database, service, binary, or quota was changed. These helper-suite passes do not qualify the daemon crate, release package, or live index exactness.

### QUAL-156 Stop current-lock advisory audit after host CPU PSI crossed the stop threshold — 2026-11-01 15:14 UTC

- Started `cargo audit --db /dev/shm/dux-advisory-db-20261101 --format json --file Cargo.lock` in a transient user scope capped at **5% CPU, 1 GiB memory, no swap**. Before an audit result was emitted, host CPU PSI avg10 rose above the established **20% stop threshold** (observed peak **55.96%**); interrupted that exact process. Subsequent process inspection confirmed no Cargo Audit process remained. At 15:14:55 UTC CPU PSI avg10 had fallen to **21.31%**, still above threshold. The tmpfs advisory database had reached about **44 MiB**; the incomplete run produced **no vulnerability result**, and no clean audit is claimed.
- Production remained active with zero restarts, dirty/not caught up, **156,492 pending**, **12,610 seconds behind**, no active scan, and `write_error=null` at 15:14:55 UTC. Counters advanced intermittently but convergence and exactness remain unproven. Do not retry Cargo Audit on this host while CPU PSI is above threshold; use a reachable isolated builder or wait for a measured low-pressure window. No production database/service mutation occurred.

### QUAL-157 Deny unused syscall groups while preserving fanotify FID resolution — 2026-11-01 15:17 UTC

- Extended the **candidate package unit** syscall denylist to include `@swap`, `@reboot`, `@raw-io`, `@debug`, and `@cpu-emulation`, in addition to existing `@module` and `@mount`. Review of source and group members found no Dux use of those syscalls. `@resources` remains available for `nice`/`ioprio_set`, and `@privileged` remains available for fanotify and `open_by_handle_at`. The earlier decision to retain `@clock` for `clock_gettime` was based on an incorrect reading of systemd's group: `@clock` contains clock-changing syscalls, while clock reads and sleeps are implicitly allowed. The current unit denies `@clock`; a transient syscall smoke now verifies clock changes fail while `clock_gettime` succeeds. Alert hooks inherit this filter and still need compatibility review in the VPS rehearsal.
- Expanded `scripts/dux-systemd-filter-smoke.py` to probe representative calls in each denied group and verify `open_by_handle_at` still reaches the kernel. Runtime transient-unit smoke passed: **restricted syscall groups denied; open_by_handle_at permitted**. `systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service` exits 0. Offline `systemd-analyze security` score for the candidate package unit improved from **5.1 MEDIUM** (effective installed unit) to **4.2 OK**. Focused package lifecycle/policy suite passed **39/39** in 1.099s under CPUQuota 5%, MemoryMax 512 MiB, no swap, tmpfs test directory; `py_compile` passed. Unit SHA-256 `e3701f0978b278286c3fff9d31a77616d3d4b1182affa6f8991e0acdf824f8bf`; lifecycle policy SHA-256 `429007598dfdf07cf83ca56477d65a8ac7becd651a62ff82af509169fd7f0be7`; filter smoke SHA-256 `30895d37bc3991ef7c95f7acaf4108f97f525f1955b15446de8ee9d81e980ce4`.
- This does not reduce required root/`CAP_SYS_ADMIN` exposure and is not yet qualified across Debian/Ubuntu/Rocky packages or user alert hooks. Production remains on its installed unit; at 15:17:16 UTC it was active with zero restarts, dirty/not caught up, **156,907 pending**, **12,751 seconds behind**, no scan, and no write error. CPU/memory/I/O PSI avg10 was 0.96%/0%/0.12%; root had **17 GiB free (91% used)** and `/dev/shm` **5.3 GiB free**. No live service/index/quota change occurred; B12 remains open.
- Follow-up at **15:17:38 UTC**: production remained active with zero restarts, dirty/not caught up, **156,912 pending**, **12,774 seconds behind**, no scan, and no write error. Host CPU/memory/I/O PSI avg10 was **0.10%/0%/0.23%**; disk remained **17 GiB free (91% used)**. Counters advanced, but the backlog and lag continued to grow. The effective installed unit is still unchanged.

### QUAL-158 Complete current Cargo.lock advisory audit after pressure subsided — 2026-11-01 15:18 UTC

- Rechecked host pressure before retry: CPU PSI some avg10 was **0.15%** at 15:17:57 UTC. The earlier partial advisory database had reached 44 MiB. The fetch completed and a no-fetch confirmation then ran in scopes capped at **5% CPU, 1 GiB/512 MiB memory, no swap**. `cargo audit` **0.22.2** exited **0** against current `Cargo.lock` SHA-256 `fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`: **229 dependencies**, **1,395 advisories in the database**, **0 vulnerabilities**, **0 warnings**. Advisory database commit `2295466c4e9df26647fd67d8fa1c705ac2fc7f81`, dated 2026-10-30T12:12:39+01:00.
- JSON report and manifest are preserved in `target/readiness/cargo-audit-current-20261101/`. Report SHA-256 `b36b975af14f4ba9382064952b4c74f6f9c93630e2693686233a175d0587dd7f`; manifest SHA-256 `5a360b3881afbca824d375c2becfa9543fd8d4d5825fd644f0eb4708b7d8bd16`. This closes only the dependency-advisory check for the recorded lockfile; it does not cover the installed binary, full supply chain, crate tests, or release package.
- At 15:18:59 UTC production remained active with zero restarts, dirty/not caught up, **157,096 pending**, **12,854 seconds behind**, no scan, and `write_error=null`. CPU/memory/I/O PSI avg10 was **2.12%/0%/0.06%**; root had **18 GiB free (91% used)**. No production service or index action occurred; B01 remains open.

### QUAL-159 Re-run loopback SSH tunnel restrictions in both GatewayPorts modes — 2026-11-01 15:20 UTC

- Ran `scripts/dux-ssh-tunnel-qualification.py` with `GatewayPorts=no` and `GatewayPorts=clientspecified`, each in a transient scope limited to **5% CPU / 512 MiB / no swap**, with temp files in `/dev/shm`. Both runs passed all eight checks: unknown/mismatched host keys rejected; verified key accepted; forced command suppresses requested shell; permitted reverse forward carries a real payload; listener remains loopback; unlisted remote listener and local forward are denied. OpenSSH **9.6p1**. Test creates a temporary sshd on `127.0.0.1` only; post-run process inspection confirmed all temporary sshd/ssh processes exited and temp dirs were removed.
- Preserved report: `target/readiness/ssh-tunnel-loopback-qualification-20261101/manifest.json`, SHA-256 `97fc4207c33582fe9ca41c84705b415f6ab664f1d982cb4da07fc5f61c00b465`. It binds test source SHA `c5a228d5e912274bb3c9d06d78d2ba66e6806b6c5ef66d0d98ac73fccc71a19d`, workflow-test SHA `7cf27f0c68d333213de6f0379f12601f7df1250672a7326acab007dbbf490ffa`, and current `src/tunnel.rs` SHA `dc75f7bca03b6975e10e45e6511bbacdf7f81cada81e61c5d2d93981a3f170fd`.
- Scope limit: the live OpenSSH test builds the authorized-keys policy in its Python fixture; it does not invoke the unbuilt current Dux binary. Rust tunnel unit tests and package integration remain pending full-crate/guest qualification. Production at 15:20:29 UTC remained active with zero restarts, dirty/not caught up, **157,020 pending**, **12,944 seconds behind**, no scan, and no write error. CPU/memory/I/O PSI avg10 was 14.07%/0%/0.38%; root had **18 GiB free (91% used)**. No production service, key, config, quota, or index changed.

### QUAL-160 Run the broad host Python suite and offline Rust compile check — 2026-11-01 15:25 UTC

- The constrained broad Python discovery suite passed **209 tests in 18.701s**. It deliberately excluded four live-quota tests that stop a systemd unit, the release-builder CLI suite (11 tests), and one debug-artifact test; this is not the complete Python suite. Earlier output from an unbounded release-builder test run was interrupted and is not counted as a pass.
- `cargo metadata --locked --no-deps` passed. `cargo check --locked --offline --jobs 1` passed for the current source in **26.60s**, under a transient user scope capped at **10% CPU, 2 GiB memory, no swap, 600s runtime**. This is a type-check, not a linked/tested release binary, and it does not qualify packaging or runtime behavior.
- Attempted `cargo test --locked --offline --jobs 1 --bin dux` under **5% CPU, 1.5 GiB memory, no swap, 900s runtime**. Host CPU PSI avg10 crossed the documented **20% stop threshold** (peak observed **22.64%**) while the test build was active; an unrelated Go compiler and the production Dux daemon were also consuming CPU, so attribution is mixed. The exact Cargo process was interrupted and exited 130 before any Rust tests ran. The run is incomplete, not a failure/pass of Rust tests. A preceding `--lib` attempt exited 101 because this package has a binary target and no library target; it made no test changes.
- Retried once after PSI temporarily returned to 1.79%; another unrelated Go compile started during the retry. Stopped the exact Cargo process at **19.64% avg10** before crossing the 20% threshold; it exited 130 before running Rust tests. This retry also provides no Rust test result and confirms on-host cargo tests are unsafe to continue while competing host builds are active.
- After stopping, CPU PSI avg10 returned to **15.00%**; no Cargo/rustc test process remained. Production service stayed active at PID **4093354**, **zero restarts**. Read-only status at 15:25:52 UTC: dirty/not caught up, **158,872 pending**, **13,268 seconds behind**, no scan, `write_error=null`, and `events_dropped=0`; root had **17 GiB free (91% used)**. No production service, quota, index, or configuration mutation occurred.
- Gate remains open for full Rust tests, linked release build, reproducible artifact provenance, package/upgrade/rollback tests, v5→v6 migration on a copied production index, exactness and fault matrix, scale and endurance qualification, and canary. Current source is still not a production-qualified candidate.
- Post-stop recheck at 15:27:08 UTC: service remained active with zero restarts; `dux --json status` showed **159,108 pending**, **13,342 seconds behind**, dirty/not caught up, no scan, no write error, and zero reported dropped events. CPU PSI avg10 was **12.98%**; root had **17 GiB free (91% used)**. No test process remained.

### QUAL-161 Static packaging checks and refreshed disk inventory — 2026-11-01 15:29 UTC

- `cargo fmt --all -- --check`, `shellcheck` on the release builder and Debian helper scripts, `bash -n`, `dash -n`, `systemd-analyze verify` for both units, Python `compileall`, and `git diff --check` all exited successfully.
- Refreshed `/home/dux/target` usage: **16 GiB total**, including **8.6 GiB `target/readiness`** and **3.7 GiB `target/debug`**. The largest identified retained qualification artifacts include the **2 GiB** `p0-churn-oom-failure-20261026/filesystem.img`, the **1.4 GiB** `production-review-20261030`, and the **534 MiB** `canary-r16-20261027` rollback evidence. `/home/dux/target/readiness/vms` is absent. These failure/rollback artifacts were preserved; no cleanup was performed in this check.
- A third capped `cargo test --bin dux` attempt was stopped at **19.81% CPU PSI avg10** before the documented 20% threshold when a separate Go compiler consumed ~115% CPU. It exited 130 before any Rust test executed. At 15:29:47 UTC CPU PSI avg10 remained 18.85%; Dux stayed active at PID **4093354**, zero restarts, with dirty/not-caught-up state, **161,510 pending**, **13,503 seconds behind**, no scan, no write error, and zero reported dropped events. Root had **16 GiB free (92% used)**. No production service/index/configuration changes occurred.
- The full Rust test and release build remain unqualified on this host because unrelated recurring builds repeatedly raise CPU pressure. Continue these gates in the isolated guest/VPS.

### QUAL-162 Run release CLI and debug-artifact packaging regressions — 2026-11-01 15:32 UTC

- Ran `test_build_release_cli.py` under a 5% CPU / 512 MiB / no-swap transient scope with `/dev/shm` as `TMPDIR`: **11/11 passed** in 29.099s. It exercises release CLI argument validation, source/version/revision provenance rejection, unpinned `cargo-auditable` rejection, and rollback of previous artifacts after injected publication failure. The first invocation used a nonexistent TMPDIR subdirectory and failed during setup; the corrected invocation above is the valid test result.
- Ran `test_build_release_debug_artifact.py` under the same limits: **1/1 passed** in 15.000s. It builds a small static C fixture and verifies build-ID-bound debug symbols and package payload hashes. This does not build or qualify the Dux Rust release binary.
- These are additional to the earlier constrained 209-test selection, for **221 passing selected Python tests** total. Four service-stopping live-quota tests remain excluded. Exact inputs: `test_build_release_cli.py` SHA-256 `3cbe41c0e6c1f3497803e0a877e146682867d7f6a72edf66220c1d66c8d98771`; `test_build_release_debug_artifact.py` SHA-256 `064990bb25ea94bfc0c9ba3d38a777a033ade24859f3de3ae4b6145c697f7528`; `build-release.sh` SHA-256 `861210cb11111401528ea720791ee74e6586b0ed72f2da51eff18d099efda848`; source archive helper SHA-256 `c89f01d8920f1235764e5b0b102c192d3c8f10c690b4737dd0ada0c7bb2f28da`.
- At 15:32:19 UTC production remained active, PID **4093354**, zero restarts, dirty/not caught up, **164,981 pending**, **13,655 seconds behind**, no scan, `write_error=null`, and zero reported dropped events. CPU PSI avg10 was **5.78%**; root had **15 GiB free (92% used)**. No production mutation occurred. Current-source Rust tests/build and production qualification remain open.

### QUAL-163 Rebuild pressure-guard module tests against the current source hash — 2026-11-01 15:33 UTC

- Hash review found the older QUAL-149 test binary did **not** match the then-current `src/guard.rs`; its 18 passing tests were historical evidence only. Rebuilt from the preserved 508-byte harness against current `src/guard.rs` using Rust **1.99.1**, under **5% CPU / 512 MiB / no swap**. The exact current source passed **19/19** tests in **0.12s**, including fail-closed direct telemetry and unknown-health regressions.
- Current guard source SHA-256 `bda0e7c651655031c9c9556efec3c429b82f74a9925b9f8da7d3b2f5d5d3f8a0`; harness SHA-256 `f663e98aed2c2773adb231c509bc8956d91541c9bc183984c6e70f6a99b2907f`; test binary SHA-256 `e0764a0f0789f034309a5ef08a9ad31d25c8daef34f64720eb86a20f41618008`. Machine report `target/readiness/current-source-guard-module-tests-20261101/current-source-guard-module-tests-r2.json` SHA-256 `4e8423a1b17d493c2b56096ad4789e03152df3d8850a1ca189c074c18d1d0cee`.
- This is exact module-level coverage only; stubs replace `crate::util::fs_stat` and four fault flags, and no full Cargo tests or production binary are covered. At 15:33:19 UTC production remained active with zero restarts, dirty/not caught up, **164,981 pending**, **13,715 seconds behind**, no scan, no write error, zero reported dropped events, and root **15 GiB free (93% used)**. No production mutation occurred.

### QUAL-164 Abort temporary live-index snapshot when host pressure approached the limit — 2026-11-01 15:35 UTC

- A first attempt to place the backup directly in shared `/dev/shm` was correctly rejected by `dux-backup-index.py` because that directory is world-writable. Retried in a private mode-0700 `TemporaryDirectory` on tmpfs, with the helper in a 5% CPU / 1.5 GiB RAM / no-swap scope. The copy reached the database's ~755 MiB size, but CPU PSI avg10 rose to **19.54%** during verification; stopped before the 20% limit. Therefore this attempt yields **no integrity/backup pass**.
- The temporary Python context cleaned the private snapshot directory; `/dev/shm` returned to its prior 5.3 GiB free, no backup helper/scope remained, and the production SQLite files were not modified. After stopping, host CPU PSI avg10 reached **30.68%**; a subsequent process sample showed separate Go vet and Python certification tests active. Dux remained active, PID **4093354**, zero restarts; status showed dirty/not caught up, **167,551 pending**, **13,834 seconds behind**, no scan, no write error, and zero reported dropped events. Root had **15 GiB free (92% used)**. No production service/index/configuration action occurred.
- The v5 database still has only prior read-only structural evidence (`PRAGMA quick_check=ok` at the earlier audit timestamp); the incomplete snapshot attempt must not be presented as a current integrity result. Reattempt on an isolated host when available.

### QUAL-165 Re-scan the current source archive for embedded secrets — 2026-11-01 15:36 UTC

- Rebuilt a deterministic archive of **124 tracked/non-ignored source inputs** and scanned its extraction with default Gitleaks rules, redacted JSON, 5 MiB file cap, and 60-second timeout. Gitleaks exited **0** with report `[]` (no findings). Tree SHA-256 `dc34090ce84869e43b622ca05c039ab3aed1ec73d62fbda7c45827eba7493b13`; archive SHA-256 `4cd8c24608c43f638be30eec221df3920637afb381c7ca1f843500fccb71d3ea` (584,907 bytes); report SHA-256 `37517e5f3dc66819f61f5a7bb8ace1921282415f10551d2defa5c3eb0985b570`; manifest SHA-256 `b68bf259bdecdf1bc8f160d50729ea8e3eaf27cd1b9b6d658a21e6e371c02a94`. Evidence is under `target/readiness/source-security-review-20261101T1536Z/`.
- Scope excludes Git history, ignored build output/`target`, host files outside the archive, and other users' homes; it is not a host credential audit. At 15:36:51 UTC production remained active with zero restarts but dirty/not caught up, **168,060 pending**, **13,926 seconds behind**, no scan, no write error, and zero reported dropped events. Root had **15 GiB free (92% used)**. No production mutation occurred.

### QUAL-166 Bind the installed v5 database and executable state — 2026-11-01 15:37 UTC

- Read-only SQLite metadata inspection confirms `/var/lib/dux/dux.db` has schema version **5**, indexed root `/`, 8 tables, 9 indexes, and 3 triggers. Main DB is **755,593,216 bytes**, WAL **134,217,728 bytes**, SHM **65,536 bytes**, all mode **0640**. No fresh `quick_check` was run in this step; the snapshot/integrity attempt in QUAL-164 was aborted.
- Installed package reports **0.6.3-16**; `/usr/bin/dux --version` reports `0.6.3 (a42739776bbd+modified) [production]`; installed binary SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. Current worktree still declares database schema version **6**. This binds the deployed package/index mismatch but does not establish provenance of the installed build or qualify v5→v6 rebuild/rollback. No service or index mutation occurred.
- Latest read-only status at 15:37:33 UTC: active service PID **4093354**, zero restarts; dirty/not caught up, **168,060 pending**, **13,968 seconds behind**, no scan, no write error, zero reported dropped events. CPU PSI avg10 **0.17%**; root **15 GiB free (92% used)**.

### QUAL-167 Review the vendored jwalk patch and its dependency provenance — 2026-11-01 15:38 UTC

- `cargo tree --offline -i jwalk` confirms Dux resolves `jwalk 0.8.1` from `/home/dux/vendor/jwalk`. Compared all **34** vendored files to the cached crates.io `jwalk-0.8.1` source: only `src/core/read_dir.rs` and `src/lib.rs` differ. The original cached `.crate` SHA-256 is `2735847566356cd2179a2a38264839308f7079fa96e6bd5a42d740460e003c56`; vendor tree SHA-256 is `54485dd8b91508209c9d4789fd8ea5b687f172bf61ef38da1ae938b5e2927be3`. The vendored MIT license is byte-identical to the cached upstream license.
- Reviewed patch behavior: the new `ClientState::read_dir_state_for_entry` defaults to cloning inherited state (preserving upstream behavior); Dux overrides it to propagate the specific entry's path-instance state into each child directory read. The only other code difference is the equivalent `usize::MAX` spelling. `cargo check --locked --offline` previously passed against this path dependency. Dux has a `jwalk_hands_each_directory_its_own_parallel_read_state` regression in `src/scan.rs`; it passed as part of the 256-test root Rust suite on 2026-11-04, recorded in the later current-source test entry. The separate upstream `vendor/jwalk` integration suite subsequently passed on 2026-11-04; see the later vendored-suite result. The passing Dux test does not replace upstream jwalk tests or full production traversal/alias qualification.

### QUAL-168 Recheck live convergence after static review — 2026-11-01 15:39 UTC

- At 15:39:43 UTC Dux remained active at PID **4093354**, zero restarts, dirty/not caught up, **168,269 pending**, **14,099 seconds behind**, no scan, `write_error=null`, zero reported dropped events. Host CPU PSI avg10 was **0.03%** (avg60 **1.60%**); root had **15 GiB free (92% used)**. The backlog grew by **209** updates and lag by **131 seconds** since 15:37:33. No production action occurred; this remains evidence against convergence, not a healthy/ready state.

### QUAL-169 Stop capped full Rust test build at CPU-pressure threshold — 2026-11-01 15:40 UTC

- Retried `cargo test --locked --offline --jobs 1 --bin dux` with `CARGO_INCREMENTAL=0` in a transient scope capped at **5% CPU, 2 GiB memory, no swap, 900s** after CPU PSI had fallen to 0% avg10. A concurrent Go test workload resumed; CPU PSI reached **22.71% avg10** by 15:40:31 UTC, exceeding the 20% stop limit. Stopped the exact Cargo process; it exited **130** at crate compilation (`Compiling dux`), before any Rust test executed. At 15:40:42, Cargo/rustc were gone; Dux remained active PID **4093354**, zero restarts. This is an incomplete run, not a test failure or pass.
- Production status at 15:40:42 UTC: dirty/not caught up, **168,450 pending**, **14,161 seconds behind**, no scan, `write_error=null`, zero reported dropped events. CPU PSI avg10 remained **24.40%** from competing work; root had **15 GiB free (92% used)**. No production service/index/configuration changes occurred.

### QUAL-170 Refresh severe host pressure and reclaim a duplicate Dux build cache — 2026-11-02 08:34 UTC

- Fresh host check at 08:29 UTC showed root **98% used / 4.4 GiB free**, `/dev/shm` **9.9/13 GiB used**, and the 4 GiB swapfile **fully used**. Top-level disk usage: `/tmp` **57 GiB**, `/var` **50 GiB**, `/home` **34 GiB**, `/root` **23 GiB**, `/usr` **6.6 GiB**; `/var/tmp` alone is **20 GiB**. `docker system df` reports 54 volumes (3 active), 5.068 GiB total / 4.853 GiB nominally reclaimable, and 3 active images totaling 5.089 GiB. These other-project caches/volumes were not deleted.
- Identified `/tmp/dux-clippy-target` as a Dux Cargo build cache, **1.3 GiB**, last modified Oct. 30, with no process using it. Its 75,748,616-byte `debug/dux` executable had SHA-256 `0586e746f8ddb08e555fb940dfecfe1deda6b613d32f89109b971df60fdee295`, identical to `/tmp/dux-test-target/debug/dux`; the latter test target was retained. After verifying the duplicate, added Cargo's standard cache tag and ran `cargo clean --manifest-path /home/dux/Cargo.toml --target-dir /tmp/dux-clippy-target`; Cargo removed **3,277 files / 1.5 GiB**. Root free space increased to **5.0 GiB**, still **98% used**. This removed only a rebuildable duplicate cache, not project source or readiness evidence.
- A full Cargo test attempt using `/tmp/dux-test-target` was stopped when CPU PSI avg10 reached **23.99%**, before test execution; no test result. After cleanup, CPU PSI avg10 was **88.39%** and swap remained full, so no further builds were run. `readiness-vps` is not configured to an address and DNS lookup fails; no isolated guest is reachable from this host yet.
- Production at 08:34:19 UTC remained active at PID **4093354**, zero restarts, dirty/not caught up, **114,740 pending**, **74,976 seconds behind**, **502,309 reported dropped events**, no scan, `write_error=null`. Journal samples from Nov. 2 include fanotify overflow at 03:50, 08:18, and 08:24 UTC plus repeated reconciliation deferrals under memory/high-load pressure. No Dux service, database, quota, or configuration change occurred.

### QUAL-171 Track rapid disk consumption from concurrent host builds — 2026-11-02 08:35 UTC

- Root free space fell from **5.0 GiB at 08:34:19** to **3.7 GiB at 08:35:42** while multiple unrelated Go compiler processes were writing in separate `/tmp/go-build*` directories. CPU PSI avg10 reached **76.79%** (avg60 **73.55%**); swap remained **4/4 GiB used**. No individual recent file exceeded 500 MiB; this is distributed temporary compiler output. I left these live temp directories untouched.
- Dux at 08:35:42: active PID **4093354**, zero restarts, dirty/not caught up, **114,740 pending**, **75,058 seconds behind**, **502,309 reported dropped events**, no scan, `write_error=null`. Root is **98% used**, `/dev/shm` 9.9/13 GiB used. No production or unrelated test process was stopped.
- Follow-up at 08:36:37 UTC: pending remained **114,740**, lag increased to **75,112 seconds**, dropped count stayed **502,309**, no scan, no write error. CPU PSI avg10 **87.97%** (avg60 **80.09%**); root **4.5 GiB free (98% used)**; swap remained full. Service PID/restart state unchanged.

### QUAL-172 Recheck production after renewed memory and I/O pressure — 2026-11-02 08:59 UTC

- Host PSI: CPU avg10 **59.41%**, memory avg10 **32.80%**, I/O avg10 **37.70%**. Root **3.8–4.0 GiB free (98% used)**; `/dev/shm` 9.9/13 GiB used; swap **4/4 GiB** used. Concurrent Go compilers and `gosec` processes were active; the `readiness-vps` alias still fails DNS resolution.
- Dux remained active PID **4093354**, zero restarts, but is **paused for memory pressure**, dirty/not caught up, **119,074 pending**, **76,487 seconds behind**, **502,309 reported dropped events**, no scan, and `write_error=null`. Since 08:36:37, pending increased **4,334** and lag by **1,375 seconds**. No service/index/configuration mutation or scan occurred. Heavy validation remains unsafe on this host.

### QUAL-173 Confirm Dux resumed processing but still lacks convergence — 2026-11-02 09:02 UTC

- Read-only status at 09:02:49 UTC: `/usr/bin/dux` remains active as PID **4093354**, with **0 restarts**. It is no longer paused and reports no active scan, but remains dirty/not caught up with **119,671 pending**, **76,689 seconds (~21.3 hours) behind**, **502,309 dropped events**, **13,448,352 seen / 3,593,044 resolved / 127,104 committed**, `write_error=null`, and a **128 MiB WAL**. Index size is **2,152,972 nodes**. The daemon was consuming about **93% of one CPU**; host CPU PSI avg10 was **61.02%**, while memory and I/O PSI avg10 had eased to **1.74% and 2.41%**. Root remained **98% used with 3.7 GiB free**; `/dev/shm` had **2.9 GiB free**.
- The latest journal entry says host pressure recovered and index writes resumed at 09:02:34. This is evidence of renewed event processing, not a recovery scan, backlog drain, or exactness pass. No build/test, service/index/configuration mutation, or cleanup was performed in this check.
- The tracked 14 release blockers remain open. B01 and B02 now reflect the current backlog/lag and Nov. 2 overflow evidence. Current-source full-Cargo/package qualification and a reachable isolated builder remain unavailable under present host pressure; the production service is still not a safe qualification target.

### QUAL-174 Run isolated Python qualification regressions under resource limits — 2026-11-02 09:06 UTC

- Ran `python3 -m unittest discover -s scripts -p 'test_*.py' -v` under a transient systemd scope capped at **5% CPU, 1 GiB memory, no swap**, with test temp files on `/dev/shm`. The suite executed **225 tests in 71.501 seconds**: **224 passed and one errored** because `test_mid_publish_rename_failure_restores_previous_release_files` exceeded that test's internal 15-second subprocess timeout under the artificial 5% CPU cap. The traceback is `subprocess.TimeoutExpired`; it is a timing failure, not an assertion failure. The timed-out fixture left only its own temporary payload tree, which was verified and removed.
- Reran that exact test alone at **25% CPU / 512 MiB / no swap**; it passed **1/1 in 2.613 seconds**. This targeted pass does not convert the broad run to 225/225; the broad run remains recorded as 224/225 under the stricter cap plus one successful targeted rerun. No production services were stopped or modified.
- At 09:06:08 UTC production remained active (PID **4093354**, zero restarts), unpaused but dirty/not caught up, with **122,228 pending**, **76,884 seconds behind**, **502,309 dropped events**, **13,457,959 seen / 3,598,424 resolved / 127,232 committed**, no scan, `write_error=null`, and a **128 MiB WAL**. Relative to 09:02:49, seen/resolved/committed rose **9,607 / 5,380 / 128**, while pending grew **2,557** and lag grew **196 seconds**. Processing resumed, but it is not converging. CPU PSI avg10 was **1.14%**; root was **99% used with 3.6 GiB free**. No Rust build was attempted.

### QUAL-175 Record a CPU-bound no-progress interval and another kernel overflow — 2026-11-02 09:07 UTC

- Paired read-only `dux status --json` samples at 09:06:51 and 09:07:14 UTC were unchanged for `events_seen` (**13,523,508**), `events_resolved` (**3,598,586**), `updates_committed` (**127,360**), and `pending_updates` (**122,154**). `behind_seconds` increased from **76,925** to **76,949**; dirty reason remained `non-watched filesystem mounts changed; reconciling visible paths`, with no active scan and no write error. The daemon process remained runnable at about **93% of one CPU** while CPU PSI avg10 was **0.01%** in the later sample. This confirms a short no-progress interval but does not identify its code path or prove a sustained stall.
- The service journal recorded another `fanotify queue overflow` at **09:06:27 UTC**. Root was **99% full with 3.2 GiB available** at 09:07:14; Dux remained PID **4093354** with zero restarts. No service/index/configuration mutation occurred. B02 now includes this overflow; B03 remains open for source-bound CPU profiling and event-storm qualification.

### QUAL-176 Recheck readiness storage ownership and stop the inventory at the pressure guard — 2026-11-02 09:11 UTC

- The read-only Dux-sidecar scan found **840** old (`>24 h`) top-level regular `-wal`/`-shm` files under `/tmp/dux*`; **758** had no matching main DB and no open descriptor, totaling **21,155,840 allocated bytes (20.18 MiB)**. The remaining sidecars either had a matching DB or were newer. No sidecars were deleted because the reclaimable amount is immaterial and retained test-state provenance was not individually qualified.
- The largest readiness input remains `target/readiness/p0-churn-oom-failure-20261026/filesystem.img`: a **2 GiB allocated raw ext4 test filesystem**, not a VM disk, accompanied by a failure report and SHA-256 `8b1588f793bd734af76e40402e84ca3fb22cb5616025fd712aff20fc2f6139ec`. Five historical Cargo targets initially totaled about **1.192 GiB**. Verified the `diagnostic-build` binary against the manifest-bound R43 static artifact (SHA-256 `35149a246437c5520f7bc2eaee5c413fb5f75598eae29fa97b1b06d7302d9f21`), `r44-build` against the exact `/usr/bin/dux` payload in retained DEB 0.6.3-42 (SHA-256 `d9d6835a91391716662b51a77ebaf77ff50726db64f5bf055afbc610b52c2eb0`), and `r45-build` against the checksum-verified R45 static artifact and manifest (SHA-256 `9d6d97e2512190c127839dc462e16566e6bfa8181469cd2e7e491da1ae6c4c28`). Removed only those three duplicate rebuildable Cargo caches with `cargo clean`; each reported 1,092 files / 237.7 MiB removed. Preserved `r45-fault-build` and `r46-build` because their binaries are unique. No release package, source archive, failure report, or fault binary was deleted.
- Ran the repository's read-only `dux_storage.inventory("target/readiness", 0)` at a 10% CPU cap. It had not produced a complete report when CPU PSI avg10 crossed **27.28%**; the exact inventory process was interrupted (exit **130**). This is **no result**, not a completed capacity report. No artifact mutation occurred. Root space recovered from **3.2 GiB free / 99%** at 09:07 to **5.4 GiB free / 97%** at 09:11 during the same interval as unrelated Go test activity ended; causality is not assigned.
- Production at 09:11:42 UTC: active PID **4093354**, zero restarts, unpaused, dirty/not caught up, **123,896 pending**, **77,218 seconds behind**, **502,309 dropped**, **13,629,489 seen / 3,607,823 resolved / 128,512 committed**, no scan, no write error. Compared with 09:07:14, seen/resolved/committed rose **105,981 / 12,237 / 1,152**, pending grew **1,742**, and lag grew **269 seconds**. This is processing without convergence; no production mutation occurred.

### QUAL-177 Refresh live incident and current-tree static checks — 2026-11-02 09:14 UTC

- Read-only status at 09:14:11 UTC: active PID **4093354**, zero restarts, dirty/not caught up, **124,748 pending**, **77,366 seconds behind**, **502,309 reported drops**, **13,634,256 seen / 3,612,446 resolved / 128,896 committed**, no scan, and `write_error=null`. Compared with 09:11:42, seen/resolved/committed advanced **4,767 / 4,623 / 384**, while pending grew **852** and lag grew **148 seconds**. The index continues to process but fails to converge.
- Journal since 09:00 UTC adds another `fanotify queue overflow` at **09:10:13**. `ps -L` shows one runnable Dux task consuming **93% of one CPU**; systemd's process-wide quota is **100%**, with `MemoryCurrent=415,277,056` bytes and `MemoryPeak=445,493,248` bytes. The same timestamp window has no active scan. This confirms the target mismatch/loss symptom; installed-binary source and stack symbols are still unavailable, so no source-level root cause is assigned.
- `cargo fmt --all -- --check` passed under **CPUQuota 5%, MemoryMax 512 MiB, no swap**; `git diff --check` passed across the worktree. `Cargo.lock` remains SHA-256 `fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`, matching the prior zero-advisory `cargo audit` evidence. These are static/dependency checks only; no current-source Rust compile or test was run and no production mutation occurred.
- Root at this sample was **98% used / 4.6 GiB free**. A separate Go linker/test remained active under `/var/tmp`; I did not touch it. B01/B02 now reflect the 09:14 state and latest observed overflow.

### QUAL-179 Recheck disk pressure, refresh incident register, and scan the exact source handoff — 2026-11-02 09:23 UTC

- Latest live status at **09:22:47 UTC**: Dux active, dirty/not caught up, **127,021 pending**, **77,888 seconds behind**, **502,309 reported drops**, no scan, and `write_error=null`. The journal records another fanotify queue overflow at **09:20:03 UTC**. Compared with 09:20:09, event counters advanced but pending rose by 1,878 and lag rose by 163 seconds; the index remains nonconverged. No service or index mutation was made.
- Disk was **98% used with 4.0 GiB free**. Read-only `du` inventory measured `/tmp` at **55 GiB**, `/var/tmp` at **20 GiB**, `/home/dux/target` at **16 GiB**, with `/home/dux/target/readiness` **8.6 GiB** and `/home/dux/target/debug` **3.7 GiB**. Large `/tmp` and `/var/tmp` entries belong to other project names/workspaces; they were left untouched. The retained `/tmp/dux-test-target` is a **1.3 GiB** Cargo cache, last updated Oct. 30, with no Rust/Cargo process active; it remains useful as a warm dependency cache for the deferred build and was preserved. No VST or session-archive directory matching the earlier requested paths was present under `/home/dux`.
- Rebuilt and scanned the exact source handoff after the B01/B02 register refresh. Gitleaks default rules, `--no-git`, full redaction, 5 MiB target cap, and 60-second timeout completed with exit **0** and **zero findings** in 1.0s. Archive SHA-256 `a2962a4110cdedec402f2f6ed48dea50fdd8d3a8001a5813b336ca6ef9c3612f`; redacted empty JSON report SHA-256 `37517e5f3dc66819f61f5a7bb8ace1921282415f10551d2defa5c3eb0985b570`, preserved under `target/readiness/source-security-review-20261102T0923Z/`. This covers only the 124-file source snapshot, not Git history, ignored files, host secrets, installed artifacts, or production data.
- The handoff remains unqualified. Full Rust tests/build, verified production backup and restore, schema migration/rollback, live exactness, final-binary fault/scale/endurance testing, and canary remain open. No production package, service, database, or configuration was changed.

### QUAL-180 Record renewed pressure and another fanotify overflow — 2026-11-02 09:27 UTC

- At **09:26:49 UTC**, production Dux remained active with zero restarts, dirty/not caught up, **128,326 pending**, **78,123 seconds behind**, **502,309 reported drops**, no scan, and `write_error=null`; systemd still reports reconciliation deferred. Since 09:22:47, events seen/resolved/committed advanced, but pending rose by **1,305** and lag by **235 seconds**.
- The journal recorded another fanotify overflow at **09:23:38 UTC**. CPU PSI at 09:26:49 was **30.09% avg10 / 20.77% avg60**; an unrelated Go compiler was using **306% CPU** to build the `/home/xgenstack.com/backend` test target. This demonstrates real concurrent host pressure during recovery, but does not explain prior intervals or assign the installed binary's behavior to current source. No Dux build/test was started.
- Current pressure and the isolated builder's unknown role leave exact-source full-crate qualification unstarted. No production service/index change was made.

### QUAL-181 Verify the installed R16 package payload and correct provenance classification — 2026-11-02 09:33 UTC

- `dpkg-query` reports installed package `dux 0.6.3-16`. The retained R16 DEB metadata agrees; its `SHA256SUMS` verifies, and streaming `usr/bin/dux` directly from the DEB yields SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, exactly matching the installed executable. The R16 build manifest also records that binary digest and source-tree SHA-256 `3fa91992e998d8f6777c241be96061c5948b9b323978c0d204ff869b78224cc8`, 57 source inputs, `worktree_dirty=true`, and rustc 1.99.1; manifest SHA-256 is `3cab6005ac3b5084c86e50a02fed6e4d584e5aaa8819e2844dc539a36a3134ce`.
- Corrected B08: the installed binary has no embedded source digest, but a retained, hash-valid build manifest binds it to a dirty-source digest. The exact source contents/archive and matching symbols are absent from the R16 artifact directory; current 124-input source tree is a different digest. This improves binary/package identity evidence but does not make the build reproducible or qualify current source.
- No package, service, or index change occurred. Full current-source build/tests, v5→v6 migration/rollback, exactness, and final-artifact release gates remain open.

### QUAL-182 Recheck the active production blocker and isolated-builder admission — 2026-11-02 09:39 UTC

- The installed package remains `dux 0.6.3-16` (R16), binary SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. At 09:39:33 UTC systemd reports active PID **4093354**, zero restarts, and `Reconciliation deferred; waiting for normal host pressure`. Read-only status reports dirty/not caught up, **135,137 pending**, **78,891 seconds behind**, **502,309 dropped events**, no active scan, and `write_error=null`. The event counters advanced from 09:37, but pending and lag did not improve; this is not recovery or convergence.
- Root has **2.27 GiB available (99% used)**. `/dev/shm` has **2.89 GiB available**, below the VM runner's **3 GiB** ephemeral workspace requirement for even the standard 2 GiB guest. The runner's persistent-workspace reserve is 8 GiB in addition to its 2 GiB image requirement. `target/readiness/vms/images.json` and the qualification VM image directory are absent, although `/dev/kvm` exists. Host memory availability is about 15.3 GiB; the immediate guest admission blockers are missing verified images and insufficient workspace capacity. No build/test or service/index/package/configuration mutation was started.
- The current R16 executable remains installed because there is no source-bound, fully tested and packaged replacement candidate, and the production index has not recovered to a stable exact baseline. Current source still requires isolated build, full test and migration/package qualification, fault/scale/endurance gates, then an exactness-gated canary. The `vps-4` peer previously inspected is not a clean builder: it contains Dux database state and a broken auto-restarting unit, and was left untouched.

### QUAL-183 Preserve the retained failure reproduction during tmpfs capacity review — 2026-11-02 09:42 UTC

- At 09:41:40 UTC production remained active at PID **4093354**, zero restarts, dirty/not caught up, with **137,774 pending**, **79,015 seconds behind**, **502,309 reported drops**, no active scan, and `write_error=null`; systemd continued to report reconciliation deferred. Since 09:39, pending rose by **2,637** and lag by **124 seconds**, further evidence that production is not converging.
- Root had **2.0 GiB free (99% used)** and `/dev/shm` **2.9 GiB free**. The largest identified Dux-owned tmpfs workspace that could otherwise be mistaken for disposable scratch is `/dev/shm/dux-readiness-7_2g8vnk` (about **508 MiB allocated**, containing a 2 GiB sparse `filesystem.img`, `crash.marker`, `daemon.log`, and runtime heartbeat). The audit records this as the retained failed R45 workspace, restored after exact content/metadata comparison. It was preserved because it is reproduction evidence. No other-project data or readiness artifacts were removed.
- A read-only search found no QCOW2 qualification disk or active QEMU/qualification VM. The local builder remains unavailable, and the source handoff needs refreshing after this qualification-document update. No Dux build/test, service/package/index change, or artifact deletion occurred.

### QUAL-185 Reclaim verified duplicate release-build caches — 2026-11-02 09:45 UTC

- Inspected the five historical Cargo targets under `target/readiness/production-review-20261030/core-limit-r40/` (about **1.192 GiB** combined). `diagnostic-build` exactly matched the retained R43 manifest-bound static binary; `r44-build` exactly matched the `/usr/bin/dux` payload extracted from retained DEB 0.6.3-42; `r45-build` exactly matched the R45 static executable whose package, source archive, manifest, and `SHA256SUMS` verify. The retained R43 and R45 package checksum manifests passed.
- No `cargo` or `rustc` process was active. Used `cargo clean` only on those three verified production-build cache directories. Cargo removed **1,092 files / 237.7 MiB per directory**, **713.1 MiB reported total**. The resulting filesystem sample was 99% used with **2.4 GiB available**; concurrent host writes make the `df` free-space delta smaller than the cleanup total. Preserved `r45-fault-build` and `r46-build` because their executable hashes are unique, along with all package/source/failure evidence.
- No Rust build/test, release artifact deletion, production service/index/package/configuration mutation, or unrelated project cleanup occurred. Full release qualification and live exact recovery remain open.
- Follow-up read-only sample at **09:45:37 UTC**: production remained active (PID **4093354**, zero restarts), dirty/not caught up, **141,089 pending**, **79,251 seconds behind**, **502,309 reported drops**, no active scan, and no write error; systemd still reported reconciliation deferred. Since 09:42:19, pending increased by **3,315** and lag by **196 seconds**. Root had **2.6 GiB free (99% used)**; `/dev/shm` had **2.9 GiB free**. The free-space sample varied between cleanup and recheck, so the measured difference is not attributed wholly to cache deletion.

### QUAL-187 Reclaim the R58 Cargo cache after verifying retained release evidence — 2026-11-02 09:48 UTC

- `target/dux-auditable` contained a **543,928,320-byte** Cargo target. Its static binary SHA-256 `85009675f502c232b80374a524eb9d98eed97f96d1b603c7e26e848aae89104d` matched the R58 build manifest and the retained R58 static artifact. R58 `SHA256SUMS` verified the DEB, RPM, static binary, source archive, and build manifest. No Cargo/Rust process was active.
- Removed only this Cargo cache with `cargo clean`; Cargo reported **1,867 files / 592.8 MiB** removed. The package artifacts, build manifest, and source archive remain intact. Root availability increased from **2.6 GiB to 3.1 GiB** across the cleanup samples (still **99% used**).
- At **09:47:49 UTC**, production remained active (PID **4093354**, zero restarts), dirty/not caught up, **141,370 pending**, **79,384 seconds behind**, **502,309 reported drops**, no active scan, and no write error; systemd continued to defer reconciliation. Since 09:46:49, pending rose by **281** and lag by **59 seconds**. No Rust build/test or production service/index/package/configuration change occurred.

### QUAL-189 Recheck live backlog and local static syntax coverage — 2026-11-02 09:50 UTC

- At **09:49:46 UTC**, production remained active (PID **4093354**, zero restarts), dirty/not caught up, **142,262 pending**, **79,501 seconds behind**, **502,309 reported drops**, no active scan, and `write_error=null`; reconciliation remained deferred. Since 09:48:58, pending increased by **892** and lag by **49 seconds**. Root had **2.7 GiB free (99% used)**; `/dev/shm` had **2.9 GiB free**.
- Tailscale status showed no new qualification peer; the configured `readiness-vps` still fails DNS resolution. A read-only `ast.parse` pass accepted all **49 Python scripts** under `scripts/`. This verifies syntax only, not harness behavior or security. No Rust build, behavioral test, or production mutation occurred.

### QUAL-199 Recheck production convergence and refresh the blocker register — 2026-11-02 10:13 UTC

- Read-only `/usr/bin/dux status --json` updated at **10:13:26 UTC**: production remains active, dirty/not caught up, with **165,113 pending**, **80,922 seconds behind**, **502,309 dropped events**, no active scan, and `write_error=null`. The dirty reason remains `non-watched filesystem mounts changed; reconciling visible paths`; the index reports **2,154,335 nodes**, **167,100,928,000 indexed bytes**, and **755,593,216 bytes on disk**. This does not establish that the visible index is exact. Root is **99% used with 2.6 GiB free**.
- The journal recorded additional fanotify queue overflows at **09:52:57, 09:55:57, 10:09:48, and 10:12:27 UTC**. Updated B01/B02 accordingly. No service, index, package, or configuration mutation occurred.
- Current-source build/test and deployment remain prohibited by the host qualification constraints and unresolved live-index recovery. The documented source handoff is refreshed separately for this qualification-register change.

### QUAL-200 Reconcile blocker metadata with the latest live check — 2026-11-02 10:14 UTC

- Refreshed B01 from the 10:14:33 UTC status sample and corrected B07/B08 handoff/resource metadata. The live sample remains dirty/not caught up with **165,023 pending**, **80,989 seconds behind**, and **502,309 dropped events**; reconciliation is deferred. The current 126-input source handoff digest is `fedef068…dff1f95`.
- Static review of the current `dux scan` request path confirms that the CLI sends SIGHUP to the identified daemon, reports a 60-second start timeout without blindly resending, and waits for a changed scan generation, clean dirty/paused markers, no active scan, and fresh kernel-empty/zero-pending/certain watch status before success. This is source inspection only: the installed R16 behavior and current source runtime remain unqualified, so B04 stays open.

### QUAL-201 Remove environment-variable admission bypass from qualification guard — 2026-11-02 10:16 UTC

- Security review found `require_disposable_qualification_vm` accepted `GITHUB_ACTIONS=true`, `CI=true`, and any existing `GITHUB_WORKSPACE` directory as proof of an ephemeral runner. Those values are caller-controlled environment variables, so a root invocation could bypass virtualization and the controller marker by setting them.
- Removed the environment-based bypass and its forwarding through `systemd-run`. Admission now requires the explicit root-owned regular mode-0400 marker; an actual KVM/QEMU guest still requires that marker, and an explicitly marked hosted runner is accepted without trusting CI environment values. The readiness workflow creates the marker on its ephemeral GitHub-hosted runner before qualification; the private worker repeats the marker check.
- Replaced the previous test that blessed the environment bypass with regressions requiring spoofed CI variables to fail and explicit marker admission to succeed. Python AST parsing and `git diff --check` pass. Behavioral tests and workflow execution were not run on the production host; the source remains unqualified until isolated CI verifies them.

### QUAL-202 Recheck production status and isolated-test capacity — 2026-11-02 10:17 UTC

- Read-only Dux status updated at **10:17:20 UTC**: active PID **4093354**, zero restarts, dirty/not caught up, **164,590 pending**, **81,156 seconds behind**, **502,309 dropped events**, no scan, and `write_error=null`. The last journaled fanotify overflow remains **10:12:27 UTC**; reconciliation is still deferred. Counters advanced and pending fell since 10:14, but the index did not reach caught-up/exact status.
- Disk is **96% used / 7.2 GiB free**; `/dev/shm` remains **2.9 GiB free**. No qualification VM image/manifest files or QEMU guest process were found; `systemd-detect-virt --vm` reports this production host itself runs under KVM. The 2 GiB runner requires at least 3 GiB available tmpfs, and the persistent guest profile needs 10 GiB root workspace, so no isolated guest is admitted by the current resource policy. No test/build was started here.

### QUAL-203 Fix guard import regression and tighten workflow regression coverage — 2026-11-02 10:19 UTC

- The post-change source review found `Path` was still used to construct `SANDBOX_MARKER` after its import had been removed. This caused an import-time `NameError`, preventing all shared-guard callers from starting. Restored `from pathlib import Path` and confirmed that the guard module and its regression-test module both import successfully.
- Added a workflow regression requiring explicit root-owned mode-0400 marker provisioning and asserting that the shared guard does not inspect `GITHUB_ACTIONS` or `GITHUB_WORKSPACE`. The test is added but not run; full behavioral tests still require the isolated runner. `git diff --check` passes.

### QUAL-204 Refuse optimized Python in assertion-based qualification harnesses — 2026-11-02 10:20 UTC

- Review found that `dux-package-lifecycle.py` uses Python `assert` for sandbox admission and multiple package/security postconditions; `python -O` removes those checks. `dux-qualification-vm.py` also uses assertions for image checksum and CLI validation. Added an explicit `__debug__` refusal to the shared sandbox module and both standalone scripts, so optimized mode exits before tests or VM side effects.
- Added subprocess regressions invoking the package lifecycle, shared-guard entry point, and VM manager with `python -O`; each must fail with the explicit guard message. Tests are added but not run on the production host. Static syntax/import checks and `git diff --check` are recorded separately.

### QUAL-205 Recheck non-converged production index — 2026-11-02 10:23 UTC

- Read-only status updated at **10:23:04 UTC**: production remains active, dirty/not caught up, with **165,740 pending**, **81,500 seconds behind**, **502,309 dropped events**, no active scan, and `write_error=null`. Pending grew by **1,576** and lag by **257 seconds** since 10:18:48 UTC; this is continuing work without convergence. No newer fanotify-overflow warning appeared in the queried journal interval after 10:12:27 UTC. Root is **97% used / 5.7 GiB free**; `/dev/shm` has **2.9 GiB free**. No service/index/configuration action was taken.

### QUAL-206 Focused qualification-guard regressions — 2026-11-02 10:25 UTC

- Ran the focused sandbox/report tests, optimized-Python refusal tests for the package lifecycle and VM manager, and the workflow marker test in a transient systemd unit capped at **10% CPU, 512 MiB memory, zero swap, IOWeight 10, Nice 10**, with temp files on `/dev/shm`. The first invocation only failed test-module discovery because systemd's default working directory was `/`; after setting `WorkingDirectory=/home/dux/scripts`, the exact focused set passed **11/11 in 9.101 seconds**. Unit result was exit **0**, about **999 ms CPU**, **356 KiB** peak memory, zero swap. No service or index operation is part of these tests.
- Post-run, Dux remained active as PID **4093354**, zero restarts. At 10:24:50–10:25:00 its event counters (**14,616,863 seen / 3,777,244 resolved / 136,192 committed**) and backlog (**168,530 pending**) were unchanged; lag rose from **81,606** to **81,616 seconds**. The daemon thread measured **93.3% of one CPU**, no scan was active, and no new journal entries appeared since 10:23 UTC. CPU PSI some avg10 was **10.55%** after the run; no causal attribution is made. This confirms the small regression set passed but does not qualify full test/build performance or explain the production stall.

### QUAL-231 Close the package permission-repair race — 2026-11-02 12:22 UTC

- Security review found `secure-index-permissions.sh` changed state-directory ownership before removing group/world write access. Mode 0700 alone is insufficient because an untrusted existing directory owner retains owner write access. The helper now applies mode **000** before ownership repair, changes ownership, then reopens mode **0700** for artifact validation and restores the intended final directory mode **0750**. Debian invokes this helper on `/var/lib/dux`, whose parent is root-controlled.
- Artifact handling now also rejects hard-linked database/WAL/SHM/journal files before changing their ownership or mode, avoiding changes to files outside the state directory. Added a regression that verifies an external hardlink target keeps its owner, group, and mode.
- Added a regression using a writable state directory initially owned by uid **65534**. It wraps `chown` and records directory mode at every ownership change; the directory transfer must occur at mode **000**, and artifact ownership changes happen only after the directory is root-owned and mode **0700**. The complete `scripts.test_dux_package_lifecycle` suite passed **44/44 in 0.465s**, including the **8** helper-specific tests, SQLite backup tests, and package policy checks. `sh -n`, ShellCheck, and `git diff --check` pass. This verifies local helper/package-script behavior; it does not qualify an installed package lifecycle or eliminate every possible hostile filesystem race.
- The previously built **0.6.3-59** DEB/RPM and binary predate this fix. Their embedded helper therefore does not contain the corrected ordering and those artifacts must not be used as the current candidate. The updated source tree must be re-archived, rebuilt, audited, and rerun through package lifecycle tests before any promotion. No live package, service, database, or index was changed.

### QUAL-232 Retest all Python qualification harnesses and fix a stale CPU timing assertion — 2026-11-02 12:32 UTC

- The first full Python discovery run completed **245/246** tests and failed one source-contract assertion: the CPU-burst harness had been corrected to measure event production from `producer_started`, but its test still expected the old `event_window_started` expression. The harness correctly kept event production separate from the daemon event/CPU measurement window. Updated the test to check both timer expressions, their ordering around `SIGCONT`, and that CPU fraction uses only the event window. The focused regression passed **1/1**.
- Reran full discovery with `python3 -m unittest discover -s scripts -p 'test_*.py'` under transient service limits **CPUQuota=10%, MemoryMax=1 GiB, MemorySwapMax=0, IOWeight=10, TasksMax=128, Nice=19**; `TMPDIR` was on `/dev/shm`. Final result: **246/246 passed in 38.304s**, **3.820 CPU-seconds**, **360 KiB** peak memory, **zero swap**. Host CPU PSI some avg10 was **4.83%** afterward and full avg10 remained **0%**. This qualifies Python harness behavior; it does **not** run Dux's Rust CPU event-burst, current-binary fault/scale, or package install tests.
- Post-run read-only production sample at 12:32:44 UTC remains active and dirty/not caught up with **233,461 pending**, **89,279 seconds behind**, **502,309 dropped events**, no active scan, and `write_error=null`. Root had **3.9 GiB free / 98% used**. No production service, package, DB, or index change occurred. The source handoff must be refreshed for this test-source correction; B03/B07/B09 remain open.


### QUAL-233 Stop current-source build at the host CPU-pressure gate — 2026-11-02 12:44 UTC

- Prepared source handoff `target/readiness/current-source-handoff-20261102T1240Z` with **126 inputs**, tree SHA-256 `4ed9714f273f8d1654af9de50175aceb195f1dcabef6f8b74327e886da8e8536`, archive SHA-256 `588448345f73d729edaf874a6d23c1fefce4f852ab97bf0b503f45b46fe800a4`, and Gitleaks **zero findings** on the exact 2.40 MB extraction. The handoff itself verifies all member hashes and checksum entries.
- Attempted a current-source static-musl build in transient systemd services with `/dev/shm` target storage, one Cargo job, no network, 2 GiB memory ceiling, no swap, IOWeight 10, Nice 19, and idle I/O. The 25% CPU attempt was stopped when host CPU PSI `some avg10` reached **22.33%**. A 10% CPU retry was stopped when PSI again crossed **20%** (20.18% then 20.51%). Both services ended via SIGTERM, no Cargo/rustc process remains, and no complete executable was produced. Partial tmpfs build cache was removed after confirming the build had stopped.
- Latest read-only production status at **12:44:08 UTC** remains dirty/not caught up: **236,000 pending**, **89,964 seconds behind**, **502,309 dropped events**, `uncertain=1`, and no active scan. The production daemon remains active with **zero restarts**. No installed executable, service configuration, database, or index state was changed.
- A third attempt at CPUQuota **5%** from the refreshed source handoff `current-source-handoff-20261102T1245Z` was also stopped: CPU PSI `some avg10` rose to **33.51%** in the first minute, despite only **3.36 CPU-seconds** and a **256.4 MiB** memory peak. That unit ended via SIGTERM; the incomplete 7.5 MiB tmpfs cache was removed after confirming no active compiler remained. By 12:46:45, CPU PSI had fallen to **5.42% avg10**, while the live index remained stale.
- These were incomplete builds, not build/test failures of Dux. Current-source build and all gates requiring that binary remain open. Stops follow the existing host CPU-pressure safety gate; resume only after a stable low-pressure window or on an isolated build host.


### QUAL-234 Build and verify current-source release 0.6.3-60 — 2026-11-02 13:01 UTC

- Reused the pinned R34 musl Cargo cache copied to `/dev/shm`; exact source input was the 126-file handoff `current-source-handoff-20261102T1247Z`, tree SHA-256 `5973bcbeeedea6325a37fd19a7bb054e518fda7c3241c354c02cf777eb0fb0fe`. Build used Rust/Cargo **1.99.1**, `cargo-auditable` **0.7.7**, `CARGO_BUILD_JOBS=1`, offline dependencies, CPU affinity **3–5**, `CPUWeight=10`, `MemoryMax=3 GiB`, no swap, `IOWeight=10`, Nice 19, and idle I/O. Build completed in **2m33s**, with **1.1 GiB** memory peak and no swap; CPU PSI avg10 stayed below 0.3% during observation.
- Produced static-musl PIE binary SHA-256 `b6f759f135590021364eefcbfb6a217d580a3ac02d1b140ef87797d7250e551c`, DEB SHA-256 `7adcc65547a669798a6871318a763db5965b99bbd3c79900d35c5d286a25f9bc`, RPM SHA-256 `45bfbcb83dc0713dbfed0c80c6cf706b18b7e72a707a9fc7a16f437945cf20ba`, and Build-ID-matched debug symbols SHA-256 `d9b148110773373de431fee069b768387d699a45a1ffa1b2ac229727aff1a08d` (Build ID `490e931ddb6f852107194ca3cd7177e721d522a2`). Build manifest and `SHA256SUMS` verify; embedded `.dep-v0` is present and no `DT_NEEDED` entry exists.
- Independently checked all 126 source archive members/sizes/hashes and recomputed the manifest tree digest. Gitleaks scanned the exact **2.40 MB** extraction with zero findings. `cargo audit bin` exited **0** and parsed metadata for **202 dependencies**; no advisory finding was emitted. DEB and RPM extracted payloads match the release binary; the shipped permission helper, backup helper, and service units match the exact source files. Version/source digest, top-level help, daemon help, and the static binary smoke checks succeeded.
- Ran `cargo test --locked --offline --release --all-features -- --test-threads=1` against the same source with systemd affinity to CPUs **3–5**, `CPUWeight=10`, `MemoryMax=2 GiB`, no swap, and low I/O priority. Result: **255/255 passed** in **19.36 seconds** of test time; service runtime **2m16s**, **767.3 MiB** peak memory, zero swap. This covers Rust unit tests, not live event, package-install, migration, scale, endurance, or canary behavior.
- Candidate package is unqualified. No DEB/RPM install/upgrade/rollback, current-binary P0/fault/scale, v5-to-v6 production-sized migration, 72-hour endurance, 24-hour canary, signing, or owner acceptance is established. Production remains on R16; no production service, executable, database, or index state changed.

### Local checks and live gate revalidated — 2026-11-02 13:54 UTC

The current source worktree passed Python qualification harness discovery (**246/246**), `cargo fmt --all -- --check`, Python `compileall`, and `git diff --check`. A fresh Rust compile/test attempt was stopped during dependency compilation to avoid resource pressure on the production host; this is not a Rust test result. Release candidate 0.6.3-62 remains uninstalled and unqualified.

At the same checkpoint, production R16 remained active with zero restarts, dirty/not caught up, **286,317 pending updates**, **94,182.6 seconds behind**, **502,309 userspace updates dropped**, no active scan, and no write error. Root had **6.4 GiB free / 97% used**. The stale `/run/dux-ext4-inspection` mount remained present. These local checks do not close B01–B15 or justify promotion.

### Candidate Rust source/test binding rechecked — 2026-11-02 13:56 UTC

Manifest comparison confirms release 0.6.3-62 differs from Rust-tested release 0.6.3-60 only in `docs/QUALIFICATION.md`; all Rust source, Cargo, build-script and toolchain inputs are byte-identical. The current worktree differs from release-62 only in `docs/QUALIFICATION.md` and `docs/STORAGE_RETENTION_REVIEW_20261030.md`, with no code/build input changes. Thus the prior **255/255** default/all-feature Rust suite is source-equivalent evidence for the release-62 code. The stopped 13:54 compile attempt did not execute tests and is not a failure. Runtime fault/overflow, production-sized migration/rollback, package lifecycle, scale, endurance and canary gates remain open.

### Legacy overflow telemetry correctness fix — 2026-11-02 14:01 UTC

The current source no longer converts pre-v4 watcher status into a numeric zero for kernel overflow notifications. Legacy status produces an unavailable value (`null` in JSON); metrics expose `dux_fanotify_overflow_count_available` and omit the count metric when unavailable; text and TUI status label the gap. Regression tests were added for v3 parsing, JSON, metrics, and TUI. Formatting and whitespace checks pass. The Rust suite has not yet completed on this patch: a compile surfaced test-fixture errors that were corrected, then the resource-capped attempt was stopped at **29.86% CPU PSI** while the production daemon and an unrelated Go test were active. Release 0.6.3-62 predates this fix and is stale for promotion; a rebuilt candidate and successful test run are required.

### Telemetry-fix test retry — 2026-11-02 14:02 UTC

The Rust suite retry used one Cargo job under a systemd scope capped at 5% CPU, 2 GiB RAM, no swap, and idle I/O. Global CPU PSI rose to **24.45% avg10** while Dux and another workload were active, so the scope was stopped during compilation; no tests ran. Systemd confirms the scope is inactive. Formatting and whitespace checks pass, but the new regression assertions remain unverified until an isolated host or lower-pressure window. Production at 14:02:37 UTC remains dirty with **294,809 pending**, **94,673.9 seconds behind**, and **502,309 userspace updates dropped**; root has **5.5 GiB free / 97% used**. Candidate 0.6.3-62 predates the fix.

Third telemetry-fix Rust test attempt at 14:03 UTC was again stopped during compilation under the 5% CPU quota: host CPU PSI reached **22.78% avg10** after about 35 seconds. The scope is inactive; no tests ran. Final source review confirms all parser consumers preserve and expose unavailable legacy counts consistently. Formatting and whitespace checks pass, but the Rust patch remains uncompiled and untested. Use an isolated VPS or a sustained low-pressure host window for the next build.

Python qualification harness discovery after the telemetry source change passed **246/246** in **6.005 seconds**. This does not verify Rust compilation or the modified status parser/metrics; those tests remain pending due repeated CPU-pressure stops.

### Legacy overflow telemetry full Rust test pass — 2026-11-02 14:14 UTC

After correcting a malformed v3 test record (missing `behind_ms`), the complete all-features Rust suite passed **256/256** in **99.45 seconds**. The locked/offline one-job run used the existing `/tmp/dux-test-target` cache under a 20% CPU quota, 2 GiB memory cap, no swap, low CPU/I/O weights and idle I/O scheduling. The earlier run was **255/256**, with only the bad fixture failing; all 255 other tests passed. This verifies the modified parser, status JSON, Prometheus availability/counter behavior, and TUI labels at Rust unit-test scope. It is not a release build, package lifecycle, actual kernel overflow, production migration, scale, endurance or canary qualification. Production remains on R16; candidate 0.6.3-62 does not contain this fix.

### Production source, schema, and disk recheck — 2026-11-02 14:20 UTC

Read-only inspection confirms the host package is `dux 0.6.3-16`, `/usr/bin/dux` reports `0.6.3 (a42739776bbd+modified) [production]`, and the live database has `meta.schema_version=5`. The current worktree is based on commit `a427397` (`v0.6.1-1-g...-dirty`) with 83 changed/untracked paths; the executable's `+modified` label does not identify those exact source bytes. Current source declares schema v6, so replacing the binary can trigger a full-root rebuild/migration and still requires tested rollback. No production binary or database was changed.

At 14:20:04 UTC the daemon (PID 4093354) was active with zero restarts, **304,900 pending**, **95,719 seconds behind**, **502,309 userspace updates dropped**, no scan in progress, and last scan age **96,899 seconds**. The journal recorded another fanotify overflow at **14:15:30 UTC**. `/` had **8.9 GiB available / 95% used**, below the 18 GiB build-and-test preflight. `/dev/loop0` still maps an **8 GiB** filesystem image whose backing path `/tmp/dux-scale-f2azn2f8/filesystem.img` is deleted; `/run/dux-ext4-inspection` remains mounted in the host and daemon namespaces. Detaching it could release backing-file space and remove a scan contaminant, but requires controlled service/mount maintenance; no such action was taken.

The effective service still runs as root with `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`, `NoNewPrivileges=yes`, and the installed service security score is **5.1 MEDIUM**. Effective CPU quota is **100%**; the packaged unit says **50%** while the current source unit says **100%**. The daemon used about **94% of one CPU** in the sample. Two unrelated Go compiler processes were each using about **120%**, with host CPU PSI some avg10 **3.38%** and I/O PSI full avg10 **0.48%**. No release build was started under this concurrent host load and insufficient disk headroom. An isolated VPS and exact source-bound package/migration evidence remain release prerequisites.

### Capped release-build attempt stopped on host pressure — 2026-11-02 14:25 UTC

After the unrelated Go compilers exited, attempted a source-only locked/offline all-features release build under a systemd scope capped at **5% CPU**, **2 GiB RAM**, no swap, low CPU/I/O weights, `nice 19`, and idle I/O. Host CPU PSI some avg10 crossed the **20% stop threshold** and peaked at **28.10%**; the build scope was stopped before Dux compiled. Disk availability fell from **8.2 GiB** to **7.0 GiB**; it remained above the 6 GiB emergency floor. The scope is inactive, no cargo/rustc remains, and the **47.3 MB** incomplete Cargo target was removed. This is a host-pressure stop, not a compiler failure; no release executable or package was produced. Production PID **4093354** stayed active with zero restarts; at 14:25:44 it remained dirty/not caught up with **307,556 pending**, **96,059 seconds behind**, **502,309 userspace updates dropped**, no scan, and last scan age **97,239 seconds**. Root had **7.0 GiB free / 97% used**. The source-bound release build and all package/migration gates remain open.

### Compact the retained P0 OOM reproduction and recheck recovery admission — 2026-11-02 14:36 UTC

The 2 GiB raw ext4 P0 churn/OOM image was losslessly compressed to
`target/readiness/p0-churn-oom-failure-20261026/filesystem.img.zst` (**41,845,099
bytes / 39.9 MiB**). `zstd -t` passed and its decompressed SHA-256 equals the
original **`8b1588f793bd734af76e40402e84ca3fb22cb5616025fd712aff20fc2f6139ec`**;
the compressed hash is
**`ca2720259352fbd7217649b0e93d969a89dda6e2c90ef37a5bc12a5edf9ed4a0`**. The
failure-analysis manifest records the compressed path and restore procedure.
The raw image was removed only after content verification and confirming no
open consumer; about **2 GiB** was reclaimed without discarding the failure
reproduction. Root availability rose from **8.8 to 11 GiB**, and
`target/readiness` fell from **8.1 to 6.2 GiB**. The 18 GiB workspace/reserve
preflight still fails.

Production at 14:35:36 UTC remained active, dirty/not caught up, **307,551
pending**, **96,652 seconds behind**, **502,309 userspace updates dropped**,
no active scan, and last scan age **97,831 seconds**. Systemd still reported
reconciliation deferred. Around 14:35–14:36 UTC, load was about **2 on 12
CPUs**, MemAvailable **15.3 GiB**, CPU PSI **0.31–0.54%**, I/O PSI **0.15–0.28%**,
and root free space **11 GiB**. Current source would classify these host-wide
readings as below its admission thresholds, but the installed binary's source
is unmatched; the observed deferral cause is not established. No service,
index, mount, or DB change was made.

### Cover optimized-Python refusal for all assertion-based CLIs — 2026-11-02 14:43 UTC

Static review found eight production `dux-*.py` CLI harnesses using Python
`assert`. All eight already import the shared `dux_qualification_sandbox`
module (or have a direct guard), which rejects `python -O` before CLI work; this
was not a live bypass. The optimized-interpreter regression had only exercised
the package lifecycle and bind-alias entry points. Expanded it to invoke
`python -O <script> --help` for the live quota, live test, readiness, scale,
systemd-status, and TUI harnesses as well. The focused test passed **1/1 in
17.699 seconds** under a systemd scope capped at **5% CPU**, **512 MiB memory**,
no swap, and idle I/O. It confirms every script exits with the explicit
assertions-required message before running. The scope exited successfully.

At 14:43:14 UTC production remained active with zero restarts, dirty/not caught
up, **315,478 pending**, **97,110 seconds behind**, **502,309 dropped userspace
updates**, last scan age **98,289 seconds**, no scan, and `write_error=null`.
Root had **7.0 GiB free / 97% used**; CPU PSI some avg10 was **10.28%** after
the Go workload eased. No production state changed.

### Stream large artifact hashes in package rollback — 2026-11-02 15:21 UTC

Review found that package lifecycle rollback hashed SQLite backups with
`Path.read_bytes()`, loading the entire database into memory. The verified live
v5 snapshot is **755,593,216 bytes**, so this could create a large transient
memory spike or OOM during rollback. Replaced whole-file hashing with a shared
1 MiB streaming SHA-256 helper and updated binary/package/artifact hashes to
use it. The rollback unit test patches `Path.read_bytes` to fail during backup
and restore, asserts the recorded hash, restores the snapshot, checks SQLite
`quick_check`, permissions, and removes stale WAL/SHM/journal artifacts.

The complete package lifecycle module passed **46/46 tests** in **23.109
seconds** (systemd service runtime 25.343 seconds) under CPUQuota 5%,
MemoryMax 512 MiB, no swap, IOWeight 10, `nice 19`, and idle I/O. This is
synthetic helper/lifecycle evidence; a bounded-memory restore rehearsal using
the full 755 MiB live snapshot has not yet run. At 15:20 UTC root had **11 GiB
free** and the production Dux service remained active with PID **4093354** and
zero restarts. Host CPU PSI rose to **32.07% avg10** after the test; no
production service, binary, package, database, or index was changed. The 18
GiB build reserve and all binary/migration/runtime qualification gates remain
open.

### Full-size rollback restore rehearsal stopped at host-pressure gate — 2026-11-02 15:22 UTC

Started an isolated restore rehearsal using the **755,593,216-byte** root-only
v5 snapshot. The harness was configured to restore only inside a fresh
mode-0700 temporary directory under `/root`, compare the complete source and
restored SHA-256, verify schema/mode/sidecar handling, and then delete the
scratch database. While the helper was still performing its initial read-only
snapshot `quick_check` (before it created its restore temporary file), host CPU
PSI crossed the established **20% avg10** stop threshold and reached **31.63%**.
Stopped the rehearsal after about **35 seconds**; systemd confirms the scope
inactive, **2.049 CPU-seconds**, **42.1 MiB** peak memory, and no swap. The only
scratch file was the 8 KiB dummy target created before invoking the helper; it
was removed after confirming no process remained. The source snapshot remains
unchanged and mode 0600. This is a pressure-gated incomplete attempt, **not** a
restore pass or failure. Production stayed active, PID **4093354**, zero
restarts; no production files or service state were changed. Retry the full
restore only when CPU PSI is stably below the gate or on an isolated host.

### Production backlog and host-pressure sample — 2026-11-02 15:23 UTC

Read-only `dux status --json` reports the production index dirty and not caught
up, with **332,673 pending updates**, **99,541.567 seconds behind**,
**502,309 dropped userspace events**, no scan in progress, `write_error=null`,
and dirty reason `non-watched filesystem mounts changed; reconciling visible
paths`. The index reports **2,137,384 nodes** and **755,593,216 bytes** on
disk. Systemd still reports the R16 daemon active with PID **4093354** and zero
restarts; a process sample measured about **94% of one CPU**. Host load was
**33.73 / 23.11 / 33.93** and CPU PSI `some avg10` reached **85.68%**. An
existing remote-session `gosec` debt-ratchet job was concurrently compiling
many Go packages; it was left untouched. Root had **9.1 GiB free**. These
conditions invalidate this window for build, migration, scale, or full-size
restore qualification. The status sample confirms the live convergence gate
remains failed; it does not identify whether the installed R16 daemon or the
background host workload is the cause of the growing backlog. No live Dux
state was changed.

### Live memory-pressure pause and fanotify overflow — 2026-11-02 15:28 UTC

At 15:25 UTC the R16 journal recorded that reconciliation was deferred,
index writes were paused for memory pressure, and the fanotify queue overflowed
again. At 15:28:40 UTC, read-only status reported `paused=true`,
`pause_reason=memory pressure`, **334,455 pending updates**, **99,836.261
seconds behind**, **502,309 dropped userspace events**, no scan in progress,
`write_error=null`, and **2,137,406 indexed nodes**. `dirty_reason` remains
`non-watched filesystem mounts changed; reconciling visible paths`. The
installed service remains active (PID **4093354**, zero restarts) and consumes
about **318 MiB** in its cgroup.

Host evidence at the same sample: **25 GiB RAM**, **9.4 GiB available**, only
**2.3 GiB free**, all **4.0 GiB swap used**; memory PSI `some avg10` **45.99%**
and `full avg10` **32.78%**; CPU PSI `some avg10` **68.75%**; root has **8.4
GiB free**. Journal shows further memory-pressure pause and fanotify-overflow
warnings at 15:25:04. The host is not currently safe for a candidate build,
scale load, full restore, or migration rehearsal. This is a confirmed
production-readiness failure in current operation; its cause remains
unattributed between the stale R16 daemon behavior and other host workloads.
Do not treat `write_error=null` or the running service as evidence of a healthy
or converged index. No production state was changed.

At **15:29:14 UTC**, a follow-up read-only status showed `paused=false`, but
still no active scan: pending rose from **334,455 to 334,569** and lag from
**99,836.261 to 99,868.498 seconds** over about 34 seconds; the committed-update
counter remained **172,800**. Memory PSI had eased to **1.53% some / 1.09%
full avg10**, but CPU PSI remained **67.97%**, host load was **72.50 / 65.35 /
50.12**, swap remained **4.0 GiB used**, and the existing `gosec` compile was
still active. This transient unpause is not recovery or convergence: there is
no scan and the backlog/lag continue to grow. Do not resume load tests in this
host window.

### Disk cleanup request rechecked — 2026-11-02 15:30 UTC

Current filesystem inventory shows `/vms` absent, `/home/vm` only **28 KiB**,
and `/home/dux/target/readiness` **6.2 GiB**. No `vst-*` directories remain
under `/root` or `/home`; no multi-GiB QCOW2/raw VM image remains in the named
readiness or VM paths. The remaining `target/readiness` data is retained
qualification evidence, including small **366 KiB** seed images, rather than
the former 10 GiB VM image. Root had **8.7 GiB free / 96% used** at this check.

The large paths the user asked about resolve under `/root`: `.local` **4.6
GiB** (contains the currently running Codex executable), `.codex` **3.3 GiB**,
`go` **2.9 GiB** (Go modules/toolchains used by the active Go security scan),
`.cache` **1.4 GiB** (Go build and staticcheck caches, with Go scanning still
active), and `codex-sessions-archive` **1.2 GiB**. The first four are in use
or may contain active application state; the VST/VM targets are already absent.
The session archive is not required by Dux or to launch Codex; it preserves
historical Codex sessions. It was retained because removing it would discard
user history, not merely clear a build cache. No filesystem data was deleted
during this recheck.

### Full-size rollback rehearsal retry stopped at pressure and memory limits — 2026-11-02 15:32 UTC

Retried the same isolated **755,593,216-byte** v5 snapshot restore after the
15:31 sample showed CPU PSI `some avg10` **1.48%**, memory PSI near zero, and
11 GiB free. The scope remained capped at **5% CPU**, **512 MiB memory**, no
swap, and idle I/O. Within **34 seconds**, CPU PSI rose to **68.20% avg10**;
host load then measured **21.55 / 39.53 / 42.67** with multiple concurrent
compiler processes. The scope reached its 512 MiB memory cap and recorded
**1,031 `memory.events:max` events**, but `oom=0` and `oom_kill=0`. Stopped the
scope before the helper's initial snapshot `quick_check` completed, before it
created the restore temporary file. Systemd confirms the scope is inactive;
the only scratch artifact was an 8 KiB dummy database, removed after checking
no process remained. This is neither a restore pass nor a restore failure.
Production remained active with PID **4093354**, zero restarts; a follow-up
status still showed no scan and **337,556 pending**, **100,069.696 seconds
behind**. No production database, service, or index was changed. Retry with a
larger memory cap only during a stable low-pressure window or on an isolated
host. The final check after cleanup showed root free space falling to **7.6
GiB**, CPU PSI `some avg10` **75.00%**, the daemon still active with zero
restarts, and no rehearsal scratch left. The unrelated compiler activity and
concurrent disk growth remain uncontained; the 18 GiB qualification reserve
is further out of reach.

### Production event processing stalled during concurrent static analysis — 2026-11-02 15:33 UTC

A new read-only sample shows the live index still dirty with **337,556 pending**
and no scan. Compared with 15:32, `events_seen`, `events_resolved`, and
`updates_committed` were unchanged while reported lag rose from **100,069.696**
to **100,145.736 seconds**. The daemon and its status heartbeat remain live,
but this interval shows no event/commit progress. The daemon process was using
about **94% of one CPU**. Host CPU PSI was **22.97% avg10**, and root had **8.4
GiB free**. An unrelated `staticcheck` process was consuming about **150% CPU**
with compiler activity in its parent workload. The installed R16 binary's
source is unmatched, so this observation cannot attribute the stall solely to
Dux or the external load. No process was interrupted and no production state
was changed; candidate work remains outside the safe resource window.

### Current-source secrets scan remains inconclusive — 2026-11-02 15:44 UTC

Created a deterministic archive of **126 exact release inputs** at
`target/readiness/source-security-review-20261102T1536Z/current-source.tar.gz`
(SHA-256 `80b9c2db490071396a732ec935dfa6805fc6ce24d51fbab6e424cfb70be02aa4`;
tree digest `4d35732c6657c9ea3803efd77fafc871422c8478cfbc59e0ff3239d54a1ab9e1`).
The first Gitleaks attempt exited **1** despite an empty JSON report; stderr was
not retained, so the cause and scan completeness are unknown. A retry disabled
the scanner timeout but was stopped after about **4 minutes** at the host
pressure gate. It consumed **2.507 CPU-seconds**, peaked at **51.9 MiB**, used no
swap, and created no final report. At stop, CPU PSI `some avg10` was **89.48%**
and root had **5.1 GiB free** while unrelated Go race tests/compiles were
active. The redacted status and archive checksums are preserved in that
directory. This is **not** a clean secrets-scan result; repeat on the isolated
builder or a stable low-pressure host. No production state was changed.

A third attempt began when CPU PSI avg10 had eased to **8.35%**, using a **3%
CPU / 256 MiB memory** scope with no swap. Within about **34 seconds**, PSI rose
above the stop threshold to **84.92% avg10**; the attempt was terminated before
a report existed. Systemd recorded **1.185 CPU-seconds**, **45.7 MiB** peak
memory, and zero swap. The extracted archive was removed. After stopping, root
had **4.8 GiB free** and CPU PSI remained **84.95% avg10**. The second and third
attempts are incomplete, not clean scans or source findings; the exact release
input archive and status/checksum manifests remain available for a clean-host
rerun.

### Fresh current-source secrets scan stopped at host-pressure gate — 2026-11-02 15:46 UTC

Regenerated the exact release-input snapshot after qualification-log changes:
**126 inputs**, archive
`target/readiness/source-security-review-20261102T1545Z/current-source.tar.gz`
(SHA-256 `c3a9eb913370d044c99628a435382e172f9de2b84376312fdb559500d585754b`).
The Gitleaks scan started at a **3% CPU / 256 MiB memory** cap with no swap,
after CPU PSI was **2.82% avg10**. Within about **34 seconds**, PSI reached
**73.64% avg10**. Stopped the service before a report existed; systemd recorded
**1.183 CPU-seconds**, **49.0 MiB** peak memory, and no swap. The temporary
extraction was removed. Root had **5.0 GiB free** after cleanup. Checksums and
the explicit incomplete status are stored beside the archive. This remains
inconclusive, not a no-findings pass. No production service or data changed.

### Production convergence and host-pressure checkpoint — 2026-11-03 09:37 UTC

- Read-only status for the installed production daemon still reports dirty and
  not caught up: **189,576 pending updates**, **165,134 seconds behind**,
  **1,503,990 dropped events**, no scan in progress, and no reported write
  error. The index reports **1,952,873 nodes**, **142,357,225,472 logical
  bytes**, **755,593,216 bytes on disk**, and a **128 MiB WAL**. Its dirty reason
  remains `non-watched filesystem mounts changed; reconciling visible paths`.
- Dux PID **4093354** remains active with zero systemd restarts and has consumed
  about **91% of one CPU** in the point sample. The service's effective quota is
  **100% of one CPU**; CPU PSI `some avg10` is **19.36%**, near the established
  20% pressure gate. Memory PSI is zero. These conditions are not suitable for
  a build, full test suite, migration, scale run, or database integrity scan on
  the production host.
- Installed package is `dux 0.6.3-16`; `/usr/bin/dux` identifies itself as
  `0.6.3 (a42739776bbd+modified) [production]`, SHA-256
  `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. The
  current checkout is dirty and contains substantial unbuilt source changes.
  It is not source-bound to the installed executable; no candidate is promoted
  or claimed qualified.
- Root has **80 GiB free**. `/tmp` has grown back to **7.0 GiB** and `/var/tmp`
  to **1.2 GiB** since the previous cleanup. The inventory sweep did not identify
  the producers; current temporary consumers were not cleaned in this checkpoint.
  A separate `du` inventory will be needed to attribute the new growth.
- The orphan inspection mount remains present at `/run/dux-ext4-inspection` on
  `/dev/loop0` (`ext4`, `ro,norecovery`). `losetup` reports the backing pathname
  `/tmp/dux-scale-f2azn2f8/filesystem.img`, which no longer exists in the
  filesystem; Dux still holds FD 9 to the mountpoint. Journal review counted
  **54 fanotify queue-overflow warnings** and one pending-backlog-cap drop at
  03:10 UTC on Nov. 3. The deleted loop backing storage remains kernel-held;
  detaching it needs planned maintenance to release Dux's mount mark. No unmount
  or loop-device change was made.
- No production binary, service, database, quota, mount, or index state was
  changed. Candidate source tests, exact accounting, overflow recovery, release
  build/package lifecycle, full-size migration/rollback, supported-platform
  qualification, scale, soak, canary, and coverage completion remain open.

### Current-source Rust regression attempt stopped at host-pressure gate — 2026-11-03 09:39 UTC

- Started `cargo test --locked --offline --all-features -- --test-threads=1`
  against this worktree with one build job, a systemd scope capped at **15% CPU**
  and **2 GiB memory**, swap disabled, low CPU/I/O weights, idle I/O scheduling,
  and `nice 19`.
- While compiling dependencies, CPU PSI `some avg10` crossed the **20% stop
  threshold** and reached **20.72%**. Stopped the exact scope immediately; it
  exited **143** after **2.110 CPU-seconds**, peaked at **387.5 MiB**, and used
  no swap. No Rust test binary or test result was produced. This is an
  incomplete pressure-gated attempt, not a test failure or pass.
- Verified the scope inactive and no Cargo/Rust compiler remained. The
  production Dux service and index were not changed. Retry the full suite on an
  isolated builder or a stable low-pressure interval; do not raise the PSI stop
  threshold on this production host.
- A post-stop read-only status sample at **09:39:54 UTC** still showed the
  installed daemon active with zero restarts, dirty/not caught up, **190,113
  pending**, **165,304 seconds behind**, **1,503,990 dropped events**, no scan,
  and `write_error=null`. Relative to the 09:37 sample, pending rose by **537**
  and lag by about **170 seconds**; the index remains unqualified despite some
  event and commit counters advancing.
- After the pressure stop, `cargo fmt --all -- --check` and `git diff --check`
  completed successfully. These checks do not substitute for compilation or
  execution of the Rust regressions.
- Ran `python3 -m unittest -q scripts.test_dux_package_lifecycle` in a separate
  scope capped at **5% CPU / 512 MiB memory**, with swap disabled and
  `TMPDIR=/dev/shm` to avoid creating event churn in Dux's watched `/tmp`. All
  **46/46 tests passed in 19.797 seconds**. The scope completed successfully
  and is inactive. A PSI sample taken immediately after completion briefly read
  **20.63% avg10**; a later post-run sample at 09:41 read **5.09%**. No further
  workload was started.
- Coverage is limited to package-helper, permission-policy, effective-unit
  verifier, tunnel-unit policy, rollback-helper, and readiness-harness unit
  behavior. These are not actual DEB/RPM install/upgrade/rollback transactions,
  Rust runtime tests, live service policy changes, or production index tests.
- Current static checks passed: `systemd-analyze verify` for both packaged
  units, shell syntax checks for the permission helper and release scripts,
  ShellCheck on those scripts, locked/offline Cargo metadata validation,
  `cargo fmt --all -- --check`, and `git diff --check`. These validate syntax
  and selected policy contracts, not binary behavior.
- At **09:41:27 UTC**, production remained active with zero restarts but dirty
  and not caught up: **190,351 pending**, **165,403 seconds behind**,
  **1,503,990 dropped events**, no scan, and `write_error=null`. The pending
  count was unchanged from 09:40:19 while reported lag increased by about 68
  seconds; this does not demonstrate convergence.

### Lower-quota Rust regression retry stopped at host-pressure gate — 2026-11-03 09:44 UTC

- Retried the same locked/offline all-features Rust suite with one build job,
  `TMPDIR=/dev/shm`, `CPUQuota=5%`, `MemoryMax=2 GiB`, swap disabled, and idle
  I/O. CPU PSI was **1.37% avg10** before launch, but reached **43.46% avg10**
  within about 20 seconds during dependency compilation. Stopped the exact scope;
  it exited **143**, never produced a Rust test binary, and no Rust test ran.
- At the last live scope sample, CPU time was **0.929 seconds**, memory peak
  **79.7 MiB**, and swap use zero. Verified the scope inactive and no Cargo or
  Rust compiler remained. This is an incomplete resource-gated attempt, not a
  source pass or test failure. It confirms that reducing the quota to 5% does
  not make this production host a safe Rust test builder.
- Root retained **79 GiB free**. Production remained active and untouched; use
  an isolated builder for the Rust suite and release build rather than retrying
  compilation on this host while CPU pressure is unstable.
- Post-stop status at **09:45:15 UTC** still showed the daemon active with zero
  restarts, dirty/not caught up, **194,374 pending**, **165,630.752 seconds
  behind**, **1,503,990 dropped events**, no scan, and `write_error=null`. Since
  09:44:11, pending rose by **3,586** while **128** more updates were committed;
  reported lag increased **65.2 seconds**. CPU PSI then read **5.71% avg10 /
  16.89% avg60**. This confirms backlog growth continues; it does not attribute
  the increase to the short stopped build attempt.

### Release-builder CLI and provenance tests — 2026-11-03 09:47 UTC

- Ran `python3 -m unittest discover -s scripts -p 'test_build_release*.py' -q`
  in a transient scope capped at **5% CPU / 512 MiB memory**, with swap disabled
  and `TMPDIR=/dev/shm`. All **13/13 tests passed in 47.701 seconds**. Scope
  result was success and it is inactive; the highest observed CPU PSI avg10
  during the run was **15.48%**. At the last live resource sample, scope CPU
  time was **2.149 seconds**, memory peak **61.9 MiB**, and swap use zero.
- Coverage includes rejecting unknown or ambiguous build arguments, mismatched
  version/revision/source digest, fault-enabled or non-static/non-auditable
  binaries, package failure without partial publication, rollback after a
  mid-publish rename failure, and Build-ID-matched debug symbols. The tests use
  mocked build/package tools; **no real release binary or DEB/RPM was built**.
- A first direct-module invocation failed during Python imports because it did
  not add `scripts/` to `sys.path`; it executed zero tests. The successful
  discovery invocation above is the authoritative result.
- No production service, database, mount, or index state changed.
- Latest read-only production status at **09:48:09 UTC** remains dirty/not
  caught up: **195,960 pending**, **165,804.036 seconds behind**, **1,503,990
  dropped events**, no scan, and `write_error=null`; Dux is active with zero
  restarts. Since 09:45:15, pending grew **1,586** and reported lag grew
  **173.3 seconds**, although events and commits advanced. CPU PSI was then
  **0.54% avg10**; low instantaneous pressure does not resolve the stale index.

### Rollback backup helper regression tests — 2026-11-03 09:49 UTC

- Ran `python3 -m unittest discover -s scripts -p
  'test_dux_backup_index.py' -q` in a transient scope capped at **5% CPU /
  512 MiB memory**, with swap disabled and `TMPDIR=/dev/shm`. All **12/12
  tests passed in 0.401 seconds**; the scope exited successfully and is
  inactive.
- Coverage includes v4/v5/v6 schema validation, transaction-consistent WAL
  backup, private output modes, hash-bound manifest, no-clobber publication,
  directory fsync ordering, symlinked database/sidecar rejection, unsupported or
  forged schema rejection, output-sidecar refusal, free-space admission, and
  writable-output-directory rejection. These are helper unit tests using
  disposable SQLite fixtures; they do not rehearse migration or restore of the
  full production index.
- No production files or service state were changed.

### Optional tunnel syscall filter hardening — 2026-11-03 09:54 UTC

- Added a syscall denylist to `packaging/dux-tunnel.service` for kernel
  management, mount/swap/reboot, clock changes, obsolete calls, raw I/O,
  debugging, CPU emulation, and `open_by_handle_at`. The tunnel does not need
  FID resolution; `dux.service` retains that syscall for fanotify indexing.
  Dropbear's required setuid/setgid capabilities remain in the tunnel's
  capability bounding set, and the tunnel stays disabled by default.
- `systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service`
  passed. The focused tunnel policy tests passed **2/2**. The transient syscall
  smoke test passed for both packaged filter lines: representative restricted
  syscalls were denied, `clock_gettime` remained available, and
  `open_by_handle_at` was allowed for the indexer and denied for the tunnel.
- On this systemd 255 host, the tunnel unit scored **3.2 OK**; an otherwise
  identical unit without the new syscall filter scored **4.5 OK**. This is a
  matched before/after comparison and supersedes the older 7.1 score from a
  different tunnel unit revision.
- Scope limit: these checks validate unit syntax and representative seccomp
  behavior only. They do not execute the current Dux binary, run Dropbear or
  OpenSSH under the exact packaged tunnel unit, or qualify real package
  install/upgrade/rollback behavior. The production service and index were
  not modified.
- The loopback SSH harness was improved to include captured stdout/stderr when
  checked commands fail. Its baseline integration run then passed **8/8 checks
  in each of both GatewayPorts modes** under a transient **5% CPU / 512 MiB / no
  swap** scope, with scratch files in `/dev/shm`. Applying the exact tunnel
  syscall filter to this test's OpenSSH `sshd` failed at verified-host-key
  login with a connection reset. The filter's `@mount` group includes
  `chroot`, which OpenSSH privilege separation may use. The failure does not
  test the local Dropbear child used by Dux; Dropbear is unavailable on this
  host, so compatibility with that child under the exact service policy stays
  unverified. Temporary SSH processes and scratch files were removed after
  each run. The harness's diagnostic helper tests passed **4/4**, and Python
  compilation plus `git diff --check` passed.
- Read-only production status at **09:54:17 UTC**: daemon active with zero
  restarts, but dirty/not caught up with **198,642 pending updates**, **166,173
  seconds behind**, **1,503,990 dropped events**, no scan in progress, and
  `write_error=null`. The latest status reports a dirty reason of changed
  non-watched filesystem mounts and a **128 MiB WAL**. Backlog growth and the
  dropped-event history remain release blockers; no readiness gate closes from
  this source-only tunnel change.
- The same read-only status reports the last scan **167,352 seconds** earlier;
  a process sample measured Dux at **91.5% of one CPU**, and the journal records
  fanotify queue overflows at **09:46:32** and **09:48:29**. Root now has **87
  GiB available** after stale `/tmp` and `/var/tmp` artifacts were cleared;
  `target/readiness` still holds **6.2 GiB** of qualification evidence. That
  retained evidence was not pruned as part of temporary-file cleanup.

### Temporary and cache cleanup — 2026-11-03 10:02 UTC

- Removed **245** stale top-level scratch/log entries from `/tmp` and
  `/var/tmp`, then removed **8** inactive, rebuildable Go/Node cache directories
  from `/dev/shm` after confirming no process held files in them. The cleanup
  recovered about **9 GiB** on the root filesystem and **8.6 GiB** of tmpfs;
  `df` now reports root **91 GiB used / 86 GiB available** and `/dev/shm`
  **3.4 GiB used / 9.5 GiB available**. Dux qualification reports and its
  current-source build/test scratch were retained.
- Remaining temporary entries are small systemd-private and Codex runtime
  directories, plus `/tmp/go-build1762785916` (**194 MiB**) and small test
  fixture directories owned by a live Go test process from another project.
  That job was confirmed active and its scratch was left intact. The
  production Dux service remained active with zero restarts; no index,
  database, configuration, or service state changed.

### Full Python suite attempts stopped at host-pressure gate — 2026-11-03 10:06 UTC

- Two attempts to run all `scripts/test_*.py` tests used a corrected
  `/home/dux` working directory, `TMPDIR=/dev/shm`, **5% CPU / 1 GiB memory / no
  swap**, and a 240-second runtime limit. Neither produced a test summary or a
  count, so neither is a pass or a failure result.
- During the first attempt CPU PSI `some avg10` reached **23.72%** at about 67
  seconds. The exact transient unit was stopped; its result and empty output
  provide no coverage evidence. A second attempt reached **20.48%** at 26
  seconds, before its redirected output log received any bytes; that exact
  unit was also stopped. The empty log and one empty temporary test directory
  were removed. No tests are reported from either attempt.
- An unrelated full Go test process remained active throughout. This is
  correlated host activity only; no causal attribution is made. At 10:06 UTC
  CPU PSI had fallen to **2.46% avg10**, but the full Python suite remains
  unqualified until a complete, captured run succeeds on a stable host or
  isolated builder. Dux remained active with zero restarts and its live index
  was not touched.

### Live production blocker refresh — 2026-11-03 10:07 UTC

- `/usr/bin/dux --version` reports **0.6.3 (a42739776bbd+modified) [production]**.
  The service is active as PID **4093354** with **zero restarts**, but status is
  dirty/not caught up: **208,731 pending updates**, **166,953.727 seconds
  behind**, **1,503,990 dropped events**, no scan in progress, no write error,
  and a **128 MiB WAL**. The last scan is **168,134 seconds** old and the dirty
  reason remains a changed non-watched filesystem mount.
- Since 09:54:17, pending work grew by **10,089**, reported lag by about **781
  seconds**, and commits advanced by **1,664**. Journal entries record more
  fanotify overflows at **09:55:45**, **10:03:55**, and **10:05:44**, plus a
  deferred full reconciliation at 10:05:44. Dux was still using about **91.5%
  of one CPU** at the sample. This confirms continuing backlog growth and
  event loss; it is not a source-matched diagnosis or exactness proof.
- Root had **86 GiB available**, `/dev/shm` **9.5 GiB available**, and CPU PSI
  `some avg10` was **0.20%**. No production service, index, database, or
  configuration was modified. The production exactness, overflow recovery,
  and source-matched performance blockers remain open.

### Full Python suite retry after external Go job ended — 2026-11-03 10:09 UTC

- Retried the complete suite after the previously observed Go test process had
  exited. The attempt again used `/home/dux`, tmpfs scratch, **5% CPU / 1 GiB / no
  swap**, and a 300-second limit. CPU PSI `some avg10` rose from **4.26%** to
  **26.63%** within 54 seconds, before the captured output log received any
  bytes. Stopped the exact transient scope; the session returned no unittest
  summary, so this attempt also provides no test result. Removed its empty log;
  no fixture files remained.
- This third pressure stop occurred after the identified external Go job had
  ended. It does not attribute the pressure to this suite or Dux, but confirms
  the full Python suite cannot currently be completed safely on this host.
  The Dux service remained active with zero restarts; no production state was
  changed.

### Temporary storage cleared and backup interruption cleanup fixed — 2026-11-03 10:15 UTC

- A constrained online backup attempt was stopped when host CPU PSI reached
  **22.95%**, above the established stop threshold. SIGTERM terminated the helper before
  Python cleanup ran, leaving a **755,593,216-byte incomplete temporary SQLite
  file** under `/root`; it was confirmed to have no process owner and removed.
  No final backup or manifest was published, and the production database was
  not modified. This exposed a real cleanup defect in the backup helper.
- Updated `scripts/dux-backup-index.py` to allocate its database temp file
  inside the cleanup-protected block and defer SIGINT/SIGTERM to safe
  checkpoints. An interrupted operation now closes SQLite handles and removes
  temporary database and sidecar files. If interruption arrives after the
  verified database file is published, that recovery copy is retained.
  Added a regression that raises SIGTERM during hashing and verifies exit status,
  diagnostic, handler restoration, absence of output artifacts, and complete
  temp cleanup. `_exclusive_temp` now also closes and removes its new artifact
  if setting mode 0600 fails; a test verifies the descriptor is closed and the
  file removed.
- Focused backup-helper suite passed **14/14 in 0.400 seconds** under a **5%
  CPU / 512 MiB RAM / no-swap** scope. Tested helper SHA-256
  `ce6c18a7c2430cfa09aa1c1e286bfbffebec177415d183fdba43ff2059d87a6f` and
  regression-test SHA-256
  `197a816a7c8a631ad1258280f4c0c33df190aa193042ff0517da8bcba02fdfe0`.
  Python compilation and `git diff --check` passed. This verifies the helper's
  interruption cleanup path; it does not qualify a production-sized backup or
  restore, migration, or full Python suite.
- Cleared abandoned scratch/cache directories from `/tmp`, `/var/tmp`, and
  `/dev/shm`. Tmpfs use fell from **3.4 GiB to about 1 MiB**. Root now reports
  **91 GiB used / 87 GiB available**. Remaining temporary entries are small
  systemd-private and Codex runtime directories and PostgreSQL shared memory;
  they were preserved as live service/runtime state. `target/readiness` and
  other qualification evidence remain intact. No Dux service, index, database,
  or configuration change was made.
- Fresh read-only check at **10:15 UTC** confirms `/usr/bin/dux` is still
  `0.6.3 (a42739776bbd+modified) [production]`; systemd reports PID **4093354**
  active with **zero restarts**. Status remains dirty/not caught up with
  **208,615 pending**, **167,439 seconds behind**, **1,503,990 dropped events**,
  no scan, and no write error. The 128 MiB WAL and 168,618-second-old scan
  reinforce that cleanup does not resolve the live reconciliation blocker.

### Full Python suite attempt stopped by production-host CPU pressure — 2026-11-03 10:16 UTC

- Re-attempted full discovery after scratch cleanup, under a transient
  **5% CPU / 1 GiB RAM / no swap / 300-second** systemd scope with a one-second
  CPU PSI watchdog. Four release-builder tests passed; the fifth,
  `test_mid_publish_rename_failure_restores_previous_release_files`, was still
  running when host CPU PSI some avg10 reached **20.07%**. Stopped the exact
  suite scope at 10:16:43 UTC. The run exited 143 without a unittest summary,
  so it provides no full-suite result; the partial log is retained at
  `target/readiness/dux-python-suite-attempt-20261103T1016Z.log` (SHA-256
  `e2e216088055c66f5b9c458b7d2e10f808c19668729917e476fe2b33fd0b04f6`).
- Systemd confirms the suite scope is inactive. The Dux service remained PID
  **4093354**, active, with **zero restarts**. A post-run read-only status
  sample shows **208,503 pending**, **167,527 seconds behind**, **1,503,990
  dropped events**, no scan, and no write error. Counters advanced during the
  attempt; no index, service, package, or configuration change was made.
- Another full-suite run on this host would repeat load against a daemon
  already consuming about one CPU and failing to converge. Complete remaining
  broad test coverage on an isolated builder; the earlier **249/249** Nov. 2
  result remains valid for its recorded source/test snapshot but does not
  qualify today's changed worktree.
- The focused `scripts.test_dux_package_lifecycle` suite then passed **46/46 in
  18.898 seconds** in its own **5% CPU / 512 MiB RAM / no-swap** scope. The
  scope completed successfully and is inactive; CPU PSI some avg10 peaked at
  **17.72%** after completion. The Dux service remained active with zero
  restarts. This is source/policy regression coverage and does not demonstrate
  current DEB/RPM installation, rollback, or production effective-policy
  qualification.
- `cargo fmt --all -- --check`, `systemd-analyze verify` on both packaged
  service units, Python byte-compilation of the backup helper/test, and
  `git diff --check` all passed after these changes. Formatting and unit syntax
  checks do not substitute for compilation, runtime sandbox testing, or final
  package qualification.

### Isolated full Python qualification and VM scratch cleanup — 2026-11-03 10:36 UTC

- The complete Python discovery suite ran in a disposable Ubuntu 24.04.5 guest
  from a writable ext4 source copy. It reported **253 tests, OK (skipped=3),
  exit 0** in **26.401 seconds**. Release-builder tests used locked offline
  cargo metadata from the current `Cargo.lock` and narrow cargo/rustc shims.
  The three skips are the debug-artifact test and two split-debug-symbol tests,
  which require guest compiler/binutils tools absent from the base image. The
  raw full-suite log was not durably recovered after the host pressure guard
  stopped the disposable guest; the captured test summary is preserved in
  `target/readiness/dux-python-full-writable-20261103.summary.json` and is
  explicitly a summary, not a raw log. This does not qualify Rust tests, a
  current release build, package install/rollback, migration, or production
  performance/endurance.
- The guest used the Canonical Ubuntu 24.04 cloud image whose signed checksum
  was verified before boot (SHA-256
  `04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94`). Its
  2-vCPU guest was constrained to a 25% host CPU quota and 2.5 GiB memory.
  The host pressure guard stopped the guest after the test summary was
  collected. The recovery mount had been backed by a disconnected NBD device;
  Dux detected the mount topology change and systemd restarted the service.
  That interim scan logged 128 I/O errors beneath `/mnt/dux-vm-recovery`.
- Stopped Dux cleanly, unmounted and removed the disposable recovery root,
  disconnected `/dev/nbd15`, and restarted the service. The mountpoint and NBD
  partition are now absent. The new daemon (PID **2212602**, zero systemd
  restarts in its new activation) completed reconciliation on three
  filesystems at **10:36:42 UTC**: **1,226,356 files**, **110,779 directories**,
  and **0 scan errors**. At **10:36:52 UTC**, status reported `dirty=false`,
  no active scan, **400 pending updates**, **81.6 seconds behind**, zero drops
  since this daemon activation, **128 updates committed**, and no write error.
  The index is not caught up yet. The fresh process's zero drop count resets
  the earlier daemon's counters; it does not erase the previously observed
  drop/backlog history or independently prove exact current contents.
- Removed the stopped guest overlay from `/dev/shm`; tmpfs is now about **1.1
  MiB** used, `/tmp` **916 KiB**, and `/var/tmp` **160 KiB**. Small live
  systemd/Codex and database runtime entries were preserved. After completing
  the VM run, removed its **593 MiB** Ubuntu base image, signed checksum files,
  guest state, and pressure-guard log from `target/readiness/vms`; the captured
  suite summary and qualification notes remain. Root now has about **89 GiB
  available (50% used)**.
- Followed up on the three guest skips on the host, where `cc` and binutils are
  installed. `scripts.test_build_release_debug_artifact` and
  `scripts.test_dux_split_debug_symbols` passed **3/3 in 17.896 seconds** in a
  transient scope limited to **5% CPU / 512 MiB RAM / no swap**; the scope
  exited successfully. CPU PSI some avg10 briefly reached **18.56%**, then
  fell to **3.27%** 12 seconds later. Dux remained active as PID **2212602**,
  with zero restarts. At 10:37:08 its status observer recovered and reported
  `dirty=false`, **469 pending**, **159.5 seconds behind**, zero drops since
  activation, **256 updates committed**, and no write error. It remains behind
  and is not caught up; the earlier daemon's historical drop counts remain
  relevant.
- A later read at **10:38:31 UTC** still showed the daemon live, `dirty=false`,
  and no write error, but `watch_status_available=false` and all live queue
  counters were null. The heartbeat continued updating (10:38:41), while the
  status record was last written at 10:38:21; current source accepts status
  records only when at most five seconds old. The daemon was using about **61%
  of one CPU** in that sample, with CPU PSI avg10 **1.02%** and no systemd
  restart. This is an observability/catch-up qualification gap; it does not
  identify the cause in the source-unmatched installed binary.
- At **10:39:51 UTC**, the observer was available again. Dux remained active
  with zero automatic restarts, `dirty=false`, no scan, and no write error, but
  still was not caught up: **439 pending**, **261.8 seconds behind**, **512
  updates committed**, and zero drops in this activation. CPU PSI some avg10
  was **17.41%**. Root had **89 GiB available / 50% used** after VM artifact
  removal. This supports preserving a live convergence blocker; it is not a
  clean production exactness or performance qualification.
- Current-source release build attempt at **10:43 UTC**: prerequisites were
  present, offline Cargo resolution was requested, disk headroom was **88 GiB**,
  and the build ran in a **5% CPU / 2 GiB RAM / no-swap** systemd scope. Rust
  compilation began, then host CPU PSI some avg10 reached **37.21%** while an
  unrelated XGen Go race-test compile was also active. Stopped only the Dux
  build scope (exit **143**); systemd confirms it is inactive. No executable,
  package, or candidate manifest was published. Removed the exact partial
  14 MiB Cargo target and its empty output directory after confirming no
  compiler remained. The production service was not restarted or modified.
- Retrospective safety note: that first target directory was under
  `/home/dux/target`, inside the filesystem Dux watches. Its creation/removal
  could have added event churn; the later overflow at 10:45:46 also overlapped
  unrelated XGen tests, so causality is unknown. Do not repeat builds in the
  watched host tree. The subsequent private-mount attempt avoided that path.
- At **10:44:34 UTC**, Dux remained active as PID **2212602**, zero systemd
  restarts. Status showed `dirty=false`, no scan, **8,557 pending paths**,
  **544.7 seconds behind**, **97,951 events seen**, **79,300 resolved**,
  **1,024 updates committed**, zero drops in this activation, and no write
  error. It is not caught up. The live executable reports **0.6.3** and its
  `/run/dux/watch.status` record is format **v3**, while current source writes
  format **v4**; this confirms the running binary is not built from this exact
  checkout. The per-process zero drop count does not clear historical loss.
  CPU PSI avg10 had recovered to **1.86%** (avg60 **12.63%**), root had **88
  GiB free**, `target/readiness` was **6.2 GiB**, and `/tmp` had returned to
  **932 KiB** after the external Go build ended. The earlier 447–565 MiB Go
  build directories were live and were not removed.
- After the first build stop, a second attempt used a private mount namespace
  with a temporary tmpfs at `/tmp`, so Cargo's many intermediate files were
  not written into Dux's watched filesystem. It was limited to **5% CPU / 3
  GiB RAM / no swap**. CPU PSI avg10 rose from **18.81%** at 10:48:08 to
  **31.87%** at 10:48:22 while multiple compiler workers were present; stopped
  that exact scope (exit **143**). No package or binary was copied to the
  workspace, and the private tmpfs disappeared with the process. The separate
  production-tree attempt and this isolated attempt both confirm that the
  current host cannot yet provide a stable release-build window.
- The same live overflow remains unresolved. At **10:48:22 UTC**, status still
  showed `dirty=true`, **9,265 pending**, **784.8 seconds behind**, **112,558
  events seen**, **83,437 resolved**, **1,792 committed**, zero known userspace
  drops, and no write error. The v3 status record indicates `kernel_empty=0`
  and `uncertain=1`; the journal logged a fanotify queue overflow at **10:45:46
  UTC**. No new reconciliation had started. Kernel overflow count is not
  available in the installed v3 record, so zero userspace drops must not be
  interpreted as zero total event loss.
- At **10:48:42 UTC**, `/` had **88 GiB free**, `/tmp` **173 MiB**, and
  `target/readiness` **6.2 GiB**. The temporary files were
  `/tmp/xgenstack-api-readiness` (**105 MiB**) and
  `/tmp/xgenstack-agent-readiness` (**75 MiB**); they belong to the active
  XGen host workload and were preserved. The remaining compile/build temp
  directories were absent. No production restart or index mutation was made.
- Follow-up at **10:50:39 UTC**: the same daemon remained active with no
  restarts, but status still showed `dirty=true`, no scan, **10,281 pending**,
  **910.6 seconds behind**, **117,922 events seen**, **87,764 resolved**,
  **2,304 committed**, zero userspace drops, and no write error. The backlog
  grew from 9,265 to 10,281 after the 10:48 sample; the overflow-triggered
  dirty state has not reconciled. CPU PSI avg10 was **4.06%**. `/tmp` was **259
  MiB**, including a **12 KiB** live Go build tree and a running XGen WordPress
  Go test; these were preserved. The two XGen readiness outputs remain about
  **173 MiB** and are outside Dux ownership.
- Production/candidate identity check: `dpkg-query` reports the installed
  package as **0.6.3-16**. `/usr/bin/dux --version` reports commit
  `a42739776bbd+modified` but no embedded source digest; its SHA-256 is
  `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. The
  retained **0.6.3-62** static candidate from Nov. 2 has a verified package
  manifest and source digest `d0bfef3c0838fc3f29916d60e61d1dc93a4900650a0bf42b86c53739969c4cce`,
  but a different binary SHA-256 (`e878cb4c5e82c1055977cc4af1cf60dc16a41e4ca4c3f7418920e8976384a43f`).
  At the identity-check time, the source tree was
  `99477a55564fee1d4f9b238de7186b49d74df263de6b426eacdcbc0dfed85694`.
  Therefore the installed package is behind the retained package revision,
  and neither the retained candidate nor the installed binary proves identity
  with today's checkout. A new candidate has not been built. Deployment remains
  gated by exact source/build identity, live kernel overflow and backlog,
  v5-to-v6 migration/rollback qualification, and the production canary gates.


### Live recovery follow-up and temporary-storage cleanup — 2026-11-03 11:03 UTC

- Following the 10:57:57 UTC graceful restart, daemon PID **2256237** completed a full reconciliation at **10:59:18 UTC**: **1,238,273 files**, **111,033 directories**, **89.63 GiB**, **0 scan errors**. The daemon logged that its backlog persisted at 10:59:28. It remains active with **zero systemd restarts**.
- The live v3 status record advanced between **11:01:36** and **11:02:32 UTC**: pending paths **251 → 842**, events seen **74,649 → 79,412**, resolved **58,628 → 60,772**, committed updates remained **384**, and lag rose **218.7 → 274.9 seconds**. At 11:02:32 it reported kernel queue not empty, zero known userspace drops, no scan, no write error, and `uncertain=0`. This indicates the daemon is processing some events but backlog/lag are growing; it is not caught up. The v3 record has no kernel-overflow counter, so historical and new kernel loss cannot be excluded from its zero userspace-drop count. CPU PSI some avg10 was **0.04%** at 11:01:31; Dux used about **68% of one CPU** cumulatively at 11:01:48.
- The temporary-storage cleanup removed the stale `/tmp/xgs-go-cache` (**832 MiB**), XGen readiness outputs (**about 180 MiB**), small stale test scratch directories, and small gate/maturity JSON outputs after confirming no process had them open. `/tmp` fell from **1,005 MiB to 684 KiB**; root went from **90 GiB used / 87 GiB free** to **89 GiB used / 88 GiB free**. Remaining `/tmp` and `/var/tmp` contents are small systemd-private runtime directories/sockets; they were retained to avoid disrupting active services. Retained Dux qualification artifacts under `target/readiness` (**6.2 GiB**) were not deleted.
- `fs.fanotify.max_queued_events` remains **65,536** only as a runtime setting; it is not persisted. Do not infer that this larger queue resolved the catch-up issue. Do not lower it while the kernel queue reports non-empty.
- Current release build, package transactions, production v5→v6 migration/rollback, exact tree reconciliation, sustained queue convergence, source-matched kernel-overflow telemetry, and production canary qualification remain open.


### Constrained current-source release-build retry stopped on host pressure — 2026-11-03 11:04 UTC

- Retried the source-bound release build into `/dev/shm` (a tmpfs superblock not among the three devices watched by the installed daemon), with `/tmp` private tmpfs, `CARGO_NET_OFFLINE=true`, one Cargo job, package revision **63**, CPUQuota **20%**, MemoryMax **6 GiB**, and swap disabled. This kept target/output away from the watched ext4 tree. The build scope was stopped at **11:04:37 UTC** after host CPU PSI some avg10 reached **65.67%** (avg60 **37.08%**) and `dux status` briefly had no fresh watch-status record. The same sample showed external XGen Go compiler processes at up to **152% CPU**, plus `go build ./...`; Dux status later refreshed.
- Only the exact release-build scope was stopped; it exited **143** and systemd reports it inactive. No Cargo/Rust compiler remained, no release output or candidate package was produced, and no Dux service/index/package/configuration was changed. The transient 847 MiB copied Cargo home and empty build/output directories in `/dev/shm` were removed after scope termination.
- At **11:04:42 UTC**, production Dux was still active with **zero systemd restarts**, fresh status, no scan, no reported write error, and zero known userspace drops, but it remained behind: **2,613 pending paths**, **405 seconds lag**, **84,849 events seen / 64,850 resolved / 640 committed**, and the kernel queue was not known to be drained. Host CPU PSI remained high. Do not retry build or test workloads on this host while this pressure and external compilation persist; use the offered VPS once reachable or wait for a verified quiet window.


### Current-checkout static and secret-scan checks — 2026-11-03 11:06 UTC

- `gitleaks` with full redaction exited **0 with no findings** across `src`, `scripts`, `packaging`, `docs`, `.github`, `vendor`, and top-level `README.md`, `Cargo.toml`, `Cargo.lock`, `build.rs`, and `rust-toolchain.toml`. This is a secret-pattern scan of those paths; it is not a general security audit or a proof that no secrets exist.
- Current-checkout `cargo fmt --all -- --check`, `shellcheck` for the release/verify/index-permission scripts, `systemd-analyze verify` for both packaged unit templates, `cargo metadata --locked --offline --no-deps`, and `git diff --check` all exited **0**. These static checks do not compile or exercise the production binary.
- Full Rust/Python results recorded in the main audit are prior qualification evidence, not a substitute for a current source-bound release build and package lifecycle run. This host remains under active XGen Go compile/vet load (CPU PSI some avg10 **31.93%** at 11:05:35 UTC); no full suite was started during that pressure.
- SSH configuration inspection found only aliases `192.168.5.106` and `forgejo-git`; `192.168.5.106` resolves to the Git user on port **2222**, not an identified clean builder. A batch-mode read-only probe returned `No route to host`. No VPS credentials/endpoint are configured, so isolated build/migration/scale testing remains unavailable until the offered VPS details are supplied.


### Live Go-test scratch and Dux backlog refresh — 2026-11-03 11:07 UTC

- At **11:06:38 UTC**, `/tmp` had grown to **1.4 GiB**, all in `/tmp/x.Kq4Ws`; process **2284912** (`go test -p 2 ... xgenstack/agent/...`) was live under `/home/xgenstack.com/backend` and writing the tree. This is external host test scratch, not a Dux-owned build. It was retained while the writer is active; deleting it could break that test. Other temp storage remained small (`/var/tmp` **160 KiB**, `/dev/shm` **1.1 MiB**). Root had **86 GiB free**.
- Dux status at 11:06:38: daemon PID **2256237**, active, zero restarts; v3 record had **7,399 pending paths**, **520.6 seconds lag**, **95,402 events seen / 71,125 resolved / 768 committed**, kernel queue **not empty**, userspace drops **0**, `uncertain=0`, no active scan or write error. Relative to 11:04:42, pending rose **2,613 → 7,399** and lag **405 → 520.6 seconds**. It is not converging during the sampled host workload; no statement is made about whether that Go test alone caused the backlog. CPU PSI some avg10/avg60 was **0.92% / 14.42%**.
- The temporary 65,536 fanotify queue cap remains unpersisted. Historical kernel overflow accounting is unavailable from this v3 daemon record. The growing pending queue and absent source-matched overflow count continue to block exactness/canary sign-off.


### Rebuilt and verified current-source handoff for isolated qualification — 2026-11-03 11:08 UTC

- Ran the four `test_dux_source_archive.py` cases with temporary files on `/dev/shm` and bytecode generation disabled; **4/4 passed** in **1.118 seconds**.
- Generated `/dev/shm/dux-current-source-handoff-20261103T1108Z/` from the current worktree. It contains **126 source entries**, tree digest **`847197bb01bfe2d9103e82b014196e5ae93bf2451bd108c49cbabea43d071820`**, deterministic `source.tar.gz` (**641,035 bytes**, SHA-256 **`fb636d9fc1556ba3e477d73a66f1e8d22fe31dfb9efc425ae47f94a67624e956`**), and `SOURCE_MANIFEST.json` (SHA-256 **`039f23e561a133b43544dc500e28c435ad6f71d313d53c8fcb3a0582ac2b7bfc`**). `SHA256SUMS`, all tar members, file lengths, and per-file hashes were independently verified. This is a source-only handoff; it contains no executable or package. It is in tmpfs and will be lost at reboot.
- At **11:08:12 UTC**, the XGen Go test remained active and Dux was still not caught up: **11,293 pending paths**, **614.1 seconds lag**, **109,989 seen / 76,962 resolved / 896 committed**, with fresh status and no write error. No Dux service or index change was made.


### Full Python harness suite on the current checkout — 2026-11-03 11:09 UTC

- Ran `python3 -m unittest discover -s scripts -p 'test_*.py' -q` with `TMPDIR=/dev/shm`, `PYTHONDONTWRITEBYTECODE=1`, `nice 19`, and idle I/O priority while host CPU PSI some avg10 was **0.16%** and avg60 **1.60%**. Result: **253/253 passed** in **5.579 seconds**. Test scratch stayed on the un-watched `/dev/shm`; no bytecode was written.
- This is current Python harness/helper coverage only; it does not compile Rust, build a package, or qualify the live index. The Rust source-bound build and production recovery gates remain open.


### Revalidate production identity and live telemetry cadence — 2026-11-03 11:11 UTC

- Re-read the installed identity and live DB: package **0.6.3-16**, `/usr/bin/dux` version `0.6.3 (a42739776bbd+modified) [production]`, binary SHA-256 **`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`**; database `meta.schema_version=5`, while the checkout requires schema **6**. The daemon remains active as PID **2256237**, systemd restarts **0**. Do not replace it without a tested full rebuild/rollback path.
- At **11:10:58 UTC**, the raw v3 record had last updated at 11:10:48: pending **14,966**, lag **770.8 seconds**, **125,875 seen / 82,312 resolved / 1,152 committed**, `kernel_empty=0`, `uncertain=0`, and zero known userspace drops. `dux status --json` returned `watch_status_available=false` at that instant because its freshness limit is five seconds while this v3 record was about ten seconds old. This is a real intermittent telemetry-availability gap in the installed CLI/daemon pairing; do not interpret null counters as healthy or as zero.
- The external XGen Go test process remained active past five minutes with child package tests and a compiler; its `/tmp/x.Kq4Ws` scratch grew to **1.9 GiB**. It remains outside Dux ownership and must stay until all writers exit. Root had **86 GiB free**; the production queue remains large and not known drained.


### Source review of live status freshness mismatch — 2026-11-03 11:12 UTC

- Reviewed the exact current-source path: `src/watch.rs` publishes pipeline status on each main watcher cycle and republishes a conservative snapshot at least every **2 seconds** during governor sleep; `src/util.rs` accepts records no older than **5 seconds**. These intervals are compatible in this checkout.
- The intermittent stale/NULL status observed on production therefore remains attributable to the installed **v3** daemon/CLI pair, which is source-unmatched and sometimes left its status record older than the five-second reader limit. This is not evidence of a defect in the current writer cadence. No TTL relaxation was made because accepting older queue snapshots could misrepresent live backlog/overflow state. Validate after a source-matched candidate is built and running.
- At **11:11:59 UTC**, the live raw record still showed **18,534 pending**, **841 seconds lag**, **143,027 seen / 87,400 resolved / 1,152 committed**, and `kernel_empty=0`; the Go test remained active with **2.0 GiB** in its temporary build tree.


### Host contention and live queue follow-up — 2026-11-03 11:13 UTC

- At **11:13:27 UTC**, production Dux remained active as PID **2256237** with zero restarts. The raw v3 status was fresh but the index was not caught up: **18,963 pending paths**, **928.99 seconds lag**, **155,092 events seen / 88,328 resolved / 1,280 committed**, `kernel_empty=0`, zero known userspace drops, `uncertain=0`, and no write error. This queue state has not advanced since the 11:12:21 commit while lag continued to increase.
- Host CPU PSI some avg10/avg60 was **46.82% / 24.60%** while external XGen PID **2333905** ran `go test -race ...` with multiple Go compiler workers. `/tmp/x.Kq4Ws` was **2.3 GiB** and actively written; root had **85 GiB free**. It was preserved as live test scratch. No release build or additional qualification workload was started.


### Refresh Rust advisory audit for the current lockfile — 2026-11-03 11:14 UTC

- `cargo audit --no-fetch --json` exited **0** against the current **229-package** `Cargo.lock` (SHA-256 **`fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`**): **0 known RustSec vulnerabilities**, **0 warnings**. The local advisory repository is at commit **`537440b6e46d3970ba9a6719e6e37bfd86ca5134`**, committed **09:37 UTC** today. The JSON report was written to `/dev/shm` and removed after summarizing.
- Scope is locked Rust dependencies versus the local advisory snapshot. It does not cover OS/native dependencies, binary provenance, or complete dependency metadata for the installed executable; the existing binary scan recovered only 44 dependencies and remains inconclusive.


### Current package hardening and live recovery status — 2026-11-03 11:21 UTC

- Restricted the packaged indexer service to `AF_UNIX` sockets by default. The optional `--alert-exec` systemd override explicitly requires `AF_UNIX AF_INET AF_INET6`; the package lifecycle verifier rejects a missing `AF_UNIX`, unknown family, or network family without that hook. The README now documents applying the hook through a systemd drop-in instead of launching a second daemon that would contend for the database lock. This hardening is source-only; it has not been installed or activated on the production host.
- Validation on the current checkout: the full Python harness suite passed **258/258** in **4.966 seconds**; `cargo fmt --all -- --check`, `git diff --check`, and `systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service` all exited **0**. Earlier same-checkout transient systemd smoke verified that AF_UNIX works, AF_INET/AF_INET6 are denied for the indexer policy, and the optional tunnel policy can use its configured network families. These checks do not build or validate a release executable/package.
- Read-only live samples at **11:20:16** and **11:21:32 UTC**: installed `/usr/bin/dux` is still **0.6.3 (a42739776bbd+modified) [production]**, service PID **2256237**, zero systemd restarts. At 11:20:16 it was dirty and not caught up with **24,591 pending paths**, **1,354.3 seconds behind**, **344,226 events seen / 105,271 resolved / 2,176 committed**, **95,460,487,168 indexed bytes**, **418,230,272 database bytes**, **128 MiB WAL**, no reported userspace drops or write error, and no scan active. At 11:21:32 it remained dirty/not caught up with **24,165 pending**, **1,411.7 seconds behind**, **374,731 events seen / 107,173 resolved / 2,688 committed**, no write error, and no scan active. PID CPU was **71.9–73.0%** across the samples. A fresh fanotify queue overflow was logged at **11:20:23 UTC**, so zero userspace-drop counts do not establish event completeness. At 11:21:32 CPU PSI some avg10 was **0.00%** and I/O PSI some avg10 **0.45%**; despite low host contention, the daemon has not caught up and continues to use about 73% of a CPU.
- The effective installed unit is still the legacy unit with no `RestrictAddressFamilies` limit; the hardened AF_UNIX-only rule exists only in the package source. Do not infer production security improvement until an exact candidate is built, package-qualified, and deliberately deployed.
- Cleanup at **11:20 UTC** left `/tmp` at **684 KiB** and `/dev/shm` at **1.1 MiB**. Active systemd-private and Codex runtime directories/sockets were retained. Root filesystem reports **89 GiB used / 88 GiB free**; no meaningful root-space increase was attributable to this final cleanup because the earlier multi-GiB test tree had already disappeared.
- Open release gates remain: exact live convergence and recovery after fanotify overflow; schema-v5 installed database to current-schema migration and rollback; source-matched release build/package identity and signing; current-binary fault matrix; 100k/1m/10m performance budgets; supported Debian/Ubuntu and Rocky lifecycle; least-privilege residual review; uninterrupted 72-hour endurance; and a 24-hour exactness-monitored production canary. The current installed executable and index were not changed.

- Follow-up at **11:21:58 UTC**: still dirty/not caught up with **24,719 pending**, **1,439.1 seconds behind**, **376,178 seen / 108,578 resolved / 3,072 committed**, no scan and no write error. Relative to 11:21:32, pending rose **554** and lag grew **27.4 seconds** while seen/resolved/committed advanced **1,447 / 1,405 / 384**. This is active but insufficient processing relative to arrivals, not a no-progress interval. Service remained active with zero restarts; no recovery action was issued.


### Live CPU contention, scratch preservation, and current disk snapshot — 2026-11-03 11:23 UTC

- At **11:22:39 UTC**, Dux status counters were unchanged from 11:21:58: **24,719 pending**, **1,481.1 seconds behind**, **376,178 seen / 108,578 resolved / 3,072 committed**, no scan, no write error. At **11:23:17 UTC**, it was still dirty/not caught up with **28,040 pending**, **1,518.2 seconds behind**, **383,563 seen / 113,272 resolved / 3,200 committed**. Thus there was a short no-counter-progress interval followed by active work that did not keep pace with arrivals. No root-cause claim is made from these samples.
- Host CPU PSI some avg10 was **59.32%** at **11:23:08 UTC**; memory PSI was near zero, I/O PSI some avg10 **0.16%**. PID **2256237** used **88.6%** of one CPU in a two-second process sample, with zero measured block I/O. A concurrent external XGen `go vet ./...` (PID **2408242**) had active Go compiler/vet workers consuming additional CPU. The production Dux service, index and quota were not changed. Concurrent host load confounds attribution; the Dux counters show it is not currently catching up.
- `/tmp/x.Fb0xR` is **1.6 GiB** of active Go build/cache scratch, held open by that live `go vet` process. It was preserved; remove it only after the Go process and all child writers exit, then recheck open handles. Root had **85–86 GiB free** (about **52% used**); `target/readiness` occupies **6.2 GiB**, `/var/lib/dux` **528 MiB**, the protected root-only database backup **721 MiB**, and `/vms` is absent. No large temp cleanup was attempted while the writer is active.


### Follow-up: additional pressure deferral despite stale production status — 2026-11-03 11:25 UTC

- At **11:25:42 UTC**, live Dux remained dirty/not caught up with **37,231 pending paths**, **1,658.3 seconds behind**, **415,414 events seen / 125,913 resolved / 3,328 committed**, no scan and no write error. Service PID **2256237** remained active (zero restarts) at about **76.4% CPU**. Journal recorded another `deferring full reconciliation until host pressure returns to Normal` at **11:24:29 UTC**; no scan had started by the sample. The installed binary is the older source-mismatched v3-status daemon, so current-checkout pressure-admission code cannot be used to attribute this behavior.
- At 11:25:42, host CPU PSI some avg10/avg60 was **1.20% / 5.10%**, I/O PSI **3.05% / 1.97%**, memory PSI **0%**. These are later samples than the deferral and cannot prove which metric caused it. Concurrent XGen Go test PID **2419438** remained alive, and `/tmp/x.Fb0xR` was about **1.5 GiB**; it was not removed because its test workers still use it. No Dux restart, scan request, or resource-policy change was made.


### Disqualify stale retained root package from current release use — 2026-11-03 11:26 UTC

- Read-only inspection of `/root/dux-latest.deb` found Debian package `dux` **0.6.1**, amd64, **1,932,032 bytes**, mtime **2026-09-12 11:55:10 UTC**, package SHA-256 `8ef508d7ee7a755a3998ecad01a7e01649f467889406c496f5ff5ca86458f691`. Its embedded executable SHA-256 is `5390de755b52ee8bd91e6ed32e9b5b673038b59b3ed1746a899b9eb5ab165c78`, different from installed `/usr/bin/dux` SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`; no current source-digest marker was found in the payload. It predates the current schema/security work and is not a deployable current-source candidate. No package was installed or removed.


### Backlog growth continues while the live daemon remains source-mismatched — 2026-11-03 11:27 UTC

- Read-only status at **11:27:30 UTC**: PID **2256237** active, zero restarts, dirty/not caught up, **45,381 pending paths**, **1,772.2 seconds behind**, **504,228 events seen / 137,759 resolved / 3,584 committed**, `events_dropped=0`, no write error, no scan. Compared with 11:25:42, pending rose **8,150** and lag **113.9 seconds** while event/resolved/commit counters advanced **88,814 / 11,846 / 256**. Event processing is active but not keeping pace; zero reported userspace drops does not negate the recorded kernel overflow at 11:20:23. Installed unit remains unrestricted by `RestrictAddressFamilies` and source-mismatched.
- Root remains **52% used / 86 GiB available**. `/tmp` is **1.7 GiB**, still held by the live XGen Go test PID **2419438** and package test workers; leave it until the writers exit, then recheck and remove the stale scratch tree. No production service, index, or quota change was made.

- Follow-up at **11:27:54 UTC** had unchanged event/commit counters from 11:27:30 (**504,228 seen / 137,759 resolved / 3,584 committed; 45,381 pending**); lag reached **1,798.2 seconds**. The watch-status heartbeat was fresh, and `last_event_commit` was **11:27:16 UTC**, so this is a short no-commit interval with active health publication, not a dead process. XGen Go test PID **2419438** and test children were still live; `/tmp/x.Fb0xR` remained **1.7 GiB**. No process was signalled and no scratch file was removed.


### Bounded production CPU diagnosis and profiling policy — 2026-11-03 11:29 UTC

- A three-second per-thread sample at **11:28:48–11:28:50 UTC** showed the sole Dux thread at **100% CPU** on one core. The preceding status samples showed no commits since **11:27:16 UTC**; at **11:29:12 UTC**, counters had resumed (**569,786 seen / 139,795 resolved / 3,712 committed**) but the index remained dirty/not caught up with **46,848 pending**, **1,873.4 seconds behind**, and no scan. This confirms an interval of high CPU with little commit progress, followed by resumed activity; it does not identify the code path or establish a persistent hot loop.
- `perf` 6.8 is installed, but `/proc/sys/kernel/perf_event_paranoid=4` disallows performance-counter profiling. It was not lowered. The installed executable has zero defined function symbols and no matching debug file; even an unprivileged sample would not map to source. Effective CPUQuota is **100% of one CPU**. Cgroup `nr_throttled` was **920 / 15,404 periods** in a cumulative sample; no interval delta was collected. Effective unit I/O byte properties are unavailable (`IOAccounting` not active), so no Dux-specific I/O attribution is claimed. No source, service, sysctl, or index change was made.
- `/tmp/x.Fb0xR` remained held by live Go test PID **2419438** and children; it was retained.


### Repeated live overflow and growing external scratch — 2026-11-03 11:29 UTC

- Journal recorded a further `fanotify queue overflow — index may have missed events` at **11:28:22 UTC**. At **11:29:33**, Dux remained dirty/not caught up with **46,848 pending**, **1,895.4 seconds behind**, **569,786 seen / 139,795 resolved / 3,712 committed**, fresh status, no scan, and no write error. The last event commit timestamp remained 11:28:22. No exactness claim can be made from `events_dropped=0`.
- The external Go test PID **2419438** was still live at **6:05 elapsed** with linker workers writing `/tmp/x.Fb0xR`; the scratch had grown to **2.0 GiB** (`/tmp` total). It was left untouched. Root had **86 GiB free**. No service/database/configuration change occurred.


### Third observed overflow in the current incident window — 2026-11-03 11:29 UTC

- Another fanotify overflow was logged at **11:29:50 UTC**. Status at **11:29:55** remained dirty/not caught up: **47,353 pending**, **1,917.4 seconds behind**, **635,326 seen / 140,988 resolved / 3,840 committed**, no scan or write error. Relative to 11:29:33, counters advanced, but pending and lag increased. The kernel loss signal makes exactness unknown; the old installed binary's zero userspace-drop field is not a counter of kernel loss.
- External Go test PID **2419438** was still active with compiler/test workers; `/tmp/x.Fb0xR` was **2.0 GiB** and retained. No production or process change occurred.


### Preview test remains active after the first Go test exits — 2026-11-03 11:30 UTC

- The broad Go test parent PID **2419438** exited, but its runner started `go test -count=1 -timeout 5m ./agent/preview` as UID `nobody`, PID **2461032**, with `go`/`vet` processes still holding `/tmp/x.Fb0xR`. At **11:30:34 UTC**, scratch had grown to **3.0 GiB** and root had **85 GiB free (53% used)**. Do not delete until this second test process tree and all open references exit.
- At 11:30:34, Dux status counters remained unchanged from 11:29:55 (**47,353 pending, 1,957.4 seconds behind, 635,326 seen / 140,988 resolved / 3,840 committed**); status heartbeat was fresh, no scan or write error. This is a further no-commit interval after the 11:29:50 kernel overflow. No production mutation occurred.


### Test runner transition and renewed shared-host pressure — 2026-11-03 11:30 UTC

- Go test PID **2419438** and the `nobody` preview test exited, but the parent shell **2396871** (`bash scripts/test-fast.sh`) is still active and has started `go test -race -count=1 -timeout 5m ./internal/ws/... ./internal/router/... ./agent/core/...` as PID **2468433**, with many compiler/vet children. `/tmp/x.Fb0xR` has expanded to **2.0 GiB**; `/tmp` total **2.2 GiB**. It remains actively used; do not remove or kill it.
- At **11:30:55 UTC**, Dux counters were still unchanged from 11:29:55 (**47,353 pending**, **1,977.4 seconds behind**, **635,326 seen / 140,988 resolved / 3,840 committed**), no scan or write error. Host CPU PSI some avg10 had risen to **41.85%** while race-test compilation ran; I/O PSI remained under **1%**. The coincident workloads confound attribution, but production is not catching up and the journal has recent kernel overflow evidence. Root had **86 GiB free**. No test or daemon was stopped.


### No local qualification guest available; host test activity continues — 2026-11-03 11:31 UTC

- Read-only `dux-qualification-vm.py status` for Debian 12, Rocky 9, and Ubuntu 24 reports all three guests **missing/inactive**; `target/readiness/vms` has no usable guest state. No local VM was started because host CPU PSI avg60 was **21.83%** and the production watcher remained unhealthy.
- At **11:31:58 UTC**, Dux remained dirty/not caught up with **47,384 pending**, **2,040.8 seconds behind**, **671,760 seen / 141,588 resolved / 3,968 committed**, no scan/write error. At that sample host CPU PSI avg10/avg60 was **6.43% / 21.83%**, I/O PSI avg10 **0.17%**. The active `scripts/test-fast.sh` runner (PID **2396871**) was running Go race tests (PID **2468433**); `/tmp/x.Fb0xR` reached **3.8 GiB**. Root: **93 GiB used / 84 GiB free**. Scratch is active and was not removed.

- Follow-up at **11:32:20 UTC**: Dux remained dirty/not caught up with **48,007 pending**, **2,061.1 seconds behind**, **703,730 seen / 144,257 resolved / 4,096 committed**, no scan/write error. Since 11:31:58, pending rose **623** and lag **20.3 seconds** while counters advanced; host CPU PSI avg10/avg60 was **0.71% / 15.14%**. `/tmp` reached **4.0 GiB**; root remained **84 GiB free**. The Go race test runner was still live.


### Reconciliation deferral and fanotify overflow recur — 2026-11-03 11:32 UTC

- At **11:32:18 UTC**, the live journal recorded both `deferring full reconciliation until host pressure returns to Normal` and `fanotify queue overflow — index may have missed events`. The 11:32:33 sample remained dirty/not caught up with **48,007 pending**, **2,075.1 seconds behind**, **703,730 seen / 144,257 resolved / 4,096 committed**, no scan and no write error; no commits had advanced since 11:31:27. Reconciliation is repeatedly deferred while event coverage remains uncertain.
- `scripts/test-fast.sh` PID **2396871** and Go race test PID **2468433** were still live. `/tmp` was **3.9 GiB**; root had **84 GiB free**. Scratch was retained while open.


### Fresh status during next test-runner phase — 2026-11-03 11:34 UTC

- At **11:34:05 UTC**, Dux remained dirty/not caught up with **48,025 pending**, **2,167.4 seconds behind**, **705,722 seen / 145,003 resolved / 4,224 committed**, no scan and no write error. Compared with 11:32:20, counters advanced, pending rose slightly, and lag increased. Host CPU PSI some avg10 was **36.98%**. Journal still shows the paired **11:32:18** pressure deferral and kernel overflow; no later recovery scan is present.
- `scripts/test-fast.sh` parent **2396871** remained alive (12m23s elapsed). `/tmp/x.Fb0xR` was about **3.0 GiB** with concurrent Go build cleanup/creation activity; `du` observed transient missing build subdirectories. Root had **85 GiB free**. The conditional cleanup watcher remains waiting for the owner to exit.

- Follow-up at **11:34:44 UTC**: Dux remained dirty/not caught up with **49,452 pending**, **2,206.7 seconds behind**, **719,367 seen / 146,788 resolved / 4,352 committed**, fresh status, no scan/write error. In 39 seconds pending rose **1,427** and lag **39.3 seconds** while processing continued. CPU PSI some avg10/avg60 was **51.13% / 37.18%**; `/tmp` was **3.2 GiB**. The test-fast owner process remains alive between batches; no new overflow was logged after 11:32:18 in this sample.


### Tighten package security verifier option matching — 2026-11-03 11:36 UTC

- Fixed a verifier false-acceptance path: network address families are now allowed only when the parsed `ExecStart` argv contains the exact `--alert-exec` option (including equals form), not when that text appears inside another option or argument. Added regressions for `--alert-exec-helper` and a path containing `--alert-exec`; retained positive coverage for both valid forms.
- Focused effective-unit security tests passed **18/18**. Full current Python harness passed **261/261** in **5.674s**. `cargo fmt --all -- --check`, `git diff --check`, and `systemd-analyze verify` for both unit templates passed. These validate packaging verifier/source behavior, not the Dux Rust executable or current production service.
- At **11:36:37 UTC**, production Dux remained dirty/not caught up: **54,290 pending**, **2,319.95 seconds behind**, **725,891 seen / 151,963 resolved / 4,480 committed**, no active scan or write error. Last commit was 11:35:11; no counters advanced since that event. Installed unit still lacks the source AF_UNIX restriction. `/tmp/x.Fb0xR` was **3.5 GiB** with an active `scripts/test-fast.sh` stage writing `vitest.log`/npm logs; root had **84 GiB free**. No live service or index change occurred.


### External test scratch cleanup complete; production queue remains dirty — 2026-11-03 11:37 UTC

- Confirmed the `scripts/test-fast.sh` parent and Go/npm child processes have exited; both `/tmp/x.Fb0xR` and prior `/tmp/x.Kq4Ws` are absent. The guarded cleanup watcher finished. `/tmp` is **696 KiB** and `/dev/shm` **1.1 MiB**; root is **90 GiB used / 88 GiB free**. The prior external scratch reached about **3.5 GiB**. No active runtime directories were deleted.
- Read-only Dux status at **11:37:12 UTC** remains dirty/not caught up: **54,317 pending**, **2,372.9 seconds behind**, **760,038 seen / 161,386 resolved / 4,608 committed**, no scan and no write error. A prior pressure deferral at 11:35:11 remains unresolved; the most recent status shows event counters advanced but backlog persists.
- The package verifier exact-option fix remains source-only; focused tests 18/18 and full Python harness 261/261 passed. No release build, package install, or service/index modification occurred.


### R16 source recovery search and post-test Dux drain sample — 2026-11-03 11:39 UTC

- Independently recomputed the canonical source-tree digest for **158** retained `target/readiness/**/*.tar.gz` snapshots against the R16 build-manifest digest `3fa91992e998d8f6777c241be96061c5948b9b323978c0d204ff869b78224cc8`: **0 matches**, **0 corrupt archives**, 9 non-UTF8 member archives skipped (these cannot be inputs accepted by the current UTF-8 source collector). No R16 source archive is available to map the installed executable to exact source.
- Two status samples 30 seconds apart after external test completion show slow backlog reduction: pending **53,015 → 52,897**, events seen **793,248 → 793,353**, resolved **163,081 → 163,180**, committed **6,016 → 6,144**. Dux remained dirty/not caught up, behind **2,485.8 → 2,514.1 seconds**, no scan/write error; process used about **83.8% CPU**. CPU PSI avg10 was **0.00%** at the second sample. This indicates the daemon is making slow progress under low host contention, not recovered. Root remains **90 GiB used / 88 GiB free**; `/tmp` **696 KiB**.


### DISK-213 Refresh filesystem accounting and clear newly regenerated test cache — 2026-11-03 11:43 UTC

- Root filesystem is **185 GiB total / 90 GiB used / 87 GiB free (51%)**. `du -xhd1 /` attributes **32 GiB to `/var`**, **25 GiB to `/home`**, **23 GiB to `/root`**, and **6.6 GiB to `/usr`**.
- `/var` is primarily live service data: PostgreSQL **14 GiB**, containerd **6.8 GiB**, Docker **4.8 GiB**, XGenStack data **2.7 GiB**, Dux index **528 MiB**, plus **1.5 GiB** of Dux index backups. The two Dux index snapshots are each **755,593,216 bytes**, timestamped Nov 1 before upgrade/rescan; they are recovery evidence and were preserved.
- `/home` includes Dux **6.3 GiB** (qualification evidence **6.2 GiB**), XGenStack **7.8 GiB**, VPN projects **3.4 GiB**, and other active projects. `/root` includes `.local` **4.6 GiB** (largest file `rtk/history.db`, **2.87 GB**), `.cache` **3.7 GiB**, Go **2.9 GiB**, `.codex` **3.4 GiB**, `.rustup` **1.9 GiB**, and Codex archives **1.2 GiB**. These are not generic temp files and were not deleted.
- A new XGen Go test run had regenerated **635 MiB** at `/tmp/xgs-go-cache` plus Go compiler scratch. After verifying the test and compiler processes exited and the cache had no open users, the cache and completed temporary maturity report were removed. Go had already removed `/tmp/go-build*`. Final temporary usage: `/tmp` **692 KiB**, `/var/tmp` **160 KiB**, `/dev/shm` **1.1 MiB** (PostgreSQL shared-memory files are active and preserved). Root usage returned from **91 GiB** to **90 GiB**.
- No PostgreSQL, container, Dux index/backup, project, or system cache data was removed. The largest reclaim candidates now require service-specific retention/ownership decisions; this scan does not establish that they are disposable.


### PROD-HEALTH-214 Confirm live reconciliation remains stalled under recurrent pressure admission — 2026-11-03 11:43 UTC

- At **11:43:21 UTC**, installed Dux PID **2256237** was still running `/usr/bin/dux daemon /`, version **0.6.3**, SHA-256 `9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. It reported **69,822 pending paths**, **2,724 seconds behind**, **819,847 events seen / 184,548 resolved / 6,784 updates committed**, `dirty=true`, `caught_up=false`, no scan, and no write error. The dirty marker was refreshed at 11:42:01; last scan began 10:57:57 and completed at 10:59:18 per journal.
- The journal then emitted another `deferring full reconciliation until host pressure returns to Normal` at **11:43:23 UTC**. This is the third such deferral in the recent incident window; fanotify overflow was last logged at **11:37:44**. `events_dropped=0` does not prove no kernel loss. Dux is using roughly **85% of its 1-CPU systemd quota**. Around the sample, host memory had **21.7 GiB available**, root had **87 GiB free**, load was about **4.3–5.9 on 12 host CPUs**, and memory/I/O PSI were low. Those point samples do not explain the deferral; pressure hysteresis or different sampling timing may account for it, but the cause is unproven.
- The live SQLite metadata is schema **v5**. Current worktree source declares schema **v6**. Installed binary SHA and build age do not identify source, and the R16 manifest source digest remains unrecovered. A current-source executable cannot be treated as a safe drop-in until source-matched build provenance, v5-to-v6 migration/rollback qualification, and recovery exactness tests pass. No live action was taken.
- The daemon's current state remains a release-blocking data-integrity incident: do not advertise the index as caught up or exact until a verified full reconciliation and post-scan accounting complete.


### PROD-SECURITY-216 Restrict Dux service address families on the next start — 2026-11-03 11:46 UTC

- Effective installed unit inspection showed `RestrictAddressFamilies=~` (unrestricted); the service runs as root with `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`. Its effective `ExecStart` is `/usr/bin/dux daemon /`, with no `--alert-exec`; `Environment=` is empty and no hook configuration was found. The packaged source unit already restricts Dux to `AF_UNIX` by default.
- Added `/etc/systemd/system/dux.service.d/70-address-families.conf` with `RestrictAddressFamilies=AF_UNIX`, then ran `systemctl daemon-reload` and `systemd-analyze verify dux.service` successfully. `systemctl show` confirms effective `RestrictAddressFamilies=AF_UNIX`.
- Dux PID **2256237** and its start timestamp **10:57:57 UTC** remained unchanged; no restart was issued because the live index is dirty and far behind. The address-family restriction is loaded for the next process start and is **not claimed to constrain the already-running process**. If a networked alert hook is introduced later, the unit and verifier must be deliberately updated together.


### PROD-PERF-217 Abort source release build after measurable host contention; confirm Dux still processes slowly — 2026-11-03 11:47 UTC

- Attempted `cargo build --release --locked --offline --jobs 1` in a transient systemd scope capped at **50% CPU**, CPU weight **20**, idle I/O priority, and nice **19**. During the first 30 seconds CPU PSI `some avg10` rose from **0.03% to 6.81%** while production Dux was already dirty and backlogged. Interrupted the build with exit **130**; it produced no executable or package. The partial `target/release` build tree (**92 MiB**) was removed after confirming compiler processes exited. This is a stopped build, not a build failure or test result.
- Read-only cgroup sampling after the build stopped showed Dux used **7.89 CPU seconds in an 8-second interval**, with exactly one process thread and no increase in cgroup `nr_throttled`. The process is CPU-bound in userspace and the current 100% CPU quota is not observed throttling it during that interval; increasing the quota is not supported as a remedy by this evidence.
- Between status samples around **11:45:22–11:47:20**, pending paths went **70,976 → 70,060**, events seen **857,768 → 870,822**, resolved **191,255 → 196,975**, committed **7,424 → 8,960**. The path backlog reduced slightly, but `behind_seconds` grew **2,844.6 → 2,963.0** and the index remained dirty/not caught up with no scan. This shows live processing continues but incoming work/lag is not converging. Last observed kernel overflow remains **11:37:44 UTC**; last reconciliation deferral is **11:43:23 UTC**.
- No current-source Rust test or candidate build has passed. Further compile/test load on this production host is deferred until the watcher has recovered or an isolated VPS is provisioned.


### PROD-MAINT-218 Remove the orphan loop test mount and reconcile the remaining root — 2026-11-03 12:00 UTC

- Confirmed `/run/dux-ext4-inspection` was an **8 GiB sparse loop mapping** backed by the deleted `/tmp/dux-scale-f2azn2f8/filesystem.img`, mounted `ro,norecovery`; the only process holder was Dux PID **2558658**. At 11:52:56 Dux was dirty/not caught up with **7,429 pending paths**, **269.8 seconds behind**, and no scan active.
- Executed a controlled sequence: requested `systemctl stop dux`; the daemon logged `received SIGTERM/SIGINT — drained + flushed, exiting` at **11:54:36** (about **98 seconds** after stop was requested); then a **normal** `umount /run/dux-ext4-inspection` succeeded. A normal `losetup -d /dev/loop0` released the mapping. No lazy unmount or forced detach was used. `findmnt -T /run/dux-ext4-inspection` now resolves to `/run` tmpfs and no `/dev/loop0` mapping remains.
- Restarted the same installed **0.6.3** binary at **11:54:55**. The loaded service now runs with `RestrictAddressFamilies=AF_UNIX`; its startup reports **2 filesystems** (previously 3) and automatically starts a full reconciliation. At **11:56:15** it completed **1,234,823 files, 111,289 directories, 89.64 GiB, zero scan errors**. This rebuilt the index after removal of the stale test mount.
- At **12:00:14**, the daemon remained active with `dirty=false`, no scan, no write error, and **841 pending paths**, **315.1 seconds behind**, **68,612 events seen / 61,349 resolved / 512 updates committed / 0 userspace drops**. The queue was empty at that sample and event counters had not advanced since 11:59:44; pending work has drained **1,027 → 841** since scan completion. It remains `caught_up=false`; no exactness claim is made. The running v3 status format has no kernel-overflow counter; no overflow warning has appeared since this process started, but that is weaker evidence than a counter plus an independent exact comparison.
- Root remains **87 GiB free / 52% used**; `/tmp` is **692 KiB**. This cleanup resolves the known orphan test mount but does not close the live recovery/caught-up, source provenance, schema migration, or current-source qualification gates.


### PROD-SECURITY-219 Verify address-family restriction is active after restart — 2026-11-03 12:00 UTC

- `systemctl show dux` confirms PID **2558658** is active under the loaded `RestrictAddressFamilies=AF_UNIX` drop-in, with zero systemd restarts since its 11:54:55 start. `systemd-analyze verify dux.service` passes. The focused package-unit policy suite passes **18/18** tests, including positive restricted-unit/alert-hook cases and rejection of unrestricted address families without an exact alert option.
- This verifies the active unit configuration and verifier logic; it does not prove confinement of code paths outside systemd's address-family control or replace current-binary provenance and package-lifecycle qualification.


### PROD-HEALTH-220 Recovery scan succeeded but live mount/event churn made the index dirty again — 2026-11-03 12:01 UTC

- After the second scan completed at **11:56:15** with **0 errors**, Dux was briefly clean but not caught up. At **12:00:14**, it had **841 pending paths** and the fanotify queue was empty. By **12:01:06**, the persisted dirty marker had returned (`non-watched filesystem mounts changed; reconciling visible paths`), there was no scan active, pending had grown to **4,759**, and behind time was **370.2 seconds**. At **12:01:28**, it remained dirty/not caught up, with **4,759 pending**, **392.2 seconds behind**, **75,085 events seen / 66,058 resolved / 512 updates committed / 0 reported userspace drops**; the kernel queue was not empty. No kernel-overflow warning has appeared since the 11:54:55 process start, but the installed v3 status has no kernel-overflow count.
- Eight Docker `overlay` mounts were visible under `/var/lib/docker/rootfs/overlayfs` at 12:01:21. The current worktree contains `duplicate_overlay_mount_churn_does_not_reconcile_indexed_tree` and overlay topology filtering, but the installed binary's exact source is still unknown. The temporal correlation between mount changes and this dirty transition is suggestive only; the specific changed mount and cause are not captured by the installed log.
- At 12:01:28 the loop test mount remained absent and the service was active under `AF_UNIX`; root had **87 GiB free**. This confirms the orphan-mount cleanup and second baseline scan succeeded, while showing the live exactness gate remains open under continuing mount/event churn.


### ARTIFACT-REVIEW-221 Refresh exact source handoff and disqualify retained release 0.6.3-62 — 2026-11-03 12:03 UTC

- Created `target/readiness/current-source-handoff-20261103T1203Z/` from the authoritative current worktree: **126 source entries**, canonical tree SHA-256 `05ead01b400964e7499c54e93d7a21975671cd2a27a538b04272a73ed719391b`, `source.tar.gz` **654,443 bytes**. Verified all **126** archive members against per-file hashes and `sha256sum -c` passed for archive and manifest. The handoff explicitly has `binary_built=false`, `packages_built=false`, `production_qualified=false`.
- The retained 0.6.3-62 static candidate's manifest digest is `d0bfef3c0838fc3f29916d60e61d1dc93a4900650a0bf42b86c53739969c4cce`; its binary embeds that same digest. It is not a current-tree candidate: the current tree digest is `05ead…`; changed Rust inputs include `src/main.rs`, `src/query.rs`, `src/tui.rs`, `src/util.rs`, and `src/watch.rs`, plus packaging/verifier and docs. The installed `/usr/bin/dux` still reports 0.6.3 at revision `a427397…+modified` without an embedded source digest. No candidate was installed or run against production.
- The current source build remains unverified. The previous bounded production-host build was interrupted; current-source Rust tests, full migration lifecycle, fault/overflow qualification, and scale/soak remain open. The new handoff is ready for the isolated host whose SSH details were requested.


### PROD-HEALTH-222 Live backlog grows after source-matched source handoff refresh — 2026-11-03 12:04 UTC

- At **12:04:27 UTC**, active Dux PID **2558658** remained dirty with reason `non-watched filesystem mounts changed; reconciling visible paths`, no scan, no write error, **12,258 pending paths**, and **571.6 seconds behind**. Counters were **93,165 events seen / 76,183 resolved / 768 committed / 0 reported userspace drops**. The v3 `watch.status` format omits kernel-overflow accounting; no overflow warning has appeared since PID 2558658 started, but this cannot prove zero kernel loss.
- Across samples from **12:01:28 to 12:04:27** (179 seconds), pending rose **4,759 → 12,258** (+7,499), while events seen rose **75,085 → 93,165**, resolved **66,058 → 76,183**, and commits **512 → 768**. The service is processing events but arrival exceeds applied updates. The dirty marker dates from **12:00:43** and no recovery scan has started. The eight Docker overlay mounts and current-source overlay-churn test remain a plausible but unproven connection.
- A separate live XGen Go race-test build is writing **215 MiB** beneath `/tmp` (Go test PID **2573393**, active compiler/vet children). The scratch is retained while the writer is live. Root has **86 GiB free**; this is not the disk-capacity constraint.
- Dux service confinement remains active (`RestrictAddressFamilies=AF_UNIX`); the orphan test mount and loop device remain absent. Current-source handoff digest `05ead01b…719391b` is checksum-verified, but no matching binary exists. Current-source build and aggressive Rust/scale/fault tests still require a separate test host or a recovered production watcher.


### PROD-HEALTH-223 Correlate renewed Dux dirty state with Docker overlay mount churn — 2026-11-03 12:05 UTC

- `findmnt` showed **8** Docker overlay mounts under `/var/lib/docker/rootfs/overlayfs` at **12:01:21**; at **12:05:26**, only **4** remained (the same four earlier mounts persisted, four others had disappeared). The Dux dirty marker refreshed at **12:05:04** with `non-watched filesystem mounts changed; reconciling visible paths`. This timing strongly correlates the current dirty transition with Docker overlay lifecycle, though the installed log does not name the changed mount.
- Current worktree has an explicit overlay view filter and `duplicate_overlay_mount_churn_does_not_reconcile_indexed_tree` test. The installed binary is an older source-unmatched executable; its observed behavior is consistent with the fix not being present, but this cannot be attributed to exact source without the matching binary/source. No Docker service/container was stopped or altered.
- At **12:05:26**, Dux remained dirty/not caught up with **12,218 pending**, **629.1 seconds behind**, status format v3 reporting **95,065 seen / 76,771 resolved / 896 committed / 0 userspace drops**, and `kernel_empty=false`; no scan or write error. Around 12:05, CPU/memory/I/O pressure readings were low, so the renewed dirty state is not explained by observed host pressure. Kernel-overflow counts remain unavailable.
- The external Go race test finished and removed its build scratch. `/tmp` is **768 KiB**; root is **86 GiB free**.


### PROD-HEALTH-224 Recheck live dirty state and mount attribution limit — 2026-11-03 12:06 UTC

- At **12:06:23 UTC**, `/usr/bin/dux status --json` reported the service live but `dirty=true`, `caught_up=false`, `scan_in_progress=false`, with **12,034 pending updates**, **688.3 seconds behind**, **102,920 events seen / 77,685 resolved / 1,152 committed / 0 userspace drops**, and no write error. The dirty reason is `non-watched filesystem mounts changed; reconciling visible paths`; the marker dates to **12:05:04**. PID **2558658** remains active with zero systemd restarts.
- A read-only inspection of the live v5 database found only `dirty_reason`, `dirty_since`, last scan/root, schema version, and throttle metadata; it has no prior mount-topology fingerprint. Current mountinfo has **4** Docker overlay mounts plus ephemeral `nsfs` and systemd credential mounts. This cannot establish which topology entry changed or prove overlay churn caused the dirty marker. Current worktree source excludes duplicate overlay views for visible-tree reconciliation and has a focused unit test; that behavior is not present in the source-unmatched installed executable unless independently demonstrated.
- Root has **87 GiB free**; after stale temporary-log removal, `/tmp` is **692 KiB** and `/var/tmp` **160 KiB**. Disk capacity and temp scratch are not the cause of the live index backlog. No restart, database write, mount change, or container change was made.
- Production exactness remains unverified. The immediate qualification requirement is a source-matched candidate on an isolated host, plus runtime instrumentation that records mount identity deltas; do not infer clean/caught-up status from the successful earlier baseline scan.

### Live recovery regression — 2026-11-03 12:42 UTC

The controlled restart and clean full scan recorded in PROD-HEALTH-238 did not
restore a qualified live index. At 12:40:53 UTC the installed daemon marked the
index dirty again because non-watched mount topology changed. Three status
samples between 12:41:50 and 12:42:20 remained dirty and not caught up, with
6,535 pending paths and no change in the event counters; lag increased from
321.4 to 351.4 seconds. Dux was using 99.4% of one CPU and CPU PSI some avg10
was 16.85%. No exact observer or build was run in this condition. The mount
change cannot be identified from the current mount snapshot because the daemon
does not retain or log its previous topology fingerprint. The running 0.6.3
binary is source-unmatched, so the current worktree's overlay-churn fix is not
evidence that the live daemon contains it. B01, B03, B04, and B07 remain open;
the baseline scan's zero traversal errors do not establish replay exactness.

At approximately 12:43 UTC, status still showed no scan and `caught_up=false`;
pending had increased to 11,481 and lag to 416.5 seconds. Event counters had
resumed advancing, but pending rose by 4,946 over about 56 seconds. Systemd
reported reconciliation deferred for normal host pressure while sampled CPU
and I/O PSI avg10 were 0.01% and 0.09%; this status string does not establish
the daemon's actual admission decision. Root had 86 GiB free and 21 GiB
available RAM. Storage capacity is not the present blocker; exact recovery,
source provenance, and isolated qualification remain open.

### Current-source Rust unit run — 2026-11-03 12:52 UTC

`cargo test --locked --all-features --jobs 1` passed **256/256 tests** with
Rust/Cargo 1.99.1. It ran in a transient systemd scope capped at 20% CPU and
2 GiB memory, with swap disabled, low CPU/I/O weight, nice 19, and idle I/O
priority. Compilation took 4m44s; tests took 98.24s. This verifies the current
worktree's debug unit suite only; it is not a release build or a production
qualification run.

The installed Dux service remained active and unchanged. At 12:51:58 UTC it
was still dirty and not caught up, with no scan, 28,171 pending paths, and
928.99 seconds of lag. The backlog grew during the test window, but this
coincidence does not prove the test caused it. The run left a 650 MiB
`target/debug` tree; its 44 small PID-scoped test files were removed after the
test process exited. No further build or test should run on this host while
the live backlog is growing. Continue release qualification on an isolated
guest or VPS.

### Live core-dump hardening — 2026-11-03 12:56 UTC

The running service previously had `LimitCORE=infinity`; its soft limit was
zero but its hard limit was unlimited, so the root daemon could raise the soft
limit. I set both live limits to zero with `prlimit` and added
`/etc/systemd/system/dux.service.d/80-core-dump.conf` with `LimitCORE=0` for
future starts. `systemctl daemon-reload` and `systemd-analyze verify dux.service`
passed. Dux remained PID 2731095 with no restart; its capability sets were
unchanged. This closes only the core-dump mismatch. The installed unit still
differs from the current package source in other hardening settings, and
source-matched release and live-index qualification remain open.

The effective unit also had `SecureBits=0` despite the current package policy
requiring `noroot noroot-locked`. Added
`/etc/systemd/system/dux.service.d/81-securebits.conf` with that tested policy;
`systemctl daemon-reload` and `systemd-analyze verify dux.service` passed.
`systemctl show` reports `SecureBits=3` with the existing capability sets. The
running PID was not restarted, so securebits apply only at the next start;
the current process capabilities remain unchanged. The index remains dirty
and unqualified.

### Mount-churn correlation — 2026-11-03 12:59 UTC

The dirty marker at 12:40:53 UTC was preceded six seconds earlier by four
Docker/containerd dead-shim cleanup sequences. Because Dux checks mount
topology every five seconds, this timing strongly correlates the dirty
transition with container overlay lifecycle. Exact changed mount identities
were not retained, so this is not proven causation. The current worktree
filters duplicate overlay-view churn and its focused regression is included in
the 256 passing Rust tests; the installed source-unmatched daemon has not been
shown to contain that fix. Current Dux still has a growing backlog, so the
runtime remains unqualified pending a source-matched candidate and exact
recovery evidence.

### Transferable source checkpoint — 2026-11-03 13:00 UTC

Refreshed the source-only handoff at
`target/readiness/current-source-handoff-20261103T1259Z/`: 126 manifest-bound
entries, source-tree SHA-256
`cc9fca935d3353bd708ad854985cb5761d00588b95d7478833dbae1aaceaf687`, and
verified deterministic archive SHA-256
`5aae5b8fb59065f1b107c53790a6991ca3401e41d2098463ca7177088c2bd9f2`.
Manifest, member hashes, and `SHA256SUMS` verify. Gitleaks scanned the exact
2.57 MB extraction with zero findings. The handoff contains no binary or
package and explicitly remains unqualified; it is ready for the isolated
builder once access is available.


### ARTIFACT-225 Verify source handoff generator after refresh — 2026-11-03 12:08 UTC

- `PYTHONPATH=scripts python3 -m unittest test_dux_source_archive -v` passed **4/4** in **1.117 seconds**. The cases cover deterministic archive metadata, rejecting source mutation between inventory and packaging, path/content-bound digests, and rejecting symlink escapes. A first invocation without `PYTHONPATH=scripts` failed during import; it did not execute tests. The refreshed handoff at `target/readiness/current-source-handoff-20261103T1208Z/` separately verified all **126** archived members against path, size, and SHA-256 metadata, and `sha256sum -c` passed.
- This is source-archive integrity coverage only; it provides no evidence about Rust correctness or a production candidate. At **12:07:17 UTC**, live Dux remained dirty/not caught up, with **11,871 pending updates**, **742.6 seconds behind**, no scan in progress, and no write error. No service or database change was made.


### PROD-QUAL-226 Full Python qualification harness under production-host resource cap — 2026-11-03 12:09 UTC

- Full discovery `python3 -m unittest discover -s scripts -p 'test_*.py' -v` passed **261/261 tests** in **85.400 seconds**, under a transient systemd scope capped at **5% CPU**, **1 GiB memory**, low CPU/I/O weight, `nice 19`, and idle I/O priority. Coverage includes release builder rejection/rollback, backup integrity and cleanup, package/security policy, live-test schemas, qualification safety, storage preflight, source archives, and verifier checks. This Python suite does not run Rust code, build packages, or exercise the installed daemon's event behavior. Static `cargo fmt --all -- --check` and `git diff --check` also pass.
- After the test process exited, a separate XGen Go race-test process briefly owned `/tmp/go-build2444661393` (**26 MiB**) and a test fixture. Its PID was checked; the process then exited and Go removed its scratch. Final `/tmp` is **692 KiB**, `/var/tmp` **160 KiB**. Existing tiny systemd-private and Codex runtime files remain active runtime state.
- At **12:09:47 UTC**, Dux PID **2558658** remained active with zero systemd restarts, but `dirty=true`, `caught_up=false`, **12,394 pending updates**, **890.3 seconds behind**, no reconciliation scan, and no write error. It reported **115,789 events seen / 82,860 resolved / 2,432 committed / 0 userspace drops**; status format v3 still omits the kernel-overflow count. Root has **87 GiB free**. No Dux restart or database modification occurred during the test.


### SECURITY-227 Refresh RustSec dependency audit and disk-pressure evidence — 2026-11-03 12:15 UTC

- `cargo audit --deny warnings --file Cargo.lock` passed with exit **0** against RustSec advisory DB commit `6c5fedf8ff03c485ed674d8f941e0180f3f72d70` (commit time **2026-11-03 09:44:19 UTC**). It loaded **1,403** advisories and scanned **229** locked dependencies with no vulnerability or warning reported. The current `Cargo.lock` SHA-256 matches the previously verified source handoff, so this audit did not modify its contents. This is dependency advisory coverage; it does not replace source review, binary scanning, or runtime qualification.
- At **12:14:49 UTC**, root had **86 GiB free**. `/var` used **32 GiB**, chiefly live service data: PostgreSQL **14 GiB**, containerd **6.0 GiB**, Docker **4.8 GiB**, XGenStack **2.7 GiB**, Dux index/backups about **2.0 GiB**; `/var/log` used **1.6 GiB** including **531 MiB** of journals. Dux backups and database data were preserved.
- `/tmp` was **1.5 GiB**, attributable to active XGen `scripts/test-fast.sh` PID **2586523** and child `go test` PID **2608766**, writing `/tmp/x.JbN6Y`. The same process remained live at 12:14:49; its data was not deleted. Three old Nginx worker handles reference already-deleted logs under a different `/tmp/x.*` tree; their small open sizes do not explain the current large scratch.
- Dux PID **2558658** remained active with zero systemd restarts, but dirty/not caught up and no scan: **29,606 pending**, **1,192.3 seconds behind**, **170,168 seen / 108,855 resolved / 3,328 committed / 0 userspace drops**, no write error. Its v3 status still has no kernel-overflow counter. CPU PSI avg10/60/300 was **0.42% / 8.03% / 13.41%**. The Go test and backlog were concurrent; causation is not established.


### Live recovery replay remains stalled — 2026-11-03 13:13 UTC

The controlled restart at 13:04:57 UTC completed gracefully. The new daemon
started with core dumps disabled and `SecureBits=3`, then completed a full
root scan at 13:10:18: 1,634,910 files, 145,480 directories, 99.66 GiB, and
zero scan errors. The live status did not report caught-up after the scan.

From 13:10:33 to 13:13:00, the status remained at 66,094 pending updates,
163,360 seen, 162,922 resolved, and zero committed updates; behind time grew
132 seconds. The daemon consumed approximately one CPU core and its database
and WAL mtimes remained at 13:10:14. A three-second syscall sample counted
about 50,000 `pread64` calls. A low-frequency profile could not resolve code
symbols: the stripped installed binary has Build ID
`2e2d4557ac578ce614c1b48cfb6b53a389b27330`, with no matching debug artifact
in retained releases. This proves sustained read activity and no observed
queue progress, but does not identify the responsible function.

The live database remains unqualified. Next evidence required: identify the
exact source/build behind the installed binary, obtain a symbolized profile or
equivalent hotspot diagnosis, observe durable queue progress and drain, then
run an independent exact comparison on an isolated qualification host. The
host currently has 78 GiB free; `/tmp` is 668 KiB and `/var/tmp` is 160 KiB
after removing about 835 MiB of unused build caches and scan reports. Active
systemd-private and Codex runtime directories were preserved.

### Current readiness checkpoint — 2026-11-03 14:44 UTC

The release-gate count remains **14 active blockers (B01–B14) and one resolved maintenance item (B15)**. These are qualification blockers, not a count of confirmed source defects.

- **Live production remains unqualified (B01–B04):** installed package 0.6.3-16/R16 is still source-unmatched. At 14:43:12, PID 2777300 was active with zero restarts; status was dirty/not caught up, **139,142 pending**, **5,678 seconds behind**, no active scan, and no write error. A 30-second read-only sample measured **99.3% of one CPU core** while host CPU PSI remained low (avg10 **1.63%** at the final sample); pending rose **960**. The v1 status reports zero userspace drops but does not expose the kernel-overflow count. Do not equate this field with proof of no loss.
- **New candidate evidence narrows but does not close migration/runtime coverage:** release-63 completed one bounded tmpfs watcher burst with exact final-tree comparison and a clean SQLite integrity check (PROD-TEST-273). A disposable copy of the verified **2.1-million-inode / 2.7-million-growth-row** v5 snapshot completed atomic conversion to v6 while scanning a 200-file fixture and preserving legacy growth rows (PROD-MIGRATION-276). Neither test touched the production index. The migration rehearsal does not measure a full `/` scan, ext4/XFS production load, migration interruption/rollback, or event catch-up after migration.
- **Source test coverage:** the current release candidate has 256/256 all-feature Rust tests, 261/261 Python harness tests, Clippy with warnings denied, formatting checks, dependency audit, artifact hash checks, and a single bounded live watcher burst. This is unit/harness and limited smoke coverage; it does not satisfy the exact-binary overflow/OOM/WAL/capability matrix (B09), same-binary 100k/1m/10m budgets (B10), installed package upgrade/rollback on supported distributions (B11), 72-hour endurance (B13), or 24-hour monitored canary (B14).
- **SQLite WAL-reset remediation (DB-01):** the current source-bound static candidate embeds SQLite **3.54.1** and its runtime regression test passed, above the official fixed-version floor of 3.51.3. The installed static R16 binary still contains SQLite **3.46.0**, in the affected range. The candidate fix therefore remains undeployed; package, migration, rollback, and rollout qualification still gate production mitigation. The upstream issue only establishes version exposure, not that corruption occurred on this host.
- **Qualification environment remains unavailable:** `/etc/dux-qualification-sandbox` is absent and there is no running local qualification VM. The guarded ext4/XFS fault and package-lifecycle matrix must run on a dedicated disposable host. Current release artifacts are not installed on production, and no replacement or schema migration should be inferred safe from the bounded tests above.
- **Disk and temporary storage at the last check:** root filesystem was **98 GiB used / 79 GiB free**; `/tmp` **692 KiB**, `/var/tmp` **160 KiB**, and `/dev/shm` **1.1 MiB** after the external Go test exited. Root free space is adequate for current housekeeping but does not qualify the full-root migration's peak workspace requirement.

The two obsolete private SSH keys from archived, stopped Rocky qualification guests were removed from ignored artifacts on Nov. 3; the exact staged Git diff for the source push contained no Gitleaks findings. The remaining blockers and all production service/index changes are unchanged.

### Current readiness checkpoint — 2026-11-03 15:15 UTC

The release-gate count remains **14 active blockers (B01–B14) and one resolved
maintenance item (B15)**. No production release is qualified.

- **Production remains unsafe to promote (B01–B04):** installed package
  0.6.3-16/R16, PID **2777300**, is active with zero restarts. At 15:15:53 UTC,
  status was dirty/not caught up with **166,658 pending**, **1,432,752 seen /
  470,904 resolved / 30,208 committed**, and **7,638 seconds behind**; no scan
  was active and `write_error=null`. Relative to 15:07:27, pending rose by
  **8,289** and lag by **505 seconds** while 512 updates were committed. The
  v1 status still does not report kernel-overflow counts. A process sample at
  15:15:53 measured Dux at approximately **98% of one CPU** with no scan
  active. The source-unmatched installed binary prevents assigning that
  behavior to the candidate source.
- **Current candidate evidence:** release-63 executable SHA-256
  `109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e` passed
  the Ubuntu 24.04 guest `capability`, `wal`, and `mount` integration suites
  (PROD-TEST-284). Its current-hash overflow suite remains unrun. An attempted
  guest startup for overflow was stopped before SSH and before any product
  suite by the host-pressure guard at CPU PSI **69.20%** and load1 **13.79**;
  contemporaneous host checks showed unrelated Go test work as well as the
  saturated production daemon. This guard event is not a Dux product failure
  or a qualification result (PROD-TEST-286). A second attempt after the Go
  race job exited was stopped before SSH at CPU PSI **63.59%** and load1
  **15.14** (PROD-TEST-287); a separate `go vet ./...` build was active soon
  afterward. A third attempt did boot the verified guest, create and scan a
  100,000-file ext4 fixture, and start the overflow suite. The durable report
  remained `status=running` at suite start when the VM guard stopped QEMU at
  load1 **14.47** (PROD-TEST-289). No durable overflow/recovery outcome was
  written, so this run is **interrupted**, not a pass or product failure.
- **Historical P0 evidence is hash-bound to an older binary:** the latest
  full P0 composite is R21, candidate SHA
  `0e61da726b784b9b7d75299bb864758284097106d7741c370c3c360cb8b4e3ab`, source
  tree SHA `46e7dfa310eb517a415f8af60c524e428f4f74f710e9a69d214bc3a4661167de`.
  All 14 listed suites passed across two separately executed reports on
  Ubuntu 24.04/ext4 with a 100,000-file seed, 2,048 MiB fixture, 50% worker
  CPU quota, and 4 GiB worker limit; the composite explicitly is not one
  consolidated invocation.
  The run does not include 72-hour soak, full scale budgets, package lifecycle
  and security, production drift convergence, or canary. It cannot qualify
  release-63 SHA `109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`
  or close B07, B09, or B10. Current Rust unit, Python harness, migration,
  and bounded watcher evidence do not replace the missing exact-release
  crash/overflow/pressure/scale matrix, package rollback, soak, or canary.
- **Disk and temporary work:** root has **73 GiB available (60% used)**;
  `/tmp` is **5.0 GiB**, occupied by active Go test/build scratch, and must be
  retained until those jobs exit. `/var/tmp` is **160 KiB** and `/dev/shm` is
  **1.1 MiB**. The temporary VM image, identity, and overlay were removed
  after the guard stopped the guest. The 6.2 GiB readiness evidence tree
  remains retained. The latest audit changes are pushed on
  `hardening/audit-remediation`; no live package, service, or database
  mutation was made.

Retry current-release overflow qualification only after external host
workloads exit and pressure stays low for a sustained interval; multiple
short low-load windows were followed by large Go test/build bursts. Preserve
the current binary hash, exact-tree verification, and recovery telemetry in
the report; use the offered dedicated VPS once its SSH route is available.

### Local KVM retry and live-state refresh — 2026-11-03 19:26 UTC

The current-source release-63 executable remains SHA-256
`109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`. A
refreshed comparison at 19:37 UTC, recorded in
`target/readiness/source-drift-r63-20261103T1935Z.json` (SHA-256
`7765ce151c66e49d8dd28b30c3d227eb6135fa64b6157efbdd6ce0de993195ea`), confirms
the full source tree differs: candidate/current tree hashes are
`d952c8d46a7b57234c46a4af24c07698c3ac07512732ce56fa520634957dd4d6` /
`f18c2774b7bf85e872e5c0b12be2c640ee6e0339e4f4eafa776396d8705d3593`, with
126/125 files. The only non-documentation differences are the intentionally
removed `.github/workflows/ci.yml` and the readiness workflow edit; no runtime
code or packaging input differs. This establishes input equivalence, not an
exact-tree build or runtime qualification. Canonical's Ubuntu 24.04
cloud-image checksum was verified before boot. A disposable standard-profile
guest completed cloud-init but ran no Dux
suite; it was stopped cleanly to select the 4 GiB qualification profile. That
profile was then stopped by the VM pressure guard before SSH became available,
at load1 **14.08** and CPU PSI some avg10 **72.52%** (PROD-TEST-290). This is
an infrastructure interruption, not a candidate pass or product failure.
At 19:26 UTC host load1 was **30.06** and CPU PSI some avg10 **54.86%**; the
local KVM fault suite remains inadmissible until host pressure clears.

The live R16 daemon is still active with zero observed service restarts, but
the index is untrustworthy (B01/B02). At 19:26:19 UTC, CLI JSON reported
**492,740 pending** against a configured **500,000** threshold,
**6,023,862 seen / 1,254,106 resolved / 57,984 committed**, **22,664 seconds
behind**, `dirty=true`, `caught_up=false`, no scan in progress, and
`write_error=null`. The process-validated v3 `/run/dux/watch.status` sample
also reports kernel-empty=false, uncertain=true, and zero userspace drops;
the CLI's zero `events_dropped` field does not count kernel queue overflows.
The raw v3 sidecar record was `3 1793733977 2777300 492740 0 6023862
1254106 57984 0 22664073 500000 1 1793733934`.
The last event commit was at 19:25:34 UTC, while the status was refreshed at
19:26:17 UTC. The journal contains **123 fanotify queue overflow warnings
since midnight UTC**, latest at **19:20:36 UTC**. The full-root scan timestamp
is 13:08:33 UTC; no scan-progress file is active. Therefore neither the
backlog nor the indexed tree can be treated as exact, even though the latest
status call reports no write error.

Disk remains adequate for the local qualification artifacts: root has **77
GiB free (57% used)**. The verified cloud image is retained under `/dev/shm`
for retry; all stopped guest overlays were removed from tmpfs. No production
service, package, database, or index mutation occurred, and the Git worktree
remains clean apart from this checkpoint.

### Production event-loss refresh — 2026-11-03 19:31 UTC

The R16 watch queue crossed a second important boundary after the 19:26
sample. At 19:31:04 UTC, the v3 sidecar recorded **6,105,283 events seen**,
**1,279,591 resolved**, **58,368 committed**, **500,180 userspace updates
dropped**, and **8,971 pending**. It remained kernel-nonempty and uncertain;
the CLI reported `dirty=true`, `caught_up=false`, no scan in progress, and
`write_error=null`. Its behind counter was **22,950.755 seconds** and the
last event commit was 19:30:49 UTC. The pending decrease from 492,740 is not
recovery: the userspace drop counter rose from zero to 500,180, and the
journal recorded a new kernel fanotify overflow at **19:30:54 UTC**, bringing
the day's overflow-warning count to **124**. R16 remains source-unmatched, so
these observations establish production impact but do not identify a current
source-level cause. The journal also records **90** full-reconciliation
deferrals since midnight, latest at **19:27:10 UTC**, explaining why no
full-root scan had started in those logged intervals; the installed source
does not establish whether the current candidate has identical scheduling.

The 4 GiB local qualification guest was stopped by its pressure guard at
19:30:43 UTC with load1 **13.27**, CPU PSI some avg10 **42.68%**, and I/O PSI
some avg10 **8.53%**. It did not reach SSH or run a Dux suite. Its stopped
overlay was removed from tmpfs; the verified base image remains available
for another guarded attempt.

At 19:34:04 UTC, the v3 sidecar still showed `kernel_empty=false` and
`uncertain=true`, with **8,889 pending**, **500,180 userspace drops**, and
**23,129.170 seconds behind**; no scan was active and `write_error=null`.
Since the 19:31 sample, pending decreased by only 82 while seen events rose
by 19,089 and the behind counter grew by 178 seconds. The userspace-drop
counter did not clear. A further 4 GiB VM attempt was stopped before SSH at
19:33:51 UTC when load1 reached **10.87** and CPU PSI some avg10 **44.65%**;
the stopped overlay was removed. The host remains outside the VM controller's
safe sustained-pressure window.

### R63 overflow run interrupted by host guard — 2026-11-03 19:45 UTC

The exact R63 candidate was finally exercised in the local 4 GiB Ubuntu 24.04
guest. The 100,000-file ext4 fixture and baseline scan completed; the
`overflow` suite entered `running` at **19:44:56 UTC**. At **19:45:23 UTC**,
the host guard stopped QEMU at load1 **13.63** and CPU PSI some avg10
**60.96%**. The SSH session closed; the last fetched report still showed
`status=running`, `current_suite=overflow`, with no terminal evidence. The
overflow suite ran for about 27 seconds and is **interrupted**, not passed or
failed. Post-stop host pressure reached load1 **21.89** and CPU PSI some
avg10 **64.12%**.

The attempt is bound to candidate SHA-256
`109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e` and
harness SHA-256 `c5db5687326e893f87f1c30c396817735c948b96c60e4ccc28adcf23f90e5970`;
both 4 GiB guest and 4 GiB worker used a 25% CPU quota. The final fetched
report snapshot is SHA-256
`af646afd4131044962680069bc30e0a8911b2de3ce7ed774e3e848093629af18`. The
guard/interruption descriptor is
`target/readiness/qualification-r63-overflow-20261103T1942Z-interrupted.json`
(SHA-256 `0f1ee68b5538fea53ba7304dac476d401921af4deefb4f43d02dc468727d0fc8`).
The stopped overlay was removed from tmpfs; the verified base image remains
for a later guarded retry. No production index or service was touched.

### Post-run production status — 2026-11-03 19:46 UTC

The production service remains active as PID **2777300** with zero restarts.
At 19:46:20 UTC, its v3 watch sidecar reported **20,517 pending**,
**6,306,253 seen / 1,311,294 resolved / 60,544 committed**,
**500,180 userspace updates dropped**, kernel-empty=false, uncertain=true, and
**23,866.642 seconds behind**. The CLI remained `dirty=true`, `caught_up=false`,
with no scan in progress and `write_error=null`; the last event commit was
19:45:16 UTC. Since the 19:39 sample, pending increased by 3,940 and the
behind counter by about 416 seconds while the drop counter stayed latched.
The journal now contains **126** fanotify overflow warnings, latest at
**19:41:35 UTC**, and **92** pressure deferrals, latest at **19:42:07 UTC**.
The index remains unqualified. Root has **77 GiB free (57% used)**; the VM is
inactive and its stopped overlay has been removed.

### Production CPU saturation and executable identity — 2026-11-03 19:54 UTC

Read-only sampling found the live daemon consuming **7.44 CPU seconds during
an 8-second interval (93% of one CPU)**. It is a single-threaded process in
`dux.service`; systemd reports `CPUWeight=10`, `CPUQuotaPerSecUSec=1s`
(one CPU), and `MemoryMax=4 GiB`. The host's other large Go test had exited,
but a separate Go package compile started and host CPU PSI some avg10 rose to
**38.39%** with load1 **12.71**. The VM admission/pressure guard therefore
continues to prohibit another qualification run.

At the 19:54 status sample, the index had **39,572 pending** updates,
**6,510,687 seen / 1,346,138 resolved / 61,184 committed**, **500,180 dropped**,
and was **24,373.562 seconds behind**. It remained `dirty=true`,
`caught_up=false`, with no scan and no reported write error. Relative to
19:46, pending grew by **19,055** and seen events by **204,434**, while commits
grew by only **640**; the drop counter remained nonzero. The latest journal
sample included fanotify overflow and deferred reconciliation warnings.
This is an observed production CPU/catch-up failure, not an inference from
the interrupted qualification VM.

Executable provenance is still not exact: `/usr/bin/dux` reports
`0.6.3 (a42739776bbd+modified) [production]`, but its SHA-256 is
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, which
does not match the retained R63 candidate's
`109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`. The
version string alone cannot establish the installed binary's source identity.
No daemon, package, database, index, or quota was changed. Production remains
unqualified; the exact-binary overflow recovery, scale, migration/rollback,
fault, endurance, and canary gates remain open.

### Bounded production no-progress sample — 2026-11-03 19:56 UTC

An 8-second read-only sample at 19:56:31 UTC measured **8.00 CPU seconds in
8.00 wall seconds** for PID 2777300. Between the immediately-before and
immediately-after status reads, pending stayed at **39,861**, events stayed at
**6,562,841 seen / 1,348,558 resolved / 61,440 committed**, and the last commit
timestamp stayed at **19:56:27 UTC**; this interval showed no counter progress.
The daemon remained dirty, not caught up, with no scan and no write error. This
bounded sample confirms CPU saturation without claiming that the pipeline
never advances.

The Go race-test process exited during the subsequent 20-second host poll, but
`staticcheck -checks=SA* ./...` was then active. At 19:57:23 UTC host load1 was
**10.23**, above the VM controller's **10.0** admission limit, and CPU PSI
some avg10 was **15.94%**. The R63 VM retry therefore remains inadmissible;
the exact test has not been restarted.

At 19:58:31 UTC, the load window worsened to **32.34** and CPU PSI some
avg10 to **76.55%** while Go security analysis and compiler jobs were active
from `/home/xgenstack.com/backend`; the Dux daemon was also present at its
one-CPU quota. Root remained **58% used with 75 GiB available** and `/dev/shm`
was **6% used**, so the current qualification blocker is host compute
pressure, not disk capacity. The guest was not restarted under these conditions.

### Production backlog and local admission recheck — 2026-11-03 20:00 UTC

At 20:00:43 UTC the live index remained `dirty=true`, `caught_up=false`, with
**44,671 pending**, **6,611,463 seen / 1,358,174 resolved / 61,696 committed**,
**500,180 drops**, and **24,716.557 seconds behind**. No full scan was active
and `write_error=null`. Compared with the 19:56:31 sample, pending grew by
**4,810** and seen events by **48,622**, while commits grew by only **256**;
this is continued backlog growth, not recovery. The last scan timestamp is
still **13:08:33 UTC**.

The VM remained inactive. Host load1 was **17.68**, CPU PSI some avg10
**32.08%**, I/O PSI some avg10 **7.75%**, and I/O PSI full avg10 **5.07%**;
the local admission limit still rejects load above **10**. A Go test was active
in the shared host. Root had **74 GiB free (59% used)** and `/dev/shm` **6% used**.

### R63 VM boot interrupted by a host-pressure surge — 2026-11-03 20:02 UTC

After load briefly fell to **7.33** and CPU PSI some avg10 to **8.91%**, the
guarded 4 GiB Ubuntu 24.04 VM was admitted at **20:02:13 UTC** with QEMU capped
at 25%. Host pressure rose during boot; at **20:02:57 UTC** the controller
stopped it at load1 **14.89** and CPU PSI some avg10 **53.01%**. The guest had
used **10.689 CPU seconds** and peaked at **547.1 MiB**. Two SSH attempts timed
out during banner exchange; cloud-init never became accessible, the candidate
was not copied, and no suite started. This is a boot interruption, not an
overflow result or candidate failure.

The VM unit and QEMU process are confirmed inactive. Its ephemeral overlay,
seed, and keys were removed; the verified base image remains with SHA-256
`04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94`. Boot
console evidence is retained at
`target/readiness/qualification-vm-boot-20261103T2002Z/console.log` (SHA-256
`ad44f4ea2cbefd990c749869883e1dca0114262dc500b4906e9c47c412dcd4ef`), with
the interruption descriptor alongside it. Future retries need a sustained
low-pressure interval; a single passing admission sample did not predict a
safe boot window.

### R63 overflow attempt stopped by fixture headroom — 2026-11-03 21:55 UTC

The next guarded attempt booted successfully, verified the exact candidate and
harness hashes, created the **100,000-file** baseline, and completed its
baseline scan. The overflow suite then failed in its precondition
`daemon.exact()` before it injected any overflow. Candidate logs show the
fixture daemon resumed a pre-existing index, started reconciliation, and
cancelled scans under its low-disk guard; one status sample reported only
**271,314,944 available bytes** on the watched fixture filesystem. The 768 MiB
image left too little headroom for the concurrent reconciliation checks, so
the pipeline remained uncertain and the harness timed out. This is a terminal
harness/setup failure, **not an overflow recovery result or a product failure**.

The terminal report is retained at
`target/readiness/qualification-r63-overflow-20261103T2150Z/terminal-report.json`
(SHA-256 `06c1f9a5d8955f56bced8769e7b0b58e58d9c46be9fe99ba945e696baf640f91`);
its classification descriptor is alongside it (SHA-256
`f434e49f35792dc7ba568d23bd329b39331526e3cc0636c8edee3ed4f7589e05`). The
QEMU console log is also preserved. After the harness exited, the outer host
guard stopped the guest at **21:57:12 UTC** when host load1 reached **14.49**
and CPU PSI some avg10 **60.47%**. The inactive guest's ephemeral overlay was
cleaned and the verified base image remains intact.

To avoid repeating this setup failure, the readiness harness now checks for
at least **512 MiB** free on the watched filesystem before starting the
overflow daemon; its targeted boundary regression passed. The next attempt
will use a **2 GiB ext4 fixture** and `/var/tmp` for the worker workspace, with
the same candidate, 100,000-file baseline and 25% quotas. The full overflow
suite remains unpassed.

### Corrected R63 retry deferred during host Go builds — 2026-11-03 22:05 UTC

After waiting through a short low-load interval, the VM controller admitted a
fresh 4 GiB guest at **22:03:46 UTC**. Go compilation/test work started during
cloud-init. At **22:05:42 UTC**, the guard stopped QEMU at load1 **13.48**, CPU
PSI some avg10 **48.75%**, and memory PSI some avg10 **1.13%**. SSH never became
ready; the revised harness and candidate were not uploaded, so no suite ran.
The evidence is retained at
`target/readiness/qualification-vm-boot-20261103T2203Z/` (console SHA-256
`88e29ecf9d421c11bd18140ec32d6173a4bf35b492292bb4f9350a8784825a9d`, descriptor
SHA-256 `c80e126c096285700eae5be25c52f2461e6c2c249ac11b0b3696a0456e227f0d`).
The guest is inactive, its tmpfs overlay was cleaned, and the verified base
image remains unchanged.


### Production index remains dirty under event backlog — 2026-11-03 22:10 UTC

A refreshed read-only production snapshot at 22:10:09 UTC shows the daemon
active with no systemd restarts, but the root index remains dirty and not caught
up. The JSON status reported **240,830 pending updates** out of a **500,000**
cap, **8,698,084 events seen / 1,771,529 resolved**, **77,952 updates
committed**, **500,180 events dropped**, **32,528 seconds behind**, no active
scan, and `write_error=null`. The 22:10:09 snapshot had 233,757 pending
and 77,824 committed; these samples show the backlog growing while only 128
additional updates committed. `dirty_reason` is **subtree reconciliation budget
exhausted**. Recent journal entries include recurring fanotify queue overflows
and reconciliation deferrals under host pressure, plus a bounded subtree walk
exhaustion under `node_modules/simple-icons/icons`.

At the same time, host load1 was **16.15** (then **13.15** on a follow-up),
CPU PSI some avg10 fell from **2.47%** to **0.89%**, root had **73 GiB free**
(59% used), and `/dev/shm` had about **12.4 GiB free**. A shared-host Go test
and linker were active. The qualification VM remained inactive; starting it
would exceed the local controller's load1 admission limit (**10**). Disk is
not the qualification blocker. Do not treat this production event as evidence
against the current worktree: installed `/usr/bin/dux` is 0.6.3 from source
revision `a42739776bbd+modified` with SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, which
does not match the current R63 candidate/source. Production recovery and CPU
behavior therefore remain unresolved and unqualified; no service, index, or
package mutation was made.

### R63 guest boot guarded during host saturation — 2026-11-03 22:17 UTC

A 4 GiB Ubuntu 24.04 guest with QEMU capped at 25% started at **22:13:11**.
The guest reached network setup, but its SSH probe at **22:14:15** was reset
while cloud-init was still running. The pressure guard stopped QEMU at **22:14:22**
when host load1 was **15.10** and CPU PSI some avg10 was **63.19%**; QEMU
accounted **17.091 CPU-seconds**, peaked at **666 MiB**, and used no swap. At
22:14:35, load1 had risen to **20.88** and CPU PSI some avg10 to **68.82%**.
The candidate was not copied and no Dux suite ran. This is a guarded boot
interruption, not a product result. The console, guard event, and descriptor
are retained under `target/readiness/qualification-vm-boot-20261103T2213Z/`;
SHA-256 values are respectively `df59717870e80e67879917741ed55351733f9962ace00076205495ab83e8dbe3`,
`3af7e5dc25af077a5a05d6e9732c17b8bf1c24766c96b01b94536185580fc7a8`, and
`19fe399715899adfa31f8a92af783b351b1dd08e362d13aed9c50c619a3f8c7b`.
The stopped VM's writable overlay was removed from tmpfs after preserving the
console and pressure record.

This attempt exposed a harness cleanup defect: `systemd-run --collect` removed
the stopped transient unit before the cleanup command called `systemctl stop`,
which made cleanup abort with “Unit not loaded” and leave the ephemeral overlay
behind. The VM controller now accepts that case only when `systemctl show`
confirms both `LoadState=not-found` and `ActiveState=inactive`; other stop
failures still raise. Regression tests cover both the collected/inactive case
and an active unit. The full VM-controller module passed **29/29** tests, and
`py_compile` plus `git diff --check` passed. These are harness results only;
the R63 overflow suite remains unrun.

### Overnight production backlog and repeated kernel overflows — 2026-11-04 07:22 UTC

A read-only refresh at **07:21:42 UTC** shows installed Dux 0.6.3
(`a42739776bbd+modified`, SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`) still
active with zero systemd restarts and an effective **100% of one CPU** quota.
The installed hash differs from the current R63 candidate
(`109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`), so
runtime symptoms cannot be attributed to the candidate source.

The live root index is dirty and not caught up. Status reported **478,273
pending updates** (500,000 cap), **18,791,041 events seen / 3,506,398
resolved**, **155,008 committed updates**, **1,001,175 userspace updates
dropped**, and **65,588 seconds behind**. No scan was active,
`write_error=null`, and the dirty reason was **non-watched filesystem mounts
changed; reconciling visible paths**. Compared with 22:19:52 UTC on Nov. 3,
pending grew from **247,018** to **478,273**, drops from **500,180** to
**1,001,175**, and lag from **33,077** to **65,588 seconds**. Journal review
counted **85 fanotify queue-overflow notices** and **59 full-reconciliation
deferrals** since 22:17 on Nov. 3, plus one logged in-memory backlog discard
at **02:03:11 UTC**. The latest overflow was at **07:19:16 UTC** and the latest
deferral at **07:23:42 UTC**.

Host root had **85 GiB free (53% used)** and `/dev/shm` about **12.4 GiB free**;
Dux used about **441 MiB** of cgroup memory. At the sample, load1 was **2.45**
and CPU PSI some avg10 **1.81%**, but a shared-host Go test/compile was active.
No qualification VM was started during that build. This confirms adequate disk
capacity and severe production convergence failure; it does not identify the
cause in the source-mismatched installed executable. No production service,
package, configuration, or database mutation was made.

### R63 overflow suite interrupted after fixture and suite startup — 2026-11-04 07:35 UTC

The 07:25 guarded Ubuntu 24.04 attempt used the exact release-63 candidate
(SHA-256 `109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`)
and readiness harness
(SHA-256 `3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`).
An initial invocation with `/var/tmp` failed the harness preflight because that
path is on the guest root filesystem; no suite ran in that invocation. The
corrected invocation remounted guest `/dev/shm` to **3 GiB** and used it as the
required tmpfs workspace for a **2,048 MiB ext4 fixture**.

The corrected run created the **100,000-file** seed and completed its baseline
scan. The durable report says the overflow suite started at **07:31:55 UTC**
with `status=running`. At **07:32:29**, the pressure guard stopped QEMU after
host load1 reached **13.91** and CPU PSI some avg10 reached **69.54%**. Systemd
reported **60.385 CPU-seconds**, **1.9 GiB peak memory**, and zero swap. The
suite report had not reached a terminal checkpoint; this attempt is
**interrupted**, with no overflow-recovery pass or product failure established.

The report was recovered read-only from the stopped guest overlay before
cleanup. Report SHA-256 is
`0ffc390a7207f5411402da9a25a949574ab0a504b38024d2c72d04c9b867e61c`; the
console, pressure event, and interruption descriptor are under
`target/readiness/qualification-vm-overflow-20261104T0725Z/`. The guest
runtime and its temporary SSH keys were removed, the copied overlay was
discarded after extracting the report, and the verified Ubuntu base image was
not changed. Full R63 overflow recovery remains unqualified.

### R63 guest stopped before candidate transfer during another Go test wave — 2026-11-04 07:42 UTC

A fresh 4 GiB Ubuntu guest was admitted at **07:37:30 UTC** with QEMU capped at
25%. SSH became reachable after cloud-init, but candidate transfer did not
succeed before new shared-host Go test/race-test work raised pressure. The
controller stopped QEMU at **07:40:54** with load1 **13.69** and CPU PSI some
avg10 **49.22%**; systemd reported **32.191 CPU-seconds**, **921.4 MiB peak
memory**, and zero swap. The candidate was not copied and no Dux suite ran.
The console, guard event, and interruption descriptor are under
`target/readiness/qualification-vm-boot-20261104T0737Z/`; the stopped guest's
tmpfs runtime and ephemeral keys were cleaned. This is a guarded boot
interruption, not a product result.

### Production queue reached its bound again — 2026-11-04 07:42 UTC

A paired read-only status check at **07:42:22 UTC** shows the dirty production
index still has no reconciliation scan and `write_error=null`. Since the
07:36:31 sample, pending updates fell from **496,592** to **5,458**, while
`userspace_updates_dropped` rose from **1,001,175** to **1,501,934**. The
journal contains a new `pending backlog exceeded 500000` warning in that
interval, so the **500,759** increase in the cumulative drop counter confirms
a second backlog discard of roughly 500,000 updates in addition to the earlier
one. Since 07:22, the journal records two more kernel fanotify overflows and
seven reconciliation deferrals. At 07:42:22 status still reported **66,827
seconds behind**, **19,149,040 events seen / 3,562,945 resolved / 157,824
committed**, and dirty reason **non-watched filesystem mounts changed;
reconciling visible paths**.

Host load1 was **11.61** with a shared-host Go race-test wave active; root had
**80 GiB free**, so disk capacity remains adequate. The current runtime remains
the source-mismatched R16 binary. This confirms recurrent live data loss and
failure to recover on this production workload; it does not establish whether
the R63 candidate has the same behavior. No service, database, package, or
configuration mutation was made.

### Installed production daemon saturated with no pipeline progress — 2026-11-04 07:48 UTC

A read-only **20.004-second** sample from PID **2777300** at 07:47:32 measured
**20.000 CPU-seconds**, exactly **1.000 of one CPU core**. Between the paired
status snapshots, `pending_updates` stayed at **11,450**; events seen,
resolved, committed, and dropped all stayed unchanged (**19,199,529 / 3,573,455
/ 158,336 / 1,501,934**). No scan was active. `behind_seconds` advanced from
**67,163.767** to **67,183.773**, and the index remained dirty with
`write_error=null`. Thus this interval showed full-core CPU consumption with
no observable ingestion or recovery progress, not merely a steadily draining
backlog. The installed binary remains R16/source-unmatched, so the cause cannot
be assigned to the current candidate. No process attach, restart, scan, or
production mutation was performed.

### Qualification guest stopped on host I/O pressure — 2026-11-04 07:52 UTC

The 07:50:28 Ubuntu guest boot was stopped by the existing pressure guard at
**07:51:27**. Host load1 was **8.58**, CPU PSI some avg10 **15.26%**, but I/O
PSI some/full avg10 had reached **45.06% / 36.47%**; the controller's critical
I/O-some limit is 40%. Systemd reported **14.239 CPU-seconds**, **360.5 MiB
peak memory**, and zero swap. A host 8-worker I/O test was active in the
contemporaneous process sample. SSH did not become ready; the candidate was
not copied and no Dux suite ran. The console, guard event, and descriptor are
retained under `target/readiness/qualification-vm-boot-20261104T0750Z/`; the
ephemeral runtime was cleaned. This is a host-pressure boot interruption, not
a product result.

### R63 attempt stopped during 100k fixture seeding — 2026-11-04 07:57 UTC

A guarded Ubuntu 24.04 guest admitted at **07:52:26 UTC** had the exact R63
candidate and harness copied and hash-verified. Its corrected tmpfs workspace
was 3 GiB, with a planned 2,048 MiB ext4 fixture. At **07:56:04**, the host
pressure guard stopped QEMU at load1 **15.19** and CPU PSI some avg10
**66.16%**. Systemd reported **43.918 CPU-seconds**, **1.5 GiB peak memory**,
and zero swap. The SSH session's last visible seed progress was **90,000 of
100,000 files**; the recovered durable report still had `current_suite=null`
and `status=running` at 07:54:59. No completed seed, baseline scan, or Dux
suite is evidenced. This is a fixture-setup interruption, not a product
result.

The report, console, guard event, and descriptor are retained under
`target/readiness/qualification-vm-overflow-20261104T0755Z/`; the report was
recovered read-only from the stopped guest overlay. The guest runtime, copied
overlay, and temporary SSH keys were discarded after report recovery; the
verified base image is unchanged.

### Repeat installed-daemon no-progress sample — 2026-11-04 07:59 UTC

A second read-only sample of PID **2777300** again measured **20.000 CPU-seconds
in 20.005 seconds** (1.000 of one core). Paired status records were unchanged:
pending **26,087**, events seen/resolved **19,320,574 / 3,618,864**, committed
**160,896**, and dropped **1,501,934**. No scan was active. Lag advanced from
**67,808.443** to **67,828.449 seconds** while `dirty=true`, `caught_up=false`,
and `write_error=null`. A concurrent `ps -L` showed the daemon has only its
main thread, currently at **97.4% CPU**. This independently reproduces the
full-core/no-counter-progress interval from 07:47; the production executable
remains source-mismatched, so source-level root cause is still unproven.

### R63 guest stopped before kernel console startup — 2026-11-04 08:02 UTC

A 4 GiB Ubuntu guest admitted at **08:01:04 UTC** was stopped by the pressure
guard at **08:01:23** before it emitted any console bytes. The guard measured
load1 **10.18** and CPU PSI some avg10 **42.34%**; systemd reported
**4.671 CPU-seconds**. A shared-host Go test suite was active in this interval.
The candidate was not copied and no Dux suite ran. The guard event and
interruption descriptor are under
`target/readiness/qualification-vm-boot-20261104T0801Z/`; the empty ephemeral
runtime was cleaned. This is a boot interruption, not a product result.

### Repeat production full-core no-progress sample and host admission hold — 2026-11-04 08:05 UTC

Paired read-only `/usr/bin/dux status --json` snapshots at **08:05:39** and
**08:05:49 UTC** show the live production index still dirty and not caught up,
with no active scan and `write_error=null`. Pending stayed at **39,206**;
events seen/resolved stayed at **19,574,911 / 3,637,902**; committed stayed at
**161,536**; dropped stayed at **1,501,934**. `behind_seconds` advanced from
**68,224.815** to **68,234.826**. In the same interval, `/proc/2777300/stat`
advanced by **952 clock ticks** (about **9.52 CPU-seconds** at 100 Hz), while
host load1 climbed to **20.06**. This is another short interval of nearly one
core consumed without observable event-pipeline progress, on the
source-mismatched installed 0.6.3 binary; it does not identify the cause in
the R63 candidate.

The host was not suitable for another qualification guest at this checkpoint:
CPU PSI some avg10 was **51.81%**, load1 was **20.06**, and an unrelated
`go test -race` build remained active. Root had **81 GiB free** and `/dev/shm`
about **12.4 GiB free**. The guarded guest remains stopped; no attempt was made
to override its pressure admission limits. No production process, package,
configuration, or index state was changed.

### Production counters resumed after the 08:05 no-progress interval — 2026-11-04 08:06 UTC

A later read-only status snapshot at **08:06:54 UTC** showed partial movement:
events seen increased from **19,574,911** to **19,611,049** since 08:05:49,
resolved from **3,637,902** to **3,638,012**, and committed from **161,536** to
**161,664**. Pending decreased by **105** to **39,101** and userspace drops
remained at **1,501,934**. The dirty index was still **68,300 seconds behind**,
with no scan active and no write error. This bounds the previous no-progress
observation to its paired 10-second sample; it does not prove that recovery is
keeping pace with filesystem changes or that the index is exact.

At this later sample, host load1 was **17.85**, root had **79 GiB free**, and
unrelated Go race-test workers were active. The qualification VM remains
stopped under the existing admission policy.

### Intermittent status freshness and production movement under renewed host saturation — 2026-11-04 08:10 UTC

At **08:09:44 UTC**, `/usr/bin/dux status --json` still had the 08:08:41
counters (**19,654,125 seen / 3,642,289 resolved / 161,920 committed / 39,304
pending**) and **68,406 seconds** of lag. At **08:09:54**, it returned
`watch_status_available=false` with event counters null, so that response is
not evidence of zero events or a healthy queue. At **08:10:04**, status had
refreshed to **19,660,983 seen / 3,644,997 resolved / 162,048 committed / 41,587
pending** and **68,488.859 seconds** behind. Thus some pipeline work resumed
between the stale and refreshed records, but the index remains dirty with no
scan and `write_error=null`.

At 08:10:04, host load1 was **31.94** and CPU PSI some avg10 was **69.76%**;
the 08:09:45 sample had shown a concurrent `gosec`, compiler, and Go test wave.
Root had **79 GiB free** and `/dev/shm` about **12.4 GiB free**. These host
conditions exceed qualification admission limits. No VM was started, and no
production service or index changes were made.

### Installed production daemon stalled again between refreshed status records — 2026-11-04 08:11 UTC

Paired read-only `/usr/bin/dux status --json` snapshots at **08:10:52** and
**08:11:03 UTC** were unchanged for pending (**41,587**), events seen/resolved
(**19,660,983 / 3,644,997**), committed (**162,048**), and dropped
(**1,501,934**). Lag advanced from **68,538.893** to **68,548.895 seconds**.
Over the same 10.2 seconds, PID **2777300** advanced by **1,000 clock ticks**
(**10 CPU-seconds** at 100 Hz). The service remains active with zero restarts,
100% one-core quota, and one runnable daemon thread. This is another bounded
full-core interval without counter progress; it still cannot establish the
cause in the source-mismatched installed binary.

At **08:11:22**, the index remained dirty/not caught up, with **68,566.899
seconds** of lag, no active scan, and `write_error=null`. The journal since
08:01 contains fanotify queue overflows at **08:02:48**, **08:04:12**, and
**08:07:31**, plus full-reconciliation deferrals at **08:01:44** and **08:07:31**.
Host load1 was **16.37** (above VM admission), while CPU PSI some avg10 was
**0.67%** and I/O PSI some avg10 **6.62%**. Root had **79 GiB free** and
`/dev/shm` about **12.4 GiB free**. The qualification guest remains stopped;
the index and service were not modified.

### Source review: latched reconciliation could bypass the old CPU governor — 2026-11-04 08:17 UTC

The repository's exact `a427397` baseline has a `CpuGovernor::throttle()` loop
whose sleep condition includes `!RESCAN`. In the watcher, event loss latches
`RESCAN` while queued work drains. When that flag is set, the old governor
abandons its duty-cycle sleep on every iteration, a direct path to sustained
event-thread CPU use during recovery. The hardening candidate removes
`RESCAN` from the sleep interrupt condition and keeps shutdown as the interrupt;
it also adds `pending_rescan_does_not_bypass_cpu_governor`, which injects a
latched rescan and checks that the event thread still meets its CPU target.

This is a strong source-level explanation for the observed one-core samples,
not proof of the installed executable's exact code. `/usr/bin/dux` reports
`a42739776bbd+modified`, and its SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64` differs
from the retained R63 candidate SHA-256
`109b7f4aaa0de22bf428efffcadf27185b691cc71c05cac0dceb2beaa6090c7e`. The
candidate's targeted regression test and full overflow suite have not been
completed on this host yet.

At the **08:17:55 UTC** admission check, the unrelated host compile wave drove
load1 to **20.24** and CPU PSI some avg10 to **72.89%**; the 12-core host had
**19 GiB available memory**. The latest disk sample at **08:15:47** showed
**81 GiB free** on root and about **12.4 GiB free** in `/dev/shm`. The load/CPU
limits rejected another guest attempt. No qualification test or production
mutation was started during that pressure spike.

### Production backlog grew while a refreshed 10-second sample made no progress — 2026-11-04 08:19 UTC

At **08:19:45 UTC**, read-only production status reported **58,294 pending**,
**19,798,453 events seen / 3,685,009 resolved / 163,968 committed**, and
**1,501,934 userspace updates dropped**. The index remained dirty/not caught
up, **69,070.777 seconds behind**, with no active scan and `write_error=null`.
At **08:19:55**, pending and all four counters were unchanged while lag
increased to **69,080.783 seconds**. PID **2777300** advanced **880 clock ticks**
(**8.8 CPU-seconds**) over the 10.3-second interval. Across the wider interval
since 08:11, ingestion and commits did advance, but pending grew from **41,587**
to **58,294**; the index is not keeping up with the live filesystem.

The contemporaneous host load1 was **32.48** and CPU PSI some avg10 **37.47%**;
the unrelated full-module Go coverage run (PID **1050936**) was still active.
Root had **78 GiB free** and `/dev/shm` about **12.4 GiB free**. These host
conditions are above qualification admission limits. No guest or production
service/index mutation was started.

### Production backlog and lag continued to grow during host stress — 2026-11-04 08:25 UTC

At **08:25:51 UTC**, read-only production status reported **66,285 pending**,
**19,879,236 events seen / 3,698,739 resolved / 164,736 committed**, and
**1,501,934 userspace updates dropped**. Since 08:19:45, pending grew by
**7,991**, seen events by **80,783**, and commits by **768**; `behind_seconds`
rose from **69,070.777** to **69,436.492**. The index remains dirty/not caught
up with no active scan and `write_error=null`. The journal since 08:07 adds a
fanotify queue overflow at **08:23:05** and reconciliation deferrals at
**08:11:42**, **08:15:20**, and **08:20:49**.

At the snapshot, host load1 was **21.95**, CPU PSI some avg10 **43.59%**, and
I/O PSI some avg10 **4.32%**. The unrelated whole-module Go coverage job
(PID **1103586**) and an `xtop-stress` worker were active. Root had **77 GiB
free** and `/dev/shm` about **12.4 GiB free**. This supports continued partial
event ingestion but also shows production falling further behind. No
qualification or production mutation was attempted under this pressure.

### Fresh production status after another overflow — 2026-11-04 08:30 UTC

At **08:30:00 UTC**, read-only status reported **73,521 pending**,
**19,963,315 events seen / 3,710,486 resolved / 165,376 committed**, and
**1,501,934 updates dropped**. Since 08:25:51, pending grew by **7,236**, seen
events by **84,079**, and commits by **640**; lag increased from **69,436.492**
to **69,686.936 seconds**. The index remains dirty/not caught up, with no scan
and `write_error=null`. The journal added a reconciliation deferral at
**08:26:53** and fanotify overflow at **08:27:03**.

Host load1 was **31.12**, CPU PSI some avg10 **49.52%**, and I/O PSI some avg10
**4.30%**; the whole-module Go coverage job (PID **1158130**) and another I/O
stress worker were active. Root had **77 GiB free** and `/dev/shm` about
**12.4 GiB free**. The service remains active with zero systemd restarts and
100% CPUQuota. No test or production mutation was attempted under this load.

### Targeted governor test build interrupted under renewed CPU pressure — 2026-11-04 08:23 UTC

At **08:22 UTC**, the focused
`cargo test --locked --offline --bin dux pending_rescan_does_not_bypass_cpu_governor`
command began in a transient systemd scope capped at **10% of one CPU**, **4 GiB
memory**, and `IOWeight=1`. Cargo used one build job, with its target directory
on `/dev/shm` to avoid build output in Dux's watched ext4 tree. The build
compiled initial dependencies but did not reach the test binary. At **08:23:26**,
host load1 was **27.00** and CPU PSI some avg10 **67.99%** while an unrelated Go
compile wave was active; the transient scope was stopped. It exited with
SIGTERM (143), so the regression test has **no result**. Its partial build tree
was removed from `/dev/shm` after the scope became inactive.

After cleanup, `/dev/shm` returned to **595 MiB used / about 12.4 GiB free**;
root had **78 GiB free**. At **08:23:58**, load1 was **24.25** and CPU PSI some
avg10 **25.75%**. The attempt was deliberately bounded and no production state
was changed; rerun the focused test and R63 guest suite when admission permits.

The focused build was retried at **08:27 UTC** with the same isolated tmpfs
target, one Cargo job, `nice`/idle I/O priority, and a transient scope capped at
**20% of one CPU** and **4 GiB memory**. It compiled further dependencies but
still did not reach the Dux test binary. At **08:28:22**, host load1 was **25.58**
and CPU PSI some avg10 **80.09%**, with unrelated compiler/stress processes
active. The scope had consumed **12.75 CPU-seconds** and **147 MiB** at stop; it
was terminated with SIGTERM (143). After removing its partial build tree,
`/dev/shm` returned to **595 MiB used**; at **08:28:51**, load1 had reached
**33.72** and CPU PSI some avg10 **81.29%**. This second bounded attempt also
produced no test result, and production state was not changed.

### Production status advanced while backlog and lag worsened — 2026-11-04 08:32 UTC

At **08:32:54 UTC**, status reported **79,125 pending**, **19,970,778 events
seen / 3,717,824 resolved / 165,504 committed**, and **1,501,934 updates
dropped**. Compared with 08:30:00, pending increased by **5,604**, events seen
by **7,463**, resolved by **7,338**, and committed by **128**; lag increased
from **69,686.936** to **69,860.354 seconds**. The index remains dirty, with no
scan and `write_error=null`. The journal added another reconciliation
deferral at **08:31:06**.

Host load1 was **30.05**, CPU PSI some avg10 **72.91%**, and I/O PSI some avg10
**0.41%**. Concurrent compiler and network-stress processes were active. Root
had **79 GiB free** and `/dev/shm` about **12.4 GiB free**. Dux continues to
ingest some events, but its queue and lag are growing; no qualification or
production mutation was attempted at this pressure level.

### Governor regression build interrupted after compiling the Dux crate — 2026-11-04 15:00 UTC

After the host cleared admission at **14:54 UTC**, the focused
`pending_rescan_does_not_bypass_cpu_governor` test was rebuilt with one Cargo
job in a transient scope limited to **20% of one CPU**, **4 GiB memory**, and
`IOWeight=1`; the target was confined to `/dev/shm`. Cargo compiled dependencies
and reached `dux v0.6.3`, but the test binary and body did not complete. At
**15:00:03**, a new Go compile wave drove host load1 to **14.65** and CPU PSI
some avg10 to **57.58%**, crossing the host controller's critical conditions.
The scope had used **67.46 CPU-seconds** and peaked at **765 MiB**; it was
stopped with SIGTERM (143). Its partial target tree was removed after the unit
was confirmed inactive.

At **15:00:28**, host load1 was **24.08** and CPU PSI some avg10 **75.11%**.
Root had **70 GiB free** and `/dev/shm` returned to **594 MiB used / about
12.4 GiB free**. This attempt produced no regression result and changed no
production state.

### Qualification-controller tests pass; Rust regression retry stopped on host load — 2026-11-04 15:03 UTC

The Python VM-controller regression suite passed **29/29** in **2.598 seconds**
under a transient scope capped at **5% CPU**, **512 MiB memory**, and
`IOWeight=1`, with `TMPDIR=/dev/shm`. Coverage includes VM admission and
pressure-stop thresholds, unavailable-telemetry fail-closed behavior, safe
ephemeral cleanup, collected systemd guest handling, and CLI command safety.
This verifies the qualification controller tests only; it is not Dux Rust or
runtime qualification.

A fresh attempt of the Rust `pending_rescan_does_not_bypass_cpu_governor` test
started at **15:02 UTC** under a **20% CPU / 4 GiB / IOWeight=1** transient
scope. Cargo compiled early dependencies but did not reach the test binary.
At **15:03:22**, host load1 reached **15.93** with CPU PSI some avg10 **33.24%**
and active staticcheck/gosec work. Because load exceeded the guest controller's
critical limit, the scope was stopped (exit **143**) and its partial target
removed. At **15:03:42**, load1 was **19.84** and CPU PSI some avg10 **68.95%**.
Root had **70 GiB free** and `/dev/shm` returned to **594 MiB used**. The Rust
regression remains unrun; production state was not changed.

### Production backlog accelerated during another Go test wave — 2026-11-04 15:05 UTC

At **15:05:19 UTC**, read-only production status showed **75,666 pending**,
**26,665,541 events seen / 5,239,352 resolved / 220,160 committed**, and
**2,003,046 userspace updates dropped**. Compared with the **14:54:08** sample,
pending increased by **16,469**, events seen by **241,610**, resolved by
**24,482**, and commits by **1,024**; lag increased by **671.344 seconds** to
**93,405.902 seconds**. The index remains dirty/not caught up with no scan and
`write_error=null`. A new reconciliation deferral was logged at **15:05:15**.

Host load1 was **13.62**, CPU PSI some avg10 **10.78%**, and I/O PSI some avg10
**15.27%**; an unrelated `go test ./...` run and `gatecat-index` were active.
The journal since 08:30 records the **14:00:04** backlog discard and repeated
fanotify overflows through **15:00:51**. Root had **70 GiB free** and
`/dev/shm` about **12.4 GiB free**. Dux is processing events, but pending work
and lag continue to grow; candidate qualification remains open.

### CPU governor regression passes on the current source — 2026-11-04 15:14 UTC

The focused regression `pending_rescan_does_not_bypass_cpu_governor` passed
(**1 passed, 255 filtered out**) using the command
`cargo test --locked --offline --bin dux pending_rescan_does_not_bypass_cpu_governor`. The test profile built in **5m08s**
inside a transient systemd scope capped at **20% CPU**, **4 GiB memory**, and
`IOWeight=1`; Cargo used one job and wrote its target to `/dev/shm`. The test
itself completed in **5.01 seconds**. The scope exited and its tmpfs build tree
was removed; `/dev/shm` returned to **594 MiB used / about 12.4 GiB free**.
This verifies the targeted governor regression only, not the remaining 255 Rust
tests or runtime recovery behavior.

At **15:14:30 UTC**, production status still reported a dirty, not-caught-up
index with **84,714 pending**, **26,779,968 events seen / 5,268,336 resolved /
221,824 committed**, and **2,003,046 updates dropped**. Compared with 15:07:06,
pending grew by **8,577**, seen events by **82,201**, resolved events by **20,751**,
and commits by **1,536**; lag increased from **93,511.457** to **93,956.003
seconds**. No scan was active and `write_error=null`. The daemon remained live
with zero service restarts. The focused test passed without changing production
state; production qualification remains open.


### Full current-source Rust unit suite passes; production lag keeps growing — 2026-11-04 15:22 UTC

`cargo test --locked --offline --bin dux` passed the full Rust binary unit suite:
**256 passed, 0 failed, 0 ignored**, in **74.58 seconds**. This includes the CPU
governor regression, watcher churn and rollup tests, schema migration and scan
projection tests, pressure admission/cancellation tests, query error handling,
and tunnel/security checks. The test profile built in **5m31s** with one Cargo
job inside a transient systemd scope limited to **20% CPU**, **4 GiB memory**,
and `IOWeight=1`; its target was isolated to `/dev/shm`. The scope exited and the
temporary target was removed, returning `/dev/shm` to **594 MiB used / about
12.4 GiB free**. This is full coverage of the 256 unit tests in the `dux`
binary test target; integration, installed-package, end-to-end runtime,
release, and endurance gates remain separate and unqualified.

At **15:22:12 UTC**, production status remained dirty/not caught up with
**95,619 pending**, **26,848,909 events seen / 5,302,919 resolved / 224,512
committed**, and **2,003,046 updates dropped**. Compared with 15:14:30, pending
grew by **10,905**, seen events by **68,941**, resolved events by **34,583**, and
commits by **2,688**; lag increased from **93,956.003** to **94,417.180
seconds**. No scan was active and `write_error=null`. Host load1 was **3.37**,
CPU PSI some avg10 **6.99%**, and I/O PSI some avg10 **0.09%** after the run.
The service stayed live; the passing unit suite did not change production
state. The widening backlog and event-drop history remain production blockers.


### Vendored jwalk integration suite deferred after host pressure spike — 2026-11-04 15:28 UTC

The vendored `jwalk` crate has its own upstream integration tests, including a
single-threaded Rayon deadlock regression, outside the root Dux test target. An
offline attempt stopped before compilation because the local Cargo index lacked
the locked `criterion` package; no tests ran. Cargo then downloaded the locked
test dependencies and started a retry with its target isolated to `/dev/shm`,
one build job, **10% CPU**, **3 GiB memory**, and `IOWeight=1`. At **15:27:56**,
host load1 reached **22.20**, so the transient scope was stopped before reaching
test execution. Its partial target was removed. The vendored crate's own tests
remain unverified; the Dux root test suite's `jwalk_hands_each_directory_its_own_parallel_read_state`
regression did pass in the 256-test run.

At **15:28:16 UTC**, after stopping the scope, host load1 was **29.67** and CPU
PSI some avg10 **70.96%**. Production remained dirty/not caught up with **99,250
pending**, **26,908,286 seen / 5,309,997 resolved / 225,024 committed**, and
**2,003,046 userspace updates dropped**; lag was **94,781.186 seconds**, no scan
was active, and `write_error=null`. The run was stopped to avoid adding load;
no production state was changed.


### Vendored jwalk unit, integration, and documentation tests pass — 2026-11-04 15:36 UTC

The locked vendored dependency test command
`cargo test --manifest-path vendor/jwalk/Cargo.toml --locked --offline` passed:
**1 deadlock regression, 50 integration tests, and 3 doctests**, with zero
failures. The crate library test harness contained zero unit tests. The test
profile built in **6m02s** within a transient systemd scope capped at **10% CPU**,
**3 GiB memory**, and `IOWeight=1`, with one Cargo job and the target isolated to
`/dev/shm`. The scope exited and its temporary build tree was removed; `/dev/shm`
returned to **594 MiB used / about 12.4 GiB free**. Cargo emitted a
future-compatibility warning for the vendored crate's test-only dependency
`fs_extra 1.2.0`; all tests passed.

At **15:36:46 UTC**, production status remained dirty/not caught up with
**100,274 pending**, **26,975,349 events seen / 5,329,624 resolved / 225,664
committed**, and **2,003,046 userspace updates dropped**. Compared with 15:29:08,
pending grew by **1,024**, seen events by **67,063**, resolved events by **19,627**,
and commits by **640**; lag increased from **94,833.200** to **95,290.930
seconds**. No scan was active and `write_error=null`. Host load1 was **3.54**,
CPU PSI some avg10 **7.06%**, and I/O PSI some avg10 **0.59%** after the run.
Root had **73 GiB free**. Tests changed no production state; the increasing
backlog and lag remain release blockers.


### Production backlog and host pressure worsened after passing suites — 2026-11-04 15:38 UTC

At **15:38:45 UTC**, Dux remained active (PID **2777300**, **0 restarts**) but
its index was dirty/not caught up with **104,816 pending**, **27,025,045 events
seen / 5,343,360 resolved / 225,920 committed**, and **2,003,046 userspace
updates dropped**. Compared with 15:36:46, pending grew by **4,542**, seen
events by **49,696**, resolved events by **13,736**, and commits by **256**; lag
increased from **95,290.930** to **95,409.699 seconds**. No scan was active and
`write_error=null`. The journal recorded a kernel fanotify queue overflow at
**15:32:53** and reconciliation deferrals at **15:30:05** and **15:36:49**.
The userspace dropped-update counter did not change in this interval; kernel
queue overflow is a separate event.

At **15:38:26**, host load averages were **22.72 / 11.63 / 9.79** and CPU PSI
some avg10 **44.84%** (full avg10 **0%**); compiler and Go test processes were
active. Root had **73 GiB free**. The host exceeded the test admission limit, so
no additional build or test was started. These readings reinforce that successful
unit/dependency tests do not qualify live convergence or production readiness.


### Live executable remains unmapped to the tested source — 2026-11-04 15:39 UTC

Read-only identity checks at **15:39:49 UTC** report `/usr/bin/dux` as
`dux 0.6.3 (a42739776bbd+modified) [production]`, SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`, build ID
`2e2d4557ac578ce614c1b48cfb6b53a389b27330`. The tested `src/watch.rs` and
`src/store.rs` are from source commit `5342ae2f52cb2a762dc2f7927f7098252dec1f29`.
The live executable's `+modified` label does not identify its source bytes, and
its build ID is not tied to the tested tree by current provenance evidence. The
unit-suite passes therefore qualify the checked-out source only, not the running
production daemon. No executable or service state was changed.


### Strict Clippy passes for all Dux targets; production backlog continues to grow — 2026-11-04 15:45 UTC

`cargo clippy --locked --offline --all-targets -- -D warnings` completed
successfully with no warnings for Dux. The check covered all configured Dux
targets, including test targets, and finished in **4m26s** inside a transient
systemd scope capped at **20% CPU**, **4 GiB memory**, and `IOWeight=1`, with one
Cargo job and output isolated to `/dev/shm`. The scope exited, its build tree was
removed, and `/dev/shm` returned to **594 MiB used / about 12.4 GiB free**. This
is static lint evidence; it does not qualify runtime or deployed-binary behavior.

At **15:45:19 UTC**, production remained dirty/not caught up with **121,479
pending**, **27,102,776 events seen / 5,367,612 resolved / 226,432 committed**,
and **2,003,046 userspace updates dropped**. Compared with 15:42:57, pending
grew by **7,368**, seen events by **50,582**, resolved events by **11,049**, and
commits by **256**; lag increased from **95,663.507** to **95,804.782 seconds**.
No scan was active and `write_error=null`. Host load1 was **7.22**, CPU PSI some
avg10 **10.88%**, and I/O PSI some avg10 **1.78%**. Root had **71 GiB free**.
The check changed no production state; convergence, event-loss recovery, and
live executable provenance remain open blockers.


### Optimized release build stopped on renewed CPU pressure — 2026-11-04 15:47 UTC

A production-profile candidate build was started with
`cargo build --locked --offline --release --bin dux`, one Cargo job, and its
target isolated to `/dev/shm`, inside a transient scope capped at **20% CPU**,
**4 GiB memory**, and `IOWeight=1`. At **15:47:16**, host load1 reached **16.76**
and CPU PSI some avg10 **62.03%**, exceeding the controller stop threshold.
The scope was stopped before it produced an executable; its partial target was
removed, restoring `/dev/shm` to **594 MiB used / about 12.4 GiB free**. The
optimized current-source binary and its CLI smoke checks remain unverified.

At **15:47:30 UTC**, production remained dirty/not caught up with **124,155
pending**, **27,147,896 events seen / 5,372,631 resolved / 226,688 committed**,
and **2,003,046 userspace updates dropped**. Compared with 15:45:19, pending
grew by **2,676**, seen events by **45,120**, resolved events by **25,019**, and
commits by **256**; lag increased from **95,804.782** to **95,935.905 seconds**.
No scan was active and `write_error=null`. The pressure spike was concurrent with the build; causality is not established.
No production binary or service state was changed.


### Release packaging CLI tests incomplete under an overly tight CPU cap — 2026-11-04 15:51 UTC

Ran the isolated release-script suites with
`PYTHONPATH=scripts python3 -m unittest test_build_release_cli
test_build_release_debug_artifact -v` in a transient scope capped at **5% CPU**,
**512 MiB memory**, and `IOWeight=1`. Of **13 tests**, **10 passed** and **3
raised `subprocess.TimeoutExpired`** at the harness's 5- or 15-second deadlines:
`test_mid_publish_rename_failure_restores_previous_release_files`,
`test_mismatched_binary_version_is_rejected_before_packaging`, and
`test_native_release_build_rejects_unpinned_cargo_auditable`. The output shows
timeouts, not assertion failures. That attempt was incomplete. A later rerun
with a one-core allowance passed all 13 tests; see the current-source release
entry below. Tests use temporary paths and
fake build/package tools; no production package or service was changed.

Low-cost syntax checks passed: `bash -n` for both release/permissions shell
scripts and Python AST parsing for all **51** scripts. `git diff --check` also
passed.

At **15:51:22 UTC**, production remained dirty/not caught up with **124,341
pending**, **27,201,161 seen / 5,374,705 resolved / 226,944 committed**, and
**2,003,046 userspace updates dropped**; lag was **96,168.004 seconds**, no scan
was active, and `write_error=null`. At 15:50:44 host load1 was **22.12** and CPU
PSI some avg10 **68.71%**; root had **70 GiB free** and `/dev/shm` was **595 MiB
used**. No further build or package test was started at that pressure level.

### Current-source release build and packaging tests pass; static package build interrupted — 2026-11-04 16:15 UTC

A fresh `cargo build --locked --offline --release --bin dux` completed in
**11m39s** under a transient **20% CPU / 4 GiB / IOWeight=1** scope, with one
Cargo job and build output in `/dev/shm`. The optimized GNU-host executable
reported Git revision **706e51a997e4**, source-tree SHA-256
`e65d6aba9e1a0da9034bcdf4a97c06ee4a20622e7c0ad55d1d5756ae89b76810`, and build
ID `d3875f8b4dcc615823b6479e57c7c1038027ba8`. An independent recomputation of
the build script's source-tree digest matched. The executable SHA-256 was
`db7965b1c039be20cdc5ce95708bdb20804630316360230f8d59120bfdd44709`. Its
`--version`, top-level help, `status --help`, and `scan --help` checks succeeded.
A read-only `status --json` invocation against `/var/lib/dux/dux.db` also
succeeded on the live v5 database and reported kernel overflow count as null
for its legacy v3 watcher-status record. This artifact was dynamically linked
for the GNU host and is **not** the required static-musl release package.

The two release packaging CLI modules were rerun at **100% CPU quota / 1 GiB**
and passed **13/13** in **2.161s**, including publication rollback, source/hash
rejection, fault-injection rejection, static metadata checks, and debug-symbol
build-ID binding. This resolves the three timeouts from the earlier 5% quota
run; no package was installed on the production host.

The official `scripts/build-release.sh` was then started for package revision
**64** with `cargo-auditable 0.7.7`, the musl target, and its target in tmpfs, in
a **20% CPU / 4 GiB / IOWeight=1** scope. Before compilation reached packaging,
host load1 rose to **23.72** and CPU PSI some avg10 to **44.77%** at 16:15:15;
the scope was stopped. At 16:15:34 host load1 was **33.87** and CPU PSI some
avg10 **73.28%**. The partial build target was removed and the empty output
directory discarded. No static-musl executable, DEB, RPM, or manifest was
produced.

At **16:15:34 UTC**, production remained dirty/not caught up with **136,152
pending**, **27,466,678 events seen / 5,430,507 resolved / 230,912 committed**,
and **2,003,046 userspace updates dropped**. Lag was **97,620.849 seconds**, no
scan was active, and `write_error=null`; the service was live with zero
restarts. Root had **71 GiB free** and `/dev/shm` returned to **595 MiB used**.
The installed binary remains the separately identified `a427397+modified`
executable; the successful GNU build and tests do not qualify the deployed
binary or production convergence.


### Effective service security policy drift and second static-build stop — 2026-11-04 16:23 UTC

A fresh read-only policy check at **16:19:44 UTC** found that the loaded unit
`/usr/lib/systemd/system/dux.service` (SHA-256
`dd1aa5dcc481c3467c3fbcebbf4f9fda11ddb7082b802412910575dc0a21d47b`) differs
from the checked-out `packaging/dux.service` (SHA-256
`dd34d3086af76117b18b06a23c5c70ace4353ff6a33ae9b3fa3c10f13c04d920`).
`systemd-analyze verify packaging/dux.service` passed. Effective systemd
properties show `SystemCallFilter=~delete_module finit_module init_module`,
while the source unit denies `@module @mount @swap @reboot @clock @obsolete
@raw-io @debug @cpu-emulation`; effective `IOAccounting=no` while source sets
`IOAccounting=yes`. Effective `SecureBits=3`, `LimitCORE=0`, capability sets,
`CPUQuotaPerSecUSec=1s`, `MemoryHigh=2 GiB`, `MemoryMax=4 GiB`, and
`RestrictAddressFamilies=AF_UNIX` match the intended policy. The active daemon
was not restarted and no unit override was changed: applying the source filter
to the source-unmapped old executable needs a qualified package/restart plan.

The official static-musl package build was retried at **16:20 UTC** for package
revision **64**, under a **20% CPU / 4 GiB / IOWeight=1** scope with one Cargo
job and tmpfs output. At **16:23:37**, host load1 reached **15.91** and CPU PSI
some avg10 **53.53%**, after a prior sample at 16:22:56 with load1 **10.84**
and CPU PSI some avg10 **42.51%**. The scope was stopped before packaging.
The partial build tree and empty output directory were removed; `/dev/shm`
returned to **595 MiB used / about 12.4 GiB free**. A process sample after stop
showed multiple external Go `compile` and `vet` processes active. Pressure and
the package build were concurrent; causality is not attributed. No release
artifacts were produced and production configuration was not changed.

Production status at **16:23:52 UTC** remained dirty/not caught up with
**156,875 pending**, **27,682,161 events seen / 5,463,382 resolved / 231,680
committed**, and **2,003,046 userspace updates dropped**. Lag was
**98,118.050 seconds**, no scan was active, and `write_error=null`. The journal
recorded fanotify queue overflows at **16:20:21** and **16:23:07**. Host load1
was **19.04**, CPU PSI some avg10 **56.35%**, and root had **69 GiB free**.
The daemon remained active with zero systemd restarts.


### Enable source-required I/O accounting; syscall-filter drift remains — 2026-11-04 16:26 UTC

The source package unit declares `IOAccounting=yes`, but the loaded production
unit had it disabled. Applied the reversible-in-source, persistent systemd
control property with `systemctl set-property dux.service IOAccounting=yes`,
then ran `systemctl daemon-reload`. The setting became effective online: Dux
remained active as PID **2777300** with **0 restarts**, `IOAccounting=yes`, and
its CPU quota unchanged at one CPU. No application process was restarted.

Reran the repository's read-only
`dux-package-lifecycle.assert_service_security_policy()` check. It now passes
the I/O-accounting assertion and fails at `SystemCallFilter`: effective
`~delete_module finit_module init_module` is missing the source-required
`@clock`, `@obsolete`, mount, reboot, swap, raw-I/O, debug, and related syscall
denials. Applying that filter to the source-unmapped old executable would take
a service restart and could affect its syscall compatibility, so the filter is
left unchanged pending a qualified current-source package and recovery plan.
`systemd-analyze verify packaging/dux.service` still passes; source syntax
validation does not replace effective-unit conformance.

At **16:26:20 UTC**, production remained dirty/not caught up with **157,280
pending**, **27,737,362 events seen / 5,465,593 resolved / 231,936 committed**,
and **2,003,046 userspace updates dropped**. Lag was **98,264.883 seconds**, no
scan was active, and `write_error=null`. Host load1 was **12.70** and CPU PSI
some avg10 **23.21%**. The I/O-accounting change added observability only; it
did not change the live index or resolve the convergence and kernel-overflow
blockers.


### Static-musl release build stopped on host pressure; live backlog persists — 2026-11-04 16:49 UTC

Retried the official static-musl release/package pipeline for package revision
**64** from checkout **baee3c0**. The one-job build used a transient systemd
scope with a **4 GiB memory cap**, `IOWeight=1`, `Nice=19`, tmpfs target/output,
and initially a **10% CPU quota**; the quota was raised to **20%** after host
load fell below 5 and the slow bundled SQLite compile became the bottleneck.
Cargo progressed through the musl dependencies and was compiling
`tracing-subscriber` when the host guard sample at **16:48:16** reached CPU PSI
some avg10 **66.56%**. The build was interrupted with exit **130**. No release
binary or package was produced. The transient scope became inactive, all of its
compiler processes exited, and its isolated tmpfs tree was removed; `/dev/shm`
returned to **595 MiB used / about 12.4 GiB free**. The attempt is stopped due
to host pressure, not a compilation failure.

The scope ran while unrelated Go compile/test jobs were active; concurrent
workloads prevent attributing the pressure spike to this build alone. At the
stop sample, load1 was **10.65** and CPU PSI full avg10 **0%**; by **16:49:21**,
after the build had exited, load1 had risen to **25.53** and CPU PSI some avg10
to **60.34%**. Root had **70 GiB free**. Production Dux remained active as PID
**2777300**, with **0 restarts**, the original one-CPU quota, and
`IOAccounting=yes`.

Read-only production status at **16:49:21 UTC** remained dirty and not caught
up: **191,848 pending**, **28,070,135 events seen / 5,561,150 resolved / 235,264
committed**, **2,003,046 userspace updates dropped**, and **99,647.772 seconds
behind**.
No scan was active and `write_error=null`. The latest recorded fanotify
overflow was **16:30:31**; reconciliation deferrals were logged at **16:32:05**,
**16:38:03**, and **16:46:28**. The live index still needs a qualified recovery
scan and exactness check. No production binary, service restart, or database
change occurred during this build attempt.

### Source systemd syscall-filter smoke passes; live unit remains mismatched — 2026-11-04 16:50 UTC

Ran `scripts/dux-systemd-filter-smoke.py` in a transient **5% CPU / 256 MiB /
IOWeight=1** scope. Both `dux.service` and `dux-tunnel.service` probes passed:
the configured restricted syscall groups returned `ENOSYS`, `clock_gettime`
remained available, Dux retained `open_by_handle_at` for fanotify FID
resolution while the tunnel denied it, and address-family policies matched
their unit files. The scope exited successfully and is inactive. This validates
the source unit's seccomp policy on this kernel; it does **not** change or prove
the loaded production unit, whose effective `SystemCallFilter` still lacks the
source denylist. No service or index state changed.

### Existing static candidate rejected for current deployment — 2026-11-04 16:51 UTC

Checked the newest preserved static candidate at
`target/readiness/current-source-release-20261103T1321Z` before considering reuse.
Its manifest binds the binary to commit **a42739776bbd** with a dirty worktree
and source digest `d952c8d46a7b57234c46a4af24c07698c3ac07512732ce56fa520634957dd4d6`.
Its `--version` independently reports `a42739776bbd+modified` and that same
digest. Comparing the manifest's **126** source-file hashes with this checkout
found **115** matches, **9** mismatches (including qualification scripts and
documentation), and **2** missing workflow files. Although the core Rust files
match, the candidate does not attest to the current complete source tree and is
not eligible for the exact-source release/package gate. The failed static build
left no replacement artifact.

At **16:52:24 UTC**, the service remained active as PID **2777300** with zero
restarts and a one-CPU quota. `IOAccounting=yes` remains effective, while the
effective syscall filter is still only `~delete_module finit_module
init_module`. The read-only status remains dirty/not caught up with **206,495
pending**, **28,136,484 events seen / 5,582,802 resolved / 235,520 committed**,
**2,003,046 userspace updates dropped**, and **99,829.166 seconds behind**; no
scan is active and `write_error=null`. Compared with 16:49:21, pending rose by
**14,647** and lag by **181.394 seconds** while seen/resolved/committed counts
advanced. Host load1 was **6.55**, CPU PSI some avg10 **1.00%**, and root had
**70 GiB free**; `/dev/shm` was back at **595 MiB used**. Resource pressure has
receded, but live indexing is still not converging and the production syscall
policy remains mismatched.


### Create a verified rollback backup of the live v5 index — 2026-11-04 17:02 UTC

Created `/var/backups/dux-index-prequalification-20261104.sqlite` with
`scripts/dux-backup-index.py` under a transient **5% CPU / 512 MiB / IOWeight=1**
scope while the daemon remained online. The helper used SQLite's online backup
API, verified the Dux schema and `PRAGMA quick_check`, computed the backup
SHA-256, and published the database plus JSON manifest without clobbering any
existing file. The verified copy is **530,878,464 bytes**, schema **v5**, with
**129,609 pages × 4,096 bytes**, `quick_check=ok`, SHA-256
`fb9274d6709d93b3b6af41512c251b13bc807f8f0a8dcc620338eef57fde7cbe`; both
files are root-owned mode **0600**. No temporary files remain. This is a
rollback snapshot, not proof that the live index is exact or caught up.

At **17:02:34 UTC**, Dux was still active as PID **2777300** with zero restarts.
Status remained dirty/not caught up with **211,951 pending**, **28,348,940 seen /
5,596,181 resolved / 236,416 committed**, **2,003,046 userspace updates
dropped**, and **100,440.261 seconds behind**; no scan was active and
`write_error=null`. Root had **67 GiB free**. The backup did not install a
binary, restart the service, or alter the production index.

### Exact-source static release package built and smoke-qualified — 2026-11-04 17:32 UTC

Completed `scripts/build-release.sh` for package revision **64** from clean
checkout **d7de5189fde2c83a336c4f6fc3d9ec537699c0db**. The guarded, single-job
static-musl build used isolated tmpfs storage, `Nice=19`, `IOWeight=1`, a 4 GiB
memory cap, and a 20% CPU quota. Its CPU-pressure controller froze and resumed
the build as host pressure changed. The release script completed successfully
and produced the static binary, matching Build-ID debug symbols, source archive,
DEB, RPM, manifest, and SHA256SUMS. The manifest reports a clean worktree,
124 source files, source-tree SHA-256
`33ec8c9685530af62a232c49baa53f4947fce5076f8edf7137b8dc7f4b4a5073`, Rust
1.99.1, and cargo-auditable 0.7.7. All six artifact checksums verified both in
tmpfs and after copying the 42 MiB artifact set to
`target/readiness/release-current-20261104-rev64/`.

The release gate verified the binary is static, production-marked, revision-
and source-digest-matched, has embedded dependency metadata, and has matching
Build-ID debug symbols. Both package payloads contain the expected executable,
permission helper, online-backup helper, and systemd units. `systemd-analyze
verify packaging/dux.service` passed; offline systemd security analysis rated
the unit exposure **3.3 (OK)**. The candidate's `--version`, `--help`, and
read-only `status --json` commands succeeded against both the live database and
the verified backup. This is build and read-only smoke qualification; it is not
a full v5-to-v6 rebuild rehearsal or a live upgrade.

At **17:31:58 UTC**, the live service remained PID **2777300** with zero
restarts. The index was still dirty and not caught up: **229,217 pending**,
**28,716,505 events seen / 5,682,073 resolved / 240,128 committed**,
**2,003,046 userspace updates dropped**, and about **102,205 seconds behind**;
no scan was active and `write_error=null`. The production DB remains schema v5;
the candidate is schema v6 and correctly opens v5 read-only. Installing it would
trigger the atomic background baseline/reconciliation path. Because the current
index is already substantially behind and prior reconciliation has been
deferred under host pressure, the production service and database were left
untouched pending a controlled rebuild/recovery qualification and post-install
convergence evidence.

After the package build, the targeted release/package regression suites also
passed: **54** package-lifecycle tests and **12** build-release CLI tests, plus
`git diff --check`. These complement the earlier Rust, Clippy, formatting, and
filter-smoke results recorded above; no code changed in this qualification
step.

GitHub Actions currently reports both registered workflows disabled manually.
The `ci.yml` and `readiness.yml` definitions are absent from both the current
branch and fetched `main`; the zero-second failures are prior runs generated
while the workflow was disabled, not active jobs. No workflow source change was
needed in this checkout.

### Installed exact-source package; live v6 rebuild completed but index remains unsafe — 2026-11-04 18:12 UTC

Installed the qualified **0.6.3-64** DEB on this production host after saving
the prior `/usr/bin/dux`, packaged unit, effective unit properties, and
installed-package state under
`target/readiness/production-preupgrade-20261104/`. Re-verified those rollback
file hashes and the v5 database backup hash after installation. The package
post-install restart requested a graceful stop; the old daemon drained and
exited successfully after about three minutes. The new exact-source binary is
now active as PID **4030438**, `NRestarts=0`. Its packaged syscall denylist is
effective, and `IOAccounting=yes` and the existing one-CPU quota remain active.

The daemon watched both filesystems and performed the v5-to-v6 atomic baseline
rebuild. The v5 database remained available until the staging index was
installed; the resulting v6 database passed a read-only `PRAGMA quick_check`
(`ok`). The first baseline found **3** coverage errors: the walker reached
short-lived Go test directories in `/tmp` that disappeared before it read
them (`ENOENT`). The second baseline had **0** scan errors, but unresolved
stale file handles and a kernel fanotify overflow still made event continuity
uncertain. A third same-loop scan started immediately after the retry circuit
reported that scans were paused. Automatic scans then paused with the index
still dirty, as the retry counter reached **3**. The v5 backup is still intact;
the live database is v6.

At **18:11:49 UTC**, the current daemon was still active with zero restarts,
**153,055 pending** updates, **1,127,688 events seen / 579,549 resolved / 4,096
committed**, **0 userspace updates dropped**, and **3 kernel overflow
notifications**. It reported `dirty=true`, reason `fanotify queue overflow`,
`caught_up=false`, and about **2,072 seconds behind**; no scan or write error
was active. The main database was about **500 MiB** with a **128 MiB WAL**;
root had **67 GiB free**. Installing the new binary did not resolve production
convergence, so this host is not qualified as production-ready.

### Regression fixes for observed retry and scan-race behavior — 2026-11-04 18:12 UTC

The live run exposed a same-loop retry-circuit race: after the second failed
reconciliation set the paused flag, a pending rescan request could still launch
another scan. Added an explicit admission predicate that refuses automatic
scan starts while the retry circuit is open, with coverage for paused, active
scan, active accounting check, and no-request cases.

The scan's walker counted `ENOENT` from a directory that vanished after parent
enumeration as incomplete coverage. Such a missing current subtree is a normal
concurrent-delete race; other I/O errors (including permission failures) still
count as coverage failures. Added handling for that case and a focused unit
test. Both changed regression tests passed using the release-profile test
binary: **2 passed, 0 failed**. `cargo fmt --all -- --check` and `git diff
--check` passed. The running production executable remains package revision 64;
these source fixes are not yet part of a deployed binary.

### Revision 65 production retry and scan-race verification — 2026-11-04 18:36 UTC

Built the static exact-source revision **65** package from commit
`af8fdccea59a41eea5daaed27cefb166ecda2b8c` (source-tree SHA-256
`3dec5ab0b1606e88a55786da3dfe08a99656fc261ff6f8eb3907ef91720476cc`). The
release manifest, artifact checksums, static/production binary metadata,
Build-ID symbols, DEB/RPM payloads, offline systemd checks, candidate CLI, and
read-only status checks passed. The package was installed and is running as
`dux 0.6.3 (af8fdccea59a) [production]`; systemd reports PID **124198** and
zero restarts. The v5 pre-upgrade and v6 pre-rescan verified backups remain
available under `/var/backups/`.

The revised walker completed two live baselines with **zero scan coverage
issues**, confirming the vanished-entry handling. The retry admission fix also
worked: after two incomplete scans, the circuit paused and did not start a
third automatic scan. Both scans nevertheless observed unresolved fanotify
events (`open_by_handle_at` returned `ESTALE` for files removed before event
resolution), so continuity could not be proven and the daemon correctly keeps
the index dirty. This host was concurrently running Go builds and tests with
high short-lived-file churn. At **18:35 UTC**, service health remained active
with zero restarts, zero user-space drops, zero kernel overflow notifications,
no write error, about **12,964 pending updates**, and a **128 MiB WAL**. The
automatic reconciliation circuit is paused after two attempts. Production
convergence is therefore **not qualified**; wait for the host's filesystem
churn to settle before requesting another manual reconciliation, then require
a complete scan with no lost events and a caught-up clean status before
claiming readiness.

### Manual quiet-window reconciliation attempt — 2026-11-04 18:41 UTC

After the earlier Go tests exited and a 30-second sample contained no stale
handle warnings, requested an operator-triggered in-place reconciliation via
`dux scan /`. It ran for about 149 seconds and again found **zero walker
coverage issues**, but the host's container workload resumed file churn during
the scan. The watcher logged repeated `ESTALE` failures while resolving
fanotify `DFID_NAME` events and recorded **one kernel queue overflow**. The
result correctly remained dirty (`incomplete coverage or changes lost during
reconciliation`); it must not be treated as a successful recovery. At 18:42
UTC, the service was still active with **zero restarts**, zero userspace drops,
about **28,248 pending updates**, and a **128 MiB WAL**. The separate
`/home/xgenstack.com/backend` Go test/build workload was active again. It was
left untouched because interrupting another application's tests or service
would risk its availability. A clean full reconciliation and post-scan event
continuity proof remain the production-readiness blockers on this host.

The daemon's single bounded automatic retry then ran from **18:42:39 to
18:44:55 UTC**. It again completed the full walk with **zero coverage issues**,
but could not prove event continuity and correctly opened its retry circuit
after two failed full scans. During the attempt the WAL briefly exceeded
**300 MiB** before checkpoint recovery; it was back at **128 MiB** at the next
status sample. At **18:45 UTC**, the daemon had zero restarts and userspace
drops, one recorded kernel overflow, about **59,673 pending updates**, and
`dirty=true`. Automatic full scans are now paused pending explicit operator
action. Repeated manual retries during the continuing Go workload would only
reproduce the same host-load and event-loss conditions.

### Continued event-pressure and flush-latency observation — 2026-11-04 18:51 UTC

While the retry circuit remained paused, the host continued running Go tests
under `/home/xgenstack.com/backend`. Dux recorded a second kernel fanotify
overflow, and the persisted dirty reason became `fanotify queue overflow`. At
18:51 UTC the daemon was active with zero restarts, **65,162 pending updates**,
**803,677 events seen / 491,894 resolved**, **3,009 updates committed**, zero
userspace drops, and a **128 MiB WAL**. This independently confirms the live
index cannot currently be trusted, even outside a reconciliation attempt.

The service also logged `rollup_reconcile` flush phases of about **50 seconds
for 52 items** and **65 seconds for 122 items** during the same workload. A
read-only query-plan check confirmed the rollup aggregate uses the expected
parent and inode indexes; representative samples for four high-fanout
directories (12,500–28,831 children) took **9–22 ms each**. That isolated query
does not explain the end-to-end flush latency; transaction-level profiling
under a controlled workload is still needed before changing the accounting
algorithm.

At **18:55 UTC**, a fresh `go test ./... -count=1` run had started under the
same external backend tree before a quiet scan window could be established.
Dux remained active with zero restarts and userspace drops, but now had
**70,404 pending updates**, **886,719 events seen / 501,599 resolved**, three
kernel overflow notifications, `dirty_reason=fanotify queue overflow`, and a
128 MiB WAL. The transient 30-second observation window therefore did not
qualify as quiet; no additional reconciliation was started.

### Stale events continue from live web containers — 2026-11-04 18:57 UTC

The Go test process had exited, but a new stale-handle event was attributed to
host PID **1282**, verified as an `nginx` worker in `nginx.service`; Docker also
reported active XGenStack WordPress, cron, Redis, and database containers. Thus
the event churn is not limited to test builds, and pausing tests alone cannot
establish a clean interval. At 18:57 UTC the daemon remained active with zero
restarts, **76,205 pending updates**, **900,338 events seen / 511,084 resolved**,
three kernel overflow notifications, zero userspace drops, and a 128 MiB WAL.
The live index remained dirty. Any clean live reconciliation now requires a
maintenance window for the relevant filesystem-writing web/container
workloads; no such workloads were stopped.

### Revision 66 rollup performance fix and live verification — 2026-11-04 19:34 UTC

The production flush traces identified a repeated full-index scan in
`reconcile_affected_rollups`: for every affected directory, the leaf repair
used a correlated `EXISTS` plan that scanned `idx_inodes_lfiles` across the
whole file table. The live index contains about **1.22 million file inodes**;
one observed old-code rollup phase took **70.6 seconds for 79 affected items**.
Replaced that predicate with a parent-index-driven `(dev_id,inode) IN (...)`
update. The resulting plan uses `idx_dirents_parent` and inode primary-key
lookups and no longer scans all file inodes. On the live DB, a read-only
equivalent for a directory with **28,831 children** took **24.27 ms**, versus
**612.18 ms** for the previous full-scan plan (about **25x faster**, same
zero-stale-leaf result).

The source change is pushed as **`e0cc915f8f37da61aa6a737052403a8d6d740419`**.
All **258 Rust tests** passed, including the query-plan and stale-leaf
correctness tests; Clippy with `-D warnings`, 54 package-lifecycle tests, 12
release CLI tests, formatting, and `git diff --check` passed. Exact-source
static package **0.6.3-66** was built under a 20% CPU / 4 GiB memory cap. Its
manifest reports source SHA-256
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`; the
static binary SHA-256 is
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`. The
Build-ID matched debug file, DEB/RPM contents, artifact checksums, candidate
CLI against live and backup DBs, `systemd-analyze verify`, and offline unit
security check passed (exposure **3.3, OK**).

Before installing, saved the revision 65 binary and effective unit properties
at `target/readiness/production-prerev66-20261104/` and created online v6
snapshot `/var/backups/dux-index-preupgrade-rev66-20261104.sqlite`: **527,355,904
bytes**, mode 0600, schema 6, `quick_check=ok`, SHA-256
`49c14e3b08eaee54a582a06579d1d890f230912c31c40c7afd2d54d4924dfcc2`. Package
0.6.3-66 is installed and running as PID **423219**, with zero restarts.

The revision 66 manual scan (19:26:47–19:29:27 UTC) and its one bounded
automatic retry (19:30:27–19:32:56 UTC) each completed with **zero walker
coverage errors and zero kernel overflow notifications**. Both still observed
unresolvable `ESTALE` fanotify handles during active Go tests and container
workloads, so the daemon correctly kept the index dirty and paused retries
after two attempts. WAL temporarily reached about **299 MiB** on the retry and
checkpointed back to **128 MiB**. Since the new binary started, event replay
committed **10,112 updates in 45 seconds** and then **16,384 in the next 22
seconds**; a 128-update flush completed in about **1.77 seconds** (another in
**0.33 seconds**). This is a substantial live catch-up improvement over the
previous 70.6-second rollup phase. At **19:34 UTC** the daemon remained active
with zero restarts, zero user-space drops, zero kernel overflows since upgrade,
about **37,834 pending updates**, a 128 MiB WAL, and `dirty=true` because event
continuity still cannot be proven. The performance fix is verified and
deployed; clean production convergence remains unqualified until a quiet
maintenance window permits a full scan and clean caught-up verification.

### Readiness continuation: dependency audit, disk, and live daemon — 2026-11-04 19:39 UTC

A fresh `cargo audit` scanned the current `Cargo.lock` (**229 crate dependencies**)
against the refreshed RustSec advisory database and exited successfully with no
vulnerability findings. The initial attempted `cargo audit --locked` was rejected
because this cargo-audit version does not accept `--locked`; the actual audit ran
without that unsupported option. The Go test process seen in an earlier process
sample has exited; no Cargo test or VM qualification process was running during
this check.

At **19:38:38 UTC**, the installed revision 66 daemon remained active as PID
**423219**, with **zero restarts**, no write error, and a **128 MiB WAL**. It had
seen **672,114 events**, resolved **468,243**, committed **107,355 updates**, and
had **26 pending updates**, zero user-space drops, and zero kernel overflow
notifications since installation. Despite the small pending count, it remained
about **738 seconds behind**, `caught_up=false`, and `dirty=true`; its dirty reason
is incomplete reconciliation after two failed full scans, with automatic retries
paused. The low pending count therefore does not establish an exact index. The
last full scan was 491 seconds old, and the active Nginx/web-container workload
continues to produce unresolved stale-handle diagnostics (29 matching log lines
in the preceding ten minutes). No service or container was stopped and no new
scan was started.

The root filesystem was **61% used with 71 GiB available**. `target/readiness`
occupies **6.4 GiB** of retained package, rollback, and qualification evidence;
it contains release builds and interrupted/failing test reports, so it was not
deleted as temporary cache. The repository remained clean and synchronized with
`origin/hardening/audit-remediation` at commit **749688d** before this log update.
Production convergence and host-load overflow/endurance qualification remain
open; exact current-release fault/scale coverage, package lifecycle on every
supported distribution, long soak, and monitored canary are not proven by the
completed unit, CLI, lifecycle, scan-plan, and audit checks.


### Exact revision 66 workload qualification continuation — 2026-11-04 20:07 UTC

The installed production binary SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060` also passed three isolated Ubuntu 24.04 guest suites on the exact binary. The guest used kernel 6.8.0-147-generic and ext4, with strict CPU and memory limits.

- Live fanotify churn suite: **passed**, 1,200 file replacements plus 1,500 concurrent nested directory swaps and descendant writes, including inode reuse, hardlinks, subtree deletion, Go-build-shaped bursts, WAL/SHM cycles, exact path/inode accounting and clean daemon exit. Elapsed 216.129 seconds. Report SHA-256 `729de65c60b6be13d902df876560f5fbd62187eb895104a68b43d9badf6cd51f` (full report retained at `target/readiness/qualification-rev66-live-20261104/report.json`). The test log includes transient ESTALE attributes and an inode-incarnation conflict; final reconciliation converged and exactness checks passed.
- Real kernel fanotify queue overflow suite: **passed**, confirmed kernel queue overflow, preserved uncertainty when the intentional dirty-marker write failed, then resumed backlog processing and completed reconciliation/accounting verification. 18,432-file workload; recovery 41.084 seconds; suite 340.229 seconds. Report SHA-256 `459e705e7156bf85420187db41e999255a02d784196086eac2d7c74b17ec300b` and runner log SHA-256 `cb624c0a7fc51c3fa2324e6fad9389e238fc2ed654824d03c9f7d6e48836924a` (full artifacts retained under `target/readiness/qualification-rev66-overflow-20261104/`). This qualifies that bounded scenario only.
- CPU burst suite: **passed**, 12,123 events produced in 6.018 seconds, catch-up in 57.456 seconds, exact validation in 6.641 seconds; daemon event CPU fraction 0.102 under the 0.12 limit. Report SHA-256 `e3e4a79271130be8d4d10b7296718ce6da91d877ec4a124c8e807dbfcb1a2b74`, harness SHA-256 `3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`.
- Capability suite: **incomplete**. The host-pressure guard stopped the disposable guest at 20:03:19 UTC while CPU PSI some was 59.83% and load1 15.07. Its report remained `running`; this is neither a pass nor a product failure. Partial report SHA-256 `7e707c5cfc71d9c410e5fc3004a915bd93b78639bfb66c1dbe2bc69ad1e9e4c1`.

The ephemeral guest overlay was removed after recovering the partial report; the VM base image and qualification evidence remain. At 20:07 UTC load1 was 4.35, CPU PSI some avg10 1.86%, and root had 69 GiB available. Production Dux PID 423219 remained active with zero restarts; its journal continued to show `ESTALE` fanotify resolutions and reconciliation activity, so its live index is still dirty/not caught up and this host remains unqualified for exact production results. No production daemon or container was stopped.

These suites do not establish power/media-loss behavior, alternate filesystems or kernels, 100k/1m/10m scale, all package upgrade/rollback combinations, interrupted migration, prolonged WAL/endurance behavior, a 72-hour soak, or a monitored 24-hour canary. Capability tests are still outstanding.


### Host-pressure and live-event diagnosis — 2026-11-04 20:13 UTC

A second attempt to boot an isolated Ubuntu 24.04 guest used the 2 GiB profile and 15% host CPU quota. It had not reached SSH readiness and ran no qualification cases. Host CPU PSI some briefly reached **59.73% avg10** while the guest was still booting; I stopped and removed the disposable overlay. At **20:13:34 UTC**, CPU PSI some avg10 had fallen to **8.88%**, load1 was **4.68**, and the guest unit was confirmed inactive. The capability suite remains unrun.

Read-only attribution of recent production fanotify diagnostics found repeated unresolved `FAN_ATTRIB` (`mask=0x4`, target-only FID) events whose reported event PIDs **1892** and **1968** are the `containerd-shim` processes for the live `xgs-wp-34ae97a3` WordPress and `xgs-redis-34ae97a3` containers. This establishes that at least some stale-handle notifications coincide with container runtime activity; it does not prove that these account for all scan coverage failures. No container or production service was stopped.

At **20:13:34 UTC**, production Dux remained PID **423219**, active with zero restarts. Read-only status reported **1,490,117 events seen**, **1,075,675 resolved**, **348,711 committed updates**, **64 pending**, zero reported drops or kernel overflows, a **128 MiB WAL**, and no write error. The index remains dirty and not caught up, **2,835 seconds behind**, with automatic retries paused after two incomplete full scans; the last scan was **2,587 seconds old**. Low pending updates and zero recorded overflows do not establish index correctness. Disk remained **62% used / 68 GiB free**.

The scan walk itself did complete with zero coverage issues. The production blocker is event-stream uncertainty: both reconciliations were rejected with `scan_errors=0`, `coverage_incomplete=false`, `resolve_warned=false`, and `lost_events=true`. The first scan also reached a **280.9 MiB WAL** where a checkpoint could not advance; the second reached **299.3 MiB**. Event writes resumed after the WAL checkpoint recovered to 128 MiB, but uncertainty remained. Thus it is inaccurate to describe these as walker coverage failures.

A current repeated event shape is `FAN_ATTRIB` (`mask=0x4`) with a target-only FID (`info_type_mask=0x2`) whose `open_by_handle_at` resolution returns `ESTALE`. Linux's [`fanotify(7)` documentation](https://man7.org/linux/man-pages/man7/fanotify.7.html) explicitly handles `ESTALE` when an event object's file handle became invalid because the object was deleted before the listener opened it. That establishes a plausible benign delete race for this event shape, but not that every unresolved event is benign or that this daemon can safely discard it on every filesystem. The parser currently treats an unresolved identity as lost events (`src/watch.rs`, `event_identity_unresolved` handling), so the next correctness change should distinguish a proven-deleted target on a filesystem confirmed to support handle lookup from unsupported filesystems and other resolution failures. No such suppression has been implemented yet.

A production scan was considered at 20:17 UTC but not triggered: host load1 was **20.51** on 12 CPUs. A read-only process sample showed a concurrent Go race-test build (`go test -race ./internal/service -run TestPluginVulns -count=1`) consuming substantial CPU. No process was stopped. The pre-upgrade DB backup remains present, mode 0600, and its SHA-256 still matches `49c14e3b08eaee54a582a06579d1d890f230912c31c40c7afd2d54d4924dfcc2`. The root filesystem had **71 GiB available**.


### Stale target-only FAN_ATTRIB handling — candidate change, 2026-11-04 20:30 UTC

The event parser now retains the `errno` class from `open_by_handle_at` and tracks filesystem IDs on which at least one handle has opened successfully. It ignores only a target-only `FAN_ATTRIB` whose FID returns `ESTALE` on one of those already-proven handle-capable filesystems. An unknown filesystem, a named event, other event masks, and all non-`ESTALE` handle failures still mark the stream uncertain. This is limited to the benign delete race documented by [`fanotify(7)`](https://man7.org/linux/man-pages/man7/fanotify.7.html); it does not weaken handling of unsupported filesystems or general unresolved events.

The new regression test verifies that the exception is restricted to the exact event shape, a known-good filesystem ID, and `ESTALE`; other errors and masks remain non-benign. All **259 Rust tests** passed (including the new test), Clippy with `-D warnings`, formatting, and `git diff --check` passed. A release build completed from this source; candidate SHA-256 is `1b27162caa8f26e8701fa032a6269e255b2305a8a2cd4317497ba09adc136b9a`.

This change is **not deployed**. The release candidate has not passed the exact-binary live fanotify churn test yet. The disposable Ubuntu guest was stopped by its pressure guard at **20:29:17 UTC** before artifact transfer or test execution: CPU PSI some avg10 **70.73%**, load1 **13.31**. Its ephemeral overlay was cleaned. Production remains on binary SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`, PID **423219**, zero restarts. At **20:30:18 UTC** the production index was dirty/not caught up, **6,528 pending**, about **3,838 seconds behind**, with retries paused; host load1 was **20.67** and CPU PSI some avg10 **65.66%**. Disk had **67 GiB available**.


### Rev 67 live fanotify attempt and host-isolation finding — 2026-11-04 20:39 UTC

Rebuilt the release candidate from clean commit **690507c74bb2083d1acf7e0e9b1fe0c8dc865309**. Candidate SHA-256 is `188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`; `dux --version` reports source SHA-256 `7e6fc87327babc862ebd5a5e15eecf8fcf3b710112af6f9158045c21a8a511f5`. In the marked Ubuntu 24.04 guest, the exact binary's live fanotify harness printed all 15 PASS checkpoints, including exact convergence checks, **1,200 replacements**, **1,500 concurrent nested-directory swaps/descendant writes**, and TUI exit. However, the final JSON report was not persisted and the process did not provide a clean completion status: the pressure guard stopped QEMU at **20:37:06 UTC** when host load1 reached **12.16**. Treat this test run as **incomplete**, not as a recorded suite pass. Partial run artifacts are retained at `target/readiness/qualification-rev67-live-20261104/`; observed-output note SHA-256 `de51afea437c4c75f9daf6f00429da6e1e7f1adabdd7f6c4e7ed2f92e0c132ce`, guest console SHA-256 `bce7b6e88731bc56625f6c76c9b9ecc8371fcd21027d108c5e6f685e8a623fa3`, pressure record SHA-256 `fbdfcfd695f731ae43aa54c92f3b01cfcbf041b316730de49265a0f5c378d04d`.

Production status before this attempt reported zero kernel overflows. At **20:39:18 UTC**, the daemon remained active with zero restarts but reported `dirty_reason=fanotify queue overflow`, **17** kernel overflow notifications, **49** pending updates, and about **4,379 seconds** behind. During this interval, the disposable QEMU overlay was stored under the host's shared `/dev/shm/dux-qualification-vm/...` path, which is in production Dux's watched tree, and concurrent host Go tests were active. The timing does not establish which workload caused the overflows, but it exposes a qualification-isolation gap: a guest's “ephemeral” disk still generates host filesystem activity. No further heavy guest run should use this shared watched path until the runner is changed to isolate its writable disk from the host watcher or the workload is run elsewhere. The temporary overlay and transfer archive have been cleaned; the Ubuntu base image remains. Production stays on the prior rev66 binary.


### Rev 67 isolated-overlay live fanotify qualification — 2026-11-04 20:51 UTC

The qualification VM runner now creates its disposable qcow2 overlay inside a
private systemd mount namespace backed by a tmpfs bounded to the guest's memory
cgroup limit. The writable guest disk is no longer visible under the host's
watched `/dev/shm` tree. Only small control files remain in the shared runtime
directory. The host-side runner tests cover shell-safe argument handling and
the private tmpfs properties; **31 Python tests** and `py_compile` pass. A live
transient-systemd probe confirmed that the tmpfs and qcow2 overlay disappear
from the host namespace after unit teardown.

The exact rev 67 binary (SHA-256
`188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`) then
completed the Ubuntu 24.04/ext4 live fanotify regression with a clean exit in
**76.76 seconds** under a 50% CPU quota. It passed exact path/inode/allocation
and recursive-rollup checks; 1,200 replacement/move/hardlink/path-reuse
operations; 1,500 concurrent nested-directory swaps and descendant writes;
Go-build-shaped churn creating 5,764 inodes; 32 SQLite WAL/SHM cycles; and
TUI quit in 5.828 ms. The report records zero rollup mismatches, zero stale
build dirents, no dirty/flush-error/uncertain state, and no error. Report
SHA-256 `89fe47ed9bb56778ba868f32399bca73bbef8bf1991e30daab71b5f83e6a6429` is
retained at
`target/readiness/qualification-rev67-private-live-20261104/report.json`;
the runner output is retained beside it. This is a pass for this bounded live
fanotify workload, not a full production qualification.

The production service was not changed or stopped. At **20:50 UTC** PID
**423219** remained active with zero restarts. Read-only status still reports
`dirty=true`, `caught_up=false`, `dirty_reason=fanotify queue overflow`, **17**
kernel overflow notifications, **9** pending updates, and about **5,059
seconds** behind; no scan was in progress (`paused=false`,
`pause_reason=null` in that status snapshot). The candidate fix is not deployed, and the
installed v6 daemon's index remains a live correctness blocker. Deployment
requires an approved maintenance window, backup/rollback, successful full
reconciliation, and exact clean caught-up verification. Scale qualification,
capability suite, package/upgrade rollback matrix, prolonged endurance, 72-hour
soak, and monitored 24-hour canary remain open.

### Capability suite admission stopped by shared-host pressure — 2026-11-04 20:55 UTC

A retry of the candidate's unrun capability-loss/recovery suite booted a fresh
Ubuntu 24.04 guest with its qcow2 overlay isolated in the private mount-namespace
tmpfs. SSH became available, but the host-pressure guard stopped the guest at
**20:54:56 UTC**, before artifact transfer and before any test case ran. The
recorded sample was CPU PSI some **74.19%**, CPU PSI full **0.00%**, I/O PSI
some **0.58%**, and load1 **13.86**. The guard operated as configured; the
attempt is **incomplete**, not a candidate failure or pass. A high-CPU Go
compile and Go tests under `/home/xgenstack.com/backend` were active in the
same window, but temporal overlap does not prove they caused the pressure.

The attempt record and guard log, including their hashes, are retained in
`target/readiness/qualification-rev67-capability-20261104/`. The guest was
stopped and its private overlay cleaned. No Dux production process, database,
or index was changed. Do not retry this CPU-intensive guest suite until host
CPU pressure returns below the qualification admission limits; capability
coverage remains open.

### Continuing production overflow and unresolved-event shape — 2026-11-04 20:59 UTC

After the capability VM had been stopped, production status continued to change.
A read-only snapshot recorded **28** kernel overflow notifications (up from 17
at 20:52 UTC), `dirty=true`, `caught_up=false`, and **5,574 seconds** behind;
the pending count had drained to **14** at this particular sample. No full scan
was in progress, and the service remained unchanged. This confirms a persistent
live convergence problem after the isolated guest was gone; it does not assign
cause to any one host workload.

The preceding 15-minute journal window contained **390** unresolved fanotify
events: **333** were target-only `FAN_ATTRIB` (`mask=0x4`, `info_type_mask=0x2`,
`ESTALE`), the exact narrow shape the candidate handles on a proven
handle-capable filesystem. The other **57** included named/delete/create and
other masks and remain unresolved by design; there is not enough evidence to
safely suppress them. This suggests the candidate fix addresses the dominant
stale-handle warning class but cannot alone be assumed to clear all uncertainty
or prevent kernel queue overflow under this production workload. The complete
status/event-shape snapshot is retained at
`target/readiness/production-fanotify-status-20261104T2058Z.json` (SHA-256
`23964d72b9ad4854f30ccffbb385fd0d3e8f175fb01d81c6e49900455aec0d6b`).

The candidate remains undeployed. A clean full reconciliation on a qualified
binary, exact post-scan filesystem comparison, and evidence that the remaining
unresolved named-event cases are correctly handled are still prerequisites for
calling the production index trustworthy.

### Rev 67 capability-loss and recovery qualification — 2026-11-04 21:03 UTC

After host pressure returned below admission limits, the exact rev 67 candidate
passed the isolated capability suite in a marked Ubuntu 24.04 KVM guest (2 GiB
RAM; QEMU CPU quota 25%). The suite verified that the daemon refuses to start
without `CAP_SYS_ADMIN`, exposes uncertainty when `CAP_DAC_READ_SEARCH` is
missing, keeps that known failure visible across a rebuild, and converges with
exact filesystem state after capabilities are restored. Suite time was **2.227
seconds**; worker CPU quota was 25%, with a 4 GiB worker memory ceiling. Binary
SHA-256 is `188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`;
harness SHA-256 is `3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`.
The final report is
`target/readiness/qualification-rev67-capability-20261104/report.json` (SHA-256
`7400937aeb5b8223dd9885a19d8ec1b1d911ba0faaf8dbc7ba6c83e5b48a0a34`), with
runner output alongside it. This closes only the bounded capability suite for
this exact binary; the other 14 readiness suites listed in that report remain
unrun in that run.

The VM was stopped and its ephemeral runtime cleaned. The production daemon
remained PID **423219**, active with zero restarts and unchanged on the prior
binary. A post-run read-only status at **21:03 UTC** still reported
`dirty=true`, `caught_up=false`, 28 kernel overflow notifications, 15 pending
updates, and about **5,799 seconds** behind. No production scan or deployment
was performed; exact live convergence remains a blocker.

### Rev 67 real kernel overflow and reconciliation qualification — 2026-11-04 21:09 UTC

The exact rev 67 candidate passed a real kernel fanotify-overflow suite in the
isolated Ubuntu 24.04/ext4 guest under a **25% CPU quota**. With the guest
queue capacity set to **16,384**, the harness stopped the daemon and created
**18,432** files, then resumed it and verified that overflow uncertainty stayed
visible even when the dirty-marker metadata write was deliberately forced to
fail. After removing the fault, the daemon reconciled the persisted backlog,
completed a full scan, verified accounting, and returned to caught-up state.
Recovery took **33.803 seconds**; the suite took **113.179 seconds**. This is
bounded scenario evidence, not proof of long-term overflow immunity or live-host
convergence. Binary SHA-256 is
`188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`; harness
SHA-256 is `3ebf425f55835f7e0a060b7296718ce6da91d877ec4a124c8e807dbfcb1a2b74`.
Report SHA-256 `069aefcaa18b236677df9034342e4f64c375330d36fc21006438bb665c61a597`
is retained at
`target/readiness/qualification-rev67-overflow-20261104/report.json`, with
runner output beside it.

The ephemeral VM and private overlay were stopped and cleaned. The production
service remained PID **423219** with zero restarts and was not modified. At
**21:08:50 UTC**, production status remained dirty/not caught up, with **28**
kernel overflow notifications, **8** pending updates, and about **6,150 seconds**
behind; no reconciliation scan was in progress. Root had **65 GiB available**.
The candidate remains undeployed, and exact live convergence is still a release
blocker.

### Rust dependency advisory refresh — 2026-11-04 21:12 UTC

Refreshed the RustSec advisory database and ran `cargo audit` with cargo-audit
**0.22.2** against the current `Cargo.lock` (**229 dependencies**). The command
exited **0** with no lockfile vulnerability findings. This is dependency
advisory evidence only; it does not qualify the current executable, distro
packages, runtime configuration, or the remaining security and release gates.

### CPU, coverage, mount, and WAL suite attempt stopped during guest boot — 2026-11-04 21:15 UTC

A fresh 2 GiB Ubuntu 24.04 guest was admitted at 21:13:44 UTC for the exact
rev 67 CPU, coverage, mount-recovery, and reader-held-WAL suites. The host
pressure guard stopped it at **21:14:43 UTC**, before SSH became available and
before any test ran. The guard sample was CPU PSI some **57.97%**, load1
**11.77**, CPU PSI full **0.00%**, and I/O PSI some **0.70%**. The attempt is
**incomplete**, not a product result. Go test processes were active in nearby
samples, but their contribution is unproven. The guest and its private overlay
were cleaned; production Dux was not changed.

The attempt manifest and guard log are retained under
`target/readiness/qualification-rev67-cpu-coverage-mount-wal-20261104/`; the
manifest SHA-256 is
`439a9c0337d3edc26b02328820194953f35c420848366f808ab1ec3952d601f6`.
Those four exact-candidate suites remain open until a run completes under
stable host pressure.

### Production overflow burst persists without a qualification VM — 2026-11-04 21:17 UTC

The latest guest had been stopped by its guard at **21:14:43 UTC** before running
any test. The production journal then recorded **21 explicit fanotify queue
overflow warnings** between **21:15:46.643** and **21:16:23.070 UTC** (36.4
seconds). A read-only status at **21:17:24 UTC** still showed `dirty=true`,
`caught_up=false`, **49** kernel overflow notifications, **25** pending updates,
and about **6,665 seconds** behind; no scan was in progress. Host I/O PSI some
was **16.81% avg10** and full was **15.65% avg10**. Independent Go build/test
workers were active in nearby process snapshots. They may contribute to host
pressure, but the event source and causal attribution remain unproven. The
burst occurred while no qualification VM was active, so the isolated guest is
not needed to explain this production incident.

The exact status, pressure text, and journal warning boundaries are preserved
in `target/readiness/production-fanotify-burst-20261104T2116Z.json`, SHA-256
`678663017a4dbf4958aa12ab479e1c7129c61eea9ea073c4c5ce8df541734d04`. Do not
start a production reconciliation while this event burst and elevated I/O
pressure continue. Production remains on its prior binary; exact convergence
and controlled recovery are unresolved release blockers.

### Rev 67 CPU, coverage, mount, and WAL qualification — 2026-11-04 21:23 UTC

The exact rev 67 candidate passed four additional readiness suites in a marked
Ubuntu 24.04 KVM guest using ext4, 2 GiB guest RAM, a 25% QEMU CPU quota, and a
4 GiB worker memory ceiling:

- CPU burst: **12,123** fanotify events; production in **6.965 s**, catch-up in
  **22.399 s**, exact validation in **3.705 s**, measured daemon CPU fraction
  **0.104** against the 0.12 limit.
- Coverage: an unreadable subtree produced explicit incomplete-coverage state.
- Mount lifecycle: newly mounted filesystems were detected and watched; removal
  triggered recovery and exact validation.
- WAL: a pinned reader retained a consistent snapshot; the WAL peaked at
  **1,108,312 bytes** and reclaimed to **0 bytes** after reader release.

All four suites passed; their combined readiness worker completed in **132.293
seconds**. Binary SHA-256 is
`188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`; harness
SHA-256 is `3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`.
The report is
`target/readiness/qualification-rev67-cpu-coverage-mount-wal-20261104/report.json`
(SHA-256 `6abc57828d73b5dfeef78ad6e4dd6e3dfa2813b4e245ddf2c37ee2937f5b1229`),
with runner output beside it. Crash, disk, OOM, pressure, retry, root-scan, churn,
startup, and soak suites were not part of this run.

The guest was stopped and its overlay cleaned. The production service remained
active with zero restarts and was not modified. Post-run status remained dirty
and not caught up, with **49** recorded kernel overflow notifications, **2**
pending updates, and about **6,998 seconds** behind; no full reconciliation was
in progress. These isolated passes do not establish exact production state.

### Production backlog and unresolved fanotify identities continue — 2026-11-04 21:33 UTC

A fresh read-only sample at **21:33:15 UTC** showed production Dux PID **423219**
active with zero restarts. The index remained dirty from fanotify queue overflow
and not caught up, about **7,616 seconds** behind, with **3,132** pending updates,
**49** kernel overflow notifications, a **128 MiB WAL**, and no full scan in
progress. The legacy v1 status output still omits the retry-circuit fields added
to the candidate; production remains on the prior executable. During the
preceding ~82 seconds, the journal contained **43** unresolved fanotify identity
warnings (mostly `ESTALE` for target handles) and **zero** new explicit queue
overflow warnings. Host CPU PSI some was **37.82% avg10** and I/O PSI some was
**1.21% avg10**. A separate Go/Postgres test workload was active on the host;
the evidence does not establish that workload as the cause of the lost events
or prior queue overflows. No production scan or restart was initiated.

The exact status, systemd counters, pressure values, and bounded journal counts
are captured in
`target/readiness/production-status-20261104T2132Z.json` (SHA-256
`3f27c85c6cfbab366609ddb92c4054318714a8d735674d6c8976ea4eb360936a`). Exact
production convergence remains unverified and blocked by the dirty event stream,
open reconciliation retry circuit, and elevated host CPU pressure.

### Full Rust suite attempt stopped at host-pressure guard — 2026-11-04 21:40 UTC

The all-feature Rust unit suite was started for source commit
`cbdeeb9be61699bdb0869f66e3f5c49e25aaafba`, with one Cargo build job, one test
thread, `CPUQuota=25%`, `MemoryMax=2G`, swap disabled, and both Cargo output and
test temporary files directed to `/dev/shm` (tmpfs, excluded from Dux's watched
real filesystems). Compilation began, but no test executable ran. At
**21:40:35 UTC**, the host measured load1 **14.84** and CPU PSI some **45.21%
avg10**; the test scope was stopped immediately as a conservative response to
that first observed sample. CPU PSI full was **0%**, I/O PSI some **0.80%**, and
memory PSI was **0%**. A separate Go certification build/test remained active
and was not interrupted. The stopped scope had reached only **287 MiB** peak
memory; this is not a Rust suite result.

The task-owned partial tmpfs target was **51 MiB** and was removed after scope
termination. Production Dux remained active; the post-stop sample still showed
dirty/not caught up, **8** pending updates, **49** recorded kernel overflows,
and no scan in progress. The incomplete attempt record is
`target/readiness/qualification-rust-suite-20261104/attempt.json` (SHA-256
`130bd8bf638e1f738466162d78e3249a6c75b2177ab3851663fa76f92d34cc7b`). This
attempt must not be counted as a pass or failure. The previous targeted status
and malformed-retry tests remain the only tests run against the recent status
change; the full unit, package, release, and live-system gates remain open.

The retry at **21:43 UTC** again stopped before any test ran. At **21:45:05**,
load1 reached **12.49**, crossing the hard host stop threshold; CPU PSI some was
**39.03% avg10**, I/O PSI some **4.49%**, and the scope memory peak was **353
MiB**. The 2 GiB/25%-CPU scope was stopped and its task-owned tmpfs target
removed. A separate Go certification test remained active and was not
interrupted. Production status at **21:45:29** showed dirty/not caught up,
**19,416** pending updates, **49** recorded kernel overflow notifications, no
active scan, and no write error. The increased pending count is concurrent
host evidence; it does not establish which workload caused each event. The
second interrupted attempt is preserved in
`target/readiness/qualification-rust-suite-20261104/attempt-2.json` (SHA-256
`8b05495a5942389d4f3bac6a8f6bf8191968b1f2a7a586bb645ae840b60c89e7`). No full
Rust suite result has been produced by either attempt.

### Full default-feature Rust unit suite passed — 2026-11-04 21:48 UTC

After the unrelated workload pressure fell below admission limits, ran the
existing debug test executable built from code revision `b38f568` directly, so
the interrupted all-feature build was not repeated. All **259 tests passed**,
with **0 failures** and **0 ignored**, in **59.40 seconds**. The binary SHA-256
is `bc7f3be07cb844d752f3d93459f036aa31046b56bdd1b4aab68dfc1707b08ea0`. The
run used one test thread, a 25% CPU / 2 GiB / no-swap systemd scope, and a
task-owned `/dev/shm` TMPDIR; Dux explicitly excludes tmpfs from watched real
filesystems. The test fixture directory was removed after the run. The source
tree has no code changes since `b38f568`.

The summary report is
`target/readiness/rust-unit-20261104/report.json` (SHA-256
`0a1bdd6090e877b76c99aa5656556cc26195505668cf550931bd8eb4c0b51255`). This
closes the default-feature unit gate for that source revision only. The
all-feature unit suite is recorded below. Release builds, package/lifecycle
gates, full-scale gates, and live production recovery remain open. The
production overflow counter stayed at **71** in the sampled before,
during, and after statuses; pending updates rose from **33** to **2,511** while
other host workloads were active, so the source of that backlog is not
attributed to this test run.

### Installed and release executable identity recheck — 2026-11-04 21:53 UTC

Read-only `--version`, SHA-256, and mtime checks confirm that the live
`/usr/bin/dux` is still version **0.6.3**, embedded commit **`e0cc915f8f37`**,
binary SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`, and
reported source SHA-256
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`. The
existing `target/release/dux` is also version **0.6.3** but embeds commit
**`690507c74bb2`**, binary SHA-256
`188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`, and
reported source SHA-256
`7e6fc87327babc862ebd5a5e15eecf8fcf3b710112af6f9158045c21a8a511f5`. Both
predate the current `b38f568` status change. The 259-test result above applies
to debug test executable SHA-256
`bc7f3be07cb844d752f3d93459f036aa31046b56bdd1b4aab68dfc1707b08ea0`; it is
not a release candidate. Rebuild and requalify a source-bound release artifact
from the current code before any production promotion. No installed binary or
service was changed.

### Full all-feature Rust unit suite passed — 2026-11-04 21:56 UTC

Ran `cargo test --all-features --locked --bin dux -- --test-threads=1` for
worktree commit `61217b06c9db54843e3e372d325aa41d956799ba`, with the
`fault-injection` feature enabled. All **259 tests passed**, with **0 failures**
and **0 ignored**, in **62.42 seconds**. The test executable SHA-256 is
`e293a029260591b31c2829769e91243cca7b810c51decb5a234f48cd81039686`. The run
used one Cargo job/test thread, a 25% CPU / 2 GiB / no-swap cgroup, and
task-owned tmpfs test fixtures. The sampled memory peak was **626,249,728
bytes**; final transient cgroup accounting was unavailable after exit.

The complete log is
`target/readiness/allfeature-unit-20261104/run.log` (SHA-256
`b7ce11d2695ebf81a83e266784ec83762d33798a31c96afc391d374faa30dd0d`); its
report is `target/readiness/allfeature-unit-20261104/report.json` (SHA-256
`2d1bf15dc841ded5a3f1e383ed185cbd1d19ad633726e18e1426e22ff95870f1`). This
closes the all-feature unit gate for the tested source. It does not qualify a
release binary, packages, scale, endurance, or production recovery.

### Source-matched release build stopped by host guard — 2026-11-04 21:59 UTC

Preserved the old unqualified `target/release/dux` (commit `690507c74bb2`, SHA
`188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`) at
`target/readiness/release-690507c/dux` before attempting a locked release build
from source commit `61217b06c9db54843e3e372d325aa41d956799ba`. The build was
stopped during compilation at **21:59:02 UTC** when load1 reached **15.37** and
CPU PSI some reached **71.69% avg10**. It ran in a 25% CPU / 4 GiB / no-swap
scope; an external Go `./...` build was also active and was not interrupted.
No replacement release binary was produced: `target/release/dux` still hashes
to `188f09518527a38af7fac4fbd47220774f040b66a501e1154a1e62087252a5d1`.

The incomplete attempt report is
`target/readiness/release-build-20261104/attempt.json` (SHA-256
`132d520225ce247e825ea742ed39666951841a5d5d86a81e417621108e4a4f27`), with
the compiler output beside it. No installed binary, daemon, configuration, or
database was changed. A release build from the current source and all
release-specific qualification remain open.

### Source-matched release candidate built — 2026-11-04 22:06 UTC

The locked optimized production binary built successfully from source commit
`bd84d998029abfa99be3203a628884c4f6777cce` using
`cargo build --release --locked --bin dux`. Candidate
`target/release/dux` is 5,173,368 bytes with SHA-256
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`; its
embedded version reports source SHA-256
`8a430e8cc6c8fd3f3605b9921492f83caa7b9c765504863dfe85ab28910fd120`.
The build took 2m37s under a 25% CPU / 4 GiB / no-swap scope. Its log, binary
manifest, and read-only status smoke output are preserved in
`target/readiness/release-candidate-bd84d99/`.

The candidate's read-only status command succeeded and exposes the paused
reconciliation reason and operator recovery command. It also observed the live
index DIRTY with 15,642 pending updates and 86 kernel overflow notifications.
The installed `/usr/bin/dux` and production service were not changed. This is a
source-matched release build and smoke check only; exact-candidate VM and scale
qualification, package lifecycle/rollback, endurance, and canary remain open.
The observed production backlog and paused automatic reconciliation remain
release blockers; do not promote this candidate until safe recovery and the
required qualification gates pass.

### VM-manager and packaged syscall-policy checks — 2026-11-04 22:13 UTC

The updated qualification VM manager's focused test file passed **31/31** in
1.505 seconds inside a 10% CPU / 512 MiB / no-swap transient scope. The report
binds test-file SHA-256
`583a6d01d05c08b27f5e786fa4df6b6a996a5928d5e7c9b1c451a82940d3b93f` and is
`target/readiness/vm-manager-unit-20261104T2213Z/report.json` (SHA-256
`60e02b149f24402146885cce3c1ad052c36f37be873008d7611ddd902b1b0989`).

`systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service`
and `scripts/dux-systemd-filter-smoke.py` also passed. The transient smoke
confirmed the denied syscall groups return `ENOSYS`, `clock_gettime` remains
available, the indexer's FID call remains permitted, and the tunnel's FID call
is denied; both units enforce their configured address-family policy. Report
`target/readiness/systemd-security-20261104T2211Z/report.json` has SHA-256
`d6ddec0548a517c8b1d5f2d221c5bd52d975cd8dba57327b01a138d8c22f8a7b` and records
the effective live unit properties. These checks do not qualify package
installation, the release binary's runtime behavior, or the dirty production
index.

### Exact source-matched candidate CPU, coverage, mount, and WAL suites — 2026-11-04 22:19 UTC

Candidate binary SHA-256
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`
passed four isolated Ubuntu 24.04 KVM suites on ext4: CPU burst, incomplete
coverage reporting, mount lifecycle, and long-reader WAL reclamation. The
guest used 2 GiB RAM, a 25% QEMU CPU quota, and a private 1.5 GiB tmpfs workspace;
the worker used a 25% CPU quota and no production index or service. All four
suite statuses are `passed` in the hash-bound report.

- CPU burst: **12,124** events; **3.857 s** event production, **16.954 s**
  catch-up, **3.237 s** exact validation, and **0.104** measured CPU fraction.
- Coverage: an unreadable subtree was explicitly reported as incomplete.
- Mount lifecycle: new mount detection/watch and disappearing-mount recovery
  both passed.
- WAL: the reader retained a consistent snapshot; WAL peaked at **1,108,312
  bytes** and reclaimed to **0 bytes** after release.

The worker completed in **1m54s** and reported a **147.5 MiB** memory peak with
no swap. Report: `target/readiness/qualification-candidate-bd84d99-20261104/report.json`
(SHA-256 `679a5e4177127808bcd0bdafd83ed7162b5677e6e863ab27f2c2a38b1929f138`);
worker log: `worker.log` beside it (SHA-256
`420ff37b549c15647e7280de333235fd03ac6b9f3e0ddecee1caffa2d8639c5e`). The
readiness harness SHA-256 is
`3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`.
The guest was stopped and its private overlay removed after evidence capture.

This run does not cover crash, overflow, churn, disk-full, OOM, pressure,
capability loss, retry/circuit behavior, root scan, startup, scale budgets,
package lifecycle, endurance, or canary. The production index remains dirty
and automatic reconciliation remains paused; exact recovery and all release
gates remain open.

### Post-qualification production status — 2026-11-04 22:21 UTC

After the isolated candidate suites and guest shutdown, a read-only status
sample at **22:21:09 UTC** shows the production daemon still active with zero
systemd restarts. The index remains **DIRTY** from fanotify queue overflow,
not caught up, and about **10,489 seconds behind**. It reports **86** kernel
overflow notifications, **3** pending updates, no active scan, and a **128 MiB
WAL**. Automatic reconciliation remains paused after two failed full scans;
user-space drops remain zero. The near-empty pending counter does not prove
exactness: the index's last successful scan is still nearly three hours old.

The captured host CPU/I/O PSI some averages are **4.46% / 2.36%**. Root has
**73.6 GB free** (62% used); `/dev/shm` has **11.5 GB free**. The full
read-only snapshot is `target/readiness/production-status-20261104T2220Z.json`
(SHA-256 `b19d128a39caa44abece36bb00f343d5ac552656a6df3f0f7eac1d1e21c0857a`).
No production restart, scan, or index mutation occurred. Exact convergence and
safe recovery remain release blockers.

### Exact candidate fault, restart, and overflow qualification — 2026-11-04

The source-matched candidate was also exercised with a dedicated
fault-injection build. Production binary SHA-256 is
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`; the
fault-enabled binary SHA-256 is
`ce41649c0a97bb891e4011d8a056111b29a42208a4b3e3eb0848c3d5a2e5c9e7`. Both
embed commit `bd84d998029a` and the same source SHA-256
`8a430e8cc6c8fd3f3605b9921492f83caa7b9c765504863dfe85ab28910fd120`.

On a disposable Ubuntu 24.04 guest, capability loss, pressure handling,
retry-circuit behavior, and startup/schema-upgrade suites passed. They
verified bounded retries, preserved prior readable state on failed scans,
continued event ingestion while scans were suppressed, replayed queued events,
and repaired stale directory/file-to-directory transitions. The report is
`target/readiness/qualification-fault-candidate-bd84d99-20261104/capability-pressure-retry-startup-report.json`
(SHA-256
`3a4c196dce32f57a247e25658aa22c7bb62430ddace8fd7b95f3f34c7b52182e`).

The same exact candidate pair passed **30/30** crash-recovery rounds covering
flush and scan/install commit boundaries plus randomized crash timing. A
real kernel fanotify queue overflow at capacity **16,384** with **18,432**
created files was detected; dirty state persisted, reconciliation ran after
backlog handling, and the index returned to exact caught-up state in **32.718
seconds**. Report:
`target/readiness/qualification-fault-candidate-bd84d99-20261104/crash-overflow-report.json`
(SHA-256
`17e2e13c7efe93ece19acba40098d7376086d144b4adb00e074a5d17d375a7fc`).

An actual scoped cgroup OOM kill also passed: systemd restarted the candidate
once, and changes made during downtime were recovered exactly. Report:
`target/readiness/qualification-fault-candidate-bd84d99-20261104/oom-report.json`
(SHA-256
`3c83b5a89acb2fbfeb3971b51c184293b35dbe5593c1366dc6693968d3d4bec6`).

### Exact candidate disk exhaustion qualification — 2026-11-04 22:53 UTC

The release candidate passed the isolated disk suite using a disposable
Ubuntu 24.04 guest, 25% QEMU CPU quota, a 4 GiB guest, a private 3 GiB tmpfs
workspace, and a disposable ext4 loop image. The seeded tree contained
**100,000 files**. Low-disk pressure paused and resumed index writes; an
actual ENOSPC scan and a read-only filesystem scan both preserved the prior
readable index. The scan reserved **1,597,714,432 bytes** before work and
returned to exact caught-up state after recovery. Suite elapsed time was
**169.68 seconds**. Candidate SHA-256 is
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`; harness
SHA-256 is
`3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`.
Report:
`target/readiness/qualification-fault-candidate-bd84d99-20261104/disk-report.json`
(SHA-256
`2eea51c03a53a0f4e95a5cf32628dd74ba62b48e51574a8b3a4d122e77ed4247`).
The guest was stopped after report capture and its ephemeral overlay removed.

The disk report's `suites_not_run` list is scoped to that individual invocation;
it does not represent the separate passing suite reports above. These runs do
not establish multi-day endurance, 100k/1m/10m performance budgets, power-loss
or media-corruption safety, other filesystem/kernel combinations, package
upgrade/rollback, signing/dependency review, a dedicated soak, or a canary.
The production index remains dirty with automatic reconciliation paused, so
the release candidate is not approved for production promotion.

### Exact candidate root scan and host-pressure-limited churn attempt — 2026-11-04

The candidate passed a disposable Ubuntu 24.04 guest root scan in **74.189
seconds**, accounting for **69,600 files**, **9,225 directories**, and **85,329
directory entries**. The scan excluded its own database, stayed on one
filesystem, and reported complete coverage. Candidate SHA-256 is
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`; harness
SHA-256 is
`3ebf425f55835f7e0a0609afa90b2953fc8301b41afa43f44d97152227cf0628`. Report:
`target/readiness/qualification-fault-candidate-bd84d99-20261104/root-report.json`
(SHA-256
`1a41d192d3acd4d2d7ecc63bd1f9e908e7b8c414fabbcb33fa14379eabbade77`).

A follow-up production-shaped churn qualification seeded its 100,000-file
baseline and passed initial exact validation, but the host pressure guard
stopped the disposable guest before the first churn batch completed. At
**23:02:46 UTC**, host CPU PSI some avg10 was **67.32%**, load1 was **13.70**,
and I/O PSI full avg10 was **0.18%**; the guard stopped the VM because CPU PSI
and load exceeded its safety thresholds. The workspace and report were inside
the guest's ephemeral overlay and were discarded on stop, so this is recorded
as **interrupted / not qualified**, not a candidate failure or a pass. At
**23:03 UTC**, host CPU PSI some avg10 remained **68.30%** and load1 was
**17.99**; a retry is deferred until the host has recovered. The production
service and index were not touched. Churn remains an open qualification gate.


### Production status and host-pressure follow-up — 2026-11-04 23:04 UTC

A second read-only status sample confirms the installed binary is still SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`, running
as PID **423219** with zero systemd restarts. The production index remains
**DIRTY** from fanotify queue overflow, with **86** kernel overflow
notifications, **1,794** pending updates, no active scan, and about **13,065
seconds** behind. The automatic pause flag is clear in this sample, but the
index is still not caught up and its last scan is about **12,817 seconds** old.
The WAL is **128 MiB**. The exact status snapshot is
`target/readiness/production-status-20261104T2304Z.json` (SHA-256
`a3e235bc3c012f0f8f1d9f0bffc3621ce1e5e11d0e4563ac4924ef847e521a2d`).

At **23:03 UTC**, the process list showed active Go `vet` and compiler workers
under `/home/xgenstack.com/backend/agent` consuming multiple cores while host
CPU PSI some avg10 was **14.99%** and load1 **16.72**; Dux was using about
**10.8% CPU** in that sample. This is concurrent host workload evidence, not
proof that it is the only source of pressure. At the next sample, load1 was
**21.21** and CPU PSI some avg10 **36.58%**. Another CPU- or I/O-intensive
qualification run is unsafe until host pressure subsides. No production scan,
restart, or index mutation was performed.

### Churn qualification retry interrupted by host pressure — 2026-11-04 23:11 UTC

A second guarded attempt began at **23:05:25 UTC** after a short improvement
in host load. It again seeded 100,000 files, completed baseline and watcher
startup, passed initial exact validation, and performed the first 2,000 churn
mutations. The guest was stopped while that batch's exactness check was in
progress. At **23:11:45 UTC**, the pressure guard recorded CPU PSI some
avg10 **46.95%** and load1 **13.95**; active host compiler workers were
visible. The guest overlay was discarded before a batch result was published,
so this attempt is also **interrupted / not qualified**. Repeated guarded
stops show that the current host is not a reliable place to complete this
long CPU/I/O workload while other builds run. The guard remains enabled.

### Current Rust dependency audit and candidate binary inventory — 2026-11-04 23:15 UTC

`cargo audit --deny warnings --format json` passed against the refreshed
RustSec advisory database (commit `965bf9ad083b58e6340d65829d85217b18be9290`,
updated **2026-11-03 20:34 UTC**). It scanned **229** locked crate dependencies
and reported **0** vulnerabilities and **0** warnings. The JSON evidence is
`target/readiness/dependency-audit-20261104/cargo-audit.json` (SHA-256
`2f551d1ceee702ba99e3c6676401c46f981ac9a0e75de146ffdba01c5049fd9f`). This
covers the lockfile against the advisory snapshot; it does not cover native OS
libraries or prove what is present in a particular executable.

The exact source-matched test candidate
`target/release/dux` (SHA-256
`f494ad29cc708048700a7cec18f8c8dd25d1bef14537c8711b55138081d410d4`) was
also scanned with `cargo audit bin`. The tool explicitly warned that it was
not built with `cargo auditable` and recovered only **43** dependencies, so
that binary scan is **incomplete** and is not a release approval. The release
builder already requires pinned cargo-auditable **0.7.7** and verifies the
embedded `.dep-v0` section, but the most recent exact-source static package
evidence is for an earlier source revision. Rebuild and scan a current-source
static package before promotion. The binary scan log is
`target/readiness/dependency-audit-20261104/candidate-bin-scan.log` (SHA-256
`d7623fc14b413599139876d4546e3f06d232761157652da9b32589f6749b85f8`).

A read-only production snapshot captured at **23:15:11 UTC** shows the
installed binary SHA-256 remains
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060` (PID
**423219**, zero restarts). The live index is still **DIRTY** from fanotify
overflow, with **98** kernel overflow notifications, **114** pending updates,
no active scan, and **13,731 seconds** behind; its last successful scan is
**13,484 seconds** old. Host CPU PSI some avg10 was **65.06%** and load1
**20.72**. No production scan, restart, or index mutation occurred. Snapshot:
`target/readiness/production-status-20261104T2315Z.json` (SHA-256
`9e7116f9d26a37d6fb67eaaf114924ea121fecf0fd600edd44efbdab6c67961c`).

### Exact candidate churn progress and pressure-guard stop — 2026-11-04 23:28 UTC

A third guarded churn attempt passed exact validation for **2/10** batches,
covering **4,000/20,000** mutations on the **100,000-file** baseline. Batch 1
converged exactly in **61.474 seconds** (10,816 seen, 9,108 resolved, 1,828
committed); batch 2 converged exactly in **69.097 seconds** (18,075 seen,
14,822 resolved, 3,679 committed). Both checkpoints verified exact paths,
inodes, allocations, and rollups, with zero drops and no uncertainty. Progress
evidence is
`target/readiness/qualification-fault-candidate-bd84d99-20261104/churn-current-progress.json`
(SHA-256
`ef1a4de0cf33bee7083d560a031218c63bcc73de5fb5982db88fdddb38f4830c`). The
run report remained `running`; no final suite report was produced.

The host guard stopped the disposable guest at **23:28:26 UTC** during batch
3 after CPU PSI some avg10 reached **61.69%** and load1 reached **14.52**. The
guest overlay was removed on stop. Captured stop and progress evidence is
`target/readiness/qualification-fault-candidate-bd84d99-20261104/churn-guard-evidence-20261104T2329Z.json`
(SHA-256
`4f918e291f494b797c638093683ebee2358be401059d0987af837356ee37f554`). This
is partial passing progress, not a completed churn qualification.

At **23:29:22 UTC**, a read-only production sample still showed the installed
binary SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`, PID
**423219**, zero restarts, and a dirty index from fanotify overflow. The index
had **98** overflow notifications, **3,450** pending updates, no scan in
progress, and was **14,583 seconds** behind. Snapshot:
`target/readiness/production-status-20261104T2329Z.json` (SHA-256
`e0f45e1ad546314eb0db9c664ddce1a7bc08e29c4478b9e11b70d70ba77b43c8`). No
production scan, restart, or index mutation was performed.

### Release-builder regression suite and current production status — 2026-11-04 23:38 UTC

Reran `test_build_release_cli` and `test_build_release_debug_artifact` under a
transient scope capped at **25% CPU**, **1 GiB memory**, and `IOWeight=1`, with
temporary files in `/dev/shm`. All **13/13** tests passed in **12.902 seconds**.
They cover release argument/provenance rejection, refusing non-auditable
binaries, preserving previous artifacts after injected package/publication
failures, and Build-ID-matched debug-symbol packaging. Report:
`target/readiness/release-builder-tests-20261104/report.json` (SHA-256
`048e8d75eb6f571e3479fbb137bc84e5b3126dd51e1c624f7937e0f2462be2e0`); verbose
log:
`target/readiness/release-builder-tests-20261104/release-builder-tests.log`
(SHA-256
`c54669b1fda86926c3c1b9896bce7a06a2912d6d67049937bcac2cfb5e645a3a`). This
is packaging-harness coverage, not a current-source static package build or
installation/upgrade test.

A read-only production sample at **23:38:30 UTC** still showed the old
installed binary SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`, PID
**423219**, and zero systemd restarts. The index remained dirty from fanotify
overflow, with **112** kernel overflows, **17** pending updates, no active scan,
and **15,130 seconds** behind; the last scan is **14,883 seconds** old. Host
load1 was **12.16**. Snapshot:
`target/readiness/production-status-20261104T2338Z.json` (SHA-256
`cfa796d54141e0eca0a6aaaa1b400af562fbcebb8a0bfee0cf3c2b47fd97bb9e`). No
production scan, restart, or index mutation occurred. Churn and current-source
package promotion remain open.

### Release builder now enforces RustSec audits — 2026-11-04 23:45 UTC

The local release builder now requires `cargo-audit`, audits the locked
dependency graph with `--deny warnings`, audits the exact executable after
checking its embedded `.dep-v0` metadata, and refuses an incomplete or
warning-producing binary scan. It preserves the lockfile JSON report and
binary scan log in the release bundle, records the advisory database commit,
Cargo.lock hash, tool version, and audit outcomes in the build manifest, and
includes both reports in `SHA256SUMS`. README release requirements now
describe this local gate; it does not depend on GitHub Actions.

The package-builder CLI and debug-symbol suites passed **14/14** in **13.303
seconds** under a **25% CPU / 1 GiB / IOWeight=1** scope with temp storage in
`/dev/shm`. Coverage includes source/revision checks, vulnerable lockfile and
incomplete binary-scan rejection, failure-safe publication, report/manifest
binding, package payload parity, and Build-ID-bound debug symbols. The final
hash-bound report is
`target/readiness/release-builder-audit-gate-final-20261104/report.json`
(SHA-256
`1f5ed937e4be2596530b9692ff70127f097f7ede083d9ef50a744ed89f00fb11`); log:
`target/readiness/release-builder-audit-gate-final-20261104/tests.log` (SHA-256
`d85c73cb785812254eea66869bbeac9cf95c08ac3c75cbd7ed73ef7d471cc66f`). An
earlier run exposed a fake-Cargo fixture that did not forward the new audit
subcommand; the fixture was corrected and the final rerun passed. The initial
failure remains preserved with its report and log.

As a scanner-path check, `cargo audit bin` also scanned the historical rev64
release executable, recovered **202 dependencies**, and exited 0 without
warnings. This validates the scanner invocation against an auditable ELF; it is
not evidence for the current source revision or a substitute for rebuilding it.
The preserved log is
`target/readiness/dependency-audit-20261104/rev64-binary-audit.log` (SHA-256
`e34e28d4ce68dea4d26a92248c0bee3a622d76a92ac96ade1d6166c2d17f50f0`).

The native current-source static-musl package build has **not** yet been run
through the new real audit gate. The current test candidate remains
non-auditable, and its previous binary scan was incomplete; rebuild and audit
the current source before promotion. This change alters the source-tree
digest, so the prior candidate's provenance no longer matches HEAD.

A fresh read-only production sample at **23:45:48 UTC** still showed the old
installed binary, dirty index, **112** fanotify overflows, **22,002** pending
updates, no scan in progress, and **15,569 seconds** behind. Host load1 was
**18.06** and CPU PSI some avg10 **40.79%**, with external compiler workers
active. Snapshot:
`target/readiness/production-status-20261104T2345Z.json` (SHA-256
`3d5e0828b0e438c23f76c6fc9717f6dcd7f6c7a705ba4c107e9add7642cc7966`). No
production scan, restart, or index mutation was performed; churn and safe
live-index recovery remain open.

### Current-source qualification refresh and production-event impact — 2026-11-05 00:17 UTC

The installed production executable is now attributable to the clean rev66
release: `/usr/bin/dux` SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060` exactly
matches `target/readiness/release-current-20261104-rev66/build-manifest.json`.
That manifest binds it to commit `e0cc915f8f37da61aa6a737052403a8d6d740419`
and source-tree SHA-256
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`; all
rev66 bundle checksums pass. This resolves source identity for the running
binary, but the current source has since changed `src/query.rs` and
`src/watch.rs` by 292 lines, so rev66 fault and performance passes do not
qualify the current source.

On commit `fed53d741d72f0aaf6ae4f7afd8e310c71526f52`, the Rust all-target test
suite passed **259/259**, all Python script tests passed **264/264**, Clippy
passed with warnings denied, and `cargo fmt --all -- --check` passed. The first
Python discovery run exposed two stale assertions: one required the removed
GitHub Actions workflow, and one expected obsolete VM tmpfs wording. Both tests
were aligned with the current requested workflow state and VM implementation;
the complete rerun passed. Logs are retained under
`target/readiness/current-source-qual-20261105-277899d/`.

Static package revision **68** was built and audited before the test fixes were
committed. Its canonical source-tree digest
`9789658b23dd20e9654ca8f61158d7c5845aba668715c814a1ceeb3189dc2b48` matches
the source contents committed as `fed53d7` at build time. This qualification
entry changes the source tree again, so the artifact no longer matches current
HEAD. The manifest also records the earlier commit and `worktree_dirty=true`;
it is not a clean promotion artifact. The DEB/RPM,
static ELF, debug symbols, SHA256SUMS, and audit reports are retained at
`target/readiness/release-current-source-20261105-277899d/`. The lockfile scan
reported **0 vulnerabilities and 0 warnings across 229 dependencies**; the
exact executable scan recovered **202 dependencies** with no warning. The
ELF is static and contains `.dep-v0`; its debug sidecar Build ID matches. Its
12 KiB fixture scan completed without coverage issues; the separate 100,000-file
scan and `dux du` output matched GNU `du` exactly.

A **100,000-file / 100-directory** scan using that exact binary passed in
**3.249 seconds**, indexed **100,101 nodes**, and reported **409,600,000 bytes**,
matching GNU `du` byte-for-byte with zero coverage issues. The test wrote its
fixture on tmpfs, but the installed production watcher still observed its
create/delete event stream. The kernel overflow counter rose from **112** just
before the run to **121** at the first post-run sample and **132** during
cleanup; concurrent host Go test workloads were also active, so attribution is
temporal rather than isolated. Cleanup removed the full fixture and its
temporary database. At **00:17:23 UTC**, production remained active as PID
**423219** with zero restarts, but was dirty/not caught up from fanotify
overflow, with **132** overflow notifications, **5,425** pending updates,
**17,464 seconds** behind, no active scan, and no write error. No direct
production database write, scan, or service restart was issued. Do not run
high-churn qualification workloads on this production host again.

A clean package rebuild was started for the pushed commit but explicitly
stopped when host load reached **30.47** and CPU PSI some avg10 reached
**63.18%** while external Go race-test/compiler workers were active. No clean
revision-69 package was published. Package upgrade/rollback testing, current
source live churn and fault recovery, large-scale performance budgets,
endurance, and canary qualification remain open. Current status and hashes are
recorded in
`target/readiness/current-source-qual-20261105-fed53d7/qualification-report.json`
and `scale100k-result.json`.

### Effective systemd hardening matches the checked-in unit — 2026-11-05 00:30 UTC

The effective `/usr/lib/systemd/system/dux.service` is now byte-for-byte equal
to `packaging/dux.service`, both SHA-256
`dd34d3086af76117b18b06a23c5c70ace4353ff6a33ae9b3fa3c10f13c04d920`.
`systemd-analyze security dux.service` reports **3.3 / OK**. Effective
properties include `NoNewPrivileges=yes`, `LimitCORE=0`, locked root bits,
only `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH` in bounding/ambient sets,
`CPUQuota=100%`, `MemoryMax=4G`, and `PrivateMounts=no` (required for the
host-wide fanotify mark). This supersedes earlier unit-drift findings; it does
not establish that the running binary or index is qualified.

At **00:30:51 UTC**, production still ran rev66 commit `e0cc915f8f37`, with
zero restarts, dirty state from fanotify overflow, **133** overflow
notifications, **13,855 pending updates**, **18,272 seconds** behind, and no
active scan. Recent journal lines show repeated unresolved stale-handle
fanotify events and successful but slow **128-update** flushes. The falling
pending count shows ongoing progress, but the stale event resolutions and
dirty marker leave exactness unproven. The samples do not isolate the source
of each event. The read-only
unit and status evidence is
`target/readiness/unit-hardening-20261105/report.json` (SHA-256
`98aa1aa54a138b5bb3a4751438d0163be50c50bfccee4321fb6d674713cb1d79`). Clean
current-HEAD package build, package lifecycle, current-source live recovery,
scale, endurance, and canary gates remain outstanding.

### Current-source all-features test and live-host event pressure — 2026-11-05 00:36 UTC

On clean HEAD `4a1f4a400381cc9885027699bd1b81690f05fd63`, the Rust all-target
suite with every Cargo feature enabled passed **259/259** in 26.53 seconds
after a 1m40s build. It ran offline and serially (`cargo test --all-features
--locked --offline --all-targets -j1 -- --test-threads=1`) in a CPU-, memory-,
and I/O-limited transient systemd unit with a private `/run` tmpfs target and
`TMPDIR`. Peak memory was 997.1 MiB with no swap. The full log is
`target/readiness/current-source-qual-20261105-4a1f4a4/rust-all-features.log`
(SHA-256 `b8a4a5e39e72c98bb2001f35af7ea64e236a43a9dcf230c631fa504cad03fd79`).

The production service remained active as PID **423219**, with zero restarts
and no write error. A read-only status sample at 00:36:42 UTC still showed a
dirty index from fanotify overflow, **144** overflow notifications, **2,909**
pending updates, no scan in progress, and about **18,623 seconds** behind.
The overflow counter was **133** in the 00:33 host sample and **144** after
the test. An unrelated Xgenstack Go test was active throughout; logs include
events from host compiler/test processes and transient-test PIDs, so the
11-notification increase cannot be attributed to one workload from this
observation. The tests used private `/run` build and temp paths; no production
database write, scan, or service restart was issued. Do not run further
high-churn qualification workloads on this production host. The captured
status JSON is
`target/readiness/current-source-qual-20261105-4a1f4a4/production-status.json`
(SHA-256 `c2dfd477d3941050bf57ca0eb525a5f7644994bef039aa407b05e81d57961973`).

Root filesystem capacity remains adequate at **66 GiB free (63% used)**. The
historical `target/readiness` tree occupies **6.5 GiB** and includes release,
rollback, failure-reproduction, and qualification evidence; it has not been
purged wholesale because some of it is referenced by this audit. A current
clean-HEAD release package, package upgrade/rollback lifecycle, exact live
reconciliation, stable caught-up window, current-source live churn/fault
recovery, production-scale budgets, endurance, and canary gates remain open.

The refreshed RustSec lockfile audit passed at 00:39 UTC: **229 dependencies,
0 vulnerabilities, 0 warnings**, advisory DB commit
`965bf9ad083b58e6340d65829d85217b18be9290` (last updated 2026-11-03). The
exact running `/usr/bin/dux` binary also passed `cargo audit bin`, with
dependency metadata recovered for **202 dependencies** and no findings. Its
SHA-256 still matches the retained rev66 manifest:
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`. The
binary audit does not qualify current HEAD; it only addresses the installed
rev66 artifact. Reports are
`target/readiness/current-source-qual-20261105-4a1f4a4/rustsec-lockfile.json`
(SHA-256 `2f551d1ceee702ba99e3c6676401c46f981ac9a0e75de146ffdba01c5049fd9f`)
and `installed-binary-audit.log` (SHA-256
`cb7d818f16a5c20b7a6311394cc9db8d13bf87565c8875452b9d9de07e5e8692`). At
00:40 UTC, production remained dirty at **144** overflow notifications and
**3,923 pending updates**; its after-audit status snapshot is
`production-status-after-audit.json` (SHA-256
`1ccf8496d27b7d7f0b2a6f6cf61e22b17a9a39b715b32b7591d6ea9865938c87`).

### Clean current-HEAD static release package — 2026-11-05 00:51 UTC

The production release builder completed successfully from clean commit
`83b19059778157cb4846ab2d5c34e49c07d62680` as package revision **70**. The
build ran in a private tmpfs scratch/target with CPU quota **50%**, memory cap
**4 GiB**, and no swap; elapsed time was **5m46s**, CPU time **2m52s**, and
peak memory **1.5 GiB**. The manifest records `worktree_dirty=false`, 124 source
files, and source-tree SHA-256
`bfe06c58f1ace0be9173cecf7006eb1ccbd572f485c2284ce0644aca071c8986`.

The builder's refreshed RustSec gate passed with 229 locked dependencies,
zero vulnerabilities and zero warnings; the exact static executable scan
recovered 202 dependencies without warnings. Checksums for every artifact
passed. The ELF is static PIE and includes `.dep-v0`; the debug sidecar Build
ID matches. The reported production version/revision/source digest matches
HEAD, and the DEB and RPM payload binary hashes match the standalone binary.
The binary, DEB metadata, and RPM metadata were inspected. SHA-256 values:

- Static executable: `fc62b1d7bf3aaa4332a0cfb73db5685ba714409a1d12a35d3a4b8ac6c3bd77e7`
- DEB: `472ec5d991633951d38436aa2c125e473cee9649425e23da51ea9710bcc20bef`
- RPM: `2322c20344d97ed1de0a34324bc175b62411a8c047cdde795d71ea0fb7526ca6`
- Build manifest: `9e6ef497bfe545a553daa11ef5d1dfa250e1b8537e7fbd8cc7e0edd8b7a4b42d`
- `SHA256SUMS`: `1bfb173aaf118b0d0805821009794bdab1d43346f893a59c2d485954de07fc53`

The full bundle is retained in
`target/readiness/release-current-head-83b1905-70/`. It is a clean, audited
artifact bound to commit `83b1905`; this qualification entry is a subsequent
docs-only source-tree change, so the bundle will not match the newer HEAD
after this entry is committed. Rebuild as revision 71 from that latest HEAD
before promotion. Package lifecycle and promotion readiness are also open. At
**00:51 UTC**, the running production service was still the rev66
binary, active with zero restarts. Its index remained dirty from fanotify
overflow, with **144** overflow notifications, **5,248 pending updates**, no
scan in progress, and **19,498 seconds** behind; status snapshot SHA-256 is
`73644e9c55935b62bd2403c6495a8cfbad0dba199772aaae5f0099658a862463`. An
external Go compile/vet workload was active and host CPU PSI some avg10 was
**43.87%**. Current-package upgrade/rollback lifecycle and any live production
promotion are deferred until host pressure permits and the dirty index is
reconciled and exactly verified. Existing current-source live churn/fault,
scale, endurance, and canary gates remain open.

### Current DEB lifecycle attempt stopped by host-pressure guard — 2026-11-05 01:00 UTC

Started the prepared, checksum-verified Ubuntu 24 guest in an ephemeral
private-tmpfs overlay (2 GiB guest RAM, 20% host CPU quota). The guard stopped
the VM at **00:58:49 UTC**, before SSH became ready and before any package was
copied or installed. Its measured host pressure was load1 **13.04** and CPU
PSI some avg10 **60.97%**; the guard reported both its critical CPU/load
conditions and terminated only the disposable QEMU unit. This is an
environment admission stop, not a package lifecycle pass or product failure.
The guest reached initramfs driver loading but did not reach the root
filesystem or userspace. The temporary QEMU overlay was cleaned and the
verified Ubuntu base image was left unchanged.

Evidence is retained under
`target/readiness/vms/ubuntu24-lifecycle-current/`: pressure record SHA-256
`6cce60232431d9bbdebb0daf1524dfb9168b1a0035cc9e78749dacd4b83ca3bf`, boot
console SHA-256
`36077ec1793762a2a50658cc4ff70348faacc53c26bdf4e5f306fba723106eba`, and
the stopped guest state. At **01:00:48 UTC**, production was still active
without restarts, but its index remained dirty from fanotify overflow, with
**161** overflow notifications, **107 pending updates**, no scan in progress,
and about **20,069 seconds** behind. Status JSON SHA-256 is
`8a5f5d35aef3df14344ccd47b33b6de3490a163101c1f31e6086b21c750eca52`.
External Xgenstack Go race tests remained active. Package upgrade/rollback
testing is still open; retry the disposable guest only after the VM manager's
host-pressure admission guard passes.

### Python harness rerun and workflow state — 2026-11-05 01:12 UTC

The complete Python discovery suite passed **264/264** in **6.118 seconds**
when run directly with scratch files on `/dev/shm`. This includes release
builder error handling and publish rollback. Three stderr assertions had
failed in a preceding run under the constrained transient systemd test
environment; both affected test cases passed when run directly, and the full
direct rerun passed. Treat the earlier failures as runner-specific evidence,
not as proof of a product defect or as a fully diagnosed runner cause. The
passing log is
`target/readiness/current-source-qual-20261105-972f03b/python-suite-direct.log`
(SHA-256
`8ea4ac5f25a83ab031621da7a023dd9ce91f1e00a33307e4ea8300f4570c6bcb`).

GitHub reports both registered workflows (`Rust checks`, id **405823139**,
and the readiness workflow, id **405893096**) as `disabled_manually`. Their
workflow files are absent from the current default branch (`main`); GitHub's
contents API returns 404 for `ci.yml`. The visible failed runs are historical
records from November 3, not active workflow executions. No workflow run
history was deleted. The root filesystem has **66 GiB free (63% used)**; the
Python run used `/dev/shm` and did not write to the production index or
restart the service.

### Current-source release build and guarded Ubuntu lifecycle attempts — 2026-11-05 01:35 UTC

Static package revision **71** built successfully from clean HEAD
`7056e9d4360baf3a42a23c890a298760c1edf5d5`. The production ELF reports that
commit and source-tree SHA-256
`4b83ec3b876e099c6bb6654112440a8a93746c2324aa9e4d01bf20c6181e2a37`; the
manifest says `worktree_dirty=false`. It is a static musl x86_64 binary with
`.dep-v0`. The DEB and RPM payload binary hashes both equal the standalone
binary hash `723788df6111dbb6502b93a855c38d6754d55b652492dbb5fdc6977ed09b216b`.
All eight entries in `SHA256SUMS` pass. The lockfile audit found **0
vulnerabilities and 0 warnings across 229 dependencies**; the exact binary
scan recovered **202 dependencies** without warnings. The Build-ID-matched
debug symbols also passed the manifest check.

The build ran under a systemd cap of **30% CPU**, **4 GiB memory**, and no
swap. It completed in **9m59s** wall time, used **2m59s** CPU, and peaked at
**1.1 GiB**. The build log SHA-256 is
`c308310798dc9386288208cf72a86d393041c84dcea985acc58fa2d15f4225bb`; the
bundle is retained at
`target/readiness/release-current-head-7056e9d-71/`. Exact-binary smoke
coverage scanned a 4 KiB fixture with zero coverage issues; Dux `du -s` and
GNU `du -B1 -s` both reported **4096 bytes**. Smoke log SHA-256:
`c6238855a38bf9e0869c03ea767f78eb6b538784c4577d66174411ac519d3b5c`.

Two disposable Ubuntu 24 lifecycle starts passed storage admission but were
stopped by the host-pressure guard before SSH/package installation. At 10%
CPU quota, the guest mounted its root filesystem and started systemd, but the
SSH banner never became available; the guard stopped QEMU at load1 **12.08**
and CPU PSI some avg10 **41.53%**. At 20% quota, the guard stopped the second
attempt earlier in boot at load1 **11.10** and CPU PSI **43.77%**. These are
environment admission stops, not package failures or lifecycle passes. No
package was copied or installed; both private tmpfs runtimes were cleaned and
the verified Ubuntu image was unchanged. Guard-event log SHA-256
`2a0d80fcd14ae7f98cdfb1bca4813705c9c46ffa7d4fdddbaacbb0eb34ccb1bd`.

After the second stop, the production daemon remained active without a write
error, but its index was still dirty from fanotify overflow: **171** kernel
overflow notifications, **12,797** pending updates, no scan in progress, and
about **22,092 seconds** behind. Snapshot SHA-256
`2379f9f825887df73ce8a16d3a50ed78a2fd0d1e0579396232f9e561478b09c8`. The
current source has no code changes relative to the 259/259 all-features Rust
run at `4a1f4a4`; the only intervening tracked changes are qualification
documentation. This entry changes the source-tree digest, so revision 71 is
bound to the prior clean commit. Rebuild from the commit containing this entry
before promotion. Package installation/upgrade/rollback and all live-index
recovery gates remain open.

### Clean post-record release bundle and guest pressure stop — 2026-11-05 01:45 UTC

After committing the previous qualification entry, static package revision
**72** built from clean HEAD `b9780b3272ed17edcd39cf858683a3cec040ee46`.
Its source-tree digest is
`11a45aaf6597fb7b28c94c7ceb2e9620072b81b6318ff5ea1224c1976ef27900`, and
the manifest records `worktree_dirty=false`. The static PIE contains
`.dep-v0`; its binary SHA-256 is
`d008c69717927cc9a426c202bfde4210d883af7a06e5314160499e27706eb032`, which
matches both DEB and RPM payloads. All eight bundle checksums pass. RustSec
reported zero vulnerabilities and warnings across 229 lockfile dependencies;
the exact binary inventory recovered 202 dependencies without warnings. The
Build-ID-matched debug artifact passed. Bundle:
`target/readiness/release-current-head-b9780b3-72/`; SHA256SUMS SHA-256
`49e19210eee3532835076023126f0d1567dd331d5787a2511fa1ce78bc344a42`. Build
log SHA-256
`4418dbb6a2e183112dee58bb88f646e32571361cbbf5a575bb13804301b491e3`.
The capped build completed in **3m10s**, with **56.3s CPU**, **671.8 MiB** peak
memory, and no swap.

A later standard Ubuntu 24 guest passed storage and host admission, reached
Ubuntu 24.04.5 userspace, and completed cloud-init. SSH became available, but
the pressure guard stopped QEMU before the package copy began: CPU PSI some
avg10 reached **71.64%**, above its 70% critical limit, at load1 **10.19**.
The failed SCP happened after QEMU had stopped; no package was copied or
installed. The base image was unchanged and the private runtime was cleaned.
Console SHA-256
`66447af3fe2329f67c17bdf94183f050fe029a261667be5ce8af7454d969a58c`; guard
event log SHA-256
`97d3b46e05528cd36266eb87c62ccea0f4d67deb2012a38b367f83680eb21a51`.

The production daemon remained active without a write error. Its index was
still dirty after fanotify overflow, **171** overflow notifications, **75**
pending updates, no scan in progress, and about **22,655 seconds** behind.
Snapshot SHA-256
`47dbfbd759de77f86c01d6ac159aeac6c64386c4fb1d28fb8bde1bb3bd06a225`. Root
disk has **65 GiB free (64% used)**. Package install/upgrade/rollback and
production index recovery remain unqualified. This entry changes the source
tree; rebuild the release bundle from its resulting commit before promotion.

### Revision 73 bundle and production reconciliation circuit diagnosis — 2026-11-05 02:15 UTC

Static package revision **73** was built from clean HEAD
`b1814ae6938c794c2c7251b50124a5f2cd3a5502`. Its manifest binds the binary to
source-tree SHA-256
`8058b0c47fe8a10e680cd27cf7eea2f0cde25cfc157fda0c12a70b58405b41f9` and
records `worktree_dirty=false`. The x86_64 static PIE includes `.dep-v0`; its
binary SHA-256 is
`b36227616af18a3740f9897e31031f8ee6a5d87c8b723f9c38371af24d00193c`, matching
the DEB and RPM payloads. All eight artifact checksums pass. RustSec reports 0
vulnerabilities and warnings across 229 lockfile dependencies; the exact
binary scan recovered 202 dependencies without warnings. The Build-ID-matched
debug artifact passed. The bundle is
`target/readiness/release-current-head-b1814ae-73/`; SHA256SUMS SHA-256 is
`49e19210eee3532835076023126f0d1567dd331d5787a2511fa1ce78bc344a42`. Build
log SHA-256 is
`7335c206ca81b8ca8f6be424c642fc3ffd522404b84c59ec5927416da2d5ac52`; the
tmpfs-cached build completed in **2m51s**, used **50.9s CPU**, peaked at
**672.6 MiB**, and used no swap. Exact-binary smoke scanned a 4 KiB fixture
with zero coverage issues and matched GNU `du` at **4096 bytes**; log SHA-256
`201b838f5fd86e2a363a39a7b056c616737ce75fb5a3b46c3687d3d8152b794e`.

The current production service is still the rev66 executable
`e0cc915f8f37`; it is active as PID **423219** without a write error. A
read-only query of `/var/lib/dux/dux.db` using the current revision 73 CLI
confirmed `automatic_reconciliation_paused=true`, **2** failed full scans,
and a maximum of **1** automatic retry. The installed rev66 status output
does not expose these fields. Journal entries for incomplete reconciliations
record `lost_events=true` and zero scan errors, consistent with event loss
during host churn rather than a filesystem traversal error. The queue has
drained near zero between external build bursts, but its overflow count has
risen to **200**. At **02:14:34 UTC**, the candidate status showed **233**
pending events, no scan in progress, and about **24,495 seconds** behind;
the host-load admission check still rejected intensive work at load1 **14.27**.

The installed rev66 CLI supports the documented `dux scan /` recovery path:
it signals the live daemon with SIGHUP and waits for an in-place, single-writer
rescan and accounting verification. The current source reports the paused
retry circuit in JSON and human status. The recovery request remains deferred
until external Go build/test and analysis workloads subside and the host
pressure gate passes; a rescan during another overflow could fail the same
circuit again. Candidate status evidence SHA-256 is
`9a993c613a647630bb04643dd3999a3663932a15397dccc74c0a30c6dbf12728`.

A further guarded Ubuntu start reached userspace and completed cloud-init, but
host CPU PSI then crossed the guard's hard limit before either package could
be copied. No package lifecycle step ran; the base image was untouched and
the tmpfs runtime was removed. Guest console SHA-256
`66447af3fe2329f67c17bdf94183f050fe029a261667be5ce8af7454d969a58c`; guard
log SHA-256
`9b34303f050d951623e74a1677800d21450b92a333cb60d21bb410f83e000601`. This
entry changes the source-tree digest, so revision 73 is bound to the previous
commit; build from the commit containing this diagnosis before promotion.
Package installation/upgrade/rollback and live-index recovery remain open.

### Live-host rescan admission recheck — 2026-11-05 02:19 UTC

The production daemon remains the installed rev66 process (PID **423219**).
Revision 73's read-only status at **02:19:45 UTC** reported the reconciliation
circuit still paused after **2** failures, **200** recorded kernel overflow
notifications, no active scan, no write error, and **9** pending updates. The
last scan is about **24,805 seconds** old; the index has about **1.37 million**
nodes. Pending work drained from **20,126** at 02:17:10 to single digits, but
the event stream remained active and unrelated Go race tests plus an automatic
host repair process were running. Load1 was **3.36**, CPU PSI `some avg10` **1.47%**,
and I/O PSI `full avg10` **2.65%**. Root had **66 GiB free**. The low pending
count is not sufficient admission evidence while host filesystem churn and a
repair operation continue; no recovery scan, service restart, or package
change was made. Recheck only after those live processes exit and counters
remain stable across observation windows. `/tmp` contained about **14 GiB**,
including active Go build data and unrelated application caches, so it was
preserved; `target/readiness` retained about **6.6 GiB** of qualification
evidence.

### Repeated production fanotify overflow during external test churn — 2026-11-05 02:27 UTC

The admission gate did not clear. At **02:26:53 UTC**, the revision 73
read-only status showed the overflow counter had increased from **200** at
02:25:06 to **215**, with the retry circuit still paused after **2** failed
reconciliations. The status showed **1,435 pending updates**, no scan in
progress, no write error, and about **25,233 seconds** since the last scan.
Since 02:26:00, the production journal recorded **15** additional
`fanotify queue overflow` warnings; individual warnings appeared seconds
apart alongside unresolved stale file handles. An unrelated Go suite remained
active in `/home/xgenstack.com/backend` (PIDs **2764614/2786939**). At this
sample, load1 was **5.51**, CPU PSI `some avg10` **1.41%**, and I/O PSI `full
avg10` **9.20%**. These observations show continuing event loss during
concurrent host churn; they do not establish that this external workload is
the sole cause. Snapshot and journal SHA-256:
`c5ad88f0cf22d03baedb9bec59eab9478f0bbe84c3985dca17c9b5686c3170e7` at
`target/readiness/current-source-qual-20261105-0bd433d/fanotify-overflow-repeat-0226Z.json`.
No recovery scan, service restart, package change, or index mutation was
attempted. Rescan admission remains closed until overflow stops across quiet
observation windows; package lifecycle and current-source release qualification
remain open.

### Recovery gate remains closed during renewed race tests — 2026-11-05 02:50 UTC

At **02:50:44 UTC**, an unrelated `go test -race -count=1 -timeout 5m`
batch was active in `/home/xgenstack.com/backend`. Read-only Dux status showed
the daemon still live as PID **423219**, no active scan, no write error, and
the overflow counter unchanged at **236**. Pending updates had climbed to
**24,395** during churn and then drained to **9,901**; the last event commit
was current to **02:50:44 UTC**, but the index remained dirty and about
**26,664 seconds** behind. The preceding minute contained **36** unresolved
fanotify/stale-handle or slow-flush log entries. Host load1 was **12.76**, CPU
PSI `some avg10` **49.25%**, and I/O PSI `some avg10` **0.89%**. Root had
**62 GiB free** and `/dev/shm` **8.7 GiB free**. The falling pending count is
useful drain progress, but the active race-test batch, high CPU pressure,
continuing unresolved events, and stale index age do not satisfy the live
recovery gate. No scan, restart, package change, or direct database operation
was performed. Recheck after the external batch exits and require stable
pending/overflow samples before the next daemon-mediated scan.

### Go build churn resumed after apparent test exit — 2026-11-05 02:54 UTC

The previously observed Go test processes exited, but new Go processes in
`/home/xgenstack.com/backend` started immediately afterward. At **02:53:51
UTC**, Dux remained dirty with **65** pending updates, **236** recorded kernel
overflows, no active scan, and no write error; the last scan was **651 seconds**
old and status remained about **26,851 seconds** behind. The preceding 15
seconds contained **14** unresolved fanotify/stale-handle or slow-flush log
entries. Host load1 was **18.15**, CPU PSI `some avg10` **50.31%**, I/O PSI
`some avg10` **0.72%**, and `/` had **62 GiB** free. This confirms the quiet
window did not hold: the low pending count is transient while new short-lived
build processes continue generating events. No scan or package/service
operation was performed. The rescan gate remains closed until host churn and
unresolved event warnings stop over repeated samples.

### Repeated test churn prevents sustained recovery window — 2026-11-05 02:57 UTC

At **02:56:38 UTC**, unrelated `/home/xgenstack.com` workloads had restarted:
`npm exec vitest run` under the UI tree and `go test ./...` under the backend.
Dux remained live without an active scan or write error, but status showed
**44,272 pending updates**, `dirty=true`, **236** recorded kernel overflows,
and about **27,019 seconds** since the last completed scan. The preceding 30
seconds contained **21** unresolved fanotify/stale-handle or slow-flush log
entries. Load1 was **10.40**, CPU PSI `some avg10` **9.84%**, and I/O PSI
`some avg10` **0.70%**; root had **66 GiB free**. The overflow count had not
increased in this sample, but ongoing unresolved events and the growing queue
show that the earlier low-pending observation was not a durable recovery
window. No scan, restart, package change, or direct database operation was
performed. Retry admission only after the external tests/builds finish and
multiple quiet samples show the queue and event-loss signals have settled.

### Live production service security posture recheck — 2026-11-05 02:58 UTC

The running service is `/usr/bin/dux` version **0.6.3** (production source ID
`e0cc915f8f37`, source SHA-256
`b7c9a8dbe1f40ff0190ade339385211f27ebc94aa0972e1c8313309d5fb19882`, binary
SHA-256 `e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`),
PID **423219**. Effective systemd settings were re-read from the manager and
live process: `NoNewPrivileges=yes`, `SecureBits=noroot noroot-locked`,
`LimitCORE=0`, `MemoryMax=4G`, `CPUQuota=100%`, `RestrictAddressFamilies=AF_UNIX`,
`SystemCallFilter=~@module @mount @swap @reboot @clock @obsolete @raw-io
@debug @cpu-emulation`, and capability bounding/ambient sets limited to
`CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`. `/proc/423219/status` confirmed
those effective/permitted capabilities, `NoNewPrivs=1`, and seccomp mode 2.
`systemd-analyze security --no-pager dux.service` scored the effective unit
**3.3 (OK)**. The state directory is mode **0750 root:dux**; the database,
WAL, and SHM are mode **0640 root:dux**, with no additional ACL grants. The
index remains sensitive because it lists host paths, and the daemon still runs
as root with `CAP_SYS_ADMIN`; residual privilege acceptance/reduction remains
open. `ProtectSystem`, `ProtectHome`, and `PrivateTmp` remain disabled to
preserve the host filesystem view required by fanotify. This security check
does not change the separate live-correctness gate: at the preceding
02:57:34 sample Dux was dirty with **34,471 pending updates** and about
**27,074 seconds** behind. No service, package, or database change was made.

### Stale-handle events persist after short-lived tests exit — 2026-11-05 03:00 UTC

The Go race-test and Vitest processes observed earlier were absent at **02:59:44
UTC**, and pending updates had fallen from **17,609** at 02:59:13 to **7,849**.
However, the preceding 45-second journal window still contained repeated
unresolved `FAN_ATTRIB` events with `ESTALE` while opening DFID_NAME handles.
The event PIDs included long-lived host processes such as Codex app-server
**2746478**, Codex session **3477315**, and container-runtime shims **1892**,
**1938**, and **1968**. These processes remained present in the host process
table; therefore ending the short-lived Go/Vitest batches did not stop the
unresolved event stream. At the sample, status remained dirty, about **27,205
seconds** behind, with the kernel-overflow count unchanged at **236** and no
active scan or write error. This shows the rescan gate is constrained by
ongoing filesystem churn from long-lived host services as well as test batches;
it does not establish that any one process is the sole cause. No scan or
service/database operation was attempted.

### Production rescan, bounded retry, and scan-progress gap — 2026-11-05 03:14 UTC

With no Go/npm/Rust test command visible, low pending work, and stable overflow
count **236**, `/usr/bin/dux scan /` was issued at **03:01:46 UTC** through the
live daemon. The first pass completed at **03:04:00** with **1,241,901 files**,
**116,909 directories**, **1,358,810 staged path instances**, **107.86 GiB**
visible bytes, and **zero traversal coverage issues**. It still reported
`lost_events=true` after unresolved fanotify identities and scheduled the
bounded retry. The WAL reached **331,186,232 bytes**; event writes paused and
resumed after checkpoint recovery at **134,217,728 bytes**.

The retry started at **03:05:00** and completed at **03:08:13** with
**1,244,369 files**, **116,915 directories**, **1,361,284 staged path
instances**, **108.20 GiB** visible bytes, and again **zero traversal coverage
issues**. It also had `lost_events=true`; **33,454 pending paths** were replayed
and the automatic circuit opened after two failed scans. Its WAL reached
**269,575,752 bytes** and recovered to **134,217,728 bytes**. The client was
then interrupted while waiting; the daemon stayed live. At **03:13:31**, a new
external Go test batch was active, pending was **596**, the dirty index was
about **28,031 seconds** behind, and the recorded overflow count had risen to
**253**. The scan installed a fresh baseline but did not establish exactness.

The run also reproduced a status-reporting defect. At **03:03:45**, status
reported no active scan and `/run/dux/scan.progress` was absent while
`/var/lib/dux/dux.db.new` was **586 MiB** and still growing; the scan client
remained active until install/replay. Source inspection found `scan_inner`
cleared progress before `rebuild_atomic` completed its staging fsync and
SQLite backup publication. The worktree now retains and refreshes progress
through atomic publication and has a regression test that holds the publication
boundary for more than the 10-second stale-marker limit. `cargo fmt --all --
--check` and `git diff --check` pass. The targeted publication regression passed
**1/1**, and the complete `scan::tests` module passed **33/33** serially. Both
ran in a transient scope limited to **10% CPU**, **2 GiB memory**, no swap,
`IOWeight=10`, and `Nice=19`; temporary fixtures used `/dev/shm`. The targeted
regression held the install boundary for **11.25 seconds**, exceeding the
10-second stale-marker cutoff, and confirmed progress was cleared after
publication. The broader all-features Rust binary unit suite subsequently
completed under the same transient resource limits: **260 passed, 0 failed, 0
ignored** in **173.37 seconds**. It covered the binary's all-feature unit
tests; it does not replace current-source release/package qualification or
host-level fault, scale, endurance, and canary qualification. The production
binary is unchanged. At the latest follow-up sample, the daemon remained live
with no restarts, **7 pending updates** out of **500,000** capacity, no
userspace drops, **253** recorded kernel overflow notifications, and the index
still marked dirty and about **8 hours** behind its last caught-up point. Root
storage had **64.88 GiB free**. No full-root scan was repeated because the
exactness gate remains open after two scans reported lost events.

### Clean release build and test-only scan helper — 2026-11-05 04:04 UTC

The non-atomic `scan()` wrapper is only referenced from unit-test code; runtime
rebuilds use `rebuild_atomic()`. It is now compiled only for tests, removing the
dead-code warning from optimized release builds. The all-features binary unit
suite passed **260/260** again after this change in **143.42 seconds**, serially
and under a transient scope capped at **10% CPU**, **2 GiB memory**, no swap,
`IOWeight=10`, and `Nice=19`. Formatting and whitespace checks passed.

The default production profile then built successfully from clean commit
`86e94cf8e42e` using `cargo build --offline --locked --release --bin dux` in
**7m13s**, under the same resource caps, with no compiler warnings. The
resulting **5.0 MiB** `target/release/dux` reports version **0.6.3**, revision
`86e94cf8e42e`, production build kind, and source digest
`2de4f9eb0a4a36d7efe7480ebaecf06a4b77cbe02f1824d6a2f21fff02157ed4`; its
`--version` and `--help` smoke checks succeeded. This artifact was not
installed or used to replace the running daemon.

At **04:04 UTC**, the installed daemon remained live with no reported userspace
drops, but its pipeline was **22,266 pending** of **500,000** capacity, the
index was dirty, and the recorded kernel overflow count had risen to **285**.
There were about **61.88 GiB free** on `/`. A clean release build and passing
unit suite do not resolve these live event-coverage failures; another full-root
scan remains inappropriate until host churn and overflow behavior stabilize.
Current-source packaging/install, live index exactness, fault/scale/endurance
qualification, and production canary remain open.
