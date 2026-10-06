#!/usr/bin/env python3
"""Collect feature-gated embed rows and real Moss frame rows in one JSON artifact."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

from bench_corpus import child, sha256
from bench_build import TARGET
import bench_env


def build(args, log):
    env = dict(os.environ, CARGO_TARGET_DIR=TARGET, CARGO_BUILD_JOBS='8', RUSTFLAGS='')
    result = subprocess.run(['bash', '-c', 'ulimit -v 8000000; exec timeout 1800 "$@"', 'build',
                             'cargo', *args], env=env, capture_output=True, text=True)
    log.write_text(result.stdout + result.stderr)
    if result.returncode:
        raise RuntimeError(f'build failed: {log}')
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--core', type=int, default=4)
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=True)
    report = {'environment': bench_env.capture(a.core, 'bench-stable'), 'categories': ['embedding'],
              'embedding': [], 'moss': [], 'failures': []}
    try:
        result = build(['bench', '-p', 'moonseed', '--bench', 'kernel', '--features', '__measure',
                        '--profile', 'bench-stable', '--no-run', '--message-format=json'], a.output / 'embed-build.log')
        executables = [r['executable'] for line in result.stdout.splitlines()
                       if (r := json.loads(line)).get('reason') == 'compiler-artifact'
                       and r.get('target', {}).get('name') == 'kernel' and r.get('executable')]
        binary = executables[-1]
        os.environ['MOONSEED_BENCH_ONLY'] = 'embed_'
        sample = child([binary], a.core)
        report['embed_sample'] = sample
        report['embed_binary_sha256'] = sha256(binary)
        if sample['returncode']:
            raise RuntimeError('embedding run failed')
        for line in sample['stdout'].splitlines():
            match = re.match(r'(embed_\w+)\s+(\d+) ns/op\s+median of 20; p95 (\d+) ns/op; (.*)', line)
            if match:
                report['embedding'].append(dict(zip(('name', 'median_ns', 'p95_ns', 'detail'),
                                                      (match[1], int(match[2]), int(match[3]), match[4]))))
        if len(report['embedding']) != 17:
            raise RuntimeError(f'expected 17 embed rows, got {len(report["embedding"])}')
    except (RuntimeError, OSError) as e:
        report['failures'].append(str(e))
    # The Moss harness is a private workspace, absent from the public tree.
    moss = Path('integration/moss/Cargo.toml')
    try:
        if not moss.exists():
            raise RuntimeError('Moss harness not present; skipped')
        build(['build', '--manifest-path', str(moss), '--profile', 'bench-stable'], a.output / 'moss-build.log')
        binary = str(Path(TARGET) / 'bench-stable/moonseed-moss-harness')
        sample = child([binary, '--bench'], a.core)
        report['moss_sample'] = sample
        report['moss_binary_sha256'] = sha256(binary)
        if sample['returncode']:
            raise RuntimeError('Moss run failed')
        for line in sample['stdout'].splitlines():
            fields = line.split()
            if len(fields) == 7 and fields[0] in ('100', '1000', '10000'):
                report['moss'].append(dict(zip(('interactions', 'median_us', 'p95_us', 'fuel_per_frame',
                                                'crossings_per_frame', 'allocations_per_frame', 'collections_total'),
                                               map(float, fields))))
        if len(report['moss']) != 3:
            raise RuntimeError('missing Moss rows')
    except (RuntimeError, OSError) as e:
        report['failures'].append(str(e))
    (a.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    lines = ['# Embedding and Moss (diagnostic wall times)', '', '| Row | Median | p95 | Unit |', '|---|---:|---:|---|']
    lines += [f'| {r["name"]} | {r["median_ns"]} | {r["p95_ns"]} | ns/op |' for r in report['embedding']]
    lines += [f'| Moss {int(r["interactions"])} interactions | {r["median_us"]} | {r["p95_us"]} | us/frame |' for r in report['moss']]
    lines += ['', 'Embedding: one warmup, 20 samples; nearest-rank p95. Wait row includes setup and completion.',
              'Moss: 10 warmups, 50 samples; counting allocator enabled; replay checked before timing.',
              'Failures: ' + repr(report['failures'])]
    (a.output / 'results.md').write_text('\n'.join(lines) + '\n')
    return int(bool(report['failures']))


if __name__ == '__main__':
    raise SystemExit(main())
