import pathlib
import os
import re
import subprocess
import tempfile
import unittest

import dux_source_archive

REPO = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = REPO / "scripts" / "build-release.sh"


def add_fake_readelf(fake_tools, contains_audit_data=True):
    readelf = fake_tools / "readelf"
    if contains_audit_data:
        sections = "  [ 1] .dep-v0 PROGBITS 0000000000000000 000040 000100 00 A 0 0 4\\n"
    else:
        sections = "  [ 1] .text PROGBITS 0000000000000000 000040 000100 00 AX 0 0 16\\n"
    readelf.write_text("#!/bin/sh\nprintf '%s' '" + sections + "'\n")
    readelf.chmod(0o755)


def add_fake_cargo_audit(fake_tools):
    audit = fake_tools / "cargo-audit"
    audit.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, sys\n"
        "args = sys.argv[1:]\n"
        "assert args and args[0] == 'audit', args\n"
        "args = args[1:]\n"
        "if args == ['--version']:\n"
        " print('cargo-audit-audit 0.22.2'); raise SystemExit(0)\n"
        "if args and args[0] == 'bin':\n"
        " print(os.environ.get('FAKE_CARGO_AUDIT_BINARY_OUTPUT', 'binary inventory complete'))\n"
        " raise SystemExit(int(os.environ.get('FAKE_CARGO_AUDIT_BINARY_EXIT', '0')))\n"
        "if args[:2] == ['--deny', 'warnings']:\n"
        " vulnerable = os.environ.get('FAKE_CARGO_AUDIT_LOCK_VULNERABLE') == '1'\n"
        " report = {'database': {'advisory-count': 1403, 'last-commit': 'test-db-commit', 'last-updated': '2026-11-03T20:34:00Z'}, 'lockfile': {'dependency-count': 229}, 'vulnerabilities': {'found': vulnerable, 'count': int(vulnerable), 'list': [{'advisory': {'id': 'RUSTSEC-TEST-0001'}}] if vulnerable else []}, 'warnings': {}}\n"
        " print(json.dumps(report))\n"
        " raise SystemExit(0)\n"
        "raise SystemExit('unexpected cargo-audit invocation: ' + repr(args))\n"
    )
    audit.chmod(0o755)
    cargo = fake_tools / "cargo"
    cargo.write_text(
        "#!/usr/bin/env python3\n"
        "import json, subprocess, sys\n"
        "args = sys.argv[1:]\n"
        "if args and args[0] == 'audit':\n"
        " result = subprocess.run(['cargo-audit', 'audit', *args[1:]], check=False)\n"
        " raise SystemExit(result.returncode)\n"
        "if args and args[0] == 'metadata':\n"
        " print(json.dumps({'packages': []})); raise SystemExit(0)\n"
        "raise SystemExit('unexpected cargo invocation: ' + repr(args))\n"
    )
    cargo.chmod(0o755)


def add_fake_release_binary(root):
    version = re.search(
        r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
    ).group(1)
    revision = subprocess.check_output(
        ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
    ).strip()
    dirty = dux_source_archive.source_tree_dirty(REPO)
    _, source_digest = dux_source_archive.collect_source_entries(REPO)
    modified = "+modified" if dirty else ""
    binary = root / "dux-current-source"
    binary.write_text(
        "#!/bin/sh\n"
        "if [ \"${1:-}\" = --version ]; then\n"
        f"  echo 'dux {version} ({revision}{modified}) [production] source-sha256={source_digest}'\n"
        "fi\n"
    )
    binary.chmod(0o755)
    return binary


class BuildReleaseCliTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            cwd=REPO,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )

    def test_help_exits_before_building(self):
        result = self.run_cli("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage: scripts/build-release.sh [OUTPUT_DIRECTORY]", result.stdout)
        self.assertNotIn("building static musl binary", result.stdout)

    def test_unknown_option_is_rejected_before_building(self):
        result = self.run_cli("--not-an-output-directory")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unknown option", result.stderr)
        self.assertNotIn("building static musl binary", result.stdout)

    def test_multiple_output_directories_are_rejected(self):
        result = self.run_cli("one", "two")
        self.assertEqual(result.returncode, 2)
        self.assertIn("at most one output directory", result.stderr)
        self.assertNotIn("building static musl binary", result.stdout)

    def test_native_release_build_rejects_unpinned_cargo_auditable(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-auditable-version-") as temp:
            root = pathlib.Path(temp)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            tool = fake_tools / "cargo-auditable"
            tool.write_text("#!/bin/sh\nexit 0\n")
            tool.chmod(0o755)
            cargo = fake_tools / "cargo"
            cargo.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = install ] && [ \"${2:-}\" = --list ]; then\n"
                "  printf '%s\\n' 'cargo-auditable v0.7.6:' '    cargo-auditable'\n"
                "  exit 0\n"
                "fi\n"
                "exit 99\n"
            )
            cargo.chmod(0o755)
            output = root / "out"
            env = os.environ.copy()
            env.pop("DUX_RELEASE_BINARY", None)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("expected cargo-auditable 0.7.7", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_mismatched_binary_version_is_rejected_before_packaging(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-version-") as temp:
            root = pathlib.Path(temp)
            binary = root / "dux-wrong-version"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                "  echo 'dux 99.99.99 (wrong-source) [production]'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n")
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("binary version does not match source package version", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_fault_injection_binary_is_rejected_before_packaging(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-fault-build-") as temp:
            root = pathlib.Path(temp)
            version = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            ).group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            dirty = bool(
                subprocess.check_output(
                    ["git", "status", "--porcelain"], cwd=REPO, text=True
                ).strip()
            )
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            modified = "+modified" if dirty else ""
            binary = root / "dux-fault-injection"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}{modified}) [fault-injection] source-sha256={source_digest}'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("refusing a non-production binary", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_same_version_binary_from_another_revision_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-revision-") as temp:
            root = pathlib.Path(temp)
            version_match = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            )
            self.assertIsNotNone(version_match)
            version = version_match.group(1)
            current_revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            wrong_revision = "0" * 12
            if wrong_revision == current_revision:
                wrong_revision = "f" * 12
            binary = root / "dux-wrong-revision"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({wrong_revision}) [production]'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n")
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("binary revision does not match packaging checkout", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_same_revision_binary_with_stale_source_digest_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-source-") as temp:
            root = pathlib.Path(temp)
            version_match = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            )
            self.assertIsNotNone(version_match)
            version = version_match.group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            binary = root / "dux-stale-source"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}+modified) [production] source-sha256={'f' * 64}'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n")
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("binary source digest does not match packaging checkout", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_matching_source_digest_passes_preflight_before_static_check(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-current-source-") as temp:
            root = pathlib.Path(temp)
            version_match = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            )
            self.assertIsNotNone(version_match)
            version = version_match.group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            dirty = bool(
                subprocess.check_output(
                    ["git", "status", "--porcelain"], cwd=REPO, text=True
                ).strip()
            )
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            modified = "+modified" if dirty else ""
            binary = root / "dux-current-source"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}{modified}) [production] source-sha256={source_digest}'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, dynamically linked'\n")
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("binary is not static", result.stdout)
            self.assertNotIn("source digest does not match", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_static_binary_without_embedded_dependency_metadata_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-metadata-") as temp:
            root = pathlib.Path(temp)
            version = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            ).group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            dirty = bool(
                subprocess.check_output(
                    ["git", "status", "--porcelain"], cwd=REPO, text=True
                ).strip()
            )
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            modified = "+modified" if dirty else ""
            binary = root / "dux-current-source"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}{modified}) [production] source-sha256={source_digest}'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text(
                "#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n"
            )
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools, contains_audit_data=False)
            output = root / "out"
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("missing embedded Rust dependency metadata", result.stderr)
            self.assertEqual(list(output.iterdir()), [])

    def test_vulnerable_or_incomplete_dependency_audits_reject_release(self):
        for mode in ("lockfile-vulnerability", "incomplete-binary"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(
                prefix="dux-release-dependency-audit-"
            ) as temp:
                root = pathlib.Path(temp)
                binary = add_fake_release_binary(root)
                fake_tools = root / "fake-tools"
                fake_tools.mkdir()
                (fake_tools / "file").write_text(
                    "#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n"
                )
                add_fake_readelf(fake_tools)
                add_fake_cargo_audit(fake_tools)
                for tool in fake_tools.iterdir():
                    tool.chmod(0o755)
                output = root / "out"
                env = os.environ.copy()
                env["DUX_RELEASE_BINARY"] = str(binary)
                env["PATH"] = f"{fake_tools}:{env['PATH']}"
                if mode == "lockfile-vulnerability":
                    env["FAKE_CARGO_AUDIT_LOCK_VULNERABLE"] = "1"
                    expected = "Cargo.lock audit report contains RustSec vulnerabilities"
                else:
                    env["FAKE_CARGO_AUDIT_BINARY_OUTPUT"] = (
                        "warning: report will be incomplete (43 dependencies recovered)"
                    )
                    expected = "binary dependency audit emitted a warning or was incomplete"
                result = subprocess.run(
                    ["bash", str(SCRIPT), str(output)],
                    cwd=REPO,
                    env=env,
                    capture_output=True,
                    text=True,
                    # This negative-path CLI check runs under low-CPU scopes in
                    # production qualification; five wall-clock seconds made
                    # a correct early rejection time out during host contention.
                    timeout=30,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(
                    expected,
                    result.stderr,
                    f"returncode={result.returncode} stdout={result.stdout!r} stderr={result.stderr!r}",
                )
                self.assertFalse(output.exists() and list(output.iterdir()))

    def test_failed_package_build_does_not_publish_partial_artifacts(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-staging-") as temp:
            root = pathlib.Path(temp)
            version_match = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            )
            self.assertIsNotNone(version_match)
            version = version_match.group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            dirty = bool(
                subprocess.check_output(
                    ["git", "status", "--porcelain"], cwd=REPO, text=True
                ).strip()
            )
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            modified = "+modified" if dirty else ""
            binary = root / "dux-current-source"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}{modified}) [production] source-sha256={source_digest}'\n"
                "fi\n"
            )
            binary.chmod(0o755)
            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            file_command = fake_tools / "file"
            file_command.write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n")
            file_command.chmod(0o755)
            add_fake_readelf(fake_tools)
            add_fake_cargo_audit(fake_tools)
            dpkg_command = fake_tools / "dpkg-deb"
            dpkg_command.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --build ]; then : > \"$4\"; exit 0; fi\n"
                "exit 42\n"
            )
            dpkg_command.chmod(0o755)
            output = root / "out"
            output.mkdir()
            prior = output / f"dux_{version}-17_amd64.deb"
            prior.write_bytes(b"previous verified artifact")
            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(list(output.iterdir()), [prior])
            self.assertEqual(prior.read_bytes(), b"previous verified artifact")

    def test_mid_publish_rename_failure_restores_previous_release_files(self):
        with tempfile.TemporaryDirectory(prefix="dux-release-publish-rollback-") as temp:
            root = pathlib.Path(temp)
            version_match = re.search(
                r'(?m)^version\s*=\s*"([^"]+)"', (REPO / "Cargo.toml").read_text()
            )
            self.assertIsNotNone(version_match)
            version = version_match.group(1)
            revision = subprocess.check_output(
                ["git", "rev-parse", "--short=12", "HEAD"], cwd=REPO, text=True
            ).strip()
            dirty = bool(
                subprocess.check_output(
                    ["git", "status", "--porcelain"], cwd=REPO, text=True
                ).strip()
            )
            _, source_digest = dux_source_archive.collect_source_entries(REPO)
            modified = "+modified" if dirty else ""
            binary = root / "dux-current-source"
            binary.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = --version ]; then\n"
                f"  echo 'dux {version} ({revision}{modified}) [production] source-sha256={source_digest}'\n"
                "fi\n"
            )
            binary.chmod(0o755)

            fake_tools = root / "fake-tools"
            fake_tools.mkdir()
            (fake_tools / "file").write_text(
                "#!/bin/sh\necho 'ELF 64-bit LSB executable, static-pie linked'\n"
            )
            add_fake_readelf(fake_tools)
            add_fake_cargo_audit(fake_tools)
            (fake_tools / "dpkg-deb").write_text(
                "#!/bin/bash\n"
                "if [[ $1 == --build ]]; then cp \"$3/usr/lib/dux/dux-backup-index.py\" \"$FAKE_DEB_HELPER\"; : > \"${@: -1}\"; exit 0; fi\n"
                "mkdir -p \"$3/usr/bin\" \"$3/usr/lib/dux\"\n"
                "cp \"$DUX_RELEASE_BINARY\" \"$3/usr/bin/dux\"\n"
                "cp \"$FAKE_DEB_HELPER\" \"$3/usr/lib/dux/dux-backup-index.py\"\n"
            )
            (fake_tools / "rpmbuild").write_text(
                "#!/bin/bash\n"
                "for arg in \"$@\"; do [[ $arg == _topdir\\ * ]] && TOP=${arg#_topdir }; done\n"
                "mkdir -p \"$TOP/RPMS/x86_64\"\n"
                "cp \"$TOP/SOURCES/dux-backup-index.py\" \"$FAKE_RPM_HELPER\"\n"
                f": > \"$TOP/RPMS/x86_64/dux-{version}-17.x86_64.rpm\"\n"
            )
            (fake_tools / "rpm2cpio").write_text("#!/bin/sh\ncat /dev/null\n")
            (fake_tools / "cpio").write_text(
                "#!/bin/bash\nmkdir -p ./usr/bin ./usr/lib/dux\n"
                "cp \"$DUX_RELEASE_BINARY\" ./usr/bin/dux\n"
                "cp \"$FAKE_RPM_HELPER\" ./usr/lib/dux/dux-backup-index.py\n"
            )
            count_file = root / "mv-count"
            (fake_tools / "mv").write_text(
                "#!/bin/bash\n"
                f"count=$(cat {count_file} 2>/dev/null || echo 0)\n"
                "count=$((count + 1))\n"
                f"echo $count > {count_file}\n"
                "if [[ $count -eq 4 ]]; then echo injected-rename-failure >&2; exit 42; fi\n"
                "exec /bin/mv \"$@\"\n"
            )
            for tool in fake_tools.iterdir():
                tool.chmod(0o755)

            output = root / "out"
            output.mkdir()
            previous = {
                "SHA256SUMS": b"old checksums\n",
                "build-manifest.json": b"old manifest\n",
                "keep.txt": b"unrelated file\n",
            }
            for name, contents in previous.items():
                (output / name).write_bytes(contents)

            env = os.environ.copy()
            env["DUX_RELEASE_BINARY"] = str(binary)
            env["FAKE_DEB_HELPER"] = str(root / "deb-backup-helper")
            env["FAKE_RPM_HELPER"] = str(root / "rpm-backup-helper")
            env["PATH"] = f"{fake_tools}:{env['PATH']}"
            result = subprocess.run(
                ["bash", str(SCRIPT), str(output)],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                # This exercises several mocked package tools and publication
                # rollback subprocesses. Keep a hard hang bound, but allow
                # heavily CPU-throttled qualification scopes to complete.
                timeout=60,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(
                "injected-rename-failure",
                result.stderr,
                f"returncode={result.returncode} stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertEqual(
                {item.name: item.read_bytes() for item in output.iterdir() if item.is_file()},
                previous,
            )
            self.assertFalse(any(item.name.startswith(".dux-release-") for item in output.iterdir()))


if __name__ == "__main__":
    unittest.main()
