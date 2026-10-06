# ADR 0034 — The string library, and a metatable per basic type

## Context

After Phase 3.22 the official suite's largest blocker that needs no host capability was `string` (9 files). Lua 5.4.9's string library is 17 functions, plus one metatable that every string shares. Its `__index` is the `string` table, which is what makes `("x"):upper()` work. Its arithmetic metamethods are what make `"10" + 1` work: in Lua 5.4 the core does not convert strings to numbers for arithmetic, the string library does.

Until now only tables had metatables in Moonseed. The core converted numeric strings in arithmetic itself.

## Decision

### A metatable per basic type

`Heap::type_metatables` holds one optional metatable per Lua basic type, by its C tag (`LUA_TNIL` .. `LUA_TTHREAD`). Tables keep their own metatable, and userdata does not exist, so those entries stay empty; restore refuses anything in them.

`Heap::metatable_of(value)` is the one lookup: a table's own metatable, else its type's. `index::metamethod` uses it, and so every metamethod lookup does: indexing, `__newindex`, `__len`, arithmetic, comparison, concatenation, `__call`, `__close`, `__tostring`, `__name`, `getmetatable`, and the table library's `checktab`. A non-table value is indexed through its type's `__index` and assigned through its `__newindex`; `#` of a string never consults a metamethod, as in Lua.

The entries are GC roots, written to snapshots as table ids, and changes to the tables themselves are ordinary table changes. Only the string entry is set, by `Runtime::install_string`. `setmetatable` still takes tables only, as Lua's does; a later `debug.setmetatable` can set the other entries without a new mechanism.

### String arithmetic

The core no longer converts strings in arithmetic. The string metatable's `__add`, `__sub`, `__mul`, `__mod`, `__pow`, `__div`, `__idiv`, and `__unm` are builtins that do what `lstrlib.c`'s `arith` does: convert both operands with `tonumber`'s rules and compute, or else call the second operand's own metamethod, unless it is a string (`trymt`). So a program sees what Lua 5.4.9 shows: `"10" + 1` is `11` once the string library is installed, a changed `getmetatable("").__add` is used for every string, and a removed one makes `"40" + 2` an error. Bitwise operators never took strings, and still do not.

### The functions

All 17: `byte`, `char`, `dump` (ADR 0036), `find`, `format`, `gmatch` (ADR 0035), `gsub`, `len`, `lower`, `match`, `pack`, `packsize`, `unpack` (ADR 0038), `rep`, `reverse`, `sub`, `upper`. Patterns are ADR 0037.

- **Arguments:** a string argument may be a number, converted in its stack slot as `lua_tolstring` does, so later steps read the same string. Positions follow `posrelatI` and `getendpos`, with no negation that can overflow.
- **Errors** carry Lua's wording: `bad argument #2 to 'string.rep' (number expected, got no value)`, `malformed pattern (missing ']')`, `invalid conversion '%y' to 'format'`. The class is still Moonseed's (`LuaFault`), with new classes for the pattern, format, and pack errors. Lua names a function by how it was called; Moonseed uses its library name, as Lua does for a function called from `pcall`.
- **Locale:** character classes and `upper`/`lower` are the C locale's, for every byte; bytes past 127 are in no class. Moonseed fixes the locale; `os.setlocale` is not there to change it.
- **Limits:** a result past the 1 MiB string limit is found before it is made. `string.rep` raises Lua's "resulting string too large"; a result that grows past the limit (`gsub`, `format`, `pack`) raises the memory error. `string.byte` and `string.unpack` check the stack for their results first.

### `format`

`string.format` is a pure-Rust port of `str_format`, the same bytes as Lua 5.4.9 on glibc x86-64 in the C locale, on every target: integers by hand, decimal digits from Rust's exact float formatting (round half to even on the exact value, as glibc does), `%a` by hand. `tostring`'s `%.14g` now comes from the same formatter. Two deliberate differences:

- Every NaN prints as `nan`, whatever its sign bit, since the sign differs across targets (`tostring` already did this).
- `%p` never prints an address. A value with an object id prints what `tostring` shows after the type name (`0x%08x` of the id); a builtin prints `builtin: <symbol>`. A string of at most 40 bytes prints a token made from its bytes, since Lua keeps one copy of each short string and so gives equal short strings one address; a longer string prints its id. Numbers, booleans, and nil print `(null)`, as in Lua.

`%s` uses `__tostring` and `__name` as `luaL_tolstring` does.

### Stepping

A string function is a machine on the table library's engine (ADR 0033), with a loop of its own so the table functions' loop stays as it was:

- A result of known length (`sub`, `rep`, `reverse`, `lower`, `upper`) is built 4,096 bytes a step.
- The pattern, format, and pack engines do 256 units of work a step. A bulk operation (comparing a back reference, copying a long `%s`) is charged in one go and may leave a debt the next steps pay, so fuel does not depend on where a step ends.
- Each step after the first costs a unit of fuel, as for the table functions (fuel revision 4).
- The bytes a function has built so far are held in its frame, charged to the logical heap as they grow, and counted by collections (GC policy 5).

A string function that calls Lua (a `gsub` replacement, `%s`'s `__tostring`, a string metamethod falling back to the other operand's) makes the call from its frame. As in Lua 5.4.9, no coroutine may yield across those calls; a yield through the string metatable's own `__index`, or a `__add` written in Lua, is an ordinary metamethod call and may.

## Alternatives

- **Special-casing strings in `GetField` and arithmetic.** A second path to keep in step with the first, and nothing for `debug.setmetatable`.
- **Keeping the core's string arithmetic.** Programs that change the string metatable's `__add` would see Lua 5.3's behaviour, and the suite's `bwcoercion.lua` fails.
- **Calling the C library's `printf`.** Output would differ across targets and locales.
- **Addresses for `%p`.** A run would differ from its replay.

## Consequences

**Revisions.** Snapshot schema 13 (type metatables, native closures, string tasks, new error classes). Fuel revision 4 (string steps; string arithmetic through metamethod calls). GC policy 5 (held string buffers; native closures have a logical size). Bytecode 11 and tables 3 are unchanged.

**Compatibility.** `LUA_COMPATIBILITY.md` lists what differs: error positions, `%p`, NaN signs, the 1 MiB limit, and the C locale.

**Evidence.** Five fixtures give Lua 5.4.9's output under the quantum, collection, and checkpoint schedules; four generated corpora, 7,890 lines, match Lua 5.4.9 line for line; the engines' own differential runs compared 80,324 pattern cases, 64,820 format cases, and about 253,000 pack cases with the oracle.

**From the milestone review** (one round, by a separate reviewer):
- A native closure was not equal to itself: raw equality had no case for it. Fixed; ADR 0035's identity rule now holds, and a proof checks it.
- A `gsub` string replacement grew its result without bound before the limit was checked: `%0` repeated half a million times over a 512 KiB match asked the host for 2 GiB. The expansion now stops before a piece would pass the string limit or the quota, and raises the memory error.
- A snapshot changed to give `gsub` a replacement no call could pass, or a formatter a state no run makes, restored and then ended in a host error. Those paths now raise a Lua error.
- `string.byte` and `string.unpack` past the stack now use Lua's wording, "stack overflow (...)".
- A string metamethod called directly with one argument now reads it twice, as Lua's C code does (`getmetatable("").__unm("5")` is -5).
- Not fixed: the capture strings of one match (at most 32, each at most the subject) are copied in one step without charging the step's work budget. They are charged to the heap, and bounded by the string limit.

