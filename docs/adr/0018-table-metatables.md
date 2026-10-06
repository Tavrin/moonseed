# ADR 0018 — Table metatables and raw metamethod lookup

## Context

Metamethods are the next layer of Lua semantics, and every later one uses the same pieces: where a metatable lives, how a metamethod is found, and how scripts set one. Only tables get metatables in this step.

## Decision

A table object has `metatable: Option<Handle<TableObj>>`. Tables can share a metatable, a table can be its own metatable, and cycles through metatables are ordinary graph state. The collector traces the reference as a strong edge, so an unreferenced table and its metatable die together, cycles included. A snapshot writes it as the metatable's `ObjectId`, or 0 for none. Restore checks that it names a table.

`index::metamethod(value, event)` is the only lookup. It reads the event name from the metatable raw, as a borrowed string key: no string object is allocated and `__index` is never consulted. There is no cache. Changing a metatable's fields changes behavior on the next access, and the mutation fixture is the oracle for any cache added later.

`setmetatable`, `getmetatable`, `rawget`, `rawset`, and `rawlen` are native functions (`base.*` symbols, `VmLocal`), registered by `register_base` and bound with `Runtime::install_base`.

- `setmetatable(t, mt)` takes a table and a table or nil, and returns `t`. It refuses to change a metatable that has a raw `__metatable` field.
- `getmetatable(t)` returns that field if it is present, otherwise the metatable.
- The raw functions bypass metamethods and keep the ordinary key rules.

Arguments of the wrong type fault with `LuaFault::Native`. Lua raises "bad argument" errors, and those messages are not reproduced.

`NativeCall` gains what these functions need:

- `arg` returns a `NativeValue<'a>`, tied to the call so it cannot outlive it, and no collection runs during a call;
- `type_of`;
- `string_bytes`, borrowed, any bytes;
- `raw_get`, `raw_set`, and `raw_len`;
- `metatable` and `set_metatable`, both without protection;
- `push` for any value.

The scalar helpers are unchanged, and a native that reads two integers costs what it did.

## Alternatives

A metamethod-presence cache in each table or metatable. That adds invalidation work before any measurement asks for it.

Type-wide metatables now (strings, numbers, functions). Nothing in this step needs them, and a string metatable belongs with a string library.

## Consequences

Tables revision 3 and snapshot schema 5. A non-table value still has no metatable: indexing it faults, `getmetatable` returns nil, and `#` works only on strings and tables. `__metatable` protection has no bypass until a debug library exists.
