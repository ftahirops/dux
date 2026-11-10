# VPS2 disk audit — 2026-11-10

Read-only inspection was performed over SSH on `192.168.5.106` (VPS2), then
one unattached Docker cache volume was removed. No Dux package, database, running
container, telemetry data, incident archive, or rollback workspace was removed.

## Filesystem capacity

Before cleanup, `df -hT /` reported the root ext4 filesystem at 93 GiB total,
83 GiB used, 5.8 GiB available (94%); inode use was 30%. `du -x -h
--max-depth=1 /` reported 83 GiB accounted across the root filesystem. After
removing the cache below, `df -h /` reported 82 GiB used and 6.2 GiB available
(still 94%). The rounded readings imply approximately 0.4 GiB recovered.

The root directory breakdown before cleanup was:

| Path | Allocated size |
|---|---:|
| `/var` | 63 GiB |
| `/root` | 8.0 GiB |
| `/usr` | 6.2 GiB |
| `/srv` | 1.1 GiB |
| `/opt` | 448 MiB |
| `/home` | 215 MiB |
| `/tmp` | 96 MiB |

The sizes are filesystem allocation measurements and can differ from logical
indexed totals, particularly for container overlay filesystems and shared
layers.

## Largest identified consumers

The live Dux index's `dux top --limit 20 --explain` reported:

| Path | Indexed allocated size | Assessment |
|---|---:|---|
| `/var/lib/docker/volumes/signoz-clickhouse/_data` | 13.01 GiB | Active ClickHouse telemetry data; preserve |
| `/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs` | 14.53 GiB | Container runtime layers; inspect image/container ownership before cleanup |
| `/var/lib/xgs/incidents` | 8.41 GiB | Application incident records; preserve pending owner retention policy |
| `/root` | 7.92 GiB | Includes Dux canary/qualification workspaces and Rust toolchain/cache |
| `/usr` | 6.14 GiB | Installed system software |
| `/var/lib/docker/volumes/be65c16…/_data/store` | 5.70 GiB | Active Docker volume (`LINKS=1`); preserve |

`docker system df -v` showed a 13.81 GiB `signoz-clickhouse` volume and a
5.947 GiB volume with one container link. It also reported 6.402 GB of
reclaimable images and 1.955 GB of reclaimable containers. Those figures are
Docker's reclaimability classification, not proof that the images or stopped
containers are unwanted; they were left untouched.

The `/root` subtree includes `dux-canary-e96e708` (1.62 GiB),
`dux-canary-120` (1.31 GiB), `dux-qualification-43a7769` (1.08 GiB), Cargo
registry (about 922 MiB), and Rust toolchain (about 842 MiB). The canary and
qualification directories may hold rollback/evidence material, so they were
not deleted without an inventory and retention decision.

## Cleanup performed

Removed Docker volume `act-toolcache` after confirming it had no attached
containers (`docker ps -a --filter volume=act-toolcache` returned none). It
contained Go 1.25.0 and Node 20.20.2 tool caches and occupied 409 MiB. Docker
can recreate tool caches as jobs request them. The volume was removed with
`docker volume rm act-toolcache`; root free space increased from 5.8 GiB to
6.2 GiB in rounded `df` output.

At 01:59 UTC, after the tmpfs-backed fanotify smoke completed and all test
artifacts were removed, root measured 83 GiB used / 6.0 GiB available (94%);
`/dev/shm` measured 189 MiB used / 2.9 GiB available. The test filesystem
image was backed by tmpfs and did not consume root-disk space.

## Operational note and follow-up

At the time of inspection, `dux` was active and the host also had ClickHouse,
Docker/containerd, and other application services running. The guarded
disposable-VM qualification runner remained blocked by recurring Go build/test
workloads on the workstation host; it did not install a candidate on VPS2.

VPS2 remains space-constrained at 94% used. Before removing more data, obtain
the retention/ownership policy for `/var/lib/xgs/incidents`, ClickHouse
retention settings, and the Dux canary/rollback workspaces. Next safe options
are to inspect age/retention policies and identify unused Docker artifacts by
owner; do not prune active data volumes or rollback evidence based only on
Docker's reclaimable estimate.

## Follow-up inventory — 2026-11-10 20:34 UTC

A read-only follow-up found `/var/tmp` at **5.3 GiB** before cleanup. Three
inactive release-output directories for Dux 0.6.3 builds 98, 99, and 100 were
checked against their `SHA256SUMS` files; every listed package, binary, source
archive, manifest, and audit artifact passed. Qualified build 99 and 100
manifests and lifecycle results are preserved under [`package-lifecycle-20261108`](package-lifecycle-20261108/); build 98 was
a baseline output, not a qualified release. The three regenerated output
directories (about **100 MiB allocated**) were removed after
confirming no process referenced them. `/var/tmp` then measured **5.2 GiB**.
Root remained at **91% used**, with **8.3 GiB available** at the 20:33 sample;
rounding and concurrent writes mean the displayed free-space change did not
track the removed bytes exactly.

The remaining `/var/tmp` entries include toolchain material needed to build
(Dux Cargo/Rust caches and a 579 MiB release-tools archive), old failed test
workspaces intentionally preserved for reproducibility, and a modified
`claude-dux` clone with 14 local changes. These were retained. The active r132
soak binary and report were also retained.

At 20:34, top-level Dux state occupied **1.3 GiB**; `dux.db` was 846,786,560
bytes and its WAL 60,110,832 bytes. `/var/log` occupied **4.5 GiB**, including
4.1 GiB of journal data. Docker reported 18.05 GB of images (7.074 GB classified
as reclaimable), 17.86 GB of active local volumes, and only 1.119 MB reclaimable
volumes. ClickHouse was using about 193% CPU and 1.34 GiB memory; its volume is
active. These are host/application-owned data stores. Do not vacuum journals,
prune images, or delete volumes without retention and service-owner approval.

At the same observation, the installed Dux service had zero restarts and its
status reported a full observation in progress, zero overflow/drop/pending
counts, and a 743-second age for the last completed scan. It was not caught up
during that observation. VPS2 CPU PSI some avg10 was 17.54% and I/O PSI some
avg10 was 4.67%; nested VM qualification was not started under the host's current
contention and 91% disk use.


## Build-cache cleanup — 2026-11-10 20:43 UTC

Two inactive musl Cargo build-cache directories (`dux-musl-build-0b81fa8` and
`dux-musl-build-2b02502`, 239 MiB each) were removed after confirming no matching
build process was running. Their generated outputs under `/var/tmp/dux-musl` and
the static-musl qualification records in the repository were retained; the
removed contents were rebuildable `target` caches. `/var/tmp` fell from 5.2 to
4.7 GiB. Root measured 90% used with 9.2 GiB available. The VPS2 CPU PSI some
avg10 was 27.54%, so no additional test VM was started. The active r132 soak
was left untouched.


## Old Rust cache cleanup — 2026-11-10 20:46 UTC

Removed the unused `/var/tmp/dux-perf-target-baseline` Cargo target tree (249
MiB; no executable was present) and only the `deps`, `build`, `.fingerprint`,
and `incremental` caches from `/var/tmp/dux-production-build-0b81fa8/release`
(240 MiB). No Cargo or rustc process was active. The separately built `dux`
binary, source/reports, Cargo/Rust toolchains, and failure evidence were kept.
`/var/tmp` fell from 4.7 to 4.2 GiB; root measured 90% used with about 9.6 GiB
available. CPU PSI some avg10 was 44.99%, so no new test VM was started.

## Follow-up — 2026-11-10 20:55 UTC

A read-only SSH check over port 22 measured root at 90% used, with
10,273,873,920 bytes available (about 9.57 GiB). The point-in-time allocation
was:

| Path or resource | Measured size | Assessment |
|---|---:|---|
| `/var/lib/dux` | 909,860,864 bytes | Database 849,592,320 bytes, WAL 60,110,832 bytes, SHM 131,072 bytes; not the source of multi-GiB growth |
| `/var/log` | 4,789,362,688 bytes | Journald is 4,295,532,544 bytes (4.00 GiB); retain until journal policy/retention is checked |
| `/var/tmp` | 4,487,704,576 bytes (4.18 GiB) | Includes toolchains, Cargo cache, qualification artifacts, and preserved failure evidence |
| Docker images | 18.13 GB reported | 7.074 GB reported reclaimable; no prune performed because image ownership/rollback use is not established |
| Docker volumes | 17.85 GB reported | 9 active; preserve application data |
| Docker containers | 2.145 GB reported | 20 active; preserve |

The Dux database and WAL were checked while the r132 soak was active; the soak
fixture is on `/dev/shm`. The disk view does not explain the earlier short-term
change in `df` by itself, but at this sample the Dux database was under 1 GiB.
The largest named `/var/tmp` item was the Rust toolchain (about 0.82 GiB),
followed by Cargo cache (about 0.98 GiB). The 617 MiB
`dux-readiness-release116` tree contains a preserved qualification failure
workspace with a 2 GiB logical, approximately 578 MiB allocated ext4 image.
It is evidence for a failed run and was retained.

No data was deleted in this follow-up. Journals, Docker images/volumes, Rust and
Cargo caches, and qualification failure artifacts have different retention or
rebuild costs; use their owners' retention policy before pruning.
