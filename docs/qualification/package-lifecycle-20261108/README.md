# VPS2 static-musl package and runtime qualification — 2026-11-08

Candidate source: commit `43a7769a7abf8d62d69b3638f5b9dcd22258f718`.
Platform: disposable KVM VPS2, Ubuntu 24.04, kernel `6.8.0-147-generic`, ext4.
The marker `/etc/dux-qualification-sandbox` was root-owned mode `0400` with
the expected qualification token. Production VPS1 was not used or changed.

## Release build results

- The exact source built with the official release script as a static-musl
  binary and DEB/RPM packages. The binary was a static PIE and identified
  itself as Dux 0.6.3, source revision `43a7769a7abf`, source tree digest
  `35d23ab86c4f05465a864effd3970f1fb021920625aeaf84c580d4c476f0fd6b`.
- Release binary SHA-256:
  `edf9c2e56be8555c6b8e9628a388a0ac9363af097d7617b4f89433ac5c34b56f`.
- `cargo-auditable` embedded 202 dependencies. The lockfile report covers 229
  dependencies and recorded zero advisories or warnings against RustSec DB
  commit `309392b18f6b17022f82c701f300433126901068` (updated 2026-11-06).
- VPS2 had no DNS egress. The advisory DB was mirrored locally and the release
  script's Cargo audit command used a local shim adding `--no-yanked`; the
  network-dependent yanked-crate check was therefore **not** qualified.
- The first static-musl compile exposed an ioctl request type mismatch on
  musl (`libc::c_ulong` versus `libc::Ioctl`). It was corrected in commit
  `43a7769`, then the exact official release build completed successfully.

## Package lifecycle attempt: timed out before package transitions

The local lifecycle harness unit suite passed **70/70**. The real Debian
install/upgrade/rollback/purge/clean-install run did **not complete**. The
harness stopped at its first baseline runtime stage after 302.753 seconds:
systemd still reported `dux.service` as `reloading`, so no later lifecycle
steps ran. This is a harness timeout, not evidence that the daemon itself
timed out: the unit allows up to one hour for startup and its scan continued
after the harness exited. The first replacement index appeared shortly after
the 300-second harness limit.
The test used package revisions `0.6.3-98` and `0.6.3-99`, both containing
the same candidate binary; this was intended to exercise package mechanics,
not a real old-code-to-new-code migration. It therefore cannot qualify an
upgrade from the production 0.6.3 executable.

The failed report is `package-lifecycle-deb.json`; the complete service log is
`dux-service-journal.log`. During the run:

- The pre-existing 2.4-GiB index had 233 `REAL` values in `inodes.recursive_bytes`
  among 1,974,271 rows. Several were outside signed 64-bit range (for example
  `-1.4842219161357818e20` and `1.4022532342561264e19`). `dux status --json`
  consequently failed with `Invalid column type Real ... recursive_bytes`.
- A full candidate observation replaced that unreadable index. Status then
  worked and reported 1,431,666 indexed nodes and 80,986,920,000 recursive
  bytes. This is evidence of index repair, but the observation was marked
  dirty and was not caught up.
- During the original package attempt, fanotify reported bounded hint-ring
  and resolver-backlog capacity overflows during the root scan. The status
  snapshot showed 842,240 events seen, zero kernel overflow notifications,
  and zero userspace update drops. A second scan was in progress when the
  five-minute harness readiness limit expired; it had visited 815,390 files
  and 116,331 directories at the last snapshot.
- The daemon logged that it could not persist the dirty marker to the old
  read-only index while reconciliation was active. The candidate remained
  dirty and never reported caught-up before the timeout.
- After stopping the service, a read-only check of the replacement DB found
  all 1,431,666 `recursive_bytes` values stored as integers and
  `PRAGMA integrity_check` returned `ok`. This verifies replacement-index
  structural health, not daemon freshness or reconciliation completion.

After the timeout, the scan finished and replaced the old index. A second
runtime observation was then performed with the harness stopped. The candidate
completed a 1.43M-node observation in about 4.5 minutes with no hint-ring or
kernel overflow; accounting verification remained pending, and a scan began
at 10:50:32 after the 301-second persistent-backlog threshold. That scan then
overflowed the hint ring twice at 10:53:20 and immediately scheduled a third
full scan. At 10:56:01, the third scan was still indexing; it had seen 52,965
events across the runtime, with zero kernel-overflow notifications and zero
reported userspace update drops. The service was stopped at that point to
avoid a repeated full-scan loop.

This longer run does not prove that the daemon would never converge, but it
demonstrates a real retry-amplification problem: one scan-time ring overflow
caused an immediate new full observation, which can repeat under sustained
churn. At the time this initial note was written, the change had not yet been
release-built or tested in the live-root runtime. The source then passed 300
Rust tests (2 ignored), strict Clippy, formatting, and all 70
package-lifecycle unit tests. The 300-second harness cutoff proved too short
for the package's full-root baseline and was increased to match the systemd
one-hour startup allowance. The lifecycle harness now treats a completed
scan-first observation as readiness and records `caught_up=false` separately,
since that mode never promises real-time exactness.

Subsequent release and runtime results are recorded in the
[fixed-cadence scan-first qualification](../scan-first-fixed-cadence-20261108/README.md).
In brief, package 101 was built and exercised on VPS2; a 40,000-file burst
caused userspace hint loss and one incomplete observation, followed by a
successful bounded retry. The fixed-cadence scheduler avoided the old
300-second dirty retry after that successful scan. These follow-up results do
not qualify sustained churn, 15-minute freshness, or production readiness.
VPS2 remains the only host modified for these tests; production was untouched.

## Evidence files

- `build-manifest.json` — source tree and release artifact hashes.
- `cargo-audit-lockfile.json` — dependency advisory result and database age.
- `package-lifecycle-deb.json` — machine-readable failed lifecycle report.
- `dux-service-journal.log` — daemon warnings, errors, and runtime metrics.
- `post-scan-index-checks.json` — read-only type and SQLite integrity result
  after stopping the daemon.
- `runtime-long-observation.log` — sampled status snapshots from the extended
  runtime observation.
- `fix-cargo-test.log` — final source Rust suite output.
- `fix-lint-test.log` — formatting, strict Clippy, and Python lifecycle suite.
