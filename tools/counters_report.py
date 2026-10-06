#!/usr/bin/env python3
"""Summarize moonseed-run / Moss counters JSON; no timing-based conclusions."""
import argparse
import collections
import json
from pathlib import Path
import re


def combine(records):
    result = {key: collections.Counter() for key in ('opcodes', 'pairs', 'events')}
    result['instructions'] = 0
    result['allocations'] = collections.defaultdict(lambda: [0, 0])
    for record in records:
        result['instructions'] += record['instructions']
        for key in ('opcodes', 'pairs', 'events'):
            result[key].update(record[key])
        for kind, (count, size) in record['allocations'].items():
            result['allocations'][kind][0] += count
            result['allocations'][kind][1] += size
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('results', type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    groups = collections.defaultdict(list)
    for path in sorted(args.results.glob('*.json')):
        name = re.sub(r'-\d\d$', '', path.stem) if path.stem.startswith('moss-') else path.stem
        record = json.loads(path.read_text())
        if 'opcodes' in record:
            groups[name].append(record)
    records = {name: combine(rows) for name, rows in groups.items()}
    total = combine(records.values())
    source = (root / 'crates/moonseed/src/runtime.rs').read_text()
    hot = set(re.findall(r'Op::(\w+)', source.split('fn hot_op(')[1].split('// Table hits for')[0]))
    common = set(re.findall(r'Op::(\w+)', source.split('fn exec(')[1].split('/// Move past')[0]))
    # Amortized whole-workload instructions divided by the named dominant loop.
    loops = {'alloc_churn': (3000000, 'allocation loop'), 'application': (12000 * 256, 'entity updates'),
             'closures': (10000000, 'main loop'), 'coroutines': (3000000, 'resume/yield exchanges'),
             'generic_for': (40000 * 256, 'combined iterator bodies'), 'metamethods': (4000000, 'main loop'),
             'method_calls': (4000000, 'main loop'), 'native_calls': (2000000, 'main loop'),
             'patterns': (200000, 'main loop'), 'sort': (4, 'outer sort-pair loop'),
             'numeric_loops': (70669667, 'all source loop bodies: 60M + 10M + 2000 + 667667'),
             'strings': (360000, 'format loop; includes later gmatch/concat work'),
             'table_fields': (6000000, 'inner movement loop')}
    text = ['# Execution counters — Phase 3.32 lane C', '',
            'Frequency evidence only. The machine is not in performance mode; no wall-clock numbers are baseline evidence. '
            'See runs.tsv for exit status and load, VALIDATION.md for gates and code identity, and the per-workload JSON for full data.', '',
            '## Measurement contract', '',
            '- Corpus: execution and exit finalization; compile, boot and library installation excluded. Failed workloads retain partial counters.',
            '- Moss: 10 warmup frames, then 50 separately reset frame samples at each interaction count. Host API setup of each frame is included.',
            '- Totals combine each corpus run once and all 100 measured Moss frames; this weights large workloads heavily.',
            '- Opcodes count fetched Lua bytecode once, including faults. Pairs follow execution order across calls and coroutine switches; reset breaks the chain.',
            '- dispatch_hot counts instructions completed by the hot handler. dispatch_common_entries includes rare fallthrough; exclusive common = common entries minus rare. Hot attempts include declined probes.',
            '- Table hits/misses count get/get_view/update_view probes, including repeated hot-to-cold probes and GC/metamethod reads. Field hits are the string-key subset of raw hits. Inserts count newly appended live entries; rehashes are index rebuilds and resizes are append-driven index capacity increases.',
            '- String bytes count actual owned/borrowed table-key hash invocations, including rehashes of existing string keys. Strings are not interned in this VM. Hash bytes/field opcode includes all hash causes, not just those directly caused by field opcodes.',
            '- Missing event/object keys mean zero. Allocation arrays are [successful object count, initial logical bytes]; later table/stack/payload growth and host malloc traffic are not allocation bytes. GC work units are actual spend() units, steps are work() invocations, cycles exclude whitening-only resets and include minors.',
            '- Lua calls count successful enter_lua entries (including tail replacements); native_calls includes builtin dispatch. lua_to_host_calls includes callback continuations. Host call attempts are counted at API entry; waits count committed transitions. Stack growth counts length extensions, not just reallocations. Frame pushes include boundary frames and host-call entry frames; initial boot thread frames are excluded.',
            '- Builtin steps distinguish resume, library and auxiliary batches; *_operations count machine next() transitions. These categories overlap and must not be summed as distinct instructions.',
            '- Counters are not snapshot state. Runtime::run and start_call scope automatically; counter_scope() includes additional host-side API work. Unscoped leaf operations are omitted. Scopes must be dropped on the creating thread; Rc enforces this.', '',
            '## Ranked opcodes', '', '| Opcode | Count | Overall % | Workload max % | Current tier |', '|---|---:|---:|---:|---|']
    for op, count in total['opcodes'].most_common():
        tier = 'hot/common' if op in hot and op in common else 'hot (rare helper)' if op in hot else 'common' if op in common else 'rare'
        maxpct = max(100 * r['opcodes'][op] / max(r['instructions'], 1) for r in records.values())
        text.append(f'| {op} | {count} | {100 * count / max(total["instructions"], 1):.3f} | {maxpct:.3f} | {tier} |')
    text += ['', '## Top 20 opcode pairs', '', '| Pair | Count |', '|---|---:|']
    text += [f'| {pair} | {n} |' for pair, n in total['pairs'].most_common(20)]
    text += ['', '## Per workload', '', '| Workload | VM instructions | Instructions/iteration | Lua/native/tail calls | Table hits/misses/inserts/rehashes/resizes | Hash bytes/field opcode | GC steps/work/cycles |', '|---|---:|---|---|---|---:|---|']
    for name, row in records.items():
        e = row['events']
        loop = loops.get(name)
        ratio = f'{row["instructions"] / loop[0]:.3f} ({loop[1]})' if loop else 'n/a (multiple loops/recursion or frame samples)'
        fields = row['opcodes']['GetField'] + row['opcodes']['SetField']
        hashed = f'{e["string_bytes_hashed"] / fields:.3f}' if fields else 'n/a'
        call = '/'.join(str(e[k]) for k in ('lua_calls', 'native_calls', 'tail_calls'))
        table = '/'.join(str(e[k]) for k in ('table_raw_hits', 'table_misses', 'table_inserts', 'table_rehashes', 'table_resizes'))
        gc = '/'.join(str(e[k]) for k in ('gc_steps', 'gc_work_units', 'gc_cycles'))
        text.append(f'| {name} | {row["instructions"]} | {ratio} | {call} | {table} | {hashed} | {gc} |')
    text += ['', 'Ratios include setup and other loops; they are amortized whole-run counts, not isolated loop-body measurements. Numeric loops use the sum of all four loop-body counts, including the 2,000 outer iterations and 667,667 inner iterations.', '', '## Allocations by type', '', '| Workload | Object type | Count | Initial logical bytes |', '|---|---|---:|---:|']
    for name, row in records.items():
        for kind, (count, size) in sorted(row['allocations'].items()):
            text.append(f'| {name} | {kind} | {count} | {size} |')
    text += ['', '## Moss per frame and interaction', '', 'Arithmetic means over 50 measured frames. Full per-frame distributions remain in moss-*.json.', '', '| Interactions/frame | Metric | Per frame | Per interaction |', '|---:|---|---:|---:|']
    for name, row in records.items():
        if not name.startswith('moss-'):
            continue
        interactions = int(name.split('-')[1])
        frames = len(groups[name])
        metrics = {'VM instructions': row['instructions']}
        metrics.update({key: row['events'][key] for key in ('lua_to_host_calls', 'host_to_lua_calls', 'host_continuations', 'host_waits', 'gc_work_units', 'gc_steps', 'gc_cycles')})
        for kind, (count, size) in row['allocations'].items():
            metrics[f'{kind} allocations'] = count
            metrics[f'{kind} logical bytes'] = size
        for metric, n in metrics.items():
            text.append(f'| {interactions} | {metric} | {n / frames:.3f} | {n / frames / interactions:.6f} |')
    text += ['', '## Decisions and limits', '',
             'Keep this wave measurement-only. Rank candidate optimization work by executed frequency, then use the other lanes’ callgrind/cachegrind attribution to estimate cost; opcode frequency alone is not a speedup estimate. No runtime optimization or wall-time comparison is claimed.', '',
             'The strings workload may hit the configured object limit; consult runs.tsv and strings.stderr. Any failed workload is a prefix, and its percentages must not be treated as a complete-run distribution.', '']
    (args.results / 'COUNTERS.md').write_text('\n'.join(text))


if __name__ == '__main__':
    main()
