# VPS2 scan-first observation cycle — 2026-11-10

## Result

The installed Dux **r121** service on VPS2 completed a scheduled full-root
observation under sustained filesystem churn. It started at **08:05:31 UTC**
and completed at **08:11:52 UTC** (about **381 seconds**). The daemon remained
active; its restart count stayed at zero.

The scan reported:

- 1,708,484 files and 202,663 directories;
- 1,911,147 staged path instances and zero directory alias paths;
- 1,907,665 unique inodes;
- 88,734,806,016 visible bytes and 88,547,495,936 unique bytes (reported as
  about 82.47 GiB unique allocated data);
- zero scan coverage issues;
- 12 vanished inode-reuse paths discarded, with zero revalidated or unresolved
  reuse paths.

Post-scan accounting verification passed at **08:13:39 UTC** after **106.913
seconds**. At 08:14:21 UTC, the service was active, used about 598 MiB, and
still had zero reported dropped updates and zero kernel overflow notifications.

## Churn and freshness

During the walk, sampled telemetry reported approximately 20,000–38,000
fanotify hints per second. The available checkpoints showed no increase in
kernel-overflow or dropped-update counters. Because scan-first mode treats
events as hints rather than resolving each one, the post-scan status remained
dirty with reason `filesystem changes await the next full observation` and
`caught_up=false`. This is the correct freshness signal when the tree changes
during an observation; a successful accounting check does not mean the index
describes the filesystem at the later follow-up instant.

## What this qualifies

This is live evidence that the installed r121 scan-first service can complete
a roughly 1.9-million-node full-root observation and pass its internal
accounting check while the host emits heavy event traffic. It is a single
observation cycle, not a soak test. The duration is a substantial fraction of
the configured 15-minute observation interval.

It does not qualify the exact r132 candidate, guarantee zero event loss over a
long period, or prove exact path-by-path equality against an independent
filesystem listing. No package was installed and no service state was changed
for this observation.

## Evidence

The [machine-readable sample and completion record](live-vps2-scan-first-cycle-20261110T080841Z.json)
contains the 08:07:35 and 08:08:37 in-progress snapshots, the completion
metrics, and the 08:14:21 follow-up. The service journal recorded the scan
completion and `reconciliation accounting verification passed` at 08:13:39
UTC.
