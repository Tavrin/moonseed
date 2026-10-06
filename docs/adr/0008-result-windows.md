# Result windows and assignment state

## Context

Phase 1 stored a single `nresults` byte and treated zero as "keep every returned value" on a host-entered frame. `Resume` wrote one value. `Return` copied results through a fresh `Vec`. Neither form can represent a nil hole, a parenthesized call, or an assignment whose addresses must survive a checkpoint before the stores run.

Lua 5.4 evaluates every right-hand value and every left-hand address before it stores, and it stores right to left. A call in final position propagates every result, including interior nils. Parentheses request one result.

## Decision

`u8::MAX` (`COUNT_OPEN`) means "every value currently in the open region." Every other byte, including zero, is an exact count. The open region ends at the thread's `top`, so `10, nil, 30` is three slots, not two.

Results are written into the caller's register window. The callee's own registers above that window are cleared when it returns, so a dead temporary is not a root. Live caller registers must sit below the call or be copied out before a later call reuses them. `proto_outer` does that copy.

Varargs are copied to a range beside the callee window. `VarargLen` and `OpenLen` are the bytecode for `select('#', ...)`, not a `select` library.

Assignment destinations are recorded on the frame (`Option<Box<Vec<...>>>`, empty on an ordinary call). `AssignCommit` charges one fuel unit, then stores right to left. The cursor is a safe point before any store and between stores. A quantum of one pauses there without charging again.

Snapshot schema is 3 (ADR 0010). Schemas 1 and 2 are not restored. Schema 2 added the fields below; schema 3 adds live and dead table slots.

## Alternatives

- A temporary `Vec<Value>` per return. It allocates on every call and is easy to forget in a snapshot.
- One special-case instruction per fixture. It would not cover padding, truncation, or a later compiler.
- Left-to-right stores. Lua 5.4.9 stores right to left when two addresses alias.

## Tradeoffs

Fixed counts stop at 254. Open counts are bounded by the snapshot's register limit. Between-store checkpoints can observe a partial assignment; that matches separate bytecode stores and metamethod yields, and fuel pauses are still not Lua yields.

## Reversibility

The bytecode and schema are private. A later compiler can emit the same window rules without keeping these opcodes stable.

## Revisit if

A measured call profile shows the destination box or `top` maintenance in the hot path, or Lua 5.4 conformance requires a different store order than 5.4.9.
