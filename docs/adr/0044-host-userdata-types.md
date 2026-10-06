# ADR 0044 — Host userdata types

## Context

Bytes alone do not make an embedding runtime: a host wants to give Lua a Rust value (a counter, a transform, an entity) and get it back in its native functions with its type checked. That must not open a way around the heap quota, the sandbox, or checkpoints, and must not let a borrowed Rust reference survive a step.

## Decision

- **Registry.** A host type implements `HostUserdata`: a stable `SYMBOL` and `logical_size(&self)`. It is registered with `HostRegistry::register_userdata::<T>()` or, with a codec, `register_portable_userdata::<T>()` (ADR 0045). The registry keeps the symbol, the `TypeId`, and the codec. The `TypeId` checks a value at run time and is never written anywhere; snapshots use the symbol. A native can make a host userdata only of a registered type (`UserdataError::Unregistered`).
- **Ownership.** The Rust value is owned by its userdata object: `Payload::Host { symbol, value: Box<dyn Any> }`. The arena is the store; nothing else refers to the value, so nothing can dangle when it goes. Every access downcasts in `userdata.rs`, checked by `TypeId`; the VM never names a host type.
- **Access.** Inside a native function, `NativeCall::userdata_ref::<T>` and `userdata_mut::<T>` lend the value for as long as they borrow the call. A native function cannot call Lua, and while the borrow lives nothing else on the call compiles, a second borrow included (a `compile_fail` test shows both). The borrow cannot outlive the call, and a waiting call keeps none. Between steps, `Runtime::with_userdata::<T>` and `with_userdata_mut::<T>` lend it to a closure while they borrow the runtime. A value of another type is `None`; `NativeCall::type_error` raises Lua's argument error naming the expected type (`bad argument #1 to 'get' (example.Counter expected, got userdata)`).
- **Methods.** No special dispatch. A host type's methods are native functions in an ordinary `__index` table, called with the userdata as `self`. A metatable belongs to each userdata, not to the Rust type: a host may give its values one metatable, and Lua can still replace any one's.
- **Charge.** A host value counts what `logical_size` declares when it is made, besides the userdata itself, checked against the object limit and the quota first. Moonseed cannot see a Rust value's own allocations: a native that grows a value must report it with `NativeCall::set_userdata_charge`, which refuses growth past the quota before counting it. `Runtime::with_userdata_mut` re-reads `logical_size` after its closure and counts the difference, past the quota if need be, since the host has already made it. A value that declares more than it was charged is refused by the snapshot (`UserdataCharge`), as restore would refuse it.
- **No hidden edges.** The collector cannot see inside a host value. A value that must keep Lua values alive keeps them in its user values, or in host roots; it must not hold Moonseed handles of its own.
- **Rust `Drop` is not `__gc`.** A host value is dropped when the collector frees its userdata or the runtime goes, at a point no Lua program can observe, in no order Lua defines, without resurrection or warnings. Deterministic hosts must not use `Drop` for cleanup Lua can observe; that waits for finalizers (Phase 3.27) and the effect architecture.

## Alternatives

- **A separate host-object store indexed by id.** A second table to keep in step with the arena and to sweep; owning the value in its object gives the same isolation with nothing to dangle.
- **Runtime borrow flags.** Needed only if natives could reenter Lua; they cannot, so the borrow checker suffices.
- **One metatable per Rust type.** Lua's userdata have per-object metatables; a type default can be built on top.

## Consequences

This is an unstable API (Phase 3.33 stabilizes embedding). External resources that must be re-bound after a restore (files, sockets, engine entities) need their own design and are not covered.
