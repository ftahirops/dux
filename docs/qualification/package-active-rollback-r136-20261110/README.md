# Active package rollback fixture — 2026-11-10

This qualification fixture pairs the current package scripts and verified
backup/restore helper with the exact r132 daemon binary. It is for disposable
Ubuntu/Rocky lifecycle testing only. The packages are unsigned and are not
release artifacts. No target VM lifecycle transaction has completed with these
artifacts yet.

The hashes and package ownership observations are recorded in
[`manifest.json`](manifest.json). The new RPM was queried with the local RPM
4.18.2 tooling: its registered `%pre` and `%posttrans` hooks are present, and
there is no unsupported `%preuntrans` hook. RPM rollback must therefore stop
the active service explicitly before downgrade. Debian rollback stops the
service through `prerm`. Both flows restore an index backup only while the
service is inactive; see the [operator procedure](../../operations/package-rollback.md).

The new package carries the same r132 binary as the old baseline, so this
fixture tests package transaction/rollback safety, not a new daemon code
revision. Rebuild the fixture if package sources or the helper change. The
package lifecycle harness covers control flow but does not replace running the
packages inside their target distributions.
