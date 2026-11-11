# Shared ext4 smoke evidence

`shared-ext4-final.json` is preserved byte-for-byte because historical build manifests reference it. Despite its `.json` suffix, it is a concatenated command transcript (systemd properties, Dux JSON status, then SQLite/schema/entry summaries), so JSON parsers cannot consume it.

Use [`shared-ext4-final.normalized.json`](shared-ext4-final.normalized.json) for machine-readable fields. The sidecar records the SHA-256 of the original capture and its provenance. The original capture remains the primary raw evidence.
