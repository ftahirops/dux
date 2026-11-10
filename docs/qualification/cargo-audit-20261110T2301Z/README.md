# Dependency audit recheck — 2026-11-10 23:01 UTC

`cargo audit -D warnings` exited **0** against the current lockfile: **229
locked dependencies, zero known advisories, zero warnings**. Cargo Audit was
0.22.2. The lockfile SHA-256 is
`fb3c7f59022ec73a7b33320665dd4f9b15140ee5f9bd8ff055fcb3ea404d10e3`.

The database contained 1,446 advisories at commit
`28d73fc95e5972af3403225a217def57dcaba3c4`; a direct `git ls-remote` at 23:03
UTC confirmed the commit still matched upstream `main`. No newer advisory
snapshot was available. Raw output is in [`cargo-audit.log`](cargo-audit.log);
machine-readable details are in [`evidence.json`](evidence.json).

This checks known dependency advisories only; it does not replace application
code security review or the unrun runtime fault matrix.
