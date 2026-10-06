# ADR 0019 — Resumable metamethod calls

## Context

`Index`, `SetIndex`, `GetField`, `SetField`, an assignment store, and `Len` were atomic. With metatables each can call a Lua or native function, which may run any number of instructions, wait on the host, or be checkpointed partway. The instruction must still finish exactly once, and nothing it needs to resume may live on the Rust stack.

## Decision

`index.rs` resolves an operation to `Resolved::Done(value)` or `Resolved::Call { function, target }`:

- **Chains.** A non-function `__index` / `__newindex` value is followed there, up to 2000 steps, Lua's `MAXTAGLOOP`. The chain is processed atomically inside the one charged instruction, with no metamethod call along the way, so its counter needs no persistent state. Past 2000 it faults with `LuaFault::MetaChain`.
- **Reads.** A primitive hit never consults `__index`.
- **Writes.** A store to a live key never consults `__newindex`. When `__newindex` is called, the raw store does not happen.
- **Length.** `#` returns a string's byte length. A table uses its `__len` if it has one, otherwise the raw border. A `__len` that is not a function faults with `BadCall`, since there is no `__call`.

For a `Call`, the VM writes the function, then `target` and the operation's operands, into scratch slots above the frame's registers, varargs, and open results. It then records a `MetaCall` on the frame:

- the event: `Index { dst }`, `NewIndex`, `NewIndexAssign`, or `Len { dst }`;
- the scratch slot and the argument count;
- a phase: `Running`, `NativePrepared`, or `NativeWaiting`.

The frame's `pc` stays on the instruction. The function runs through the ordinary call machinery. A Lua closure gets a pushed frame whose single result lands in the scratch slot, and the caller is not advanced. A native goes through the ordinary native path with its policy: a `VmLocal` native runs at once, and an `External` one stops prepared with an effect id. Either may wait; a wait's result is delivered to the slot.

When the frame is back on top with a `Running` call, `poll` commits it in a separate, uncharged step:

- `Index` and `Len` write the result to `dst`;
- `NewIndex` drops it;
- an assignment store drops it and moves the assignment cursor.

Then `pc` advances past the instruction.

Fuel: the instruction is charged once, when it begins. A Lua metamethod's instructions are charged as ordinary instructions. The commit is not charged, and it is a safe point.

Phase 3.32's direct non-vararg Lua `__call` setup preflights both the argument
insertion and the callee's register window before either is written. It keeps
the resolver's shifted arguments, function slot, caller PC and ordinary Lua
frame identical. Other callable chains, native/vararg handlers, short restored
stacks and quota/depth/reserve failures use the existing resolver and frame
entry. No event value is cached: a changed metatable or handler takes effect on
the next call.

A runtime-owned spare box reuses storage for completed plain `MetaCall`s.
The spare contains only continuation metadata, with no values, handles or close
state. The fast entry overwrites every field before installing it on the caller;
ordinary completion returns the box to the spare. Close continuations remain on
frames. Live continuation contents and commit boundaries are unchanged. This
allocation cache has at most one box, is omitted from snapshots and starts empty
after restore; it changes neither logical heap charging nor GC roots.

`MetaCall` is separate from `Frame::pending` because an assignment store (`Pending::Assigning`) can itself call `__newindex`. The recorded destinations and cursor stay untouched until the store's call commits, so a checkpoint anywhere inside the metamethod restores without recomputing a destination or running the metamethod twice.

A snapshot writes `MetaCall` as plain numbers. Restore checks:

- the event matches the instruction at `pc`, including `dst`;
- the argument count fits the event;
- the slot lies above the frame's registers;
- `NewIndexAssign` sits on an `AssignCommit` with a pending assignment;
- a native phase's slot holds a native of the matching policy.

A metamethod that faults ends the run with that fault. A faulting native clears the call; a faulting Lua metamethod leaves its frames as they were. Either way the faulted runtime still snapshots and restores.

## Alternatives

Call the metamethod recursively from Rust, with `Index` waiting on a nested interpreter. That puts the continuation on the Rust stack, which a checkpoint cannot capture.

Rewrite `Index` into a `Call` plus a result copy at compile time. The compiler cannot know which accesses hit a metamethod, and every access would pay for the call.

Store the continuation in `Frame::pending`. An assignment store already uses that slot while it may need a metamethod.

## Consequences

Bytecode revision 6: the source indexing instructions and assignment stores now follow metamethods, and `Len` is tag 43. Every future metamethod (`__call`, arithmetic, comparison, concatenation) can use the same `MetaCall` shape with new events. Protected calls will need to unwind frames that carry a `MetaCall`; today a fault simply ends the run. A metamethod value that is neither a Lua function nor a native is followed as a chain for `__index` / `__newindex`, and is a fault for `__len`, until `__call` exists.
