#!/usr/bin/env python3
"""Render instruction evidence; exclusive counts are additive, inclusive are not."""
import argparse
from collections import defaultdict
import json
from pathlib import Path
import statistics


def function_name(label):
    # callgrind_annotate emits source-file:function [object]. Merge the same
    # machine-code function's costs attributed to different inlined source files.
    return label.split(':', 1)[-1].split(' [', 1)[0]


def subsystem(name):
    n = name.lower()
    # Classify symbols, never arbitrary source paths (Rust's /library/ is not
    # Moonseed's standard library). Inlined callee ownership is unrecoverable.
    if any(s in n for s in ('collect', 'gc::', 'gc_', 'trace_', 'mark_', 'sweep')):
        return 'GC'
    if any(s in n for s in ('malloc', 'alloc::', 'realloc', 'free', 'alloc_')):
        return 'allocation'
    if any(s in n for s in ('::hash::', '::hash<', 'hasher')):
        return 'string/hash (includes non-string hashing)'
    if any(s in n for s in ('table::', 'table_get', 'table_set', 'raw_get', 'raw_set', 'tablekey', 'hashbrown::')):
        return 'table get/set'
    if any(s in n for s in ('builtin', 'stdlib', 'standard::', 'run_lib', 'strpat::')):
        return 'builtins'
    if any(s in n for s in ('fuel', 'safe_point', 'safepoint')):
        return 'fuel/safe points'
    if any(s in n for s in ('call_', 'return_', 'invoke', 'call::', 'continuation', 'lua_frame')):
        return 'call/return'
    if any(s in n for s in ('execute', 'dispatch', 'vm::', 'step', 'run_inner', 'run_hot', '::exec')):
        return 'dispatch loop / VM (inlined work inseparable)'
    return 'other / unattributed'


def main():
    p = argparse.ArgumentParser()
    p.add_argument('results', type=Path)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    report = json.loads(a.results.read_text())
    lines = ['# Whole-program instruction profile', '',
             f'Corpus revision {report["corpus_revision"]}; divisor {report["divisor"]}. Checksums compare the exact same scaled source.',
             'Counts include loader, compilation, boot, execution, teardown and profiler overhead boundaries. No subtraction of startup.',
             'Scaling changes working-set and GC behavior; these are scaled workload profiles, not full-size cost predictions.',
             'Per-iteration counts divide whole-program Ir by the primary outer loop bound; nested loops remain inside that unit. Tail recursion uses recursive transitions as its unit; fib has no source loop.',
             'Symbols are grouped heuristically; inlining hides callee costs inside callers. Inclusive costs overlap and must not be summed.', '',
             '| Workload | Scaling substitutions | MS Ir | PUC Ir | Ratio | Primary iterations | MS Ir/iteration | PUC Ir/iteration |',
             '|---|---|---:|---:|---:|---:|---:|---:|']
    aggregate = defaultdict(int)
    total = 0
    ratios = []
    for r in report['workloads']:
        m, p = r['engines']['moonseed'], r['engines']['puc']
        if r['Ir_ratio'] is None:
            lines.append(f'| {r["name"]} | FAILED | | | | | | |')
            continue
        mi, pi = m['events']['Ir'], p['events']['Ir']
        replacements = r['replacements']
        # Sort's primary outer loop is 4; scaled 100000 is its inner array length.
        iterations = 4 if r['name'] == 'sort' else (int(next(iter(replacements.values()))) if replacements and r['name'] != 'fib' else None)
        lines.append(f'| {r["name"]} | {replacements} | {mi} | {pi} | {mi/pi:.3f} | {iterations or "n/a"} | {mi/iterations if iterations else "n/a"} | {pi/iterations if iterations else "n/a"} |')
        if r['name'] != 'empty':
            ratios.append(mi/pi)
            total += mi
            for f in m['exclusive_all']:
                aggregate[function_name(f['function'])] += f['Ir']
    if ratios:
        lines += ['', f'Ir ratio geomean {statistics.geometric_mean(ratios):.3f}; median {statistics.median(ratios):.3f}; best {min(ratios):.3f}; worst {max(ratios):.3f}. Empty excluded.']
    ranked = sorted((r for r in report['workloads'] if r['name'] != 'empty' and r['Ir_ratio'] is not None), key=lambda r: r['Ir_ratio'], reverse=True)
    if ranked:
        lines += ['', 'Largest scaled relative gaps: ' + ', '.join(f"{r['name']} {r['Ir_ratio']:.3f}x" for r in ranked[:5]) + '. These rank relative overhead, not production frequency or absolute optimization benefit.']
    lines += ['', '## Aggregate exclusive symbols', '', 'Raw Ir summed across scaled workloads (empty excluded); workload sizes weight this ranking. Source-file splits of each symbol are merged.', '', '| Function | Ir | Share of MS Ir |', '|---|---:|---:|']
    groups = defaultdict(int)
    for name, count in aggregate.items():
        groups[subsystem(name)] += count
    for name, count in sorted(aggregate.items(), key=lambda x: -x[1])[:25]:
        lines.append(f'| {name.replace("|", "/")} | {count} | {count/total:.2%} |')
    lines += ['', '## Subsystems (heuristic exclusive symbol attribution)', '', '| Subsystem | Ir | Share |', '|---|---:|---:|']
    for name in ('dispatch loop / VM (inlined work inseparable)', 'call/return', 'table get/set', 'string/hash (includes non-string hashing)', 'allocation', 'GC', 'builtins', 'fuel/safe points', 'other / unattributed'):
        count = groups[name]
        lines.append(f'| {name} | {count} | {count/total:.2%} |')
    lines += ['', 'Zero attributed symbols do not mean zero cost. The first attribution follow-up is the large run_hot bucket (including inlined indexing/stack operations), followed by table/hash and call-frame paths. Inspect per-workload inclusive callers and counter-lane evidence before changing code.', '', '## Cachegrind', '', '| Workload | Engine | I1 miss rate | D1 miss rate | LL miss rate |', '|---|---|---:|---:|---:|']
    for r in report['workloads']:
        for engine, data in r.get('cachegrind', {}).items():
            rates = data.get('miss_rates')
            if rates:
                lines.append(f'| {r["name"]} | {engine} | {rates["I1"]:.3%} | {rates["D1"]:.3%} | {rates["LL"]:.3%} |')
    lines += ['', 'LL denominator = Ir + Dr + Dw; I1 denominator = Ir; D1 denominator = Dr + Dw. Simulated caches, not hardware counters.']
    lines += ['', 'Per-workload inclusive and exclusive top-25 tables: [top-functions.md](top-functions.md).']
    analysis = lines
    lines = ['# Per-workload Callgrind top functions', '', 'Inclusive costs overlap; exclusive costs are additive. Source-file splits are retained as emitted by callgrind_annotate.']
    for r in report['workloads']:
        lines += ['', f'## {r["name"]}: top functions']
        for kind in ('inclusive', 'exclusive'):
            lines += ['', f'### {kind}', '', '| Function | Ir |', '|---|---:|']
            for f in r['engines']['moonseed'].get(kind + '_top25', []):
                lines.append(f'| {f["function"].replace("|", "/")} | {f["Ir"]} |')
    a.output.with_name('top-functions.md').write_text('\n'.join(lines) + '\n')
    a.output.write_text('\n'.join(analysis) + '\n')


if __name__ == '__main__':
    main()
