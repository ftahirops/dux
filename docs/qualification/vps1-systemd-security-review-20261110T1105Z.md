# VPS1 effective systemd security review — 2026-11-10 11:05 UTC

This is a read-only review of the loaded production unit, not of a candidate
package. The installed package is `dux 0.6.3-66`; the active daemon remains the
older r66 binary. The effective unit at `/usr/lib/systemd/system/dux.service`
has SHA-256
`dd34d3086af76117b18b06a23c5c70ace4353ff6a33ae9b3fa3c10f13c04d920`, while
the current packaged unit `packaging/dux.service` has SHA-256
`e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`.

Observed effective controls:

- The process runs as UID 0, group `dux`, with only `CAP_SYS_ADMIN` and
  `CAP_DAC_READ_SEARCH` in its capability bounding and ambient sets.
- `SecureBits=3` (`noroot`, `noroot-locked`) and `NoNewPrivileges=yes` are
  active. Address families are restricted to `AF_UNIX`.
- The effective syscall deny filter blocks the broad unrelated groups but
  **does not deny `bpf`**. The checked-in package unit explicitly denies BPF,
  because `CAP_SYS_ADMIN` can authorize legacy BPF operations on older
  supported kernels. This is confirmed production-versus-candidate policy
  drift; do not describe the currently installed unit as having the candidate
  policy.
- Effective restart policy uses `RestartSec=10s`, six restart steps, and a
  one-minute maximum. The current package unit uses portable `RestartSec=30s`
  because Rocky 9 systemd 252 rejects the newer restart-step directives.
- `systemd-analyze security dux.service` reports heuristic exposure **3.3
  (OK)**. Its UMask finding says files are group-readable by default. At this
  sample, `/var/lib/dux` is `root:dux` mode 0750, the database and WAL are
  `root:dux` mode 0640, and NSS reports no members in group `dux`. The group
  read policy is intentional for index access but should be revisited if group
  membership changes.

`systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service`
passed with exit 0. This validates syntax of the candidate source units, not
the active production unit's security or successful package rollout. No
service, daemon, or configuration was changed or restarted. Full root identity
and the retained `CAP_SYS_ADMIN` remain material threat-model items.
