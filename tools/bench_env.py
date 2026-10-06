#!/usr/bin/env python3
"""Provenance shared by the wall-time and instruction-count collectors."""
import os
import hashlib
import tomllib
from pathlib import Path
import platform
import subprocess


def probe(*args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=30)
    return (result.stdout + result.stderr).strip()


def capture(core, profile):
    governor = Path(f'/sys/devices/system/cpu/cpu{core}/cpufreq/scaling_governor')
    root = Path(__file__).resolve().parents[1]
    manifest = tomllib.loads((root / 'Cargo.toml').read_text())
    source = hashlib.sha256()
    for path in sorted((root / 'crates').rglob('*.rs')):
        source.update(str(path.relative_to(root)).encode() + b'\0' + path.read_bytes())
    load = os.getloadavg()
    threshold = float(os.environ.get('BENCH_MAX_LOAD', '1.0'))
    return {
        'profile_definitions': manifest.get('profile', {}),
        'rust_source_sha256': source.hexdigest(),
        'cargo_lock_sha256': hashlib.sha256((root / 'Cargo.lock').read_bytes()).hexdigest(),
        'rustc_llvm': probe('rustc', '-vV'), 'cc': probe('cc', '--version'),
        'puc_build_flags': os.environ.get('PUC_BUILD_FLAGS', 'unknown (not inferred from binary)'),
        'cpu': probe('lscpu'), 'kernel': platform.release(),
        'governor': governor.read_text().strip() if governor.exists() else None,
        'allowed_affinity': sorted(os.sched_getaffinity(0)), 'pinned_core': core,
        'load_average': load, 'load_threshold': threshold, 'profile': profile,
        'rustflags': os.environ.get('RUSTFLAGS', ''),
        'cargo_profile_overrides': {k: v for k, v in os.environ.items() if k.startswith('CARGO_PROFILE_')},
        'git_head': probe('git', 'rev-parse', 'HEAD'),
        'git_diff': probe('git', 'diff', '--stat'),
        'not_acceptance_evidence': load[0] > threshold or os.environ.get('BENCH_DIAGNOSTIC', '1') == '1',
        'wall_time_policy': 'diagnostic by default; non-performance machine on 2026-10-02',
    }
