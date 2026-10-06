# ADR 0013 — A hot dispatch tier over canonical state

## Context

The interpreter was one function: `run` looped over `poll`, and `poll` called `exec`, a match over every opcode. LLVM inlined all of it into `run`, and which helpers it pulled in depended on the size of the whole match. Adding two opcodes in Phase 2B changed that choice and slowed dispatch-heavy loops by 20–35%, although those loops never ran the new opcodes. Every register operation also looked up the active thread and its frame again through the arenas. The numbers are in `PERFORMANCE.md`.

## Decision

Dispatch has two tiers over one set of semantics.

Compare-and-branch (ADR 0055) adds one hot arm for same-type numeric operands
on the shared comparison helpers. Other types decline without changing PC or
registers; `exec` shares Compare's cold helper and VM-owned Truth continuation.
The fused op costs one instruction; its branch is canonical before the next
instruction boundary. Gate Q checks Full, NoFastCalls and Off at tiny quanta.

`run_hot` is a separate, non-inlined function. It reads the frame base, `pc`, the prototype's code, fuel, and the quantum into locals, and executes instructions until one of these happens: the quantum or the fuel limit runs out, an instruction faults, or the thread reaches a state it does not handle. It first checks the same conditions `poll` checks: no trap, the thread `Ready`, no pending host call or assignment. It charges one fuel unit before each instruction, as `poll` does.

`hot_op` is the only implementation of the hot opcodes: `LoadNil`, `LoadInt`, `LoadFloat`, `LoadBool`, `Move`, `Add`, `Jump`, `JumpIfFalse`, and `JumpIfLt`. It reads and writes only the running frame's registers. The hot set is a match with a default arm, so a new opcode is cold unless someone adds it there.

Every other opcode is charged, the locals are written back to the frame, and `exec` runs it against canonical state. `exec` is `#[inline(never)]`. Then `run_hot` rebuilds its locals, because the instruction may have pushed or popped a frame or switched threads. `poll` remains for everything `run_hot` hands back: pending host calls and assignment stores, the pause and termination outcomes, and non-`Ready` threads. When `poll` meets a hot opcode it runs it through `hot_op`.

At every exit from `run_hot` the frame's `pc`, the fuel counter, and the quantum are canonical. Nothing the VM needs to continue lives only in Rust locals at a safe point, a snapshot, a collection, or a host call.

## Alternatives

Keep one `match` and steer it with `#[inline]` hints. In Phase 2B the hints were ignored, and the result would still depend on the opcode count.

Tables of handler functions or boxed handlers. That makes every instruction an indirect call, and boxed handlers allocate.

Computed goto, assembly, or `unsafe` register access. The runtime is `#![forbid(unsafe_code)]`, and a native tier is a separate design.

A separate single-step interpreter for quantum 1. That would be two implementations of every opcode. Quantum 1 is the same loop with an allowance of 1.

## Consequences

Adding a cold opcode changes `exec` and leaves `run_hot` the same size. Six unused opcodes left the hot workloads unchanged within noise. Programs made mostly of cold opcodes still move a little, about 7% for the closure fixture, because those opcodes all run in `exec`.

Moving table access, upvalue access, or calls into the hot tier would need `hot_op` to reach arenas other than the thread's stack, or a frame change inside the loop. Each is a later measured step, and each must keep the write-back rule.

`#[inline(never)]` is a hint. If a compiler ignored it, the tiers would still be correct. Only the size isolation would be lost.

## Phase 3.32 compiler destinations

The compiler can now target the final uncaptured local with an existing `Add` or
`Arith`. Both hot and resumable paths already load operands before storing the
result. No dispatch, exit, or charging rule changes: removing a result copy removes
one instruction and its safe point. Newly compiled programs are checked under
tiny fuel quanta and checkpoint-every-step execution.

Constant-operand arithmetic (ADR 0054) adds one hot arm for integer immediates
with integer registers, using the same `int_op` and `hot_numbers` primitives as
register arithmetic. Mixed/coerced operands decline without writing a register or PC,
then share `op_arith` and its existing Store continuation. Gate Q compares
snapshots, fuel, results, GC and journals at each small-quantum boundary.

## Phase 3.32 fixed Lua calls (stage 4)

One epoch owns the active thread and fuel allowance; successful fixed Lua
Call/Return re-derives the frame's closure, proto, constants, upvalues, register
window and PC. The core checks the callee tag first: natives and non-closures
exit directly to `exec`, without constructing the frame-helper context.
Cold opcodes stay references to immutable code; frame operands are decoded in
`fast_frame_at`, outside instruction dispatch. The cold opcode copy precedes
fuel/quantum publication, with publication still before any execution, fault
or poll. This keeps Call's argument fields out of the hot-loop backedge.

`fast_call` and `fast_return` decline before mutation. Fixed arguments/results,
non-vararg callees, ordinary depth/slot quota checks, exact nil-fill and result
windows, PC advancement, stack high-water charging and growth counters match
the slow path. Returns requiring closes, open upvalues, boundary/meta/pending
continuations or result growth decline. FrameStep is separate from the one-byte
instruction Step. A charged-slot growth or due collector returns Resync: publish
and hand back to `poll`, preserving its GC scheduling point. Open windows,
TailCall, natives, metamethods and other precondition misses remain slow.

Boxing Pending reduces the native Frame from 120 to 72 bytes. Boxed and inline
variants were measured against the same baseline; the selected boxed version
has the better corpus sum and both Moss sizes. Allocation traces show no extra
Pending allocation in the measured Moss path; actual Some(Pending) construction
can allocate, and this result is not a general no-allocation claim. Snapshot
encoding and logical charges remain unchanged.

Coordinator acceptance is corpus sum improvement, each workload and Moss size
at most +1%, all gates and Gate Q. The selected variant passes performance:
corpus -5.449%, worst workload +0.919%, Moss +0.486%/+0.527%. Move is 31 Ir
(before 33); loop control is 62 Ir (before 62). Call+Return0 is 690 Ir; <=400
remains a goal. The 320 B core stack guard is waived; the 8 KB size guard remains.
No stage 5 was attempted. Full variants, proof limits, hashes and final gate
results are appended to `results/hotcore/stage-4/REPORT.md`.

## Phase 3.32 tail calls, open windows and Lua metamethods (stage 5)

The frame handoff also accepts open Call argument/result windows, open Returns,
and fixed-argument TailCall to a non-vararg Lua function. TailCall replaces the
canonical Frame in place, retaining its result mode and setting its tail flag;
affected open upvalues and closes decline before mutation. Stack copying,
clearing, truncation, high-water charging and Resync follow Entry::Replace.
Vararg callees, natives, callable resolution, reserves and result growth keep
their existing fallback paths.

Direct non-vararg Lua metamethod setup uses one thread borrow for scratch and
frame construction. MetaCall's event, slot, arguments, phase and the caller's PC
are unchanged. Lua Return can place the result into that canonical continuation
window. Its out-of-line frame helper completes plain Store, Truth (including
CompareBranch) and NewIndex continuations only with remaining allowance. A
return exhausting the quantum leaves MetaCall intact for poll's uncharged step.
Close and assignment continuations retain poll; callable chains retain their
argument-shifting resolver. This adds no check to the instruction backedge.

The first frame-window commit design regressed numeric dispatch and was rejected.
The selected return-helper design passes the full corpus and Moss +1% gates:
tail_recursion -29.583%, metamethods -24.173%, corpus sum -3.751%, worst workload
+0.030%. Gate Q and every-step restores cover all three modes, including yielding
metamethod debug fixtures. Fuel, bytecode and snapshot revisions are unchanged.
Measurements, rejected source, and final gates: `results/hotcore/stage-5/REPORT.md`.
