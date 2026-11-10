# r127 Debian lifecycle and WAL behavior on VPS2 — 2026-11-10

## Result

The checked-in Debian lifecycle harness completed **passed** on the marked KVM
qualification VM VPS2 (Ubuntu 24.04, Linux 6.8.0-147-generic) in 1,332.532
seconds. It exercised the exact r121 and r127 DEBs recorded in the report.
All five lifecycle stages passed: r121 baseline, upgrade to r127 while
preserving disabled-service state, rollback to r121 with a checked SQLite
backup restore, re-upgrade/remove/purge, and clean r127 install.

The r127 clean-install scan observed 1,664,114 files and 197,276 directories
(1,861,390 staged path instances), reported **zero coverage issues**, and
revalidated four inode-reuse paths. Accounting verification passed. The final
status was dirty only because scan-first waits for its next full observation;
`behind_seconds_applicable=false`, zero dropped events, zero kernel overflows,
and no write error were recorded. Systemd reported zero service restarts, a
2 GiB `MemoryHigh`, 4 GiB `MemoryMax`, one-core CPU quota, and 5.67 GiB read /
7.39 GiB written during that daemon lifetime. No 24-hour canary or independent
filesystem equality test was part of this lifecycle.

## WAL finding and interpretation

This lifecycle tested source commit `f0673f548fe60f8df238682d2a7c40ada75c3dd8`,
which attempts PASSIVE checkpoints while a scan is active. During the clean
install, the SQLite WAL nevertheless reached **717,609,272 bytes**, above its
256 MiB guard. The checkpoint returned `busy=1`, `log_frames=-1`, and
`checkpointed_frames=-1`; event writes were paused. The scan completed three
seconds later with zero coverage issues, and the WAL returned to
134,217,728 bytes with event writes resumed. No events were reported dropped
and no kernel overflow or write error occurred.

This shows the code change does not prevent the clean-install publication
burst: the backup/publication writer can pin the database so PASSIVE checkpoint
cannot make progress. The guard bounds behavior by pausing event writes, but
still creates a large temporary WAL and a freshness risk. The r127 package
lifecycle **passed**, while the WAL reliability/freshness gate remains open.
The 256 MiB guard must not be described as avoided or fixed.

## VPS2 restoration

Before the destructive lifecycle, the service was stopped and its r121 index
and binary were preserved. Afterward, r121 was reinstalled and the saved
`dux.db` restored byte-for-byte: SHA-256
`75f8c0d3fea0765b076c7b8fb4d7b0550d92a6bf0d500096ed80c80c64e2bf1a`; SQLite
`quick_check` returned `ok`. The r121 binary again matched SHA-256
`b59d46b66e38d74da4ea4359137977e670ac9c9a05192b971131f8f0937d0d70`.
The post-restore r121 observation completed with 1,665,459 files and 197,491
directories, zero coverage issues, 130 vanished paths safely discarded, and
two inode-reuse paths revalidated. Accounting verification passed in 76.8
seconds. The service remained active and enabled with zero restarts, drops, or
overflows; status returned to the normal scan-first dirty reason awaiting the
next observation. Lifecycle-only staged packages, runner, rollback snapshot,
and preservation copy were removed after verification. VPS2 free space
increased from 2.9 to 4.8 GiB.

## Build and artifacts

The build manifest records clean source commit and source-tree digest. The
r127 DEB SHA-256 is
`b0644a9674837019c2fb0e6aeeb3a010e70e3cf31beef158811618248744f94b`.
The package pipeline passed binary/dependency audits, DEB/RPM packaging,
payload checks, source/revision checks, and artifact checksum verification.
The binaries themselves remain in the local ignored build tree
`target/readiness/production-r127-20261110/`.

- `dux-r127-lifecycle-report.json` — complete stage-by-stage assertions.
- `dux-r127-lifecycle.log` — streamed stage checkpoints and terminal pass.
- `dux-r127-lifecycle-journal.log` — VPS2 service and WAL log evidence.
- `build-manifest.json`, `SHA256SUMS`, and audit logs — exact release build
  evidence.
