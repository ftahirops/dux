# Dux security and privilege review

## Scope and evidence

This review covers the static-musl `0.6.3-17` candidate whose executable SHA-256
is `919aac72338dc57bbc1b39f74ce4bd6abd2f801eb46c51f4bda1eae92a6fc522` and
whose package manifest is
`target/readiness/current-release-r17/build-manifest.json`. It is a source and
unit-file review, not a production deployment approval. At review time:

- `cargo audit` examined 231 dependencies from `Cargo.lock` SHA-256
  `c7d392c638fc891978b194097710538c4574f663d12357adc1cff6cf16b3d8b1`; it
  found zero known vulnerabilities and zero warnings using the advisory
  database updated 2026-10-27.
- `systemd-analyze verify` accepted `packaging/dux.service` and
  `packaging/dux-tunnel.service` with no diagnostics.
- `systemd-analyze security --offline` scored the indexer unit **5.1 MEDIUM**.
  Exact inputs, output hashes, and tool versions are recorded in
  `target/readiness/security-audit-current-20261027.json`.

These results are point-in-time evidence. They do not prove that all possible
vulnerabilities are absent or that the exact package behaves securely after
installation; exact-current DEB and RPM lifecycle gates now pass for the audited
R17 package hashes. The separately tracked production health, scale, endurance,
and canary gates remain open.

### R25 installed-policy verification — 2026-10-28

The later R25 DEB and RPM clean-install lifecycle reports independently
verified the effective service policy on Ubuntu 24.04 and Rocky 9. Both report
the exact packaged unit SHA-256 `1d65a448d1c3f76de211dd4767227bc9d31275dd422565a1eb7cf45cac109d67`,
root user with `Group=dux`, ambient and bounding capabilities limited to
`CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH`, `NoNewPrivileges=yes`, the module
syscall deny list, `UMask=0027`, `StateDirectory=dux` mode `0750`, 50% CPU quota,
2 GiB `MemoryHigh`, 4 GiB `MemoryMax`, and `IOAccounting=yes`. The DEB/RPM clean
install also preserves the optional tunnel disabled and without an identity
key. Reports:
`target/readiness/r25-startup-recovery/package-lifecycle-r25-deb-20261028.json`
(SHA-256 `7ea6ee23eb30cb49d118c76229f6ef6f0534a6a2896b9af42a4840fc3b403f7c`)
and `target/readiness/r25-startup-recovery/package-lifecycle-r25-rpm-20261028.json`
(SHA-256 `05aa0b74bf9ec80d9fe002e4b60f01aff0b311f19ac3fa74d2fc3c83e9d0f306`).
The installed production unit has the same unit-file hash and effective
capability/resource policy, although its running executable remains R16.

This closes the R25 package-policy verification item, not the broader privilege
risk acceptance: the service still runs as root with broad `CAP_SYS_ADMIN`,
systemd exposure remains 5.1 MEDIUM, and runtime coverage on duplicate mount
roots remains unresolved. A release owner must explicitly accept or redesign
those risks before production sign-off. The active-source release, live
convergence, 10m same-binary scale, endurance, and canary gates remain open.

## Data and trust boundaries

The current source unit sets `LimitCORE=0`. `WatchdogSignal=SIGABRT` remains
useful for identifying a watchdog-triggered failure, but process memory can
contain the complete path index and queued filesystem names. The installed
service was observed with soft core limit **0** and hard limit **unlimited**:
the soft limit prevents an ordinary core at that sample, but a root daemon
could raise it. Setting both limits to zero makes that defense immutable for
the service lifetime. The exact package lifecycle verifier requires the
effective `LimitCORE=0` property so an override or packaging omission fails
qualification. This does not suppress journald diagnostics or prove that every
external core collector is configured safely.

The SQLite index contains paths, inode metadata, owners, allocated sizes, and
growth history for the indexed filesystems. Path names can reveal sensitive
information even though Dux does not store file contents. The packaged index
directory is `root:dux` mode `0750`; the database and its WAL/SHM/rollback-journal
files are `root:dux` mode `0640`. Membership in `dux` therefore grants broad filename
enumeration. Keep that group limited to trusted users. Operators needing a
root-only index can remove the group grant and use the documented `0700` state
directory / `0077` umask policy.

The indexer runs as root with exactly `CAP_SYS_ADMIN` and
`CAP_DAC_READ_SEARCH` in its capability bounding set and ambient set. These
privileges support filesystem-wide fanotify marking and resolving file handles
through directories that the service account could not otherwise search. The
bounding set excludes `CAP_SYS_MODULE`, `CAP_SYS_PTRACE`, network administration,
and other capabilities. `NoNewPrivileges`, `ProtectKernelTunables`,
`ProtectControlGroups`, `ProtectKernelLogs`, `ProtectClock`,
`ProtectHostname`, `RestrictNamespaces`, `RestrictRealtime`,
`RestrictSUIDSGID`, `LockPersonality`, and `MemoryDenyWriteExecute` provide
additional restrictions.

`CAP_SYS_ADMIN` remains a broad kernel privilege. The current architecture does
not isolate the fanotify operation into a small privileged broker; reducing
this capability further requires a different watcher design and exact-coverage
qualification. Treat compromise of the indexer as a host-level security event.

## systemd exposure findings

Several `systemd-analyze security` findings are direct consequences of the
indexer's whole-filesystem view:

- `ProtectSystem`, `ProtectHome`, `PrivateTmp`, and `PrivateMounts` are not
  enabled because they create a different mount view and can hide paths or
  cause fanotify marks not to observe writes made through the host mount
  namespace. `ProtectKernelModules` is also disabled: it hid
  `/usr/lib/modules` in a prior isolated qualification. The unit instead denies
  the `@module` syscall class and excludes `CAP_SYS_MODULE`.
- `PrivateDevices` and `DeviceAllow` restrictions are not enabled because Dux
  indexes filesystem names and metadata, including device-tree paths. Revisit
  this only with an exact coverage comparison.
- `ProtectProc` and `ProcSubset` are not enabled because altering `/proc` view
  can change the root tree being indexed. They may be candidates for a
  separately qualified `/proc` exclusion policy.
- The packaged daemon has no `--alert-exec` by default and now restricts address
  families to `AF_UNIX`. The package policy test and runtime systemd probe cover
  that default. A systemd override that enables a networked `--alert-exec` hook
  must explicitly add `AF_INET` and `AF_INET6`; this restriction applies to the
  hook child too.
- `UMask=0027` intentionally permits the trusted `dux` group to read the
  filename index. Use `0077` and a root-only directory if group access is not
  wanted.

The optional alert hook executes `sh -c` as UID 0 for compatibility with existing
hooks. By default, Dux clears the child's ambient, effective, permitted, and
inheritable Linux capabilities before exec. The packaged unit supplies only
`CAP_SYS_ADMIN` and `CAP_DAC_READ_SEARCH` to Dux and locks `SECBIT_NOROOT`, so
UID 0 cannot reacquire those capabilities at exec. Direct root launches set
and lock `SECBIT_NOROOT` in the child before clearing its capability sets; if
that operation fails, the hook is not run. Operators can explicitly opt into
capability inheritance with `--alert-exec-keep-capabilities`; this should be
used only for a reviewed hook that requires Dux's capabilities. Capability
removal does not change the hook's UID, filesystem visibility, environment, or
access granted by ordinary file permissions, so it is defense in depth rather
than a complete privilege boundary. Only root-controlled command configuration
is supported; the command must safely quote `DUX_PATH` and treat its value as
untrusted filesystem input.
Alert evaluation is skipped while host pressure is Elevated or Critical. Hooks
are best-effort notifications and are not a reliable emergency response path
under sustained load.

## Optional remote access

Remote access uses a separate, disabled-by-default `dux-tunnel.service`. Package
installation must not generate a key, enable the service, or start a tunnel.
An explicit operator-created `/etc/dux/ed25519` identity and protected
`/etc/dux/tunnel.conf` plus `/etc/dux/known_hosts` satisfy the service
conditions. The config lists reviewed `HOST PORT` endpoints; the known-hosts
file must contain independently verified SSH host keys for those endpoints.
No endpoint is embedded in the executable. The tunnel uses strict checking and
does not trust first-contact keys or update pins automatically. Its capability
set, network families, CPU, memory, and task count are distinct from the
indexer. Keep the tunnel service disabled unless remote access is explicitly
required and its endpoints and host keys are reviewed.

The tunnel now rejects private identities unless they are regular, non-symlink
files owned by root with exact mode `0600`. `dux tunnel keygen` also rejects a
symlinked or non-root-owned `/etc/dux` directory and fails if it cannot set
directory mode `0700`. The optional Dropbear host key receives the same metadata
checks before it is forwarded, and failure to secure that key disables only the
break-glass endpoint. These checks reduce accidental key exposure and unsafe
path redirection; they do not defend against a process that already has root
access.

## Release decision

The dependency advisory scan and unit syntax check pass for the identified
candidate. The medium systemd exposure is understood but intentionally not
treated as a clean security score: the path-visibility and fanotify
requirements explain the main exceptions, while broad `CAP_SYS_ADMIN` and
root-configured alert execution remain material risks. Security signoff still
requires reviewing the installed effective policy on the exact r17 package,
verifying index permissions and process capabilities after clean install, and
deciding whether to add a separately tested unprivileged alert-runner mode.
Package lifecycle, installation-time defaults, and the current production
service remain unqualified for this candidate.

## Exact R21 package and dependency addendum — 2026-10-28

The current static-musl `0.6.3-21` payloads were reviewed after package creation.
The DEB SHA-256 is
`afec718e1f63bb5b2e2dd151f63cb95e3da6c2490e7a099a30f370e434c7d978`; the RPM
SHA-256 is
`26caf76cf7d4bc506b8e3f3a925afc46f98605d28fe6c33b95f2cfc2ca78bad7`. Both
contain an identical `dux.service` (SHA-256
`1d65a448d1c3f76de211dd4767227bc9d31275dd422565a1eb7cf45cac109d67`) and
`systemd-analyze verify` accepted both extracted unit files with no diagnostics.
The R21 lockfile remains SHA-256
`c7d392c638fc891978b194097710538c4574f663d12357adc1cff6cf16b3d8b1`; `cargo
audit` scanned 231 dependencies against advisory database commit
`99670cfc0bc574433034b75876a4b791ccaa2efb` (updated 2026-10-27) and reported
zero known vulnerabilities or warnings. Machine-readable output is
`target/readiness/r21-incremental-rollup/cargo-audit-20261028.json` (SHA-256
`5855c4da96c1670075660c28d8aab18a0f381da2d10dc9cdc11c1e279bee6060`).

The extracted DEB and RPM units each score **5.1 MEDIUM** with systemd 255's
offline analyzer. The exact package lifecycles independently confirmed their
installed effective capabilities, `NoNewPrivileges`, seccomp deny list,
`root:dux` state/index permissions, and absence of a tunnel key. Reports are
`target/readiness/r21-incremental-rollup/security-payload-analysis-r21-20261028.json`
(SHA-256 `252bd418ef3f06f235f0fe511cacc2b09b28f573b1819ca759c01404cb8cd064`)
and the DEB/RPM lifecycle reports named above. Rocky 9's systemd 252 warns that
`RestartSteps` and `RestartMaxDelaySec` are unknown and ignores those optional
ramp settings; its compatible fixed `RestartSec=10s` remains active. This is a
compatibility warning to include in the RPM review.

The earlier release-decision caveats remain: the indexer has broad
`CAP_SYS_ADMIN`, system-wide path visibility prevents several sandboxing
controls, and `systemd-analyze security` remains medium. The R21 Rocky clean
install also exposed an exact-coverage limitation when `/efi` and
`/boot/efi` have the same mounted directory identity: Dux marks the index dirty
and retries instead of claiming freshness. This is safer than silently
publishing incomplete data, but exact duplicate-mount path semantics and the
operator-facing recovery behavior still need qualification before security and
release signoff. This addendum verifies package and dependency evidence; it is
not final security approval or production deployment authorization.

## Exact R34 candidate static review — 2026-10-29

The current-source R34 release artifacts were reviewed without installing them.
The static-musl executable SHA-256 is
`b18a041d08fd31e13eee03c9f6c7565a596db975a2879cdef04076a6c5017ddb`; the DEB
SHA-256 is `7ace35066421f85ad844217a9e4c10d4dce75768e86bd754ee769ea95523d7a2`,
and the RPM SHA-256 is
`ea3e8ec0b5f764387351260e5fea9b2dab96a776adad9ca417f4709ba6cadacf`. The
source archive SHA-256 is
`a6a6c130a3faa0dcb71666017e7da065adf3675a21882016afab1fe851b18e62`; artifact
manifest SHA-256 is
`88fecc6339a22f9d496a342d6322bf96660621de0a12a1d3be9f8d56f16149bb`.

Both extracted packages carry the same unit payload hashes as the reviewed
source: `dux.service`
`1d65a448d1c3f76de211dd4767227bc9d31275dd422565a1eb7cf45cac109d67` and
`dux-tunnel.service`
`53ce2a91c4df885acfedd10eb7a3cda7b2a1fc7165f16762dc2b76994a4e0fde`.
`systemd-analyze verify` reports no diagnostics. Systemd 255.4's offline
security analysis scores the indexer **5.1 MEDIUM** and the optional tunnel
**7.1 MEDIUM**. These scores describe different units; the tunnel has a
separate root/capability policy and remains disabled by default.

The current R34 `Cargo.lock` SHA-256 is
`2fbcf3f01af2eb56ed2bea4b4bcbad4d923ff6fc330a6499335e4cbdff006d97`;
`cargo audit` scanned 231 locked dependencies against
advisory database commit `ee1a49a2769a24a596afc1c20cb33748d2769586`, updated
2026-10-29, and reported zero known vulnerabilities or warnings. JSON output is
`target/readiness/r34-current-source-20261029/cargo-audit-r34-20261029.json`,
SHA-256 `df7f1b7b7f08f897c893cde5e48334943a1b9e048e9fd9f29d03904b682038b5`.

This is static evidence only. The R34 DEB/RPM have not completed exact
install/upgrade/rollback/remove lifecycle tests, and effective permissions,
process capabilities, tunnel-disabled state, and runtime scan health have not
been verified after installing these exact packages. The 5.1/7.1 exposure
scores and the previously documented root/`CAP_SYS_ADMIN` risk remain release
risks, not resolved findings.

## Current v6 jobs2 recovery candidate — 2026-10-29

The exact static-musl candidate reviewed in the current readiness addendum is
`target/readiness/production-review-20261029/dux-v6-tui-hardened-jobs2-recovery-musl`
(SHA-256
`23a3f247bb3ed0b15d39c90f70a75c62239d4def19a5c8991db84bbf42353f61`). The
upstream-verified RustSec scan for the current lockfile found zero known
vulnerabilities and zero warnings; its report and advisory-database provenance
are preserved in `target/readiness/production-review-20261029/` and covered by
`SHA256SUMS-v6-jobs2-recovery-20261029`.

The candidate has isolated recovery evidence and code-level tests, but this
review does not qualify package lifecycle, installed effective privileges,
production convergence, or the existing broad `CAP_SYS_ADMIN` and systemd
medium-exposure findings. No release security signoff is implied.

## Exact v36 package runtime checks — 2026-10-30

The current v6 candidate was packaged as `0.6.3-36`; the DEB and RPM each
contain the exact reviewed binary SHA-256
`23a3f247bb3ed0b15d39c90f70a75c62239d4def19a5c8991db84bbf42353f61`. Exact
package lifecycle tests passed in disposable Ubuntu 24.04 and Rocky 9 guests.
Their clean installs verified effective `cap_dac_read_search cap_sys_admin`,
`NoNewPrivileges=yes`, the kernel-module syscall denylist, configured resource
limits, `root:dux` state/index permissions, an enabled indexer service, and a
disabled tunnel with no created identity. Reports and package/build provenance
are in `target/readiness/production-review-20261029/v36-package/`.

These checks confirm the intended controls are present after installation;
they do not reduce the indexer's root and `CAP_SYS_ADMIN` privilege. Offline
systemd exposure remains **5.1 MEDIUM** for the indexer and **7.1 MEDIUM** for
the optional tunnel. Privilege minimization and the broad system-wide indexing
boundary remain explicit security risks.

## v37 tunnel secret-file hardening — 2026-10-30

The v37 source adds checks for required tunnel and Dropbear host secret files:
each must be a root-owned regular file, not a symlink or special file, with
exact mode `0600`. Key generation requires `/etc/dux` to be a real root-owned
directory and fails if it cannot set mode `0700`. Failure to secure a Dropbear
host key now disables only the optional break-glass endpoint. Tests cover loose
permissions, mismatched ownership, and symlink rejection. These source changes
are included in the exact v37 package lifecycle and scale evidence recorded in
`PRODUCTION_READINESS_AUDIT.md`.

This is secret-file handling hardening, not privilege separation: the optional
tunnel service and indexer still run with root-level capabilities, and
`CAP_SYS_ADMIN` exposure remains. The production daemon is still the prior
binary; v37 has not been deployed or canary-tested. Independent capture/apply,
live convergence, configured 10k-event catch-up/CPU ceiling, 72-hour endurance,
and 24-hour canary remain open.

## Current static package candidate with immutable core limit — 2026-10-30

Candidate `0.6.3-40` was built from the current worktree as a static-musl PIE.
The DEB and RPM both contain the exact `packaging/dux.service` bytes, including
`LimitCORE=0`, and the exact same binary SHA-256
`f9e3a1a4ceeb21e343197746d833e708655f24f6af523a1440c631855546284d`. Package
hashes are DEB
`8894b9b8ef163e05cc0239de24f942ce336e9df33796f97b2e84976f77575342` and RPM
`38eb9747275204beed856fd226d6759edb701941ba90a5d16e3a26cd4c1f637e`. Both
extracted units pass `systemd-analyze verify`; the build manifest and SHA256SUMS
are in `target/readiness/production-review-20261030/core-limit-r40/`.

The static executable passed a disposable scan/status/explain smoke test,
including exact `explain` lookup of a path containing byte `0xff`. This verifies
the packaged executable's startup and selected CLI paths, not package lifecycle
or mount/permission behavior. The 10 GiB workspace plus 8 GiB reserve admission
still fails, so exact DEB/RPM install/upgrade/rollback/remove tests had not yet
been run at this audit point. No package was installed on the production host;
production remains on the older unit with an
unlimited hard core limit and a zero soft limit at the observed sample. This is
not a production rollout approval. Root/`CAP_SYS_ADMIN`, exact package
lifecycle, live recovery, endurance, and canary risks remain open.

### Exact Ubuntu DEB lifecycle update — 2026-10-30

The R40 DEB was subsequently exercised through the complete R39→R40 lifecycle
in a disposable Ubuntu 24.04 KVM guest. Upgrade, rollback, re-upgrade,
remove/purge, clean install, effective unit security properties, and index
permissions passed. The full evidence is in
`target/readiness/production-review-20261030/core-limit-r40/package-lifecycle-r40-deb-ubuntu24.json`.
The ephemeral guest used a guarded `/dev/shm` overlay and its manager-verified
cleanup succeeded. The exact RPM lifecycle was not run: after saving the DEB
report and stopping the guest, the required 2 GiB free-space admission plus
1 GiB reserve narrowly failed. Static RPM package/unit checks still pass.
This closes only the Ubuntu DEB package lifecycle gate; production deployment,
live convergence, independent capture/apply, bounded WAL behavior under full
load, endurance, and canary remain unqualified.

The exact R39→R40 RPM lifecycle subsequently passed in a disposable Rocky
Linux 9.8 guest; the report is
`target/readiness/production-review-20261030/core-limit-r40/package-lifecycle-r40-rpm-rocky9.json`.
The service journal also showed that systemd 252 ignores `RestartSteps` and
`RestartMaxDelaySec`, and the initial scan found `/efi` and `/boot/efi` as
aliases for the same directory identity, leaving reconciliation dirty with an
incomplete-coverage error. Thus RPM package lifecycle is covered, but Rocky 9
scanner completeness and restart backoff semantics are not qualified. Two
earlier starts at 20% CPU quota did not reach SSH; a third at the standard 75%
quota booted successfully. All ephemeral guest overlays were cleaned.

The lifecycle gate now requires the daemon to report a live watch status, no
dirty/paused state, no scan in progress, and `caught_up=true`; systemd `active`
alone is insufficient. A repeat Rocky 9.8 run completed all package transitions
but failed this gate after 180 seconds and three incomplete scans. The report
is `target/readiness/production-review-20261030/core-limit-r40/package-lifecycle-r40-rpm-rocky9-readiness-gate.json`.
This confirms package installation works while complete root indexing does not
currently qualify on the Rocky 9.8 image.

## R47 optional tunnel unit hardening — 2026-10-30

The optional tunnel unit now enables private devices, kernel-interface
protections, namespace and realtime restrictions, native syscall architecture,
W^X memory protection, and locked process personality. These settings do not
change its endpoint configuration, root identity, or existing bounded
capability set. `systemd-analyze verify` accepts the source unit, and local
offline `systemd-analyze security` exposure decreased from **6.7** to **4.5**.
The exact R47 DEB/RPM payloads contain the same hardened unit bytes and the
previously qualified R46 production binary. Package construction and payload
hash checks pass; the R47 install lifecycle and an enabled tunnel runtime have
not been qualified. The tunnel continues to run as root with
`CAP_SETUID`, `CAP_SETGID`, and `CAP_DAC_OVERRIDE`; the residual privilege risk
requires explicit release-owner review.

## R21/R22 dependency audit and current release gate — 2026-10-30

The current locked Rust dependency set has 229 packages.
`cargo audit --deny warnings` found zero known RustSec advisories or warnings at database
commit `2295466c4e9df26647fd67d8fa1c705ac2fc7f81` (1,395 advisories). The R21
release binary was not built with dependency metadata; `cargo audit bin` could
recover only 43 dependencies and warned that the binary result was incomplete.
Its lockfile and binary audit outputs are retained with the R21 artifact under
`target/readiness/release-r21-io-admission-20261030/`.

The R22 release path now requires `cargo-auditable 0.7.7`, builds in an isolated
Cargo target directory to avoid reusing ordinary non-auditable build outputs,
and refuses static release binaries without the ELF `.dep-v0` section. The CI
workflow is configured to build package revision 22 and require
`cargo audit bin` to find the embedded dependency list. The R22 release build
and binary audit have not run yet; R21 remains the latest built artifact and is
unsigned/uninstalled. The zero-advisory
lockfile result is point-in-time dependency evidence, not a source-security,
native-library, provenance, signature, or runtime safety certification.

Read-only offline analysis of the current source units on Ubuntu systemd 255
passes `systemd-analyze verify`. `systemd-analyze security --offline=yes`
reports **5.0 MEDIUM** for the indexer and **4.5 OK** for the optional tunnel.
CI is configured to enforce these systemd 255 score ceilings with `--threshold=50` and
`--threshold=45` respectively, so additional exposure requires an explicit
review and threshold change.
The indexer must retain root and `CAP_SYS_ADMIN` to watch host mounts, but the
source unit now also denies the `@mount` syscall group without creating a private
mount namespace. `@mount` on systemd 252 and 255 does not include
`open_by_handle_at`, which Dux needs for fanotify FID resolution. The repeatable
`scripts/dux-systemd-filter-smoke.py` test runs in a unique transient systemd
unit with a 10-second runtime cap; it confirmed a mount syscall was denied and
the FID syscall was not blocked. CI is configured to rerun this test. This
filter check does not prove all daemon
behavior under the hardened package unit or older systemd releases: Rocky 9.8
systemd 252 previously ignored `RestartSteps` and `RestartMaxDelaySec`, and its
root scan did not reach a clean caught-up state.

The R21 candidate passed four disposable runtime smoke suites (three crash
boundaries, overflow plus reconciliation, bounded CPU catch-up, and mount
appearance/disappearance); the report is
`target/readiness/release-r21-io-admission-20261030/runtime-smoke.json`. This
does not replace the unrun OOM, WAL, pressure, retry, coverage, churn, scale,
72-hour endurance, or 24-hour canary gates. Production remains on the older
0.6.3-16 daemon with a dirty, growing backlog and no reconciliation scan; no
candidate deployment or index mutation has been made.

## Pre-SEC-20 tunnel and native-boundary review — 2026-11-01

A read-only review of the current worktree covered the tunnel config/key checks,
SSH argument construction, fanotify event-record bounds, file-handle parsing,
fanotify descriptor handling, alert-hook capability clearing, and fault-injection
process stop. The existing all-feature test binary (SHA-256
`c1e053ff32c8cca169f3af9dae72bfa8c28821889e862a736fe06979010b116d`) passed all
12 `tunnel::tests`; the full Python qualification harness passed 194/194, and
`git diff --check` passed. No confirmed memory-safety or privilege-escalation
bug was found in these inspected paths. This is a focused manual/static review,
not a proof of all unsafe-code invariants or a replacement for fuzzing and exact
package/runtime qualification.

Follow-up hardening now requires a root-owned, mode-`0600`, regular, non-symlink
`/etc/dux/known_hosts` file with at least one host-key entry. SSH uses
`StrictHostKeyChecking=yes`, the explicit file, no global host-key file, and
`UpdateHostKeys=no`; the service also requires that file before startup. The
operator must still verify each fingerprint through an independent trusted
channel when provisioning it. The tunnel refuses to connect when a key is
missing or changed. The feature remains disabled by default and has not been
enabled on the production host.

## SEC-20 Enforce pinned host keys for optional SSH tunnel — 2026-11-01

The prior trust-on-first-use behavior is removed. `dux tunnel run` now requires
`/etc/dux/known_hosts` to be a non-empty, root-owned regular file with exact
mode `0600`, opened without following a final symlink. The tunnel service has a
matching `ConditionPathExists`; OpenSSH is invoked with
`StrictHostKeyChecking=yes`, the explicit known-hosts path, global host keys
disabled, and `UpdateHostKeys=no`. The key, config, and verified host-key file
must all exist before the optional supervisor starts. The file's integrity and
its fingerprints still depend on operators obtaining and checking endpoint keys
through a trusted independent channel. The indexer service is unchanged.

Validation: all **13 tunnel unit tests** passed, including private-file,
symlink, empty-file, and strict SSH-argument cases. The disposable loopback SSH
qualification passed **8/8** checks, including rejection of unknown and
mismatched server keys, acceptance of the pinned key, forced-command behavior,
loopback-only reverse forwarding, and denied unauthorized forwarding. The full
current-source Rust suite passed **242/242** after the startup presence
predicate update; all-target/all-feature Clippy with warnings denied also
passed on that source. The Python harness passed **197/197** after adding CI-gate
regressions; formatting, unit verification, and `git diff --check` passed. The
tunnel was not enabled on production, and distro package installation with the
new condition remains part of candidate lifecycle qualification.

## SEC-21 Pin SSH helper executable paths — 2026-11-01

The optional privileged tunnel previously launched `ssh`, `ssh-keygen`, and
local `sshd` by PATH lookup; privileged alert hooks also selected `sh` through
PATH. The packaged systemd service has a controlled environment, but a root
CLI/daemon invocation could inherit a manipulated PATH and execute an
unintended helper. The source now invokes `/usr/bin/ssh`,
`/usr/bin/ssh-keygen`, `/usr/sbin/sshd`, and `/bin/sh` directly. The tunnel
passes `-F /dev/null` so ambient root SSH configuration cannot add a
`ProxyCommand` or `LocalCommand`; endpoint, key, and host-key settings all come
from explicit dux configuration. These are the standard package paths for the
supported Debian and Rocky families. The focused Python source regressions
pass **4/4** and full discovery now passes **218/218**. Rust path/argument
regressions remain uncompiled and unrun. This change has not been packaged or
deployed, and optional-client availability remains part of current package
lifecycle qualification.

## Production executable dependency-audit status — 2026-11-01

The earlier R21/R22 status above is historical. A later retained R58 package
was built with `cargo-auditable 0.7.7`, but its manifest binds it to a stale
source tree: the current handoff snapshot has nine newer source files and 19
changed shared files, including `src/main.rs`, `src/scan.rs`, `src/store.rs`,
`src/tunnel.rs`, and `src/watch.rs`. R58 is therefore not the current release
candidate.

`cargo audit bin /usr/bin/dux` against installed **dux 0.6.3-16** reports that
the binary lacks `cargo auditable` metadata and recovers only **44 dependencies**
from panic strings, explicitly warning that its result is incomplete. The
retained R58 binary exposes 202 dependencies, but that audit does not qualify
the current source. The current source snapshot still has no built, hash-bound
executable or package with an embedded dependency inventory. Do not claim a
complete production-binary dependency audit until a current-source artifact is
built and `cargo audit bin` reports its embedded dependency set. Even then,
RustSec results cover known Rust advisories only; OS/native dependencies,
source-level defects, provenance, signing, and runtime qualification remain
separate checks.

## Current-source rollback backup schema validation — 2026-11-02

Static review found that `scripts/dux-backup-index.py` treated any SQLite file
with a decimal `meta.schema_version` and a passing `PRAGMA quick_check` as a Dux
index. A non-Dux database could therefore be published with a Dux rollback
manifest, creating false confidence before a destructive rebuild. The helper
now accepts only schema versions 4–6 and requires the core `meta`, `inodes`,
`dirents`, and `growth` tables with the columns needed for those versions. It
requires `inodes.birth_ns` from v5 and `growth.birth_ns` from v6. Regression
cases cover a forged metadata-only SQLite file, unsupported versions, the
supported v4/v5/v6 structural shapes, and free-space falling below the reserve
during a long backup. The helper now rechecks free space as SQLite copies pages
and aborts before publishing the rollback artifact if the required reserve can
no longer be maintained. It also validates the copied snapshot's Dux schema
after `quick_check` and before publishing either artifact.

The focused behavioral suite passed **11/11** in **0.909 seconds** under a
systemd scope capped at 5% CPU and 512 MiB memory, with no swap and idle I/O.
It covers the schema checks, forged SQLite rejection, symlink/sidecar safety,
free-space reserve checks before and during backup, WAL-consistent snapshots,
private modes, hash binding, and no-clobber publication. The tests used only
temporary databases; no production index was opened. `git diff --check` passes.
This closes the helper's local unit-test gap, but not package installation or
production-sized backup/restore. Schema-shape checks and SQLite `quick_check`
still do not prove logical index exactness or that a restore reproduces the
complete filesystem; isolated migration and backup/restore qualification
remain open.

## State-directory permission repair and hardlink isolation — 2026-11-02

Review of `packaging/secure-index-permissions.sh` found two unsafe cases when
repairing an existing attacker-writable state directory. First, chown-before-
chmod left a window for replacing an inspected SQLite artifact; applying mode
0700 first still allowed the prior owner to write until chown. The helper now
sets the directory to mode 000, changes its owner/group, and only then reopens it
at mode 0700 for artifact inspection. The final mode remains 0750. Second,
chown/chmod on a regular hard-linked artifact would also mutate its inode through
names outside `/var/lib/dux`; the helper now rejects artifacts with link count
other than one before changing file ownership or mode.

Regressions use a writable directory initially owned by uid 65534 and verify it
is inaccessible at the ownership transition; symlinks and special files fail
closed, and hard-linked external targets retain their original owner, group,
and mode. The complete package-helper/lifecycle module passed **44/44**; shell
syntax, ShellCheck, and `git diff --check` passed.
These tests exercise the helper and package policy locally; no DEB/RPM install,
upgrade, rollback, or removal test has been run for the current source. Existing
0.6.3-59 release artifacts predate these changes and must not be promoted.

## Current-source Rust dependency advisory audit — 2026-11-02

Ran `cargo audit --json` against the current checkout's 229 locked dependency
packages. The RustSec advisory database contained 1,402 advisories and was
updated at **2026-11-02 11:45:44 UTC** (database commit
`263a4d951a0867d4bddc29dddd6d59427c78f9a7`). It reported **zero known
vulnerabilities** and no warnings for the lockfile SHA-256
`fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`.

This audits the current source lockfile only. The installed production binary
has no cargo-auditable dependency metadata; its package audit remains
incomplete. A clean source-bound release binary and package still need a
binary-level dependency audit, signing/provenance verification, OS/native
dependency review, and runtime qualification.

A direct `cargo audit bin /usr/bin/dux` recheck confirms the installed package
is **0.6.3-16**, binary SHA-256
`9433d56d2ebd67162dc381437d030d428254373cbde9f86f63610bb0a4e87f64`. The tool
warns that the binary lacks `cargo auditable` metadata and recovers only **44
dependencies**, so this result is incomplete and is not evidence that the
installed production binary has no vulnerable dependencies.


### Live service policy recheck — 2026-11-02 14:49 UTC

The historical R25 installation record above does not describe the currently
running service policy. A current read-only `systemctl show` reports the live
service as root (`User=`), `Group=dux`, `NoNewPrivileges=yes`, and the expected
`CAP_SYS_ADMIN` / `CAP_DAC_READ_SEARCH` bounding and ambient sets. It also
reports `SecureBits=0`, `LimitCORE=infinity` with `LimitCORESoft=0`,
`SystemCallFilter=~delete_module finit_module init_module`, and an effective
100% CPU quota. The soft core limit currently prevents ordinary core creation,
but the hard limit is not locked to zero; this fails the current package
lifecycle policy. The effective unit scores **5.1 MEDIUM** under
`systemd-analyze security`.

The installed base unit SHA-256 is
`dd1aa5dcc481c3467c3fbcebbf4f9fda11ddb7082b802412910575dc0a21d47b`; the
current checked-in `packaging/dux.service` SHA-256 is
`a583c23cf8837a9e25802c76fbd5357efe5d84d650e5638b41a2ac230b92603b`. The
checked-in unit explicitly sets `LimitCORE=0`, `SecureBits=noroot
noroot-locked`, denies a broader syscall-group set including `@clock`, and
sets `CPUQuota=100%`. `systemd-analyze verify packaging/dux.service` passed
with no diagnostics. This validates unit syntax only; current DEB/RPM install,
effective-unit upgrade, rollback, and removal remain unqualified. No unit
reload or live service change was made.


### Package lifecycle regression rerun — 2026-11-02 14:51 UTC

Re-ran `scripts.test_dux_package_lifecycle` from `/home/dux` in a transient
systemd scope capped at 5% CPU and 512 MiB memory, with swap disabled, low I/O
weight, `nice 19`, and idle I/O priority. All **46/46 tests passed in 34.099
seconds**; scope CPU time was 1.781 seconds and swap use was zero.
Coverage includes effective-unit policy rejection, core-limit and capability
hardening, private index/SQLite permissions, symlink/hardlink rejection for
the database and all three SQLite sidecars, rollback backup, package helper
payload hashes, restart policy, and optimized-Python refusal. This does not
exercise actual current DEB/RPM install, upgrade, rollback, or removal, and
does not change the live service policy.


### Full Python qualification suite rerun — 2026-11-02 15:00 UTC

The complete `scripts/test_*.py` discovery suite passed **249/249 tests in
97.799 seconds** in a transient scope with CPUQuota 5%, MemoryMax 1 GiB, swap
disabled, IOWeight 10, `nice 19`, and idle I/O priority. The scope used
**4.878 CPU-seconds**, peaked at **185.2 MiB**, used no swap, and exited with
`Result=success`. This includes the 46 package lifecycle tests and 12 backup
helper tests. Coverage validates source helpers and harness behavior; it does
not replace a current DEB/RPM install/upgrade/rollback/removal, a Rust binary
build, production-sized v5→v6 migration, exact live recovery, or scale and
endurance qualification. The production service and index were not changed.


### Live v5 rollback snapshot — 2026-11-02 15:04 UTC

Created the current live SQLite index backup at
`/root/dux-prequalification-20261102T1503Z.sqlite` using the online backup
helper. The report records schema v5, **184,471 pages**, **755,593,216 bytes**,
`quick_check=ok`, and SHA-256
`21bc898fd23496fe257ab20d28a9a2b9de55e6cb86a94781ab43e556f0a28425`. Database
and manifest are mode **0600** under root's mode-0700 home. The helper's
post-publication hash, size, and permission values were independently checked.
This protects a transaction-consistent copy for rollback rehearsal; it does
not make the dirty production index an exact filesystem baseline. No package
or production database mutation occurred.


The backup received an additional read-only full `PRAGMA integrity_check` at
15:18 UTC; it returned **`ok`** in 4m28.972s under a 5% CPU cap and 512 MiB
memory limit (367 MiB peak, no swap). This verifies the saved SQLite database
more thoroughly than the backup helper's initial `quick_check`; it still does
not prove filesystem exactness for the dirty source index.

## Current-source RustSec refresh — 2026-11-03

Refreshed the official RustSec advisory database and ran `cargo audit --json`
against the exact current `Cargo.lock` (SHA-256
`fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`). The
command exited **0**; it scanned **229 locked packages** against **1,402
advisories**, database commit
`537440b6e46d3970ba9a6719e6e37bfd86ca5134` (updated **2026-11-03
09:37:25 UTC**) and reported **zero known vulnerabilities and zero warnings**.

This refresh supersedes the Nov. 2 source-lockfile advisory timestamp. It is
limited to Rust packages represented in this lockfile. The installed
`/usr/bin/dux` still lacks complete `cargo auditable` metadata (the prior scan
recovered only 44 dependencies); this does not audit the installed binary's
full dependency set, distribution libraries, native runtime behavior, or
unknown vulnerabilities. It also does not close live convergence, package
lifecycle, scale, endurance, rollback, or canary gates.

## Live installed service policy recheck — 2026-11-03 09:43 UTC

Read-only `systemctl show` and filesystem checks reconfirm that the running
service differs from the hardened source unit. It runs as root (`User=` empty)
with `Group=dux`, `NoNewPrivileges=yes`, and only `CAP_SYS_ADMIN` plus
`CAP_DAC_READ_SEARCH` in the bounding and ambient sets. The state directory is
`0750 root:dux` and the database `0640 root:dux`; `getent group dux` lists no
supplementary members. The index therefore remains sensitive path-name data,
but its observed modes are restrictive.

The active unit still reports `SecureBits=0`, `LimitCORE=infinity` with soft
limit zero, and only `SystemCallFilter=~delete_module finit_module init_module`.
Thus a root process can raise its soft core limit, and the active syscall deny
list is narrower than the checked-in unit policy. Effective CPU quota is
**100% of one core**, while the installed base unit sets **50%**. The active
service remains at **5.1 MEDIUM** in `systemd-analyze security`. Installed unit
SHA-256 is
`dd1aa5dcc481c3467c3fbcebbf4f9fda11ddb7082b802412910575dc0a21d47b`; checked-in
`packaging/dux.service` SHA-256 is
`a583c23cf8837a9e25802c76fbd5357efe5d84d650e5638b41a2ac230b92603b` and
contains `LimitCORE=0`, locked `SecureBits=noroot`, a broader syscall filter,
and the 100% quota defined by this current worktree.

No live policy, quota, unit, or service change was made: applying the source
policy requires a source-bound package and controlled service maintenance, and
the running index is already dirty with a large event backlog. This closes no
runtime security gate. Exact package installation, effective-policy
verification, and acceptance of the remaining root/`CAP_SYS_ADMIN` exposure
remain required.

## Optional tunnel syscall filter — 2026-11-03

The optional `dux-tunnel.service` now denies syscall groups used for kernel
module, mount, swap, reboot, clock changes, obsolete interfaces, raw I/O,
debugging, CPU emulation, and `open_by_handle_at`. The last denial is specific
to the tunnel: the filesystem indexer still needs `open_by_handle_at` for
fanotify FID resolution. Dropbear's required setuid/setgid capability set is
unchanged. The tunnel remains disabled by default.

Validation on the current source passed `systemd-analyze verify` for both
packaged units, two focused tunnel unit-policy tests, and transient systemd
scope syscall probes using each unit's exact `SystemCallFilter` line. The
probes observed representative restricted calls denied, `clock_gettime`
allowed, and the FID syscall permitted for the indexer but denied for the
tunnel. On the same systemd 255 host, the tunnel unit's offline security score
is **3.2 OK**, compared with **4.5 OK** for an otherwise identical unit without
this filter. This matched comparison supersedes the older 7.1 score, which
came from a different earlier unit revision.

This is source-level policy hardening, not an enabled-tunnel qualification. No
current Dux binary or Dropbear session was run under this exact unit sandbox,
and package install/upgrade/rollback remains unqualified. The loopback OpenSSH
qualification passed outside this filter, but failed when the same filter was
applied to its test `sshd`: the connection reset at login. The `@mount` syscall
group includes `chroot`, which OpenSSH privilege separation may use; that
failure is specific to this test server and does not establish compatibility
or incompatibility of the local Dropbear child. The SSH harness now includes
captured command output in raised errors to make future failures diagnosable.
The live production service and its policy were not changed.

## Production unit policy recheck — 2026-11-08 23:51 UTC

The 2026-11-03 live-policy snapshot above is historical and no longer
describes the loaded production unit. A fresh read-only check on `vps1` found
`LimitCORE=0`, `SecureBits=noroot noroot-locked`, `NoNewPrivileges=yes`,
`CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH`, and the broad
syscall deny filter. `systemd-analyze security dux.service` now reports **3.3
OK** (previously 5.1 MEDIUM). The loaded unit hash is
`dd34d3086af76117b18b06a23c5c70ace4353ff6a33ae9b3fa3c10f13c04d920`; the
checked-in `packaging/dux.service` hash is
`e12d1f4a6da87f529f481c4de92f501566ab40b11040fde2054a8718a932a51b`. Review
of the loaded filter found it does not explicitly deny `bpf`, while the
checked-in unit does. This is a remaining config drift item, not a claim that
the production service is otherwise unprotected.

This update is policy evidence only. The running production binary remains
`dux 0.6.3 (e0cc915f8f37)` with SHA-256
`e148692fe0bc42f809fa7fea41462feeda1acffb66b70e5affa03cdb5b0af060`. Its
read-only status still showed 326 kernel overflow notifications, a dirty
index, and a last completed scan about 3.46 days old. It does not qualify the
new candidate or close correctness/freshness gates. No service or database
state was changed.
