#!/usr/bin/env bash
# Validation only. Never publishes, tags, or changes Git state.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
mode=${1:---all}
case "$mode" in --all|--fast|--msrv|--native|--wasm) ;; *) echo 'usage: tools/release_check.sh [--all|--fast|--msrv|--native|--wasm]' >&2; exit 2 ;; esac
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
export CARGO_TARGET_DIR CARGO_BUILD_JOBS=8 PYTHONDONTWRITEBYTECODE=1
export RUSTUP_TOOLCHAIN=${RELEASE_TOOLCHAIN:-stable}
export MOONSEED_RELEASE_RESULTS=${MOONSEED_RELEASE_RESULTS:-$root/results/release-check}
mkdir -p "$MOONSEED_RELEASE_RESULTS"
MOONSEED_RELEASE_RESULTS=$(cd "$MOONSEED_RELEASE_RESULTS" && pwd)
export MOONSEED_RELEASE_RESULTS
summary=$MOONSEED_RELEASE_RESULTS/summary.tsv
printf 'step\tstatus\texit\n' > "$summary"
active=setup
scratch=''
finish() {
  code=$?
  if [[ $code != 0 && $(tail -n 1 "$summary") != *$'\tFAIL\t'* ]]; then
    printf '%s\tFAIL\t%s\n' "$active" "$code" >> "$summary"
  fi
  [[ -z $scratch ]] || rm -rf "$scratch"
  cat "$summary"
  if [[ $code == 0 ]]; then
    echo 'PASS: selected checks completed; no publication performed. Read SKIP rows for qualification limits.'
  else
    echo "FAIL: $active (exit $code); logs: $MOONSEED_RELEASE_RESULTS" >&2
  fi
}
trap finish EXIT
run() {
  active=$1; shift
  printf '\n== %s ==\n' "$active"
  if python3 "$root/tools/ci_run.py" -- "$@" > "$MOONSEED_RELEASE_RESULTS/$active.log" 2>&1; then
    :
  else
    code=$?
    printf '%s\tFAIL\t%s\n' "$active" "$code" >> "$summary"
    tail -n 20 "$MOONSEED_RELEASE_RESULTS/$active.log" >&2
    return "$code"
  fi
  printf '%s\tPASS\t0\n' "$active" >> "$summary"
  tail -n 4 "$MOONSEED_RELEASE_RESULTS/$active.log"
}
skip() {
  printf '%s\tSKIP: %s\t0\n' "$1" "$2" >> "$summary"
  printf 'SKIP %s: %s\n' "$1" "$2"
}
fast() {
  run fmt cargo fmt --all --check
  run host-boundary python3 tools/check_host_boundary.py
  run compat-baseline python3 tools/check_release_baseline.py
  run clippy-default cargo clippy --locked --workspace --all-targets -- -D warnings
  run clippy-all cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
  run clippy-no-default cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
  run clippy-portable cargo clippy --locked -p moonseed --all-targets --no-default-features -- -D warnings
  run workspace-tests cargo test --locked --workspace
  run doc-default env RUSTDOCFLAGS=-Dwarnings cargo doc --locked --workspace --no-deps
  run doc-all env RUSTDOCFLAGS=-Dwarnings cargo doc --locked --workspace --all-features --no-deps
  run doc-no-default env RUSTDOCFLAGS=-Dwarnings cargo doc --locked -p moonseed --no-default-features --no-deps
}
native() {
  run public-build-default cargo build --locked -p moonseed
  run public-build-no-default cargo build --locked -p moonseed --no-default-features
  run public-build-native cargo build --locked -p moonseed --no-default-features --features native-host
  run public-build-counters cargo build --locked -p moonseed --no-default-features --features counters
  run public-build-all cargo build --locked -p moonseed --all-features
  run public-tests-default cargo test --locked -p moonseed
  run public-tests-no-default cargo test --locked -p moonseed --no-default-features
  run public-tests-all cargo test --locked -p moonseed --all-features
  # Explicit receipt: these must execute on each platform, in addition to core
  # and integration tests above. Unix-only symlink cases stay cfg-gated.
  run native-filesystem cargo test --locked -p moonseed --lib hostcaps::native::tests
}
msrv() {
  if ! python3 tools/ci_run.py --seconds 30 -- rustup run 1.88 rustc --version > "$MOONSEED_RELEASE_RESULTS/msrv-version.log" 2>&1; then
    [[ $mode != --msrv ]] || { active=msrv-unavailable; return 1; }
    skip msrv 'Rust 1.88 is not installed'; return
  fi
  for config in default no-default all; do
    flags=()
    [[ $config != no-default ]] || flags=(--no-default-features)
    [[ $config != all ]] || flags=(--all-features)
    run "msrv-build-$config" cargo +1.88 build --locked -p moonseed "${flags[@]}"
    run "msrv-tests-$config" cargo +1.88 test --locked -p moonseed --lib --tests "${flags[@]}"
  done
  run msrv-build-native cargo +1.88 build --locked -p moonseed --no-default-features --features native-host
  run msrv-build-counters cargo +1.88 build --locked -p moonseed --no-default-features --features counters
}
wasm() {
  # Missing target fails; install it deliberately with rustup target add first.
  for config in no-default default all; do
    flags=()
    [[ $config != no-default ]] || flags=(--no-default-features)
    [[ $config != all ]] || flags=(--all-features)
    run "wasm-build-$config" cargo build --locked -p moonseed --target wasm32-unknown-unknown "${flags[@]}"
  done
  run wasm-probe-build cargo build --locked -p moonseed-wasm-probe --release --target wasm32-unknown-unknown
  run wasm-roundtrip env MOONSEED_WASM_PROBE="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/moonseed_wasm_probe.wasm" \
    cargo test --locked -p moonseed-wasm-probe --release --test roundtrip -- --ignored --exact wasm_roundtrip
}
slow() {
  run corpus-runner cargo build --locked -p moonseed-bench --bin moonseed-run
  export MOONSEED_RUN=$CARGO_TARGET_DIR/debug/moonseed-run
  export MOONSEED_DIAG_RESULTS=$MOONSEED_RELEASE_RESULTS/diag
  export MOONSEED_HOOK_RESULTS=$MOONSEED_RELEASE_RESULTS/hooks
  export MOONSEED_UTF8_RESULTS=$MOONSEED_RELEASE_RESULTS/utf8
  export MOONSEED_HOSTLIB_RESULTS=$MOONSEED_RELEASE_RESULTS/hostlib
  if [[ -n ${MOONSEED_LUA54:-} ]]; then
    # Missing/misconfigured oracles fail, rather than silently skipping tests.
    active=oracle-inputs
    test -x "$MOONSEED_LUA54"
    test -x "${MOONSEED_LUA54_UD:?set MOONSEED_LUA54_UD to the matching userdata harness (tools/build_lua54.sh)}"
    MOONSEED_LUA54=$(realpath "$MOONSEED_LUA54")
    MOONSEED_LUA54_UD=$(realpath "$MOONSEED_LUA54_UD")
    export MOONSEED_LUA54 MOONSEED_LUA54_UD
    scratch=$(mktemp -d "$MOONSEED_RELEASE_RESULTS/oracle-wrappers-XXXXXX")
    for name in lua userdata; do
      binary=$MOONSEED_LUA54
      [[ $name != userdata ]] || binary=$MOONSEED_LUA54_UD
      printf '#!/usr/bin/env bash\nexec python3 %q --runtime -- %q "$@"\n' \
        "$root/tools/ci_run.py" "$binary" > "$scratch/$name"
      chmod +x "$scratch/$name"
    done
    run oracle-tests env MOONSEED_LUA54="$scratch/lua" MOONSEED_LUA54_UD="$scratch/userdata" \
      cargo test --locked -p moonseed --lib lua54_ -- --ignored
    run diag-corpus python3 tools/diag_corpus.py --check
    # diag_corpus's historical exit status only reports tool errors.
    run diag-acceptance python3 - "$MOONSEED_DIAG_RESULTS/baseline.md" <<'PY'
from pathlib import Path
import re, sys
report = Path(sys.argv[1]).read_text()
rows = re.findall(r'^\| [^|]+ \| (\d+) \| (\d+) \| (\d+) \| (\d+) \| (\d+) \|', report, re.M)
if not rows or any(int(c) != int(m) or any(map(int, rest)) for c, m, *rest in rows):
    sys.exit('diagnostic mismatches/missing records')
if '## Process failures\n\nNone.' not in report:
    sys.exit('diagnostic process failure')
print('diagnostic acceptance: all records match, no process failures')
PY
  else
    skip oracle-tests 'MOONSEED_LUA54 unset; PUC-dependent checks not qualified'
    skip diag-corpus 'needs MOONSEED_LUA54 for oracle identity'
  fi
  run hooks-corpus python3 tools/hooks_corpus.py --check
  run utf8-corpus python3 tools/utf8_corpus.py --check
  run hostlib-corpus python3 tools/hostlib_corpus.py --check
  run official-suite bash tools/lua_suite.sh run "$MOONSEED_RELEASE_RESULTS/lua54-actual.json"
  run official-suite-baseline python3 tools/check_release_baseline.py "$MOONSEED_RELEASE_RESULTS/lua54-actual.json"
}
package() {
  run package-list cargo package --locked -p moonseed --list --allow-dirty
  run package cargo package --locked -p moonseed --allow-dirty
  run publish-dry-run cargo publish --locked -p moonseed --dry-run --allow-dirty
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/moonseed-unpacked-XXXXXX")
  # Identify the public package via metadata, not a hard-coded RC version.
  run package-metadata cargo metadata --locked --no-deps --format-version 1
  version=$(python3 - "$MOONSEED_RELEASE_RESULTS/package-metadata.log" <<'PY'
import json, sys
p = next(p for p in json.load(open(sys.argv[1]))['packages'] if p['name'] == 'moonseed')
print(p['version'])
PY
)
  run package-unpack tar -xzf "$CARGO_TARGET_DIR/package/moonseed-$version.crate" -C "$scratch"
  (
    cd "$scratch/moonseed-$version"
    # Match the workspace's proof-test optimization while preserving assertions.
    export CARGO_PROFILE_TEST_OPT_LEVEL=1
    for config in default no-default all; do
      flags=()
      [[ $config != no-default ]] || flags=(--no-default-features)
      [[ $config != all ]] || flags=(--all-features)
      run "tarball-build-$config" cargo build --locked "${flags[@]}"
      run "tarball-tests-$config" cargo test --locked "${flags[@]}"
    done
  )
  rm -rf "$scratch"; scratch=''
}
case "$mode" in
  --fast) fast ;;
  --native) native ;;
  --msrv) msrv ;;
  --wasm) wasm ;;
  --all) fast; native; msrv; wasm; slow; package ;;
esac
