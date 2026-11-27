#!/usr/bin/env python3
"""Regression for scan RSS measurement after large fixture generation."""
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name('dux-scale.py')
SPEC = importlib.util.spec_from_file_location('dux_scale', SCRIPT)
DUX_SCALE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DUX_SCALE)


class ScaleMeasurementTests(unittest.TestCase):
    def test_scan_budget_covers_production_count_between_standard_levels(self):
        self.assertEqual(DUX_SCALE.scan_budget_seconds(100000), 30)
        self.assertEqual(DUX_SCALE.scan_budget_seconds(1000000), 180)
        self.assertEqual(DUX_SCALE.scan_budget_seconds(1370000), 300)
        self.assertEqual(DUX_SCALE.scan_budget_seconds(10000000), 1800)

    def test_reused_fixture_requires_a_read_only_mount(self):
        with tempfile.TemporaryDirectory(prefix='dux-scale-reuse-rw-') as directory:
            with self.assertRaisesRegex(RuntimeError, 'read-only mount'):
                DUX_SCALE.validate_reused_tree(Path(directory), 10000000, Path(directory) / 'evidence.json')

    def test_timing_report_retains_samples_and_uses_documented_percentiles(self):
        samples = [float(value) for value in range(1, 21)]
        result = DUX_SCALE.timing_summary(samples)

        self.assertEqual(result['sample_count'], 20)
        self.assertEqual(result['samples_seconds'], samples)
        self.assertEqual(result['p50_seconds'], 10.0)
        self.assertEqual(result['p95_seconds'], 19.0)

    def test_wait4_parent_highwater_is_not_reported_as_scan_peak(self):
        # Model the scale worker retaining memory from 10m Path objects. Linux
        # wait4 can carry this parent's peak across fork; the scan child uses far
        # less and has a measurable VmHWM of its own.
        parent_memory = bytearray(96 * 1024 * 1024)
        for offset in range(0, len(parent_memory), 4096):
            parent_memory[offset] = 1
        child = (
            'import time; b=bytearray(32*1024*1024); '
            '[(b.__setitem__(i,1)) for i in range(0,len(b),4096)]; time.sleep(.2)'
        )
        with tempfile.TemporaryDirectory(prefix='dux-scale-rss-test-') as directory:
            result = DUX_SCALE.measured_process(
                [sys.executable, '-c', child], os.environ.copy(), Path(directory) / 'scan.log')
        self.assertGreater(result['peak_rss_kib'], 24 * 1024)
        self.assertLess(result['peak_rss_kib'], 64 * 1024)
        self.assertGreater(resource_maxrss_kib(), result['peak_rss_kib'])
        self.assertEqual(parent_memory[0], 1)


def resource_maxrss_kib():
    import resource
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss


if __name__ == '__main__':
    unittest.main()
