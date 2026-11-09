# dux operations runbook

This runbook describes health checks and recovery for the Linux systemd
installation. Run read-only checks first. `caught_up` means the event pipeline
has no known work or uncertainty; it does not independently prove that indexed
paths and byte totals match the filesystem. After a suspected accounting
incident, require an exact reconciliation/observer result as well.

## Health check

```sh
sudo systemctl is-active dux.service
sudo systemctl show dux.service -p ActiveState -p SubState -p NRestarts -p ExecMainStatus
sudo dux --json status
sudo journalctl -u dux.service --since '-15 min' --no-pager
```

Healthy steady state requires all of the following:

- `systemctl` reports `active`; restart count is not unexpectedly increasing.
- Status reports `daemon_live: true`, `watch_status_available: true`,
  `caught_up: true`, `dirty: false`, `paused: false`, and
  `scan_in_progress: false`.
- `pending_updates`, `events_dropped` (known userspace backlog entries), and
  `kernel_overflow_notifications` are zero; `write_error` is null. A nonzero
  kernel overflow count records loss episodes; fanotify does not reveal the
  exact number of notifications lost in each episode.
- In releases that expose the reconciliation circuit, inspect
  `automatic_reconciliation_paused` and `failed_reconciliation_attempts` too.
  This is separate from `paused`, which reports a transient pressure pause on
  writes. A true circuit field means automatic full scans will not restart
  until an operator requests one after resolving the failure cause; null means
  the client or index predates this status field. A corrupt retry counter must
  remain visible as a fail-closed paused circuit.
- `last_scan_age_seconds` is reasonable for the host's workload and there are
  no recurring accounting, queue-overflow, permission, or storage errors in
  the journal.

A nonzero `behind_seconds`, rising pending count, dirty/paused state, missing
watch status, dropped events, or a stale heartbeat means the index is not
currently trustworthy as a complete live view **in event-apply mode**. In
scan-first mode, event batches are hints and are intentionally not applied to
the index; `behind_seconds` is not an index freshness metric and is omitted
from Prometheus metrics. JSON status reports `behind_seconds_applicable: false`.
Use `last_scan_age_seconds` for JSON status and
`dux_last_scan_age_seconds` plus `dux_scan_first_mode` and
`dux_last_scan_available` in Prometheus metrics to assess scan-first freshness.
`caught_up` remains false in scan-first mode by design. Require valid JSON with
`status_version: 1`; if `dux --json status` prints human-readable text or JSON
parsing fails, classify health as **unknown**, not healthy. Older clients do not
expose enough machine-readable state to distinguish a drained event queue from
an active rebuild or uncertain accounting. Do not infer daemon health from
systemd `StatusText` alone; it can lag the database-backed status. Upgrade to a
qualified release or use its independent live observer before treating the
index as current. `dux metrics` emits Prometheus text exposition suitable for
the node_exporter textfile collector. In event-apply mode, alert on pipeline
unavailable, dirty/paused, dropped events, and sustained backlog. In scan-first
mode, `dirty` and `caught_up=0` are expected while changes await the next full
observation; alert on missing scan timestamps, scan age beyond the configured
freshness SLO, repeated scan/accounting failures, or resource exhaustion instead.

## Triage and recovery

1. Capture evidence before changing state:

   ```sh
   date -u
   sudo systemctl status dux.service --no-pager
   sudo systemctl show dux.service -p NRestarts -p ExecMainStatus -p MemoryCurrent -p CPUUsageNSec
   sudo dux --json status
   sudo journalctl -u dux.service --since '-1 hour' --no-pager
   df -h /
   df -i /
   ```

   Save command output with the incident record. Do not delete the database,
   WAL, or shared-memory files while the daemon is running.

2. Check the cause indicated by `pause_reason`,
   `automatic_reconciliation_paused`, `dirty_reason`, `write_error`, and the
   journal. Resolve disk/inode exhaustion, filesystem availability,
   permissions, event loss, or memory pressure before asking the daemon to
   rebuild.

3. For a one-off recoverable outage, request a reconciliatory scan only after
   its cause is resolved and host memory/storage pressure has returned to
   Normal. With the packaged default database, this asks the live daemon to
   rebuild atomically while preserving the previous readable index during the
   build. If accounting mismatches repeatedly trigger full-root scans, the
   queue repeatedly overflows, or host pressure remains elevated, do not queue
   another scan or restart; follow **Host load and I/O pressure** below and keep
   the incident open:

   Qualified releases that implement the persisted reconciliation circuit report
   when automatic scans have paused after repeated failures. After the cause is
   fixed and host pressure is Normal, request one explicit scan to reopen the
   circuit, then verify exact filesystem equality. Do not use this as a retry for
   an unresolved mismatch. Older releases may not have this circuit and can keep
   rescanning; do not wait for automatic recovery on those builds.

   ```sh
   sudo dux scan /
   watch -n 5 'sudo dux --json status'
   ```

   The daemon-driven `dux scan` client must observe either live scan progress or
   a new scan generation within 60 seconds. If it reports that the request may
   still be queued, inspect status and logs before requesting another scan; the
   client timeout does not cancel the daemon's request. After progress starts,
   the existing 30-minute overall deadline still covers reconciliation, event
   replay, and accounting verification.

   Wait for `scan_in_progress: false`, then require live daemon/watch status,
   no dirty/paused state, zero pending updates, and `caught_up: true`. A clean
   status is necessary but is not by itself an exact accounting proof after a
   known drift incident.

   For a suspected accounting mismatch, run the staged independent observer
   under resource limits after the scan. It reads the database read-only,
   compares root totals and every directory rollup, checks SQLite integrity,
   and records both the status and accounting snapshots. Root totals matching
   inode aggregates is not sufficient: opposing errors in two directory
   rollups can cancel at the root. Require zero rollup mismatches even when
   status says caught up and the global totals agree. The observer's parser must
   be from the qualified candidate so it can interpret versioned machine status;
   do not treat legacy human-readable status as proof of catch-up:

   ```sh
   stamp=$(date -u +%Y%m%dT%H%M%SZ)
   candidate_binary=/path/to/qualified/dux-candidate
   sudo systemd-run --wait --collect --unit="dux-accounting-observer-$stamp" \
     --property=CPUQuota=20% --property=MemoryMax=512M --property=MemorySwapMax=0 \
     --property=TimeoutStartSec=15min \
     /usr/bin/python3 /usr/local/libexec/dux-canary/dux-live-observer.py \
     --binary "$candidate_binary" --service-binary /usr/bin/dux \
     --db /var/lib/dux/dux.db --output "/run/dux/accounting-observer-$stamp.json" \
     --exact-filesystem --stable-catchup-before-exact 30 --catchup-timeout 900
   ```

   This exact mode waits for a stable caught-up window, then independently
   compares every path, inode identity, allocated block count, directory
   rollup, history invariant, and SQLite integrity against the filesystem.
   Exit 0 means the window held and the exact comparison passed. Exit 1 means
   the index never held a stable window, the filesystem walk changed during the
   comparison, an invariant failed, or the observer encountered an error;
   preserve the report and treat the incident as open. A weaker point-in-time
   diagnostic can omit `--exact-filesystem` and the stable-window option; its
   exit code 2 means a mismatch was observed and later repaired, which is useful
   incident evidence but is not a stable exact pass. During a canary, the
   candidate binary path is `/usr/bin/dux`; before the upgrade, point it at the
   exact staged candidate executable. The report is stored under `/run/dux`, not
   beside the SQLite database. Copy it to the approved incident evidence store
   after capture; it contains filesystem path names and is sensitive metadata.

4. If the daemon is stopped, preserve logs and status first, then restart it:

   ```sh
   sudo systemctl restart dux.service
   sudo journalctl -fu dux.service
   ```

   Wait for the initial scan/reconciliation to finish and apply the health
   checks above. Repeated restart loops, repeated queue overflow, or recurrent
   accounting mismatch are release blockers; capture evidence and roll back
   using the release's tested package procedure rather than repeatedly
   restarting.

## Host load and I/O pressure

When the host becomes unresponsive or its load average rises sharply, capture
host-wide pressure and per-process activity before restarting services or
rebooting, if the machine still accepts commands:

```sh
date -u
uptime
nproc
free -h
df -h /
vmstat 1 5
cat /proc/pressure/cpu /proc/pressure/io /proc/pressure/memory
iostat -xz 1 5
pidstat -dru 1 5
systemd-cgtop -b -n 3 --depth=2
systemctl show dux.service -p ActiveState -p NRestarts -p MemoryCurrent \
  -p ControlGroup -p CPUUsageNSec -p CPUQuotaPerSecUSec -p IOAccounting \
  -p IOReadBytes -p IOWriteBytes
cg=$(systemctl show dux.service -p ControlGroup --value)
cat "/sys/fs/cgroup$cg/cpu.max" "/sys/fs/cgroup$cg/cpu.stat" \
  "/sys/fs/cgroup$cg/cpu.pressure"
```

Compare at least two samples. Load average alone does not identify the cause;
CPU PSI shows runnable-task contention, I/O PSI shows tasks stalled on storage,
and device utilization/latency plus `pidstat` help distinguish a busy device
from one process issuing reads or writes. A low Dux CPU quota limits its CPU
use but does not by itself bound its total I/O or prove another process is
uninvolved. Record Dux's journal alongside the samples, especially
reconciliation start/completion, accounting mismatch, queue overflow and
pressure deferral messages. If the host was already rebooted, inspect both
boots with `journalctl --list-boots`, `journalctl -b -1 -u dux.service`, and
`journalctl -k -b -1`; an orderly Dux SIGTERM confirms shutdown handling, not
the exact operator action or the cause of the preceding load.

On cgroup v2, compare `cpu.stat` samples: `usage_usec` is consumed CPU time,
while `nr_throttled` and `throttled_usec` show how often and how long the unit
hit its `cpu.max` limit. A high throttled-period fraction with a growing Dux
event backlog means the service has exhausted its own CPU budget; inspect host
CPU PSI and competing workloads before changing the quota. The unit's lifetime
`CPUUsageNSec` and `nr_throttled` values are counters, so use timestamped deltas
to measure the current interval.

If repeated short samples show that the Dux cgroup consumes its full CPU quota,
the watcher counters do not advance, and cgroup `io.stat` has no read/write
deltas, classify this as a **CPU-bound no-progress incident**. Preserve status,
the exact executable checksum/build ID, and timestamped `cpu.stat`/`io.stat`
samples. If `perf` is available and host policy permits profiling, collect one
short sample (a few seconds) rather than leaving a profiler attached:

```sh
pid=$(systemctl show dux.service -p MainPID --value)
stamp=$(date -u +%Y%m%dT%H%M%SZ)
sha256sum /proc/$pid/exe
readelf -n /proc/$pid/exe | grep -m1 'Build ID'
sudo perf record -F 49 -g -p "$pid" -o "/run/dux/stall-$stamp.data" -- sleep 3
sudo perf report --stdio -i "/run/dux/stall-$stamp.data" \
  > "/run/dux/stall-$stamp.txt"
```

The profile is diagnostic evidence, not a root cause by itself. A stripped
binary can leave Dux frames as raw offsets; do not assign them to source
functions without matching symbols/source. Record competing processes and host
CPU PSI separately, preserve the index, and keep the incident open until watcher
progress resumes and exact accounting passes. Do not raise the quota or start a
full scan solely because the service is throttled.

The packaged unit enables `IOAccounting=yes`; `IOReadBytes` and `IOWriteBytes`
are cumulative cgroup counters. Take two timestamped samples and compare their
deltas. If either property is blank, the effective unit lacks I/O accounting;
check `systemctl show dux.service -p IOAccounting -p ControlGroup` and inspect
the service's cgroup `io.stat` directly. Device-wide IOPS alone cannot attribute
traffic to Dux. On stacked storage (for example, device mapper over a physical
disk), cgroup counters may include accounting at more than one device layer;
do not treat their sum as physical-device traffic. Compare successive deltas
with each `io.stat` device line and the underlying device metrics.

If the journal shows the periodic accounting verifier repeatedly launching
full-root reconciliations after each scan, do not queue another manual scan or
restart loop as an I/O diagnostic. Preserve the database and logs, track the
active scan and host pressure, and establish the repair against a qualified
candidate in an isolated environment before changing the production service.
Repeated reconciliation, fanotify overflow, or a DIRTY/BEHIND index remains a
correctness incident even when the host itself is responsive. A quiet device
sample after reboot does not prove the index is exact or establish a single
cause for the earlier load.

## Host-wide memory OOM

One-off direct scans set `oom_score_adj=800` so a large foreground scan is a
preferred victim during a **global** host OOM. The long-lived daemon keeps the
neutral systemd value `OOMScoreAdjust=0`; its memory is still bounded by the
service's `MemoryHigh`/`MemoryMax`, while the kernel chooses OOM victims from
their actual memory use instead of an artificial Dux bias. This distinction is
important: an earlier daemon binary overrode systemd's neutral setting to `+800`
and was repeatedly selected at low resident memory, causing restarts and stale
index periods. The package lifecycle qualification checks both the unit
property and the live process value. Confirm them with the kernel journal and
process state before diagnosing an OOM restart:

```sh
sudo journalctl -k --since '-15 min' --no-pager |
  grep -E 'oom-kill:|Out of memory: Killed process'
sudo systemctl show dux.service \
  -p ActiveState -p SubState -p NRestarts -p Result -p OOMScoreAdjust -p MainPID \
  -p MemoryCurrent -p MemoryPeak
pid=$(systemctl show dux.service -p MainPID --value)
cat "/proc/$pid/oom_score_adj"
sudo dux --json status
```

After an OOM restart, the daemon marks an index it did not build as dirty,
reattaches watches, and requests a background reconciliation. If the pressure
guardian sees Elevated or Critical host pressure before the rebuild starts, it
defers that full-root work while keeping event watches active; the bounded event
backlog is retained and incrementally written when possible, then replay drains
before reconciliation is retried after non-CPU pressure remains lower for ten
seconds. This matches the kernel PSI `avg10` window and prevents scan/cancel
thrashing around a fluctuating threshold. CPU-only Elevated pressure permits a
low-priority reconciliation because CPU contention can persist while memory and
storage have headroom. If memory, disk, or I/O pressure
reaches Elevated, or pressure reaches Critical for any reason during a rebuild,
the scan is cancelled cooperatively before atomic installation, its private
staging files are removed, and the previous index remains installed. CPU-only
Elevated pressure does not cancel an already running low-priority scan: CPU PSI
can reflect that scan being throttled, and cancelling it repeatedly prevents
recovery. New scans still wait for Normal pressure unless CPU PSI alone is
Elevated. Incremental updates continue
under Elevated pressure; Critical pressure pauses writes too. The service status
reports the cancellation/defer state. Restart
systemd 254 and newer ramp restart delay from 10 seconds to a one-minute cap;
older systemd versions ignore the ramp settings and use the fixed 10-second
fallback. A running PID, `active` state, drained queue, or
retained old snapshot does not mean recovery finished. Relieve host memory/I/O
pressure and wait for the scan and event replay to complete; if OOM repeats,
keep the incident open and do not start another whole-root diagnostic scan.
Require the exact observer from the previous section before calling accounting
recovered.
The daemon's neutral score is an explicit availability tradeoff, not a promise
that Dux cannot be killed during global OOM. If the host cannot sustain the
service within its cgroup budget, defer reconciliation and resolve host memory
pressure; do not raise the daemon score to hide a capacity failure.

Memory pressure classification uses the host's `MemTotal`: the Elevated
available-memory floor is one sixteenth of total memory, clamped between 256 MiB
and 1 GiB. Critical memory pressure remains at or below 256 MiB. This keeps
large hosts from starting expensive reconciliations with little remaining RAM
while allowing small qualification guests to scan when they still have a safe
fraction of their own memory available.

## Storage and index integrity

The default index is `/var/lib/dux/dux.db`; it contains path names from the
indexed filesystem and is intentionally restricted to root and the `dux`
group. Do not publish it in support bundles without treating it as sensitive
metadata. Do not remove it as a first-line repair: it contains growth history.

Before planned package maintenance, create a SQLite online backup and retain its
manifest, checksum, and restrictive permissions. DEB and RPM packages install
`/usr/lib/dux/dux-backup-index.py` and depend on Python 3. The helper uses
SQLite's online backup API to include committed WAL frames, checks the snapshot,
refuses to overwrite an existing backup, and requires an additional 1 GiB of
free space after the estimated copy size. It writes a mode-0600 database and
JSON manifest. Use the helper from the exact installed package version.

Example (choose a unique output name and keep the directory root-only):

```sh
sudo install -d -o root -g root -m 0700 /var/backups/dux
sudo python3 /usr/lib/dux/dux-backup-index.py \
  --database /var/lib/dux/dux.db \
  --output /var/backups/dux/dux.db.pre-upgrade-20261101.sqlite
sudo sha256sum /var/backups/dux/dux.db.pre-upgrade-20261101.sqlite
```

Do not use a plain copy of a live SQLite database as a backup. Validate backups
before relying on them. After an unplanned stop, preserve the database and
sidecars for diagnosis; SQLite integrity checks establish database consistency,
not filesystem accounting correctness.

### Corrupt or unreadable index

The daemon now fails closed when SQLite cannot open or inspect an existing
index. It does not classify corruption as an ordinary schema upgrade, because
the rebuild path must validate and preserve the previous index's growth history.
Do not repeatedly restart the service or run `dux scan` against that same
database: both paths validate the existing source and will refuse an unreadable
database. The source, WAL, SHM, and rollback journal may contain complementary
recovery evidence; do not remove or replace any of them.

Capture service status and logs, then follow the incident process to stop writes
cleanly and preserve the database plus all sidecars with their ownership,
permissions, sizes, and hashes. Prefer restoring a previously created and
validated SQLite online backup. Run `PRAGMA quick_check` on the restored copy,
then reconcile it against the filesystem with the independent accounting
observer before trusting it. A clean SQLite check alone does not establish
correct filesystem totals.

If no validated backup exists, keep the damaged files intact and escalate for a
reviewed recovery plan. This release has no supported in-place command to
discard corrupt index history and create a replacement. Do not rename, delete,
or overwrite the only source as an improvised recovery step. A replacement
index built at a separate path can support diagnosis, but is not an approved
production cutover until its root, coverage, exact accounting, service
configuration, and rollback procedure have been reviewed.

## Installed package integrity

Before a canary or package transaction, confirm that the package manager's
recorded version, installed executable, and release manifest agree. This
catches a manually replaced or partially upgraded executable before rollback
assumptions are used.

For Debian packages:

```sh
dpkg-query -W -f='${Package} ${Version} ${Status}\n' dux
sudo dpkg -V dux
sha256sum /usr/bin/dux /usr/lib/systemd/system/dux.service
```

For RPM packages:

```sh
rpm -q --qf '%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\n' dux
sudo rpm -V dux
sha256sum /usr/bin/dux /usr/lib/systemd/system/dux.service
```

Compare executable and unit hashes with the selected release and rollback
manifests. Treat a package version that differs from `dux --version`, a
verification report for `/usr/bin/dux`, or a hash mismatch as a deployment
blocker. Preserve the current database and logs, capture a verified SQLite
backup, and use the qualified package transaction to reconcile package state;
do not replace the live executable with an ad hoc file copy. Capture service
enablement, active state, restart count, and machine-readable status before and
after reconciliation.

## Package upgrade service state

Before an upgrade, record whether `dux.service` is enabled and active:

```sh
systemctl is-enabled dux.service || true
systemctl is-active dux.service || true
```

Some legacy package maintainer scripts disable the unit during upgrade. After
installing a qualified package, restore the intended state explicitly. If the
service was enabled before the upgrade:

```sh
sudo systemctl enable dux.service
sudo systemctl start dux.service
```

If it was disabled before the upgrade, leave it disabled and do not start it
unless the host operator intends to activate it. In either case, check
`systemctl is-enabled`, `systemctl is-active`, and the versioned
`dux --json status` after the package transaction. Wait for catch-up and run
the independent accounting observer before declaring recovery. Keep the
pre-upgrade enablement and post-upgrade state in the deployment record.

## Safe operating constraints

- Keep exactly one writer for an index. Do not use `dux scan --force` while the
  daemon is active; the per-database lock is a safety boundary, not a normal
  operating mode.
- The indexer requires `CAP_SYS_ADMIN` for fanotify and
  `CAP_DAC_READ_SEARCH` to read protected paths. Do not grant broader
  capabilities without a reviewed need. Keep the optional tunnel service
  disabled unless explicitly configured and approved.
- Do not add systemd settings that create a private mount namespace for the
  indexer (`PrivateMounts`, `ProtectSystem`, `ProtectHome`, or similar): its
  filesystem marks must observe host mounts.
- A newly mounted filesystem may require a supervised daemon restart and a
  baseline rebuild so the watch set matches current mount topology.
- The index is a filesystem view, not a deletion recommendation. Review
  ownership, open handles, retention policy, and backups before removing data.

## Incident record

Record UTC start/end, package version and binary SHA-256, kernel/systemd and
filesystem/mount details, status JSON before and after recovery, relevant
journal lines, disk/inode and cgroup pressure, scan duration, exact observer
result (if accounting drift is suspected), backup checksum, and rollback
result. Never call an incident resolved solely because the daemon restarted or
printed a caught-up message.
