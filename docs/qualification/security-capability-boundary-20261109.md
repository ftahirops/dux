# Dux capability boundary — 2026-11-09

## Finding

The current whole-filesystem event watcher and FID resolver require two
privileges in the daemon:

| Capability | Current operation | Consequence if removed |
|---|---|---|
| `CAP_SYS_ADMIN` | `fanotify_init` plus `FAN_MARK_FILESYSTEM` marks for each filesystem below the configured root | The daemon cannot establish its current filesystem-wide event coverage. On Linux, unprivileged fanotify groups are restricted; filesystem/mount marks are not available to them. |
| `CAP_DAC_READ_SEARCH` | `open_by_handle_at` for resolving fanotify file handles to paths | Event handles cannot be reopened to resolve paths through protected directories. Dux's own diagnostics expect `EPERM` and identify failed resolution rather than treating it as healthy tracking. |

This is a real privilege cost of the current architecture, not merely a
systemd-analyzer false positive. Dropping either capability without changing
the collection/resolution design would lose a required behavior or make the
index visibly incomplete. Do not remove either capability as a packaging-only
hardening change.

## Source evidence

- [`packaging/dux.service`](../../packaging/dux.service) limits the bounding
  and ambient sets to these two capabilities, sets `NoNewPrivileges=yes`,
  locks securebits, restricts address families to AF_UNIX, denies BPF and
  unrelated syscall groups, and documents why private mount namespaces are
  incompatible with host-wide filesystem marks.
- [`src/watch.rs`](../../src/watch.rs) calls `fanotify_init`, creates
  `FAN_MARK_FILESYSTEM` marks for mounted filesystems, and resolves FIDs with
  `open_by_handle_at`.
- The package unit syntax and systemd's offline hardening heuristic passed in
  [`systemd-security-review-20261109`](systemd-security-review-20261109/README.md).
  Its 3.3 “OK” score is not a security certification and does not eliminate
  the capability risk.

## Kernel interface references

- [`fanotify_init(2)`](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)
  documents the privilege restrictions on fanotify groups and marks.
- [`open_by_handle_at(2)`](https://man7.org/linux/man-pages/man2/open_by_handle_at.2.html)
  documents the `CAP_DAC_READ_SEARCH` requirement.

## Required follow-up before calling privilege exposure resolved

1. Threat-model a compromised daemon with these capabilities and the full
   host's filesystem namespace; include the path-name index's sensitive data.
2. Evaluate a privilege-separated collector/resolver design. The privileged
   component should expose only a narrow, authenticated protocol and validate
   all handles, filesystem identities, and request rates. Measure whether this
   actually reduces the impact of compromise; splitting processes alone is not
   a security boundary if the unprivileged daemon can request arbitrary opens.
3. Re-run capability-negative tests and the full filesystem coverage,
   event-resolution, package lifecycle, and production-like qualification after
   any design change. Verify the effective merged systemd unit on supported
   distributions, not only this standalone unit file.

Until that work is completed, record the capability exposure as an accepted
but unresolved design risk. The present evidence supports least privilege
within the existing design; it does not establish that Dux is safe against a
compromised privileged daemon.
