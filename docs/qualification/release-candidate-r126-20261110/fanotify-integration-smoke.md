# r126 privileged fanotify integration smoke — VPS2, 2026-11-10

## Artifact and environment

- Binary: r126 static production candidate, built from source commit
  `2a884a8ed6d5a63a5019322241de418354f560dc`.
- Binary SHA-256:
  `9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b`.
- Host: VPS2, Ubuntu 24.04.3 LTS, kernel `6.8.0-147-generic`, ext4.
- Test filesystem: 128 MiB ext4 loop image backed by `/dev/shm`, mounted only
  inside a private mount namespace. The live r121 Dux service and its database
  were not modified.
- Harness: [`fanotify-integration-smoke.sh`](fanotify-integration-smoke.sh),
  SHA-256 `698cd105c41004a64001620d9ec5fec97660dcba522dbfa0f26bc6c5991b7184`.

The run staged the binary at `/dev/shm/dux-r126-fanotify-test`, verified its
SHA-256 against the value above, and invoked the harness as root with:

```sh
unshare --mount --propagation private --fork \
  /bin/bash /dev/shm/dux-vps2-r126-fanotify-smoke.sh
```

## Results

The harness ran both modes sequentially on the isolated filesystem.

| Check | Event-apply compatibility mode | Default scan-first mode |
|---|---|---|
| `fanotify_init` | Succeeded | Succeeded |
| `fanotify_mark` | Succeeded | Succeeded |
| `open_by_handle_at` | Multiple successful syscalls traced | Not used for event hints, as expected |
| Runtime changes | Eight creates and one rename | One create recorded as a hint |
| Final index | New and renamed paths present; old name absent | New path appeared after forced full observation |
| Events | 11 seen, 11 resolved | 1 seen, 0 resolved before full observation |
| Reported drops / kernel overflows | 0 / 0 | 0 / 0 |
| Full observation and accounting | 0 coverage issues; accounting passed in 99 ms | 0 coverage issues; accounting passed in 99 ms |

The default-mode daemon reported `scan_first=true` and a 900-second scheduled
observation interval. A `SIGHUP` requested the next full observation after the
test file was created; the file was then found in the index. This confirms the
intended distinction: event-apply resolves event handles and updates paths,
while scan-first records hints and publishes filesystem state through an
observation.

The first harness attempt stopped before its event assertions because its
fixture expected zero-padded sequence numbers that `seq -w 1 8` did not produce.
The fixture was corrected and the complete rerun passed. This was a harness
setup error, not a Dux failure.

Afterward, the test image, loop device, temporary databases and sidecars,
heartbeat/status files, and both test daemons were confirmed cleaned up. VPS2's
existing r121 service remained active with zero restarts, zero kernel
overflows, and zero reported drops. Root remained at 94% use with about 6.0 GiB
free; the test image occupied tmpfs and did not consume root-disk space.

## Limits

This is a small isolated privileged integration test. It closes the basic
fanotify success-path gap for this kernel/filesystem and candidate binary. It
does not test capability-negative startup, stale-handle failure semantics,
package install/upgrade/rollback, production-scale throughput, or a 24-hour
canary. Runtime syscall tracing is separate evidence; the saved LLVM unit
coverage profile still has no branch counters and does not instrument these
syscall call sites.
