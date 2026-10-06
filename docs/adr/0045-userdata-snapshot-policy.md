# ADR 0045 — Userdata in snapshots

## Context

A checkpoint must be restorable on another target, in another process. A byte payload is plain data. An arbitrary Rust value is not: it may hold a file descriptor, a lock, a GPU handle, or an address. A runtime that promises portable checkpoints cannot find that out while it serializes.

## Decision

Every host type declares how snapshots treat it, by how it is registered:

- **Portable codec** (`register_portable_userdata`, `PortableUserdata::encode` / `decode`). A snapshot records the type's symbol and its encoded bytes, at most 1 MiB. `encode` must be deterministic across targets. Restore looks the symbol up, requires it registered as portable, and decodes; `decode` sees untrusted bytes, cannot reach the runtime or Lua, and returns `None` to refuse them.
- **Refused** (`register_userdata`, the default for anything not plain data). A snapshot of a heap holding one fails with `SnapshotError::NonPortableUserdata` before anything is written: the value is never dropped, replaced with nil, or written as an address.

Snapshots write every object in the heap, reachable or not, so the rule covers every userdata still there. A host can collect first to drop those nothing reaches; a snapshot never collects behind its back.

**Image.** A full userdata is written as its id, its metatable's id, its user values, its payload (bytes, or symbol and codec bytes), and its charge. A light userdata is its domain and bits (ADR 0043). No `TypeId`, pointer, or arena index is written.

**Restore** refuses, before a runtime exists:
- an unknown payload or light domain tag;
- a host symbol not registered, or registered without a codec (`UnknownUserdataType`);
- codec bytes the type refuses (`UserdataDecode`), or a decoded value that declares more bytes than the image recorded (`UserdataCharge`, which the snapshot itself also refuses, so no checkpoint is written that restore cannot take);
- a byte payload whose charge is not its length, a metatable that is not a table, a user value that names nothing (`InvalidStructure`, `DanglingReference`);
- more user values or payload bytes than the bounds, payload charges that together pass the image's quota, or more objects than the image's object limit (`LimitExceeded`);
- a VM token naming an id that never existed (`InvalidStructure`).

Restore is transactional: host values are decoded into the heap being built, and any refusal drops that heap with everything decoded so far; no root, effect, or object survives a failed restore.

## Alternatives

- **Serializing whatever a type can serialize, silently skipping the rest.** A restored program would meet a nil where it had an object.
- **An external-resource policy (re-bind a key to a new resource on restore).** Files, sockets, and engine entities need it, with effect and rebinding semantics of their own; it is left for a later design.

## Consequences

**From the milestone review** (one round, by a separate reviewer):
- Blocker, fixed: natives could make userdata past `Config::max_objects`, which every other allocation honours; with a limit of 1,000, a loop made 3,000 and the heap reached 3,236 objects, and the snapshot restored. Userdata now count against the limit, and restore refuses a heap past it.
- Major, fixed: a host value grown without a reported charge snapshotted and then failed to restore. The snapshot now refuses it (`UserdataCharge`), and `Runtime::with_userdata_mut`, which had no way to report growth, recounts the charge itself (ADR 0044).
- Major, fixed: the host could hand a light userdata token the VM made to another runtime, where it named nothing or another cell. Only host keys come in now (ADR 0043).
- Minor, fixed: `setmetatable({})`, `rawget({})`, and `rawset({}, 1)` ran without Lua's "value expected" and "nil or table expected" errors; and a shrink of a host value's charge did not give back its debt.
- Minor, recorded: `debug.upvalueid` of a `string.gmatch` iterator fails at index 3, where Lua's iterator has three C upvalues; Moonseed's native closure keeps two values (ADR 0035).
- Held up: `__eq` for full and light userdata and every operator on both, `__index`/`__newindex` chains, `load` with a userdata environment, `__close`, the table library over userdata, the three debug functions against Lua 5.4.9, `__name` in argument errors, tracing and freeing, the hand-written `KeyView` equality against the derived one, and overflow in the charge arithmetic.

Snapshot schema 15. A portable value crosses native and wasm32 targets: the proof's waiting runtime, holding a host counter, byte payloads, light keys, and an `upvalueid` key, gives byte-identical snapshots on both and finishes the same from either.
