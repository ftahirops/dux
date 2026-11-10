# VPS2 production status comparison — 2026-11-10 02:14 UTC

## Read-only observation

VPS2 is running the installed r121 service on kernel `6.8.0-147-generic`. At
02:13 UTC, the service was active in scan-first mode, dirty only because
filesystem changes were waiting for the next observation. The most recent scan
was about 5.6 minutes old. The daemon reported zero kernel overflows, zero
dropped events, zero pending updates, and no write error. Root storage was 94%
used with 5.9 GiB available. The raw installed-service snapshot is
[`vps2-status.json`](vps2-status.json).

The installed r121 status also reported `behind_seconds` of about 12.2 hours
while scan-first was enabled and a recent scan had completed. This is the stale
event-throttle age that the r126 status change is intended to mark as
inapplicable in scan-first mode.

## Candidate comparison on the same live database

Without installing or starting r126, the exact candidate binary
(SHA-256
`9428b49d5d9ee5935cf77e3042eb0a321cd7fb106de6d4464ddaaf4d3951a92b`)
was streamed to a private temporary directory in `/dev/shm` and ran
`status --db /var/lib/dux/dux.db --json`. Candidate output confirmed:

- `scan_first=true`
- `behind_seconds_applicable=false`
- `throttled_since_timestamp_seconds=null`
- `last_scan_age_seconds` about 6.4 minutes
- zero kernel overflows, zero dropped events, zero pending updates
- no write error; daemon still live

The read-only candidate snapshot is
[`vps2-r126-readonly-status.json`](vps2-r126-readonly-status.json). This
demonstrates the intended status semantics against the same live database; it
does not install or exercise the r126 daemon, prove freshness under churn, or
qualify the candidate for production. The installed r121 service and database
were not modified.
