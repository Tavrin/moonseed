# ADR 0024 — Stackless errors, protected-call boundaries, and unwind phases

## Context

Until Phase 3.13 a fault ended the run. A metamethod that failed, a native that returned `Fault`, and a stack overflow all stopped the whole runtime, and nothing in Lua could catch them. An External native's fault was reported as `Completed`.

Lua needs `error`, `pcall`, and `xpcall`. Moonseed also needs each of them to survive what every other piece of state survives:
- a pause at any fuel quantum;
- a checkpoint and restore;
- a collection;
- a coroutine yield and resume;
- nesting inside metamethod calls and inside other protected calls.

A C implementation unwinds with `longjmp` to the nearest `setjmp`. That puts the protected call on the C stack, where a snapshot cannot see it. Moonseed keeps every continuation in the heap (ADR 0005, ADR 0019), so the protected call, the error in flight, and the progress of the unwind have to be heap state too.

## Decision

### Two kinds of failure

- **`LuaFault`: a Lua error.** Lua code can catch it. It has 25 classes: the old fault kinds, plus:
  - `Error`, raised by `error(value)`;
  - `Memory`;
  - `StackOverflow`;
  - `ErrorHandling`, when a message handler keeps failing;
  - `YieldAcross`, a yield inside a message handler.
- **`VmError`: corruption or host misuse.** A broken handle, an impossible frame, or an unknown native is a bug or a bad host call. It is returned to the host and is never catchable. A test corrupts a frame under `pcall` and checks that `run` returns `Err(Corrupt)`.

Allocation failures are a special case:
- An allocation that fails the object limit or the heap quota returns `VmError::MemoryLimit` inside the step.
- `vm_error`, an out-of-line cold path at the step boundary, turns it into `LuaFault::Memory`.
- The failing instruction either allocated nothing yet or belongs to frames the unwind removes, so this is safe.

`FuelLimitExceeded` and `ObjectIdExhausted` stay terminations. Fuel is the host's budget, and no script path clears it.

### The error object

An error object is any Lua value.

**Reserved messages.**
- Every class except `Error` uses a string reserved for that class: "not enough memory", "stack overflow", "error in error handling", and so on (`LuaFault::text`).
- The 25 strings are made at boot, before anything can fail. They are GC roots and snapshot state (`fault_strings`).
- So raising an error never allocates. That includes raising a memory error when the heap is full.

**`error(value [, level])`.**
- It raises `Error` with `value` itself. Identity is kept, so `pcall(error, t)` returns the same table `t`.
- The level must be an integer, a float with an integer value, or a string that reads as one. Anything else is a `Native` fault.
- The level has no other effect. In PUC Lua it chooses which call's source position to prefix to a string message. Moonseed has no line information at run time, so `error("x")` gives `"x"` where PUC gives `"file:1: x"`. `error("x", 0)` is the same in both.
- The oracle fixtures use level 0 wherever they compare a message.

**One conversion.** Every fault goes through `fault` → `fault_value` → `throw_on`, and nothing else builds an error object.

### Unwind state

An error in flight is `ThreadObj::unwind`, which holds:
- `fault`: the class;
- `error`: the object;
- `phase`: where the unwind has got to.

`throw_on` sets the phase to `Raised` and touches nothing else. The failing instruction's frame keeps its `pc`, pending call, metamethod call, and assignment cursor until the unwind removes the frame.

`poll` then advances the unwind, one uncharged step at a time. A quantum of 0 pauses between steps, as it does before a metamethod commit.

**`Raised`.** Find the nearest boundary frame:
- A `Protect` frame with a message handler, and an error the handler may see: call the handler now, above the failing frames (below).
- A `Handler` frame: the error escaped the message handler. Call the handler again with the new error, one level deeper. After 20 levels (`MAX_HANDLER_DEPTH`), the error becomes `ErrorHandling`, with its reserved message. A memory error keeps its own class.
- Otherwise the target is the nearest `Protect` frame, or none. The phase becomes `Popping { target }`.

**`Popping`.** Each step removes one frame (`pop_unwound_frame`):
- The frame's open upvalues at or above its base are closed, so closures keep the values they saw.
- Its pending host call, `MetaCall`, and recorded assignment go with it.
- A wait abandoned this way can no longer be completed; its key is unknown. A wait that `complete` ends with an error is cleared at once, before the unwind reaches its frame.

When the target is on top, the protected call finishes with `false, error`. With no target, the thread fails.

`pop_unwound_frame` is where a scope's `__close` handlers will run. Because the unwind removes one frame per step, and the phase is snapshot state, a close handler can later be an ordinary call that pauses, waits, or errors, and the unwind resumes after it. There is no bulk truncate that would have to be undone.

**A failed thread.** Its frames and stack are cleared, its status becomes `Failed`, and it keeps `(fault, error)`.
- A thread started by `Resume` passes the same error to the thread that resumed it, which unwinds in turn. This is the behaviour of `coroutine.wrap`; the coroutine library does not exist yet.
- The entry thread stops the run with `StepOutcome::LuaError(fault)`. Stepping it again returns the same outcome.
- `Runtime::lua_error()` returns the class and the error object as a `HostValue`. It is unstable API. An object error comes back as its `ObjectId`, which the host can root.

### Protected-call boundaries

`pcall(f, ...)` pushes a boundary frame and returns to the loop. It does not call `f` from Rust.

**`Protect { func, advance_caller, handler }`.** The boundary frame:
- sits where `pcall`'s own call would, with `base == limit == func + 1`;
- has `pc` 0 and carries the closure of the frame below; it runs no instructions.

`f` is called at `func + 1` through the ordinary call path, wanting every result. Any callable value works, so `pcall` of a table with `__call`, of a native, or of `pcall` itself follows the usual rules.

When `f` returns to the boundary frame, the next step (`finish_protect`):
1. pops the boundary;
2. writes `true` at `func`, followed by the results;
3. applies the result count the caller asked of `pcall`;
4. moves the caller's `pc` on.

A call that returns into a boundary frame keeps the registers and varargs of the Lua frame below it; the boundary owns none. Step 3 clears the dropped results but stops below the caller's varargs.

`advance_caller` records whether step 4 applies. It is true when the caller made an ordinary `Call`. It is false when `pcall` was itself called by a metamethod call or by another boundary, which finish the call their own way.

**`Handler { slot, protect, depth, fault }`.** This is `xpcall`'s message handler, run above the failing frames before any of them is removed:
- `slot` is above everything live: the larger of the stack length and `top`.
- The handler gets the error object and runs one level deeper than the error.
- Its first result, or nil, becomes the error.
- The unwind then pops down to the `Protect` frame at `protect`.
- A test reads the stack depth from inside a handler and finds the failing frames still there.

**Rules for the message handler:**
- It must be a function, a Lua closure or a native, as in Lua 5.4. A table with `__call` is refused with `Native`.
- It is not called for `Memory` or `ErrorHandling`. PUC Lua does not call it for memory errors either.
- A yield anywhere above a `Handler` frame is `YieldAcross`. Host waits are allowed: they are not Lua yields, and the handler's frames are snapshot state like any other.

**Yields.** A protected call does not block a yield. A coroutine that yields inside `pcall` keeps the `Protect` frame in its frame list, and resuming it continues inside the protected call. A hand-bytecode test yields from inside `pcall` and resumes.

**Builtins.** `error`, `pcall`, and `xpcall` are base-library values, registered like other natives (`register_builtin`) and dispatched in `call_native`. They are not opcodes. Bytecode revision 7 is unchanged.

### Stack overflow

- A Lua call past 1,000 frames raises `StackOverflow`, which `pcall` catches.
- A thread may hold up to 1,080 frames (`MAX_FRAMES`). The extra 80 are for message handlers running on top of a stack that overflowed: 20 handler levels of a few frames each.
- The snapshot's frame bound is the same number. Its 10,000-register bound on a stack can bind first: recursion through functions with many registers passes it well before 1,000 frames, and `snapshot()` then refuses the state with `LimitExceeded` until the stack shrinks.

### Natives

- **`NativeOutcome::Fault`:** a `Native` error. For External natives this fixes the Phase 3.12 bug. If the native's `raw_set` failed on the quota, the fault is `Memory` instead.
- **Completing a wait:** `Runtime::complete(key, Completion)` ends any native wait:
  - `Completion::Return(values)` gives any number of `HostValue`s;
  - `Completion::Error(value)` raises `Error` with that value in the waiting thread, as if the native had called `error`.
  - `complete_wait(key, i64)` is the one-integer case and allocates nothing.
  - A value that names no object or native this runtime has returns `WaitError::InvalidValue` and leaves the wait as it was. Only a value the heap has no room for raises a memory error in the waiting thread.
- **Calling Lua from a native:** `pcall` is the pattern. The native pushes explicit state, a boundary frame, and returns to the loop. The callee runs as an ordinary frame, and its return is finished by a `poll` step. A native that calls Lua in general will use the same mechanism, not Rust recursion.

### Snapshot

Schema 8 adds, beside the heap quota (ADR 0025):
- the frame boundary;
- the thread's unwind and error;
- the reserved error strings.

Restore refuses:
- an unwind target that is not a `Protect` frame, or is out of range;
- a boundary frame whose registers, slot, or function do not fit it;
- a `Protect` frame whose `advance_caller` disagrees with the frame below;
- a `Handler` frame without an `xpcall` handler behind it, or deeper than 20;
- a handler frame for a `Memory` or `ErrorHandling` error;
- an error object that names nothing;
- a failed thread with frames, or without an error;
- an unwind on a thread that is not ready;
- an unwind that pops past the nearest `Protect` frame, or has no target while a boundary remains;
- a `Protect` frame whose caller is not stopped on a `Call` at its slot with its result count;
- reserved error strings that are shared, owned by a prototype, or not their class's text;
- a wait on any frame but the top one, or a thread whose status disagrees with its top frame's wait;
- a running frame whose varargs are not on the stack.

Frame slot indices are bounded by the snapshot's register bound; see SECURITY.md.

## Alternatives

- **Propagate errors as Rust `Result`s through recursive calls.** The protected call would live on the Rust stack, which a checkpoint cannot hold.
- **Unwind all frames in one step.** It is at most 1,080 frames, but it leaves no place for a `__close` handler to run as a resumable call. It also makes one uncharged step do unbounded work.
- **`pcall` as an opcode.** `pcall` could not then be stored, passed, or called through `__call`. It would also need a bytecode revision.
- **A new message string for each error.** Raising a memory error would then need memory.
- **Keep message-handler state in `Unwind` instead of in frames.** The handler runs arbitrary Lua code, which needs frames anyway. With a frame, the handler's return point is checkpointed like any other return.

## Consequences

- **Positions:** error messages have no source position, and Moonseed's reserved messages are not PUC's. Tests compare error classes, or messages raised with `error(v, 0)`.
- **Tail calls:** Moonseed has no proper tail calls. `return f()` grows the frame list, so deep tail recursion overflows where PUC Lua runs in constant space. A fixture had to write `return (f())` to keep PUC from looping forever. This is a gap for the call milestone.
- **Cost:** a `pcall` of an empty function costs about 110 ns over a plain call. Catching an error costs less per frame than returning normally. `PERFORMANCE.md`, Phase 3.13.
- **Next:** `<close>` and `__close` run in `pop_unwound_frame` and at ordinary scope exit, then generic `for` on top of them.
