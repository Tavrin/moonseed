# ADR 0016 — Language-level indexing and `_ENV`

## Context

Source can now build tables, read and write fields, and use globals. Two Lua rules shape how that is compiled. A free name `x` means `_ENV.x`, and `_ENV` is an ordinary lexically scoped variable. And `t[k]`, `t[k] = v`, and every global access are the operations that `__index` and `__newindex` will later extend. If source compiled straight to raw table access, metatables would force a compiler and bytecode redesign.

## Decision

### Two kinds of table access

`GetTable` and `SetTable` stay raw: a direct lookup or insert, never a metamethod. Hand-built programs use them, constructors use them on their fresh table, and a future `rawget` / `rawset` will too.

Source uses four language-level operations, all defined in `index.rs`:

| Opcode | Meaning |
|---|---|
| `Index { dst, obj, key }` | `dst = obj[key]` |
| `GetField { dst, obj, name }` | `dst = obj.name`, `name` a string constant |
| `SetIndex { obj, key, src }` | `obj[key] = src` |
| `SetField { obj, name, src }` | `obj.name = src` |

Today they resolve only tables. A missing key reads nil, and a nil or NaN key reads nil. Writing nil deletes. Writing with a nil or NaN key faults with `NilKey` / `NanKey`. Indexing or assigning into anything but a table faults with the new `LuaFault::Index`. `AssignField`, the recorded destination of a parallel assignment, now stores through the same language-level set.

The future metamethod path is the negative space of the fast path. `__index` belongs where `get` finds no live entry or the value is not a table. `__newindex` belongs where `set` finds no live entry or the value is not a table. A metamethod can run Lua, so that path will be resumable and live in `exec`. The hot tier handles only table hits: a live key for a read, and a live key updated to a non-nil value for a write. Everything else declines to `exec` with nothing written. That covers a miss, a delete, a new key, an object key, and a non-table.

A string key is looked up through a borrowed `KeyView`, so reading a constant field or a global does not allocate. A new string key is allocated only when a `SetField` inserts it.

### `_ENV`

A compiled chunk has one capture, `_ENV`; the validator allows a chunk at most that one (hand-built programs have none). `Runtime::load_chunk` binds it, as a closed upvalue, to the runtime's globals table, which the runtime creates, roots, and snapshots already. Name resolution looks through locals and upvalues as before. When a name is not found anywhere, it is `_ENV.name`: the compiler resolves `_ENV` by the same rules and emits `GetField` or `SetField` on it. A local or parameter named `_ENV` shadows the chunk's, and closures capture whichever `_ENV` is in scope where they are defined. `local _ENV = { x = x }` reads the outer `x`, because a local is not visible in its own initializer. `_G` is not special.

### Constructors

`{ ... }` is `NewTable`, then each field in source order: evaluate, then store. List fields count from 1, independently of keyed fields. A final list field that is a call stores all its results with `SetList`. A nil result still takes its position. A call anywhere else, or in parentheses, gives one value. Lua leaves the order of constructor assignments unspecified; Moonseed's source order is a deterministic choice within that freedom. The table stays in a register throughout, so a checkpoint or collection mid-constructor sees a rooted, partly filled table.

### Assignment

One target stores directly: `SetIndex`, `SetField`, `Move`, or `SetUpvalue`. Several targets that are all locals or upvalues store from registers, right to left, as PUC does. Several targets with a table or global among them use `AssignLocal` / `AssignField` and `AssignCommit`. Every destination's table and key are recorded, left to right, before any value is evaluated. The commit then stores right to left, resumable between stores. That is how `i, t[i] = 2, 99` writes `t[1]`. An upvalue destination in such a statement is written to a temporary register by the commit and copied to the upvalue after it.

### Table key fixes

The same work fixed two table-key rules, so the tables revision is 2. The float 2^63 was a saturating cast away from the integer key `math.maxinteger`; it is now its own float key. A float key that normalizes to an integer is now stored with the integer as its key object, so `next` returns `2`, not `2.0`, as Lua does. An update now keeps the entry's original key object.

## Alternatives

Compile free names to a global-table opcode. That bypasses `_ENV` scoping and would need changing once `_ENV` or metatables matter.

Reuse `GetTable` / `SetTable` for source access. Their raw meaning would have to change, and hand programs and future `rawget` depend on it.

Compile every assignment through the pending-assignment instructions. That is correct, but a single `t.x = v` would cost two extra instructions and an extra dispatch.

## Consequences

Bytecode revision 5, tables revision 2, schema unchanged. `hot_op` now receives the table arena, the string arena, and the running prototype's constants, and writes only registers and table values. The table-hit bodies are `#[inline(never)]` helpers. With them inline, the size and timing of `run_hot` depended on the opcode count again; see `PERFORMANCE.md`. A future hot promotion with a non-trivial body should follow the same rule. Host functions are still registry symbols called by `CallHost`, not values, so a global cannot yet hold a native function. That is the gap before a standard library or an engine API can live in `_ENV`.
