import hashlib
import os
import struct
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import dux_storage as storage


class StorageTests(unittest.TestCase):
    @staticmethod
    def write_qcow2(path, backing_name=None, version=2, backing_offset=None):
        header_size = storage.QCOW2_HEADER_V2_BYTES if version == 2 else storage.QCOW2_HEADER_V3_BYTES
        backing = b"" if backing_name is None else os.fsencode(backing_name)
        offset = backing_offset if backing_offset is not None else header_size
        data = bytearray(max(header_size, offset + len(backing)))
        data[:4] = b"QFI\xfb"
        struct.pack_into(">I", data, 4, version)
        if version == 3:
            struct.pack_into(">I", data, 100, storage.QCOW2_HEADER_V3_BYTES)
        if backing:
            struct.pack_into(">Q", data, 8, offset)
            struct.pack_into(">I", data, 16, len(backing))
            data[offset:offset + len(backing)] = backing
        path.write_bytes(data)

    def test_preflight_counts_space_available_to_unprivileged_writer(self):
        fs = SimpleNamespace(f_bavail=10, f_bfree=1000, f_frsize=4096,
                             f_files=10000, f_favail=4096)
        with patch.object(storage.os, "statvfs", return_value=fs):
            passed = storage.capacity(".", required_bytes=4096, reserve_bytes=9 * 4096)
            failed = storage.capacity(".", required_bytes=4097, reserve_bytes=9 * 4096)
        self.assertTrue(passed["passed"])
        self.assertEqual(failed["reasons"], ["insufficient_free_bytes"])

    def test_preflight_rejects_inode_exhaustion(self):
        fs = SimpleNamespace(f_bavail=10000000, f_frsize=4096, f_files=100, f_favail=0)
        with patch.object(storage.os, "statvfs", return_value=fs):
            with self.assertRaisesRegex(RuntimeError, "insufficient_free_inodes"):
                storage.require_capacity(".")

    def test_preflight_propagates_unavailable_filesystem(self):
        with patch.object(storage.os, "statvfs", side_effect=PermissionError("denied")):
            with self.assertRaises(PermissionError):
                storage.capacity(".")

    def test_preflight_rejects_negative_requirements(self):
        for requirements in ({"required_bytes": -1}, {"reserve_bytes": -1},
                             {"minimum_free_inodes": -1}):
            with self.subTest(requirements=requirements), self.assertRaises(ValueError):
                storage.capacity(".", **requirements)

    def test_inventory_preserves_sparse_hardlinked_and_symlinked_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "evidence").mkdir()
            sparse = root / "evidence/disk.qcow2"
            with sparse.open("wb") as stream:
                stream.truncate(16 * 1024 * 1024)
            (root / "report.json").write_bytes(b"exact evidence")
            os.link(root / "report.json", root / "report-copy.json")
            (root / "alias").symlink_to(root / "evidence", target_is_directory=True)
            (root / "outside").symlink_to("/etc/passwd")
            (root / "loop").symlink_to(root, target_is_directory=True)
            report = storage.inventory(root, hash_max_bytes=1024)
            rows = {row["path"]: row for row in report["files"]}
            self.assertTrue(report["complete"], report["errors"])
            self.assertEqual(rows["report.json"]["sha256"],
                             hashlib.sha256(b"exact evidence").hexdigest())
            self.assertLess(rows["evidence/disk.qcow2"]["allocated_bytes"],
                            rows["evidence/disk.qcow2"]["apparent_bytes"])
            self.assertEqual(rows["report.json"]["charged_bytes"] +
                             rows["report-copy.json"]["charged_bytes"],
                             (root / "report.json").stat().st_blocks * 512)
            self.assertEqual(rows["outside"]["kind"], "symlink")
            self.assertIsNone(rows["outside"]["sha256"])
            self.assertFalse(any(path.startswith("alias/") for path in rows))
            self.assertFalse(report["reclamation_authorized"])
            self.assertEqual(sum(row["allocated_bytes"] for row in report["groups"].values()),
                             report["allocated_bytes"])
            self.assertEqual((root / "report.json").read_bytes(), b"exact evidence")

    def test_cross_group_hardlink_allocation_uses_deterministic_canonical_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a-group").mkdir()
            (root / "z-group").mkdir()
            source = root / "z-group/shared-evidence.bin"
            source.write_bytes(b"evidence" * 4096)
            alias = root / "a-group/shared-evidence.bin"
            os.link(source, alias)

            report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(report["version"], 2)
            canonical = rows["a-group/shared-evidence.bin"]
            alias_row = rows["z-group/shared-evidence.bin"]
            a_directory = rows["a-group"]
            z_directory = rows["z-group"]
            self.assertGreater(canonical["allocated_bytes"], 0)
            self.assertEqual(canonical["charged_bytes"], canonical["allocated_bytes"])
            self.assertEqual(alias_row["charged_bytes"], 0)
            self.assertEqual(report["groups"]["a-group"]["allocated_bytes"],
                             canonical["allocated_bytes"] + a_directory["allocated_bytes"])
            self.assertEqual(report["groups"]["z-group"]["allocated_bytes"],
                             z_directory["allocated_bytes"])
            self.assertEqual(sum(group["allocated_bytes"]
                                 for group in report["groups"].values()),
                             report["allocated_bytes"])

    def test_inventory_records_only_safe_explicit_manifest_consumers(self):
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside:
            root = Path(directory)
            package = root / "release"
            package.mkdir()
            artifact = package / "candidate.bin"
            artifact.write_bytes(b"candidate")
            digest = hashlib.sha256(b"candidate").hexdigest()
            (package / "SHA256SUMS").write_text(
                f"{digest}  candidate.bin\n{digest}  ../outside.bin\n")
            (root / "build-manifest.json").write_text(
                '{"artifacts":{"release/candidate.bin":"' + digest + '"}}')
            (Path(outside) / "outside.bin").write_bytes(b"outside")

            report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(rows["release/candidate.bin"]["declared_consumers"],
                             ["build-manifest.json", "release/SHA256SUMS"])
            self.assertEqual(rows["release/candidate.bin"]["consumer_scope"],
                             "explicit_manifests_only")
            self.assertFalse(any("outside.bin" in row["path"] for row in rows.values()))
            self.assertFalse(report["reclamation_authorized"])

    def test_inventory_resolves_qcow2_backing_dependencies_without_opening_images(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = root / "base.img"
            base.write_bytes(b"base image placeholder")
            overlay = root / "overlay.qcow2"
            self.write_qcow2(overlay, "base.img")

            report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(rows["overlay.qcow2"]["image_format"], "qcow2")
            self.assertEqual(rows["overlay.qcow2"]["backing_file"], "base.img")
            self.assertEqual(rows["overlay.qcow2"]["backing_target"], "base.img")
            self.assertEqual(rows["overlay.qcow2"]["backing_target_state"], "present_in_inventory")
            self.assertEqual(rows["base.img"]["dependent_overlays"], ["overlay.qcow2"])
            self.assertEqual(report["qcow2_backing_graph"]["edges"], [{
                "overlay": "overlay.qcow2", "backing_file": "base.img",
                "target": "base.img", "state": "present_in_inventory"}])
            self.assertFalse(report["reclamation_authorized"])

    def test_qcow2_v3_resolves_parent_relative_backing_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = root / "vms" / "base.qcow2"
            base.parent.mkdir()
            self.write_qcow2(base, version=3)
            overlay = root / "vms" / "runs" / "overlay.qcow2"
            overlay.parent.mkdir()
            self.write_qcow2(overlay, "../base.qcow2", version=3)

            report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(rows["vms/runs/overlay.qcow2"]["backing_target"],
                             "vms/base.qcow2")
            self.assertEqual(rows["vms/runs/overlay.qcow2"]["backing_target_state"],
                             "present_in_inventory")
            self.assertEqual(rows["vms/base.qcow2"]["dependent_overlays"],
                             ["vms/runs/overlay.qcow2"])

    def test_qcow2_absolute_backing_outside_inventory_is_never_followed(self):
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside:
            root = Path(directory)
            external = Path(outside) / "base.qcow2"
            external.write_bytes(b"must not be opened")
            overlay = root / "overlay.qcow2"
            self.write_qcow2(overlay, str(external))

            report = storage.inventory(root)
            row = next(item for item in report["files"]
                       if item["path"] == "overlay.qcow2")
            self.assertEqual(row["backing_target_state"], "outside_inventory_root")
            self.assertEqual(report["qcow2_backing_graph"]["edges"][0]["state"],
                             "outside_inventory_root")
            self.assertEqual(report["entry_count"], 2)  # root and overlay only

    def test_qcow2_invalid_backing_bounds_are_reported_without_traversal_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = root / "broken.qcow2"
            self.write_qcow2(image, "missing.img", backing_offset=4096)
            image.write_bytes(image.read_bytes()[:storage.QCOW2_HEADER_V2_BYTES])
            report = storage.inventory(root)
            row = next(item for item in report["files"] if item["path"] == "broken.qcow2")
            self.assertTrue(report["complete"], report["errors"])
            self.assertEqual(row["image_format"], "qcow2")
            self.assertEqual(row["metadata_error"], "invalid_backing_file_bounds")
            self.assertEqual(report["qcow2_backing_graph"]["metadata_error_count"], 1)
            self.assertEqual(report["qcow2_backing_graph"]["edge_count"], 0)

    def test_qcow2_v3_rejects_invalid_header_length_and_header_overlap(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            invalid_length = root / "invalid-length.qcow2"
            self.write_qcow2(invalid_length, version=3)
            with invalid_length.open("r+b") as stream:
                stream.seek(100)
                stream.write((105).to_bytes(4, "big"))  # v3 length must be >=104 and 8-byte aligned
                stream.seek(111)
                stream.write(b"\0")

            overlap = root / "overlap.qcow2"
            self.write_qcow2(overlap, "base.img", version=3, backing_offset=104)
            with overlap.open("r+b") as stream:
                stream.seek(100)
                stream.write((112).to_bytes(4, "big"))

            report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(rows["invalid-length.qcow2"]["metadata_error"],
                             "invalid_v3_header_length")
            self.assertEqual(rows["overlap.qcow2"]["metadata_error"],
                             "invalid_backing_file_bounds")
            self.assertEqual(report["qcow2_backing_graph"]["edge_count"], 0)

    def test_qcow2_backing_scan_budget_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_qcow2(root / "overlay.qcow2", "base.img")
            with patch.object(storage, "MAX_TOTAL_QCOW2_METADATA_READ_BYTES", 0):
                report = storage.inventory(root)
            row = next(item for item in report["files"] if item["path"] == "overlay.qcow2")
            self.assertNotIn("image_format", row)
            self.assertEqual(report["qcow2_backing_graph"]["metadata_skipped_budget_count"], 1)
            self.assertEqual(report["qcow2_backing_graph"]["edge_count"], 0)

    def test_manifest_rewrite_with_restored_mtime_is_not_trusted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "artifact.bin"
            artifact.write_bytes(b"artifact")
            manifest = root / "SHA256SUMS"
            manifest.write_text("a" * 64 + "  artifact.bin\n")
            report = storage.inventory(root)
            captured = {row["path"]: row for row in report["files"]}
            self.assertEqual(captured["artifact.bin"]["declared_consumers"],
                             ["SHA256SUMS"])
            self.assertEqual(captured["SHA256SUMS"]["sha256"],
                             hashlib.sha256(manifest.read_bytes()).hexdigest())
            self.assertTrue(report["manifest_integrity"]["content_revalidated_before_reference_use"])

            manifest.write_text("b" * 64 + "  artifact.bin\n")
            current = manifest.stat()
            os.utime(manifest, ns=(current.st_atime_ns, captured["SHA256SUMS"]["mtime_ns"]))
            original_fstat = os.fstat
            captured_manifest = captured["SHA256SUMS"]

            def coarse_manifest_fstat(fd):
                current_stat = original_fstat(fd)
                if (current_stat.st_dev, current_stat.st_ino) == (
                        captured_manifest["device"], captured_manifest["inode"]):
                    return SimpleNamespace(
                        st_dev=current_stat.st_dev, st_ino=current_stat.st_ino,
                        st_size=current_stat.st_size,
                        st_mtime_ns=captured_manifest["mtime_ns"],
                        st_ctime_ns=captured_manifest["ctime_ns"])
                return current_stat

            with patch.object(storage.os, "fstat", side_effect=coarse_manifest_fstat):
                consumers = storage.declared_consumers(root, report["files"])
            self.assertNotIn("artifact.bin", consumers)

    def test_manifest_hash_budget_fails_closed_for_unhashed_consumers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "SHA256SUMS"
            second = root / "SHA256SUMS-2"
            first.write_text("a" * 64 + "  artifact-a\n")
            second.write_text("b" * 64 + "  artifact-b\n")
            (root / "artifact-a").write_bytes(b"a")
            (root / "artifact-b").write_bytes(b"b")
            with patch.object(storage, "MAX_MANIFEST_BYTES", 100), \
                    patch.object(storage, "MAX_TOTAL_MANIFEST_HASH_BYTES", 100):
                report = storage.inventory(root)
            rows = {row["path"]: row for row in report["files"]}
            self.assertEqual(rows["artifact-a"]["declared_consumers"], ["SHA256SUMS"])
            self.assertEqual(rows["artifact-b"]["declared_consumers"], [])
            self.assertEqual(report["manifest_integrity"]["hashed_bytes"], first.stat().st_size)
            self.assertEqual(report["manifest_integrity"]["unhashed_manifest_count"], 1)

    def test_inventory_rejects_symlink_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "alias").symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "real directory"):
                storage.inventory(root / "alias")

    def test_unreadable_directory_is_incomplete_not_empty_success(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(storage.os, "scandir", side_effect=PermissionError("denied")):
                report = storage.inventory(directory)
        self.assertFalse(report["complete"])
        self.assertIn("denied", report["errors"][0]["error"])

    def test_file_replaced_by_symlink_cannot_be_hashed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "artifact"
            path.write_bytes(b"original")
            observed = path.lstat()
            path.unlink()
            path.symlink_to("/etc/passwd")
            with self.assertRaises(OSError):
                storage.file_hash(path, observed)

    def test_file_replaced_during_observation_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "artifact"
            path.write_bytes(b"original")
            observed = path.lstat()
            path.write_bytes(b"changed artifact")
            with self.assertRaisesRegex(RuntimeError, "changed before"):
                storage.file_hash(path, observed)

    def test_ancestor_replaced_by_symlink_cannot_escape_root(self):
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside:
            root = Path(directory)
            (root / "child").mkdir()
            (Path(outside) / "secret").write_bytes(b"never read")
            real_open = os.open

            def replace_before_open(path, flags, **kwargs):
                if str(path) == "child" and kwargs.get("dir_fd") is not None:
                    (root / "child").rmdir()
                    (root / "child").symlink_to(outside, target_is_directory=True)
                return real_open(path, flags, **kwargs)

            with patch.object(storage.os, "open", side_effect=replace_before_open):
                report = storage.inventory(root, hash_max_bytes=1024)
            self.assertFalse(report["complete"])
            self.assertFalse(any("secret" in row["path"] for row in report["files"]))


if __name__ == "__main__":
    unittest.main()
