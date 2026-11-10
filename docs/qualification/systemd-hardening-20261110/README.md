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
