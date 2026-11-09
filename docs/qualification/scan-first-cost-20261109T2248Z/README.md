# Scan-first live cost sample — VPS2 r121, 2026-11-09 22:48 UTC

This read-only analysis uses consecutive five-minute canary samples 101–105.
The counters are from the Dux systemd cgroup; the report uses device `252:0`
once and does not add its lower-level duplicate device counters. Sample 105
passed, with a full observation active, scan age 844 seconds, zero overflow
notifications/drops, and no OOM events.

| Interval | Observation state across interval | CPU time / one-core share | Cgroup reads | Cgroup writes | Fanotify events seen |
|---|---|---:|---:|---:|---:|
| 101→102 | Observation began | 184.0 s / 61.4% | 1.519 GB | 0.173 GB | 5.78M (19.3k/s) |
| 102→103 | Observation completed; accounting passed | 152.9 s / 51.0% | 1.302 GB | 1.737 GB | 7.49M (25.0k/s) |
| 103→104 | No observation active | 64.1 s / 21.4% | 0 | 0 | 7.07M (23.6k/s) |
| 104→105 | Next observation began | 172.5 s / 57.5% | 1.636 GB | 0.654 GB | 7.79M (26.0k/s) |

The 22:34 observation itself walked 1,613,677 files and 192,125 directories
in about 275 seconds, reported zero coverage issues, and passed independent
accounting in 51.258 seconds. On a 900-second schedule, that scan plus
accounting consumes about 36% of each cadence before counting fanotify drain,
database queries, and normal service work. The no-scan interval also used
21.4% of one core while 23.6k events/second were reported.

These are measured costs and correlations, not an isolated benchmark: cgroup
counters include all work charged to Dux, and the intervals include work on
both sides of scan start/completion. They show that the architecture is
maintaining freshness without observed loss in this sample, but they do not
support a claim of negligible overhead. Next performance work should profile
scan, accounting, and hint-drain CPU/I/O separately before changing scan
cadence or relaxing the event reader budget.

Raw samples: [`samples-101-to-105.jsonl`](samples-101-to-105.jsonl), SHA-256
`e11927bab23a4b8d1148e1d8cb0a4d5ce0e88c73518c3821b8c8e63ea542624f`.
