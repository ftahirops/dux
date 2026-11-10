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
