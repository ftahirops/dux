import hashlib
import pathlib
import shutil
import subprocess
import tempfile
import unittest

from dux_split_debug_symbols import split_debug_symbols


class SplitDebugSymbolsTests(unittest.TestCase):
    @unittest.skipUnless(
        all(
            shutil.which(tool)
            for tool in ("cc", "objcopy", "strip", "readelf", "nm", "addr2line")
        ),
        "requires a C compiler and binutils",
    )
    def test_debug_sidecar_matches_stripped_runtime_binary(self):
        with tempfile.TemporaryDirectory(prefix="dux-split-debug-") as temp:
            root = pathlib.Path(temp)
            source = root / "probe.c"
            binary = root / "probe"
            debug = root / "probe.debug"
            source.write_text(
                '#include <stdio.h>\n'
                '__attribute__((section(".dep-v0"), used)) '
                'const char audit_metadata[] = "dependency metadata";\n'
                'int main(void) { puts("ok"); }\n'
            )
            subprocess.run(
                ["cc", "-g", "-O2", "-Wl,--build-id", str(source), "-o", str(binary)],
                check=True,
            )
            build_id = subprocess.check_output(
                ["readelf", "-n", str(binary)], text=True, stderr=subprocess.STDOUT
            ).split("Build ID: ", 1)[1].splitlines()[0]
            original_hash = hashlib.sha256(binary.read_bytes()).hexdigest()

            split_debug_symbols(binary, debug)

            runtime_sections = subprocess.check_output(
                ["readelf", "-W", "-S", str(binary)], text=True,
                stderr=subprocess.STDOUT,
            )
            debug_sections = subprocess.check_output(
                ["readelf", "-W", "-S", str(debug)], text=True,
                stderr=subprocess.STDOUT,
            )
            self.assertNotEqual(hashlib.sha256(binary.read_bytes()).hexdigest(), original_hash)
            self.assertIn(".gnu_debuglink", runtime_sections)
            self.assertNotIn(".debug_info", runtime_sections)
            self.assertIn(".symtab", runtime_sections)
            self.assertIn(".dep-v0", runtime_sections)
            self.assertIn(".debug_info", debug_sections)
            self.assertIn(
                build_id,
                subprocess.check_output(
                    ["readelf", "-n", str(binary)],
                    text=True,
                    stderr=subprocess.STDOUT,
                ),
            )
            self.assertIn(build_id, subprocess.check_output(
                ["readelf", "-n", str(debug)], text=True, stderr=subprocess.STDOUT
            ))
            main_symbol = next(
                line.split() for line in subprocess.check_output(
                    ["nm", "-n", str(debug)], text=True
                ).splitlines() if line.endswith(" main")
            )
            resolved = subprocess.check_output(
                ["addr2line", "-f", "-e", str(binary), f"0x{main_symbol[0]}"],
                text=True,
            )
            self.assertIn("main", resolved)
            self.assertIn("probe.c:3", resolved)
            self.assertEqual(subprocess.check_output([str(binary)], text=True), "ok\n")

    @unittest.skipUnless(
        all(
            shutil.which(tool)
            for tool in ("cc", "objcopy", "strip", "readelf", "nm", "addr2line")
        ),
        "requires a C compiler and binutils",
    )
    def test_missing_dwarf_does_not_mutate_binary(self):
        with tempfile.TemporaryDirectory(prefix="dux-split-debug-no-dwarf-") as temp:
            root = pathlib.Path(temp)
            source = root / "probe.c"
            binary = root / "probe"
            debug = root / "probe.debug"
            source.write_text("int main(void) { return 0; }\n")
            subprocess.run(
                ["cc", "-Wl,--build-id", str(source), "-o", str(binary)], check=True
            )
            original_hash = hashlib.sha256(binary.read_bytes()).hexdigest()

            with self.assertRaisesRegex(RuntimeError, "no DWARF"):
                split_debug_symbols(binary, debug)

            self.assertEqual(hashlib.sha256(binary.read_bytes()).hexdigest(), original_hash)
            self.assertFalse(debug.exists())


if __name__ == "__main__":
    unittest.main()
