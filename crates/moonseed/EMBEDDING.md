# Embedding Moonseed

Moonseed's rooted embedding API is the 0.1 release surface. Deprecated integer-only APIs and doc-hidden proof/measurement fixtures remain workspace tooling and are excluded from that surface. See [ADR 0053](https://github.com/Tavrin/moonseed/blob/main/docs/adr/0053-embedding-api.md) for its contract and [ADR 0052](https://github.com/Tavrin/moonseed/blob/main/docs/adr/0052-scalability-envelope.md) for the resource model. The crate forbids unsafe code.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `native-host` | Yes (`default = ["native-host"]`) | Compiles native filesystem, stream, clock, environment and process adapters. Grants no authority until attached. Builds on Wasm but does not supply a native OS there. |
| `counters` | No | Unstable runtime diagnostics and JSON reporting through optional `serde_json`; independent of `native-host` and excluded from snapshots. |

Disabling default features keeps the portable VM, in-memory filesystem and mock
capabilities. `Libraries` selects Lua functions at runtime, independently of
Cargo features. The library MSRV is Rust 1.88; minor 0.x releases may raise it
with a changelog entry. See the compatibility policy for version boundaries.

## Runtime lifecycle

`Runtime::builder()` starts with default limits, an empty registry, and no libraries. Select `Libraries`, register callbacks and host userdata, and supply optional capabilities before `build()`. `Runtime::new(config, registry)` builds the same idle runtime. Compile bytes with `compile` or `compile_with_limits`, then `load_main(&chunk)` and `run(quantum, &mut journal)`. `load_main` preserves globals and the main thread. A running chunk or unfinished host call prevents another top-level load or call.

`run` reports completion, a catchable Lua error, a host wait, a Lua yield, a fuel pause, termination, or an exit request. Check the outcome. On completion, `result_values` returns rooted values. Drive `begin_close` through pauses and waits before dropping a finished runtime when Lua cleanup is needed. A root can outlive its runtime as a Rust value, but cannot be used with another runtime.

[hello_call](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/hello_call.rs) loads a Lua function and calls it with typed arguments and results. All ten examples assert their results; `cargo run -p moonseed --example hello_call` runs one. `cargo test --workspace` runs their test entry points in CI.

## Values, lifetimes, and roots

`Value` owns primitives or rooted objects: `LuaString`, `Table`, `Function`, `Thread`, and `AnyUserData`. Clones keep the same object alive. The last drop releases the shared root; a later collection can reclaim the object. Dropping a root does not require borrowing the runtime. `ValueRef` and `StrRef` borrow the runtime or native context. Convert a view to an owned value before a call that may collect.

This attempt to hold an ephemeral view across execution fails at compilation:

```compile_fail,E0499
use moonseed::{Runtime, Journal};
let mut rt = Runtime::builder().build().unwrap();
let s = rt.create_string(b"held").unwrap();
let view = rt.object_ref(s.id()).unwrap();
rt.run(100, &mut Journal::new()).unwrap();
assert!(!view.is_nil());
```

Rooted objects belong to exactly one runtime. Restore creates a new runtime identity. Using the original root in that runtime or any other runtime yields `ApiError::WrongRuntime`:

```rust
use moonseed::{Runtime, Error, ApiError};
let mut first = Runtime::builder().build().unwrap();
let second = Runtime::builder().build().unwrap();
let table = first.create_table().unwrap();
assert!(matches!(table.raw_len(&second), Err(Error::Api(ApiError::WrongRuntime))));
```

`ObjectId` is a logical identity, preserved across snapshots. It keeps nothing alive. `runtime.object(id)` reacquires a root when the object still exists and returns `None` after collection. A freed id never resolves to the object that reused its slot. Store objects in reachable Lua tables or keep owned roots. This attempt to pass an id as a root fails at compilation:

```compile_fail,E0599
use moonseed::{Runtime, IntoLua};
let mut rt = Runtime::builder().build().unwrap();
let object = rt.create_table().unwrap();
let id = object.id();
id.into_lua(&mut rt).unwrap();
```

## Strings and tables

Strings are bytes, including NUL and non-UTF-8 bytes. `LuaString::as_bytes` borrows exact bytes. `to_str` and conversion to Rust `String` refuse invalid UTF-8 with `ApiError::Conversion`; nothing converts lossily. Use `Vec<u8>` for arbitrary bytes. [tables_and_strings](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/tables_and_strings.rs) demonstrates the round trip.

`raw_get`, `raw_set`, `raw_len`, and `next` do not run Lua. Table `get` and `set` may invoke metamethods and use the host-call machinery with a journal and fuel bound. They may wait or run out of fuel. Inside a Rust callback, `NativeContext::raw_get` is synchronous and raw: it cannot invoke `__index`. To honor Lua indexing, supply a Lua accessor such as `function(t, key) return t[key] end` and return `cx.call_lua(accessor, (table, key), tag, keep)?`. Handle `ResumeOutcome::Returned` and `Errored` when the native resumes. This runs ordinary Lua indexing, including chained metatables and host waits, without retaining a Rust stack frame. Outside a callback, use `Table::get` and drive its `CallOutcome`.

Plain `f64` accepts Lua integer/number values and rejects numeric strings; `Coerce<f64>` explicitly parses them with Lua's conversion rules. This applies equally to typed callback arguments and table fields. For example:

```rust
use moonseed::{ApiError, Coerce, Error, Runtime};
let mut rt = Runtime::builder().build().unwrap();
let table = rt.create_table().unwrap();
table.raw_set(&mut rt, "amount", "42.5").unwrap();
assert!(matches!(table.raw_get::<_, f64>(&mut rt, "amount"),
    Err(Error::Api(ApiError::Conversion(_)))));
let Coerce(amount): Coerce<f64> = table.raw_get(&mut rt, "amount").unwrap();
assert_eq!(amount, 42.5);
```

Rust integer conversions check ranges; `u64` above `i64::MAX` and an integer outside `u8` both yield `ApiError::Conversion`. `Coerce<T>` opts into Lua coercions. `MultiValue` keeps nil holes and distinguishes zero results from one nil. Tuples convert fixed sequences; `Variadic<T>` converts an open sequence.

## Errors

`Error::Lua` carries a rooted error object, `LuaFault`, and a traceback when present. Lua may raise any value. `Error::Vm` covers execution integrity and snapshot failures. `Error::Api` covers host misuse, including wrong ownership, invalid call state, missing registration, and failed conversion. `run` retains its low-level signature: a callback API failure is `VmError::Api`, and an uncaught Lua failure is `StepOutcome::LuaError`. Converting `VmError` into `Error` preserves the API category. Lua resource failures are catchable Lua errors, including for host-started calls. `snapshot` returns `SnapshotError`; `restore` wraps decode and registration failures in `Error::Vm(VmError::Snapshot(...))`.

## Host functions and host-to-Lua calls

Register `HostRegistry::typed(symbol, policy, callback)` for typed arguments/results, or `function` for explicit `NativeReturn`. A failed typed argument conversion becomes Lua's catchable argument error. Other API misuse stays a host API error. Bind a function through `make_closure` and the globals table. [lua_calls_rust](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/lua_calls_rust.rs) registers a typed callback.

`start_call(&function, args)` installs a call on the idle main thread. Drive it with `run` and consume the outcome with `finish_call::<R>()`. Starting another call is `ApiError::Busy`. Finishing an absent or unfinished call is `ApiError::InvalidCallState`. `call::<R>` combines these steps up to a fuel bound and returns `CallOutcome::Done`, `Waiting`, `OutOfFuel`, or `ExitRequested { status, close }`. After `Waiting` or `OutOfFuel`, continue with `run` and `finish_call`; do not start the call again. The journal must be the same journal lineage. `call_closure` and integer-only value paths are deprecated.

An exit request is a terminal control outcome, never a catchable Lua error. `StepOutcome::ExitRequested` and `CallOutcome::ExitRequested` carry `ExitStatus::Success`, `Failure`, or `Code(i32)`; the embedder maps success and failure to its platform's status constants. The runtime never exits the host process. With `close: false`, execution stops immediately and runs no closing actions. With `close: true`, pending main-thread `<close>` variables run first, then registered finalizers; suspended coroutine close variables are left alone, as in PUC 5.4.9. Close errors reach the remaining close functions; finalizer errors become warnings. Fuel pauses, host waits, and snapshots work during closing. Snapshot schema 25 preserves the pending status and closing stage. The outcome is returned after closing finishes; further execution, loads or calls return an API error. Installing `Libraries::OS` supplies `os.exit`; it needs no process capability.

Handle the outcome in the host's run loop without exiting the embedding process:

```rust
use moonseed::{ExitStatus, StepOutcome};
fn requested_status(outcome: StepOutcome) -> Option<ExitStatus> {
    match outcome {
        StepOutcome::ExitRequested { status, close: _ } => Some(status),
        _ => None,
    }
}
```

For example, a service can finish only the requesting script or job with that
status. Do not call `begin_close` again after an exit with `close: true`.

## Userdata and borrowing

Register host types before creating their userdata. Implement `HostUserdata` with a stable type symbol and an accurate logical size. Mutable borrow guards borrow the runtime or context exclusively and update the payload's charged size on drop. Lua references held inside arbitrary Rust payloads are not traced; keep Lua values in native captures, userdata user values through the legacy API, or owned roots. [host_userdata](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/host_userdata.rs) attaches methods to a Rust object. [stateful_function](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/stateful_function.rs) keeps Rust state in userdata and Lua values in native closure captures.

A userdata borrow cannot remain live while preparing a Lua call through the same context. This fails at compilation:

```compile_fail,E0502
use moonseed::{AnyUserData, HostUserdata, NativeContext, NativeReturn, MultiValue};
struct Counter(i64);
impl HostUserdata for Counter {
    const SYMBOL: &'static str = "guide.Counter";
    fn logical_size(&self) -> u64 { 8 }
}
fn incorrect(cx: &mut NativeContext<'_>) -> moonseed::Result<NativeReturn> {
    let object: AnyUserData = cx.argument(0)?;
    let mut guard = cx.borrow_userdata_mut::<Counter>(&object)?;
    let call = NativeReturn::CallLua {
        function: cx.arg(1).to_owned_value()?,
        args: MultiValue::new(), tag: 0, keep: MultiValue::new(),
    };
    guard.0 += 1;
    Ok(call)
}
```

End the borrow in a scope, then prepare the call. The callback returns before Lua executes, so no Rust borrow guard crosses the continuation.

## Native-to-Lua continuations

Return `NativeReturn::CallLua { function, args, tag, keep }`. On a later VM step the same registered symbol runs with `cx.resumed()`. Its outcome is `Returned(MultiValue)` or `Errored(LuaError)`; `tag` and kept values identify the continuation. Handle both outcomes. No Rust stack frame survives, and the Lua call may yield, wait, or be snapshotted where the containing thread permits it. [native_calls_lua](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/native_calls_lua.rs) forwards both results and errors. Captures, tags, and kept values are snapshot state; callback code is host registration state.

Use `NativeContext::resumed_ref() -> Option<&Resume>` to inspect that continuation without cloning kept or returned value buffers. `resumed()` remains an owned copy and may allocate. Prepare a call with `cx.call_lua(function, args, tag, keep)?`, where `function` implements `IntoLua` and `args`/`keep` implement `IntoLuaMulti`; scalar or tuple inputs use the runtime's reusable buffers. Consume or copy the needed fields from the borrowed resume before mutating the context, and release userdata guards before preparing the call.

Warmed scalar calls and borrowed continuations reuse their buffers. New objects,
storage growth and owned resume copies can allocate. Construction and restore
start with empty pools. See the performance measurements for their scope.

## Waits, effects, and the journal

Return `NativeReturn::Wait(WaitRequest { operation, payload })`. The VM issues a `WaitKey`; inspect its operation and rooted payload with `wait(key)`. Submit host work, then complete it with `Completion::Return` or `Completion::Error` and resume execution. Completion checks ownership. A second completion is `ApiError::AlreadyCompleted`, even after restore. A key with no pending operation is `ApiError::NotWaiting`. [async_wait](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/async_wait.rs) shows deferred completion.

`NativePolicy::VmLocal` is for work whose effects remain in VM state. `External` supplies an effect id and journal. Commit the external outcome with `Journal::commit`; an existing record replays its outcome without running the supplied closure. Persist the journal separately from snapshots and retain its domain and sequence identities. Exactly-once replay depends on retaining committed outcomes and coordinating the external operation with durable journal recording. Restore does not undo external work. Do not perform an external effect before entering the journal's fresh callback. A wait alone does not make an external operation durable or deduplicate host dispatch. Use the wait key/effect identity in the host's own operation ledger when needed.

## Snapshots and registrations

`snapshot()` encodes VM state, including active calls, continuation frames, native captures, and pending waits. `Runtime::restore(bytes, &Host)` validates the bytes, native symbols, userdata types and policies, codecs, rebinds, effect domain, and host limits before returning a runtime. The snapshot never contains callback code, host resource maps, output sinks, or the journal. Reacquire rooted objects by globals or `ObjectId` after restore. [snapshot_restore](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/snapshot_restore.rs) restores portable userdata and native symbols.

Every native symbol, host userdata type and retained host-hook symbol named by the snapshot is required, including masked-off hooks. Output, warnings, entropy, and module resolution capabilities are optional for decoding and may be needed for later execution. `print` without a sink writes nowhere. Moonseed supplies its own library symbols automatically, so a fresh restore registry needs only host callbacks, userdata types, and hooks. `Host::new(registry)` permits `Libraries::ALL` by default; restrict an untrusted checkpoint with `Host::libraries(allowed)`. Any retained symbol from a denied library fails with `SnapshotError::UnknownHostSymbol`, even if the registry manually registered it, before userdata codecs/rebinds run. Missing allowed symbols use Moonseed's implementations; an explicit restore registration retains precedence and must match the snapshot's native policy/work shape, as before. Restore reuses the snapshot's existing globals; it does not install extra libraries or grant external capabilities. The older `from_snapshot` forms delegate to restore with default or supplied limits.

## Portable, refusing, and rebindable userdata

`register_portable_userdata` uses `PortableUserdata::encode` and `decode`. The codec must be deterministic and must validate untrusted bytes. `register_userdata` refuses snapshots containing that type. `register_rebind_userdata` stores a type symbol and an external key of at most 4 KiB; `RebindUserdata::rebind` resolves it against `HostEnv` before a runtime exists. The host owns the resource; the snapshot carries its key. Rebind failure aborts restore. [external_rebind](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/external_rebind.rs) binds the restored key to an existing host resource.

A snapshot cannot carry a file, socket, or GPU object. Register such a payload as refusing, or use a stable rebind key for a resource the restoring host already owns. The runtime refuses the following safe misuse with a snapshot error:

```rust
use moonseed::{HostUserdata, HostRegistry, Runtime, SnapshotError};
struct GpuObject;
impl HostUserdata for GpuObject {
    const SYMBOL: &'static str = "guide.GpuObject";
    fn logical_size(&self) -> u64 { 1 }
}
let mut registry = HostRegistry::new();
registry.register_userdata::<GpuObject>();
let mut rt = Runtime::builder().registry(registry).build().unwrap();
let resource = rt.create_host_userdata(GpuObject, 0).unwrap();
assert_eq!(rt.snapshot(), Err(SnapshotError::NonPortableUserdata));
drop(resource);
```

## Module resolver and replay policy

Install a `ModuleResolver` with the package library. It answers exact name bytes with `Resolved::Source`, `Binary`, `Native`, or `NotFound`. Its searcher follows preload and, when supplied, the filesystem Lua searcher; `require` controls loader invocation and `package.loaded`. Source and binary chunks are compiled or validated under limits. Diagnostics and loader failures are catchable Lua errors. [module_resolver](https://github.com/Tavrin/moonseed/blob/main/crates/moonseed/examples/module_resolver.rs) resolves `require "game.foo"` from host source.

`ResolverPolicy::Pure` promises immutable deterministic answers. `External` records the first resolution's bytes in the journal; replay uses those bytes even if the current resolver changes or is absent. Keep that journal with the checkpoint. A pure resolver's contents are not captured automatically.

## Sandboxing, limits, and threading

Choose libraries explicitly; `STANDARD` omits debug. Restrict native registrations and resolver contents to what the Lua program may access. Set runtime limits for objects, logical heap, stack slots, string bytes, and snapshot bytes. Set compiler limits for source bytes, instructions, constants, and functions. Fuel bounds execution, while host callbacks must bound their own work. Logical heap charging is a resource model, not a measurement of process RSS. Snapshot decoding has separate budgets and structural bounds; smaller restore limits can refuse otherwise valid snapshots. See [ADR 0052](https://github.com/Tavrin/moonseed/blob/main/docs/adr/0052-scalability-envelope.md).

A runtime is single-threaded, neither `Send` nor `Sync`, and not reentrant. An asynchronous host performs work outside the runtime and completes waits on its owning thread. Native-to-Lua continuations are the supported reentry path. Do not recursively drive execution from a custom conversion or callback. Root lifetimes and borrow guards enforce borrowing at compile time; state and ownership checks report misuse at runtime.

## Host debug hooks

Register `HostRegistry::register_hook("profiler", callback)` before construction
or restore. A callback receives `&mut HookContext` and returns
`Result<HookAction>`. `Runtime::set_hook(None, "profiler", mask, count)` selects
the main thread; `Some(&Value::Thread(...))` selects a rooted thread. `clear_hook`
and `get_hook` use the same selection and reject foreign roots with
`ApiError::WrongRuntime` before mutation. `HookSettings` distinguishes a host
symbol, a rooted Lua function, and an inherited Lua wrapper without a function.
`HookMask::CALL | HookMask::RETURN | HookMask::LINE` selects those events; tail
calls use CALL. A positive `i32` count independently enables count events.

A small call/return profiler can count events without allocating per callback:

```rust
use moonseed::{compile, HookAction, HookMask, HostRegistry, Journal, Runtime, StepOutcome};
use std::{cell::Cell, rc::Rc};

let events = Rc::new(Cell::new(0_u64));
let counter = events.clone();
let mut registry = HostRegistry::new();
registry.register_hook("profiler", move |_| {
    counter.set(counter.get() + 1);
    Ok(HookAction::Continue)
});
let mut rt = Runtime::builder().registry(registry).build().unwrap();
rt.load_main(&compile(b"local function f(x) return x+1 end return f(4)").unwrap()).unwrap();
rt.set_hook(None, "profiler", HookMask::CALL | HookMask::RETURN, 0).unwrap();
let mut journal = Journal::new();
loop {
    match rt.run(100, &mut journal).unwrap() {
        StepOutcome::Paused(_) => continue,
        StepOutcome::Completed => break,
        other => panic!("unexpected {other:?}"),
    }
}
assert!(events.get() > 0);
rt.clear_hook(None).unwrap();
```

For function-level samples use `cx.event()` and `cx.info(0)?`; owned metadata
may allocate. This counter is host state: restore it separately if it must
replay with a checkpoint.

Inside a native, the matching `NativeContext` methods default to the active
thread. New coroutines inherit host symbols and mask/base count with a fresh
countdown. Lua hooks inherit the wrapper and settings without the parent's
function. Installing a hook never roots its thread globally. `debug.gethook`
reports `"external hook"` for a host hook, including after restore.

`HookContext::event` and `line` describe the event. Level zero in `info`, `local`,
`set_local` and `upvalue` is the interrupted activation; larger levels inspect
its callers. `HookInfo` supplies getinfo-style byte strings and transfer ranges.
Locals use one-based indexes, negative indexes inspect varargs, and `set_local`
has debug.setlocal's permissions. No VM borrow may outlive the callback. Borrowed `ValueRef`s end with the context
borrow; convert them to owned values to retain roots. Inspection and raw table
operations run no Lua. Inspection that returns owned metadata may allocate;
an empty warmed callback allocates nothing per event.

Callbacks are synchronous and bounded by the host. No host waits may run in
the callback itself. They cannot wait, recursively
drive the VM, or return a Rust continuation. They may return a catchable Lua error
prepared with `cx.error(value)`. Hook delivery remains suppressed through that
error's message handler until unwind recovers it. Replacement or removal inside
a callback takes effect after the callback while suppression remains active.
A Rust panic propagates to the host and leaves the runtime unusable: subsequent
execution and snapshots are refused. Discard that runtime after catching the panic.

`HookAction::Yield` yields zero values only from a line/count event in a yieldable
coroutine. Resume ignores supplied values for this yield and executes the
interrupted instruction without redelivering or recounting it. A count yield
also skips a simultaneous line event, matching PUC. Main-thread yield raises
`attempt to yield from outside a coroutine`. Call, return and tail-call yields
raise `attempt to yield across a C-call boundary`; this is Moonseed's safe error
for unsupported C-hook yields which crash the pinned non-asserting PUC build.
Lua functions installed with debug.sethook remain non-yieldable for every event.

Snapshot schema 25 encodes every legal hook configuration and continuation, including
pending fuel-paused delivery, mid-Lua-hook frames, host waits inside a Lua hook,
and a coroutine suspended by a host-hook yield. It stores host symbols, never
callback code or host callback state. Restore requires those symbols before any
runtime or userdata rebind is constructed, even when a hook is masked off in a
retained state. Re-register callbacks and any host resources explicitly; rooted
values from the old runtime cannot be reused. A missing symbol fails with
`SnapshotError::UnknownHostSymbol`. Fuel revision remains 7: delivery costs one
unit, hook Lua bodies use ordinary fuel, and count measures begun Moonseed
instructions independently of fuel and source lines.

## Host capabilities

`HostCapabilities::sandbox()` is the default: no filesystem, standard streams,
clock, civil time, environment or process authority. Independently attach an
`Arc<dyn Filesystem>`, `Stdio`, `Clock`, `CivilTime`, `Environment` or `Process`
with the corresponding capability, builder or restore-host setter. These shared
objects need not be `Send` or `Sync`, matching the runtime's existing host model.
`Libraries::IO` and `Libraries::OS` are independent `u16` flags included in
`STANDARD` and `ALL`. Both install their function tables in globals and `_LOADED`.
Library installation grants no host authority. Before construction or restore,
audit the six optional capability fields, their backend policies and limits,
and registered callbacks/resolvers; those objects define the script's authority.

### Capability installation examples

A sandbox may expose the standard functions without granting any filesystem,
stream, clock, environment or process access:

```rust
use moonseed::{HostCapabilities, Libraries, Runtime};
let sandbox = Runtime::builder()
    .libraries(Libraries::STANDARD)
    .capabilities(HostCapabilities::sandbox())
    .build().unwrap();
```

A read-only VFS exposes only seeded byte paths and keeps process execution disabled:

```rust
use moonseed::{Libraries, MemoryFilesystem, MemoryOptions, Runtime};
use std::sync::Arc;
let files = Arc::new(MemoryFilesystem::new(
    [(b"main.lua".to_vec(), b"return 42".to_vec())],
    MemoryOptions { read_only: true, ..MemoryOptions::default() },
).unwrap());
let read_only = Runtime::builder()
    .libraries(Libraries::STANDARD)
    .filesystem(files.clone())
    .package_paths(b"?.lua", b"")
    .build().unwrap();
```

Keep `files` alive for Rebind restore. Its contents and open-resource table are
host state, separate from the VM snapshot.

With the `native-host` feature, root filesystem access at an existing `scripts/`
directory. Lua paths are relative to that root; `?.lua` refers to
`scripts/<name>.lua`, not `scripts/scripts/<name>.lua`:

```no_run
# #[cfg(feature = "native-host")]
# {
use moonseed::{native_host, Libraries, NativeOptions, Runtime};
let caps = native_host("scripts", NativeOptions {
    process: false, env: false, stdio: false, clock: false,
    read_only: true, ..NativeOptions::default()
}).unwrap();
let scripts = Runtime::builder()
    .libraries(Libraries::STANDARD)
    .capabilities(caps)
    .package_paths(b"?.lua;?/init.lua", b"")
    .build().unwrap();
# }
```

This uses native path validation with the confinement limits below. A writable
filesystem with process execution disabled uses the same profile with
`read_only: false`; filesystem permission does not imply process permission.

Mock time can be installed independently, with no native clock or filesystem:

```rust
use moonseed::{hostcaps::testing::FixedClock, Libraries, Runtime};
use std::sync::Arc;
let deterministic = Runtime::builder()
    .libraries(Libraries::STANDARD)
    .clock(Arc::new(FixedClock { now: 1_700_000_000, cpu: 0.25 }))
    .build().unwrap();
```

Without a civil capability, local conversion uses UTC. Supply a `CivilTime`
implementation for local timezone rules and abbreviations.

A host with standalone-compatible filesystem, streams, clocks, environment and
shell operations explicitly enables each native grant. The root limits
filesystem operations; shell commands need their own outer confinement.
`ALL` also installs debug, so this profile grants inspection/mutation authority:

```no_run
# #[cfg(feature = "native-host")]
# {
use moonseed::{native_host, Libraries, NativeOptions, Runtime};
let caps = native_host("scripts", NativeOptions {
    process: true, env: true, stdio: true, clock: true,
    read_only: false, ..NativeOptions::default()
}).unwrap();
let mut standalone = Runtime::builder()
    .libraries(Libraries::ALL)
    .capabilities(caps)
    .output(|bytes| {
        use std::io::Write;
        std::io::stdout().write_all(bytes).expect("stdout");
    })
    .package_paths(b"?.lua;?/init.lua", b"")
    .build().unwrap();
standalone.install_arg(b"main.lua", &[b"level1".as_slice()],
    &[b"moonseed".as_slice()]).unwrap();
# }
```

This profile retains Moonseed's C locale, UTC fallback, logical write buffering
and lack of C modules. `print` and `warn` use separate output/warning sinks;
the example attaches print output. Add a builder `warnings` sink with the
host's policy for continuation pieces and Lua warning control messages.

### Filesystem and resource policies

`memory_filesystem(files, MemoryOptions)` constructs a filesystem-only profile.
`MemoryFilesystem::new` lets the embedder retain the backend, inspect its byte
contents and share it across restores. Its paths are exact byte keys, including
NUL and invalid UTF-8. Reads and writes are positional: Lua file userdata
owns its cursor, read-ahead buffer and pending output in snapshot state. Resource IDs survive rename and remove;
`rebind` verifies a still-open key without reopening or truncating anything.
Keep the backend alive across restore; the VM does not serialize its files or
open-resource table. Anonymous temporary files disappear on close; `temp_name`
reserves a visible empty file which the caller owns and removes explicitly.

`native_host(root, NativeOptions)` is available with the default-on
`native-host` Cargo feature. Disable default features for an OS-free core.
Process execution and environment access are opt-in. The native filesystem
rejects absolute paths and parent components and checks each component with
`symlink_metadata`, denying symlinks by default. Namespace changes can race
these checks and the subsequent OS operations (TOCTOU); use the VFS for strong
confinement. Opting into symlinks permits targets outside the root. Native live
file handles and pipes use `HandlePolicy::Refuse`; the VFS uses `Rebind`.
Temporary files use OS entropy, exclusive creation and Unix mode 0600; a named
temp reservation persists until explicitly removed. Native temporary allocation
currently returns Unsupported outside Unix. CPU seconds come from Linux process
user/system ticks and the kernel tick rate, never elapsed wall time; other
platforms return Unsupported. No native civil converter is attached: consumers
use UTC for local time when that capability is absent. Opt-in shell commands
are process-wide authority and are not confined by the filesystem root.

Capability traits use `hostcaps::Completion<T>` (also exported as
`CapabilityCompletion<T>`); the existing root-level `Completion` remains the
Lua-native wait protocol. Structured `HostIoError` carries portable kind, OS or
stable synthetic errno, and byte-message fields. Synthetic codes are 2, 13, 17,
21, 22, 38 and 5 for NotFound, PermissionDenied, AlreadyExists, IsDirectory,
InvalidInput, Unsupported and Other respectively. Partial positional and stream
writes return their written count; callers must advance by that count. Append
returns the new file length and is used with append-mode resources; native
append writes the supplied bounded buffer fully, or records an error (a host
error can occur after an external partial write).

Every builtin host operation goes through `Runtime::capability(request, journal)`.
`CapabilityRequest` determines the read/mutation class and encodes operation,
resource ID, offset, length, bytes and paths as exact journal identity. Replay
checks these bytes and the class before returning a recorded outcome, including
errors, without invoking the backend. Persist complete `EffectRecord` values,
including the new `request` field, and reconstruct with `seed_record`.
`commit_request`/`replay_request` expose this same validation to embedders;
legacy integer/byte and module records remain supported with their existing
APIs. Snapshots preserve VM history; they do not freeze the future external world. Exactly-once
is deduplication within the preserved journal; atomic crash durability between
an external mutation and journal persistence remains the embedder's responsibility.

### Completing Pending operations

`CapabilityPoll::Waiting(key)` leaves the builtin at its current work stage.
`Runtime::wait(key)` exposes operation and three rooted payload slots: encoded
request bytes, host token's 64-bit representation in a Lua integer, and nil until
completion. `complete_capability(key, result)` accepts a typed `CapabilityValue`
or `HostIoError`, rejects wrong/oversized results and duplicate completions, and
resumes that stage. The builtin re-enters the helper with the same request; this
commits the completion without running the backend again, then advances its work.
The completion itself remains checkpointed if a snapshot is taken before that
step. Generic Lua-native `complete` cannot complete a capability wait. No host
waiting time costs fuel; capability reads/writes are bounded to 64 KiB and
request/result strings respect VM string, heap and snapshot limits. Panics from
capability callbacks become structured host errors.

Complete the result of the already submitted operation on the runtime's owning
thread, then resume with the same journal. For a pending positional read:

```rust
use moonseed::{CapabilityValue, Runtime, WaitKey};
fn finish_read(rt: &mut Runtime, key: WaitKey, bytes: Vec<u8>) -> moonseed::Result<()> {
    rt.complete_capability(key, Ok(CapabilityValue::Bytes(bytes)))
}
```

Other operations require their corresponding typed result, or
`Err(HostIoError)`. Inspect `wait(key)` before dispatching completion: ordinary
native waits use the root-level `Completion` protocol instead. Retain the host
token and dispatch ledger across restore; a waiting snapshot does not cancel
or resubmit an external operation. A completion saved before consumption is
committed when the builtin resumes, without invoking the backend again.

`hostcaps::testing` is always public, without a feature: `FixedClock`,
`FixedCivilTime`, `MemoryEnvironment`, `MemoryStdio`, `MockProcess` and
`PendingHost` support deterministic native/Wasm fixtures. `PendingHost` returns
Pending for all waitable trait operations and keeps an invocation log.
A pending file acquisition follows its backend's live-resource policy, including
an acquired resource in a completed wait that the builtin has not consumed yet.
Refuse backends and pipes reject those checkpoints; Rebind restore validates the
acquired key before constructing the runtime. Failed acquisitions own no resource.

To release Lua files, drive `begin_close` through any capability waits before
dropping a finished runtime. Dropping a runtime runs no Lua cleanup and does not
close resources in a capability retained elsewhere: those keys may still belong
to a checkpoint being restored. Native adapter destruction releases its remaining
OS resources; embedders retaining a shared backend own abandoned-operation cleanup.

Snapshot schema 25 adds typed capability waiting/completed work; schema 24 is
refused according to the existing exact-schema policy. The restore host's library allowlist is policy, not serialized state; retained
library symbols are already recorded and validated. No library-selection mask
is serialized, so selecting it adds no snapshot state. Fuel revision 7,
bytecode 14, tables 4, GC 12 and binary chunk format 2 are unchanged.
The architecture guard is `python3 tools/check_host_boundary.py`, including
historical proof and measurement code; all ambient OS calls live in one adapter
module. `io`, `os`, `loadfile`, `dofile`, `package.searchpath`, and the filesystem
Lua searcher all use this capability boundary.

### File and module loading

`Libraries::IO` installs file userdata and the full IO table. Methods and lines
iterators retain mode, cursor, EOF/lookahead and charged read-ahead bytes.
`io.lines(filename)` owns a closing file; generic-for break, exhaustion,
explicit close and finalizers share one journaled close path. Standard streams
are file userdata with explicit-close refusal. `setvbuf` selects no/full/line buffering; pending bytes are snapshot state.
Writes reach the capability at semantic flush points (explicit flush/close,
seek, full capacity, or a newline in line mode), and effects are journaled then. Rebind/Refuse policy is
per live file, with closed files requiring no backend on restore. The native
policy is conservative over retained userdata awaiting collection.

`Runtime::builder().package_paths(b"scripts/?.lua;scripts/?/init.lua", b"")`
sets the initial `package.path` and `package.cpath` (both default empty). These
strings grant no authority. With a filesystem capability the Lua searcher sits
between preload and the optional host resolver; a fresh `require` returns its
loader's filename as the second result. `package.loadlib` is absent: Moonseed
supports no dynamic C modules, regardless of cpath.

`loadfile` shares `load`'s text compiler, binary decoder, mode checks and explicit
environment binding, including explicit nil. It names files `@filename` and
stdin `=stdin`, strips an optional UTF-8 BOM and initial # line, and returns
`nil, message` for file/compile/mode failures. No filename uses only the supplied
stdio capability. `dofile` raises loading errors, calls the chunk with a
yieldable checkpointed continuation, and returns every result, including nils.

Source loading uses `Filesystem::read_file_range(path, offset, max)`, a bounded
positional extension of the handle-free `read_file` convenience. VFS and native
adapters implement it; the default supports only a file fitting in its initial
whole-file request. Each range is at most 64 KiB and goes through the same
journal/Pending helper. No native fd survives a range call. Source accumulation
is charged and bounded by the default compiler source limit and max string
limit; path/name expansion advances in 1 KiB work units. Compilation remains the
existing bounded compiler operation. A committed source read replays its original
bytes; uncommitted later ranges observe the current backend. The host must
provide immutable files or pin a version in its adapter if it needs an atomic
view across ranges while the external world changes. Stdin is read until an
empty result, since a short stream read need not be EOF.

Load/search state and the all-results dofile continuation use snapshot schema
25, without another revision bump. Capabilities and the external journal remain
host-owned. The benchmark runner opts into a rooted native filesystem and stdin
with `moonseed-run --host=native:ROOT FILE`, using public builder/profile APIs.
It installs the complete libraries through the public builder. Add
`--host-process` or `--host-environment=process` only when those grants are needed.

### OS calls and explicit arguments

`Libraries::OS` installs clock, date, difftime, execute, exit, getenv, remove,
rename, setlocale, time and tmpname. Legacy clients use `register_os` and
`Runtime::install_os`; `register_standard`/`install_standard` include OS.
Clock reads use the CPU and wall operations independently. Missing clock
access raises "time source not available"; an explicit date/time table needs
no clock. Local civil conversion uses `CivilTime`, falling back to UTC when
absent. Attach `CivilTime::zone_name` for `%Z`: offset/DST alone cannot identify
a timezone. Its default returns an empty abbreviation. Both the numeric
conversion and the abbreviation are journaled, including waits.

The date formatter uses the pure C-locale implementation in bounded work
steps. Time-table reads and normalized writeback honor `__index`/`__newindex`
in PUC order. Those metamethods are non-yieldable, but host waits and snapshots
remain supported. `os.setlocale` returns "C" for nil, C, POSIX and empty locale
requests; unsupported locale names return nil. This never changes process locale.

`Runtime::install_arg(script, args, pre_args)` explicitly creates a rooted
argument table and binds it globally after successful construction. Byte strings
are preserved: script is index 0, arguments start at 1, and pre-arguments retain
command-line order from `-pre_args.len()` through -1. It is never automatic and
does not set a chunk's varargs. Supply those separately when calling the chunk.
`os.exit` uses the existing terminal `ExitRequested` outcome described above.

The test runner accepts `--host=native:DIR`, `--host-environment=process`,
`--host-process`, and `--lua-args` (trailing arguments plus the explicit arg
helper), alongside the complete IO/OS libraries.
`--host-civil=fixture-tz` attaches fixed New York rules only for the frozen
`TZ=America/New_York` fixture; this is not a general timezone database. Other
profiles use the core's documented UTC fallback.

On Linux, native temporary names normally use `/dev/urandom`; if the host
mounts devices with `nodev`, the adapter can use the kernel's version-4 UUID
source at `/proc/sys/kernel/random/uuid` (122 random bits). Names are reserved
exclusively with mode 0600 inside the capability root and returned as relative
`.moonseed-...` byte paths. Their spelling deliberately differs from PUC's
absolute `/tmp/lua_...` names. Without filesystem authority, the RFC's synthetic
`nil, message, code` failure applies; a supplied backend's temp-name failure
raises PUC's "unable to generate a unique filename" error.

For the frozen Linux hostlib fixture, `--host-fixture-tmp` explicitly aliases
`/tmp/lua_<32 hex digits>` to the same secure native reservation inside the
supplied root. Open/probe/read/remove/rename share that alias; other absolute
paths still fail. This profile changes names, not authority, and is intended
for the corpus rather than claiming unrestricted host `/tmp` access.

## Snapshot compatibility and the 0.1 API

Restore requires exact snapshot schema 25, bytecode 14, tables 4, fuel 7 and GC 12.
Moonseed binary chunks use format 2 and are Moonseed-specific, portable across
Moonseed native/Wasm targets; PUC binary chunks are refused. A snapshot is an
execution checkpoint for a matching build/schema policy, not a durable save
format with migration guarantees. See [COMPATIBILITY_POLICY.md](https://github.com/Tavrin/moonseed/blob/main/docs/COMPATIBILITY_POLICY.md) for version policy and
[DETERMINISM.md](https://github.com/Tavrin/moonseed/blob/main/docs/DETERMINISM.md) for the
qualified cross-target boundary. Old Rust roots cannot cross restore even when
their `ObjectId` remains valid. Persist host callback/resource state and journals
separately, and reacquire rooted values in the restored runtime.

Errors, produced metadata, and extensible outcomes carry `#[non_exhaustive]`;
match them with a fallback arm. Public input records remain constructible where
that is their documented contract; opaque types already prohibit construction.
Deprecated integer-only interfaces and doc-hidden qualification fixtures are
not the rooted 0.1 embedding surface. Those fixtures remain Rust-reachable for
the workspace's native/Wasm proofs; removing that tooling boundary would require
a separate packaging/consumer migration, not an embedding API change.
