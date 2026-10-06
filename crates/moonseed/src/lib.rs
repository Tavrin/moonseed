#![allow(deprecated)] // Legacy proof and compatibility call sites.
//! Moonseed implements the Lua 5.4 source language and Lua-visible runtime
//! semantics in safe Rust, with a broad standard library and portable execution
//! checkpoints. Hosts choose libraries, resource limits, and external authority.
//! There is no Lua C API, arbitrary C-module loading, or PUC binary-chunk support.
//! Moonseed binary chunks work across Moonseed native/Wasm targets.
//!
//! # Quick start
//!
//! Compile byte source, load it on an idle [`Runtime`], and drive bounded work.
//! Each outcome matters: waits need host completion and pauses need more fuel.
//!
//! ```
//! use moonseed::{compile, FromLuaMulti, Journal, Runtime, StepOutcome};
//! let mut runtime = Runtime::builder().build()?;
//! runtime.load_main(&compile(b"return 6 * 7").unwrap())?;
//! let mut journal = Journal::new();
//! loop {
//!     match runtime.run(1_000, &mut journal)? {
//!         StepOutcome::Paused(_) => continue,
//!         StepOutcome::Completed => break,
//!         other => panic!("unexpected outcome: {other:?}"),
//!     }
//! }
//! let results = runtime.result_values()?;
//! assert_eq!(i64::from_lua_multi(results, &mut runtime)?, 42);
//! # Ok::<(), moonseed::Error>(())
//! ```
//!
//! # Features and authority
//!
//! | Cargo feature | Default | Purpose |
//! | --- | --- | --- |
//! | `native-host` | yes | Compiles opt-in native adapters; grants no authority by itself. |
//! | `counters` | no | Unstable instruction counters and JSON reports; never saved in snapshots. |
//!
//! With default features disabled the core, in-memory filesystem, and mock
//! capabilities remain available on native and Wasm. [`Libraries`] select Lua
//! functions independently of Cargo features. The builder installs no libraries;
//! [`Libraries::STANDARD`] includes IO/OS functions but excludes debug.
//! [`HostCapabilities::sandbox`] supplies no filesystem, streams, clock, civil-time,
//! environment, or process authority. Host callbacks and module resolvers are
//! also capabilities: expose only what a script may access.
//!
//! # Values and ownership
//!
//! [`Value`] owns primitives or shared roots: [`LuaString`], [`Table`],
//! [`Function`], [`Thread`], and [`AnyUserData`]. Cloning keeps the same Lua
//! object alive; the last drop releases its root. Roots may outlive the runtime
//! as Rust objects but cannot be used with a different runtime, including one
//! restored from its snapshot ([`ApiError::WrongRuntime`]). [`ObjectId`] is a
//! snapshot-stable logical identity, not a root or arena index. [`ValueRef`] and
//! [`StrRef`] borrow a runtime/context and cannot cross execution. Strings are
//! bytes; UTF-8 conversions and integer ranges are checked.
//!
//! [`FromLua`] uses strict conversions: `f64` accepts numeric values, never a
//! numeric string. [`Coerce`] explicitly opts into Lua coercion. [`MultiValue`]
//! preserves nil holes and distinguishes no results from one nil.
//!
//! # Host calls, userdata, and hooks
//!
//! Register stable host symbols with [`HostRegistry::typed`] or
//! [`HostRegistry::function`], then bind [`Runtime::make_closure`] into globals.
//! Lua calls the callback with a borrowed [`NativeContext`]. A Rust callback
//! returns before Lua executes: use [`NativeContext::call_lua`] for a
//! snapshot-backed continuation, and inspect [`ResumeOutcome`] on reinvocation.
//! [`Runtime::call`] drives an idle main-thread call with a fuel bound;
//! [`CallOutcome::Waiting`] and [`CallOutcome::OutOfFuel`] require continued
//! `run`/`finish_call`, without starting the call again.
//!
//! [`HostUserdata`] payloads are Rust-owned and type checked. Register a
//! [`PortableUserdata`] codec, [`RebindUserdata`] key, or a refusing policy.
//! The collector does not trace arbitrary Rust payloads. Store Lua references
//! in captures, user values, or owned roots. Mutable [`UserDataRefMut`] guards
//! keep the runtime/context borrowed and update logical charging on drop.
//! [`HostRegistry::register_hook`] installs synchronous bounded profiling
//! callbacks. [`HookContext`] supplies semantic activation metadata without
//! exposing VM frames or program counters. Host callback state stays outside
//! snapshots. A native, host-hook, output/warning sink, entropy or module-resolver panic
//! propagates and poisons execution/snapshots; discard the runtime if the host catches it.
//!
//! # Tables and modules
//!
//! [`Table::raw_get`] and [`NativeContext::raw_get`] never run `__index`.
//! [`Table::get`] drives semantic access on an idle main thread. Within a native,
//! call a Lua accessor (`function(t, k) return t[k] end`) through
//! [`NativeContext::call_lua`]; this honors table/function `__index` chains,
//! errors, host waits, and checkpointed continuations. Do not recursively run
//! the VM from a callback. [`ModuleResolver`] receives exact module-name bytes.
//! [`ResolverPolicy::External`] journals resolution for replay; pure resolvers
//! must supply immutable answers and be reattached on restore.
//!
//! # Checkpoints and errors
//!
//! [`Runtime::snapshot`] includes VM globals, active calls, Lua coroutines,
//! native captures, hooks, pending waits, RNG streams, fuel, and GC state.
//! [`Runtime::restore`] validates before returning a fresh runtime. [`Host`]
//! supplies limits, journal lineage, host callbacks/types/hooks, and capabilities.
//! Moonseed automatically re-registers its library symbols. Use
//! [`Host::libraries`] to refuse snapshots retaining a disallowed library;
//! this allowlist grants no external authority and installs no new globals.
//! Persist the [`Journal`] and external resource/dispatch ledger separately.
//! Restore never undoes external effects or captures the future external world;
//! exactly-once effects require durable coordination by the embedder.
//!
//! Snapshot schema 25, bytecode revision 14, table revision 4, fuel revision 7,
//! GC revision 12, and binary-chunk format 2 are exact version boundaries.
//! Snapshots are not a durable cross-version save format. Decoding rejects
//! incompatible revisions, malformed state, missing registrations, denied
//! libraries, wrong effect domains, and exceeded limits with [`SnapshotError`].
//! [`Error`] distinguishes catchable [`LuaError`], host [`ApiError`], and
//! execution-integrity [`VmError`]. Driving APIs preserve their documented
//! signatures: an uncaught Lua error is [`StepOutcome::LuaError`]; a snapshot
//! decode failure from restore is `Error::Vm(VmError::Snapshot(..))`.
//! Output/error metadata and outcomes marked `non_exhaustive` need fallback
//! arms in downstream matches. Input records with public fields retain their
//! documented construction contract. Deprecated integer-only APIs and
//! `doc(hidden)` proof/measurement helpers are outside the 0.1 embedding API.
//! The latter remain Rust-reachable for workspace native/Wasm qualification.
//!
//! Explore [`api`] for rooted operations and [`hostcaps`] for capability
//! contracts and deterministic backends. The repository embedding guide and
//! ten executable examples provide lifecycle and sandbox recipes.
//! The library MSRV is Rust 1.88. Minor 0.x releases may raise it with a
//! changelog entry. See the [compatibility policy](https://github.com/Tavrin/moonseed/blob/main/docs/COMPATIBILITY_POLICY.md)
//! for Rust API, Lua semantics, format and determinism boundaries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "__measure")]
mod measure;
#[cfg(feature = "__measure")]
#[doc(hidden)] // Unstable benchmark helpers; outside the embedding API.
pub use measure::{Sample, collect as measure, write_listing};

/// Rooted embedding operations; also re-exported at the crate root.
pub mod api;
#[macro_use]
mod counters_macros;
mod arith;
mod ast;
mod base;
mod check;
mod chunk;
mod chunkname;
mod civil;
mod compare;
mod compile;
mod concat;
mod corolib;
#[cfg(feature = "counters")]
#[doc(hidden)]
pub mod counters;
mod debuginfo;
mod debuglib;
mod error;
mod fornum;
mod gc;
#[cfg(any(test, feature = "__measure"))]
mod gc_reference;
mod hashutil;
mod heap;
mod host;
/// Explicit, optional host authority and deterministic reference backends.
pub mod hostcaps;
mod hostproof;
mod id;
mod index;
mod iolib;
mod lex;
mod library;
mod limits;
mod observe;
mod opcode;
mod ops;
mod oslib;
mod package;
mod parse;
mod program;
mod runtime;
mod snapshot;
mod span;
mod strformat;
mod strlib;
mod strpack;
mod strpat;
mod table;
mod userdata;
mod utf8;
mod utf8lib;
mod value;

pub use api::{
    AnyUserData, ApiError, CallOutcome, Coerce, Completion, ConversionError, Error, FromLua,
    FromLuaMulti, Function, FunctionKind, HookAction, HookContext, HookEvent, HookFunction,
    HookInfo, HookMask, HookSettings, Host, HostCapabilities, HostEnv, IntoLua, IntoLuaMulti,
    Libraries, LuaError, LuaString, ModuleResolver, MultiValue, NativeContext, NativeReturn,
    Resolved, ResolverPolicy, Result, Resume, ResumeOutcome, RuntimeBuilder, StrRef, Table, Thread,
    ThreadStatus, UserDataRefMut, Value, ValueRef, Variadic, WaitInfo, WaitRequest,
};
pub use base::register_base;
pub use compile::{CompileLimits, CompiledChunk, compile, compile_with_limits};
pub use corolib::register_coroutine;
pub use debuglib::register_debug;
pub use error::{CompileError, CompileErrorKind};
pub use host::{
    EffectId, EffectRecord, HostRegistry, HostValue, Journal, JournalError, JournalPayload,
    LegacyCompletion, LuaType, NativeCall, NativeFn, NativeOutcome, NativePolicy, NativeValue,
    RawSetError, UserdataError, Warnings, register_userdata_proof,
};
#[cfg(test)]
use host::{ProofCounter, ProofHandle};

pub use hostcaps::{
    CapabilityPoll, CapabilityRequest, CapabilityValue, CivilOffset, CivilTime, Clock,
    Completion as CapabilityCompletion, EffectClass, Environment, Filesystem, HandlePolicy,
    HostIoError, HostIoErrorKind, MemoryFilesystem, MemoryOptions, OpenMode, PendingToken,
    PipeMode, Process, ProcessStatus, ResourceId, Stdio, Stream, memory_filesystem,
};
#[cfg(feature = "native-host")]
pub use hostcaps::{NativeFilesystem, NativeOptions, native_host};
pub use id::{
    ExitStatus, LuaFault, ObjectId, PauseReason, Root, RootError, SnapshotError, StepOutcome,
    TerminationReason, VmError, WaitError, WaitKey,
};
pub use iolib::register_io;
pub use library::{register_math, register_standard, register_table};
pub use observe::{Observation, ObserveError};
pub use oslib::register_os;
pub use package::register_package;
pub use runtime::{Config, GcMode, Limits, MemoryUsage, Runtime};
pub use span::{Span, line_col};
pub use strlib::register_string;
pub use userdata::{
    HostLightKey, HostUserdata, LightUserdata, MAX_REBIND_KEY, PortableUserdata, RebindError,
    RebindUserdata, UserdataPolicy,
};
pub use utf8lib::register_utf8;

#[cfg(test)]
mod prove;

impl Runtime {
    /// Instantiate a compiled chunk. Its `_ENV` is this runtime's globals
    /// table, a fresh empty table. Unstable API.
    pub fn load_chunk(
        config: Config,
        registry: HostRegistry,
        chunk: &CompiledChunk,
    ) -> Result<Self, VmError> {
        Self::boot(config, registry, &chunk.proto, false)
    }

    /// [`Self::load_chunk`], with `args` as the chunk's arguments: `...` in
    /// the chunk gives them (ADR 0028). A string is made in the new heap;
    /// `HostValue::Object` must name one of its objects. More arguments than
    /// the stack bound holds is `VmError::StackLimit`. Unstable API.
    pub fn load_chunk_with_args(
        config: Config,
        registry: HostRegistry,
        chunk: &CompiledChunk,
        args: &[HostValue],
    ) -> Result<Self, VmError> {
        let mut runtime = Self::boot(config, registry, &chunk.proto, false)?;
        runtime.set_entry_args(args)?;
        Ok(runtime)
    }

    /// A compiled chunk as a function value in this runtime, not yet
    /// called, as Lua's `load` makes one (ADR 0031): a vararg Lua closure
    /// whose `_ENV` is this runtime's globals table. Nothing roots it: root
    /// it with [`Runtime::root_id`] before the next `run`, or store it
    /// where Lua can reach it. Unstable API.
    pub fn load_function(&mut self, chunk: &CompiledChunk) -> Result<ObjectId, VmError> {
        self.instantiate_chunk(chunk)
    }

    #[doc(hidden)] // Kernel proof fixture; outside the embedding API.
    pub fn boot_canonical(config: Config, registry: HostRegistry) -> Result<Self, VmError> {
        Self::boot(config, registry, &program::canonical(), false)
    }

    #[doc(hidden)] // Kernel proof fixture; outside the embedding API.
    pub fn boot_park(config: Config, registry: HostRegistry) -> Result<Self, VmError> {
        Self::boot(config, registry, &program::park_program(), false)
    }

    #[doc(hidden)] // Kernel proof fixture; outside the embedding API.
    pub fn boot_yield(config: Config, registry: HostRegistry) -> Result<Self, VmError> {
        Self::boot(config, registry, &program::yield_program(), true)
    }

    #[doc(hidden)] // Kernel proof fixture; outside the embedding API.
    pub fn boot_spin(config: Config, registry: HostRegistry) -> Result<Self, VmError> {
        Self::boot(config, registry, &program::spin_adds(), false)
    }
}

/// Run the table-semantics fixture, checkpoint after the current key is
/// deleted, restore, and pack the traversal and border registers.
///
/// Native and wasm32 both call this. The packed integer is the cross-target
/// observation, not a public Lua value.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn table_semantics_fingerprint() -> Result<i64, VmError> {
    let proof = program::table_semantics_program();
    let mut runtime = Runtime::boot(Config::default(), HostRegistry::proof(), &proof.spec, false)?;
    let mut journal = Journal::new();
    for _ in 0..proof.resume_at {
        match runtime.run(1, &mut journal)? {
            StepOutcome::Paused(_) => {}
            _ => return Err(VmError::Corrupt),
        }
    }
    let domain = runtime.effect_domain();
    let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
    let mut runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
        .map_err(|_| VmError::Corrupt)?;
    match runtime.run_until_terminal(u64::MAX, &mut journal)? {
        StepOutcome::Completed => {}
        _ => return Err(VmError::Corrupt),
    }
    let mut packed = 0i64;
    for (shift, reg) in [10u8, 12, 14, 16, 18, 19, 20, 21, 22, 23, 26]
        .into_iter()
        .enumerate()
    {
        let piece = match runtime.entry_slot(reg)? {
            value::Value::Nil => 0,
            value::Value::Integer(integer) if (0..16).contains(&integer) => integer,
            _ => return Err(VmError::Corrupt),
        };
        packed |= piece << (shift * 4);
    }
    if !matches!(runtime.entry_slot(16)?, value::Value::Nil) {
        return Err(VmError::Corrupt);
    }
    Ok(packed)
}

/// Compile the closure fixture, checkpoint halfway, restore, and pack `1,1,2,2`.
///
/// Native and wasm32 both call this. The integer is a cross-target check, not
/// a public Lua value. Source spans are not part of the snapshot.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_closure_fingerprint() -> Result<i64, VmError> {
    const SOURCE: &[u8] = include_bytes!("../fixtures/lua/closure_pair.lua");
    let chunk = compile::compile(SOURCE).map_err(|_| VmError::Corrupt)?;
    if chunk.prototype_count() < 3
        || chunk.max_registers() == 0
        || chunk.mapped_instructions() != chunk.instruction_count()
    {
        return Err(VmError::Corrupt);
    }
    let steps = {
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )?;
        let mut journal = Journal::new();
        match runtime.run_until_terminal(u64::MAX, &mut journal)? {
            StepOutcome::Completed => runtime.fuel_consumed(),
            _ => return Err(VmError::Corrupt),
        }
    };
    if steps < 2 {
        return Err(VmError::Corrupt);
    }
    let values = run_with_checkpoint(&chunk.proto, |_| Ok(()), |_, step| step == steps / 2)?;
    pack_results(&values, 4, 1..=2)
}

/// Compile the branch-close fixture, checkpoint just after its
/// `CloseUpvalues`, restore, and pack `10,11,11,99`. Native and wasm32 both
/// call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_branch_fingerprint() -> Result<i64, VmError> {
    const SOURCE: &[u8] = include_bytes!("../fixtures/lua/branch_close.lua");
    let chunk = compile::compile(SOURCE).map_err(|_| VmError::Corrupt)?;
    let close_pc = chunk
        .proto
        .ops
        .iter()
        .position(|op| matches!(op, opcode::Op::CloseUpvalues { .. }))
        .ok_or(VmError::Corrupt)?;
    let after = u32::try_from(close_pc + 1).map_err(|_| VmError::Corrupt)?;
    let values = run_with_checkpoint(
        &chunk.proto,
        |_| Ok(()),
        |runtime, _| entry_frame_pc(runtime) == Some(after),
    )?;
    pack_results(&values, 4, 10..=99)
}

/// Compile the `while`, `repeat`, `break`, and numeric `for` fixtures,
/// checkpoint each halfway through, restore, and fold the results. Native
/// and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_loops_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 5] = [
        include_bytes!("../fixtures/lua/while_capture.lua"),
        include_bytes!("../fixtures/lua/repeat_capture.lua"),
        include_bytes!("../fixtures/lua/break_block.lua"),
        include_bytes!("../fixtures/lua/for_capture.lua"),
        include_bytes!("../fixtures/lua/for_bounds.lua"),
    ];
    let mut folded = 0i64;
    for source in SOURCES {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let steps = {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )?;
            let mut journal = Journal::new();
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => runtime.fuel_consumed(),
                _ => return Err(VmError::Corrupt),
            }
        };
        for value in run_with_checkpoint(&chunk.proto, |_| Ok(()), |_, step| step == steps / 2)? {
            let value::Value::Integer(integer) = value else {
                return Err(VmError::Corrupt);
            };
            folded = folded.wrapping_mul(131).wrapping_add(integer);
        }
    }
    Ok(folded)
}

/// Compile the constructor, indexed-assignment, globals, and `_ENV`
/// fixtures, checkpoint each halfway, restore, and fold the results.
/// Native and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_tables_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 5] = [
        include_bytes!("../fixtures/lua/table_ctor.lua"),
        include_bytes!("../fixtures/lua/table_index.lua"),
        include_bytes!("../fixtures/lua/assign_index.lua"),
        include_bytes!("../fixtures/lua/globals.lua"),
        include_bytes!("../fixtures/lua/env_shadow.lua"),
    ];
    let mut folded = 0i64;
    for source in SOURCES {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let steps = {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )?;
            let mut journal = Journal::new();
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => runtime.fuel_consumed(),
                _ => return Err(VmError::Corrupt),
            }
        };
        for value in run_with_checkpoint(&chunk.proto, |_| Ok(()), |_, step| step == steps / 2)? {
            let part = match value {
                value::Value::Integer(integer) => integer,
                value::Value::Bool(bit) => 1000 + i64::from(bit),
                value::Value::Nil => -1,
                _ => return Err(VmError::Corrupt),
            };
            folded = folded.wrapping_mul(131).wrapping_add(part);
        }
    }
    Ok(folded)
}

/// Bind the proof registry's native functions as globals of the same name.
fn bind_proof_natives(runtime: &mut Runtime) -> Result<(), VmError> {
    for symbol in ["add", "sub", "many", "none", "second", "upto"] {
        runtime.set_global_native(symbol, symbol)?;
    }
    runtime.install_base()?;
    // Strings take part in arithmetic through the string metatable
    // (ADR 0034).
    runtime.install_string()
}

/// Compile the native-function and metatable fixtures, bind the natives and
/// base functions, checkpoint each
/// run halfway, restore (which rebinds the natives by symbol), and fold the
/// results. Native and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_natives_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 7] = [
        include_bytes!("../fixtures/lua/native_calls.lua"),
        include_bytes!("../fixtures/lua/native_results.lua"),
        include_bytes!("../fixtures/lua/meta_index.lua"),
        include_bytes!("../fixtures/lua/meta_newindex.lua"),
        include_bytes!("../fixtures/lua/meta_len.lua"),
        include_bytes!("../fixtures/lua/meta_protect.lua"),
        include_bytes!("../fixtures/lua/meta_mutate.lua"),
    ];
    let mut folded = 0i64;
    for source in SOURCES {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let steps = {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )?;
            bind_proof_natives(&mut runtime)?;
            let mut journal = Journal::new();
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => runtime.fuel_consumed(),
                _ => return Err(VmError::Corrupt),
            }
        };
        let (values, strings) =
            run_with_checkpoint_strings(&chunk.proto, bind_proof_natives, steps / 2)?;
        for (value, text) in values.into_iter().zip(strings) {
            let part = match value {
                value::Value::Integer(integer) => integer,
                value::Value::Bool(bit) => 1000 + i64::from(bit),
                value::Value::Nil => -1,
                value::Value::String(_) => text.iter().fold(7i64, |acc, byte| {
                    acc.wrapping_mul(31).wrapping_add(i64::from(*byte))
                }),
                _ => return Err(VmError::Corrupt),
            };
            folded = folded.wrapping_mul(131).wrapping_add(part);
        }
    }
    Ok(folded)
}

/// Where automatic collections ran, for four garbage-making loops under a
/// small object limit, stepped one instruction at a time and restored from a
/// snapshot partway. Each collection folds in the fuel consumed when it ran
/// and the objects left. A target with a different collection schedule gets
/// a different number.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn gc_schedule_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 4] = [
        b"local s = 0 for i = 1, 1500 do local x = 'n' .. i .. '.' s = s + #x end return s",
        b"local s = 0 for i = 1, 1500 do local t = { i, i + 1 } s = s + t[2] end return s",
        b"local s = 0 for i = 1, 1500 do local f = function() return i end s = s + f() end return s",
        b"local t = setmetatable({}, { __index = function(self, key) return { 1 } end }) \
          local s = 0 for i = 1, 1500 do s = s + t.foo[1] end return s",
    ];
    let config = Config {
        max_objects: 400,
        gc_min_debt: 2048,
        ..Config::default()
    };
    let mut folded = 0i64;
    let mut fold = |part: u64| folded = folded.wrapping_mul(131).wrapping_add(part as i64);
    for source in SOURCES {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let mut runtime =
            Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false)?;
        bind_proof_natives(&mut runtime)?;
        let mut journal = Journal::new();
        let mut seen = runtime.memory().collections;
        let mut steps = 0u64;
        loop {
            let outcome = runtime.run(1, &mut journal)?;
            let memory = runtime.memory();
            if memory.collections != seen {
                seen = memory.collections;
                fold(runtime.fuel_consumed());
                fold(u64::from(memory.objects));
            }
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                _ => return Err(VmError::Corrupt),
            }
            steps += 1;
            if steps == 4000 {
                let domain = runtime.effect_domain();
                let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
                runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
                    .map_err(|_| VmError::Corrupt)?;
            }
        }
        let memory = runtime.memory();
        if memory.collections < 5 {
            return Err(VmError::Corrupt);
        }
        for part in [
            memory.collections,
            memory.debt,
            memory.threshold,
            memory.logical_bytes,
        ] {
            fold(part);
        }
        match runtime.entry_results()?.first() {
            Some(value::Value::Integer(result)) => fold(*result as u64),
            _ => return Err(VmError::Corrupt),
        }
    }
    Ok(folded)
}

/// The operator fixtures, each checkpointed halfway, and programs that
/// wait in a native operator handler or `__call`, restored while waiting.
/// Floats fold by their bits, so the `pow` corpus below is compared exactly
/// between native and wasm32.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_operators_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 10] = [
        include_bytes!("../fixtures/lua/meta_unary.lua"),
        include_bytes!("../fixtures/lua/meta_call.lua"),
        include_bytes!("../fixtures/lua/arith.lua"),
        include_bytes!("../fixtures/lua/arithk.lua"),
        include_bytes!("../fixtures/lua/cmpbr.lua"),
        include_bytes!("../fixtures/lua/bitwise.lua"),
        include_bytes!("../fixtures/lua/meta_arith.lua"),
        include_bytes!("../fixtures/lua/meta_compare.lua"),
        include_bytes!("../fixtures/lua/concat.lua"),
        b"return 2 ^ 10, 2 ^ 0.5, 10 ^ -2, 2 ^ 53, 3 ^ 40, 1.5 ^ 2.5, 2 ^ -1074, 10 ^ 308, 7 ^ 0.5, 0.5 ^ 3",
    ];
    const WAITING: [&[u8]; 7] = [
        b"local t = setmetatable({}, { __add = park }) local v = t + 1 return v",
        b"local t = setmetatable({}, { __sub = park }) return 3 - t",
        b"local mt = { __eq = park } return setmetatable({}, mt) ~= setmetatable({}, mt)",
        b"local c = setmetatable({}, { __call = park }) local t = setmetatable({}, { __concat = c }) return 'x' .. t",
        b"local t = setmetatable({}, { __lt = park }) if 1 < t then return 11 end return 22",
        b"local t = setmetatable({}, { __le = park }) if not (1 <= t) then return 11 end return 22",
        b"local mt = { __eq = park } local a, b = setmetatable({}, mt), setmetatable({}, mt) if a ~= b then return 11 end return 22",
    ];
    fold_checkpointed_sources(&SOURCES, Vec::new(), &WAITING)
}

/// The `<close>` fixtures and the coroutine close cases, each checkpointed
/// halfway, and closes that wait in a native at a scope's exit, in an
/// unwind, and in a return, restored while waiting (ADR 0026).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_close_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 4] = [
        include_bytes!("../fixtures/lua/close_basic.lua"),
        include_bytes!("../fixtures/lua/close_error.lua"),
        include_bytes!("../fixtures/lua/close_xpcall.lua"),
        include_bytes!("../fixtures/lua/close_meta.lua"),
    ];
    const WAITING: [&[u8]; 3] = [
        b"local n = 0 do local a <close> = setmetatable({}, { __close = function() n = n + 1 end }) \
          local b <close> = setmetatable({}, { __close = park }) end return n",
        b"local ok, e = pcall(function() local b <close> = setmetatable({}, { __close = park }) \
          error('x', 0) end) return ok, e",
        b"local f = function() local b <close> = setmetatable({}, { __close = park }) return 1, 2 end \
          return f()",
    ];
    let programs = program::CloseCase::ALL
        .into_iter()
        .map(program::close_case_program)
        .collect();
    fold_checkpointed_sources(&SOURCES, programs, &WAITING)
}

/// The generic `for` fixtures and the yielding-iterator coroutine, each
/// checkpointed halfway, and loops whose iterator or closing value waits
/// in a native, restored while waiting (ADR 0027).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_generic_for_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 7] = [
        include_bytes!("../fixtures/lua/gfor_basic.lua"),
        include_bytes!("../fixtures/lua/gfor_init.lua"),
        include_bytes!("../fixtures/lua/gfor_capture.lua"),
        include_bytes!("../fixtures/lua/gfor_close.lua"),
        include_bytes!("../fixtures/lua/gfor_error.lua"),
        include_bytes!("../fixtures/lua/gfor_callable.lua"),
        include_bytes!("../fixtures/lua/gfor_builtin.lua"),
    ];
    const WAITING: [&[u8]; 3] = [
        b"local n = 0 local closed = 0 \
          for x in park, nil, nil, setmetatable({}, { __close = function() closed = closed + 1 end }) do \
            n = n + x break end return n, closed",
        b"local n = 0 for x in upto, 4, nil, setmetatable({}, { __close = park }) do n = n + x end \
          return n",
        b"local f = function() for x in upto, 4, nil, setmetatable({}, { __close = park }) do \
          if x == 3 then return x, x * 2 end end end return f()",
    ];
    fold_checkpointed_sources(
        &SOURCES,
        vec![program::generic_for_yield_program()],
        &WAITING,
    )
}

/// The vararg fixtures, the hand-built vararg programs (a nested call over
/// a caller's extras, a yield and a yielding close with extras live), each
/// checkpointed halfway; vararg frames waiting in a native, restored while
/// waiting; and a chunk given arguments, checkpointed halfway (ADR 0028).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_varargs_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 2] = [
        include_bytes!("../fixtures/lua/vararg_basic.lua"),
        include_bytes!("../fixtures/lua/vararg_frames.lua"),
    ];
    const WAITING: [&[u8]; 2] = [
        b"local f = function(a, ...) local w = park() return w, a, select('#', ...), ... end \
          return f(1, nil, 3, 'x')",
        b"local f = function(...) local x <close> = setmetatable({}, { __close = park }) \
          return ... end return f(1, nil, 3)",
    ];
    let programs = vec![
        program::vararg_overlap_program(20),
        program::vararg_coroutine_program(),
    ];
    let mut folded = fold_checkpointed_sources(&SOURCES, programs, &WAITING)?;
    let chunk = compile::compile(b"local n = select('#', ...) local a, b = ... return n, b, ...")
        .map_err(|_| VmError::Corrupt)?;
    let args = [
        HostValue::Integer(10),
        HostValue::Nil,
        HostValue::Number(0.5),
        HostValue::String(b"s".to_vec()),
    ];
    let mut runtime =
        Runtime::load_chunk_with_args(Config::default(), HostRegistry::proof(), &chunk, &args)?;
    runtime.install_base()?;
    let mut journal = Journal::new();
    for _ in 0..3 {
        if runtime.run(1, &mut journal)? != StepOutcome::Paused(PauseReason::FuelExhausted) {
            return Err(VmError::Corrupt);
        }
    }
    let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
    let mut runtime =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), 1).map_err(|_| VmError::Corrupt)?;
    if runtime.run_until_terminal(u64::MAX, &mut journal)? != StepOutcome::Completed {
        return Err(VmError::Corrupt);
    }
    for value in runtime.results()? {
        let part = match value {
            HostValue::Integer(integer) => integer,
            HostValue::Number(float) => float.to_bits() as i64,
            HostValue::Nil => -1,
            HostValue::String(bytes) => bytes.iter().map(|byte| i64::from(*byte)).sum(),
            _ => return Err(VmError::Corrupt),
        };
        folded = folded.wrapping_mul(131).wrapping_add(part);
    }
    Ok(folded)
}

/// The tail-call fixtures but the deep one, each checkpointed halfway, and
/// tail calls to a native that waits, restored while waiting: from a vararg
/// frame, from a metamethod, from a thread's first frame, and at the end of
/// a tail recursion inside `pcall` (ADR 0029).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_tail_calls_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 2] = [
        include_bytes!("../fixtures/lua/tail_basic.lua"),
        include_bytes!("../fixtures/lua/tail_pcall.lua"),
    ];
    const WAITING: [&[u8]; 4] = [
        b"local f = function(...) return park(...) end local a, b = f(1) return a, b",
        b"local t = setmetatable({}, { __len = function() return park() end }) return #t",
        b"local t = setmetatable({}, {}) return park(t)",
        b"local f f = function(n) if n == 0 then return park() end return f(n - 1) end \
          return pcall(f, 5000)",
    ];
    fold_checkpointed_sources(&SOURCES, Vec::new(), &WAITING)
}

/// The `and` / `or` / `not` and method fixtures, each checkpointed halfway,
/// and waits inside a short-circuit, a method call's argument, and at the
/// end of a `local function`'s and a method's tail recursion, restored
/// while waiting.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_syntax_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 2] = [
        include_bytes!("../fixtures/lua/logic_ops.lua"),
        include_bytes!("../fixtures/lua/methods.lua"),
    ];
    const WAITING: [&[u8]; 4] = [
        b"local n = 0 local r = park() or (function() n = n + 1 return 1 end)() return r, n",
        b"local o = setmetatable({}, { __index = { m = function(self, x) return x + 1 end } }) \
          return o:m(park())",
        b"local function loop(n) if n == 0 then return park() end return loop(n - 1) end \
          return loop(1000)",
        b"local o = {} function o:f(n) if n == 0 then return park() end return self:f(n - 1) end \
          return o:f(1000)",
    ];
    fold_checkpointed_sources(&SOURCES, Vec::new(), &WAITING)
}

/// The `goto` fixtures, each checkpointed halfway, and a `__close` that
/// waits because a goto leaves its scope, and a wait inside a backward goto
/// loop, restored while waiting (ADR 0030).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_goto_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 2] = [
        include_bytes!("../fixtures/lua/goto_basic.lua"),
        include_bytes!("../fixtures/lua/goto_close.lua"),
    ];
    const WAITING: [&[u8]; 2] = [
        b"local n, closes = 0, 0 ::again:: do \
          local v <close> = setmetatable({}, { __close = function() \
            if n == 1 then closes = closes + park() else closes = closes + 1 end end }) \
          n = n + 1 if n < 2 then goto again end end return n, closes",
        b"local n, s = 0, 0 ::top:: n = n + 1 if n == 3 then s = s + park() end \
          if n < 5 then goto top end return n, s",
    ];
    fold_checkpointed_sources(&SOURCES, Vec::new(), &WAITING)
}

/// The base-library fixtures, each run straight and again with a
/// checkpoint halfway, with `print` writing to a buffer on both runs; and
/// `tostring`, `print`, and a `load` reader that wait, restored while
/// waiting. Folds the results and everything written. Native and wasm32
/// both call this (ADR 0031).
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_base_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 6] = [
        include_bytes!("../fixtures/lua/base_env.lua"),
        include_bytes!("../fixtures/lua/base_tostring.lua"),
        include_bytes!("../fixtures/lua/base_tonumber.lua"),
        include_bytes!("../fixtures/lua/base_iter.lua"),
        include_bytes!("../fixtures/lua/base_gc.lua"),
        include_bytes!("../fixtures/lua/base_load.lua"),
    ];
    const WAITING: [&[u8]; 3] = [
        b"local t = setmetatable({}, { __tostring = function() return tostring(park()) end }) \
          print('w', t) return tostring(t) == '7', _G == _ENV",
        b"local n = 0 local f = load(function() n = n + 1 if n == 1 then return 'return 5 + ' \
          elseif n == 2 then return park() end end) return f(), n",
        b"local s = 0 for i, v in ipairs(setmetatable({}, { __index = function(t, i) \
          if i < 3 then return park() end end })) do s = s + i * v end \
          for k, v in pairs(setmetatable({}, { __pairs = function(t) return next, { a = park() } end })) do s = s + v end \
          return s, tonumber('z', 36), next({})",
    ];
    fold_sources_with_output(&SOURCES, &WAITING, Runtime::install_base)
}

/// The math and table fixtures, each run straight and again with a
/// checkpoint halfway, with their output; and a sort's order function,
/// a table function's `__len`, and `math.max`'s `__lt` that wait,
/// restored while waiting (ADR 0032, ADR 0033). Native and wasm32 both
/// call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_library_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 4] = [
        include_bytes!("../fixtures/lua/lib_math.lua"),
        include_bytes!("../fixtures/lua/lib_random.lua"),
        include_bytes!("../fixtures/lua/lib_table.lua"),
        include_bytes!("../fixtures/lua/lib_sort.lua"),
    ];
    const WAITING: [&[u8]; 4] = [
        // Builtins calling builtins a thousand deep: on wasm32 this trapped
        // before calls were deferred (ADR 0033).
        b"local t = setmetatable({ 1, 2, 3 }, { __newindex = table.insert }) \
          local ok = pcall(table.insert, t, 1, 'x') return ok, rawlen(t)",
        b"local t = { 5, 3, 9, 1, 7 } \
          table.sort(t, function(a, b) if a == 9 then park() end return a < b end) \
          return table.concat(t, ','), math.random(1, 100)",
        b"local t = setmetatable({}, { __len = function() return park() end }) \
          table.insert(t, 'v') return rawget(t, 8), table.unpack({ 1, 2, 3 }, 2)",
        b"local lt = { __lt = function(a, b) return park() == 7 end } \
          return math.max(setmetatable({ 1 }, lt), setmetatable({ 2 }, lt))[1], \
          select('#', table.unpack(table.pack(1, nil, 3), 1, 3))",
    ];
    fold_sources_with_output(&SOURCES, &WAITING, Runtime::install_standard)
}

/// The string fixtures, each run straight and again with a checkpoint
/// halfway, with their output; `string.dump` bytes, packed floats, and
/// formatted numbers, which must be the same bytes on every target; and
/// a `gsub` replacement, a `%s` conversion, a `gmatch` loop, and the
/// string metatable's `__index` that wait, restored while waiting
/// (ADR 0034, ADR 0035, ADR 0036). Native and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_string_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 7] = [
        include_bytes!("../fixtures/lua/lib_string.lua"),
        include_bytes!("../fixtures/lua/lib_pattern.lua"),
        include_bytes!("../fixtures/lua/lib_format.lua"),
        include_bytes!("../fixtures/lua/lib_pack.lua"),
        include_bytes!("../fixtures/lua/lib_dump.lua"),
        b"local function hex(s) return (s:gsub('.', function(c) \
            return string.format('%02x', c:byte()) end)) end \
          local f = function(a, b, ...) local t = { a, b, ... } return #t, select('#', ...), 'k' .. a end \
          local d = string.dump(f) print(#d, hex(d)) print(load(d, 'd', 'b')(1, 2, 3, 4)) \
          print(hex(string.dump(function() return f end, true))) \
          print(hex(string.pack('<d >d f n j i3 !8 s1', math.pi, -0.1, 1 / 3, 2 ^ -1074, \
            math.mininteger, -5, 'q'))) \
          local out = {} for i = 1, 200 do \
            out[#out + 1] = string.format('%.17g %a %.3e %g', i / 7 * 10 ^ (i % 40 - 20), i * 1.1, \
              2 ^ (i - 100), i / 3) end \
          print(table.concat(out, ',')) \
          return string.gsub(string.rep('abc', 50), '(b)(c)', '%2%1')",
        // UTF-8 extended encoding and scans, including a mid-run checkpoint.
        b"local t={} for i=1,600 do t[i]=0x7fffffff end local s=utf8.char(table.unpack(t)) local n=utf8.len(s,1,-1,true) local sum=0 for p,c in utf8.codes(s,true) do sum=sum+p+c end local v={utf8.codepoint(s,1,#s,true)} return n,#v,sum,utf8.offset(s,-500),pcall(utf8.codepoint,s..'\\255',1,#s+1,true)",
    ];
    const WAITING: [&[u8]; 4] = [
        b"return string.gsub('a-b', '%a', function(c) return park() end)",
        b"local t = setmetatable({}, { __tostring = function() return tostring(park()) end }) \
          return string.format('%s|%5s', t, t)",
        b"local it = ('a b c'):gmatch('%a') local x = it() park() return x, it(), it()",
        b"getmetatable('').__index = function(s, k) return park() end return ('x').anything",
    ];
    fold_sources_with_output(&SOURCES, &WAITING, Runtime::install_standard)
}

/// The debug and package corpora, each run straight and again with a
/// checkpoint halfway, with their output; an unstripped `string.dump`,
/// whose debug information must be the same bytes on every target; and a
/// loader and a traceback's name search that wait, restored while
/// waiting (ADR 0039, ADR 0040). Native and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_debug_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 3] = [
        include_bytes!("../fixtures/lua/corpus_debug.lua"),
        include_bytes!("../fixtures/lua/corpus_package.lua"),
        b"local function hex(s) return (s:gsub('.', function(c) \
            return string.format('%02x', c:byte()) end)) end \
          local function f(a, ...) local t = { a, ... } for i = 1, #t do t[i] = t[i] * 2 end \
            return print(t[1]), debug.getinfo(1, 'l').currentline end \
          print(hex(string.dump(f)), hex(string.dump(f, true))) \
          return debug.traceback('x')",
    ];
    const WAITING: [&[u8]; 2] = [
        b"package.preload.w = function(name) return park() end \
          return require('w'), package.loaded.w",
        b"for i = 1, 1000 do _G['g' .. i] = i end \
          local function f() local t = debug.traceback('m') park() return t end \
          local r = f() return r",
    ];
    fold_sources_with_output(&SOURCES, &WAITING, |runtime| {
        runtime.install_standard()?;
        runtime.install_debug()
    })
}

/// The coroutine corpus, run straight and again with a checkpoint
/// halfway, with its output; and host waits inside a resumed coroutine,
/// a `wrap` function, a nested resume, and a close, restored while
/// waiting (ADR 0041). Native and wasm32 both call this.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_coroutine_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 4] = [
        include_bytes!("../fixtures/lua/corpus_coroutine.lua"),
        include_bytes!("../fixtures/lua/cmpbr_yield.lua"),
        include_bytes!("../fixtures/lua/metamethod_transfer.lua"),
        b"local log = {} \
          local gen = coroutine.wrap(function(a, ...) \
            local n = select('#', ...) \
            for i = 1, 5 do a = a + (coroutine.yield(a, nil, i, n) or 0) end return 'end', a end) \
          for i = 1, 6 do log[#log + 1] = table.concat({ tostring(gen(i, nil, nil)) }, ',') end \
          local co = coroutine.create(function() local x <close> = setmetatable({}, \
            { __close = function(_, e) log[#log + 1] = 'closed ' .. tostring(e) end }) error('f', 0) end) \
          log[#log + 1] = tostring(select(2, coroutine.resume(co))) \
          log[#log + 1] = tostring(select(2, coroutine.close(co))) \
          print(table.concat(log, ';')) return coroutine.status(co)",
    ];
    const WAITING: [&[u8]; 4] = [
        b"local co = coroutine.create(function(a) local b = park() coroutine.yield(a + b) return park() end) \
          local ok1, r1 = coroutine.resume(co, 1) local ok2, r2 = coroutine.resume(co) return r1, r2",
        b"local w = coroutine.wrap(function() local x = park() error(x, 0) end) return pcall(w)",
        b"local inner = coroutine.create(function() return park() end) \
          return coroutine.wrap(function() return coroutine.resume(inner) end)()",
        b"local co = coroutine.create(function() local x <close> = setmetatable({}, \
            { __close = function() park() end }) coroutine.yield() end) \
          coroutine.resume(co) return coroutine.close(co)",
    ];
    fold_sources_with_output(&SOURCES, &WAITING, |runtime| {
        runtime.install_standard()?;
        runtime.install_debug()
    })
}

/// The userdata corpus and host-object programs, each checkpointed
/// halfway, and programs that wait on the host while holding userdata,
/// checkpointed at each wait: byte payloads, user values,
/// identity, light keys, metamethod results, the portable host counter,
/// and `upvalueid` tokens.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_userdata_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 2] = [
        include_bytes!("../fixtures/lua/corpus_userdata.lua"),
        b"local C = {} C.__index = C C.get = counter_get C.add = counter_add \
          local c = counter_new(5, C) local log = {} \
          for i = 1, 4 do log[#log + 1] = c:add(i):get() end \
          local t = { [c] = 'c', [light(3)] = 'l' } \
          print(table.concat(log, ','), t[c], t[light(3)], pcall(counter_get, newud(1))) \
          return c:get()",
    ];
    const WAITING: [&[u8]; 2] = [EXCHANGE, b"local b = newud(3, 1) udpoke(b, 2, park()) \
          debug.setuservalue(b, light(park())) return udpeek(b, 2), debug.getuservalue(b) == light(7)"];
    fold_sources_with_output(&SOURCES, &WAITING, install_userdata_proof)
}

/// Weak tables, ephemerons, finalizers, and warnings, the
/// incremental collector, and generational collection: the GC corpus, the `collectgarbage` corpus, and the
/// generational corpus checkpointed halfway and closed at the end, their
/// snapshot bytes after the run (the collector's state, ages, and event
/// hash included), and a finalizer waiting on the host, checkpointed
/// while it waits. Output and warnings go to one buffer.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_gc_semantics_fingerprint() -> Result<i64, VmError> {
    use std::cell::RefCell;
    use std::rc::Rc;
    const SOURCES: [&[u8]; 5] = [
        include_bytes!("../fixtures/lua/corpus_gc.lua"),
        // Tiny incremental steps: the halfway checkpoint is mid-cycle,
        // and the final snapshot holds the collector's event hash.
        include_bytes!("../fixtures/lua/corpus_incgc.lua"),
        // Young and major collections, mode switches, ages.
        include_bytes!("../fixtures/lua/corpus_gengc.lua"),
        // `step` by state: young, major, falling back, returning.
        include_bytes!("../fixtures/lua/corpus_genstep.lua"),
        b"collectgarbage('stop') local log = {} \
          local function make() setmetatable({}, {__gc = function(o) \
            log[#log + 1] = 'wait' local x = park() log[#log + 1] = 'got ' .. x \
            setmetatable(o, getmetatable(o)) error('after', 0) end}) end \
          make() collectgarbage() collectgarbage() print(table.concat(log, ','))",
    ];
    let written = Rc::new(RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
        let sink = written.clone();
        runtime.set_warnings(Box::new(move |bytes, more| {
            let mut out = sink.borrow_mut();
            out.extend_from_slice(bytes);
            out.push(if more { b'+' } else { b'\n' });
        }));
    };
    let restore = |runtime: &Runtime| {
        Runtime::from_snapshot(
            &runtime.snapshot().map_err(|_| VmError::Corrupt)?,
            &HostRegistry::proof(),
            runtime.effect_domain(),
        )
        .map_err(|_| VmError::Corrupt)
    };
    let mut folded = 0i64;
    let mut fold = |bytes: &[u8]| {
        for byte in bytes {
            folded = folded.wrapping_mul(131).wrapping_add(i64::from(*byte));
        }
    };
    for source in SOURCES {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let boot = || -> Result<Runtime, VmError> {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )?;
            install_userdata_proof(&mut runtime)?;
            runtime.set_global_native("park", "park")?;
            Ok(runtime)
        };
        // Straight, to learn the fuel, then checkpointed halfway.
        let mut runtime = boot()?;
        let mut journal = Journal::new();
        let fuel = loop {
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => break runtime.fuel_consumed(),
                StepOutcome::Waiting(key) => runtime
                    .complete_wait(key, 7)
                    .map_err(|_| VmError::Corrupt)?,
                _ => return Err(VmError::Corrupt),
            }
        };
        written.take();
        let mut runtime = boot()?;
        attach(&mut runtime);
        let mut journal = Journal::new();
        for _ in 0..fuel / 2 {
            match runtime.run(1, &mut journal)? {
                StepOutcome::Paused(_) => {}
                StepOutcome::Waiting(key) => runtime
                    .complete_wait(key, 7)
                    .map_err(|_| VmError::Corrupt)?,
                _ => return Err(VmError::Corrupt),
            }
        }
        runtime = restore(&runtime)?;
        attach(&mut runtime);
        loop {
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => break,
                StepOutcome::Waiting(key) => {
                    runtime = restore(&runtime)?;
                    attach(&mut runtime);
                    runtime
                        .complete_wait(key, 7)
                        .map_err(|_| VmError::Corrupt)?;
                }
                _ => return Err(VmError::Corrupt),
            }
        }
        fold(&runtime.snapshot().map_err(|_| VmError::Corrupt)?);
        // Closing runs the finalizers left, which may wait too.
        runtime.begin_close()?;
        loop {
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => break,
                StepOutcome::Waiting(key) => {
                    runtime = restore(&runtime)?;
                    attach(&mut runtime);
                    runtime
                        .complete_wait(key, 8)
                        .map_err(|_| VmError::Corrupt)?;
                }
                _ => return Err(VmError::Corrupt),
            }
        }
        fold(&written.take());
    }
    Ok(folded)
}

/// A program that waits on the host holding userdata of every kind.
const EXCHANGE: &[u8] = b"local mt = { __index = { get = counter_get, add = counter_add } } \
    local c = counter_new(40, mt) \
    local b = newud(5, 2) udpoke(b, 4, 99) \
    debug.setuservalue(b, c, 1) debug.setuservalue(b, light(77), 2) \
    local x = 1 local f = function() return x end local g = function() return x end \
    local keys = { [light(1)] = 'l1', [b] = 'b', [debug.upvalueid(f, 1)] = 'cell' } \
    local by = park() c:add(by) \
    return c:get(), udpeek(b, 4), keys[light(1)], keys[b], keys[debug.upvalueid(g, 1)], \
      debug.getuservalue(b, 2) == light(77), debug.getuservalue(b, 1) == c";

fn install_userdata_proof(runtime: &mut Runtime) -> Result<(), VmError> {
    runtime.install_standard()?;
    runtime.install_debug()?;
    for (name, _) in host::USERDATA_NATIVES {
        runtime.set_global_native(name, name)?;
    }
    Ok(())
}

/// A snapshot of the exchange fixture waiting on the host, for the native and
/// wasm32 targets to restore each other's.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn userdata_exchange_snapshot() -> Result<Vec<u8>, VmError> {
    let chunk = compile::compile(EXCHANGE).map_err(|_| VmError::Corrupt)?;
    let mut runtime = Runtime::boot(
        Config::default(),
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )?;
    install_userdata_proof(&mut runtime)?;
    runtime.set_global_native("park", "park")?;
    match runtime.run_until_terminal(u64::MAX, &mut Journal::new())? {
        StepOutcome::Waiting(_) => runtime.snapshot().map_err(|_| VmError::Corrupt),
        _ => Err(VmError::Corrupt),
    }
}

/// Restore an [`userdata_exchange_snapshot`], answer its wait with 7, and
/// fold its results.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn userdata_exchange_finish(bytes: &[u8]) -> Result<i64, VmError> {
    let mut runtime =
        Runtime::from_snapshot(bytes, &HostRegistry::proof(), 1).map_err(|_| VmError::Corrupt)?;
    let mut journal = Journal::new();
    // `park` always waits on key 1.
    runtime
        .complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![HostValue::Integer(7)]),
        )
        .map_err(|_| VmError::Corrupt)?;
    if runtime.run_until_terminal(u64::MAX, &mut journal)? != StepOutcome::Completed {
        return Err(VmError::Corrupt);
    }
    let mut folded = 0i64;
    for value in runtime.results()? {
        let text = format!("{value:?}");
        for byte in text.bytes() {
            folded = folded.wrapping_mul(131).wrapping_add(i64::from(byte));
        }
    }
    Ok(folded)
}

const LARGE_EXCHANGE: &[u8] = br#"
    collectgarbage('stop')
    collectgarbage('generational', 1, 4)
    local keep = {}
    for i = 1, 40000 do keep[i] = {i} end
    for i = 1, 10000 do keep['key' .. i] = i * 3 end
    local a = string.rep('a', 2 << 20)
    local b = string.rep('b', 2 << 20)
    local c = string.rep('c', 2 << 20)
    local weak = setmetatable({keep[1]}, {__mode = 'v'})
    local counter = counter_new(40)
    local payload = newud(4096, 2)
    udpoke(payload, 0, 17) udpoke(payload, 4095, 99)
    debug.setuservalue(payload, counter, 1)
    debug.setuservalue(payload, b, 2)
    local cos = {}
    local function inside(i)
      local node = keep[i]
      local by = coroutine.yield(i)
      return node[1] + by + counter_get(debug.getuservalue(payload, 1))
    end
    for i = 1, 4 do
      cos[i] = coroutine.create(function(n) return 2 * inside(n) end)
      local ok, value = coroutine.resume(cos[i], i)
      assert(ok and value == i)
    end
    collectgarbage()
    collectgarbage('restart')
    local d = string.rep('d', 1 << 20)
    weak.gone = {}
    local by = park()
    counter_add(counter, by)
    local sum = 0
    for i = 1, 40000 do sum = sum + keep[i][1] end
    for i = 1, 10000 do sum = sum + keep['key' .. i] end
    for i = 1, 4 do
      local ok, value = coroutine.resume(cos[i], by)
      assert(ok and coroutine.status(cos[i]) == 'dead')
      sum = sum + value
    end
    assert(debug.getuservalue(payload, 2) == b)
    collectgarbage()
    assert(weak[1] == keep[1] and weak.gone == nil)
    print(sum, #keep, #a, #b, #c, #d, udpeek(payload, 0), udpeek(payload, 4095),
          counter_get(counter), string.byte(a, 1), string.byte(b, #b),
          string.byte(c, #c), string.byte(d, #d))
    return sum
"#;

fn check_large_exchange(runtime: &Runtime) -> Result<(), VmError> {
    let heap = runtime.heap();
    let strings: usize = heap.strings.iter().map(|(_, _, s)| s.bytes.len()).sum();
    let large_string = heap
        .strings
        .iter()
        .any(|(_, _, s)| s.bytes.len() >= 2 << 20);
    let mixed_table = heap.tables.iter().any(|(_, _, t)| {
        let mut array = 0;
        let mut hash = 0;
        for slot in t.table.slots() {
            if let table::Slot::Live { key, .. } = slot {
                match key {
                    table::TableKey::Integer(i) if *i > 0 => array += 1,
                    table::TableKey::String(..) => hash += 1,
                    _ => {}
                }
            }
        }
        array >= 40000 && hash >= 10000
    });
    let weak = heap
        .tables
        .iter()
        .any(|(_, _, t)| gc::weak_mode(heap, t.metatable) == (false, true));
    let suspended = heap
        .threads
        .iter()
        .filter(|(_, _, t)| t.status == heap::Status::LuaSuspended && t.frames.len() >= 2)
        .count();
    let userdata = heap.userdata.iter().any(|(_, _, u)| {
        u.payload.bytes().is_some_and(|b| b.len() == 4096)
            && matches!(
                u.user_values.as_ref(),
                [value::Value::Userdata(_), value::Value::String(_)]
            )
    });
    if runtime.memory().objects < 40000
        || strings <= 4 << 20
        || !large_string
        || !mixed_table
        || !weak
        || suspended < 4
        || !userdata
        || !heap.gc.generational
        || heap.collector.decide != gc::Decide::Major
        || heap.collector.phase != gc::Phase::Propagate
    {
        return Err(VmError::Corrupt);
    }
    Ok(())
}

/// A large mixed heap waiting on the host during a generational major
/// collection, built with the same fixed quantum on native and wasm32.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn large_exchange_snapshot() -> Result<Vec<u8>, VmError> {
    let chunk = compile::compile(LARGE_EXCHANGE).map_err(|_| VmError::Corrupt)?;
    let mut runtime = Runtime::boot(
        Config::default(),
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )?;
    install_userdata_proof(&mut runtime)?;
    runtime.set_global_native("park", "park")?;
    if runtime.run_until_terminal(4096, &mut Journal::new())? != StepOutcome::Waiting(WaitKey(1)) {
        return Err(VmError::Corrupt);
    }
    check_large_exchange(&runtime)?;
    let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
    if bytes.len() <= 2 << 20 {
        return Err(VmError::Corrupt);
    }
    Ok(bytes)
}

/// Restore a large exchange, answer its wait with 7, and fold its output,
/// results, and total fuel into one cross-target observation.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn large_exchange_finish(bytes: &[u8]) -> Result<i64, VmError> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut runtime =
        Runtime::from_snapshot(bytes, &HostRegistry::proof(), 1).map_err(|_| VmError::Corrupt)?;
    check_large_exchange(&runtime)?;
    let written = Rc::new(RefCell::new(Vec::new()));
    let sink = written.clone();
    runtime.set_output(Box::new(move |bytes| {
        sink.borrow_mut().extend_from_slice(bytes)
    }));
    runtime
        .complete_wait(WaitKey(1), 7)
        .map_err(|_| VmError::Corrupt)?;
    if runtime.run_until_terminal(4096, &mut Journal::new())? != StepOutcome::Completed {
        return Err(VmError::Corrupt);
    }
    let mut folded = 0i64;
    let mut fold = |bytes: &[u8]| {
        for byte in bytes {
            folded = folded.wrapping_mul(131).wrapping_add(i64::from(*byte));
        }
    };
    fold(&written.borrow());
    for value in runtime.results()? {
        fold(format!("{value:?}").as_bytes());
    }
    fold(&runtime.fuel_consumed().to_le_bytes());
    Ok(folded)
}

/// The bits of every transcendental function and of `^` over a fixed
/// corpus of 2,000 inputs, and through the runtime for 64 of them: native
/// and wasm32 compare them bit for bit, with no decimal formatting between
/// (ADR 0032). A NaN folds as one value, since its sign and payload are
/// not part of the contract.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn math_bits_fingerprint() -> Result<i64, VmError> {
    let mut inputs = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        2.0,
        10.0,
        5e-324,
        2.2250738585072014e-308,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        std::f64::consts::PI,
        709.78,
        -745.2,
        1e22,
    ];
    let mut state: u64 = 0x1234_5678_9abc_def0;
    while inputs.len() < 2_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let exponent = ((state >> 52) % 80) as i32 - 40;
        let mantissa = (state & ((1 << 52) - 1)) as f64 / (1u64 << 52) as f64 + 1.0;
        let sign = if state & 1 == 0 { 1.0 } else { -1.0 };
        inputs.push(sign * mantissa * libm::pow(2.0, f64::from(exponent)));
    }
    let mut folded = 0i64;
    let mut fold = |value: f64| {
        let bits = if value.is_nan() {
            1
        } else {
            value.to_bits() as i64
        };
        folded = folded.wrapping_mul(131).wrapping_add(bits);
    };
    let unary: [fn(f64) -> f64; 11] = [
        libm::sin,
        libm::cos,
        libm::tan,
        libm::asin,
        libm::acos,
        libm::atan,
        libm::exp,
        libm::log,
        libm::log2,
        libm::log10,
        libm::sqrt,
    ];
    for (index, &x) in inputs.iter().enumerate() {
        for function in unary {
            fold(function(x));
        }
        let y = inputs[(index * 7 + 3) % inputs.len()];
        fold(libm::atan2(x, y));
        fold(libm::pow(x, y));
        fold(libm::fmod(x, y));
    }
    const SOURCE: &[u8] = b"local x = ... return math.sin(x), math.cos(x), math.tan(x), \
        math.asin(x), math.acos(x), math.atan(x, 0.5), math.exp(x), math.log(x), \
        math.log(x, 2), math.log(x, 10), math.log(x, 3), math.sqrt(x), x ^ 1.5, \
        math.fmod(x, 0.75), math.deg(x), math.rad(x)";
    let chunk = compile::compile(SOURCE).map_err(|_| VmError::Corrupt)?;
    for &x in inputs.iter().take(64) {
        let mut registry = HostRegistry::new();
        register_standard(&mut registry);
        let mut runtime = Runtime::load_chunk_with_args(
            Config::default(),
            registry,
            &chunk,
            &[HostValue::Number(x)],
        )?;
        runtime.install_standard()?;
        if runtime.run_until_terminal(u64::MAX, &mut Journal::new())? != StepOutcome::Completed {
            return Err(VmError::Corrupt);
        }
        for value in runtime.results()? {
            match value {
                HostValue::Number(float) => fold(float),
                _ => return Err(VmError::Corrupt),
            }
        }
    }
    Ok(folded)
}

/// Runs each of `sources` straight and again with a checkpoint halfway,
/// and each of `waiting` to its end, restoring at each wait, which it
/// completes with 7; `install` sets up the libraries. Folds the results
/// and everything `print` wrote.
fn fold_sources_with_output(
    sources: &[&[u8]],
    waiting: &[&[u8]],
    install: fn(&mut Runtime) -> Result<(), VmError>,
) -> Result<i64, VmError> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let written = Rc::new(RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
    };
    let restore = |runtime: &Runtime| {
        Runtime::from_snapshot(
            &runtime.snapshot().map_err(|_| VmError::Corrupt)?,
            &HostRegistry::proof(),
            runtime.effect_domain(),
        )
        .map_err(|_| VmError::Corrupt)
    };
    let mut folded = 0i64;
    let mut fold = |bytes: &[u8]| {
        for byte in bytes {
            folded = folded.wrapping_mul(131).wrapping_add(i64::from(*byte));
        }
    };
    let text = |runtime: &Runtime| -> Result<Vec<u8>, VmError> {
        let mut out = Vec::new();
        for value in runtime.results()? {
            match value {
                HostValue::String(bytes) => out.extend_from_slice(&bytes),
                HostValue::Integer(integer) => {
                    out.extend_from_slice(integer.to_string().as_bytes())
                }
                HostValue::Boolean(bit) => out.push(u8::from(bit)),
                HostValue::Nil => out.push(b'~'),
                _ => return Err(VmError::Corrupt),
            }
            out.push(b'\t');
        }
        Ok(out)
    };
    for source in sources {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let boot = || -> Result<Runtime, VmError> {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )?;
            install(&mut runtime)?;
            Ok(runtime)
        };
        let mut runtime = boot()?;
        attach(&mut runtime);
        let mut journal = Journal::new();
        if runtime.run_until_terminal(u64::MAX, &mut journal)? != StepOutcome::Completed {
            return Err(VmError::Corrupt);
        }
        let straight = (written.take(), text(&runtime)?);
        let half = runtime.fuel_consumed() / 2;
        let mut runtime = boot()?;
        attach(&mut runtime);
        let mut journal = Journal::new();
        for _ in 0..half {
            if !matches!(runtime.run(1, &mut journal)?, StepOutcome::Paused(_)) {
                return Err(VmError::Corrupt);
            }
        }
        let mut runtime = restore(&runtime)?;
        attach(&mut runtime);
        if runtime.run_until_terminal(u64::MAX, &mut journal)? != StepOutcome::Completed {
            return Err(VmError::Corrupt);
        }
        if (written.take(), text(&runtime)?) != straight {
            return Err(VmError::Corrupt);
        }
        fold(&straight.0);
        fold(&straight.1);
    }
    for source in waiting {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )?;
        install(&mut runtime)?;
        runtime.set_global_native("park", "park")?;
        attach(&mut runtime);
        let mut journal = Journal::new();
        loop {
            match runtime.run_until_terminal(u64::MAX, &mut journal)? {
                StepOutcome::Completed => break,
                StepOutcome::Waiting(key) => {
                    runtime = restore(&runtime)?;
                    attach(&mut runtime);
                    runtime
                        .complete_wait(key, 7)
                        .map_err(|_| VmError::Corrupt)?;
                }
                _ => return Err(VmError::Corrupt),
            }
        }
        fold(&written.take());
        fold(&text(&runtime)?);
    }
    Ok(folded)
}

/// The error, `pcall` and `xpcall` fixtures, each checkpointed halfway, and
/// protected calls that wait in a native, restored while waiting.
#[doc(hidden)] // Cross-target proof fixture; outside the embedding API.
pub fn source_errors_fingerprint() -> Result<i64, VmError> {
    const SOURCES: [&[u8]; 6] = [
        include_bytes!("../fixtures/lua/pcall_basic.lua"),
        include_bytes!("../fixtures/lua/pcall_nested.lua"),
        include_bytes!("../fixtures/lua/pcall_upvalue.lua"),
        include_bytes!("../fixtures/lua/xpcall_basic.lua"),
        include_bytes!("../fixtures/lua/error_meta.lua"),
        include_bytes!("../fixtures/lua/error_overflow.lua"),
    ];
    const WAITING: [&[u8]; 3] = [
        b"local ok, v = pcall(park) return ok, v",
        b"local ok, e = pcall(function() local v = park() error(v + 1, 0) end) return ok, e",
        b"local ok, e = xpcall(function() error('x', 0) end, function(m) return park() end) return ok, e",
    ];
    fold_checkpointed_sources(&SOURCES, Vec::new(), &WAITING)
}

/// Runs each of `sources` to the end, then again with a checkpoint halfway,
/// and each of `waiting` to its wait, which it completes with 7 after a
/// restore. Folds every run's results.
fn fold_checkpointed_sources(
    sources: &[&[u8]],
    programs: Vec<program::ProtoSpec>,
    waiting: &[&[u8]],
) -> Result<i64, VmError> {
    let mut folded = 0i64;
    let mut fold_values =
        |values: Vec<value::Value>, strings: Vec<Vec<u8>>| -> Result<(), VmError> {
            for (value, text) in values.into_iter().zip(strings) {
                let part = match value {
                    value::Value::Integer(integer) => integer,
                    value::Value::Float(float) => float.to_bits() as i64,
                    value::Value::Bool(bit) => 1000 + i64::from(bit),
                    value::Value::Nil => -1,
                    value::Value::String(_) => text.iter().fold(7i64, |acc, byte| {
                        acc.wrapping_mul(31).wrapping_add(i64::from(*byte))
                    }),
                    _ => return Err(VmError::Corrupt),
                };
                folded = folded.wrapping_mul(131).wrapping_add(part);
            }
            Ok(())
        };
    let mut specs = Vec::with_capacity(sources.len() + programs.len());
    for source in sources {
        specs.push(
            compile::compile(source)
                .map_err(|_| VmError::Corrupt)?
                .proto,
        );
    }
    specs.extend(programs);
    for spec in &specs {
        let steps = {
            let mut runtime = Runtime::boot(Config::default(), HostRegistry::proof(), spec, false)?;
            bind_proof_natives(&mut runtime)?;
            match runtime.run_until_terminal(u64::MAX, &mut Journal::new())? {
                StepOutcome::Completed => runtime.fuel_consumed(),
                _ => return Err(VmError::Corrupt),
            }
        };
        let (values, strings) = run_with_checkpoint_strings(spec, bind_proof_natives, steps / 2)?;
        fold_values(values, strings)?;
    }
    for source in waiting {
        let chunk = compile::compile(source).map_err(|_| VmError::Corrupt)?;
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )?;
        bind_proof_natives(&mut runtime)?;
        runtime.set_global_native("park", "park")?;
        let mut journal = Journal::new();
        let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal)? else {
            return Err(VmError::Corrupt);
        };
        let domain = runtime.effect_domain();
        let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
        let mut runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
            .map_err(|_| VmError::Corrupt)?;
        runtime
            .complete_wait(key, 7)
            .map_err(|_| VmError::Corrupt)?;
        if runtime.run_until_terminal(u64::MAX, &mut journal)? != StepOutcome::Completed {
            return Err(VmError::Corrupt);
        }
        let values = runtime.entry_results()?;
        let strings = vec![Vec::new(); values.len()];
        fold_values(values, strings)?;
    }
    Ok(folded)
}

/// [`run_with_checkpoint`] at step `at`, also returning each result's
/// string bytes (empty for non-strings) from the restored runtime.
fn run_with_checkpoint_strings(
    spec: &program::ProtoSpec,
    bind: fn(&mut Runtime) -> Result<(), VmError>,
    at: u64,
) -> Result<(Vec<value::Value>, Vec<Vec<u8>>), VmError> {
    let mut runtime = Runtime::boot(Config::default(), HostRegistry::proof(), spec, false)?;
    bind(&mut runtime)?;
    let mut journal = Journal::new();
    for _ in 0..at {
        match runtime.run(1, &mut journal)? {
            StepOutcome::Paused(_) => {}
            _ => return Err(VmError::Corrupt),
        }
    }
    let domain = runtime.effect_domain();
    let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
    let mut runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
        .map_err(|_| VmError::Corrupt)?;
    match runtime.run_until_terminal(u64::MAX, &mut journal)? {
        StepOutcome::Completed => {}
        _ => return Err(VmError::Corrupt),
    }
    let values = runtime.entry_results()?;
    let strings = values
        .iter()
        .map(|value| match value {
            value::Value::String(handle) => runtime
                .heap()
                .string_bytes(*handle)
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            _ => Vec::new(),
        })
        .collect();
    Ok((values, strings))
}

fn entry_frame_pc(runtime: &Runtime) -> Option<u32> {
    let heap = runtime.heap();
    let thread = heap.threads.get(heap.entry?)?;
    match &thread.frames[..] {
        [frame] => Some(frame.pc),
        _ => None,
    }
}

/// Run one step at a time, snapshot and restore at the first step where
/// `at` holds, and finish in the restored runtime.
fn run_with_checkpoint(
    spec: &program::ProtoSpec,
    bind: fn(&mut Runtime) -> Result<(), VmError>,
    at: impl Fn(&Runtime, u64) -> bool,
) -> Result<Vec<value::Value>, VmError> {
    let mut runtime = Runtime::boot(Config::default(), HostRegistry::proof(), spec, false)?;
    bind(&mut runtime)?;
    let mut journal = Journal::new();
    let mut step = 0u64;
    while !at(&runtime, step) {
        match runtime.run(1, &mut journal)? {
            StepOutcome::Paused(_) => step += 1,
            _ => return Err(VmError::Corrupt),
        }
    }
    let domain = runtime.effect_domain();
    let bytes = runtime.snapshot().map_err(|_| VmError::Corrupt)?;
    let mut runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
        .map_err(|_| VmError::Corrupt)?;
    match runtime.run_until_terminal(u64::MAX, &mut journal)? {
        StepOutcome::Completed => {}
        _ => return Err(VmError::Corrupt),
    }
    runtime.entry_results()
}

fn pack_results(
    values: &[value::Value],
    count: usize,
    range: std::ops::RangeInclusive<i64>,
) -> Result<i64, VmError> {
    if values.len() != count {
        return Err(VmError::Corrupt);
    }
    let mut packed = 0i64;
    for (shift, value) in values.iter().enumerate() {
        let value::Value::Integer(integer) = value else {
            return Err(VmError::Corrupt);
        };
        if !range.contains(integer) {
            return Err(VmError::Corrupt);
        }
        packed |= integer << (shift * 8);
    }
    Ok(packed)
}

/// Portable VFS and mock-capability fingerprint for native/Wasm verification.
#[doc(hidden)]
pub fn host_capabilities_fingerprint() -> std::result::Result<i64, VmError> {
    hostproof::fingerprint()
}

// Test the package documents directly so the displayed examples cannot drift.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_examples {}

#[cfg(doctest)]
#[doc = include_str!("../EMBEDDING.md")]
mod embedding_examples {}
