# ADR 0027 — Generic `for`

## Context

Lua 5.4's `for n1, ..., nk in explist do body end` works like this:
1. The expression list is evaluated once and adjusted to exactly four values: the iterator, a state, the initial control value, and a closing value.
2. Each iteration calls `iterator(state, control)` and adjusts its results to `k` values.
3. A nil first result ends the loop. Any other value, false included, becomes the next control value, and the body runs with `n1..nk` as new locals.
4. The closing value is a to-be-closed variable for the whole loop. It closes when the loop ends, however it ends.

By Phase 3.14 Moonseed already had every mechanism this needs:
- calls that pause, wait, yield, raise, and restore, for any callable value;
- result windows that adjust a call's results to a count;
- `<close>` values whose closes are resumable calls (ADR 0026);
- one scope-exit helper, `emit_scope_exit`, and loop contexts for `break`.

## Decision

### Registers

`base` is the first free register at the statement.

| Register | Holds |
|---|---|
| `base` | the iterator |
| `base + 1` | the state |
| `base + 2` | the control value, hidden |
| `base + 3` | the closing value, a hidden `<close>` local |
| `base + 4 ..` | the loop variables `n1..nk`, the body scope's first locals |

The four hidden values are locals whose name source cannot write, `(for state)`. They are defined after the expression list is compiled, so the list cannot see them or the loop variables. `for k in f(k)` passes the outer `k`.

### Code

```text
    <explist adjusted to 4 values into base..base+3>   place_values, as `local` does
    MarkClose   base+3
    Jump        call
body:
    <body>
    <body scope exit>          CloseScope or CloseUpvalues from base+4, or nothing
call:
    Move        base+4, base
    Move        base+5, base+1
    Move        base+6, base+2
    Call        base+4, 2 arguments, k results
    GenericForLoop base, body
    CloseScope  base
```

- **The iterator call is an ordinary `Call`.** It calls a copy of the iterator with copies of the state and control, and its `k` results land on the loop variables. A Lua closure, a native, a table with `__call`, even `pcall`, work as they do anywhere.
- **`GenericForLoop { base, offset }` (tag 50)** decides. A nil first result falls through to the loop's exit. Anything else is copied to `base + 2` and the loop jumps back to the body. The loop variable itself stays the body's to change, so `x = 999` in the body does not steer the next call.
- **Exit.** `CloseScope { from: base }` closes the closing value.
- **`break`** leaves every scope back to the one outside the hidden values. That is the same `CloseScope { from: base }`, then a jump past the loop's own exit.
- **`return`** closes everything at or above the frame's base, closing value included (ADR 0026).
- **An error** unwinds through the frame, which closes its values with the error.

The body scope is entered once per iteration. Its exit runs before the next call's `Move`s. A captured loop variable's cell is closed before its register is overwritten, so each iteration's closures see their own values.

### The closing value

- `MarkClose` registers it once per loop, before the first call. Nil and false are ignored. Any other value needs `__close` then, or the loop raises "variable got a non-closable value" before the iterator is ever called. PUC names the variable `(for state)`.
- The compiler cannot know which value the loop will get, so the hidden local is always marked `<close>`. `emit_scope_exit` then gives `CloseScope` on both the exit and `break`. With nil there, `CloseScope` closes nothing.
- Body `<close>` locals sit above `base + 3`. They close at the end of each iteration, before the closing value's slot is ever reached. The thread's to-be-closed list stays increasing, and on any exit the body's values close before the loop's.
- `__close` is looked up when the value closes, and closes may yield, wait, raise, and be checkpointed, as in ADR 0026.

### Why an ordinary `Call`

Everything that must survive a pause in the middle of an iteration is ordinary state:
- the four hidden values and the loop variables are registers;
- where the loop is is the frame's `pc`;
- a running iterator is a frame, a waiting native is the frame's pending call, and `pcall` as the iterator is a boundary frame;
- the closing value is in the to-be-closed list, and a close in progress is the frame's close state.

Every part of the runtime that reads a call site from the instruction at `pc` already expects a `Call`: native call sites, pending-wait validation, and a `Protect` frame's caller check. The iterator call gives them one. There is no new continuation and no new snapshot field, so the snapshot schema stays 9. The bytecode revision is 9 because of the new instruction.

### Validation

A `GenericForLoop` in restored or compiled code must:
- follow `Call { func: base + 4, nargs: 2, nresults }` with `nresults` from 1 to 254. That means at least one loop variable, and never an open count;
- have `base + 4` inside the frame;
- branch backward, to before that call, and inside the prototype.

The `Call`'s own check bounds the result window.

### Fuel, dispatch, and collection

- **Fuel:** every instruction costs one unit. The loop's own control is 5 units per iteration: three `Move`s, the `Call`, and `GenericForLoop`. The iterator's instructions are charged as they run. Setup is `MarkClose` and a `Jump`, and the exit is one `CloseScope`.
- **Dispatch:** `GenericForLoop` runs in `exec_rare`; the `Move`s are hot. Moving it into `run_hot` would save 15–20 ns of a 135–160 ns iteration and grow `run_hot` by 429 bytes. The iterator call dominates, so it stays out.
- **Collection:** each call overwrites the result registers, and the result window clears the callee's scratch above them, so an iteration leaves no stale values behind. After the loop, the hidden registers keep their last values until later locals reuse them or the function returns. That is true of every local that has left its scope in Moonseed, and the collector treats a frame's whole register window as roots.

## Alternatives

- **PUC's `TFORCALL`, one instruction that copies and calls.** It would save three fuel units and three hot dispatches per iteration. Every place that reads a call site would need a second instruction shape: native calls, waits, protected calls, and their restore checks. The `Move`s run in the hot tier and cost a few nanoseconds.
- **`GenericForLoop` in the hot tier.** Measured above. It is not the loop's main cost.
- **A separate close for generic `for`.** Not needed: `MarkClose` and `CloseScope` are exactly the loop's semantics.
- **Clearing the hidden registers at the exit.** `break` leaves through `CloseScope`, not the exit, so a clear in `GenericForLoop` would cover only one of the two ways out. Hidden values are not special among dead registers.
- **Reusing numeric `for`'s state layout.** Numeric `for` rewrites its visible variable from a hidden index. Generic `for` has a hidden control and visible variables that come from a call. Sharing the layout would blur both.

## Consequences

- **Language:** Lua 5.4's generic `for` works in full. `pairs`, `ipairs`, `next`, and `__pairs` are library functions and do not exist yet. Loops use custom iterators.
- **Cost:** about 135 ns per iteration over a Lua iterator, and about 110 ns over a native one. A numeric `for` making the same call costs 146–180 ns, and the loop written with `while` about 173 ns. A real closing value adds about 170–190 ns per loop. `PERFORMANCE.md`, Phase 3.15.
- **Proof natives:** `upto(n, i)`, an iterator over `1..n`, was added to the proof registry for fixtures and benchmarks.
