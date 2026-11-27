import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parent.parent
VERIFY = REPO / "scripts" / "dux-verify.sh"
WATCH_SOURCE = REPO / "src" / "watch.rs"


class VerifyCliTests(unittest.TestCase):
    def test_ground_truth_mounts_match_duxs_excluded_filesystem_policy(self):
        source = WATCH_SOURCE.read_text()
        function = re.search(
            r"fn is_unwatched_mount_fs\(fs: &str\) -> bool\s*\{(.*?)\n\}",
            source,
            re.DOTALL,
        )
        self.assertIsNotNone(function, "scanner mount exclusion function not found")
        excluded = re.findall(r'"([^" ]+)"', function.group(1))
        self.assertIn("proc", excluded)
        self.assertIn("overlay", excluded)
        self.assertIn('fs.starts_with("fuse.")', function.group(1))
        excluded.extend(("fuse.sshfs", "fuse.testfs"))
        for fstype in excluded:
            with self.subTest(fstype=fstype):
                result = subprocess.run(
                    [
                        "bash", "-c",
                        'source "$1"; is_unwatched_fstype "$2"',
                        "bash", str(VERIFY), fstype,
                    ],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

        for fstype in ("ext4", "xfs", "btrfs", "vfat", "fuse-overlayfs"):
            with self.subTest(fstype=fstype):
                result = subprocess.run(
                    [
                        "bash", "-c",
                        'source "$1"; is_unwatched_fstype "$2"',
                        "bash", str(VERIFY), fstype,
                    ],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 1, f"unexpectedly excluded {fstype}")

        result = subprocess.run(
            [
                "bash", "-c",
                'findmnt() { printf "/ ext4\\n/tmp tmpfs\\n/var/lib/container overlay\\n/boot vfat\\n/run/fuse fuse.sshfs\\n/proc proc\\n"; }; '
                'source "$1"; real_mounts /',
                "bash", str(VERIFY),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.stdout.splitlines(), ["/", "/boot"])

    def test_shared_queries_open_the_index_read_only(self):
        with tempfile.TemporaryDirectory() as temp:
            temp_path = Path(temp)
            argv_log = temp_path / "argv"

            env = os.environ.copy()
            env.update(
                {
                    "DUX_DB": str(temp_path / "index.db"),
                    "DUX_VERIFY_ARGV_LOG": str(argv_log),
                }
            )
            subprocess.run(
                [
                    "bash",
                    "-c",
                    'sudo() { "$@"; }; '
                    'sqlite3() { printf "%s\\n" "$@" > "$DUX_VERIFY_ARGV_LOG"; }; '
                    'source "$1"; q "SELECT 1"',
                    "bash",
                    str(VERIFY),
                ],
                check=True,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                argv_log.read_text().splitlines(),
                ["-readonly", "-noheader", "-batch", str(temp_path / "index.db"), "SELECT 1"],
            )

    def test_install_cron_cannot_interpolate_environment_into_root_job(self):
        with tempfile.TemporaryDirectory():
            attacker_value = (
                "/tmp/verify; touch /tmp/cron-injected\n"
                "* * * * * root /bin/touch /tmp/cron-injected"
            )
            env = os.environ.copy()
            env["DUX_LOGDIR"] = attacker_value
            env["DUX_RECONCILE"] = "1"
            result = subprocess.run(
                ["bash", "-c", 'source "$1"; cron_entry', "bash", str(VERIFY)],
                check=True,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                result.stdout,
                "0 */3 * * * root DUX_RECONCILE=1 /usr/local/sbin/dux-verify audit 2>&1 | "
                "/usr/bin/logger -t dux-verify\n",
            )
            source = VERIFY.read_text()
            self.assertIn('line="$(cron_entry)" || return $?', source)
            self.assertIn('sudo tee /etc/cron.d/dux-verify', source)
            self.assertNotIn("DUX_LOGDIR", source)

            invalid = subprocess.run(
                ["bash", "-c", 'source "$1"; cron_entry', "bash", str(VERIFY)],
                env={**env, "DUX_RECONCILE": "1\n* * * * * root id"},
                capture_output=True,
                text=True,
            )
            self.assertEqual(invalid.returncode, 2)
            self.assertIn("DUX_RECONCILE must be 0 or 1", invalid.stderr)

            invalid_install = subprocess.run(
                ["bash", "-c", 'source "$1"; install_cron', "bash", str(VERIFY)],
                env={**env, "DUX_RECONCILE": "1\n* * * * * root id"},
                capture_output=True,
                text=True,
            )
            self.assertEqual(invalid_install.returncode, 2)
            self.assertIn("DUX_RECONCILE must be 0 or 1", invalid_install.stderr)


if __name__ == "__main__":
    unittest.main()
