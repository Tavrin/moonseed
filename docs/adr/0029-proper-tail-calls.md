# ADR 0029 — Proper tail calls: frame replacement for Lua callees, native callees handed to the caller's caller

## Context

Lua 5.4 guarantees proper tail calls. `return f(args)` does not keep the calling function's activation, so tail recursion of any depth runs in constant space. The manual limits the guarantee to one form: the list after `return` is a single function call, not in parentheses, and the `return` is not in the scope of a to-be-closed variable. A `<close>` value must close after the call returns, so its frame has to stay.

Until Phase 3.17 every call pushed a frame. `return f(n - 1, acc + 1)` recursed 1,000 deep and failed with "stack overflow" (`deep_tail_recursion_reproducer`). Phase 3.16 had moved a vararg frame's extras below its registers (ADR 0028), so a frame's own call slot, the slot a tail callee must take over, sits below the extras.

## Decision

### Eligibility

The compiler emits `TailCall { func, nargs }`, tag 51, for `return` of exactly one `Expr::Call`, when no local of the current function marked `<close>` is in scope. A generic `for`'s closing value is such a local, so a `return f()` in a generic `for` body is an ordinary call, as in Lua 5.4. A `<close>` local of an enclosing function does not count; the nested function has its own frame. `return (f())`, `return 1, f()`, `return f(), 1`, and `return 2 * f()` compile as before.

`Return { base: func, count: COUNT_OPEN }` always follows `TailCall`. The validator refuses a `TailCall` without it, or with an argument window past the registers. A tail call's arguments are compiled exactly as an ordinary call's (`call_window`), including an open last argument.

### A Lua callee replaces the frame

`TailCall` runs in one step, with no safe point inside it.
1. `__call` resolves as for any call, inside the instruction, so no resolution step leaves a frame. The chain bound (`MAX_CALL_CHAIN`) still applies.
2. The stack bound is checked for the callee's frame before anything changes. The depth is not checked: a tail call keeps the depth it had.
3. The running frame's open upvalues close, through the same function every return and scope exit uses.
4. The callee and its arguments move down to where the running frame's own arguments were passed: its call slot is `base - vararg_len - 1`, and the arguments start just above it. The move is `copy_within`, which handles overlap. A thread's first frame has no call slot; its arguments move to slot 0.
5. `enter_lua` builds the callee's frame. It is the one frame builder for both kinds of call: `Entry::Push` pushes, `Entry::Replace` writes the new frame over the running one and truncates the stack to its registers. The vararg layout, the discarding of arguments a fixed function does not take, and the clearing of registers are the same code.

The new frame keeps the old frame's `nresults`, so the callee's results go to the caller's caller, adjusted once, as that caller asked. Frames below are not touched, so the index of a `Protect` boundary, which a message handler's boundary and an unwind refer to, stays valid.

### A native callee becomes the caller's caller's call

A native does not run in a frame of its own. Every native path (VM-local, `pcall`, `xpcall`, `error`, an External native stopped in `NativePrepared`, a native waiting in `NativeWaiting`, a fault, a completion with an error) runs from the frame that made the call, found by `call_site`. The frame below the tail-calling frame made the call that frame answers, at the same slot and wanting the same results:
- an ordinary frame's `Call`, its `pc` just past it;
- a metamethod or `__close` call, `meta.slot`, one result;
- a `Protect` boundary's protected call, `func + 1`, every result;
- a `Handler` boundary's message handler, `slot`, one result.

So the native becomes that call. The window moves down to the slot and the tail-calling frame is removed. An ordinary caller's `pc` moves back onto its `Call`, which the native's return moves past again. Then the native runs as if called from there. The caller is gone before the native runs: a native that waits holds no Lua frame of the caller, a fault unwinds from the frame below, and an External native's effect id is taken as for any call. No new continuation exists, so no new snapshot state does either.

`call_site` now takes a metamethod call's argument count from `top`, as it already did for an ordinary call and a boundary's, because a Lua metamethod that tail-calls a native passes the native arguments of its own. `meta.nargs` stays the count of the metamethod call as made.

**A thread's first frame** has no frame below: its results leave the thread, to the host or to the resumer. There the native is called from the frame itself. Its open upvalues close, its registers and extras around the call window are cleared, `call_site` treats the `TailCall` as a `Call` wanting every result, and the `Return` after it returns them. The frame stays until the native returns, holding nothing else.

### To-be-closed values

Compiled code never reaches a `TailCall` with a to-be-closed value of the frame live. Hand-built code can. At run time a `TailCall` whose frame owns a to-be-closed slot returns `VmError::Corrupt`; the check is one comparison with the last entry of the thread's to-be-closed list. Restore refuses a frame stopped at a `TailCall` that owns such a slot.

### Restore

A native tail call rewinds the frame below, so that frame must really be making the call. Restore now checks that every Lua frame above another answers that frame's call: its call slot and result count match the `Call` just before an ordinary frame's `pc`, a metamethod call with its call running, or a boundary's call. States a run reaches always satisfy this. A frame stopped on a `TailCall` with a native pending or a `Protect` boundary above it must be its thread's first frame.

### Fuel

`TailCall` is one charged instruction. For a Lua callee, or a native handed to the frame below, the `Return` after it never runs. A call and its return were two instructions, so a tail hop costs one fewer than before. A native tail call from a thread's first frame runs the `Return` and is charged for it, as an ordinary call and return would be. Fuel stays one unit per executed instruction; the fuel revision is unchanged, and bytecode revision 11 marks the new instruction.

### Dispatch

`TailCall` is handled in `exec_rare`. `run_hot` and `exec` are unchanged.

### Revisions

- Bytecode revision 11: `TailCall` (51), always followed by the `Return` of its open window.
- Snapshot schema 10, unchanged: a replaced frame is an ordinary frame, and a native tail call is an ordinary pending call of the frame below.
- Tables, fuel, and GC policy are unchanged.

## Alternatives

- **Call, then return, for a native (PUC Lua's way).** PUC Lua keeps the Lua caller while a C function runs and returns its results through the `RETURN` after `TAILCALL`. Moonseed does the same only in a thread's first frame, where nothing else is possible. Elsewhere, handing the call down costs one frame move and lets a waiting native hold nothing of the caller.
- **A tail-native continuation.** A compact record of "deliver this native's results to that destination" would be a new kind of pending state to trace, snapshot, and validate. The frame below already is that record.
- **Detecting `Call` followed by `Return` in emitted code.** Eligibility is a property of the source: `return (f())` is also a call followed by a return. The compiler decides from the AST.
- **A separate frame builder for tail calls.** Two builders could disagree on the vararg layout or on which registers are cleared. `enter_lua` is one function with two entries.

## Consequences

- **Language:** proper tail calls as in Lua 5.4, checked against Lua 5.4.9: three fixtures, one of them 100,000 hops of self, vararg, mutual, `__call`, and native-ending tail recursion.
- **Resources:** tail recursion runs in constant frames and stack slots. In the tests, the most frames, slots, and objects any step sees are the same at 1,000 and 100,000 hops. Tail calls do not count against the 1,000-frame depth. Only fuel bounds a tail-recursive loop, as it bounds any loop.
- **Debug information:** the caller of a tail call is gone, as in Lua. A future debug library cannot show it, and neither can an `xpcall` message handler's view of the stack. A failed coroutine keeps the frames still there, not tail callers. A native tail call from elsewhere than a thread's first frame also removes the Lua caller, which PUC Lua keeps while a C function runs; only frame counts show the difference.
- **Cost:** `PERFORMANCE.md`, Phase 3.17.
- **Review:** restore bounded the native's arguments below by the count of the caller's own `Call`. A native tail-called with fewer arguments than that call passed, including every iterator of a generic `for` that tail-calls one with fewer than two, was checkpointed and then refused. Restore now requires only the callee below `top`, which is what `call_site` reads; `__call` had already made the `Call`'s count only a lower bound.

## Phase 3.32 destination-copy precedent

Destination-aware arithmetic follows the same fuel precedent as tail calls:
source programs can compile to fewer unit-cost instructions without changing
fuel revision 6. Existing bytecode is untouched; cross-build source fuel budgets
require a pinned compiler. Whole-run and sliced/checkpointed executions of the
new bytecode must still agree.

`ArithK` follows this precedent as well (ADR 0054), while raising bytecode
revision to 13 because the stored instruction set changes.

## Amendment — 2026-10-04: native tail calls retain the Lua frame

PUC Lua 5.4 runs a C callee above the still-live Lua frame at `OP_TAILCALL`,
then finishes that frame with `luaD_poscall`. Erasing it made argument names,
debug levels, traceback, `coroutine.wrap` positions, and `xpcall` handlers
observe a different stack. A native tail call now uses the former first-frame
path at every depth: it closes upvalues, clears dead slots around the call,
runs the native from the `TailCall` frame with open results, and executes the
compiled `Return` after the native completes. Lua-callee frame replacement is
unchanged. Native recursion such as `return pcall(f)` consumes frames and can
reach the 1,000-frame limit, as in PUC Lua.

The snapshot format and revisions are unchanged. Restore accepts the new
pending native call at `TailCall` in any Lua frame and still accepts old images
where the caller was erased and the native call was handed to the frame below.
The earlier native-erasure decision, its resource and fuel consequences, and
its listed alternative above describe the historical implementation; this
amendment supersedes them for native callees.
