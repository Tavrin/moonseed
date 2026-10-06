# Status

Phase 1 acceptance gate: **passed** on this machine (2026-09-22).

Phase 1.5, Gate A (call/result/assignment semantics) and Gate B (measurement, no speculative optimizer): **passed** on this machine (2026-09-22). See `PERFORMANCE.md`. The call path did not need a further allocation rewrite: warmed scalar, nested, and multi-result calls report `stack_grows 0` and `frame_grows 0`.

Phase 2, deletion-stable `next` and raw table length: **passed** on this machine (2026-09-22). Snapshot schema is 3. Warmed calls still report `stack_grows 0` and `frame_grows 0`. Integer field update measured 155 ns and string field update 182 ns, against the Phase 1.5 figures of 163 ns and 195 ns. No field cache was added.

Phase 3, Gates A–C (Lua 5.4 byte lexer, a closure/upvalue compiler, and source execution under the existing fuel, GC, and checkpoint gates): **passed** on this machine (2026-09-22). Snapshot schema is still 3. `LoadBool` is opcode tag 31. See ADR 0011.

Phase 3.37 (host capabilities, IO/OS and filesystem loading): **done** (2026-10-05).
Six optional capabilities provide filesystem, standard streams, wall/CPU time,
local civil conversion, environment and process operations. Library installation
grants no authority. IO, file methods/lines, loadfile/dofile, package.searchpath,
the filesystem Lua searcher, OS and explicit `install_arg` are implemented.
The VFS and deterministic mocks are portable; native adapters are feature-gated.
Native root validation has a TOCTOU gap; strong confinement needs VFS or an
outer sandbox. C locale only, no C modules; `os.exit` reports a terminal host
outcome and never kills the process. See [ADR 0060](adr/0060-host-capabilities.md).

Snapshot schema **24 → 25** preserves file cursors/read buffers, typed Pending
operations, IO/loading/OS work and exit closing state. Fuel **7**, bytecode
**14**, tables **4**, GC policy **12** and binary chunk **2** are unchanged.
External reads are journaled, mutations deduplicated exactly once with retained
request/outcome records. VFS live files rebind; native files and pipes refuse
snapshots, including pending acquisitions; closed files are portable.

Final F1 gates pass: workspace **697 passed, 0 failed**, 13 explicit PUC oracle
tests, both Clippy configurations, formatting and host boundary checks.
Hostlib matches **1,869/1,869** (1,270 native + 599 VFS); diagnostics
**3,815/3,815**, hooks **151/151 acceptance** (157/193 overall, 36 documented
oracle-only differences), UTF-8 **508,865/508,865**. Checkpoint/replay tests cover
quanta 1/2/3/7, both collectors and Ready/Pending hosts; the portable host
fingerprint matches native/Wasm, with both ignored roundtrip tests run and passed.
One review/fix round repaired ten findings, rejected the extreme-seek hypothesis
against PUC, and retained the documented native-confinement and buffering limits.

The official suite compiles **33/33** and rises **8 → 14 PASS**, with 19
LUA_ERROR files. New passes are bitwise, heavy, sort, tracegc, vararg and verybig;
the checked-in C6 ledger is `tests/compat/lua54-phase337.json` and current.
The final F1 suite independently retains 14/33. `files.lua:675` still fails
PUC's deferred full-buffer visibility assertion: Moonseed writes through.
Missing T.* methods, C searchers, PUC binary headers, `_HOOKKEY`, long-string
`%p`, stackless/C-stack assumptions and CLI/coroutine choreography remain.

After F1, nonempty Ir geomean versus `45facb7` is **+0.121252%**; 20 ordinary
workloads are within ±0.5%, with allocator-driven `alloc_churn` −1.3416%.
Startup remains **+12.2300%**. Buffered line reads need **50** capability reads
instead of 100,001 but diagnostic wall ratios remain **4.0–4.5x PUC**.
Cached require allocates **zero Lua objects**, with wall cost still open.
See [PERFORMANCE.md](PERFORMANCE.md#phase-337-host-capabilities).

Next: **3.38 release closure**. Resolve long-string `%p` identity, the
write-through versus full-buffering policy, the c/r hook fast path, startup
cost, cached-require/line-read wall time and async capability ergonomics.
Broader suite conformance, fuzzing, security and release checks remain open;
3.38 is not started here.

Phase 3.36 (Lua 5.4.9 UTF-8 library): **done** (2026-10-05).
`char`, `charpattern`, `len`, `codepoint`, `offset` and `codes` are installed
through the module registry. Lua strings remain bytes; strict decoding accepts
Unicode scalar values, lax decoding Lua's extended UTF-8 through `0x7fffffff`.
There is no normalization, grapheme handling or case folding. The lexer and
`char` share one encoder, and strict/lax iterators have stable function values.
Snapshot schema **23 → 24** adds `Work::Utf8`; bytecode **14**, tables **4**,
fuel **7**, GC policy **12** and binary chunk format **2** are unchanged.
Work steps handle 256 sequences/arguments or 4,096 navigation steps, using
the existing builtin fuel, scratch accounting and checkpoint model.

The frozen UTF-8 corpus passes **508,865/508,865**; fresh review fuzz matches
**168,300/168,300**. Checkpoint matrices cover quanta 1/2/3/7 and both
collectors; results, errors, fuel and exact heap charges agree. Native/Wasm,
hooks (**151/151** acceptance cases), workspace (**600 library tests**),
Clippy and oracle gates pass in the final review. One P2 finding was fixed:
restore now rejects structurally impossible UTF-8 work, with a 13-mutation
regression. Diagnostics remain **3,813/3,815**, with only the two existing
`package.searchpath` differences.

Fixed-corpus nonempty Callgrind Ir geomean is **+0.058%**, worst ordinary
workload **+0.216%**; `run_hot` remains 6,650 bytes. Gate N characterizes
22 UTF-8 shapes; full-process wall time is diagnostic. The official suite
compiles **33/33**: **8 PASS, 23 LUA_ERROR, 2 COMPLETED_WITHOUT**, up from
6 PASS; `pm.lua` and `utf8.lua` now pass. See
[LUA_COMPATIBILITY.md](LUA_COMPATIBILITY.md#utf-8),
[PERFORMANCE.md](PERFORMANCE.md#phase-336-utf-8-library), and
`tests/compat/lua54-phase336.json`.

The planned IO/OS/filesystem follow-up is completed in Phase 3.37 above.

Phase 3.35 (Lua and host debug hooks): **done** (2026-10-05).
Call, tail-call, return, line and count events, `debug.sethook/gethook`,
per-thread suppression and PUC coroutine inheritance are implemented. Lua
hooks are non-yieldable; registered synchronous host hooks support zero-value
line/count preemption and inspection of the interrupted activation. Snapshot
schema **23** preserves installed, pending, mid-Lua-hook, host-wait and
host-yield states. Fuel revision **7** charges one unit per delivery; bytecode
14, tables 4, GC policy 12 and binary chunk format 2 are unchanged.
The frozen hook corpus passes **151/151 acceptance** cases, **157/193** overall;
36 mismatches are designated oracle-only count/yield-position or unsupported
C-yield cases. Diagnostics remain **3,813/3,815**. Checkpoint matrices cover
quanta 1/2/3/7, both collectors and hot/cold modes; native/Wasm hook exchange,
workspace, Clippy, oracle and allocation gates pass in the final reports.
The single review/fix round repairs pending-event validation, argument-window
quotas, main reload charging/reentry, native transfer windows and source lines.
Private `_HOOKKEY`, multiline-local concatenation line granularity, MSC chunk
bytes and absolute PUC count positions remain documented differences.

Final Z hooks-off Ir geomean is **+0.116434%** versus `eee7dff`, within the
final geomean gate; `alloc_churn` is **+1.222328%**, mostly allocator work.
`run_hot` grows 12 bytes, with no per-instruction hook work when hooks are off.
Enabled hooks cost 11.231–137.812× whole-process Ir in the representative
matrix; warmed delivery allocates zero per event. Moss CPU-only Ir/frame
increases +0.6126–0.6336%, with replay and fuel/crossing checks passing.
The official suite compiles **33/33**: **6 PASS, 25 LUA_ERROR, 2
COMPLETED_WITHOUT**. Four formerly skipped C-library files now reach missing
individual T methods; this adds coverage, not passes. `db.lua` reaches private
`_HOOKKEY` at 328, and `coroutine.lua` reaches missing `T.testC` at 656.
Broader suite conformance and release acceptance remain open. See
[ADR 0059](adr/0059-portable-debug-hooks.md),
[PERFORMANCE.md](PERFORMANCE.md#phase-335-debug-hooks), and
`tests/compat/lua54-phase335.json`.

Phase 3.34 (Lua 5.4.9 diagnostics): **implemented** (2026-10-04).
Runtime positions and operand names, `error` levels, call-site argument errors,
native-tail caller retention, and structured compile diagnostics match the
pinned corpus in 3,813/3,815 cases, including eight FLAT cases. The two
remaining cases call absent `package.searchpath`. The official suite compiles
33/33 files: 6 PASS, 21 LUA_ERROR, 4 COMPLETED_WITHOUT_T and 2
COMPLETED_WITHOUT. `constructs.lua` and `math.lua` were the new passes; at
that milestone first blockers included hooks, UTF-8, IO/OS, module loading, PUC binary-header
assumptions, and string identity. Phase 3.33 to 3.34 successful-workload
Callgrind Ir geomean moves −0.073%, all ordinary rows within ±0.725%. No
diagnostic sidecar or snapshot, bytecode or binary-chunk format revision was
added. See [ADR 0058](adr/0058-diagnostic-authority-and-static-provenance.md),
[PERFORMANCE.md](PERFORMANCE.md#phase-334-diagnostic-cost), and
`tests/compat/lua54-phase334.json`.

Phase 3.33 (activation record and call ABI): **implemented, Outcome B**
(2026-10-04). The 21-workload revision-2 Callgrind Ir/PUC geomean improves
**2.703 → 2.575x** and warmed fixed 0/0 call+return **687 → 445 Ir**
(−35.2%). This meets the minimum useful outcome at its 445-Ir floor, with
the remaining 4x rows and call costs attributed, but misses the ≤2.2x and
≤250 targets; 445 also exceeds the owner's 400-Ir stop threshold. The
72 → 40-byte frame, recycled cold/boundary/task boxes, retained logical
stack/frame extents and in-loop switches preserve semantic snapshot images.
Snapshot schema 22, bytecode 14, tables 4, fuel 6, GC policy 12 and binary
chunk format 2 are unchanged and verified against code constants. The final
review found no confirmed correctness finding. Paired wall 2.929 → 2.704x
PUC is diagnostic under load; Moss is +0.09% Ir/frame at 1,000 interactions.
The official suite remains 33/33 compile with 4 PASS, 23 LUA_ERROR, 2
COMPLETED_WITHOUT and 4 COMPLETED_WITHOUT_T; PUC oracle 13/13. See
[ADR 0057](adr/0057-compact-activation-records.md) and
[PERFORMANCE.md](PERFORMANCE.md#phase-333-activation-record-and-call-abi).

Phase 3.32 (the interpreter performance campaign): **implemented, Outcome B** (2026-10-04). The minimum instruction improvement gate is **missed by about 2%**: 1.962x versus 2x. Snapshot schema 22, bytecode 14 (ArithK 13, then CompareBranch 14), tables 4, fuel 6, GC policy 12 and binary chunk format 2, verified against the code constants. See [ADR 0056](adr/0056-interpreter-core-and-execution-caches.md) and [PERFORMANCE.md](PERFORMANCE.md#phase-332-interpreter-performance-campaign).
- **Corpus:** all 21 nonempty revision-2 workloads; Callgrind Ir-ratio geomean 5.300 at `48f94bd` → 2.701 at `d9154eb`, median 2.580, best numeric_loops 1.783, worst sort 4.754. Required final geomean for the 2x gate: at most 2.650. Final2 changes the PUC control binary; the reported ratio improvement is 1.962x, same-source Moonseed-only absolute Ir improvement 1.960x, both below the gate.
- **Wall and hardware:** paired-wall geomeans 6.270 → 2.781 (2.25x descriptive improvement) at the earlier `08d4448`; CPU 4, alternating pairs under load, outside performance mode. IPC is close to PUC on numeric/call workloads. These are diagnostic measurements, not quiet-machine acceptance.
- **Core and compiler:** epoch/frame-window register slices, one fuel countdown, inline numerics/upvalues and eligible Lua call/return/tail/open/metamethod frames; destination-aware emission, exact integer ArithK and CompareBranch. Per-emitted-instruction fuel stays one; source totals and pause locations can change.
- **Caches and crossings:** derived string hashes, validated field/userdata hints, dense integer and five-slot table paths; immediate builtins/iterators; reused continuation/call buffers. Warmed scalar host calls and borrowing native continuations allocate zero. Small constructor requests fall 8 → 3; other growth/wait allocations remain.
- **Moss:** at `08d4448`, 1,000 interactions use 1.334 ms median / 1.585 ms p95 per frame under load (8.00% / 9.51% of 60 Hz); 12.084M → 7.619M Ir/frame (1.586x), 4,536 → 14 normal allocation callbacks/frame. CPU-only ECS/ABI replay evidence, not renderer/GPU acceptance.
- **Review:** restored-top fast-return panic and Off-mode builtin proof coverage fixed in `4c675c3`; super-linear accelerator refill fixed in `128d529`. Pre-existing scalar-store barrier asymmetry is recorded as a Gate Q limit. Reviews precede the last string/sort/setup changes; those have their lane witnesses.
- **Semantic gates:** lane workspace, oracle, native/wasm and Moss evidence is retained in the reports; final PUC oracle 13/13 at `d9154eb`. The official suite at `4c675c3` is unchanged: 33/33 compile, 4 PASS, 23 LUA_ERROR, 2 COMPLETED_WITHOUT and 4 COMPLETED_WITHOUT_T, with the same blockers. No full conformance or release claim.
- **At the 3.32 handoff:** call pairs 654 intrinsic Ir versus PUC about 133–155, sort, metamethods, recursive calls, generic iterators, allocation churn and coroutines. Phase 3.33 addresses this handoff above; its remaining 445-Ir fixed-call cost has a separate proposal in ROADMAP.md.

Phase 3.31 (the embedding API, a 0.1 candidate): **passed** on this machine (2026-10-02). Snapshot schema 22; bytecode 12, tables 4, fuel 6, GC policy 12 and binary chunk format 2 unchanged. See ADR 0053 and docs/EMBEDDING.md.
- **Values:** owned `Value`s whose objects are rooted references, used with another runtime gives an API error; borrowed views for callbacks; strings are bytes; strict conversions with positioned errors; `Error { Lua, Vm, Api }` with any Lua error object.
- **Lookup by id:** expected O(1) (44 to 376 ns from 1,000 to 500,000 objects; a scan took up to 1.24 ms); stale and reused ids find nothing.
- **Host functions and closures:** stable symbols, typed adapters with Lua's argument errors, native closures with captured values.
- **Native → Lua:** continuation frames, no Rust recursion; they resume exactly once across yields, waits and checkpoints.
- **Host → Lua:** `start_call`/`run`/`finish_call` and `call` on the main thread, no thread per call.
- **Waits and effects:** general host waits with deterministic keys and an O(1) index; external effects replay exactly once.
- **Host objects and modules:** portable, refusing and rebindable userdata (`Rebind`: a key, looked up again on restore); a host module resolver, pure or journaled; `Runtime::restore` checks every host requirement before a runtime exists.
- **Moss:** `integration/moss/` (a private harness, not in the public repository) runs real `moss_ecs` entities from Lua through the public API only, checkpoints mid-frame with a wait pending and a continuation in flight, restores into a fresh host with entities and assets rebound, and replays the same frames; about 1.2 µs per host interaction from 100 to 10,000 per frame.
- **Examples and tests:** ten examples and 24 misuse tests; `missing_docs` on the public API.
- **Non-regression:** against the Phase 3.30 baseline, each workload's ratio to PUC moved by -28% to +7% (layout noise; nothing in the corpus calls the host). The official suite is unchanged.
- **Review:** one round. Two blockers and three majors fixed with regression tests (ADR 0053).

Phase 3.30 (the scalability envelope): **passed** on this machine (2026-10-01). Snapshot schema 20, bytecode 12, tables 4; fuel 6, GC policy 12, and binary chunk format 2 unchanged. See ADR 0052.
- **One resource model:** runtime limits (`Config`, `Limits`, `Runtime::limits`), compiler limits (`CompileLimits`, `compile_with_limits`), and decoder limits (the snapshot bound, the decode budget, structural ceilings). Defaults: 64 MiB quota (at most 2^31), 1,048,576 objects (at most 2^28), strings bounded by the quota (at most 1 GiB), 128 MiB snapshots, 2^20 instructions and constants per function, 65,536 functions per chunk, 16 MiB of source.
- **Every allowed state snapshots:** 100,000 strings, tables, closures and mixed objects; a million short-lived objects around 100,000 live ones in both modes; 2, 8 and 16 MiB strings; 100,000-entry dense, hashed, mixed, weak and ephemeron tables; a combined state of 44 MiB with coroutines, userdata, large strings and library calls in flight, restored 20 times per schedule with the same output and fuel. A full default heap makes a snapshot of 0.33 to 0.99 bytes per logical byte.
- **Restore into other limits:** `Runtime::from_snapshot_with_limits` runs the runtime under the smaller of each pair; a state past a smaller host limit is refused before a runtime exists.
- **Bounded decoding:** counts are checked against the input left, reservations are capped, and a budget bounds what a snapshot or binary chunk decodes before validation.
- **Quadratic paths removed:** a table churned by deletes and inserts (20,000 pairs on 100,000 keys: past 120 s before, 0.10 s now, PUC 0.09 s), and three restore lookups.
- **Benchmark baseline:** a fixed 15-workload corpus against PUC Lua 5.4.9 (`bench/`): Moonseed 2.4x (strings) to 13.3x (sort, numeric loops) slower, about 8-10x on most, startup at parity. See PERFORMANCE.md.
- **Official suite:** `gc.lua` runs to its end (its 4 MiB strings no longer stop it), and `big.lua` gets past its old blocker; no file regressed.
- **Review:** one round. No blocker; three majors and three minors fixed, one minor and the host lookups recorded (ADR 0052).

Phase 3.29 stabilization (the review's fixes): **passed** on this machine (2026-10-01). Snapshot schema 19, GC policy 12; bytecode 11, tables 3, fuel 6, and binary chunk format 2 unchanged.
- **Exact heap authority:** the quota is on `GcState::used`, the exact sum of every object's logical size, kept at every allocation, growth, compaction, shrink, and free; collector estimates only schedule. Tests check it against a recount at every collection's end in every runtime test, and at every step of the torture programs. A compaction during a sweep making survivors old, the review's 64 KiB overshoot, now leaves the heap exact, and filling to the quota never passes it. A compaction forgives no collector work: with 20,000 dead slots compacted mid-sweep the sweep takes the steps it takes without one, and no emergency follows.
- **Snapshot security:** one generational validator (`gc::check_gen_invariant`) for every phase: between collections, a young collection's start, atomic steps, sweep and correction, a sweep making survivors old, and the finalizer positions. Weak references count. Restore counts the logical heap from the objects; it never reads it. The review's tampered images are refused; seeded random tampering at a point in every phase (five seeds, hundreds of images each) never restores into a stale handle, a broken invariant, or an inexact count. The tampering found five gaps first, all closed (ADR 0051).
- **`collectgarbage`:** `step` follows Lua's `genstep` by state: young or major (returning false even after a bad major, which it returned true for), the whole fallback cycle while falling back (`stepgenfull`), and a major running finished. A new corpus, `corpus_genstep.lua`, checks each state against Lua 5.4.9. `generational` tests the mode Lua chose, never the collector's machinery: with the call in every round of a ring program, no full collection is asked for except while falling back, where Lua's collector is incremental. `incremental` mid-major normalizes.
- **Minor scalability:** young collections look only at finalizable objects registered since the one before (Lua's `finobjold1`): 462 units each against 9,388 with 9,000 old finalizable tables. No collection walks every thread to measure what it holds: a thread is charged for its stack and builtin bytes, and measured again when a collection traces it.
- **Remembered sets:** one bit per slot keeps `again` duplicate-free whatever holds the slot. A host write during a young collection's sweep cannot list an object twice.
- **Also found:** restore refused a legitimate checkpoint taken while an empty ephemeron table was being scanned (a Phase 3.28 check), and hand-built test heaps reused a freed prototype's handle; both fixed.
- **Performance:** interpreter and allocation rows at parity or faster; a first build that inlined the barrier's new slow path ran calls 10–12% slower, fixed by moving it out of line. See PERFORMANCE.md.
- **The second review** (one round, a separate reviewer) found one blocker, fixed: library work out of its frame (`gsub` building its result) lost its charge to an emergency collection near the quota, leaving the count about 1.2 KB low and the next checkpoint refused; the thread now keeps its charge while the work is out. Every other claim held, the tamper fuzz at six more seeds included.
- **Gates:** 403 library tests (14 new review regressions), the workspace, the 12 oracle tests, the wasm32 roundtrip, clippy in both feature sets, fmt; the official suite unchanged (4 files pass, `gengc.lua` to `OK`).

Phase 3.29 (generational collection): **passed** on this machine (2026-10-01). Snapshot schema 18 (ages, young and remembered lists, the generational schedule and state), GC policy 11 (generational mode, the default); bytecode 11, tables 3, fuel 6, and binary chunk format 2 unchanged.
- **Gate 0:** the oracle test of the library corpora gives the heavy table corpus its own 240 s CPU limit (PUC Lua takes 10 s of it on a quiet machine, up to 70 s loaded); all 12 oracle tests pass.
- **Ages and the barrier** (ADR 0051): one age byte per object beside its mark, Lua's ages but `OLD0`, since the one barrier is on the parent. `Arena::get_mut` grays an old object about to change and makes it touched; `get_mut_storing` lets a write that stores no reference skip it between collections, as Lua's barrier checks for a collectable value. Negative controls: with the barrier off, a young table, closure, table, and userdata given to an old table, userdata, closed upvalue, and thread stack are freed by the next young collection.
- **Young collections** trace young objects, touched ones, and `OLD1` and `TOUCHED2` ones, and run whole before Lua goes on, in units charged to fuel. On 3,000 random heaps changed between five young collections each, a young collection decides exactly what the Phase 3.27 collector decides with every old object as a root: live ids, table contents, the finalizer queue in order, registrations. A full collection afterwards matches it with no roots added, and leaves every object old. With 8,000 old tables, a young collection takes under 1,000 units.
- **Major collections** are bounded incremental cycles that leave and re-enter generational form. Bad majors fall back on incremental cycles and return when growth stops, as Lua's. Full and emergency collections make survivors old. `collectgarbage("generational")` and `("incremental")` switch both ways, `step` is a young collection (a major when debt is due), and Lua 5.4.9 prints the same for a 243-line corpus in either mode it starts in, as does Moonseed booted in either.
- **Determinism and snapshots:** output, fuel, and the collector's event hash are the same under quanta 1, 2, 3, 7, and 1,000, and with a restore at every step taken in every phase of young and major collections. The generational invariant holds at every point Lua runs. The GC fingerprint, now with the generational corpus, matches native and wasm32. A generational snapshot is no larger than an incremental one: marks and ages are written only where they differ from a default. Restore refuses impossible ages, lists, flags, and states breaking either invariant.
- **Accounting:** what threads hold beyond their objects (stacks, builtins' buffers) is measured again at each young collection, compacted tables and shrunk payloads give their bytes back, and a sweep to old counts survivors exactly. The estimate is exact after young collections and never below the heap.
- **Performance:** with 300,000 old entries and young churn the collector's time falls 12-fold against incremental (8.8 against 116.5 ms) and the run's by 58%. In Phase 3.28's worst case (a 300,000-entry table being built) the largest slice falls from 906 to 384 µs and full cycles from 47 to 13. Pure churn with a tiny heap costs the same collector time within 10–20%, but in larger slices. Barriers are within noise in every mode. The default mode leaves interpreter rows unchanged and allocation rows 5–8% slower.
- **The official suite:** 4 files pass, as before. `gengc.lua` runs to its end (`OK`, its `T`-only checks skipped). `gc.lua` passes its mode checks and stops at line 471, a 4 MiB string past Moonseed's limit; scaled under the limits it runs to its end with no failed assertion.
- **The review** (one round) found no blockers. Two majors, fixed: a table compacted, or a payload shrunk, during a sweep making survivors old left the estimate 10% below the heap and let a fill pass the quota; restore accepted images tampered inside a young collection or a sweep to old. Minors fixed: a give-back stalled a major's sweep; `step(0)` could run a major where Lua's never does; `generational` during a major asked for a full collection; a host write between a young collection's units left a stale remembered entry. Accepted: young collections walk every registered finalizable object and every thread's frames, bounded by the object limit.

See ADR 0051.

Phase 3.28 (the incremental collector): **passed** on this machine (2026-10-01). Snapshot schema 17 (the collector section), fuel revision 6 (collector work: a unit of fuel per four units of work, paid in advance and carried over), GC policy 10 (incremental cycles, steps, and their pacing); bytecode 11, tables 3, and binary chunk format 2 unchanged.
- **Gate 0:** the Phase 3.27 collector is kept as `gc_reference.rs`, an oracle for tests and measurements. On 3,000 random heaps (tables, closures and upvalues, suspended threads, userdata, type metatables, weak and ephemeron tables, finalizable objects, cycles) a cycle done in random slices decides exactly as it does: live ids, table contents, the finalizer queue in order, registrations, and an exact live estimate. On 3,000 more, changed between slices, it frees nothing the final heap reaches, finalizes nothing it reaches, clears no weak entry it reaches strongly, and settles where the reference does. A full collection asked for at any point of a cycle matches it too.
- **The collector** (ADR 0050): a state machine that is snapshot state (pause, begin, propagate, a nine-step resumable atomic phase in Lua's order, sweep); dense mark bytes with two whites; new objects live for their cycle; tables, stacks, and prototypes traced from a stored position.
- **One barrier authority:** `Arena::get_mut`, the only mutable access to a heap object, regrays a black object about to change; the atomic phase grays every root again; host calls by id or into Lua finish an atomic phase first. Negative controls: with the barrier off, a table value, a table key, a user value, a closed upvalue, and a stack slot written into a traced object are freed while reachable; without the atomic re-gray, a host root, the globals, and a type metatable set during a cycle are freed.
- **Fuel and determinism:** a step does exactly its units, a piece begun is finished, an atomic phase runs to its end; output, fuel, and the collector's event hash are the same under quanta 1, 2, 3, 7, and 1,000, with a restore at every step taken in every phase, and native against wasm32. Four schedule-dependent decisions were found by these tests and fixed (ADR 0050).
- **`collectgarbage`:** `incremental`, `setpause`, `setstepmul`, `step` with and without a size, `stop`, `restart`, `isrunning`, `count`, `collect`, as `lua_gc` encodes them; a corpus matches Lua 5.4.9. `generational` is Phase 3.29's.
- **Performance:** a full collection costs 0.75–1.45× the stop-the-world collector's; median slices are a default step of about 800 units (5–90 µs); the largest are atomic phases re-tracing what changed while the cycle marked, as long as a stop-the-world collection of the heap or longer on write-heavy programs; a quantum bounds what the host sees. Barriers: within placement noise when no cycle runs, +5–8% on table and global writes while marking.
- **The official suite:** 4 files pass, as before. `gc.lua` stops at `collectgarbage("generational")` (line 15) instead of `"incremental"` (line 12); with that stubbed and three loops scaled under the string and object limits, it runs to its end with no failed assertion (line 201's step counts now hold). A string key in a table weak both ways now goes with its entry, as in Lua; the earlier collector kept it a collection longer.
- **The review** (one round) found no blockers. Two majors, fixed: the host could call a closure or resume a thread by id while paused in an atomic phase, freeing the thread or leaving a weak table first reached in the last step uncleared, so a checkpoint failed to restore; restore accepted gray objects in no list, an unreachable full-collection target, and an overflowing schedule. Minors: the live estimate counted objects traced again or made while marking twice (fixed: 1.74× at worst before, under 1.2× now); a value waiting on its ephemeron key is not withdrawn when its entry changes (kept one cycle longer, accepted); a few slot walks are not charged (bounded by the slot count, accepted).

See ADR 0050.

Phase 3.27 (weak tables, ephemerons, finalizers, and warnings): **passed** on this machine (2026-10-01). Snapshot schema 16 (the finalization lists and flags, finalizer frames, `collectgarbage`'s frame waiting for them), fuel revision 5 (starting a finalizer costs a unit, as a call does), GC policy 9 (weak tables, ephemerons, finalization, and its pacing); bytecode 11, tables 3, and binary chunk format 2 unchanged.
- **Gate 0:** a `gmatch` iterator keeps Lua's three upvalues, the third a userdata standing for its state. The table-lookup layout regression of Phase 3.26 was left for Phase 3.36 (now 3.37).
- **Warnings** (ADR 0049): `warn` and a host warning sink, each warning one journaled effect, pieces with Lua's continuation flag.
- **Weak tables** (ADR 0046): `__mode` read at each collection; strings, numbers, light userdata, and builtins never cleared; weak-key tables are ephemerons, settled in linear time after the rest of the graph; clearing goes through the table's own delete, so `next` survives it.
- **Finalizers** (ADR 0047, ADR 0048): registered only when a metatable with `__gc` is set, through one authority; run newest registration first, `__gc` looked up at its turn; objects resurrected for their finalizer and freed a collection later; weak values cleared before finalizers, weak keys after; errors as warnings; no yields; emergency collections only inside them; run one per step as resumable calls, waits and checkpoints included; `Runtime::begin_close` for shutdown.
- **Evidence:** a 70-line corpus matches Lua 5.4.9, warnings included, and keeps its output and fuel under small quanta and with a restore at every step; tests for weak references across a snapshot, host waits in finalizers, quota pressure, ten thousand finalizers, long ephemeron chains, closing order before Rust `Drop`, refused images, and replayed warnings; direct reachability tests in the collector; native and wasm32 agree on the GC fingerprint.
- **Performance:** a full collection costs about 8 ns per strong entry, 13 per weak-value entry, 14 per ephemeron entry; a finalizer about 200 ns; ordinary rows did not move beyond placement.
- **The official suite:** `closure.lua` passes (4 files pass). `gc.lua` and `gengc.lua` stop at `collectgarbage` modes; with them stubbed, both run to their end, `gc.lua` with one failure (incremental step counts) and three loops scaled under the string and object limits.
- **The review** (one round) found six blockers, all fixed: finalizers that register again kept `collectgarbage` waiting and could take every step; no collection at all inside a finalizer; closing allocated per finalizer and failed near the object limit; three snapshot states wrongly refused or accepted. Also fixed: finalizer code ran outside the fast tier; zero bytes in warnings; `gmatch`'s room check. See ADR 0048.

See ADR 0046–0049.

Phase 3.26 (full userdata, light userdata, and host objects): **passed** on this machine (2026-10-01). Snapshot schema 15 (full userdata, light userdata values and keys), GC policy 8 (userdata bytes, user values, and host charges count); bytecode revision 11, tables 3, fuel 4, and binary chunk format 2 unchanged.
- **Gate 0, the value model:** `Value::Userdata(Handle<UserdataObj>)`, an object with an `ObjectId`, its own metatable, fixed user values, and a byte or host payload (ADR 0042); `Value::LightUserdata(domain, bits)`, an identity token that is no object (ADR 0043). `Value` stays 16 bytes.
- **Lua semantics:** `type` is `userdata` for both; every metamethod event works on full userdata through the one metatable lookup; `__eq` only between distinct full userdata, never for light ones; light userdata share one metatable; both are table keys; `getmetatable` honours `__metatable` and `setmetatable` stays table-only; `debug.getuservalue`, `setuservalue`, and `upvalueid` follow Lua 5.4.9.
- **Host objects** (ADR 0044): types registered by symbol and `TypeId`, values owned by their userdata, typed borrows tied to the native call or a host closure (a second borrow does not compile; natives cannot call Lua), argument errors naming the type, methods through ordinary `__index` tables, logical charges declared and reported. Rust `Drop` is not `__gc`.
- **Snapshots** (ADR 0045): byte payloads exactly, portable host values through their codec, and a refusal (`NonPortableUserdata`) for host values without one; restore treats codec bytes as untrusted and is transactional.
- **Evidence:** a 156-line corpus matches Lua 5.4.9 driven through its C API (`tools/lua54_userdata_harness.c`), and keeps its output and fuel under small quanta and with a collection, checkpoint, and restore at every step; tests for user-value rooting, host methods and borrows, portable and refused snapshots, the object limit and heap quota, crafted and tampered images, and unreported growth; native and wasm32 agree, and a runtime holding every kind of userdata gives byte-identical snapshots on both and finishes the same from either.
- **Performance:** making a userdata about 200 ns, a typed host borrow about 20 ns on top of a native call, light keys as fast as string keys. In the default build ordinary rows are equal or faster; in the aligned one-unit layout table hits read 5–10% slower, traced to the table index's trait-object probe no longer being devirtualized (PERFORMANCE.md).
- **The official suite:** `goto.lua` passes (it stopped at `upvalueid`); 3 files pass. Nothing else moved: the suite makes userdata only through PUC's test library and `io`.
- **The review** (one round) found one blocker: natives could make userdata past `Config::max_objects`; they now count against it, and restore refuses a heap past it. Also fixed: a host value grown without a reported charge snapshotted and then failed to restore (the snapshot now refuses it, and `with_userdata_mut` recounts the charge); the host could hand a VM-made light token to another runtime (only host keys come in); three missing-argument errors; a shrink's debt. Recorded: a `gmatch` iterator has two values where Lua's has three upvalues.

See ADR 0042–0045.

Phase 3.25 (the coroutine library): **passed** on this machine (2026-10-01). Snapshot schema 14, bytecode revision 11, tables 3, fuel 4, and binary chunk format 2 unchanged; GC policy 7 (every slot of a thread's stack counts against the heap quota).
- **Gate 0, the thread model:** Lua's four statuses map onto the states threads already had (ADR 0041's table); nothing new is stored. A coroutine never resumed has no frames and holds its body in slot 0; a resumer's frame stays on its call, and what it receives is read from that call.
- **The eight functions** give Lua 5.4.9's results and messages: exact counts and nil holes both ways, `false` and the error from a failed coroutine whose stack stays for `debug`, `normal` status, `isyieldable` in every state, `close` with `<close>` values that cannot yield, and `wrap` functions that close a failed coroutine and prefix the caller's position. A builtin can be a coroutine's body.
- **Transfers at the stack bound**, the gap left since Phase 3.16, now fail as Lua's resume fails, "too many arguments to resume" or "too many results to resume", with nothing moved and the coroutine as it was.
- **Resume chains** stop at Lua's 196 coroutines with "C stack overflow"; none of it grows the Rust stack.
- **Snapshots:** restore checks the thread graph: one acyclic chain of resumers from the active thread to the entry thread, nothing else running, each resumer waiting in a call that resumes or closes.
- **Debug:** the frames an error unwind is leaving are hidden while their `__close` runs, as Lua has popped them.
- **Evidence:** a 122-line corpus matches Lua 5.4.9 and keeps its output and fuel under small quanta and with a collection, checkpoint, and restore at every step; host waits inside coroutines restore exactly once; fuel covers every thread; native and wasm32 agree.
- **Performance:** a resume and yield take about 600 ns, a `wrap` call and yield 440 ns, against 125 ns for a direct Lua call; a suspended coroutine adds about 170 snapshot bytes. Ordinary rows did not move in the one-unit, aligned layout.
- **The official suite:** no file stops on `coroutine`. `nextvar.lua` and `strings.lua` go on to `io`, `calls.lua` to a compile-error message, and `coroutine.lua` to `debug.sethook`; with hooks stubbed it runs to its end with two failures outside the library (a hook trace and a weak table).
- **The review** (one round) found one blocker: thread stacks were not charged to the heap quota, which ADR 0025 had left for when source could make threads; a thousand coroutines held 638 MB against a 64 MiB quota. Every stack slot now counts (GC policy 7), the quota is checked where stacks grow, and a test fills the quota with coroutine stacks. Also fixed: a crafted snapshot of a `wrap` function's close with no error restored and then hit a host error; a finished coroutine still linked to its resumer and a suspended one not in `coroutine.yield` restored; another thread's pending builtin reached through `pcall` or a metamethod was missing from its debug levels. Recorded: a coroutine with a builtin body costs a trampoline prototype and closure of its own.

See ADR 0041.

Phase 3.24 (registry, `package`, `require`, debug information, and the `debug` library without hooks): **passed** on this machine (2026-09-30). Snapshot schema 14 (the registry, package and traceback tasks, prototypes' debug information and chunk names, a frame's tail-call mark, new error classes), bytecode revision 11, tables 3, fuel 4, GC policy 6 (debug information, a `require` task's message, and a traceback's text count against the heap), binary chunk format 2.
- **Gate 0:** the base, math, table, and string libraries raise argument errors through one authority, with Lua's `bad argument #n to 'name' (...)` wording; `math` and `table` now use Lua's messages. Numerals of any length, decimal and hex, read as Lua reads them: a corpus of 157 lengths up to a megabyte matches Lua 5.4.9, through `tonumber` and `load`.
- **The registry** is an ordinary table, rooted and snapshotted, with the main thread at 1, the globals at 2, and `_LOADED` and `_PRELOAD`, which are `package.loaded` and `package.preload`. Every installer registers its table there, in any order. The VM's own state stays typed; nothing it relies on is a registry field.
- **`require`** follows Lua 5.4.9's protocol step for step, as a resumable machine: no searcher or loader runs twice across a checkpoint. At this milestone only preload was installed; the host resolver followed in 3.31, filesystem Lua/searchpath in 3.37. Paths are now host-configurable.
- **Debug information:** chunk names, lines, locals with live ranges, upvalue names, and call-site names, charged to the heap, in snapshots and in binary chunks (stripped as Lua strips). Lines follow Lua's attribution rules, and `\r`, `\r\n`, and `\n\r` count as line breaks as in Lua.
- **The `debug` library:** `getregistry`, `getmetatable`, `setmetatable`, `getinfo`, `getlocal`, `setlocal`, `getupvalue`, `setupvalue`, `upvaluejoin`, `traceback`, on levels that map Lua's, suspended and failed coroutines included. Hooks, `upvalueid`, `getuservalue`, `setuservalue`, `debug.debug`, and `setcstacklimit` are absent, not stubbed. No standard installer includes `debug`: it breaks sandboxes.
- **Evidence:** two corpora (180 and 51 lines) give Lua 5.4.9's output, under small quanta, and the package one with a collection, checkpoint, and restore at every step; a traceback restores mid-search; restore refuses debug states the runtime cannot make; native and wasm32 agree.
- **Performance:** no change in the dispatch loop or the benchmarks' ordinary rows. Debug information costs about 10 logical bytes an instruction; dumping and loading binary chunks got about 60% slower, compiling source 4%.
- **The official suite:** no file stops on `require`. None of the 13 `require "debug"` files passes; their blockers are now the coroutine library (`calls`, `coroutine`), hooks (`db`), `upvalueid` (`goto`), compile-error wording (`constructs`, `errors`, `literals`), `io`/`os` (`events`, `files`), `collectgarbage` modes (`gc`, `gengc`), the heap quota (`big`), and a suite module (`locals`). Across the suite the frontier is `io`/`os`/`arg` (8 files), the coroutine library (4), error wording (4, `math` included), the suite's own modules (3), and the rest one or two each.
- **The review** (one round) found one blocker: local and upvalue names over 256 bytes were cut when written, so a checkpoint or a dump changed them and the heap's count. Names are now kept whole. It also found call and operator lines in multi-line expressions that differed from Lua (fixed: a call is on its arguments' opening line, an operator's instruction on the operator's, a constructor on its `{`), a traceback's text not checked against the heap quota (fixed), and `load`'s room check not counting a new chunk name (fixed). Recorded, not fixed: `activelines` still differ where Lua folds constants or loads operands late; `debug.getinfo(f, 'L')` and the level walk are not charged to fuel by size; and four library differences in `LUA_COMPATIBILITY.md`. No host error or panic was found under a `setlocal` fuzz of every register kind.

See ADR 0039 and ADR 0040.

Phase 3.23 (`string`): **passed** on this machine (2026-09-30). Snapshot schema 13 (a metatable per basic type, native closures, string tasks, new error classes), bytecode revision 11, tables 3, fuel 4 (a string function's step does bounded work; string arithmetic runs through metamethod calls), GC policy 5 (string buffers count against the heap; native closures have a logical size), and a new binary chunk format, revision 1.
- **Type metatables:** every basic type can have one metatable, found through one lookup (`Heap::metatable_of`); the string library sets the string one, `__index = string`, so `("x"):upper()` works. The core no longer converts strings in arithmetic: the string metatable's metamethods do, as in Lua 5.4.9, so a changed `__add` is seen.
- **The 17 functions**, with Lua's error wording, the C locale, and the 1 MiB string limit checked before results are made. Patterns run on an explicit, resumable machine (200 frames, Lua's bound); `format` is pure Rust with glibc's output; `pack` uses one ABI on every target; `gmatch` returns a native closure, a new function value with state; `string.dump` writes a Moonseed binary chunk, which `load` validates before installing.
- **Evidence:** five fixtures under every schedule and four generated corpora (7,890 lines) give Lua 5.4.9's output. The engines alone matched Lua 5.4.9 on 80,324 pattern, 64,820 format, and about 253,000 pack cases. Native and wasm32 agree, dumped chunks and packed floats included.
- **Performance:** the pattern machine costs four to six times PUC's recursive C per step; shortcuts that change no result make literal-start searches and `gsub` faster than PUC's. The regression check found three slowdowns in shared paths, fixed before the commit.
- **The review:** found a native closure unequal to itself, a `gsub` replacement that could grow without bound before the limit was checked, and changed snapshots that ended in a host error. All fixed with proofs; one low item (capture copies not charged to the step budget) is recorded.
- **The official suite:** `tpack.lua` and `bwcoercion.lua` pass, the first files to. All nine `string` blockers cleared, and every failure reached in the string library was investigated. The new frontier is `require` (17, 13 for `debug`), `package` (2), `io`/`os`/`arg` (5), `utf8` and `coroutine` (1 each), and the table and math libraries' argument-error wording (`sort`, `math`).

See ADR 0034–0038.

Phase 3.22 (`math` and `table`): **passed** on this machine (2026-09-29). Snapshot schema 12 (library state, library tasks in base-function frames, reserved `integer` and `float`, four error classes), bytecode revision 11, tables 3, fuel 3 (a math or table function's step runs up to 32 operations; each after the first costs a unit), GC policy 4 (`table.concat`'s text counts against the heap).
- **`math`:** Lua 5.4.9's whole library over the portable `libm` crate, which `^` now uses too. Transcendental results are bit-identical native and on wasm32 over 2,000 inputs per function, and within 1 ulp of glibc's.
- **`math.random`:** Lua 5.4.9's xoshiro256** sequences from explicit seeds. Its state is snapshot state, seeded from `Config::entropy` or from journaled host entropy, never the clock.
- **`table`:** its seven functions are state machines over semantic operations. They follow every metamethod, pause and checkpoint between steps, and allow no yield, as in Lua 5.4.9. `table.sort` is PUC's quicksort, with deterministic pivots.
- **`#`:** no longer scans the table, so appending costs 0.8 µs instead of 43 µs at 5,000 elements.
- **Evidence:** fixtures and corpora of 3,063 math calls, 1,506 lines of random outputs, and 568 table cases match Lua 5.4.9.
- **The review:** found functions the VM implements calling each other on the Rust stack, a thousand levels in one unit of fuel. That pattern was there since `__tostring = tostring` in Phase 3.21. Such calls now run in their own step.
- **Tests:** compiled at `opt-level = 1`. The library tests take 18 s instead of 290 s, since the per-step checkpoint walks are quadratic.
- **The official suite:** clears all eight `math` and `table` blockers. `vararg.lua` and `heavy.lua` run to their end, and the new frontier is `require` (16), `string` (9), and `io`/`os`/`arg` (5).

See ADR 0032 and ADR 0033.

Phase 3.21 (the base library): **passed** on this machine (2026-09-23). Snapshot schema 11 (reserved type-name strings, five error classes, base-function frames, and a message handler's target), bytecode revision 11, tables 3, fuel 2 (a base-function frame's step after a call costs one unit), GC policy 3 (a `load` reader's source counts against the heap). `print`, `tostring`, `tonumber`, `type`, `assert`, `next`, `pairs`, `ipairs`, `collectgarbage`, text `load` with reader functions, `_G`, and `_VERSION` are installed by `Runtime::install_base`; `install_base_only` installs part of it. A base function that calls Lua pushes a boundary frame, as `pcall` does, so `__tostring`, `__pairs`, `__index`, and a reader can wait, pause, and be checkpointed; a coroutine may yield across `pairs` only, as in Lua 5.4.9. `print` writes to a host output, one journaled effect per write, in Lua's order. Six base fixtures give Lua 5.4.9's output under every schedule, 32,000 `tonumber` conversions match it, and native and wasm32 agree. The official suite now gets 27 of 33 files past their Phase 3.20 blocker; none passes, and the new frontier is `require` (16, 13 of them for `debug`), `math` (6), `string` (3), `table` (2), and `io`/`os` (3). See ADR 0031.

Phase 3.20 (`<const>`, the language-core audit, and the official Lua 5.4.9 suite baseline): **passed** on this machine (2026-09-23). Snapshot schema 10, bytecode revision 11, tables 3, fuel 1, GC policy 2, all unchanged. `<const>` locals are read-only through the compiler's one assignment test and compile to ordinary locals. `LUA_LANGUAGE_AUDIT.md` classifies every production of the Lua 5.4 grammar as supported, one with a documented deviation (constructor order for repeated keys), and lists every limit against PUC Lua's. The pinned official suite (SHA-256 `7d971845…c343fca`) runs through `moonseed-compat`: all 33 files compile, none passes, each stopping at a missing library. The Lua 5.4 source-language core is implemented, with those deviations; Moonseed does not claim Lua 5.4 compatibility.

Phase 3.19 (labels and `goto`, and the compiler's register bound): **passed** on this machine (2026-09-23). Snapshot schema 10, bytecode revision 11, tables 3, fuel 1, GC policy 2, all unchanged: labels and gotos exist only in the compiler, and a goto compiles to a `Jump`, or the scope-exit close its left locals need and a `Jump`, decided when the function's code is complete. Visibility, duplicates, the rule against entering a local's scope, and Lua's trailing-label rule match Lua 5.4.9. `break` now decides its close the same way. No register at or past 250 is ever named; every overflow is a `Limit` error. See ADR 0030.

Phase 3.18 (`and`, `or`, `not`, method calls, call shorthands, `function` statements, `local function`): **passed** on this machine (2026-09-23). Snapshot schema 10, bytecode revision 11, tables 3, fuel 1, GC policy 2, all unchanged: every form compiles to instructions that already existed, and none adds runtime state. Truth has one definition, `Value::truthy`. Method calls evaluate the receiver once and copy it before the lookup, as Lua does. `goto`, labels, and `<const>` are the syntax left.

Phase 3.17 (proper tail calls): **passed** on this machine (2026-09-23). Snapshot schema 10 (unchanged), bytecode revision 11 (`TailCall` 51, always followed by the `Return` of its open window), tables 3, fuel 1, GC policy 2. `return f(args)` outside every `<close>` scope replaces the caller's frame, so tail recursion runs in constant stack space; before, 10,000 tail hops hit the 1,000-frame limit. A tail call to a native becomes the call of the frame below, with no new continuation state. Restore now checks that every Lua frame answers the call of the frame below it. See ADR 0029.

Phase 3.16 (varargs and the stack bound): **passed** on this machine (2026-09-23). Snapshot schema 10 (a frame keeps only its extra-argument count; the stack bound), bytecode revision 10 (`Vararg` reads extras below the registers, only in vararg prototypes), tables 3, fuel 1, GC policy 2. A vararg frame's extras now sit below its registers, where no call can overwrite them; under the old layout a callee of four registers cleared them. Chunks are vararg. `select` is a base function. `Config::max_stack_slots` bounds every thread's stack, and snapshots accept exactly that bound. See ADR 0028.

Phase 3.15 (generic `for`: the four-value initializer, the iterator call, per-iteration variables, and the closing value): **passed** on this machine (2026-09-23). Snapshot schema 9 (unchanged), bytecode revision 9 (`GenericForLoop` 50), tables 3, fuel 1, GC policy 2. The iterator call is an ordinary `Call`, so the loop adds no continuation or snapshot state. `pairs`, `ipairs`, and `next` are not provided. See ADR 0027.

Phase 3.14 (`<close>` and `__close`, resumable scope closing, coroutine failure and close): **passed** on this machine (2026-09-23). Snapshot schema 9, bytecode revision 8 (`MarkClose` 47, `CloseScope` 48, `CloseThread` 49), tables 3, fuel 1, GC policy 2. A failed coroutine now keeps its stack until it is closed. The default logical-heap quota is 64 MiB, provisional. See ADR 0026.

Phase 3.13 (Lua errors, stackless unwinding, `error` / `pcall` / `xpcall`, and a logical-heap quota): **passed** on this machine (2026-09-23). Snapshot schema 8, bytecode revision 7 (unchanged: the three functions are base natives, not opcodes), tables 3, fuel 1, GC policy 2 (collection near the quota, ADR 0025). Memory errors and stack overflow are catchable. An External native's fault is a Lua error. Restore bounds every stack slot index. See ADR 0024 and ADR 0025.

Phase 3.12 (`__call`, the arithmetic, bitwise, comparison, and concatenation operators and their metamethods, `rawequal`): **passed** on this machine (2026-09-23). Snapshot schema 7, bytecode revision 7, tables 3, fuel 1, GC policy 1. `__len` now receives its operand twice, as in Lua 5.4. See ADR 0023.

Phase 3.11 (stabilization: constant strings, automatic collection, a common dispatch tier): constants and automatic collection **passed** on this machine (2026-09-22). Dispatch is **partly passed**: the common handlers' machine code no longer changes when opcodes are added, but some multi-function paths still vary by 15–60% with code placement alone (ADR 0022). Snapshot schema 6, bytecode revision 6, tables 3, fuel 1, GC policy 1. See ADR 0020, ADR 0021, ADR 0022.

Phase 3.10 (table metatables and resumable `__index` / `__newindex` / `__len`): **passed** on this machine (2026-09-22). Snapshot schema 5, bytecode revision 6, tables 3, fuel 1. See ADR 0018 and ADR 0019.

Phase 3.9 (native functions as first-class values): **passed** on this machine (2026-09-22). Snapshot schema 4, bytecode revision 5, tables 2, fuel 1. See ADR 0017 and the new `PROJECT_GOALS.md`.

Phase 3.8 (table constructors, indexing, indexed assignment, globals, `_ENV`): **passed** on this machine (2026-09-22). Snapshot schema 3, bytecode revision 5 (`Index` 38, `SetIndex` 39, `GetField` 40, `SetField` 41, `SetList` 42), tables revision 2, fuel 1. Table hits run in the hot tier. See ADR 0016.

Phase 3.7 (numeric `for`, unary minus): **passed** on this machine (2026-09-22). Snapshot schema 3, bytecode revision 4 (`Neg` 35, `ForPrep` 36, `ForLoop` 37), tables 1, fuel 1. `ForLoop` runs in the hot tier. See ADR 0015.

Phase 3.6 (comparisons, `do`, `elseif`, `while`, `repeat`, `break`): **passed** on this machine (2026-09-22). Snapshot schema 3, bytecode revision 3 (`Compare` is tag 34), tables 1, fuel 1. Two-number comparisons run in the hot tier. See ADR 0014.

Phase 2C (dispatch stability and restored-bytecode validation): **passed** on this machine (2026-09-22). The hot tier (ADR 0013) recovers the Phase 2B regression and beats the pre-2B build on every measured loop. Six added cold opcodes leave the hot loops unchanged within noise. Restored prototypes now pass the same bytecode check as compiled code. Snapshot schema 3, bytecode revision 2, tables 1, fuel 1, all unchanged. See `PERFORMANCE.md`. The milestone numbering in chat called Phase 3.5 "2B"; this file keeps its own numbers.

Phase 3.5, Gates A–C (closing captured locals on scope exit, `if`/`else` from source, and the fuel, checkpoint, GC, and wasm matrix): **passed** on this machine (2026-09-22). Snapshot schema 3, bytecode revision 2, tables revision 1, fuel revision 1. `CloseUpvalues` is tag 32 and `JumpIfFalse` is tag 33. See ADR 0012. The two new `Op` variants slowed dispatch-heavy loops by about 20–35% through an inlining change; see `PERFORMANCE.md`.

The categories below are not interchangeable.

## Implemented and unit-tested

Native `cargo test --workspace` (the wasm roundtrip is ignored in that command and run separately).

- Canonical hand-built program: cycle, shared upvalue, nested calls, one host `mark`, a yielded thread kept alive from table `A`
- Uninterrupted, quanta 1/2/3/7, and a checkpoint at every safe point (including `Prepared`) produce the same observation
- Collection at those safe points does too
- `Waiting` snapshots, restores, and completes once; a second complete and a wrong key fail; fuel slices do not clear the wait
- Torn journal returns the stored outcome `99` instead of allocating a second effect
- A VM restored from after `mark`, onto an empty journal, does not call `mark` again
- Effect-domain mismatch returns `SnapshotError::EffectDomainMismatch` and leaves the source runtime runnable
- Malformed snapshots return a specific error. Structural cases are re-encoded with a valid CRC. `u32::MAX` string count returns `LimitExceeded`
- Unreachable cycles, a released root, a stale handle, a retired generation, an abandoned self-referential thread, and an open upvalue that keeps its thread
- Host yield is `LuaYielded`; fuel pause is not
- Hard fuel limit stays terminal
- `Value` is 16 bytes, `Handle` is 8, `Frame` is 112, no `unsafe` in `moonseed`
- Fixed and open result windows, including an interior nil, zero results, padding, and truncation
- Parenthesized calls are a fixed one-result call. `select('#', ...)` in the fixture is `VarargLen` / `OpenLen`, not a `select` library
- Parallel assignment records addresses before stores, stores right to left, and restores from the pre-store cursor and from between stores
- Lua 5.4.9 ran the two source fixtures when `MOONSEED_LUA54` pointed at a locally built `lua`. That binary is not a dependency and the test is ignored in CI
- Arithmetic, branch, call, field, alloc, host, and quantum-1 loops. Numbers are in `PERFORMANCE.md`
- `next` in insertion order, including after the current key is deleted, across fuel 1, checkpoints, and collection
- Dead anchors do not root object keys or string objects. Equal string bytes still find the anchor
- Raw length is the smallest Lua border: empty 0, sequence 3, hole `{[1]=10,[3]=30}` returns 1, missing key 1 returns 0
- Lua 5.4.9 checked the sequence lengths exactly. Holed tables were checked with the border predicate. `MOONSEED_LUA54` is still ignored in CI
- Malformed anchors (bad ordinal, duplicate key, live nil, NaN, illegal object id, absurd dead count, bad dead tag) fail closed with a valid CRC
- Lua 5.4 lexical surface on bytes, with spans: numerals (including `3..4` as one malformed number), short and long strings, `\u{...}` below `2^31`, comments, and operators
- Source subset: `local`, name assignment, `return`, calls, `function` expressions, parentheses, and integer `+`. The closure fixture returns `1, 1, 2, 2`
- That fixture matches a hand-bytecode program under quanta 1/2/3/7, a checkpoint at every safe point, and collection at every safe point. Two closures of one local share one upvalue id
- Compiling the same source twice yields the same prototype. A compile error does not build a runtime. A broken prototype fails validation
- Free names, operators other than `+`, and statements such as `while` are `Unsupported` or `Syntax` with a span
- `if` / `else` from source. `nil` and `false` are false. `0` and `""` are true. A call condition uses its first result
- Leaving a block closes only the block's captured locals (`CloseUpvalues`), on fallthrough and before the jump over `else`. A block without a capture emits no close. A branch ending in `return` relies on `Return`'s close
- `crates/moonseed/fixtures/lua/branch_close.lua` returns `10, 11, 11, 99` with the branch local's register reused by the later `local x`. `branch_threshold.lua` returns `3, 2`: the inner capture closed, the outer one stayed open. Both match Lua 5.4.9 and hold under quanta 1/2/3/7, a checkpoint at every safe point, and collection at every safe point. The first also matches a hand-bytecode program
- Replacing the close with a no-op, in the hand program or the compiled one, changes the result. The comparator catches it
- A snapshot before the close restores open cells, and one after it restores the closed cell. Both closures still share one cell after restore
- A table held only by a closed cell survives collection after its register is cleared. The cell is collected once no closure holds it
- A snapshot that declares bytecode revision 1 is `BadVersion`
- Restored prototypes pass the compiler's bytecode check. Snapshots with a valid CRC and valid graph but bad code fail with `InvalidBytecode`: a register, jump (forward and backward), child index, upvalue index, call window, return window, `CloseUpvalues` threshold, `JumpIfFalse` operand, or constant out of range, and a child capture outside its parent. Unreachable code after `Return` is checked too, and the source runtime still runs. An unknown opcode tag is `InvalidTag`
- Comparisons: numbers by value across integers and floats, exact above 2^53 and at the `i64` limits; NaN unequal and unordered; strings by bytes for equality and order; nil, booleans, and identity types by value or identity. Ordering other types is `LuaFault::Compare`. The Lua 5.4.9 oracle matches all 1,200 `<`, `<=`, `==` results over 20 edge values, including negatives, `mininteger`, ±2^63, infinities, and NaN
- `do_scope`, `elseif_chain`, `while_capture`, `break_block`, `break_nested`, `repeat_capture`, `repeat_scope`, and `compare` fixtures match Lua 5.4.9, and hold under quanta 1/2/3/7, a checkpoint at every safe point, and collection at every safe point
- Captured loop locals are distinct per iteration. The `repeat` condition sees body locals. `break` closes crossed captures and exits only the innermost loop. Later locals reuse the closed registers; removing the closes changes the results
- `while true do end` pauses at every quantum and ends in `FuelLimitExceeded` at the hard limit
- A loop that makes two closures per iteration, collected at every safe point, never holds more than 4 closures and 3 upvalues
- `break` outside a loop, or inside a function nested in a loop, is a `Syntax` error
- The native and wasmi results of the `while`, `repeat`, and `break` fixtures match, each checkpointed halfway
- Restored `Compare` operands are validated; an unknown comparison kind is `InvalidTag`
- Numeric `for`: `for_basic`, `for_scope`, `for_capture`, `for_float`, `for_strings`, `for_bounds`, `for_eval_once`, and `neg` fixtures match Lua 5.4.9, and hold under quanta 1/2/3/7, a checkpoint at every safe point, and collection at every safe point. They cover default, explicit, and negative steps, zero iterations, fractional limits in integer mode, float mode from either `init` or `step`, numeric strings, `math.maxinteger` / `math.mininteger` bounds without wrap, `math.mininteger` as a step, clamped and skipped out-of-range limits, 2^53 edges, a control-variable assignment that does not steer the loop, per-iteration captured variables, and one evaluation of each control expression
- Zero step (integer, float, `-0.0`, computed) is `ForZeroStep`; non-number values are `ForValue`; Lua raises the matching errors
- `string_to_number` matches Lua's `tonumber` on 25 strings, bit for bit
- Huge integer loops and a float loop that never advances pause at every quantum and stop at the hard fuel limit
- A `for` loop making closures, collected at every safe point, never holds more than 4 closures and 2 upvalues. Removing the closes changes the captured result
- A `ForLoop` over registers that are not a numeric-for state is `VmError::Corrupt`. Restored `ForPrep` / `ForLoop` / `Neg` operands and jumps are validated
- The native and wasmi results of `for_capture` and `for_bounds`, checkpointed halfway, match
- `table_ctor`, `table_index`, `assign_index`, `globals`, `env_shadow`, `env_init`, `env_param`, and `table_keys` fixtures match Lua 5.4.9 and hold under quanta 1/2/3/7, a checkpoint at every safe point (including mid-constructor and between recorded destinations and their stores), and collection at every safe point
- `i, t[i] = 2, 99` records its destinations with `AssignField` and commits; restored from the point where both destinations are recorded and nothing is stored, after a collection, it still writes `t[1]`
- Indexing or assigning into a non-table is `LuaFault::Index`; a nil key write is `NilKey`, in assignments and constructors; nil and NaN key reads are nil. `{}[1]`, `1()`, and `x.y, 1 = ...` are syntax errors, as in Lua
- The chunk's one capture is `_ENV`; after running, the runtime's globals table holds the globals, and still does after restore. A prototype with two chunk captures fails validation
- `t[2^63]` and `t[math.maxinteger]` are distinct keys; `next` returns an integer for a float-written integral key
- Restored `Index` / `SetIndex` / `GetField` / `SetField` / `SetList` operands and constants are validated
- The native and wasmi results of the constructor, indexing, assignment, globals, and `_ENV` fixtures, checkpointed halfway, match
- Native functions from Rust are Lua values: bound as globals, stored in locals, upvalues, tables, and table keys, compared by symbol, and called with ordinary `Call`. `native_calls` and `native_results` match Lua 5.4.9 (with Lua definitions of the same functions) and hold under quanta 1/2/3/7, a checkpoint at every safe point, and collection at every safe point. Zero, one, and several results, nil holes, parenthesized and open results, and constructors all go through the ordinary result windows
- Calling a non-function is `LuaFault::BadCall`, no longer VM corruption; a native `Fault` is `LuaFault::Native`
- A `VmLocal` native takes no effect id and writes no journal entry. An `External` one stops prepared with an effect id; restored from there it commits once onto an empty journal, returns the stored outcome from a torn journal, and does not run again when restored past the call
- A native wait restores and completes once, with a wrong key rejected, a second completion rejected, fuel charged once, and the same key reusable by the next wait
- Restore fails closed for a symbol the registry lacks, a duplicate symbol, a native index past the symbol table, and a native pending state with the wrong policy
- `tests/embed.rs` exposes a Rust function using only the public API, and restores it by symbol
- The native and wasmi results of the native fixtures, checkpointed halfway and rebound by symbol, match
- `meta_index`, `meta_newindex`, `meta_len`, `meta_protect`, and `meta_mutate` match Lua 5.4.9 and hold under quanta 1/2/3/7, a checkpoint at every safe point (inside metamethods, and between a metamethod's return and the instruction's commit), and collection at every safe point. They cover table and function `__index` / `__newindex`, chains, primitive hits bypassing metamethods, results adjusted to one, `__len` and raw fallback, protected and shared metatables, and a metatable changed after use
- `i, u[i] = 2, 99` with a Lua `__newindex` calls it exactly once, for key 1, under every checkpoint schedule
- A native `__index`, `__len`, and `__newindex` that wait restore from the wait and from between completion and commit, and finish once
- A `__index` / `__newindex` cycle faults with `MetaChain` after 2000 steps; protected `setmetatable`, bad arguments, `#` on a number, a table `__len`, and a faulting metamethod each fault with the expected class, and every faulted runtime still snapshots and restores
- A table and its metatable, and self-metatable cycles, are collected when unreferenced; a kept table keeps its metatable
- Two tables sharing a metatable still share one metatable object after restore
- A metamethod continuation with the wrong argument count, the wrong event for its instruction, or a scratch slot inside the frame fails to restore
- Metamethod lookup is raw: a metatable's own `__index` is not consulted when looking for `__index`
- The native and wasmi results of the metatable fixtures, checkpointed halfway and rebound by symbol, match
- Constant strings: `t.foo` into `__index`, `t.foo = i` inserts and deletes, and `local x = 'foo'` allocate the same number of objects at 10 and 1,000 iterations. The 10,000-iteration `t.foo` fixture allocates nothing per iteration
- Constant and run-time strings with the same bytes are equal and index the same key. A restored prototype keeps its constant strings. A constant that names anything but a string fails to restore
- Garbage loops of tables, closures, and metamethod results finish under an object limit of 300 by collecting automatically, and fail with `MemoryLimit` with automatic collection off. A loop that keeps everything still reaches `MemoryLimit`
- Automatic collections run at the same fuel points uninterrupted, at quanta 1, 2, 3, and 7, and when restored from snapshots every 1, 13, or 97 slices
- Every control, table, native, and metatable fixture matches Lua with a collection after almost every allocation, under quanta and a checkpoint at every step
- The collection state, including the automatic flag, survives restore. A zero threshold, another GC policy revision, or two constants naming one string fails to restore
- Calling `run` again while a native waits, before `complete_wait`, does not move any collection
- Metamethod calls in a loop no longer grow the stack. Each call used to leave `top` one slot higher, which kept every result reachable
- `meta_unary`, `meta_call`, `arith`, `bitwise`, `meta_arith`, `meta_compare`, and `concat` match Lua 5.4.9 (built without `LUA_COMPAT_5_3`) and hold under quanta 1/2/3/7, a checkpoint at every safe point, collection at every safe point, and eager automatic collection. They cover the operand-twice rule for `__unm`, `__bnot`, `__len`; `__call` argument order, zero arguments, several results, a three-deep callable chain, and a callable `__add`; a callable table as `__index` still indexed; primitive edge values; numeric strings; first-then-second metamethod selection for every event; Lua 5.4 `__eq` rules; `__lt` / `__le` truth; `rawequal`; number text and `__concat` order
- Fault classes for integer division by zero, arithmetic, bitwise, and concatenation on the wrong types, a float with no integer value, ordering mixed types, `<=` with only `__lt`, a non-callable value or metamethod, and a `__call` cycle; the same programs error in Lua 5.4.9
- A `__call` chain of 200 steps runs; 201 faults with `CallChain`
- Runaway recursion, an `__eq` that compares two tables, and an `__index` that indexes its own table raise `StackOverflow` at 1,000 frames instead of exhausting host memory (catchable since Phase 3.13); recursion one frame short of the bound finishes and restores from its deepest point
- A `..` result over 1 MiB is a memory error (since Phase 3.13 a catchable one, checked before copying); a string of exactly 1 MiB is made
- Float `%` with a negative divisor or an infinite operand matches Lua 5.4.9 (`-1 % -2.5` is `-1.0`). A 5,000-case differential run of every operator over edge numbers and numeric strings agrees with Lua, apart from PUC's `-0.0 - 0` constant-folding artifact
- A native `__add`, `__eq`, `__lt`, `__concat`, `__unm`, `__call`, and a callable `__add` whose `__call` is native, each waiting for the host, restore from the wait and from between completion and commit, and finish once. A wrong key and a second completion are refused
- A metamethod continuation with a wrong argument count, a truth event on an arithmetic instruction, a flipped negation, a wrong destination, or a slot inside the frame fails to restore
- The hot dispatch tier gives the same fuel as Phase 2B in all 24 benchmark workloads that report it, and every schedule test above runs through it
- `pcall`, `xpcall`, and `error` fixtures, including errors from Lua and native metamethods, error objects that are tables, nil, false, integers, and strings, stack overflow, and a handler that fails, match Lua 5.4.9. They also match under every checkpoint schedule, the deep-recursion fixture under a sparse one
- A coroutine yields inside `pcall` and resumes inside it (hand bytecode). A message handler cannot yield
- An `xpcall` message handler sees the failing frames still on the stack
- Unwinding closes upvalues: the canonical fixture sees closed values, and with closing disabled by a test switch the same fixture fails, so the test would notice a missing close
- Memory errors from the quota are catchable, skip the message handler, and leave the heap usable after `pcall` returns. Legal-sized strings, and closures and upvalues alone, cannot pass the quota
- An External native's fault is a Lua error, not `Completed`. A waiting native completes with values or with an error object
- An unprotected error fails the thread, keeps its object for the host, and survives a snapshot. A snapshot taken between two pops of an unwind restores and finishes the same
- `pcall` does not catch VM corruption
- Restore refuses impossible error states, boundaries, and handler states (ADR 0024), and ten kinds of out-of-range slot index at every step of two programs. `snapshot()` refuses a stack or table restore would refuse
- `<close>` fixtures (reverse order, nil and false, `break`, backedge, `repeat`, `return` and returning the closed local, error objects passed to closes, close errors, a non-closable value, a non-callable `__close`, `xpcall` with close errors, a replaced or removed `__close`, a callable `__close`, captures) match Lua 5.4.9 under every quantum, a checkpoint at every step, and a collection at every safe point
- Eight coroutine programs (a close that yields at a scope exit, in a return, and in a protected unwind; a failed coroutine closed later; closing a suspended one; a close error, a yield, and an inner `pcall` during a close) give Lua 5.4.9's results for the same programs written with the coroutine library, under the same schedules
- A failed coroutine keeps its frames and its pending value, survives a snapshot, and is closed later
- A native `__close`, an External one committed to the journal once, and one waiting on the host at a scope exit, in an unwind, and in a return restore from the wait and finish once
- A memory error closes its pending values, one of which fails the quota again; a stack overflow with a `<close>` in each of 998 frames closes all of them
- A closed value is collectable once its scope is left
- `<close>` locals are read-only, directly, from nested functions, and in multiple assignment; two per list, `<const>`, and unknown attributes are refused
- Restore refuses out-of-frame, repeated, and misordered to-be-closed slots, a `Close` that does not match its `CloseScope` or `Return`, an unwind close with no error or aimed past the nearest `pcall`, a `closing` flag without a waiting closer, and a failed non-coroutine with frames
- From the Phase 3.14 review:
  - a thread close whose `__close` waits on the host restores from the wait;
  - a waiting close the host fails is not caught by the coroutine's own `pcall`;
  - a coroutine that fails while resuming another is closed later under every checkpoint schedule;
  - a close that overflows the reserve gives "error in error handling", and a `pcall` inside a close works in the reserve, as in Lua 5.4.9;
  - closing a normal coroutine gives Lua's message;
  - three more impossible close states fail to restore
- From the milestone review: a vararg function keeps its varargs across `pcall` of a Lua function or a native; table stores collect before a memory error, and stores and `rawset` after a caught memory error succeed; a completion naming no object is a host error that leaves the wait; a failed wait leaves no wait behind; a ready thread still holding a wait, an unwind that skips the nearest `pcall`, and aliased reserved strings fail to restore; a global the quota refuses is `MemoryLimit`, not `Corrupt`

- Generic `for` fixtures (`gfor_basic`, `gfor_init`, `gfor_capture`, `gfor_close`, `gfor_error`, `gfor_callable`) match Lua 5.4.9 under quanta 1/2/3/7, a checkpoint at every step, collection at every safe point, and eager automatic collection. They cover one to three variables, nil holes, extra and missing results, nil ending the loop and false continuing it, an empty loop, a loop variable assigned in the body, the initializer's scope, the four-value adjustment with extra, missing, and multi-result expressions evaluated once in order, captured variables per iteration, the closing value on exhaustion, `break`, nested `break`, `return`, and errors, close order with body `<close>` locals, nil and false closing values, a non-closable one refused before the first call, a replaced `__close`, iterator and body errors, a close error, `xpcall`, and Lua, native, `__call`, chained `__call`, and `pcall` iterators
- An iterator that yields inside a coroutine at every call, with a closing value that yields too, gives Lua 5.4.9's results under every schedule (hand bytecode)
- A waiting iterator, one completed with an error, and a closing value waiting at the loop's end, after `break`, in a `return`, and in an unwind, restore at the wait and after the completion, finish once, use the same fuel, and do not call the iterator again after its final nil
- Every fixture uses the same fuel under a checkpoint at every step
- Without the body's `CloseUpvalues`, two iterations' closures share one cell and return the wrong values
- Restore and the compiler's check refuse a `GenericForLoop` with zero or open results, the wrong argument count or call slot, no call before it, a forward branch, a branch to itself, to the call, or out of the code, a base past the registers, or no instruction before it
- A garbage-making iterator runs 20,000 iterations under a 256 KiB quota, and under an object limit of 300; the same loop keeping its tables reaches the quota, and `pcall` catches it
- A loop's state and closing value are collected once later locals reuse their registers
- An iterator that never returns nil stops at the fuel limit; a recursive iterator with a closing value in each loop overflows the stack, closes every value, and is caught; an endless `__call` chain faults
- Loops of 10 and 5,000 iterations grow the stack and frame storage the same number of times
- Too many loop variables for the registers is a compile `Limit` error
- `vararg_basic` and `vararg_frames` match Lua 5.4.9 under quanta 1/2/3/7, a checkpoint at every step, collection at every safe point, and eager automatic collection. They cover fewer, exact, and more arguments than parameters, `...` in single-value, parenthesized, middle, and last positions, returns, call arguments, constructors with holes, multiple assignment and conditions, a generic `for` over `...`, recursion, `select` (count, `'#x'`, negative, numeric-string and float indices, and five error cases), extras surviving Lua, native, and metamethod calls, `pcall`, `xpcall`, a metamethod that raises, and a `<close>` before `return ...`, and captured fixed parameters against copied extras
- The Phase 3.15 layout's overwrite, reproduced first as a hand-built test (a callee of four registers cleared its caller's extras), passes with callees of 1 to 200 registers under every quantum and checkpoint
- `...` in a function that is not vararg, including a nested one, is a compile error at the `...`; five malformed parameter lists are syntax errors; chunks are vararg
- The count of extras is exact frame state: 0, 1, 2, and 4 with nils, through a checkpoint
- A yield with extras live and a yielding `__close` before `return ...` give Lua 5.4.9's results under every schedule (hand bytecode)
- A native wait inside a vararg frame, `return ...` through a waiting `__close`, and extras passed to a waiting native restore at the wait and after completion, finish once, and use the same fuel
- A table held only by `...` survives collections during a call and a wait; after the frame returns, and for extra arguments to a function without `...`, collection frees them
- A 1 MiB string passed three times, and 200 tables as extras
- A chunk given `10, nil, 30, "s"` through `Runtime::load_chunk_with_args` returns them, restored before it runs too; too many arguments is `StackLimit` (public API test)
- Stack growth by extra arguments and by recursion through 150-register functions stops at a 4,000-slot bound with "stack overflow", and every state on the way checkpoints; a completion with more values than the stack holds fails the call with "stack overflow"
- The stack bound is clamped at boot, kept by snapshots, and refused out of range or smaller than a thread's stack; `Vararg` in a non-vararg prototype, extras that do not fit below a frame's base, or extras off the stack fail to restore
- From the milestone review: an unwind's close call, a message handler's call, an External native's results, and an assignment's `__newindex` call could return `Err(StackLimit)` to the host, and growth other than frames could use the eighth kept for error handling; each reproduction now ends in a catchable Lua error, with the handler's room kept. A `__close` recursing during an ordinary error's unwind gives "stack overflow", as in Lua 5.4.9, instead of "error in error handling"
- An unwind that stops to run a frame's closes starts them at that frame's registers; before, each close of an overflowing recursion started where the previous one had left `top`, and the stack crept upward
- `tail_basic`, `tail_pcall`, and `tail_deep` match Lua 5.4.9: result counts and nil holes through a tail call in every result context, fixed and vararg callers and callees, captured locals, `__call`, native callees, the close order when a `<close>` keeps the call ordinary, `pcall`, `xpcall`, and errors from tail callees, and 100,000 hops of self, vararg, mutual, `__call`, and native-ending tail recursion. The first two run under quanta 1/2/3/7, a checkpoint at every step, and collection at every safe point; `tail_deep` under sparse checkpoints and eager collection
- The Phase 3.16 failure, reproduced first (`return f(n - 1, acc + 1)` 10,000 deep overflowed), completes
- Compiled code has a `TailCall` exactly for `return` of one unparenthesized call outside every `<close>` scope, a generic `for`'s included; nine eligible and twelve ineligible forms are checked by opcode, and a `<close>` of an enclosing function does not stop a nested function's tail call
- Seven tail recursions of 100,000 hops, one after 990 ordinary calls, run under the 1,000-frame limit, stepped one instruction at a time: the most frames, stack slots, and live objects seen do not change between 1,000 and 100,000 hops, and the fuel equals an uninterrupted run's
- `return (f())`, `return f(), 1`, `return 0 + f()`, and a call before the return still overflow and are caught; a recursion with a `<close>` in every frame overflows, and every marked value is closed
- A tail call closes the caller's captured locals first; with that step skipped, the test reads the callee's register instead
- A callee in the frame's first register, and a vararg frame's extras copied up and tail-called back down over themselves, with nil holes and 200 arguments, move intact under every quantum and checkpoint (patched bytecode)
- Tail calls to natives: VM-local, `pcall` and `error`, a `__call` cycle, from a metamethod, and inside `pcall`; a waiting native from a vararg frame, from a metamethod, and from a thread's first frame, and one completed with an error, restore at the wait and after the completion and use the same fuel; while the native waits, the erased frame is gone
- An External native tail-called stops before it runs with the caller already gone, and commits one journal effect across two restores from that point
- An `xpcall` message handler for an error from a tail callee sees four frames, not five; a coroutine failing in `g` after `f` tail-called it keeps the body's frame and `g`'s only, and its `CloseThread` gives Lua 5.4.9's results
- Hand-built code tail-calling with a value still to close fails with `VmError::Corrupt`, and a snapshot of it at the `TailCall` is refused
- Restore refuses a `TailCall` outside the registers, with an argument window past them, or without its `Return`; a replacement frame whose extras or result count do not match its caller's call; a caller moved off its `Call`; a native tail call waiting above a thread's first frame; and a first frame whose native call lost its window
- One tail hop costs one instruction where a call and its return cost two
- Nothing of the erased frame stays a root: its locals, arguments a fixed callee drops, and a first frame's registers around its native call; a tail recursion allocating a table per hop runs 100,000 hops under a 256 KiB quota
- A chunk given arguments tail-calls with them and returns the callee's results through `Runtime::results`, restored before and during the run
- From the milestone review: restore still required the frame below to hold as many native arguments as its own `Call` passed, so a native tail-called with fewer (`f(1, 2, 3)` doing `return park()`, any generic-`for` iterator doing `return park()`, an External native at its prepared stop) was checkpointed but refused on restore. Restore now requires only the callee below `top`, as the runtime reads it; each reproduction is a test. The review ran 46 programs against Lua 5.4.9 and under a checkpoint at every step and found nothing else
- `const_locals` matches Lua 5.4.9 under quanta 1/2/3/7, a checkpoint at every step, and collection at every safe point: several `<const>` per list, nil for missing initializers, the initializer seeing the outer name, captures at two depths, table contents changed through a const local, shadowing, and a goto over a const local to a trailing label
- Assigning a `<const>` local directly, in a list, by a `function` statement, or from a nested function one or two levels down is a `Syntax` error, as are two attributes on one name, an unknown attribute, attributes on parameters or loop variables, and a goto into a const local's scope; a const local compiles to exactly the prototype of an ordinary one
- 34 `<const>` programs and a 105-program grammar corpus give Lua 5.4.9's acceptance and results, apart from the constructor order for a repeated key and three programs that read library globals
- Every frontend limit holds at its exact boundary, one past it being a `Limit` error: string literal 65,536 bytes, numeral 1,024 characters, 98 nested parentheses, 39 nested function expressions, 200 locals, 200 upvalues, 200 functions directly inside one, 256 functions in a chunk, 4,096 constants, 10,000 instructions
- Chains of 1,000 and 100,000 links of `+`, `or`, `and`, `==`, `..`, `.a`, `[1]`, `()`, and `:m()`, and chains nested in parentheses, are `Limit` errors on a 2 MiB thread stack
- The official Lua 5.4.9 suite, fetched and checked by hash on every run, compiles in all 33 files; each run stops at a missing library, recorded with the files that name each global (`tests/compat/lua54-baseline.json`); a tampered archive is refused
- From the milestone review: a long left-associative chain (8,000 `+` terms on a 2 MiB stack) overflowed the Rust stack while compiling; more than 256 functions in a chunk gave `InvalidProgram` instead of `Limit`; the harness recorded the pinned hash whatever it ran on, and the script checked it only on the first fetch; and the audit overstated what the harness's classification shows and had four wrong figures. All fixed; the review found no `<const>` escape in about 90 programs and no grammar difference in about 250
- `goto_basic` and `goto_close` match Lua 5.4.9 under quanta 1/2/3/7, a checkpoint at every step, and collection at every safe point: forward, backward, and outward gotos, `continue` in every loop kind, trailing labels, a fresh captured local per backward pass, nested-scope close order, a `__close` error during a goto, a generic `for`'s closing value closed by a goto that leaves the loop and not by one that stays in its body, and `break` after a capture that a backward goto ran first
- 47 hand-written programs, legal and illegal, give Lua 5.4.9's acceptance and results; of 9,000 generated programs mixing blocks, loops, labels, gotos, locals, captures, and `<close>`, 5,450 are refused by both and the rest agree, except 9 runaway loops that reach Moonseed's heap or string limit before its fuel limit and time out like Lua under less fuel
- Illegal gotos are `Syntax` errors naming the label, and for a scope error the local: no visible label, a label in a nested block or another function, a duplicate visible label, and jumps into the scope of a plain, `<close>`, loop, or `local function` local, including before `until`
- A goto compiles to one `Jump` when it leaves nothing to close, and to the scope-exit close and a `Jump` otherwise, decided after every capture is known; the same source compiles to the same prototype
- A `__close` that waits because a goto leaves its scope restores at the wait and after the completion, closes once, and the goto lands once, with the same fuel
- `::a:: goto a` runs one instruction at a time, restores in the middle, and stops at the fuel limit with a handful of objects; a goto loop allocating on every pass runs 50,000 passes under a 256 KiB quota
- Every consecutive-register form, at its largest size and one past it, stays within 250 registers and runs, or is refused with `Limit`; without the new checks an argument list reached register 251
- From the milestone review: `break` chose its close where it was compiled, so a capture after it in the source, run first through a backward goto, kept its upvalue open on a reused register (`100` where Lua gives `1`). `break` now uses the goto's deferred slots; the review's reproductions are in `goto_basic`. The review ran 24,000 more generated programs and 170 under a checkpoint at every step and found nothing else
- `logic_ops` and `methods` match Lua 5.4.9 under quanta 1/2/3/7, a checkpoint at every step, and collection at every safe point: operand results of `and` and `or`, short-circuit side effects, `not` on every kind of value, precedence, one value from a call on either side, a receiver evaluated once, `__index` tables and functions and `__call` for methods, `f{...}`, `f"..."`, and `f[[...]]` arguments, dotted and `:` function statements with `__index` on the prefix and `__newindex` on the store, `local function` recursion and shadowing, and variadic methods. `methods_deep` runs 100,000 hops of method and `local function` tail recursion under the 1,000-frame limit
- A receiver made by a counted function is made once, under every quantum and checkpoint; a wait in the receiver, in a method lookup through a waiting `__index`, and in a function statement's waiting `__newindex` restores at the wait and after the completion with the same fuel
- `park() or f()` and `park() and f()` evaluate the right side only when the restored left side does not decide, under every answer; a skipped right side costs no fuel however long it is
- `and`, `or`, and `not` compile to `Move`, `JumpIfFalse`, `Jump`, and `LoadBool` only, and consult no metamethod
- `local function f` captures its own local, and `local f = function` does not; `return self:m(...)` and a `local function`'s `return f(...)` are tail calls, and not inside a `<close>` scope
- A 200-call method chain, 200-operand `and` and `or` chains, a 50-part function name, and 50 nested local functions stay within 12 registers
- A `(` on the next line continues the expression, as in Lua; seven malformed method and function names are syntax errors
- `Value::truthy` is the one definition of Lua truth: nil and false are false; 0, -0.0, NaN, strings, tables, and functions are true
- From the milestone review: a method call on a receiver held in a temporary other than the call's first register (`t[i + 1]:m()`, `(a + 1):m()`) failed to compile, and a local receiver was copied to `self` after the lookup, so an `__index` that reassigned it changed `self`. The receiver is now copied first, from wherever it is held, as Lua's `SELF` does; both reproductions are in the `methods` fixture. The review ran the other claims against Lua 5.4.9 and under every schedule and found nothing else
- From the Phase 3.15 review: a `GenericForLoop` that branches onto its own iterator call was accepted and is now refused; 2,900 generated programs mixing generic `for` with `break`, `return`, `<close>`, `pcall`, and captures matched Lua 5.4.9

## Wasm-executed

`cargo build -p moonseed-wasm-probe --release --target wasm32-unknown-unknown`, then `cargo test -p moonseed-wasm-probe --release -- --ignored wasm_roundtrip` with `MOONSEED_WASM_PROBE` set.

- Artifact: `moonseed_wasm_probe.wasm`, 547 KiB, release
- Native fresh snapshot restored inside wasmi, run to completion, fields match
- Wasm fresh snapshot restored natively, run to completion, observation matches
- Not a browser. The JS integer/string boundary is not implemented
- The table-semantics fingerprint matches between native and wasmi, including a checkpoint after the current key is deleted
- The source-closure fingerprint matches between native and wasmi, including a checkpoint halfway through the compiled fixture. Source spans are not in the snapshot
- The branch-close fingerprint matches between native and wasmi, with the checkpoint taken just after `CloseUpvalues`
- The automatic-collection schedule of four garbage loops (tables, closures, metamethod results, concatenated strings), stepped one instruction at a time and restored partway, matches between native and wasmi
- The operator fixtures checkpointed halfway, three programs waiting in a native operator handler or `__call` and restored while waiting, and a ten-case `^` corpus match between native and wasmi, floats by their bits
- The six error fixtures checkpointed halfway, and three protected calls waiting in a native (one inside a message handler) and restored while waiting, match between native and wasmi
- The four `<close>` fixtures and the nine coroutine close programs checkpointed halfway, and closes waiting in a native at a scope exit, in an unwind, and in a return, restored while waiting, match between native and wasmi
- The six generic `for` fixtures and the yielding-iterator program checkpointed halfway, and loops whose iterator or closing value waits in a native, restored while waiting, match between native and wasmi
- The two vararg fixtures, the overwrite regression and the vararg coroutine program checkpointed halfway, vararg frames waiting in a native restored while waiting, and a chunk given arguments checkpointed partway match between native and wasmi
- The `goto_basic` and `goto_close` fixtures checkpointed halfway, a `__close` waiting because a goto leaves its scope, and a wait inside a backward goto loop, restored while waiting, match between native and wasmi
- The `logic_ops` and `methods` fixtures checkpointed halfway, and waits inside a short-circuit, a method call's argument, and at the end of a `local function`'s and a method's 1,000-hop tail recursion, restored while waiting, match between native and wasmi
- The `tail_basic` and `tail_pcall` fixtures checkpointed halfway, and tail calls to a waiting native from a vararg frame, a metamethod, a thread's first frame, and the end of a 5,000-hop tail recursion inside `pcall`, restored while waiting, match between native and wasmi
- The userdata corpus and host-object programs checkpointed halfway, and waits holding userdata restored while waiting, match between native and wasmi (`source_userdata_fingerprint`); a runtime waiting with a portable host value, byte payloads, user values, light keys, and an `upvalueid` key gives byte-identical snapshots on both targets, and each target's snapshot finishes the same on the other
- The string fixtures checkpointed halfway, `string.dump` bytes, packed floats (`<d >d f n j`), 200 formatted numbers (`%.17g %a %.3e %g`), and waits in a `gsub` replacement, a `%s` conversion, a `gmatch` loop, and the string metatable's `__index`, restored while waiting, match between native and wasmi byte for byte (`source_string_fingerprint`)

## Benchmarked

`cargo bench -p moonseed --bench kernel --features measure`

- Profile: `bench` (optimized)
- Host: x86_64, AMD Ryzen 9 7945HX, rustc 1.98.1 (`48a229cea`, 2026-09-01)
- `Value` 16 bytes, `Handle<u8>` 8 bytes, align 8
- Checked handle lookup 20_767_380 ns / 20_480_000 lookups ≈ 1 ns; raw slot read 13_505_678 ns; ratio 1.54
- 1k integer keys: insert ≈ 166 ns, get ≈ 11 ns
- Canonical program: 60 charged instructions, ≈ 5.7 µs/run including allocation, ≈ 95 ns/charged instruction
- Finished heap: 1532 snapshot bytes, 31 live objects, ≈ 49 bytes/object; encode ≈ 8.5 µs, decode ≈ 14 µs

Checked lookup is not an order of magnitude slower than a raw index on this machine. The ~95 ns figure is the canonical program, allocation included. Phase 1.5 remeasured dispatch on an arithmetic loop at about 14 ns per charged instruction, and a warmed call does not allocate. The closure fixture lexes in about 1.2 µs, compiles in about 4.1 µs, and runs in about 761 ns against 702 ns for the hand program (24 instructions and fuel 32, versus 22 and 29). Those numbers are in `PERFORMANCE.md`. This is not a comparison with PUC Lua.

## Not claimed

Lua 5.4 compatibility beyond the subset in `LUA_COMPATIBILITY.md`, D2, the sign and payload of NaN across targets, browser execution, authentication of snapshots, a non-rollback security quota, and interning. `next` and raw length follow the policies in ADR 0010, not full Lua table iteration. The hand-bytecode `Resume` instruction raises a failed coroutine's error in the resumer without closing it; the coroutine library follows Lua. Tables and full userdata have their own metatables, and every other type has one shared metatable. Host userdata come from the embedder; IO creates internal file userdata. A host value's Rust `Drop` is not `__gc`. Finalizer timing is Moonseed's deterministic rule. Strings use the C locale; `%p` prints deterministic tokens, not addresses. Frontend limits are not a sandbox profile. Restore drops source spans, but retains serialized debug information.

## Deviations from the master plan worth remembering

- Private design notes are not part of the public tree.
- One crate plus a wasm probe, not the eleven-crate sketch.
- Strings are not interned. Equality is still by bytes.
- Host functions are registry symbols, not first-class values, until a host closure has serializable captures.
- `Resume` carries a result count. The canonical program requests one value. An open resume is representable; the fixture does not exercise a multi-value yield.
- The VM remembers only the last completed wait key, so a reused key is treated as already completed. Wait keys must be unique for the VM lifetime until that is replaced.
- Snapshot checksum is CRC-32, not BLAKE3. It is not authentication.
