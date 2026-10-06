# Frozen Lua 5.4.9 diagnostic corpus

`tools/diag_generate.py` defines deterministic case records `(id, chunk name,
source, optional setup)` and emits one pure-Lua driver protocol for both
engines. Hand-selected cases cover runtime locations and provenance,
`error`/`assert`, library argument errors, metamethods, stripped functions,
lexer/parser and compile limits. Generated cases cross source forms, bad value
types, operand sides, and operations, plus call forms for argument errors.
`provenance_flow` adds branches, loops, jumps, unusual register writers, and
long writer-to-error distances. `stripped` expands dump/load coverage across
operand forms, argument calls, error levels, traceback prefixes, and debug
source locations.

The relative `driver.lua` runs from a fixed results directory in both engines.
Its batch records occupy one source line, so every driver call site retains the
same line number across batches. It uses `load` then `pcall`. Every record is a tab-separated line:
`id`, `ok|compile|error`, error object type, lowercase hex of **all** string
error bytes (or `-`), and a table identity marker (`same|other|-`). Hex avoids
differences in `%q` formatting and preserves arbitrary byte values. The
runner validates record count and IDs before accepting a capture.

The oracle is the pinned Lua 5.4.9 build without `LUA_COMPAT_5_3` at
`vendor/lua-5.4.9/src/lua`.
Userdata cases use the existing `lua54-userdata` C harness on the PUC side
and `moonseed-run --userdata`, which installs the same natives, on Moonseed's.
There is no uncaught file mode: `moonseed-run` prints `ERROR: ...` without the
PUC command-line traceback, so its output is not an equivalent oracle surface.

From the repository root:

```sh
python3 tools/diag_corpus.py --capture
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build -p moonseed-bench --bin moonseed-run )
python3 tools/diag_corpus.py --check
```

`--capture` rewrites `expected/*.txt` from PUC. `--check` writes
`results/diag/baseline.md` by default;
`--report PATH` selects another location. The report includes totals,
top mismatch shapes, process failures, and every expected versus actual byte
record. All Lua processes have a 2,000,000 KiB address-space limit and a
60-second timeout. The generated driver is in the results directory.

Tool paths are configured by environment variables; see
[portable tool configuration](../../tools/PATHS.md) for defaults.
