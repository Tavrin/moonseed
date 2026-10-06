#!/usr/bin/env python3
"""Bound CI processes and their children; also used as Cargo's test runner."""
import argparse
import json
import os
import platform
import signal
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--runtime', action='store_true')
    mode.add_argument('--test-harness', action='store_true')
    parser.add_argument('--seconds', type=int)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command and command[0] == '--':
        command = command[1:]
    if not command:
        parser.error('a command is required')
    seconds = args.seconds if args.seconds is not None else (60 if args.runtime else 1800)
    if seconds < 1:
        parser.error('seconds must be positive')
    env = dict(os.environ, CARGO_BUILD_JOBS='8', RUST_TEST_THREADS='8')
    # Rust test harnesses keep Cargo caps; standalone oracle programs use --runtime.
    if command[0] == 'cargo' and 'test' in command:
        runner = [sys.executable, os.path.abspath(__file__), '--test-harness', '--']
        # CLI TOML arrays preserve spaces in Python/script paths on Windows.
        config = 'target.' + json.dumps('cfg(not(target_arch = "wasm32"))')
        config += '.runner=' + json.dumps(runner)
        index = command.index('test') + 1
        command[index:index] = ['--config', config]

    def limits():
        import resource
        def tighten(kind, requested):
            soft, hard = resource.getrlimit(kind)
            bound = min([requested] + [v for v in (soft, hard)
                                       if v != resource.RLIM_INFINITY])
            resource.setrlimit(kind, (bound, bound))

        tighten(resource.RLIMIT_CORE, 0)
        tighten(resource.RLIMIT_CPU, seconds)
        if platform.system() == 'Linux':
            size = (2_000_000 if args.runtime else 8_000_000) * 1024
            tighten(resource.RLIMIT_AS, size)

    proc = subprocess.Popen(command, env=env, start_new_session=os.name != 'nt' and not args.test_harness,
                            preexec_fn=limits if os.name != 'nt' else None)
    class Stopped(Exception):
        def __init__(self, signum):
            self.signum = signum

    def stop(signum, _frame):
        raise Stopped(signum)

    signal.signal(signal.SIGTERM, stop)
    try:
        code = proc.wait(timeout=seconds)
    except (subprocess.TimeoutExpired, KeyboardInterrupt, Stopped) as exc:
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(proc.pid), '/T', '/F'], timeout=10,
                           check=False, stdout=subprocess.DEVNULL)
        else:
            try:
                if args.test_harness:
                    # Stay in Cargo's process group, so the parent deadline also
                    # terminates the harness. Never signal the parent's group here.
                    proc.kill()
                else:
                    os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        proc.wait(timeout=10)
        if isinstance(exc, subprocess.TimeoutExpired):
            print(f'FAIL: process deadline {seconds}s: {command}', file=sys.stderr)
            return 124
        return 128 + (exc.signum if isinstance(exc, Stopped) else signal.SIGINT)
    return code if code >= 0 else 128 - code


if __name__ == '__main__':
    sys.exit(main())
