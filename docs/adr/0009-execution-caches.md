# Execution caches are not semantic state

## Context

The performance addendum separates canonical simulation state from disposable execution caches, and suggests a guarded constant-field cache as an early optimization. Phase 1.5 measured the interpreter that actually exists.

## Decision

No execution cache is implemented. Snapshots continue to contain only canonical state: heap, registers, frames, `top`, varargs, assignment destinations, and host-call continuations.

A warmed Lua-to-Lua call already does not allocate. Checked handles stay. A field-slot cache is not added. Traversal now defines part of the invalidation: an in-place value change, a slot becoming a dead anchor, and an absent-key insert that compacts and moves live slots. `__index`, `__newindex`, and `__len` are still missing, so the cache stays deferred. Slot indexes and `next` links are not snapshot state.

Any later cache may change speed. Dropping it at a safe point must leave the same continuation, the same fuel charge for the semantic operations already performed, and the same host effects. The cache is not part of a snapshot.

## Alternatives

- Implement the field cache now, against tables that have no metamethods. Invalidation would be wrong as soon as `__index` exists.
- Box `Pending` or intern host symbols to shrink the 96-byte frame. The call measurements do not show that copy as the problem.

## Tradeoffs

The arithmetic loop is about 14 ns per charged instruction here, and a scalar call is 118 ns with zero vector growth. Those numbers are local and will move. They are a baseline, not a contract.

## Reversibility

Adding a cache later does not change the snapshot schema if the cache is omitted from it. Shrinking `Frame` is a private layout change.

## Revisit if

A profile of a real script, not this loop, shows handle checks or frame copies as a substantial fraction of time, or field lookup remains dominant after metamethods exist.
