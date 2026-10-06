# ADR 0026 — To-be-closed variables and resumable scope cleanup

## Context

Lua 5.4's `local x <close> = v` calls `v`'s `__close` metamethod when `x` leaves scope. That happens however the scope is left:
- falling out of a block;
- `break`;
- a loop's next iteration;
- `return`;
- an error unwinding through the scope;
- closing a coroutine that still holds the value.

A `__close` is ordinary Lua or native code. It can yield, wait on the host, raise an error, or overflow the stack. Moonseed must also be able to checkpoint anywhere during all of this, and then continue the original control flow exactly once.

Two Lua 5.4 rules become observable once `<close>` exists:
- A coroutine that fails with an error nothing in it catches is dead, but its stack is not unwound, so its `<close>` values stay pending. `coroutine.close` closes them later, with the error, and cannot yield while doing so.
- A close that raises does not stop the others. Each later close receives the newest error, and the last one raised is the one that propagates.

Phase 3.13's unwinder unwound failed coroutines, so that path changed before anything else.

## Decision

### Source

- `local a, b <close>, c = ...` is accepted. A list may hold one `<close>`.
  - "multiple to-be-closed variables in local list" is a syntax error.
  - `<const>` is rejected as unsupported.
  - Any other attribute is a syntax error.
- A `<close>` local is read-only. Assigning to it, directly, from a nested function, or in a multiple assignment, is a compile error: "attempt to assign to const variable 'x'". The value it refers to can still change.
- As for any local, the name is not visible in its own initializer.

### Instructions (bytecode revision 8)

| Instruction | Tag | Meaning |
|---|---|---|
| `MarkClose { reg }` | 47 | After the declaration. Nil and false are ignored. Any other value must have a non-nil `__close` now, or the declaration raises "variable got a non-closable value". The value's slot then joins the thread's to-be-closed list. |
| `CloseScope { from }` | 48 | Leave every scope down to register `from`: close its open upvalues, then its to-be-closed values, newest first, then continue after the instruction. |
| `CloseThread { dst, thread }` | 49 | `coroutine.close`. See below. |

`emit_scope_exit` stays the one place that decides how a scope is left:
- `CloseScope` when the scope holds a `<close>` local;
- `CloseUpvalues` when it holds only captured locals;
- nothing otherwise.

`break`, loop backedges, `repeat`, numeric `for`, `if`, and `do` all go through it.

`Return` closes every value at or above the frame's base before it returns.

Only the order of the list is fixed at declaration. The `__close` metamethod is looked up again when the value closes, from its current metatable. A replaced `__close` runs the new function, and a removed or non-callable one raises "attempt to call" at close time, as in Lua. Any callable value works, including a table with `__call`.

### State

- **`ThreadObj::tbc`:** the slots of the active to-be-closed values, in declaration order. Declaration order is register order, and a scope's values close before its registers are reused, so the list is strictly increasing. It holds slots, not values. The values stay rooted through the stack like any register, and nothing is traced twice.
- **A frame running its closes:**
  - `MetaEvent::Close` on the frame's metamethod call, with `MetaCall::close` holding `Closing { from, next }`.
  - `next` says what the frame does once no listed value at or above `from` remains: `Advance` past `CloseScope`; `Return { src, produced }`; or `Unwind(unwind)`, resuming the unwind that stopped at the frame.
  - Phase `Idle` means no call is running: the next step closes the next value or finishes. Otherwise the phase is a call's (running, native prepared, native waiting).
- **Metamethod calls stay small.** The close state is boxed apart from `MetaEvent`, so the event of every other metamethod call stays a few bytes. An earlier layout kept the close state inside the event. That made every metamethod call pass a 24-byte value, and `t[k]` through `__index` measured 10–17% slower.

### Running the closes

One value closes at a time (`close_step`, not charged):
1. The newest listed slot at or above `from` is removed from the list.
2. Its `__close` is looked up, and called with the value and either the unwind's error or nil. The call goes through the ordinary call path, like a metamethod call (`call_close`), with its results dropped.
3. A scope's own register is cleared, since the local is going out of scope. A `Return` or an unwind keeps the register, which may hold a result: `return x` returns the closed `x`.

When a call returns, its commit sets the frame back to `Idle`, and the same step closes the next value. The first close starts in the step of the instruction or unwind step that began the closes.

Nothing runs on the Rust stack. A close call can:
- pause at any quantum;
- be checkpointed;
- wait on the host;
- yield inside a coroutine, which resumes into it.

### Ordinary exits

- **`CloseScope`:** charged once, as an instruction. Its closes are uncharged steps, and each close call pays its own fuel.
- **`Return`:** keeps its results in their registers. It records `src` and `produced` and runs the closes, which are placed above the results. Then it returns exactly those values, even when a close yielded, waited, or was checkpointed between.

### Error unwinds (ADR 0024)

The unwind's `Popping` step does not remove a frame that still has listed values. Instead it:
- abandons the stopped instruction's pending call, metamethod call, and assignment;
- closes the frame's open upvalues;
- moves the unwind into the frame's `Close` state;
- closes the values with the unwind's error.

The frame stays alive while its closes run. When they are done, the unwind is restored and pops the frame.

A close that raises starts a new unwind from the top:
- The close's frames are unwound first, with their own closes.
- The new unwind reaches the closing frame, abandons its close state, and closes the frame's remaining values with the new error.
- The protected call that was the target is still the nearest one, so the last error is what `pcall` returns.
- An `xpcall` message handler runs for the close error too.

This matches Lua 5.4.9 case for case: `close_error.lua` and `close_xpcall.lua`.

A frame whose closes run during the unwind of a stack overflow may use the frames past `MAX_CALL_DEPTH`, like a message handler. (Until Phase 3.16 any unwind's closes could; ADR 0028.) On a stack that overflowed, each close still has room, bounded by `MAX_FRAMES`. That includes a `pcall` inside the close. A test closes 998 values while a `pcall` catches the overflow.

A call that would pass the reserve raises "error in error handling". Lua 5.4.9 raises the same for a stack that overflows again while an overflow is being handled.

When the host ends a waiting `__close` with an error (`complete(key, Completion::Error)`), only the call is over. The frame keeps its close state, so the unwind reaches it and closes the rest. The state also still marks where a `CloseThread` stops catching.

### Coroutines (Gate 0)

`ThreadObj::coroutine` marks threads made by `NewThread`. When a coroutine raises an error nothing in it catches:
- it becomes `Failed`;
- it keeps its frames, stack, open upvalues, and to-be-closed list;
- the thread that resumed it raises the same error.

The entry thread, and host calls through `call_closure`, still unwind to their bottom and run their closes; the host boundary acts as a protected call.

A coroutine that yields while holding values keeps them listed. Resuming it continues with the same list.

### `CloseThread`

`CloseThread { dst, thread }` is Lua's `coroutine.close`.

**What it accepts:**
- A suspended or failed coroutine is closed.
- A dead one answers `true, nil` at once.
- The running coroutine raises "cannot close a running coroutine", and one that resumed it "cannot close a normal coroutine".

**How it runs.** Closing works like a resume. The coroutine is marked `closing`, its resumer waits in `Resuming`, and an unwind with no target pops every frame, closing each frame's values. The unwind carries the failed coroutine's error, or no error for a suspended one; only a thread close can have none.

**While it closes:**
- The boundary search of a new error stops at the frame being closed, so the coroutine's own `pcall`s do not catch close errors. A `pcall` inside a close method still catches its own.
- Any yield is "attempt to yield across a C-call boundary".

**Nested coroutines.** When a coroutine fails, its resumer's `Resume` is over before the resumer raises the same error; a failed resumer keeps no wait on a coroutine that is later closed.

**The answer.** When the frames are gone, the coroutine is dead, and the closer's `dst, dst + 1` get `true, nil`, or `false` and the last error. `error(nil)` in a close gives `false, nil`. A second close answers `true`.

`CloseThread` always writes two registers; `coroutine.close` returns only `true` on success.

### Snapshot (schema 9)

A thread adds its `coroutine` and `closing` flags and its `tbc` list. A frame's metamethod call can be a `Close` event, with `from` and `next`, in the `Idle` phase or any call phase. An unwind's error is optional.

Restore refuses:
- a listed slot that is not a register of a Lua frame;
- a list that repeats a slot or is out of order;
- a `Close` event that does not match its instruction: `CloseScope`'s `from`, `Return`'s window and count;
- a `Close` on a frame with a pending call or assignment;
- an idle call with a slot or arguments;
- a closing unwind that has no error outside a thread close, or does not aim at the nearest `pcall` below the frame;
- `closing` on a thread that is not a coroutine with a resumer waiting in `CloseThread`, or that is neither ready nor waiting on the host;
- a thread close whose new error aims at a `pcall` below the frame being closed;
- a frame idle between closes with a call running above it, when no unwind or message handler explains it;
- an entry thread marked as a coroutine;
- a `Resuming` whose instruction is not `Resume` or `CloseThread` with the matching window;
- a failed thread that is not a coroutine but kept frames;
- a running frame whose varargs are not on the stack.

Two of the prompt's malformed cases cannot be expressed:
- A consumed entry is removed from the list, so no state can refer to it.
- Normal closes carry no error field.

## Alternatives

- **PUC's layout, a linked list of stack deltas.** A plain list of slots is easier to validate and has the same ordering.
- **Pop the frames first, then run the closes on the bare stack, as PUC does in `luaD_closeprotected`.** The frames would then have to be rebuilt to resume after a yield or a checkpoint. Keeping the frame alive makes the close an ordinary call above it.
- **Store the `__close` function at registration.** Lua resolves it at close time, and a fixture checks that a replaced metamethod runs.
- **Close a failed coroutine at once.** That is `coroutine.wrap`'s behaviour, not the thread model's. Lua leaves the values pending until `coroutine.close`.
- **Reuse `CloseUpvalues` with a new meaning.** Old bytecode would change meaning silently. `CloseScope` is new, and `CloseUpvalues` keeps its cheap path.

## Consequences

- **Cost:** one close costs about 150–200 ns more than a scope without one. That is two `__close` lookups, dispatch, and the call. The numbers are in `PERFORMANCE.md`, Phase 3.14.
- **`Resume` is not quite either Lua function.** When the coroutine fails, the opcode raises its error in the resumer without closing it, which neither `coroutine.resume` (returns `false, err`) nor `coroutine.wrap` (closes, then raises) does. The coroutine library will build both on `Resume` and `CloseThread`.
- **Next:** generic `for` adjusts its initializer to four values, and marks the fourth `<close>`.
