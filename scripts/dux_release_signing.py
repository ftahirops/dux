#!/usr/bin/env python3
"""Sign and verify Dux release artifacts with a pinned OpenPGP identity."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


FINGERPRINT_RE = re.compile(r"(?:[0-9A-F]{40}|[0-9A-F]{64})\Z")
SIGNATURE_NAME = "SHA256SUMS.asc"
PUBLIC_KEY_NAME = "release-signing-key.asc"


class SigningError(RuntimeError):
    pass


def run(args: list[str], *, cwd: Path | None = None) -> str:
    result = subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=False)
    if result.returncode:
        detail = (result.stderr or result.stdout).strip()
        raise SigningError(f"command failed ({result.returncode}): {args[0]}: {detail}")
    return result.stdout + result.stderr


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def checked_fingerprint(value: str) -> str:
    value = value.upper()
    if not FINGERPRINT_RE.fullmatch(value):
        raise SigningError("signing key must be a full 40- or 64-character fingerprint")
    return value


def checksum_names(path: Path) -> list[str]:
    names: list[str] = []
    seen: set[str] = set()
    for number, line in enumerate(path.read_text().splitlines(), start=1):
        match = re.fullmatch(r"[0-9a-f]{64}  ([^/\\\x00]+)", line)
        if not match:
            raise SigningError(f"invalid SHA256SUMS entry on line {number}")
        name = match.group(1)
        if name in {".", ".."} or name in seen:
            raise SigningError(f"unsafe or duplicate SHA256SUMS filename: {name!r}")
        seen.add(name)
        names.append(name)
    if not names:
        raise SigningError("SHA256SUMS is empty")
    return names


def validate_checksums(directory: Path, names: list[str]) -> None:
    for name in names:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise SigningError(f"checksum entry is not a regular artifact file: {name}")
        line = next(
            line for line in (directory / "SHA256SUMS").read_text().splitlines()
            if line.endswith(f"  {name}")
        )
        expected = line[:64]
        actual = sha256(path)
        if actual != expected:
            raise SigningError(f"artifact hash does not match SHA256SUMS: {name}")


def write_atomic(path: Path, data: bytes) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        with temporary.open("xb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def secret_key_fingerprints(fingerprint: str) -> set[str]:
    listing = run(["gpg", "--batch", "--with-colons", "--fingerprint", "--list-secret-keys", fingerprint])
    return primary_fingerprints(listing, "sec")


def primary_fingerprints(listing: str, primary_record: str) -> set[str]:
    fingerprints: set[str] = set()
    awaiting_primary_fingerprint = False
    for line in listing.splitlines():
        fields = line.split(":")
        if fields[0] == primary_record:
            awaiting_primary_fingerprint = True
        elif fields[0] in {"sub", "ssb"}:
            awaiting_primary_fingerprint = False
        elif fields[0] == "fpr" and awaiting_primary_fingerprint and len(fields) > 9:
            fingerprints.add(fields[9].upper())
            awaiting_primary_fingerprint = False
    return fingerprints


def _load_manifest(directory: Path) -> dict:
    try:
        manifest = json.loads((directory / "build-manifest.json").read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise SigningError(f"cannot read build-manifest.json: {error}") from error
    if not isinstance(manifest.get("artifacts"), dict):
        raise SigningError("build manifest has no artifact hash map")
    return manifest


def sign_release(directory: Path, fingerprint: str) -> None:
    directory = directory.resolve(strict=True)
    fingerprint = checked_fingerprint(fingerprint)
    sums = directory / "SHA256SUMS"
    manifest_path = directory / "build-manifest.json"
    if not sums.is_file() or sums.is_symlink() or not manifest_path.is_file() or manifest_path.is_symlink():
        raise SigningError("release directory must contain regular SHA256SUMS and build-manifest.json files")
    if (directory / SIGNATURE_NAME).exists() or (directory / PUBLIC_KEY_NAME).exists():
        raise SigningError("release directory already contains signing output")

    names = checksum_names(sums)
    validate_checksums(directory, names)
    rpms = [directory / name for name in names if name.endswith(".rpm")]
    if len(rpms) != 1:
        raise SigningError(f"expected exactly one RPM in SHA256SUMS, found {len(rpms)}")
    rpm = rpms[0]
    if "build-manifest.json" not in names:
        raise SigningError("SHA256SUMS must authenticate build-manifest.json")
    manifest = _load_manifest(directory)
    if manifest["artifacts"].get(rpm.name) != sha256(rpm):
        raise SigningError("build manifest RPM hash does not match the unsigned package")
    if fingerprint not in secret_key_fingerprints(fingerprint):
        raise SigningError("the requested signing fingerprint is not present in the GPG secret-key store")

    gpg_executable = shutil.which("gpg")
    if not gpg_executable:
        raise SigningError("gpg is required to sign release artifacts")
    run([
        "rpmsign", "--define", f"__gpg {gpg_executable}",
        "--define", f"_gpg_name {fingerprint}", "--addsign", str(rpm),
    ])
    signed_hash = sha256(rpm)
    manifest["artifacts"][rpm.name] = signed_hash
    manifest["artifact_signing"] = {
        "rpm_signature": "OpenPGP via rpmsign",
        "checksums_signature": SIGNATURE_NAME,
        "signing_key_fingerprint": fingerprint,
        "signed_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
    }
    write_atomic(manifest_path, (json.dumps(manifest, indent=2) + "\n").encode())

    lines = [f"{sha256(directory / name)}  {name}\n" for name in names]
    write_atomic(sums, "".join(lines).encode())
    public_key = directory / PUBLIC_KEY_NAME
    temporary_key = directory / f".{PUBLIC_KEY_NAME}.{os.getpid()}.tmp"
    temporary_signature = directory / f".{SIGNATURE_NAME}.{os.getpid()}.tmp"
    try:
        run([
            "gpg", "--batch", "--yes", "--armor", "--output",
            str(temporary_key), "--export", fingerprint,
        ])
        if not temporary_key.is_file() or temporary_key.stat().st_size == 0:
            raise SigningError("GPG did not export the requested public key")
        os.replace(temporary_key, public_key)
        run([
            "gpg", "--batch", "--yes", "--local-user", fingerprint,
            "--armor", "--detach-sign", "--output", str(temporary_signature), str(sums),
        ])
        if not temporary_signature.is_file() or temporary_signature.stat().st_size == 0:
            raise SigningError("GPG did not create a checksum signature")
        os.replace(temporary_signature, directory / SIGNATURE_NAME)
    finally:
        temporary_key.unlink(missing_ok=True)
        temporary_signature.unlink(missing_ok=True)

    verify_release(directory, fingerprint)


def _public_key_fingerprints(path: Path) -> set[str]:
    listing = run(["gpg", "--batch", "--with-colons", "--show-keys", "--fingerprint", str(path)])
    return primary_fingerprints(listing, "pub")


def verify_release(directory: Path, fingerprint: str) -> None:
    directory = directory.resolve(strict=True)
    fingerprint = checked_fingerprint(fingerprint)
    sums = directory / "SHA256SUMS"
    signature = directory / SIGNATURE_NAME
    public_key = directory / PUBLIC_KEY_NAME
    if not all(path.is_file() and not path.is_symlink() for path in (sums, signature, public_key)):
        raise SigningError("signed release must include SHA256SUMS, SHA256SUMS.asc, and release-signing-key.asc")
    if fingerprint not in _public_key_fingerprints(public_key):
        raise SigningError("public signing key fingerprint does not match the pinned fingerprint")

    names = checksum_names(sums)
    validate_checksums(directory, names)
    manifest = _load_manifest(directory)
    if "build-manifest.json" not in names:
        raise SigningError("SHA256SUMS does not authenticate build-manifest.json")
    signing = manifest.get("artifact_signing", {})
    if signing.get("signing_key_fingerprint") != fingerprint or signing.get("checksums_signature") != SIGNATURE_NAME:
        raise SigningError("build manifest does not declare the pinned signing identity")
    rpms = [directory / name for name in names if name.endswith(".rpm")]
    if len(rpms) != 1 or manifest["artifacts"].get(rpms[0].name) != sha256(rpms[0]):
        raise SigningError("signed RPM does not match the build manifest")

    with tempfile.TemporaryDirectory(prefix="dux-rpm-signature-verify-") as temporary:
        keyring = Path(temporary) / "release-signing-key.gpg"
        run(["gpg", "--batch", "--dearmor", "--output", str(keyring), str(public_key)])
        status = run([
            "gpgv", "--status-fd=1", "--keyring", str(keyring),
            str(signature), str(sums),
        ])
        valid_fingerprints = set()
        for line in status.splitlines():
            fields = line.split()
            if len(fields) >= 3 and fields[:2] == ["[GNUPG:]", "VALIDSIG"]:
                valid_fingerprints.update(
                    field.upper()
                    for field in fields[2:]
                    if FINGERPRINT_RE.fullmatch(field.upper())
                )
        if fingerprint not in valid_fingerprints:
            raise SigningError("checksum signature is invalid or was not made by the pinned fingerprint")
        database = Path(temporary) / "rpmdb"
        run(["rpm", "--dbpath", str(database), "--initdb"])
        run(["rpm", "--dbpath", str(database), "--import", str(public_key)])
        output = run(["rpm", "--dbpath", str(database), "--checksig", "--verbose", str(rpms[0])])
    if not re.search(r"(?im)^\s*Header.*Signature.*:\s*OK\s*$", output):
        raise SigningError("RPM package signature did not verify against the pinned key")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("sign", "verify"))
    parser.add_argument("directory", type=Path)
    parser.add_argument("--fingerprint", required=True, help="full trusted OpenPGP primary-key fingerprint")
    options = parser.parse_args()
    try:
        if options.action == "sign":
            sign_release(options.directory, options.fingerprint)
        else:
            verify_release(options.directory, options.fingerprint)
    except (OSError, SigningError, subprocess.SubprocessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(f"PASS: {options.action} verified for {options.fingerprint.upper()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
