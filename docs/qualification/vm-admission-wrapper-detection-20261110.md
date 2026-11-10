# VM admission wrapper detection — 2026-11-10

## Finding and change

The qualification VM admission probe classified direct `go test`, compilers,
and explicitly named shell test scripts. A PostgreSQL test launcher can instead
keep the test session in a `pg_virtualenv` wrapper process while it prepares a
temporary database. The admission probe did not have an explicit classification
for that wrapper. Added a `PostgreSQL test wrapper` workload label so this
launcher blocks VM admission during its test session.

This is a qualification safety fix; it does not change the Dux daemon.

## Verification

Added a synthetic `/proc` fixture using the observed `pg_virtualenv -t bash -c`
argument shape with a nested `go test` command. The complete VM runner unit-test
module passed after the change:

```text
PYTHONPATH=scripts python3 -m unittest test_dux_qualification_vm
Ran 41 tests in 0.233s
OK
```

The full Python qualification suite (380 passed, 1 skipped in the previous run)
has not been rerun after this change. The guarded P0 VM matrix has not started.
The exact r132 candidate binary and source digest were verified and staged, but
a source-matched fault-injection build was stopped under host pressure before it
completed.

At 21:36 UTC, host admission was blocked by active Go compilation/test work:
load1 17.73 and CPU PSI some avg10 33.45%. No VM was started. Evidence:
[`vm-admission-20261110T2136Z.json`](vm-admission-20261110T2136Z.json).
