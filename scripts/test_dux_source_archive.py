import hashlib
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import unittest

import dux_source_archive


class SourceArchiveTests(unittest.TestCase):
    def test_dirty_state_ignores_excluded_local_files_but_tracks_source_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            source = root / "src" / "module.rs"
            source.parent.mkdir()
            source.write_text("fn main() {}\n")
            audit = root / "PRODUCTION_READINESS_AUDIT.md"
            audit.write_text("audit evidence\n")
            settings = root / ".claude" / "settings.local.json"
            settings.parent.mkdir()
            settings.write_text('{"local": true}\n')
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "initial"], cwd=root, check=True)

            audit.write_text("updated audit evidence\n")
            settings.write_text('{"local": false}\n')
            self.assertFalse(dux_source_archive.source_tree_dirty(root))

            source.write_text("fn main() { /* changed */ }\n")
            self.assertTrue(dux_source_archive.source_tree_dirty(root))
            source.write_text("fn main() {}\n")
            (root / "src" / "new.rs").write_text("// untracked source\n")
            self.assertTrue(dux_source_archive.source_tree_dirty(root))

    def test_source_tree_digest_binds_paths_and_contents_but_excludes_local_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            source = root / "src" / "module.rs"
            source.parent.mkdir()
            source.write_bytes(b"fn main() {}\n")
            audit = root / "PRODUCTION_READINESS_AUDIT.md"
            audit.write_text("machine-local audit evidence")
            settings = root / ".claude" / "settings.local.json"
            settings.parent.mkdir()
            settings.write_text('{"local": true}')

            first_entries, first_digest = dux_source_archive.collect_source_entries(root)
            audit.write_text("changed audit evidence")
            settings.write_text('{"local": false}')
            second_entries, second_digest = dux_source_archive.collect_source_entries(root)
            self.assertEqual(first_digest, second_digest)
            self.assertEqual(first_entries, second_entries)
            self.assertEqual([entry["path"] for entry in first_entries], ["src/module.rs"])

            source.write_bytes(b"fn main() { println!(\"changed\"); }\n")
            changed_entries, changed_digest = dux_source_archive.collect_source_entries(root)
            self.assertNotEqual(first_digest, changed_digest)
            self.assertNotEqual(first_entries[0]["sha256"], changed_entries[0]["sha256"])

            renamed = source.with_name("renamed.rs")
            source.rename(renamed)
            renamed_entries, renamed_digest = dux_source_archive.collect_source_entries(root)
            self.assertNotEqual(changed_digest, renamed_digest)
            self.assertEqual(renamed_entries[0]["path"], "src/renamed.rs")

    def test_source_tree_digest_rejects_symlink_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            outside = root.parent / f"{root.name}-outside-secret"
            outside.write_text("private data")
            try:
                (root / "leak").symlink_to(outside)
                with self.assertRaisesRegex(RuntimeError, "symlink source input is not allowed"):
                    dux_source_archive.collect_source_entries(root)
            finally:
                outside.unlink(missing_ok=True)

    def test_archive_bytes_are_reproducible_and_metadata_is_normalized(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "src" / "module.rs"
            source.parent.mkdir()
            source.write_bytes(b"fn main() {}\n")
            entry = {
                "path": "src/module.rs",
                "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                "size": source.stat().st_size,
            }
            first, second = root / "first.tar.gz", root / "second.tar.gz"

            dux_source_archive.write_source_archive(root, [entry], first)
            time.sleep(1.1)
            dux_source_archive.write_source_archive(root, [entry], second)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(int.from_bytes(first.read_bytes()[4:8], "little"), 0)
            with tarfile.open(first, "r:gz") as archive:
                member = archive.getmember("src/module.rs")
                self.assertEqual(member.mtime, 0)
                self.assertEqual(member.uid, 0)
                self.assertEqual(member.gid, 0)
                self.assertEqual(archive.extractfile(member).read(), b"fn main() {}\n")

    def test_source_change_after_inventory_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "input"
            source.write_bytes(b"before")
            entry = {
                "path": "input",
                "sha256": hashlib.sha256(b"before").hexdigest(),
                "size": 6,
            }
            source.write_bytes(b"after!")
            with self.assertRaisesRegex(RuntimeError, "source changed while packaging"):
                dux_source_archive.write_source_archive(root, [entry], root / "source.tar.gz")


if __name__ == "__main__":
    unittest.main()
