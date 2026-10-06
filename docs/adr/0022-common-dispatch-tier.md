# ADR 0022 — A common tier between the hot loop and rare opcodes

## Context

ADR 0013 made `run_hot` independent of the rest of the instruction set: adding opcodes no longer changed the hot loop's machine code. Everything else was one large `exec` match of about 18 KB. Adding six unused opcodes rewrote that function, and paths that go through it moved by 15–40%. Affected were native calls, metamethod calls, `#` with `__len`, and constructors.

## Decision

`exec` is a dispatcher of about 300 bytes. It sends each common cold operation to a handler of its own, `#[inline(never)]`:
- `call` and `do_return`;
- `op_index`, `op_get_field`, `op_set_index`, and `op_set_field`, covering table misses, inserts, and metamethod setup;
- `op_len`;
- `op_load_bytes`, `op_new_table`, and `op_set_list`;
- `op_make_closure`, `op_get_upvalue`, `op_set_upvalue`, and `op_close_upvalues`;
- `op_compare`, `op_neg`, and `op_for_prep`.

Every other opcode goes to `exec_rare`: hot opcodes re-run from `poll`, raw table access, coroutines, host calls, varargs, parallel assignment, and traversal. `exec_rare`'s match is still exhaustive, so a new opcode does not compile until some tier handles it.

Each opcode still has one implementation. The handlers are the old `exec` arms, moved. Fuel, effects, waits, and result windows are unchanged.

A new opcode goes into `exec_rare` unless it is common enough to earn a handler, and into `hot_op` only when measured.

## Evidence

`tools/cold_opcode_growth.py` adds six unused opcodes to `exec_rare` by default, or to the dispatcher with handlers of their own with `--common`. `tools/code_diff.sh` compares the builds' machine code with addresses masked:

- **Rare growth:** changes none of `exec`, `run_hot`, `run`, `call`, `do_return`, `call_meta`, `commit_meta`, the `op_*` handlers, or `index::get_name`. Before this change, the same growth changed 2,331 instruction lines of `exec`.
- **Common growth:** changes only the dispatcher's jump code, 22 lines.

The dependence of the common paths' machine code on unrelated opcodes is gone.

The timings do not follow the code as closely. Across nine builds, some rows move by 15–60% between builds whose code on the path is byte-identical:
- the two dispatch designs;
- with and without growth;
- with and without `-C llvm-args=-align-all-functions=6`.

Moving rows include a global or table-field native call, the metamethod rows, `#` with `__len`, constructors, and a plain field miss. They move in both directions. Building the old tree with only a different function alignment moves them as much. On this AMD Zen 4 machine the variation follows where code lands in memory, not what it is. Stable Rust gives no control over function placement, and `perf` counters are not available here to name the mechanism. Rows that dispatch one cold operation per iteration are stable at every layout within about 5%: native calls from a local, with zero or several results, and Lua calls. So are the hot loops.

## Alternatives

- **Keep one `exec` and tune `#[inline]` attributes.** In Phase 3.10 that made native calls slower and left them unstable.
- **Promote more operations into `run_hot`.** That is the answer for operations that need it, one at a time, when measured. It does not keep the cold paths stable.
- **Control layout with linker scripts or `-C llvm-args` in the workspace.** A downstream build would not inherit it, and the effect is not only function alignment.

## Consequences

Call and return pay one more call and return, about 1 ns. The common handlers are the unit to optimize next, for example by promoting `GetUpvalue` for `_ENV`, so that a global access is fully hot.

Performance comparisons of multi-function paths need several layouts, not one build against one other. `PERFORMANCE.md` records the method: the growth tool's variants serve as layout samples, and a difference smaller than the spread across layouts is not a result.

## Phase 3.32 fixed Lua calls (stage 4)

Eligible fixed Lua Call/Return use frame helpers within the core epoch. Natives
and non-closures decline by callee tag before helper setup. Every other decline
publishes the charged instruction's canonical PC/fuel/stack state and reaches
the existing `exec` dispatcher. Full/NoFastCalls/Off differential modes remain.
`exec` and all `op_*` bodies stay unchanged. Boxing Pending requires mechanical
continuation access changes in `poll`; generated frame offsets/copies differ
and are recorded in code_diff. `exec_rare` rejects the new instruction-only
Frame handoff on its existing corruption arm; its dispatcher never sends Call
or Return through that arm.

The coordinator's revised per-workload/Moss +1% gates and corpus-sum gate pass.
The selected boxed variant beats its matching inline variant on corpus sum and
both Moss sizes. All gate evidence, Gate Q and rejected variants are recorded
in the stage-4 report. Call+Return0 <=400 remains a goal; the 320 B stack guard
is waived. Stage 5 remains unimplemented.

## Phase 3.32 stage 5

Open-result calls/returns and eligible TailCall now use frame helpers. Direct Lua
metamethod setup specializes the existing call_meta path while retaining the
same suspended MetaCall. Return helpers complete plain continuations with
remaining allowance; exhausted quanta, close/assignment state, callable chains,
varargs, natives and reserve faults keep their cold handling. Full/NoFastCalls/Off
still compare canonical snapshots and fuel. Existing yielding debug fixtures
also restore every instruction in each mode.

Only call_meta's setup changes in the common tier. exec, exec_call, do_return,
tail_call, the op_* handlers and Runtime::run retain identical machine code in
the measured before/after builds. The complete corpus and both Moss sizes stay
below +1%; metamethods improves 24.173%. See the stage-5 report for the rejected
frame-window design, instruction profiles and proof limits.
