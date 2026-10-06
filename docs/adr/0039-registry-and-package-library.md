# ADR 0039 — The Lua registry, one module registry, and `package`/`require`

## Context

After Phase 3.23 the official suite's largest blocker was `require`: 17 of 33 files stop on it, 13 of them on `require "debug"`. Lua 5.4.9 keeps loaded modules in its registry, a table that C code reaches at `LUA_REGISTRYINDEX`: `package.loaded` is the registry's `_LOADED`, and `package.preload` its `_PRELOAD`. `luaL_requiref` stores each standard library there as it opens it, so `require "string"` returns the `string` global itself, and `debug.getregistry()` returns the registry.

Moonseed had no registry. Its installers put tables in the globals only.

## Decision

### The registry

The registry is an ordinary table (`Heap::registry`), made the first time something needs it (an installer, `require`, `debug.getregistry`) and a GC root from then on. Snapshots carry it (schema 14). It holds what Lua's does:
- `[1]`: the main thread (`LUA_RIDX_MAINTHREAD`);
- `[2]`: the globals (`LUA_RIDX_GLOBALS`);
- `_LOADED` and `_PRELOAD`, made as `luaL_getsubtable` makes them: read, and replaced by a new table when not a table.

Moonseed's own invariants are never registry fields. The globals, the type metatables, library state, and host values stay typed fields of the heap. A program that changes the registry changes what Lua code, `require`, and `package.loaded` see, and nothing the VM relies on. One consequence differs from Lua: `load` binds the heap's globals, so replacing `registry[2]` does not change the environment of new chunks.

### One registration authority

`Runtime::register_module(name, table)` stores a library table in `_LOADED[name]`, raw. Every installer calls it: `install_base` for `_G`, `install_package`, `install_math`, `install_table`, `install_string`, `install_debug`. So the global and `_LOADED[name]` are the same table in any install order, with any subset installed. A library that is not installed is not loaded either: in a sandbox without `string`, `require "string"` fails.

### `package`

`install_package` installs:
- `package.loaded` and `package.preload`: the registry's `_LOADED` and `_PRELOAD`, the same objects;
- `package.searchers`: one entry, the preload searcher;
- `package.config`: Lua's `"/\n;\n?\n!\n-\n"`;
- `package.path` and `package.cpath`: empty strings, so code that reads or appends to them works;
- `require`: a native closure (ADR 0035) whose one value is the `package` table, as Lua's `require` is a C closure over it. It reads `searchers` from that table, not from the global `package`.

There is no file searcher, no C searcher, no `loadlib`, and no `searchpath`: Moonseed reads no files. A host that wants modules puts loaders in `package.preload`, or adds searchers.

### `require`

`require(name)` follows Lua 5.4.9's `ll_require`, `findloader`, and `searcher_preload`:
1. The name must be a string; a number is converted, as `luaL_checkstring` does.
2. `_LOADED[name]` is read, with metamethods, as `lua_getfield` reads it. If it is true, it is the only result.
3. `package.searchers` is read with metamethods, and must be a table ("'package.searchers' must be a table"). Its entries are read raw, from 1 on, until one is nil.
4. Each searcher is called with the name. A function result means the loader was found, and its second result is the loader data. A string result is added to the message. Anything else is ignored.
5. If no searcher finds a loader: "module 'name' not found:" and the searchers' strings.
6. The loader is called with the name and the loader data. A non-nil first result is stored in `_LOADED[name]`. If `_LOADED[name]` is then still nil, `true` is stored.
7. The results are `_LOADED[name]` and the loader data.

The preload searcher reads `_PRELOAD[name]` and returns it with `":preload:"`, or "\n\tno field package.preload['name']" when it is nil.

`require` and the preload searcher are machines on the library engine (ADR 0033): each read, write, and call is a step, the values between steps sit in the frame's scratch slots, and the searchers' message so far is part of the machine's state, charged to the logical heap (GC policy 6). A checkpoint in the middle restores the machine where it was, so a searcher or loader is never called twice. As in Lua 5.4.9, no coroutine may yield across a searcher or a loader ("attempt to yield across a C-call boundary"); pauses, host waits, and checkpoints may.

### A host module resolver

The milestone allowed an optional host capability that resolves module names to source. It is not in this phase. Reading a module from the host is an external effect: a replay must see the same bytes, so the resolver needs the journal's effect protocol (ADR 0005), and a design for what a checkpoint taken during a resolution holds. `package.preload` covers hosts that know their modules at boot.

## Alternatives

- **A Rust-side table of loaded modules.** A second authority to keep in step with `package.loaded`, which Lua code replaces and changes freely.
- **The registry as the home of the VM's own state.** A program could then break the VM through `debug.getregistry`. Lua can afford that; its C code checks types everywhere. Moonseed keeps typed state typed.
- **A file searcher over a virtual filesystem.** A capability with no host behind it would be a fake.

## Consequences

**Revisions.** Snapshot schema 14 (the registry, package tasks, and the error class `Require`). GC policy 6 (a `require` task's message counts against the heap). Bytecode 11, tables 3, and fuel 4 are unchanged: `require`'s steps follow the fuel rule of ADR 0033.

**Evidence.** A corpus of 51 lines (`corpus_package.lua`: preload, loader results nil, false, and several values, loaded modules, custom searchers, their strings and ignored results, errors, metamethods on `package.loaded`, replaced `package` and `package.loaded`, nested `require`) gives Lua 5.4.9's output, and keeps it with a collection, a checkpoint, and a restore at every step. Three install orders and a partial sandbox agree on module identity.

**Suite.** No file stops on `require` any more; the 13 that needed `debug` go on to the blockers recorded in ADR 0040. Five files now stop on a module that is a suite file (`bwcoercion` in `bitwise.lua`, `tracegc` in `cstack.lua` and `locals.lua`) or a library Moonseed does not have (`io` in `attrib.lua`, `utf8` in `utf8.lua`).
