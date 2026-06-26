# dux — security notes

## Terminal output is escaped
Filenames are attacker-controlled (any local user can create them). All paths
printed by the CLI and rendered by the TUI pass through `util::display_path` /
`util::display_name`, which escape C0/C1 control characters, DEL, ESC, and
newlines as visible `\xNN` / `\n`. This prevents a crafted filename from
injecting ANSI/OSC escape sequences (e.g. OSC 52 clipboard writes) or forging
terminal output for an admin running `dux`.

## Index exposure (filename disclosure)
`/var/lib/dux/dux.db` contains the **full filename list of the root
filesystem**, including names inside directories an unprivileged user could not
otherwise traverse. By default it is world-readable (0644) so unprivileged
`dux find` / `dux status` work — dux is a companion to user-facing tooling.

This is a deliberate, documented information-disclosure tradeoff. To make the
index **root-only**, set in `dux.service`:

```
StateDirectory=dux
StateDirectoryMode=0700
UMask=0077
```

The daemon then creates `dux.db` with mode 0600 and unprivileged users must
query through a privileged wrapper.

## Privileged service hardening
The daemon runs as root with `CAP_SYS_ADMIN` (required by fanotify) and
`CAP_DAC_READ_SEARCH` (read every file). It **cannot** use the mount-namespace
sandboxing options (`ProtectSystem`, `ProtectHome`, `PrivateMounts`,
`ReadOnlyPaths`, …): a fanotify filesystem mark inside a private mount namespace
does not observe writes happening through the host's mounts.

`dux.service` therefore applies every hardening directive that is *compatible*
with the host mount namespace: `CapabilityBoundingSet` pinned to the two needed
caps, `NoNewPrivileges`, `MemoryDenyWriteExecute`, `RestrictNamespaces`,
`RestrictRealtime`, `RestrictSUIDSGID`, `LockPersonality`, `SystemCallArchitectures=native`,
and the `Protect*` options that don't require a private mount namespace.

Alert commands (`--alert-exec`) are spawned via `sh -c`, tracked, reaped, and
capped at 16 concurrent processes so an event storm cannot fork-bomb the host.

## Accepted dependency advisories
Two **transitive** advisories come in through `ratatui 0.28.1` and are not
fixable without an upstream `ratatui` release that upgrades them:

- `RUSTSEC-2026-0002` — `lru 0.12.5` `IterMut` soundness. dux's read-only TUI
  does not use the affected `IterMut` path; ratatui uses `lru` only as a small
  internal render cache. Patched in `lru >= 0.16.3`, which `ratatui 0.28`/`0.29`
  do not yet require.
- `RUSTSEC-2024-0436` — `paste 1.0.15` unmaintained. No known vulnerability;
  compile-time-only proc-macro.

Both are tracked for resolution when `ratatui` ships a release that bumps them.
