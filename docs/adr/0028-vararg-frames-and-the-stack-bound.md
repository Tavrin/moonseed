# ADR 0028 — Vararg frames below the registers, and a stack bound every snapshot accepts

## Context

Lua 5.4 functions declared `function (a, b, ...)` keep the arguments past their fixed parameters, their extras. The expression `...` produces the extras as a multi-value expression, like a call's results. A chunk is itself the body of a vararg function.

Until Phase 3.16, only hand-built bytecode had vararg functions. A call copied the extras above the callee's registers, to `limit .. limit + extras`. A call the vararg function made put the callee's frame at a register of the caller, `func + 1`. That frame reaches past the caller's registers whenever the callee has more registers than the caller has left, and its registers then cover the extras. A callee with four registers was enough to clear them. `vararg_overlap_program` reproduces this, and is the regression test.

Separately, a thread's stack had no bound of its own. The frame limit allowed 1,080 frames of up to 256 registers, about 276,000 slots. A snapshot accepted 10,000, so recursion through functions with many registers ran into states that `Runtime::snapshot()` refused.

## Decision

### Layout

A vararg frame's extras stay where the call passed them, just above the function's slot, and the frame's registers start above them. PUC Lua gets the same property by moving the function and fixed parameters up.

```text
F                 the call's slot: the callee, then its results
F + 1 .. base     the extras, vararg_len of them
base ..           the fixed parameters, then the other registers, to limit
```

- `push_lua_frame` rotates the argument window by the fixed-parameter count, so the fixed arguments move above the extras. A call with no extras, or to a non-vararg function, has `base = F + 1` as before.
- Every call the frame makes is at or above `base`, and every metamethod call, close call, and protected call is above `limit`. Nothing the frame calls can reach its extras.
- The frame records only `vararg_len`. The extras are `base - vararg_len .. base`, and a return writes its results at `base - vararg_len - 1`. `Frame::vararg_at` is gone.
- A return clears the callee's slots above its results as before. That now includes the extras, so they stop being roots once the frame returns. A non-vararg callee clears extra arguments at the call.

The extras are ordinary stack slots: traced by the collector, written to snapshots, and kept across pauses, waits, yields, protected calls, and unwinds.

### Source

- The parameter list may end in `...`: `()`, `(...)`, `(a)`, `(a, b, ...)`. `...` anywhere else in it is a syntax error.
- The prototype has `params` and `vararg`, taken from the declaration and not from the body. Compiled chunks are vararg with no parameters.
- `...` is legal only directly inside a vararg function. It is not an upvalue: `function(...) return function() return ... end end` is a compile error, "cannot use '...' outside a vararg function", at the inner `...`.
- `...` is a multi-value expression, compiled by the same code as a call (`multi`). `Vararg { dst, count }` copies the extras to `dst` with a fixed count, padded with nil, or all of them with `COUNT_OPEN`, which sets `top`.
  - It is one value in a single-value context, and in `(...)`.
  - It expands as the last expression of a list: `return ...`, `f(x, ...)`, `{ ... }`, `local a, b = ...`, and the four values of a generic `for`.
  - Elsewhere in a list it is its first value, as a call is.
- The validator refuses `Vararg` and `VarargLen` in a prototype that is not vararg.

### Chunk arguments

`Runtime::load_chunk_with_args(config, registry, chunk, args)` loads a chunk as `load_chunk` does, then lays the arguments out as a call would. `Runtime::results()` gives the entry function's return values as `HostValue`s. Both are unstable API. `load_chunk` still passes none.

### `select`

`select` is a base function, an ordinary native:
- `select('#', ...)` gives the count; any string starting with `#` counts.
- `select(n, ...)` gives the extras from the `n`th.
- A negative `n` counts from the end.
- `n` is an integer, a float with an integer value, or a numeric string, as `luaL_checkinteger` takes. `error`'s level check uses the same conversion.
- Zero, and a negative `n` before the first, are errors.

### The stack bound

`Config::max_stack_slots` bounds each thread's stack: registers, call windows, extras, and open results.
- **Default and range.** The default is 50,000. Boot clamps it to 1,024..=100,000.
- **Frames.** A frame whose registers would pass seven-eighths of the bound raises "stack overflow". The last eighth is kept for message handlers and for the closes of a stack overflow's unwind, as the frames past 1,000 are. Past the whole bound, the error is "error in error handling".
- **Other growth.** Every other way a running program grows the stack is checked by the same rule before anything changes: the call windows of metamethods and closes, open results of natives and host completions, `...` copies, and the argument shift of `__call`. A message handler's window is checked against the whole bound. So passing the bound is always a Lua error, never a host error. Chunk arguments may fill only the ordinary part.
- **Snapshots.** The bound is snapshot state. Restore accepts only bounds in the range, and only threads whose stack and slot indices fit their snapshot's bound. The decoder's structural limit is the top of the range.

A running thread can therefore never hold a stack its own snapshot would refuse. The frame limit (1,080), the stack bound, and the heap quota are three separate limits.

**Which frames get the reserve.** A message handler's boundary, as before, and a frame whose closes an unwind runs, but now only when that unwind carries a stack overflow or "error in error handling". Lua 5.4 extends its stack past the limit only while handling an overflow. A `__close` that recurses during an ordinary error's unwind therefore overflows with "stack overflow", as in Lua 5.4.9, where Phase 3.14 gave it the reserve and so "error in error handling".

The bound exposed one case where the stack grew without need. When an unwind stopped at a frame to run its closes, `top` stayed where the deepest popped frame had left it, and each close call started there. Each `__close` of an overflowing recursion that itself overflowed started a little higher, and the stack crept up close after close. The unwind now truncates the stack to the frame's registers before its closes run.

### Revisions

- Snapshot schema 10: frames drop `vararg_at`, and the image carries `max_stack_slots`.
- Bytecode revision 10: `Vararg` reads the extras below the frame and is legal only in vararg prototypes.
- Tables, fuel, and GC policy are unchanged.

## Alternatives

- **An explicit per-frame vararg segment.** Keeping the extras in a separate array, or a separate part of the thread, would put a second kind of stack storage into the collector, the snapshot, and every frame check. Rotating the argument window keeps the extras where the call wrote them. The cost is a rotation of the arguments, and none when there are no fixed parameters.
- **PUC's `VARARGPREP`, an instruction that moves the frame at entry.** The call already knows the prototype, so it can build the final frame. A separate step would add a state in which the frame exists half-built.
- **Raising only the snapshot bound.** Some bound on the stack is needed either way. Without a runtime bound, a snapshot bound only moves the point where checkpoints fail.
- **A fixed reserve, like PUC's 200 slots.** A proportional one keeps small configured bounds usable.

## Consequences

- **Language:** vararg functions, `...`, and vararg chunks work as in Lua 5.4, checked against Lua 5.4.9.
- **Tail calls:** a tail call replaces the caller's frame with the callee's. A vararg caller's extras sit just above its call slot, so a tail call from it must move the callee's function and arguments down to that slot before building the new frame, as PUC does.
- **Remaining gap:** a snapshot's total size is still bounded by 1 MiB, and one table by 10,000 entries. Both are much smaller than the heap quota allows, so a large heap can still be refused by `Runtime::snapshot()`. That bound is separate from the stack's and is next on the resource track.
- **Cost:** `PERFORMANCE.md`, Phase 3.16.
- **Review:** the milestone review found that four uncharged steps could still return `VmError::StackLimit` to the host from a Lua program: an unwind's close call, a message handler's call, an External native's results, and an assignment's `__newindex` call. It also found that growth other than frames could use the reserved eighth. The rule above, checked before any state changes, replaced those paths. A test runs each reproduction.
- **Not changed:** a resume or yield delivering more values than the receiving stack holds faults the coroutine with "stack overflow"; Lua reports "too many results to resume" and a yielding coroutine stays suspended. Source cannot reach this yet; the coroutine library will define resume errors.
