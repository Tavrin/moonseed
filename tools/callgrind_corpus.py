#!/usr/bin/env python3
"""Whole-program profiles, including process startup; scaled sources never overwrite corpus."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time

import bench_env
from bench_corpus import CORPUS, DEFAULT_MOONSEED, DEFAULT_PUC, WORKLOADS, sha256
from bench_manifest import BOUNDS, REVISION, TAGS

VALGRIND = os.environ.get('VALGRIND', 'valgrind')


def scaled(name, divisor):
    source = (CORPUS / f'{name}.lua').read_text()
    replacements = {}
    for bound in BOUNDS[name]:
        if name == 'fib':
            # Exponential work: reduce argument rather than divide it.
            value = f'fib({max(2, 34 - round(math.log(divisor, 1.61803398875)))})'
        else:
            value = str(max(1, int(bound) // divisor))
        replacements[bound] = value
    source = re.sub(r'fib\(34\)|\b\d+\b', lambda m: replacements.get(m[0], m[0]), source)
    return source, replacements


def run(argv, core, output):
    start = time.monotonic()
    result = subprocess.run(['bash', '-c', 'ulimit -v 6000000; exec timeout --kill-after=2s 1800 "$@"',
                             'profile', 'taskset', '-c', str(core), *argv], capture_output=True)
    Path(str(output) + '.stdout').write_bytes(result.stdout)
    Path(str(output) + '.stderr').write_bytes(result.stderr)
    return {'returncode': result.returncode, 'wall_seconds': time.monotonic() - start,
            'stdout_hex': result.stdout.hex(), 'load_average': os.getloadavg(), 'command': argv}


def totals(path):
    content = path.read_text()
    events = re.search(r'^events: (.+)$', content, re.M)
    summary = re.search(r'^(?:summary|totals): (.+)$', content, re.M)
    if not events or not summary:
        raise ValueError(f'missing events/totals: {path}')
    return dict(zip(events[1].split(), map(int, summary[1].split())))


def annotate(path, inclusive):
    command = [str(Path(VALGRIND).with_name('callgrind_annotate')), '--auto=no', '--show=Ir',
               '--threshold=100', f'--inclusive={"yes" if inclusive else "no"}', str(path)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stderr)
    rows = []
    for line in result.stdout.splitlines():
        match = re.match(r'^\s*([\d,]+)\s+\([^)]*\)\s+(.+)$', line)
        if match and match[2] != 'PROGRAM TOTALS':
            rows.append({'Ir': int(match[1].replace(',', '')), 'function': match[2]})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--moonseed', default=DEFAULT_MOONSEED)
    parser.add_argument('--puc', default=DEFAULT_PUC)
    parser.add_argument('--divisor', type=int, default=100)
    parser.add_argument('--core', type=int, default=4)
    parser.add_argument('--workloads', nargs='+', default=list(WORKLOADS))
    parser.add_argument('--cachegrind', action='store_true')
    parser.add_argument('--profile', default='bench-stable', help='provenance label for the supplied binary')
    args = parser.parse_args()
    if args.divisor < 1 or not set(args.workloads) <= set(WORKLOADS):
        parser.error('positive divisor and known workloads required')
    args.output.mkdir(parents=True, exist_ok=True)
    report = {'corpus_revision': REVISION, 'divisor': args.divisor,
              'environment': bench_env.capture(args.core, args.profile),
              'valgrind': bench_env.probe(VALGRIND, '--version'),
              'binaries': {k: {'path': v, 'sha256': sha256(v)} for k, v in [('puc', args.puc), ('moonseed', args.moonseed)]},
              'workloads': []}
    failed = False
    for name in args.workloads:
        source, replacements = scaled(name, args.divisor)
        script = args.output / f'{name}.lua'
        script.write_text(source)
        row = {'name': name, 'categories': TAGS[name], 'replacements': replacements,
               'original_sha256': sha256(CORPUS / f'{name}.lua'), 'scaled_sha256': sha256(script), 'engines': {}}
        for engine, binary in [('puc', args.puc), ('moonseed', args.moonseed)]:
            dest = args.output / f'{name}.{engine}.callgrind'
            cmd = [VALGRIND, '--tool=callgrind', f'--callgrind-out-file={dest}', binary]
            cmd += ['-E', str(script)] if engine == 'puc' else ['--timings', str(script)]
            data = run(cmd, args.core, dest)
            if data['returncode'] == 0:
                data['events'] = totals(dest)
                if engine == 'moonseed':
                    for inclusive in (False, True):
                        key = 'inclusive' if inclusive else 'exclusive'
                        functions = annotate(dest, inclusive)
                        data[key + '_top25'] = functions[:25]
                        # Keep all exclusive rows for additive subsystem aggregation.
                        if not inclusive:
                            data['exclusive_all'] = functions
            else:
                failed = True
            row['engines'][engine] = data
        p, m = row['engines']['puc'], row['engines']['moonseed']
        row['checksum_equal'] = p['returncode'] == m['returncode'] == 0 and p['stdout_hex'] == m['stdout_hex']
        failed |= not row['checksum_equal']
        row['Ir_ratio'] = m['events']['Ir'] / p['events']['Ir'] if row['checksum_equal'] else None
        if args.cachegrind and name in ('fib', 'table_fields', 'sort'):
            row['cachegrind'] = {}
            for engine, binary in [('puc', args.puc), ('moonseed', args.moonseed)]:
                dest = args.output / f'{name}.{engine}.cachegrind'
                cmd = [VALGRIND, '--tool=cachegrind', '--cache-sim=yes', f'--cachegrind-out-file={dest}', binary]
                cmd += ['-E', str(script)] if engine == 'puc' else ['--timings', str(script)]
                data = run(cmd, args.core, dest)
                if data['returncode'] == 0:
                    data['checksum_equal'] = data['stdout_hex'] == row['engines'][engine]['stdout_hex']
                    failed |= not data['checksum_equal']
                    e = data['events'] = totals(dest)
                    data['miss_rates'] = {'I1': e['I1mr'] / e['Ir'], 'D1': (e['D1mr'] + e['D1mw']) / (e['Dr'] + e['Dw']),
                                          'LL': (e['ILmr'] + e['DLmr'] + e['DLmw']) / (e['Ir'] + e['Dr'] + e['Dw'])}
                else:
                    failed = True
                row['cachegrind'][engine] = data
        report['workloads'].append(row)
        (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
        print(name, row['Ir_ratio'], flush=True)
    report['inputs_unchanged'] = all(sha256(v['path']) == v['sha256'] for v in report['binaries'].values())
    (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    return int(failed or not report['inputs_unchanged'])


if __name__ == '__main__':
    raise SystemExit(main())
