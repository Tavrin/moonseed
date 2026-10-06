# Contributing

Use the toolchain in `rust-toolchain.toml` for development. The library supports
Rust 1.88. Run commands below from the repository root. Changes should include a
small regression for the behavior being fixed and explain any compatibility or
resource-accounting impact. Report security issues according to [SECURITY.md](SECURITY.md).

## Build and test

Run one Cargo command at a time on shared machines. Choose a build directory
with enough space; `CARGO_TARGET_DIR` is honored by the test and corpus tools.
The examples below use a shell helper to cap build jobs, virtual memory and
elapsed time on Linux:

```sh
export CARGO_TARGET_DIR="$PWD/target"
ccargo() { ( ulimit -v 8000000; CARGO_BUILD_JOBS=8 timeout 1800 cargo "$@" ); }
ccargo build --workspace
ccargo fmt --all --check
ccargo clippy --workspace --all-targets -- -D warnings
ccargo clippy --workspace --all-targets --all-features -- -D warnings
ccargo test --workspace
ccargo test -p moonseed --no-default-features
ccargo test -p moonseed --all-features
ccargo test --doc
RUSTDOCFLAGS='-D warnings' ccargo doc --workspace --no-deps --all-features
ccargo +1.88 test -p moonseed
```

On other platforms use equivalent process limits. The ten examples under
`crates/moonseed/examples/` have test entry points; workspace tests execute them.
README and embedding-guide Rust blocks are included in doctests. Keep the root
README authoritative and update its package copy with the same prose/code and
package-appropriate links. Edit `docs/EMBEDDING.md`, then copy it byte-for-byte to
`crates/moonseed/EMBEDDING.md`; its repository links work in either location.
Keep the feature tables consistent with `crates/moonseed/FEATURES.md` and rustdoc.

## Lua oracle and official suite

Use PUC Lua **5.4.9 without `LUA_COMPAT_5_3`**. The stock Makefile enables that
compatibility macro; remove `-DLUA_COMPAT_5_3` from its build flags and rebuild.
The tests reject a reference that retains the old `__le` fallback. PUC is an
external test tool, not a dependency of the library.

Set `MOONSEED_LUA54` to the resulting executable. Userdata and collector tests
also need the small C harness linked against the same PUC `liblua.a`:

```sh
export LUA_SRC="$PWD/vendor/lua-5.4.9/src"
export MOONSEED_LUA54="$LUA_SRC/lua"
mkdir -p results/oracle
cc -O2 -Wall -Wextra -I"$LUA_SRC" tools/lua54_userdata_harness.c \
  "$LUA_SRC/liblua.a" -lm -ldl -o results/oracle/lua54-userdata
export MOONSEED_LUA54_UD="$PWD/results/oracle/lua54-userdata"
ccargo test -p moonseed --lib lua54_ -- --ignored --test-threads=1
```

Oracle child processes use the repository's capped launcher. Keep generated
or adversarial Lua runs inside memory/time limits and file-writing tests inside
a temporary root or the in-memory filesystem.

The official suite is separate from these oracle tests. On Linux it requires
`curl`, `sha256sum`, `tar`, Python 3 and `bwrap`. The script verifies and freshly
extracts the pinned archive, builds the compatibility runner and confines each
file to a writable temporary root:

```sh
bash tools/lua_suite.sh run results/official-suite.json
```

Use a results path to preserve the checked-in ledger. Compare each file with
[the classified baseline](docs/LUA_54_SUITE.md); a script exit alone is not a
full-suite pass. Do not edit the upstream tests to change that baseline.

## Frozen corpora

Build the compared runner, then check existing expectations:

```sh
ccargo build -p moonseed-bench --bin moonseed-run
export MOONSEED_RUN="$CARGO_TARGET_DIR/debug/moonseed-run"
python3 tools/diag_corpus.py --check
python3 tools/hooks_corpus.py --check
python3 tools/utf8_corpus.py --check
python3 tools/hostlib_corpus.py --check
```

Diagnostic userdata cases use `MOONSEED_LUA54_USERDATA` (set it to the userdata
harness above); external-hook capture uses `MOONSEED_LUA54_HOOKS`, built from
`tools/lua54_hook_harness.c` against the same reference. Frozen checks compare
saved bytes; `--capture` rewrites expectations and belongs only in a deliberate
corpus update with reviewed oracle changes. Host-library checks require Linux
and `bwrap` and run both native and VFS profiles.

Each corpus README records its protocol, helper build and acceptance rules:
[diagnostics](tests/diag/README.md), [hooks](tests/hooks/README.md),
[UTF-8](tests/utf8/README.md), [host libraries](tests/hostlib/README.md).
See [tool paths](tools/PATHS.md) for all overrides. Do not compare results made
by a runner that was rebuilt after its identity was recorded.

## Wasm

Install the target and check portable configurations:

```sh
rustup target add wasm32-unknown-unknown
ccargo check -p moonseed --target wasm32-unknown-unknown --no-default-features
ccargo check -p moonseed --target wasm32-unknown-unknown --all-features
ccargo build -p moonseed-wasm-probe --release --target wasm32-unknown-unknown
export MOONSEED_WASM_PROBE="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/moonseed_wasm_probe.wasm"
ccargo test -p moonseed-wasm-probe --release -- --ignored wasm_roundtrip
```

The exchange tests execute the built probe through wasmi. A target check
only establishes compilation; it does not execute the exchange tests.

## Performance changes

Start with a profile and retain matching result checksums. Use the portable
`bench-stable` build and the fixed corpus. Keep compiler flags, source hashes,
binary hashes, CPU affinity and workload scaling in the result record.
Callgrind instruction counts include startup/compile/teardown; the empty
startup workload is excluded from nonempty geometric means. Scaled counts
cannot be multiplied into full-size predictions. Keep enabled counters and
allocation instrumentation in separate builds.

Wall measurements need controlled load, warmups, repeated interleaved engine
samples and environment records. Shared-machine timings are diagnostic.
Performance changes must retain fuel, checkpoint, collection and host-effect
semantics. Follow [the benchmark method](tools/BENCHMARKING.md) and compare with
[recorded results](docs/PERFORMANCE.md).

## Architecture and review

Read [ARCHITECTURE.md](docs/ARCHITECTURE.md) and the relevant
[design decisions](docs/adr/) before changing execution or serialized state.
[Embedding](docs/EMBEDDING.md), [determinism](docs/DETERMINISM.md),
[compatibility](docs/COMPATIBILITY_POLICY.md) and [security](SECURITY.md) define
the host contract. Run `python3 tools/check_host_boundary.py` when changing
host access. Keep ambient OS calls inside the native capability adapter.

Keep `moonseed` free of unsafe code. Never serialize addresses, `usize`, Rust
enum discriminants or arena generations; host functions are registry symbols.
Keep fuel pauses, Lua yields, host waits, catchable Lua errors and VM failures
distinct. Do not copy third-party implementation source. Use `tools/capped.sh`
for generated or adversarial programs.

A proposal that changes a format or semantic revision must explain why,
how old data is rejected or read, and how the change is tested. Include
only relevant validation in the change description, with unrun checks stated.
