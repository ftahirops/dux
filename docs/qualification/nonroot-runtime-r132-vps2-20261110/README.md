# r132 non-root runtime probe on VPS2

## Result

**Passed bounded runtime probe** on 2026-11-10. The exact r132 binary
(`998db222dcc0b86943b33b05bc0153c8729fc074617f95e164b7940da03f0d76`, source
`f02ba7ad235ea841a99de27b69fcd402e3c3927c`) started in a transient systemd
unit as the locked `dux` system account on a disposable 512 MiB ext4 loop
filesystem.

The process ran as UID **996**, GID **115**, with all five capability masks
(`CapEff`, `CapPrm`, `CapBnd`, `CapInh`, `CapAmb`) equal to **`0x200004`** —
`CAP_DAC_READ_SEARCH | CAP_SYS_ADMIN`. The unit used a 10% one-core CPU quota,
256 MiB memory limit, 120-second runtime limit, and `UMask=0027`.

The probe seeded 32 files, waited for the initial observation, created one
additional file, and observed the fanotify event counter advance. A `dux scan`
triggered a second observation; the candidate reported accounting verification
passed, zero scan coverage issues, and 35 staged paths. `dux find` returned the
new file and SQLite `PRAGMA quick_check` returned `ok`. The sampled status
remained conservatively dirty while hints overlapped the scan; this test does
not claim a clean steady-state index under continuous host churn.

The existing VPS2 Dux service and the soak were both still active with zero
restarts after cleanup. The temporary account, loop mount, image, and fixture
were removed; the original `dux:x:115:` group entry was verified restored.

### Direct file-handle capability test

A separate transient unit ran the checked-in C probe as the actual `dux` UID
996/GID 115 with the same five `0x200004` capability masks. On VPS2 ext4 it
called `name_to_handle_at` for a file inside a mode-000 directory, then opened
and read that file with `open_by_handle_at`. It returned the expected payload
and `handle_type=1` (`handle_bytes=8`). This confirms the kernel capability
path for an inaccessible file under the intended service identity. It does
not exercise Dux's internal event-handle decoder or replay logic.

## Test setup incidents

Two setup attempts did not pass and are not counted as product failures:

1. The first transient unit could not execute the binary because its parent
   soak directory is root-only (`0700`). The trap removed its temporary fixture
   and account.
2. A 128 MiB ext4 image provided only about 99 MiB free. Dux correctly deferred
   reconciliation below its 256 MiB free-space floor. The next attempt used a
   512 MiB image and passed.

The first cleanup used `userdel` on an account whose primary group was the
pre-existing `dux` private group, causing `userdel` to remove that group. The
original GID 115 group was immediately recreated and verified, the service
continued with zero restarts, and the later probe cleanup explicitly preserves
and verifies the original group entry.

The initial C helper used an `O_PATH` mount descriptor and got `EBADF` from
`open_by_handle_at`; using an `O_RDONLY|O_DIRECTORY` mount descriptor fixed the
probe. One later invocation also had a remote helper-path typo and exited before
the helper ran; its cleanup checks passed.

## Scope limits

This validates the exact r132 daemon’s non-root fanotify startup, capability
sets, small ext4 scan, event observation, explicit rescan, query, and SQLite
integrity. The separate C probe validates the kernel handle API under the
`dux` UID and capabilities. It does **not** qualify Dux's internal handle
decoder/replay, DEB/RPM install/upgrade/rollback, a full production-size tree,
multi-day operation under that UID, or the full packaged systemd sandbox. The
probes used temporary units; the installed VPS2 service was not changed.

## Evidence

- [`unit-journal.log`](unit-journal.log): startup, fanotify, scan, and shutdown
  output from the transient unit.
- [`open-by-handle-probe.c`](open-by-handle-probe.c): direct kernel handle API
  test source.
- [`handle-unit-journal.log`](handle-unit-journal.log) and
  [`open-by-handle-result.txt`](open-by-handle-result.txt): handle-test result.
- [`cleanup-state.txt`](cleanup-state.txt): group restoration, account removal,
  no loop mounts, and existing service/soak state after the test.
