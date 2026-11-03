#!/usr/bin/env python3
"""Source regressions for privileged helper executable resolution."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]


class PrivilegedExecutablePathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tunnel = (ROOT / "src/tunnel.rs").read_text()
        cls.watch = (ROOT / "src/watch.rs").read_text()

    def test_tunnel_helpers_are_pinned_and_used(self):
        for name, path in (
            ("SSH_BIN", "/usr/bin/ssh"),
            ("SSHD_BIN", "/usr/sbin/sshd"),
            ("SSH_KEYGEN_BIN", "/usr/bin/ssh-keygen"),
        ):
            with self.subTest(helper=name):
                self.assertIn(f'const {name}: &str = "{path}";', self.tunnel)
                self.assertIn(f"Command::new({name})", self.tunnel)

    def test_alert_shell_is_pinned_and_used(self):
        self.assertIn('const ALERT_SHELL_BIN: &str = "/bin/sh";', self.watch)
        self.assertIn("Command::new(ALERT_SHELL_BIN)", self.watch)
        self.assertNotIn('Command::new("sh")', self.watch)

    def test_tunnel_does_not_resolve_privileged_helpers_through_path(self):
        for helper in ("ssh", "sshd", "ssh-keygen"):
            with self.subTest(helper=helper):
                self.assertNotIn(f'Command::new("{helper}")', self.tunnel)

    def test_tunnel_ignores_ambient_root_ssh_config(self):
        self.assertRegex(self.tunnel, r'"-F",\s*"/dev/null"')


if __name__ == "__main__":
    unittest.main()
