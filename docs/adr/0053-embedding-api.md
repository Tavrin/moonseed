# ADR 0053 — The embedding API: a 0.1 candidate

Status: accepted with Phase 3.31. The public API it describes is a 0.x candidate: public, documented, usable without internals, and allowed to change before 0.1. The user guide is docs/EMBEDDING.md.

## Context

Moonseed's host surface grew one proof at a time (inventory: Phase 3.31 notes). It has a sound core:
- stable native symbols;
- `NativeCall` borrows;
- owner-checked `Root`s;
- `Completion` with arbitrary error objects;
- host userdata with typed borrows and codecs;
- native closures (ADR 0035);
- builtin continuations (ADR 0031, ADR 0034);
- a replay journal (ADR 0005).

What it lacks:
- **A value model.** `HostValue` names objects by an unrooted `ObjectId`, and an id from one runtime can resolve to an unrelated object in another.
- **A typed API.** It has no typed values, conversions or error split.
- **Ways for a host to make native closures, for a native to call Lua, or for the host to call Lua.** The only host call into Lua is `call_closure`, which takes no arguments, returns one integer, and allocates a thread per call.
- **Integer-only legacy paths.** Waits are integer-keyed and completed by scanning every thread, and the journal records integers only.
- **No external-key userdata, no module resolver, and no single restore entry point that checks the host.**

A game host (Moss) needs all of it, through public API alone.

## Decision

### Threading

A `Runtime` is single-threaded and not reentrant: it is neither `Send` nor `Sync`. The one supported reentry is native→Lua through a continuation (below). The crate is `#![forbid(unsafe_code)]`. The API's contract is therefore that safe misuse gives an error, never a panic, a stale result, a duplicated effect, or a state a snapshot refuses.

### Values

- **`Value`** is owned. Its variants are:
  - primitives: `Nil`, `Boolean`, `Integer(i64)`, `Number(f64)`, `LightUserdata`;
  - objects: `String(LuaString)`, `Table(Table)`, `Function(Function)`, `Thread(Thread)`, `UserData(AnyUserData)`.
- **The object types are rooted references.** Each holds a slot in its runtime's root table (an `Rc` the runtime shares) and keeps its object alive until the last clone drops. Dropping needs no runtime: the slot is released in the shared table, and the next collection sees it gone. Cloning bumps a count.
- **Wrong runtime.** A reference used with a runtime other than its own is `ApiError::WrongRuntime`. That includes a runtime restored from a snapshot of its own runtime.
- **Ephemeral views.** `ValueRef<'cx>` and `StrRef<'cx>` borrow a callback context (`NativeContext`) or a `&Runtime`. Primitives and borrowed string bytes cost nothing. The borrow checker keeps them from outliving the borrow, and no collection runs while a callback's context is borrowed. They are how immediate callbacks avoid a root per argument.
- **Identity.** `ObjectId` is a logical identity, not a root.
  - `runtime.object(id)` returns a rooted `Value`, or `None` when no live object has that id (expected O(1); ADR 0052, Phase 3.31).
  - A host that kept an id across a snapshot reacquires its object this way after restore.
  - A root is never carried from one runtime instance to another.
- **Strings are bytes.**
  - `LuaString::as_bytes(&rt)` borrows them, and `to_str(&rt)` is a strict UTF-8 view that fails on invalid UTF-8.
  - No API converts lossily.

### Tables and functions

- **`Table` operations are named by what they may do:**
  - `raw_get`, `raw_set` and `raw_len` never run Lua;
  - `next` follows raw traversal order;
  - `get` and `set`, which may run `__index` and `__newindex`, are calls: they go through the call machinery below and can wait or error like any call.
- **Functions.** A `Function` is any callable function value: a Lua closure, native, native closure or builtin. `Function::kind` tells them apart only for introspection.

### Conversions

- `IntoLua`, `FromLua`, `IntoLuaMulti` and `FromLuaMulti`. Implementations cover:
  - `()`, `bool`, `i8`–`i64`, `u8`–`u64`, and `isize`/`usize`, range-checked (a `u64` past `i64::MAX` is an error, never wrapped);
  - `f32` and `f64`;
  - `Vec<u8>`, `&[u8]`, `String` and `&str` (strict UTF-8 when reading);
  - `Option<T>` (nil is `None`);
  - tuples up to 12, and `Value` and every rooted type;
  - `Variadic<T>` for open counts, and `MultiValue`, an exact sequence in which `nil` holes and a lone `nil` are kept.
- Strict by default. Lua's own coercions (a numeric string to a number, a number to a string) are opt-in wrappers: `Coerce<T>`.
- **A failed conversion** is `ConversionError { expected, actual: LuaType, position: Option<Position> }`, where the position is an argument or result index. A typed callback adapter turns it into Lua's own argument error ("bad argument #2 to 'f' (number expected, got string)").

### Errors

- One `Error` enum:
  - `Lua(LuaError)`, a catchable Lua error;
  - `Vm(VmError)`, a runtime or snapshot failure;
  - `Api(ApiError)`, misuse.
- **`LuaError`** holds the error object as a rooted `Value` (any type), its class (`LuaFault`), and a traceback when one was made. `Display` formats it as `lua.c` would, without changing the value.
- **Resource errors are Lua errors.** A memory limit or stack overflow caused by Lua execution is a `LuaError`, also when the host started the call.
- **`ApiError` covers misuse:**
  - `WrongRuntime`;
  - `Busy`, a top-level call while one runs;
  - `NotWaiting` and `AlreadyCompleted`;
  - `InvalidCallState`;
  - `Conversion`;
  - `Released`;
  - `UnknownSymbol`;
  - `Unregistered`.

  No `ApiError` becomes catchable in Lua, and no Lua error is reported as `ApiError`.

### Host functions

- **Registration.** `HostRegistry::function(symbol, policy, f)` registers `f: Fn(&mut NativeContext) -> Result<NativeReturn, Error>`, kept as `Rc<dyn Fn>` in the registry.
  - The registry is host state. Only the symbol is snapshot state, and restore requires every symbol a snapshot names before a runtime exists.
  - `HostRegistry::typed(symbol, policy, f)` adapts `Fn(&mut NativeContext, A) -> Result<R, Error>` for any `A: FromLuaMulti` and `R: IntoLuaMulti`.
- **`NativeContext`** (it generalizes `NativeCall`) gives:
  - arguments as `ValueRef`s, and the captured values of the native closure being called;
  - type checks, string bytes, raw table operations and metatables;
  - typed userdata borrows;
  - making strings, tables and userdata;
  - the effect id and journal when the policy is `External`.
- **What a callback returns (`NativeReturn`):**
  - `Return(MultiValue)` for its results;
  - `Error(Value)` to raise an error object;
  - `Wait(WaitRequest)` to wait on the host;
  - `CallLua { function, args, tag, keep }` to call Lua and continue (below).
- **Native closures.**
  - `NativeContext::make_closure(symbol, captures)` and `Runtime::make_closure` make a native closure over Lua values (ADR 0035).
  - Its captures are traced, snapshotted, and readable and writable through `captures()` and `set_capture(i, v)`.
  - Rust state a closure needs goes in host userdata it captures, so it follows that type's snapshot policy. There is no other host-state mechanism.
  - Restore accepts a host closure of a registered symbol with at most 255 captures.
- **Borrows.**
  - A mutable userdata borrow comes from the context and borrows it mutably.
  - A borrow guard cannot be held across `NativeReturn::CallLua`: the callback returns first, so the borrow ends before Lua runs.
  - This is a compile-time property, kept by a compile-fail doctest.

### Native → Lua continuations

When a callback returns `CallLua { function, args, tag, keep }`:
1. The VM pushes a `Boundary::Native { symbol, tag }` frame over the call's arguments. It keeps the `keep` values in the frame's slots, as builtin frames keep theirs (ADR 0031), and calls `function` with `args` as an ordinary call.
2. When the call returns or raises, the VM calls the same symbol's callback again, in a new step. `NativeContext::resumed()` gives `Some(Resume { tag, outcome })`, where the outcome is `Returned(MultiValue)` or `Errored(LuaError)`, with the kept values.
3. The callback can return results, raise, wait, or call again.

No Rust stack frame survives between the two invocations. The Lua call may yield (when the native's own frame may), wait on the host, or be checkpointed and restored. The continuation then resumes exactly once, from snapshot state: the symbol, the tag (a `u32`), and the kept values. A native's "own call" (`pcall`-like catching) is the `Errored` outcome: the frame catches errors from its call, as `load`'s frame does.

### Host → Lua calls

- **The call API:**
  - `Runtime::start_call(&function, args)` installs a host-call boundary on the main thread when it is idle, as `lua_pcall` on the main state does, and returns `Err(ApiError::Busy)` while another call or the chunk runs;
  - `Runtime::run(quantum, journal)` drives it, unchanged;
  - `Runtime::finish_call::<R>()` returns its results or its `LuaError`.
- **The convenience form.** `Runtime::call::<R>(&function, args, journal, fuel)` drives until the call completes, fails, waits, or uses `fuel`, and returns `CallOutcome::{Done(R), Waiting(WaitKey), OutOfFuel}`. It never blocks on a host operation.
- **Cost and state.** Calls on the main thread allocate no thread. A call in progress is snapshot state, and after restore the host goes on with `run` and `finish_call`. `call_closure` is deprecated.

### Host waits and effects

- **A wait.** A callback returns `Wait(WaitRequest { operation: symbol, payload: MultiValue })`. The VM allocates the `WaitKey` deterministically (from the effect sequence), records the key → thread association in a wait table (O(1) completion), and the run returns `StepOutcome::Waiting(key)`.
- **Completing it.** `Runtime::wait(key)` shows the operation and payload. `Runtime::complete(key, Completion)` returns values or raises an error object. A second completion is `ApiError::AlreadyCompleted`, and a key the runtime does not hold is `ApiError::NotWaiting`.
- **Snapshots.** Pending waits are snapshot state: after restore the same keys are pending and complete exactly once.
- **Effects.** A native's `NativePolicy` is `VmLocal` or `External`. External natives keep the journal: an effect id per call, and a recorded outcome replayed on re-execution. Journal records carry byte payloads, not only integers (the module resolver needs them).

### Host userdata policies

`HostUserdata::policy()` returns one of three policies:
- **`Portable`:** a codec, ADR 0045.
- **`Refuse`:** the snapshot is refused.
- **`Rebind`**, new. The snapshot keeps the type symbol and a bounded external key (`fn key(&self) -> Vec<u8>`, at most 4 KiB). Restore calls the type's `rebind(&key, &HostEnv) -> Result<Self, RebindError>`:
  - it looks the key up in the host environment, and its signature cannot reach the runtime, Lua, or the journal;
  - any failure aborts restore before a runtime exists.

  Rebinding is meant for an engine world, entity or asset id that names an object the host already has, not for reopening files or sockets.

### Modules

- **Resolution.** A host `ModuleResolver` capability answers `resolve(name) -> Resolved`, where `Resolved` is one of `Source(bytes)`, `Binary(bytes)`, `Native(symbol)` or `NotFound(diagnostic)`.
  - It is installed as a searcher in `package.searchers`, after the preload searcher, so `require` keeps Lua's protocol: a loader, its data, `package.loaded`.
  - A pure resolver (immutable, declared `Pure`) is called directly.
  - An `External` resolver is journaled: the first resolution of a name in a run is recorded, and replay returns the recorded bytes. A checkpoint and restore can therefore never fetch different source for the same `require`.

### Construction and restore

- **Building a runtime.**
  - `Runtime::builder()` takes:
    - `limits` and the configuration;
    - the `registry`;
    - the libraries;
    - optional capabilities: `output`, `warnings`, `entropy`, `module_resolver` and `host_env` (for rebinding);

    then `build()`.
  - `Runtime::load(chunk)` runs the main chunk; `Runtime::new(config, registry)` stays.
- **Restoring one.** `Runtime::restore(bytes, &Host)` is the single entry point, with `Host { registry, limits, env, capabilities }`. Before a runtime exists it checks:
  - the native symbols;
  - the host userdata types and their policies;
  - the codecs and rebinds;
  - the effect domain.
- **Required and optional host pieces.** A capability only a function's execution needs, such as an output sink for `print`, is optional: a snapshot holding `print` restores without one, and `print` then writes nowhere.
- **Older entry points.** `from_snapshot` and `from_snapshot_with_limits` stay as thin forms of `restore`.

### What stays internal

Arena handles, marks, ages, frames, the image and decoder types, `OwnerToken`, `BASE_FUNCTIONS`, and the proof natives (`USERDATA_NATIVES`, `ProofCounter` and `ProofHandle` move behind a `proof` feature). `HostValue`, `call_closure`, `complete_wait`, `global_integer`, `thread_integers` and the legacy `HostFn` path are deprecated.

## Alternatives

- **mlua's model, where every value is a registry reference.** It is simple, but costs a root per argument in every callback. The ephemeral views avoid that for the common case.
- **`Box<dyn FnMut>` as Lua function state.** Not serializable: it breaks checkpoints. Native closures over captured values do the same job and snapshot.
- **Native→Lua calls on the Rust stack.** Simple, but a checkpoint or yield inside would have to capture Rust frames. Continuations keep everything in VM state.
- **A thread per host call.** That is what `call_closure` does: an allocation and a garbage object every frame. The main thread serves, as in `lua_pcall`.

## Consequences

- Snapshot schema 22 (21: native continuation frames, wait payloads and completed wait keys, main-thread host calls, rebindable userdata; 22: a continuation keeps its external effect's sequence). Bytecode 12, tables 4, fuel 6, GC policy 12 and binary chunk format 2 are unchanged.
- Deprecated, with notes naming the replacement: `NativeCall`, `call_closure`, `complete_wait`, `global_integer`, `thread_integers`, `HostValue`, and the legacy `HostFn` path. Proof natives, fingerprints and measurement helpers are hidden from the documentation. `OwnerToken` is private. `missing_docs` covers the public API.
- Lookup by `ObjectId` is expected O(1) through a per-arena index made on the first lookup (Phase 3.31 notes in PERFORMANCE.md). An id from another runtime is just an absent or different object: identity across runtimes goes through rooted values, which know their runtime.
- A heap past its quota because a host grew its userdata cannot be checkpointed: `snapshot()` refuses it with `LimitExceeded`, as restore has refused it since ADR 0045. This corrects ADR 0052, which said such a heap restores under a larger quota.
- Ten examples (`crates/moonseed/examples/`), run by the workspace tests; 24 misuse tests asserting the exact error kind.
- A Moss integration harness (`integration/moss/`, a separate workspace on `moss_ecs` and `moss_runtime_abi` pinned to `dd8673cb1`) uses only this API. It shows that a Moss host can checkpoint its Lua VM mid-frame, with a pending wait and a continuation in flight, restore it into a fresh host with entities and assets rebound by key, and replay the same frames exactly.

### Review

One review round; its probe programs reproduced every claim. All five were fixed, each with a test that failed before:

- **Blocker:** a typed native's converted results sat in an untraced window, so a later conversion that allocated could collect an earlier result. The call failed with `ApiError::Released`, and the next snapshot found a dangling reference. Emergency collections now pin the window.
- **Blocker:** an external native that called Lua minted a new effect id when it resumed, and a memory failure there escaped. A checkpoint replay ran the host effect again. The continuation frame keeps its effect's sequence (schema 22), and resource failures unwind as Lua errors.
- **Major:** an oversized wait payload escaped as a `VmError` for `External` natives. Both policies now raise a catchable Lua memory error.
- **Major:** userdata grown past the quota made a snapshot that restore refused. `snapshot()` now refuses it (above).
- **Major:** host userdata allocations failed without collecting the handles the host had dropped. They now collect once first, as instructions do.

The reviewer also checked continuation snapshots at every step, the wait index, the restore registration checks, and the harness's replay comparison, and found nothing more there.
