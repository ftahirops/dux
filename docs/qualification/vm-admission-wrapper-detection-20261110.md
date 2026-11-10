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

## Recheck — 2026-11-10 21:43–21:45 UTC

The targeted admission module was rerun at 21:43 UTC after the wrapper
classification change: **41 passed, 0 failed**. The full Python suite has not
been rerun since the added regression test, so the prior aggregate of 380
passed and 1 skipped is unchanged as the last full-suite result. Exact output:
[`targeted test log`](vm-admission-wrapper-regression-20261110T2146Z.log).

At 21:45 UTC, the admission probe found 18 active build/test/analysis
processes, load1 20.96, and CPU PSI some avg10 63.92% (CPU PSI full 0%). The
probe correctly blocks a VM run for both active workloads and pressure. No VM
was started and no fault-matrix suite ran. This preserves the test isolation
gate rather than treating a high-pressure run as useful qualification.
