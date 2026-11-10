# Dependency audit — 2026-11-10 22:15 UTC

`cargo audit -D warnings` exited 0 against the current `Cargo.lock`: **229
locked dependencies, zero known advisories, zero warnings**. Cargo Audit was
0.22.2. The lockfile SHA-256 is recorded in [`evidence.json`](evidence.json).

The command fetched the RustSec advisory database. It loaded 1,446 advisories
at commit `28d73fc95e5972af3403225a217def57dcaba3c4`, dated
2026-11-10 14:45:03 +01:00. That is the same database head as the prior audit,
and a direct `git ls-remote` at 22:15 UTC confirmed it matched upstream
`main`; this is a fresh scan, not a newer advisory snapshot. Raw output:
[`cargo-audit.log`](cargo-audit.log).

This checks known dependency advisories only; it is not an application code
security review.
