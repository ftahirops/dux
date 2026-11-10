"""Exercise release debug-symbol wiring with a fake Cargo build and real ELF tools."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import dux_source_archive


REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / "scripts" / "build-release.sh"


@unittest.skipUnless(
    all(
        shutil.which(tool)
        for tool in ("cc", "file", "objcopy", "strip", "readelf", "sha256sum")
    ),
    "requires a C compiler and binutils",
)
class ReleaseDebugArtifactTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._saved_signing_env = {
            name: os.environ.get(name)
            for name in ("DUX_ALLOW_UNSIGNED_RELEASE", "DUX_RELEASE_SIGNING_KEY")
        }
        os.environ.pop("DUX_RELEASE_SIGNING_KEY", None)
        os.environ["DUX_ALLOW_UNSIGNED_RELEASE"] = "1"

    @classmethod
    def tearDownClass(cls):
        for name, value in cls._saved_signing_env.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def test_native_release_path_publishes_build_id_bound_symbols(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-debug-artifact-") as temp:
            root = Path(temp)
            tools = root / "tools"
            tools.mkdir()
            version = next(
                line.split('"')[1]
                for line in (REPO / "Cargo.toml").read_text().splitlines()
                if line.startswith("version = ")
            )
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            modified = dux_source_archive.source_tree_dirty(REPO)
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            suffix = "+modified" if modified else ""
            version_output = (
                f"dux {version} ({revision}{suffix}) [production] "
                f"source-sha256={source_digest}"
            )
            c_source = root / "probe.c"
            c_source.write_text(
                "#include <stdio.h>\n"
                "#include <string.h>\n"
                "__attribute__((section(\".dep-v0\"), used)) "
                "const char dependency_data[] = \"auditable fixture\";\n"
                f"static const char version[] = {json.dumps(version_output)};\n"
                "int main(int argc, char **argv) {\n"
                "  if (argc == 2 && strcmp(argv[1], \"--version\") == 0) {\n"
                "    puts(version); return 0;\n"
                "  }\n"
                "  puts(\"ok\"); return 0;\n"
                "}\n"
            )

            cargo = tools / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, pathlib, subprocess, sys\n"
                "args = sys.argv[1:]\n"
                "if args and args[0] == 'audit':\n"
                " subprocess.run(['cargo-audit', *args], check=True); raise SystemExit(0)\n"
                "if args[:2] == ['install', '--list']:\n"
                " print('cargo-auditable v0.7.7:\\n    cargo-auditable'); raise SystemExit(0)\n"
                "if args and args[0] == 'metadata':\n"
                " print(json.dumps({'packages': []})); raise SystemExit(0)\n"
                "if args[:2] == ['auditable', 'build']:\n"
                " assert os.environ['CARGO_PROFILE_RELEASE_DEBUG'] == '1'\n"
                " assert os.environ['CARGO_PROFILE_RELEASE_STRIP'] == 'none'\n"
                " out = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'x86_64-unknown-linux-musl/release/dux'\n"
                " out.parent.mkdir(parents=True, exist_ok=True)\n"
                " subprocess.run(['cc', '-static', '-g', '-O2', '-Wl,--build-id', "
                "os.environ['DUX_FAKE_C_SOURCE'], '-o', str(out)], check=True)\n"
                " raise SystemExit(0)\n"
                "raise SystemExit('unexpected cargo invocation: ' + repr(args))\n"
            )
            cargo.chmod(0o755)
            cargo_auditable = tools / "cargo-auditable"
            cargo_auditable.write_text("#!/bin/sh\nexit 0\n")
            cargo_auditable.chmod(0o755)
            cargo_audit = tools / "cargo-audit"
            cargo_audit.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                "args = sys.argv[1:]\n"
                "assert args and args[0] == 'audit', args\n"
                "args = args[1:]\n"
                "if args == ['--version']:\n"
                " print('cargo-audit-audit 0.22.2'); raise SystemExit(0)\n"
                "if args and args[0] == 'bin':\n"
                " print('binary inventory complete'); raise SystemExit(0)\n"
                "if args[:2] == ['--deny', 'warnings']:\n"
                " print(json.dumps({'database': {'advisory-count': 1403, 'last-commit': 'test-db-commit', 'last-updated': '2026-11-03T20:34:00Z'}, 'lockfile': {'dependency-count': 229}, 'vulnerabilities': {'found': False, 'count': 0, 'list': []}, 'warnings': {}})); raise SystemExit(0)\n"
                "raise SystemExit('unexpected cargo-audit invocation: ' + repr(args))\n"
            )
            cargo_audit.chmod(0o755)

            (tools / "dpkg-deb").write_text(
                "#!/bin/sh\n"
                "case $1 in\n"
                " --build) cp \"$3/usr/bin/dux\" \"$FAKE_DEB_BINARY\"; cp \"$3/usr/lib/dux/dux-backup-index.py\" \"$FAKE_DEB_HELPER\"; : > \"$4\" ;;\n"
                " --extract) mkdir -p \"$3/usr/bin\" \"$3/usr/lib/dux\"; cp \"$FAKE_DEB_BINARY\" \"$3/usr/bin/dux\"; cp \"$FAKE_DEB_HELPER\" \"$3/usr/lib/dux/dux-backup-index.py\" ;;\n"
                " *) exit 42 ;;\n"
                "esac\n"
            )
            (tools / "rpmbuild").write_text(
                "#!/bin/sh\n"
                "top=${2#_topdir }\n"
                "version=$(sed -n 's/^Version:[[:space:]]*//p' \"$top/SPECS/dux.spec\")\n"
                "release=$(sed -n 's/^Release:[[:space:]]*\\([0-9][0-9]*\\).*/\\1/p' \"$top/SPECS/dux.spec\")\n"
                "mkdir -p \"$top/RPMS/x86_64\"\n"
                "cp \"$top/SOURCES/dux\" \"$FAKE_RPM_BINARY\"\n"
                "cp \"$top/SOURCES/dux-backup-index.py\" \"$FAKE_RPM_HELPER\"\n"
                ": > \"$top/RPMS/x86_64/dux-$version-$release.x86_64.rpm\"\n"
            )
            (tools / "rpm2cpio").write_text("#!/bin/sh\nexit 0\n")
            (tools / "cpio").write_text(
                "#!/bin/sh\nmkdir -p ./usr/bin ./usr/lib/dux\n"
                "cp \"$FAKE_RPM_BINARY\" ./usr/bin/dux\n"
                "cp \"$FAKE_RPM_HELPER\" ./usr/lib/dux/dux-backup-index.py\n"
            )
            for tool in tools.iterdir():
                tool.chmod(0o755)

            output = root / "release"
            env = os.environ.copy()
            env.update(
                {
                    "PATH": f"{tools}:{env['PATH']}",
                    "CARGO_HOME": str(root / "cargo-home"),
                    "CARGO_TARGET_DIR": str(root / "cargo-target"),
                    "DUX_FAKE_C_SOURCE": str(c_source),
                    "FAKE_DEB_BINARY": str(root / "deb-payload"),
                    "FAKE_RPM_BINARY": str(root / "rpm-payload"),
                    "FAKE_DEB_HELPER": str(root / "deb-helper-payload"),
                    "FAKE_RPM_HELPER": str(root / "rpm-helper-payload"),
                    "DUX_PACKAGE_RELEASE": "93",
                }
            )
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

            runtime = output / f"dux-{version}-x86_64-linux-static"
            debug = output / "dux.debug"
            manifest = json.loads((output / "build-manifest.json").read_text())
            self.assertTrue(runtime.is_file())
            self.assertTrue(debug.is_file())
            self.assertEqual(
                hashlib.sha256(runtime.read_bytes()).hexdigest(),
                hashlib.sha256(Path(env["FAKE_DEB_BINARY"]).read_bytes()).hexdigest(),
            )
            self.assertEqual(
                hashlib.sha256(runtime.read_bytes()).hexdigest(),
                hashlib.sha256(Path(env["FAKE_RPM_BINARY"]).read_bytes()).hexdigest(),
            )
            source_helper = REPO / "scripts/dux-backup-index.py"
            for helper_payload, manifest_key in (
                (Path(env["FAKE_DEB_HELPER"]), "deb_payload_backup_helper_sha256"),
                (Path(env["FAKE_RPM_HELPER"]), "rpm_payload_backup_helper_sha256"),
            ):
                self.assertEqual(helper_payload.read_bytes(), source_helper.read_bytes())
                self.assertEqual(
                    manifest[manifest_key],
                    hashlib.sha256(source_helper.read_bytes()).hexdigest(),
                )
            self.assertEqual(manifest["debug_symbols"]["path"], "dux.debug")
            self.assertEqual(
                manifest["debug_symbols"]["sha256"],
                hashlib.sha256(debug.read_bytes()).hexdigest(),
            )
            self.assertEqual(
                manifest["debug_symbols"]["build_id"],
                subprocess.check_output(
                    ["readelf", "-n", str(runtime)], text=True
                ).split("Build ID: ", 1)[1].splitlines()[0],
            )
            self.assertIn("dux.debug", (output / "SHA256SUMS").read_text())
            self.assertEqual(manifest["cargo_audit"], "cargo-audit-audit 0.22.2")
            self.assertEqual(
                manifest["dependency_audit"]["advisory_database"]["last-commit"],
                "test-db-commit",
            )
            self.assertEqual(
                manifest["dependency_audit"]["lockfile_vulnerability_count"], 0
            )
            lock_report = output / "cargo-audit-lockfile.json"
            self.assertEqual(
                manifest["dependency_audit"]["lockfile_report"]["sha256"],
                hashlib.sha256(lock_report.read_bytes()).hexdigest(),
            )
            checksums = (output / "SHA256SUMS").read_text()
            self.assertIn("cargo-audit-lockfile.json", checksums)
            self.assertIn("cargo-audit-binary.log", checksums)
            subprocess.run(
                ["sha256sum", "--check", "SHA256SUMS"],
                cwd=output,
                check=True,
                capture_output=True,
                text=True,
            )


if __name__ == "__main__":
    unittest.main()
