# ADR 0015 — Numeric `for` state in ordinary registers

## Context

Numeric `for` is not `while` with a counter. Lua 5.4 chooses integer or float mode from the initial value and the step, converts a float limit for an integer loop, errors on a zero step, and never lets an integer loop wrap. The control variable the body sees is a copy: assigning to it does not change the next iteration. All of this has to survive a checkpoint at any instruction.

## Decision

A loop owns four consecutive registers, `base..=base + 3`. The compiler evaluates the initial value, the limit, and the step into the first three, once, left to right, before any loop local exists. It then declares them as hidden locals. The fourth is the control variable, the first local of the body scope.

`ForPrep { base, offset }` turns the three values into the loop's state, or jumps by `offset` past the loop when it runs zero times:

| Register | Integer loop | Float loop |
|---|---|---|
| `base` | index | index |
| `base + 1` | iterations left, the bits of a `u64` | limit |
| `base + 2` | step (integer) | step (float) |
| `base + 3` | control variable | control variable |

The step's subtype is the mode, so the mode is visible in the registers and in a snapshot. The count is private to `fornum`: nothing in the body reads the hidden registers.

Integer mode is chosen when the initial value and the step are integers, whatever the limit. A float limit is floored for a positive step and ceiled for a negative one. A limit beyond the `i64` range clamps to the end the loop runs toward, or skips the loop when it lies behind the start. NaN counts as below the range. The count is `(limit - init) / step` in unsigned arithmetic, and `-(step + 1) + 1` stands in for `-step` so `math.mininteger` is never negated. Every other combination is a float loop over the three values converted to floats. A string that reads as a number is accepted; it never selects integer mode. A zero step, `-0.0` included, is `LuaFault::ForZeroStep`. A value that is not a number is `LuaFault::ForValue`.

`ForLoop { base, offset }` decrements the count, or adds the step to a float index and compares with the limit. While the loop continues, it writes the index and the control variable and jumps back by `offset` to the first body instruction. Since the control variable is rewritten from the index, a body assignment to it lasts only until the next iteration.

The body is a scope. `emit_scope_exit` runs before `ForLoop`, so a closure that captured the control variable or a body local keeps that iteration's cell. `break` leaves the same way it leaves `while`. After the loop the hidden registers are free.

`fornum.rs` holds `prepare` and `advance`, the only implementation. `ForPrep` runs in `exec`. `ForLoop` touches only registers and runs in `hot_op`. If its registers are not a numeric-for state, `hot_op` declines and `exec` reports `VmError::Corrupt`.

Unary minus is `Op::Neg`: integers wrap and floats negate. Anything else is `LuaFault::Type`, and `__unm` belongs there later.

## Alternatives

Lower the loop to `while i <= limit do ...; i = i + step end`. That wraps at `math.maxinteger`, changes the iteration when the body assigns `i`, and ignores the mode and limit-conversion rules.

A dedicated snapshot object for loop state. Four registers already serialize, validate, and restore, and they need no new schema.

Keep the index in the visible variable and copy only on assignment. A write to the variable is an ordinary `Move`, and the compiler cannot tell whether the body assigns it.

## Consequences

Bytecode revision 4, snapshot schema unchanged. `ForLoop` in the hot tier cut integer and float loops from about 75 ns to 36 ns per iteration. That made `run_hot` 7,444 bytes. Generic `for` needs its own design: Lua 5.4 gives it a fourth value that is closed like a `<close>` variable when the loop ends, including on `break` and error.
