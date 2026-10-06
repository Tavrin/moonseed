#!/usr/bin/env python3
"""Warmed call-family slopes. Never rewrites bench/calls or the frozen corpus.

The net slope subtracts a matched no-call loop. It includes any differing
argument setup and callee body; it is not automatically an intrinsic ABI cost.
Instruction-address decomposition is needed to remove those supporting ops.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time

import bench_env
from callgrind_corpus import VALGRIND, totals

ROOT = Path(__file__).resolve().parents[1]
PERF = os.environ.get('PERF', 'perf')
PUC = os.environ.get('PUC', str(ROOT / 'vendor/lua-5.4.9/src/lua'))
MOONSEED = os.environ.get('MOONSEED_RUN', str(Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'bench-stable/moonseed-run'))
EVENTS = 'cycles:u,instructions:u,branches:u,branch-misses:u,duration_time'


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def render(path, iterations, measured):
    source = path.read_text()
    source, n = re.subn(r'local iterations = \d+ -- CALL_BENCH_N',
                        f'local iterations = {iterations} -- CALL_BENCH_N', source)
    source, m = re.subn(r'local measured = (?:true|false) -- CALL_BENCH_MODE',
                        f'local measured = {str(measured).lower()} -- CALL_BENCH_MODE', source)
    if (n, m) != (1, 1):
        raise ValueError(f'missing or duplicate parameters in {path}')
    return source


def run(command, prefix, core, valgrind=False):
    cap, seconds = (6000000, 1800) if valgrind else (2000000, 300)
    wrapped = ['bash', '-c', f'ulimit -v {cap}; exec timeout --kill-after=2s {seconds} "$@"',
               'call-bench', 'taskset', '-c', str(core), *map(str, command)]
    before = os.getloadavg()
    start = time.monotonic_ns()
    result = subprocess.run(wrapped, capture_output=True)
    elapsed = time.monotonic_ns() - start
    Path(str(prefix) + '.stdout').write_bytes(result.stdout)
    Path(str(prefix) + '.stderr').write_bytes(result.stderr)
    return {'command': wrapped, 'returncode': result.returncode,
            'stdout': result.stdout.decode(errors='replace'),
            'elapsed_ns': elapsed, 'load_before': before, 'load_after': os.getloadavg()}


def perf_events(path):
    data = {}
    for line in path.read_text().splitlines():
        fields = line.split(';')
        if len(fields) > 2:
            try:
                value = float(fields[0].strip())
            except ValueError:
                continue
            data[fields[2].strip()] = value
    return data


def slope(samples, counts, mode, field, calls):
    low, high = (samples[f'{mode}.{n}'][field] for n in counts)
    if isinstance(low, dict):
        return {key: (high[key] - low[key]) / ((counts[1] - counts[0]) * calls)
                for key in low.keys() & high.keys()}
    return (high - low) / ((counts[1] - counts[0]) * calls)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--moonseed', default=MOONSEED)
    parser.add_argument('--puc', default=PUC)
    parser.add_argument('--iterations', type=int, nargs=2, default=[200, 1000])
    parser.add_argument('--benchmarks', nargs='+')
    parser.add_argument('--core', type=int, default=4)
    parser.add_argument('--perf', action='store_true')
    parser.add_argument('--alloc', type=Path, help='call-alloc binary (alloc-gc feature)')
    parser.add_argument('--dump-instr', action='store_true')
    args = parser.parse_args()
    if not 0 < args.iterations[0] < args.iterations[1]:
        parser.error('two increasing positive iteration counts required')
    sources = sorted((ROOT / 'bench/calls').glob('*.lua'))
    if args.benchmarks:
        known = {p.stem for p in sources}
        if not set(args.benchmarks) <= known:
            parser.error(f'unknown benchmarks: {set(args.benchmarks) - known}')
        sources = [p for p in sources if p.stem in args.benchmarks]
    args.output.mkdir(parents=True, exist_ok=True)
    report = {'environment': bench_env.capture(args.core, 'bench-stable'),
              'iterations': args.iterations, 'definition': 'call slope minus no-call control slope',
              'binaries': {k: {'path': str(v), 'sha256': sha256(v)}
                           for k, v in [('moonseed', args.moonseed), ('puc', args.puc)]},
              'workloads': []}
    if args.alloc:
        report['binaries']['alloc'] = {'path': str(args.alloc), 'sha256': sha256(args.alloc)}
    failed = False
    for template in sources:
        calls = int(re.search(r'^-- calls-per-iteration: (\d+)$', template.read_text(), re.M)[1])
        row = {'name': template.stem, 'calls_per_iteration': calls,
               'template_sha256': sha256(template), 'engines': {}}
        for engine, binary in [('moonseed', args.moonseed), ('puc', args.puc)]:
            samples = row['engines'][engine] = {}
            for mode in ['call', 'control']:
                for n in args.iterations:
                    key = f'{mode}.{n}'
                    script = args.output / f'{template.stem}.{key}.lua'
                    script.write_text(render(template, n, mode == 'call'))
                    dest = args.output / f'{template.stem}.{engine}.{key}.callgrind'
                    command = [VALGRIND, '--tool=callgrind', f'--callgrind-out-file={dest}']
                    if args.dump_instr:
                        command += ['--dump-instr=yes', '--collect-jumps=yes']
                    command += [binary] + (['-E'] if engine == 'puc' else []) + [script]
                    sample = samples[key] = run(command, dest, args.core, True)
                    sample['script_sha256'] = sha256(script)
                    failed |= sample['returncode'] != 0
                    if sample['returncode'] == 0:
                        sample['events'] = totals(dest)
                    if args.perf:
                        dest = args.output / f'{template.stem}.{engine}.{key}.perf'
                        command = [PERF, 'stat', '-x', ';', '-o', dest, '-e', EVENTS,
                                   binary] + (['-E'] if engine == 'puc' else []) + [script]
                        sample['perf'] = run(command, dest, args.core)
                        sample['perf']['events'] = perf_events(dest) if dest.exists() else {}
                        # perf permissions/hardware failure is reported, not hidden.
                    if args.alloc and engine == 'moonseed':
                        dest = args.output / f'{template.stem}.{key}.alloc'
                        sample['alloc'] = run([args.alloc, script], dest, args.core)
                        failed |= sample['alloc']['returncode'] != 0
                        if sample['alloc']['returncode'] == 0:
                            sample['alloc']['measurement'] = json.loads(sample['alloc']['stdout'])
            if all(s.get('returncode') == 0 for s in samples.values()):
                slopes = {mode: slope(samples, args.iterations, mode, 'events', calls)
                          for mode in ['call', 'control']}
                row.setdefault('slopes', {})[engine] = {
                    **slopes, 'net': {k: slopes['call'][k] - slopes['control'][k]
                                     for k in slopes['call']}}
                if args.perf and all(s['perf']['returncode'] == 0 for s in samples.values()):
                    perf_samples = {k: s['perf'] for k, s in samples.items()}
                    call = slope(perf_samples, args.iterations, 'call', 'events', calls)
                    control = slope(perf_samples, args.iterations, 'control', 'events', calls)
                    row.setdefault('perf_net', {})[engine] = {
                        k: call[k] - control[k] for k in call.keys() & control.keys()}
                    row['perf_net'][engine]['elapsed_ns'] = (
                        slope(perf_samples, args.iterations, 'call', 'elapsed_ns', calls)
                        - slope(perf_samples, args.iterations, 'control', 'elapsed_ns', calls))
        row['checksum_equal'] = all(
            row['engines']['moonseed'][key]['stdout'] == row['engines']['puc'][key]['stdout']
            for key in row['engines']['moonseed'])
        row['control_checksum_equal'] = all(
            row['engines']['moonseed'][f'call.{n}']['stdout']
            == row['engines']['moonseed'][f'control.{n}']['stdout'] for n in args.iterations)
        failed |= not row['checksum_equal'] or not row['control_checksum_equal']
        if 'slopes' in row and len(row['slopes']) == 2:
            row['ratio'] = row['slopes']['moonseed']['net']['Ir'] / row['slopes']['puc']['net']['Ir']
        report['workloads'].append(row)
        (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
        print(row['name'], row.get('ratio'), flush=True)
    report['inputs_unchanged'] = all(sha256(v['path']) == v['sha256'] for v in report['binaries'].values())
    report['failed'] = failed or not report['inputs_unchanged']
    (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    lines = ['# Call-family instruction slopes', '',
             'Net host Ir per dynamic Lua invocation: call slope minus matched no-call slope.',
             'Includes differing setup and callee body. Wall/cycles are diagnostic only.', '',
             '| Benchmark | Moonseed Ir/call | PUC Ir/call | Ratio |', '|---|---:|---:|---:|']
    for row in report['workloads']:
        if 'ratio' in row:
            lines.append(f"| {row['name']} | {row['slopes']['moonseed']['net']['Ir']:.4f} | "
                         f"{row['slopes']['puc']['net']['Ir']:.4f} | {row['ratio']:.4f} |")
    (args.output / 'TABLE.md').write_text('\n'.join(lines) + '\n')
    return int(report['failed'])


if __name__ == '__main__':
    raise SystemExit(main())
