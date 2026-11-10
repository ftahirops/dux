# r132 Rocky RPM lifecycle prerequisite — 2026-11-10

## Verified guest image

The Rocky 9 GenericCloud Base image was downloaded from Rocky's official
[Rocky 9 x86_64 image repository](https://download.rockylinux.org/pub/rocky/9/images/x86_64/):
`Rocky-9-GenericCloud-Base.latest.x86_64.qcow2` (645,988,352 bytes). The
published SHA-256 checksum file was `CHECKSUM.asc`-verified with the Rocky 9
release key. Its full fingerprint
`21CB256AE16FC54C6E652949702D426D350D275D` matches Rocky's
[published signing-key information](https://rockylinux.org/sv-SE/resources/gpg-key-info).
The signature verified successfully and the downloaded image matched signed
SHA-256 `92c206cc6f790c61583247eefe87890f8828420662c17cacf247cec78ab4eec8`.
The repository listed this image as Rocky 9.8, published 2026-05-25.

The image is staged in `/dev/shm/dux-qualification-images/rocky9-verify/` and
is linked into the ignored local VM image catalog. The local catalog records
the source URL, digest, signature URL, and signing-key fingerprint.

## RPM lifecycle status

The Rocky RPM lifecycle has **not run**. VM startup was refused by the
qualification admission guard because unrelated Go test/build workloads were
active; at the latest refusal, load and I/O pressure also exceeded the guard's
limits. No guard bypass was attempted and no guest was started. The verified
image is ready for a later retry once host admission is clear. Until then, RPM
upgrade/rollback/removal/clean-install behavior on Rocky is unqualified.

### Guarded retry — 2026-11-10 09:08 UTC

A later start passed the three-sample startup admission and launched the
ephemeral guest at 09:08:37 UTC. Eleven seconds later, the independent runtime
pressure guard detected five Go compiler processes and one Go race-test process
and stopped only this qualification VM. SSH never became available; all three
file-copy attempts were refused, and no RPM lifecycle phase ran or package was
changed. The stopped event and pressure sample are preserved in
[`the captured guard event`](r132-rocky-rpm-guarded-start-20261110T090848Z.jsonl).
Retry after the new host workloads finish and admission remains clear.

### Admission retry refused — 2026-11-10 09:15 UTC

At 09:15 UTC, the ephemeral Rocky guest start was retried with the standard
profile and a 25% CPU quota. The admission guard refused before starting a VM:
sample 1/3 reported host load1 **11.73**, above the configured limit of **10**.
The host still had a Go test process consuming substantial CPU. No guest was
started, no package lifecycle phase ran, and no package or service was changed.
This is another admission refusal, not a failed RPM lifecycle test. Retry only
after the guard's three stable samples pass.
