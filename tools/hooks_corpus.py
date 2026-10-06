#!/usr/bin/env python3
"""Capture pinned PUC hook traces or compare the same driver against Moonseed."""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import subprocess
import sys

from hooks_generate import HOOKS, ROOT, all_cases, driver, grouped_batches

PUC = Path(os.environ.get('MOONSEED_LUA54', ROOT / 'vendor' / 'lua-5.4.9' / 'src' / 'lua')).resolve()
TARGET = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).resolve()
MOONSEED = Path(os.environ.get('MOONSEED_RUN', TARGET / 'debug' / 'moonseed-run')).resolve()
RESULTS = Path(os.environ.get('MOONSEED_HOOK_RESULTS', ROOT / 'results' / 'hooks')).resolve()
HARNESS = Path(os.environ.get('MOONSEED_LUA54_HOOKS', RESULTS / 'lua54-hooks')).resolve()
EXPECTED = HOOKS / 'expected'


def limit_memory():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_AS, (2_000_000 * 1024, 2_000_000 * 1024))


def run(executable, source, *flags):
    path = RESULTS / 'driver.lua'
    path.write_bytes(source)
    try:
        return subprocess.run([str(executable), *flags, path.name], cwd=RESULTS,
                              capture_output=True, timeout=60, preexec_fn=limit_memory,
                              check=False)
    except subprocess.TimeoutExpired as exc:
        return subprocess.CompletedProcess([str(executable), path.name], 124,
                                           exc.stdout or b'', (exc.stderr or b'')+b'[timeout 60s]')


def parse(data, cases):
    """Validate ordered, contiguous records and exactly one final status per case."""
    ids = [c['id'] for c in cases]
    rows = collections.defaultdict(list)
    index = 0
    for line in data.splitlines(keepends=True):
        fields = line.split(b'\t')
        if len(fields) != 4 or not line.endswith(b'\n'):
            raise RuntimeError(f'foreign/malformed stdout: {line[:200]!r}')
        key, sequence, kind, payload = fields
        if index >= len(ids) or key != ids[index].encode('ascii'):
            raise RuntimeError(f'out-of-order/unknown record: {line[:200]!r}')
        if sequence != str(len(rows[ids[index]])+1).encode() or kind not in [b'H',b'O',b'S',b'P']:
            raise RuntimeError(f'bad sequence or kind: {line[:200]!r}')
        payload = payload[:-1]
        if not re.fullmatch(rb'(?:[0-9a-f]{2})*', payload):
            raise RuntimeError(f'bad hex: {line[:200]!r}')
        rows[ids[index]].append(line)
        if kind == b'P':
            if not cases[index]['oracle_only'] or cases[index]['group'] != 'external_unsupported':
                raise RuntimeError('process outcome outside unsupported C-hook probes')
            if not bytes.fromhex(payload.decode('ascii')).startswith(b'signal:'):
                raise RuntimeError('invalid process outcome')
            index += 1
        elif kind == b'S':
            decoded = bytes.fromhex(payload.decode('ascii'))
            if decoded.split(b'|')[0] not in [b'ok',b'error',b'compile']:
                raise RuntimeError('invalid final status')
            index += 1
    if index != len(ids):
        raise RuntimeError(f'incomplete records: {index}/{len(ids)} final statuses')
    return {key: b''.join(value) for key, value in rows.items()}


def validate_frozen(data, cases):
    rows = parse(data, cases)
    for c in cases:
        for line in rows[c['id']].splitlines():
            payload = bytes.fromhex(line.split(b'\t')[3].decode('ascii'))
            # String fields are themselves hex, so inspect both encoding layers.
            strings = [payload]
            for match in re.finditer(rb'string:([0-9a-f]*)', payload):
                strings.append(bytes.fromhex(match[1].decode('ascii')))
            if any(re.search(rb'(?:^|[\s\'"(])/(?:[\w.-]+/)+[\w.-]+', s) for s in strings):
                raise RuntimeError(f'absolute path in oracle: {c["id"]}')
            if any(re.search(rb'0x[0-9a-fA-F]{6,}', s) for s in strings):
                raise RuntimeError(f'pointer address in oracle: {c["id"]}')
    return rows


def run_group(executable, cases, *flags):
    if cases[0]['group'] != 'external_unsupported':
        return run(executable, driver(cases), *flags)
    records = []
    for c in cases:
        result = run(executable, driver([c]), *flags)
        if result.returncode < 0:
            name = signal.Signals(-result.returncode).name
            payload = ('signal:'+name+'|stdout:'+result.stdout.hex()+'|stderr:'+result.stderr.hex()).encode('ascii')
            records.append(c['id'].encode('ascii')+b'\t1\tP\t'+payload.hex().encode('ascii')+b'\n')
        elif result.returncode or result.stderr:
            return result
        else:
            records.append(result.stdout)
    return subprocess.CompletedProcess([str(executable), 'driver.lua'], 0, b''.join(records), b'')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def capture(batches):
    pending = {}
    for group, cases in batches:
        # Direct embedding omits the Lua CLI's private pmain C frame.
        executable = HARNESS
        result = run_group(executable, cases)
        if result.returncode or result.stderr:
            raise RuntimeError(f'{group}: exit={result.returncode}, stderr={result.stderr[:1000]!r}')
        validate_frozen(result.stdout, cases)
        pending[group] = result.stdout
        print(f'capture {group}: {len(cases)} cases, {len(result.stdout.splitlines())} records, sha256={hashlib.sha256(result.stdout).hexdigest()}')
    EXPECTED.mkdir(parents=True, exist_ok=True)
    for group, data in pending.items():
        (EXPECTED / (group+'.txt')).write_bytes(data)
    for stale in EXPECTED.glob('*.txt'):
        if stale.stem not in pending:
            stale.unlink()
    manifest = dict(protocol=1, oracle='Lua 5.4.9 without LUA_COMPAT_5_3',
                    puc_sha256=digest(PUC), harness_sha256=digest(HARNESS),
                    harness_source_sha256=digest(ROOT / 'tools' / 'lua54_hook_harness.c'),
                    generator_sha256=digest(ROOT / 'tools' / 'hooks_generate.py'),
                    runner_sha256=digest(ROOT / 'tools' / 'hooks_corpus.py'),
                    groups={g: dict(cases=len(cs), records=len(pending[g].splitlines()),
                                    sha256=hashlib.sha256(pending[g]).hexdigest(),
                                    acceptance=not cs[0]['oracle_only']) for g, cs in batches})
    (HOOKS / 'manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True)+'\n')


def validate_manifest(batches):
    manifest = json.loads((HOOKS / 'manifest.json').read_text())
    for key, path in [('generator_sha256', ROOT / 'tools' / 'hooks_generate.py'),
                      ('runner_sha256', ROOT / 'tools' / 'hooks_corpus.py'),
                      ('harness_source_sha256', ROOT / 'tools' / 'lua54_hook_harness.c')]:
        if manifest[key] != digest(path):
            raise RuntimeError(f'{key}: source changed; recapture deliberately')
    if manifest['protocol'] != 1 or set(manifest['groups']) != {g for g, _ in batches}:
        raise RuntimeError('manifest protocol/groups do not match case definitions')
    for group, cases in batches:
        item = manifest['groups'][group]
        path = EXPECTED / (group+'.txt')
        if item['cases'] != len(cases) or item['sha256'] != digest(path):
            raise RuntimeError(f'{group}: frozen size/hash differs from manifest')
        if item['records'] != len(path.read_bytes().splitlines()) or any(item['acceptance'] == c['oracle_only'] for c in cases):
            raise RuntimeError(f'{group}: frozen record count/acceptance differs from manifest')


def check(batches, report):
    validate_manifest(batches)
    if not MOONSEED.is_file():
        raise RuntimeError(f'build moonseed-run first: {MOONSEED}')
    counts, failures = {}, []
    for group, cases in batches:
        frozen = (EXPECTED / (group+'.txt')).read_bytes()
        expected = validate_frozen(frozen, cases)
        # External cases use the C harness's natives, which moonseed-run
        # provides through the public host-hook API.
        flags = ('--hooks',) if cases[0]['group'].startswith('external') else ()
        result = run_group(MOONSEED, cases, *flags)
        (RESULTS / (group+'.actual.txt')).write_bytes(result.stdout)
        (RESULTS / (group+'.stderr.txt')).write_bytes(result.stderr)
        try:
            actual = parse(result.stdout, cases)
        except RuntimeError as exc:
            failures.append(dict(group=group, exit=result.returncode, reason=str(exc), stderr=result.stderr.decode('utf-8','backslashreplace')[:1000]))
            actual = {}
        else:
            if result.returncode or result.stderr:
                failures.append(dict(group=group, exit=result.returncode, reason='process failure or stderr', stderr=result.stderr.decode('utf-8','backslashreplace')[:1000]))
        matches = sum(expected[c['id']] == actual.get(c['id']) for c in cases)
        counts[group] = dict(cases=len(cases), match=matches,
                             mismatch=sum(c['id'] in actual and expected[c['id']] != actual[c['id']] for c in cases),
                             missing=sum(c['id'] not in actual for c in cases), acceptance=not cases[0]['oracle_only'])
        print(f'check {group}: {len(cases)} cases, {matches} matches, exit={result.returncode}')
    total = sum(c['cases'] for c in counts.values())
    matched = sum(c['match'] for c in counts.values())
    acceptance = [c for c in counts.values() if c['acceptance']]
    passed = all(c['match'] == c['cases'] for c in acceptance) and not any(counts[f['group']]['acceptance'] for f in failures)
    result = dict(total_cases=total, matches=matched, acceptance_passed=passed,
                  groups=counts, process_failures=failures, moonseed_sha256=digest(MOONSEED))
    report.with_suffix('.json').write_text(json.dumps(result, indent=2, sort_keys=True)+'\n')
    lines = ['# Frozen PUC 5.4.9 hook baseline', '',
             f'{matched}/{total} cases match byte-exact records. Acceptance: {"PASS" if passed else "FAIL"}.', '',
             f'Moonseed binary SHA-256: `{digest(MOONSEED)}`.',
             'Baseline is counts only. Raw actual records/stderr are retained beside this report.',
             'Count positions and external count-yield positions are oracle-only; bytecode instruction positions differ.', '',
             '| Group | Cases | Match | Mismatch | Missing | Acceptance |',
             '| --- | ---: | ---: | ---: | ---: | --- |']
    for group, c in counts.items():
        lines.append(f'| {group} | {c["cases"]} | {c["match"]} | {c["mismatch"]} | {c["missing"]} | {"yes" if c["acceptance"] else "oracle only"} |')
    lines += ['', f'Process/protocol failures: {len(failures)}.', '']
    for f in failures:
        lines.append(f'- `{f["group"]}`: exit {f["exit"]}; {f["reason"]}.')
    report.write_text('\n'.join(lines)+'\n')
    return passed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--capture', action='store_true')
    mode.add_argument('--check', action='store_true')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    RESULTS.mkdir(parents=True, exist_ok=True)
    batches = list(grouped_batches())
    (RESULTS / 'cases.json').write_text(json.dumps(all_cases(), indent=2)+'\n')
    if args.capture:
        capture(batches)
        return 0
    report = (args.report or RESULTS / 'baseline.md').resolve()
    report.parent.mkdir(parents=True, exist_ok=True)
    return 0 if check(batches, report) else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, KeyError) as exc:
        print(f'hooks_corpus: {exc}', file=sys.stderr)
        sys.exit(2)
