#!/usr/bin/env python3
"""Build a frozen profile with resource caps and a provenance sidecar."""
import argparse
import json
import hashlib
import os
from pathlib import Path
import subprocess
import time
import bench_env

TARGET = os.environ.get('CARGO_TARGET_DIR', str(Path(__file__).resolve().parents[1] / 'target'))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--profile', choices=['release', 'bench-stable', 'bench-native'], default='bench-stable')
    p.add_argument('--lto', choices=['off', 'thin', 'fat'])
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    env = dict(os.environ, CARGO_TARGET_DIR=TARGET, CARGO_BUILD_JOBS='8',
               RUSTFLAGS='-C target-cpu=native' if a.profile == 'bench-native' else '')
    env.pop('CARGO_ENCODED_RUSTFLAGS', None)
    if a.lto is not None:
        env['CARGO_PROFILE_' + a.profile.upper().replace('-', '_') + '_LTO'] = a.lto
    command = ['cargo', 'build', '--profile', a.profile, '-p', 'moonseed-bench', '--bin', 'moonseed-run']
    start = time.monotonic()
    result = subprocess.run(['bash', '-c', 'ulimit -v 8000000; exec timeout 1800 "$@"', 'build', *command], env=env, capture_output=True, text=True)
    a.output.parent.mkdir(parents=True, exist_ok=True)
    a.output.with_suffix('.log').write_text(result.stdout + result.stderr)
    os.environ.update({k: v for k, v in env.items() if k.startswith(('RUSTFLAGS', 'CARGO_PROFILE_'))})
    binary = Path(TARGET) / a.profile / 'moonseed-run'
    a.output.write_text(json.dumps({'command': command, 'returncode': result.returncode,
                                   'binary': str(binary),
                                   'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest() if result.returncode == 0 else None,
                                   'wall_seconds': time.monotonic() - start,
                                   'environment': bench_env.capture(4, a.profile)}, indent=2) + '\n')
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
