#!/usr/bin/env python3
"""Validate the classified release ledger, optionally enforcing a fresh run."""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
SHA = '7d971845f545ffc09fbb3128a86b2c6524161c70d0fdf0154a16e8c00c343fca'
ACCEPTED = {
    'HOST/CAPABILITY DIFFERENCE', 'REFERENCE C-API TEST ONLY',
    'PUC INTERNAL/PRIVATE DETAIL', 'PUC BINARY-FORMAT DETAIL',
    'IMPLEMENTATION-DEFINED IDENTITY', 'RESOURCE/POLICY DIFFERENCE',
    'STANDALONE-CLI DIFFERENCE', 'UNSUPPORTED LUA C MODULE ABI',
}


def rows(data):
    if (data['suite'] != 'lua-5.4.9-tests' or data['archive_sha256_verified'] != SHA
            or data['url'] != 'https://www.lua.org/tests/lua-5.4.9-tests.tar.gz'
            or data['fuel_limit'] != 200_000_000):
        raise ValueError('suite identity/hash/fuel policy differs')
    result = {r['file']: r for r in data['files']}
    if len(result) != 33 or len(result) != len(data['files']):
        raise ValueError('expected 33 distinct suite files')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('actual', nargs='?', type=Path)
    args = parser.parse_args()
    data = json.loads((ROOT / 'tests/compat/lua54-0.1.0.json').read_text())
    baseline = rows(data)
    if data['revisions'] != dict(snapshot=25, bytecode=14, tables=4, fuel=7, gc=12, chunk=2):
        raise ValueError('release revisions changed; review the baseline deliberately')
    for name, row in baseline.items():
        if not row['compiled']:
            raise ValueError(f'{name}: not compiled')
        if row['outcome'] == 'PASS':
            if (row['status'], row['classification'], row['bucket']) != ('PASS', 'PASS', 'PASS'):
                raise ValueError(f'{name}: inconsistent PASS')
        elif (row['outcome'] != 'LUA_ERROR' or row['status'] != 'FAIL'
              or row['classification'] not in ACCEPTED
              or row['bucket'] != row['classification'] or not row['rationale'].strip()):
            raise ValueError(f'{name}: unclassified or blocking release discrepancy')
    if args.actual:
        actual = rows(json.loads(args.actual.read_text()))
        if actual.keys() != baseline.keys():
            raise ValueError('suite file set changed')
        # Fuel, printed identity and missing-global observations can vary. Compare
        # execution outcomes and exact first error, so a changed frontier is reviewed.
        for name, want in baseline.items():
            for key in ('compiled', 'outcome', 'blocker', 'detail'):
                if want[key] != actual[name][key]:
                    raise ValueError(f'{name}: {key}: {want[key]!r} -> {actual[name][key]!r}')
    print('release baseline: 33 classified files; fresh run matches' if args.actual
          else 'release baseline: 33 classified files; zero unknown/blocking classes')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as exc:
        sys.exit(f'release baseline FAIL: {exc}')
