# ADR 0035 — Native closures: functions the VM implements, with state of their own

## Context

`string.gmatch` returns an iterator: a function, `type` `function`, that keeps its subject, its pattern, and where the next match starts, and moves on each time it is called. PUC Lua makes it a C closure over a userdata. Moonseed has neither C closures nor userdata, and its natives (`Value::Native`) are symbols, the same value every time, with no state.

A callable table would not do: `type(string.gmatch(...))` must be `function`.

## Decision

`Value::NativeClosure` is a function value backed by a heap object, `NativeClosureObj`:

- **`native`:** the builtin that runs it, as an index into the runtime's native symbols, so a snapshot names it by symbol;
- **`values`:** Lua values it keeps alive, traced by the collector and written to snapshots by id;
- **`state`:** a few integers it keeps between calls.

It is an object with an id (`Kind::NativeClosure`): `type` gives `function`, `tostring` gives `function: 0x...`, two of them are equal only if they are the same object, it can be a table key, and it is collected once unreachable. Its logical size is one object and a reference per value and number (GC policy 5).

A call of one runs its builtin (`Runtime::callable` maps it to its native on every call path), which finds the closure in the call slot, just as a native finds its arguments. So the call machinery, deferred builtin calls (ADR 0033), tail calls, `pcall`, and the metamethod paths needed no change of their own.

Only builtins make native closures; the host cannot yet. Restore accepts a closure only for a builtin that says it makes that shape (`library::closure_fits`): for `gmatch`'s iterator, two strings and three integers within the subject.

### `gmatch`'s iterator

The closure keeps the subject and the pattern as values, and in `state` the next start, the end of the last match (-1 for none), and whether it is exhausted. A call copies that into a matcher (ADR 0037), runs it in its frame, possibly over several steps and checkpoints, and writes the new position back only when the call ends. A call that fails leaves the closure as it was, as in Lua: calling it again scans from the same place.

## Alternatives

- **A userdata type.** Userdata is a non-goal, and would bring its own metatables, finalizers, and user values.
- **A Lua closure over a hidden table.** It would be a Lua function, but its state would be visible to `debug` later and could be changed by the program, and each call would run Lua code.
- **A host closure (`Box<dyn FnMut>`).** Not serializable, and not the same on native and wasm32.

## Consequences

- Snapshot schema 13 has a section for native closures and a value tag for them.
- A later embedding API can let a host make native closures over values it gives, from this representation, once it defines what a host builtin may keep.
