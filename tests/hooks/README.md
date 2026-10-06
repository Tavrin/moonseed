# Frozen Lua 5.4.9 hook corpus (Gate 0)

`tools/hooks_generate.py` is the source of truth for fixed `(id, chunk, source,
mask, count, acceptance)` cases. `expected/*.txt` freezes PUC output;
`manifest.json` records group sizes, record hashes and oracle/tool identities.
The coordinator commits these files. This lane changes no Moonseed crate code.

The pinned oracle is Lua **5.4.9 without LUA_COMPAT_5_3**, with 64-bit Lua
integers and 32-bit C `int`. All capture groups run through the C harness linked
to that pinned build's liblua.a: direct embedding omits the Lua CLI's private
`pmain` C frame, so recorded stack depths are comparable to moonseed-run.
The CLI executable hash also identifies the reference installation. Override executables only deliberately; capture
records their SHA-256 identities. Mask junk, NUL termination, numeric masks,
integer coercion and large counts preserve this build's observed behavior.

## Reproduce

From the repository root, build the external-hook harness (also documented in
its source header):

```sh
cc -O2 -Wall -Wextra -Werror \
  -Ivendor/lua-5.4.9/src \
  tools/lua54_hook_harness.c \
  vendor/lua-5.4.9/src/liblua.a \
  -lm -ldl -o results/hooks/lua54-hooks
python3 tools/hooks_corpus.py --capture
( ulimit -v 8000000; CARGO_TARGET_DIR=target CARGO_BUILD_JOBS=8 timeout 1800 cargo build -p moonseed-bench --bin moonseed-run )
python3 tools/hooks_corpus.py --check
```

Every Lua/Moonseed process has a 2,000,000 KiB address-space cap, 60-second
wall limit and disabled core dumps. Groups run in deterministic alphabetical
order, cases in definition order. Every engine runs the **same driver bytes**,
from the results directory using relative `driver.lua`. Case sources are loaded
as `@hooks.lua`; inner functions use `@victim.lua` or `@strip.lua`. The case table
occupies one driver source line, keeping driver line numbers independent of
batch size. There are no absolute paths or pointer addresses in frozen records.

Overrides:

| Setting | Meaning |
| --- | --- |
| `MOONSEED_LUA54` | reference CLI executable for installation identity |
| `MOONSEED_LUA54_HOOKS` | C-hook oracle executable |
| `CARGO_TARGET_DIR` | defaults to lane `target-g0`; locates debug Moonseed binary |
| `MOONSEED_RUN` | explicit Moonseed runner (same native names for external cases) |
| `MOONSEED_HOOK_RESULTS` | results directory; default lane `results/g0` |
| `--report PATH` | check Markdown report and adjacent JSON report |

`--capture` validates all groups before replacing expected files. Run it twice
and compare every expected file and manifest byte. `--check` validates frozen
records, retains group actual stdout/stderr in results, and writes a **counts-only**
baseline. Exit 0 means acceptance groups match; 1 means mismatches/process failures;
2 means tooling, capture or frozen-input failure. Acceptance groups must pass on the current runner.

## Byte protocol

Each newline-terminated ASCII TSV record is:

```
case.id<TAB>sequence<TAB>kind<TAB>lowercase-hex(payload)
```

Sequence begins at 1 within each case. `H` is a hook event, `O` is one program
`print`'s output bytes (tab-separated arguments plus newline), `S` is final
`ok|returned-values...`, `error|error-value`, or `compile|error-value`.
The runner requires ordered known IDs, contiguous sequences and one terminal
record per case. Errors are not normalized: string bytes are preserved exactly.
Print replaces opaque pointer rendering with deterministic identity tokens;
primitive print arguments keep Lua's `tostring` bytes. No case uses IO/OS/UTF-8.

`H` payloads use `|`-separated typed values in this exact order:

```
event, line-or-nil, name, namewhat, what, short_src, currentline, linedefined,
istailcall, ftransfer, ntransfer, nparams, isvararg, depth:N, transfers...
```

The event recorder obtains `debug.getinfo(2,"nSltur")` directly inside the Lua
hook, before calling any helper or optional behavior callback. Depth counts all
levels from 2 upwards, including the fixed driver/protected-call frames. On call
and return, each transfer is `typed-local-name=typed-value` from
`debug.getlocal(2,ftransfer+i)`, with zero-based `i < ntransfer`. Nil holes survive.
Strings are `string:<hex bytes>`, booleans `boolean:true|false`, numbers
`number:<%.17g>`, nil `nil`. Opaque values are `type:<encounter ordinal>` per case,
using identity without invoking their metamethods. No integer/float tag is added:
these are Lua `type` plus a printable value. Event line nil differs from the
getinfo currentline sentinel -1. All installed trace hooks use the same recorder;
semantic-only count hooks below intentionally emit summaries instead of events.

Recording is buffered while hooks are active, then printed after removal;
recording itself cannot generate nested hooks. The driver includes installation,
removal and its protected-call boundary in unfiltered traces. The fixed driver
is part of the oracle surface; edit it only with deliberate corpus recapture.
Twenty thousand buffered records/resumes is a secondary runaway guard.

The unsupported C call/return yield probes each run in their own capped process.
A signal termination yields terminal kind `P`, with payload
`signal:NAME|stdout:<hex>|stderr:<hex>` from the common process runner. This is a
process observation, not a Lua error or an invented successful hook result.
Their observed `SIGSEGV` is reproducible on the pinned non-asserting PUC build.

## Groups and acceptance

| Group | Cases | Acceptance | Coverage |
| --- | ---: | --- | --- |
| `api` | 37 | exact | removal/replacement, thread forms, gethook, mask/coercion/argument errors, all requested count sizes |
| `calls` | 26 | exact | ordinary/nested/method/native/builtin callbacks, Lua and builtin tails, deep recursion, transfers, varargs, stripped |
| `close_gc` | 5 | exact | close on return/error/goto/coroutine.close; no hook events inside GC finalizer |
| `coroutines` | 5 | exact | resume/yield/wrap, suspended installation, per-thread hooks and inherited wrapper settings |
| `count` | 13 | semantic summaries | firing/stopping, reentrancy suppression, replacement/reset, interval retrieval and stress |
| `count_positions` | 32 | oracle only | absolute count traces for controls/intervals, count+line ordering, long-hook/sethook counter behavior, count errors/yield |
| `errors` | 16 | exact | call/return/tail/line errors through pcall/xpcall/resume/wrap, installed-hook recovery |
| `external` | 7 | exact/semantic summaries | gethook, line yields and correct resume, suspended inspection/local edit, transfers; count intervals 1/2 finish with four Lua calls (no duplicate entry) |
| `external_positions` | 8 | oracle only | full count-yield traces at intervals 1/2, call+count, line+count, simultaneous yielding masks |
| `external_unsupported` | 2 | oracle only | attempted C call/return yield: pinned PUC process behavior |
| `lines` | 23 | exact | if/elseif, loops, break, forward/backward/same-line goto, short-circuit, multiline call/expression/constructor/return |
| `lua_yield` | 4 | exact | Lua call/return/tail/line hooks cannot yield; count counterpart in count_positions |
| `metamethods` | 6 | exact | function __index, __add, __call, __eq, __lt, __concat; __close in close_gc |
| `reentrancy` | 9 | exact | hook calls Lua, debug, pcall, metamethod, native; long body, replacement/removal/error |

**193 cases total; 151 acceptance cases; 42 oracle-only observations.** Absolute
PUC instruction counts are not a Moonseed acceptance target. `count` never emits
an absolute event count, resume count, interrupted PC or instruction-dependent
trace: it checks flags, count retrieval, deterministic program results, and
suppression/removal. Large intervals report their settings and successful loop
results without assuming whether a different VM reaches the threshold. Count
errors and Lua count-hook yields currently carry full traces in `count_positions`;
their non-yieldability/error semantics still guide runtime tests. Position-only
groups are reported but cannot turn an otherwise passing acceptance check into
a failure. Invalid frozen input always fails the tool.

## External native contract

`tools/lua54_hook_harness.c` installs these globals. A future Moonseed runner
should register the same names through its public host-hook API; no C API is
required in Moonseed.

* `chook([thread,] mask, count, yieldmode)` replaces that thread's hook and resets
  its counter. Default thread is the active one. Mask recognizes `c`, `r`, `l`
  like PUC (other chars ignored, C-string NUL termination); positive count
  independently enables count events. It returns a fresh mutable array of event
  tables, appended before any yield. Installations on threads are weakly keyed;
  installing a hook alone does not keep the thread alive. Old arrays remain
  readable after replacement/removal. Mask and count use PUC string/integer
  argument conversions; count is cast to the pinned 32-bit C `int`.
* `yieldmode` is required: `none`, `line`, `count`, or `both` (line and count).
  Each selected event attempts `lua_yield(L,0)` after recording, with no values
  or continuation. Resume must execute the interrupted instruction without
  redelivering the same event. Deliberate negative-test modes `call` and `return`
  attempt the same operation at those event kinds; they are outside Lua's legal
  C-hook yield contract and currently crash PUC. Moonseed should reject illegal
  yields, not reproduce this crash. Unknown modes raise an argument error.
* `chookget([thread])` delegates to `debug.gethook` and returns its exact arity:
  nil when absent; `"external hook", canonical-mask, base-count` for this C hook;
  or the Lua hook function/mask/count if subsequently replaced by debug.sethook.
  The wrapper and its debug.gethook invocation are ordinary native calls.
* `chookoff([thread])` removes any installed hook, resets count/mask, returns
  nothing and leaves previously returned arrays intact.

Each C event table has `event`, `line` (nil except nonnegative line events), all
11 getinfo fields above, `depth`, and `transfers`. The C hook gets info using
`lua_getinfo("nSltur")` on the interrupted activation, counts stack levels from
C level 0, and enumerates the same call/return transfer ranges. Each transfer
table has `name`, `value` (a nil value remains an entry with a name). `C(rows)`
emits these through the common `H` encoder after execution. No Lua hook frame
exists in C-hook traces. A coroutine suspended there is inspected with
`debug.getinfo(co,0,...)` / `debug.getlocal(co,0,i)` / `debug.setlocal(co,0,i,v)`.
The inspection fixture preserves varargs and verifies the resumed edited result.

Review cases cover native return stack positions and multiline field/logical reads.
The stripped-function fixture disables hooks during dump/load preparation: binary
chunk bytes intentionally differ between VMs; the stripped function itself remains
fully traced. No records or transfer fields are normalized.

Tool paths are configured by environment variables; see
[portable tool configuration](../../tools/PATHS.md) for defaults.
