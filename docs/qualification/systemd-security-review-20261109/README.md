# Offline systemd service hardening check — 2026-11-09

The checked unit is `packaging/dux.service`, SHA-256
`e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`, matching
the unit hash expected by the current r123 candidate and the VPS2 r121 canary.

Commands on the qualification host (systemd 255.4-1ubuntu8.18):

- `systemd-analyze verify packaging/dux.service` — exit 0, no diagnostics.
- `systemd-analyze --offline=yes security --no-pager packaging/dux.service` —
  exit 0; heuristic exposure level **3.3 (OK)**. Full raw output is retained
  in `systemd-analyze-security.txt`.

The score is not a security certification. Findings include root execution,
ambient `CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`, and lack of `RootDirectory`;
these are tied to global fanotify coverage and reading protected filesystem
metadata. Dux also needs AF_UNIX for systemd notification. `ProtectHome`,
`ProtectSystem`, and private-device/mount restrictions can hide paths or alter
the filesystem view that Dux is required to index. The unit instead denies
unneeded capabilities and syscall groups, blocks BPF, restricts namespaces and
network families, enables `NoNewPrivileges`, and limits core dumps. `UMask=0027`
intentionally permits the `dux` group to read the path-name index while keeping
it private from other users.

This check validates the packaged unit's syntax and summarizes systemd's
heuristic. It does not inspect merged host drop-ins, prove the effective
runtime sandbox, or replace a threat model and capability review. No daemon or
service was modified or restarted by this check.
