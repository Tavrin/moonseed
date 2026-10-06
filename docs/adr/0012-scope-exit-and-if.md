# ADR 0012 — Closing captured locals on scope exit, and the first `if`

## Context

Phase 3 compiled closures from source, but `Jump` did not close upvalues. When a block ends, a later local can reuse the register of a local that a closure captured. If that capture is still open, the closure then reads and writes the new local. `if`, loops, `break`, and `goto` all leave blocks, so this had to be fixed before any of them compiled.

## Decision

Closing is a separate instruction, `CloseUpvalues { from }`. It closes the running frame's open upvalues whose register is `from` or above. The cell keeps its `ObjectId`. Its state changes from `Open { thread, slot }` to `Closed(value)`, taking the register's value at that moment. Every closure that held the cell still holds the same one. Captures below `from` stay open. `Return` uses the same routine with `from = 0`. The routine walks the thread's open list once and allocates nothing.

`Jump` does not close anything. An edge that leaves a scope with a captured local is compiled as `CloseUpvalues` followed by `Jump`. A block that falls through its end gets the `CloseUpvalues` too, without a jump. A block with no captured local gets no close.

The compiler keeps one scope record per block: the local count and the register frontier on entry. A local is marked captured when a nested function resolves it as `Capture::Local`. Leaving the block emits the close through `emit_scope_exit` when a local in the block was captured. It then truncates the locals and returns the frontier to its value on entry, so the next local reuses the register. A branch that ends in `return` emits neither the close nor the jump over `else`. `Return` already closes the frame.

`if cond then ... [else ...] end` compiles to `JumpIfFalse { src, offset }`. `nil` and `false` jump. Everything else, including `0` and `""`, falls through. The condition is a single-value context: a call condition is `Call` with one result, so the rest of its results are dropped by the existing result-window rule. `elseif` is `Unsupported`. It is not rewritten as a nested `if`.

`CloseUpvalues` costs one fuel unit. Its physical work is linear in the thread's open list, which the compiler bounds at 200 upvalues per function. It runs no Lua code and cannot be interrupted. That is only true because it closes plain captured locals. Lua 5.4 `<close>` calls `__close` in reverse order, on normal exit, `break`, `goto`, `return`, and error, and can raise. It will need its own resumable operation. It cannot reuse this one.

Open and closed state were already snapshot fields, and restore rebuilds the thread's open list from open cells. The container schema stays 3.

## Versioning

The snapshot header already had three revision fields after the schema number: bytecode, tables, and fuel. All three were 1. `LoadBool` (tag 31) was added in Phase 3 without bumping the bytecode field. The fields are now named constants and mean:

| Field | Changes when | Value |
|---|---|---|
| Schema | The graph or wire layout changes | 3 |
| Bytecode | An opcode is added, or an opcode's encoding or meaning changes | 2 (tags 31–33) |
| Tables | `next`, anchor, or border semantics change | 1 |
| Fuel | The charging rule changes | 1 |

A decoder accepts only its own values. A snapshot from revision-1 code fails with `BadVersion` at the header. An older runtime reading a revision-2 snapshot also fails at the header, before any prototype is decoded. The Phase 3 note that a snapshot without tag 31 still restores is no longer true: old snapshots are refused rather than decoded op by op.

## Alternatives

Give `Jump` a close threshold. Every jump would then carry a field that most jumps do not need. Separate close and jump instructions are also what Lua 5.4 uses. A future `<close>` unwind is a different operation, so a close field on `Jump` would not carry it either.

Close every upvalue of the frame at each block exit. That breaks the outer-capture fixture: `outer_get` has to keep seeing writes to `outer` after the inner block closes.

Keep bumping nothing and rely on unknown-tag errors. That detects an unknown tag only once decoding reaches it. It does not detect an opcode whose meaning changed but whose tag did not.

## Consequences

Loops, `break`, and `goto` can use the same exit helper: close from the frontier of the outermost scope being left, then jump. Labels and `goto` legality are not implemented. The bytecode validator cannot check that a jump does not enter a local's scope, because prototypes carry no scope metadata.

Adding two variants to `Op` slowed the dispatch-heavy loops by about 20–35% on this machine. That is a change in LLVM's inlining, not extra work per instruction; see `PERFORMANCE.md`. Every future opcode can move these numbers again. The dispatch shape belongs to the performance track.
