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

## Refreshed local scans — 2026-11-09 09:48 UTC

- A fresh `cargo audit --json` again found **0 advisories across 229
  dependencies**. Its advisory database was updated at
  `2026-11-09T10:22:53+01:00` (09:22 UTC).
- Gitleaks `dir` scans found no leaks in `src/`, `scripts/`, or `packaging/`.
  A scan of `docs/` returned three `generic-api-key` matches on Rocky Linux
  X.509 certificate signer labels in a console log; inspection confirmed the
  matched text was certificate-name metadata, not credentials. The preserved
  docs finding report redacts all matched strings.
- `.github/` does not exist in this checkout, so there are no workflow files
  to scan here.
- Semgrep `p/rust` could not fetch its ruleset because TLS verification of
  `semgrep.dev` failed with `SSLCertVerificationError`. I kept certificate
  verification enabled; the run produced no findings report and remains
  **inconclusive**, not a pass.

Refresh evidence is in
[`live/security-current-20261109/`](live/security-current-20261109/): the
current Cargo audit JSON and separate Gitleaks JSON reports for source,
scripts, packaging, and the redacted documentation false positives. The empty
Gitleaks result files are identical because each scan produced no findings.

## Evidence

- [`cargo-audit-20261109.json`](live/cargo-audit-20261109.json),
  SHA256 `bd13b9d963fd12acba59b6ad1a007225bb5311bc2758a8f0cc10e4dcba74a89d`
- [`trivy-vuln-misconfig-20261109.json`](live/trivy-vuln-misconfig-20261109.json),
  SHA256 `c92ac4a1d14e738ac08690b63ac38298e2c5ba85b74f757cbb4ba09c906ad76e`

These are scoped static scans. They do not establish absence of application
security defects, secret exposure in unscanned paths/history, secure runtime
configuration, or a complete release security review.
