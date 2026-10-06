#!/usr/bin/env python3
"""Compare the coroutine lane's retained before/after instruction and fuel witnesses."""
import argparse
import hashlib
import json
from pathlib import Path

from callgrind_corpus import totals


def fields(path):
    return dict(line.split('=', 1) for line in path.read_text().splitlines())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('results', type=Path)
    args = parser.parse_args()
    root = args.results
    phases = {phase: root / phase for phase in ('before', 'after')}
    reports = {phase: json.loads((p / 'corpus/results.json').read_text())
               for phase, p in phases.items()}
    before = {w['name']: w for w in reports['before']['workloads']}
    after = {w['name']: w for w in reports['after']['workloads']}
    assert before.keys() == after.keys(), 'incomplete corpus comparison'
    rows = []
    for name, a in before.items():
        b = after[name]
        assert a['scaled_sha256'] == b['scaled_sha256'], 'different scaled sources'
        assert a['checksum_equal'] and b['checksum_equal'], 'PUC mismatch'
        assert a['engines']['moonseed']['stdout_hex'] == b['engines']['moonseed']['stdout_hex']
        ir_a = a['engines']['moonseed']['events']['Ir']
        ir_b = b['engines']['moonseed']['events']['Ir']
        witnesses = {phase: fields(p / f'corpus/{name}.alloc.txt')
                     for phase, p in phases.items()}
        state_fields = ('fuel', 'objects', 'logical_bytes', 'debt', 'threshold',
                        'collections', 'boot_collections', 'young_collections', 'auto_gc')
        states_equal = all(witnesses['before'][k] == witnesses['after'][k] for k in state_fields)
        snapshots_equal = all(digest(phases['before'] / f'corpus/{name}.alloc.{stage}.snapshot')
                              == digest(phases['after'] / f'corpus/{name}.alloc.{stage}.snapshot')
                              for stage in ('early', 'terminal'))
        rows.append({'name': name, 'before_Ir': ir_a, 'after_Ir': ir_b,
                     'change_percent': 100 * (ir_b / ir_a - 1),
                     'fuel': int(witnesses['after']['fuel']),
                     'fuel_and_gc_equal': states_equal, 'snapshots_equal': snapshots_equal})
    micro = []
    for scalars in (1, 3):
        row = {'scalars': scalars}
        for phase, p in phases.items():
            lo = fields(p / f'alloc-{scalars}-1000.txt')
            hi = fields(p / f'alloc-{scalars}-2000.txt')
            ir = (totals(p / f'roundtrip-{scalars}-2000.callgrind')['Ir']
                  - totals(p / f'roundtrip-{scalars}-1000.callgrind')['Ir']) / 1000
            row[phase] = {'Ir_per_roundtrip': ir, **{
                key + '_per_roundtrip': (int(hi[key]) - int(lo[key])) / 1000
                for key in ('allocation_requests', 'requested_bytes', 'fuel')}}
        row['Ir_change_percent'] = 100 * (row['after']['Ir_per_roundtrip']
                                        / row['before']['Ir_per_roundtrip'] - 1)
        micro.append(row)
    passed = (all(r['change_percent'] <= 1 and r['fuel_and_gc_equal'] and r['snapshots_equal']
                  for r in rows)
              and all(r['after']['Ir_per_roundtrip'] < r['before']['Ir_per_roundtrip']
                      and r['after']['allocation_requests_per_roundtrip'] == 0
                      and r['after']['fuel_per_roundtrip'] == r['before']['fuel_per_roundtrip']
                      for r in micro)
              and all(r['inputs_unchanged'] for r in reports.values()))
    (root / 'comparison.json').write_text(json.dumps(
        {'measurement_acceptance': passed, 'corpus': rows, 'roundtrips': micro}, indent=2) + '\n')
    lines = ['# Whole-program scaled corpus', '',
             'Bench-stable, default features, divisor 100. Includes startup, compile, boot, execution and shutdown. '
             'Every scaled source and PUC/Moonseed checksum matches. Raw profiles and environment/load snapshots '
             'are retained in before/corpus and after/corpus. Wall times are diagnostic only.', '',
             '| Workload | Before Ir | After Ir | Change | Fuel (identical) | Snapshot/GC match |',
             '|---|---:|---:|---:|---:|---|']
    for r in rows:
        lines.append(f'| {r["name"]} | {r["before_Ir"]:,} | {r["after_Ir"]:,} | '
                     f'{r["change_percent"]:+.4f}% | {r["fuel"]:,} | '
                     f'{r["snapshots_equal"] and r["fuel_and_gc_equal"]} |')
    lines += ['', 'Fuel/GC and early (after quantum 1000)/terminal byte-identical snapshot witnesses use '
              'the same measure+alloc-gc+counters build on both sides, outside allocation intervals. '
              'The counters feature instruments execution but never changes snapshot state. '
              'The microbenchmark uses a separate feature build; never compare its absolute Ir to the corpus.']
    (root / 'CORPUS.md').write_text('\n'.join(lines) + '\n')
    lines = ['# Warmed round trips', '',
             '1000 unmeasured scalar resume/yield pairs precede a host wait. Complete the wait outside '
             'the allocation interval, then measure 1000 or 2000 cached resume/yield pairs. '
             'Subtract the two totals and divide by 1000. The driver loop and scalar copies are included; '
             'fixed completion/loop-entry/return costs cancel. Whole-process Callgrind includes setup, '
             'which cancels except small bound-parsing/formatting differences. '
             'This is instruction-count evidence, not a wall-speedup claim.', '',
             'Callgrind binary: measure only. Host allocations/fuel/opcode pairs: '
             'measure+alloc-gc+counters, using the existing allocation-counter dependency. '
             'Both feature sets are identical before/after; the allocator measures Rust allocation '
             'requests (including realloc), not VM objects. Zero-iteration controls are retained.', '',
             '| Scalars | Before Ir/pair | After Ir/pair | Change | Allocations/pair before -> after | '
             'Bytes/pair before -> after | Fuel/pair |',
             '|---|---:|---:|---:|---:|---:|---:|']
    for r in micro:
        a, b = r['before'], r['after']
        lines.append(f'| {r["scalars"]} | {a["Ir_per_roundtrip"]:.3f} | '
                     f'{b["Ir_per_roundtrip"]:.3f} | {r["Ir_change_percent"]:+.2f}% | '
                     f'{a["allocation_requests_per_roundtrip"]:g} -> '
                     f'{b["allocation_requests_per_roundtrip"]:g} | '
                     f'{a["requested_bytes_per_roundtrip"]:g} -> {b["requested_bytes_per_roundtrip"]:g} | '
                     f'{b["fuel_per_roundtrip"]:g} |')
    (root / 'ROUNDTRIPS.md').write_text('\n'.join(lines) + '\n')
    print(f'measurement acceptance: {passed}')
    return int(not passed)


if __name__ == '__main__':
    raise SystemExit(main())
