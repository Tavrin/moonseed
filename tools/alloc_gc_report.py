#!/usr/bin/env python3
"""Gate H allocation/GC witnesses; use with portable bench-stable binaries.

First collect callgrind_corpus.py into OUTPUT/before and OUTPUT/after (divisor
100). Save each runner and alloc-gc binary before rebuilding. Witnesses always
use OUTPUT/before's scaled sources so source metadata stays byte-identical.
All Lua execution is capped; wall times are diagnostic only.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess

import callgrind_corpus as profile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stats(path):
    return dict(line.split('=', 1) for line in path.read_text().splitlines())


def witnesses(output, phase, binary):
    dest = output / f'witness-{phase}'
    dest.mkdir(parents=True, exist_ok=True)
    inputs = [(src, src.stem, False) for src in sorted((output / 'before').glob('*.lua'))]
    for count in (0, 30000):
        src = output / f'constructor-{count}.lua'
        src.write_text(f'local sum = 0 for i = 1, {count} do '
                       'local t = { i, i + 1, x = i, y = i * 2 } '
                       'sum = sum + t[1] + t.y end print(sum)\n')
        inputs.append((src, src.stem, True))
    inputs.append((output / 'before/alloc_churn.lua', 'churn-no-gc', True))
    provenance = {'binary': str(binary), 'sha256': digest(binary),
                  'load_average': os.getloadavg(), 'commands': []}
    for src, name, no_gc in inputs:
        argv = [str(binary), str(src), str(dest / name)] + (['--no-gc'] if no_gc else [])
        result = subprocess.run(['bash', '-c', 'ulimit -v 2000000; exec timeout 300 "$@"',
                                 'alloc-gc', *argv], capture_output=True)
        provenance['commands'].append({'argv': argv, 'returncode': result.returncode,
                                       'source_sha256': digest(src)})
        if result.returncode:
            raise RuntimeError(result.stderr.decode())
    argv = [str(binary), '--fingerprint']
    result = subprocess.run(['bash', '-c', 'ulimit -v 2000000; exec timeout 300 "$@"',
                             'alloc-gc', *argv], capture_output=True, check=True)
    (dest / 'fingerprint.txt').write_bytes(result.stdout)
    assert digest(binary) == provenance['sha256']
    (dest / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')


def no_gc(output, phase, binary):
    src = output / 'alloc_churn-nogc.lua'
    src.write_text('collectgarbage("stop")\n' + (output / 'before/alloc_churn.lua').read_text())
    dest = output / f'{phase}-nogc.callgrind'
    data = profile.run([profile.VALGRIND, '--tool=callgrind', f'--callgrind-out-file={dest}',
                        str(binary), '--timings', str(src)], 4, dest)
    if data['returncode']:
        raise RuntimeError(data)
    data['binary_sha256'] = digest(binary)
    data['source_sha256'] = digest(src)
    data['events'] = profile.totals(dest)
    for inclusive in (False, True):
        data['inclusive_all' if inclusive else 'exclusive_all'] = profile.annotate(dest, inclusive)
    (output / f'{phase}-nogc.json').write_text(json.dumps(data, indent=2) + '\n')


def compare(output):
    data = {phase: json.loads((output / phase / 'results.json').read_text())
            for phase in ('before', 'after')}
    phases = {phase: {row['name']: row for row in report['workloads']}
              for phase, report in data.items()}
    assert phases['before'].keys() == phases['after'].keys()
    assert set(phases['before']) == set(profile.WORKLOADS), 'complete corpus required'
    assert all(report['inputs_unchanged'] and report['divisor'] == 100 for report in data.values())
    rows = []
    failures = []
    for name, before in phases['before'].items():
        after = phases['after'][name]
        assert before['scaled_sha256'] == after['scaled_sha256']
        assert before['original_sha256'] == after['original_sha256']
        assert before['checksum_equal'] and after['checksum_equal']
        assert before['engines']['moonseed']['stdout_hex'] == after['engines']['moonseed']['stdout_hex']
        b = before['engines']['moonseed']['events']['Ir']
        a = after['engines']['moonseed']['events']['Ir']
        rows.append({'workload': name, 'before_Ir': b, 'after_Ir': a,
                     'change_percent': (a / b - 1) * 100,
                     'after_PUC_ratio': after['Ir_ratio']})
        if a > b * 1.01:
            failures.append(name)
    with (output / 'corpus.csv').open('w') as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    witnesses_data = []
    for b in sorted((output / 'witness-before').glob('*.txt')):
        a = output / 'witness-after' / b.name
        if b.name == 'fingerprint.txt':
            assert b.read_bytes() == a.read_bytes()
            continue
        bs, after_stats = stats(b), stats(a)
        semantics = [k for k in bs if k not in ('allocation_requests', 'requested_bytes')]
        assert all(bs[k] == after_stats[k] for k in semantics), b.name
        entry = {'workload': b.stem, 'stats_equal': True,
                 'before_requests': int(bs['allocation_requests']),
                 'after_requests': int(after_stats['allocation_requests'])}
        for suffix in ('early.snapshot', 'terminal.snapshot', 'stdout'):
            bf, af = b.with_suffix('.' + suffix), a.with_suffix('.' + suffix)
            assert bf.read_bytes() == af.read_bytes(), str(bf)
            entry[suffix + '_sha256'] = digest(bf)
        witnesses_data.append(entry)
    report = {'corpus': rows, 'regressions_over_one_percent': failures,
              'fingerprint': (output / 'witness-before/fingerprint.txt').read_text().strip(),
              'witnesses': witnesses_data}
    (output / 'comparison.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'regressions': failures, 'alloc_churn': rows[0],
                      'fingerprint': report['fingerprint']}))
    if failures:
        raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('witness', 'no-gc', 'compare'))
    parser.add_argument('--output', type=Path, default=Path('results/alloc-gc'))
    parser.add_argument('--phase', choices=('before', 'after'))
    parser.add_argument('--binary', type=Path)
    args = parser.parse_args()
    if args.action == 'compare':
        compare(args.output)
    else:
        if args.phase is None or args.binary is None:
            parser.error('--phase and --binary required for collection')
        (witnesses if args.action == 'witness' else no_gc)(args.output, args.phase, args.binary)


if __name__ == '__main__':
    main()
