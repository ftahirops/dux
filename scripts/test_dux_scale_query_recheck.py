#!/usr/bin/env python3
"""Regression tests for bounded, resumable 10m query measurements."""
import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("dux-scale-query-recheck.py")
SPEC = importlib.util.spec_from_file_location("dux_scale_query_recheck", SCRIPT)
RECHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RECHECK)


class ScaleQueryRecheckTests(unittest.TestCase):
    def test_nearest_rank_p95_and_samples_are_preserved(self):
        values = [float(value) for value in range(1, 51)]
        result = RECHECK.summarize(values)
        self.assertEqual(result["sample_count"], 50)
        self.assertEqual(result["samples_seconds"], values)
        self.assertEqual(result["p50_seconds"], 25.0)
        self.assertEqual(result["p95_seconds"], 48.0)

    def test_three_over_budget_samples_prove_fifty_sample_p95_failure(self):
        self.assertFalse(RECHECK.p95_failure_is_proven([1.1, 1.2], 1.0, 50))
        self.assertTrue(RECHECK.p95_failure_is_proven([1.1, 1.2, 1.3], 1.0, 50))

    def test_values_equal_to_strict_budget_count_as_failures(self):
        self.assertTrue(RECHECK.p95_failure_is_proven([1.0, 1.0, 1.0], 1.0, 50))

    def test_query_budget_check_requires_all_query_groups(self):
        empty = {name: {"p95_seconds": 0.01} for name in RECHECK.QUERY_BUDGETS}
        empty["du_depth_1"] = {"p95_seconds": 1.0}
        failures = RECHECK.budget_failures(empty, 0.01, 0.01)
        self.assertEqual(failures, ["du_depth_1 p95 must be below 1.0s"])


if __name__ == "__main__":
    unittest.main()
