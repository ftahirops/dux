# VPS2 r121 canary follow-up — 2026-11-09 22:23 UTC

The read-only five-minute monitor produced sample **100**, captured at
22:23:17 UTC. It passed with no monitor errors. The service remained active
with zero systemd restarts; the snapshot reported zero kernel overflow
notifications, kernel-reported drops, userspace drops, and pending updates.
The last completed full observation was 116 seconds old, within the monitor's
1,200-second limit. SQLite `quick_check` returned `ok`, and the schema-v7 path
graph and root/inode accounting checks passed.

The status was dirty with reason `filesystem changes await the next full
observation`. This is the scan-first mode's expected interim state: sample 100
passed because it checks scan freshness and accounting, not event-batch
catch-up. `status_before` briefly reported accounting verification pending;
`status_after` showed that state cleared and no scan was in progress. Do not
interpret `behind_seconds` as per-file event lag in scan-first mode.

The daemon is still **r121**, not r123. The 24-hour monitor was only about
8 hours 15 minutes into its required duration at this capture. This is not
release sign-off.

The cgroup snapshot reported `MemoryCurrent=310,075,392`,
`MemoryPeak=2,148,278,272`, `MemoryMax=4,294,967,296`, and cumulative
`memory.events.high=1173`, with zero OOM and OOM-kill events. The peak is above
the service's configured 2 GiB `MemoryHigh`; the counter is cumulative and
cannot establish how many high events occurred during this sample. Track its
delta in later samples and investigate if it continues increasing under
ordinary workload. It does establish that the memory-pressure threshold has
been crossed at some point, even though this sample passed and current usage
was lower.

The raw monitor record is [`vps2-canary-sample-100.json`](vps2-canary-sample-100.json),
SHA-256 `5ccf96cb872164fabb183a43223657fa56028ba6a742eac92c87d7cc5f7d3483`.
No production configuration, daemon, or database was changed.
