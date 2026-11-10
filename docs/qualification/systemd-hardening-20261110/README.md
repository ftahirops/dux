# Dux systemd hardening review — 2026-11-10

The installed VPS2 unit and the r126 Debian package unit have the same SHA-256:
`e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.
`systemctl show` reported no drop-in overrides. On systemd 255,
`systemd-analyze security --no-pager dux.service` reported overall exposure
**3.3 (“OK”)**. This is a policy checklist score, not a security certification.
The captured analyzer output and effective unit properties are in this folder.

## Strengths observed

- The capability bounding and ambient sets contain only `CAP_SYS_ADMIN` and
  `CAP_DAC_READ_SEARCH`; `NoNewPrivileges=yes` and `SecureBits=noroot` are set.
- A syscall deny filter blocks mount, module, BPF, reboot, clock, swap, debug,
  raw-I/O, and obsolete syscall groups; network families are restricted to
  `AF_UNIX`.
- Core dumps, namespace creation, realtime scheduling, writable executable
  memory, kernel tunables, and control-group writes are restricted.
- The service runs with low CPU/I/O weights and a 100% one-core CPU quota.

## Findings and tradeoffs

The analyzer flags UID 0, ambient capabilities, the DAC-read/search capability,
and access to devices/modules. The unit intentionally avoids private mount,
home, and device views because Dux must index the host's visible mount tree,
including `/dev` and `/usr/lib/modules`; it also needs fanotify and
`open_by_handle_at`, which prevent denying all `@privileged` syscalls. These
choices constrain hardening without changing Dux's observation scope.

Running as the dedicated `dux` UID with the same two ambient capabilities may
reduce the impact of a daemon compromise, but it has **not been validated**.
Before changing the package, verify full-tree read/search coverage, fanotify
marks, handle resolution, index/runtime directory ownership, lifecycle scripts,
and rollback on a disposable guest. The current unit deliberately remains root
with a minimal capability set until that evidence exists.

**Follow-up:** package and unit source was subsequently changed to run as the
dedicated `dux` UID. Its source checks and limits are recorded in
[`nonroot-service-followup.md`](nonroot-service-followup.md). This change is
not yet built into an authenticated release or runtime-qualified; production
still runs the root-owned installed unit described above.

## Mount-namespace behavior to qualify — 2026-11-10 19:59 UTC

A read-only check found that the running service's mount namespace differs from
the host's (`mnt:[4026532520]` vs. `mnt:[4026531841]`). The installed unit has
`ProtectKernelTunables=yes` and `ProtectControlGroups=yes`, while
`PrivateMounts=no`. A transient `systemd-run` probe with those two protection
properties also entered a distinct mount namespace (`mnt:[4026532817]`). The
Dux watcher currently uses `FAN_MARK_FILESYSTEM` at `src/watch.rs:4762`.

This namespace difference alone did **not** establish a Dux defect. A controlled test has since passed for ext4 using the exact r132 binary, non-root credentials, matching capability masks, and the packaged syscall and protection settings: host-namespace create, rename, and delete events were reflected in Dux while its mount namespace differed from the host. This resolves the ext4 namespace-visibility question for this configuration; it does not qualify every filesystem, the package-created UID, or the complete package lifecycle. Do not remove hardening properties based on the earlier namespace observation.

Machine-readable capture: [`mount-namespace-observation-20261110T1959Z.json`](mount-namespace-observation-20261110T1959Z.json).

### Cross-namespace runtime result — 2026-11-10 20:05 UTC

The exact r132 binary ran in a transient systemd unit with the packaged security and syscall restrictions, `ProtectKernelTunables=yes`, `ProtectControlGroups=yes`, non-root UID 65534, and exactly `CAP_SYS_ADMIN | CAP_DAC_READ_SEARCH`. Its mount namespace differed from the host namespace. A host-side create was indexed in 187 ms; host-side rename and delete were also reflected. The daemon saw zero queue overflows or drops. The 60-second runtime cap then sent SIGTERM; the daemon logged that it drained and flushed before exit. The unit, fixture, and copied binary were removed; the installed production service stayed active.

The transient used event-apply compatibility mode to make event visibility observable as path updates. The production default remains reconciliation-first, but both modes use the same filesystem mark and event reader. This was one ext4 filesystem and a temporary unit, not a package install or a cross-kernel test. See [structured result](cross-namespace-event-test-20261110T2004Z.json) and [raw run log](cross-namespace-event-test-20261110T2004Z.log).
