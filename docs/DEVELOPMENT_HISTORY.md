# Changelog

## Host capabilities, IO/OS and filesystem loading (Phase 3.37)

Implemented (2026-10-05). Six optional filesystem, stdio, clock, civil-time,
environment and process capabilities provide IO/file methods, lines iterators,
OS, loadfile/dofile, package.searchpath and a filesystem Lua searcher.
Library installation grants no authority; `install_arg` is explicit. External
reads are journaled, mutations deduplicated with exact request identity, and
Pending operations resume through typed host completion. VFS files rebind;
live native files/pipes refuse checkpoints. `os.exit` reports a terminal
host outcome, with checkpointable closing, and never kills the process.

Snapshot schema **24 → 25**; fuel **7**, bytecode **14**, tables **4**, GC
policy **12** and binary chunk format **2** are unchanged. Hostlib passes
**1,869/1,869** and diagnostics **3,815/3,815**; hooks and UTF-8 retain their
acceptance counts. Native/Wasm portable-host checkpoint/replay gates pass.
The unmodified official suite rises **8 → 14 PASS** of 33 compiled files,
with 19 LUA_ERROR files. Review fixes resource-policy/snapshot, IO, loader,
runner namespace and exit issues. Native confinement retains a TOCTOU gap.

F1 adds charged checkpointed file read-ahead, restores hot-loop register use
and removes cached-require Lua allocations. Final nonempty Ir geomean versus
`45facb7` is **+0.121252%**; startup remains **+12.2300%**. Line reads use
50 rather than 100,001 capability reads, but diagnostic wall ratios remain
4.0–4.5x PUC. C locale only, no C modules, and write-through `setvbuf` hints
remain explicit differences (`files.lua:675`). Next: 3.38 release closure,
including long-string `%p`, buffering policy, c/r hooks, startup and IO/module
wall costs, and async capability ergonomics. See
[ADR 0060](docs/adr/0060-host-capabilities.md),
[LUA_COMPATIBILITY.md](docs/LUA_COMPATIBILITY.md#io-and-os-phase-337) and
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-337-host-capabilities).

## Lua 5.4.9 UTF-8 library (Phase 3.36)

Implemented (2026-10-05). The six-field `utf8` module provides `char`,
`charpattern`, `len`, `codepoint`, `offset` and `codes`, with stable strict/lax
iterators and one encoder shared with lexical escapes. Strings remain bytes:
strict decoding accepts Unicode scalar values; lax decoding and `char` support
Lua's extended encoding through `0x7fffffff`. No normalization, grapheme
handling or case folding. Work uses the existing bounded builtin engine,
fuel, quota accounting, hooks and mid-operation checkpoints.

Snapshot schema **23 → 24** for `Work::Utf8`; bytecode **14**, tables **4**,
fuel **7**, GC policy **12** and binary chunk format **2** are unchanged.
The frozen UTF-8 corpus matches **508,865/508,865** cases. The review fixed one
snapshot-integrity defect with tampered-image regressions; checkpoint, quota,
hook, native/Wasm, workspace and oracle gates pass. The official suite rises
**6 → 8 PASS** of 33 compiled files (`pm.lua`, `utf8.lua`), with 23 LUA_ERROR
and 2 COMPLETED_WITHOUT. Fixed-corpus Ir geomean moves **+0.058%**, worst
ordinary workload **+0.216%**; Gate N wall measurements are diagnostic. See
[LUA_COMPATIBILITY.md](docs/LUA_COMPATIBILITY.md#utf-8) and
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-336-utf-8-library).
The planned IO/OS/filesystem follow-up is completed in Phase 3.37 above.

## Lua and host debug hooks (Phase 3.35)

Implemented (2026-10-05). `debug.sethook/gethook` support call, tail-call,
return, line and count events, per-thread settings, PUC coroutine inheritance,
transfer windows, suppression and catchable hook errors. Lua hooks are
non-yieldable. Symbol-registered synchronous host hooks inspect the interrupted
activation and may yield zero values for line/count in yieldable coroutines.
Snapshots preserve pending delivery, Lua hook bodies and their host waits,
and suspended host-hook continuations. Restore requires host symbols and
validates hook state before construction. Panicking host callbacks fail closed.

Snapshot schema **22 → 23**; fuel revision **6 → 7** adds one unit per delivery.
Bytecode **14**, tables **4**, GC policy **12** and binary chunk format **2**
are unchanged. The frozen hook corpus passes **151/151** acceptance cases;
absolute PUC count positions, multiline-local concatenation line events,
private registry `_HOOKKEY`, MSC chunks and unsupported C-hook yields retain
explicit differences. The official suite remains 6 PASS of 33 compiled files,
with 25 LUA_ERROR and 2 COMPLETED_WITHOUT; hook-related T harness support
advances coverage. Diagnostics remain 3,813/3,815.

Final hooks-off Callgrind Ir geomean is **+0.116434%** versus `eee7dff`;
`alloc_churn` is +1.222328%, mostly allocator work. `run_hot` grows 12 bytes;
warmed delivery allocates zero per event. Enabled hooks use the cold executor
and have substantial measured overhead. See
[LUA_COMPATIBILITY.md](docs/LUA_COMPATIBILITY.md#debug-hooks-phase-335),
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-335-debug-hooks), and
[ADR 0059](docs/adr/0059-portable-debug-hooks.md).

## Lua 5.4.9 diagnostics (Phase 3.34)

Implemented. Runtime errors now use Lua source positions and operand names;
`error` levels, native and library argument errors, method `self`, compile and
lexer errors, and stripped-chunk degradation follow the pinned Lua 5.4.9
corpus. Native tail calls retain their Lua caller. Flat expression and suffix
chains lower iteratively; genuinely nested expressions keep a 300-edge safety
limit. The final diagnostic corpus matches 3,813/3,815 cases; the two remaining
cases call absent `package.searchpath`. The official suite compiles 33/33 files
and rises from 4 to 6 PASS (`constructs.lua`, `math.lua`). Successful workloads
move −0.073% in Callgrind instruction geomean versus Phase 3.33, with every
ordinary workload within ±0.725%. No diagnostic sidecar or format revision was
added. See [LUA_COMPATIBILITY.md](docs/LUA_COMPATIBILITY.md#errors),
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-334-diagnostic-cost), and
[ADR 0058](docs/adr/0058-diagnostic-authority-and-static-provenance.md).

## Compact activation records and call ABI (Phase 3.33)

Implemented, Outcome B. Native `Frame` shrinks 72 → 40 bytes; cold
continuations and callback boxes are recycled, stack/frame storage is reused,
and fixed Lua calls switch frames in the hot loop. Warmed fixed 0/0 calls fall
687 → 445 instructions (−35.2%); the 21-workload corpus geomean improves
2.703 → 2.575x PUC. The 250-instruction and 2.2x targets remain unmet, so
further call work stops pending the narrow proposal in [ROADMAP.md](docs/ROADMAP.md).
Snapshot and bytecode revisions are unchanged. Results and attribution:
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-333-activation-record-and-call-abi)
and [ADR 0057](docs/adr/0057-compact-activation-records.md).

## Interpreter performance campaign (Phase 3.32)

Implemented, Outcome B: revision-2 corpus Ir-ratio geomean 5.300 → 2.701
at `d9154eb`, a 1.962x improvement, missing the minimum 2x gate by about 2%.
The earlier loaded paired-wall comparison improves descriptively by 2.25x.
Validated hot-core windows, numeric/upvalue/frame paths, derived table/string
caches, immediate builtins/iterators and reused embedding buffers reduce work;
warmed scalar host calls and borrowed continuations allocate zero. Bytecode is
14 (ArithK then CompareBranch); snapshot 22, tables 4, fuel 6, GC 12 and chunk 2
remain. Compiler reductions change per-source fuel totals and pause boundaries.
Two review blockers are fixed; the official suite's blockers are unchanged.
Full before/after tables, rejected designs, proof limits and remaining hotspots:
[PERFORMANCE.md](docs/PERFORMANCE.md#phase-332-interpreter-performance-campaign)
and [ADR 0056](docs/adr/0056-interpreter-core-and-execution-caches.md).

## Lua frame setup cleanup (Phase 3.32, lane CAL)

Direct Lua calls use the installed prototype's parameter count without repeating
its validated register clamp. Metamethod frame setup clears existing scratch
slots and lets stack resize initialize new registers once. Canonical stack and
frame images, fuel, charging and continuation boundaries are unchanged.

Against 2ded2ae, Call+Return0 falls 658 -> 654 Ir and whole-program corpus
instruction geomean falls 0.052%. Every workload and both actual Moss host sizes
meet +1%. Broader frame-code caches and return fusion were rejected after
instruction profiling. The 400-Ir aspiration remains open. Evidence and gates:
`results/cal/RESULTS.md`.

## Metamethod calls and coroutine transfers (Phase 3.32, lane MM)

Direct non-vararg Lua `__call` handlers use one checked frame setup. Coroutine
resume/yield transfers copy existing result windows in bulk and decode ordinary
yield call sites once. Plain completed metamethod continuations reuse one
runtime-owned box; live continuations stay on frames and restore starts with
no spare. Callable chains, native/vararg handlers, close continuations,
short restored stacks and fault reserves retain their general paths. Fuel,
canonical frames, stack contents and snapshot revisions are unchanged.

A yielding fixture changes handlers while suspended, checks debug names and
open/padded/discarded transfers, and restores every step in all three modes.
It also runs against PUC Lua and in the native/wasm coroutine fingerprint.
Instruction comparisons and gate logs: `results/meta2/RESULTS.md`.

## Cheaper Lua call/result windows (Phase 3.32, lane CC)

Lua frame helpers avoid redundant nil writes and no-op growth calls, specialize
discarded and single-result returns, and drop finished frames in place. Retained
stack slots and snapshot images keep their exact meanings; Frame layout, fuel,
bytecode and GC revisions are unchanged.

Against 9ddf98f, Call0 falls 696 ->661 Ir and corpus sum Ir falls 0.852%.
Fib, closures, method calls and tail recursion improve; all workloads meet +1%
and Moss frame Ir is effectively unchanged. The 400-Ir goal remains unmet.
Evidence and correctness gates: `results/callpath/RESULTS.md`.

## Tail calls, open windows and Lua metamethods (Phase 3.32, stage 5)

Eligible TailCall replaces the core frame in constant space; Lua Call/Return
accept open argument/result windows. Direct Lua metamethod setup and plain
return continuations use frame helpers while preserving the canonical MetaCall
at every pause, yield and restore. Closes, varargs, natives, callable-chain
argument shifting and fault reserves retain their fallbacks. All three core
modes compare snapshots, fuel, output and GC state; yielding debug handlers
also checkpoint and restore at each instruction in every mode.

Against 2a6df86, Callgrind Ir falls 29.583% for tail_recursion, 24.173% for
metamethods and 3.751% for the corpus sum. Every workload and both Moss sizes
pass the +1% gate. No fuel, bytecode or snapshot revision changes.
Evidence: `results/hotcore/stage-5/REPORT.md`.

## Compare-and-branch (Phase 3.32)

`CompareBranch` evaluates branch-only comparisons without materializing a
boolean. Comparison kind, original operand order and branch sense survive
yielding or waiting metamethods through the existing VM-owned Truth continuation.
Value-producing comparisons retain `Compare`. `Op` stays 16 bytes.
Bytecode revision is 14; snapshots and binary chunks refuse revision 13.
Snapshot schema 22, chunk format 2, fuel revision 6 and GC policy 12 retain their
layouts and charging rules. A fused comparison/branch costs one fuel unit and
has one instruction pause boundary, lowering compiled source totals. Its debug
line is the condition's; locals/call PCs follow emitted code. Evidence and exact
fuel assertion changes: `results/cmpbr/RESULTS.md`; design: ADR 0055.
The sort fuel pins change 315 -> 307 (primitive) and 583 -> 568 (comparator).

## Fixed Lua Call/Return core (Phase 3.32, stage 4)

Eligible fixed Lua calls and returns change frames inside one core epoch,
preserving canonical result windows, fuel, stack charges and GC scheduling.
Natives decline by callee tag in the core before frame-helper setup. Cold
opcodes remain references until the epoch exits, reducing dispatch pressure.
Pending remains boxed (native Frame 120 ->72 bytes) after corpus/Moss comparison.
Snapshots, bytecode revision 13 and fuel revision 6 retain their meanings.

The revised performance gates pass: corpus Ir -5.449%, worst workload +0.919%,
native_calls +0.477%, Moss +0.486%/+0.527%. Call+Return0 is 690 Ir; 400 is a
remaining goal, and the 320-byte core stack guard is waived by the coordinator.
Stage 5 was not attempted. Evidence: `results/hotcore/stage-4/REPORT.md`.

## Constant-operand arithmetic (Phase 3.32)

`ArithK` embeds one exact integer literal, removing its register load
while preserving operand order and shared arithmetic/metamethod behavior.
Bytecode revision is 13; snapshot schema 22 and binary-chunk format 2 retain
their layouts and reject mismatching bytecode revisions. Fuel revision remains
6; compiled source totals and suspension boundaries can fall. Operator lines
and metamethod names remain, while a removed literal's line can disappear from
active lines. Results and exact assertion changes: `results/arithk/RESULTS.md`;
design and compatibility: ADR 0054.

Float immediates are deferred after their required mixed-type cold path failed
the instruction-count regression gate. Exact sort fuel totals change from
330 to 315 (primitive fixture) and 625 to 583 (comparator fixture); the compiler
limit fixture accepts 4,999 repetitions rather than 3,332 under its 10,000-op
emission cap.

## Destination-aware arithmetic compilation (Phase 3.32)

Final arithmetic results use their requested temporary slot directly. Scalar
assignments can also write an uncaptured local directly after evaluating both
operands, removing the result-copy chain. Captured locals (including captures
later in source), parallel assignments, and close/control operations retain
their stores and ordering. Branches and debug PC metadata follow removed copies.
New compilations use fewer VM instructions and lower per-source fuel totals;
the one-unit model and fuel revision 6 are unchanged. That destination-only
change kept stored bytecode compatible. Exact changed fuel assertions and corpus counts are
recorded in the lane's `results/dest/RESULTS.md`.

## 0.0.1

Unreleased. Phase-1 kernel plus result windows, assignment continuations, deletion-stable `next`, raw table length, a Lua 5.4 byte lexer, a compiler for one closure subset, `if`/`else` with upvalue closing on scope exit, a hot dispatch tier, bytecode validation of restored prototypes, primitive comparisons, `do`, `elseif`, `while`, `repeat`, `break`, numeric `for`, unary minus, table constructors, indexing, globals through `_ENV`, native functions as Lua values, table metatables with `__index`, `__newindex`, `__len`, and the base metatable functions, prototype-owned constant strings, deterministic automatic garbage collection, `__call`, every arithmetic, bitwise, comparison, and concatenation operator with its metamethod, Lua error values with stackless unwinding, `error`, `pcall`, and `xpcall`, catchable memory errors and stack overflow, a logical-heap quota (default 64 MiB), `local x <close>` with resumable `__close` calls on every scope exit, return, error unwind, and coroutine close, Lua 5.4's rule that a failed coroutine keeps its stack, generic `for` with the four-value initializer and a closing value, vararg functions and `...` on a frame layout that keeps extra arguments below the registers, vararg chunks with `Runtime::load_chunk_with_args`, `select`, a per-thread stack bound that snapshots share, proper tail calls, `and`, `or`, `not`, method calls, `f{...}` and `f"..."` call arguments, `function` statements, `local function`, `goto` with labels, and `<const>` locals; a Lua 5.4 language audit and an official Lua 5.4.9 suite baseline (`moonseed-compat`, `tools/lua_suite.sh`); the base library (`assert`, `type`, `tostring`, `tonumber`, `print` through a host output, `next`, `pairs`, `ipairs`, `collectgarbage`, text `load` with reader functions, `_G`, `_VERSION`), with base functions that call Lua resumable like `pcall`; the `math` library over a portable float backend, with `math.random` as snapshot state, and the `table` library as resumable machines, `table.sort` included; `#` without a scan; the `string` library, with a metatable per basic type (the string metatable), string arithmetic through it, a resumable Lua pattern machine, `string.format` in pure Rust, `string.pack` over one ABI, `gmatch` iterators as native closures, and `string.dump` with binary `load` of Moonseed binary chunks; the Lua registry, one module registry for every installer, `package` and `require` with the preload searcher, prototype debug information (chunk names, lines, locals, upvalue and call names) in snapshots and binary chunks, and the `debug` library's introspection and hooks, installed on request; the `coroutine` library over the existing threads, with exact transfers, Lua's statuses, `close`, `wrap`, yield boundaries, and restore checks of the thread graph; full userdata with their own metatables, user values, and byte payloads, light userdata as identity tokens, `debug.getuservalue`, `debug.setuservalue`, and `debug.upvalueid`, and host userdata types with typed borrows, logical charges, and portable-codec or snapshot-refusing policies (unstable API); weak tables and ephemerons, `__gc` finalizers with Lua's registration rule, order, resurrection, and errors as warnings, run as resumable calls, `Runtime::begin_close` for shutdown finalizers, and `warn` with a host warning sink; an incremental collector whose state is snapshot state and whose work is charged to fuel, with one write barrier in the heap's mutable accessor, bounded steps, a resumable atomic phase, and `collectgarbage`'s `incremental`, `setpause`, `setstepmul`, and `step`; generational collection, the default as in Lua 5.4, with object ages, old objects remembered through the same barrier, young collections that trace only young and touched objects, major collections as bounded incremental cycles, Lua's fallback after a bad major, `collectgarbage("generational")` and switching modes, and `Config::gc_mode`; a quota on the exact logical heap, and one generational validator for restore in every phase; Lua's argument-error wording in every library; numerals of any length; a scalability envelope with one resource model (`Limits`, `Config::max_string_bytes`, `Config::max_snapshot_bytes`, `Runtime::from_snapshot_with_limits`, `CompileLimits`), a million objects, strings and snapshots bounded by the quota rather than 1 MiB, compiler limits past 10,000 instructions, a bounded snapshot decoder, and a fixed PUC-versus-Moonseed benchmark corpus (`bench/`); a 0.1-candidate embedding API (ADR 0053, docs/EMBEDDING.md): rooted values, conversions, one error model, host functions and native closures, native-to-Lua continuations, host-to-Lua calls, host waits, rebindable userdata, a host module resolver, `Runtime::restore` with host checks, object lookup by id in constant time, ten examples, and a Moss integration harness (`integration/moss/`). Not a Lua implementation and not a stable API. Snapshot schema 23, bytecode revision 14, tables revision 4, fuel revision 7, GC policy revision 12, binary chunk format revision 2. Depends on `libm`. Tags 31–53 are `LoadBool`, `CloseUpvalues`, `JumpIfFalse`, `Compare`, `Neg`, `ForPrep`, `ForLoop`, `Index`, `SetIndex`, `GetField`, `SetField`, `SetList`, `Len`, `Arith`, `BNot`, `Concat`, `MarkClose`, `CloseScope`, `CloseThread`, `GenericForLoop`, `TailCall`, `ArithK`, and `CompareBranch`.
