# Portable tool configuration

Run tools from the repository root. Paths are local inputs, not committed
machine-specific locations. On shared machines use the operator-assigned
`CARGO_TARGET_DIR` and results directories, and keep the documented resource caps.

| Variable | Default | Used by |
| --- | --- | --- |
| `CARGO_TARGET_DIR` | repository `target/` | Cargo, builds and corpus tools |
| `MOONSEED_LUA54` | repository `vendor/lua-5.4.9/src/lua` | Diagnostic, hook, UTF-8, host-library tools and ignored oracle tests |
| `PUC` | repository `vendor/lua-5.4.9/src/lua` | Benchmark and call-family tools |
| `MOONSEED_RUN` | `$CARGO_TARGET_DIR/debug/moonseed-run` (corpus) or `bench-stable/moonseed-run` (benchmarks) | Compared runner |
| `MOONSEED_LUA54_UD` | required explicit path to the matching userdata harness | Ignored userdata/GC oracle tests |
| `MOONSEED_LUA54_USERDATA` | repository `target/lua54-userdata` | Diagnostic userdata oracle |
| `MOONSEED_LUA54_HOOKS` | `$MOONSEED_HOOK_RESULTS/lua54-hooks` | Hook C oracle |
| `MOONSEED_DIAG_RESULTS` | repository `results/diag/` | Diagnostic output |
| `MOONSEED_HOOK_RESULTS` | repository `results/hooks/` | Hook output |
| `MOONSEED_UTF8_RESULTS` | repository `results/utf8/` | UTF-8 output |
| `MOONSEED_HOSTLIB_RESULTS` | repository `results/hostlib/` | Host-library output |
| `VALGRIND` | `valgrind` on PATH | Instruction profiling |
| `PERF` | `perf` on PATH | Call-family profiling |

Set oracle variables to a Lua 5.4.9 build without `LUA_COMPAT_5_3`; captures
record its executable hash. `--check` compares frozen data, while `--capture`
rewrites it. Choose `--check` for validation. `vendor/` and `results/` are ignored.
The Lua sources/library used for C harness compilation must match that oracle.
Moss integration dependency source URLs require owner configuration separately.

For the CI/release feature matrix, pinned downloads, resource caps and
validation-only packaging, see [RELEASE_CHECK.md](RELEASE_CHECK.md).
