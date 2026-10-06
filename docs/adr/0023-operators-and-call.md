# ADR 0023 — Operators, operator metamethods, and `__call`

## Context

Source compiled `+` (integers only), comparisons, unary minus and `#`. The rest of Lua 5.4's operators were rejected, and only `__index`, `__newindex` and `__len` existed. ADR 0019 made a metamethod call resumable for a handful of instructions. Every other operator needs the same thing:
- a primitive result when the operands allow one;
- otherwise a metamethod chosen by Lua's rules, run as an ordinary call that can pause, wait or be checkpointed;
- then the instruction finished exactly once.

`__call` is different, because it changes what an ordinary call calls.

## Decision

### One primitive authority per operator family

- **`arith.rs`** defines every arithmetic and bitwise operator on numbers:
  - Integer arithmetic wraps.
  - `//` and `%` round toward negative infinity. `mininteger // -1` is `mininteger` and `mininteger % -1` is 0.
  - Integer `//` or `%` by zero raises `DivideByZero` before any metamethod lookup, as in Lua.
  - `/` and `^` give floats, and `x ^ 2` is `x * x`, as PUC's `luai_numpow` has it.
  - Bitwise operands must be integers or floats with an exact integer value; anything else is a metamethod case.
  - A shift of 64 or more gives 0, and a negative count shifts the other way.
  - Arithmetic, but not bitwise operators, accepts strings that read as numbers, through the same parser as `tonumber` and numeric `for`. Lua 5.4 does this through the string library's metamethods, and it has no bitwise ones.
- **`compare.rs`** stays the authority for raw equality and for ordering two numbers or two strings.
- **`concat.rs`** defines `..` on strings and numbers, and the text of a number (below).

The hot tier's `hot_arith` and the common handler `op_arith` both call `arith::numbers`, so the tiers cannot disagree.

### Metamethod selection (`ops.rs`)

- **Arithmetic, bitwise and `..`:** the first operand's metamethod for the event, else the second's. It is called with both operands in their original order. With none, the fault the primitive named: `Arith`, `Bitwise`, `NoInteger` or `Concat`.
- **Unary `-` and `~`:** `__unm` and `__bnot`, called with the operand twice. `__len` gets the same dummy second argument; Phase 3.10 passed one, which a metamethod could observe.
- **`==`:** raw equality first. Two distinct tables then try the first's `__eq`, then the second's, and none means `false`. They do not need the same handler; that is Lua 5.4's rule, not 5.2's.
- **`~=`:** the negation of `==`.
- **`<` and `<=`:** primitive for two numbers or two strings. Anything else tries `__lt` or `__le` on the first operand, then the second, and none is `Compare`. `>` and `>=` swap their operands at compile time, so selection follows Lua.

`<=` never falls back to `not (b < a)` through `__lt`. That fallback is `LUA_COMPAT_LT_LE`, which Lua 5.4's stock Makefile turns on through `LUA_COMPAT_5_3`; the 5.4 manual lists it as removed. The oracle binary is therefore built without `LUA_COMPAT_5_3`, and the oracle test refuses one built with it.

Only tables have metatables, so a number or string operand never supplies a metamethod. The lookup takes any value, so per-type metatables can join it without changing selection.

### Completion policies

`MetaEvent` no longer names the operation. It names how the instruction finishes. The instruction at `pc` says which operation it was.

| Event | Finish | Used by |
|---|---|---|
| `Store { dst }` | first result to `dst` | `__index`, `__len`, arithmetic, bitwise, `__unm`, `__bnot`, `__concat` |
| `Truth { dst, negate }` | truth of the first result, as a boolean, to `dst`; negated for `~=` | `__eq`, `__lt`, `__le` |
| `NewIndex` | drop the result | `SetIndex`, `SetField` |
| `NewIndexAssign` | drop, step the assignment cursor | `AssignCommit` stores |

No results count as nil, so `Truth` gives `false`, or `true` for `~=`.

Restore checks the event against the instruction at `pc`, including its destination and whether it negates. It also checks the argument count:
- exactly 2 for `__index`, and 3 for `__newindex` and assignment stores, whose handlers are called only when they are functions;
- for the other events, at least 2 and at most 2 plus the `__call` bound, because a callable handler adds arguments.

Snapshot schema 7 encodes the events as tags 1 to 5.

### Callable values and `__call`

A `Call` whose callee is not a Lua or native function resolves it first (`resolve_callable`):
1. Raw-look up `__call` on the value.
2. Move the arguments up one slot and insert the value as the first argument.
3. Put `__call`'s value in the callee slot.
4. Repeat until the callee is a function.

This belongs to the `Call` instruction and completes within it; no state is left to resume. The call's result window is unchanged, so `__call` keeps the full result contract (none, fixed, or open), unlike one-result metamethods. The slot above the last argument is free: calls are compiled at the top of the live registers, as in PUC Lua.

A native callee finds its argument count in `top`. `call` sets `top` before a native runs, and `call_site` reads it. So a native reached through `__call`, including one prepared or waiting across a restore, sees the inserted argument. Restore checks that `top` covers the call's fixed arguments.

Metamethod handlers go through the same resolution, so a table with `__call` works as `__add`, `__len`, `__eq` and so on. `__index` and `__newindex` keep ADR 0018's rule: only a function is called, and any other value, a callable table included, is indexed or assigned into.

Resolution stops after 200 steps with `LuaFault::CallChain` (`MAX_CALL_CHAIN`). PUC Lua bounds `__call` only by its stack, about a million slots, and shifting the stack at every step makes a cycle quadratic. The PUC 5.4.9 reference did not finish a two-table cycle within two minutes. Moonseed's bound keeps a metamethod call's argument count within one byte and a cycle short.

### Instructions

- `Add` (tag 6) is still `+`, and its two-integer case stays inline in `hot_op`.
- `Arith { op, dst, a, b }` (tag 44) carries the eleven other binary operators.
- `BNot` (tag 45) is unary `~`, and `Concat` (tag 46) is `..`.
- `Neg`, `Compare` and `Len` keep their tags, with the semantics above.

Bytecode revision 7 covers these changes and `Len`'s second argument.

`..` compiles right-associatively as binary `Concat`s, with no bulk concatenation. Metamethod order is then Lua's: `a .. b .. c` handles `b .. c` first.

### The text of a number

Lua leaves it unspecified. Moonseed uses PUC's form:
- integers as `%d`;
- floats as `%.14g`, with `.0` appended when the text reads as an integer;
- `inf` and `-inf`.

The digits come from Rust's correctly rounded formatting, not the C library, so neither locale nor target changes them. Every NaN prints `nan`. PUC on glibc prints the NaN's sign (`-nan` for `0/0` on x86), and that sign depends on how the target produced the NaN.

### Hot tier

Primitive `Arith`, and `Add` on anything but two integers, first ran in the common tier. The benchmarks put each at about 45 ns there, against a few nanoseconds for hot `Add`. The two-number cases now run in the hot tier through one out-of-line helper, `hot_arith`, which reads and writes the registers itself. It declines numeric strings, metamethods, integer division by zero and non-integral bitwise operands, and the common handler then takes them. Keeping the register access in the helper left `run_hot` at 8,824 bytes, smaller than before. An inline version grew it to 9,902 bytes and slowed the empty loops by 16%.

### Call depth

A thread holds at most 1,000 frames (`MAX_CALL_DEPTH`), the same bound snapshot decoding already enforced. A Lua call past it, including a Lua metamethod, faults with `LuaFault::StackOverflow`. Before this there was no bound. Runaway recursion grew the frame and stack vectors until the host ran out of memory, and a review run hit exactly that with an `__eq` that compared two tables. PUC Lua raises "stack overflow" at its stack limit, about 200,000 Lua calls. Moonseed's bound is lower, and every running thread stays within what a checkpoint can hold.

### String size

`..` is the first operation that makes strings at run time. A result longer than 1 MiB (`heap::MAX_STRING_BYTES`, the snapshot's bound on string bytes) ends the run with `MemoryLimit`, so `s = s .. s` in a loop stops at 1 MiB instead of doubling until the host runs out of memory. Many live strings can still add up to much more than 1 MiB. A logical-byte quota over all objects is the GC track's next item.

## Alternatives

- **Resolve `__call` in a pending state, a new continuation.** The resolution never waits, so nothing needs resuming. Recording it would only add snapshot state.
- **Pass `__call`'s result through the one-result metamethod completion.** That would break multiple results, which only `__call` can return.
- **Support Lua 5.3's `__le` fallback because the stock 5.4 build has it.** The 5.4 manual removes it, and it cannot be taken away later without breaking programs.
- **Format numbers through `libc`.** That depends on locale and target, and wasm has no `printf`.

## Consequences

- **Faults:** errors are still unprotected. A fault inside a metamethod ends the run, and a faulting non-callable handler drops its scratch slots.
- **`pow`:** `^` uses the target's `pow`. A ten-case corpus matches bit-for-bit between native and wasm32, but arbitrary `^` is not part of the cross-target bit-exact profile.
- **Next:** error unwinding and `pcall` must unwind frames carrying a `MetaCall` of any of these kinds.
