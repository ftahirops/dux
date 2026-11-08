# OverlayFS upperdir escape parsing regression

**Date:** 2026-11-08

**Status:** fixed in source; not yet in the running VPS2 candidate

## Defect

The container view extracts `upperdir=` from `/proc/<pid>/mountinfo` and uses
the value as a path in Dux's index. Linux OverlayFS emits mount options through
`seq_show_option()`, which escapes comma, whitespace, and backslash bytes as
three-digit octal sequences. Dux previously returned that escaped spelling
unchanged. For example, a real path containing a space can be reported as
`/srv/docker/upper\040dir`, which is not the real directory. Its lookup then
returns zero and undercounts the container's writable layer.

The kernel implementation calls `seq_show_option(m, "upperdir", ...)` for
OverlayFS upper paths ([Linux OverlayFS params.c](https://code.googlesource.com/linux/torvalds/linux/+/669d089a7fe1eacf4e86160297f32c678ee71ec5/fs/overlayfs/params.c));
`seq_show_option()` escapes option values with `seq_escape()` ([Linux seq_file.h](https://github.com/torvalds/linux/blob/master/include/linux/seq_file.h)),
and the kernel documents that `seq_escape()` represents selected characters
in octal ([seq_file documentation](https://kernel.googlesource.com/pub/scm/linux/kernel/git/stable/linux-stable/+/refs/heads/master/Documentation/filesystems/seq_file.rst)).

## Change and checks

`src/containers.rs` now decodes three-digit octal bytes in the `upperdir`
option before constructing the path. It rejects truncated/non-octal escapes,
NUL, and values above one byte rather than returning a misleading path.
Escaping is decoded after comma-delimited option parsing, so an escaped comma
inside a path is not mistaken for a separator. Tests cover an upperdir
containing an escaped comma, space, and literal backslash, plus malformed
escape cases.

Results on this source:

- Focused container module: **13 passed, 0 failed**.
- Full default-feature Rust suite: **315 passed, 0 failed, 2 ignored**.
- Full all-features Rust suite: **318 passed, 0 failed, 2 ignored**.
- Formatting, strict all-target/all-features Clippy, diff checks, stable and
  nightly coverage runs, and a default-feature optimized release build passed.
- The local default-feature release binary reported SHA-256
  `a0f4221cbfd206af5dd8e5484036f97f079e8e1549f9bcf0faf22b3c6d0b9981`; its
  version output included source hash
  `8dcda3835f25e6e8a3dc2524c649013cf40db6af3a6d8afee87946fb805ae6ea`. A
  strings check found no `DUX_TEST_*` fault-injection controls in this
  production-profile binary.

Coverage after this change is **71.94% stable line** and **67.51% nightly
branch** overall. `containers.rs` is **62.76% line / 58.97% branch**. These
percentages are not a complete proof of the container accounting path.

The live VPS2 service and its d8ea72d canary were not changed. The fix has not
yet been packaged or exercised against a real container whose storage path
contains these escaped characters. The parser regression tests use
kernel-format mountinfo fixtures.

### Kernel-format smoke check

On the local Linux 6.8 host, a temporary OverlayFS mount with an upperdir
whose name contained a space mounted successfully. The kernel's actual
`/proc/self/mountinfo` record emitted that path as `upperdir=.../upper\040space`,
matching the format handled by the regression test. The mount was unmounted
and its temporary tree removed. This verifies real kernel formatting for a
space, but is not a Dux-to-container end-to-end test; comma and backslash
paths were verified with parser fixtures only.
