# ADR 0042 — Full userdata as heap objects

## Context

Lua's eighth basic type has two forms. A full userdata is a block of host memory that Lua cannot read, with its own metatable and a fixed number of user values; a light userdata is a bare pointer (ADR 0043). Lua's `type` calls both `userdata`. Moonseed had neither, so a host could not hand Lua an object of its own, and the official suite stopped where it needed one (`debug.upvalueid`, `debug.getuservalue`).

## Decision

A full userdata is `Value::Userdata(Handle<UserdataObj>)`, an object in an arena of its own, like a table:

```text
UserdataObj {
    id:          ObjectId            identity: equality, keys, tostring, snapshots
    metatable:   Option<table>       its own, or none
    user_values: [Value; n]          fixed at creation, nil until set
    payload:     Bytes(Box<[u8]>) | Host { symbol, value }   (ADR 0044)
    charge:      u64                 logical bytes the payload counts
}
```

- **Identity.** Each one is a distinct object. Equality without `__eq`, table keys, `tostring`, and `%p` use its `ObjectId`, never an arena index or an address, so identity survives collection, checkpoint, and restore on any target. Assignment and calls copy the handle.
- **User values.** `n` is fixed when the userdata is made, at most 65,534 (Lua's API takes fewer than `USHRT_MAX`). They start nil, hold any value, are traced, and are written to snapshots. Lua reaches them only through `debug.getuservalue` and `debug.setuservalue`, which follow Lua 5.4.9 exactly, down to the `(int)` cast of the index.
- **Metatable.** A full userdata has its own, as a table does. `Heap::metatable_of`, the single authority for metamethod lookup, now asks a table for its own, a full userdata for its own, and any other value for its type's. No operator has a userdata case: every event (`__index` through `__close`, `__name` in `tostring` and argument errors) works through that lookup. `getmetatable` honours `__metatable`; `setmetatable` stays table-only, as in Lua; `debug.setmetatable` sets a full userdata's own metatable.
- **Equality.** `==` on two distinct full userdata tries the first's `__eq`, then the second's, as for tables (`ops::compare`).
- **Bytes.** A byte payload is zeroed when made, at most 1 MiB (the longest string), and Lua source cannot read or write it. The host reads it through `NativeCall::userdata_bytes` and `Runtime::with_userdata_bytes`, borrows that end with the call or the closure.
- **Cost.** A userdata is an object under `Config::max_objects`, and counts 32 logical bytes, 16 for each user value, and its payload's charge: a byte payload its length, a host value what its type declares (ADR 0044). The object limit and the quota are checked before anything is made, so a refused byte payload is never allocated. This is GC policy 8.
- **Collection.** The metatable and the user values are a userdata's only edges. When it dies, its slot, user values, bytes, and host value go. No Lua code runs: finalizers (`__gc`) are not implemented (Phase 3.27), and a metatable may hold `__gc` without effect. Nothing here assumes when an object is marked for finalization, so Lua's rule (marked when a metatable that already has `__gc` is set) can be added.

## Alternatives

- **The payload inside the value, or `Box<dyn Any>` in `Value`.** Makes every value bigger, puts host types on every VM path, and leaves snapshots to find out what they hold. `Value` stays 16 bytes, which a compile-time assertion checks.
- **Charging only the handle.** A program could pass the heap quota with a few large userdata.

## Consequences

Snapshot schema 15 and GC policy 8. Bytecode 11, tables 3, fuel 4, and binary chunks 2 are unchanged: a userdata is runtime state, never in dumped code. The type metatable of full userdata does not exist; restore refuses one.
