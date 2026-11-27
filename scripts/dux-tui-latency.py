#!/usr/bin/env python3
"""Measure TUI first-frame, help acknowledgement, and quit latency on an index.

Designed to reuse the exact PTY interaction used by `dux-scale.py` without
rebuilding a large fixture or rerunning its scan/query suites.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import tempfile
import time
from dux_qualification_sandbox import require_disposable_qualification_vm


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--db', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=50)
    parser.add_argument('--scale-harness', type=Path, required=True,
                        help='dux-scale.py copy that defines the PTY benchmark')
    args = parser.parse_args()
    assert 3 <= args.samples <= 100
    try:
        require_disposable_qualification_vm()
    except RuntimeError as error:
        parser.error(str(error))
    spec = importlib.util.spec_from_file_location('dux_scale_harness', args.scale_harness)
    module = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(module)

    args.report.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='dux-tui-latency-') as runtime:
        env = dict(os.environ, DUX_RUNTIME_DIR=runtime)
        env.pop('NOTIFY_SOCKET', None)
        samples = []
        started = time.monotonic()
        for index in range(args.samples):
            sample = module.tui_sample(args.binary, args.db, env)
            sample['sample'] = index + 1
            samples.append(sample)
        input_values = [s['input_ack_seconds'] for s in samples]
        quit_values = [s['quit_seconds'] for s in samples]
        input_p95 = module.percentile(input_values, 0.95)
        quit_p95 = module.percentile(quit_values, 0.95)
        report = {
            'status': 'passed' if input_p95 < 0.1 and quit_p95 < 0.25 else 'failed',
            'binary_sha256': digest(args.binary),
            'database_bytes': args.db.stat().st_size,
            'platform': platform.platform(),
            'samples': samples,
            'input_ack_p50_seconds': module.percentile(input_values, 0.50),
            'input_ack_p95_seconds': input_p95,
            'input_ack_max_seconds': max(input_values),
            'quit_p95_seconds': quit_p95,
            'quit_max_seconds': max(quit_values),
            'thresholds_seconds': {'input_ack_p95': 0.1, 'quit_p95': 0.25},
            'wall_seconds': round(time.monotonic() - started, 3),
        }
        args.report.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: v for k, v in report.items() if k != 'samples'}, indent=2))
    if report['status'] != 'passed':
        raise SystemExit(1)


if __name__ == '__main__':
    main()
