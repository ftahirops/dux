# Readiness artifact storage and retention review — 2026-10-30

## Scope and method

This review is based on the complete, non-atomic filesystem metadata inventory
captured at 2026-10-30 02:50:38 UTC under `target/readiness`. It covered 187,675
entries with zero traversal errors. The inventory records 81,294,966,784 bytes
allocated (75.7 GiB) and 98,872,686,941 bytes apparent (92.1 GiB). Hardlinks are
charged once to their bytewise smallest relative path; CoW/shared extents across
qcow2 files are not deduplicated. File contents were not read for hashing. The
canonical report was recomputed from the saved rows at 02:53 UTC, without another
filesystem walk; its source report and attribution policy are embedded in the
JSON. Neither report is an atomic snapshot.

At the later disk checkpoint (02:56 UTC), `/` was 96% used with 7.36 GiB
available by `statvfs`, below the 8 GiB reserve by about 0.64 GiB and below
the 10 GiB VM admission threshold by about 2.64 GiB. `target` used about
80 GiB, including 76 GiB under `target/readiness`; `/tmp` used about 7.1 GiB
and contains other workspace/build caches. No cleanup was performed.
The earlier inventory’s 8 GiB reserve preflight passed by only about 0.38 GiB
at capture time; that result is stale after subsequent disk growth. Inode use
was about 11%, so bytes, not inode exhaustion, are the current admission limit.

## Largest consumers and dependencies

| Artifact | Allocated | Apparent | Observed purpose / dependency |
| --- | ---: | ---: | --- |
| `vms` group (126 entries) | 60.20 GB | 75.43 GB | Mixed qualification VM images and state; not one disposable guest |
| `vms/ubuntu24/disk-r5-qualification.qcow2` | 33.61 GB | 38.31 GB | Persistent Ubuntu qualification disk; qcow2 backing file is the Ubuntu 24.04 cloud image |
| `vms/rocky9/disk.qcow2` | 17.78 GB | 17.88 GB | Persistent Rocky 9 guest disk; backing file is `vms/rocky9.qcow2` |
| `scale-e30f4288-10m-oom-resume.qcow2` | 11.02 GB | 11.14 GB | Retained 10m OOM/resume evidence; backing file is `vms/rocky9.qcow2` |
| `vms/ubuntu24/disk.qcow2.host-root-backup` | 3.92 GB | 14.15 GB | Preserved qualification overlay backup; backed by the Ubuntu cloud image |
| `p0-churn-oom-failure-20261026/filesystem.img` | 2.15 GB | 2.15 GB | P0 churn/OOM failure reproduction |
| `production-review-20261029` | 1.35 GB | 3.60 GB | Candidate-specific review and qualification evidence |
| `production-review-20261030` | 1.04 GB | 1.04 GB | Current candidate artifacts, reports, packages and partial guest evidence |
| `prior-workspaces-20261023.tar.zst` | 316 MB | 316 MB | Recovery archive of 14 generated temporary workspaces |

`qemu-img info` reported the inspected large qcow2 files as not corrupt and with
clean dirty flags; this is metadata inspection, not a full image integrity test.
No QEMU process was observed. `scripts/dux-qualification-vm.py` uses the shared
`target/readiness/vms` image directory; `vms/images.json` records expected base
image digests, and Rocky lifecycle state files refer to the Rocky base. The
10m image shares that base. Treat backing images as shared objects and keep them
until every overlay dependency has been retired or relocated and revalidated.

The workspace archive has an earlier SHA-256 and successful `zstd -t` plus GNU
tar comparison recorded in the readiness audit. The host-root overlay backup
has a creation-time digest recorded in `docs/QUALIFICATION.md`; neither digest
was rechecked during this inventory. Do not treat recorded historical hashes as
proof of current content identity.

## Current revalidation — 2026-11-02 13:38 UTC

The 2026-10-30 inventory is historical and must not be used to authorize current
cleanup. A low-priority metadata walk now reports `target/readiness` at **8.1
GiB**, with no `vms` directory and no QCOW2/raw virtual-disk images anywhere in
that tree. The only image over 1 GiB is
`p0-churn-oom-failure-20261026/filesystem.img` (**2.0 GiB**), a retained P0
churn/OOM reproduction referenced by its report and daemon log; preserve it
unless that reproduction is formally retired. The smaller `seed.img` files are
about 375 KiB each. The historical VM artifacts totaling 60.20 GB allocated
are absent from this tree; this check does not establish whether they were
deleted or relocated.

`/tmp` currently accounts for **63 GiB**. Its largest directory is
`xgs-backend-gocache-wjaDw3` (**15 GiB**), followed by multiple 0.5–3.1 GiB
`xgs-*` Go/Rust/test caches. These appear to belong to another project, not
Dux. They were not deleted: their owner, active consumers, and rebuild cost
were not established. The Dux warm test cache `/tmp/dux-test-target` is **1.4
GiB** and was retained for future low-impact qualification runs. At 13:38:41
UTC, `/` was **96% used with 7.4 GiB available**. This is a point-in-time
measurement; other host processes continue to consume and release space.

## Custody and retention disposition

Filesystem ownership on inspected VM files is `root:root`; that does not identify
a human decision-maker. The workspace does not assign accountable custodians,
expiry dates, or a complete consumer graph for these artifacts. The machine
review catalog `target/readiness/storage-review-20261030.json` records large
files, observed UID/GID/mode, allocation, apparent size, known purpose, known
consumers, provisional retention class, digest status and the unassigned human
custodian. Every row has `reclamation_authorized=false`.

Use these provisional classes until a project owner approves a policy:

- **Shared VM base/dependency:** preserve while any test or overlay uses it.
- **Rollback/recovery backup:** preserve until restore/rollback value is formally
  retired and an independent restore check succeeds.
- **Open gate or failure evidence:** preserve while the matching correctness,
  performance, security or recovery gate remains open and until replacement
  evidence is accepted for the exact candidate.
- **Workspace recovery archive:** preserve until the workspace owner retires
  recovery need.
- **Unclassified:** preserve until a custodian identifies purpose, consumers,
  retention period and replacement/equivalence evidence.

This review authorizes no removal, truncation, compression, relocation or
compaction. The oldest/failed-looking artifacts are not automatically disposable:
several are dependencies or the only retained evidence for unresolved gates.
STORAGE-01 remains open until a human custodian is assigned, every large group
has a verified consumer/dependency map and retention period, recovery archives
are validated, and any reclamation plan is approved with adequate headroom.


## Current operational checkpoint

At 02:56 UTC the installed daemon remained active but dirty and not caught up:
87,249 pending updates, 10,331 seconds behind (~2h52m), zero userspace drops,
and 128 MiB WAL. No QEMU VM process was observed. The package lifecycle and
scale qualification manager remains inadmissible at current disk headroom. The
read-only status and exact inventory artifacts remain separate: the status is a
live point sample, while the inventory is a non-atomic tree walk captured at
02:50 UTC.

## Refreshed inventory checkpoint

At 06:07 UTC, the read-only inventory script completed another non-atomic walk
with **187,764 entries**, **zero traversal errors**, and `complete=true`. It
measured **82,590,846,976 allocated bytes (76.92 GiB)** and **100,295,484,767
apparent bytes (93.41 GiB)**. This is metadata-only: file contents were not
hashed, qcow2 shared extents were not deduplicated, and the inventory does not
prove atomicity or a complete consumer graph. The temporary 119 MiB JSON report
was removed after extracting this summary; only aggregate results are retained.

The VM group remains the largest consumer at **60,203,184,128 allocated bytes
(56.06 GiB)** and **75,431,195,409 apparent bytes (70.25 GiB)** across 126
entries. The 10m OOM/resume image alone measures **11,021,139,968 allocated
bytes (10.27 GiB)** and **11,137,777,664 apparent bytes (10.37 GiB)**. Its
read-only QEMU-NBD mapping was live during this checkpoint and remains in
custody. `production-review-20261030` now accounts for **2,331,500,544 allocated
bytes (2.17 GiB)**; `p0-churn-oom-failure-20261026` accounts for **2,147,569,664
bytes (2.00 GiB)**. These are protected evidence, not cleanup recommendations.

At capture, `/` was **94% used** with roughly **11.65 GiB** available and about
11 million free inodes. An 8 GiB reserve-only preflight passes, but the required
10 GiB workspace plus 8 GiB reserve preflight fails with
`insufficient_free_bytes`. No VM qualification or artifact reclamation was
started. All rows continue to have `reclamation_authorized=false`; the report's
explicit-manifest consumer graph is incomplete by design. This checkpoint
updates measurements only; it does not assign a human custodian or retention
expiry and does not close STORAGE-01.

## 07:43 UTC follow-up

Read-only host capacity remains **95% used with 11 GiB available** on `/`,
`target/readiness` remains **78 GiB**, and `/dev/shm` has **3.1 GiB free** after
the Rocky 9 ephemeral guest was stopped through the qualification manager. The
new RPM transaction report and diagnostics are retained as release evidence
(roughly 35 KiB); the guest test report, journal, and controller state were
saved before the manager cleaned its ephemeral overlay. No retained VM, backup,
source, or qualification artifact was deleted or compacted. The largest
precious files remain the Ubuntu qualification qcow2 (**38.3 GB**), Rocky 9
qcow2 (**17.9 GB**), Ubuntu host-root backup (**14.1 GB**), and 10m scale qcow2
(**11.1 GB**). They remain protected as shared test inputs, recovery state, or
evidence for open gates. No custodian or reclamation authorization has been
assigned; STORAGE-01 remains open.

At **07:48 UTC**, the live service database measured **708,210,688 bytes**,
the WAL **134,217,728 bytes**, and the SHM file **1,409,024 bytes**. All three
are `root:dux` mode `0640`; they were inspected with read-only metadata calls
and left untouched. The daemon was active but dirty/not caught up with **147,802
pending events**. The WAL's current size plateau is useful evidence, not a
strict-bound guarantee. Root capacity remained **95% used with 11 GiB free**;
no database, WAL, SHM, VM image, or backup was reclaimed.

## Current storage recheck — 2026-11-02 13:55 UTC

A fresh point check found `/` at **97% used with 6.4 GiB available** and
`target/readiness` at **8.1 GiB**. There is no `target/readiness/vms` directory,
QCOW2/VMDK/VDI image, or raw disk image in that tree; its only image above 1 GiB
is the retained **2.0 GiB** P0 churn/OOM reproduction `filesystem.img`. No
`vst*` directory, `codex-sessions-archive`, `.local`, `.cache`, or `go` directory
was present under `/home/dux` during this check. This is a read-only observation;
no cleanup occurred in this checkpoint. `/tmp` remains about **63 GiB**, with
large `xgs-*` caches whose owner and active consumers are unresolved, so they
remain untouched. The qualification workspace preflight still fails its
10 GiB workspace plus 8 GiB reserve requirement.

## Host-level storage follow-up — 2026-11-02 14:06 UTC

Root availability has fallen to **5.0 GiB free / 98% used**, below the
10 GiB workspace plus 8 GiB reserve preflight. Dux's own workspace is **8.19
GiB**. Additional consumers include `/tmp` (**64.29 GiB**), `/var/tmp` (**19.69
GiB**), `/var` (**30.01 GiB**), and `/root` (**20.25 GiB**). The largest
`/var/tmp` directories are XGS/XGen Rust, Go and test caches; the active XGen Go
test process is still running. Ownership and active consumers for those trees
belong to other services/workloads and were not inferred from root ownership;
no files were deleted. A read-only `lsof +L1` sample found no large deleted-open
file that could immediately reclaim space.

At **14:15:28 UTC** `/` had **4.2 GiB available / 98% used**. This improved from
3.1 GiB shortly before, but remains below the 18 GiB qualification workspace
plus reserve. Treat the readings as fluctuating point samples, not a recovered
capacity margin.

## Reclaim idle Rust test cache — 2026-11-02 14:18 UTC

After the all-features suite passed, confirmed `/tmp/dux-test-target` had no
active Cargo/Rust consumer and contained only `.rustc_info.json` plus Cargo's
`debug/` outputs (3,997 regular files, no symlinks or non-regular files). The
built-in `cargo clean` refused because this custom target lacked `CACHEDIR.TAG`;
a path-checked cleanup removed only that verified cache, **1,958,791,552 bytes**.
Root availability increased from **4.1 to 5.4 GiB** in the immediate check and
was **5.2 GiB / 98% used** at 14:18:05 UTC after concurrent host writes. The
cache can be rebuilt, but root remains far below the **18 GiB** build/test
reserve; no release build was started.

## Recheck after capped tests — 2026-11-02 14:30 UTC

At **14:29:51 UTC** `/` had **8.9 GiB free / 95% used**, up 2.2 GiB from the
14:28:57 sample while unrelated host scratch was released. This is still below
the 18 GiB qualification preflight and is a fluctuating margin, not durable
headroom. `target/readiness` is **8.1 GiB**; `/tmp` is **63 GiB** and `/var/tmp`
is **20 GiB**, primarily other workloads' caches/scratch whose consumers are
not safe to interrupt; `/dev/shm` is **12 GiB used of 13 GiB**, so it cannot
hold the required isolated workspace. The requested old `vst*`, `.local`,
`.cache`, `go`, and `codex-sessions-archive` paths remain absent under
`/home/dux`; the readiness tree still has no VM images.

`/dev/loop0` remains an **8 GiB** loop mapping to the deleted
`/tmp/dux-scale-f2azn2f8/filesystem.img`, with `/run/dux-ext4-inspection`
mounted read-only in the host and Dux namespaces. This retains a potentially
reclaimable backing image and remains a possible scan contaminant, but
releasing it requires controlled service/mount maintenance. No unmount,
detach, cleanup of other workloads' caches, or deletion of retained failure
artifacts was performed. A source-only release build was stopped after CPU PSI
crossed its guard; its **47.3 MB** partial target was removed. The production
service and index were not changed.

## Preserve the P0 filesystem image in compressed form — 2026-11-02 14:36 UTC

The retained P0 churn/OOM filesystem image was not deleted: it was losslessly
compressed. Its original size was **2,147,483,648 bytes** and its compressed
`filesystem.img.zst` is **41,845,099 bytes (39.9 MiB)**. `zstd -t` passed, and
the SHA-256 of the decompressed stream exactly matches the original image hash
`8b1588f793bd734af76e40402e84ca3fb22cb5616025fd712aff20fc2f6139ec`. The
compressed file hash is
`ca2720259352fbd7217649b0e93d969a89dda6e2c90ef37a5bc12a5edf9ed4a0`; the
failure analysis JSON records the storage representation and restore command.
The raw image was unlinked only after those checks and confirming no process had
it open. This reclaimed about **2 GiB**; `target/readiness` is now **6.2 GiB**.

The compression ran with a **5% CPU** quota and idle I/O. A CPU PSI sample
briefly reached **20.86% avg10**; by the attempted stop the short compression
had already completed. No further compression/build workload was started.
At **14:35:36 UTC** `/` had **11 GiB free / 94% used**, still below the **18
GiB** workspace-plus-reserve qualification gate. The service continued to
report reconciliation deferred and no scan active despite contemporaneous
host load **2.26 on 12 CPUs**, **15.3 GiB MemAvailable**, CPU PSI **0.31%**,
I/O PSI **0.15%**, and disk free **11 GiB**. Installed R16 is source-unmatched;
this observation does not establish why its recovery gate remained deferred.

## Headroom and active scratch recheck — 2026-11-02 14:38 UTC

After compressing the retained P0 image, root availability measured **11 GiB**
at 14:35 and then **9.6 GiB / 95% used** at 14:38. `target/readiness` is
**6.2 GiB**, `/tmp` **64 GiB**, `/var/tmp` **20 GiB**, and `/dev/shm` has only
**1.7 GiB free**. A live XGen Go compile/vet workload has processes using
`/tmp/x.yNY1o/go-build...` (about **1.1 GiB**); the XGS/XGen cache trees under
`/tmp` and `/var/tmp` remain outside Dux ownership and were not touched. A
read-only `lsof +L1` sample found no large deleted-open files. Space is
fluctuating while unrelated builds run, and the **18 GiB** qualification
workspace-plus-reserve gate still fails. Do not launch another build or clean
the active scratch until the host is quiet and its owners have completed.

## Readiness artifact inventory refresh — 2026-11-02 14:48 UTC

Root availability has recovered to **9.7 GiB / 95% used**, still below the
**18 GiB** qualification reserve. `target/readiness` occupies **6.2 GiB allocated
and 6.1 GiB apparent**; `target/readiness/vms` is absent. The earlier VM-heavy
listing is not present in the current tree, and its disappearance is not attributed
to a specific cleanup from the available evidence.
The largest retained subtrees are `production-review-20261030` (**724 MiB**),
`canary-r16-20261027` (**534 MiB**), `zero-truncate-repro-untraced-failure-full`
(**430 MiB**), `r15-fault-injection` (**262 MiB**), and
`r15-scale-host-oom-20261026T213019Z` (**197 MiB**). These are named historical
qualification/rollback artifacts referenced by the audit, so no directory was
removed. The canary rollback subtree contains a **416,874,496-byte SQLite
rollback index** and a **130,314,240-byte SQLite scan-staging database** from
Oct. 27; neither file is currently open by a process. Their age and lack of an
open handle do not establish that they are disposable, so both were retained.
The stale lifecycle rootfs is **301 MiB** and likewise remains referenced by
package-test evidence. No large readiness artifact was proven safe to delete;
no other project's `/tmp` or `/var/tmp` scratch was touched. The production
service, database, mount topology, and retained evidence were not changed.


## Current live v5 rollback snapshot — 2026-11-02 15:13 UTC

Created a transaction-consistent online backup of `/var/lib/dux/dux.db` at
`/root/dux-prequalification-20261102T1503Z.sqlite`, with its manifest beside it.
The backup is **755,593,216 bytes**, schema v5, 184,471 pages, `quick_check=ok`,
SHA-256
`21bc898fd23496fe257ab20d28a9a2b9de55e6cb86a94781ab43e556f0a28425`; the
published file and manifest are root-owned mode **0600**, under `/root` mode
**0700**. A separate capped read pass confirmed the published file's hash,
size, and mode match the manifest. The helper fsynced the artifact and
manifest. This is a database-consistent rollback snapshot, not an exact tree
baseline: at 15:05 UTC the live index was still dirty with 321,482 pending
updates and 502,309 drops, with no scan in progress.

Root now has **7.7 GiB free / 96% used** after concurrent host writes and this
721 MiB backup. No retained evidence was deleted. The snapshot is retained for
a controlled v5→v6 rehearsal; it must not be used to claim production exactness.


## Full SQLite integrity check of the live v5 snapshot — 2026-11-02 15:18 UTC

A separate read-only `PRAGMA integrity_check` against the root-only backup
returned **`ok`**. It ran in a transient scope capped at 5% CPU, 512 MiB,
no swap, IOWeight 10, `nice 19`, and idle I/O priority; duration was **4m
28.972s**, CPU time **13.449s**, memory peak **367 MiB**, zero swap. This
strengthens the snapshot's database-integrity evidence but does not make its
rows an exact representation of the dirty live filesystem. At 15:18 UTC root
had **9.2 GiB free / 95% used**, still below the 18 GiB build reserve; the
snapshot remains private under `/root` and the source index was not modified.
