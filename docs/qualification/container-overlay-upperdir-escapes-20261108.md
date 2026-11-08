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
NUL, values above one byte, empty paths, and relative paths. Malformed values
return an explicit error propagated to the caller; they cannot fall through to
the container inventory's zero-byte fallback. Escaping is decoded after
comma-delimited option parsing, so an escaped comma inside a path is not
mistaken for a separator. Tests cover an upperdir containing an escaped comma,
space, and literal backslash, non-UTF8 path bytes, and invalid values.

Results on this source:

- Focused container module: **14 passed, 0 failed**.
- Full default-feature Rust suite: **316 passed, 0 failed, 2 ignored**.
- Full all-features Rust suite: **319 passed, 0 failed, 2 ignored**.
- Formatting, strict all-target/all-features Clippy, diff checks, and stable
  and nightly coverage runs passed. The previous host release binary predates
  explicit malformed-input error propagation and is not the current candidate.

Coverage after this change is **69.33% stable line** and **67.35% nightly
branch** overall. `containers.rs` is **63.30% line / 58.97% branch**. These
percentages are not a complete proof of the container accounting path.

An initial package revision 104 build completed its static-musl link, then the
release script rejected the artifact because the source tree changed while
that build was running (qualification notes had been updated). The provenance
check worked as intended; no package was published from that attempt. A clean
build from the finalized source commit is still required.

The live VPS2 Dux service and its d8ea72d binary were not changed by this fix.
The first monitor attempt for that binary subsequently failed on a separate
inode-incarnation scan issue; a separately named retry monitor is active. The
OverlayFS parser fix has not yet been packaged or exercised against a real
container whose storage path contains these escaped characters. The parser
regression tests use kernel-format mountinfo fixtures.

### Kernel-format smoke check

On the local Linux 6.8 host, a temporary OverlayFS mount with an upperdir
whose name contained a space mounted successfully. The kernel's actual
`/proc/self/mountinfo` record emitted that path as `upperdir=.../upper\040space`,
matching the format handled by the regression test. The mount was unmounted
and its temporary tree removed. This verifies real kernel formatting for a
space, but is not a Dux-to-container end-to-end test; comma and backslash
paths were verified with parser fixtures only.
