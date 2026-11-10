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

At 10:12:40 UTC, a fresh read-only host sample measured load average **10.77**,
above the configured start limit of 10. No VM was started or lifecycle attempt
made in this sample; the guard remains in force.

At 10:23:29 UTC, the guard's read-only admission evaluator still found three
active Go test workloads and I/O PSI `some avg10=16.84%`, above its
10% startup limit. No VM was launched. Retry only after both the workload and
I/O pressure clear through the normal three-sample admission procedure.

At 10:45 UTC, a new guarded ephemeral start was attempted after load and I/O
pressure had fallen. Admission refused on sample 1/3 because five host Go
compiler/test processes were active. No VM was started and no package phase
ran. The guard was not bypassed.

At 10:48 UTC, the normal three-sample admission passed and a new ephemeral
guest booted successfully as Rocky Linux 9.8; SSH and cloud-init completed.
Before the first package copy, the independent runtime guard stopped the VM at
10:50:15 UTC when a Go test binary and `go test` process appeared. The first
SCP was refused because the guest had already stopped. No package/key reached
the VM and the RPM lifecycle did not start. The stop was not a Dux or RPM
failure. A disposable-key-signed r132 copy had been generated and verified on
the host, but it was never installed; its key workspace was removed. The
guard event and test artifact identity are recorded in
[`the 10:48 guarded attempt`](r132-rocky-guarded-abort-20261110T1048Z.json).

At 10:51:58 UTC, another startup passed admission, but the runtime guard
stopped the guest ten seconds later after a Go compiler and `go test` process
appeared. SSH was not confirmed and no package copy or lifecycle phase ran.
The second disposable signing workspace was removed as well. See
[`the 10:51 guarded attempt`](r132-rocky-guarded-abort-20261110T1051Z.json).

At approximately 11:01 UTC, another start was refused at admission sample 2/3
when Go compiler/build/test workloads appeared. CPU PSI some avg10 was 25.37%
in the subsequent host sample. The disposable-key-signed r132 RPM had passed
host verification, but no VM started and no package was copied. The
throwaway key/workspace was removed. See
[`the 11:01 admission refusal`](r132-rocky-admission-refusal-20261110T1101Z.json).

At 11:09:42 UTC, a guarded Rocky 9.8 guest started and completed cloud-init.
After importing a disposable test key, Rocky's RPM verifier accepted the
signed r132 copy and reported the header signature, header digests, and payload
digest all `OK`; package queries returned baseline `0.6.3-131` and candidate
`0.6.3-132`. At 11:11:22 UTC the independent guard stopped the VM when a Go
test started, before the lifecycle harness or any RPM install began. This
qualifies cross-platform RPM signature mechanics with a test key, not the
release owner's publisher identity or the install/upgrade/rollback lifecycle.
The temporary key and workspace were removed. Evidence:
[`the Rocky signature check`](r132-rocky-rpm-signature-test-20261110T1111Z.json).

## Static package authenticity inspection — 2026-11-10 10:13 UTC

The exact r132 artifact
`target/readiness/r132-release-20261110/dux-0.6.3-132.x86_64.rpm` has SHA-256
`bb1adbd0b2bc5a36c30076bcfd3436e424dbd596aa07a26d78d952a24650af78`. RPM
metadata inspection confirmed version `0.6.3-132`, x86_64 architecture, the
expected `/usr/bin/dux` and service/helper payload, and the embedded executable
hash matching the qualified r132 binary. `rpm --checksig -v` verified header
and payload digests, but both RPM signature fields are `(none)`: **the package
is unsigned**. A checksum detects accidental or post-publication changes only
when the checksum itself is trusted; it does not establish publisher identity.

This is not a failed lifecycle test. No package was installed. Before
production distribution, the release owner needs to sign the RPM and provide
an independently trusted signing-key fingerprint; the detached checksum
manifest should also be signed and verified in the Rocky guest. RPM lifecycle
qualification remains pending separately.

The command transcript is preserved in
[`the static RPM inspection`](r132-rocky-rpm-static-inspection-20261110T1014Z.txt).

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

### Guest stopped during boot — 2026-11-10 09:19 UTC

A later startup passed admission with a 25% CPU quota and an ephemeral tmpfs
overlay. The Rocky 9 guest was still booting and SSH had not become available
when the independent pressure guard stopped it at 09:19:17 UTC, after Go test
processes appeared on the host. The guard observed load1 2.01, CPU PSI `some`
45.7% and `full` 0%, and IO PSI `some` 0.82%; its stop reason was the active
Go test workload. No RPM was copied and no lifecycle phase ran. The exact guard
event is preserved in
[`the 09:19 guard event`](r132-rocky-rpm-guarded-start-20261110T091917Z.jsonl).
This confirms the practical issue: a guest boot takes longer than the gap
between unrelated host tests. Do not weaken the guard; retry when those tests
are quiescent long enough for boot and qualification.

### Guest reached SSH; guard stopped before lifecycle — 2026-11-10 09:28 UTC

A later start passed admission at 09:27:10 UTC with the standard guest and a
75% CPU quota. SSH became available; both RPMs and the lifecycle script were
copied to the guest, and the guest-side RPM hashes matched the verified
artifacts (r131 `2309e823…68d44a`, r132 `bb1adbd0…4af78`). Before the lifecycle
script started, Go test processes appeared and the pressure guard stopped the
guest at 09:28:20 UTC. The guard recorded memory PSI `full` 3.93%, CPU PSI
`some` 34.06%, and load1 3.39; its stop reason was the active Go test workload.
No package install or upgrade ran. The exact record is in
[`the 09:28 guard event`](r132-rocky-rpm-guarded-start-20261110T092820Z.jsonl).

At approximately 09:29:29 UTC, another 75%-quota start was refused on admission
sample 1/3 after seven Go compiler/test processes appeared. No VM was created.

At 09:31:18 UTC, another start was refused on admission sample 1/3 after a Go
test binary and `go test` process appeared. No VM was created. The admission
decision is saved in
[`the 09:31 refusal record`](r132-rocky-rpm-admission-refusal-20261110T093118Z.jsonl).

At 09:35:06 UTC, a further start was refused on sample 3/3 after three Go
compiler processes and one Go test process appeared. No VM was created. The
decision is preserved in
[`the 09:35 refusal record`](r132-rocky-rpm-admission-refusal-20261110T093506Z.jsonl).

### Admission window closed during sampling — 2026-11-10 09:23 UTC

At 09:23 UTC, a fresh host sample showed no build/test workloads, load1 3.72,
CPU PSI `some` 2.27%, and no admission blockers. A guarded retry using a 75%
CPU quota was refused on sample 3/3 when a Go test process appeared. An
immediate retry was refused on sample 2/3 after two more Go test processes
started. Both refusals happened before VM creation; no package lifecycle step
ran. This demonstrates that current host test activity is recurring even
across brief idle snapshots. The qualification guard is functioning as
designed; wait for a sustained quiet period before another attempt.
