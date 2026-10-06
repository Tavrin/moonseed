# CI and release validation

`bash tools/release_check.sh` runs the full Linux release gate and stops at
`cargo publish --dry-run`. It never publishes, tags, commits or changes the Git
index. Each step writes a log and a PASS/FAIL/SKIP receipt in
`results/release-check/summary.tsv`; failures stop the run with a nonzero exit.
Set `MOONSEED_RELEASE_RESULTS` and the operator-assigned `CARGO_TARGET_DIR` to
store evidence/builds elsewhere. All Cargo operations use the committed lockfile.
`--allow-dirty` permits coordinator-owned uncommitted work during package checks;
CI starts from a clean HEAD clone. Publication always requires a separate human
command and is absent from both workflows.

Prerequisites: Bash, Python 3.11+, rustup, stable with rustfmt/clippy and
`wasm32-unknown-unknown`, tar, curl, bubblewrap, and a working Linux user namespace.
Install Rust 1.88 for the MSRV check; a missing MSRV toolchain is explicitly
SKIP locally and FAIL for `--msrv`. A missing Wasm target or unusable bubblewrap
fails the full gate; there is no unconfined suite/hostlib fallback.

Build an optional fresh PUC oracle:

```sh
bash tools/build_lua54.sh "$PWD/vendor/release-lua54"
export MOONSEED_LUA54="$PWD/vendor/release-lua54/lua-5.4.9/src/lua"
export MOONSEED_LUA54_UD="$PWD/vendor/release-lua54/lua54-userdata"
bash tools/release_check.sh
```

The source download is `https://www.lua.org/ftp/lua-5.4.9.tar.gz`, SHA-256
`2335b6c582a52654f94612bf10d2f4672805d05329aa6568b1d8cd9e5c6fb8e6`
([official checksums](https://www.lua.org/ftp/)). Every build verifies the archive,
extracts fresh sources and removes `-DLUA_COMPAT_5_3` from the stock Makefile.
The userdata harness is compiled from this repository against that library.
With `MOONSEED_LUA54` set, the matching `MOONSEED_LUA54_UD` is required; invalid
inputs fail. Without PUC, only oracle tests and the diagnostic corpus are skipped.
Frozen hooks, UTF-8 and hostlib checks still run. UTF-8 digest failures may need
the exact original oracle binary for diagnostics; a newly built compiler-dependent
oracle never rewrites expectations or suppresses the failure.

The official test archive is downloaded by `tools/lua_suite.sh` from
`https://www.lua.org/tests/lua-5.4.9-tests.tar.gz`, SHA-256
`7d971845f545ffc09fbb3128a86b2c6524161c70d0fdf0154a16e8c00c343fca`.
It is verified on every run and extracted afresh. Neither download needs local
scratch state or secrets. Frozen corpora are checked, never recaptured.
`check_release_baseline.py` rejects unknown/blocking classes and compares a fresh
suite's file set, compiled status, outcome, blocker and exact first-error detail
against `tests/compat/lua54-0.1.0.json`. Accepted failures must stay at their
reviewed frontier; changes, including improvements, require deliberate review.
Fuel counts and printed identities are observational and are not compared.
The historical `lua54-current.json` is not the release acceptance baseline.

CI splits fast Linux stable checks, Rust 1.88 public build/test configurations,
macOS/Windows public/core tests (including native filesystem behavior), and Wasm
no-default/default/all-feature builds. The probe roundtrip executes through the
existing `wasmi` Rust runner, exchanging snapshots in both directions. The large
ignored near-quota stress/exchange tests remain separate milestone checks.
The weekly/manual release job uses Ubuntu 22.04 with Python 3.12 and probes
bubblewrap before compiling; Ubuntu 24.04's AppArmor user-namespace restrictions
can prevent an unprivileged bubblewrap launch
([upstream report](https://github.com/containers/bubblewrap/issues/632)).
The release workflow builds PUC and runs the full gate in a fresh
local clone, including standalone unpacked-crate builds and tests for all three
feature configurations. No publication workflow or credentials are configured.

`ci_run.py` caps Cargo/tool commands at 1,800 seconds, eight Cargo build jobs,
eight test-harness threads, and on Linux 8,000,000 KiB address space. Each Cargo test/example harness retains the 1,800-second/8,000,000 KiB
Cargo cap. A harness runs many bounded VM proofs and is not a standalone Lua
program. Each oracle wrapper has a separate 60-second cap and on Linux
2,000,000 KiB address space. Corpora retain their per-engine 60-second/2,000,000 KiB limits. Stricter inherited limits are preserved, including the oracle tests’ existing
1,000,000 KiB/CPU launch limits. Unix also caps CPU time and disables core dumps. macOS uses CPU/wall caps (RLIMIT_AS is not
reliable there); Windows uses wall deadlines with PID-scoped child-tree shutdown
and the workflow job deadline. These are validation resource limits, not an OS
sandbox. Rustdoc compilation has Cargo's cap; generated doctest execution stays
inside the capped Cargo invocation. Parent timeouts terminate their process group.

For focused CI execution, use `--fast`, `--native`, `--msrv` or `--wasm`.
The full gate uses stable by default, independent of the dev toolchain file;
`RELEASE_TOOLCHAIN` selects an installed validation toolchain. Unpacked tests set
only `CARGO_PROFILE_TEST_OPT_LEVEL=1` to match the workspace proof-test profile.

CI disables Rust debug symbols and incremental compilation to fit the hosted
runner's ephemeral disk ([runner specifications](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)).
Debug assertions remain enabled in dev/test, and the test optimization stays at
level 1. Local checks preserve caller profile settings and record their own
artifacts; they do not establish the identity of an unrun remote binary.
