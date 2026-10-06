#!/usr/bin/env python3
"""Exclusive instruction-address accounting, including signed control subtraction.

Callgrind call-edge costs are inclusive and must never be added to exclusive
instruction counts. Verify the parser against PROGRAM TOTALS on every input.
Address range labels describe the supplied binary, not arbitrary future builds.
"""
import argparse
from collections import defaultdict
import csv
import json
from pathlib import Path
import re

from callgrind_corpus import totals


def read_profile(path):
    names = {'fn': {}, 'ob': {}}
    fn = obj = ''
    counts = defaultdict(int)
    position = [0, 0]
    edge = False
    columns = 0
    for line in Path(path).read_text().splitlines():
        if line.startswith('positions: '):
            positions = line.split()[1:]
            if positions[0] != 'instr':
                raise ValueError('requires --dump-instr=yes')
            columns = len(positions)
        match = re.match(r'(c?fn|c?ob)=\((\d+)\)(?: (.*))?$', line)
        if match:
            kind, ident, value = match.groups()
            namespace = kind.removeprefix('c')
            if value is not None:
                names[namespace][ident] = value
            value = names[namespace][ident]
            if kind == 'fn':
                fn = value
            elif kind == 'ob':
                obj = value
            continue
        if line.startswith('calls='):
            edge = True
            continue
        if not line or line[0] not in '*+-0123456789':
            continue
        fields = line.split()
        for i in range(columns):
            value = fields[i]
            if value == '*':
                continue
            if value.startswith(('+', '-')):
                position[i] += int(value, 0)
            else:
                position[i] = int(value, 0)
        cost = int(fields[columns]) if len(fields) > columns else 0
        if not edge:
            counts[(obj, fn, position[0])] += cost
        edge = False
    expected = totals(Path(path))['Ir']
    actual = sum(counts.values())
    if actual != expected:
        raise ValueError(f'{path}: exclusive sum {actual} != total {expected}')
    return counts


def read_assembly(path):
    rows = {}
    function = source = ''
    for line in Path(path).read_text().splitlines():
        match = re.match(r'^[0-9a-f]+ <(.+)>:$', line)
        if match:
            function = match[1]
            source = ''
        elif re.match(r'^/.*:\d+(?: \(discriminator \d+\))?$', line):
            source = line
        else:
            match = re.match(r'^\s*([0-9a-f]+):\s+(.+)$', line)
            if match:
                rows[int(match[1], 16)] = {'function': function, 'source': source, 'asm': match[2]}
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profiles', type=Path, nargs=4, required=True,
                        metavar=('CALL_LOW', 'CALL_HIGH', 'CONTROL_LOW', 'CONTROL_HIGH'))
    parser.add_argument('--denominator', type=int, required=True)
    parser.add_argument('--assembly', type=Path, required=True)
    parser.add_argument('--binary', required=True, help='object path as recorded by callgrind')
    parser.add_argument('--ranges', type=Path, help='JSON [{start,end,bucket}] (hex half-open ranges)')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    profiles = [read_profile(p) for p in args.profiles]
    assembly = read_assembly(args.assembly)
    ranges = json.loads(args.ranges.read_text()) if args.ranges else []
    keys = set().union(*(set(p) for p in profiles))
    rows = []
    buckets = defaultdict(float)
    functions = defaultdict(float)
    for key in sorted(keys):
        obj, fn, address = key
        delta = profiles[1].get(key, 0) - profiles[0].get(key, 0) - profiles[3].get(key, 0) + profiles[2].get(key, 0)
        if delta == 0:
            continue
        matches = [r['bucket'] for r in ranges if obj == args.binary and int(r['start'], 16) <= address < int(r['end'], 16)]
        if len(matches) > 1:
            raise ValueError(f'overlapping ranges at {address:x}')
        bucket = matches[0] if matches else 'Residual: supporting opcodes / other functions / startup'
        cost = delta / args.denominator
        info = assembly.get(address, {}) if obj == args.binary else {}
        rows.append({'object': obj, 'function': fn, 'address': hex(address), 'delta_Ir': delta,
                     'Ir_per_call': cost, 'bucket': bucket, **{k: info.get(k, '') for k in ['source', 'asm']}})
        buckets[bucket] += cost
        functions[fn] += cost
    args.output.mkdir(parents=True, exist_ok=True)
    with (args.output / 'instructions.csv').open('w') as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    data = {'denominator': args.denominator, 'profiles': [str(p) for p in args.profiles],
            'total_net_Ir_per_call': sum(buckets.values()), 'buckets': dict(buckets),
            'functions': dict(sorted(functions.items(), key=lambda v: -v[1]))}
    (args.output / 'decomposition.json').write_text(json.dumps(data, indent=2) + '\n')
    print(json.dumps(data, indent=2))


if __name__ == '__main__':
    main()
