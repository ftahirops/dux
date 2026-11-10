#!/usr/bin/env python3
"""Tests for pinned release signing and fail-closed release build policy."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import dux_release_signing as signing


REPO = Path(__file__).resolve().parents[1]
FINGERPRINT = "A" * 40


def write_checksums(directory, names):
    (directory / "SHA256SUMS").write_text(
        "".join(
            f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n"
            for name in names
        )
    )


class ChecksumValidationTests(unittest.TestCase):
    def test_accepts_full_openpgp_fingerprints_and_normalizes_case(self):
        self.assertEqual(signing.checked_fingerprint("a" * 40), FINGERPRINT)
        self.assertEqual(signing.checked_fingerprint("b" * 64), "B" * 64)

    def test_rejects_short_or_nonhex_fingerprints(self):
        for value in ("A" * 16, "G" * 40, "A" * 41, ""):
            with self.subTest(value=value):
                with self.assertRaises(signing.SigningError):
                    signing.checked_fingerprint(value)

    def test_checksum_parser_rejects_path_traversal_and_duplicate_names(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-checksums-") as temp:
            path = Path(temp) / "SHA256SUMS"
            path.write_text("0" * 64 + "  ../outside\n")
            with self.assertRaises(signing.SigningError):
                signing.checksum_names(path)
            path.write_text("0" * 64 + "  dux.rpm\n" + "1" * 64 + "  dux.rpm\n")
            with self.assertRaises(signing.SigningError):
                signing.checksum_names(path)

    def test_checksum_validation_rejects_tampered_artifact(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-tamper-") as temp:
            directory = Path(temp)
            artifact = directory / "dux.rpm"
            artifact.write_bytes(b"original")
            sums = directory / "SHA256SUMS"
            sums.write_text(hashlib.sha256(b"original").hexdigest() + "  dux.rpm\n")
            signing.validate_checksums(directory, ["dux.rpm"])
            artifact.write_bytes(b"modified")
            with self.assertRaisesRegex(signing.SigningError, "does not match"):
                signing.validate_checksums(directory, ["dux.rpm"])

    def test_signing_preflight_rejects_missing_manifest_entries_before_key_use(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-preflight-") as temp:
            directory = Path(temp)
            (directory / "dux.rpm").write_bytes(b"rpm")
            (directory / "build-manifest.json").write_text('{"artifacts": {}}\n')
            (directory / "SHA256SUMS").write_text(
                hashlib.sha256(b"rpm").hexdigest() + "  dux.rpm\n"
            )
            with mock.patch.object(signing, "secret_key_fingerprints") as key_lookup:
                with self.assertRaisesRegex(signing.SigningError, "must authenticate"):
                    signing.sign_release(directory, FINGERPRINT)
                key_lookup.assert_not_called()


class SigningFlowTests(unittest.TestCase):
    def test_sign_updates_manifest_checksums_and_verifies_pinned_rpm_signature(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-flow-") as temp:
            directory = Path(temp)
            rpm = directory / "dux-0.6.3-132.x86_64.rpm"
            rpm.write_bytes(b"unsigned rpm fixture")
            manifest_path = directory / "build-manifest.json"
            manifest_path.write_text(json.dumps({"artifacts": {rpm.name: hashlib.sha256(rpm.read_bytes()).hexdigest()}}))
            write_checksums(directory, [rpm.name, manifest_path.name])

            def fake_run(args, *, cwd=None):
                if args[0] == "rpmsign":
                    with rpm.open("ab") as stream:
                        stream.write(b"signed")
                    return ""
                if args[0] == "gpg" and "--list-secret-keys" in args:
                    return f"sec:u:::::::\nfpr:::::::::{FINGERPRINT}\n"
                if args[0] == "gpg" and "--show-keys" in args:
                    return f"pub:u:::::::\nfpr:::::::::{FINGERPRINT}\n"
                if args[0] == "gpg" and "--export" in args:
                    output = Path(args[args.index("--output") + 1])
                    output.write_bytes(b"-----BEGIN PGP PUBLIC KEY BLOCK-----\\nfixture\\n")
                    return ""
                if args[0] == "gpg" and "--dearmor" in args:
                    output = Path(args[args.index("--output") + 1])
                    output.write_bytes(b"test binary public key")
                    return ""
                if args[0] == "gpg" and "--detach-sign" in args:
                    output = Path(args[args.index("--output") + 1])
                    output.write_bytes(b"test signature")
                    return ""
                if args[0] == "gpgv":
                    return f"[GNUPG:] VALIDSIG {FINGERPRINT} 20261110 0 4 0 1 10 00 {FINGERPRINT}\n"
                if args[0] == "rpm" and "--checksig" in args:
                    return "Header V4 RSA/SHA256 Signature, key ID 12345678: OK\n"
                return ""

            with mock.patch.object(signing, "run", side_effect=fake_run):
                signing.sign_release(directory, FINGERPRINT)

            manifest = json.loads(manifest_path.read_text())
            self.assertEqual(manifest["artifact_signing"]["signing_key_fingerprint"], FINGERPRINT)
            self.assertEqual(manifest["artifacts"][rpm.name], hashlib.sha256(rpm.read_bytes()).hexdigest())
            self.assertTrue((directory / signing.SIGNATURE_NAME).is_file())
            self.assertTrue((directory / signing.PUBLIC_KEY_NAME).is_file())
            signing.validate_checksums(directory, signing.checksum_names(directory / "SHA256SUMS"))

    @unittest.skipUnless(
        os.environ.get("DUX_RELEASE_SIGNING_TEST_RPM"),
        "set DUX_RELEASE_SIGNING_TEST_RPM to run real GPG/RPM signing integration",
    )
    def test_real_gpg_and_rpm_signature_and_tamper_rejection(self):
        source = Path(os.environ["DUX_RELEASE_SIGNING_TEST_RPM"]).resolve(strict=True)
        for tool in ("gpg", "gpgv", "rpmsign", "rpm"):
            if not shutil.which(tool):
                self.skipTest(f"integration requires {tool}")
        tmpfs = Path("/dev/shm")
        with tempfile.TemporaryDirectory(
            prefix="dux-rpm-sign-integration-",
            dir=tmpfs if tmpfs.is_dir() else None,
        ) as temp:
            directory = Path(temp)
            gnupg = directory / "gnupg"
            gnupg.mkdir(mode=0o700)
            env = os.environ.copy()
            env["GNUPGHOME"] = str(gnupg)
            subprocess.run(
                [
                    "gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "",
                    "--quick-generate-key", "Dux Qualification Test <dux-test@example.invalid>",
                    "rsa2048", "sign", "0",
                ],
                env=env, check=True, capture_output=True, text=True, timeout=45,
            )
            listing = subprocess.run(
                ["gpg", "--batch", "--with-colons", "--fingerprint", "--list-secret-keys"],
                env=env, check=True, capture_output=True, text=True, timeout=10,
            ).stdout
            fingerprint = next(
                line.split(":")[9].upper()
                for line in listing.splitlines()
                if line.startswith("fpr:")
            )
            rpm = directory / source.name
            shutil.copyfile(source, rpm)
            manifest = directory / "build-manifest.json"
            manifest.write_text(json.dumps({
                "artifacts": {rpm.name: hashlib.sha256(rpm.read_bytes()).hexdigest()}
            }, indent=2) + "\n")
            write_checksums(directory, [rpm.name, manifest.name])

            previous_home = os.environ.get("GNUPGHOME")
            os.environ["GNUPGHOME"] = str(gnupg)
            try:
                signing.sign_release(directory, fingerprint)
                signing.verify_release(directory, fingerprint)
                rpm.write_bytes(rpm.read_bytes() + b"tamper")
                with self.assertRaisesRegex(signing.SigningError, "does not match SHA256SUMS"):
                    signing.verify_release(directory, fingerprint)
            finally:
                if previous_home is None:
                    os.environ.pop("GNUPGHOME", None)
                else:
                    os.environ["GNUPGHOME"] = previous_home

    def test_builder_fails_closed_when_signatures_are_required_without_key(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-policy-") as temp:
            env = os.environ.copy()
            env.pop("DUX_RELEASE_SIGNING_KEY", None)
            env.pop("DUX_ALLOW_UNSIGNED_RELEASE", None)
            result = subprocess.run(
                ["bash", str(REPO / "scripts/build-release.sh"), temp],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("DUX_RELEASE_SIGNING_KEY is required", result.stderr)

    def test_builder_rejects_invalid_unsigned_override(self):
        with tempfile.TemporaryDirectory(prefix="dux-signing-policy-") as temp:
            env = os.environ.copy()
            env.pop("DUX_RELEASE_SIGNING_KEY", None)
            env["DUX_ALLOW_UNSIGNED_RELEASE"] = "yes"
            result = subprocess.run(
                ["bash", str(REPO / "scripts/build-release.sh"), temp],
                cwd=REPO,
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("DUX_ALLOW_UNSIGNED_RELEASE must be 0 or 1", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
