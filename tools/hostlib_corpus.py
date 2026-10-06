#!/usr/bin/env python3
"""Freeze or check PUC 5.4.9 host-library records in fresh, controlled fixtures."""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import sys
import tempfile
import time

from hostlib_generate import CORPUS, GROUPS, ROOT, driver, grouped_batches
from utf8_corpus import parse_record, typed_value

PUC = Path(os.environ.get('MOONSEED_LUA54', ROOT / 'vendor' / 'lua-5.4.9' / 'src' / 'lua')).expanduser().resolve()
TARGET = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).expanduser().resolve()
MOONSEED = Path(os.environ.get('MOONSEED_RUN', TARGET / 'debug' / 'moonseed-run')).expanduser().resolve()
RESULTS = Path(os.environ.get('MOONSEED_HOSTLIB_RESULTS', ROOT / 'results' / 'hostlib'))
FIXTURES = CORPUS / 'fixtures'
EXPECTED = CORPUS / 'expected'
ENV = dict(TZ='UTC', LC_ALL='C', PATH='/usr/bin:/bin', MS_A='alpha', MS_EMPTY='')
ARGV = ['alpha', '', 'omega']
PROFILES = CORPUS / 'runner_profiles.json'


def runner_profiles():
    table = json.loads(PROFILES.read_text())
    if (table['version'] != 1 or set(table['groups']) != set(GROUPS)
            or table['environment'] != ENV or table['argv'] != ARGV):
        raise RuntimeError('invalid runner profile table')
    for group, item in table['groups'].items():
        if (item['timezone'], item['stdin']) != GROUPS[group][1:]:
            raise RuntimeError('runner profile differs from frozen fixture: ' + group)
        if item['profile'] not in table['profiles']:
            raise RuntimeError('unknown runner profile: ' + group)
    return table


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')


def fixture_identity():
    return {str(p.relative_to(FIXTURES)): sha(p.read_bytes())
            for p in sorted(FIXTURES.rglob('*')) if p.is_file()}


def limit_memory():
    size = 2_000_000 * 1024
    resource.setrlimit(resource.RLIMIT_AS, (size, size))


def run(executable, source, group, results, engine_args=(), lua_argv=False, profiles=None):
    """No ambient env; all mutation (including libc /tmp) stays in this fixture."""
    started = time.monotonic()
    bwrap = shutil.which('bwrap')
    if not bwrap:
        raise RuntimeError('bwrap required: os.tmpname must not create files outside the fixture')
    with tempfile.TemporaryDirectory(prefix='fixture-', dir=results) as folder:
        cwd = Path(folder)
        shutil.copytree(FIXTURES, cwd, dirs_exist_ok=True)
        (cwd / '.host-tmp').mkdir()
        (cwd / 'driver.lua').write_bytes(source)
        fixture = profiles['groups'][group] if profiles else dict(timezone=GROUPS[group][1], stdin=GROUPS[group][2])
        env = dict(profiles['environment'] if profiles else ENV, TZ=fixture['timezone'])
        stdin = (cwd / 'stdin' / fixture['stdin']).read_bytes()
        command = [bwrap, '--die-with-parent', '--ro-bind', '/', '/',
                   '--dev-bind', '/dev/urandom', '/dev/urandom',
                   '--bind', str(cwd), str(cwd), '--bind', str(cwd / '.host-tmp'), '/tmp',
                   '--chdir', str(cwd), '--argv0', 'hostlib-engine', '--',
                   str(executable), *engine_args, 'driver.lua']
        if group == 'arg' and lua_argv:
            command += profiles['argv'] if profiles else ARGV
        try:
            result = subprocess.run(command, cwd=cwd, env=env, input=stdin,
                                    capture_output=True, timeout=60,
                                    preexec_fn=limit_memory, check=False)
        except subprocess.TimeoutExpired as exc:
            result = subprocess.CompletedProcess(command, 124, exc.stdout or b'',
                                                 (exc.stderr or b'') + b'[timeout 60s]')
        except OSError as exc:
            result = subprocess.CompletedProcess(command, 127, b'', str(exc).encode())
    return result, time.monotonic() - started


def records(data, ids):
    rows = {}
    for line in data.splitlines(keepends=True):
        key = parse_record(line)
        if key in rows or key not in ids:
            raise ValueError('duplicate/foreign record: ' + key)
        fields = line[:-1].split(b'\t')
        values = [(fields[i], fields[i+1]) for i in range(3, len(fields)-2, 2)]
        if fields[-2] != b'-':
            values.append((fields[-2], fields[-1]))
        for typ, payload in values:
            decoded = typed_value(typ, payload)
            if decoded is not None:
                if re.search(rb'(?:^|[\s\'"(])/(?:[\w.-]+/)*[\w.-]+', decoded):
                    raise ValueError('absolute path in record: ' + key)
                if re.search(rb'0x[0-9a-fA-F]{6,}', decoded):
                    raise ValueError('possible address in record: ' + key)
        rows[key] = line
    if list(rows) != ids:
        raise ValueError(f'missing/reordered records: {len(rows)}/{len(ids)}')
    return rows


def checked_run(executable, source, group, ids, results, engine_args=(), lua_argv=False):
    result, seconds = run(executable, source, group, results, engine_args, lua_argv)
    if result.returncode or result.stderr:
        raise RuntimeError(f'engine exit={result.returncode}; stderr={result.stderr[:600]!r}')
    rows = records(result.stdout, ids)
    return result.stdout, rows, seconds


def capture(results):
    manifest = dict(protocol=1, storage_version=1,
                    oracle_sha256=sha(PUC.read_bytes()),
                    generator_sha256=sha((ROOT / 'tools' / 'hostlib_generate.py').read_bytes()),
                    fixtures=fixture_identity(), environment=ENV, argv=ARGV,
                    groups={}, storage={}, batches=[])
    full = collections.defaultdict(bytearray)
    timings = []
    for name, cases in grouped_batches():
        group = cases[0]['group']
        source = driver(cases)
        ids = [c['id'] for c in cases]
        try:
            data, _, seconds = checked_run(PUC, source, group, ids, results, lua_argv=True)
        except (RuntimeError, ValueError) as exc:
            # Preserve the failed source without publishing any partial expectations.
            (results / 'failed-driver.lua').write_bytes(source)
            raise RuntimeError(f'{name}: {exc}') from exc
        full[group].extend(data)
        manifest['groups'][group] = manifest['groups'].get(group, 0) + len(ids)
        manifest['batches'].append(dict(name=name, group=group, cases=len(ids),
                                       driver_sha256=sha(source), records_sha256=sha(data)))
        timings.append(dict(batch=name, seconds=seconds))
    EXPECTED.mkdir(parents=True, exist_ok=True)
    for group, data in full.items():
        filename = group + '.txt'
        (EXPECTED / filename).write_bytes(data)
        manifest['storage'][group] = dict(file=filename, bytes=len(data), sha256=sha(data),
                                         acceptance=GROUPS[group][0], timezone=GROUPS[group][1],
                                         stdin=GROUPS[group][2])
    write_json(CORPUS / 'manifest.json', manifest)
    write_json(results / 'capture.json', dict(cases=sum(manifest['groups'].values()),
               batches=len(timings), manifest_sha256=sha((CORPUS / 'manifest.json').read_bytes()),
               max_process_seconds=max(t['seconds'] for t in timings), timings=timings))
    print(f'captured {sum(manifest["groups"].values())} records in {len(timings)} batches', flush=True)
    return 0


def check(results, report, executable, engine_args=(), lua_argv=False, profiles=None, selection="all"):
    manifest = json.loads((CORPUS / 'manifest.json').read_text())
    if manifest['protocol'] != 1 or manifest['storage_version'] != 1:
        raise RuntimeError('unknown protocol/storage version')
    if (manifest['fixtures'] != fixture_identity() or manifest['environment'] != ENV
            or manifest['argv'] != ARGV
            or manifest['generator_sha256'] != sha((ROOT / 'tools' / 'hostlib_generate.py').read_bytes())):
        raise RuntimeError('fixture/environment/generator identity changed; recapture deliberately')
    batches = list(grouped_batches())
    if len(batches) != len(manifest['batches']):
        raise RuntimeError('frozen/generated batch counts differ')
    stored = {}
    for group, item in manifest['storage'].items():
        if item['file'] != group + '.txt':
            raise RuntimeError('invalid storage filename')
        data = (EXPECTED / item['file']).read_bytes()
        if sha(data) != item['sha256'] or len(data) != item['bytes']:
            raise RuntimeError('frozen storage corrupt: ' + group)
        stored[group] = data.splitlines(keepends=True)
        if len(stored[group]) != manifest['groups'][group]:
            raise RuntimeError('frozen group count differs: ' + group)
    # Validate every frozen batch before launching a compared engine.
    offsets = collections.Counter()
    prepared = []
    for (name, cases), item in zip(batches, manifest['batches']):
        group = cases[0]['group']; source = driver(cases); ids = [c['id'] for c in cases]
        if dict(name=name, group=group, cases=len(ids), driver_sha256=sha(source)) != {k:item[k] for k in ('name','group','cases','driver_sha256')}:
            raise RuntimeError('frozen driver identity differs: ' + name)
        start = offsets[group]; offsets[group] += len(ids)
        frozen = b''.join(stored[group][start:offsets[group]])
        if sha(frozen) != item['records_sha256']:
            raise RuntimeError('frozen batch hash differs: ' + name)
        prepared.append((name, group, source, ids, records(frozen, ids)))
    if dict(offsets) != manifest['groups'] or set(stored) != set(offsets):
        raise RuntimeError('extra frozen groups/records')
    runs = []
    for name, group, source, ids, expected in prepared:
        if profiles is None:
            runs.append((name, group, source, ids, expected, list(engine_args), lua_argv))
            continue
        item = profiles['groups'][group]
        if selection in ('all', 'native'):
            options = profiles['profiles'][item['profile']] + list(engine_args)
            runs.append((name, group, source, ids, expected, options, item['lua_argv'] or lua_argv))
        if selection in ('all', 'vfs') and item['vfs']:
            options = profiles['profiles']['vfs'] + list(engine_args)
            runs.append((name + '-vfs', group + '@vfs', source, ids, expected, options, False))
    actual_dir = results / 'actual'; actual_dir.mkdir(exist_ok=True)
    counts = collections.defaultdict(collections.Counter)
    failures, timings = [], []
    with (results / 'mismatches.jsonl').open('w') as out:
        for name, group, source, ids, expected, options, argv in runs:
            result, seconds = run(executable, source, group.split("@")[0], results, options, argv, profiles)
            (actual_dir / (name + '.txt')).write_bytes(result.stdout)
            timings.append(dict(batch=name, seconds=seconds, exit=result.returncode, engine_args=options, lua_argv=argv))
            try:
                actual = records(result.stdout, ids)
                if result.returncode or result.stderr:
                    raise ValueError('nonzero exit or stderr')
            except ValueError as exc:
                actual = {}
                failures.append(dict(batch=name, exit=result.returncode, error=str(exc),
                                     stderr=result.stderr.decode('utf-8','backslashreplace')))
            for key in ids:
                want, got = expected[key], actual.get(key)
                counts[group]['cases'] += 1
                category = 'match' if want == got else ('missing' if got is None else 'mismatch')
                counts[group][category] += 1
                if category != 'match':
                    out.write(json.dumps(dict(id=key, batch=name, category=category,
                              expected=want.decode('ascii'), actual=got.decode('ascii') if got else None), sort_keys=True)+'\n')
    totals = collections.Counter()
    for count in counts.values():
        totals.update(count)
    summary = dict(totals=dict(totals), groups={g:dict(c) for g,c in counts.items()},
                   process_failures=failures, timings=timings,
                   executable=str(executable), executable_sha256=sha(executable.read_bytes()),
                   oracle_sha256=manifest['oracle_sha256'],
                   manifest_sha256=sha((CORPUS / 'manifest.json').read_bytes()),
                   engine_args=list(engine_args), lua_argv=lua_argv,
                   runner_profiles_sha256=sha(PROFILES.read_bytes()) if profiles else None,
                   profile_selection=selection if profiles else "oracle")
    write_json(results / 'baseline.json', summary)
    lines = ['# Host-library frozen corpus check', '',
             f'Engine: `{executable}`; SHA-256 `{summary["executable_sha256"]}`.',
             f'Manifest SHA-256: `{summary["manifest_sha256"]}`.', '',
             '| Group | Cases | Match | Mismatch | Missing |', '| --- | ---: | ---: | ---: | ---: |']
    for group, count in counts.items():
        lines.append(f'| {group} | {count["cases"]} | {count["match"]} | {count["mismatch"]} | {count["missing"]} |')
    lines += ['', f'Total: {dict(totals)}. Process/protocol failures: {len(failures)}.',
              'Exact differences: `mismatches.jsonl`; raw outputs: `actual/*.txt`.',
              'Fresh fixture copies are deleted after every batch. Times and identities are in `baseline.json`.', '']
    report.parent.mkdir(parents=True, exist_ok=True); report.write_text('\n'.join(lines))
    print(f'check: {dict(totals)}; process/protocol failures={len(failures)}; report={report}', flush=True)
    return 2 if failures else int(totals['mismatch'] != 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--capture', action='store_true')
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--self-check', action='store_true')
    parser.add_argument('--results-dir', type=Path, default=RESULTS)
    parser.add_argument('--report', type=Path)
    parser.add_argument('--engine-arg', action='append', default=[], help='Moonseed runner options before FILE (repeatable)')
    parser.add_argument('--profile', choices=('all', 'native', 'vfs'), default='all',
                        help='automatic Moonseed profiles: all groups native plus IO VFS (default)')
    parser.add_argument('--lua-argv', action='store_true', help='pass alpha, empty string, omega after FILE in arg group; requires runner support')
    args = parser.parse_args()
    results = args.results_dir.expanduser().resolve(); results.mkdir(parents=True, exist_ok=True)
    try:
        if args.capture:
            return capture(results)
        executable = PUC if args.self_check else MOONSEED
        if args.self_check and sha(PUC.read_bytes()) != json.loads((CORPUS / 'manifest.json').read_text())['oracle_sha256']:
            raise RuntimeError('self-check oracle differs from frozen executable')
        return check(results, args.report or results / 'baseline.md', executable,
                     () if args.self_check else args.engine_arg, args.self_check or args.lua_argv,
                     None if args.self_check else runner_profiles(), args.profile)
    except (OSError, RuntimeError, ValueError, KeyError) as exc:
        print('tool failure: ' + str(exc), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
