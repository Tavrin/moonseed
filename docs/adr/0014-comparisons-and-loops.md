# ADR 0014 — Primitive comparisons and structured loops

## Context

Loops need comparisons, and both need the scope-exit rule from ADR 0012 to hold on every edge: backedges, the `repeat` condition, and `break`. Lua's comparison rules are exact across integers and floats, and they will later fall back to metamethods. This phase has no metatables.

## Decision

### Comparisons

`compare.rs` is the only definition of `==`, `<`, and `<=`. `Op::Compare { kind, dst, a, b }` (tag 34) writes a boolean, with `kind` one of `Eq`, `Ne`, `Lt`, `Le`. `~=` is `Ne`, the negation of the same equality function. `a > b` compiles to `Lt` with the operands swapped, and `a >= b` to `Le` swapped, as in Lua. Operands are still evaluated left to right. A comparison is an ordinary value: `local x = 1 < 2` works, and `if` tests that value with `JumpIfFalse`. There is no fused compare-and-branch.

Equality: numbers by mathematical value, strings by bytes (embedded zeros included), nil and booleans by value, tables, closures, and threads by identity. Values of different types are unequal.

Order: two numbers, or two strings. A mixed integer/float comparison never converts the integer to `f64`. The float is turned into an integer bound instead, its floor or ceiling, clamped to the `i64` range. So `2^53 + 1 > 2^53.0`, and `math.maxinteger < 2^63.0`. NaN is unequal to everything and unordered. Strings order by unsigned bytes, shorter prefix first. That is PUC Lua's order in the `C` locale, which the standalone `lua` uses. Moonseed does not consult a process locale. Ordering anything else is `LuaFault::Compare`.

When metatables arrive, `__eq` is tried only where `equal` now answers "distinct tables are unequal", and `__lt` / `__le` only where these functions now fault. The opcode does not change.

`numbers_only` is the heap-free part: two numbers. The hot tier calls it for `Compare`. If it returns `None`, nothing has been written, and `exec` runs the full `compare`. Both use the same number functions.

### Loops

Each function being compiled keeps a stack of loop contexts. A context holds the scope just outside the loop body and the `break` jumps waiting for the loop's exit.

`while c do b end`: test, `JumpIfFalse` to the exit, body as a scope, then a backward `Jump` to the test. The body's scope exit (a `CloseUpvalues` when a body local was captured) runs before the backedge, so each iteration's captured locals are distinct cells.

`repeat b until c`: the body scope stays open while `c` is compiled and evaluated, so `c` sees the body's locals. With no captured body local, the test is one backward `JumpIfFalse`. With one, the false edge closes and jumps back, and the true edge closes and joins the `break`s at the exit.

`break` emits `emit_scope_exit` to the loop's outer scope, which closes every captured local declared since the loop started, then a `Jump` patched to the exit. It targets the innermost loop in the same function. Outside a loop it is a `Syntax` error with the `break`'s span.

`do b end` is `scoped_block`. `elseif` extends the `if` chain: each arm tests, runs its scope, and jumps past the rest unless it ends in `return` or is the last arm with no `else`.

The VM knows nothing about loops. Loops are `Jump`, `JumpIfFalse`, and `CloseUpvalues`.

## Alternatives

Six comparison opcodes. One opcode with a kind validates the same way and keeps `exec` smaller.

Order strings with the host locale's collation. That would make a script's result depend on the process, and it is not reproducible across machines.

Compile `repeat b until c` as `while true do b; if c then break end end`. That has the right scope for `c` only if the `if` is inside the body's scope, and it adds an instruction per iteration. The explicit two-edge form states the rule directly.

## Consequences

Bytecode revision 3. Comparisons with two numbers run in the hot tier. That made `run_hot` 5,017 bytes, up from 2,949, and made the source loops two to three times faster. The numbers are in `PERFORMANCE.md`.

`goto` will need labels, forward and backward jumps between arbitrary scopes, and the rule that a jump may not enter a local's scope. `emit_scope_exit` to a target scope already handles the closing side.
