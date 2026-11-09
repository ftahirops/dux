# r121 to r123 Debian lifecycle qualification

## Result of the first real run

The disposable Ubuntu 24.04 KVM guest completed all six package stages on
2026-11-09 in 88.22 seconds. The lifecycle report says `passed`; its SHA-256 is
`5782d947b5ff5b4c067b345aed503d868fb708df23aaa56a028ff17f56afd92d`.
The guest was stopped and cleaned up successfully. This was an isolated test
guest; neither production service nor the VPS2 canary was modified.

Stages and recorded outcomes:

1. Reinstalled and started the r121 baseline package.
2. Upgraded to r123 and verified the candidate payload binary hash.
3. Rolled back to r121 and verified the restored index with SQLite `quick_check`.
4. Re-upgraded to r123.
5. Removed and purged the package; verified state retention on remove and state
   deletion on purge.
6. Clean-installed r123; verified service enablement, disabled tunnel, no
   generated tunnel identity, systemd policy, state/index ownership and modes,
   and installed binary/helper hashes.

Evidence file: [`report.json`](report.json).

## Qualification limitation found in the report

The report exposed a harness timing gap: the clean-install stage returned while
the first observation's `dirty_reason` was still
`reconciliation accounting verification pending`. The daemon was active and
had completed a scan, but independent accounting had not completed. The same
condition appeared in the r121 baseline. Therefore the six package operations
were exercised successfully, but this run does not qualify completed initial
accounting/readiness.

The harness now waits past that specific pending-accounting state before
accepting a completed initial observation. Its regression test and full Python
suite pass; a second real lifecycle run using the tightened harness is still
required before marking the lifecycle gate qualified.

## Artifact identity and scope

- Candidate source commit: `0cad80e6436202fefc2ff385c18872ce2b4a910c`.
- r123 executable SHA-256: `0539487212efc3da1fd144e4aa58f648634c5ba66340ecdab6af3705fa2853a8`.
- r123 DEB SHA-256: `3e9b5c61422780d577112d8fc8f4cfb3cf95b88f14b379eb60793c63f6e205c4`.
- r121 DEB SHA-256: `091930e93fd36dd7a93cdc7983ded6e893c207035b66d5d010cec585bde81d57`.
- Guest kernel: `6.8.0-147-generic`; virtualization: KVM.
- Package-stage coverage is Debian/Ubuntu only. RPM lifecycle, long-running
  candidate behavior, production migration, and endurance remain separate gates.

The clean-install unit retained root execution with `CAP_SYS_ADMIN` and
`CAP_DAC_READ_SEARCH`; this remains a material privilege risk requiring the
documented threat review. This package test does not resolve that risk.
