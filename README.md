# Moonseed

Moonseed is a deterministic, resource-governed, checkpointable Lua 5.4 runtime in pure Rust.

It is built for embedding: execution can pause for fuel, wait for host work,
and resume from a checkpoint in a fresh runtime. The crate forbids unsafe code.
It is not a drop-in replacement for PUC Lua.

## Status and compatibility

This is the first release, 0.1.0. The public interface is the Rust embedding API.

Moonseed implements the Lua 5.4 source language and its Lua-visible runtime
semantics, with most of the standard library. It does not implement the Lua C
API, PUC Lua binary chunks or C modules. The `io` and `os` libraries only reach
the outside world through capabilities the host provides.

The pinned, unmodified Lua 5.4.9 suite compiles **33/33** files: **14 PASS,
19 FAIL, zero UNKNOWN**. Each failure has a classified reason. This is not a
full-suite pass. See the [suite ledger](docs/LUA_54_SUITE.md) and
[compatibility details](docs/LUA_COMPATIBILITY.md).

## Quick start

Add `moonseed = "0.1"` under `[dependencies]`.

Compile source bytes, load the chunk, and inspect the execution outcome:

```rust
use moonseed::{compile, FromLuaMulti, Journal, Runtime, StepOutcome};

let mut runtime = Runtime::builder().build().unwrap();
runtime.load_main(&compile(b"return 6 * 7").unwrap()).unwrap();
let mut journal = Journal::new();
loop {
    match runtime.run(1_000, &mut journal).unwrap() {
        StepOutcome::Paused(_) => continue,
        StepOutcome::Completed => break,
        other => panic!("unexpected outcome: {other:?}"),
    }
}
let values = runtime.result_values().unwrap();
assert_eq!(i64::from_lua_multi(values, &mut runtime).unwrap(), 42);
```

This follows [hello_call](crates/moonseed/examples/hello_call.rs). The example's
program cannot wait or request an exit. An application's run loop must handle
Lua errors, host waits, yields, termination and exit requests as appropriate.
A quantum limits one slice; configure a total fuel limit to bound the whole run.

## Embedding

Register Rust functions under stable symbols, then expose only the functions
Lua should be able to call. Typed adapters check argument and result conversions:

```rust
use moonseed::{compile, FromLuaMulti, HostRegistry, Journal, NativePolicy,
               Runtime, StepOutcome};

let mut registry = HostRegistry::new();
registry.typed("app.scale", NativePolicy::VmLocal,
    |_cx, (x, scale): (i64, i64)| Ok(x.wrapping_mul(scale)));
let mut runtime = Runtime::builder().registry(registry).build().unwrap();
let scale = runtime.make_closure("app.scale", ()).unwrap();
runtime.globals().raw_set(&mut runtime, "scale", scale).unwrap();
runtime.load_main(&compile(b"return scale(6, 7)").unwrap()).unwrap();
assert_eq!(runtime.run(1_000, &mut Journal::new()).unwrap(), StepOutcome::Completed);
let values = runtime.result_values().unwrap();
assert_eq!(i64::from_lua_multi(values, &mut runtime).unwrap(), 42);
```

See [lua_calls_rust](crates/moonseed/examples/lua_calls_rust.rs) for the full
example. The API also provides rooted strings, tables, functions and userdata,
Rust-to-Lua calls, coroutine control, debug hooks, and native-to-Lua continuations.
A runtime is single-threaded, neither `Send` nor `Sync`. Host work can run
elsewhere and complete a wait on the runtime's owning thread.

## Checkpoints

A checkpoint contains the VM's execution state, including active calls,
coroutines, RNG state, fuel, collection state and pending waits:

```rust
use moonseed::{compile, FromLuaMulti, Host, HostRegistry, Journal, Runtime, StepOutcome};

let mut runtime = Runtime::builder().build().unwrap();
runtime.load_main(&compile(b"local n = 40; return n + 2").unwrap()).unwrap();
let mut journal = Journal::new();
assert!(matches!(runtime.run(1, &mut journal).unwrap(), StepOutcome::Paused(_)));
let bytes = runtime.snapshot().unwrap();
let host = Host::new(HostRegistry::new()).effect_domain(runtime.effect_domain());
let mut restored = Runtime::restore(&bytes, &host).unwrap();
assert_eq!(restored.run(1_000, &mut journal).unwrap(), StepOutcome::Completed);
let values = restored.result_values().unwrap();
assert_eq!(i64::from_lua_multi(values, &mut restored).unwrap(), 42);
```

[Snapshot restore](crates/moonseed/examples/snapshot_restore.rs) also covers
portable userdata and a suspended coroutine. Re-register host callbacks, codecs
and rebind resources before restore; Moonseed supplies its own library symbols.
Rust roots from the old runtime cannot be reused in the restored one.

Keep the journal and host resource state separately. Restoring does not undo
external effects or freeze future filesystem reads. Replay deduplicates effects
only with the preserved journal; crash durability is the host's responsibility.
Snapshots are versioned execution checkpoints, not an archival format. See the
[compatibility policy](docs/COMPATIBILITY_POLICY.md#snapshots).

## Sandbox and capabilities

The builder starts with no libraries and no external authority. Installing
`Libraries::STANDARD` adds functions, including IO/OS, but grants no filesystem,
standard streams, clock, civil-time, environment or process access. It omits debug.

This sandbox exposes one read-only in-memory file and no native OS capabilities:

```rust
use moonseed::{compile, FromLuaMulti, Journal, Libraries, MemoryFilesystem,
               MemoryOptions, Runtime, StepOutcome};
use std::sync::Arc;

let files = Arc::new(MemoryFilesystem::new(
    [(b"answer.lua".to_vec(), b"return 42".to_vec())],
    MemoryOptions { read_only: true, ..MemoryOptions::default() },
).unwrap());
let mut runtime = Runtime::builder()
    .libraries(Libraries::STANDARD)
    .filesystem(files)
    .package_paths(b"?.lua", b"")
    .build().unwrap();
runtime.load_main(&compile(br#"
    local denied = io.open('new.lua', 'w') == nil
    local answer = require('answer')
    return answer, denied, os.execute()
"#).unwrap()).unwrap();
let mut journal = Journal::new();
assert_eq!(runtime.run(10_000, &mut journal).unwrap(), StepOutcome::Completed);
let values = runtime.result_values().unwrap();
let result: (i64, bool, bool) = FromLuaMulti::from_lua_multi(values, &mut runtime).unwrap();
assert_eq!(result, (42, true, false));
```

Native filesystem adapters check paths and symlinks, but namespace changes can
race those checks and escape the root. Use an in-memory filesystem or an outer
OS sandbox when confinement is required. Enabling shell execution grants the
host process's authority; a filesystem root does not confine shell commands.
Live native files and pipes refuse checkpoints. In-memory files can rebind to a
preserved backend.

Fuel and logical heap quotas are not wall-clock or process-memory limits.
Bound host callbacks and use outer resource limits for untrusted work. The
`debug` library grants inspection and mutation of otherwise hidden Lua state.
Read [SECURITY.md](SECURITY.md) before exposing capabilities.

## Performance

Measured in executed instructions, Moonseed runs the benchmark corpus at about
**2.58× PUC Lua 5.4.9** (geometric mean over 21 workloads). Against other Rust
implementations it is **1.27× omniLua** (21 workloads), **0.70× Luna's
interpreter** (21) and **0.56× Piccolo** (16).
Lower means fewer executed host instructions. Piccolo lacks five required
library workloads; these coverage sets must not be treated as identical.

Callgrind counts the whole process, including startup, compilation, library
setup, execution and teardown, on the same divisor-100 sources. The separate
empty-program probe is excluded from aggregates. These are instruction counts,
not elapsed-time ratios or full-size workload estimates; wall-clock numbers are
not published for this release. [Performance](docs/PERFORMANCE.md#release-instruction-counts)
records the source identity, engine versions and method.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `native-host` | Yes (`default = ["native-host"]`) | Compiles native adapters. Grants no authority until the host attaches them. Builds on Wasm but does not supply a native OS there. |
| `counters` | No | Unstable diagnostic counters and JSON reporting through optional `serde_json`. Independent of `native-host`; excluded from snapshots. |

With `default-features = false`, the portable VM, in-memory filesystem and mock
capabilities remain available. Lua libraries are selected at runtime through
`Libraries`, independently of Cargo features.

The library MSRV is **Rust 1.88**. Minor 0.x releases may raise it, with a
changelog entry. The [versioning policy](docs/COMPATIBILITY_POLICY.md) separates
Rust API compatibility from Lua semantics, snapshots, binary chunks and fuel.

## Documentation

- [Embedding guide](docs/EMBEDDING.md) and [executable examples](crates/moonseed/examples/)
- [Rust API](https://docs.rs/moonseed)
- [Release notes](docs/RELEASE_NOTES_0.1.0.md) and [changelog](CHANGELOG.md)
- [Determinism](docs/DETERMINISM.md) and [compatibility policy](docs/COMPATIBILITY_POLICY.md)
- [Architecture](docs/ARCHITECTURE.md), [design decisions](docs/adr/), and [0.2 backlog](docs/ROADMAP.md)
- [Contributing](CONTRIBUTING.md)

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
