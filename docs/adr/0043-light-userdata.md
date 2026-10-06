# ADR 0043 — Light userdata as identity tokens

## Context

In C a light userdata is a `void *`. Lua code cannot dereference it: it observes only identity (equality and table keys), its type, and the metatable all light userdata share. Moonseed runs on targets with different pointer widths, restores checkpoints in other processes, and never trusts a host address as state. `debug.upvalueid` needs a light userdata naming an upvalue cell.

## Decision

A light userdata is `Value::LightUserdata(LightDomain, u64)`: a domain and 64 bits, compared and hashed as a pair. It is not an object: it has no `ObjectId`, is never traced, and keeps nothing alive.

| Domain | Made by | Bits |
|---|---|---|
| `Host` | `NativeCall::light_userdata(HostLightKey(n))` | any `u64` the host picks |
| `Upvalue` | `debug.upvalueid` of a Lua closure | the upvalue cell's `ObjectId` |
| `NativeValue` | `debug.upvalueid` of a native closure | the closure's `ObjectId` << 8, plus the index |

- **Identity.** Equal domain and bits are equal; nothing else is. A host key never equals a VM token, whatever its bits. Moonseed promises only identity: the bits are not an address, and a host that maps keys to its resources keeps that map itself. An unsafe pointer adapter, if one is ever wanted, goes on top of this.
- **Equality.** Raw identity only: `__eq` is never called for light userdata, even when their metatable has one, as in Lua.
- **Metatable.** One for all light userdata, set with `debug.setmetatable`, in the per-type table (ADR 0034). Every other event goes through it.
- **Text.** `tostring` and `%p` print a host key as 16 hex digits and a VM token as its object id's 8, never an address. `__name` from the shared metatable applies, as in Lua's `luaL_tolstring`. An argument error names a light userdata `light userdata`, as `luaL_typeerror` does.
- **`debug.upvalueid`.** Closures sharing a cell get equal tokens, distinct cells different ones even with equal values, and a cell keeps its token when it closes and across `debug.upvaluejoin` and checkpoints, because the cell's id never changes. Ids are never reused, so a token outlives its cell without ever naming another. A builtin has no upvalues: `upvalueid` fails (`nil`), as Lua's does for a C function without upvalues; a native closure's values are named as Lua names a C closure's upvalues.
- **Snapshots.** A token is written as its domain and bits. Restore refuses an unknown domain, and a VM token naming an id the image has not reached (`0`, or at least `next_object_id`), or a native index past 15.

## Alternatives

- **Pointer bits as state.** Not portable between targets or processes, and an address in a checkpoint is a leak.
- **Light userdata as objects.** Lua does not collect them; a token that kept its cell alive would change what `upvalueid` means.

## Consequences

`Value` stays 16 bytes. The host sees a light userdata as `HostValue::LightUserdata(LightUserdata)`, an opaque copyable identity. Only host keys go back in: a VM token handed to a runtime is refused (`WaitError::InvalidValue`), since it names a cell of the runtime that made it and could equal a token of another's (found by the milestone review).
