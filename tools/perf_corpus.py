#!/usr/bin/env python3
"""Noisy hardware counters and cycle samples alongside deterministic instruction profiles."""
import argparse
import json
import os
from pathlib import Path
import subprocess

import bench_env
from bench_corpus import child, CORPUS, DEFAULT_MOONSEED, DEFAULT_PUC, WORKLOADS, sha256
from bench_manifest import REVISION

PERF = '/usr/lib/linux-tools/6.17.0-22-generic/perf'
EVENTS = ['cycles:u', 'instructions:u', 'branches:u', 'branch-misses:u',
          'L1-icache-load-misses:u', 'L1-dcache-load-misses:u', 'LLC-load-misses:u']


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--moonseed', default=DEFAULT_MOONSEED)
    p.add_argument('--puc', default=DEFAULT_PUC)
    p.add_argument('--core', type=int, default=4)
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=True)
    report = {'corpus_revision': REVISION, 'noisy': True, 'not_acceptance_evidence': True,
              'environment': bench_env.capture(a.core, 'bench-stable'),
              'perf_version': bench_env.probe(PERF, '--version'), 'events': EVENTS,
              'binaries': {k: {'path': v, 'sha256': sha256(v)} for k,v in [('puc', a.puc), ('moonseed', a.moonseed)]},
              'workloads': []}
    failed = False
    for name in WORKLOADS:
        script = CORPUS / f'{name}.lua'
        row = {'name': name, 'source_sha256': sha256(script), 'engines': {}}
        for engine, binary in [('puc', a.puc), ('moonseed', a.moonseed)]:
            command = [binary, '-E', str(script)] if engine == 'puc' else [binary, '--timings', str(script)]
            prefix = a.output / f'{name}.{engine}'
            stat_file = Path(str(prefix) + '.stat.csv')
            stat_cmd = [PERF, 'stat', '-x', ';', '-o', str(stat_file), '-e', ','.join(EVENTS), '--', *command]
            stat = child(stat_cmd, a.core)
            stat['command'] = stat_cmd
            counts = {}
            if stat_file.exists():
                for line in stat_file.read_text().splitlines():
                    fields = line.split(';')
                    if len(fields) >= 5 and fields[2].removesuffix(':u') in [e.removesuffix(':u') for e in EVENTS]:
                        try:
                            count = float(fields[0])
                        except ValueError:
                            count = None
                        counts[fields[2].removesuffix(':u') + ':u'] = {'count': count, 'raw': fields[0],
                                             'counter_runtime': fields[3], 'percent_running': fields[4]}
            data_file = Path(str(prefix) + '.data')
            record_cmd = [PERF, 'record', '-e', 'cycles:u', '-F', '499', '-o', str(data_file), '--', *command]
            record = child(record_cmd, a.core)
            record['command'] = record_cmd
            top = []
            report_error = None
            if record['returncode'] == 0:
                result = subprocess.run([PERF, 'report', '--stdio', '--no-children', '--percent-limit', '0',
                                         '-t', ';', '--sort', 'symbol', '-i', str(data_file)],
                                        capture_output=True, text=True, timeout=60, env=dict(os.environ, LC_ALL='C'))
                Path(str(prefix) + '.report.txt').write_text(result.stdout + result.stderr)
                if result.returncode:
                    report_error = result.stderr
                for line in result.stdout.splitlines():
                    fields = line.strip().split(';')
                    if len(fields) >= 2:
                        try:
                            percent = float(fields[0].strip().rstrip('%'))
                        except ValueError:
                            continue
                        top.append({'percent_cycles': percent, 'symbol': fields[1].strip()})
            row['engines'][engine] = {'stat': stat, 'counters': counts, 'record': record,
                                       'top25': top[:25], 'report_error': report_error}
            failed |= stat['returncode'] != 0 or record['returncode'] != 0 or report_error is not None
        samples = [v[k] for v in row['engines'].values() for k in ('stat', 'record')]
        row['checksum_equal'] = all(s['returncode'] == 0 and s['stdout_hex'] == samples[0]['stdout_hex'] for s in samples)
        failed |= not row['checksum_equal']
        report['workloads'].append(row)
        (a.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
        print(name, row['checksum_equal'], flush=True)
    report['inputs_unchanged'] = all(sha256(v['path']) == v['sha256'] for v in report['binaries'].values())
    (a.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    lines = ['# perf hardware counters and cycle profiles — NOISY', '',
             'Full-size revision-2 sources; one stat run and one sampling run per engine/workload, pinned core.',
             'Machine is not in performance mode. Counters may be multiplexed; running percentages and unsupported events are retained in JSON/CSV.',
             'Cycles and cache misses are diagnostic only. Empty may have too few samples to rank functions.', '',
             '| Workload | Engine | ' + ' | '.join(EVENTS) + ' |', '|---|---|' + '---:|' * len(EVENTS)]
    for r in report['workloads']:
        for engine, data in r['engines'].items():
            values = []
            for event in EVENTS:
                counter = data['counters'].get(event, {})
                values.append(str(counter['count']) if counter.get('count') is not None else counter.get('raw', 'unavailable').replace('<', '').replace('>', ''))
            lines.append('| ' + ' | '.join([r['name'], engine, *values]) + ' |')
    for r in report['workloads']:
        lines += ['', f'## {r["name"]}: cycle-sampled top functions (noisy)', '', '| Engine | Symbol | Cycles share |', '|---|---|---:|']
        for engine, data in r['engines'].items():
            for f in data['top25']:
                lines.append(f'| {engine} | {f["symbol"].replace("|", "/")} | {f["percent_cycles"]:.2f}% |')
    (a.output / 'results.md').write_text('\n'.join(lines) + '\n')
    return int(failed or not report['inputs_unchanged'])


if __name__ == '__main__':
    raise SystemExit(main())
