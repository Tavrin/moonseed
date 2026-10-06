#!/usr/bin/env python3
"""Capture frozen PUC 5.4.9 UTF-8 records or check a Moonseed binary."""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
from itertools import zip_longest
import os
from pathlib import Path
import re
import resource
import subprocess
import sys
import time

from utf8_generate import CORPUS, ROOT, SEED, driver, grouped_batches

PUC = Path(os.environ.get('MOONSEED_LUA54', ROOT / 'vendor' / 'lua-5.4.9' / 'src' / 'lua')).expanduser().resolve()
TARGET = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).expanduser().resolve()
MOONSEED = Path(os.environ.get('MOONSEED_RUN', TARGET / 'debug' / 'moonseed-run')).expanduser().resolve()
RESULTS = Path(os.environ.get('MOONSEED_UTF8_RESULTS', ROOT / 'results' / 'utf8'))
EXPECTED = CORPUS / 'expected'
LIMIT_BYTES = 2_000_000 * 1024
FULL_RECORD_LIMIT = 1024 * 1024
STORAGE_VERSION = 2


def sha(data):
    return hashlib.sha256(data).hexdigest()


def limit_memory():
    resource.setrlimit(resource.RLIMIT_AS, (LIMIT_BYTES, LIMIT_BYTES))


def run(executable, source, results):
    path = results / 'driver.lua'
    path.write_bytes(source)
    started = time.monotonic()
    try:
        result = subprocess.run([str(executable), path.name], cwd=results,
                                capture_output=True, timeout=60,
                                preexec_fn=limit_memory, check=False)
    except subprocess.TimeoutExpired as exc:
        result = subprocess.CompletedProcess([str(executable), path.name], 124,
                                             exc.stdout or b'', (exc.stderr or b'') + b'[timeout 60s]')
    except OSError as exc:
        result = subprocess.CompletedProcess([str(executable), path.name], 127,
                                             b'', str(exc).encode())
    return result, time.monotonic() - started


def typed_value(typ, payload):
    if typ == b'string':
        if not re.fullmatch(rb'(?:[0-9a-f]{2})*', payload):
            raise ValueError('invalid string hex')
        return bytes.fromhex(payload.decode('ascii'))
    if typ == b'number.integer' and re.fullmatch(rb'-?(?:0|[1-9][0-9]*)', payload):
        value = int(payload)
        if -(1 << 63) <= value < (1 << 63):
            return None
    elif typ == b'number.float' and re.fullmatch(rb'-?(?:0x[0-9a-f]+(?:\.[0-9a-f]*)?p[+-]?[0-9]+|inf|nan)', payload):
        return None
    elif typ == b'nil' and payload == b'-':
        return None
    elif typ == b'boolean' and payload in (b'true', b'false'):
        return None
    elif typ == b'function' and payload in (b'strict-iterator', b'lax-iterator'):
        return None
    raise ValueError(f'invalid typed value {typ!r}/{payload!r}')


def parse_record(line):
    if not line.endswith(b'\n') or b'\r' in line:
        raise ValueError('noncanonical line ending')
    fields = line[:-1].split(b'\t')
    if len(fields) < 5 or fields[1] not in (b'ok', b'error', b'compile'):
        raise ValueError('bad status/field count')
    if not re.fullmatch(rb'0|[1-9][0-9]*', fields[2]):
        raise ValueError('invalid result count')
    count = int(fields[2])
    if len(fields) != 5 + count * 2:
        raise ValueError('result count differs from field count')
    for i in range(count):
        typed_value(fields[3 + 2*i], fields[4 + 2*i])
    if fields[1] == b'ok':
        if fields[-2:] != [b'-', b'-']:
            raise ValueError('successful record has error')
    else:
        message = typed_value(fields[-2], fields[-1])
        if message is not None and re.search(rb"(?:^|[\s'\"(])/(?:[\w.-]+/)+[\w.-]+", message):
            raise ValueError('absolute path in error record')
    return fields[0].decode('ascii')


def line_map(data, ids):
    allowed = set(ids)
    rows, foreign = {}, []
    for line in data.splitlines(keepends=True):
        try:
            key = parse_record(line)
            if key not in allowed or key in rows:
                raise ValueError('duplicate or foreign id')
            rows[key] = line
        except (ValueError, UnicodeDecodeError) as exc:
            foreign.append(dict(line=line.decode('ascii', 'backslashreplace'), reason=str(exc)))
    return rows, foreign


def write_json(path, value):
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')


def reference_records(source, ids, results, name):
    result, seconds = run(PUC, source, results)
    rows, foreign = line_map(result.stdout, ids)
    if result.returncode or foreign or set(rows) != set(ids) or result.stderr:
        raise RuntimeError(f'{name}: PUC exit={result.returncode}, rows={len(rows)}/{len(ids)}, '
                           f'foreign={foreign[:2]!r}, stderr={result.stderr[:400]!r}')
    frozen = b''.join(rows[i] for i in ids)
    if result.stdout != frozen:
        raise RuntimeError(f'{name}: PUC record order differs from generated case order')
    return frozen, rows, seconds


def capture(results):
    EXPECTED.mkdir(parents=True, exist_ok=True)
    manifest = dict(protocol=1, storage_version=STORAGE_VERSION, seed=SEED,
                    oracle_sha256=sha(PUC.read_bytes()),
                    generator_sha256=sha((ROOT / 'tools' / 'utf8_generate.py').read_bytes()),
                    groups={}, storage={}, batches=[])
    timings = []
    records = collections.defaultdict(bytearray)
    digests = collections.defaultdict(list)
    for name, cases in grouped_batches():
        source = driver(cases)
        ids = [c['id'] for c in cases]
        frozen, _, seconds = reference_records(source, ids, results, name)
        group = cases[0]['group']
        records[group].extend(frozen)
        digests[group].append(f'{name}\t{len(cases)}\t{sha(frozen)}\n')
        manifest['groups'][group] = manifest['groups'].get(group, 0) + len(cases)
        manifest['batches'].append(dict(name=name, group=group, cases=len(cases),
                                        driver_sha256=sha(source), records_sha256=sha(frozen)))
        timings.append(dict(batch=name, seconds=seconds))
        if len(timings) % 100 == 0:
            print(f'capture {len(timings)} batches; latest {name}: {len(cases)} records', flush=True)
    # Publish one file per group only after every oracle batch has validated.
    for group, data in records.items():
        large = group in {'decoder2', 'random'} or len(data) > FULL_RECORD_LIMIT
        kind = 'digest' if large else 'records'
        filename = group + ('.sha256' if large else '.txt')
        stored = ''.join(digests[group]).encode('ascii') if large else bytes(data)
        (EXPECTED / filename).write_bytes(stored)
        manifest['storage'][group] = dict(kind=kind, file=filename,
                                         record_bytes=len(data), sha256=sha(stored))
    write_json(CORPUS / 'manifest.json', manifest)
    names = {item['file'] for item in manifest['storage'].values()}
    for path in EXPECTED.iterdir():
        if path.is_file() and path.name not in names:
            path.unlink()
    receipt = dict(cases=sum(manifest['groups'].values()), batches=len(timings),
                   storage=manifest['storage'],
                   max_process_seconds=max(t['seconds'] for t in timings),
                   records_sha256=sha(b''.join(bytes.fromhex(b['records_sha256']) for b in manifest['batches'])),
                   manifest_sha256=sha((CORPUS / 'manifest.json').read_bytes()), timings=timings)
    write_json(results / 'capture.json', receipt)
    print(f'captured {receipt["cases"]} cases in {receipt["batches"]} batches; '
          f'{len(names)} group files; max process {receipt["max_process_seconds"]:.3f}s', flush=True)
    return 0


def load_storage(manifest):
    """Validate consolidated files before running any compared engine."""
    if manifest.get('storage_version') != STORAGE_VERSION:
        raise RuntimeError('unknown frozen storage version; recapture deliberately')
    if set(manifest['storage']) != set(manifest['groups']):
        raise RuntimeError('storage groups differ from frozen group counts')
    records, digests = {}, {}
    for group, item in manifest['storage'].items():
        expected_name = group + ('.txt' if item['kind'] == 'records' else '.sha256')
        if item['kind'] not in {'records', 'digest'} or item['file'] != expected_name:
            raise RuntimeError(f'{group}: invalid storage description')
        data = (EXPECTED / item['file']).read_bytes()
        if sha(data) != item['sha256']:
            raise RuntimeError(f'{group}: frozen group file hash differs from manifest')
        if item['kind'] == 'records':
            lines = data.splitlines(keepends=True)
            if len(lines) != manifest['groups'][group] or len(data) != item['record_bytes']:
                raise RuntimeError(f'{group}: full record count/size differs from manifest')
            records[group] = lines
        else:
            rows = {}
            for line in data.splitlines(keepends=True):
                fields = line.rstrip(b'\n').split(b'\t')
                if (not line.endswith(b'\n') or len(fields) != 3
                        or not re.fullmatch(rb'[1-9][0-9]*', fields[1])
                        or not re.fullmatch(rb'[0-9a-f]{64}', fields[2])):
                    raise RuntimeError(f'{group}: invalid batch digest record')
                name = fields[0].decode('ascii')
                if name in rows:
                    raise RuntimeError(f'{group}: duplicate batch digest')
                rows[name] = (int(fields[1]), fields[2].decode('ascii'))
            expected = {b['name']: (b['cases'], b['records_sha256'])
                        for b in manifest['batches'] if b['group'] == group}
            if rows != expected or sum(n for n, _ in rows.values()) != manifest['groups'][group]:
                raise RuntimeError(f'{group}: batch digests differ from manifest')
            digests[group] = rows
    return records, digests


def shape(want, got):
    if got is None:
        return 'missing record'
    w, g = want.rstrip(b'\n').split(b'\t'), got.rstrip(b'\n').split(b'\t')
    if w[1] != g[1]:
        return f'status {w[1].decode()} -> {g[1].decode()}'
    if w[2] != g[2]:
        return 'result count'
    if w[-2:] != g[-2:]:
        return 'error type/bytes'
    return 'result type/bytes'


def check(results, report, diff_limit=3):
    manifest = json.loads((CORPUS / 'manifest.json').read_text())
    if manifest['protocol'] != 1:
        raise RuntimeError('unknown frozen protocol')
    stored_records, stored_digests = load_storage(manifest)
    consumed = collections.Counter()
    counts = collections.defaultdict(collections.Counter)
    shapes = collections.Counter()
    examples, failures, timings, oracle_timings = [], [], [], []
    digest_failures = []
    actual_dir = results / 'actual'
    actual_dir.mkdir(exist_ok=True)
    seen = []
    oracle_verified = False
    with (results / 'mismatches.jsonl').open('w') as mismatches:
        for name, cases in grouped_batches():
            source = driver(cases)
            ids = [c['id'] for c in cases]
            group = cases[0]['group']
            index = len(seen)
            if index >= len(manifest['batches']):
                raise RuntimeError(f'{name}: extra generated batch')
            item = manifest['batches'][index]
            generated = dict(name=name, group=group, cases=len(cases), driver_sha256=sha(source))
            if generated != {k: item[k] for k in generated}:
                raise RuntimeError(f'{name}: frozen source identity mismatch; recapture deliberately')
            seen.append(item)
            expected = None
            digest_group = group in stored_digests
            if not digest_group:
                offset = consumed[group]
                frozen = b''.join(stored_records[group][offset:offset + len(cases)])
                consumed[group] += len(cases)
                if sha(frozen) != item['records_sha256']:
                    raise RuntimeError(f'{name}: frozen batch hash differs from manifest')
                expected, bad_expected = line_map(frozen, ids)
                if bad_expected or set(expected) != set(ids):
                    raise RuntimeError(f'{name}: malformed frozen records')
            result, seconds = run(MOONSEED, source, results)
            (actual_dir / f'{name}.txt').write_bytes(result.stdout)
            actual, foreign = line_map(result.stdout, ids)
            timings.append(dict(batch=name, seconds=seconds, exit=result.returncode))
            valid = not (result.returncode or foreign or set(actual) != set(ids) or result.stderr)
            if not valid:
                failures.append(dict(batch=name, exit=result.returncode, records=len(actual),
                                     expected_records=len(ids), stderr=result.stderr.decode('utf-8', 'backslashreplace'),
                                     foreign=foreign[:3]))
            if digest_group:
                count, digest = stored_digests[group][name]
                if valid and len(actual) == count and sha(result.stdout) == digest:
                    counts[group]['cases'] += count
                    counts[group]['match'] += count
                    if len(seen) % 100 == 0:
                        print(f'check {len(seen)} batches; latest {name}: digest matched', flush=True)
                    continue
                digest_failures.append(name)
                if not oracle_verified:
                    if sha(PUC.read_bytes()) != manifest['oracle_sha256']:
                        raise RuntimeError('MOONSEED_LUA54 binary differs from frozen oracle; cannot diagnose digest failures')
                    oracle_verified = True
                frozen, expected, oracle_seconds = reference_records(source, ids, results, name)
                oracle_timings.append(dict(batch=name, seconds=oracle_seconds))
                if sha(frozen) != digest:
                    raise RuntimeError(f'{name}: rerun PUC bytes differ from frozen digest; refusing new expectations')
            differing = []
            for id_ in ids:
                want, got = expected[id_], actual.get(id_)
                counts[group]['cases'] += 1
                if want == got:
                    counts[group]['match'] += 1
                else:
                    counts[group]['missing' if got is None else 'mismatch'] += 1
                    category = shape(want, got)
                    shapes[category] += 1
                    row = dict(id=id_, batch=name, shape=category, expected=want.decode('ascii'),
                               actual=None if got is None else got.decode('ascii'))
                    if not digest_group or len(differing) < diff_limit:
                        mismatches.write(json.dumps(row, sort_keys=True) + '\n')
                    if len(differing) < diff_limit:
                        differing.append(row)
                    if len(examples) < 20:
                        examples.append(row)
            if digest_group:
                if not differing:
                    # A digest also detects changed record order or non-record bytes.
                    pairs = zip_longest(frozen.splitlines(keepends=True),
                                        result.stdout.splitlines(keepends=True))
                    for line_number, (want, got) in enumerate(pairs, 1):
                        if want != got:
                            row = dict(batch=name, line=line_number, shape='record order/extra output',
                                       expected=None if want is None else want.decode('ascii'),
                                       actual=None if got is None else got.decode('ascii', 'backslashreplace'))
                            differing.append(row)
                            mismatches.write(json.dumps(row, sort_keys=True) + '\n')
                            if len(differing) == diff_limit:
                                break
                print(f'digest mismatch {name}: first {len(differing)} differences (PUC rerun verified)', flush=True)
                for row in differing:
                    print(json.dumps(row, sort_keys=True), flush=True)
            if len(seen) % 100 == 0:
                print(f'check {len(seen)} batches; latest {name}: exit={result.returncode}', flush=True)
    if seen != manifest['batches'] or any(consumed[g] != len(lines) for g, lines in stored_records.items()):
        raise RuntimeError('frozen storage has extra batches/records')
    totals = collections.Counter()
    for count in counts.values():
        totals.update(count)
    summary = dict(totals=dict(totals), groups={k: dict(v) for k,v in counts.items()},
                   mismatch_shapes=dict(shapes), process_failures=failures,
                   digest_mismatches=digest_failures, oracle_reruns=oracle_timings, diff_limit=diff_limit,
                   oracle_path=str(PUC), storage=manifest['storage'],
                   moonseed_path=str(MOONSEED), moonseed_sha256=sha(MOONSEED.read_bytes()),
                   oracle_sha256=manifest['oracle_sha256'], manifest_sha256=sha((CORPUS / 'manifest.json').read_bytes()),
                   max_process_seconds=max(t['seconds'] for t in timings), timings=timings)
    write_json(results / 'baseline.json', summary)
    lines = ['# Frozen PUC 5.4.9 UTF-8 baseline', '',
             f'Moonseed: `{MOONSEED}`; SHA-256 `{summary["moonseed_sha256"]}`.',
             f'Captured oracle SHA-256: `{summary["oracle_sha256"]}`.',
             f'Frozen manifest SHA-256: `{summary["manifest_sha256"]}`.', '',
             'Both engines execute identical drivers using relative `driver.lua` and fixed `@utf8-case.lua` chunks.',
             'Records preserve counts, typed exact values, errors and any completed iterator prefix.', '',
             '| Group | Cases | Match | Mismatch | Missing |', '| --- | ---: | ---: | ---: | ---: |']
    for group, count in counts.items():
        lines.append(f'| {group} | {count["cases"]} | {count["match"]} | {count["mismatch"]} | {count["missing"]} |')
    lines += ['', f'Total: {totals["cases"]} cases; {totals["match"]} matches; '
              f'{totals["mismatch"]} mismatches; {totals["missing"]} missing.',
              f'Process/protocol failures: {len(failures)}. Maximum process time: {summary["max_process_seconds"]:.3f}s.', '',
              f'Exact mismatches: `mismatches.jsonl` (full-record groups: all; digest groups: first {diff_limit} per batch).',
              f'Digest-mismatching batches: {len(digest_failures)}; verified PUC reruns: {len(oracle_timings)}.',
              'Raw actual records: `actual/*.txt`; digest diagnostics are also printed to stdout.',
              'Process details, per-batch timings and identities: `baseline.json`.', '', '## Examples', '']
    for row in examples:
        lines += [f'### {row["id"]} ({row["shape"]})', '',
                  f'- expected: `{row["expected"].rstrip()}`',
                  f'- actual: `{(row["actual"] or "<missing>").rstrip()}`', '']
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text('\n'.join(lines) + '\n')
    print(f'baseline: {dict(totals)}, process failures={len(failures)}; report={report}', flush=True)
    return 2 if failures else int(totals['mismatch'] != 0 or bool(digest_failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--capture', action='store_true')
    mode.add_argument('--check', action='store_true')
    parser.add_argument('--results-dir', type=Path, default=RESULTS)
    parser.add_argument('--report', type=Path)
    parser.add_argument('--diff-limit', type=int, default=3,
                        help='first differing records printed/saved per digest batch (default: 3)')
    args = parser.parse_args()
    if not 1 <= args.diff_limit <= 256:
        parser.error('--diff-limit must be between 1 and 256')
    results = args.results_dir.resolve(); results.mkdir(parents=True, exist_ok=True)
    return capture(results) if args.capture else check(results, args.report or results / 'baseline.md', args.diff_limit)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, KeyError) as exc:
        print(f'utf8_corpus: {exc}', file=sys.stderr)
        sys.exit(2)
