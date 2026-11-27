# Python qualification rerun — 2026-11-10 22:05 UTC

The complete qualification tooling suite was run twice after the last admission
detector change.

## Result used for qualification

After host pressure had fallen to load1 5.96, CPU PSI some 6.47%, I/O PSI some
3.27%, and no detected build/test workloads, the normal-priority command
completed successfully:

```text
PYTHONPATH=scripts python3 -m unittest discover -s scripts -p 'test_*.py'
Ran 381 tests in 17.800s
OK (skipped=1)
```

The one skip is the RPM signing integration that requires its RPM fixture.
The full output, including JSON emitted by the soak-evidence parser tests, is
in [`python-suite-rerun-20261110T2205Z.log`](python-suite-rerun-20261110T2205Z.log).

## Throttled attempt

An earlier full run was deliberately constrained to `CPUQuota=5%`,
`IOWeight=1`, and `Nice=19`. It ran 381 tests in 426.299 seconds and ended
with 11 errors and one skip. Every reported error was a subprocess timeout in
release-builder or CLI tests, with individual budgets from 5 to 60 seconds.
The entire suite passed on the subsequent normal-priority rerun without code
changes. The capped run is therefore retained as a resource-starvation result,
not counted as a product regression or a passing run. It demonstrates that
this suite's wall-clock timeout assertions are not valid under a 5% CPU quota.
The 11 timed-out cases were:

- `test_failed_package_build_does_not_publish_partial_artifacts`
- `test_fault_injection_binary_is_rejected_before_packaging`
- `test_matching_source_digest_passes_preflight_before_static_check`
- `test_mid_publish_rename_failure_restores_previous_release_files`
- `test_mismatched_binary_version_is_rejected_before_packaging`
- `test_native_release_build_rejects_unpinned_cargo_auditable`
- `test_same_revision_binary_with_stale_source_digest_is_rejected`
- `test_same_version_binary_from_another_revision_is_rejected`
- `test_static_binary_without_embedded_dependency_metadata_is_rejected`
- `test_native_release_path_publishes_build_id_bound_symbols`
- `test_cpu_quota_out_of_range_is_rejected_before_runtime`

The host admission guard still blocks VM qualification while any detected
build/test process is active; at 22:05 UTC only two Go test processes remained.
No VM was started in that sample.
