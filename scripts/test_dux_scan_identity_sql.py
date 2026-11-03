#!/usr/bin/env python3
"""Execute the scanner's exact SQLite identity predicates on edge cases."""

from pathlib import Path
import re
import sqlite3
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCAN_SOURCE = (ROOT / "src/scan.rs").read_text()


def query_for(function_name):
    start = SCAN_SOURCE.index(f"fn {function_name}(")
    match = re.search(
        r'\.query_row\(\s*"((?:[^"\\]|\\.)*)"',
        SCAN_SOURCE[start:],
        re.DOTALL,
    )
    if not match:
        raise AssertionError(f"could not extract SQL from {function_name}")
    return bytes(match.group(1), "utf-8").decode("unicode_escape")


class ScanIdentitySqlTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.db.execute(
            "CREATE TABLE scan_nodes(dev INTEGER,inode INTEGER,birth_ns INTEGER)"
        )
        self.db.executemany(
            "INSERT INTO scan_nodes VALUES(?,?,?)",
            [
                (1, 10, 0),
                (1, 10, 0),  # repeated, generation unavailable: uncertain
                (1, 11, 0),  # one observation: no ambiguity in this scan
                (1, 12, 100),
                (1, 12, 100),  # repeated, known same incarnation
                (1, 13, 0),
                (1, 13, 200),  # differing incarnations: separate conflict
            ],
        )

    def tearDown(self):
        self.db.close()

    def count(self, function_name):
        return self.db.execute(query_for(function_name)).fetchone()[0]

    def test_unknown_repeated_identity_predicate(self):
        self.assertEqual(
            self.count("count_unknown_repeated_inode_identities"), 1
        )

    def test_known_incarnation_conflict_predicate(self):
        self.assertEqual(self.count("count_conflicting_inode_incarnations"), 1)


if __name__ == "__main__":
    unittest.main()
