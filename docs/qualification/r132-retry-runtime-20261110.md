# r132 working-tree release and retry qualification — 2026-11-10

This report covers source commit `fad49686709d`, which includes the scan-first
pending-accounting guard. The binaries were built from this committed tree,
but no package was produced and no installed service was replaced.

## Release build

- Production build: `cargo build --locked --release --bin dux`
- Fault-injection build: `cargo build --locked --release --features fault-injection --bin dux`
- Both reported source SHA-256 `07762b1089a13f6f346cde72edbe4b1dfbf5c70034c4172dfd7241edb5c72e55`
- Production binary SHA-256: `9f166fe626c476690def55a6154c17a5fc0d67b324253ff8a7fab320ce02726f`
- Fault-injection binary SHA-256: `f53aadf6184795acc79927894f2b9bd002994d5d52176bf5d98e70e86ab511e7`
- Both `--version` checks identified source revision `fad49686709d`; production
  output identified `[production]`, and the test binary identified
  `[fault-injection]`.

The 5.4 MiB production ELF was dynamically linked for x86-64 glibc and stripped.
The build outputs were placed in tmpfs at low CPU and I/O priority to avoid
consuming the nearly full VPS2 root filesystem.

## Isolated runtime test on VPS2

The `retry` suite from `scripts/dux-readiness.py` passed on the KVM test guest
`192.168.5.106` in a transient systemd unit with a private mount namespace,
25% CPU quota, and 4 GiB memory ceiling. It used a disposable 768 MiB ext4 loop
filesystem on `/dev/shm`; the initial fixture had 256 seeded files plus the
suite's sparse-file, hardlink, and byte-name cases. Runtime was 62.058 seconds.

Evidence reports:

- `retry_mode=scan_first_persisted_bounded_backoff`
- Persisted reconciliation failure state and cooldown survived daemon restart
- Event ingestion continued during cooldown
- Automatic retry completed without an operator request
- Final fixture index matched the filesystem exactly
- Suite result: passed

The installed r121 daemon remained at PID 576850 with zero restarts. Its own
scheduled full observation was in progress at the post-test sample. The
isolated fixture directory was removed; the production database and service
were not changed. Local copies of the machine-readable suite report and log
are under `target/readiness/r132-working-tree-20261110/`.

## Scope limits

This is a runtime test of the bounded scan-first retry lifecycle. It does not
specifically orchestrate an in-scope filesystem event between scan completion
and pending accounting verification; the newly added guard currently has a
focused unit test, not a runtime race test. This suite also does not cover
power loss, other filesystems or kernels, long-duration WAL readers, large
scale performance budgets, package upgrade/rollback, default-privilege review,
the 72-hour soak, or the 24-hour canary. The r132 working-tree code is not
production-ready sign-off.
