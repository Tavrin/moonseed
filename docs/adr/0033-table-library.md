# ADR 0033 — The table library: resumable machines over semantic operations

## Context

Lua 5.4.9's `table` library is seven functions: `concat`, `insert`, `move`, `pack`, `remove`, `sort`, `unpack`. In PUC Lua each is a C loop over `lua_geti`, `lua_seti`, `luaL_len`, and comparisons. Any of those can call a metamethod, and `sort` calls an order function. `math.min` and `math.max` compare with `<`, which can call `__lt`.

Moonseed calls nothing on the Rust stack (ADR 0019, ADR 0024). A loop can run as long as its list, so it cannot run inside one unit of fuel. A checkpoint may fall anywhere in it, including inside a comparator or a metamethod.

PUC's `checktab` accepts a non-table whose metatable has the fields an operation needs: `__index` to read, `__newindex` to write, `__len` for the length.

Checked with Lua 5.4.9: no coroutine may yield across any of these calls. That covers `sort`'s order function, `__lt` under `sort` or `math.min`, and `__len` and `__index` under `insert`, `concat`, and `unpack`.

## Decision

### A machine per function

Each function, and `math.min` / `math.max`, is a `library::Work` value: a state machine over semantic operations.

The operations (`runtime/library.rs`) are:
- read `obj[i]`, into a scratch slot;
- write `obj[i] = v`;
- take `#obj`;
- `a < b`;
- `a == b`, for `table.move`'s overlap test;
- call `sort`'s order function.

Each operation goes through the VM's own code: `index::get`, `set_value`, `index::len`, and `ops::compare`. So it follows `__index`, `__newindex`, `__len`, `__lt`, and `__eq` exactly as instructions do.

Values the machine keeps live in scratch slots above its arguments, on the stack, where the collector and the snapshot already find them. The machine itself holds only integers and stages:
- `sort` keeps the pivot and two elements;
- `remove` keeps the removed value;
- `unpack` keeps its results;
- `concat` keeps its text as bytes, charged to the logical heap and counted by collections, as a reader's source is (GC policy 4).

The engine alternates two calls:
- `lib_next` asks the machine for its next operation;
- `feed` gives it the result.

An operation that needs no Lua call happens at once. One that does is made from a `Boundary::Builtin` frame with `Task::Lib`; when it returns, `finish_lib` feeds its result, according to the frame's `Wait`: which scratch slot, the length, or the truth.

A frame is pushed only when needed: for a Lua call, or when the function runs past one step. `table.insert(t, v)` on a plain table runs entirely in its `Call`.

### A builtin called by a builtin runs in the next step

A function the VM implements can call another: `__newindex = table.insert`, `__lt = math.max`, `__tostring = tostring`, `pcall(pcall, …)`. Made at once, each such call ran the next function's first step inside the step that asked for it, so a chain of them nested on the Rust stack a thousand levels deep, in one unit of fuel. The milestone review found this: a 1 MiB thread overflowed and aborted, and wasm32 trapped. The `tostring` case was there since Phase 3.21.

Now `builtin_call`, the calls of a base-function or library frame, and `call_protected`, `pcall`'s, leave such a call as `Pending::Deferred` on the calling frame. The function and its arguments are already at the call slot. The next `poll` step makes the call, as a step that costs one unit of fuel. So the Rust stack stays flat whatever the chain, each level costs fuel, and a checkpoint may fall at a deferred call.

Restore accepts `Deferred` only on the top frame of a running thread, and only on a boundary frame whose call slot holds a function the VM implements. A chain ends in the catchable "stack overflow" at the frame limit; Lua 5.4.9 says "C stack overflow".

### Fuel

A step runs at most 32 operations (`BATCH`). The first step runs in the function's `Call`. Each further step is a `poll` step and costs one unit of fuel and quantum, as a Phase 3.21 base-function step does, and so does each deferred call (fuel revision 3).

So a sort of 5,000 elements costs about 2,000 units. Quantum 1 moves through it, and a checkpoint may fall between any two steps.

Nothing is transactional, as in PUC Lua. An error part way leaves the writes already made. A checkpoint resumes after the last committed operation, never redoing one.

### Table-likeness

`table_like` is PUC's `checktab`. Only tables have metatables in Moonseed, so only a table passes. Once other types have metatables, the test checks for the needed fields.

`unpack` checks nothing, as in PUC.

### `sort`

`sort` is Lua 5.4.9's `auxsort` and `partition`, transcribed to 28 steps (`SortStep`):
- The comparisons are the same, in the same order, as long as the pivot is the middle one; the call counts of `corpus_table.lua` match PUC's.
- The invalid-order checks are PUC's two: `a[i] < P` at `up - 1`, and `P < a[j]` with `j < i`. They raise "invalid order function for sorting".
- The recursion is a stack of pending ranges. The smaller part is sorted first, so the stack holds at most one range per halving: `MAX_SORT_PENDING`, 40.
- The order function must be a function (a closure or a native), as in Lua. A callable table is refused.
- `n` must be below `INT_MAX`.
- Indices are `u32` with wrapping arithmetic, and every index stays within `1..=n`. A comparator that lies, or that changes the list, cannot make an access leave that range, or loop without end: every step makes progress, and fuel bounds the run.

PUC Lua picks a random pivot, from the clock and the time, after a partition that came out too unbalanced. Moonseed picks one from a hash of the range's bounds, so a sort does the same on every run. The order among equal elements is not specified in either.

### The other functions

They follow `ltablib.c`, including the order of their checks and operations.

- **`insert`:**
  - Takes `#t`, then checks the position (`1 <= pos <= #t + 1`), then shifts up from the end.
  - Any call with other than two or three arguments is an argument error.
- **`remove`:**
  - A position equal to `#t` skips the bounds check, and otherwise it must be in `1..=#t + 1`.
  - Reads the element, shifts down, clears the last.
- **`move`:**
  - Checks its integers and the wrap-around bounds before anything moves.
  - Copies backward when the ranges overlap in one table.
  - When a second table is given, `==`, possibly `__eq`, decides the direction.
- **`concat`:**
  - Takes strings and numbers; numbers are written as `..` writes them.
  - Its text is bounded by the 1 MiB string limit and the heap quota.
- **`pack`:**
  - A new table, filled `1..n` in order, then `n`.
- **`unpack`:**
  - Checks its count against the stack bound before reading anything. Past it, the error is "too many results to unpack".

Four error classes are new: `LengthType`, `ConcatValue`, `Unpack`, `OrderFunction`.

### Snapshots

`Task::Lib` is written as its `Wait` and its `Work`: stages as tags, counters as integers, the text of `concat`, and the sort's steps and pending ranges.

Restore refuses:
- a task that has not started, since a frame exists only after the first step;
- a `Wait::Get` beyond the scratch slots;
- counters outside the arguments or the stack bound;
- a sort index past `n + 1`, or more than 40 pending ranges;
- a `concat` text over 1 MiB.

When a library function raises, its frame gets its work back first. The unwind pops the frame a step later, and a checkpoint between the two must hold a valid task; the milestone's checkpoint walk found this.

### The length of a table

`#t` is the smallest border (ADR 0010). It was found by probing `1, 2, 3, …`, and counting the positive integer keys by a scan, so `table.insert(t, v)` on 5,000 elements cost 39 µs.

`Table` now keeps two derived fields, rebuilt on restore and never written:
- the count of positive integer keys;
- a prefix `1..=k` known to be present. A delete at or below `k` shortens it; `#` extends it.

The result is still the smallest border; a test compares it with the scan after 4,000 random inserts and deletes. Appending costs 0.8 µs. The tables revision does not change.

## Alternatives

- **Writing the table library in Lua.** It would be resumable at once, but yields would cross it, which Lua 5.4.9 does not allow, and it would be slower than a native loop.
- **`slice::sort_by` with a callback.** It cannot pause inside a comparison.
- **One step per operation.** A frame, and a step, for every `table.insert`.
- **Copying a machine into each step.** `concat`'s text and the sort's state would be copied every 32 operations.

## Consequences

**Compatibility.**
- `table.pack(...)`'s fields are in insertion order `1..n`, then `n`.
- A string is not table-like, since strings have no metatable yet.
- Argument error messages are Moonseed's.
- A list with holes may have another border than PUC's. Both are legal.

**Evidence.**
- Two fixtures give Lua 5.4.9's output under the quantum, checkpoint, and collection schedules, with a checkpoint at every step for the output.
- A generated corpus of 568 lines matches Lua 5.4.9.
- Waits inside comparators and metamethods restore.
- Fuel grows with the work.

**From the milestone review:**
- Builtins calling builtins nested on the Rust stack, and a thousand levels ran in one unit of fuel. They are now deferred calls, above.
- ADR 0032 overstated how many inputs go through the runtime in `math_bits_fingerprint`, and what the oracle tolerance is.
- A restore without `set_entropy` shifted later effect ids. `randomseed()` now takes its two sequence numbers either way.

**Revisions.**

| Revision | Value | Why |
|---|---|---|
| Snapshot schema | 12, with ADR 0032 | |
| Fuel | 3 | |
| GC policy | 4 | |
| Tables | 3 | unchanged |
| Bytecode | 11 | unchanged |
