# Performance

The early kernel measurements below are local Moonseed measurements. Later
phase sections compare Moonseed with PUC Lua and other interpreters using
separate corpus and call probes.

## Release instruction counts

Recorded 2026-10-05, with Moonseed source `0caed9fef9f9bcdcf7c64d3c768f041a9ce1a5f3`.
These measurements precede the final API and packaging changes; they are not a
measurement of every subsequent release-candidate build. The whole-process
Moonseed/PUC instruction geometric mean is **2.5795×** over 21 nonempty workloads.

| Moonseed / comparator | Nonempty workloads | Geometric mean | Median | Best | Worst |
|---|---:|---:|---:|---:|---:|
| PUC Lua 5.4.9 | 21 | 2.5795 | 2.4996 | 1.7816 | 4.2149 |
| omniLua 0.7.1 | 21 | 1.2656 | 1.2708 | 0.8331 | 2.1797 |
| Luna 4.0.1, interpreter | 21 | 0.7017 | 0.6332 | 0.1684 | 2.0615 |
| Piccolo 0.3.3 | 16 | 0.5635 | 0.4571 | 0.3097 | 1.4756 |

Lower means fewer executed host instructions. All 22 workloads, including the
empty startup probe, were attempted on all five engines. PUC, Moonseed,
omniLua and Luna matched outputs on 22/22; Piccolo on 17/22. Piccolo's missing
library calls exclude strings, numeric_loops, patterns, application and
string_format. Failed-run counts are excluded. On the common 16 nonempty
workloads, Moonseed/comparator geometric means are 2.6882 (PUC), 1.2617
(omniLua), 0.6276 (Luna) and 0.5635 (Piccolo).

Method: Callgrind 3.25.1, revision-2 corpus, identical divisor-100 generated
sources, logical CPU 4 on an AMD Ryzen 9 7945HX. Engines were interleaved per
workload; accepted cells require checksum-equal output. Counts include process
startup, compilation, library setup, execution and teardown. Aggregates exclude
the empty startup probe, but do not subtract startup from the other workloads.
Scaling changes nested work and working sets; multiplying counts by 100 does
not estimate full-size execution. Each cell is one retained instruction run.

Moonseed uses Rust 1.98.1 / LLVM 22.1.8, optimization 3, one codegen unit,
fat LTO, line tables and empty RUSTFLAGS (`bench-stable`). Reused comparator
binaries: omniLua at `90dcf85a8cfb95305b77056af00a114fd0df30a1` (fat LTO,
one codegen unit), Luna at `f90669dfa50f24bc05d33e3091deb1bb0ef0b37b`
(LTO, 16 codegen units, `--no-jit`), Piccolo at
`ce709eb1dae5c543cbc78e7e12bb80249d88c55f` (default release, no LTO,
small public-API runner). PUC is 5.4.9 without compatibility macros, invoked
with `-E`; its original compiler flags are unknown.

| Binary | SHA-256 |
|---|---|
| Moonseed runner | `55053af0c00c4751a45cc1762bea77e2e53eba1394c51776168a8a3d15d87bf7` |
| PUC Lua | `3881230fd6ef8553c379fc273112dbe3328b582775a010c4997fbe178dae0abe` |
| omniLua | `9148c652d4f0d52c3af46ad79a88c6b2d62500440e0f21fc8a7f11c686db8c04` |
| Luna | `18c377b0e3b9c565a8377a427981f85059519e16866d367310553367f57de277` |
| Piccolo runner | `4096eb3ac2f32e19a7a6188f500b8c0a53431bb77a6c4293baf9c0266319e9e9` |

The machine was shared, with one-minute load averages 48.95–65.09 during
instruction collection. These counts do not establish elapsed-time ratios,
quiet-machine ranking or application capacity. Controlled wall measurements
remain pending. See [the benchmark protocol](../tools/BENCHMARKING.md) for
collection tools and [the corpus](../bench/README.md) for workload definitions.
The sections below retain historical measurements and their original scope.

## How the numbers were taken

`cargo bench -p moonseed --bench kernel --features measure`

- Profile: `bench` (`debug_assertions` off)
- rustc 1.98.1 (`48a229cea`, 2026-09-01)
- x86_64, AMD Ryzen 9 7945HX, Linux
- Parent commit at measurement time: `0507ffc` (the harness itself is the commit that adds this file)
- Each loop workload: 1 warmup run, then 20 timed runs, median
- `rewind_entry` and, for allocation churn, `collect` sit outside the timer
- The result register is checked after warmup and after the last timed run
- A timed run that grows the stack or frame vector reports `stack_grows` / `frame_grows`

`Value` is 16 bytes, `Handle<u8>` is 8, `Frame` is 96, alignment 8.

## Measured

| Workload | Median | What that is |
|---|---|---|
| Checked handle lookup | ~1 ns, 1.35× a raw slot read | 20,480,000 lookups. Same conclusion as Phase 1 |
| Integer table, 1k keys | insert ~143 ns, get ~12 ns | Direct `Table` calls, not the interpreter |
| Arithmetic loop | 42 ns/iteration | 100,000 iterations, 3 charged instructions each, ~14 ns/instruction |
| Branch loop | 71 ns/iteration | 100,000 iterations, 5 charged instructions, taken and untaken tests |
| Scalar Lua call | 118 ns/call | 20,000 calls, `stack_grows 0`, `frame_grows 0` |
| Nested Lua call | 201 ns/outer call | The outer call performs one inner call. Same zero growth |
| Multi-result call | 158 ns/call | Returns `10, nil, 30` and adds the two integers. Zero growth |
| Upvalue increment | 148 ns/call | Shared open upvalue. Zero growth |
| Constant-string field | 195 ns/update | Get, add one, set. One `LoadBytes` per run, not per iteration |
| Integer field | 163 ns/update | Same loop, key `1` |
| Table allocation | 46 ns/table | 2,000 `NewTable`s, collection between runs, not inside the timer |
| Host `tick` | 89 ns/call | Returns `arg + 1`, no journal write |
| Arithmetic, quantum 1 | 70 ns/iteration | Same loop as the 42 ns row, 20,000 iterations |
| Canonical program | ~115 ns/charged instruction | 60 instructions, ~6.9 µs/run including allocation. Not a Lua benchmark |
| Canonical snapshot | encode ~9.7 µs, decode ~15 µs | 1,564 bytes, 31 live objects |

Quantum 1 on the arithmetic loop is about 1.7× the unbounded quantum. It still finishes and the register matches. That is the cost of pausing every instruction, not a reason to batch fuel yet.

## What the call path does

A warmed scalar call, a nested call, and a multi-result call do not grow the stack vector or the frame vector. Arguments and results stay in the existing register window. Returning does not allocate a `Vec<Value>`.

`Frame` is 96 bytes because `Pending` carries a `String` for the host symbol, and every frame reserves that enum. Pushing a frame copies those bytes. On this machine that copy is not visible as allocation and is not a large fraction of the 118 ns call. It was left in place.

Checked handles stay. A ~1 ns check is small next to ~14 ns for an arithmetic instruction and ~160–195 ns for a field update.

## Not a target

Nothing here is a promise about speed relative to another runtime. The addendum's 1.10× / 1.25× figures are not acceptance gates for this tree.

## Later, only if a profile still shows it

Constant-string field update was the slowest non-allocating loop in the Phase 1.5 run. The Phase 2 remeasurement is below. A cache could remember a slot index in the entry vector, not the value. It has to miss when that slot's value changes, when the slot becomes a dead anchor, and when an absent-key insert compacts and moves live slots. It also has to miss when a metatable or `__index` / `__newindex` would change the answer. Those metamethods do not exist yet. On restore the slots are rebuilt and the cache must be empty, because it is not snapshot state. The cache is still not implemented.

## Phase 2 table operations

Same machine and the same harness shape as the table above, after dead anchors and raw length. The timed interpreter is `a0b9e2b`. The harness is `8e03dda`. The bench process printed `309cef5` because it ran before those commits; the sources match this pair. The loop rows were remeasured so a table-representation change could not hide inside the old field numbers.

| Workload | Median | What that is |
|---|---|---|
| Integer table, 1k keys | insert 101 ns, update 12 ns, delete 19 ns, get 11 ns | Direct `Table` calls. Delete leaves the anchor. 1000 dead anchors remain |
| `next` over live entries | 27 ns/step | 4096 integer keys |
| `next` after deleting the current key | 40 ns | The successor link still names a live slot |
| `next` across 4095 dead anchors | 6.5 µs | One call. About 1.6 ns per skipped slot. One fuel unit |
| Raw border, 1024-key sequence | 11 µs | Border 1024. One probe per integer up to the border |
| Raw border, keys 1 and 3 | 21 ns | Smallest border 1 |
| Raw border, key `10^12` only | 9 ns | Border 0. One slot, not a trillion |
| Canonical snapshot | 1631 bytes, encode 8.6 µs, decode 17 µs | 31 live objects. Phase 1.5 was 1564 bytes |
| 1000 live integer entries | 23347 bytes, encode 102 µs, decode 206 µs | |
| 1000 dead anchors, no live entries | 14373 bytes, encode 62 µs, decode 147 µs | Retained until the next absent-key insert |

Integer field update through the interpreter was 155 ns/iteration. Constant-string field update was 182 ns. The Phase 1.5 run was 163 ns and 195 ns. Warmed scalar, nested, and multi-result calls still report `stack_grows 0` and `frame_grows 0` (123 ns, 210 ns, and 152 ns).

A repeated insert-then-delete of distinct keys keeps at most one dead anchor. Deleting every key and not inserting again keeps one anchor per deleted key. That is the state `next(t, deleted_key)` needs. The 1000-anchor snapshot is that case.

One `Next` is not constant work when the dead run is long. One raw-length call is not O(log n). Neither is chunked. A field cache is not added. Invalidation, once metamethods exist, is the list in the paragraph above plus `__len` for a cached border.

Shrinking `Pending` so a call copies less than 96 bytes is the other small candidate. It is not justified by the call numbers above.

Region fuel, inline caches, hidden classes, incremental collection, copy-on-write heaps, and a JIT are not in this tree. Clearing a future execution cache must not change the semantic continuation. At every safe point the continuation is already in the heap.

## Source frontend

Same machine, bench profile, commit `7f2d419`. The closure fixture is `crates/moonseed/fixtures/lua/closure_pair.lua` (308 bytes). Lex and compile medians are 200 runs. Execution is 20 fresh runtimes; boot is outside the timer. Fuel is one run, not a source-level compatibility promise.

| Workload | Median | What that is |
|---|---|---|
| Lex the fixture | 1.2 µs | 308 bytes |
| Parse and compile | 4.1 µs | 4 prototypes, 24 instructions, max register 7, 24 mapped spans |
| Run the compiled fixture | 761 ns | Fuel 32. Closures and upvalues allocated during the run |
| Run the hand-bytecode equivalent | 702 ns | Fuel 29, 22 instructions |

The compiled program is three instructions and three fuel units above the hand program. That is the copy that keeps the `make_pair` local alive across its call, not a pathological frontend. Integer and string field loops on this run were 163 ns and 184 ns, next to the Phase 2 figures of 155 ns and 182 ns. Warmed calls still report `stack_grows 0` and `frame_grows 0`. No inline cache, quickening, or superinstruction was added.

## Scope exit and `if`

Same machine, bench profile, parent commit `e262843`. Loops are 1 warmup and 20 timed runs, median. The close loops allocate `width + 1` objects per iteration and do not collect during a run, so the iteration count shrinks with the width.

| Workload | Median | What that is |
|---|---|---|
| `JumpIfFalse` taken | 43 ns/iteration | 3 charged instructions: the test, add, loop branch |
| `JumpIfFalse` not taken | 53 ns/iteration | 4: the fallthrough runs a filler instruction |
| Make a 1-capture closure, then close | 119 ns/iteration | vs 112 ns making it over a cell that is still open |
| 32 captures, then close | 701 ns | vs 281 ns reusing 32 open cells. The gap is 32 cell allocations plus the close |
| 200 captures, then close | 7.4 µs | vs 6.1 µs reusing 200 open cells |
| Compiled `branch_close.lua` | 611 ns | Fresh runtime. 5 prototypes, 36 instructions, fuel 30, max register 7 |
| Hand-bytecode equivalent | 611 ns | 29 instructions, fuel 27 |
| `if` with no capture | 111 ns | 6 instructions, fuel 6. No close emitted |

The close is linear in the thread's open list and charges one fuel. At the compiler's limit of 200 captures it stays in the microsecond range, so it is not chunked. The 200-capture baseline is 6.1 µs because `MakeClosure` looks up each captured slot with a linear scan of the open list. That is quadratic in the number of captures, and it predates this phase.

The compiled branch fixture has seven more instructions than the hand program. Four are moves from a temporary into an assigned local. One is the second `GetUpvalue` for `return x` inside `inc`. Two are a second copy of `function() return 0 end`, which the hand program shares.

### Regression from adding two opcodes

Dispatch-heavy loops got slower in this phase. The code paths they run did not change. Measured by alternating the two bench binaries, pinned to one core, three times each:

| Workload | Before (`e262843`) | After |
|---|---|---|
| Branch loop | 68–70 ns | 88–90 ns |
| Arithmetic, quantum 1 | 63–65 ns | 86–87 ns |
| Scalar call | 121–127 ns | 145–160 ns |
| Integer field | 156–160 ns | 174–178 ns |
| `next` over live entries | 26–28 ns | 27–28 ns |

A source bisect reproduces it with the base tree plus only the two new `Op` variants, and their arms, in the interpreter returning an error. Moving the variants to the end of the enum does not change it. `Op` is 16 bytes either way. Forcing function and block alignment does not close the gap. The base binary's `Runtime::run` is 14.4 KB with `call`, `do_return`, `make_closure`, and `exec_next` inlined into it. With the new variants it is 5.8 KB and those are separate functions. `#[inline]` hints did not restore the old shape. `perf` is not available on this machine, so the cause inside the loop is not pinned down further.

The work per instruction did not grow. What moved is how the compiler lays out a large `match`, and any later opcode can move it again. The next section fixes that.

## Dispatch structure (Phase 2C)

### Method

- `cargo bench -p moonseed --bench kernel --features measure`, `bench` profile, no extra `RUSTFLAGS`
- rustc 1.98.1 (`48a229cea`), x86_64, AMD Ryzen 9 7945HX, Linux
- Each build is an export of its commit, built into its own target directory
- The bench binaries run alternately, 7 rounds, each run pinned with `taskset -c 4`. Each run is 1 warmup plus 20 timed runs, and reports its median. The table shows the median of the 7 medians, with the range of the 7 in brackets. p95 is not reported: with 7 runs it would be one sample
- Builds: `pre` is `e262843`, before the two Phase 2B opcodes. `2B` is `97903d6`. `now` is this phase. `now+6` is `now` patched with `tools/cold_opcode_growth.py`: six extra opcodes that decode, validate, and have real `exec` arms, and that no workload runs

| Workload | pre | 2B | now | now+6 |
|---|---|---|---|---|
| Arithmetic loop | 62 [43–64] | 55 [53–61] | 9 [9–9] | 9 [8–9] |
| Branch loop | 69 [68–74] | 84 [82–88] | 14 [14–16] | 14 [14–15] |
| Arithmetic, quantum 1 | 61 [59–65] | 85 [83–86] | 48 [47–50] | 47 [46–61] |
| Scalar call | 124 [120–137] | 146 [144–154] | 75 [73–80] | 74 [73–77] |
| Nested call | 208 [205–225] | 249 [244–257] | 140 [138–149] | 138 [137–148] |
| Multi-result call | 151 [149–162] | 184 [180–221] | 82 [81–89] | 82 [82–89] |
| Upvalue increment | 141 [137–148] | 156 [152–164] | 105 [103–112] | 104 [103–111] |
| Integer field | 156 [154–163] | 177 [173–181] | 123 [122–129] | 124 [123–139] |
| Constant-string field | 182 [180–188] | 207 [201–211] | 152 [150–159] | 152 [150–163] |
| Host `tick` | 83 [81–91] | 93 [92–102] | 68 [67–70] | 69 [66–87] |
| `if` taken | — | 47 [44–49] | 9 [8–9] | 9 [8–10] |
| `if` not taken | — | 55 [54–59] | 11 [11–11] | 10 [10–12] |
| Compiled closure fixture | 782 [752–801] | 831 [802–1222] | 741 [712–752] | 791 [742–832] |

All in ns per iteration, call, or run. Fuel is identical to Phase 2B in all 24 workloads that report it.

The six extra opcodes leave the hot workloads unchanged within their ranges. Before this phase, the same patch applied to `2B` moved the arithmetic loop +20% and the compiled closure fixture +12%, and moved other rows by 5–10% in both directions. The closure fixture is the one row that still moves (+7%). Most of its instructions are cold (`MakeClosure`, `Call`, `Return`, upvalue access), and the new arms land in `exec`, where those run.

A 5-round pinned run of the close loops against `2B`:

| Workload | 2B | now |
|---|---|---|
| 1 capture, close / reuse | 138 / 133 | 92 / 66 |
| 32 captures, close / reuse | 790 / 304 | 791 / 277 |
| 200 captures, close / reuse | 7553 / 6303 | 10323 / 8076 |
| Compiled / hand `branch_close.lua` | 682 / 602 | 531 / 511 |

The 200-capture loop is slower. That loop is almost all the linear open-list scan in `make_closure`, repeated per capture, which now runs from the out-of-line `exec`. It is the quadratic capture lookup listed below. It is not a hot path, and it was not tuned here.

### What the code looked like

| Build | text | `run` | `run_hot` | `exec` |
|---|---|---|---|---|
| pre | 790,804 | 14,396 | — | inlined into `run` |
| 2B | 810,372 | 5,814 | — | inlined into `run` |
| 2B + 6 cold opcodes | 813,960 | 7,947 | — | inlined into `run` |
| now | 817,128 | 2,010 | 2,949 | 12,959 |
| now + 6 cold opcodes | 820,756 | 2,010 | 2,949 | 14,913 |

Sizes are bytes of the bench binary. The wasm probe is 313,570 bytes, from 306,395.

Before this phase, `poll` and `exec` were inlined into `run`, one function with one jump table. Which helpers LLVM pulled into that function depended on its total size. `pre` inlined `call`, `do_return`, `make_closure`, `exec_next`, and others, and called `store` out of line 12 times. `2B` called those helpers out of line but inlined `store`. Its stack frame went from 0x248 to 0x178 bytes, and `run` shrank to 40% of its size. In both builds every register operation still called an out-of-line `load` or `store`. Each call looked up the active thread and its frame again, and fetching the instruction took three arena lookups. The opcode count changed the inlining, and the inlining changed the speed.

`run_hot` is now its own function of 2,949 bytes, with a 0x88-byte stack frame and one jump table. Its only calls are the stack-growth path, one call through the dynamic-linking table, and `exec`. Its size does not change when opcodes are added. Adding six opcodes changed only `exec`.

`Op` is 16 bytes with alignment 8 in every build. The discriminant is a byte, and the 33 and 39 variant enums dispatch through the same kind of table. The representation was not the cause and was left alone.

### Structure

`Runtime::run` calls `run_hot`, then `poll` for anything `run_hot` hands back.

`run_hot` sets up locals from canonical state: the frame base, `pc`, the prototype's code slice, fuel, and the quantum. It checks the same things `poll` checks first: no trap, the thread `Ready`, no pending host call or assignment. The loop fetches `ops[pc]`, charges one unit, and calls `hot_op`. `hot_op` is the only implementation of `LoadNil`, `LoadInt`, `LoadFloat`, `LoadBool`, `Move`, `Add`, `Jump`, `JumpIfFalse`, and `JumpIfLt`. They touch only the frame's registers. Any other opcode writes the locals back to the frame and calls `exec`, which is `#[inline(never)]`. The locals are then rebuilt, because a call, return, yield, or resume may change the frame or the thread. A hot fault, the end of the quantum, and the fuel limit also write the locals back first. `poll` still reports the pause or termination, so the order of those checks has not changed.

Quantum 1 is the same loop with an allowance of 1. `exec` still runs a hot opcode, through `hot_op`, when `poll` gives it one, for example right after a pending assignment. No opcode has two implementations.

### Rejected

- Keep the single big `match` and tune `#[inline]` hints. The hints were ignored in Phase 2B, and the shape would still depend on the total opcode count
- Handler tables of boxed trait objects or function pointers. That makes every instruction an indirect call
- Computed goto, assembly, or `unsafe`. The runtime is `#![forbid(unsafe_code)]`
- Table ops, upvalue access, and calls in the hot tier now. They need the collector, the table arenas, or a frame change. That is the next step if a profile asks for it, not part of making the loop stable

### Deferred

`MakeClosure` looks up each `Capture::Local` with a linear scan of the thread's open list. That is quadratic in the number of captures: 8 µs for 200 captures in one closure, 6.3 µs in Phase 2B. No realistic script has that shape yet, so it is left for when one does.

## Comparisons and loops (Phase 3.6)

Same method as Phase 2C: exported builds, 7 alternating rounds pinned with `taskset -c 4`, median of medians with the range in brackets. `2C` is `a23f722`. `cold` is this phase with `Compare` only in `exec`. `hot` adds the two-number path to `hot_op`. `hot+6` is `hot` patched with `tools/cold_opcode_growth.py`. Loop rows are compiled from source and run in a fresh runtime. Each is ns per iteration: 20,000 iterations, or 2,000 for the capture loops, which allocate a closure and a cell per iteration.

| Workload | 2C | cold | hot | hot+6 |
|---|---|---|---|---|
| Arithmetic loop | 9 | 9 | 9 | 9 |
| Branch loop | 14 | 14 | 14 | 14 |
| Arithmetic, quantum 1 | 47 | 47 | 47 | 47 |
| Scalar call | 75 | 73 | 74 | 74 |
| Nested call | 140 | 140 | 141 | 140 |
| Multi-result call | 82 | 81 | 83 | 81 |
| Integer / string field | 123 / 154 | 124 / 151 | 127 / 155 | 126 / 151 |
| Host `tick` | 67 | 68 | 67 | 68 |
| Compiled closure fixture | 762 | 752 | 761 | 772 |
| `while i < n do i = i + 1 end` | — | 52 [52–60] | 25 [24–28] | 25 [24–28] |
| … plus `local t = i == c` | — | 82 [81–97] | 28 [27–29] | 28 [27–38] |
| … `i == 0.5` (mixed) | — | 84 [83–99] | 29 [28–29] | 29 [28–30] |
| … `i < c` / `i <= c` | — | 82 / 82 | 28 / 28 | 28 / 28 |
| `repeat ... until i >= n` | — | 52 [52–59] | 24 [24–25] | 24 [24–24] |
| `while` making one capturing closure | — | 140 [137–149] | 118 [116–175] | 114 [113–115] |
| `repeat` making one capturing closure | — | 148 [142–156] | 120 [117–165] | 118 [116–119] |
| inner loop that breaks at once | — | 55 [55–63] | 31 [30–33] | 30 [29–37] |
| … breaking across two nested blocks | — | 57 [57–65] | 34 [32–35] | 32 [32–33] |
| four-arm `elseif` chain, else taken | — | 167 [164–194] | 70 [68–72] | 69 [68–73] |
| `do local x = i end` per iteration | — | 59 [57–71] | 31 [30–31] | 30 [30–37] |

Executed instruction mix, from one quantum-1 run of the `hot` build: every loop instruction runs in the hot tier except the final `Return`, and in the capture loops `MakeClosure`, `CloseUpvalues`, and the `SetUpvalue`/`Move` around them (4,004 of 24,011 in the `while` capture loop). Before the promotion each plain loop ran one cold `Compare` per iteration: 20,002 of 160,005 instructions in the `while` loop, and those accounted for about half its time. Fuel is identical in `cold` and `hot` for every workload.

| Build | text | `run_hot` | `exec` |
|---|---|---|---|
| 2C | 817,128 | 2,949 | 12,959 |
| cold | 842,720 | 2,949 | 13,505 |
| hot | 845,932 | 5,017 | 13,639 |
| hot+6 | 849,544 | 5,017 | 15,464 |

Adding `Compare` to `exec` left `run_hot` byte-for-byte the same size. Promoting the two-number path grew it to 5,017 bytes, most of it the float/integer boundary code, and the hot workloads from Phase 2C did not move. Six more cold opcodes changed only `exec` again. The wasm probe is 332,418 bytes, from 313,570. Most of the text growth is the new benchmark programs and the comparison code.

A loop iteration costs about 3 ns per executed instruction, the same rate as the arithmetic loop. The compiler emits no fused compare-and-branch; `i < n` writes a boolean that `JumpIfFalse` then tests. That is the next obvious loop cost if a profile asks for it.

The capture lookup stays deferred. Making a closure with 1, 4, and 32 captures costs 67, 70, and 265 ns over already-open cells, and 86, 140, and 730 ns when they are closed and made again. The 200-capture case is 7.8–9.7 µs, and no fixture or loop comes near it.

## Numeric `for` (Phase 3.7)

Same method: exported builds, 7 alternating rounds pinned with `taskset -c 4`, median with range. `3.6` is `40f8936`. `cold` is this phase with `ForLoop` in `exec`. `hot` runs `ForLoop` in `hot_op`. `hot+6` adds the six unused opcodes. 20,000 iterations per loop; the capture loop runs 2,000.

| Workload (ns/iteration) | 3.6 | cold | hot | hot+6 |
|---|---|---|---|---|
| `for i = 1, n do s = s + i end` | — | 65 [63–68] | 27 [26–32] | 27 [26–36] |
| the same as a `while` | — | 37 [35–46] | 37 [35–45] | 36 [35–59] |
| step 2 | — | 74 | 36 | 37 |
| step −1 | — | 74 | 35 | 36 |
| up to `math.maxinteger` | — | 75 | 36 | 36 |
| float loop | — | 78 | 37 | 36 |
| capturing the control variable | — | 142 | 102 | 103 |
| inner `for` that breaks at once | — | 124 | 86 | 87 |
| body assigns the control variable | — | 84 | 47 | 45 |
| earlier workloads (arith, branch, calls, fields, host, `while`, comparisons, `elseif`) | | | | within noise of `3.6` |

The `for` loop runs 3 instructions per iteration against the `while` loop's 10, because `ForLoop` replaces the compare, the conditional jump, the increment, and the backedge. Cold, the one `ForLoop` cost about 55 ns, more than the rest of the loop: the hand-off to `exec`, then three loads and four stores that each look up the thread and frame. In the hot tier the same loop is 27 ns, faster than the `while` form. Only `ForPrep` and the final `Return` run cold. In the inner-`break` loop each outer iteration runs a cold `ForPrep`, which is most of that row.

Fuel is identical in `cold` and `hot` across all 47 workloads that report it.

| Build | text | `run_hot` | `exec` |
|---|---|---|---|
| 3.6 | 845,932 | 5,017 | 13,639 |
| cold | 865,208 | 5,017 | 15,349 |
| hot | 867,904 | 7,444 | 15,169 |
| hot+6 | 871,360 | 7,444 | 16,842 |

Three new cold opcodes again left `run_hot` unchanged. Promoting `ForLoop` grew it by 2.4 KB, most of it the float-loop path, and the six unused opcodes did not touch it. The wasm probe is 351,831 bytes.

The capture lookup stays deferred. A one-capture closure made in a `for` loop costs about 100 ns per iteration including the closure and cell allocation and the close.

## Tables, indexing, and globals (Phase 3.8)

### Method change

A full benchmark run takes about a minute, and the 7-round, four-build comparisons had reached half an hour per milestone. From this phase on, `MOONSEED_BENCH_ONLY=a,b,c` limits a run to the named loop workloads, so a filtered run takes about 2 seconds. A comparison is 9 rounds of each build, one at a time, alternated, pinned with `taskset -c 4`, about a minute in all. Running the builds at the same time on separate cores was tried and rejected: the medians were bimodal (for example 15 or 48 ns for the arithmetic loop). The machine was also running another agent's game-engine captures, so absolute numbers in this section are higher than in earlier ones; compare within a table.

The instruction mix is now counted by the runtime: `cold_steps` counts instructions run by `exec`, and hot = fuel − cold. The previous helper cloned the table arena before each step, which made the 32-field constructor loop effectively never finish.

### Results

`3.7` is `8486d3d` with this phase's harness. `cold` is this phase with the new opcodes only in `exec`. `hot` runs table hits in `hot_op`. `hot+6` adds the six unused opcodes. ns per loop iteration, 20,000 iterations (2,000 for constructors), median of 9 with the range.

| Workload | 3.7 | cold | hot | hot+6 |
|---|---|---|---|---|
| `s = t[2]` | — | 91 [91–92] | 56 [55–57] | 56 [55–56] |
| `s = t.x` | — | 108 [108–109] | 51 [50–53] | 50 [50–51] |
| `t[1] = i` | — | 100 [99–101] | 53 [52–55] | 52 [52–53] |
| `t.x = i` | — | 95 [94–98] | 52 [51–53] | 51 [50–51] |
| `local s = t.missing` | — | 52 [52–53] | 59 [58–62] | 58 [58–59] |
| global read | — | 136 [135–138] | 80 [79–81] | 79 [79–80] |
| global write | — | 121 [119–122] | 79 [78–80] | 79 [78–79] |
| `s = t.a.b.c` | — | 238 [236–240] | 108 [107–108] | 107 [107–108] |
| `{}` | — | 68 | 69 | 68 |
| `{ 1, 2, 3, 4 }` | — | 795 | 794 | 781 |
| 32 list fields | — | 6352 | 6338 | 6339 |
| `{ 1, 2, x = 3, y = 4, [10] = 5 }` | — | 901 | 962 | 943 |
| empty `for` | 19 | 19 | 19 | 19 |
| arithmetic loop | 15 | 15 | 14 | 13 |
| arithmetic, quantum 1 | 72 | 73 | 82 | 82 |
| scalar / nested / multi call | 105 / 201 / 119 | 105 / 200 / 119 | 106 / 202 / 119 | 106 / 202 / 119 |
| hand-bytecode integer / string field | 159 / 201 | 156 / 196 | 156 / 197 | 157 / 198 |
| `while` / comparison / `elseif` loops | 37 / 44 / 103 | 38 / 45 / 105 | 36 / 42 / 103 | 34 / 41 / 98 |
| `for` capturing its variable | 175 | 187 | 190 | 192 |

Fuel is identical in `cold` and `hot` across all 60 workloads that report it.

Table hits roughly halve field reads, writes, and global access. A cold field read paid the hand-off to `exec`, register loads that each resolve the thread and frame, and, before this phase's `KeyView`, an allocated copy of the key's bytes. The costs that stay:

- A missing key is tried in the hot tier, declines, and is looked up again in `exec`, so a miss is about 7 ns slower. That is the price of keeping misses, where `__index` will go, on one path.
- A constructor's named field is a new key, so it also declines once; the mixed constructor is about 6% slower.
- Quantum 1 is about 12% slower: each one-instruction slice also sets up the table view.
- Constructors are dominated by the table insert, about 160 ns per field. They are not dispatch-bound. Table size hints were not added.

### Code size and opcode growth

| Build | text | `run_hot` | `exec` |
|---|---|---|---|
| 3.7 | 874,876 | 7,451 | 15,169 |
| cold | 906,960 | 7,451 | 18,126 |
| hot, table bodies inline in `hot_op` | 912,300 | 10,752 | 19,401 |
| … plus six unused opcodes | 914,712 | 9,672 | 20,961 |
| hot, table bodies in `#[inline(never)]` helpers | 910,700 | 8,569 | 18,981 |
| … plus six unused opcodes | 914,148 | 8,569 | 20,522 |

The five new cold opcodes left `run_hot` unchanged. The first promotion put the table-hit bodies inline in `hot_op`. `run_hot` then changed size when unused opcodes were added, and some rows moved by 20–35% between the two builds, in both directions: `s = t[2]` was 84 against 55 ns, and the capturing `for` 227 against 190. Moving the four bodies into out-of-line helpers restored the property: `run_hot` is the same size with or without the extra opcodes, and every row agrees within about 5%. The helpers cost one call per table hit. The hot switch is again only register operations plus calls. The wasm probe is 390,561 bytes.

## Native function calls (Phase 3.9)

Method as in Phase 3.8: filtered runs, 9 alternating rounds pinned to one core. `3.8` is `7605d63`. `now` is this phase. `now+6` adds the six unused opcodes. ns per loop iteration, 20,000 iterations. The wait row is per wait, 2,000 waits.

| Workload | now | now+6 |
|---|---|---|
| Lua function call `s = f(i, 1)` | 86 [84–93] | 86 [85–90] |
| native `add` from a local | 82 [81–85] | 83 [82–122] |
| native `add` from a table field | 97 [95–107] | 97 [96–102] |
| native `add` from a global | 123 [122–129] | 124 [123–181] |
| native with no results | 51 [50–53] | 50 [49–56] |
| native with three results `a, b, c = many()` | 83 [82–115] | 83 [82–88] |
| external journaled native | 159 [158–161] | 158 [157–160] |
| native wait, `complete_wait`, resume | 135 [133–140] | 132 [130–138] |

A native call from a local costs about the same as a Lua call. The global form adds the `_ENV` upvalue load and a field read. An external call adds the prepared stop, the second trip through `poll`, and a journal insert.

The first measurement was slower, and it was fixed before these numbers were taken:

- Every native call allocated an argument `Vec` and a result `Vec`: 115 ns from a local, 81 ns with no results. The buffers now live on the runtime and are reused.
- The external row was 3,985 ns. The reference `Journal` looked up effect ids by linear scan, so a run of 20,000 commits was quadratic. It now keeps a hash index next to its record list.

Earlier workloads against `3.8` (arithmetic, branch, quantum 1, scalar, nested, and multi calls, upvalue calls, host calls, `for`, capture loops, field and global reads, `elseif`) are all within noise; for example the scalar call is 76 against 75 ns and the nested call 144 against 143. `run_hot` is 8,610 bytes in both `now` and `now+6`, up 41 bytes from 8,569, since the new `Value` variant reaches the value matches inside it. `Call`, and therefore native dispatch, stays in `exec`.

## Metatables and metamethods (Phase 3.10)

Regression check, 9 alternating rounds pinned to one core, against Phase 3.9 (`09489e8`): every earlier workload matched within noise, including a native call from a local (83 against 83 ns). `run_hot` is 8,610 bytes, also with six unused opcodes added.

Metamethod rows, per-row minimum of three filtered runs of this phase (ns per loop iteration of 20,000). The machine was also running other workloads, so treat single numbers as approximate. The rows index with a key held in a local; see the allocation note below.

| Workload | ns |
|---|---|
| `t.x` hit on a table that has an `__index` | 35 |
| missing key, no metatable | 43 |
| missing key, metatable without `__index` | 44 |
| `__index` table, 1 / 8 / 64 hops | 110 / 256 / 1,464 |
| Lua `__index` function | 205 |
| native `__index` | 199 |
| native `__index` that waits, per wait including `complete_wait` | 223 |
| a 2000-step `__index` cycle to its fault, one run | 38,543 |
| `t.x = i` live key, table has `__newindex` | 38 |
| absent-key insert and delete, no metatable | 357 |
| `__newindex` table / Lua / native | 204 / 191 / 195 |
| `rawget` / `rawset` / `rawlen` | 238 / 220 / 212 |
| `#` string / table raw / Lua `__len` / native `__len` | 42 / 73 / 148 / 144 |
| `setmetatable` / `getmetatable` | 174 / 171 |

A primitive hit costs what it did: the hot tier handles it and never looks at the metatable. A miss leaves the hot tier and checks for a metatable in `exec`, about 43 ns against 35 for a hit. A metamethod call costs roughly a Lua or native call plus the commit step. The chain walk is linear, about 22 ns a hop. The 2000-step cycle is about 39 µs of work inside one charged instruction before it faults. That is the most work one instruction can now do without being charged more, and a candidate for chain-step accounting if runtime control needs it.

Two findings not fixed here:

- A constant-key access that reaches the slow path allocates a string object each time: `t.x` handed to an `__index` function, or `t.x = v` inserting a new key. So does `LoadBytes`, as before. Nothing collects automatically, so a long enough loop of these hits the object limit (`MemoryLimit`); the first version of these benchmarks did. Constant strings that allocate once, or collection triggered by allocation debt, are GC-track work.
- With six unused opcodes, native calls in `exec` were 30–40% slower, while `run_hot` did not change. `exec`-heavy paths depend on `exec`'s layout. `#[inline(never)]` on the native-call functions was tried; it was slower and still unstable, and was reverted. A deliberate second dispatch tier for the common cold operations, or a measured promotion of native calls, is the remedy, as a performance-track item.

## Constant strings, automatic collection, and the common tier (Phase 3.11)

Filtered runs pinned to one core, as in Phase 3.8. The machine was shared with other workloads; compare within a table.

### Constant strings

ns per loop iteration, 20,000 iterations, three runs of this phase. Allocations counted by the object-id counter.

| Workload | Constant key (`t.foo`) | Key in a register (`t[k]`) | Objects allocated per iteration |
|---|---|---|---|
| Field hit | 34 | 35 | 0 |
| Field miss, no metatable | 37 | — | 0 |
| Lua `__index` | 149–151 | 184–186 | 0, was 1 |
| Native `__index` | 154–156 | 206–208 | 0, was 1 |
| Insert then delete a field | 329–331 | 327–338 | 0, was 2 |

Before this phase each constant-key row made one string per slow access (two for insert and delete, one per `local x = 'foo'`), and the 20,000-iteration rows could not run: they reached the object limit near 10,000.

A string key held in a register is slower than a constant on the metamethod paths. `Index` normalizes the key into an owned `TableKey`, which copies the bytes. That path predates this phase and is left for later.

### Automatic collection

4,000 iterations, default limits (10,000 objects, `gc_min_debt` 64 KiB). ns per iteration with collections included, median of 20 runs.

| Loop | On | Off | Collections per run (on) | Objects at the end (on / off) |
|---|---|---|---|---|
| `local t = { i, i + 1 }` | 306 | 421 | 5 | 617 / 4,016 |
| `local f = function() return i end` | 184 | 176 | 4 | 769 / 8,017 |
| `t.foo[1]` through an `__index` that returns a new table | 349 | 464 | 3 | 978 / 4,023 |
| `keep[i] = {}`, all retained | 267 | 264 | 2 | 4,017 / 4,017 |

- **Faster with collection on:** tables and metamethod results. Freed slots are reused while their memory is still in cache; without collection the arenas grow to 4,000 slots.
- **Slower with collection on:** closures (4%) and a heap that only grows (1%), which pays for collections that free nothing.
- **String churn:** not measured. Source code cannot build a string at run time yet.

Full collection of a reachable heap, 200 collections each:

| Heap | Objects | Logical bytes | Median | p95 | p99 |
|---|---|---|---|---|---|
| Small | 115 | 13,959 | 1.1 µs | 1.1 µs | 1.8 µs |
| Medium | 2,015 | 257,159 | 16 µs | 17 µs | 23 µs |
| Large | 9,015 | 1,153,159 | 79 µs | 85 µs | 100 µs |

That is about 9 ns per live object, most of it marking. The collector still clones each thread's stack and frames while tracing, as it did before. These are the baseline numbers for an incremental collector.

### Dispatch

Nine bench builds, 7 alternating rounds in one session. Old design (`exec` as one match):
- `base`, which is `7d34040`;
- `base+6`;
- both again built with `-C llvm-args=-align-all-functions=6`.

New design (dispatcher, common handlers, `exec_rare`):
- `now`;
- `now+6 rare`;
- `now+6 common`;
- `now` and `now+6 rare` again, aligned to 64 bytes.

Each cell is the range of the builds' medians, ns per iteration.

| Workload | Old design | New design |
|---|---|---|
| Lua call | 85–108 | 84–85 |
| Native call, from a local | 80–83 | 82–87 |
| Native call, zero / multiple results | 51–52 / 79–81 | 52 / 81–82 |
| Native call, global | 126–170 | 133–185 |
| Native call, table field | 99–137 | 95–130 |
| Lua / native `__index` | 162–200 / 181–201 | 159–209 / 183–238 |
| Lua / native `__newindex` | 156–185 / 158–200 | 155–181 / 154–184 |
| Lua / native `__len` | 114–147 / 107–152 | 115–147 / 111–177 |
| Constructor, mixed | 709–763 | 711–838 |
| Field miss | 39–41 | 37–59 |
| Integer `for`, arithmetic loop | 25 / 8 | 25 / 8 |

The machine code tells a different story. With the new design, adding six rare opcodes leaves the dispatcher and every common handler byte-identical, and adding six common ones changes only the dispatcher's jump code. With the old design, the same rare growth rewrote `exec`.

The timings do not follow. Rows that go through several cold handlers per iteration vary by 15–60% between builds whose code on the path is identical, in both directions, with or without 64-byte function alignment. Building the old tree with only the alignment flag changed moves them as much as adding opcodes does. On this CPU, those paths depend on where the code lands in memory. Rows that dispatch one cold operation per iteration stay within about 5% at every layout: native calls from a local, with zero or several results, and Lua calls.

The Phase 3.10 native-call cliff did not come back in any of the nine builds. The field-miss row was 59 ns in both `now+6 rare` builds and 37 in the other three new-design builds. So the timing bar of this phase, rare growth within single digits for the common paths, is met for the single-handler rows and not for the multi-handler ones. `perf` counters are not available on this machine (`perf_event_paranoid` 4), so the mechanism is not identified.

Code size:
- `run_hot`: 8,610 → 9,011 bytes, from the collection check at the top of its slice loop. The hot loops did not move.
- `exec`: 18,360 bytes → a 291-byte dispatcher, a 9,511-byte `exec_rare`, and 17 handlers of 148 to 2,143 bytes.
- Bench text: 764,542 → 777,022 bytes.
- The wasm probe is 456,774 bytes.

### Method change

A comparison of workloads that cross several cold handlers now uses several layouts per side, not one build against one other. `tools/cold_opcode_growth.py`, with and without `--common`, doubles as a layout sampler. A difference smaller than the spread across layouts is not reported as a change. `tools/code_diff.sh` compares the handlers' machine code between the variants, to separate code changes from placement.

## Operators, operator metamethods, and `__call` (Phase 3.12)

Filtered runs pinned to one core, as in Phase 3.8. The regression check is 9 rounds against `d6639a9` (Phase 3.11 plus the `__len` fix). The new rows are the median of 5 rounds over two layouts: this tree, and this tree with six unused rare opcodes. They are ns per loop iteration, 20,000 iterations. Metamethod rows cost a call each and are not expected to approach the primitive rows.

### Primitive operators

| Workload | ns | Cold steps per run |
|---|---|---|
| `s = (s + i * 3 - 1) % 1000` | 58 | 2 |
| `s = i // 7` | 42 | 2 |
| `s = s * 0.5 + i / 4 - 1.5 ^ 2` | 77 | 2 |
| `s = ((s ~ i) & 65535 \| i << 3) >> 1` | 80–82 | 2 |
| `local b = i < 2.5` | 47–48 | 2 |
| `s = '1' + s` (numeric string) | 94–99 | 40,002 |

The first version ran `Arith` and non-integer `Add` in the common tier: 157, 74, 249, and 219 ns for the first four rows, about 45 ns per operator. Promoting the two-number cases into the hot tier (ADR 0023) gave 2–4× on those rows. An inline promotion grew `run_hot` from 9,042 to 9,902 bytes and slowed the empty `for` and `while` loops from 25 to 29 ns. The out-of-line `hot_arith` helper, which reads and writes the registers itself, shrank `run_hot` to 8,824 bytes and left the empty loops at 24–25. The integer arithmetic loop reads 9 ns against 8 in both versions. Numeric strings stay in the common tier.

### Metamethods and `__call`

| Workload | ns |
|---|---|
| `t + i`, Lua `__add` | 153–156 |
| `t + i`, native `__add` | 171–202 |
| `t + i`, `__add` a table with `__call` | 189–207 |
| native `__add` that waits, per wait including `complete_wait` | 194–199 |
| `a == b`, Lua `__eq` | 147 |
| `a < b`, Lua `__lt` | 146–150 |
| `a <= b`, native `__le` | 169–179 |
| `t(i)`, Lua `__call` | 112–130 |
| `c3(i)`, three-deep `__call` chain | 160–200 |
| `'ab' .. 'cd'` | 106–111 |
| `'n' .. i` | 98 |
| `t .. 'x'`, Lua `__concat` | 149–156 |
| `'x' .. t`, native `__concat` | 158–211 |

A metamethod costs about a Lua or native call plus the commit step, as in Phase 3.10. A callable table costs about 30–45 ns more than a Lua call (84 ns), for the `__call` lookup and the argument shift. Each concatenation allocates one string. The loops above collected 11 times per 20,000 iterations with default limits, and no host call was needed.

### Regression check and code

Against `d6639a9`:
- **Unchanged:** the empty loops (`for` 25 → 24, `while` 23 → 24), Lua calls (85 → 84), constructors, field hits, field misses, and table churn.
- **Arithmetic loop:** 8 → 9, as above.
- **Moved in both directions between identical-code builds:** native calls, metamethod rows, and `#` with `__len`. `call_native_local` was 88 in this tree, 113 with six rare opcodes, and 104 in `d6639a9`. `tools/code_diff.sh` shows `call`, `call_native`, `run_native`, `deliver_native`, and `finish_result_window` identical between the two trees; only their placement within 64-byte lines differs. This is the placement sensitivity of ADR 0022, which now reaches single-handler native calls too. As decided, it is recorded and not chased here.

Code size:
- `run_hot`: 9,011 → 8,824 bytes.
- `exec`: 291 → 432 bytes.
- `call`: 249 → 473 bytes (`__call` resolution and the native argument count).
- New handlers: `op_arith` 902, `op_compare` 938 (was 675), `op_unary` 873 (replacing the 366-byte `op_neg`), `op_concat` 1,192.
- Bench text: 787,838 → 813,182 bytes.
- The wasm probe is 527,046 bytes.

Six added rare opcodes leave `exec`, `run_hot`, `run`, `call`, `call_meta`, every operator handler, and `hot_arith` byte-identical.

## Errors, protected calls, and the heap quota (Phase 3.13)

Filtered runs pinned to one core, 5–7 rounds, two layouts per side: the default build and `-C llvm-args=-align-all-functions=6`. Rows are ns per loop iteration. Pairs are the two layouts.

### Protected calls and caught errors

| Workload | ns | Iterations |
|---|---|---|
| plain Lua call, for reference (`call_lua_scalar`) | 86 / 86 | 20,000 |
| `pcall(f)`, empty `f` | 197 / 194 | 20,000 |
| `pcall(f, i)`, one result | 197 / 192 | 20,000 |
| `pcall(f, i)`, three results | 220 / 205 | 20,000 |
| `pcall(g, i)` where `g` calls `pcall(f, a)` | 367 / 367 | 20,000 |
| `xpcall(f, h, i)`, no error | 208 / 196 | 20,000 |
| `xpcall` whose `f` raises, handler returns 1 | 424 / 425 | 20,000 |
| `error(t)` with a table, caught | 377 / 394 | 20,000 |
| error raised 1 frame below `pcall` | 493 / 521 | 20,000 |
| 10 frames | 1,359 / 1,357 | 5,000 |
| 100 frames | 9,750 / 9,673 | 1,000 |
| the same 100-frame recursion returning normally | 10,204 / 10,177 | 1,000 |
| 995 frames | 93,145 / 93,869 | 200 |
| stack overflow at 1,000 frames, caught | 66,127 / 66,247 | 200 |
| `s .. s` past 1 MiB, caught | 408 / 419 | 20,000 |
| a table filled to a 256 KiB quota, caught | 1.82 ms / 1.82 ms | 100 |
| native fault (`add('x')`), caught | 333 / 336 | 20,000 |
| Lua `__add` that raises, caught | 549 / 555 | 20,000 |
| native `__add` that faults, caught | 439 / 442 | 20,000 |

- **Cost of `pcall`:** about 110 ns over a plain call. That is the builtin dispatch, the boundary frame, and the step that finishes it.
- **Unwinding:** about 93 ns per frame, linear to 995 frames. The 100-frame catch is cheaper than the same recursion returning normally, because a pop copies no results.
- **Quota row:** mostly the fill. Each iteration inserts about 8,000 entries and collects about 14 times on the way to the quota.

A deeper fill is much worse. Filling one table to the default 256 MiB quota took 15 s and 1,695 collections, and reached 1.3 GB RSS. The collection threshold is capped by the object headroom, about 160 KB of debt, and each collection marks the whole table. See ADR 0025. The same fill with 64 KiB strings reached 265 MB RSS for 268 MB logical.

The `..` bound used to be checked after the result was built. A failing concatenation of two 512 KiB strings then cost 20.6 µs, one full copy. The check now runs on the lengths first, and the row fell to 408 ns.

### Regression check

Against `bcd334a` (Phase 3.12), in each layout:

| Workload | base | this tree | base, aligned | this tree, aligned |
|---|---|---|---|---|
| `arith_loop` | 9 | 9 | 9 | 9 |
| `scalar_call` | 73 | 76 | 74 | 76 |
| `nested_call` | 139 | 143 | 140 | 144 |
| `upvalue_call` | 104 | 112 | 105 | 108 |
| `int_field` | 123 | 124 | 123 | 125 |
| `string_field` | 153 | 153 | 152 | 153 |
| `call_lua_scalar` | 85 | 85 | 84 | 85 |
| `call_native_local` | 82 | 83 | 83 | 82 |
| `call_native_external` | 164 | 162 | 157 | 160 |

The first build of this tree was slower in four places. Each cause was found and fixed:

- **Native calls, +39%:** `call_builtin` was inlined into `call_native`, which grew from 335 to 1,598 bytes. It is out of line now; `call_native` is 370 bytes.
- **Every cold step, 3–4 ns:**
  - `lua_errors` wrapped the result of each `exec`, to turn a memory failure into a Lua error.
  - A variant without the new `run_hot` checks recovered nothing. A variant with a `#[cold]` out-of-line error branch (`vm_error`) recovered almost everything.
  - `upvalue_call` had lost 15 ns, and `scalar_call` 5.
- **Field stores, +12–18%:** `table_insert` looked the key up before every non-nil store, to see whether the store adds a slot. The quota test now runs first, and the lookup happens only when the slot would not fit.
- **Native waits:** `complete_wait` built two vectors per completion through the general `complete`. It now passes one value without allocating.

What remains is 2–4 ns on Lua calls. The frame grew from 104 to 112 bytes for the boundary field, and `run_hot` makes two more checks each time it re-enters; neither was measured alone. Field and native rows are back within noise.

Rows through several cold handlers moved by up to ±25% in both directions:
- metamethod calls, native `__index`;
- `..`;
- `call_native_wait`, and the pending metamethod rows.

They moved between the two layouts and between runs of the same two binaries. For example, `meta_index_native` read 175 → 214 in one run and 200 → 155 in another. This is the placement sensitivity of ADR 0022, and it is recorded, not chased.

The table above is the build before the last change, which added the quota check to closure, upvalue, thread, and prototype allocation. In the final tree, every function on the call paths is byte-identical to that build (`tools/code_diff.sh`). Only placement moved:
- Default layout: `call_native_local` read 108 against 89, and `upvalue_call` 118 against 108.
- Aligned layout: 95 against 102, and 119 against 121.

The review fixes changed `finish_result_window` and `finish_protect`. They were measured against the build just before them, 11 rounds on a loaded machine: `scalar_call` 87 → 85, `nested_call` 163 → 163, `pcall_zero` 205 → 209, `pcall_scalar` 209 → 212.

### Code size

| | bytes |
|---|---|
| `run_hot` | 8,824 → 8,945 |
| `exec` | 432, unchanged |
| `call_native` | 335 → 370 |
| `push_lua_frame` | 1,268 → 1,363 |
| bench text | 813,182 → 848,766 |
| wasm probe | 527,046 → 560,020 |

## To-be-closed variables (Phase 3.14)

Filtered runs pinned to one core, 5–7 rounds, in the default and the aligned layouts. Rows are ns per loop iteration. Another workload kept the machine at a load average of 8–9 during the final runs, so absolute numbers are 10–15% above those of Phase 3.13's tables; compare within a table.

### Closes

| Workload | ns (default / aligned) |
|---|---|
| `do local x = obj end`, no close (`scope_plain`) | 45 / 44 |
| `do local x <close> = nil end` (`close_nil`) | 60 / 60 |
| the closer called directly, `f(obj, nil)` (`call_closer`) | 100 / 99 |
| one `<close>` with a Lua `__close` (`close_one`) | 232 / 235 |
| one `<close>` with a native `__close` | 224 / 237 |
| 4 in one scope | 713 / 725 |
| 32 in one scope | 4,970 / 5,072 |
| `pcall` of a function that raises, no close (`catch_depth_1`) | 569 / 538 |
| the same with one `<close>` | 700 / 679 |
| the same with 32 | 6,029 / 6,215 |
| a Lua call, no close (`call_lua_scalar`) | 100 / 100 |
| a function returning one value through one `<close>` | 301 / 330 |
| a `__close` that waits on the host, per wait with `complete_wait` | 231 / 268 |

What each part costs:
- **Registration and a scope exit with nothing to close:** about 15 ns (`close_nil` against `scope_plain`).
- **The call itself:** about 55 ns (`call_closer`).
- **The rest of one close:** about 130 ns. That covers the `__close` lookups at registration and at close time, the scope close, and the commit.
- **Scaling:** closes cost 155–180 ns each from 4 to 32 in one scope. During an unwind they cost 130–170 ns each over the unwind alone.
- **A return through one close:** about 200 ns over a plain call.

The first working version cost 266 ns for `close_one`. Two changes brought it to 198 ns, measured on a quiet machine:
- **Box reuse.** Each close step boxed a fresh state, three per close; the frame now keeps one box from call to call.
- **No separate dispatch step.** The first close and each next one start in the step that ends the previous one, except after a native close, to keep the Rust stack flat.

### Regression check

Against `9dffe0b` (Phase 3.13), in the same loaded conditions:

| Workload | base | this tree | base, aligned | this tree, aligned |
|---|---|---|---|---|
| `arith_loop` | 10 | 11 | 10 | 11 |
| `scalar_call` | 87 | 90 | 90 | 88 |
| `nested_call` | 164 | 170 | 162 | 167 |
| `int_field` | 141 | 140 | 153 | 151 |
| `call_lua_scalar` | 97 | 100 | 96 | 101 |
| `meta_index_lua` | 197 | 226 | 241 | 225 |
| `meta_add_lua` | 183 | 209 | 194 | 166 |
| `pcall_zero` | 232 | 208 | 228 | 218 |
| `catch_depth_100` | 11,701 | 10,208 | 11,242 | 10,386 |

Plain calls are within 3–5%. The metamethod rows move by up to 15% in opposite directions between the two layouts, which is ADR 0022's placement effect.

One earlier layout was a real cost. It kept the close state inside `MetaEvent`, which made every metamethod call pass a 24-byte, non-`Copy` event. There, `meta_index_lua` was 10–17% slower in both layouts, and `op_index`'s code differed from the base. The close state now sits in its own box on the metamethod call (ADR 0026). `op_index` is back within 12 differing lines of the base, and the remaining differences follow the layout.

### Code size

| | bytes |
|---|---|
| `run_hot` | 8,945, unchanged |
| `exec` | 432 → 438 |
| `exec_rare` | 9,475 → 11,238 (the three new instructions' handlers inlined there) |
| `call_meta` | 1,444 → 1,422 |
| bench text | 848,766 → 871,742 |
| wasm probe | 560,020 → 592,619 |

Nothing moved into `run_hot`.

## Generic `for` (Phase 3.15)

Filtered runs pinned to one core, 7 rounds, in the default and the grown layout (`tools/cold_opcode_growth.py`). Rows are ns per loop iteration, or per loop where noted. The review job was running tests during these runs, at a load average of about 7; compare within a table.

### Loops

| Workload | ns (default / grown) |
|---|---|
| Lua iterator, one variable, nil closing value (`gfor_lua`) | 134 / 135 |
| the same call made from a numeric `for` (`gfor_numeric_call`) | 180 / 146 |
| the same loop written with `while` (`gfor_while`) | 175 / 173 |
| native iterator, `upto` (`gfor_native`) | 114 / 113 |
| table with `__call` as the iterator (`gfor_callable`) | 180 / 188 |
| two results and variables (`gfor_2`) | 139 / 144 |
| four (`gfor_4`) | 152 / 163 |
| a closure capturing the loop variable each iteration (`gfor_capture`) | 228 / 238 |
| per loop: set up, one call that ends it, nil closing value (`gfor_setup_nil`) | 214 / 183 |
| per loop: the same with a real closing value (`gfor_setup_close`) | 400 / 356 |
| per loop: `break` in the first iteration (`gfor_break`) | 261 / 202 |
| per loop: the iterator raises under `pcall` (`gfor_error`) | 630 / 575 |
| a Lua call from a numeric `for` (`call_lua_scalar`) | 135 / 101 |
| `for i = 1, n do s = s + i end` (`for_int`) | 52 / 28 |

What each part costs:
- **The iterator call** is most of an iteration: about 100–135 ns for a Lua iterator (`call_lua_scalar`), less for a native one.
- **The loop's own control** is three hot `Move`s and `GenericForLoop` in `exec_rare`. A build with `GenericForLoop` in the hot tier ran `gfor_lua` in 136 / 142 ns against 156 / 160 in the same session, and `gfor_native` in 116 / 118 against 126 / 131. So the rare dispatch is about 15–20 ns of an iteration. That is not the dominant cost, and the instruction stays in `exec_rare`. The hot build's `run_hot` was 9,374 bytes, against 8,945.
- **Against the alternatives:** the generic loop is as fast as a numeric `for` making the same call, or faster, and about 40 ns faster than the same loop written with `while`, which runs 22 instructions per iteration against 15.
- **More variables:** each extra result costs about 5 ns.
- **A real closing value:** about 170–190 ns per loop over nil, the cost of one `__close` call (Phase 3.14).
- **Captured variable:** a closure and its cell per iteration cost about 95–100 ns over `gfor_lua`, as in numeric `for` (Phase 3.7). One open capture at a time keeps the open-upvalue lookup trivial; generic `for` gives no reason to change it.

Fuel per iteration: 15 for `gfor_lua` (5 of them the loop's control), 17 for `gfor_numeric_call`, 22 for `gfor_while`. Charged instructions per iteration that leave the hot tier: 3 for `gfor_lua` (`Call` and `Return` in `exec`, `GenericForLoop` in `exec_rare`), 2 for `gfor_numeric_call`.

Allocation: none per iteration with a Lua or native iterator. The capture row makes a closure and a cell per iteration, and the error row makes its iterator, a closure, once per loop. Loops of 10 and 5,000 iterations grow the stack and frame vectors the same number of times, for Lua, native, and `__call` iterators and with a capture.

### Regression check

Against `a532daf` (Phase 3.14), in the same session, in performance mode:

| Workload | this tree | base | this tree, grown | base, grown |
|---|---|---|---|---|
| `arith_loop` | 12 | 12 | 12 | 12 |
| `scalar_call` | 100 | 102 | 106 | 97 |
| `nested_call` | 194 | 190 | 192 | 183 |
| `int_field` | 164 | 159 | 168 | 164 |
| `call_lua_scalar` | 147 | 112 | 112 | 147 |
| `meta_index_lua` | 246 | 240 | 239 | 267 |
| `meta_add_lua` | 229 | 211 | 177 | 202 |
| `pcall_zero` | 250 | 235 | 241 | 281 |
| `for_int` | 54 | 30 | 31 | 55 |
| `while_loop` | 29 | 30 | 30 | 30 |
| `close_one` | 295 | 262 | 257 | 289 |
| `catch_depth_1` | 600 | 601 | 567 | 586 |
| `close_return` | 374 | 345 | 340 | 379 |

No regression. `run_hot`, `exec`, `call`, `push_lua_frame`, `op_for_prep`, `do_return`, and `call_meta` have the same machine code as in the base (`tools/code_diff.sh`, 0 differing lines). This tree's default layout and the base's grown layout form the slow band, and the other two the fast band. `for_int`, which runs entirely in the hot tier, is 30 or 54 ns by placement alone.

### Code size

| | bytes |
|---|---|
| `run_hot` | 8,945, unchanged |
| `exec` | 438, unchanged |
| `exec_rare` | 11,238 → 8,610. LLVM no longer inlines `mark_close` (518) and `close_scope` (390) into it |
| bench text | 871,742 → 884,638, most of it the new rows |
| wasm probe | 592,619 → 608,979, with the new fingerprint and fixtures |

## Varargs and the stack bound (Phase 3.16)

Filtered runs pinned to one core, 7 rounds, in the default and the grown layout. Another workload kept the load average at 7–10. Rows are ns per loop iteration, one call each.

### Calls and varargs

| Workload | ns (default / grown) |
|---|---|
| `f()`, `f(1, 2)`, `f(1, 2, 3, 4)` to fixed parameters (`call_0/2/4`) | 91 / 90, 91 / 90, 96 / 97 |
| `f(...)` called with no extras (`va_0`) | 91 / 89 |
| with 1, 4, and 32 extras (`va_1`, `va_4`, `va_32`) | 98 / 96, 109 / 104, 190 / 199 |
| 4 extras, `local x = ...` (`va_read`) | 125 / 125 |
| 4 extras, `local a, b, c, d = ...` (`va_all`) | 134 / 131 |
| `local w, x, y, z = f(1, 2, 3, 4)` with `return ...` (`va_return`) | 144 / 143 |
| `return g(...)` (`va_pass`) | 215 / 212 |
| `local t = { ... }` (`va_table`) | 460 / 467 |
| a call, then `(...)` (`va_nested`) | 201 / 198 |
| `pcall`, then `(...)` (`va_pcall`) | 326 / 309 |

What each part costs:
- **A vararg function with no extras** costs the same as a plain call.
- **Each extra** costs about 3 ns to pass: 200 extras cost 675 ns against 90 for none, argument loads included.
- **The rotation** only moves the fixed parameters, which the benchmarks' `(...)` functions do not have.
- **Reading `...`** once is one `Vararg` in `exec_rare`, about 17–20 ns (`va_read` against `va_4`).
- **Returning all of them** costs about 4.5 ns per value, for the copy, the return's clearing, and the stack refill: `return ...` to a call that drops the results costs 16 ns more than `return 1` with 0 or 1 extras, 161 ns with 32, and 905 ns with 200.

Fuel: `Vararg` is one unit whatever it copies. Nothing allocates per call except `{ ... }`.

### Regression check

Against `22454c9` (Phase 3.15), same session, after the review's fixes:

| Workload | this tree | base | this tree, grown | base, grown |
|---|---|---|---|---|
| `arith_loop` | 10 | 10 | 10 | 10 |
| `scalar_call` | 85 | 85 | 86 | 84 |
| `nested_call` | 161 | 161 | 161 | 160 |
| `call_lua_scalar` | 96 | 95 | 98 | 95 |
| `call_native_local` | 93 | 93 | 89 | 92 |
| `meta_index_lua` | 199 | 178 | 189 | 239 |
| `meta_add_lua` | 181 | 190 | 170 | 179 |
| `pcall_zero` | 197 | 195 | 192 | 212 |
| `close_one` | 225 | 238 | 227 | 219 |
| `catch_depth_1` | 535 | 529 | 578 | 545 |
| `close_return` | 305 | 302 | 301 | 320 |
| `gfor_lua` | 128 | 127 | 130 | 130 |

Calls without varargs are within 1–3 ns. The metamethod and protected-call rows move by up to 25% in both directions between layouts, as before. An earlier run of this table, before the review's fixes, showed the same pattern.

### Code size

| | bytes |
|---|---|
| `run_hot` | 8,945, unchanged |
| `exec` | 438, unchanged |
| `push_lua_frame` | 1,350 → 1,330 |
| `finish_result_window` | 514 → 482 |
| `call_meta` | 1,422 → 1,448, the stack check |
| `run_native` | 1,554 → 1,738, the stack check on results |
| `exec_rare` | 8,610 → 8,818 |
| bench text | 884,638 → 898,366, most of it the new rows |
| wasm probe | 608,979 → 630,140 |

## Tail calls (Phase 3.17)

Filtered runs pinned to one core, 7 rounds. Another workload kept the load average at 4–7, so absolute figures are noisier than in earlier sections; comparisons are within one run.

### What a tail call costs

`hop_*` rows are per hop of a recursion 100 deep, run 2,000 times; the rest are per call of a Lua function returning a native's results.

| Workload | tail call | `return (f(...))`, a call and a return |
|---|---|---|
| fixed arguments (`hop_tail` / `hop_call`) | 80 | 111 |
| four extras through a vararg function (`_va`) | 118 | 154 |
| alternating fixed and vararg functions, per hop (`hop_tail_fixed_va`) | 87 | |
| through a table's `__call` (`_meta`) | 97 | 130 |
| closing a captured upvalue each hop (`_upval`) | 164 | 237 |
| one recursion of 200,000 hops (`hop_tail_deep`) | 76 | overflows |
| to a VM-local native (`tail_native` / `call_native_ret`) | 235 | 243 |
| to an External native (`tail_external` / `call_external_ret`) | 322 | 350 |
| to a native that waits, host completion included (`tail_pending` / `call_pending_ret`) | 225 | 239 |

- **A tail hop is cheaper than a call and a return,** by about 30 ns: one instruction instead of two, and no second frame. Fuel per hop is 8 against 9 for the fixed recursion.
- **Frames and stack slots do not grow.** 200,000 hops allocate the same 2 objects as one; the proof tests step 100,000 hops one instruction at a time and see the same peak frames, slots, and objects as at 1,000 hops. The `_upval` rows allocate a closure and a cell per hop in both forms; the tail form closes the cell when the frame is replaced.
- **A native tail call** saves one instruction and the Lua frame's return, 4–10%. The native still runs through the ordinary native call path of the frame below.
- **`va_pass`** (`return g(...)`) is now a tail call: 243 → 194 ns in the same run as the base.

`TailCall` is dispatched in `exec_rare`. `run_hot` and `exec` are unchanged, byte for byte. At about 80 ns a hop, dispatch through `exec_rare` is not what a tail hop costs; the move of the window and the frame rebuild are. It stays there until a workload shows otherwise.

### Regression check, and code generation units

Against `5f22142` (Phase 3.16). In the default bench profile, plain calls, `nested_call`, `pcall_zero`, `gfor_lua`, and the vararg rows are within noise. Several metamethod and close rows moved between builds, in different directions from one build to the next: `meta_index_table_1` 85 → 114–139 ns in two builds, `meta_add_lua` −14% to +13% across four variants of this tree, `close_return` −1% to +16%.

The cause is code generation, not the tail-call code. The default profile compiles the crate in 16 code generation units. Which functions share a unit decides what LLVM can inline, and adding functions to `runtime.rs` moved `Table::get` and `Heap::table_get` so that `index::get` called them out of line. `table.rs`, `heap.rs`, and `index.rs` did not change. Built with one code generation unit (`CARGO_PROFILE_BENCH_CODEGEN_UNITS=1`), every function on the call, metamethod, and index paths is identical to the base's apart from one moved `inc` in `push_lua_frame` (`tools/code_diff.sh`), and a run with both sides built that way put every compared row within noise.

`#[inline]` on `Table::get` alone, on it and `Heap::table_get`, or a direct lookup in `index::get` each fixed some rows and moved others by as much, so none was kept. Building the bench profile with one code generation unit would make these comparisons depend on the code, not on how the crate is split; that is a change to every measured figure, so it is left for the performance track.

### Code size

| | bytes |
|---|---|
| `run_hot` | 8,945, unchanged |
| `exec` | 438, unchanged |
| `push_lua_frame` | 1,330 → 1,329 (now `enter_lua` with `Entry::Push`) |
| `tail_call` | 1,655, new |
| `tail_native` | 1,346, new, with `calls_at` |
| `exec_rare` | 8,818 → 4,495: LLVM now keeps `exec_next` (1,031) and `exec_raw_len` (510) out of line |
| bench text | 898,366 → 916,174, most of it the new rows |
| wasm probe | 630,140 → 641,345 |

## Logical operators, method calls, and function statements (Phase 3.18)

Filtered runs pinned to one core, 7 rounds, load average 2–5. Rows are ns per loop iteration.

| Workload | ns | fuel per iteration |
|---|---|---|
| `local x = v`, the loop the logical rows add to (`logic_base`) | 41 | 6 |
| `local x = not v` (`not_value`) | 38 | 8 |
| `v and w`, `nothing or w`, right side evaluated (`and_eval`, `or_eval`) | 47, 48 | 8 |
| `nothing and f()`, `v or f()`, the call skipped (`and_skip`, `or_skip`) | 42, 43 | 7, 8 |
| `o:get(1)` against `o.get(o, 1)` (`method_call`, `method_plain`) | 112, 113 | 10, 10 |
| `o:get(1)` with `get` found through an `__index` table (`method_index`) | 170 | 10 |
| a method returning another method call, a tail call (`method_tail`) | 179 | 14 |
| `function g() end` each iteration: a closure and a global store (`func_stmt`) | 119 | 8 |
| a call of a `local function` (`local_func`) | 91 | 9 |
| per hop of a `local function` tail recursion 100 deep (`local_tail`) | 74 | 8 |

- **Logical operators** are one to three hot-tier instructions each: a `Move` or two, a `JumpIfFalse`, and for `or` a `Jump`. They add 0–7 ns to the loop; a skipped right side costs nothing, whatever it contains. `not` is three executed instructions, `JumpIfFalse`, `LoadBool`, and a `Jump` or the other `LoadBool`; its 2 extra fuel units do not show in the time. No dedicated opcode was added: none of these leaves the hot tier.
- **Method calls** cost what the desugared call costs: `o:get(1)` and `o.get(o, 1)` both run a `Move` and a `GetField` before the call, and allocate nothing. The method call copies the receiver first, as PUC Lua's `SELF` does.
- **`local function`** and its tail recursion cost what `local f = function` and Phase 3.17's tail calls cost.
- **Registers**: a chain of 200 method calls, 200-operand `and` and `or` chains, a 50-part function name, and 50 nested local functions compile to at most 12 registers (`chains_of_the_new_forms_reuse_registers`). A method chain reuses its registers because the receiver is evaluated before its register pair is claimed, as in PUC Lua's `SELF`.

### Regression check

Against `7cc8d0a` (Phase 3.17): `run_hot`, `exec`, `exec_rare`, and every call, return, and metamethod handler compared are byte-identical (`tools/code_diff.sh`), the truthiness helper included. `scalar_call`, `call_lua_scalar`, `call_native_local`, `gfor_lua`, `hop_tail`, and `branch_loop` are within 1–6 ns. In the default 16-unit build, `ops::arith` differs although `ops.rs` did not change, and `meta_add_lua` came out 15% slower and `pcall_zero` 5% slower. Built with one code generation unit, `ops::arith` and the other handlers are identical to the base's, and the same rows came out 15–20% faster instead. Both are code placement (Phase 3.17 section).

### Code size

| | bytes |
|---|---|
| `run_hot`, `exec`, `exec_rare` | 8,945, 438, 4,495, unchanged |
| bench text | 916,174 → 930,142, the new rows and the compiler |
| wasm probe | 641,345 → 654,259 |

## `goto` (Phase 3.19)

Filtered runs pinned to one core, 7 rounds, load average up to 11. Rows are ns per pass of a 20,000-pass loop.

| Workload | ns | fuel per pass |
|---|---|---|
| numeric `for` body, no goto (`goto_none`) | 35 | 5 |
| the same with a forward goto skipping a statement (`goto_forward`) | 37 | 6 |
| a `while` loop (`goto_while`) | 26 | 8 |
| the same loop as a label and a guarded backward goto (`goto_backward`) | 25 | 8 |
| a backward goto leaving a captured local, a closure allocated each pass (`goto_upval`) | 131 | 12 |
| a backward goto leaving a `<close>` local, an empty Lua `__close` called each pass (`goto_close`) | 213 | 12 |
| a backward goto leaving three nested blocks, a captured local in the innermost (`goto_nested`) | 138 | 14 |
| `goto_backward` one instruction per slice (`goto_quantum1`) | 163 | 8 |

- **A goto with nothing to close is one `Jump`**, in the hot tier: a forward goto adds one instruction and about 2 ns; a goto loop costs what a `while` loop costs.
- **Cleanup costs what a scope exit costs**: one `CloseUpvalues` or `CloseScope` before the `Jump`, plus the closure or `__close` call the program itself makes. The goto does not allocate.
- **No label exists at run time**, so no dispatch or state changed: `run_hot`, `exec`, `exec_rare`, the call and return handlers, `close_scope`, and `close_step` are byte-identical to Phase 3.18's (`tools/code_diff.sh`), and the other rows measured are within noise of it.
- **Registers**: the register-bound checks are compile-time only.

### Code size

| | bytes |
|---|---|
| `run_hot`, `exec`, `exec_rare` | 8,945, 438, 4,495, unchanged |
| bench text | 930,046 → 943,838, the new rows and the compiler |
| wasm probe | 654,259 → 669,318 |


## The base library (Phase 3.21)

Filtered runs pinned to one core, 7 rounds. The machine was busy: ranges were wide and every row read 20–50% slower than in Phase 3.19, so compare rows within this section only. Rows are ns per call, in a 20,000-pass numeric `for` body, except where noted.

| Workload | ns |
|---|---|
| the loop with a local copy (`base_none`) | 54 |
| `type(v)` (`type_value`) | 238 |
| `assert(v)` (`assert_true`) | 174 |
| `tostring(i)`, an integer (`tostring_int`) | 323 |
| `tostring(1.5)` (`tostring_float`) | 912 |
| `tostring(s)`, a string (`tostring_string`) | 247 |
| `tostring(t)`, a table (`tostring_table`) | 304 |
| `tostring(m)` with a Lua `__tostring` (`tostring_meta`) | 323 |
| `tonumber('42')` (`tonumber_string`) | 191 |
| `tonumber('ff', 16)` (`tonumber_base`) | 200 |
| `next(t)` (`next_first`) | 177 |
| `pairs(t)` (`pairs_setup`) | 201 |
| `print(i)` with no output set (`print_int`) | 343 |
| `ipairs` over ten elements, per element (`ipairs_step`) | 214 |
| `pairs` over ten elements, per element (`pairs_step`) | 182 |
| `load('return 1')`, per load (`load_small`) | 854 |
| `load` with a three-piece reader, per load (`load_reader`) | 2,543 |
| a proof native through a global, for comparison (`call_native_global`) | 243 |

- **A base function costs what a native call through a global costs**: a `GetGlobal` of the name and one `Call`. `type`, `assert`, `next`, `pairs`, and `tonumber` are within that.
- **`tostring` allocates** for a number, a table, or a function; the float text is `%.14g` built from Rust's formatting (`concat::number_text`), which is most of the 912 ns. A string, nil, or a boolean returns an existing or reserved string.
- **`tostring_meta`** is a builtin frame, a Lua call, and the frame's finishing step, about 150 ns over a plain `tostring`.
- **`print`** is the conversion and one journal commit per write. With a host output, the output's own cost adds to it.
- **`ipairs` is an iterator call per element**, like any generic `for`: no fast path was added.
- **`load`** compiles and installs a prototype per call; a reader adds a builtin frame and one Lua call per piece.

### Regression check

The ordinary rows, against Phase 3.20 (`7bc77cd`) in the same run, 7 rounds: arithmetic and `for` loops, captures and upvalue calls, Lua and native calls, metamethod reads and writes, `#`, constructors, field reads, collection, `pcall`, and generic `for`. None is slower beyond the noise. Most are equal or faster: native calls, metamethods, and `pcall` by 10–25%, which is code placement, not a change to those paths.

`run_hot` and `exec_rare` are the same size as before, with a few lines differing in call targets; `exec` is identical. `run_native` is smaller, 1,738 → 1,333 bytes, because its return path moved into `native_returned`, which the builtins share. `push_lua_frame` grew by 56 bytes.

### Code size

| | bytes |
|---|---|
| `run_hot`, `exec`, `exec_rare` | 8,945, 438, 4,495, unchanged |
| bench text | 947,742 → 985,486, the base library and the new rows |
| wasm probe | 669,318 → 722,297 |

After the milestone review's fixes (a unit of fuel per base-function frame step, `print` output in pieces), the affected rows, 5 rounds on a quieter machine, before and after: `tostring_meta` 327 → 297, `print_int` 264 → 273, `load_small` 698 → 692, `load_reader` 2,135 → 2,083 ns. No change beyond noise.

## The math and table libraries (Phase 3.22)

Filtered runs pinned to one core; the machine was shared, so compare rows within this section. Math rows are ns per call in a 20,000-pass loop; `base_none` is the loop with a local copy.

| Workload | ns |
|---|---|
| the loop (`base_none`) | 48 |
| `math.abs` of an integer / a float | 248 / 273 |
| `math.floor`, `math.sqrt` | 279, 286 |
| `math.sin`, `math.cos`, `math.log` | 283, 276, 274 |
| `math.random()`, `math.random(1, 1000)` | 246, 274 |
| `math.max(v, f, 3)` | 388 |

A math function costs what a native call through a global and a field costs. The `libm` backend is not visible at this scale.

Measured alone, `libm` against the host's glibc, ns per call:

| Function | `libm` | glibc |
|---|---|---|
| `sin` | 12.4 | 15.7 |
| `cos` | 13.0 | 15.1 |
| `atan2` | 10.8 | 15.7 |
| `log` | 10.7 | 8.2 |
| `exp` | 13.6 | 7.4 |
| `pow` | 49.3 | 16.3 |

`^` therefore costs about 33 ns more per call when the exponent is not 2.

Table rows are ns per call, over lists built before the timed loop. They were measured before the `#` cache; the `#` rows were measured again after it.

| Workload | ns | fuel per call |
|---|---|---|
| `table.pack(1, 2, 3, 4)` | 2,213 | 13 |
| `table.pack` of 32 | 7,525 | 42 |
| `table.unpack` of 4 / 32 / 256 | 1,063 / 3,625 / 24,755 | 10 / 11 / 22 |
| `table.concat` of 10 / 1,000 | 1,598 / 143,378 | 11 / 182 |
| `table.insert(t, v)`, `table.remove(t)` at 5,000 elements, before the `#` cache | 43,257 / 43,409 | |
| the same after the cache | 808 / 1,575 | 11 / 17 |
| `table.insert(t, 1, v)` / `table.remove(t, 1)` at 1,000 elements | 94,675 / 102,599 | 43 / 49 |
| `table.move` of 100 / 10,000, overlapping | 17,056 / 1,901,353 | 20 / 4,139 |
| `table.sort` of 10 / 100 / 1,000 | 10,824 / 177,500 / 2,674,230 | 44 / 88 / 1,309 |
| `table.sort` of 1,000 with a Lua order function | 3,947,657 | 29,146 |

- **A table function is a loop of semantic operations**, each a table read or write through the VM's own paths: about 190 ns per element moved (a read and a write), and about 250 ns per comparison in a sort, its reads and writes included. There is no fast path for plain tables yet; one would skip the operation dispatch when the list has no metatable.
- **Fuel grows with the work**: a unit per 32 operations. A sort with a Lua order function also pays for the function's own instructions.
- **`#` was a scan.** The smallest border was found by counting the positive integer keys over every slot, then probing up from 1. So `table.insert(t, v)` on 5,000 elements cost 43 µs, and so did every `#t`. `Table` now keeps both numbers as entries come and go (ADR 0033), and appending costs 0.8 µs. The result is unchanged, so the tables revision is unchanged.

### Regression check

Ordinary rows against Phase 3.21 (`8d96647`), 5 rounds, in ns: nothing beyond noise.

| Row | Phase 3.21 | Now |
|---|---|---|
| `len_table_lua` | 224 | 189 |
| `len_table_native` | 228 | 184 |
| `ctor_mixed` | 1,042 | 1,026 |
| `call_native_global` | 224 | 215 |
| `pcall_zero` | 301 | 276 |
| `gfor_lua` | 173 | 170 |
| `for_int` | 34 | 34 |
| `arith_loop` | 14 | 14 |

`#` itself got faster with the cache.

## The string library (Phase 3.23)

Filtered runs pinned to one core; the machine was shared, so compare rows within this section. Rows are ns per call in a loop whose strings are made before it; `base_none` is the loop alone.

| Workload | ns | fuel per call |
|---|---|---|
| the loop (`base_none`) | 44 | 6 |
| `string.len` of 100 bytes | 189 | 10 |
| `string.sub`, 16 bytes / 1,000 bytes of 1 KiB | 307 / 356 | 12 / 12 |
| `string.reverse`, 1 KiB / 1 MiB | 388 / 406,899 | 10 / 292 |
| `string.lower`, 1 KiB | 371 | 10 |
| `string.rep('abc', 100, ',')` | 1,442 | 12 |
| `string.byte`, one / 16 results | 211 / 227 | 11 / 12 |
| `string.find`, plain, 1 KiB | 524 | 13 |
| `string.find`, `ne+dle` in 1 KiB | 904 | 11 |
| `string.find` with two captures | 761 | 11 |
| `string.find(('a'):rep(1000), 'a*b')`, a failing backtrack | 1,823,463 | 2,039 |
| `string.match` with three captures | 816 | 11 |
| a `gmatch` loop over 200 words | 74,194 (371 a word) | 1,019 |
| `string.gsub`, 128 string replacements in 1 KiB | 6,742 | 14 |
| `string.gsub`, 200 function replacements | 64,187 (321 a call) | 412 |
| `string.format('%d')` / `('%.3f')` / four items | 336 / 499 / 727 | 11 / 13 / 16 |
| `string.pack('i4 i8 d')` / `unpack` | 512 / 398 | 13 / 11 |
| `string.pack` / `unpack` of 1,000 `i4` | 159,099 / 29,827 | 109 / 75 |
| `string.dump` of a small / 300-statement function | 610 / 33,024 | 10 / 16 |
| `load` of that function's chunk / of its source | 40,046 / 138,544 | 17 / 15 |

The same shapes in PUC Lua 5.4.9 on this machine, timed with `os.clock` around a Lua loop that calls a closure (so its call overhead is included too):

| Workload | Moonseed | PUC Lua |
|---|---|---|
| `find` of `ne+dle` in 1 KiB | 904 | 5,041 |
| `find` of a plain word in 1 KiB | 524 | 66 |
| the failing backtrack | 1,823,463 | 3,247,600 |
| `gsub`, 128 replacements in 1 KiB | 6,742 | 7,957 |
| a `gmatch` loop over 200 words | 74,194 | 15,362 |
| `format` of four items | 727 | 318 |
| `match` with three captures | 816 | 187 |
| `reverse` of 1 MiB (PUC's figure includes making it) | 406,899 | 2,438,750 |

- **The pattern machine** (ADR 0037) costs about 28 ns per transition, four to six times PUC's recursive C. Two shortcuts that change no result take most of that back where it matters: a scan skips positions that cannot start a match when the pattern starts with a plain byte, and a greedy repetition counts its bytes and skips hopeless retries in one step. Before them, `find` of `ne+dle` took 29,117 ns, the backtrack 14.2 ms, and the `gsub` 28,215 ns.
- **A call costs more than PUC's.** A string function is a native call through a global and a field (about 200 ns here), and a result is a new string. That dominates `match`, `format`, and each `gmatch` iteration, which also copies the iterator's state in and out of its closure and makes its capture strings. A plain `find` also pays for its machine; PUC's is a `memchr` and a `memcmp`.
- **Byte work is cheap:** `reverse`, `lower`, `upper`, `sub`, and `rep` copy at 0.3–0.4 ns a byte, and cost a unit of fuel per 4 KiB after their first step.
- **Fuel grows with the work:** the backtrack costs 2,039 units, the 1 MiB `reverse` 292.
- **Snapshot state.** A `gsub` paused 40 KiB into its result adds its text so far and a few dozen integers: the snapshot grew from 58,676 to 99,085 bytes, and took 0.4 ms to write.

### Regression check

Ordinary rows against Phase 3.22 (`9f9d7de`), 5 rounds, in ns, in the default build and in one-codegen-unit builds with every function aligned to 64 bytes, the second layout the Phase 3.11 note recommends:

| Row | 3.22 default | 3.23 default | 3.22 one unit, aligned | 3.23 one unit, aligned |
|---|---|---|---|---|
| `len_table_native` | 146 | 181 | 138 | 173 |
| `call_native_global` | 151 | 164 | 161 | 162 |
| `pcall_zero` | 204 | 207 | 239 | 210 |
| `meta_lt_lua` | 217 | 192 | 187 | 186 |
| `meta_index_table_8` | 307 | 318 | 227 | 233 |
| `table_concat10` | 1,307 | 1,379 | 1,312 | 1,396 |
| `table_unpack32` | 2,609 | 2,925 | 2,418 | 2,512 |
| `table_sort10` | 9,179 | 9,505 | 8,129 | 9,167 |
| `for_int`, `arith_loop` | 28, 10 | 28, 10 | | |

The first comparisons found three real costs, now fixed:
- `index::metamethod` was no longer inlined into the indexing paths once it looked up type metatables, which made an 8-deep `__index` chain 37% slower. The table case is again one test, the other types' lookup is out of line, and `metamethod` is inlined.
- The string functions' arms in the table functions' machine loop made table rows 10–24% slower in every layout. String work now has a loop of its own.
- Every metamethod lookup went through a jump table over the value's type.

What remains moves both ways between layouts, within the placement band recorded for multi-handler paths since Phase 3.11.

## `require` and the debug library (Phase 3.24)

Filtered runs pinned to one core, on a shared machine: compare rows within this section. Per call, in a loop (`call_native_global`, a native call through a global and a field, was 216 ns in the same run):

| Workload | ns | fuel per call |
|---|---|---|
| `require` of a loaded module | 407 | 9 |
| `require` through the preload searcher, first time | 3,455 | 27 |
| `debug.getinfo(1, 'Sl')` | 1,497 | 11 |
| `debug.getinfo(f)`, every field | 3,290 | 10 |
| `debug.getlocal(1, 2)` | 292 | 11 |
| `debug.getupvalue(f, 1)` | 275 | 11 |
| `debug.traceback` at depth 10 / 100 / 1,000 | 5,797 / 10,254 / 15,134 | 10 / 11 / 30 |

- **A traceback shows at most 22 levels**, so its cost grows with depth only through the level walk (up to 1,080 frames) and the search of `package.loaded` for each shown function's name, 256 entries a step.
- **`getinfo` makes its table and strings** (10 objects for the full form); the level walk and the line lookup are small next to that.
- **`require` of a loaded module** makes the `_LOADED` key string each call, and runs as a machine of a few steps.

**Debug information.** A chunk of 1,510 instructions carries 15,641 logical bytes of it, about 10 bytes an instruction, and its snapshot grows from 11,130 to 15,545 bytes, since the snapshot encodes lines as deltas of about a byte each. Binary chunks carry it too: against Phase 3.23, `string.dump` of a 300-statement function takes 50 µs instead of 31 µs, and `load` of the dump 57 µs instead of 36 µs. Compiling the same function's source takes 4% longer.

### Regression check

Ordinary rows against Phase 3.23 (`d7340c9`), 5 rounds, default build, in ns:

| Row | 3.23 | 3.24 |
|---|---|---|
| `for_int`, `arith_loop` | 26, 10 | 26, 10 |
| `call_2`, `call_4`, `call_lua_scalar` | 94, 102, 96 | 92, 97, 96 |
| `call_native_global`, `call_native_local` | 159, 117 | 167, 95 |
| `call_table` | 129 | 135 |
| `pcall_zero` | 225 | 185 |
| `meta_lt_lua`, `meta_index_table_8` | 209, 279 | 198, 255 |
| `len_table_native` | 188 | 170 |
| `table_concat10`, `table_sort10` | 1,361, 8,803 | 1,364, 8,888 |
| `string_find_simple` | 640 | 608 |

Nothing was added to the dispatch loop; a frame gained a tail-call flag and is still 112 bytes. The differences are within the placement band recorded since Phase 3.11.

## The coroutine library (Phase 3.25)

Filtered runs pinned to one core, on a shared machine: compare rows within this section. Per operation in a loop (`call_native_global`, a native call through a global and a field, was 260 ns in the same run):

| Workload | ns | fuel |
|---|---|---|
| a direct Lua call, for comparison (`co_direct_call`) | 125 | 9 |
| `coroutine.create` | 329 | 10 |
| create, then a first resume that returns | 776 | 16 |
| `resume` of a coroutine that yields back (a switch there and back) | 594 | 18 |
| a `wrap` function's call that yields back | 441 | 15 |
| `wrap`, then a call that returns | 756 | 13 |
| `status` / `running` / `isyieldable` | 329 / 263 / 263 | 10 / 9 / 9 |
| create, resume to a yield, close / with one `<close>` value | 1,124 / 1,815 | 24 / 31 |

- **A switch** costs a builtin call on each side, the stack check for the values moved, and the delivery through the receiving call's result window. Nothing is allocated for a switch; creating a coroutine allocates its thread (and a trampoline for a builtin body).
- **Snapshots:** a suspended coroutine yielded from a small function adds about 170 bytes (+378 for the first, which brings its function's prototype, +16,812 for 100).
- **The Rust stack** does not grow with coroutine nesting: 196 nested coroutines run on a 256 KiB thread stack in the tests.
- `ThreadObj` did not change.

### Regression check

Ordinary rows against Phase 3.24 (`5b1a0a4`), in ns. In the default build, `call_native_global` read 212 before and 243 after, `pcall_zero` 284 and 302, and `len_table_native` 201 and 218; in one-codegen-unit builds with every function aligned to 64 bytes, 7 rounds:

| Row | 3.24 | 3.25 |
|---|---|---|
| `call_native_global` | 214 | 165 |
| `call_native_local` | 135 | 109 |
| `pcall_zero` | 281 | 252 |
| `len_table_native` | 199 | 170 |
| `call_2` | 130 | 122 |
| `meta_index_table_8` | 295 | 292 |
| `for_int` | 37 | 37 |

The builtin dispatch gained one arm. The default-build differences reverse in the second layout: code placement, the band recorded since Phase 3.11.

## Userdata (Phase 3.26)

Filtered runs pinned to one core, on a shared machine: compare rows within this section. Per operation in a loop, default build (`call_native_global` was 161 ns and a table field read 63 ns in the same run):

| Workload | ns |
|---|---|
| `newud(0)` / `newud(64)` / `newud(4096)` / `newud(0, 4)` | 201 / 222 / 345 / 223 |
| `a == b`, two full userdata, no `__eq` / two light userdata | 90 / 78 |
| `t[u]`, a full userdata key / a light userdata key / `t.x`, for comparison | 156 / 62 / 63 |
| `u.x` through `__index` table / `u.x = i` through `__newindex` table | 154 / 232 |
| `debug.getuservalue(u)` / `debug.setuservalue(u, i)` | 144 / 157 |
| `counter_get(h)`, a typed host borrow / `h:get()` through a method table | 181 / 252 |
| `debug.upvalueid(f, 1)` | 151 |

- **Making one** is a native call plus the object; a 4 KiB byte payload adds its zeroing. A full userdata as a key takes the object-key path tables and functions take (the hot tier declines object keys); a light key stays in the hot tier.
- **A typed borrow** (`userdata_ref::<T>`, a `TypeId` check and a downcast) adds about 20 ns to a native call.
- **Collection** of 4,000 live userdata: 7 ns each with no user values, 8 with four, 6 with a 4 KiB payload (bytes are not traced), 7 with a metatable.
- **Snapshots:** a byte userdata of 16 bytes with two user values adds about 74 bytes (+98 for the first); a portable host counter about 93 (+116 for the first).
- `Value` stays 16 bytes; a compile-time assertion keeps it there.

### Regression check

Ordinary rows against Phase 3.25 (`8233b8b`), 9 rounds, in ns. In the default build every row is equal or faster (`src_field_read` 74 → 44, `call_native_global` 204 → 156); the Phase 3.25 build happened to sit in a slow placement. In one-codegen-unit builds with every function aligned to 64 bytes:

| Row | 3.25 | 3.26 |
|---|---|---|
| `for_int` / `while_loop` / `cmp_int_eq` | 33 / 34 / 39 | 33 / 35 / 39 |
| `src_field_read` | 44 | 47 |
| `src_int_key_read` | 58 | 62 |
| `src_field_update` | 48 | 53 |
| `src_global_read` | 73 | 71 |
| `meta_index_hit` | 45 | 48 |
| `call_lua_scalar` / `call_native_global` | 122 / 164 | 114 / 156 |
| `meta_index_lua` / `meta_add_lua` | 242 / 241 | 217 / 229 |
| `ctor_4` | 583 | 630 |

Table hits read 5–10% slower in this layout. The machine code shows why: a table's index is a `HashMap<TableKey, u32>` probed with a borrowed `&dyn AsKeyView`, and LLVM used to inline the probe's key comparison and see through the trait object; with the light userdata arm in `TableKey` and `KeyView`, the comparison closure is out of line and calls `as_view` through the vtable on each probe. A derived `KeyView` equality made it worse (`src_global_read` 159 against 145 in a first run); the hand-written one (string and integer keys in line) recovered that part. `#[inline]` on the key-view functions did not change LLVM's choice. A probe that does not go through a trait object is left for the performance campaign (Phase 3.36). `op_add_string` fails at Phase 3.25 too (its source needs the string library, which the bench does not install for it), so it is not in the table.

## Weak tables, ephemerons, and finalizers (Phase 3.27)

Filtered runs pinned to one core, on a shared machine (load average near 10): compare rows within this section. Full collections over 4,000 live entries, 20 runs each (`gc_full_*`):

| Heap | median ns per entry | median pause | p95 | p99 |
|---|---|---|---|---|
| strong tables | 8 | 32 µs | 37 µs | 37 µs |
| a weak-value table over them | 13 | 55 µs | 69 µs | 69 µs |
| an ephemeron table keyed by them | 14 | 59 µs | 96 µs | 96 µs |
| full userdata with a user value | 7 | 31 µs | 39 µs | 39 µs |
| registered (finalizable) tables, alive | 19 | 80 µs | 90 µs | 90 µs |
| mixed: finalizable tables, userdata, ephemeron and weak-value entries | 12 | 48 µs | 64 µs | 64 µs |

- **Ephemerons** settle in linear time: a 4,500-link chain built so every key is found unmarked costs 15 ns per link per collection (`gc_ephemeron_chain`), a 1,500-table chain 87 ns per link (`gc_ephemeron_tables`, a table's mode lookup per link). A first version traversed ephemeron tables as it reached them, so nearly every entry went through the waiting map: 114 ns per entry. Ephemeron tables are now traversed after the rest of the graph.
- **Clearing** dead weak values: 65 ns each for 100, 47 for 1,000, 52 for 9,000 (`gc_weak_clear_*`): linear, no rehash.
- **Finalizers**, per object, 4,000 dead objects made and collected (`gc_fin_*`): 237 ns with a metatable without `__gc`, 455 with an empty Lua `__gc`, 421 with a native one, 447 with a second collection after (which frees them). A finalizer's discovery, queueing, and call cost about 200 ns, the second collection's reclamation little more.

### Regression check

Ordinary rows against Phase 3.26 (`c4ef09f`), 9 rounds, in ns, default build / one-unit aligned build:

| Row | 3.26 | 3.27 |
|---|---|---|
| `gc_tables_on` | 332 / 334 | 353 / 337 |
| `gc_closures_on` | 239 / 232 | 239 / 232 |
| `gc_retained_on` | 303 / 312 | 332 / 296 |
| `for_int` | 27 / 32 | 27 / 31 |
| `src_field_read` | 41 / 45 | 41 / 44 |
| `call_lua_scalar` | 103 / 105 | 104 / 105 |
| `call_native_global` | 154 / 145 | 146 / 140 |
| `meta_index_table_1` | 106 / 107 | 98 / 106 |
| `set_metatable` | 158 / 153 | 158 / 156 |
| `ctor_4` | 524 / 555 | 565 / 521 |

The collector reads each traversed table's `__mode` when it has a metatable, and checks the ephemeron map on each mark; the churn rows differ by a few percent with opposite signs in the two layouts: placement. The load average rose to 28 during these runs.

## The incremental collector (Phase 3.28)

Filtered runs pinned to one core on a shared machine whose load average ran between 9 and 24 during these measurements: compare rows within one run, not across runs or with earlier sections.

**Total cost.** A full collection (`Runtime::collect`, `gc_full_*`) against the Phase 3.27 collector (`gc_reference.rs`, `gc_ref_*`) over the same heaps, ns per entry, two runs:

| Heap | quieter run: incremental / 3.27 | loaded run: incremental / 3.27 |
|---|---|---|
| 4,000 strong tables | 9 / 9 | 16 / 11 |
| and a weak-value table over them | 13 / 17 | 25 / 20 |
| and an ephemeron table keyed by them | 14 / 18 | 25 / 20 |

So between 0.75× and 1.45× of the stop-the-world collector. Two changes got it there from a first version at 2.7× to 4×: marks moved from the arena slots, one line each with the object, into a dense byte array per arena (propagation from 9.4 to about 5 ns a unit), and the sweep passes a batch of slots per call (11 to 1.2 ns an object).

**Slices** (`gc_inc_*`): each program run to its end with the default parameters, best of five. Without a quantum a slice is a whole step, or a step and an atomic phase:

| Workload | slices | p50 | p95 | p99 | max | units p50 | largest atomic | collector / run | stop-the-world, final heap | largest slice, 1,000-fuel quantum |
|---|---|---|---|---|---|---|---|---|---|---|
| 1,000 live tables, churn | 90 | 8 µs | 44 µs | 50 µs | 50 µs | 811 | 50 µs | 14% | 21 µs | 85 µs |
| 9,000 live tables, churn | 162 | 27 µs | 215 µs | 226 µs | 238 µs | 9,530 | 238 µs | 60% | 128 µs | 96 µs |
| a 300,000-entry table being built | 563 | 45 µs | 283 µs | 737 µs | 912 µs | 16,937 | 912 µs | 23% | 616 µs | 292 µs |
| 40 coroutines 150 frames deep | 68 | 5 µs | 31 µs | 34 µs | 34 µs | 811 | 27 µs | 10% | 5 µs | 46 µs |
| a 4,000-entry weak-value table | 107 | 19 µs | 64 µs | 68 µs | 72 µs | 3,419 | 72 µs | 16% | 76 µs | 89 µs |
| a 4,000-entry ephemeron table | 218 | 87 µs | 127 µs | 173 µs | 214 µs | 13,889 | 214 µs | 37% | 194 µs | 61 µs |
| 2,000 finalizable tables | 11 | 8 µs | 51 µs | 51 µs | 51 µs | 811 | 51 µs | 5% | 121 µs | 42 µs |
| mixed: finalizable, userdata, both weak kinds, a 50,000-entry table | 180 | 19 µs | 116 µs | 204 µs | 221 µs | 5,767 | 221 µs | 16% | 211 µs | 113 µs |
| churn of two-entry tables | 174 | 16 µs | 23 µs | 31 µs | 33 µs | 808 | 33 µs | 9% | 2 µs | 24 µs |
| a list of 9,000 tables growing | 85 | 15 µs | 117 µs | 175 µs | 175 µs | 2,373 | 139 µs | 26% | 148 µs | 45 µs |

- **The median slice** is a default step, about 800 units, or larger near the object limit, where a step does its share of finishing a cycle in the room left (9,530 and 13,889 units for the two heaps of 8,000 to 9,000 objects).
- **The largest slices are atomic phases.** The phase re-traces every table written to and every object made while the cycle marked, as Lua's does: the table being built is traced again whole (325,000 units, 0.9 ms). These are as long as a stop-the-world collection of the heap, or longer, where the heap is mostly what the cycle saw change. Phase 3.29's young generation is aimed at that work.
- **With a quantum** the executor returns to the host between units: a slice is at most four units a unit of fuel and one piece. The largest slices measured with a 1,000-fuel quantum, 24–292 µs, include the machine's own preemption at this load.
- **The collector's share** of a run's time is 5–37% on these allocation-heavy programs; 60% for the heap of 9,000 objects under a 10,000-object limit, which the scheduling collects nearly continuously to stay under it.

**Barriers** (`bw_*`, 200,000 writes, ns per write, the plain row with no cycle running, `_marking` with one stopped while it marks):

| Write | no cycle | marking |
|---|---|---|
| table field update | 62 | 67 |
| table insert | 447 | 355 (no collection runs while stopped) |
| global | 127 | 134 |
| upvalue | 59 | 60 |
| `debug.setuservalue` | 340 | 346 |
| a register-only loop | 52 | 52 |

An armed barrier costs one byte load and compare on the first write to an object after it is traced; a dormant one, a flag test on each mutable access.

### Regression check

Rows against Phase 3.27 (`a070b25`), 7 rounds, default build / one-unit aligned build, ns:

| Row | 3.27 | 3.28 |
|---|---|---|
| `int_field` | 153 / 327 | 164 / 285 |
| `string_field` | 193 / 441 | 200 / 381 |
| `upvalue_call` | 151 / 286 | 156 / 339 |
| `arith_loop` | 11 / 26 | 11 / 26 |
| `alloc_churn` | 29 / 63 | 38 / 66 |
| `gc_tables_on` | 335 / 737 | 403 / 638 |
| `gc_tables_off` | — / 934 | — / 842 |
| `gc_retained_on` | 304 / 586 | 346 / 613 |
| `gc_closures_on` | 242 / 587 | 271 / 583 |

The two builds ran at different machine loads (the aligned pair later, at about twice the times). In the default build the allocation rows read 12–31% slower, `gc_tables_off` too, which runs no collection at all; in the aligned build they move both ways within the noise, so the default-build gap is code placement in the cold allocation handlers (ADR 0022), not collector work. Allocating a new object during a cycle gray instead of white, as Lua does, measured the same on these rows.

## Generational collection (Phase 3.29)

Filtered runs pinned to one core on a shared machine whose load average ran between 11 and 40 during these measurements: compare rows within one run, not across runs or with earlier sections.

**Incremental against generational** (`gc_inc_*`, `gc_gen_*`): the same program run to its end in each mode with the default parameters, best of five. The ms columns are the collector's time and the run's. The µs columns are slices: without a quantum, a slice is a whole step, an atomic phase, or a whole young collection. "1k quantum" is the largest slice with a 1,000-fuel quantum. "Remembered" is the most objects waiting for the next young collection: revisited and touched.

| Workload | mode | collector ms | run ms | full cycles | young | p50 µs | p95 µs | p99 µs | max µs | 1k quantum µs | peak KiB | remembered |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 3,000 old tables of 100 entries, young churn | inc | 116.5 | 191.2 | 73 | 0 | 84.8 | 280.3 | 341.7 | 445.0 | 275.9 | 9,819 | 0 |
| | gen | 8.8 | 81.0 | 12 | 105 | 12.6 | 63.6 | 102.7 | 110.1 | 52.3 | 9,683 | 48 |
| the same, an old table given a new table each round | inc | 136.3 | 219.1 | 80 | 0 | 83.4 | 364.0 | 497.6 | 654.9 | 423.6 | 9,858 | 0 |
| | gen | 11.0 | 83.3 | 12 | 116 | 13.8 | 89.3 | 105.0 | 130.5 | 71.5 | 9,774 | 111 |
| a 300,000-entry table being built (Phase 3.28's worst case) | inc | 38.7 | 142.4 | 47 | 0 | 37.5 | 300.9 | 711.6 | 906.3 | 232.0 | 9,724 | 0 |
| | gen | 6.4 | 106.2 | 13 | 53 | 11.9 | 89.7 | 365.9 | 383.7 | 347.8 | 9,749 | 19 |
| 9,000 live tables, churn | inc | 10.8 | 17.6 | 57 | 0 | 26.4 | 174.4 | 177.0 | 177.5 | 48.8 | 883 | 0 |
| | gen | 1.5 | 7.9 | 6 | 64 | 5.9 | 52.2 | 56.8 | 66.7 | 49.5 | 870 | 649 |
| 1,000 live tables, churn | inc | 0.87 | 6.5 | 10 | 0 | 5.4 | 30.4 | 38.4 | 38.4 | 60.6 | 330 | 0 |
| | gen | 0.72 | 6.3 | 3 | 21 | 24.4 | 38.4 | 46.2 | 46.2 | 92.9 | 307 | 648 |
| a 4,000-entry ephemeron table | inc | 11.1 | 30.4 | 64 | 0 | 72.2 | 87.2 | 93.6 | 120.7 | 67.0 | 698 | 0 |
| | gen | 3.2 | 21.3 | 6 | 55 | 42.8 | 58.2 | 78.7 | 78.7 | 40.9 | 680 | 778 |
| a 4,000-entry weak-value table | inc | 2.1 | 12.7 | 16 | 0 | 16.5 | 48.8 | 56.9 | 60.9 | 105.8 | 538 | 0 |
| | gen | 1.4 | 13.5 | 4 | 18 | 14.7 | 65.2 | 69.9 | 69.9 | 58.1 | 542 | 650 |
| mixed: finalizable, userdata, both weak kinds, a 50,000-entry table | inc | 3.9 | 22.1 | 18 | 0 | 18.6 | 68.8 | 144.2 | 151.5 | 112.6 | 2,043 | 0 |
| | gen | 2.3 | 20.1 | 10 | 13 | 13.3 | 64.8 | 152.4 | 165.1 | 50.1 | 1,934 | 4 |
| a list of 9,000 tables growing | inc | 2.3 | 7.9 | 14 | 0 | 10.2 | 101.3 | 158.0 | 158.0 | 31.5 | 856 | 0 |
| | gen | 1.5 | 7.0 | 7 | 10 | 10.6 | 59.7 | 148.4 | 148.4 | 30.1 | 868 | 477 |
| 2,000 finalizable tables | inc | 0.12 | 2.2 | 1 | 0 | 7.2 | 42.1 | 42.1 | 42.1 | 33.6 | 284 | 0 |
| | gen | 0.19 | 2.5 | 1 | 2 | 10.4 | 72.6 | 72.6 | 72.6 | 67.3 | 334 | 970 |
| 40 coroutines 150 frames deep | inc | 0.66 | 6.5 | 22 | 0 | 4.4 | 23.4 | 24.2 | 24.2 | 29.1 | 117 | 0 |
| | gen | 0.61 | 6.5 | 1 | 23 | 26.3 | 29.0 | 32.4 | 32.4 | 32.6 | 94 | 48 |
| churn of two-entry tables | inc | 1.91 | 20.8 | 87 | 0 | 13.9 | 14.8 | 17.5 | 23.1 | 21.8 | 81 | 0 |
| | gen | 2.01 | 21.1 | 1 | 87 | 22.7 | 24.4 | 31.5 | 31.5 | 41.6 | 73 | 5 |
| short-lived strings | inc | 1.19 | 23.7 | 68 | 0 | 5.5 | 7.3 | 11.4 | 14.6 | 21.4 | 89 | 0 |
| | gen | 1.46 | 25.3 | 1 | 68 | 20.6 | 27.3 | 36.2 | 36.2 | 33.9 | 73 | 5 |
| short-lived closures | inc | 0.91 | 15.1 | 65 | 0 | 4.6 | 5.5 | 7.9 | 8.2 | 10.1 | 89 | 0 |
| | gen | 1.06 | 15.8 | 1 | 65 | 16.0 | 20.0 | 22.2 | 22.2 | 21.7 | 73 | 5 |
| short-lived userdata | inc | 0.47 | 6.9 | 25 | 0 | 10.3 | 10.6 | 16.7 | 16.7 | 14.3 | 81 | 0 |
| | gen | 0.53 | 7.4 | 1 | 25 | 20.1 | 21.8 | 40.9 | 40.9 | 45.8 | 73 | 5 |

- **A young collection does not trace the old heap.** With 3,000 old tables of 100 entries (300,000 entries), the median slice, a young collection, is 2,700–4,400 units; the largest, 14,500–15,100, are steps of a major collection. Incremental slices over the same heap are 14,100–14,800 units at the median and up to 39,000–51,000. The collector's time falls 12-fold (13-fold where old tables are written to), and the run's by 58–62%. A unit test holds the bound: under 1,000 units for 100 young tables beside 8,000 old ones.
- **Phase 3.28's worst case** (a 300,000-entry table being built): the collector's time falls from 38.7 to 6.4 ms, and full cycles from 47 to 13; the largest slice from 906 to 384 µs, p99 from 712 to 366 µs. Storing a number gives the table no reference, so the barrier leaves it alone and young collections do not trace it again. Before that refinement each young collection traced the table whole: 729 µs.
- **Mostly-live growth** falls back as Lua's bad collections do. The growing list, and the build phases of the old-graph rows, run 7–13 full cycles instead of 14–80. The `bad_majors_fall_back_and_return` test checks it: no young collections while falling back, cycles once each time the heap doubles, back to young collections when growth stops.
- **Pure churn** with a tiny live heap costs about the same: 1.06 against 0.91 ms for closures, 1.46 against 1.19 for strings, 2.01 against 1.91 for tables, the runs within 0–7%. Both modes sweep each dead object once. A young collection is one slice of about 1,000 objects (20–40 µs), where incremental steps are 5–15 µs; a quantum still splits it. A first version passed each young object through the heap's dispatch by kind and computed its size there: 1.6–3× the incremental time. The arena's own loop (`Arena::sweep_young_some`) removed that.
- **Remembered objects** stay small: 4 to 111 where old objects are written to, up to 970 right after a finalizable or large live set is promoted (`OLD1` objects waiting for their one extra trace).

**Barriers** (`bw_*`, 200,000 writes, ns per write, 7 rounds; incremental with no cycle, incremental while marking, generational into a young object, and into an old one):

| Write | inc, no cycle | inc, marking | gen, young | gen, old |
|---|---|---|---|---|
| table field update | 44 | 45 | 45 | 45 |
| table insert | 332 | 272 | 284 | 284 |
| global | 67 | 67 | 67 | 68 |
| upvalue | 44 | 44 | 45 | 45 |
| `debug.setuservalue` | 207 | 211 | 207 | 208 |
| a register-only loop | 38 | 38 | 38 | 38 |

Within noise. The armed barrier costs a byte compare on the first write to an old object after each young collection, and nothing after: the object is gray. The table insert row differs by the collections each mode runs, not by the barrier.

**Memory.** One age byte per object beside its mark byte, and a 4-byte young-list entry per young object. The remembered set is the existing `again` lists plus an 8-byte entry per `OLD1` or `TOUCHED2` object. Snapshots write marks and ages only where they differ from a default (black and old off the young lists in generational form). A heap of 5,209 objects between young collections snapshots to 360,482 bytes, against 360,463 for the same heap incremental; writing every old object's mark and age cost 52,000 bytes more.

### Regression check

Rows against Phase 3.28 (`c9ae921`), 7 and 11 rounds, ns (generational is now the default, so the allocation rows run young collections):

| Row | 3.28 | 3.29 |
|---|---|---|
| `int_field` | 156 | 152 |
| `string_field` | 185 | 184 |
| `upvalue_call` | 132 | 137 |
| `arith_loop` | 11 | 10 |
| `alloc_churn` | 37 | 39 |
| `scalar_call` | 90 | 91 |
| `nested_call` | 172 | 173 |
| `gc_tables_on` | 360, 351 | 342, 371 |
| `gc_tables_off` | 470 | 441 |
| `gc_retained_on` | 325 | 313 |
| `gc_closures_on` | 240, 239 | 295, 259 |
| `ud_create_uv4` | 242 | 226 |

Interpreter rows are unchanged. Allocation-heavy rows read 5–8% slower in the default mode (`gc_closures_on` 259 against 239 in the longer run), the cost of a young-list entry per allocation and of young collections in one slice, against incremental steps on a heap this small; `gc_retained_on`, with a live set, reads faster.

### Stabilization (the review's fixes)

Single-codegen-unit builds of the Phase 3.29 commit (`cc00e2e`) and the stabilized tree, alternated and pinned to one core; load average 7–20. Three runs a side, medians.

**Young collections and old populations** (`gc_gen_*`; "units/young" is collector work per young collection):

| Workload | side | collector ms | run ms | young | units/young | p50 µs | p95 µs | p99 µs | max µs |
|---|---|---|---|---|---|---|---|---|---|
| 9,000 old finalizable tables, young churn | 3.29 | 4.99 | 24.5 | 290 | 9,388 | 14.3 | 24.3 | 60.2 | 122.9 |
| | stabilized | 2.58 | 21.0 | 290 | 462 | 5.7 | 12.8 | 56.9 | 132.0 |
| 1,000 suspended old coroutines, young churn | 3.29 | 2.54 | 25.1 | 65 | 1,108 | 34.2 | 42.0 | 58.1 | 58.1 |
| | stabilized | 2.70 | 25.3 | 65 | 1,101 | 34.7 | 43.6 | 59.0 | 59.0 |
| the same, 10 of them resumed with young values | 3.29 | 2.66 | 27.6 | 66 | 1,212 | 36.3 | 40.7 | 46.6 | 61.7 |
| | stabilized | 2.73 | 26.5 | 67 | 1,212 | 36.1 | 43.6 | 52.9 | 56.6 |
| 3,000 old tables of 100 entries, young churn | 3.29 | 8.13 | 83.3 | 105 | 7,361 | 13.0 | 60.2 | 77.3 | 117.8 |
| | stabilized | 8.53 | 86.9 | 105 | 7,361 | 17.2 | 60.6 | 105.3 | 132.7 |
| the same, an old table written each round | 3.29 | 11.02 | 88.2 | 116 | 10,308 | 14.2 | 91.4 | 137.0 | 195.5 |
| | stabilized | 10.63 | 90.2 | 116 | 10,308 | 11.9 | 93.9 | 132.7 | 160.6 |
| a 300,000-entry table being built | 3.29 | 5.73 | 106.7 | 53 | 454 | 9.0 | 85.2 | 348.0 | 416.7 |
| | stabilized | 5.59 | 103.5 | 53 | 454 | 8.3 | 78.3 | 268.7 | 432.3 |
| a list of 9,000 tables growing (bad majors) | 3.29 | 1.38 | 7.9 | 10 | 8,796 | 10.5 | 52.3 | 149.8 | 149.8 |
| | stabilized | 1.34 | 7.4 | 10 | 8,796 | 9.8 | 51.4 | 146.0 | 146.0 |
| churn of two-entry tables | 3.29 | 1.90 | 23.1 | 87 | 785 | 21.5 | 25.4 | 28.3 | 28.3 |
| | stabilized | 2.14 | 22.8 | 87 | 785 | 23.9 | 32.0 | 37.5 | 37.5 |

- **Old finalizable objects** no longer cost young collections anything: 462 units each against 9,388 (one per registered object), the collector's time halves, and the median slice falls from 14.3 to 5.7 µs. Against incremental mode on the same program (66.9 ms) the collector takes 26 times less.
- **Old threads.** Phase 3.29 measured every thread's frames at every young collection without charging the work, so units did not show it; the walk was short here (one frame per coroutine). Its removal shows in no row beyond noise; young collections now touch only threads that ran.
- **Other rows** are within the run-to-run noise of a shared machine (±10% on the collector's time, more on single slices). The exact count adds one size computation per object freed and a counter update per charge.
- **Remembered sets, peak heap, full cycles, and young collections** are unchanged on every row.

**Regression check**, the same builds, 11 rounds, ns:

| Row | 3.29 | stabilized |
|---|---|---|
| `int_field` | 150 | 151 |
| `string_field` | 184 | 182 |
| `upvalue_call` | 140 | 131 |
| `arith_loop` | 11 | 11 |
| `alloc_churn` | 38 | 38 |
| `scalar_call` | 95 | 89 |
| `nested_call` | 179 | 169 |
| `gc_tables_on` | 373 | 381 |
| `gc_tables_off` | 469 | 480 |
| `gc_retained_on` | 352 | 318 |
| `gc_closures_on` | 278 | 247 |
| `ud_create_uv4` | 250 | 200 |
| `bw_table_insert` | 392 | 355 |
| `bw_table_insert_gen_old` | 332 | 323 |

No row is slower beyond noise. A first build inlined the barrier's slow path, now with its duplicate-free `again` bit, into every mutable heap access: calls ran 10–12% slower and `gc_closures_on` 21%. Moving the slow path out of line (`Arena::touch`, cold) restored them.

**Memory.** A thread carries 12 bytes more (`charged_slots`, `charged_held`); each arena one bit per slot (`queued`). A snapshot writes the same 12 bytes per thread and 8 for the finalizer positions, and three fewer collector words (four estimate fields dropped, `unreleased` added).

## The scalability envelope (Phase 3.30)

Release builds with one codegen unit, pinned to core 4, on a machine other jobs were loading (load 4 to 17 during these runs). Compare within a table only. ADR 0052 has the design.

### Collectors at scale

`scale_gc_<mode>_<shape>` (`MOONSEED_BENCH_ONLY=scale_ cargo bench -p moonseed --features measure`): each shape is built, collected, then churned by 100,000 short-lived tables with 1,000 writes into the old root, a `step` every 100 rounds and a full collection every 250. Slices are whole steps, atomic phases, or young collections (no quantum); a full collection is one slice.

| Shape | Mode | Young slices p50 / max | Major slices p50 / max | Major units per object per cycle |
|---|---|---:|---:|---:|
| 100,000 live | incremental | — | 4.9 µs / 4.2 ms | 6.2 |
| 500,000 live | incremental | — | 8.0 µs / 9.4 ms | 4.3 |
| 100,000 live | generational | 1.36 / 2.91 ms | 1.89 / 2.24 ms | 3.7 |
| 500,000 live | generational | 2.67 / 3.04 ms | 6.61 / 6.76 ms | 3.7 |
| one 100,000-entry table | generational | 0.39 / 0.65 ms | 0.44 / 1.03 ms | — |
| 10,000 suspended coroutines | generational | 0.26 / 0.35 ms | 0.86 / 1.20 ms | — |
| 100,000-entry weak-value table | generational | 0.77 / 0.86 ms | 2.37 / 2.80 ms | — |
| 50,000-entry ephemeron table | generational | 0.72 / 1.06 ms | 3.94 / 4.31 ms | — |
| 20,000 finalizable objects | generational | 0.20 / 0.21 ms | 0.57 / 1.49 ms | — |

- No work grows faster than the live heap from 100,000 to 500,000 objects: work per object per major cycle falls or holds.
- A young collection here traverses the whole old root table the churn writes into (about 112,000 units at 100,000 live, 512,000 at 500,000), as Lua's does for a touched table; the remembered set itself stays near 100 objects.
- Exact accounting holds at 300,000+ objects in both modes (`prove::scale`), checked against a recount at sampled steps and every collection's end.

### Snapshots at scale

`snapshot-scale` (`crates/moonseed-bench`, a counting allocator): each heap is built to a target logical size; restore is decode, validation and the runtime's construction. Extra bytes are the allocator's peak during the call above its level before.

| Heap (about 60 MiB logical) | Objects | Snapshot bytes per logical byte | Encode | Restore | Extra during encode | Extra during restore |
|---|---:|---:|---:|---:|---:|---:|
| strings | 29,962 | 0.99 | 0.30 s | 0.32 s | 124 MiB | 128 MiB |
| tables | 196,775 | 0.72 | 0.32 s | 0.72 s | 144 MiB | 350 MiB |
| closures | 939,019 | 0.63 | 0.52 s | 1.33 s | 270 MiB | 469 MiB |
| threads | 157,472 | 0.46 | 0.24 s | 0.41 s | 244 MiB | 309 MiB |
| userdata | 29,773 | 0.99 | 0.28 s | 0.29 s | 128 MiB | 130 MiB |
| mixed | 128,882 | 0.87 | 0.26 s | 0.35 s | 153 MiB | 183 MiB |
| mixed, mid young collection | 129,883 | 0.87 | 0.26 s | 0.34 s | 153 MiB | 183 MiB |
| mixed, mid incremental mark | 129,883 | 0.87 | 0.26 s | 0.32 s | 153 MiB | 182 MiB |

Every size from 1 MiB to 60 MiB scales linearly; the ratio of each shape does not change with size. Encoding builds an image and then one output vector, so it needs about twice the heap's logical size; streaming is later work.

### The hot loop after wide operands

Widening jump offsets to `i32` (bytecode 12) first put the offset into `Flow`, the value every hot instruction returns, and doubled it. A bisect between the parent commit, the compiler change alone, and the whole phase:

| Row (ns) | parent | compiler change alone | whole phase | whole phase, offset-free `Flow::Jump` |
|---|---:|---:|---:|---:|
| for_int | 30 | 52 | 48 | 30 |
| call_lua_scalar | 104 | 131 | 129 | 102 |
| call_native_zero | 64 | 89 | 89 | 63 |
| const_field_hit | 43 | 70 | 69 | 43–53 |

The dispatcher now reads a jump's offset from the instruction. Rows through cold handlers moved by 0 to 17% between runs and layouts (with `-C llvm-args=-align-all-functions=6` on both sides: `meta_index_lua` 218 → 245, `call_native_global` 129 → 156, `upvalue_call` 124 → 121), the placement band Phase 3.11 describes; they are left to Phase 3.32.

### Baseline against PUC Lua 5.4.9

The fixed corpus in `bench/` (`tools/bench_corpus.py`, W=1, R=9, core 4, governor `performance`, load 6.5 to 9.8). Times are milliseconds of wall time for the whole process; Moonseed's split is from `moonseed-run --timings`.

| Workload | PUC median | Moonseed median | Ratio | Moonseed compile | Moonseed run |
|---|---:|---:|---:|---:|---:|
| empty (startup) | 2.5 | 2.8 | 1.09x | 0.016 | 0.006 |
| fib | 252 | 2,329 | 9.24x | 0.043 | 2,325 |
| table_fields | 390 | 2,818 | 7.23x | 0.071 | 2,815 |
| alloc_churn | 297 | 2,575 | 8.68x | 0.054 | 2,572 |
| strings | 299 | 711 | 2.38x | 0.065 | 692 |
| sort | 364 | 4,861 | 13.34x | 0.072 | 4,852 |
| numeric_loops | 298 | 3,956 | 13.26x | 0.056 | 3,952 |
| generic_for | 245 | 1,934 | 7.88x | 0.056 | 1,931 |
| method_calls | 197 | 1,647 | 8.35x | 0.052 | 1,644 |
| closures | 254 | 2,525 | 9.94x | 0.064 | 2,517 |
| coroutines | 305 | 2,064 | 6.76x | 0.059 | 2,061 |
| metamethods | 305 | 3,025 | 9.93x | 0.059 | 3,021 |
| patterns | 395 | 1,408 | 3.57x | 0.065 | 1,404 |
| native_calls | 312 | 2,640 | 8.45x | 0.061 | 2,637 |
| application | 289 | 2,335 | 8.09x | 0.103 | 2,332 |

- Startup and compilation are not the gap: Moonseed boots in about 0.1 ms and compiles each workload in under 0.11 ms. Every ratio is execution.
- The string and pattern workloads, which spend their time in library code, are 2.4x and 3.6x; everything that is bytecode dispatch, calls and table access is 7x to 13x.
- The earlier five ad-hoc timings (3x to 20x) are superseded by this table; Phase 3.32 compares against it.

## The embedding API (Phase 3.31)

Release builds, measure rows `embed_*` and `id_lookup_*` (`MOONSEED_BENCH_ONLY=embed_,id_lookup_ cargo bench -p moonseed --features measure --bench kernel`), loaded machine. The full tables, with the Moss frame benchmark's method, are in `integration/moss/BENCHMARKS.md`.

### Object lookup by id

Before Phase 3.31 a lookup by `ObjectId` scanned every arena. Each arena now keeps an id → slot index, made on the first lookup and kept current from then on.

| Heap | Rooted reacquisition (`Runtime::object`), ns | A full scan of the strings, before, ns |
|---:|---:|---:|
| 1,000 objects | 44 | 1,697 |
| 10,000 | 52 | 12,838 |
| 100,000 | 248 | 214,128 |
| 500,000 | 376 | 1,239,694 |

The index's cost rises from 100,000 objects because the map no longer fits in cache; it stays four orders of magnitude under the scan at 500,000.

### Crossings

| Operation | ns |
|---|---:|
| Lua → host, immediate native, raw `NativeContext` | 236 |
| Lua → host, typed adapter | 231 |
| Host → Lua call, one argument and result (`start_call`, `run`, `finish_call`) | 998 |
| Lua method on host userdata | 610 |
| Native → Lua continuation, call and resume | 1,002 |
| Host userdata borrow | 135 |
| String: borrow 256 bytes / copy them | 4 / 19 |
| Table: raw get / raw set | 69 / 78 |
| Table: semantic get / set, through the call machinery | 689 / 401 |
| Rooted value: acquire by id and drop | 42 |
| Wait: request, inspect, complete, resume, finish | 1,187 |
| `require` through the host resolver, compiling the module | 6,563 |
| Restore, 11.5 KB snapshot with one portable / one rebindable userdata | 211,709 / 206,312 |

A typed native costs the same as a raw one; in a quieter run of the same rows the two were 129 and 112 ns.

### A Moss frame

`integration/moss`: real `moss_ecs` entities with a cooked transform and an asset id, driven by `update(dt)` from a module the host resolver loads. Each entity's update reads and writes its position through host methods; ten warm-up frames, fifty measured.

| Interactions per frame | Median µs | p95 µs | Fuel | Native crossings | Allocations |
|---:|---:|---:|---:|---:|---:|
| 100 | 117 | 126 | 1,728 | 152 | 486 |
| 1,000 | 1,128 | 1,144 | 16,578 | 1,502 | 4,536 |
| 10,000 | 12,292 | 13,706 | 165,078 | 15,002 | 45,037 |

- A frame costs about 1.2 µs per host interaction, linear from 100 to 10,000.
- About 4.5 host allocations per interaction, not yet attributed. Where they come from is one of Phase 3.32's first questions, with the PUC corpus.

### Non-regression against the PUC corpus

The Phase 3.30 corpus rerun with the embedding API in place (W=1, R=9, core 4, load about 6). Ratios are Moonseed's median over PUC's in the same run, so the machine's drift between the two runs cancels out.

| Workload | Ratio, Phase 3.30 | Ratio, Phase 3.31 | Change |
|---|---:|---:|---:|
| fib | 9.24x | 8.97x | -3% |
| table_fields | 7.23x | 5.23x | -28% |
| alloc_churn | 8.68x | 8.19x | -6% |
| strings | 2.38x | 2.40x | +1% |
| sort | 13.34x | 13.43x | +1% |
| numeric_loops | 13.26x | 12.74x | -4% |
| generic_for | 7.88x | 7.62x | -3% |
| method_calls | 8.35x | 8.78x | +5% |
| closures | 9.94x | 10.62x | +7% |
| coroutines | 6.76x | 7.10x | +5% |
| metamethods | 9.93x | 9.80x | -1% |
| patterns | 3.57x | 3.74x | +5% |
| native_calls | 8.45x | 8.65x | +2% |
| application | 8.09x | 7.15x | -12% |

None of these programs calls the host, and no change touched the dispatch loop since Phase 3.30's `Flow` fix, so these moves are code placement, within the band Phase 3.11 recorded. `table_fields` changed the most; Phase 3.32 should find out why before building on it.

## Phase 3.32: interpreter performance campaign

**Implemented, Outcome B.** The nonempty corpus's geometric mean of Moonseed / PUC
instructions falls from **5.300 at `48f94bd` to 2.701 at `d9154eb`**, a reported
**1.962x** improvement (the final PUC control differs; see below). The minimum
2x improvement gate requires at most 2.650; it is missed
by about 2%. The accepted changes and measured residual costs are recorded in
[ADR 0056](adr/0056-interpreter-core-and-execution-caches.md).

### Environment, method and evidence

The shared AMD Ryzen 9 7945HX machine (16 cores, 32 threads) was **not in
performance mode**, with the `powersave` governor and other work running.
The closing environment records load averages 19.74 / 27.22 / 30.31.
Instruction counts decide acceptance. All wall times, hardware cycles, IPC and
hardware misses below are **under load, diagnostic, not a quiet-machine baseline**.

Corpus revision 2 contains 22 files, including the startup probe `empty`;
aggregates exclude `empty` and include all 21 other workloads. PUC is Lua 5.4.9,
without compatibility macros, invoked with `-E`. The baseline and closing run
use `g332/puc-g/src/lua`, built with `gcc -std=gnu99 -O2 -Wall -Wextra
-DLUA_USE_LINUX -g`. The final instruction run uses the separate prebuilt
`lua/lua-5.4.9-nocompat/src/lua`; its environment records build flags as unknown.
Moonseed uses Rust 1.98.1 / LLVM 22.1.8 and portable `bench-stable`: optimization
3, one code generation unit, fat LTO and line tables, with no `target-cpu=native`.
Ordinary instruction measurements have no counters; VM counts and allocation
tracing use separate opt-in builds. See [the frozen method](../tools/BENCHMARKING.md)
and [counter definitions](COUNTERS.md).

Callgrind 3.25.1 measures whole-process Ir on divisor-100 sources, pinned to
logical CPU 4: loading, compilation, library boot, execution and teardown are
included. The exact scaled sources and output checksums match across the baseline
and final run. Scaling changes nested work and working sets;
these counts cannot be multiplied by 100 to predict full-size costs. A single
count per engine is load independent but not a guarantee of identical repeated
counts: PUC table layout and process setup vary. **The PUC binary hash also changes
in final2**, from `b9d6c74e19ba…` (baseline/closing) to `3881230fd6ef…`.
Thus the reported 1.962x ratio improvement is not a same-control-binary
longitudinal comparison. The same-source Moonseed-only geometric-mean absolute
Ir improvement, computed from those raw counts without PUC denominators, is
**1.960x**, also below 2x. Final PUC counts differ from baseline by -0.60% to
+2.77% per nonempty workload; their cause is not isolated. A fresh baseline rebuild differs
by at most 0.349% on nonempty workloads; repeated PUC `table_fields` counts span
0.788% in the closing measurements.

The raw evidence (profiles, lane reports and phase notes) is kept outside the
repository in a separate results directory; the `results/...` names below refer to it.
The tables here reproduce the numbers, and the commit messages in
`48f94bd..d9154eb` state each change's baseline and measured effect.
The final instruction corpus is at `d9154eb`; the paired wall corpus, hardware
counters, embedding, Moss, ledger and build comparisons are at **`08d4448`**.
Those earlier measurements were not rerun after the review fixes and final push.
Each table retains its revision rather than treating all measurements as one build.

### Whole corpus before and after

All ratios below are Moonseed / PUC; lower is better. The improvement factor
divides the baseline Ir ratio by the final Ir ratio, using unrounded counts.
Wall columns are full-size, independently collected rounds: one warmup and nine
adjacent alternating Moonseed/PUC pairs on CPU 4, with the median of per-pair
ratios. The last column retains final wall p10–p90 (nearest rank).

| Workload | Baseline Ir `48f94bd` | Final Ir `d9154eb` | Ir improvement | Baseline paired wall | Paired wall `08d4448` | Final wall p10–p90 |
|---|---:|---:|---:|---:|---:|---:|
| empty | 1.679 | 1.637 | 1.026x | 1.132 | 1.153 | 0.889–1.392 |
| fib | 8.670 | 3.824 | 2.267x | 9.034 | 3.846 | 3.275–5.237 |
| table_fields | 5.286 | 1.965 | 2.690x | 5.576 | 1.869 | 1.639–2.115 |
| alloc_churn | 5.063 | 3.428 | 1.477x | 7.253 | 3.985 | 3.430–4.962 |
| strings | 2.934 | 2.515 | 1.167x | 2.710 | 2.314 | 2.034–2.474 |
| sort | 8.128 | 4.754 | 1.710x | 11.636 | 6.521 | 4.201–9.611 |
| numeric_loops | 6.207 | 1.783 | 3.481x | 8.731 | 1.607 | 1.308–1.938 |
| generic_for | 5.788 | 3.480 | 1.663x | 6.929 | 4.025 | 3.237–4.303 |
| method_calls | 5.743 | 2.905 | 1.977x | 8.426 | 3.259 | 2.701–3.568 |
| closures | 7.312 | 2.940 | 2.487x | 8.697 | 2.966 | 2.509–3.244 |
| coroutines | 5.925 | 3.211 | 1.846x | 7.392 | 3.492 | 2.757–3.983 |
| metamethods | 7.391 | 4.143 | 1.784x | 9.183 | 4.636 | 3.903–6.664 |
| patterns | 3.201 | 3.023 | 1.059x | 3.658 | 3.275 | 3.050–3.975 |
| native_calls | 6.272 | 2.580 | 2.431x | 8.059 | 2.390 | 1.991–2.960 |
| application | 5.763 | 2.567 | 2.245x | 7.330 | 2.626 | 2.318–2.945 |
| branches | 5.359 | 2.271 | 2.360x | 6.211 | 2.234 | 1.770–2.815 |
| array_access | 5.693 | 2.426 | 2.347x | 6.611 | 2.573 | 2.255–3.070 |
| globals | 8.653 | 2.618 | 3.305x | 9.525 | 2.292 | 1.820–3.185 |
| tail_recursion | 6.034 | 2.542 | 2.374x | 6.869 | 2.781 | 2.191–3.323 |
| field_writes | 5.626 | 2.157 | 2.609x | 6.353 | 1.988 | 1.861–2.277 |
| string_concat | 1.869 | 1.835 | 1.018x | 1.759 | 1.609 | 1.429–1.970 |
| string_format | 2.290 | 1.836 | 1.247x | 2.456 | 2.024 | 1.905–2.711 |

| Statistic, excluding empty | Baseline `48f94bd` | Closing `08d4448` | Final `d9154eb` |
|---|---:|---:|---:|
| Ir-ratio geomean | 5.300 | 2.743 | 2.701 |
| Ir-ratio median | 5.763 | 2.618 | 2.580 |
| Best Ir ratio | 1.869 (string_concat) | 1.783 (numeric_loops) | 1.783 (numeric_loops) |
| Worst Ir ratio | 8.670 (fib) | 5.132 (sort) | 4.754 (sort) |
| Paired-wall-ratio geomean | 6.270 | 2.781 | not recollected |

The earlier closing instruction improvement was 1.932x with the same PUC binary;
the final push's reported ratio improvement is 1.962x with the changed control.
Paired-wall geomeans improve descriptively by **2.25x** (6.270 / 2.781).
Each round is paired with PUC separately, so that quotient is not a simultaneous
baseline/final speedup estimate and does not replace the instruction gate.
The intermediate instruction geomeans were 4.003, 3.330, 2.904 and 2.819.

Categories use the overlapping tags in `tools/bench_manifest.py`; they cannot
be added. Final category geomeans below are computed from the unrounded baseline
and `final2/instructions/results.json` counts. The `08d4448` column preserves
the earlier closing table.

| Category | Workloads | Baseline Ir geomean | `08d4448` | `d9154eb` | Final improvement |
|---|---:|---:|---:|---:|---:|
| core VM | 11 | 6.532 | 2.872 | 2.860 | 2.284x |
| tables | 10 | 6.210 | 2.973 | 2.933 | 2.117x |
| allocation/GC | 3 | 3.028 | 2.582 | 2.510 | 1.206x |
| strings | 5 | 2.971 | 2.391 | 2.310 | 1.286x |
| stdlib | 4 | 4.397 | 3.007 | 2.872 | 1.531x |

Embedding has no PUC-compatible corpus tag; its separate API measurements have
no PUC instruction ratio.

### Rust Lua interpreters

`competitors/COMPETITORS.md` records a separate run with Moonseed at `128d529`.
omniLua 0.7.1 is pinned to `90dcf85`; Piccolo 0.3.3 to `ce709eb`; Luna 4.0.1
to `f90669d`, using `--no-jit`. omniLua uses its fat-LTO, one-unit release
profile; Luna uses its LTO, 16-unit release profile; Piccolo uses default release
without LTO and a small runner around its public API. LuaJIT was not installed
and was excluded.

| Engine | Valid Ir workloads | Ir-ratio geomean | Valid wall workloads | Wall-ratio geomean |
|---|---:|---:|---:|---:|
| Moonseed `128d529` | 21 | 2.73 | 21 | 2.99 |
| omniLua 0.7.1 | 21 | 2.03 | 21 | 2.12 |
| Luna 4.0.1 interpreter | 21 | 3.66 | 20 | 3.97 |
| Piccolo 0.3.3 | 16 | 4.74 | 16 | 5.83 |

These are the source report's available-workload summaries, **not a common
16-workload comparison**. Piccolo cannot run application, numeric_loops,
patterns, string_format or strings because library functions are absent.
Luna's full-size metamethods run exceeds 300 seconds; its scaled Ir ratio is
23.30 and remains in the instruction aggregate. Supported outputs match PUC.
Competitor wall samples are diagnostic: seven alternating samples, load 23–53,
with some collection processes concurrently using SMT siblings 4 and 5. They
do not establish a quiet-machine ranking or conformance equivalence.

### Moss and embedding crossings

The closing Moss measurements at `08d4448` use the real CPU-only ECS/ABI
`Game::update_only` integration, with checkpoint/rebind/replay checks. Ten
warmup frames precede 50 measured frames; trace construction is excluded, host
setup and the counting allocator included. Ir/frame is the slope between 20
and 40 measured frames with identical initialization and ten warmups, which
approximately removes fixed startup and teardown. No renderer/GPU claim follows.

| Transform interactions/frame | Baseline median / p95 ms | `08d4448` median / p95 ms | Baseline Ir/frame | `08d4448` Ir/frame | Ir improvement | Alloc callbacks/frame before → after |
|---|---:|---:|---:|---:|---:|---:|
| 100 | 0.238 / 0.253 | 0.193 / 0.206 | 1,264,826 | 803,296 | 1.575x | 486 → 14 |
| 1,000 | 2.161 / 2.328 | 1.334 / 1.585 | 12,084,234 | 7,619,251 | 1.586x | 4,536 → 14 |
| 10,000 | 20.408 / 26.864 | 9.394 / 34.246 | 120,242,295 | 75,787,242 | 1.587x | 45,037 → 14 |

A 60 Hz frame has 16.67 ms available. At 1,000 interactions the closing median
uses 8.00% of that budget, p95 9.51%, below the 10% / 1.667 ms target in this
run. At 10,000, median uses 56.35% and p95 205.43%; this is not evidence that
10,000 interactions fit the budget. The through-origin median estimate of
1,249 interactions per 10% budget at the 1,000 size is an estimate, not a
measured capacity. The SRT final-push lane separately reports 7,577,792
Ir/frame at 1,000, 0.55% below its own baseline; final wall/frame was not rerun.

Allocation definitions matter. The normal counter above counts alloc callbacks,
including its inherited alloc/copy/free realloc implementation, using a median
over 50 frames. A separate calibrated trace at `08d4448` averages three frames:
12 fresh allocations/frame at every size, plus 4.667 / 5.667 / 4.000 reallocations
at 100 / 1,000 / 10,000 interactions. The tracer was corrected to recognize Rust
1.98.1's `__rust_realloc` bridge and passed a known-allocation calibration.
These trace counts and the normal counter are different sample windows and
definitions, not interchangeable totals.

The original nine allocations/entity were eight continuation-path allocations
and one `assert` scratch allocation, or 4.5 per transform interaction.
`results/alloc-after/k2/REPORT.md` records the same warmed attribution protocol
before and after reusable buffers and the borrowing APIs:

| Warmed operation | Baseline fresh allocations/op | After K2 | Baseline reallocations/op | After K2 |
|---|---:|---:|---:|---:|
| Native-to-Lua continuation | 6 | 0 | 0 | 0 |
| Scalar host-to-Lua call, including start/run/finish | 10 | 0 | 3.8 | 0 |
| Wait inspection/completion cycle | 15.1 | 5.1 | 2.8 | 0 |
| Typed/raw/method native and six Lua call forms | 0 | 0 | 0 | 0 |
| Moss interaction, 100/frame | 4.773 | 0.142 | 0.106 | 0.032 |
| Moss interaction, 1,000/frame | 4.5273 | 0.0142 | 0.0109 | 0.0035 |

Small constructor allocation requests fall **8 → 7 → 5 → 3** across the initial
reserve, five-slot mode and compact key-owner changes. The final SRT constructor
witness gives 5.0015 → 3.0015 requests/table, 18 fewer requested bytes/table;
the fractional remainder is amortized setup/arena growth. Coroutine round trips
fall from three host allocations to zero. Warmup and finite nesting/capacity
limits apply; owned continuation copies, new Lua objects and growth may allocate.

Closing embedding timings use warmed API operations, median and p95 of 20
samples, with separate baseline/final runs and no PUC control. **All are under
load and diagnostic.** The full crossing/API table from `results/final/FINAL.md`
is retained here; units are ns/op.

| API workload | Baseline median | Baseline p95 | `08d4448` median | `08d4448` p95 |
|---|---:|---:|---:|---:|
| continuation | 1,209 | 1,226 | 702 | 724 |
| host_lua_call | 1,161 | 1,184 | 536 | 555 |
| host_method | 839 | 849 | 362 | 373 |
| module_resolve | 21,982 | 25,688 | 8,606 | 10,460 |
| native_raw | 389 | 397 | 202 | 209 |
| native_typed | 363 | 383 | 177 | 181 |
| restore_portable | 764,991 | 813,373 | 266,156 | 338,558 |
| restore_rebind | 761,072 | 805,679 | 401,613 | 499,932 |
| root_acquire_drop | 107 | 112 | 84 | 88 |
| string_borrow | 6 | 6 | 4 | 4 |
| string_copy | 28 | 28 | 26 | 26 |
| table_get | 760 | 809 | 506 | 540 |
| table_raw_get | 100 | 107 | 76 | 81 |
| table_raw_set | 88 | 94 | 63 | 68 |
| table_set | 662 | 698 | 471 | 506 |
| userdata_borrow | 189 | 195 | 10 | 10 |
| wait_complete | 2,996 | 3,996 | 1,317 | 1,559 |

Use `NativeContext::resumed_ref` and `call_lua` for the warmed allocation-free
continuation pattern; [the embedding guide](EMBEDDING.md#native-to-lua-continuations)
describes the lifetime and ownership rules.

### Hardware counters and remaining attribution

Closing perf-stat uses full-sized sources, CPU 4, one warmup and nine adjacent
engine pairs with checksum validation. IPC is hardware instructions/cycles;
miss rates are per 1,000 hardware instructions. Medians at `08d4448` follow.

| Workload | Moonseed IPC | PUC IPC | Moonseed / PUC branch misses | Moonseed / PUC L1I misses | Moonseed / PUC L1D misses |
|---|---:|---:|---:|---:|---:|
| fib | 3.686 | 4.098 | 0.045 / 0.497 | 0.001 / 0.000 | 0.102 / 0.054 |
| table_fields | 3.274 | 2.646 | 0.063 / 0.044 | 0.026 / 0.003 | 0.895 / 0.232 |
| alloc_churn | 3.070 | 4.038 | 0.095 / 0.136 | 0.034 / 0.015 | 5.812 / 2.946 |
| numeric_loops | 4.101 | 4.986 | 0.002 / 0.002 | 0.000 / 0.000 | 0.015 / 0.013 |
| strings | 2.621 | 1.784 | 0.070 / 0.711 | 0.881 / 0.031 | 5.361 / 5.451 |
| native_calls | 3.990 | 3.966 | 0.048 / 0.082 | 0.005 / 0.002 | 0.632 / 0.100 |

IPC is now close to PUC on numeric and call workloads: numeric_loops rises from
2.386 to 4.101; native_calls from 2.919 to 3.990. Allocation and string paths
still have larger cache costs. These noisy hardware samples do not qualify wall
performance. `results/final/perf-paired/results.json` retains cycles, all requested
events, multiplexing, loads and pair dispersion; Cachegrind tables are simulated
misses with different denominators, not hardware rates. SRT's sort inlining
raises simulated L1I misses 13,375 → 24,018 while reducing Ir 7.4%.

The final profile sums exclusive instructions across scaled workloads, weighted
by their actual instruction totals. `run_hot` holds **41.22%**, including inlined
register, numeric and frame work; this is not a pure dispatch cost. The handoff
estimates per-instruction dispatch within it at about **38%** of total Ir.
Other final symbol shares are `hot_get_name` 5.14%, `hot_set_name` 3.46%, pattern
matcher 3.19%, `fast_call` and `fast_return` 1.94% each, and `run_lib` 1.91%.
The heuristic subsystem groups attribute allocation 4.68%, GC 0.26%, builtins
8.06%, and leave 35.73% unattributed. Inlining hides fuel/GC checks inside callers;
zero separately attributed fuel symbols does not mean zero fuel cost.

### Semantic cost ledger

These are separate, deliberately contract-breaking attribution builds, never
production candidates. Positive numbers are geometric-mean saved Ir over 21
nonempty workloads. Baseline variants are in `results/diag/LEDGER.md` (measurement
HEAD `7358ac8`, runtime baseline `48f94bd`); closing variants in
`results/final/FINAL.md` at `08d4448`.

| Intervention | Baseline saved Ir | Closing saved Ir | What the intervention removes |
|---|---:|---:|---|
| no_budget | 3.779% | 1.351% | Hot-loop budget condition only; accounting remains |
| no_generation | 4.743% | 3.024% | Arena get/get_mut generation guards, retaining other checks |
| no_object_id | 0.051% | 0.044% | Lazy by-id index maintenance, not all identity/snapshot costs |
| no_reentry_checks | 1.095% | 0.488% | Re-entry schedule/trap/GC/finalizer guard; may change schedules |
| presized | 3.617% | unavailable | Old register-write grow fallback; absent from the new hot loop |

These deltas are not additive prices for semantics. Code layout and changed
schedules affect unrelated paths; matching checksums do not prove equivalence.
Whole-process deltas below 0.1% are negligible for this ledger. Fuel, generation
validation and safe points remain required; removing them is not the next step.

### Build configurations

Gate N at `08d4448` compares the required configurations, each independently
paired with PUC. Build seconds include dependency compilation and cache/load
differences and are observed durations, not comparable clean-build costs.

| Configuration | Build seconds | Ir-ratio geomean | Absolute Moonseed Ir saved vs stable | Paired wall geomean |
|---|---:|---:|---:|---:|
| bench-stable, 1 CGU / fat LTO | 110.316 | 2.743 | 0.000% | 2.781 |
| default release, 16 CGUs / no LTO | 45.489 | 2.745 | -0.586% | 2.933 |
| 1 CGU / thin LTO | 40.470 | 2.731 | 0.097% | 3.028 |
| bench-native, target-cpu=native | 81.294 | unavailable | unavailable | 2.989 |

Every build exits zero and each wall corpus has 21 valid nonempty rows. Native
Callgrind cannot execute any row: Valgrind/LibVEX rejects AVX-512 instructions
with SIGILL before Lua starts. Partial counts are excluded. PGO was optional and
skipped. Retain portable bench-stable as the reference; these small instruction
differences and noisy timings do not select a new production profile.

### Rejected designs and accepted costs

| Attempt | Measured effect | Decision |
|---|---|---|
| Shared `Rc<[u8]>` strings/keys (K3) | 7 → 3 allocations/table; alloc_churn -6.03%; ten workloads exceed +1%, string_concat +11.64%, table_fields +4.04%; key 24 → 32 bytes | Rejected; wider probes and generated-string copying offset allocation savings |
| Shared `Rc<Vec<u8>>` variant | Strings, patterns, string_concat and string_format exceed +1% | Rejected; compact final key owner retains 24-byte TableKey without changing all strings |
| Eager string hashing / shared storage for every string | string_concat +3.31% / +11.30% | Rejected; hash only on demand and share stored keys |
| Float immediate ArithK | numeric_loops +12.34%; native_calls +3.43% | Rejected; exact integer immediates accepted, float loads remain |
| Split-epoch stage 2, 232-byte frame | Corpus sum +0.63%, closures +7.65%, Moss about +3.1% | Rejected; original loop with borrowed numeric context saves 4.61% |
| Initial stage-4 calls | fib -40.23%, but native_calls +6.49%, Moss +2.44% / +2.59% | Rejected; early native decline and revised frame setup pass the workload bounds |
| Upvalue-field opcodes (`g-rejected.patch`) | globals -11.7%, four workloads +1–1.8%, branches +1.8% | Rejected for dispatch pressure; candidate for a measured retry |
| Caller-window caches / return-continuation fusion (CAL) | Caller cache focused workloads +2.0–3.1%; fusion saves 9 Ir/index but adds 21 Ir/call pair | Rejected; small setup cleanup alone accepted |

Original core stack-frame guards were missed and explicitly waived by the
coordinator, not passed: stage 1 reserved 440 bytes against 256; stage 5 472
against 320; CAL's selected core reserves 520 bytes, with a 6,352-byte symbol.
The 400-Ir call-pair aspiration also remains unmet. Immediate builtin eligibility
checks initially cost generic_for +2.01% and coroutines +1.74%, accepted for the
native_calls reduction of 34.36%. Short-key hint hits improve, while 17/64-byte
hit microloops cost 8–12.5% more; those keys are absent from the corpus. The
combined MetaCall pool was accepted after clarification of the +1% Moss bound;
its +0.009% / +0.007% Moss deltas are recorded, not material regressions.

### Remaining hotspots and proof limits

The next work should start with the frame/window and dispatch model: the final
call-pair slope is **654 Ir**, versus PUC about **133–155**, depending on supporting
Move inclusion. The before-cleanup CAL split attributes about 293 Ir to window
switching, 166 to fast_call, 77 to fast_return and 57 to helper dispatch;
supporting vector work makes up the remainder. These are microloop attribution
groups, not additive forecasts for the whole corpus.

Ranked by final relative corpus gap, the residual workloads are **sort 4.754x,
metamethods 4.143x, fib 3.824x, generic_for 3.480x, alloc_churn 3.428x,
coroutines 3.211x**, then patterns 3.023x and closures 2.940x. Sort still pays
library batches, table access and Lua comparator windows; metamethods, recursion
and coroutines still pay frame/continuation transfers. Generic iterators retain
call and register-window work; allocation churn retains slot/key construction
and reclamation. This ranks overhead relative to PUC, not production frequency
or an isolated handler's cost. Re-profile before retrying upvalue-field ops or
float immediates; a smaller opcode count alone did not establish acceptance.

The two reviews found and fixed a restored-top return panic (`4c675c3`) and
super-linear integer-index refill (`128d529`); Off-mode immediate builtin
coverage was fixed too. A pre-existing hot/slow scalar-store write-barrier
asymmetry remains collector-safe but limits byte-identical Gate Q coverage after
collection. The final PUC oracle passes **13/13 at `d9154eb`**. The official suite
at `4c675c3` is unchanged from `tests/compat/lua54-current.json`: 33/33 compile,
4 PASS, 23 LUA_ERROR, 2 COMPLETED_WITHOUT and 4 COMPLETED_WITHOUT_T, with the same
blockers. Lane correctness, native/wasm fingerprints and Moss replay evidence
are retained in their reports; they do not imply full Lua conformance, platform
CI or release acceptance. The following lane sections preserve their own
baselines and measured deltas; their savings overlap and must not be summed.

## Phase 3.33: activation record and call ABI

The final comparison uses `cd04210` and `442922d`, portable `bench-stable`
Moonseed binaries, pinned PUC Lua 5.4.9, and revision-2 corpus sources scaled
by divisor 100. Callgrind `Ir` counts whole-process instructions; the call
family subtracts matched no-call slopes over 200 and 1,000 iterations, so its
numbers still include argument setup and the callee body. Cachegrind supports
the locality check. Source/output hashes, binary identities and raw profiles
are kept with the phase notes outside the repository. The machine was loaded
and outside performance mode (first load average 10.57–37.06 in final wall
runs). CPU 4 paired wall medians, cycles and embedding nanoseconds are
**diagnostic**, not quiet-machine acceptance. All 22 scaled corpus outputs
match PUC; `empty` is excluded from geomeans.

The native frame shrinks **72 → 40 bytes**. Cold continuation state exists only
while used and is recycled. Logical stack length and frame depth can be less
than their retained physical high-water storage; only live slices are read,
traced or encoded. The shared fixed-frame builder and in-loop frame switches
preserve semantic snapshots, fuel and result windows. See [ADR 0057](adr/0057-compact-activation-records.md).

### Fixed-call cost and attribution

Gate 0's fixed 0/0 pair measured **687 Ir**, including a supporting `Move`;
its intrinsic Call/Return subtotal was **654**. PUC measured **155 / 133**
on the same definitions. The 1/1 pair measured Moonseed **708 / 642** and PUC
**187 / 143**. Gate 0's disjoint 0/0 accounting included 37 frame push/init,
27 pop, 18 frame validation, 24+22 checked closure/prototype window lookup,
80 code/base/register derivation, 58 window handoff, 28 stack growth, 57
call/return decode and helper dispatch, 42 call/return dispatch and 33 for its
supporting `Move`. The remaining charged checks, writes and cleanup sum to the
687 total; they are not extra costs on top of it.

The first redesign pass reached **575 Ir** and missed its ≤490 Gate N. It left
an out-of-line frame push and common 0/0 stack regrowth. Retaining physical
stack/frame storage and switching within the loop brought stage 8 to **445 Ir**
and missed its ≤400 Gate N2. Stage-8 exclusive line attribution of that 445:

| Cost bucket | Approximate Ir / fixed 0/0 pair |
|---|---:|
| `Move`, `Call`, `Return` loop and decode | 74 |
| Frame-arm dispatch and helper marshalling | 37 |
| Two in-loop window switches | 62 |
| Call prologue, checked callee resolution, builder checks/writes | 117 |
| Return prologue, eligibility, checked caller resolution, result window | 107 |
| Difference between rounded named buckets and total | ~48 |

The last row is not an independently measured function: the source-line
buckets in Gate N2 are approximate and do not exhaust the measured total.
The checked callee/caller resolution alone is about 32+30
Ir, and builder writes about 45. The call path remains above the owner's
400-Ir stop threshold. No 250-Ir target or 2.2x corpus target is claimed.

| Warmed call family, net Ir/call | PUC | Before | After | After / PUC |
|---|---:|---:|---:|---:|
| Fixed 0/0 | 155 | 687 | 445 | 2.87x |
| Fixed 0/1 | 191 | 743 | 502 | 2.63x |
| Fixed 1/1 | 187 | 708 | 486 | 2.60x |
| Fixed 2/1 | 209 | 795 | 549 | 2.63x |
| Fixed 2/2 | 279 | 709 | 458 | 1.64x |
| Fixed vararg | 407 | 1,720 | 1,598 | 3.93x |
| One upvalue | 195 | 784 | 540 | 2.77x |
| Tail call | 183 | 639 | 449.5 | 2.46x |
| Method | 243 | 832 | 610 | 2.51x |
| `__call` | 275 | 1,212 | 1,013 | 3.68x |
| `__add` | 339 | 1,488 | 1,369 | 4.04x |
| Recursive fib | 245 | 945 | 685 | 2.80x |

The final 0/0 saving is **35.2%**; the family numbers are final-binary slopes,
while Gate N2's line map comes from its earlier stage-8 binary.

### Corpus and host boundary

| Revision-2 workload | Before / PUC Ir | After / PUC Ir |
|---|---:|---:|
| fib | 3.823 | 2.783 |
| table_fields | 1.984 | 1.979 |
| alloc_churn | 3.418 | 3.482 |
| strings | 2.508 | 2.485 |
| sort | 4.753 | 4.222 |
| numeric_loops | 1.783 | 1.779 |
| generic_for | 3.475 | 3.430 |
| method_calls | 2.905 | 2.706 |
| closures | 2.939 | 2.481 |
| coroutines | 3.228 | 3.099 |
| metamethods | 4.143 | 3.931 |
| patterns | 3.023 | 2.995 |
| native_calls | 2.597 | 2.650 |
| application | 2.580 | 2.428 |
| branches | 2.270 | 2.243 |
| array_access | 2.425 | 2.431 |
| globals | 2.618 | 2.603 |
| tail_recursion | 2.541 | 2.113 |
| field_writes | 2.156 | 2.132 |
| string_concat | 1.834 | 1.880 |
| string_format | 1.826 | 1.815 |
| **21-workload geomean** | **2.703** | **2.575** |

The geomean improves **4.7%**. Overlapping category geomeans before → after:
core VM 2.862 → 2.630 (11), tables 2.936 → 2.845 (10), allocation/GC
2.505 → 2.534 (3), strings 2.308 → 2.281 (5), standard library 2.873 →
2.793 (4). Paired-wall geomean moves **2.929 → 2.704x PUC** under load;
that is descriptive only. On common scaled workloads the after Ir geomean is
**1.263x omniLua** (21), **0.700x Luna interpreter** (21), and **0.563x
Piccolo** (16). Piccolo's five unsupported workloads prevent a direct
21-workload aggregate comparison; Luna's full-source metamethod wall run
overflowed its stack and is excluded from wall summaries.

| Moonseed Ir / comparator | Common workloads | Before | After |
|---|---:|---:|---:|
| PUC Lua 5.4.9 | 21 | 2.703x | 2.575x |
| omniLua | 21 | 1.326x | 1.263x |
| Luna interpreter | 21 | 0.735x | 0.700x |
| Piccolo | 16 | 0.596x | 0.563x |

The real CPU-only Moss replay harness at 100 / 1,000 / 10,000 interactions
changes by +0.10 / +0.09 / +0.09% Ir/frame. At 1,000, it moves
**7,577,009 → 7,583,731 Ir/frame**, with 11 host allocations/frame in both
revisions, 501 Lua and 1,506 native calls/frame. Loaded median wall is
0.714 → 0.783 ms/frame: below 1 ms in this run, without quiet-machine or GPU
acceptance. The host boundary dominates its profile (`run_callback`,
`Value::raw`, `write_abs`), explaining why cheaper Lua calls barely move it.
The 17 feature-gated embedding rows are timed diagnostics only. Selected
crossings and restores (warmed median ns/op):

| Embedding operation | Before | After |
|---|---:|---:|
| Raw native call | 124 | 119 |
| Host-to-Lua call | 325 | 328 |
| Continuation | 437 | 453 |
| Module resolve | 5,841 | 9,538 |
| Portable restore | 210,079 | 199,769 |
| Restore with host rebind | 203,710 | 191,923 |

The module-resolve increase is not instruction evidence.

### Residual lanes and limits

These are local before/after experiments at different revisions, so their
percentages must not be added to reproduce the final campaign delta.

| Lane | Measured effect and disposition |
|---|---|
| Generic-for result write | Retained; removes intermediate writes; 22/22 outputs match and the worst nonempty corpus increase is below +0.001%. |
| Metamethod completion / direct `__call` | Retained; `__add` and `__index` 1,469 → 1,447 and 1,592 → 1,570 Ir/event, `__call` 1,103 → 1,095; corpus metamethods −0.946%. |
| Sort machine | Retained checked integer comparison and bounded argument reuse; default split −3.993%, comparator split −0.117%, corpus sort −1.504%, corpus geomean −0.0723%. |
| Slow-path recovery | Retained partial recovery; coroutine −3.944% and strings −0.914% against the lane base, but native_calls +2.078% and alloc_churn +1.532% versus the 3.32 final remained outside its recovery goal. An uncompiled helper relocation was reverted. |
| Metatable event hint | Retained validated slot numbers; `__add` 1,447 → 1,369, `__call` 1,095 → 1,013, `__index` 1,570 → 1,585; corpus geomean −0.076%, worst row +0.655%. |
| Inline callback operands / decode | Retained; matched incremental comparator 2,556 → 2,311 Ir (−9.59%), large comparator sort −6.996%, corpus geomean −0.284%. |
| Recycled callback boundary/task boxes | Retained for zero allocation machinery; matched comparator 2,311 → 2,134 Ir and warmed `gsub` callback machinery 2 → 0 allocations/callback, but large comparator sort +0.715% and corpus geomean +0.0026%. |
| Final callback decode / one-result return | Two separate candidates initially missed a half-bucket pursuit rule; coordinator retained their combined measured gain after final-binary gates: large comparator sort −3.353%, matched comparator 2,134 → 2,036 Ir, corpus geomean −0.147%. |

A matched sort comparator round trip was **2,556 → 2,311 Ir** in the first
callback lane against PUC's synchronous **321** incremental Ir. Later retained
changes brought the matched increment to about **2,036 Ir** at their own
revision. The initial disjoint 2,556-Ir ledger assigned 262 to sort transition,
53 to saving library state, 563 to boundary/cold-box work, 376 to Lua frame
push, 329 to comparator/interpreter, 261 to boundary return, 562 to decode
and resume, 7 to truth conversion, 5 to fuel/quantum increments, 202 to final
delivery, and −64 signed residual. The boundary, resumable sort state, one
fuel unit, frame/result transfer and truth conversion are deliberate stackless
semantics. Allocation, repeated decode and generic marshalling were measured
overhead. The signed residual and mixed `run_hot`/`run_lib` rows prevent a
claim that all 2,036 instructions are removable.

Remaining common gaps rank as follows: (1) `sort` **4.222x PUC**, dominated
by comparator callback and resumable library work; (2) `metamethods` **3.931x**,
with event lookup, frame setup and completion; (3) `alloc_churn` **3.482x**,
table/arena allocation and reclamation; (4) `generic_for` **3.430x**,
iterator call/result plumbing and table traversal; (5) `coroutines` **3.099x**,
resume/return/parent delivery. The fixed call's 445-Ir line attribution ranks
the structural call path above another broad residual pass. Recursive fib fell
to 2.783x, but still shows call overhead. Gate O's `run_hot` share (41.31% of
summed corpus Ir at its earlier revision) includes opcode work and is not an
available dispatch saving. Frame-locality depth 500 improved from 1.4452 to
0.3836 simulated D1 misses/call across the phase-to-Gate-O change; that is not
an isolated frame-size ablation. Final correctness evidence includes the
3,587-image snapshot witness, quantum/restore/GC matrix, 13/13 PUC oracle,
native/wasm roundtrip and CPU-only Moss replay. The official suite remains
33/33 compile, 4 PASS, 23 LUA_ERROR, 2 COMPLETED_WITHOUT and 4
COMPLETED_WITHOUT_T with the same blockers. The final adversarial review found
no confirmed blocker, major or minor finding.

## Phase 3.32: destination-aware arithmetic compilation

Compared with baseline commit `9fb19ceae5e090c46dd4b67680a379e69bd579b2`, the
compiler emits final arithmetic into its requested free result slot, then folds
scalar local stores after capture analysis. Numeric loops lose both result
copies; call argument and return windows also lose arithmetic-result copies.
Captured-local stores, simultaneous assignment stores, and non-arithmetic local
stores remain conservative. The interpreter and all format/fuel revisions stay
unchanged. See ADR 0040 for the safety and debug metadata contract.

All 22 corpus-revision-2 workloads have identical output before/after. Full-size
counter runs fall from 2,603,269,251 to 2,182,552,709 executed VM instructions
(16.16% fewer in the sum); no workload increases. `numeric_loops` falls from
423,348,363 to 282,013,029 (33.39%, exactly 141,335,334 removed copies).

Fresh before/after whole-program callgrind runs use `bench-stable`, `measure`,
no counters, the same sources scaled with divisor 100, and core 4. Ir falls in
all workloads: numeric loops 447,635,434 → 359,427,721 (19.71%); branches 10.42%;
array access 10.19%. The sum is 5,654,039,149 → 5,390,589,400 (4.66%). These
include compilation, boot, libraries, execution and process startup; the summed
percentage is corpus weighting, not a predicted application speedup. Both
scaled runs match PUC output. Full counters also match the recorded PUC output.

Collected on 2026-10-02 while the shared machine was not in performance mode.
Wall times are diagnostic only; no wall-clock speedup is claimed. The complete
per-workload table, binary/source hashes, load records, raw counter JSON and
callgrind profiles are in the campaign's `results/dest/RESULTS.md` and its sibling
`counters-before`, `counters-after`, `ir-before`, and `ir-after` directories.
The compiler-limit test changes from 2,499 to 3,332 repeated assignments under
its 10,000-instruction emission cap; no exact fuel or wasm fixture assertion
required an update.

## Phase 3.32: integer constant-operand arithmetic

`ArithK` removes an integer literal's load while retaining the shared arithmetic
and metamethod semantics (ADR 0054). The baseline is this lane's starting
commit `43bc7c9e7e33d44f90ac6dbf4b94d433a9529e7e`, including the destination
optimization above. All 22 full-size corpus workloads complete with identical
checksums. Their summed VM instructions fall from 2,182,552,709 to 1,862,271,177
(14.67% fewer); no workload increases. Numeric loops lose exactly 60,667,667
instructions, falling from 282,013,029 to 221,345,362 (21.51%).

Fresh whole-program callgrind runs use ordinary `bench-stable` binaries without
measurement features, identical divisor-100 sources and core 4. Ir falls by
19.86% in numeric loops, 15.27% in branches and 11.30% in array access. The sum
is 4,226,856,162 → 4,109,613,404 (2.77% fewer). The largest workload increase
is string_concat at 0.913%, below the 1% threshold. Summed percentages are
corpus weighting, not predicted application speedups. Both scaled runs match
PUC Lua 5.4.9 output. Counters use separate `counters,measure` builds.

An initial integer/float immediate form also fit in 16 bytes, but sending
integer-register/float-immediate multiplication through the requested cold path
made numeric loops 12.34% worse in Ir and native_calls 3.43% worse. It is rejected.
Float literals keep their loads; the numeric workload retains ten million
`0.5` loads. No numeric pool or new default dependency is introduced.

Wall/cycle timings on 2026-10-02 are diagnostic only: the shared machine is not
in performance mode. Full tables, profiles, counters, load records, binary
hashes, rejected-candidate evidence and gates are in `results/arithk/RESULTS.md`
and `results/arithk/TABLES.md`. Exact sort fuel assertions change 330 → 315 and
625 → 583; the compiler-limit fixture changes 3,332 → 4,999 repetitions. Fuel
revision stays 6, bytecode revision becomes 13, and schema/chunk/GC layouts stay
unchanged. Operator fingerprint inputs add `arithk.lua` and a reverse-operand
host wait; native/wasm comparisons cover the newly compiled bytecode.

## Compare-and-branch (Phase 3.32, Gate D)

`CompareBranch` removes boolean materialization and a separate branch for
branch-only comparison conditions. Value-producing comparisons keep `Compare`.
Same-type numeric operands share inline comparison primitives; the existing
cold comparison helper and Truth continuation handle all other types and
suspensions. Bytecode revision is 14, fuel revision remains 6, and Op stays
16 bytes. See ADR 0055 for pause-boundary and debug compatibility.

Fresh before/after full-size VM counts and divisor-100 whole-program callgrind
Ir are recorded in `results/cmpbr/TABLES.md` and `results/cmpbr/RESULTS.md`, with
source/binary identity, oracle checksums, gates and exact fuel assertion changes.
Machine wall/cycle timing on 2026-10-02 is diagnostic only; instruction counts
are the acceptance metric. Scaled Ir includes compilation, boot and teardown
and must not be extrapolated by multiplying by 100.

Starting from `1292388a61ae8d8d5b171028434b0168d7a6bc49`, all 22 workloads
complete with equal checksums. The 104,910,997 dynamic Compare/JumpIfFalse pairs
become exactly that many CompareBranch executions, leaving no such pairs in
this corpus. VM instructions fall 1,862,271,177 -> 1,757,323,742 (5.635%).
Scaled whole-program Ir falls 3,777,679,021 -> 3,735,939,124 (1.105%); branches
improves 13.170%, table_fields 4.548%, application 2.672%, fib 2.589%.
Worst Ir change is numeric_loops at +0.624%, within the 1% gate. No VM-count
regression occurs. Exact sort fuel pins change 315 -> 307 and 583 -> 568.
Oracle, Gate Q, quantum-1 restores, native/wasm fingerprints and Moss gates pass.
These instruction counts are acceptance evidence; the wall times remain
shared-machine diagnostics, with measurement loads 4.84–8.41.

## Fixed Lua calls in the core (Phase 3.32, stage 4)

Baseline is 1292388; selected source is publish-frame-boxed. Both corpus and
actual Moss Game/update_only binaries use ordinary bench-stable builds, CPU 4,
revision-2 corpus divisor 100. Microloops use 1,000/10,000-iteration slopes;
Call+Return0 subtracts control and its supporting Move. Moss uses 10/60-frame
slopes after ten warmups at 100/1,000 interactions. All corpus/PUC outputs and
Moss fuel/crossing outputs match. Wall/cycle times remain diagnostic only.

| Metric | Before | Selected | Revised acceptance |
|---|---:|---:|---|
| Corpus sum Ir | 3,777,534,611 | 3,571,696,591 | -5.449%, pass |
| Worst workload, string_concat | — | +0.919% | <=+1%, pass |
| native_calls | — | +0.477% | <=+1%, pass |
| Moss Ir/frame, 100 | 832,232.36 | 836,274.32 | +0.486%, pass |
| Moss Ir/frame, 1,000 | 7,852,500.38 | 7,893,900.82 | +0.527%, pass |
| Move / loop control Ir | 33 / 62 | 31 / 62 | measured |
| Call+Return0 Ir | 1,182 | 690 | <=400 is a goal |
| Native Frame size | 120 B | 72 B | boxing selected by corpus/Moss |

V1's original native/Moss regressions were retained as history. The selected
variant checks native tags before frame setup, keeps opcode references across
dispatch, decodes frame operands out of line, and copies a cold opcode before
publishing the epoch. Resync publishes and returns to poll. Zero-result returns
skip the empty copy. Fixed frame-window semantics and slow handlers remain.
Both initial v1 and final-design boxing alternatives were measured; boxed wins
the corpus sum and both Moss sizes. Moss traces have equal fresh/reallocation
counts and no Pending construction stack in either variant. Pending boxing
still allocates on actual pending transitions outside the measured path.

The coordinator revised acceptance to corpus sum improvement and <=+1% for
all workloads and both Moss sizes, kept correctness gates/Gate Q and the 8 KB
symbol guard, made Call+Return0 a goal and waived the 320 B stack guard. Variant
numbers, assembly/stack sizes, code_diff, allocation stacks and final gate
results are appended in `results/hotcore/stage-4/REPORT.md`. No stage 5 work.

## Tail calls, open windows and Lua metamethods (Phase 3.32, stage 5)

Against 2a6df86, ordinary bench-stable builds on the divisor-100 revision-2
corpus reduce whole-program Callgrind Ir by 29.583% for tail_recursion and
24.173% for metamethods. Tail/open-window work alone passes every workload and
both Moss +1% gates; adding metamethod setup and return-helper continuation
completion also passes. Combined corpus Ir falls 3.751%; the worst row is
sort at +0.030%. Every terminal output matches the saved baseline and PUC 5.4.9.

Instruction profiles attribute repeated thread/frame lookups, scratch writes,
Lua frame construction and result/continuation finishing before optimization.
A first design committed metamethods at frame-window entry and regressed three
numeric workloads beyond +1%; it is rejected. Completion now belongs to the
out-of-line return helper, conditional on remaining instruction allowance.
The canonical MetaCall remains at an exhausted-quantum boundary. No fuel,
snapshot, bytecode, dependency or unsafe change is required.

Results, hashes, per-workload/Moss tables, microloop slopes, cache miss counts,
assembly and correctness gates are in `results/hotcore/stage-5/REPORT.md`.
Machine load and power configuration make wall/cycle data diagnostic only;
acceptance uses instruction counts. The core remains below 8 KB. Its stack
reservation is 472 B; the stage-4 coordinator waiver of the 320 B guard remains
explicit, rather than being counted as a passed guard.

## Lua call-path overhead (Phase 3.32, lane CC)

Against 9ddf98f, ordinary bench-stable Callgrind measurements reduce the
Call+Return0 slope from 696 to 661 Ir. The Call1 loop, including its LoadInt
and two supporting Moves, falls from 835 to 785 Ir. The 400-Ir goal remains
unmet. Pre-change instruction-address attribution separates preconditions,
frame initialization/push/pop, nil fill, result transfer, truncation, window
re-entry and fuel bookkeeping in `results/callpath/PROFILE.md`.

Frame helpers pass the existing FrameHeap borrow directly, skip no-op growth
calls and avoid clearing slots that resize already initialized. Discarded
returns omit source arithmetic; single-result copies use a scalar assignment.
Frame truncation drops in place. Return and tail-call clearing still nils all
retained slots; omitted writes affect only Vec elements immediately removed
before any collection, debug inspection, snapshot or subsequent instruction.
A future resize initializes those removed slots to nil again. Canonical stack
contents/length, logical charges, GC resynchronization and pause points retain
their meanings. Frame layout and portable images are unchanged.

All 22 divisor-100 corpus outputs match PUC and before/after. Corpus sum Ir
falls 0.852%; fib improves 5.308%, closures 2.758%, method_calls 1.559% and
tail_recursion 5.079%. The worst row, empty, rises 0.219%, within +1%.
Actual Moss frame Ir is effectively unchanged. Every comparison uses
instruction counts; wall/cycle data on the shared non-performance machine
is diagnostic only. Full tables, source/binary identity, Gate Q and required
gate results are in `results/callpath/RESULTS.md`.

## Library Lua callbacks (Phase 3.32, lane SC)

Against 9ddf98f, ordinary bench-stable Callgrind measurements on the
divisor-100 revision-2 corpus reduce sort Ir from 232,937,980 to 201,083,202
(-13.675%). All 22 outputs match both the baseline and PUC Lua 5.4.9;
the worst workload change is table_fields at +0.292%, within the +1% gate.
The corpus sum falls 1.331%. Actual Moss Game/update_only instruction slopes
improve 0.114% and 0.085% at 100 and 1,000 interactions per frame.

A two-element sort microloop, with one comparator call per iteration,
falls from 7,899.011 to 7,122.008 Ir/call. Subtracting its matched primitive
sort control gives 3,282.006 to 2,509.002 Ir/call (-23.553%) for the extra
Lua callback machinery. The whole-iteration slope includes the loop, table
resets and sort; neither row claims an isolated function-call cost.

Direct fixed Lua library callbacks and xpcall handlers use one thread borrow
for the existing scratch window and frame setup. Library returns transfer
the ordinary boundary result window, then run the continuation within
run_hot after publishing fuel. A builtin continuation still costs one fuel
unit; a handler completion remains uncharged. Exhausted quanta, collection,
finalizers, callable chains, varargs, closes and reserve/quota cases retain
their original boundaries or slow paths. Sort's algorithm and machine state,
yield restrictions, snapshot encoding and fuel revision are unchanged.

Full measurements, rejected dispatch variants, callback yield/error fixtures,
Gate Q and gate logs are in results/sortcmp/RESULTS.md and TABLES.md.
All wall/cycle observations remain diagnostic on the loaded machine;
acceptance uses instructions, without extrapolating scaled counts to full size.

## Phase 3.32 metamethod and coroutine transfers (lane MM)

Pinned baseline `f9cc5e7`; ordinary default-feature `bench-stable`, corpus
revision 2, divisor 100, CPU 4. Callgrind whole-program instructions include
startup, compilation, installation and teardown. The shared machine is outside
performance mode; wall/cycle measurements are diagnostic only.

| Measurement | Before Ir | After Ir | Change |
|---|---:|---:|---:|
| metamethods, whole scaled workload | 278,356,081 | 258,396,375 | -7.170566% |
| coroutines, whole scaled workload | 175,336,566 | 162,646,421 | -7.237592% |
| `a.double`, including Lua driver | 3,089.000 | 2,950.000 | -4.500% |
| `a + b`, including Lua driver | 2,116.000 | 1,977.000 | -6.569% |
| `b(i)`, including Lua driver | 1,771.003 | 1,566.003 | -11.575% |
| warmed one-scalar resume/yield pair | 3,892.011 | 3,470.011 | -10.843% |

Operation counts subtract 1,000 from 2,000 iterations. The existing coroutine
microbinary uses `measure`; its 1,000 warmup pairs remain outside that slope.
The three metamethod microloops use the ordinary corpus runner. All 22 corpus
outputs match PUC Lua 5.4.9; every workload meets the +1% bound. The worst row,
fib, rises 0.418981%. Corpus sum falls from 2,900,394,156 to 2,869,135,129 Ir
(-1.077751%), saving 9,307,550 Ir over the earlier selected-only implementation.
Actual Moss ECS/ABI frame slopes rise 0.009325% / 0.006818% at 100 / 1,000
interactions. The coordinator clarified that Moss uses the same +1% bound;
these differences are measurement-level noise and pass that gate.

The final implementation combines direct non-vararg Lua `__call` resolution
and frame setup, decodes ordinary suspended yield sites once, bulk-copies
existing ordinary coroutine result windows, and reuses one completed plain
MetaCall box. The spare contains only metadata, with no values, handles or close
state. Live continuations stay on frames; restore starts with an empty spare.
No handler value is cached. Bounds, high-water logical charging, status
publication, scratch clearing, nil padding, stack length, top and PC retain
their meanings. Fault/reserve cases and boundary/close continuations keep
their existing paths. Core Call/Return helpers, field and arithmetic handlers
are unchanged; FrameHeap adds only the spare-box reference.

Full fresh data and disjoint per-operation attribution are in
`results/meta2/RESULTS.md`, `combined-OPERATION-PROFILE.md`,
`combined-OPERATIONS.md`, `combined-CORPUS.md`, `combined-MOSS.md` and
`combined-comparison.json`. The earlier selected/rejected measurements remain
as historical evidence; the combined result supersedes their selection.

The yielding metamethod fixture changes handlers while suspended, checks debug
names and transfer windows, restores every instruction in Full/NoFastCalls/Off,
runs against PUC, and joins the native/wasm coroutine fingerprint. Bytecode,
fuel, snapshot and GC revisions, default dependencies and frame layout are
unchanged. Full gate evidence is under `results/meta2/combined/gates`.

## String library machine overhead (Phase 3.32, lane PAT)

Against `2ded2ae`, portable default-feature `bench-stable` Callgrind runs on
corpus revision 2, divisor 100, reduce whole-program Ir by 2.239% for patterns,
5.380% for strings, and 7.901% for string_format. All 22 checksums match PUC
Lua 5.4.9 and the local baseline; no workload regresses. Nonempty corpus
Moonseed instruction geomean falls 0.768%; its sum falls 0.489%. With the
baseline PUC counts held fixed, the ratio geomean changes 2.732701 -> 2.711721.
This lane alone does not reach the campaign's approximately 2.65 ratio goal.
Actual Moss update-only instruction slopes improve 0.071% and 0.007% at 100
and 1,000 host interactions/frame.

The first string-library step enters its existing auxiliary runner directly,
omitting a temporary Work box. Find captures append to one sized result
buffer; gmatch borrows and updates its three-word closure state in place.
Format borrows string arguments and builds integer digits backwards in a fixed
stack buffer. Matcher internals, saved machine encodings, transition budgets,
copy debts, logical heap charges and builtin counters retain their meanings.
No physical batching or fuel revision change is needed.

The find, gsub, gmatch and format microloop instruction slopes improve 9.483%,
0.321%, 5.606% and 8.078%. These include Lua driver work, conversion and GC;
gmatch also includes subject construction. Before/after witnesses compare
every snapshot byte and fuel count at quanta 1, 7 and 257 for the pattern,
format and string fixtures, including their terminal output, memory and GC
logs. Gate Q compares all three interpreter modes. Full tables, source/binary
identity, source attribution and correctness gate logs are in
`results/pat/RESULTS.md`, `COMPARISON.md` and `PROFILE.md`. The shared machine
is outside performance mode; wall times are diagnostic only.

## Phase 3.32 remaining frame setup (lane CAL)

Current baseline `2ded2ae`; ordinary default-feature `bench-stable`, divisor-100
revision-2 corpus, CPU 4. Whole-program Callgrind instructions include startup,
compilation and teardown. The shared machine remains outside performance mode;
wall times are diagnostic only.

| Measurement | Before | Selected | Change |
|---|---:|---:|---:|
| Call+Return0, Ir/pair | 658.002 | 654.000 | -0.608% |
| Function `__index`, including Lua driver, Ir/access | 2,837.003 | 2,829.001 | -0.282% |
| fib, whole-program Ir | 144,040,578 | 143,440,396 | -0.417% |
| metamethods, whole-program Ir | 254,436,660 | 253,956,600 | -0.189% |
| corpus sum Ir | 2,815,149,372 | 2,813,353,555 | -0.064% |
| nonempty instruction geomean | 1.000000 | 0.999480 | -0.052% |

All 22 Moonseed outputs match both baseline and PUC Lua 5.4.9, with identical
scaled sources and PUC binary hash. Every workload meets +1%; the largest
increase is the empty program's 0.001812%. Actual Moss ECS/ABI slopes are
806,680.90 -> 806,677.82 and 7,619,508.28 -> 7,619,503.66 Ir/frame at
100/1,000 transform interactions. These tiny differences establish the host
no-regression gate, rather than a material host speedup. PUC hash-table layout
varies across runs; acceptance uses matched Moonseed absolute instructions.

The selected source changes only `fast_call` and `fast_meta_call`. Compiler,
binary-chunk and snapshot validation enforce `params <= max_reg`; the installed
production prototype's parameter count needs no repeated clamp. Metamethod
setup clears only old slots after growth, because resize already initialized
new slots to nil. Scratch arguments, missing parameters, discarded arguments,
top, frame fields, high-water charging and capacity counters remain canonical.
Frame layout, snapshot/fuel/GC/bytecode revisions and dependencies are unchanged.
The instruction window, return and continuation machine code match baseline.

Two-entry and single-caller epoch code caches increased register pressure and
failed corpus bounds. Fusing the metamethod return with its continuation saved
only 9 Ir/access while adding 21 Ir to ordinary call pairs, so it was rejected.
The broader window and continuation costs, the 400-Ir aspiration and the phase's
2x geomean improvement target remain open. This lane contributes a measured
0.052% geomean reduction and stops at its accepted gate boundary.

Full tables, current-main profiles, rejected sources, proof limits and gate
logs: `results/cal/{RESULTS,CORPUS,PROFILE,MOSS,EXPERIMENTS,GATES}.md`.
## Phase 3.34: diagnostic cost

Final Callgrind measurements compare Phase 3.34 at `acbf853` with Phase 3.33 at `51af42c`, using the same portable `bench-stable` build and revision-2 corpus at divisor 100. The 21 nonempty success-path workloads have a whole-program instruction-count geomean change of **−0.073%**; every workload is within ±0.725%, below the ±2% gate. The empty startup probe is excluded. Outputs and PUC checksums match. These counts precede the later FLAT compiler change, which leaves bytecode identical for previously accepted inputs.

| Call benchmark | Base Ir/call | Phase 3.34 Ir/call | Change |
|---|---:|---:|---:|
| fixed 0/0 | 445.001 | 446.001 | +0.225% |
| metamethod index | 1584.993 | 1580.004 | −0.315% |
| method | 609.999 | 610.999 | +0.164% |
| tail call | 449.494 | 451.033 | +0.343% |

The call slopes subtract matched no-call controls and include setup and callee work. A separate one-million-call `return math.abs(x)` native-tail microbenchmark moves from 2,547,084,945 to 2,596,102,860 whole-program instructions (**+1.924%**); it has no no-call control. `Runtime::run_hot` is 6,762 → 6,638 bytes (−124, −1.83%) by `nm -S -C`; the code is not byte-identical.

Diagnostics reconstruct provenance on the cold error path. No diagnostic sidecar is serialized and snapshot, bytecode and binary-chunk revisions do not change. Four prototype-accounting probes have identical base and Phase 3.34 logical boot-heap totals: small script 4,756 bytes, 100,005-instruction function 1,604,742, 180 locals 9,787, and 1,000 field writes 81,675. These are charged heap totals, not host allocator measurements. The loaded-machine paired wall geomean of 1.044× is diagnostic only. Raw Callgrind, binary identity, metadata and suite evidence is in the Phase 3.34 lane Z report under `g334/results/z/report.md`.

## Phase 3.35: debug hooks

Final lane Z compares `8c0db66` with the Phase 3.34 end at `eee7dff`.
Portable `bench-stable`: opt 3, one codegen unit, fat LTO, line tables, empty
RUSTFLAGS, rustc 1.98.1; Callgrind on CPU 4. Revision-2 corpus sources use
divisor 100. Whole-process Ir includes startup, compilation, installation,
execution and teardown. All 22 outputs match PUC Lua 5.4.9 and the paired
revision, with matching source hashes and unchanged measured binaries.
Scaled results are not extrapolated to full-size execution. Final evidence,
source/binary identities and receipts are in `g335/results/z/report.md`.

### Hooks off

| Workload | Base Ir | HEAD Ir | Delta |
| --- | --- | --- | --- |
| empty | 1,029,602 | 1,037,945 | +0.8103% |
| fib | 104,578,316 | 104,588,885 | +0.0101% |
| table_fields | 122,117,763 | 122,138,741 | +0.0172% |
| alloc_churn | 212,038,234 | 214,630,037 | +1.2223% |
| strings | 72,288,971 | 72,538,700 | +0.3455% |
| sort | 165,173,254 | 165,310,113 | +0.0829% |
| numeric_loops | 128,309,854 | 128,323,048 | +0.0103% |
| generic_for | 175,826,972 | 175,496,816 | -0.1878% |
| method_calls | 116,789,607 | 116,843,763 | +0.0464% |
| closures | 132,348,990 | 132,366,169 | +0.0130% |
| coroutines | 153,054,574 | 154,116,561 | +0.6939% |
| metamethods | 240,599,365 | 240,811,302 | +0.0881% |
| patterns | 172,982,502 | 172,997,662 | +0.0088% |
| native_calls | 148,413,191 | 148,625,307 | +0.1429% |
| application | 129,822,445 | 129,672,778 | -0.1153% |
| branches | 80,989,096 | 80,999,599 | +0.0130% |
| array_access | 118,853,499 | 118,864,988 | +0.0097% |
| globals | 73,866,448 | 73,875,471 | +0.0122% |
| tail_recursion | 57,197,881 | 57,207,336 | +0.0165% |
| field_writes | 112,008,242 | 112,017,948 | +0.0087% |
| string_concat | 56,925,585 | 57,006,759 | +0.1426% |
| string_format | 62,607,649 | 62,528,721 | -0.1261% |

The 21 nonempty workloads have an Ir geomean change of **+0.116434%**,
within the final ±1% gate and ±0.5% stretch. `empty` is excluded. The
`alloc_churn` outlier is **+1.222328%**, beyond the earlier individual ±1%
bound; the final gate is the nonempty geomean. Disjoint exclusive malloc
source rows grow 20,740,436 → 23,159,036 Ir (+2,418,600), accounting for
93.32% of the whole-program +2,591,803. `_int_malloc` contributes +1,372,962,
`malloc_consolidate` +632,867 and `unlink_chunk` +330,408 Ir. This locates
allocator work, without proving a source-level allocation regression or
subtracting it from the reported total.

Call slopes use 200/1000 iteration pairs and matched no-call controls. They
include differing setup and callee work, not isolated intrinsic ABI costs;
all PUC and paired checksums match.

| Family | Base Ir/call | HEAD Ir/call | Delta |
| --- | --- | --- | --- |
| fixed_00 | 446.001484 | 446.001797 | +0.0001% |
| meta_index | 1579.979766 | 1582.986797 | +0.1903% |
| method | 610.998828 | 610.999375 | +0.0001% |
| tail_call | 449.987188 | 449.966680 | -0.0046% |

`Runtime::run_hot` grows **6,638 → 6,650 bytes** (+12, +0.1808%). The
hook-trap branch is checked once per epoch; hooks off add no per-instruction
work. ThreadObj and arena element sizes are unchanged by hook storage.

The CPU-only Moss ECS/ABI harness uses pinned Moss `dd8673c`, 10 warmup
frames and a 10/60 measured-frame pair: Ir/frame = (Ir60−Ir10)/50.

| Interactions/frame | Base Ir/frame | HEAD Ir/frame | Delta | Host allocs/frame | Fuel/frame | Crossings/frame |
| --- | --- | --- | --- | --- | --- | --- |
| 100 | 801,898.00 | 806,978.78 | +0.6336% | 11 → 11 | 1528 | 152 |
| 1,000 | 7,570,518.04 | 7,617,017.48 | +0.6142% | 11 → 11 | 14578 | 1502 |
| 10,000 | 75,210,561.72 | 75,671,306.86 | +0.6126% | 11 → 11 | 145078 | 15002 |

All three sizes stay within +1%; paired fuel and crossing counts match.
Both builds pass the four-frame checkpoint/rebind replay witness and report
zero collections in the separate 50-frame allocation run. This is CPU harness
evidence; renderer/GPU and broader Moss release acceptance are not established.
Paired corpus wall-clock geomean 1.002× and Moss wall values are diagnostic
on the shared machine and do not decide acceptance.

### Hooks on

Four representative revision-2 bodies use divisor 1000. Each ratio compares
the same final measurement runner and body with hooks off. Lua callbacks are
empty functions; the public host callback returns Continue. Output checksums
match. These are whole-process Ir ratios including installation/startup,
not per-event costs. Hooked threads use the cold executor even for c/r-only
hooks, producing substantial slowdown with no enabled-hook target claimed.

| Workload | Off Ir | Lua c/r | Lua l | Lua count=1 | Lua count=100 | Lua l+count=100 | Host c/r | Host count=1 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| fib | 16,277,761 | 37.799× | 33.491× | 78.890× | 20.259× | 34.486× | 26.331× | 41.551× |
| numeric_loops | 13,963,972 | 31.322× | 99.792× | 137.812× | 32.468× | 100.219× | 31.319× | 70.792× |
| method_calls | 12,870,505 | 17.027× | 27.727× | 52.763× | 13.265× | 27.954× | 14.386× | 27.639× |
| generic_for | 19,762,998 | 17.506× | 18.220× | 39.591× | 11.231× | 18.673× | 13.027× | 21.548× |

The existing recording `chook` callback creates inspection tables and trace
records. Measured separately at divisor 10000 against the official runner
with hooks off, it includes those callback-owned costs:

| Existing chook c/r (divisor 10000) | Off Ir | Enabled Ir | Ratio |
| --- | --- | --- | --- |
| fib | 2,531,626 | 316,460,791 | 125.003× |
| numeric_loops | 2,517,646 | 45,403,184 | 18.034× |
| method_calls | 2,458,431 | 81,801,695 | 33.274× |
| generic_for | 4,172,461 | 181,714,701 | 43.551× |

### Allocations and checkpoint sizes

After 20 warmup iterations, paired 100/1000-iteration windows measure delivered
event differences. Every window has one constant 6-byte wait-marker operation
name; its cost cancels, leaving zero incremental allocations and bytes per
VM-delivered event in all ten configurations:

| Target | Mask | Count | Extra delivered events | Allocs/event | Bytes/event |
| --- | --- | --- | --- | --- | --- |
| Lua | cr | 0 | 1800 | 0 | 0 |
| Lua | l | 0 | 1800 | 0 | 0 |
| Lua | count | 1 | 4500 | 0 | 0 |
| Lua | count | 100 | 47 | 0 | 0 |
| Lua | l | 100 | 1854 | 0 | 0 |
| host | cr | 0 | 1800 | 0 | 0 |
| host | l | 0 | 1800 | 0 | 0 |
| host | count | 1 | 4500 | 0 | 0 |
| host | count | 100 | 45 | 0 | 0 |
| host | l | 100 | 1845 | 0 | 0 |

The existing allocation regression also passes. New stacks, installation,
owned inspection metadata, tables, roots and trace buffers may allocate;
zero applies to warmed delivery with nonallocating callbacks.

Schema-23 snapshots give these measured per-thread HookImage spans. The
increment subtracts the absent state's one-byte tag; whole-runtime totals
come from different programs and are not pure hook-state deltas.

| Thread state | Whole snapshot B | HookImage B | Incremental B vs absent |
|---|---:|---:|---:|
| no hook | 12425 | 1 | 0 |
| Lua hook | 12778 | 86 | +85 |
| host hook | 12493 | 51 | +50 |
| pending event | 12522 | 53 | +52 |
| host-yield suspended coroutine | 12946 | 57 | +56 |

Host symbol length, cursors, transfer payloads and instruction stages affect
encoding. Lua closures, prototypes and event-name strings also live outside
HookImage. Same-program host installation grows 12,425 → 12,493 bytes (+68),
of which +50 is the hook span. A paired Lua probe retaining the same closure
and compiled source grows 12,670 → 12,886 bytes (+216), including associated
heap/frame encoding. The separate internal snapshot witness reports no hook
11,228, host 11,277, Lua 11,530, pending 11,623 and suspended host yield 12,896
bytes; its different scripts cannot be subtracted as state-only costs.

## Phase 3.36: UTF-8 library

Portable `bench-stable`, fixed corpus revision 2, divisor 100, CPU 4,
whole-program Callgrind Ir against the pre-implementation `060411c` base.
The 21 nonempty workloads have an Ir geomean change of **+0.058%**, with
**+0.216%** the worst ordinary delta (`alloc_churn`), within the ±0.5%
geomean / ±1% per-workload gates. All checksums match; hooks are off and
these workloads do not use UTF-8. Empty startup rises **+3.725%** from
standard-profile module installation and is excluded from the nonempty gate.
`run_hot` remains **6,650 bytes** in both binaries. The final review rerun
confirms +0.05792% nonempty geomean and +0.21618% worst ordinary delta.
Source/executable hashes, raw counts and receipts are in
`g336/results/u1/report.md` and `g336/results/rv/REVIEW.md`.

### UTF-8 characterization (Gate N)

All 22 shapes match PUC Lua 5.4.9 output. One paired warmup and three measured
sequential process pairs supply the wall columns below. **Wall ns/processed
byte is diagnostic full-process time, including startup and setup, on the
shared machine; it is not timing qualification.** The separate Rust probe's
fuel, auxiliary steps and logical object allocations include script setup,
loops and result-table allocations, not just the UTF-8 function. Logical
allocations count heap objects, not allocator calls. No SIMD or target-specific
optimization was added.

| Shape | Moonseed wall ns/byte (diagnostic) | PUC wall ns/byte (diagnostic) | Fuel | Aux steps | Logical object allocations |
| --- | ---: | ---: | ---: | ---: | ---: |
| len-ascii-1024 | 27.272 | 24.010 | 1296 | 513 | 1 |
| len-two-1024 | 22.968 | 26.163 | 1040 | 257 | 1 |
| len-three-1024 | 25.212 | 19.959 | 1040 | 257 | 1 |
| len-four-1024 | 28.874 | 24.913 | 912 | 129 | 1 |
| len-mixed-1024 | 29.142 | 27.861 | 1040 | 257 | 1 |
| len-ascii-1048576 | 5.228 | 3.841 | 17327 | 16640 | 1 |
| len-two-1048576 | 3.417 | 3.428 | 9135 | 8448 | 1 |
| len-three-1048576 | 3.076 | 2.359 | 6407 | 5720 | 1 |
| len-four-1048576 | 2.456 | 2.025 | 5039 | 4352 | 1 |
| len-mixed-1048576 | 2.556 | 2.295 | 7499 | 6812 | 1 |
| codepoint-1 | 4111.851 | 3568.066 | 12332 | 1001 | 1001 |
| char-1 | 993.588 | 908.698 | 12018 | 1000 | 1001 |
| codepoint-32 | 191.527 | 92.680 | 13012 | 1001 | 1001 |
| char-32 | 60.206 | 40.590 | 13619 | 1000 | 1001 |
| codepoint-1000 | 127.636 | 42.089 | 19043 | 401 | 101 |
| char-1000 | 37.579 | 23.681 | 10898 | 400 | 101 |
| offset-ascii-1 | 5.127 | 4.154 | 723 | 260 | 1 |
| offset-ascii--1 | 4.946 | 4.092 | 851 | 324 | 1 |
| codes-ascii | 356.224 | 205.702 | 114928 | 16385 | 1 |
| offset-multibyte-1 | 1.974 | 1.241 | 1617 | 1040 | 1 |
| offset-multibyte--1 | 1.721 | 1.273 | 1745 | 1104 | 1 |
| codes-multibyte | 94.588 | 50.841 | 114928 | 16401 | 1 |

Raw samples, source/executable hashes, fuel, heap bytes and allocation categories
are in `g336/results/u1/utf8-perf/report.json`; reproduction scripts and receipts
are listed in `g336/results/u1/report.md`.

## Phase 3.37: host capabilities

Final F1 measurements compare Phase 3.36 base `45facb7`, the reviewed
pre-F1 source `2cc93e8`, and the final implementation integrated as `ab991a2`.
Portable `bench-stable`: opt-level 3, one codegen unit, fat LTO, empty RUSTFLAGS,
Rust 1.98.1 / LLVM 22.1.8, CPU 4 on Ryzen 9 7945HX in performance mode.
The fixed revision-2 non-IO corpus uses divisor 100 and whole-process Callgrind
Ir; checksums match base, before, final and pinned PUC Lua 5.4.9. These counts
include startup, compilation, library setup and teardown. Evidence:
`g337/results/perf/report.md`, `g337/results/f1/report.md` and
`g337/results/f1/performance.md`; F1 supersedes the initial performance counts.

### Non-IO instructions and startup after F1

All 20 nonempty workloads except `alloc_churn` are within ±0.5% of `45facb7`;
the 21-workload geometric mean is **+0.121252%**. Moving the epoch's fuel-limit
input restores register-window length to a register; separating auxiliary
library dispatch preserves the table/sort loop. `numeric_loops` improves from
+1.2722% to +0.1035%; sort from +0.8026% to −0.2532%.

Startup remains **+12.2300%**, 1,076,554 → 1,208,217 Ir, rather than meeting
the every-workload ±0.5% target. Its primary `install_os` inclusive subtree
costs 52,197 Ir, with other registration/lookup and dynamic-loader work;
inclusive subtrees overlap and are not summed. The historical non-IO runner
installs OS, not IO, so this is not full IO/OS profile startup cost.
`alloc_churn` is −1.3416%, dominated by allocator-placement changes, not a VM
allocation improvement. The matched warmed call slopes include argument setup
and callee work, not just intrinsic ABI cost. Final `run_hot` is 6,610 bytes,
100 below before and 40 below base; instruction counts do not prove code identity.

| Workload | Base Ir | Before Ir | Final Ir | Before/base | Final/base |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty | 1,076,554 | 1,208,335 | 1,208,217 | +12.2410% | +12.2300% |
| fib | 104,627,327 | 105,135,004 | 104,759,771 | +0.4852% | +0.1266% |
| table_fields | 122,179,029 | 123,002,911 | 122,324,423 | +0.6743% | +0.1190% |
| alloc_churn | 215,078,236 | 211,956,933 | 212,192,775 | -1.4512% | -1.3416% |
| strings | 72,572,019 | 72,527,544 | 72,561,731 | -0.0613% | -0.0142% |
| sort | 165,310,278 | 166,637,131 | 164,891,722 | +0.8026% | -0.2532% |
| numeric_loops | 128,361,323 | 129,994,332 | 128,494,170 | +1.2722% | +0.1035% |
| generic_for | 175,535,015 | 176,076,530 | 176,237,568 | +0.3085% | +0.4002% |
| method_calls | 116,883,506 | 117,336,773 | 117,096,653 | +0.3878% | +0.1824% |
| closures | 132,405,727 | 133,139,858 | 132,539,832 | +0.5545% | +0.1013% |
| coroutines | 154,155,095 | 154,825,304 | 154,735,884 | +0.4348% | +0.3768% |
| metamethods | 240,849,828 | 241,103,476 | 241,383,349 | +0.1053% | +0.2215% |
| patterns | 173,113,441 | 173,266,434 | 173,374,177 | +0.0884% | +0.1506% |
| native_calls | 148,663,639 | 148,936,419 | 149,076,372 | +0.1835% | +0.2776% |
| application | 129,779,441 | 130,782,064 | 130,421,809 | +0.7726% | +0.4950% |
| branches | 81,038,218 | 81,628,117 | 81,170,781 | +0.7279% | +0.1636% |
| array_access | 118,904,502 | 119,789,672 | 119,039,541 | +0.7444% | +0.1136% |
| globals | 73,913,977 | 74,465,921 | 74,045,747 | +0.7467% | +0.1783% |
| tail_recursion | 57,245,979 | 57,678,069 | 57,378,100 | +0.7548% | +0.2308% |
| field_writes | 112,056,219 | 113,087,708 | 112,187,597 | +0.9205% | +0.1172% |
| string_concat | 57,047,693 | 57,142,962 | 57,324,818 | +0.1670% | +0.4858% |
| string_format | 62,569,847 | 62,743,768 | 62,773,651 | +0.2780% | +0.3257% |

21 nonempty workload geometric mean: **+0.121252%**.

| Call family | Base Ir/call | Before Ir/call | Final Ir/call |
| --- | ---: | ---: | ---: |
| fixed_00 | 445.945859 | 448.000703 | 445.952422 |
| meta_index | 1583.079609 | 1582.005000 | 1584.986797 |
| method | 611.005313 | 613.772578 | 610.999844 |
| tail_call | 449.997695 | 452.514375 | 450.000273 |

run_hot bytes: base **6650**, before **6710**, final **6610**.

### IO and module windows after F1

Wall values below are diagnostic: one warmup and five alternating paired
windows against a pinned PUC marker binary on the shared machine. Setup and
compilation precede the markers; measured operations include their loops and
open/close where applicable. Separate wall and metrics binaries prevent the
allocation counters from affecting timing. Native fixtures stay inside a
scratch root; VFS supplies the same bytes, while PUC has only native IO. VFS/PUC
ratios compare end-to-end implementations, not isolated VM/backend speed.
All before/final, VFS/native and PUC outputs agree. Full samples, host load,
source/binary hashes and exits are retained in the F1 results.

Dimensions: 100,000 seven-byte lines (800,000-byte file), 1 MiB read/write/load,
10,000 set-position seeks, 100,000 cached requires, 1,000 cold requires, and
100 searches over 100 templates (99 misses). Read-op columns count `read_at`
only: loadfile/cold require use `read_file_range`, and searchpath uses probes.
Lua objects are logical heap allocations; host allocated bytes are allocator
request totals, not retained or peak memory. Fuel includes bytecode, builtin
work and scheduled collection; the fuel revision remains 7.

| Workload | Backend | Before ms | Final ms | Before/PUC | Final/PUC | Read ops before/final | Lua objects before/final | Host allocated bytes before/final | Fuel before/final |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| empty_window | vfs | 0.0008 | 0.0010 | 3.439x | 3.455x | 0/0 | 0/0 | 291/291 | 4/4 |
| empty_window | native | 0.0008 | 0.0009 | 3.306x | 3.909x | 0/0 | 0/0 | 291/291 | 4/4 |
| read_line_loop | vfs | 582.2825 | 65.6556 | 42.899x | 4.353x | 100001/50 | 200004/100053 | 1,704,188,499/4,524,285 | 1,454,226/1,326,868 |
| read_line_loop | native | 734.1030 | 55.2739 | 50.390x | 4.539x | 100001/50 | 200004/100053 | 1,705,239,805/4,544,167 | 1,454,226/1,326,868 |
| read_1m | vfs | 2.8918 | 1.9788 | 1.539x | 1.259x | 17/17 | 21/21 | 7,311,082/7,308,378 | 905/768 |
| read_1m | native | 2.8521 | 2.6859 | 1.246x | 1.458x | 17/17 | 21/21 | 7,377,299/7,374,595 | 905/768 |
| write_1m | vfs | 2.5947 | 2.2387 | 5.617x | 5.242x | 0/0 | 19/19 | 6,240,429/6,240,461 | 42/42 |
| write_1m | native | 2.3260 | 1.9505 | 4.859x | 4.917x | 0/0 | 19/19 | 4,209,442/4,209,474 | 42/42 |
| seek_loop | vfs | 5.6003 | 6.3486 | 0.833x | 0.857x | 0/0 | 3/3 | 1,313,777/1,313,809 | 110,023/110,023 |
| seek_loop | native | 5.0095 | 5.6249 | 0.912x | 0.850x | 0/0 | 3/3 | 1,314,458/1,314,490 | 110,023/110,023 |
| io_lines_100k | vfs | 563.0778 | 43.7539 | 60.880x | 4.113x | 100001/50 | 200005/100054 | 1,704,188,581/4,524,367 | 854,370/726,915 |
| io_lines_100k | native | 744.2109 | 58.1680 | 71.487x | 4.004x | 100001/50 | 200005/100054 | 1,705,239,887/4,544,249 | 854,370/726,915 |
| loadfile_1m | vfs | 4.6316 | 6.0269 | 2.720x | 3.631x | 0/0 | 21/21 | 7,308,013/7,308,013 | 697/697 |
| loadfile_1m | native | 4.2541 | 3.6225 | 2.775x | 3.018x | 0/0 | 21/21 | 7,380,723/7,380,723 | 697/697 |
| require_cached | vfs | 63.1593 | 26.8284 | 12.592x | 5.395x | 0/0 | 100000/0 | 2,744,339/947 | 626,408/600,008 |
| require_cached | native | 66.0969 | 28.1211 | 14.148x | 4.564x | 0/0 | 100000/0 | 2,883,619/947 | 626,408/600,008 |
| require_cold | vfs | 8.0530 | 12.1032 | 0.869x | 0.870x | 0/0 | 12000/12000 | 5,171,127/5,171,127 | 26,421/26,421 |
| require_cold | native | 22.3213 | 33.4220 | 2.403x | 3.042x | 0/0 | 12000/12000 | 71,538,127/71,538,127 | 26,421/26,421 |
| searchpath_100 | vfs | 11.2003 | 13.4947 | 0.713x | 0.755x | 0/0 | 10100/10100 | 6,315,631/6,315,631 | 15,432/15,432 |
| searchpath_100 | native | 46.6945 | 43.6014 | 2.377x | 2.730x | 0/0 | 10100/10100 | 12,079,131/12,079,131 | 15,432/15,432 |

A 16 KiB canonical file buffer cuts each line workload from 100,001 reads to
**50** (49 data refills and one EOF probe). StringObj allocations fall from
200,003 to **100,052**, including refill/setup; each line needs only its result
string. Total host allocated bytes fall from about 1.704–1.705 GB to
**4.524–4.544 MB**. This retains charged buffers, bounded work, Pending support
and checkpoint/replay semantics.

Final line-read ratios remain **4.0–4.5x PUC**, missing the approximately 3x wall
target. Cached require allocates **zero Lua objects**, down from 100,000,
performs zero filesystem operations and requests 947 host bytes in the window;
its paired wall ratio remains **4.56–5.40x**. Cold require still makes 1,000
probes plus 1,000 range reads; searchpath makes 10,000 probes. Read/load 1 MiB
use 17 calls including EOF; write uses 16 × 64 KiB requests. Set-position seeks
change VM cursor state without backend IO. Startup, cached-require and line-read
wall costs remain release follow-ups; no stable wall-speed or release acceptance
is inferred from these shared-machine samples.
