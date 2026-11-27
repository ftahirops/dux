# Strict Cargo dependency audit — 2026-11-10 10:07 UTC

Ran both `cargo audit` and `cargo audit -D warnings` against the current
`Cargo.lock`; both exited 0. A machine-readable report from the strict run
records **229 dependencies**, **0 known vulnerabilities**, and an empty warning
set. The RustSec advisory database contained 1,430 advisories at commit
`d9cb059f3217db87aeccd190b81eaffe8cc29cb9`, last updated
`2026-11-09T17:13:08+01:00`.

This is evidence only for known advisories in the locked Cargo dependencies
against that database snapshot. It does not cover application-level security,
runtime behavior, non-Cargo components, or advisories published after the
database revision. The exact JSON output is `audit.json` (SHA-256
`4a99ec5910224d931220d181948a5abdbb7ff605d621ef23a8cb2a7ca96fd24b`).
