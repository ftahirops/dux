# r132 readiness harness regression retest — 2026-11-10 09:50 UTC

The Python qualification and release harnesses were rerun at low scheduler
priority (`nice -n 19`, `python3 -B`) while unrelated Go build/test activity
was present on VPS1. All commands exited successfully.

| Suite | Tests passed | Failures | Runtime |
|---|---:|---:|---:|
| Readiness workflow | 62 | 0 | 1.038 s |
| Package lifecycle harness | 72 | 0 | 1.587 s |
| Release builder CLI | 13 | 0 | 7.932 s |
| Release debug-artifact verifier | 1 | 0 | 3.910 s |
| Qualification VM harness | 41 | 0 | 0.132 s |
| **Total** | **189** | **0** | **14.599 s** |

These validate the qualification scripts and their mocked/unit behavior. They
do not exercise the Dux Rust binary, install an RPM, or replace the pending
Rocky guest lifecycle. The separate r132 72-hour runtime soak remains active;
its final gates have not run yet. No product code changed in this retest.
