# Frozen PUC Lua 5.4.9 host-library corpus

The corpus freezes **1,620 records in 23 groups and 71 batches** for `io`,
`loadfile`, `dofile`, `package.searchpath`, filesystem `require`, `os`, and
`arg`. Phase 3.38 B1 adds 350 buffering records. The original 1,270 records,
fixtures and driver batches remain byte-identical. The check harness selects
integrated runner profiles; native and VFS together compare 2,569 records.
PUC capture and self-check supply the reference independently of Moonseed.
Buffer capacities are observations of the frozen Linux libc, not Lua's
portable guarantee of a particular capacity for NULL setvbuf storage.

## Reproduce

From the repository root, on Linux with Python 3 and working `bwrap`:

```sh
python3 tools/hostlib_corpus.py --capture
python3 tools/hostlib_corpus.py --self-check --results-dir results/hostlib/self-check
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build -p moonseed-bench --bin moonseed-run )
python3 tools/hostlib_corpus.py --check
```

`--capture` deliberately rewrites `manifest.json` and `expected/GROUP.txt`
using only the pinned PUC 5.4.9 build without `LUA_COMPAT_5_3`:
`vendor/lua-5.4.9/src/lua`.
Its executable SHA-256 is frozen in the manifest. `MOONSEED_LUA54` overrides
the reference executable. `--self-check` requires that executable's captured
hash and compares its fresh execution against frozen bytes.

`MOONSEED_RUN` overrides the compared binary, otherwise
`$CARGO_TARGET_DIR/debug/moonseed-run` is used; the default target is
`target`.
`--results-dir PATH` overrides `MOONSEED_HOSTLIB_RESULTS`, whose default is
`results/hostlib`.
`--report PATH` overrides the Markdown check report. Use separate results
directories for simultaneous invocations. `python3 tools/hostlib_generate.py
OUT` optionally materializes every driver and ordered `cases.jsonl` for
inspection; capture/check regenerate these directly without storing case
sources in the corpus.

Checks write `baseline.md`, `baseline.json`, all exact differences to
`mismatches.jsonl`, and raw per-batch output to `actual/*.txt`. Exit codes:
**0** for capture or a matching check, **1** for a completed semantic
mismatch check, **2** for launch, timeout, process, protocol, or frozen
identity failures. No new expectations are inferred from Moonseed.

## Environment and fixture control

Every engine process gets its own **fresh copy** of the versioned
`fixtures/` tree in a temporary directory under the selected results dir.
That directory is the process cwd. It is removed, including generated
files, after the batch completes or fails. PUC and Moonseed never reuse each
other's mutated fixture. Runtime batches have at most 32 cases; each stdin
case and each standard-stream case runs alone. Sources use relative paths
and fixed `@hostlib-case.lua`/`@fixtures/NAME.lua` chunk names. The driver
saves/restores default input/output and package paths between cases.

Both engines run with an explicitly supplied empty-based environment, the
equivalent of `env -i`, containing only:

```text
TZ=UTC                       (America/New_York in os_new_york)
LC_ALL=C
PATH=/usr/bin:/bin
MS_A=alpha
MS_EMPTY=
```

`MS_UNSET` is absent. No inherited HOME, locale, Lua path, or Lua init
variables enter the child. Stdin is always supplied from a fixed copied
file: `stdin/empty`, `stdin/read.txt`, or `stdin/chunk.lua`, selected by group.
Stdout is the protocol stream, and stderr must be empty. `print` remains
separate from the changeable `io.output()` default.

`bwrap` makes the outer filesystem read-only and binds just the fresh fixture
writable. `/tmp` is bound to that fixture's `.host-tmp/`: the pinned PUC
implementation hard-codes `/tmp/lua_XXXXXX` for `os.tmpname`, so setting
TMPDIR alone would not confine it. Temporary-file names are never recorded;
the tmpname case checks string/prefix/existence/removal booleans and removes
the generated file. This also confines libc `tmpfile`. No host paths are
put in records. Searchpath templates that could otherwise turn a leading
dot into an absolute path use `./?`.

All processes execute sequentially with a **2,000,000 KiB address-space
limit and 60-second timeout**. The sandbox is required; there is no fallback
that silently permits temporary files outside the fixture. This harness
controls test mutations; it does not qualify a Moonseed capability adapter's
own traversal, symlink, process, or sandbox policy.

## Shared byte protocol and frozen identity

Both engines run identical ASCII `driver.lua` bytes. The case table occupies
one source line, keeping driver call-site positions constant across batches.
Every case is `load`ed as `@hostlib-case.lua` then called through `pcall`.
The driver is itself relative `driver.lua` in both engines.

Each LF-terminated ASCII TSV record is:

```text
id  status  count  [type  value] x count  error-type  error-value
```

Status is `ok`, `error`, or `compile`. Counts preserve zero results, nil
holes and final nils. Strings and error strings contain lowercase hex of
**every byte**; integers use signed decimal; floats use Lua `%a` (including
`inf`/`nan`); booleans use `true`/`false`; nil uses `-`. Successes end in
`-`, `-`. Failures have zero results and the typed error object. Calls caught
inside a case explicitly return their success boolean and error, alongside
observable file contents, cursor, or iterator prefix.

Opaque handles/functions are never formatted as pointers: cases return
`io.type`, `type`, identity booleans, arity and close state. The open-file
tostring case retains only its type-prefix match; closed-file text is exact.
The parser reuses the UTF-8 corpus's typed protocol authority and rejects
malformed fields, counts, types, hex, duplicate/foreign IDs, missing/reordered
records, absolute paths in any decoded string, and pointer-shaped strings.
Captures require zero exit status, empty stderr and the complete ordered ID
set before publishing expectations.

The deterministic manifest binds the generator, every fixture file, the
environment/argv layout, oracle binary, group sizes, all expected bytes and
every batch's regenerated driver and record digest. All frozen files are
validated before a compared engine launches. Each small group keeps its
full records in one file: 23 files; byte counts and hashes are in the manifest. Timings,
executable paths, and other machine receipts stay in the results directory,
outside frozen records. No RNG is needed: all matrices have fixed iteration
order and explicit inputs.

## Groups and acceptance scope

Exact records mean byte equality under this controlled **Linux** environment.
Argument/compile errors, values, cursor positions and arities are exact.
OS strerror text and numeric errno are retained for these Linux fixtures,
including ENOENT 2, EBADF 9 and EINVAL 22; these are platform-specific rows,
not a portable errno requirement on a virtual filesystem.

| Group | Records | Acceptance / coverage |
| --- | ---: | --- |
| io_modes | 116 | Exact; all six r/w/a with optional + families, each with 0/1/2/8 trailing b characters; existing/missing files, defaults, invalid strings/NUL and rejected b+, access/append/truncate behavior, argument/open errors; Linux failure tuples |
| io_read | 244 | Exact; seven byte subjects, default/n/a/l/L, legacy `*n/*a/*l/*L`, counts 0/1/3/100, combinations, invalid formats, cursor/remainder, EOF and omission after failure |
| io_numerals | 38 | Exact; hex, decimal/exponents, whitespace, malformed prefixes, overflow/underflow, integer boundaries and 199/200/201/301-character numerals, cursor and unconsumed suffix |
| io_write | 44 | Exact; integers/floats, strings/NUL, chaining, invalid late arguments and prefix writes, readonly failure, flush and setvbuf modes/sizes; Linux failure tuples |
| io_buffering | 350 | Linux PUC effective capacity and requested sizes; no/full/line visibility, bulk and prefilled overflow, flush/read/seek/newline, default switching, every close path and mode transitions |
| io_seek | 57 | Exact; whence/offset matrix, defaults, negative/past-end/extreme seeks, failed-seek cursor, sparse write; Linux failure tuples |
| io_defaults | 20 | Exact; initial defaults, filename/handle/nil switching, closed defaults, io.read/write/flush/close and restoration |
| io_lines | 45 | Exact; actual iterator return arity and nil state/control, 4th-value io.type after exhaustion/break/error; default-input break/error stays open; file:lines formats; 249/250/251 format limit; closed/exhausted iterator |
| io_lifecycle | 28 | Exact plus tostring shape; all closed methods, io.type, explicit __gc/__close, actual GC closing, scope/error/coroutine close, tmpfile, io.close variants |
| io_streams | 4 | Exact; standard stream types and PUC failure to close stdin/stdout/stderr |
| io_stdin | 3 | Exact; fixed stdin formats, read combinations and default io.lines |
| loadfile | 46 | Exact; text, env, shebang, compile/missing/runtime errors and chunk names, t/b/bt/invalid modes, dofile multiple/nil results, coroutine yield/resume through dofile |
| load_binary | 10 | Shape-only; own-engine string.dump, strip modes, t/b/bt rejection/acceptance, truncated own-engine chunk, binary dofile; no binary-byte comparison |
| loadfile_stdin | 2 | Exact; no filename and nil filename with mode/env |
| dofile_stdin | 1 | Exact; stdin chunk's full returned tuple |
| searchpath | 199 | Exact; 15 template paths x ten names, empty/leading/trailing/double semicolons, repeated ?, dotted names, sep/rep variations, invalid args, 1/20/100 missing templates then optional found file; full error lists |
| require | 5 | Exact filtered searchers; found/cached modules, second loader-data filename, nested name, missing list, existing compile/runtime-broken modules |
| os_fs | 12 | Linux-specific exact remove/rename success/failure tuples and errors; tmpname shape/existence/removal |
| os_utc | 289 | Linux-specific explicit timestamps, local TZ=UTC and !UTC formats, *t/!*t fields, invalid conversions, time(table) normalization and updated fields, out-of-range values, difftime |
| os_new_york | 54 | Separate Linux timezone group; explicit timestamps and normalization, standard/daylight offsets and transition/fold cases; requires a timezone-aware capability |
| os_misc | 38 | Shape-only clock type/nonnegative and time() type; exact set/unset/empty getenv and setlocale nil/C/empty/invalid in every category; Linux locale behavior |
| process | 13 | Requires process capability; fixed true/false/exit 3 execute and pipe-close tuples, availability, echo output, fixed cat pipe-write, invalid pipe modes |
| arg | 2 | Requires argv contract; fixed negative/zero/positive layout and chunk varargs |
| **Total** | **1,620** | **23 groups, 71 batches** |

Binary failures compare `mode:binary`, `mode:text`, `invalid-binary` or
`other-error` categories plus return types; successful binary loads return
the same fixed payload. The bytecode format remains engine-specific.
`require` sets `package.path` to `modules/?.lua;modules/?/init.lua` and cpath
to empty. Missing-module records retain the module header, preload attempt,
and both filesystem attempts. PUC's C-searcher/empty-cpath diagnostics are
excluded because Moonseed implements neither dynamic C modules nor those
searchers. Existing-file compile/runtime errors are unfiltered.

Read-all/zero-count line iterators can return empty strings indefinitely;
their exhaustion-labelled probes stop after eight yields and verify generic-for
closing. Ordinary line/numeral/count iterators run through actual EOF.
The GC witness drains old garbage, briefly wraps the original shared file
metatable's `__gc`, collects one unreachable handle, records one `closed file`,
then restores the metatable. It checks closure behavior, not GC timing.
Time-table cases set `isdst=false` when absent so libc's ambiguous-time cache
cannot make captures history-dependent; explicit true/false fold cases remain.

`os.exit` is deliberately **not executed** here: host-terminal-outcome/close
semantics belong to Rust tests. Real wall/CPU values, random temporary names,
PID/address/shell-version observations are never frozen. Snapshots, replay,
Wasm, waits, quotas, resource policy, official-suite progress and performance
are separate implementation gates.

## Contract for the Moonseed runner

`tools/hostlib_corpus.py --check` chooses the per-group runner options from
[`runner_profiles.json`](runner_profiles.json). No external wrapper or
`--engine-arg` is needed. Build `moonseed-run` in `CARGO_TARGET_DIR`, or set
`MOONSEED_RUN` to the binary to compare. Every native group uses
`--host=native:.` rooted at its fresh fixture copy, with IO, OS, loading and
package libraries installed through public builder APIs. Additional options
are selected only by the groups that need them:

| Groups | Additional runner options |
| --- | --- |
| os_misc | `--host-environment=process` |
| os_new_york | `--host-civil=fixture-tz` |
| os_fs | `--host-fixture-tmp` |
| process | `--host-process` |
| arg | `--lua-args`, followed by the three fixed Lua arguments |

The table also specifies each group's frozen timezone and stdin fixture;
the harness verifies these against the generator. The New York adapter uses
fixed fixture rules, rather than a system timezone database. Temporary-name
mapping preserves the PUC `/tmp/lua_` spelling while keeping native secure
reservations inside the fixture. Process authority is enabled only for the
process group. Native stdin comes from the supplied bytes; the VFS profile
copies those bytes into its memory stdio capability.

All eleven `io_*` groups run **both native and VFS** by default: 1,620 native
records plus 949 VFS records. VFS results appear as `GROUP@vfs`, with separate
raw batches and counts. The VFS fixture adapter seeds the fixture files and
directory namespace and uses the frozen Linux missing-file error spelling.
`--profile=native` checks the 23 native groups only; `--profile=vfs` checks
the eleven IO groups only. VFS does not claim process or timezone acceptance.

The arg group supplies `arg[-2]=nil`, `arg[-1]='hostlib-engine'`,
`arg[0]='driver.lua'`, `arg[1]='alpha'`, `arg[2]=''`, `arg[3]='omega'`,
`arg[4]=nil`; chunk varargs are the same three positive values. Configuration
options never enter this table. The runner uses the public `install_arg` and
`start_call` APIs.

Repeated `--engine-arg=FLAG` remains an optional override, appended after the
automatic profile options. For example, `--engine-arg=--host=native:.`
selects that filesystem even for a VFS-labelled comparison; such an override
is recorded in the result receipt and does not qualify the default VFS
profile. `--lua-argv` remains available for manual comparisons. PUC capture
and self-check never receive Moonseed options and still check exactly the
1,620 frozen records.

Tool paths are configured by environment variables; see
[portable tool configuration](../../tools/PATHS.md) for defaults.
