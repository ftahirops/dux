# Security scan checkpoint — 2026-11-09

## Results

- `cargo audit --json`: **0 advisories** in the 229 dependencies in the
  workspace lockfile. Advisory database last updated at
  `2026-11-08T16:03:42-07:00`.
- Trivy filesystem scan (`vuln,misconfig`, excluding build output): **0
  vulnerabilities** in `Cargo.lock` and `vendor/jwalk/Cargo.lock`. Trivy
  reported **0 configuration files detected**, so this run does not qualify
  deployment configuration security.
- Gitleaks scanned `src/` (about 1.36 MB) and `scripts/` (about 2.01 MB)
  separately with `--no-git`: **no leaks found** in either scanned tree.
- A broader Gitleaks `--no-git` scan of the entire workspace was stopped
  after it spent sustained CPU scanning generated/build material. It produced
  no report and is not counted as a pass. Git history, documentation, build
  output, and every other workspace path were not secret-scanned.
- Semgrep's remote `p/security-audit` ruleset invocation did not produce a
  report; it is inconclusive and is not counted as a pass.

## Evidence

- [`cargo-audit-20261109.json`](live/cargo-audit-20261109.json),
  SHA256 `bd13b9d963fd12acba59b6ad1a007225bb5311bc2758a8f0cc10e4dcba74a89d`
- [`trivy-vuln-misconfig-20261109.json`](live/trivy-vuln-misconfig-20261109.json),
  SHA256 `c92ac4a1d14e738ac08690b63ac38298e2c5ba85b74f757cbb4ba09c906ad76e`

These are scoped static scans. They do not establish absence of application
security defects, secret exposure in unscanned paths/history, secure runtime
configuration, or a complete release security review.
