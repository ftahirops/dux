# Non-root service packaging follow-up

Source/artifact verification: 2026-11-10 12:04 UTC. Live state was refreshed
through 12:01 UTC. This records source changes and read-only runtime
observations. The packaging change has not been built into a release,
installed, or qualified on Debian or Rocky; it does not change the deployed
service.

## Change under review

The service unit now specifies `User=dux`, and package scripts create or
validate a locked `dux` system account. The database owner changes from
`root:dux` to `dux:dux`. The lifecycle verifier checks the live process UID,
GID, and all four capability sets against the intended identity and the exact
`CAP_DAC_READ_SEARCH | CAP_SYS_ADMIN` mask.

The RPM migration ordering was corrected during review: `%pre` synchronously
stops an active old daemon before RPM applies `%files` ownership; `%post`
repairs state-file ownership; `%posttrans` restarts only a previously active
service (and starts on first install). The active-state marker is kept under
`/run`. Debian stops the daemon in `postinst` before changing state-file
ownership and restarts only when it had been active.

This removes UID 0 from the daemon but retains `CAP_SYS_ADMIN`, which is a
powerful capability. Treat this as a reduction in ambient root authority, not
as a complete privilege boundary. Runtime behavior and path-handle access as
the dedicated UID remain unproven.

## Local verification

- Full Python discovery: **377 passed, 0 failed, 1 skipped** across 25 modules.
  The focused lifecycle suite is included in this total and independently
  passed **75 tests**.
- Standard-library `trace` run of the focused lifecycle suite: **53% statement
  coverage** for `dux-package-lifecycle.py` (691 statements) and **99%** for
  its test module (887 statements). Branch coverage is not measured. The
  annotated source report is [`dux-package-lifecycle.cover`](dux-package-lifecycle.cover).
- `PYTHONPATH=scripts python3 -B -m unittest -q test_dux_package_lifecycle`:
  **75 passed, 0 failed** after the RPM ordering and inheritable-capability
  regressions were added.
- `sh -n` for Debian maintainer scripts and the state-permission helper:
  passed.
- `rpmspec -P packaging/dux.spec` and `bash -n` of RPM `%pre`, `%post`, and
  `%posttrans`: passed.
- `systemd-analyze verify packaging/dux.service packaging/dux-tunnel.service`:
  passed.
- `git diff --check`: passed.
- A temporary RPM build and payload inspection passed for
  `dux-0.6.3-17.x86_64.rpm`; the package includes `%pre`, `%post`, and
  `%posttrans`, and declares `/var/lib/dux` as `dux:dux` mode `0750`.
- A temporary DEB staging build and payload inspection passed using the source
  control metadata version `0.6.3-12`; postinst, prerm, and postrm were included.

The temporary artifacts were unsigned, not release outputs, and were removed
after inspection. These checks validate packaging structure and syntax. They do
not replace package installation/upgrade tests or running the daemon under the
new UID.

## Live observations

### VPS1 production

At 12:01 UTC, `/usr/bin/dux status --json` reported the active installed
service dirty and behind by **239,417 seconds** (about 66.5 hours), with
**752 kernel overflows** and **2,518,862 dropped updates**. Its last full scan
was 2026-11-05; the service still runs as root (`User=` unset) with
`CAP_DAC_READ_SEARCH` and `CAP_SYS_ADMIN`. The current source change is not
deployed there. Root filesystem had 60 GiB available.

### VPS2 soak

At 12:00 UTC, transient unit `dux-readiness-1304925.service` was active with
zero restarts. The exact r132 binary SHA-256 remained
`998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`. The
progress file recorded 10,511.9 seconds elapsed, 245,857 mutations, 244
periodic samples, and two completed mutation bursts. No held-reader or
controlled-crash scenario had run; terminal exactness/history/integrity were
pending. The most recent exact convergence sample was 1,470 ms. The run still
uses the older copied pacing harness and remains an active soak, not a pass.
The captured checkpoint is [`soak-progress-20261110T1200Z.json`](../r132-72h-soak-vps2-20261110/soak-progress-20261110T1200Z.json).

## Remaining qualification

1. Build authenticated release DEB and RPM artifacts containing this source
   and verify their checksums and scriptlet content.
2. On disposable Ubuntu and Rocky guests, test clean install, upgrade from the
   old root-owned state, inactive-service preservation, rollback, and removal.
3. Run the candidate as UID `dux`; verify fanotify, handle reads, full-tree
   scan, DB/WAL permissions, and exact comparisons under concurrent churn.
4. Measure effective capabilities and systemd sandbox policy from the running
   process, then assess the residual `CAP_SYS_ADMIN` exposure.
5. Complete the existing 72-hour soak and production-scale qualification
   before any production rollout.
