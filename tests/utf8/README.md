# Frozen PUC Lua 5.4.9 UTF-8 differential corpus

This corpus covers Gates 0/A/M with **508,865 records in 2,028 batches**.
It changes no Moonseed crate code. The entry baseline is main HEAD
`d93ad1a0bb4248f914f4e27a14ec30718f0b6042`, which has no `utf8` library.
Baseline mismatches are expected; they do not indicate a tool failure.
The final recorded baseline uses a release build of that same HEAD.

From the repository root:

```sh
python3 tools/utf8_corpus.py --capture
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build -p moonseed-bench --bin moonseed-run )
python3 tools/utf8_corpus.py --check
```

To reproduce the final baseline with its optimized executable:

```sh
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build --release -p moonseed-bench --bin moonseed-run )
MOONSEED_RUN=target/release/moonseed-run python3 tools/utf8_corpus.py --check
```

The pinned oracle is the PUC Lua 5.4.9 build without `LUA_COMPAT_5_3` at
`vendor/lua-5.4.9/src/lua`.
Its executable hash is frozen in `manifest.json`. `--capture` deliberately
rewrites the 14 group files in `expected/` and the manifest; no compared
engine is used to infer expected values. Two final captures were compared byte for byte, including the manifest.
The measured slowest PUC batch was under 0.1 seconds. These timings are
batch-sizing evidence, not an interpreter performance qualification.

`MOONSEED_LUA54` overrides the PUC executable used for capture and digest-failure
diagnostics (default: the pinned path above). `MOONSEED_RUN` overrides the
checked executable. Otherwise it is
`$CARGO_TARGET_DIR/debug/moonseed-run`, with the target directory above as the
default. `--results-dir PATH` overrides the default
`results/utf8/`; the environment variable
`MOONSEED_UTF8_RESULTS` supplies that default. `--report PATH` selects the check
report. Checks write `baseline.md`, `baseline.json`, exact mismatch diagnostics
in `mismatches.jsonl`, and raw per-batch output in `actual/*.txt`. Full-record
groups save every mismatch; digest groups print and save the first three
differing records per failing batch by default (`--diff-limit N` changes this). The JSON report
records executable/manifest hashes, group totals, exit codes, process/protocol
failures and per-batch durations. Exit codes: 0 for capture or a fully matching
check; 1 for a completed check with semantic mismatches; 2 for tool, process or
protocol failures. Timeouts, failed launches and nonzero engine exits are
reported, rather than interpreted as semantic mismatches alone.

`python3 tools/utf8_generate.py OUT` optionally materializes every driver and
an ordered `manifest.jsonl` of case source records into a scratch directory.
Ordinary capture/check regenerate sources directly and need no materialized
case-source files. Use separate results directories for concurrent invocations:
each invocation writes its own fixed `driver.lua`.

## Shared byte protocol

Both engines run identical pure-Lua source from relative `driver.lua` in the
results directory. Each batch contains at most 256 cases, embedded in a single
source line so the driver call-site lines remain constant. Each case uses
`load(source, '@utf8-case.lua')` then `pcall`; lexer-equivalence cases additionally
load `@utf8-escape.lua`. There are no absolute paths in record messages.
No IO/OS libraries, host Unicode conversions, timestamps or addresses enter
the records.

Each ASCII, LF-terminated tab-separated record is:

```text
id  status  count  [type  value] × count  error-type  error-value
```

`status` is `ok`, `error` or `compile`. `count` preserves zero results and nil
holes. Successes end with `-`, `-`. Failures record the exact error object's type
and bytes. Values use these representations:

| Type | Value representation |
| --- | --- |
| `string` | Lowercase hex of every byte; the empty string has an empty field |
| `number.integer` | Exact signed decimal Lua integer |
| `number.float` | Lua `%a` hexadecimal float (also `inf`/`nan` if encountered) |
| `boolean` | `true` or `false` |
| `nil` | `-` |
| `function` | `strict-iterator` or `lax-iterator`, identified by `rawequal` against the corresponding `utf8.codes('')` iterator |

The iterator triple records the actual function's type and stable identity,
state string bytes, and integer control. The separate identity case compares
iterators across calls and modes directly, and records the triple's arity.
Pointers cannot provide reproducible function values.

Full generic-for iteration flattens each `(position, codepoint)` pair into
results. When iteration errors, `count` and those results preserve its completed
prefix, followed by the terminal error type/message. Ordinary failing calls,
including `codepoint` errors after an internal partial decode, expose zero
results. Successful iterations expose every pair, including zero pairs.
The runner rejects malformed fields/types/hex, duplicate or foreign IDs,
missing records, and absolute paths in error messages. It validates all frozen
record and regenerated-driver hashes before accepting a comparison. Exact
record comparisons retain source positions and argument function names.

## Compact frozen storage

Every case and all 2,028 runtime batches are retained. Storage version 2 in
`manifest.json` describes one expected file per group and binds each file's
SHA-256, original record-byte size, group count and every batch's driver/record
hashes. The Lua record protocol remains version 1.

Eleven small groups keep all frozen protocol lines in `expected/GROUP.txt`.
Records for a group follow its batches in generator order, even when other
groups run between those batches. `decoder2`, `random`, and `positions` use
`expected/GROUP.sha256`, one tab-separated LF-terminated entry per batch:

```text
batch-name  record-count  sha256-of-exact-record-bytes
```

`decoder2` and `random` always use digests. Capture also selects digests for
any other group exceeding 1 MiB of full record bytes (`positions` currently).
The manifest and digest files are deterministic; timing receipts stay in the
results directory. Only 16 files belong under `tests/utf8`: README, manifest,
11 record files and 3 digest files. The current footprint is **1.7 MB**
(`du -sh tests/utf8`), comfortably below the 3 MB / 60-file budget.

Checks first validate all stored group hashes, batch counts and driver hashes.
For digest groups, a correctly formed, complete output whose count and exact
byte digest match is accepted without running PUC. A differing digest causes
that batch to run again on `MOONSEED_LUA54`; its validated output must reproduce
the frozen digest before it can provide diagnostic expectations. The reference
executable must match the captured oracle hash. An unavailable or changed
reference, or a reference output differing from the frozen digest, is a tool
failure (exit 2), never an automatic expectation update.

The tool then compares all case records in the failing batch for accurate
match/mismatch/missing totals, and prints the first differing expected/actual
records as JSON to stdout. Those examples are also saved in `mismatches.jsonl`.
Changed record order is a digest failure even when per-ID values match.
`baseline.json` lists every failing digest batch and verified reference rerun.

To show more differences per batch, rerun the ordinary check with, for example:

```sh
MOONSEED_LUA54=vendor/lua-5.4.9/src/lua \
MOONSEED_RUN=target/release/moonseed-run \
python3 tools/utf8_corpus.py --check --diff-limit 10 --results-dir results/utf8/delta
```

`--diff-limit 256` retains every differing record in each digest batch. All raw
actual records and diagnostics remain outside the repository fixtures.

## Deterministic groups

| Group | Records | Coverage |
| --- | ---: | --- |
| `decoder1` | 1,792 | Every 256 one-byte subject × seven modes |
| `decoder2` | 458,752 | Every 65,536 two-byte subject × seven modes |
| `boundaries` | 2,401 | 343 subjects: 1–6-byte boundaries and adjacent values, every proper truncation, invalid continuations, surrogates, overlongs for lengths 2–6, values through `0x7fffffff`, six-byte encodings above it, continuation starts and extra continuations, valid prefixes with NUL |
| `char` | 36 | Required boundary values, zero/many arguments, bad late argument, negatives, integer extremes, integral/fractional floats, numeric strings, NaN/infinity and nonnumbers |
| `lexer` | 1,020 | Boundary values, accepted float/string coercions, and all 1,000 many-argument values: return both `utf8.char` and loaded `\u{...}` bytes plus their equality |
| `defaults` | 407 | Default arguments and omitted final positions on every selected subject |
| `positions` | 12,716 | Full 17 × 17 i/j matrix for len/codepoint in strict/lax modes over 11 subjects |
| `offset_positions` | 2,178 | Eleven n expressions × default or 17 explicit positions × 11 subjects |
| `lax` | 198 | `false`, `nil`, `true`, `0`, `''`, `{}` on len/codepoint/codes over 11 subjects |
| `codes` | 523 | Actual triples, complete generic-for iteration, manual controls including continuation bytes, -1/0/past end, floats, strings/nil and strict/lax identities |
| `charpattern` | 12 | Exact pattern bytes/length and gmatch over all selected subjects |
| `arguments` | 156 | Missing/wrong/coercible arguments for every function and iterator; no `__tostring` coercion; local/method/multiline error provenance |
| `random` | 28,672 | 4,096 byte strings of lengths 0–16 × seven modes |
| `module` | 2 | require/global/package.loaded identity and exactly the six sorted fields |
| **Total** | **508,865** | |

The seven decoder modes are len strict/lax, codepoint strict/lax over the full
subject, codes strict/lax through exhaustion/error, and offset. Position tests
use empty, ASCII, 2/3/4-byte, six-byte extended, surrogate, malformed,
continuation-start, embedded-NUL and mixed-width subjects. Positions include
0, 1, #s, #s+1, #s+2, -1, -#s, -#s-1, MININTEGER/MAXINTEGER, integral/fractional
floats, numeric/bad strings, tables, false and nil. Offset n includes
0, ±1, ±2, ±#s, ±(#s+1) and both integer extremes.

Random generation uses explicit xorshift32 operations with seed `0x33605409`;
length is case index modulo 17. It is independent of Python/Lua RNG versions.
The Python encoder creates test subjects, including intentionally invalid ones;
PUC supplies their semantics. Lua strings remain byte strings. Strict decoding
accepts Unicode scalar values; lax decoding permits the extended range through
`0x7fffffff` while still rejecting malformed/overlong sequences.

All engine subprocesses have a 2,000,000 KiB address-space cap and a 60-second
timeout. Batches execute sequentially. This bounded small-input corpus does not
certify large-string quotas, fuel, snapshots, hooks, Wasm, official-suite or
performance behavior; those belong to the other Phase 3.36 lanes.

Tool paths are configured by environment variables; see
[portable tool configuration](../../tools/PATHS.md) for defaults.
