//! Explicit frames, fuel, and host continuations.
//!
//! The interpreter loop does not recurse: Lua calls push [`Frame`]s.
//! Dispatch has two tiers over one set of semantics. [`Runtime::run_hot`]
//! executes the cheap register and branch opcodes from locals; every other
//! opcode, and every state the hot tier does not handle, goes through
//! `poll` and the out-of-line `exec`.
//! `CallHost` stops in `Prepared` before the host runs, so a checkpoint can
//! see the effect id before anything is committed. Fuel for that instruction
//! is charged on entry and not again when the host is invoked.

mod builtins;
mod capability;
mod coroutine;
mod debug;
mod diag;
#[path = "api/runtime.rs"]
mod embedding;
#[path = "api/execution.rs"]
mod execution;
mod finalize;
pub(crate) mod hooks;
pub(crate) mod hostload;

/// What closing may allocate past the object limit (ADR 0048).
pub(crate) fn finalize_close_objects() -> u32 {
    finalize::CLOSE_OBJECTS
}
pub(crate) mod io;
pub(crate) mod library;
mod os;
mod package;
mod resolver;
mod string;
mod userdata;
mod utf8;
pub(crate) use string::gmatch_fits;

use crate::compare;
use crate::gc;
use crate::heap::{
    Arena, AssignTarget, Boundary, CloseNext, Closing, ClosureObj, Frame, FrameCold, GC_MIN_DEBT,
    GcState, Heap, MAX_LOGICAL_HEAP, MAX_OBJECTS, MetaCall, MetaEvent, MetaPhase, OBJECTS_CEILING,
    Pending, Proto, Status, StringObj, TableObj, ThreadObj, TraceRef, Unwind, UnwindPhase,
    UpvalueObj, UpvalueState, cost,
};
use crate::host::{
    EffectId, HostCtx, HostRegistry, HostResult, HostValue, Journal, LegacyCompletion, NativeCall,
    NativeOutcome, NativePolicy,
};
use crate::id::{
    Handle, Kind, LuaFault, ObjectId, OwnerToken, PauseReason, Root, RootError, StepOutcome,
    TerminationReason, VmError, WaitError, WaitKey,
};
use crate::index::Resolved;
use crate::opcode::{ArithOp, COUNT_OPEN, Capture, CmpKind, Op};
use crate::ops::{self, Joined, Step as OpStep, Truth};
use crate::program::ProtoSpec;
use crate::table::KeyView;
use crate::value::Value;
use crate::{fornum, index};

/// The most `__call` steps one call follows (ADR 0023). Each step inserts
/// one argument. PUC Lua bounds this only by its stack size, about a
/// million slots, and a `__call` cycle takes quadratic time to get there.
pub(crate) const MAX_CALL_CHAIN: u32 = 200;

/// The most Lua and protected calls one thread may hold. A Lua call past it
/// faults with `LuaFault::StackOverflow` instead of
/// growing the host's memory without end. PUC Lua's bound is its stack of
/// about a million slots, some 200,000 Lua calls deep.
pub(crate) const MAX_CALL_DEPTH: usize = 1_000;

/// How many times an `xpcall` message handler is called for one error when
/// the handler itself keeps failing, before the error becomes "error in
/// error handling". PUC Lua's bound is its C stack, about 200 calls.
pub(crate) const MAX_HANDLER_DEPTH: u32 = 20;

/// The most frames a thread holds at all: Lua calls and protected calls up
/// to [`MAX_CALL_DEPTH`], plus room for message handlers to run on top of a
/// stack that overflowed, or for the `__close` calls of an unwind from such
/// a stack. It is the bound a snapshot accepts.
pub(crate) const MAX_FRAMES: usize = MAX_CALL_DEPTH + 4 * MAX_HANDLER_DEPTH as usize;

/// Default bound on one thread's stack, in value slots: registers, call
/// windows, extra arguments, and open results (ADR 0028). The last eighth
/// is kept for message handlers and the closes of an unwind, like the
/// frames past [`MAX_CALL_DEPTH`].
pub(crate) const MAX_STACK_SLOTS: u32 = 50_000;

/// The range a host may configure for the stack bound. The upper end is
/// what a snapshot accepts, so a runtime never holds a stack it could not
/// checkpoint.
pub(crate) const STACK_SLOTS_RANGE: std::ops::RangeInclusive<u32> = 1_024..=100_000;

/// Runtime configuration, resource limits, and deterministic execution policy.
#[derive(Clone, Debug)]
pub struct Config {
    /// The host-selected lineage shared with the journal.
    pub effect_domain: u64,
    /// An optional total execution fuel bound; restore rewinds the consumed counter.
    pub fuel_limit: Option<u64>,
    /// Bound on live objects, in all kinds together (ADR 0052): an
    /// allocation past it raises a Lua memory error. Clamped to
    /// 1..=2^28.
    pub max_objects: u32,
    /// Collect garbage automatically at safe points (ADR 0021).
    pub auto_gc: bool,
    /// Smallest allocation, in logical bytes, between automatic collections.
    pub gc_min_debt: u64,
    /// Hard quota on the logical heap, in logical bytes (ADR 0025). An
    /// allocation that would pass it raises a Lua memory error. Clamped
    /// to 1..=2^31 (ADR 0052).
    pub max_logical_heap: u64,
    /// Bound on each thread's stack, in value slots (ADR 0028). A call or
    /// result that would pass it raises a Lua "stack overflow". Clamped to
    /// 1,024..=100,000.
    pub max_stack_slots: u32,
    /// Bound on one string's length, in bytes (ADR 0052). A string is also
    /// bounded by the quota; by default only by the quota. Clamped to
    /// 1,024..=2^30.
    pub max_string_bytes: u64,
    /// Bound on a snapshot this runtime writes, in bytes (ADR 0052): a
    /// larger one is refused with `SnapshotError::LimitExceeded`. Host
    /// policy, not snapshot state. Clamped to 4,096..=2^31.
    pub max_snapshot_bytes: u64,
    /// Start of the deterministic entropy stream that seeds `math.random`
    /// (ADR 0032): the same value gives the same random numbers.
    pub entropy: u64,
    /// The collector's mode at boot (ADR 0051); Lua's
    /// `collectgarbage("incremental")` and `("generational")` change it.
    pub gc_mode: GcMode,
}

/// How the collector works (ADR 0050, ADR 0051).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GcMode {
    /// Whole incremental cycles, in steps.
    Incremental,
    /// Young collections, and a major collection (an incremental cycle)
    /// once memory has grown enough since the last: Lua 5.4's default.
    #[default]
    Generational,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            effect_domain: 1,
            fuel_limit: None,
            max_objects: MAX_OBJECTS,
            auto_gc: true,
            gc_min_debt: GC_MIN_DEBT,
            max_logical_heap: MAX_LOGICAL_HEAP,
            max_stack_slots: MAX_STACK_SLOTS,
            max_string_bytes: crate::heap::MAX_STRING_BYTES as u64,
            max_snapshot_bytes: crate::snapshot::MAX_SNAPSHOT_BYTES,
            entropy: 0,
            gc_mode: GcMode::default(),
        }
    }
}

impl Config {
    /// The resource limits this configuration asks for, clamped as a
    /// runtime clamps them.
    pub fn limits(&self) -> Limits {
        Limits {
            max_logical_heap: self.max_logical_heap,
            max_objects: self.max_objects,
            max_stack_slots: self.max_stack_slots,
            max_string_bytes: self.max_string_bytes,
            max_snapshot_bytes: self.max_snapshot_bytes,
        }
        .clamped()
    }
}

/// The largest logical-heap quota a runtime may have: a full heap's
/// snapshot (under a byte per logical byte, measured) stays within the
/// snapshot ceiling, and a table's slots within their `u32` index.
pub(crate) const LOGICAL_HEAP_CEILING: u64 = 1 << 31;

/// Every resource dimension a runtime is bounded in (ADR 0052): what it
/// runs under, and what a checkpoint is restored into
/// ([`Runtime::from_snapshot_with_limits`]). The defaults are
/// [`Config::default`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The logical-heap quota, in logical bytes.
    pub max_logical_heap: u64,
    /// Live objects, all kinds together.
    pub max_objects: u32,
    /// Each thread's stack, in value slots.
    pub max_stack_slots: u32,
    /// One string's length, in bytes.
    pub max_string_bytes: u64,
    /// One snapshot, in bytes, written or read.
    pub max_snapshot_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Config::default().limits()
    }
}

impl Limits {
    /// Each limit within the range a runtime accepts.
    pub fn clamped(self) -> Self {
        let strings = crate::heap::STRING_BYTES_RANGE;
        let snapshots = crate::snapshot::SNAPSHOT_BYTES_RANGE;
        Self {
            max_logical_heap: self.max_logical_heap.clamp(1, LOGICAL_HEAP_CEILING),
            max_objects: self.max_objects.clamp(1, OBJECTS_CEILING),
            max_stack_slots: self
                .max_stack_slots
                .clamp(*STACK_SLOTS_RANGE.start(), *STACK_SLOTS_RANGE.end()),
            max_string_bytes: self
                .max_string_bytes
                .clamp(*strings.start() as u64, *strings.end() as u64),
            max_snapshot_bytes: self
                .max_snapshot_bytes
                .clamp(*snapshots.start(), *snapshots.end()),
        }
    }
}

/// What the heap holds and when the next automatic collection runs.
/// Sizes are logical bytes (ADR 0021), not bytes of host memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryUsage {
    /// Live objects, including garbage not yet collected.
    pub objects: u32,
    /// The hard object limit.
    pub max_objects: u32,
    /// The logical heap, exactly: every object's logical size, garbage not
    /// yet freed included. The quota is on it.
    pub logical_bytes: u64,
    /// Logical bytes allocated since the last collection.
    pub debt: u64,
    /// Debt at which the next automatic collection runs.
    pub threshold: u64,
    /// Collections so far, automatic or requested: young collections and
    /// full ones.
    pub collections: u64,
    /// Young collections so far (generational mode).
    pub young_collections: u64,
    /// Whether automatic collection is enabled.
    pub auto_gc: bool,
}

pub(crate) struct RestoredParts {
    pub(crate) registry: HostRegistry,
    pub(crate) heap: Heap,
    pub(crate) effect_domain: u64,
    pub(crate) next_sequence: u64,
    pub(crate) fuel_consumed: u64,
    pub(crate) fuel_limit: Option<u64>,
    pub(crate) max_objects: u32,
    pub(crate) max_stack_slots: u32,
    pub(crate) max_snapshot: u64,
    pub(crate) last_completed_wait: Option<u64>,
    pub(crate) completed_waits: std::collections::HashSet<u64>,
    pub(crate) host_call: bool,
    pub(crate) callback_failed: bool,
    pub(crate) trap: Option<TerminationReason>,
}

/// Diagnostic dispatch selection; absent from ordinary release builds.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HotCoreMode {
    #[default]
    Full,
    NoFastCalls,
    Off,
}

#[cfg(test)]
std::thread_local! {
    static PROOF_MODE: std::cell::Cell<HotCoreMode> = const { std::cell::Cell::new(HotCoreMode::Full) };
}

#[cfg(any(test, debug_assertions))]
impl HotCoreMode {
    fn initial() -> Self {
        #[cfg(test)]
        {
            PROOF_MODE.get()
        }
        #[cfg(not(test))]
        {
            Self::Full
        }
    }

    #[cfg(test)]
    pub(crate) fn with<T>(self, f: impl FnOnce() -> T) -> T {
        struct Reset(HotCoreMode);
        impl Drop for Reset {
            fn drop(&mut self) {
                PROOF_MODE.set(self.0);
            }
        }
        let _reset = Reset(PROOF_MODE.replace(self));
        f()
    }
}

/// A single-threaded Lua runtime with rooted values and portable checkpoints.
/// It is neither `Send` nor `Sync`; drive and complete waits on its owning thread.
/// It owns Lua state, not the host's [`HostRegistry`] callbacks, shared capability
/// backends, or [`Journal`] persistence. Owned [`crate::Value`] roots keep objects
/// alive; borrowed views cannot cross a mutable execution call.
///
/// Construct through [`Runtime::builder`], compile source with [`crate::compile`],
/// and handle every [`StepOutcome`] from [`Runtime::run`]. Host misuse returns
/// [`crate::ApiError`]; uncaught Lua failures are control outcomes, while VM
/// integrity failures return [`VmError`]. For native-to-Lua reentry use
/// [`crate::NativeContext::call_lua`]. A host callback panic poisons execution and
/// snapshots; discard the runtime after catching it.
///
/// [`Runtime::snapshot`] captures active VM state, including coroutines and
/// pending calls. [`Runtime::restore`] requires an explicit [`crate::Host`],
/// returns a fresh runtime identity, and never restores host callback code or
/// external resources. Drive [`Runtime::begin_close`] when Lua cleanup is needed:
/// dropping a runtime executes no Lua.
///
/// ```
/// use moonseed::{compile, FromLuaMulti, Journal, Runtime, StepOutcome};
/// let mut rt = Runtime::builder().build()?;
/// rt.load_main(&compile(b"return 42").unwrap())?;
/// assert_eq!(rt.run(100, &mut Journal::new())?, StepOutcome::Completed);
/// let values = rt.result_values()?;
/// assert_eq!(i64::from_lua_multi(values, &mut rt)?, 42);
/// # Ok::<(), moonseed::Error>(())
/// ```
pub struct Runtime {
    #[cfg(any(test, debug_assertions))]
    pub(crate) hot_core: HotCoreMode,
    #[cfg(feature = "counters")]
    pub(crate) counters: crate::counters::Sink,
    owner: OwnerToken,
    heap: Heap,
    registry: HostRegistry,
    /// Registry slot of each `heap.natives` symbol. A cache: rebuilt on
    /// restore, never written to a snapshot.
    native_slots: Vec<usize>,
    /// Argument and result buffers reused across native calls. Empty
    /// between calls; not snapshot state.
    native_args: Vec<Value>,
    pub(crate) native_results: Vec<Value>,
    pub(crate) native_owned_buffers: [Vec<crate::api::Value>; 4],
    /// Cleared exceptional-frame storage; holds no Lua values or handles.
    /// Live continuations belong to frames; this spare is never serialized.
    cold_spare: Option<Box<FrameCold>>,
    /// A completed library task, cleared before leaving the traced frames.
    lib_task_spare: Option<Box<crate::library::LibTask>>,
    /// Weak cache: validated before use, rebuilt from the heap after restore.
    main_call_closure: Option<Handle<ClosureObj>>,
    in_callback: bool,
    callback_failed: bool,
    /// The arguments of a string function's call of Lua, from the
    /// operation that asks for it to the call (ADR 0034), so the
    /// operation stays plain data. Empty between steps; not snapshot state.
    lib_call_args: Vec<Value>,
    /// Resources and capabilities supplied by the host, never snapshot state.
    host_capabilities: crate::HostCapabilities,
    /// Where `print` writes. Host state, never written to a snapshot.
    output: Option<crate::host::Output>,
    /// Where warnings go. Host state, never written to a snapshot.
    warnings: Option<crate::host::Warnings>,
    /// Where `math.randomseed()` gets entropy. Host state, never written
    /// to a snapshot.
    entropy: Option<crate::host::Entropy>,
    effect_domain: u64,
    next_sequence: u64,
    fuel_consumed: u64,
    fuel_limit: Option<u64>,
    max_objects: u32,
    max_stack_slots: u32,
    /// Bound on the snapshots this runtime writes: host policy, never
    /// written to a snapshot.
    max_snapshot: u64,
    trap: Option<TerminationReason>,
    hook_trap: bool,
    last_completed_wait: Option<u64>,
    completed_waits: std::collections::HashSet<u64>,
    wait_index: std::collections::HashMap<u64, Handle<ThreadObj>>,
    /// Roots and VM-allocation provenance; legacy keys may use any bits.
    wait_roots: std::collections::HashMap<u64, (crate::api::Value, bool)>,
    host_call: bool,
    pins: Vec<TraceRef>,
    /// Capacity growth of the active stack and frame vectors. Not snapshot
    /// state: a restore starts these at zero. Used to see whether a call
    /// allocated.
    pub(crate) stack_grows: u64,
    pub(crate) frame_grows: u64,
    /// Instructions run by `exec` rather than the hot tier. Measurement
    /// only; not snapshot state.
    pub(crate) cold_steps: u64,
    /// Fuel consumed at each collection, for schedule tests. Not snapshot
    /// state: a restored runtime starts an empty log.
    #[cfg(test)]
    pub(crate) gc_log: Vec<u64>,
    /// Each collector slice `run` did: units, nanoseconds, and whether
    /// it began in the sweep, touched the atomic phase, and did a young
    /// collection's work (measurements).
    #[cfg(feature = "__measure")]
    pub(crate) gc_slices: Vec<(u64, u64, bool, bool, bool)>,
    /// Test hook: unwinding pops frames without closing their upvalues, to
    /// show the tests would notice.
    #[cfg(test)]
    pub(crate) skip_unwind_close: bool,
    /// Test hook: a tail call leaves the finished frame's upvalues open, to
    /// show the tests would notice.
    #[cfg(test)]
    pub(crate) skip_tail_close: bool,
}

enum Poll {
    Continue,
    Stop(StepOutcome),
}

/// How the hot loop in [`Runtime::run_hot`] stopped. Locals are written
/// back to the frame before any of these is acted on.
enum SliceEnd<'a> {
    /// Quantum or fuel limit reached; `poll` reports it.
    Budget(bool),
    /// A charged instruction that is not hot, for `exec`.
    Cold(&'a Op),
    /// A charged hot instruction faulted. `pc` is on it.
    Fault(LuaFault),
    /// The frame's code or jump target is invalid.
    Broken(VmError),
}

/// The hot instruction publishes its next pc itself. Rust uses the unused
/// LuaFault discriminants for the other variants, keeping dispatch one byte.
enum Step {
    Continue,
    Cold,
    Frame,
    Fault(LuaFault),
}
const _: () = assert!(std::mem::size_of::<Step>() == 1);

enum FrameStep {
    Continue,
    Cold,
    Resync,
    /// A return into a caller holding a plain metamethod continuation,
    /// which `run_hot` commits when the epoch still has an allowance.
    Meta,
    /// The new top frame is not described by the helper's `Switch` (a
    /// library or handler boundary); `run_hot` validates it afresh.
    Revalidate,
}
const _: () = assert!(std::mem::size_of::<FrameStep>() == 1);

/// The frame a call, tail call or return made the running one, filled by
/// the helper so `run_hot` re-derives its window without leaving the
/// instruction loop and without a second arena lookup. Stack memory for one
/// epoch; never stored across epochs.
struct Switch<'h> {
    closure: &'h ClosureObj,
    proto: &'h Proto,
    base: u32,
    pc: u32,
}

struct HotCore<'a, 'h> {
    regs: &'a mut [Value],
    pc: u32,
    heap: &'a mut HotHeap<'h>,
    upvalues: Option<HotUpvalues<'a>>,
}

/// A boundary frame on top whose call is done: which kind finishes.
#[derive(Clone, Copy)]
enum Ready {
    Hook,
    HookNative,
    Protect,
    Handler,
    Builtin,
    Native,
    Finalizer,
}

/// Where [`Runtime::enter_lua`] puts a Lua function's frame.
#[derive(Clone, Copy)]
enum Entry {
    /// An ordinary call: a new frame on top. `advance_caller` moves the
    /// caller past its `Call`; a metamethod call or a boundary leaves the
    /// caller where it is.
    Push { advance_caller: bool },
    /// A tail call (ADR 0029): the new frame replaces the running one. The
    /// callee and its arguments, at `from - 1` and `from`, move down to the
    /// running frame's own call slot first.
    Replace { from: u32 },
}

/// The close state of a frame that is running its closes.
fn closing(frame: &Frame) -> Option<&Closing> {
    frame.meta()?.close.as_deref()
}

/// A frame whose work may use the frames past [`MAX_CALL_DEPTH`] and the
/// last eighth of the stack bound: a message handler's boundary, or a frame
/// an unwind is closing after a stack overflow. Lua 5.4 likewise grows its
/// stack past the limit only to handle an overflow; the closes of another
/// error's unwind have the ordinary room, and overflow there as anywhere.
fn in_reserve(frame: &Frame) -> bool {
    matches!(frame.boundary(), Some(Boundary::Handler { .. }))
        || closing(frame).is_some_and(|closing| {
            matches!(
                closing.next,
                CloseNext::Unwind(Unwind {
                    error: Some((LuaFault::StackOverflow | LuaFault::ErrorHandling, _)),
                    ..
                })
            )
        })
}

/// A frame closing its values on a memory error's unwind.
fn closes_memory_error(frame: &Frame) -> bool {
    closing(frame).is_some_and(|closing| {
        matches!(
            closing.next,
            CloseNext::Unwind(Unwind {
                error: Some((LuaFault::Memory, _)),
                ..
            })
        )
    })
}

/// The frame a `CloseThread` unwind has reached: its closes wait on an
/// unwind with no target. Nothing below it may catch an error.
fn closes_thread(frame: &Frame) -> bool {
    closing(frame).is_some_and(|closing| {
        matches!(
            closing.next,
            CloseNext::Unwind(Unwind {
                phase: UnwindPhase::Popping { target: None },
                ..
            })
        )
    })
}

/// A frame about to run its closes: `Close` between calls.
fn idle_close(from: u32, next: CloseNext) -> Option<MetaCall> {
    Some(MetaCall {
        event: MetaEvent::Close,
        slot: 0,
        nargs: 0,
        phase: MetaPhase::Idle,
        close: Some(Box::new(Closing { from, next })),
    })
}

/// Whether an error class goes through an `xpcall` message handler. Lua
/// calls no handler for a memory error, nor for "error in error handling".
fn handles(fault: LuaFault) -> bool {
    !matches!(fault, LuaFault::Memory | LuaFault::ErrorHandling)
}

/// `error`'s optional level: an integer, a float with an integer value, or a
/// string that reads as one.
fn error_level(heap: &Heap, level: Value) -> Option<i64> {
    crate::base::lua_integer(heap, level)
}

/// A refused table insert as a VM result: the quota is a memory error the
/// step boundary turns into a Lua error; a missing table is corruption.
fn insert_error(error: crate::heap::InsertError) -> VmError {
    match error {
        crate::heap::InsertError::Memory => VmError::MemoryLimit,
        crate::heap::InsertError::NoTable => VmError::Corrupt,
    }
}

/// Grow a thread's stack to `len` slots, charging the slots it has not
/// been charged for to the logical heap (ADR 0041, ADR 0051). The quota
/// itself is checked before a call or a result window grows the stack
/// (`slot_fault`).
/// Returns whether the physical storage grew.
fn grow_stack(thread: &mut ThreadObj, len: usize, gc: &mut crate::heap::GcState) -> bool {
    if thread.stack.len() < len {
        thread.charge_slots(len, gc);
        count!("stack_growths");
        return thread.stack.grow_to(len);
    }
    false
}

struct FrameHeap<'a> {
    closures: &'a Arena<ClosureObj>,
    protos: &'a Arena<crate::heap::Proto>,
    gc: &'a mut crate::heap::GcState,
    /// The ordinary part of the stack bound (`max_stack_slots` less its
    /// last eighth), computed once per epoch.
    ordinary: u32,
    stack_grows: &'a mut u64,
    frame_grows: &'a mut u64,
    cold_spare: &'a mut Option<Box<FrameCold>>,
}

/// The builder's rare growth: charge slots past the thread's charge and
/// lengthen the physical storage past its high-water. Returns whether the
/// charge grew.
#[cold]
#[inline(never)]
fn grow_charged(thread: &mut ThreadObj, heap: &mut FrameHeap<'_>, end: u32) -> bool {
    let charged = crate::heap::charge_slots_to(&mut thread.charged_slots, end as usize, heap.gc);
    if end as usize > thread.stack.values.len() {
        crate::heap::grow_values(&mut thread.stack.values, end as usize);
        *heap.stack_grows = heap.stack_grows.saturating_add(1);
    }
    charged
}

/// A fixed-arity Lua window [`check_fixed_frame`] admitted. Only the checker
/// makes one, so [`write_fixed_frame`] never re-tests depth, bound or quota.
struct Admit {
    limit: u32,
    end: u32,
}

/// The checks of the one builder for a fixed-arity Lua entry whose function
/// sits at `base - 1` and whose arguments sit at `base..base + passed` (the
/// call window is the callee's register window), against a thread at
/// `depth` frames whose stack will be `len` slots long when the frame is
/// written. `charged_first` is a charge the caller makes before the write
/// (a metamethod's or library callback's scratch window): when present the
/// quota test always runs, on that charge plus the window's growth, which is
/// the slow path's second `slot_fault`. `None` means the caller's slow path
/// produces the fault or the reserve-frame handling.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn check_fixed_frame(
    depth: usize,
    len: usize,
    heap: &FrameHeap<'_>,
    proto: &Proto,
    base: u32,
    passed: u32,
    charged_first: Option<u64>,
) -> Option<Admit> {
    // `depth == 0 || depth >= MAX_CALL_DEPTH` in one unsigned compare.
    if depth.wrapping_sub(1) >= MAX_CALL_DEPTH - 1 {
        return None;
    }
    let limit = base + u32::from(proto.max_reg);
    if limit > heap.ordinary {
        return None;
    }
    let end = limit.max(base + passed);
    let growth = (end as usize).saturating_sub(len) as u64;
    match charged_first {
        None if growth == 0 => {}
        charge => {
            if !heap
                .gc
                .fits(charge.unwrap_or(0) + cost::STACK_SLOT * growth)
            {
                return None;
            }
        }
    }
    Some(Admit { limit, end })
}

/// The builder's writes, after [`check_fixed_frame`]: preconditions proved
/// by the caller are that `closure` resolves to `proto`, `!proto.vararg`,
/// and `passed` is a count. Retained storage (RFC2 §1): raising the logical
/// length is a store unless the charge or the physical high-water is passed.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn write_fixed_frame(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'_>,
    admit: Admit,
    closure: Handle<ClosureObj>,
    params: u8,
    base: u32,
    passed: u32,
    nresults: u8,
    advance_caller: bool,
) -> FrameStep {
    let Admit { limit, end: end32 } = admit;
    let end = end32 as usize;
    let old_len = thread.stack.len;
    let mut charged = false;
    if end > old_len {
        count!("stack_growths");
        if end32 > thread.charged_slots || end > thread.stack.values.len() {
            charged = grow_charged(thread, heap, end32);
        }
        thread.stack.len = end;
    }
    let values = &mut thread.stack.values;
    // One range: missing parameters, non-parameter registers, discarded
    // extra arguments (`params <= max_reg`, `check.rs`) and every slot the
    // logical length just took in, stale or new. Arguments end at or below
    // the old length, so nothing a caller passed is in the second part.
    let params = u32::from(params);
    let clear_from = ((base + params.min(passed)) as usize).min(old_len);
    if clear_from < end {
        values[clear_from..end].fill(Value::Nil);
    }
    thread.top = base + params;
    if advance_caller {
        thread.frames.slots[thread.frames.depth - 1].pc += 1;
    }
    count!("lua_calls");
    count!("frame_pushes");
    if thread.frames.push_hot(closure, base, limit, nresults) {
        *heap.frame_grows = heap.frame_grows.saturating_add(1);
    }
    // Hot code cannot charge and `gc_schedule` ran before the epoch, so the
    // collector can become due inside it only through this charge.
    if charged {
        FrameStep::Resync
    } else {
        FrameStep::Continue
    }
}

/// The one builder for a fixed-arity Lua entry: [`check_fixed_frame`] on the
/// current length, then [`write_fixed_frame`]. Every check precedes the
/// first write.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn build_fixed_frame(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'_>,
    closure: Handle<ClosureObj>,
    proto: &Proto,
    base: u32,
    passed: u32,
    nresults: u8,
    advance_caller: bool,
) -> Option<FrameStep> {
    let admit = check_fixed_frame(
        thread.frames.depth,
        thread.stack.len,
        heap,
        proto,
        base,
        passed,
        None,
    )?;
    Some(write_fixed_frame(
        thread,
        heap,
        admit,
        closure,
        proto.params,
        base,
        passed,
        nresults,
        advance_caller,
    ))
}

/// Lua calls use the same windows and quota checks as `enter_lua`.
/// Every decline precedes any write but the caller's `pc` (the call's own,
/// as the slow path expects). On `Continue` `switch` holds the callee.
#[inline(never)]
fn fast_call<'h>(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'h>,
    pc: u32,
    func: u8,
    nargs: u8,
    nresults: u8,
    switch: &mut Switch<'h>,
) -> FrameStep {
    let Some(caller) = thread.frames.last_mut() else {
        return FrameStep::Cold;
    };
    caller.pc = pc;
    let func = caller.base + u32::from(func);
    // `func < caller.limit <= stack.len()`: a register (`check_code`) of a
    // hot frame, whose window the epoch checked against the logical length.
    debug_assert!((func as usize) < thread.stack.len());
    let Value::Closure(closure) = thread.stack.values[func as usize] else {
        return FrameStep::Cold;
    };
    let Some(callee) = heap.closures.get(closure) else {
        return FrameStep::Cold;
    };
    let Some(proto) = heap.protos.get(callee.proto) else {
        return FrameStep::Cold;
    };
    if proto.vararg {
        return FrameStep::Cold;
    }
    let base = func + 1;
    let passed = if nargs == COUNT_OPEN {
        thread.top.saturating_sub(base)
    } else {
        u32::from(nargs)
    };
    let Some(step) = build_fixed_frame(thread, heap, closure, proto, base, passed, nresults, true)
    else {
        return FrameStep::Cold;
    };
    *switch = Switch {
        closure: callee,
        proto,
        base,
        pc: 0,
    };
    step
}

/// A return that needs neither closes nor a boundary continuation. Result
/// writes, dead-slot clearing and vector length match `finish_result_window`.
/// Every decline precedes any write but the returning frame's `pc`. On
/// `Continue` `switch` holds the caller, resolved before any write.
#[inline(never)]
fn fast_return<'h>(
    thread: &mut ThreadObj,
    heap: &FrameHeap<'h>,
    pc: u32,
    base: u8,
    count: u8,
    switch: &mut Switch<'h>,
) -> FrameStep {
    // The slow path runs a declined return from the frame's own pc.
    let Some(frame) = thread.frames.last_mut() else {
        return FrameStep::Cold;
    };
    frame.pc = pc;
    let Some(below) = thread.frames.depth.checked_sub(2) else {
        return FrameStep::Cold;
    };
    let frame = &thread.frames.slots[below + 1];
    let caller = &thread.frames.slots[below];
    // One null test for an ordinary caller; a plain metamethod caller
    // commits after the return; boundaries and continuations go elsewhere.
    let meta = match &caller.cold {
        None => false,
        Some(cold) if cold.boundary.is_none() && cold.pending.is_none() => cold.meta.is_some(),
        Some(cold) => {
            return if matches!(
                cold.boundary,
                Some(Boundary::Builtin { .. } | Boundary::Handler { .. })
            ) {
                match builtins::fast_builtin_return(thread, base, count) {
                    FrameStep::Continue => FrameStep::Revalidate,
                    step => step,
                }
            } else {
                FrameStep::Cold
            };
        }
    };
    if thread.open_above > frame.base || thread.tbc.last().is_some_and(|slot| *slot >= frame.base) {
        return FrameStep::Cold;
    }
    let dest = frame.base.saturating_sub(frame.vararg_len + 1) as usize;
    let scratch_end = frame.limit as usize;
    let caller_limit = caller.limit as usize;
    let mode = frame.nresults;
    // The caller's window must lie within the length the return leaves
    // (`max(caller.limit, dest + want)`; the result window is checked
    // below): a restored short caller takes the slow path.
    if caller_limit > thread.stack.len {
        return FrameStep::Cold;
    }
    if !meta {
        let Some(closure) = heap.closures.get(caller.closure) else {
            return FrameStep::Cold;
        };
        let Some(proto) = heap.protos.get(closure.proto) else {
            return FrameStep::Cold;
        };
        *switch = Switch {
            closure,
            proto,
            base: caller.base,
            pc: caller.pc,
        };
    }
    // One logical view: the checks below bound every access by it.
    let values: &mut [Value] = &mut thread.stack;
    // Discarded returns need no source arithmetic, reads or padding.
    let want = if mode == 0 {
        if dest > values.len() {
            return FrameStep::Cold;
        }
        0
    } else {
        let src = frame.base as usize + usize::from(base);
        let produced = if count == COUNT_OPEN {
            (thread.top as usize).saturating_sub(src)
        } else {
            usize::from(count)
        };
        let want = if mode == COUNT_OPEN {
            produced
        } else {
            usize::from(mode)
        };
        // A restored short caller can need charged result growth. Preserve
        // the slow path's slot checks and collector scheduling in that case.
        if dest + want > values.len() {
            return FrameStep::Cold;
        }
        let copied = want.min(produced);
        // A restored `top` past the stack's end makes the slow path read
        // nils; the fast path declines rather than index out of range.
        if src + copied > values.len() {
            return FrameStep::Cold;
        }
        // `dest < src` for a Lua caller, so ascending scalar copies are
        // overlap-safe; the general case stays out of line. Both windows
        // end at or below the logical length (checked above).
        match copied {
            0 => {}
            1 => values[dest] = values[src],
            2 => {
                values[dest] = values[src];
                values[dest + 1] = values[src + 1];
            }
            _ => values.copy_within(src..src + copied, dest),
        }
        if copied < want {
            values[dest + copied..dest + want].fill(Value::Nil);
        }
        want
    };
    // A depth decrement and one null test; the slot stays for the next call.
    thread.frames.pop_hot();
    let clear_from = dest + if want == 0 { 1 } else { want };
    let new_top = dest + want;
    let keep = caller_limit.max(new_top);
    // Slots past `keep` stay as retained storage: never traced, snapshotted,
    // inspected or read before `grow_to` or a builder nils them. Retained
    // logical slots still match the slow window. `scratch_end` is the
    // callee's limit, within the logical length since its window began.
    let clear_end = scratch_end.min(keep);
    if clear_from < clear_end {
        values[clear_from..clear_end].fill(Value::Nil);
    }
    thread.stack.truncate(keep);
    thread.top = new_top as u32;
    if meta {
        FrameStep::Meta
    } else {
        FrameStep::Continue
    }
}

/// Replace a finished frame without changing its depth or its caller's result
/// mode. Upvalues and closes retain the slow path, before any stack mutation.
/// On `Continue` `switch` holds the callee.
#[inline(never)]
fn fast_tail_call<'h>(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'h>,
    pc: u32,
    func: u8,
    nargs: u8,
    switch: &mut Switch<'h>,
) -> FrameStep {
    let Some(frame) = thread.frames.last_mut() else {
        return FrameStep::Cold;
    };
    frame.pc = pc;
    if nargs == COUNT_OPEN {
        return FrameStep::Cold;
    }
    let frame = &*frame;
    if thread.open_above > frame.base || thread.tbc.last().is_some_and(|slot| *slot >= frame.base) {
        return FrameStep::Cold;
    }
    let func = frame.base + u32::from(func);
    let Value::Closure(closure) = thread.stack[func as usize] else {
        return FrameStep::Cold;
    };
    let Some(callee) = heap.closures.get(closure) else {
        return FrameStep::Cold;
    };
    let Some(proto) = heap.protos.get(callee.proto) else {
        return FrameStep::Cold;
    };
    let base = frame.base - frame.vararg_len;
    let limit = base + u32::from(proto.max_reg);
    if proto.vararg
        || limit > heap.ordinary
        || (limit as usize > thread.stack.len()
            && !heap
                .gc
                .fits(cost::STACK_SLOT * (u64::from(limit) - thread.stack.len() as u64)))
    {
        return FrameStep::Cold;
    }
    let nresults = frame.nresults;
    let charged_before = thread.charged_slots;
    let passed = u32::from(nargs);
    let from = func + 1;
    let src_end = (from + passed) as usize;
    // Match Entry::Replace's first growth and its capacity-counter boundary.
    if thread.stack.len() < src_end {
        grow_stack(thread, src_end, heap.gc);
    }
    let (src, dst) = if base == 0 {
        (from, base)
    } else {
        (func, base - 1)
    };
    thread
        .stack
        .copy_within(src as usize..src_end, dst as usize);
    let arg_end = base + passed;
    let old_len = thread.stack.len();
    let end = limit.max(arg_end) as usize;
    let stack_grew = old_len < end && grow_stack(thread, end, heap.gc);
    let params = u32::from(proto.params).min(u32::from(proto.max_reg));
    let clear_from = (base + params.min(passed)) as usize;
    // The replaced frame truncates to limit immediately, so extra arguments
    // above limit are dead before any observer can run. Resize nilled new slots.
    let clear_end = old_len.min(limit as usize);
    if clear_from < clear_end {
        thread.stack[clear_from..clear_end].fill(Value::Nil);
    }
    thread.top = base + params;
    thread.stack.truncate(limit as usize);
    count!("tail_calls");
    count!("lua_calls");
    count!("frame_replacements");
    *thread.frames.last_mut().expect("hot tail caller") = Frame {
        closure,
        pc: 0,
        base,
        limit,
        nresults,
        vararg_len: 0,
        flags: Frame::TAIL,
        cold: None,
    };
    if stack_grew {
        *heap.stack_grows = heap.stack_grows.saturating_add(1);
    }
    *switch = Switch {
        closure: callee,
        proto,
        base,
        pc: 0,
    };
    if thread.charged_slots > charged_before {
        FrameStep::Resync
    } else {
        FrameStep::Continue
    }
}

/// Direct Lua metamethods keep the ordinary MetaCall image, including its
/// scratch window and the caller's unchanged pc. Callable chains, varargs and
/// reserve/quota cases decline before touching that image.
#[inline(never)]
fn fast_meta_call(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'_>,
    event: MetaEvent,
    function: Value,
    args: &[Value],
) -> bool {
    let Value::Closure(closure) = function else {
        return false;
    };
    let Some(proto) = heap
        .closures
        .get(closure)
        .and_then(|closure| heap.protos.get(closure.proto))
    else {
        return false;
    };
    if proto.vararg {
        return false;
    }
    let caller = thread.frames.last().expect("metamethod caller");
    let slot = caller.limit.max(thread.top);
    let base = slot + 1;
    let scratch_end = base + args.len() as u32;
    let len = thread.stack.len() as u32;
    // The slow path checks the scratch window, charges its high-water growth,
    // then checks the Lua window against that updated logical heap: the
    // scratch part here, the window part once in the builder's checker.
    let scratch_charge =
        cost::STACK_SLOT * u64::from(scratch_end.saturating_sub(thread.charged_slots));
    if scratch_end > heap.ordinary
        || !heap
            .gc
            .fits(cost::STACK_SLOT * u64::from(scratch_end.saturating_sub(len)))
    {
        return false;
    }
    let Some(admit) = check_fixed_frame(
        thread.frames.depth,
        len.max(scratch_end) as usize,
        heap,
        proto,
        base,
        args.len() as u32,
        Some(scratch_charge),
    ) else {
        return false;
    };
    // write_abs grows once per missing scratch slot. Bulk growth has the same
    // logical charge; preserve that diagnostic count in instrumented builds.
    if scratch_end > len {
        thread.charge_slots(scratch_end as usize, heap.gc);
        #[cfg(feature = "counters")]
        for _ in len..scratch_end {
            count!("stack_growths");
        }
        thread.stack.grow_to(scratch_end as usize);
    }
    thread.stack[slot as usize] = function;
    thread.stack[base as usize..scratch_end as usize].copy_from_slice(args);
    thread.top = scratch_end;
    let image = MetaCall {
        event,
        slot,
        nargs: args.len() as u8,
        phase: MetaPhase::Running,
        close: None,
    };
    thread
        .frames
        .last_mut()
        .expect("metamethod caller")
        .set_meta(Some(image), heap.cold_spare);
    write_fixed_frame(
        thread,
        heap,
        admit,
        closure,
        proto.params,
        base,
        args.len() as u32,
        1,
        false,
    );
    true
}

/// Complete a plain metamethod continuation at a frame boundary. A return
/// exhausting the quantum leaves MetaCall intact, just as poll does; this
/// uncharged step runs only when the epoch has an instruction allowance.
#[inline(never)]
fn fast_commit_meta(
    thread: &mut ThreadObj,
    closures: &Arena<ClosureObj>,
    protos: &Arena<crate::heap::Proto>,
    spare: &mut Option<Box<FrameCold>>,
) -> bool {
    let frame = thread.frames.last_mut().expect("metamethod caller");
    let meta = frame.meta().expect("metamethod continuation");
    if meta.phase != MetaPhase::Running || meta.close.is_some() {
        return false;
    }
    let result = thread
        .stack
        .get(meta.slot as usize)
        .copied()
        .unwrap_or(Value::Nil);
    let mut pc = frame.pc + 1;
    let store = match meta.event {
        MetaEvent::Store { dst } => Some((frame.base + u32::from(dst), result)),
        MetaEvent::Truth { dst, negate } => {
            let bit = result.truthy() != negate;
            if dst == COUNT_OPEN {
                let Some(Op::CompareBranch { sense, offset, .. }) = closures
                    .get(frame.closure)
                    .and_then(|closure| protos.get(closure.proto))
                    .and_then(|proto| proto.ops.get(frame.pc as usize))
                else {
                    return false;
                };
                pc = (i64::from(frame.pc) + 1 + i64::from(if bit == *sense { *offset } else { 0 }))
                    as u32;
                None
            } else {
                Some((frame.base + u32::from(dst), Value::Bool(bit)))
            }
        }
        MetaEvent::NewIndex => None,
        MetaEvent::NewIndexAssign | MetaEvent::Close => return false,
    };
    if store.is_some_and(|(dst, _)| dst >= meta.slot || dst as usize >= thread.stack.len()) {
        return false;
    }
    thread.stack.truncate(meta.slot as usize);
    thread.top = meta.slot;
    if let Some((dst, value)) = store {
        thread.stack[dst as usize] = value;
    }
    // The continuation was inspected above. Clear it in place, then return
    // an otherwise empty cold payload to the reusable slot.
    let cold = frame.cold.as_mut().expect("metamethod cold state");
    cold.meta = None;
    if cold.is_empty() {
        *spare = frame.cold.take();
    }
    frame.pc = pc;
    true
}

/// What the hot tier may touch besides the frame's registers: table
/// contents, string bytes for keys, and the running prototype's constants.
pub(crate) struct HotHeap<'a> {
    tables: &'a mut Arena<TableObj>,
    strings: &'a Arena<StringObj>,
    proto: &'a Proto,
}

/// Cells of the validated running closure, and stack slots in enclosing
/// frames. These operations allocate no VM objects and charge no bytes, so
/// they cannot make gc_schedule due within an epoch.
struct HotUpvalues<'a> {
    closure: &'a ClosureObj,
    upvalues: &'a mut Arena<UpvalueObj>,
    active: &'a Handle<ThreadObj>,
    below: &'a mut [Value],
}

/// Both tiers share the hot handlers. A decline writes neither registers nor
/// pc. Register operands and jump targets have been checked by `check_code`.
#[inline(always)]
fn hot_op(core: &mut HotCore<'_, '_>, op: Op) -> Step {
    macro_rules! hit {
        ($value:expr) => {
            match $value {
                Some(value) => value,
                None => return Step::Cold,
            }
        };
    }
    let regs = &mut *core.regs;
    let heap = &mut *core.heap;
    match op {
        Op::Call { func, .. } | Op::TailCall { func, .. } => {
            return if matches!(regs[usize::from(func)], Value::Closure(_)) {
                Step::Frame
            } else {
                Step::Cold
            };
        }
        Op::Return { .. } => return Step::Frame,
        Op::Index { dst, obj, key } => {
            regs[dst as usize] = hit!(hot_get(heap, regs[obj as usize], regs[key as usize]));
        }
        Op::GetField { dst, obj, name } => {
            regs[dst as usize] = hit!(hot_get_name(heap, regs[obj as usize], name, core.pc));
        }
        Op::SetIndex { obj, key, src } => {
            hit!(hot_set(
                heap,
                regs[obj as usize],
                regs[key as usize],
                regs[src as usize]
            ));
        }
        Op::SetField { obj, name, src } => {
            hit!(hot_set_name(
                heap,
                regs[obj as usize],
                name,
                regs[src as usize],
                core.pc
            ));
        }
        Op::LoadNil { dst } => regs[dst as usize] = Value::Nil,
        Op::LoadInt { dst, value } => regs[dst as usize] = Value::Integer(value),
        Op::LoadFloat { dst, bits } => regs[dst as usize] = Value::Float(f64::from_bits(bits)),
        Op::LoadBool { dst, value } => regs[dst as usize] = Value::Bool(value),
        Op::Move { dst, src } => regs[dst as usize] = regs[src as usize],
        Op::GetUpvalue { dst: reg, index } | Op::SetUpvalue { index, src: reg } => {
            let upvalues = hit!(core.upvalues.as_mut());
            let handle = *hit!(upvalues.closure.upvalues.get(index as usize));
            match hit!(upvalues.upvalues.get(handle)).state {
                UpvalueState::Closed(value) => {
                    if matches!(op, Op::GetUpvalue { .. }) {
                        regs[reg as usize] = value;
                    } else {
                        // The same cell write barrier as write_upvalue.
                        hit!(upvalues.upvalues.get_mut(handle)).state =
                            UpvalueState::Closed(regs[reg as usize]);
                    }
                }
                UpvalueState::Open { thread, slot }
                    if (thread.index == upvalues.active.index)
                        & (thread.generation == upvalues.active.generation) =>
                {
                    let below = hit!(upvalues.below.get_mut(slot as usize));
                    if matches!(op, Op::GetUpvalue { .. }) {
                        regs[reg as usize] = *below;
                    } else {
                        // threads.get_mut took the thread barrier at epoch entry.
                        *below = regs[reg as usize];
                    }
                }
                _ => return Step::Cold,
            }
        }
        Op::Add { dst, a, b }
        | Op::Arith {
            op: ArithOp::Add,
            dst,
            a,
            b,
        } => {
            regs[dst as usize] = hit!(hot_arith(ArithOp::Add, regs[a as usize], regs[b as usize]));
        }
        Op::Arith { op, dst, a, b } => {
            regs[dst as usize] = hit!(hot_arith(op, regs[a as usize], regs[b as usize]));
        }
        Op::ArithK {
            op,
            dst,
            reg,
            constant,
            reverse,
        } => {
            let Value::Integer(value) = regs[reg as usize] else {
                return Step::Cold;
            };
            let (a, b) = if reverse {
                (constant, value)
            } else {
                (value, constant)
            };
            regs[dst as usize] = if let Some(value) = crate::arith::int_op(op, a, b) {
                Value::Integer(value)
            } else {
                hit!(hot_numbers(op, Value::Integer(a), Value::Integer(b)))
            };
        }
        Op::Neg { dst, src } => {
            regs[dst as usize] = match regs[src as usize] {
                Value::Integer(value) => Value::Integer(value.wrapping_neg()),
                Value::Float(value) => Value::Float(-value),
                _ => return Step::Cold,
            };
        }
        Op::Jump { offset } => {
            core.pc = (i64::from(core.pc) + 1 + i64::from(offset)) as u32;
            return Step::Continue;
        }
        Op::JumpIfFalse { src, offset } => {
            if !regs[src as usize].truthy() {
                core.pc = (i64::from(core.pc) + 1 + i64::from(offset)) as u32;
                return Step::Continue;
            }
        }
        Op::Compare { kind, dst, a, b } => {
            let bit = match (regs[a as usize], regs[b as usize]) {
                (Value::Integer(x), Value::Integer(y)) => compare::int_op(kind, x, y),
                (Value::Float(x), Value::Float(y)) => compare::float_op(kind, x, y),
                (a, b) => hit!(hot_compare(kind, a, b)),
            };
            regs[dst as usize] = Value::Bool(bit);
        }
        Op::CompareBranch {
            kind,
            a,
            b,
            sense,
            offset,
        } => {
            let bit = match (regs[a as usize], regs[b as usize]) {
                (Value::Integer(x), Value::Integer(y)) => compare::int_op(kind, x, y),
                (Value::Float(x), Value::Float(y)) => compare::float_op(kind, x, y),
                _ => return Step::Cold,
            };
            if bit == sense {
                core.pc = (i64::from(core.pc) + 1 + i64::from(offset)) as u32;
                return Step::Continue;
            }
        }
        Op::ForLoop { base, offset } => {
            let at = base as usize;
            let more = match (regs[at], regs[at + 1], regs[at + 2]) {
                (Value::Integer(index), Value::Integer(count), Value::Integer(step)) => {
                    if let Some((index, count)) = fornum::advance_int(index, count, step) {
                        regs[at] = Value::Integer(index);
                        regs[at + 1] = Value::Integer(count);
                        regs[at + 3] = Value::Integer(index);
                        true
                    } else {
                        false
                    }
                }
                _ => hit!(hot_for_loop(regs, at)),
            };
            if more {
                core.pc = (i64::from(core.pc) + 1 + i64::from(offset)) as u32;
                return Step::Continue;
            }
        }
        Op::JumpIfLt { a, b, offset } => match (regs[a as usize], regs[b as usize]) {
            (Value::Integer(left), Value::Integer(right)) => {
                if left < right {
                    core.pc = (i64::from(core.pc) + 1 + i64::from(offset)) as u32;
                    return Step::Continue;
                }
            }
            _ => return Step::Fault(LuaFault::Type),
        },
        _ => return Step::Cold,
    }
    core.pc += 1;
    Step::Continue
}

/// Restored stacks may end before the frame limit. Missing registers read
/// as nil, and only an actual write extends the serialized stack. Padding the
/// canonical stack at frame entry would change checkpoints even for a Jump.
/// Keep this exceptional path outside the core; the handlers still have one
/// implementation. This is called only for exec_rare's simple hot set.
#[cold]
#[inline(never)]
fn short_hot(
    op: Op,
    stack: &mut crate::heap::Stack,
    base: usize,
    max_reg: usize,
    pc: &mut u32,
    mut heap: HotHeap<'_>,
) -> Step {
    let mut window = [Value::Nil; u8::MAX as usize];
    let present = stack.len().saturating_sub(base).min(max_reg);
    if present != 0 {
        window[..present].copy_from_slice(&stack[base..base + present]);
    }
    let write_end = match op {
        Op::LoadNil { dst }
        | Op::LoadInt { dst, .. }
        | Op::LoadFloat { dst, .. }
        | Op::LoadBool { dst, .. }
        | Op::Move { dst, .. } => usize::from(dst) + 1,
        Op::ForLoop { base, .. } => {
            let at = usize::from(base);
            if matches!(
                fornum::advance(window[at], window[at + 1], window[at + 2]),
                Ok(Some(_))
            ) {
                at + 4
            } else {
                0
            }
        }
        _ => 0,
    };
    let mut core = HotCore {
        regs: &mut window[..max_reg],
        pc: *pc,
        heap: &mut heap,
        upvalues: None,
    };
    let step = hot_op(&mut core, op);
    if matches!(step, Step::Continue) {
        *pc = core.pc;
        if write_end != 0 {
            if stack.len() < base + write_end {
                count!("stack_growths");
                stack.grow_to(base + write_end);
            }
            stack[base..base + write_end].copy_from_slice(&core.regs[..write_end]);
        }
    }
    step
}

// Table hits for `hot_op`, kept out of line so the hot dispatch `match`
// stays small and its layout does not depend on these bodies. `None` means
// not a hit; nothing was written.

/// Inline only the small same-type numeric operations. The full primitive
/// fallback is out of line, and a decline leaves the registers untouched.
#[inline(always)]
fn hot_arith(op: ArithOp, a: Value, b: Value) -> Option<Value> {
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => {
            if let Some(value) = crate::arith::int_op(op, x, y) {
                return Some(Value::Integer(value));
            }
        }
        (Value::Float(x), Value::Float(y))
            if matches!(
                op,
                ArithOp::Add | ArithOp::Sub | ArithOp::Mul | ArithOp::Div
            ) =>
        {
            return Some(Value::Float(crate::arith::float_op(op, x, y)));
        }
        _ => {}
    }
    hot_numbers(op, a, b)
}

#[inline(always)]
fn arithk_operands(reg: Value, constant: i64, reverse: bool) -> (Value, Value) {
    let constant = Value::Integer(constant);
    if reverse {
        (constant, reg)
    } else {
        (reg, constant)
    }
}

#[inline(never)]
fn hot_numbers(op: ArithOp, a: Value, b: Value) -> Option<Value> {
    crate::arith::numbers(op, a, b)
}

#[inline(never)]
fn hot_compare(kind: CmpKind, a: Value, b: Value) -> Option<bool> {
    compare::numbers_only(kind, a, b)
}

#[inline(never)]
fn hot_for_loop(regs: &mut [Value], at: usize) -> Option<bool> {
    let (Value::Float(index), Value::Float(limit), Value::Float(step)) =
        (regs[at], regs[at + 1], regs[at + 2])
    else {
        return None;
    };
    if let Some(next) = fornum::advance_float(index, limit, step) {
        regs[at] = Value::Float(next);
        regs[at + 3] = Value::Float(next);
        Some(true)
    } else {
        Some(false)
    }
}

#[inline(never)]
fn hot_get(heap: &HotHeap<'_>, obj: Value, key: Value) -> Option<Value> {
    let Value::Table(table) = obj else {
        return None;
    };
    let bytes = match key {
        Value::String(handle) => {
            let object = heap.strings.get(handle)?;
            Some((object.bytes.as_slice(), object.hash()))
        }
        _ => None,
    };
    let view = crate::table::value_view(key, bytes)?;
    heap.tables.get(table)?.table.get_view(view)
}

#[inline(never)]
fn hot_get_name(heap: &HotHeap<'_>, obj: Value, name: u32, pc: u32) -> Option<Value> {
    let Value::Table(table) = obj else {
        return None;
    };
    let hash = *heap.proto.byte_hashes.get(name as usize)?;
    let name = heap.proto.byte_consts.get(name as usize)?;
    let hint = &heap.proto.field_hints.get(pc as usize)?.name;
    heap.tables
        .get(table)?
        .table
        .get_name_hint(name, hash, hint)
}

#[inline(never)]
fn hot_set(heap: &mut HotHeap<'_>, obj: Value, key: Value, value: Value) -> Option<()> {
    let Value::Table(table) = obj else {
        return None;
    };
    let bytes = match key {
        Value::String(handle) => {
            let object = heap.strings.get(handle)?;
            Some((object.bytes.as_slice(), object.hash()))
        }
        _ => None,
    };
    let view = crate::table::value_view(key, bytes)?;
    heap.tables
        .get_mut_storing(table, crate::gc::value_ref(value).is_some())?
        .table
        .update_view(view, value)
        .then_some(())
}

#[inline(never)]
fn hot_set_name(
    heap: &mut HotHeap<'_>,
    obj: Value,
    name: u32,
    value: Value,
    pc: u32,
) -> Option<()> {
    let Value::Table(table) = obj else {
        return None;
    };
    let hash = *heap.proto.byte_hashes.get(name as usize)?;
    let name = heap.proto.byte_consts.get(name as usize)?;
    let hint = &heap.proto.field_hints.get(pc as usize)?.name;
    heap.tables
        .get_mut_storing(table, crate::gc::value_ref(value).is_some())?
        .table
        .update_name_hint(name, hash, hint, value)
        .then_some(())
}

fn jump_target(pc: u32, offset: i32) -> Result<u32, VmError> {
    let next = i64::from(pc) + 1 + i64::from(offset);
    u32::try_from(next).map_err(|_| VmError::Corrupt)
}

impl Runtime {
    pub(crate) fn boot(
        config: Config,
        registry: HostRegistry,
        spec: &ProtoSpec,
        suspended: bool,
    ) -> Result<Self, VmError> {
        let limits = config.limits();
        let mut runtime = Self {
            #[cfg(any(test, debug_assertions))]
            hot_core: HotCoreMode::initial(),
            #[cfg(feature = "counters")]
            counters: Default::default(),
            owner: OwnerToken::mint(),
            heap: Heap::new(),
            registry,
            native_slots: Vec::new(),
            native_args: Vec::new(),
            native_results: Vec::new(),
            native_owned_buffers: std::array::from_fn(|_| Vec::new()),
            cold_spare: None,
            lib_task_spare: None,
            main_call_closure: None,
            in_callback: false,
            callback_failed: false,
            lib_call_args: Vec::new(),
            host_capabilities: crate::HostCapabilities::default(),
            output: None,
            warnings: None,
            entropy: None,
            effect_domain: config.effect_domain,
            next_sequence: 1,
            fuel_consumed: 0,
            fuel_limit: config.fuel_limit,
            max_objects: limits.max_objects,
            max_stack_slots: limits.max_stack_slots,
            max_snapshot: limits.max_snapshot_bytes,
            trap: None,
            hook_trap: false,
            last_completed_wait: None,
            completed_waits: std::collections::HashSet::new(),
            wait_index: std::collections::HashMap::new(),
            wait_roots: std::collections::HashMap::new(),
            host_call: false,
            pins: Vec::new(),
            stack_grows: 0,
            frame_grows: 0,
            cold_steps: 0,
            #[cfg(test)]
            gc_log: Vec::new(),
            #[cfg(feature = "__measure")]
            gc_slices: Vec::new(),
            #[cfg(test)]
            skip_unwind_close: false,
            #[cfg(test)]
            skip_tail_close: false,
        };
        #[cfg(test)]
        {
            runtime.heap.collector.audit = true;
        }
        runtime.heap.library = crate::library::LibraryState::new(config.entropy);
        runtime.heap.max_string = limits.max_string_bytes as usize;
        runtime.heap.gc = GcState::new(
            config.auto_gc,
            config.gc_min_debt,
            runtime.max_objects,
            limits.max_logical_heap,
        );
        // Every error class's message, made before anything can fail, so
        // raising an error never allocates (ADR 0024); then the names
        // `type` and `tostring` return without allocating (ADR 0031).
        for text in crate::heap::reserved_texts() {
            let handle = runtime.alloc_string(text.as_bytes().to_vec())?;
            runtime.heap.reserved.push(handle);
        }
        let source = match spec.debug.as_ref().and_then(|debug| debug.source.clone()) {
            Some(name) => Some(runtime.alloc_string(name)?),
            None => None,
        };
        let proto = runtime.install(spec, source)?;
        let globals = runtime.alloc_table()?;
        runtime.heap.globals = Some(globals);
        let closure = runtime.chunk_closure(proto, spec, Value::Table(globals))?;
        let thread = runtime.alloc_thread(
            closure,
            if suspended {
                Status::LuaSuspended
            } else {
                Status::Ready
            },
        )?;
        runtime.heap.entry = Some(thread);
        runtime.heap.active = Some(thread);
        // Generational mode starts as Lua's interpreter does, with a full
        // collection that makes what there is old (ADR 0051).
        if config.gc_mode == GcMode::Generational {
            runtime.heap.gc.generational = true;
            gc::full(&mut runtime.heap, &runtime.pins, runtime.max_objects);
        }
        Ok(runtime)
    }

    pub(crate) fn owner(&self) -> OwnerToken {
        self.owner
    }

    /// Execution fuel consumed since construction, including snapshot-restored fuel.
    pub fn fuel_consumed(&self) -> u64 {
        self.fuel_consumed
    }

    /// The host-selected journal lineage.
    pub fn effect_domain(&self) -> u64 {
        self.effect_domain
    }

    /// The next deterministic effect sequence number.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// The main thread's logical object identity.
    pub fn entry_id(&self) -> Result<ObjectId, VmError> {
        let handle = self.heap.entry.ok_or(VmError::Corrupt)?;
        Ok(self.heap.threads.get(handle).ok_or(VmError::Corrupt)?.id)
    }

    /// Borrow the native and userdata registrations used by this runtime.
    pub fn registry(&self) -> &HostRegistry {
        &self.registry
    }

    /// Whether this logical object identity is still live.
    pub fn contains_id(&self, id: ObjectId) -> bool {
        self.heap.find_by_id(id).is_some()
    }

    /// A host call that names objects by id, roots one, or runs Lua,
    /// while a run is paused in an atomic phase: the phase finishes
    /// first, so the call never reaches an object the cycle has already
    /// decided about, nor makes a reference the phase has passed
    /// (ADR 0050). Charged like the collector's other work.
    fn settle_atomic(&mut self) {
        if self.in_callback {
            return;
        }
        if self.heap.collector.holds() {
            #[cfg(test)]
            let cycles = self.heap.gc.collections + self.heap.gc.minors;
            let units = gc::work(&mut self.heap, &self.pins, u64::MAX, self.max_objects);
            self.charge_gc(units);
            #[cfg(test)]
            if self.heap.gc.collections + self.heap.gc.minors != cycles {
                self.gc_log.push(self.fuel_consumed);
            }
        }
    }

    /// Create a legacy explicit root; prefer Runtime::object and owned Value objects.
    pub fn root_id(&mut self, id: ObjectId) -> Result<Root, RootError> {
        // While a cycle marks, the atomic phase grays every root again.
        self.settle_atomic();
        let (kind, index, generation) = self.heap.find_by_id(id).ok_or(RootError::NotFound)?;
        self.heap.host_roots.push(crate::heap::HostRoot {
            kind,
            index,
            generation,
            id,
        });
        Ok(Root {
            owner: self.owner,
            kind,
            index,
            generation,
            id,
        })
    }

    /// Release a legacy explicit root in its owning runtime.
    pub fn release_root(&mut self, root: Root) -> Result<(), RootError> {
        self.ensure_owner(root)?;
        let position = self
            .heap
            .host_roots
            .iter()
            .position(|held| {
                held.kind == root.kind
                    && held.index == root.index
                    && held.generation == root.generation
                    && held.id == root.id
            })
            .ok_or(RootError::Released)?;
        self.heap.host_roots.swap_remove(position);
        Ok(())
    }

    /// Check whether a legacy explicit root is still registered and live.
    pub fn root_alive(&self, root: &Root) -> Result<bool, RootError> {
        self.ensure_owner(*root)?;
        let registered = self.heap.host_roots.iter().any(|held| {
            held.kind == root.kind
                && held.index == root.index
                && held.generation == root.generation
                && held.id == root.id
        });
        if !registered {
            return Err(RootError::Released);
        }
        if self.heap.find_by_id(root.id).is_none() {
            return Err(RootError::Stale);
        }
        Ok(true)
    }

    /// A full collection now (ADR 0050): a cycle still marking is
    /// abandoned and one past its atomic phase finishes first, then one
    /// whole cycle runs. Like any cycle it resets the debt and sets the
    /// next threshold. The host's own work, so not charged; nothing
    /// while a finalizer runs.
    pub fn collect(&mut self) {
        if self.in_callback || self.trap.is_some() || self.heap.finalizers.running {
            return;
        }
        gc::full(&mut self.heap, &self.pins, self.max_objects);
        #[cfg(test)]
        self.gc_log.push(self.fuel_consumed);
    }

    /// Read logical heap usage and collection counters.
    pub fn memory(&self) -> MemoryUsage {
        let gc = &self.heap.gc;
        MemoryUsage {
            objects: self.heap.live_objects(),
            max_objects: self.max_objects,
            logical_bytes: gc.used,
            debt: gc.debt,
            threshold: gc.threshold,
            collections: gc.collections.saturating_add(gc.minors),
            young_collections: gc.minors,
            auto_gc: gc.auto,
        }
    }

    /// Turn automatic collection on or off. Explicit [`Runtime::collect`]
    /// works either way. The setting is snapshot state.
    pub fn set_auto_gc(&mut self, on: bool) {
        self.heap.gc.auto = on;
    }

    /// A collection an allocation needs: a full one at once, even inside
    /// a finalizer, as Lua's emergency collection is, and like any
    /// collection it only queues the finalizers it finds due (ADR 0048).
    /// Its work is charged to the fuel like any collector work.
    fn emergency_collect(&mut self) {
        // Typed conversions may have written results before a later one
        // allocates. Their raw result window is a root until delivery.
        let pins = self.pins.len();
        self.pins.extend(
            self.native_results
                .iter()
                .copied()
                .filter_map(gc::value_ref),
        );
        let units = gc::full(&mut self.heap, &self.pins, self.max_objects);
        self.pins.truncate(pins);
        self.charge_gc(units);
        #[cfg(test)]
        self.gc_log.push(self.fuel_consumed);
    }

    /// Charge `units` of collector work to the fuel (ADR 0050): a unit of
    /// fuel pays for [`gc::WORK_PER_FUEL`] units, and what it paid for and
    /// was not used yet carries over, so how the work is split cannot
    /// change what it costs. Returns the fuel charged.
    fn charge_gc(&mut self, units: u64) -> u64 {
        let gc = &mut self.heap.gc;
        let prepaid = u64::from(gc.prepaid);
        if units <= prepaid {
            gc.prepaid = (prepaid - units) as u32;
            return 0;
        }
        let per = u64::from(gc::WORK_PER_FUEL);
        let owed = units - prepaid;
        let fuel = owed.div_ceil(per);
        gc.prepaid = (fuel * per - owed) as u32;
        self.fuel_consumed = self.fuel_consumed.saturating_add(fuel);
        fuel
    }

    /// Collector work is wanted before Lua goes on: a step's units, the
    /// rest of an atomic phase, a full collection asked for.
    #[inline]
    fn gc_wanted(&self) -> bool {
        self.heap.gc.owed > 0 || self.heap.gc.full.is_some() || self.heap.collector.holds()
    }

    /// At a safe point: schedule a step once the allocation since the last
    /// one calls for it (ADR 0050).
    #[inline]
    fn gc_schedule(&mut self) {
        if self.heap.gc.due() {
            self.schedule_step();
        }
    }

    #[cold]
    #[inline(never)]
    fn schedule_step(&mut self) {
        // Not between the finalizers of one batch either: Lua runs a
        // batch with nothing collected in between, so finalizers that
        // register their objects again cannot keep the batch going.
        let finalizers = &self.heap.finalizers;
        if self.trap.is_some() || finalizers.running || !finalizers.pending.is_empty() {
            return;
        }
        let room = self.gc_room();
        gc::step(&mut self.heap, room, self.max_objects, false);
    }

    /// Logical bytes left before the quota or the object limit (each
    /// object costs at least `cost::OBJECT`), whichever is nearer
    /// (ADR 0050).
    pub(super) fn gc_room(&self) -> u64 {
        let objects = self.max_objects.saturating_sub(self.heap.live_objects());
        self.heap
            .gc
            .headroom()
            .min(cost::OBJECT * u64::from(objects))
    }

    /// Do collector work, as much as the quantum and the fuel limit pay
    /// for and no more than is wanted.
    #[cold]
    #[inline(never)]
    fn gc_slice(&mut self, quantum: &mut u64) -> Result<Poll, VmError> {
        if *quantum == 0 {
            return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
        }
        if self
            .fuel_limit
            .is_some_and(|limit| self.fuel_consumed >= limit)
        {
            self.trap = Some(TerminationReason::FuelLimitExceeded);
            return Ok(Poll::Stop(StepOutcome::Terminated(
                TerminationReason::FuelLimitExceeded,
            )));
        }
        let per = u64::from(gc::WORK_PER_FUEL);
        let mut fuel = *quantum;
        if let Some(limit) = self.fuel_limit {
            fuel = fuel.min(limit - self.fuel_consumed);
        }
        let budget = fuel
            .saturating_mul(per)
            .saturating_add(u64::from(self.heap.gc.prepaid));
        #[cfg(test)]
        let cycles = self.heap.gc.collections + self.heap.gc.minors;
        #[cfg(feature = "__measure")]
        let (start, sweeping, atomic, minor) = (
            crate::hostcaps::native::Instant::now(),
            self.heap.collector.phase == gc::Phase::Sweep,
            self.heap.gc.trace,
            (self.heap.collector.minor, self.heap.gc.minors),
        );
        let units = gc::work(&mut self.heap, &self.pins, budget, self.max_objects);
        #[cfg(feature = "__measure")]
        self.gc_slices.push((
            units,
            start.elapsed().as_nanos() as u64,
            sweeping,
            atomic != self.heap.gc.trace && self.heap.collector.phase != gc::Phase::Propagate,
            minor.0 || self.heap.collector.minor || minor.1 != self.heap.gc.minors,
        ));
        let charged = self.charge_gc(units);
        *quantum = quantum.saturating_sub(charged);
        #[cfg(test)]
        if self.heap.gc.collections + self.heap.gc.minors != cycles {
            self.gc_log.push(self.fuel_consumed);
        }
        Ok(Poll::Continue)
    }

    /// Called by an instruction that will allocate up to `count` objects
    /// and `bytes` logical bytes, before it allocates any. If they would
    /// not fit under the object limit or the logical-heap quota, collect
    /// first. Nothing is held outside the roots yet, so this
    /// is as safe as the safe point before the instruction.
    fn make_room(&mut self, count: u32, bytes: u64) {
        if self.heap.gc.auto
            && self.trap.is_none()
            && (self.heap.live_objects().saturating_add(count) > self.max_objects
                || !self.heap.gc.fits(bytes))
        {
            self.emergency_collect();
        }
    }

    /// A table store the quota refused collects once, when automatic
    /// collection may run, and is then tried again (ADR 0025). The refused
    /// store changed nothing, and its table, key, and value sit in
    /// registers, constants, or the frame's recorded targets, so this is as
    /// safe as collecting before the instruction. True when it collected.
    fn collect_for_store(&mut self) -> bool {
        let collect = self.heap.gc.auto && self.trap.is_none();
        if collect {
            self.emergency_collect();
        }
        collect
    }

    /// `index::set`, collecting once if the quota refuses the store.
    fn set_value(&mut self, obj: Value, key: Value, value: Value) -> Result<Resolved, LuaFault> {
        match index::set(&mut self.heap, obj, key, value) {
            Err(LuaFault::Memory) if self.collect_for_store() => {
                index::set(&mut self.heap, obj, key, value)
            }
            other => other,
        }
    }

    /// Drive execution for at most quantum fuel units. API misuse is VmError::Api; Lua failures are StepOutcome::LuaError.
    pub fn run(&mut self, mut quantum: u64, journal: &mut Journal) -> Result<StepOutcome, VmError> {
        #[cfg(feature = "counters")]
        let _scope = self.counter_scope();
        if self.in_callback
            || self.callback_failed
            || self
                .heap
                .finalizers
                .exit
                .is_some_and(|exit| exit.phase == crate::heap::ExitPhase::Terminal)
        {
            return Err(VmError::Api(crate::ApiError::InvalidCallState));
        }
        // A freshly built idle runtime has no execution to poll. Completed
        // executions remain pollable for legacy callers and checkpoint loops.
        if !self.host_call && self.trap.is_none() && self.fuel_consumed == 0 {
            let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
            let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
            if thread.status == Status::Completed && thread.frames.is_empty() {
                return Err(VmError::Api(crate::ApiError::InvalidCallState));
            }
        }
        let result = (|| {
            let outcome = loop {
                if let Some(outcome) = self.run_hot(&mut quantum, journal)? {
                    break outcome;
                }
                match self.poll(&mut quantum, journal)? {
                    Poll::Continue => {}
                    Poll::Stop(outcome) => break outcome,
                }
            };
            Ok(outcome)
        })();
        if matches!(result, Err(VmError::Api(_))) {
            self.callback_failed = true;
        }
        result
    }

    /// Drive successive quanta until an outcome other than a fuel pause.
    /// A zero quantum returns a pause without looping.
    pub fn run_until_terminal(
        &mut self,
        quantum: u64,
        journal: &mut Journal,
    ) -> Result<StepOutcome, VmError> {
        if quantum == 0 {
            return self.run(0, journal);
        }
        loop {
            match self.run(quantum, journal)? {
                StepOutcome::Paused(_) => {}
                other => return Ok(other),
            }
        }
    }

    /// End a native wait with one integer result.
    #[deprecated(note = "use Runtime::complete with Completion::Return")]
    pub fn complete_wait(&mut self, key: WaitKey, result: i64) -> Result<(), WaitError> {
        self.finish_wait(key, None, result)
    }

    /// End the native call waiting on `key`: it returns the given values,
    /// or raises the given error in the waiting thread, as `error(value)`
    /// would (ADR 0024). A value the heap has no room for raises a memory
    /// error instead. `CallHost` waits take the first value only.
    #[deprecated(note = "use complete with rooted Completion values")]
    pub fn complete_legacy(
        &mut self,
        key: WaitKey,
        completion: LegacyCompletion,
    ) -> Result<(), WaitError> {
        self.finish_wait(key, Some(&completion), 0)
    }

    /// `complete`, or, without a completion, `complete_wait` with `integer`,
    /// which allocates nothing.
    fn finish_wait(
        &mut self,
        key: WaitKey,
        completion: Option<&LegacyCompletion>,
        integer: i64,
    ) -> Result<(), WaitError> {
        self.settle_atomic();
        if self.last_completed_wait == Some(key.raw()) {
            return Err(WaitError::AlreadyCompleted);
        }
        let target = self.wait_index.get(&key.raw()).copied();
        let one = [Value::Integer(integer)];
        let converted;
        let values: &[Value] = match (target, completion) {
            (None, _) => &[],
            (Some(_), None) => &one,
            (Some(_), Some(completion)) => match self.completion_values(completion) {
                Ok(values) => {
                    converted = values;
                    &converted
                }
                Err(VmError::MemoryLimit) => {
                    // No room for the values: the call fails instead.
                    let error = self.fault_value(LuaFault::Memory);
                    return self.fail_wait(target, key, LuaFault::Memory, error);
                }
                Err(_) => return Err(WaitError::InvalidValue),
            },
        };
        self.finish_wait_raw(
            key,
            values,
            matches!(completion, Some(LegacyCompletion::Error(_))),
        )
    }

    fn finish_wait_raw(
        &mut self,
        key: WaitKey,
        values: &[Value],
        error: bool,
    ) -> Result<(), WaitError> {
        self.settle_atomic();
        if self.last_completed_wait == Some(key.raw()) || self.completed_waits.contains(&key.raw())
        {
            return Err(WaitError::AlreadyCompleted);
        }
        let target = self.wait_index.get(&key.raw()).copied();
        let mut matched = None;
        let mut native = None;
        if let Some(thread) = target {
            let frame = self
                .heap
                .threads
                .get(thread)
                .and_then(|t| t.frames.last())
                .ok_or(WaitError::NotWaiting)?;
            if matches!(frame.pending(), Some(Pending::Capability { .. })) {
                return Err(WaitError::NotWaiting);
            }
            match frame.pending() {
                Some(Pending::Waiting { dest, .. }) => matched = Some((thread, *dest, frame.base)),
                _ => native = Some(thread),
            }
        }
        let other = !self.wait_index.is_empty();
        let target = native.or(matched.map(|(thread, _, _)| thread));
        if error && target.is_some() {
            let error = values.first().copied().unwrap_or(Value::Nil);
            return self.fail_wait(target, key, LuaFault::Error, error);
        }
        if let Some(thread) = target {
            self.clear_wait_request(thread);
        }
        if let Some(thread) = native {
            // Delivered through the call's result window like any other
            // native return.
            let (func_abs, nresults, passed, _) =
                self.call_site(thread).map_err(|_| WaitError::NotWaiting)?;
            let produced = u32::try_from(values.len()).unwrap_or(u32::MAX);
            let end = func_abs.saturating_add(Self::wanted(nresults, produced));
            // More values than the stack holds: the call fails instead.
            if let Some(fault) = self
                .slot_fault(thread, end)
                .map_err(|_| WaitError::NotWaiting)?
            {
                let error = self.fault_value(fault);
                return self.fail_wait(Some(thread), key, fault, error);
            }
            self.deliver_native(thread, func_abs, nresults, values, func_abs + 1 + passed)
                .map_err(|_| WaitError::NotWaiting)?;
            let object = self
                .heap
                .threads
                .get_mut(thread)
                .ok_or(WaitError::NotWaiting)?;
            if let Some(frame) = object.frames.last_mut() {
                match frame.meta_mut() {
                    // The instruction commits on the next step.
                    Some(meta) => meta.phase = MetaPhase::Running,
                    None => {
                        frame.set_pending(None, &mut self.cold_spare);
                        frame.set_wait_request(None, &mut self.cold_spare);
                        if frame.boundary().is_none() {
                            frame.pc = frame.pc.saturating_add(1);
                        }
                    }
                }
            }
            object.status = Status::Ready;
            self.heap.active = Some(thread);
            self.refresh_hook_trap();
            self.wait_finished(key);
            return Ok(());
        }
        let Some((thread, dest, base)) = matched else {
            return Err(if other {
                WaitError::UnknownKey
            } else {
                WaitError::NotWaiting
            });
        };
        let absolute = base + u32::from(dest);
        let Some(object) = self.heap.threads.get_mut(thread) else {
            return Err(WaitError::NotWaiting);
        };
        grow_stack(object, absolute as usize + 1, &mut self.heap.gc);
        object.stack[absolute as usize] = values.first().copied().unwrap_or(Value::Nil);
        if let Some(frame) = object.frames.last_mut() {
            frame.set_pending(None, &mut self.cold_spare);
            frame.pc = frame.pc.saturating_add(1);
        }
        object.status = Status::Ready;
        self.heap.active = Some(thread);
        self.refresh_hook_trap();
        self.wait_finished(key);
        Ok(())
    }

    /// The waiting call fails with `error`: the thread unwinds from it.
    fn fail_wait(
        &mut self,
        thread: Option<Handle<ThreadObj>>,
        key: WaitKey,
        fault: LuaFault,
        error: Value,
    ) -> Result<(), WaitError> {
        let thread = thread.ok_or(WaitError::NotWaiting)?;
        self.clear_wait_request(thread);
        // The wait is over. Clear it now, so no later completion reaches
        // the frame while the unwind is still on its way to it.
        if let Some(frame) = self
            .heap
            .threads
            .get_mut(thread)
            .and_then(|object| object.frames.last_mut())
        {
            frame.set_pending(None, &mut self.cold_spare);
            frame.set_wait_request(None, &mut self.cold_spare);
            if let Some(meta) = frame.meta_mut()
                && matches!(meta.phase, MetaPhase::NativeWaiting { .. })
            {
                if meta.event == MetaEvent::Close {
                    // The close call is over, not the closes: the frame's
                    // close state stays for the unwind to reach, and it
                    // still marks where a `CloseThread` stops catching.
                    meta.phase = MetaPhase::Running;
                } else {
                    frame.set_meta(None, &mut self.cold_spare);
                }
            }
        }
        self.throw_on(thread, fault, error);
        self.heap.active = Some(thread);
        self.refresh_hook_trap();
        self.wait_finished(key);
        Ok(())
    }

    fn completion_values(&mut self, completion: &LegacyCompletion) -> Result<Vec<Value>, VmError> {
        match completion {
            LegacyCompletion::Return(values) => {
                values.iter().map(|value| self.host_value(value)).collect()
            }
            LegacyCompletion::Error(value) => Ok(vec![self.host_value(value)?]),
        }
    }

    /// A host value as a Lua value. Strings are allocated; objects and
    /// natives must already exist.
    fn host_value(&mut self, value: &HostValue) -> Result<Value, VmError> {
        Ok(match value {
            HostValue::Nil => Value::Nil,
            HostValue::Boolean(bit) => Value::Bool(*bit),
            HostValue::Integer(integer) => Value::Integer(*integer),
            HostValue::Number(float) => Value::Float(*float),
            HostValue::String(bytes) => Value::String(self.alloc_string(bytes.clone())?),
            HostValue::Object(id) => {
                let (kind, index, generation) =
                    self.heap.find_by_id(*id).ok_or(VmError::NotRunnable)?;
                match kind {
                    Kind::Table => Value::Table(Handle::new(index, generation)),
                    Kind::Closure => Value::Closure(Handle::new(index, generation)),
                    Kind::Thread => Value::Thread(Handle::new(index, generation)),
                    Kind::String => Value::String(Handle::new(index, generation)),
                    Kind::NativeClosure => Value::NativeClosure(Handle::new(index, generation)),
                    Kind::Userdata => Value::Userdata(Handle::new(index, generation)),
                    _ => return Err(VmError::NotRunnable),
                }
            }
            HostValue::Native(symbol) => self.native_value(symbol)?,
            // A token the VM made names a cell of the runtime that made
            // it; handed to another runtime it could equal a token of
            // its own (ADR 0043). Only host keys come in.
            HostValue::LightUserdata(light) => match light.host_key() {
                Some(key) => Value::LightUserdata(crate::value::LightDomain::Host, key.0),
                None => return Err(VmError::NotRunnable),
            },
        })
    }

    /// A Lua value as the host sees it.
    fn view_value(&self, value: Value) -> HostValue {
        match value {
            Value::Nil => HostValue::Nil,
            Value::Bool(bit) => HostValue::Boolean(bit),
            Value::Integer(integer) => HostValue::Integer(integer),
            Value::Float(float) => HostValue::Number(float),
            Value::String(handle) => {
                HostValue::String(self.heap.string_bytes(handle).unwrap_or_default().to_vec())
            }
            Value::Native(index) => HostValue::Native(
                self.heap
                    .natives
                    .get(index as usize)
                    .cloned()
                    .unwrap_or_default(),
            ),
            Value::LightUserdata(domain, bits) => {
                HostValue::LightUserdata(crate::userdata::LightUserdata { domain, bits })
            }
            other => self
                .heap
                .object_id_of_value(other)
                .map_or(HostValue::Nil, HostValue::Object),
        }
    }

    /// The error that failed the active thread, or the entry thread: its
    /// class and object. The failed thread keeps the object alive; root its
    /// id to keep it after the thread goes. Unstable API.
    pub fn lua_error(&self) -> Option<(LuaFault, HostValue)> {
        let thread = self
            .heap
            .active
            .and_then(|active| self.heap.threads.get(active))
            .filter(|thread| thread.error.is_some())
            .or_else(|| self.heap.threads.get(self.heap.entry?))?;
        let (fault, error) = thread.error?;
        Some((fault, self.view_value(error)))
    }

    /// Resume an existing Lua coroutine and drive it for this quantum.
    /// This legacy entry point takes the thread by unrooted logical identity.
    pub fn resume_thread(
        &mut self,
        id: ObjectId,
        quantum: u64,
        journal: &mut Journal,
    ) -> Result<StepOutcome, VmError> {
        if self.in_callback || self.callback_failed {
            return Err(VmError::Api(crate::ApiError::InvalidCallState));
        }
        self.settle_atomic();
        let handle = self.thread_by_id(id)?;
        let status = self
            .heap
            .threads
            .get(handle)
            .ok_or(VmError::Corrupt)?
            .status;
        if status != Status::LuaSuspended {
            return Err(VmError::NotRunnable);
        }
        {
            let thread = self.heap.threads.get_mut(handle).ok_or(VmError::Corrupt)?;
            thread.status = Status::Ready;
            thread.resumed_by = None;
        }
        self.heap.active = Some(handle);
        self.refresh_hook_trap();
        self.run_until_terminal(quantum, journal)
    }

    #[deprecated(note = "use Runtime::call and Runtime::finish_call with rooted Value objects")]
    /// Call an unrooted Lua closure through the legacy integer-only path.
    pub fn call_closure(&mut self, id: ObjectId, journal: &mut Journal) -> Result<i64, VmError> {
        #[cfg(feature = "counters")]
        let _scope = self.counter_scope();
        count!("host_to_lua_calls");
        if self.in_callback || self.callback_failed {
            return Err(VmError::Api(crate::ApiError::InvalidCallState));
        }
        self.settle_atomic();
        let closure = self.closure_by_id(id)?;
        let max_reg = {
            let proto = self
                .heap
                .closures
                .get(closure)
                .ok_or(VmError::Corrupt)?
                .proto;
            self.heap.protos.get(proto).ok_or(VmError::Corrupt)?.max_reg
        };
        let thread = self.alloc_thread(closure, Status::Ready)?;
        {
            let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
            grow_stack(object, usize::from(max_reg), &mut self.heap.gc);
            object.frames.clear();
            count!("frame_pushes");
            object.frames.push(Frame {
                closure,
                pc: 0,
                base: 0,
                limit: u32::from(max_reg),
                nresults: 1,
                vararg_len: 0,
                flags: 0,
                cold: None,
            });
        }
        let saved = self.heap.active;
        self.heap.active = Some(thread);
        self.refresh_hook_trap();
        // Collection may run inside the call; the thread it replaces as
        // active must survive it too.
        let pins = self.pins.len();
        for held in [Some(thread), saved].into_iter().flatten() {
            self.pins.push(TraceRef {
                kind: Kind::Thread,
                index: held.index,
            });
        }
        let outcome = self.run_until_terminal(u64::MAX, journal);
        self.pins.truncate(pins);
        self.heap.active = saved;
        self.refresh_hook_trap();
        let outcome = outcome?;
        if !matches!(outcome, StepOutcome::Completed) {
            return Err(VmError::NotRunnable);
        }
        let result = self
            .heap
            .threads
            .get(thread)
            .and_then(|thread| thread.host_results.first().copied())
            .ok_or(VmError::Corrupt)?;
        match result {
            Value::Integer(value) => Ok(value),
            _ => Err(VmError::Corrupt),
        }
    }

    #[deprecated(note = "use Runtime::call and Runtime::finish_call with rooted Value objects")]
    /// Read an integer global through the legacy value API.
    pub fn global_integer(&self, key: &str) -> Result<i64, VmError> {
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        let value = self.lookup_str(globals, key.as_bytes())?;
        match value {
            Value::Integer(integer) => Ok(integer),
            _ => Err(VmError::Corrupt),
        }
    }

    pub(crate) fn heap(&self) -> &Heap {
        &self.heap
    }

    /// Test hook: change state behind the runtime's back.
    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// Test hook: one unwind step of the active thread, so a test can stop
    /// between frame pops, which no fuel quantum does.
    #[cfg(test)]
    pub(crate) fn unwind_one_step(&mut self) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        self.unwind_step(active, &mut Journal::new()).map(|_| ())
    }

    /// Test hook: break the top frame's closure handle, as a VM bug would.
    #[cfg(test)]
    pub(crate) fn corrupt_top_frame(&mut self) {
        if let Some(active) = self.heap.active
            && let Some(thread) = self.heap.threads.get_mut(active)
            && let Some(frame) = thread.frames.last_mut()
        {
            frame.closure = Handle::new(u32::MAX - 1, 7);
        }
    }

    /// Test hook: a new string object, not a prototype constant, in
    /// register `reg` of the active frame. Source code cannot build one yet.
    #[cfg(test)]
    pub(crate) fn store_new_string(&mut self, reg: u8, bytes: &[u8]) -> Result<ObjectId, VmError> {
        let handle = self.alloc_string(bytes.to_vec())?;
        self.store(reg, Value::String(handle))?;
        Ok(self.heap.strings.get(handle).ok_or(VmError::Corrupt)?.id)
    }

    pub(crate) fn at_prepared(&self) -> Option<u64> {
        let thread = self.heap.active?;
        let object = self.heap.threads.get(thread)?;
        let frame = object.frames.last()?;
        if let Some(MetaCall {
            phase: MetaPhase::NativePrepared { sequence },
            ..
        }) = frame.meta()
        {
            return Some(*sequence);
        }
        match frame.pending() {
            Some(
                Pending::Prepared { sequence, .. }
                | Pending::NativePrepared { sequence }
                | Pending::Capability {
                    sequence,
                    completed: true,
                    ..
                },
            ) => Some(*sequence),
            _ => None,
        }
    }

    /// Finish a `Prepared` host call without charging fuel again.
    #[cfg(test)]
    pub(crate) fn commit_prepared(&mut self, journal: &mut Journal) -> Result<(), VmError> {
        if self.at_prepared().is_none() {
            return Err(VmError::Corrupt);
        }
        self.perform_host(journal)
    }

    pub(crate) fn entry_results(&self) -> Result<Vec<Value>, VmError> {
        let handle = self.thread_by_id(self.entry_id()?)?;
        Ok(self
            .heap
            .threads
            .get(handle)
            .ok_or(VmError::Corrupt)?
            .host_results
            .clone())
    }

    /// The values the entry function returned, once the run completed,
    /// as the host sees them. Unstable API.
    pub fn results(&self) -> Result<Vec<HostValue>, VmError> {
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        if thread.status != Status::Completed {
            return Err(VmError::NotRunnable);
        }
        Ok(thread
            .host_results
            .iter()
            .map(|value| self.view_value(*value))
            .collect())
    }

    /// Pass `args` to the entry function before it runs, laid out as a
    /// call lays out its arguments (`push_lua_frame`): a vararg function's
    /// extras below its registers, the fixed parameters in its first
    /// registers, and nothing else kept.
    pub(crate) fn set_entry_args(&mut self, args: &[HostValue]) -> Result<(), VmError> {
        let values = args
            .iter()
            .map(|arg| self.host_value(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let closure = self
            .heap
            .threads
            .get(entry)
            .and_then(|thread| thread.frames.first())
            .ok_or(VmError::Corrupt)?
            .closure;
        let proto = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let proto = self.heap.protos.get(proto).ok_or(VmError::Corrupt)?;
        let (params, max_reg) = (usize::from(proto.params), usize::from(proto.max_reg));
        let extra = if proto.vararg {
            values.len().saturating_sub(params)
        } else {
            0
        };
        let limit = extra + max_reg;
        // The ordinary part of the bound, so error handling keeps its room.
        if limit > (self.max_stack_slots - self.max_stack_slots / 8) as usize {
            return Err(VmError::StackLimit);
        }
        let mut stack = vec![Value::Nil; limit];
        stack[..extra].copy_from_slice(&values[values.len() - extra..]);
        let fixed = params.min(values.len()).min(max_reg);
        stack[extra..extra + fixed].copy_from_slice(&values[..fixed]);
        let thread = self.heap.threads.get_mut(entry).ok_or(VmError::Corrupt)?;
        let [frame] = thread.frames.as_mut_slice() else {
            return Err(VmError::NotRunnable);
        };
        if frame.pc != 0 || thread.status != Status::Ready {
            return Err(VmError::NotRunnable);
        }
        frame.base = extra as u32;
        frame.limit = limit as u32;
        frame.vararg_len = extra as u32;
        thread.top = limit as u32;
        thread.stack = stack.into();
        thread.charge_slots(limit, &mut self.heap.gc);
        Ok(())
    }

    #[deprecated(note = "use Runtime::call and Runtime::finish_call with rooted Value objects")]
    /// Read completed coroutine results through the legacy integer-only path.
    pub fn thread_integers(&self, id: ObjectId) -> Result<Vec<i64>, VmError> {
        let handle = self.thread_by_id(id)?;
        let thread = self.heap.threads.get(handle).ok_or(VmError::Corrupt)?;
        thread
            .host_results
            .iter()
            .map(|value| match value {
                Value::Integer(integer) => Ok(*integer),
                _ => Err(VmError::Corrupt),
            })
            .collect()
    }

    pub(crate) fn fuel_limit(&self) -> Option<u64> {
        self.fuel_limit
    }

    pub(crate) fn trap_public(&self) -> Option<TerminationReason> {
        self.trap
    }

    pub(crate) fn max_objects_public(&self) -> u32 {
        self.max_objects
    }

    pub(crate) fn max_stack_slots_public(&self) -> u32 {
        self.max_stack_slots
    }

    pub(crate) fn max_snapshot(&self) -> u64 {
        self.max_snapshot
    }

    /// The limits this runtime runs under.
    pub fn limits(&self) -> Limits {
        Limits {
            max_logical_heap: self.heap.gc.quota,
            max_objects: self.max_objects,
            max_stack_slots: self.max_stack_slots,
            max_string_bytes: self.heap.max_string as u64,
            max_snapshot_bytes: self.max_snapshot,
        }
    }

    pub(crate) fn in_callback(&self) -> bool {
        self.in_callback
    }

    pub(crate) fn completed_waits(&self) -> Vec<u64> {
        let mut keys: Vec<_> = self.completed_waits.iter().copied().collect();
        keys.sort_unstable();
        keys
    }
    pub(crate) fn host_call(&self) -> bool {
        self.host_call
    }
    pub(crate) fn callback_failed(&self) -> bool {
        self.callback_failed
    }

    pub(crate) fn last_completed_wait(&self) -> Option<u64> {
        self.last_completed_wait
    }

    pub(crate) fn from_restored(parts: RestoredParts) -> Self {
        // Restore checked that every symbol is registered.
        let native_slots = parts
            .heap
            .natives
            .iter()
            .map(|symbol| parts.registry.native_slot(symbol).unwrap_or(usize::MAX))
            .collect();
        #[allow(unused_mut)]
        let mut heap = parts.heap;
        #[cfg(test)]
        {
            heap.collector.audit = true;
        }
        let wait_index = heap
            .threads
            .iter()
            .filter_map(|(index, generation, object)| {
                let frame = object.frames.last()?;
                let key = match frame.meta().map(|meta| meta.phase) {
                    Some(MetaPhase::NativeWaiting { wait_key, .. }) => Some(wait_key),
                    _ => match frame.pending() {
                        Some(
                            Pending::Waiting { wait_key, .. }
                            | Pending::NativeWaiting { wait_key, .. }
                            | Pending::Capability {
                                wait_key,
                                completed: false,
                                ..
                            },
                        ) => Some(*wait_key),
                        _ => None,
                    },
                }?;
                Some((key, Handle::new(index, generation)))
            })
            .collect();
        let mut runtime = Self {
            #[cfg(any(test, debug_assertions))]
            hot_core: HotCoreMode::initial(),
            #[cfg(feature = "counters")]
            counters: Default::default(),
            owner: OwnerToken::mint(),
            heap,
            registry: parts.registry,
            native_slots,
            native_args: Vec::new(),
            native_results: Vec::new(),
            native_owned_buffers: std::array::from_fn(|_| Vec::new()),
            cold_spare: None,
            lib_task_spare: None,
            main_call_closure: None,
            in_callback: false,
            callback_failed: parts.callback_failed,
            lib_call_args: Vec::new(),
            host_capabilities: crate::HostCapabilities::default(),
            output: None,
            warnings: None,
            entropy: None,
            effect_domain: parts.effect_domain,
            next_sequence: parts.next_sequence,
            fuel_consumed: parts.fuel_consumed,
            fuel_limit: parts.fuel_limit,
            max_objects: parts.max_objects,
            max_stack_slots: parts.max_stack_slots,
            max_snapshot: parts.max_snapshot,
            trap: parts.trap,
            hook_trap: false,
            last_completed_wait: parts.last_completed_wait,
            completed_waits: parts.completed_waits,
            wait_index,
            wait_roots: std::collections::HashMap::new(),
            host_call: parts.host_call,
            pins: Vec::new(),
            stack_grows: 0,
            frame_grows: 0,
            cold_steps: 0,
            #[cfg(test)]
            gc_log: Vec::new(),
            #[cfg(feature = "__measure")]
            gc_slices: Vec::new(),
            #[cfg(test)]
            skip_unwind_close: false,
            #[cfg(test)]
            skip_tail_close: false,
        };
        for (key, thread) in runtime.wait_index.clone() {
            let vm_allocated = runtime
                .heap
                .threads
                .get(thread)
                .and_then(|object| object.frames.last())
                .is_some_and(|frame| frame.wait_request().is_some());
            if let Ok(root) = runtime.api_owned(Value::Thread(thread)) {
                runtime.wait_roots.insert(key, (root, vm_allocated));
            }
        }
        runtime
    }

    fn ensure_owner(&self, root: Root) -> Result<(), RootError> {
        if root.owner != self.owner {
            Err(RootError::ForeignRuntime)
        } else {
            Ok(())
        }
    }

    fn poll(&mut self, quantum: &mut u64, journal: &mut Journal) -> Result<Poll, VmError> {
        if let Some(exit) = self.heap.finalizers.exit
            && exit.phase == crate::heap::ExitPhase::Terminal
        {
            return Ok(Poll::Stop(StepOutcome::ExitRequested {
                status: exit.status,
                close: exit.close,
            }));
        }
        if let Some(reason) = self.trap {
            return Ok(Poll::Stop(StepOutcome::Terminated(reason)));
        }
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let status = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .status;
        match status {
            Status::Waiting => {
                let key = self.wait_key(active)?.ok_or(VmError::Corrupt)?;
                return Ok(Poll::Stop(StepOutcome::Waiting(WaitKey(key))));
            }
            Status::Completed => return Ok(Poll::Stop(StepOutcome::Completed)),
            Status::Failed => {
                let fault = self
                    .heap
                    .threads
                    .get(active)
                    .and_then(|thread| thread.error)
                    .map(|(fault, _)| fault)
                    .ok_or(VmError::Corrupt)?;
                return Ok(Poll::Stop(StepOutcome::LuaError(fault)));
            }
            Status::LuaSuspended => return Err(VmError::NotRunnable),
            Status::Ready => {}
        }
        // Collector work comes before anything Lua does next (ADR 0050).
        self.gc_schedule();
        if self.gc_wanted() {
            return self.gc_slice(quantum);
        }
        // An error being unwound comes before anything the failed frames
        // were waiting to finish.
        if self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .unwind
            .is_some()
        {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            return self.unwind_step(active, journal);
        }
        if self.hook_trap
            && let Some(step) = self.poll_hook(quantum, journal)?
        {
            return Ok(step);
        }
        let deferred = matches!(
            self.heap
                .threads
                .get(active)
                .and_then(|thread| thread.frames.last())
                .and_then(|frame| frame.pending()),
            Some(Pending::Deferred)
        );
        if deferred {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            if self
                .fuel_limit
                .is_some_and(|limit| self.fuel_consumed >= limit)
            {
                self.trap = Some(TerminationReason::FuelLimitExceeded);
                return Ok(Poll::Stop(StepOutcome::Terminated(
                    TerminationReason::FuelLimitExceeded,
                )));
            }
            self.fuel_consumed += 1;
            *quantum -= 1;
            return self
                .run_deferred(active, journal)
                .or_else(|error| self.vm_error(error));
        }
        // A queued finalizer starts before the interrupted code goes on
        // (ADR 0048). Starting one is a call, and costs a unit.
        if self.finalizer_due(active) {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            if self
                .fuel_limit
                .is_some_and(|limit| self.fuel_consumed >= limit)
            {
                self.trap = Some(TerminationReason::FuelLimitExceeded);
                return Ok(Poll::Stop(StepOutcome::Terminated(
                    TerminationReason::FuelLimitExceeded,
                )));
            }
            self.fuel_consumed += 1;
            *quantum -= 1;
            return self
                .start_finalizer(active, journal)
                .or_else(|error| self.vm_error(error));
        }
        if let Some(ready) = self.boundary_ready(active) {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            // A base function's step may call again, and a native it calls
            // returns at once, so each step costs a unit, as an
            // instruction does; otherwise a native `load` reader would run
            // without end inside one quantum (ADR 0031).
            if matches!(ready, Ready::Builtin | Ready::Native) {
                if self
                    .fuel_limit
                    .is_some_and(|limit| self.fuel_consumed >= limit)
                {
                    self.trap = Some(TerminationReason::FuelLimitExceeded);
                    return Ok(Poll::Stop(StepOutcome::Terminated(
                        TerminationReason::FuelLimitExceeded,
                    )));
                }
                self.fuel_consumed += 1;
                *quantum -= 1;
            }
            let step = match ready {
                Ready::Hook => self.finish_hook(active),
                Ready::HookNative => self.step_hook_native(active, journal),
                Ready::Protect => self.finish_protect(active, None),
                Ready::Handler => self.finish_handler(active),
                Ready::Builtin => self.finish_builtin(active, journal),
                Ready::Native => self.finish_native(active, None, journal),
                Ready::Finalizer => self.finish_finalizer(active, None, journal),
            };
            return step.or_else(|error| self.vm_error(error));
        }
        if self.meta_ready() {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            // A close's commit goes straight on to the next close.
            if self.commit_meta(active)? {
                return self
                    .close_step(active, journal)
                    .or_else(|error| self.vm_error(error));
            }
            return Ok(Poll::Continue);
        }
        if self.close_ready(active) {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            return self
                .close_step(active, journal)
                .or_else(|error| self.vm_error(error));
        }
        if self.at_prepared().is_some() {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            // Capability continuations use the cold prepared-call path. Keep
            // boundary_ready unchanged for ordinary library hot resumes, and
            // consume a completed work step without charging it a second time.
            if self.capability_builtin_ready(active) {
                return self
                    .finish_builtin(active, journal)
                    .or_else(|error| self.vm_error(error));
            }
            if let Err(error) = self.perform_host(journal) {
                return self.vm_error(error);
            }
            let status = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .status;
            if status == Status::Waiting {
                let key = self.wait_key(active)?.ok_or(VmError::Corrupt)?;
                return Ok(Poll::Stop(StepOutcome::Waiting(WaitKey(key))));
            }
            return Ok(Poll::Continue);
        }
        if self.assign_remaining().is_some() {
            if *quantum == 0 {
                return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
            }
            match self.perform_assign_store(journal)? {
                Poll::Continue => {
                    *quantum -= 1;
                    return Ok(Poll::Continue);
                }
                stop => return Ok(stop),
            }
        }
        if *quantum == 0 {
            return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
        }
        if self
            .fuel_limit
            .is_some_and(|limit| self.fuel_consumed >= limit)
        {
            self.trap = Some(TerminationReason::FuelLimitExceeded);
            return Ok(Poll::Stop(StepOutcome::Terminated(
                TerminationReason::FuelLimitExceeded,
            )));
        }
        if self.hook_trap
            && let Some(step) = self.trace_hook_instruction(quantum, journal)?
        {
            return Ok(step);
        }
        self.fuel_consumed += 1;
        *quantum -= 1;
        let op = self.current_op()?;
        #[cfg(feature = "counters")]
        crate::counters::opcode(op);
        self.cold_steps = self.cold_steps.wrapping_add(1);
        let result = self.exec(op, journal).or_else(|error| self.vm_error(error));
        if self.hook_trap
            && let Some(hook) = self
                .heap
                .threads
                .get_mut(active)
                .and_then(|t| self.heap.hooks.get_mut(t.id))
        {
            hook.instruction = None;
        }
        result
    }

    /// The interpreter slice. Runs instructions of the active thread until
    /// the quantum or the fuel limit runs out, an instruction stops, or the
    /// thread reaches a state only `poll` handles (a trap, a pending host
    /// call or assignment, a status other than `Ready`). `None` hands the
    /// rest to `poll`; `Some` is the outcome to return.
    ///
    /// Hot opcodes run from locals: `pc`, the frame base, the prototype's
    /// code, fuel, and the quantum. Any other opcode is charged, the locals
    /// are written back to the frame, and the out-of-line `exec` runs it
    /// against canonical state; the locals are then rebuilt, because it may
    /// have pushed or popped a frame or switched threads. Charging and order
    /// match `poll`: one unit per instruction, before it executes, including
    /// one that faults.
    ///
    /// Out of line, and `exec` is too, so a cold handler's size cannot
    /// change how the hot loop is compiled.
    #[inline(never)]
    fn run_hot(
        &mut self,
        quantum: &mut u64,
        journal: &mut Journal,
    ) -> Result<Option<StepOutcome>, VmError> {
        loop {
            // Between steps. Collector work and a finalizer due to start
            // go through `poll`; a finalizer running runs here like any
            // code.
            self.gc_schedule();
            if self.trap.is_some()
                || self.hook_trap
                || self.gc_wanted()
                || (!self.heap.finalizers.pending.is_empty() && !self.heap.finalizers.running)
            {
                return Ok(None);
            }
            // An epoch input, dead before opcode dispatch: keeping it live
            // across epochs displaced the register-window length in the loop.
            let limit = self.fuel_limit.unwrap_or(u64::MAX);
            let allow0 = (*quantum).min(limit.saturating_sub(self.fuel_consumed));
            let mut allow = allow0;
            let active = self.heap.active.ok_or(VmError::Corrupt)?;
            // Disjoint borrows for the epoch: the thread, the arenas the
            // windows read, and the frame helpers' state, built once.
            let Heap {
                threads,
                tables,
                strings,
                closures,
                protos,
                upvalues,
                gc,
                ..
            } = &mut self.heap;
            let thread = threads.get_mut(active).ok_or(VmError::Corrupt)?;
            if thread.status != Status::Ready || thread.unwind.is_some() {
                return Ok(None);
            }
            let closures: &Arena<ClosureObj> = closures;
            let protos: &Arena<Proto> = protos;
            let mut frame_heap = FrameHeap {
                closures,
                protos,
                gc,
                ordinary: self.max_stack_slots - self.max_stack_slots / 8,
                stack_grows: &mut self.stack_grows,
                frame_grows: &mut self.frame_grows,
                cold_spare: &mut self.cold_spare,
            };
            // The frame a helper made the running one; read only after the
            // helper returns `Continue`.
            let mut switch: Switch<'_>;
            let end = 'epoch: loop {
                // Validation: on epoch entry and after a transition no helper
                // described (a metamethod commit, a boundary on top).
                let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
                // One null test on the common path. A box holding only
                // assignment targets stays hot: `a, b = b, a + b` records
                // its targets before its values are computed.
                if let Some(cold) = &frame.cold
                    && (cold.pending.is_some() || cold.meta.is_some() || cold.boundary.is_some())
                {
                    break SliceEnd::Budget(matches!(
                        frame.boundary(),
                        Some(Boundary::Builtin { .. } | Boundary::Handler { .. })
                    ));
                }
                let closure = closures.get(frame.closure).ok_or(VmError::Corrupt)?;
                switch = Switch {
                    closure,
                    proto: protos.get(closure.proto).ok_or(VmError::Corrupt)?,
                    base: frame.base,
                    pc: frame.pc,
                };
                // A short restored stack must keep nil-on-miss and write-only
                // growth; `poll` executes it without changing checkpoint shape.
                // Every helper transition preserves `limit <= len` (RFC2 §3).
                if thread.stack.len < frame.limit as usize {
                    break SliceEnd::Budget(false);
                }
                let mut hot = HotHeap {
                    tables: &mut *tables,
                    strings,
                    proto: switch.proto,
                };
                // A Lua frame's limit is `base + max_reg` (builders, restore
                // validation), so the window lies within the logical length.
                debug_assert!(
                    switch.base as usize + usize::from(switch.proto.max_reg) <= thread.stack.len
                );
                let (below, rest) = thread.stack.values.split_at_mut(switch.base as usize);
                let mut ops = switch.proto.ops.as_slice();
                let mut core = HotCore {
                    regs: &mut rest[..usize::from(switch.proto.max_reg)],
                    pc: switch.pc,
                    heap: &mut hot,
                    upvalues: Some(HotUpvalues {
                        closure: switch.closure,
                        upvalues: &mut *upvalues,
                        active: &active,
                        below,
                    }),
                };
                let end = loop {
                    if allow == 0 {
                        break SliceEnd::Budget(false);
                    }
                    let Some(op) = ops.get(core.pc as usize) else {
                        break SliceEnd::Broken(VmError::Corrupt);
                    };
                    allow -= 1;
                    #[cfg(feature = "counters")]
                    crate::counters::opcode(*op);
                    count!("dispatch_hot_attempts");
                    #[cfg(any(test, debug_assertions))]
                    if self.hot_core == HotCoreMode::Off
                        || (self.hot_core == HotCoreMode::NoFastCalls
                            && matches!(
                                op,
                                Op::Call { .. } | Op::Return { .. } | Op::TailCall { .. }
                            ))
                    {
                        break SliceEnd::Cold(op);
                    }
                    match hot_op(&mut core, *op) {
                        Step::Continue => {
                            count!("dispatch_hot");
                            continue;
                        }
                        Step::Cold => break SliceEnd::Cold(op),
                        Step::Fault(fault) => {
                            count!("dispatch_hot");
                            break SliceEnd::Fault(fault);
                        }
                        Step::Frame => {}
                    }
                    // A Lua call, tail call or return. The arm already holds
                    // the decoded operands; the helper publishes `pc` to the
                    // frame, so `core` is dead across it.
                    let pc = core.pc;
                    let step = match *op {
                        Op::Call {
                            func,
                            nargs,
                            nresults,
                        } => fast_call(
                            thread,
                            &mut frame_heap,
                            pc,
                            func,
                            nargs,
                            nresults,
                            &mut switch,
                        ),
                        Op::Return { base, count } => {
                            fast_return(thread, &frame_heap, pc, base, count, &mut switch)
                        }
                        Op::TailCall { func, nargs } => {
                            fast_tail_call(thread, &mut frame_heap, pc, func, nargs, &mut switch)
                        }
                        _ => break SliceEnd::Cold(op),
                    };
                    match step {
                        FrameStep::Continue => {
                            count!("dispatch_hot");
                            // The frame switch, inside the instruction loop:
                            // the helper proved the callee or caller window
                            // (resolved closure and prototype, `limit <= len`,
                            // no cold state), so only the slices are re-cut.
                            let (below, rest) =
                                thread.stack.values.split_at_mut(switch.base as usize);
                            hot.proto = switch.proto;
                            ops = switch.proto.ops.as_slice();
                            core = HotCore {
                                regs: &mut rest[..usize::from(switch.proto.max_reg)],
                                pc: switch.pc,
                                heap: &mut hot,
                                upvalues: Some(HotUpvalues {
                                    closure: switch.closure,
                                    upvalues: &mut *upvalues,
                                    active: &active,
                                    below,
                                }),
                            };
                        }
                        FrameStep::Meta => {
                            count!("dispatch_hot");
                            // Uncharged, so only with an allowance left, as before.
                            if allow != 0 {
                                fast_commit_meta(
                                    thread,
                                    frame_heap.closures,
                                    frame_heap.protos,
                                    frame_heap.cold_spare,
                                );
                            }
                            continue 'epoch;
                        }
                        FrameStep::Revalidate => {
                            count!("dispatch_hot");
                            continue 'epoch;
                        }
                        FrameStep::Resync => {
                            count!("dispatch_hot");
                            break 'epoch SliceEnd::Budget(false);
                        }
                        FrameStep::Cold => break 'epoch SliceEnd::Cold(op),
                    }
                };
                // Exits from the instruction loop itself: publish its pc.
                let pc = core.pc;
                thread.frames.last_mut().ok_or(VmError::Corrupt)?.pc = pc;
                break end;
            };
            let used = allow0 - allow;
            let cold = match end {
                SliceEnd::Cold(op) => *op,
                SliceEnd::Budget(boundary) => {
                    self.fuel_consumed += used;
                    *quantum -= used;
                    if boundary && let Some(step) = self.finish_hot_boundary(quantum, journal)? {
                        if let Poll::Stop(outcome) = step {
                            return Ok(Some(outcome));
                        }
                        continue;
                    }
                    return Ok(None);
                }
                SliceEnd::Broken(error) => {
                    self.fuel_consumed += used;
                    *quantum -= used;
                    return Err(error);
                }
                SliceEnd::Fault(fault) => {
                    self.fuel_consumed += used;
                    *quantum -= used;
                    self.fault(fault);
                    continue;
                }
            };
            self.fuel_consumed += used;
            *quantum -= used;
            self.cold_steps = self.cold_steps.wrapping_add(1);
            let step = match self.exec(cold, journal) {
                Ok(step) => step,
                Err(error) => self.vm_error(error)?,
            };
            if let Poll::Stop(outcome) = step {
                return Ok(Some(outcome));
            }
        }
    }

    fn wait_key(&self, thread: Handle<ThreadObj>) -> Result<Option<u64>, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let Some(frame) = object.frames.last() else {
            return Ok(None);
        };
        if let Some(MetaCall {
            phase: MetaPhase::NativeWaiting { wait_key, .. },
            ..
        }) = frame.meta()
        {
            return Ok(Some(*wait_key));
        }
        Ok(match frame.pending() {
            Some(
                Pending::Waiting { wait_key, .. }
                | Pending::NativeWaiting { wait_key, .. }
                | Pending::Capability {
                    wait_key,
                    completed: false,
                    ..
                },
            ) => Some(*wait_key),
            _ => None,
        })
    }

    fn perform_host(&mut self, journal: &mut Journal) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        if self
            .heap
            .threads
            .get(active)
            .and_then(|t| t.frames.last())
            .is_some_and(|f| {
                matches!(
                    f.pending(),
                    Some(Pending::Capability {
                        completed: true,
                        ..
                    })
                )
            })
        {
            let (_, _, _, callee) = self.call_site(active)?;
            let Value::Native(index) = self.callable(callee) else {
                return Err(VmError::Corrupt);
            };
            let slot = *self
                .native_slots
                .get(index as usize)
                .ok_or(VmError::Corrupt)?;
            let builtin = self
                .registry
                .native(slot)
                .and_then(|entry| entry.builtin)
                .ok_or(VmError::Corrupt)?;
            // The original instruction/work step was already charged before it
            // waited. Consume its completion without charging that step again.
            self.call_builtin(builtin, None, journal)?;
            return Ok(());
        }

        let native_sequence = {
            let frame = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .frames
                .last()
                .ok_or(VmError::Corrupt)?;
            match (frame.meta(), frame.pending()) {
                (
                    Some(MetaCall {
                        phase: MetaPhase::NativePrepared { sequence },
                        ..
                    }),
                    _,
                ) => Some(*sequence),
                (None, Some(Pending::NativePrepared { sequence })) => Some(*sequence),
                _ => None,
            }
        };
        if let Some(sequence) = native_sequence {
            let effect = EffectId {
                domain: self.effect_domain,
                sequence,
            };
            // A fault ends the thread; `poll` reports it on the next step.
            let resuming = self
                .heap
                .threads
                .get(active)
                .and_then(|t| t.frames.last())
                .is_some_and(|f| {
                    matches!(f.boundary(), Some(Boundary::Native { resuming: true, .. }))
                });
            let _ = if resuming {
                self.finish_native(active, Some(effect), journal)?
            } else {
                self.run_native(active, Some(effect), journal)?
            };
            return Ok(());
        }
        let (sequence, symbol, arg, dest, base) = {
            let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = object.frames.last().ok_or(VmError::Corrupt)?;
            match frame.pending() {
                Some(Pending::Prepared {
                    sequence,
                    symbol,
                    arg,
                    dest,
                }) => (*sequence, symbol.clone(), *arg, *dest, frame.base),
                _ => return Err(VmError::Corrupt),
            }
        };
        let effect = EffectId {
            domain: self.effect_domain,
            sequence,
        };
        let result = {
            count!("lua_to_host_calls");
            let mut ctx = HostCtx { journal, effect };
            self.registry
                .call(&symbol, &mut ctx, arg)
                .ok_or(VmError::Corrupt)?
        };
        match result {
            HostResult::Fault => return Err(VmError::Corrupt),
            HostResult::Ready(value) => {
                self.write_abs(active, base + u32::from(dest), Value::Integer(value))?;
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                frame.set_pending(None, &mut self.cold_spare);
                frame.pc = frame.pc.saturating_add(1);
                object.status = Status::Ready;
            }
            HostResult::Pending(key) => {
                if self.wait_index.contains_key(&key.raw())
                    || self.completed_waits.contains(&key.raw())
                {
                    return Err(VmError::Api(crate::ApiError::InvalidCallState));
                }
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                frame.set_pending(
                    Some(Pending::Waiting {
                        sequence,
                        symbol,
                        arg,
                        dest,
                        wait_key: key.raw(),
                    }),
                    &mut self.cold_spare,
                );
                count!("host_waits");
                object.status = Status::Waiting;
                self.index_wait(key.raw(), active)?;
                // A new wait: completing the previous one again is no longer
                // a duplicate to guard against, and its key may repeat.
                self.last_completed_wait = None;
            }
        }
        Ok(())
    }

    /// Execute `op`, the instruction at the frame's `pc`, against canonical
    /// state. Fuel is already charged.
    ///
    /// Only a dispatcher (ADR 0022). The operations ordinary code runs often
    /// that the hot tier does not finish each have an out-of-line handler:
    /// calls and returns, table misses and inserts, metamethod calls, `#`,
    /// constructors, string constants, closures and upvalues, comparisons,
    /// negation, and loop setup. Their machine code then does not depend on
    /// what else the instruction set contains. Every other opcode goes to
    /// `exec_rare`, which can grow without moving them.
    #[inline(never)]
    fn exec(&mut self, op: Op, journal: &mut Journal) -> Result<Poll, VmError> {
        count!("dispatch_common_entries");
        match op {
            Op::Call {
                func,
                nargs,
                nresults,
            } => self.exec_call(func, nargs, nresults, journal),
            Op::Return { base, count } => self.do_return(base, count, journal),
            Op::Index { dst, obj, key } => self.op_index(dst, obj, key, journal),
            Op::GetField { dst, obj, name } => self.op_get_field(dst, obj, name, journal),
            Op::SetIndex { obj, key, src } => self.op_set_index(obj, key, src, journal),
            Op::SetField { obj, name, src } => self.op_set_field(obj, name, src, journal),
            Op::Len { dst, src } => self.op_len(dst, src, journal),
            Op::LoadBytes { dst, const_index } => self.op_load_bytes(dst, const_index),
            Op::NewTable { dst } => self.op_new_table(dst),
            Op::SetList { table, src, start } => self.op_set_list(table, src, start),
            Op::MakeClosure { dst, child } => self.op_make_closure(dst, child),
            Op::GetUpvalue { dst, index } => self.op_get_upvalue(dst, index),
            Op::SetUpvalue { index, src } => self.op_set_upvalue(index, src),
            Op::CloseUpvalues { from } => self.op_close_upvalues(from),
            Op::Add { dst, a, b } => {
                self.op_arith(ArithOp::Add, dst, self.load(a)?, self.load(b)?, journal)
            }
            Op::Arith { op, dst, a, b } => {
                self.op_arith(op, dst, self.load(a)?, self.load(b)?, journal)
            }
            Op::ArithK {
                op,
                dst,
                reg,
                constant,
                reverse,
            } => {
                let (a, b) = arithk_operands(self.load(reg)?, constant, reverse);
                self.op_arith(op, dst, a, b, journal)
            }
            Op::Compare { kind, dst, a, b } => self.op_compare(kind, dst, a, b, journal),
            Op::CompareBranch { kind, a, b, .. } => {
                self.op_compare(kind, COUNT_OPEN, a, b, journal)
            }
            Op::Neg { dst, src } => self.op_unary(false, dst, src, journal),
            Op::BNot { dst, src } => self.op_unary(true, dst, src, journal),
            Op::Concat { dst, a, b } => self.op_concat(dst, a, b, journal),
            Op::ForPrep { base, offset } => self.op_for_prep(base, offset),
            _ => self.exec_rare(op, journal),
        }
    }

    /// Move past the finished instruction.
    fn next_op(&mut self) -> Result<Poll, VmError> {
        self.advance_pc()?;
        Ok(Poll::Continue)
    }

    #[inline(never)]
    fn op_index(
        &mut self,
        dst: u8,
        obj: u8,
        key: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (obj, key) = (self.load(obj)?, self.load(key)?);
        match index::get(&self.heap, obj, key) {
            Ok(Resolved::Done(value)) => self.store(dst, value)?,
            Ok(Resolved::Call { function, target }) => {
                return self.call_meta(MetaEvent::Store { dst }, function, &[target, key], journal);
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_get_field(
        &mut self,
        dst: u8,
        obj: u8,
        name: u32,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let obj = self.load(obj)?;
        let proto = self.current_proto()?;
        let proto = self.heap.protos.get(proto).ok_or(VmError::Corrupt)?;
        let bytes = proto
            .byte_consts
            .get(name as usize)
            .ok_or(VmError::Corrupt)?;
        let hash = *proto
            .byte_hashes
            .get(name as usize)
            .ok_or(VmError::Corrupt)?;
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let pc = self
            .heap
            .threads
            .get(active)
            .and_then(|thread| thread.frames.last())
            .ok_or(VmError::Corrupt)?
            .pc;
        let hint = proto.field_hints.get(pc as usize);
        #[cfg(any(test, debug_assertions))]
        let hint = hint.filter(|_| self.hot_core != HotCoreMode::Off);
        match index::get_name(&self.heap, obj, KeyView::cached_string(bytes, hash), hint) {
            Ok(Resolved::Done(value)) => self.store(dst, value)?,
            Ok(Resolved::Call { function, target }) => {
                let key = Value::String(self.const_string(name)?);
                return self.call_meta(MetaEvent::Store { dst }, function, &[target, key], journal);
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_set_index(
        &mut self,
        obj: u8,
        key: u8,
        src: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (obj, key, value) = (self.load(obj)?, self.load(key)?, self.load(src)?);
        self.set_resolved(obj, key, value, journal)
    }

    #[inline(never)]
    fn op_set_field(
        &mut self,
        obj: u8,
        name: u32,
        src: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        // The hot tier handled a live-key update; this path may insert,
        // delete, or call `__newindex`, so it needs the key as a string value.
        let (obj, value) = (self.load(obj)?, self.load(src)?);
        let key = Value::String(self.const_string(name)?);
        self.set_resolved(obj, key, value, journal)
    }

    /// The language-level store of `SetIndex` and `SetField`.
    fn set_resolved(
        &mut self,
        obj: Value,
        key: Value,
        value: Value,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        match self.set_value(obj, key, value) {
            Ok(Resolved::Done(_)) => self.next_op(),
            Ok(Resolved::Call { function, target }) => self.call_meta(
                MetaEvent::NewIndex,
                function,
                &[target, key, value],
                journal,
            ),
            Err(fault) => Ok(self.fault(fault)),
        }
    }

    #[inline(never)]
    fn op_len(&mut self, dst: u8, src: u8, journal: &mut Journal) -> Result<Poll, VmError> {
        let value = self.load(src)?;
        match index::len(&self.heap, value) {
            Ok(Resolved::Done(result)) => self.store(dst, result)?,
            Ok(Resolved::Call { function, target }) => {
                // Lua 5.4 passes a unary metamethod its operand twice.
                return self.call_meta(
                    MetaEvent::Store { dst },
                    function,
                    &[target, target],
                    journal,
                );
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_load_bytes(&mut self, dst: u8, const_index: u32) -> Result<Poll, VmError> {
        let handle = self.const_string(const_index)?;
        self.store(dst, Value::String(handle))?;
        self.next_op()
    }

    #[inline(never)]
    fn op_new_table(&mut self, dst: u8) -> Result<Poll, VmError> {
        self.make_room(1, cost::OBJECT);
        let handle = self.alloc_table()?;
        self.store(dst, Value::Table(handle))?;
        self.next_op()
    }

    #[inline(never)]
    fn op_set_list(&mut self, table: u8, src: u8, start: u32) -> Result<Poll, VmError> {
        let table = self.load(table)?;
        let count = u32::try_from(self.open_len(src)?).map_err(|_| VmError::Corrupt)?;
        for offset in 0..count {
            let reg = u32::from(src) + offset;
            let value = self.load_abs_offset(reg)?;
            let key = i64::from(start) + i64::from(offset);
            let Value::Table(handle) = table else {
                return Ok(self.fault(LuaFault::Type));
            };
            let insert = |heap: &mut Heap| {
                heap.table_insert(
                    handle,
                    crate::table::TableKey::Integer(key),
                    Value::Integer(key),
                    value,
                )
            };
            match insert(&mut self.heap) {
                Err(crate::heap::InsertError::Memory) if self.collect_for_store() => {
                    insert(&mut self.heap)
                }
                other => other,
            }
            .map_err(insert_error)?;
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_make_closure(&mut self, dst: u8, child: u32) -> Result<Poll, VmError> {
        let value = self.make_lua_closure(child)?;
        self.store(dst, value)?;
        self.next_op()
    }

    #[inline(never)]
    fn op_get_upvalue(&mut self, dst: u8, index: u8) -> Result<Poll, VmError> {
        let value = self.read_upvalue(index)?;
        self.store(dst, value)?;
        self.next_op()
    }

    #[inline(never)]
    fn op_set_upvalue(&mut self, index: u8, src: u8) -> Result<Poll, VmError> {
        let value = self.load(src)?;
        self.write_upvalue(index, value)?;
        self.next_op()
    }

    #[inline(never)]
    fn op_close_upvalues(&mut self, from: u8) -> Result<Poll, VmError> {
        self.close_upvalues(from)?;
        self.next_op()
    }

    /// Both register and immediate forms use the same coercions, errors,
    /// metamethod selection and VM-owned Store continuation.
    #[inline(never)]
    fn op_arith(
        &mut self,
        op: ArithOp,
        dst: u8,
        a: Value,
        b: Value,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        match ops::arith(&self.heap, op, a, b) {
            Ok(OpStep::Done(value)) => self.store(dst, value)?,
            Ok(OpStep::Call(handler)) => {
                return self.call_meta(MetaEvent::Store { dst }, handler, &[a, b], journal);
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    /// `Neg`, or `BNot` when `bitwise`. A metamethod gets the operand twice.
    #[inline(never)]
    fn op_unary(
        &mut self,
        bitwise: bool,
        dst: u8,
        src: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let value = self.load(src)?;
        let step = if bitwise {
            ops::bnot(&self.heap, value)
        } else {
            ops::negate(&self.heap, value)
        };
        match step {
            Ok(OpStep::Done(result)) => self.store(dst, result)?,
            Ok(OpStep::Call(handler)) => {
                return self.call_meta(MetaEvent::Store { dst }, handler, &[value, value], journal);
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_compare(
        &mut self,
        kind: CmpKind,
        dst: u8,
        a: u8,
        b: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (left, right) = (self.load(a)?, self.load(b)?);
        let negate = kind == CmpKind::Ne;
        match ops::compare(&self.heap, kind, left, right) {
            Ok(Truth::Done(bit)) => {
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                self.finish_truth(active, dst, bit != negate)?;
            }
            Ok(Truth::Call(handler)) => {
                return self.call_meta(
                    MetaEvent::Truth { dst, negate },
                    handler,
                    &[left, right],
                    journal,
                );
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        Ok(Poll::Continue)
    }

    #[inline(never)]
    fn op_concat(&mut self, dst: u8, a: u8, b: u8, journal: &mut Journal) -> Result<Poll, VmError> {
        let (a, b) = (self.load(a)?, self.load(b)?);
        let mut joined = ops::concat(&self.heap, a, b);
        let mut roomed = false;
        if let Ok(Joined::NoRoom(len)) = joined {
            // Collect before the bytes are copied, then look again: the
            // operands sit in registers.
            self.make_room(1, cost::OBJECT + len as u64);
            roomed = true;
            joined = ops::concat(&self.heap, a, b);
        }
        match joined {
            Ok(Joined::TooLong | Joined::NoRoom(_)) => return Err(VmError::MemoryLimit),
            Ok(Joined::Bytes(bytes)) => {
                // Nothing but the owned bytes is held here.
                if !roomed {
                    self.make_room(1, cost::OBJECT + bytes.len() as u64);
                }
                let handle = self.alloc_string(bytes)?;
                self.store(dst, Value::String(handle))?;
            }
            Ok(Joined::Call(handler)) => {
                return self.call_meta(MetaEvent::Store { dst }, handler, &[a, b], journal);
            }
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    #[inline(never)]
    fn op_for_prep(&mut self, base: u8, offset: i32) -> Result<Poll, VmError> {
        let init = self.load(base)?;
        let limit = self.load(base.checked_add(1).ok_or(VmError::Corrupt)?)?;
        let step = self.load(base.checked_add(2).ok_or(VmError::Corrupt)?)?;
        match fornum::prepare(&self.heap, init, limit, step) {
            Ok(fornum::Prep::Skip) => {
                self.jump(offset)?;
                return Ok(Poll::Continue);
            }
            Ok(fornum::Prep::Run(state)) => self.store_for(base, state)?,
            Err(fault) => return Ok(self.fault(fault)),
        }
        self.next_op()
    }

    /// Opcodes outside the common set: hot opcodes reached from `poll`
    /// (for example after a pending assignment), raw table access,
    /// coroutines, host calls, varargs, parallel assignment, traversal.
    #[inline(never)]
    fn exec_rare(&mut self, op: Op, journal: &mut Journal) -> Result<Poll, VmError> {
        count!("dispatch_rare");
        match op {
            Op::LoadNil { .. }
            | Op::LoadInt { .. }
            | Op::LoadFloat { .. }
            | Op::LoadBool { .. }
            | Op::Move { .. }
            | Op::Jump { .. }
            | Op::JumpIfFalse { .. }
            | Op::JumpIfLt { .. }
            | Op::ForLoop { .. } => {
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                let proto = self.current_proto()?;
                let heap = &mut self.heap;
                let code = heap.protos.get(proto).ok_or(VmError::Corrupt)?;
                let thread = heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let ThreadObj { stack, frames, .. } = thread;
                let frame = frames.last_mut().ok_or(VmError::Corrupt)?;
                let mut hot = HotHeap {
                    tables: &mut heap.tables,
                    strings: &heap.strings,
                    proto: code,
                };
                let step = if stack.len() < frame.limit as usize {
                    short_hot(
                        op,
                        stack,
                        frame.base as usize,
                        usize::from(code.max_reg),
                        &mut frame.pc,
                        hot,
                    )
                } else {
                    let (_, rest) = stack.split_at_mut(frame.base as usize);
                    let mut core = HotCore {
                        regs: &mut rest[..usize::from(code.max_reg)],
                        pc: frame.pc,
                        heap: &mut hot,
                        upvalues: None,
                    };
                    let step = hot_op(&mut core, op);
                    frame.pc = core.pc;
                    step
                };
                match step {
                    Step::Continue => {
                        count!("rare_hot_helper");
                        return Ok(Poll::Continue);
                    }
                    Step::Fault(fault) => {
                        count!("rare_hot_helper");
                        return Ok(self.fault(fault));
                    }
                    Step::Cold if matches!(op, Op::ForLoop { .. }) => {
                        return Ok(self.fault_text(
                            LuaFault::Type,
                            b"'for' loop state is not a number".to_vec(),
                        ));
                    }
                    Step::Cold | Step::Frame => return Err(VmError::Corrupt),
                }
            }
            Op::GetTable { dst, table, key } => {
                let table_value = self.load(table)?;
                let key_value = self.load(key)?;
                let Value::Table(table_handle) = table_value else {
                    return Ok(self.fault(LuaFault::Type));
                };
                let normalized = match self.heap.key_view(key_value) {
                    Ok(key) => key,
                    Err(fault) => return Ok(self.fault(fault)),
                };
                let value = self
                    .heap
                    .table_get_view(table_handle, normalized)
                    .ok_or(VmError::Corrupt)?;
                self.store(dst, value)?;
            }
            Op::SetTable { table, key, src } => {
                let table_value = self.load(table)?;
                let key_value = self.load(key)?;
                let source = self.load(src)?;
                let Value::Table(table_handle) = table_value else {
                    return Ok(self.fault(LuaFault::Type));
                };
                if self.heap.update_string_key(table_handle, key_value, source) {
                    return self.next_op();
                }
                let normalized = match self.heap.normalize_value(key_value) {
                    Ok(key) => key,
                    Err(fault) => return Ok(self.fault(fault)),
                };
                let mut stored =
                    self.heap
                        .table_insert(table_handle, normalized, key_value, source);
                if stored == Err(crate::heap::InsertError::Memory) && self.collect_for_store() {
                    let normalized = self
                        .heap
                        .normalize_value(key_value)
                        .map_err(|_| VmError::Corrupt)?;
                    stored = self
                        .heap
                        .table_insert(table_handle, normalized, key_value, source);
                }
                stored.map_err(insert_error)?;
            }
            Op::Yield { base, count } => return self.do_yield(base, count),
            Op::CallHost { dst, symbol, arg } => {
                let name = self.const_symbol(u32::from(symbol))?;
                if !self.registry.contains(&name) {
                    return Ok(self.fault(LuaFault::UnboundSymbol));
                }
                let arg_value = self.load(arg)?;
                let Value::Integer(arg_int) = arg_value else {
                    return Ok(self.fault(LuaFault::Type));
                };
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                self.set_pending(Pending::Prepared {
                    sequence,
                    symbol: name,
                    arg: arg_int,
                    dest: dst,
                })?;
                return Ok(Poll::Continue);
            }
            Op::NewThread { dst, child } => {
                let value = self.new_thread(child)?;
                self.store(dst, value)?;
            }
            Op::Resume {
                dest,
                thread,
                nresults,
            } => return self.do_resume(dest, thread, nresults),
            Op::MarkClose { reg } => return self.mark_close(reg),
            Op::CloseScope { from } => return self.close_scope(from, journal),
            Op::CloseThread { dst, thread } => return self.close_thread(dst, thread),
            Op::GenericForLoop { base, offset } => return self.generic_for_loop(base, offset),
            Op::TailCall { func, nargs } => return self.tail_call(func, nargs, journal),
            Op::VarargLen { dst } => {
                let len = i64::from(self.vararg_len()?);
                self.store(dst, Value::Integer(len))?;
            }
            Op::Vararg { dst, count } => return self.copy_varargs(dst, count),
            Op::OpenLen { dst, from } => {
                let len = self.open_len(from)?;
                self.store(dst, Value::Integer(len))?;
            }
            Op::AssignLocal { reg } => self.push_local_target(reg)?,
            Op::AssignField { table, key } => self.push_field_target(table, key)?,
            Op::AssignCommit { src, n } => {
                self.begin_assign(src, n)?;
                return Ok(Poll::Continue);
            }
            Op::Next { dst, table, key } => return self.exec_next(dst, table, key),
            Op::RawLen { dst, table } => return self.exec_raw_len(dst, table),
            Op::GetGlobal { dst } => {
                let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
                self.store(dst, Value::Table(globals))?;
            }
            Op::Halt => {
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                self.heap
                    .threads
                    .get_mut(active)
                    .ok_or(VmError::Corrupt)?
                    .status = Status::Completed;
                return Ok(Poll::Stop(StepOutcome::Completed));
            }
            // `exec` routes these to their own handlers.
            Op::Call { .. }
            | Op::Return { .. }
            | Op::Index { .. }
            | Op::GetField { .. }
            | Op::SetIndex { .. }
            | Op::SetField { .. }
            | Op::Len { .. }
            | Op::LoadBytes { .. }
            | Op::NewTable { .. }
            | Op::SetList { .. }
            | Op::MakeClosure { .. }
            | Op::GetUpvalue { .. }
            | Op::SetUpvalue { .. }
            | Op::CloseUpvalues { .. }
            | Op::Compare { .. }
            | Op::CompareBranch { .. }
            | Op::Neg { .. }
            | Op::ForPrep { .. }
            | Op::Add { .. }
            | Op::Arith { .. }
            | Op::ArithK { .. }
            | Op::BNot { .. }
            | Op::Concat { .. } => return Err(VmError::Corrupt),
        }
        self.advance_pc()?;
        let _ = journal;
        Ok(Poll::Continue)
    }

    fn exec_next(&mut self, dst: u8, table: u8, key: u8) -> Result<Poll, VmError> {
        let value_reg = dst.checked_add(1).ok_or(VmError::Corrupt)?;
        let table_value = self.load(table)?;
        let key_value = self.load(key)?;
        let Value::Table(table_handle) = table_value else {
            return Ok(self.fault(LuaFault::Type));
        };
        let normalized = if matches!(key_value, Value::Nil) {
            None
        } else {
            match self.heap.key_view(key_value) {
                Ok(key) => Some(key),
                Err(LuaFault::NanKey | LuaFault::NilKey) => {
                    return Ok(self.fault(LuaFault::NextKey));
                }
                Err(fault) => return Ok(self.fault(fault)),
            }
        };
        let found = {
            let object = self.heap.tables.get(table_handle).ok_or(VmError::Corrupt)?;
            object.table.next_view(normalized)
        };
        let pair = match found {
            Ok(pair) => pair,
            Err(()) => return Ok(self.fault(LuaFault::NextKey)),
        };
        match pair {
            Some((next_key, next_value)) => {
                self.store(dst, next_key)?;
                self.store(value_reg, next_value)?;
            }
            None => {
                self.store(dst, Value::Nil)?;
                self.store(value_reg, Value::Nil)?;
            }
        }
        self.advance_pc()?;
        Ok(Poll::Continue)
    }

    fn exec_raw_len(&mut self, dst: u8, table: u8) -> Result<Poll, VmError> {
        let table_value = self.load(table)?;
        let Value::Table(table_handle) = table_value else {
            return Ok(self.fault(LuaFault::Type));
        };
        let border = self
            .heap
            .tables
            .get(table_handle)
            .ok_or(VmError::Corrupt)?
            .table
            .raw_border();
        self.store(dst, Value::Integer(border))?;
        self.advance_pc()?;
        Ok(Poll::Continue)
    }

    /// Raise a runtime error of class `fault` in the active thread; its
    /// error object is the class's reserved string (ADR 0024).
    fn fault(&mut self, fault: LuaFault) -> Poll {
        match self.heap.active {
            Some(active) => {
                let error = self.diagnostic_fault(fault, active);
                self.throw_on(active, fault, error)
            }
            None => Poll::Continue,
        }
    }

    /// Raise `fault` with `text` as its message, as Lua's library
    /// functions word theirs (ADR 0034). Without room for the text, the
    /// class's reserved message.
    fn fault_text(&mut self, fault: LuaFault, text: Vec<u8>) -> Poll {
        let Some(active) = self.heap.active else {
            return Poll::Continue;
        };
        let message = if matches!(fault, LuaFault::Type | LuaFault::BadCall | LuaFault::Arith) {
            let location = self.nearest_lua_location(active);
            self.prefixed(location, text, fault)
        } else {
            self.alloc_string(text)
                .map_or_else(|_| self.fault_value(fault), Value::String)
        };
        self.throw_on(active, fault, message)
    }

    /// The reserved error object of a class. Nothing is allocated.
    fn fault_value(&self, fault: LuaFault) -> Value {
        self.heap
            .reserved
            .get(usize::from(fault.tag()))
            .map_or(Value::Nil, |handle| Value::String(*handle))
    }

    /// Start unwinding `thread` with an error. The failing instruction stops
    /// where it is; `poll` carries the error to a protected call or out of
    /// the thread in later steps. The thread must not already be unwinding.
    fn throw_on(&mut self, thread: Handle<ThreadObj>, fault: LuaFault, error: Value) -> Poll {
        if let Some(object) = self.heap.threads.get_mut(thread) {
            object.status = Status::Ready;
            object.unwind = Some(Box::new(crate::heap::Unwind {
                error: Some((fault, error)),
                phase: UnwindPhase::Raised,
            }));
        }
        Poll::Continue
    }

    /// One step of the unwind in `thread` (ADR 0024, ADR 0026). Not charged.
    ///
    /// `Raised`: find the nearest boundary frame. An `xpcall` whose error
    /// may be handled gets its message handler called now, above the failing
    /// frames; an error escaping a message handler calls it again, up to
    /// [`MAX_HANDLER_DEPTH`]. Otherwise the target is the nearest `Protect`
    /// frame. With none, a coroutine fails where it stands, its stack and
    /// to-be-closed values kept, and any other thread unwinds to its bottom.
    /// While `CloseThread` closes the thread, the search stops at the frame
    /// being closed, so the thread's own boundaries do not catch.
    ///
    /// `Popping`: the top frame first closes its open upvalues and its
    /// to-be-closed values, each a call given the error, while the unwind
    /// waits in the frame's `Close` event. Then the frame is removed with its
    /// pending call, metamethod call, and assignment. With the target on
    /// top, finish the protected call with `false, error`; with no target,
    /// the thread has failed, or its close is done.
    fn unwind_step(
        &mut self,
        thread: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let unwind = **object.unwind.as_ref().ok_or(VmError::Corrupt)?;
        match unwind.phase {
            UnwindPhase::Raised => {
                let (fault, error) = unwind.error.ok_or(VmError::Corrupt)?;
                let closing = object.closing || self.exit_closing_scopes(thread);
                let mut nearest = None;
                for (index, frame) in object.frames.iter().enumerate().rev() {
                    if closing && closes_thread(frame) {
                        break;
                    }
                    // A base function's frame lets errors through, except
                    // `load`'s, which returns them (ADR 0031).
                    if let Some(boundary) = frame.boundary()
                        && (boundary.catches() || matches!(boundary, Boundary::Handler { .. }))
                    {
                        nearest = Some((index, boundary.clone()));
                        break;
                    }
                }
                let (fault, error, target) = match nearest {
                    None if object.coroutine && !closing => {
                        return self.fail_coroutine(thread, fault, error);
                    }
                    None => (fault, error, None),
                    // `load` keeps the message handler of the protected
                    // call it runs in: an `xpcall`'s handler runs first,
                    // and `load` returns what it gives (ADR 0031).
                    Some((index, Boundary::Builtin { .. })) => {
                        let enclosing = object.frames[..index].iter().enumerate().rev().find_map(
                            |(at, frame)| frame.boundary().filter(|b| b.catches()).map(|b| (at, b)),
                        );
                        if let Some((
                            protect,
                            Boundary::Protect {
                                handler: Some(handler),
                                ..
                            },
                        )) = enclosing
                            && handles(fault)
                        {
                            let handler = *handler;
                            return self.call_handler(
                                thread, protect, index, handler, 1, fault, error, journal,
                            );
                        }
                        (fault, error, Some(index))
                    }
                    Some((_, Boundary::Hook { .. } | Boundary::HookNative { .. })) => {
                        return Err(VmError::Corrupt);
                    }
                    // A finalizer's error stops at its frame, with no
                    // message handler: an `xpcall` below is not asked.
                    Some((index, Boundary::Native { .. } | Boundary::Finalizer { .. })) => {
                        (fault, error, Some(index))
                    }
                    Some((index, Boundary::Protect { handler, .. })) => {
                        if let Some(handler) = handler
                            && handles(fault)
                        {
                            return self.call_handler(
                                thread, index, index, handler, 1, fault, error, journal,
                            );
                        }
                        (fault, error, Some(index))
                    }
                    Some((
                        _,
                        Boundary::Handler {
                            protect,
                            target,
                            depth,
                            ..
                        },
                    )) => {
                        let owner = object
                            .frames
                            .get(protect as usize)
                            .and_then(|frame| frame.boundary());
                        let Some(Boundary::Protect {
                            handler: Some(handler),
                            ..
                        }) = owner.cloned()
                        else {
                            return Err(VmError::Corrupt);
                        };
                        if handles(fault) && u32::from(depth) < MAX_HANDLER_DEPTH {
                            return self.call_handler(
                                thread,
                                protect as usize,
                                target as usize,
                                handler,
                                depth + 1,
                                fault,
                                error,
                                journal,
                            );
                        }
                        if fault == LuaFault::Memory {
                            (fault, error, Some(target as usize))
                        } else {
                            let fault = LuaFault::ErrorHandling;
                            (fault, self.fault_value(fault), Some(target as usize))
                        }
                    }
                };
                let target = target
                    .map(|index| u32::try_from(index).map_err(|_| VmError::Corrupt))
                    .transpose()?;
                self.set_unwind(
                    thread,
                    Some((fault, error)),
                    UnwindPhase::Popping { target },
                )?;
                Ok(Poll::Continue)
            }
            UnwindPhase::Popping { target } => {
                let keep = target.map_or(0, |index| index as usize + 1);
                if object.frames.len() > keep {
                    return self.unwind_frame(thread, journal);
                }
                match target {
                    Some(_) => {
                        let (fault, error) = unwind.error.ok_or(VmError::Corrupt)?;
                        self.heap
                            .threads
                            .get_mut(thread)
                            .ok_or(VmError::Corrupt)?
                            .unwind = None;
                        // The frames that held the memory are gone. Collect
                        // now, so the code after `pcall`, natives included,
                        // finds the room it freed. The error object is a
                        // reserved string, a root.
                        if fault == LuaFault::Memory && self.heap.gc.auto && self.trap.is_none() {
                            self.emergency_collect();
                        }
                        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
                        let target = object.frames.last().and_then(|frame| frame.boundary());
                        if let Some(Boundary::Native { .. }) = target {
                            let object =
                                self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
                            let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                            frame.set_pending(None, &mut self.cold_spare);
                            if let Some(Boundary::Native {
                                error: caught,
                                func,
                                passed,
                                kept,
                                ..
                            }) = frame.boundary_mut()
                            {
                                *caught = Some((fault, error));
                                let slot = *func + 1 + *passed + *kept;
                                object.stack.truncate(slot as usize);
                                object.top = slot;
                            }
                            return Ok(Poll::Continue);
                        }
                        if let Some(Boundary::Builtin { .. }) = target {
                            return self.finish_load_error(thread, error);
                        }
                        if let Some(Boundary::Finalizer { .. }) = target {
                            return self.finish_finalizer(thread, Some(error), journal);
                        }
                        self.finish_protect(thread, Some(error))
                    }
                    None if self.exit_closing_scopes(thread) => self.finish_exit_scopes(thread),
                    None if object.closing => self.finish_close_thread(thread, unwind.error),
                    None => {
                        let (fault, error) = unwind.error.ok_or(VmError::Corrupt)?;
                        self.fail_thread(thread, fault, error)
                    }
                }
            }
        }
    }

    /// The unwind reached the top frame. A frame with to-be-closed values
    /// closes its open upvalues, then its values, newest first, each a call
    /// given the error; the unwind waits in its `Close` event meanwhile, and
    /// the instruction the error stopped is abandoned. Otherwise the frame
    /// is removed.
    fn unwind_frame(
        &mut self,
        thread: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        if matches!(frame.boundary(), Some(Boundary::Hook { .. })) {
            self.finish_hook(thread)?;
            return Ok(Poll::Continue);
        }
        let base = frame.base;
        if frame.boundary().is_some() || !object.tbc.last().is_some_and(|slot| *slot >= base) {
            self.pop_unwound_frame(thread)?;
            return Ok(Poll::Continue);
        }
        let unwind = *object.unwind.take().ok_or(VmError::Corrupt)?;
        frame.set_pending(None, &mut self.cold_spare);
        frame.clear_targets(&mut self.cold_spare);
        frame.set_meta(
            idle_close(base, CloseNext::Unwind(unwind)),
            &mut self.cold_spare,
        );
        // The frames above are gone and their slots are dead, so the close
        // calls start at this frame's registers, not wherever the deepest
        // popped frame left `top`.
        let limit = frame.limit;
        object.top = limit;
        object.stack.truncate(limit as usize);
        #[cfg(test)]
        let skip = self.skip_unwind_close;
        #[cfg(not(test))]
        let skip = false;
        if !skip {
            self.close_open_upvalues(thread, base)?;
        }
        self.close_step(thread, journal)
    }

    /// One step of the top frame's closes (ADR 0026). Not charged: the
    /// instruction or the unwind that began them was. The next value, the
    /// newest at or above `from`, gets its `__close` looked up now, from its
    /// current metatable, and called with the value and the error or nil.
    /// With none left, the frame does `next`.
    fn close_step(
        &mut self,
        thread: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        let Some(Closing { from, next }) = closing(frame).copied().filter(|_| {
            frame
                .meta()
                .is_some_and(|meta| meta.phase == MetaPhase::Idle)
        }) else {
            return Err(VmError::Corrupt);
        };
        if let Some(&slot) = object.tbc.last()
            && slot >= from
        {
            object.tbc.pop();
            let value = object
                .stack
                .get(slot as usize)
                .copied()
                .unwrap_or(Value::Nil);
            // A scope's value leaves with the scope. A return or an unwind
            // keeps the register, which may hold a result.
            let error = match next {
                CloseNext::Unwind(unwind) => unwind.error.map_or(Value::Nil, |(_, error)| error),
                CloseNext::Advance | CloseNext::Return { .. } => Value::Nil,
            };
            if next == CloseNext::Advance
                && let Some(register) = object.stack.get_mut(slot as usize)
            {
                *register = Value::Nil;
            }
            let function = index::metamethod(&self.heap, value, b"__close").unwrap_or(Value::Nil);
            return self.call_close(function, &[value, error], journal);
        }
        frame.set_meta(None, &mut self.cold_spare);
        match next {
            CloseNext::Advance => {
                frame.pc = frame.pc.saturating_add(1);
                Ok(Poll::Continue)
            }
            CloseNext::Return { src, produced } => self.return_values(thread, src, produced),
            CloseNext::Unwind(unwind) => {
                object.unwind = Some(Box::new(unwind));
                Ok(Poll::Continue)
            }
        }
    }

    /// True when the active frame is on top between two of its closes.
    fn close_ready(&self, thread: Handle<ThreadObj>) -> bool {
        self.heap
            .threads
            .get(thread)
            .and_then(|object| object.frames.last())
            .and_then(|frame| frame.meta())
            .is_some_and(|meta| meta.phase == MetaPhase::Idle)
    }

    /// A coroutine's error that nothing in it catches. As in Lua 5.4, its
    /// stack is not unwound: the frames, open upvalues, and to-be-closed
    /// values stay until `CloseThread` closes them. The resumer raises the
    /// same error; a coroutine the host resumed stops the run.
    fn fail_coroutine(
        &mut self,
        thread: Handle<ThreadObj>,
        fault: LuaFault,
        error: Value,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        object.unwind = None;
        object.status = Status::Failed;
        object.error = Some((fault, error));
        if let Some(hook) = self.heap.hooks.get_mut(object.id) {
            hook.allow_hook = true;
            hook.pending = None;
            hook.after = hooks::AfterHook::Continue;
            hook.transfer = None;
            hook.restore_cursor = None;
            hook.hook_yield = false;
        }
        match object.resumed_by.take() {
            Some(parent) => self.child_failed(parent, thread, fault, error),
            None => Ok(Poll::Stop(StepOutcome::LuaError(fault))),
        }
    }

    /// A resumed thread failed: its resumer's `Resume` is over, and it
    /// raises the same error.
    fn raise_in_resumer(
        &mut self,
        parent: Handle<ThreadObj>,
        fault: LuaFault,
        error: Value,
    ) -> Result<Poll, VmError> {
        if let Some(frame) = self
            .heap
            .threads
            .get_mut(parent)
            .and_then(|object| object.frames.last_mut())
            && matches!(frame.pending(), Some(Pending::Resuming { .. }))
        {
            frame.set_pending(None, &mut self.cold_spare);
        }
        self.heap.active = Some(parent);
        self.refresh_hook_trap();
        Ok(self.throw_on(parent, fault, error))
    }

    /// `CloseThread` finished closing `thread`: it is dead, and the closer's
    /// `CloseThread` gets `true, nil`, or `false` and the last error.
    fn finish_close_thread(
        &mut self,
        thread: Handle<ThreadObj>,
        error: Option<(LuaFault, Value)>,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        object.unwind = None;
        object.closing = false;
        object.frames.clear();
        object.stack.clear();
        object.top = 0;
        object.tbc.clear();
        object.status = Status::Completed;
        object.error = None;
        let parent = object.resumed_by.take().ok_or(VmError::Corrupt)?;
        // `coroutine.close` or a `coroutine.wrap` function closing it.
        if !matches!(
            self.heap
                .threads
                .get(parent)
                .and_then(|object| object.frames.last())
                .and_then(|frame| frame.pending()),
            Some(Pending::Resuming { .. })
        ) {
            return self.close_finished(parent, error);
        }
        let (ok, error) = match error {
            Some((_, error)) => (false, error),
            None => (true, Value::Nil),
        };
        let dest = {
            let parent_obj = self.heap.threads.get_mut(parent).ok_or(VmError::Corrupt)?;
            let frame = parent_obj.frames.last_mut().ok_or(VmError::Corrupt)?;
            let Some(&Pending::Resuming { child, dest, .. }) = frame.pending() else {
                return Err(VmError::Corrupt);
            };
            if child != thread {
                return Err(VmError::Corrupt);
            }
            frame.set_pending(None, &mut self.cold_spare);
            frame.pc = frame.pc.saturating_add(1);
            frame.base + u32::from(dest)
        };
        self.write_abs(parent, dest, Value::Bool(ok))?;
        self.write_abs(parent, dest + 1, error)?;
        self.heap.active = Some(parent);
        self.refresh_hook_trap();
        Ok(Poll::Continue)
    }

    fn set_unwind(
        &mut self,
        thread: Handle<ThreadObj>,
        error: Option<(LuaFault, Value)>,
        phase: UnwindPhase,
    ) -> Result<(), VmError> {
        self.heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .unwind = Some(Box::new(crate::heap::Unwind { error, phase }));
        Ok(())
    }

    /// Call `xpcall`'s message handler with the error, above the failing
    /// frames, under a `Handler` boundary that takes its first result. The
    /// unwind is suspended until then; the failing frames stay as they are.
    #[allow(clippy::too_many_arguments)]
    fn call_handler(
        &mut self,
        thread: Handle<ThreadObj>,
        protect: usize,
        target: usize,
        handler: Value,
        depth: u8,
        fault: LuaFault,
        error: Value,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let slot = u32::try_from(object.stack.len())
            .map_err(|_| VmError::Corrupt)?
            .max(object.top);
        // The handler runs in the reserve, frames and stack slots alike.
        if object.frames.len() + 1 >= MAX_FRAMES || slot + 2 > self.max_stack_slots {
            let fault = LuaFault::ErrorHandling;
            let error = self.fault_value(fault);
            let target = u32::try_from(target).map_err(|_| VmError::Corrupt)?;
            self.set_unwind(
                thread,
                Some((fault, error)),
                UnwindPhase::Popping {
                    target: Some(target),
                },
            )?;
            return Ok(Poll::Continue);
        }
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let closure = object.frames.last().ok_or(VmError::Corrupt)?.closure;
        object.unwind = None;
        count!("frame_pushes");
        object.frames.push(Frame {
            closure,
            pc: 0,
            base: slot + 1,
            limit: slot + 1,
            nresults: 1,
            vararg_len: 0,
            flags: 0,
            cold: FrameCold::with_boundary(
                Boundary::Handler {
                    slot,
                    protect: u32::try_from(protect).map_err(|_| VmError::Corrupt)?,
                    target: u32::try_from(target).map_err(|_| VmError::Corrupt)?,
                    depth,
                    fault,
                },
                &mut self.cold_spare,
            ),
        });
        self.write_abs(thread, slot, handler)?;
        self.write_abs(thread, slot + 1, error)?;
        self.heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .top = slot + 2;
        match self.callable(handler) {
            Value::Closure(closure) => {
                if !self.hook_trap
                    && self.try_fast_boundary_call(thread, handler, &[error], slot, 1)?
                {
                    return Ok(Poll::Continue);
                }
                self.push_lua_frame(closure, slot, 1, 1, false)
            }
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// The message handler returned: its first result replaces the error,
    /// and unwinding continues down to the `xpcall`, or to the `load` that
    /// caught the error.
    fn finish_handler(&mut self, thread: Handle<ThreadObj>) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.pop().ok_or(VmError::Corrupt)?;
        let Some(Boundary::Handler {
            slot,
            target,
            fault,
            ..
        }) = frame.boundary().cloned()
        else {
            return Err(VmError::Corrupt);
        };
        let error = object
            .stack
            .get(slot as usize)
            .copied()
            .unwrap_or(Value::Nil);
        self.set_unwind(
            thread,
            Some((fault, error)),
            UnwindPhase::Popping {
                target: Some(target),
            },
        )?;
        Ok(Poll::Continue)
    }

    /// Remove the top frame of an unwinding thread. A Lua frame's open
    /// upvalues close first, keeping the values they share; the frame's
    /// pending call, metamethod call, and assignment go with it. This is
    /// where a scope's close actions will run once `__close` exists.
    fn pop_unwound_frame(&mut self, thread: Handle<ThreadObj>) -> Result<(), VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        #[cfg(test)]
        let skip = self.skip_unwind_close;
        #[cfg(not(test))]
        let skip = false;
        if frame.boundary().is_none() && !skip {
            let base = frame.base;
            self.close_open_upvalues(thread, base)?;
        }
        self.heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .frames
            .pop();
        Ok(())
    }

    /// The protected call on top of `thread` ends: with the callee's results
    /// (`error` is `None`), or with `false, error`. The results go to the
    /// `pcall` slot through the caller's result window, as any call's do.
    fn finish_protect(
        &mut self,
        thread: Handle<ThreadObj>,
        error: Option<Value>,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.pop().ok_or(VmError::Corrupt)?;
        let Some(Boundary::Protect {
            func,
            advance_caller,
            ..
        }) = frame.boundary().cloned()
        else {
            return Err(VmError::Corrupt);
        };
        // Everything above the call is the protected call's scratch; the
        // caller's extra arguments are below its registers (ADR 0028).
        let scratch_end = u32::try_from(object.stack.len())
            .map_err(|_| VmError::Corrupt)?
            .max(object.top);
        let _ = object;
        let hooked = self.hook_trap && self.wrap_native_return(thread, func, frame.nresults)?;
        let mode = if hooked { COUNT_OPEN } else { frame.nresults };
        let produced = match error {
            None => {
                let count = self
                    .heap
                    .threads
                    .get(thread)
                    .ok_or(VmError::Corrupt)?
                    .top
                    .saturating_sub(func + 1);
                self.write_abs(thread, func, Value::Bool(true))?;
                count + 1
            }
            Some(error) => {
                self.write_abs(thread, func, Value::Bool(false))?;
                self.write_abs(thread, func + 1, error)?;
                2
            }
        };
        for offset in produced..Self::wanted(mode, produced) {
            self.write_abs(thread, func + offset, Value::Nil)?;
        }
        self.finish_result_window(thread, func, mode, produced, scratch_end)?;
        if advance_caller && !hooked {
            let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
            let caller = object.frames.last_mut().ok_or(VmError::Corrupt)?;
            caller.pc = caller.pc.saturating_add(1);
        }
        Ok(Poll::Continue)
    }

    /// Nothing caught the error: the thread is failed and keeps the error.
    /// A coroutine passes it on to the thread that resumed it, as
    /// `coroutine.wrap` does; the entry thread reports it to the host.
    fn fail_thread(
        &mut self,
        thread: Handle<ThreadObj>,
        fault: LuaFault,
        error: Value,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        object.unwind = None;
        object.frames.clear();
        object.stack.clear();
        object.top = 0;
        object.status = Status::Failed;
        object.error = Some((fault, error));
        if let Some(hook) = self.heap.hooks.get_mut(object.id) {
            hook.allow_hook = true;
            hook.pending = None;
            hook.after = hooks::AfterHook::Continue;
            hook.transfer = None;
            hook.restore_cursor = None;
            hook.hook_yield = false;
        }
        match object.resumed_by.take() {
            Some(parent) => self.child_failed(parent, thread, fault, error),
            None => Ok(Poll::Stop(StepOutcome::LuaError(fault))),
        }
    }

    /// The boundary frame on top whose call is done, if any.
    fn boundary_ready(&self, thread: Handle<ThreadObj>) -> Option<Ready> {
        let frame = self.heap.threads.get(thread)?.frames.last()?;
        if frame.pending().is_some() {
            return None;
        }
        Some(match frame.boundary()? {
            Boundary::Hook { .. } => Ready::Hook,
            Boundary::HookNative { .. } => Ready::HookNative,
            Boundary::Protect { .. } => Ready::Protect,
            Boundary::Handler { .. } => Ready::Handler,
            Boundary::Builtin { .. } => Ready::Builtin,
            Boundary::Native { .. } => Ready::Native,
            Boundary::Finalizer { .. } => Ready::Finalizer,
        })
    }

    /// The base functions the VM implements, called from the active
    /// frame's call site like any native: `error`, `pcall`, and `xpcall`
    /// here, the rest in `builtins`. Out of line: inlined, it quadrupled
    /// `call_native` and slowed every native call by a third.
    #[inline(never)]
    fn call_builtin(
        &mut self,
        builtin: crate::host::Builtin,
        site: Option<(u32, u8, u32)>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("builtin_calls");
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (func, nresults, passed) = match site {
            Some(site) => site,
            None => {
                let (func, nresults, passed, _) = self.call_site(active)?;
                (func, nresults, passed)
            }
        };
        let arg = |runtime: &Self, index: u32| -> Result<Value, VmError> {
            Ok(if index < passed {
                runtime
                    .heap
                    .threads
                    .get(active)
                    .ok_or(VmError::Corrupt)?
                    .stack
                    .get((func + 1 + index) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            })
        };
        match builtin {
            #[cfg(test)]
            crate::host::Builtin::CapabilitySmoke => {
                let request = crate::CapabilityRequest::StdioWriteStdout {
                    bytes: b"smoke".to_vec(),
                };
                match self.capability(&request, journal)? {
                    crate::CapabilityPoll::Waiting(_) => Ok(Poll::Continue),
                    crate::CapabilityPoll::Ready(Ok(crate::CapabilityValue::Unsigned(n))) => self
                        .native_returned(
                            active,
                            func,
                            nresults,
                            passed,
                            &[Value::Integer(n as i64)],
                        ),
                    _ => Err(VmError::Corrupt),
                }
            }
            crate::host::Builtin::Error => {
                let level = match arg(self, 1)? {
                    Value::Nil => 1,
                    value => match error_level(&self.heap, value) {
                        Some(level) => level,
                        None => {
                            let ctx = library::Ctx {
                                active,
                                func,
                                passed,
                                framed: false,
                            };
                            let next = self
                                .int_arg(&ctx, 1)
                                .err()
                                .unwrap_or(library::Next::Fault(LuaFault::Argument));
                            return Ok(self.finish_next(active, next));
                        }
                    },
                };
                let mut error = arg(self, 0)?;
                if let Value::String(handle) = error
                    && let Ok(level) = usize::try_from(level)
                    && level > 0
                    && let Some(location) =
                        self.location(active, level).filter(|(_, line)| *line > 0)
                {
                    let text = self.heap.string_bytes(handle).unwrap_or_default().to_vec();
                    error = self.prefixed(Some(location), text, LuaFault::Error);
                }
                Ok(self.throw_on(active, LuaFault::Error, error))
            }
            crate::host::Builtin::Pcall | crate::host::Builtin::Xpcall => {
                let xpcall = builtin == crate::host::Builtin::Xpcall;
                let handler = if xpcall {
                    let handler = arg(self, 1)?;
                    if passed < 2 || !handler.is_function() {
                        return Ok(self.fault(LuaFault::Native));
                    }
                    Some(handler)
                } else {
                    if passed == 0 {
                        return Ok(self.fault(LuaFault::Native));
                    }
                    None
                };
                if let Some(fault) = self.depth_fault(active)? {
                    return Ok(self.fault(fault));
                }
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let mut nargs = passed - 1;
                if xpcall {
                    // The handler leaves the window; the arguments close up.
                    let start = (func + 3) as usize;
                    let end = (func + 1 + passed) as usize;
                    grow_stack(object, end, &mut self.heap.gc);
                    object.stack.copy_within(start..end, start - 1);
                    nargs -= 1;
                }
                let caller = object.frames.last().ok_or(VmError::Corrupt)?;
                let advance_caller = caller.boundary().is_none() && caller.meta().is_none();
                let closure = caller.closure;
                count!("frame_pushes");
                object.frames.push(Frame {
                    closure,
                    pc: 0,
                    base: func + 1,
                    limit: func + 1,
                    nresults,
                    vararg_len: 0,
                    flags: 0,
                    cold: FrameCold::with_boundary(
                        Boundary::Protect {
                            func,
                            advance_caller,
                            handler,
                        },
                        &mut self.cold_spare,
                    ),
                });
                object.top = func + 2 + nargs;
                self.call_protected(active, func + 1, nargs, journal)
            }
            other => self.call_base(other, journal),
        }
    }

    /// Call the value at `slot` with `nargs` arguments from the `Protect`
    /// frame on top, wanting all its results. Anything that fails here is
    /// already inside the protected call.
    fn call_protected(
        &mut self,
        thread: Handle<ThreadObj>,
        slot: u32,
        nargs: u32,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (callee, nargs) = match self.resolve_callable(thread, slot, nargs)? {
            Ok(resolved) => resolved,
            Err(fault) => return Ok(self.fault(fault)),
        };
        match self.callable(callee) {
            Value::Closure(closure) => self.push_lua_frame(closure, slot, nargs, COUNT_OPEN, false),
            Value::Native(index) => {
                self.heap
                    .threads
                    .get_mut(thread)
                    .ok_or(VmError::Corrupt)?
                    .top = slot + 1 + nargs;
                if self.is_builtin(index)? {
                    return self.defer_call(thread);
                }
                self.call_native(index, journal)
            }
            _ => Err(VmError::Corrupt),
        }
    }

    /// Whether the native at `index` is one the VM implements.
    /// What a call of `value` runs: a native closure runs its builtin
    /// (ADR 0035), which finds the closure in the call slot.
    #[inline]
    pub(crate) fn callable(&self, value: Value) -> Value {
        match value {
            Value::NativeClosure(handle) => self
                .heap
                .native_closures
                .get(handle)
                .map_or(Value::Nil, |closure| Value::Native(closure.native)),
            other => other,
        }
    }

    fn is_builtin(&self, index: u32) -> Result<bool, VmError> {
        let slot = *self
            .native_slots
            .get(index as usize)
            .ok_or(VmError::Corrupt)?;
        Ok(self
            .registry
            .native(slot)
            .ok_or(VmError::Corrupt)?
            .builtin
            .is_some())
    }

    /// Leave the call at the top boundary frame's call slot for the next
    /// step (`Pending::Deferred`, ADR 0033).
    fn defer_call(&mut self, thread: Handle<ThreadObj>) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        object
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?
            .set_pending(Some(Pending::Deferred), &mut self.cold_spare);
        Ok(Poll::Continue)
    }

    /// Make a deferred call: the native at the top frame's call slot.
    fn run_deferred(
        &mut self,
        thread: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        object
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?
            .set_pending(None, &mut self.cold_spare);
        let (_, _, _, callee) = self.call_site(thread)?;
        let Value::Native(index) = self.callable(callee) else {
            return Err(VmError::Corrupt);
        };
        self.call_native(index, journal)
    }

    /// A step that ran out of memory raises a Lua memory error; every other
    /// VM error stays a host error. The step either allocated nothing or
    /// belongs to frames the unwind removes. Cold and out of line: mapping
    /// every step's result inline cost the call paths 5-10%.
    #[cold]
    #[inline(never)]
    fn vm_error(&mut self, error: VmError) -> Result<Poll, VmError> {
        match error {
            VmError::MemoryLimit => Ok(self.fault(LuaFault::Memory)),
            VmError::StackLimit => Ok(self.fault(LuaFault::StackOverflow)),
            other => Err(other),
        }
    }

    fn current_op(&self) -> Result<Op, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
        let pc = frame.pc;
        let closure = frame.closure;
        let proto = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        self.heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .ops
            .get(pc as usize)
            .cloned()
            .ok_or(VmError::Corrupt)
    }

    fn advance_pc(&mut self) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?;
        frame.pc = frame.pc.saturating_add(1);
        Ok(())
    }

    fn set_pending(&mut self, pending: Pending) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?;
        frame.set_pending(Some(pending), &mut self.cold_spare);
        Ok(())
    }

    fn load(&self, reg: u8) -> Result<Value, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
        let absolute = frame.base + u32::from(reg);
        Ok(thread
            .stack
            .get(absolute as usize)
            .copied()
            .unwrap_or(Value::Nil))
    }

    fn store(&mut self, reg: u8, value: Value) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let base = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?
            .base;
        self.write_abs(active, base + u32::from(reg), value)
    }

    fn write_abs(
        &mut self,
        thread: Handle<ThreadObj>,
        slot: u32,
        value: Value,
    ) -> Result<(), VmError> {
        let bound = self.max_stack_slots;
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let index = slot as usize;
        if object.stack.len() <= index {
            if slot >= bound {
                return Err(VmError::StackLimit);
            }
            grow_stack(object, index + 1, &mut self.heap.gc);
        }
        // `grow_stack` has made this logical slot live, so the physical
        // high-water vector contains it too.
        object.stack.values[index] = value;
        Ok(())
    }

    fn const_bytes(&self, index: u32) -> Result<Vec<u8>, VmError> {
        let proto = self.current_proto()?;
        self.heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .byte_consts
            .get(index as usize)
            .cloned()
            .ok_or(VmError::Corrupt)
    }

    /// The prototype's string object for constant `index`.
    fn const_string(&self, index: u32) -> Result<Handle<StringObj>, VmError> {
        let proto = self.current_proto()?;
        self.heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .const_strings
            .get(index as usize)
            .copied()
            .ok_or(VmError::Corrupt)
    }

    fn const_symbol(&self, index: u32) -> Result<String, VmError> {
        let bytes = self.const_bytes(index)?;
        String::from_utf8(bytes).map_err(|_| VmError::Corrupt)
    }

    fn current_proto(&self) -> Result<Handle<Proto>, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let closure = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?
            .closure;
        Ok(self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto)
    }

    #[inline(never)]
    fn exec_call(
        &mut self,
        func: u8,
        nargs: u8,
        nresults: u8,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (func_abs, passed, mut callee) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            let func_abs = frame.base + u32::from(func);
            let value = thread
                .stack
                .get(func_abs as usize)
                .copied()
                .unwrap_or(Value::Nil);
            let passed = if nargs == COUNT_OPEN {
                thread.top.saturating_sub(func_abs.saturating_add(1))
            } else {
                u32::from(nargs)
            };
            (func_abs, passed, value)
        };
        let mut passed = passed;
        if !callee.is_function() {
            #[cfg(any(test, debug_assertions))]
            let fast = self.hot_core == HotCoreMode::Full;
            #[cfg(not(any(test, debug_assertions)))]
            let fast = true;
            if fast
                && !self.hook_trap
                && self.try_callable_call(active, func_abs, passed, nresults, callee)?
            {
                return Ok(Poll::Continue);
            }
            match self.resolve_callable(active, func_abs, passed)? {
                Ok(resolved) => (callee, passed) = resolved,
                Err(fault) => return Ok(self.fault(fault)),
            }
        }
        match self.callable(callee) {
            Value::Closure(closure) => {
                self.push_lua_frame(closure, func_abs, passed, nresults, true)
            }
            Value::Native(index) => {
                // A native reads its argument count from `top` (`call_site`),
                // so arguments `__call` inserted count too.
                self.heap
                    .threads
                    .get_mut(active)
                    .ok_or(VmError::Corrupt)?
                    .top = func_abs + 1 + passed;
                self.call_native_at(index, Some((func_abs, nresults, passed)), journal)
            }
            _ => Err(VmError::Corrupt),
        }
    }

    /// A direct non-vararg Lua __call handler. Preflight both the resolver's
    /// inserted argument and enter_lua's frame window before changing either.
    /// Chains, native handlers, short stacks and all fault cases use the original
    /// resolver, including its writes before a later depth/quota failure.
    #[inline(never)]
    fn try_callable_call(
        &mut self,
        active: Handle<ThreadObj>,
        slot: u32,
        passed: u32,
        nresults: u8,
        callee: Value,
    ) -> Result<bool, VmError> {
        let Some(Value::Closure(closure)) = index::metamethod(&self.heap, callee, b"__call") else {
            return Ok(false);
        };
        let proto_handle = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let proto = self.heap.protos.get(proto_handle).ok_or(VmError::Corrupt)?;
        let base = slot + 1;
        let arg_end = base + passed + 1;
        let limit = base + u32::from(proto.max_reg);
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let len = thread.stack.len() as u32;
        let ordinary = self.max_stack_slots - self.max_stack_slots / 8;
        let argument_charge =
            cost::STACK_SLOT * u64::from(arg_end.saturating_sub(thread.charged_slots));
        if proto.vararg
            || thread.frames.len() >= MAX_CALL_DEPTH
            || thread.frames.last().is_none()
            || slot >= len
            || arg_end > ordinary
            || limit > ordinary
            || !self
                .heap
                .gc
                .fits(cost::STACK_SLOT * u64::from(arg_end.saturating_sub(len)))
            || !self.heap.gc.fits(
                argument_charge
                    + cost::STACK_SLOT * u64::from(limit.saturating_sub(len.max(arg_end))),
            )
        {
            return Ok(false);
        }
        let Heap {
            threads,
            closures,
            protos,
            gc,
            ..
        } = &mut self.heap;
        let thread = threads.get_mut(active).ok_or(VmError::Corrupt)?;
        grow_stack(thread, arg_end as usize, gc);
        thread
            .stack
            .copy_within(base as usize..arg_end as usize - 1, base as usize + 1);
        thread.stack[base as usize] = callee;
        thread.stack[slot as usize] = Value::Closure(closure);
        // The preflight above covered both the inserted argument and the
        // callee window before the shift. Enter through the shared writer;
        // rechecking here would inspect the already-mutated stack.
        let proto = protos.get(proto_handle).ok_or(VmError::Corrupt)?;
        let mut heap = FrameHeap {
            closures,
            protos,
            gc,
            ordinary: self.max_stack_slots - self.max_stack_slots / 8,
            stack_grows: &mut self.stack_grows,
            frame_grows: &mut self.frame_grows,
            cold_spare: &mut self.cold_spare,
        };
        write_fixed_frame(
            thread,
            &mut heap,
            Admit {
                limit,
                end: limit.max(arg_end),
            },
            closure,
            proto.params,
            base,
            passed + 1,
            nresults,
            true,
        );
        Ok(true)
    }

    /// `TailCall` (ADR 0029). The running frame is finished: its open
    /// upvalues close, and its caller gets the callee's results as its own.
    /// A Lua callee's frame replaces it, at the same depth and call slot.
    /// For a native callee, see [`Self::tail_native`]. `__call` resolves as
    /// for any call, inside this instruction, so no hop leaves a frame.
    #[inline(never)]
    fn tail_call(&mut self, func: u8, nargs: u8, journal: &mut Journal) -> Result<Poll, VmError> {
        count!("tail_calls");
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (func_abs, mut passed, mut callee, args, nresults) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            // Source makes a tail call only outside every `<close>` scope.
            // A value this frame must still close means code that would
            // skip the close.
            if thread.tbc.last().is_some_and(|slot| *slot >= frame.base) {
                return Err(VmError::Corrupt);
            }
            let func_abs = frame.base + u32::from(func);
            let passed = if nargs == COUNT_OPEN {
                thread.top.saturating_sub(func_abs.saturating_add(1))
            } else {
                u32::from(nargs)
            };
            let callee = thread
                .stack
                .get(func_abs as usize)
                .copied()
                .unwrap_or(Value::Nil);
            // Where the frame's own arguments were passed: just above its
            // call slot, or the bottom of the stack for a thread's first
            // frame.
            let args = frame
                .base
                .checked_sub(frame.vararg_len)
                .ok_or(VmError::Corrupt)?;
            (func_abs, passed, callee, args, frame.nresults)
        };
        if !callee.is_function() {
            match self.resolve_callable(active, func_abs, passed)? {
                Ok(resolved) => (callee, passed) = resolved,
                Err(fault) => return Ok(self.fault(fault)),
            }
        }
        match self.callable(callee) {
            Value::Closure(closure) => self.enter_lua(
                closure,
                args,
                passed,
                nresults,
                Entry::Replace { from: func_abs + 1 },
            ),
            Value::Native(index) => self.tail_native(index, func_abs, passed, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// A native tail call stays in its Lua frame, as a PUC C callee does.
    /// The native answers this frame's open call; the compiled `Return`
    /// after `TailCall` delivers its results to the frame below. Clear
    /// everything except the call window before the native runs or waits.
    fn tail_native(
        &mut self,
        index: u32,
        func_abs: u32,
        passed: u32,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (base, args, limit) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            (
                frame.base,
                frame
                    .base
                    .checked_sub(frame.vararg_len)
                    .ok_or(VmError::Corrupt)?,
                frame.limit,
            )
        };
        self.close_tail_upvalues(active, base)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let window_end = func_abs + 1 + passed;
        grow_stack(thread, window_end as usize, &mut self.heap.gc);
        thread.stack[args as usize..func_abs as usize].fill(Value::Nil);
        let stale = (window_end as usize).min(thread.stack.len());
        thread.stack[stale..].fill(Value::Nil);
        thread.stack.truncate(limit.max(window_end) as usize);
        thread.top = window_end;
        self.call_native(index, journal)
    }

    /// A tail call closes the finished frame's open upvalues, from its
    /// `base`, before anything reuses their registers.
    fn close_tail_upvalues(&mut self, thread: Handle<ThreadObj>, base: u32) -> Result<(), VmError> {
        #[cfg(test)]
        if self.skip_tail_close {
            return Ok(());
        }
        self.close_open_upvalues(thread, base)
    }

    /// Lua's `__call`: while the value at `slot` is not a function, move its
    /// `nargs` arguments up one slot, insert the value as the first
    /// argument, and put its `__call` value at `slot`. Gives the function
    /// and the final argument count, or the fault that ends the call. Part
    /// of the instruction that makes the call; nothing is left to resume.
    fn resolve_callable(
        &mut self,
        thread: Handle<ThreadObj>,
        slot: u32,
        mut nargs: u32,
    ) -> Result<Result<(Value, u32), LuaFault>, VmError> {
        for _ in 0..MAX_CALL_CHAIN {
            let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
            let callee = object
                .stack
                .get(slot as usize)
                .copied()
                .unwrap_or(Value::Nil);
            if callee.is_function() {
                return Ok(Ok((callee, nargs)));
            }
            let Some(handler) = index::metamethod(&self.heap, callee, b"__call") else {
                return Ok(Err(LuaFault::BadCall));
            };
            let end = slot + 1 + nargs;
            if let Some(fault) = self.slot_fault(thread, end + 1)? {
                return Ok(Err(fault));
            }
            let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
            let (first, end) = (slot as usize + 1, end as usize);
            grow_stack(object, end + 1, &mut self.heap.gc);
            object.stack.copy_within(first..end, first + 1);
            object.stack[first] = callee;
            object.stack[slot as usize] = handler;
            nargs += 1;
        }
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        Ok(
            match object
                .stack
                .get(slot as usize)
                .copied()
                .unwrap_or(Value::Nil)
            {
                callee if callee.is_function() => Ok((callee, nargs)),
                _ => Err(LuaFault::CallChain),
            },
        )
    }

    /// Push a Lua frame for `closure` at `func_abs` with `passed` arguments
    /// after it. `advance_caller` moves the caller past its `Call`; a
    /// metamethod call leaves the caller on its instruction.
    fn push_lua_frame(
        &mut self,
        closure: Handle<ClosureObj>,
        func_abs: u32,
        passed: u32,
        nresults: u8,
        advance_caller: bool,
    ) -> Result<Poll, VmError> {
        self.enter_lua(
            closure,
            func_abs + 1,
            passed,
            nresults,
            Entry::Push { advance_caller },
        )
    }

    /// The one place a Lua function's frame is built, for a call and a tail
    /// call alike: `passed` arguments at `args`, and `nresults` wanted by
    /// the caller. Each caller gets its own copy, specialised to its entry.
    #[inline(always)]
    fn enter_lua(
        &mut self,
        closure: Handle<ClosureObj>,
        args: u32,
        passed: u32,
        nresults: u8,
        entry: Entry,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (max_reg, params, vararg) = {
            let proto = self
                .heap
                .closures
                .get(closure)
                .ok_or(VmError::Corrupt)?
                .proto;
            let proto = self.heap.protos.get(proto).ok_or(VmError::Corrupt)?;
            (proto.max_reg, proto.params, proto.vararg)
        };
        // A tail call keeps the depth it had.
        if let Entry::Push { .. } = entry
            && let Some(fault) = self.depth_fault(active)?
        {
            return Ok(self.fault(fault));
        }
        let params = u32::from(params);
        // A vararg function's extra arguments stay where they were passed,
        // just above the function's slot, and its registers start above
        // them (ADR 0028). Every call it makes is above its registers, so
        // none can reach them.
        let extra = if vararg {
            passed.saturating_sub(params)
        } else {
            0
        };
        let base = args + extra;
        let limit = base + u32::from(max_reg);
        if let Some(fault) = self.slot_fault(active, limit)? {
            return Ok(self.fault(fault));
        }
        // The fixed-arity push is the builder's; it declines only for the
        // reserve shapes (depth, logical limit) the general code below keeps.
        if !vararg && let Entry::Push { advance_caller } = entry {
            let Heap {
                threads,
                closures,
                protos,
                gc,
                ..
            } = &mut self.heap;
            let thread = threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let proto = closures
                .get(closure)
                .and_then(|closure| protos.get(closure.proto))
                .ok_or(VmError::Corrupt)?;
            let mut heap = FrameHeap {
                closures,
                protos,
                gc,
                ordinary: self.max_stack_slots - self.max_stack_slots / 8,
                stack_grows: &mut self.stack_grows,
                frame_grows: &mut self.frame_grows,
                cold_spare: &mut self.cold_spare,
            };
            if build_fixed_frame(
                thread,
                &mut heap,
                closure,
                proto,
                base,
                passed,
                nresults,
                advance_caller,
            )
            .is_some()
            {
                if self.hook_trap {
                    self.lua_hook_entry(active, false)?;
                }
                return Ok(Poll::Continue);
            }
        }
        if let Entry::Replace { from } = entry {
            // The running frame is done with its registers. The open
            // upvalues naming them close first, keeping their values; then
            // the callee and its arguments move down, overlapping where
            // they may, to where the frame's own were passed. A thread's
            // first frame has no call slot below its arguments.
            let old_base = self
                .heap
                .threads
                .get(active)
                .and_then(|thread| thread.frames.last())
                .ok_or(VmError::Corrupt)?
                .base;
            self.close_tail_upvalues(active, old_base)?;
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let src_end = (from + passed) as usize;
            grow_stack(thread, src_end, &mut self.heap.gc);
            let (src, dst) = match args.checked_sub(1) {
                Some(slot) => (from - 1, slot),
                None => (from, args),
            };
            thread
                .stack
                .copy_within(src as usize..src_end, dst as usize);
        }
        let stack_grew;
        let frame_grew;
        {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let arg_end = args.saturating_add(passed);
            stack_grew = grow_stack(thread, limit.max(arg_end) as usize, &mut self.heap.gc);
            if extra > 0 {
                // The fixed arguments move above the extras.
                thread.stack[args as usize..arg_end as usize].rotate_left(params as usize);
            }
            let clear_from = base + params.min(passed).min(u32::from(max_reg));
            // Missing parameters and every non-parameter register start nil.
            // Extra arguments of a non-vararg function are discarded here.
            for slot in clear_from..limit {
                thread.stack[slot as usize] = Value::Nil;
            }
            if !vararg && arg_end > limit {
                for slot in limit..arg_end {
                    thread.stack[slot as usize] = Value::Nil;
                }
            }
            thread.top = base + params.min(u32::from(max_reg));
            // Built where it is stored, so an ordinary call's code stays
            // what it was before tail calls.
            let frame = || Frame {
                closure,
                pc: 0,
                base,
                limit,
                nresults,
                vararg_len: extra,
                flags: 0,
                cold: None,
            };
            match entry {
                Entry::Push { advance_caller } => {
                    if advance_caller {
                        let caller = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
                        caller.pc = caller.pc.saturating_add(1);
                    }
                    count!("lua_calls");
                    count!("frame_pushes");
                    frame_grew = thread.frames.push(frame());
                }
                Entry::Replace { .. } => {
                    // Everything above the new registers was the replaced
                    // frame's, and is dead.
                    thread.stack.truncate(limit as usize);
                    count!("lua_calls");
                    count!("frame_replacements");
                    *thread.frames.last_mut().ok_or(VmError::Corrupt)? = Frame {
                        flags: Frame::TAIL,
                        ..frame()
                    };
                    frame_grew = false;
                }
            }
        }
        if stack_grew {
            self.stack_grows = self.stack_grows.saturating_add(1);
        }
        if frame_grew {
            self.frame_grows = self.frame_grows.saturating_add(1);
        }
        if self.hook_trap {
            self.lua_hook_entry(active, matches!(entry, Entry::Replace { .. }))?;
        }
        Ok(Poll::Continue)
    }

    /// Start a metamethod call for the active frame's current instruction.
    /// The function and its arguments go to scratch slots above the frame's
    /// registers, varargs, and open results; the frame records a
    /// [`MetaCall`] and its `pc` stays on the instruction. The function runs
    /// as an ordinary call: a Lua frame, or a native with its policy. When
    /// its first result is in the scratch slot, [`Self::commit_meta`]
    /// finishes the instruction. Nothing about the operation lives on the
    /// Rust stack.
    #[inline(never)]
    fn call_meta(
        &mut self,
        event: MetaEvent,
        function: Value,
        args: &[Value],
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("metamethod_calls");
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        #[cfg(any(test, debug_assertions))]
        let fast = self.hot_core == HotCoreMode::Full;
        #[cfg(not(any(test, debug_assertions)))]
        let fast = true;
        if fast && !self.hook_trap && matches!(function, Value::Closure(_)) {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let mut heap = FrameHeap {
                closures: &self.heap.closures,
                protos: &self.heap.protos,
                gc: &mut self.heap.gc,
                ordinary: self.max_stack_slots - self.max_stack_slots / 8,
                stack_grows: &mut self.stack_grows,
                frame_grows: &mut self.frame_grows,
                cold_spare: &mut self.cold_spare,
            };
            if fast_meta_call(thread, &mut heap, event, function, args) {
                return Ok(Poll::Continue);
            }
        }
        let slot = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            frame.limit.max(thread.top)
        };
        if let Some(fault) = self.slot_fault(active, slot + 1 + args.len() as u32)? {
            return Ok(self.fault(fault));
        }
        self.write_abs(active, slot, function)?;
        for (offset, arg) in args.iter().enumerate() {
            self.write_abs(active, slot + 1 + offset as u32, *arg)?;
        }
        // Any callable value: a table with `__call` becomes the first
        // argument of its `__call`, as an ordinary call would make it.
        let (function, nargs) = match self.resolve_callable(active, slot, args.len() as u32)? {
            Ok(resolved) => resolved,
            Err(fault) => {
                let text = self.metamethod_call_message(function);
                let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                thread.stack.truncate(slot as usize);
                return Ok(self.fault_text(fault, text));
            }
        };
        let nargs = u8::try_from(nargs).map_err(|_| VmError::Corrupt)?;
        {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            thread.top = slot + 1 + u32::from(nargs);
            thread.frames.last_mut().ok_or(VmError::Corrupt)?.set_meta(
                Some(MetaCall {
                    event,
                    slot,
                    nargs,
                    phase: MetaPhase::Running,
                    close: None,
                }),
                &mut self.cold_spare,
            );
        }
        match self.callable(function) {
            Value::Closure(closure) => {
                self.push_lua_frame(closure, slot, u32::from(nargs), 1, false)
            }
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// The error a call one frame deeper raises, if any. Past
    /// [`MAX_CALL_DEPTH`] only error handling may go on: a message handler,
    /// or a close run by an unwind, and whatever they call. Past the reserve
    /// that leaves, the error is "error in error handling", as Lua 5.4
    /// reports a stack overflow while handling one.
    fn depth_fault(&self, thread: Handle<ThreadObj>) -> Result<Option<LuaFault>, VmError> {
        let frames = &self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames;
        if frames.len() < MAX_CALL_DEPTH {
            return Ok(None);
        }
        Ok(Some(if !frames.iter().any(in_reserve) {
            LuaFault::StackOverflow
        } else if frames.len() + 1 >= MAX_FRAMES {
            LuaFault::ErrorHandling
        } else {
            return Ok(None);
        }))
    }

    /// The error a frame whose registers end at stack slot `end` raises, if
    /// any. Past the ordinary part of the stack bound, only error handling
    /// may go on, as past [`MAX_CALL_DEPTH`]; past the bound itself, the
    /// error is "error in error handling" (ADR 0028).
    fn slot_fault(&self, thread: Handle<ThreadObj>, end: u32) -> Result<Option<LuaFault>, VmError> {
        // New slots count against the heap quota (ADR 0041), except while
        // a memory error's unwind runs `__close` values: they still run,
        // within the stack bound, as Lua's spare stack lets them.
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let len = object.stack.len() as u64;
        if u64::from(end) > len
            && !self
                .heap
                .gc
                .fits(cost::STACK_SLOT.saturating_mul(u64::from(end) - len))
            && !object.frames.iter().any(closes_memory_error)
        {
            return Ok(Some(LuaFault::Memory));
        }
        if end <= self.max_stack_slots - self.max_stack_slots / 8 {
            return Ok(None);
        }
        let frames = &self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames;
        Ok(Some(if !frames.iter().any(in_reserve) {
            LuaFault::StackOverflow
        } else if end > self.max_stack_slots {
            LuaFault::ErrorHandling
        } else {
            return Ok(None);
        }))
    }

    /// Start the next `__close` call of the top frame's closes, as
    /// `call_meta` would, but the frame's `Close` state stays where it is:
    /// only the call's slot, argument count, and phase change. When the
    /// value cannot be called, the error leaves that state as it was.
    fn call_close(
        &mut self,
        function: Value,
        args: &[Value],
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let slot = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            frame.limit.max(thread.top)
        };
        if let Some(fault) = self.slot_fault(active, slot + 1 + args.len() as u32)? {
            return Ok(self.fault(fault));
        }
        self.write_abs(active, slot, function)?;
        for (offset, arg) in args.iter().enumerate() {
            self.write_abs(active, slot + 1 + offset as u32, *arg)?;
        }
        let (function, nargs) = match self.resolve_callable(active, slot, args.len() as u32)? {
            Ok(resolved) => resolved,
            Err(fault) => {
                let text = self.metamethod_call_message(function);
                let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                thread.stack.truncate(slot as usize);
                return Ok(self.fault_text(fault, text));
            }
        };
        let nargs = u8::try_from(nargs).map_err(|_| VmError::Corrupt)?;
        {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            thread.top = slot + 1 + u32::from(nargs);
            let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
            let meta = frame.meta_mut().ok_or(VmError::Corrupt)?;
            meta.slot = slot;
            meta.nargs = nargs;
            meta.phase = MetaPhase::Running;
        }
        match self.callable(function) {
            Value::Closure(closure) => {
                self.push_lua_frame(closure, slot, u32::from(nargs), 1, false)
            }
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// True when the active frame is on top with a finished metamethod
    /// call whose result is waiting to be committed.
    fn meta_ready(&self) -> bool {
        let Some(active) = self.heap.active else {
            return false;
        };
        let Some(thread) = self.heap.threads.get(active) else {
            return false;
        };
        let Some(frame) = thread.frames.last() else {
            return false;
        };
        frame.meta().is_some_and(|meta| {
            meta.phase == MetaPhase::Running
                && !matches!(frame.pending(), Some(Pending::Capability { .. }))
        })
    }

    /// Finish the instruction a metamethod call was made for, with the
    /// call's first result. Not charged: the instruction was charged when
    /// it began.
    /// True when the call was a close and the frame's closes go on.
    fn commit_meta(&mut self, thread: Handle<ThreadObj>) -> Result<bool, VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        let meta = frame
            .take_meta(&mut self.cold_spare)
            .ok_or(VmError::Corrupt)?;
        let base = frame.base;
        let result = object
            .stack
            .get(meta.slot as usize)
            .copied()
            .unwrap_or(Value::Nil);
        // The call's slots, result included, are scratch above the frame's
        // registers. Drop them so the next call reuses the same slot.
        object.stack.truncate(meta.slot as usize);
        object.top = meta.slot;
        match meta.event {
            MetaEvent::Store { dst } => {
                self.write_abs(thread, base + u32::from(dst), result)?;
                self.bump_pc(thread)?;
            }
            MetaEvent::Truth { dst, negate } => {
                self.finish_truth(thread, dst, result.truthy() != negate)?;
            }
            MetaEvent::NewIndex => self.bump_pc(thread)?,
            MetaEvent::NewIndexAssign => self.step_assignment(thread)?,
            // The next step closes the next value or finishes the closes.
            MetaEvent::Close => {
                let mut meta = meta;
                meta.slot = 0;
                meta.nargs = 0;
                meta.phase = MetaPhase::Idle;
                let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
                object
                    .frames
                    .last_mut()
                    .ok_or(VmError::Corrupt)?
                    .set_meta(Some(meta), &mut self.cold_spare);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Both immediate and resumed comparison results finish here. The saved
    /// instruction owns the branch sense/target; no operand is read again.
    fn finish_truth(
        &mut self,
        thread: Handle<ThreadObj>,
        dst: u8,
        bit: bool,
    ) -> Result<(), VmError> {
        let frame = self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?;
        if dst != COUNT_OPEN {
            let base = frame.base;
            self.write_abs(thread, base + u32::from(dst), Value::Bool(bit))?;
            return self.bump_pc(thread);
        }
        let proto = self
            .heap
            .closures
            .get(frame.closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let op = self
            .heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .ops
            .get(frame.pc as usize)
            .ok_or(VmError::Corrupt)?;
        let Op::CompareBranch { sense, offset, .. } = *op else {
            return Err(VmError::Corrupt);
        };
        let pc = jump_target(frame.pc, if bit == sense { offset } else { 0 })?;
        self.heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?
            .pc = pc;
        Ok(())
    }

    fn bump_pc(&mut self, thread: Handle<ThreadObj>) -> Result<(), VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        frame.pc = frame.pc.saturating_add(1);
        Ok(())
    }

    /// One assignment store is done: move the cursor, and finish the
    /// instruction after the last store.
    fn step_assignment(&mut self, thread: Handle<ThreadObj>) -> Result<(), VmError> {
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        let finished = match frame.pending_mut() {
            Some(Pending::Assigning { next, .. }) => {
                *next = next.saturating_sub(1);
                *next == 0
            }
            _ => return Err(VmError::Corrupt),
        };
        if finished {
            frame.set_pending(None, &mut self.cold_spare);
            frame.clear_targets(&mut self.cold_spare);
            frame.pc = frame.pc.saturating_add(1);
        }
        Ok(())
    }

    /// Register `offset` of the running frame, which may be past `u8`.
    fn load_abs_offset(&self, offset: u32) -> Result<Value, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let base = thread.frames.last().ok_or(VmError::Corrupt)?.base;
        Ok(thread
            .stack
            .get((base + offset) as usize)
            .copied()
            .unwrap_or(Value::Nil))
    }

    fn store_for(&mut self, base: u8, state: fornum::ForState) -> Result<(), VmError> {
        let reg = |k: u8| base.checked_add(k).ok_or(VmError::Corrupt);
        self.store(reg(0)?, state.index)?;
        self.store(reg(1)?, state.limit)?;
        self.store(reg(2)?, state.step)?;
        self.store(reg(3)?, state.control)
    }

    fn jump(&mut self, offset: i32) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?;
        frame.pc = jump_target(frame.pc, offset)?;
        Ok(())
    }

    /// Put the entry thread back at its first instruction without allocating.
    /// Capacity of the stack and frame vectors is kept, so a second run can
    /// show whether the call path still grows them.
    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn rewind_entry(&mut self) {
        let Some(entry) = self.heap.entry else {
            return;
        };
        if let Some(thread) = self.heap.threads.get_mut(entry) {
            thread.status = Status::Ready;
            thread.frames.truncate(1);
            if let Some(frame) = thread.frames.first_mut() {
                frame.pc = 0;
                frame.set_pending(None, &mut self.cold_spare);
                frame.clear_targets(&mut self.cold_spare);
                thread.top = frame.limit;
            }
            thread.stack.fill(Value::Nil);
            thread.host_results.clear();
            thread.resumed_by = None;
        }
        self.heap.active = Some(entry);
        self.refresh_hook_trap();
        self.fuel_consumed = 0;
        self.stack_grows = 0;
        self.frame_grows = 0;
        self.cold_steps = 0;
    }

    /// Install Moonseed's base library: all standard base functions,
    /// `_G`, and `_VERSION` (ADR 0031). The registry must have them
    /// (see [`crate::register_base`]). `print`
    /// writes to the output set with [`Runtime::set_output`].
    pub fn install_base(&mut self) -> Result<(), VmError> {
        let mut names: Vec<&str> = crate::base::BASE_FUNCTIONS
            .iter()
            .map(|(name, _)| *name)
            .collect();
        names.extend(["_G", "_VERSION"]);
        self.install_base_only(&names)
    }

    /// Install only the named parts of the base library: names from
    /// the standard base functions, `_G` (the globals table itself), and
    /// `_VERSION` (`"Lua 5.4"`). A sandbox leaves out what it does not
    /// want, such as `print` or `load`. Any other name is
    /// `VmError::UnknownNative`. Unstable API.
    pub fn install_base_only(&mut self, names: &[&str]) -> Result<(), VmError> {
        // Lua's base library is module `_G`: the globals table (ADR 0039).
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        self.register_module("_G", Value::Table(globals))?;
        for name in names {
            match *name {
                "_G" => {
                    let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
                    self.set_global(name, Value::Table(globals))?;
                }
                "_VERSION" => {
                    let version = Value::String(self.alloc_string(b"Lua 5.4".to_vec())?);
                    self.set_global(name, version)?;
                }
                _ => {
                    let (_, symbol) = crate::base::BASE_FUNCTIONS
                        .iter()
                        .find(|(global, _)| global == name)
                        .ok_or(VmError::UnknownNative)?;
                    self.set_global_native(name, symbol)?;
                }
            }
        }
        Ok(())
    }

    /// Where `print` writes from now on (ADR 0031). Not snapshot state: set
    /// it again on a restored runtime. Unstable API.
    pub fn set_output(&mut self, output: crate::host::Output) {
        self.output = Some(output);
    }

    /// Where warnings go (ADR 0049). Without a sink, warnings are still
    /// effects, and go nowhere. Unstable API.
    pub fn set_warnings(&mut self, warnings: crate::host::Warnings) {
        self.warnings = Some(warnings);
    }

    /// Send one warning, the pieces `pieces` gives, as one external
    /// effect: a replay of the same effect id sends nothing (ADR 0049).
    /// `pieces` writes each piece through the closure it is given, which
    /// passes it on with Lua's continuation flag.
    pub(crate) fn warn_effect(
        &mut self,
        journal: &mut Journal,
        pieces: impl FnOnce(&Heap, &mut dyn FnMut(&[u8])),
    ) {
        let effect = EffectId {
            domain: self.effect_domain,
            sequence: self.next_sequence,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        // A panicking sink leaves the runtime poisoned, as a native's would.
        let outer = std::mem::replace(&mut self.in_callback, true);
        let heap = &self.heap;
        let warnings = &mut self.warnings;
        let _ = journal.commit(effect, 1, || {
            let Some(sink) = warnings else {
                return 0;
            };
            // Hold each piece back one, so the last goes with `false`. A
            // piece ends at a zero byte, as Lua's C-string pieces do.
            let mut held: Option<Vec<u8>> = None;
            pieces(heap, &mut |piece| {
                let piece = piece.split(|byte| *byte == 0).next().unwrap_or_default();
                if let Some(previous) = held.replace(piece.to_vec()) {
                    sink(&previous, true);
                }
            });
            sink(held.as_deref().unwrap_or_default(), false);
            0
        });
        self.in_callback = outer;
    }

    /// Bind global `name` to the native function registered as `symbol`.
    /// Unstable API.
    pub fn set_global_native(&mut self, name: &str, symbol: &str) -> Result<(), VmError> {
        let value = self.native_value(symbol)?;
        self.set_global(name, value)
    }

    fn set_global(&mut self, name: &str, value: Value) -> Result<(), VmError> {
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        let key = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
        match index::set(&mut self.heap, Value::Table(globals), key, value) {
            Ok(_) => Ok(()),
            Err(LuaFault::Memory) => Err(VmError::MemoryLimit),
            Err(_) => Err(VmError::Corrupt),
        }
    }

    /// The Lua value for the native registered as `symbol`, interning the
    /// symbol on first use. Same symbol, same value.
    pub(crate) fn native_value(&mut self, symbol: &str) -> Result<Value, VmError> {
        let slot = self
            .registry
            .native_slot(symbol)
            .ok_or(VmError::UnknownNative)?;
        let index = match self.heap.natives.iter().position(|known| known == symbol) {
            Some(index) => index,
            None => {
                self.heap.natives.push(symbol.to_string());
                self.native_slots.push(slot);
                self.heap.natives.len() - 1
            }
        };
        Ok(Value::Native(
            u32::try_from(index).map_err(|_| VmError::Corrupt)?,
        ))
    }

    /// The `Call` a frame is stopped on: callee slot, wanted results,
    /// argument count, and the callee value.
    fn call_site(&self, thread: Handle<ThreadObj>) -> Result<(u32, u8, u32, Value), VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        // A boundary frame calls one function: the protected callee, which
        // wants every result, the message handler, which gives one, or what
        // a base function calls (ADR 0031). Its arguments end at `top`.
        if frame.boundary().is_some() {
            return self.boundary_call_site(thread);
        }
        // A metamethod's arguments end at `top` too: a native it tail-calls
        // takes over its call with arguments of its own (ADR 0029).
        if let Some(meta) = frame.meta() {
            let callee = object
                .stack
                .get(meta.slot as usize)
                .copied()
                .unwrap_or(Value::Nil);
            let passed = object.top.saturating_sub(meta.slot + 1);
            return Ok((meta.slot, 1, passed, callee));
        }
        let proto = self
            .heap
            .closures
            .get(frame.closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let op = *self
            .heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .ops
            .get(frame.pc as usize)
            .ok_or(VmError::Corrupt)?;
        let (func, nresults) = match op {
            Op::Call { func, nresults, .. } => (func, nresults),
            // Every Lua frame can call a native it tail-calls itself,
            // for the `Return` after it (ADR 0029).
            Op::TailCall { func, .. } => (func, COUNT_OPEN),
            _ => return Err(VmError::Corrupt),
        };
        let func_abs = frame.base + u32::from(func);
        // `call` set `top` past the arguments, including any that `__call`
        // inserted; `nargs` alone would miss those.
        let passed = object.top.saturating_sub(func_abs.saturating_add(1));
        let callee = object
            .stack
            .get(func_abs as usize)
            .copied()
            .unwrap_or(Value::Nil);
        Ok((func_abs, nresults, passed, callee))
    }

    /// Cold boundary windows stay out of the ordinary Lua call-site decoder.
    /// Extending their variants must not change its disabled-hook register use.
    #[cold]
    #[inline(never)]
    fn boundary_call_site(
        &self,
        thread: Handle<ThreadObj>,
    ) -> Result<(u32, u8, u32, Value), VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        let boundary = frame.boundary().ok_or(VmError::Corrupt)?;
        let (slot, nresults) = match boundary {
            Boundary::Hook { func, .. } => (*func, 0),
            Boundary::HookNative { func, .. } => (*func, COUNT_OPEN),
            Boundary::Protect { func, .. } => (func + 1, COUNT_OPEN),
            Boundary::Handler { slot, .. } => (*slot, 1),
            Boundary::Builtin {
                func, passed, task, ..
            } => (func + 1 + passed + task.scratch(), task.wants()),
            Boundary::Finalizer { func, .. } => (*func, 0),
            Boundary::Native {
                func, passed, kept, ..
            } => (func + 1 + passed + kept, COUNT_OPEN),
        };
        let callee = object
            .stack
            .get(slot as usize)
            .copied()
            .unwrap_or(Value::Nil);
        Ok((slot, nresults, object.top.saturating_sub(slot + 1), callee))
    }

    /// `Call` on a native function. A VM-local native runs now. An external
    /// one gets an effect id and stops in `NativePrepared`, a safe point
    /// before it runs, like `CallHost`; `poll` runs it without charging
    /// again.
    #[inline(never)]
    fn call_native(&mut self, index: u32, journal: &mut Journal) -> Result<Poll, VmError> {
        self.call_native_at(index, None, journal)
    }

    /// Ordinary Call already decoded this window. Other native entry points
    /// retain call_site's boundary, tail-call and metamethod handling.
    fn call_native_at(
        &mut self,
        index: u32,
        site: Option<(u32, u8, u32)>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        if self.hook_trap && self.hooks_allowed() && !self.hook_native_running() {
            return self.enter_hook_native(index, site);
        }
        count!("native_calls");
        let slot = *self
            .native_slots
            .get(index as usize)
            .ok_or(VmError::Corrupt)?;
        let entry = self.registry.native(slot).ok_or(VmError::Corrupt)?;
        if let Some(builtin) = entry.builtin {
            if let Some((func, nresults, passed)) = site {
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                // Gate Q's Off mode exercises the original builtin path.
                #[cfg(any(test, debug_assertions))]
                let iterator_enabled = self.hot_core != HotCoreMode::Off;
                #[cfg(not(any(test, debug_assertions)))]
                let iterator_enabled = true;
                if iterator_enabled
                    && matches!(
                        builtin,
                        crate::host::Builtin::Next | crate::host::Builtin::IpairsNext
                    )
                    && let Some((values, produced)) =
                        self.immediate_iterator(builtin, active, func, passed)?
                {
                    count!("builtin_calls");
                    return self.immediate_iterator_returned(
                        active,
                        func,
                        nresults,
                        passed,
                        &values[..produced],
                    );
                }
                // `Off` (debug proofs) compares these with the slow path too.
                if iterator_enabled
                    && let Some(value) = self.immediate_builtin(builtin, active, func, passed)?
                {
                    count!("builtin_calls");
                    return self.immediate_returned(active, func, nresults, passed, value);
                }
            }
            return self.call_builtin(builtin, site, journal);
        }
        match entry.policy {
            NativePolicy::External => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                let frame = self
                    .heap
                    .threads
                    .get_mut(active)
                    .ok_or(VmError::Corrupt)?
                    .frames
                    .last_mut()
                    .ok_or(VmError::Corrupt)?;
                match frame.meta_mut() {
                    Some(meta) => meta.phase = MetaPhase::NativePrepared { sequence },
                    None => frame.set_pending(
                        Some(Pending::NativePrepared { sequence }),
                        &mut self.cold_spare,
                    ),
                }
                Ok(Poll::Continue)
            }
            NativePolicy::VmLocal => {
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                self.run_native(active, None, journal)
            }
        }
    }

    /// Run the native the active frame's `Call` names, with the arguments
    /// still in their registers, and deliver its results through the
    /// call's result window.
    fn run_native(
        &mut self,
        active: Handle<ThreadObj>,
        effect: Option<EffectId>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("lua_to_host_calls");
        let (func_abs, nresults, passed, callee) = self.call_site(active)?;
        let Value::Native(index) = self.callable(callee) else {
            return Err(VmError::Corrupt);
        };
        let slot = *self
            .native_slots
            .get(index as usize)
            .ok_or(VmError::Corrupt)?;
        let entry = self.registry.native(slot).ok_or(VmError::Corrupt)?;
        if let Some(callback) = entry.typed.clone() {
            return self.run_typed(
                active, callee, func_abs, nresults, passed, callback, effect, journal,
            );
        }
        if let Some(callback) = entry.callback.clone() {
            return self.run_callback(
                active, callee, func_abs, nresults, passed, callback, effect, None, journal,
            );
        }
        let mut args = std::mem::take(&mut self.native_args);
        let mut results = std::mem::take(&mut self.native_results);
        args.clear();
        results.clear();
        {
            let stack = &self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack;
            args.extend((0..passed).map(|offset| {
                stack
                    .get((func_abs + 1 + offset) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            }));
        }
        let out_of_memory;
        let native_error;
        let outcome = {
            let mut call = NativeCall {
                heap: &mut self.heap,
                args: &args,
                results: &mut results,
                effect,
                journal: effect.map(|_| &mut *journal),
                out_of_memory: false,
                registry: &self.registry,
                max_objects: self.max_objects,
                error: None,
            };
            let outcome = (entry.function)(&mut call);
            out_of_memory = call.out_of_memory;
            native_error = call.error;
            outcome
        };
        args.clear();
        self.native_args = args;
        match outcome {
            NativeOutcome::Ready => {
                let returned = self.native_returned(active, func_abs, nresults, passed, &results);
                let mut results = results;
                results.clear();
                self.native_results = results;
                returned
            }
            NativeOutcome::Pending(key) => {
                if self.wait_index.contains_key(&key.raw())
                    || self.completed_waits.contains(&key.raw())
                {
                    return Err(VmError::Api(crate::ApiError::InvalidCallState));
                }
                let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
                let sequence = effect.map(|effect| effect.sequence);
                match frame.meta_mut() {
                    Some(meta) => {
                        meta.phase = MetaPhase::NativeWaiting {
                            sequence,
                            wait_key: key.raw(),
                        };
                    }
                    None => {
                        frame.set_pending(
                            Some(Pending::NativeWaiting {
                                sequence,
                                wait_key: key.raw(),
                            }),
                            &mut self.cold_spare,
                        );
                    }
                }
                count!("host_waits");
                thread.status = Status::Waiting;
                self.index_wait(key.raw(), active)?;
                self.last_completed_wait = None;
                Ok(Poll::Continue)
            }
            // The unwind removes the frame and anything it was waiting on.
            NativeOutcome::Fault if out_of_memory => Ok(self.fault(LuaFault::Memory)),
            NativeOutcome::Fault => match native_error {
                Some(crate::host::NativeError::Argument(arg, message)) => {
                    let ctx = library::Ctx {
                        active,
                        func: func_abs,
                        passed,
                        framed: false,
                    };
                    let (name, method) = self.argument_name(&ctx);
                    let text = library::arg_error_text(
                        &name,
                        method,
                        u32::try_from(arg).unwrap_or(u32::MAX - 1),
                        &message,
                    );
                    Ok(self.library_error(active, LuaFault::Argument, text))
                }
                Some(crate::host::NativeError::Message(text, true)) => {
                    Ok(self.library_error(active, LuaFault::Native, text))
                }
                Some(crate::host::NativeError::Message(text, false)) => {
                    Ok(self.fault_text(LuaFault::Native, text))
                }
                None => Ok(self.fault(LuaFault::Native)),
            },
        }
    }

    /// The common one-result ordinary Call can place its scalar and clear
    /// arguments with one stack borrow. Keep the general return path for
    /// open/padded results, boundary calls, and windows requiring growth.
    fn immediate_returned(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        nresults: u8,
        passed: u32,
        value: Value,
    ) -> Result<Poll, VmError> {
        let end = func + 1;
        let scratch_end = end + passed;
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        if nresults != 1
            || frame
                .cold
                .as_ref()
                .is_some_and(|cold| cold.boundary.is_some() || cold.meta.is_some())
            || scratch_end as usize > object.stack.len()
        {
            return self.native_returned(active, func, nresults, passed, &[value]);
        }
        // Even an existing slot must observe the stack's error reserve.
        if let Some(fault) = self.slot_fault(active, end)? {
            return Ok(self.fault(fault));
        }
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        object.stack.values[func as usize] = value;
        object.stack.values[end as usize..scratch_end as usize].fill(Value::Nil);
        object.stack.truncate(frame.limit.max(end) as usize);
        object.top = end;
        frame.clear_pending(&mut self.cold_spare);
        frame.pc = frame.pc.saturating_add(1);
        Ok(Poll::Continue)
    }

    /// The usual iterator result window has two slots and already exists.
    /// Retain general delivery for open/padded results and boundary frames.
    /// A final nil still produces one value, even when this window pads it.
    fn immediate_iterator_returned(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        nresults: u8,
        passed: u32,
        values: &[Value],
    ) -> Result<Poll, VmError> {
        let end = func + 2;
        let scratch_end = (func + 1 + passed).max(end);
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        if nresults != 2
            || frame
                .cold
                .as_ref()
                .is_some_and(|cold| cold.boundary.is_some() || cold.meta.is_some())
            || scratch_end as usize > object.stack.len()
        {
            return self.native_returned(active, func, nresults, passed, values);
        }
        if let Some(fault) = self.slot_fault(active, end)? {
            return Ok(self.fault(fault));
        }
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        object.stack.values[func as usize] = values[0];
        object.stack.values[func as usize + 1] = values.get(1).copied().unwrap_or(Value::Nil);
        object.stack.values[end as usize..scratch_end as usize].fill(Value::Nil);
        object.stack.truncate(frame.limit.max(end) as usize);
        object.top = end;
        frame.clear_pending(&mut self.cold_spare);
        frame.pc = frame.pc.saturating_add(1);
        Ok(Poll::Continue)
    }

    /// A native call made at `func_abs` returns `values`: they go through
    /// the call's result window, and the frame that made the call moves
    /// on. A metamethod call commits; a boundary frame finishes in its own
    /// step.
    fn native_returned(
        &mut self,
        active: Handle<ThreadObj>,
        func_abs: u32,
        nresults: u8,
        passed: u32,
        values: &[Value],
    ) -> Result<Poll, VmError> {
        if self.hook_trap
            && self.capture_hook_native_return(active, func_abs, nresults, passed, values)?
        {
            return Ok(Poll::Continue);
        }
        let produced = u32::try_from(values.len()).unwrap_or(u32::MAX);
        let end = func_abs.saturating_add(Self::wanted(nresults, produced));
        if let Some(fault) = self.slot_fault(active, end)? {
            return Ok(self.fault(fault));
        }
        self.deliver_native(active, func_abs, nresults, values, func_abs + 1 + passed)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        if let Some(meta) = frame.meta_mut() {
            meta.phase = MetaPhase::Running;
            self.commit_meta(active)?;
        } else {
            frame.clear_pending(&mut self.cold_spare);
            if frame.boundary().is_none() {
                frame.pc = frame.pc.saturating_add(1);
            }
        }
        Ok(Poll::Continue)
    }

    /// Write a native's results at the callee slot, adjusted to the call's
    /// wanted count, then close the window the same way a Lua return does.
    fn deliver_native(
        &mut self,
        thread: Handle<ThreadObj>,
        func_abs: u32,
        nresults: u8,
        values: &[Value],
        scratch_end: u32,
    ) -> Result<(), VmError> {
        let produced = u32::try_from(values.len()).map_err(|_| VmError::Corrupt)?;
        let want = Self::wanted(nresults, produced);
        for offset in 0..want {
            let value = values.get(offset as usize).copied().unwrap_or(Value::Nil);
            self.write_abs(thread, func_abs + offset, value)?;
        }
        self.finish_result_window(thread, func_abs, nresults, produced, scratch_end)
    }

    #[inline(never)]
    fn do_return(&mut self, base: u8, count: u8, journal: &mut Journal) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        self.close_upvalues(0)?;
        let (src, produced) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let src = thread.frames.last().ok_or(VmError::Corrupt)?.base + u32::from(base);
            (src, self.span_len(thread.top, src, count))
        };
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        // To-be-closed values close first, newest first, while the results
        // wait in their registers (ADR 0026).
        if thread.tbc.last().is_some_and(|slot| *slot >= frame.base) {
            frame.set_meta(
                idle_close(frame.base, CloseNext::Return { src, produced }),
                &mut self.cold_spare,
            );
            return self.close_step(active, journal);
        }
        self.return_values(active, src, produced)
    }

    /// Return the `produced` values at `src` from the active frame.
    #[inline]
    fn return_values(
        &mut self,
        active: Handle<ThreadObj>,
        src: u32,
        produced: u32,
    ) -> Result<Poll, VmError> {
        if self.hook_trap && self.queue_lua_return(active, src, produced)? {
            return Ok(Poll::Continue);
        }
        let (mode, dest, is_last) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            (
                frame.nresults,
                frame.base.saturating_sub(frame.vararg_len + 1),
                thread.frames.len() == 1,
            )
        };
        if is_last {
            let resumed_by = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .resumed_by;
            if let Some(parent) = resumed_by {
                let values = self.read_span(active, src, produced, COUNT_OPEN)?;
                return self.deliver_to_parent(active, parent, values, Status::Completed);
            }
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            thread.host_results.clear();
            for offset in 0..Self::wanted(mode, produced) {
                let value = if offset < produced {
                    thread
                        .stack
                        .get((src + offset) as usize)
                        .copied()
                        .unwrap_or(Value::Nil)
                } else {
                    Value::Nil
                };
                thread.host_results.push(value);
            }
            thread.frames.clear();
            thread.status = Status::Completed;
            return Ok(Poll::Stop(StepOutcome::Completed));
        }
        let scratch_end = {
            let frame = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .frames
                .last()
                .ok_or(VmError::Corrupt)?;
            frame.limit
        };
        self.place_results(active, src, produced, dest, mode)?;
        {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            thread.frames.pop();
        }
        self.finish_result_window(active, dest, mode, produced, scratch_end)?;
        if self.hook_trap {
            self.correct_hook_cursor(active, true)?;
        }
        Ok(Poll::Continue)
    }

    fn do_yield(&mut self, base: u8, count: u8) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        // A message handler cannot yield, as in Lua, nor can a close that
        // `CloseThread` runs. Pausing is still fine.
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        // Nor can a base function called without a continuation: all but
        // `pairs` (ADR 0031).
        if coroutine::blocks_yield(thread) || thread.closing {
            return Ok(self.fault(LuaFault::YieldAcross));
        }
        let (src, produced) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            let src = frame.base + u32::from(base);
            (src, self.span_len(thread.top, src, count))
        };
        self.advance_pc()?;
        let parent = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .resumed_by;
        if let Some(parent) = parent {
            let values = self.read_span(active, src, produced, COUNT_OPEN)?;
            self.deliver_to_parent(active, parent, values, Status::LuaSuspended)
        } else {
            let values = self.read_span(active, src, produced, COUNT_OPEN)?;
            let child = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            child.host_results = values;
            child.status = Status::LuaSuspended;
            Ok(Poll::Stop(StepOutcome::LuaYielded))
        }
    }

    fn do_resume(&mut self, dest: u8, thread: u8, nresults: u8) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let value = self.load(thread)?;
        let Value::Thread(child) = value else {
            return Ok(self.fault(LuaFault::Type));
        };
        let status = self.heap.threads.get(child).ok_or(VmError::Corrupt)?.status;
        if status != Status::LuaSuspended {
            return Ok(self.fault(LuaFault::ResumeState));
        }
        {
            let child_obj = self.heap.threads.get_mut(child).ok_or(VmError::Corrupt)?;
            child_obj.status = Status::Ready;
            child_obj.resumed_by = Some(active);
        }
        self.set_pending(Pending::Resuming {
            child,
            dest,
            nresults,
        })?;
        self.heap.active = Some(child);
        self.refresh_hook_trap();
        // A coroutine `coroutine.create` made starts with no arguments.
        if self
            .heap
            .threads
            .get(child)
            .ok_or(VmError::Corrupt)?
            .frames
            .is_empty()
        {
            return self.start_coroutine(child, &[]);
        }
        Ok(Poll::Continue)
    }

    /// `MarkClose`: nil and false are ignored; any other value needs a
    /// `__close` metamethod now, and joins the to-be-closed list. The
    /// metamethod itself is looked up again when the value closes.
    fn mark_close(&mut self, reg: u8) -> Result<Poll, VmError> {
        let value = self.load(reg)?;
        if !value.truthy() {
            return self.next_op();
        }
        if index::metamethod(&self.heap, value, b"__close").is_none() {
            return Ok(self.fault(LuaFault::Close));
        }
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let slot = thread.frames.last().ok_or(VmError::Corrupt)?.base + u32::from(reg);
        // Declaration order is register order, and a scope's values close
        // before its registers are reused.
        if thread.tbc.last().is_some_and(|last| *last >= slot) {
            return Err(VmError::Corrupt);
        }
        thread.tbc.push(slot);
        self.next_op()
    }

    /// `GenericForLoop`: the iterator call left the loop variables at
    /// `base + 4`. A nil first one ends the loop; any other value, false
    /// included, becomes the hidden control at `base + 2`, and the body runs
    /// again. The loop variable itself stays the body's to change.
    fn generic_for_loop(&mut self, base: u8, offset: i32) -> Result<Poll, VmError> {
        let first_reg = base.checked_add(4).ok_or(VmError::Corrupt)?;
        let control_reg = base.checked_add(2).ok_or(VmError::Corrupt)?;
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        let first_abs = frame.base + u32::from(first_reg);
        let first = thread
            .stack
            .get(first_abs as usize)
            .copied()
            .unwrap_or(Value::Nil);
        if matches!(first, Value::Nil) {
            return self.next_op();
        }
        // A present first result proves the lower control slot is live.
        let control_abs = frame.base + u32::from(control_reg);
        thread.stack.values[control_abs as usize] = first;
        frame.pc = jump_target(frame.pc, offset)?;
        Ok(Poll::Continue)
    }

    /// `CloseScope`: close the open upvalues at or above register `from`,
    /// then, when to-be-closed values remain there, start closing them.
    /// Their calls run in later steps; the instruction ends with the last.
    fn close_scope(&mut self, from: u8, journal: &mut Journal) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let from = thread.frames.last().ok_or(VmError::Corrupt)?.base + u32::from(from);
        self.close_open_upvalues(active, from)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        if !thread.tbc.last().is_some_and(|slot| *slot >= from) {
            return self.next_op();
        }
        thread
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?
            .set_meta(idle_close(from, CloseNext::Advance), &mut self.cold_spare);
        self.close_step(active, journal)
    }

    /// `CloseThread`: close a suspended or failed coroutine, as
    /// `coroutine.close` does. The coroutine runs as if resumed: an unwind
    /// with no target pops its frames, closing their values with its error,
    /// or nil, and then answers here. A dead coroutine answers `true` at
    /// once; a running one is an error.
    fn close_thread(&mut self, dst: u8, thread: u8) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let Value::Thread(child) = self.load(thread)? else {
            return Ok(self.fault(LuaFault::Type));
        };
        let object = self.heap.threads.get(child).ok_or(VmError::Corrupt)?;
        let fresh = object.frames.is_empty() && child != active;
        let status = object.status;
        match status {
            Status::Completed => {
                self.store(dst, Value::Bool(true))?;
                self.store(dst.saturating_add(1), Value::Nil)?;
                return self.next_op();
            }
            // A coroutine never resumed has nothing to close.
            Status::LuaSuspended if fresh => {
                let object = self.heap.threads.get_mut(child).ok_or(VmError::Corrupt)?;
                object.stack.clear();
                object.top = 0;
                object.status = Status::Completed;
                self.store(dst, Value::Bool(true))?;
                self.store(dst.saturating_add(1), Value::Nil)?;
                return self.next_op();
            }
            Status::LuaSuspended | Status::Failed if child != active => {}
            _ if child == active => return Ok(self.fault(LuaFault::CloseRunning)),
            // Ready or waiting but not active: it resumed the running one.
            _ => return Ok(self.fault(LuaFault::CloseNormal)),
        }
        let object = self.heap.threads.get_mut(child).ok_or(VmError::Corrupt)?;
        let error = object.error.take();
        object.status = Status::Ready;
        object.closing = true;
        object.resumed_by = Some(active);
        object.unwind = Some(Box::new(Unwind {
            error,
            phase: UnwindPhase::Popping { target: None },
        }));
        let id = self.heap.threads.get(child).ok_or(VmError::Corrupt)?.id;
        if let Some(hook) = self.heap.hooks.get_mut(id) {
            hook.hook_yield = false;
        }
        self.set_pending(Pending::Resuming {
            child,
            dest: dst,
            nresults: 2,
        })?;
        self.heap.active = Some(child);
        self.refresh_hook_trap();
        Ok(Poll::Continue)
    }

    #[cfg(test)]
    pub(crate) fn entry_field_integer(
        &self,
        table_reg: u8,
        key: i64,
    ) -> Result<Option<i64>, VmError> {
        let Value::Table(handle) = self.entry_slot(table_reg)? else {
            return Err(VmError::Corrupt);
        };
        let normalized = self
            .heap
            .normalize_value(Value::Integer(key))
            .map_err(|_| VmError::Corrupt)?;
        match self
            .heap
            .table_get(handle, &normalized)
            .ok_or(VmError::Corrupt)?
        {
            Value::Nil => Ok(None),
            Value::Integer(value) => Ok(Some(value)),
            _ => Err(VmError::Corrupt),
        }
    }

    pub(crate) fn entry_slot(&self, reg: u8) -> Result<Value, VmError> {
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        Ok(thread
            .stack
            .get(usize::from(reg))
            .copied()
            .unwrap_or(Value::Nil))
    }

    #[cfg(test)]
    pub(crate) fn slot_object_id(&self, reg: u8) -> Result<Option<ObjectId>, VmError> {
        Ok(self.heap.object_id_of_value(self.entry_slot(reg)?))
    }

    /// Stores still waiting in the active assignment. `Some(n)` at the
    /// pre-store boundary means all `n` destinations are pending.
    pub(crate) fn assign_remaining(&self) -> Option<u16> {
        let thread = self.heap.active?;
        let frame = self.heap.threads.get(thread)?.frames.last()?;
        match frame.pending() {
            Some(Pending::Assigning { next, .. }) if *next > 0 => Some(*next),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn assign_target_id(&self, index: usize) -> Option<ObjectId> {
        let thread = self.heap.active?;
        let frame = self.heap.threads.get(thread)?.frames.last()?;
        let target = frame.targets().get(index)?;
        match target {
            AssignTarget::Field { key, .. } => self.heap.object_id_of_value(*key),
            AssignTarget::Register(_) => None,
        }
    }

    fn span_len(&self, top: u32, start: u32, count: u8) -> u32 {
        if count == COUNT_OPEN {
            top.saturating_sub(start)
        } else {
            u32::from(count)
        }
    }

    fn wanted(mode: u8, produced: u32) -> u32 {
        if mode == COUNT_OPEN {
            produced
        } else {
            u32::from(mode)
        }
    }

    fn read_span(
        &self,
        thread: Handle<ThreadObj>,
        src: u32,
        produced: u32,
        mode: u8,
    ) -> Result<Vec<Value>, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let want = Self::wanted(mode, produced);
        let mut values = Vec::with_capacity(want as usize);
        for offset in 0..want {
            let value = if offset < produced {
                object
                    .stack
                    .get((src + offset) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            };
            values.push(value);
        }
        Ok(values)
    }

    fn transfer_span(
        &mut self,
        src_thread: Handle<ThreadObj>,
        src: u32,
        produced: u32,
        dst_thread: Handle<ThreadObj>,
        dest: u32,
        mode: u8,
    ) -> Result<(), VmError> {
        let want = Self::wanted(mode, produced);
        let copy_n = want.min(produced);
        let ascending = dest <= src || src_thread != dst_thread;
        let copy_one = |this: &mut Self, offset: u32| -> Result<(), VmError> {
            let value = this
                .heap
                .threads
                .get(src_thread)
                .ok_or(VmError::Corrupt)?
                .stack
                .get((src + offset) as usize)
                .copied()
                .unwrap_or(Value::Nil);
            this.write_abs(dst_thread, dest + offset, value)
        };
        if ascending {
            for offset in 0..copy_n {
                copy_one(self, offset)?;
            }
        } else {
            for offset in (0..copy_n).rev() {
                copy_one(self, offset)?;
            }
        }
        for offset in copy_n..want {
            self.write_abs(dst_thread, dest + offset, Value::Nil)?;
        }
        Ok(())
    }

    fn place_results(
        &mut self,
        thread: Handle<ThreadObj>,
        src: u32,
        produced: u32,
        dest: u32,
        mode: u8,
    ) -> Result<(), VmError> {
        if Self::wanted(mode, produced) == 0 {
            return Ok(());
        }
        self.transfer_span(thread, src, produced, thread, dest, mode)
    }

    fn finish_result_window(
        &mut self,
        thread: Handle<ThreadObj>,
        dest: u32,
        mode: u8,
        produced: u32,
        scratch_end: u32,
    ) -> Result<(), VmError> {
        let want = Self::wanted(mode, produced);
        let clear_from = if want == 0 { dest + 1 } else { dest + want };
        let new_top = if want == 0 { dest } else { dest + want };
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        // The caller keeps its registers and its varargs above them. A
        // boundary frame owns neither, so a call returning into one keeps
        // those of the Lua frame below it.
        let limit = object
            .frames
            .iter()
            .rev()
            .find(|frame| frame.boundary().is_none())
            .map_or(0, |frame| frame.limit);
        let clear_end = scratch_end.max(clear_from);
        grow_stack(object, clear_end as usize, &mut self.heap.gc);
        // Only the callee's scratch dies. Live registers above that scratch stay.
        for slot in clear_from..clear_end {
            object.stack[slot as usize] = Value::Nil;
        }
        let keep = limit.max(new_top);
        if object.stack.len() > keep as usize {
            object.stack.truncate(keep as usize);
        }
        object.top = new_top;
        Ok(())
    }

    fn vararg_len(&self) -> Result<u32, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?;
        Ok(frame.vararg_len)
    }

    /// `Vararg`: copy the frame's extra arguments, which sit below its
    /// registers, to `dst`, padded with nil to `count`, or all of them for
    /// `COUNT_OPEN`, which sets `top` past the last.
    fn copy_varargs(&mut self, dst: u8, count: u8) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (base, extra) = {
            let frame = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .frames
                .last()
                .ok_or(VmError::Corrupt)?;
            (frame.base, frame.vararg_len)
        };
        let n = if count == COUNT_OPEN {
            extra
        } else {
            u32::from(count)
        };
        let from = base - extra;
        let dest = base + u32::from(dst);
        let end = dest.saturating_add(n.max(1));
        if let Some(fault) = self.slot_fault(active, end)? {
            return Ok(self.fault(fault));
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        grow_stack(thread, end as usize, &mut self.heap.gc);
        // The extras are below `dest`, so an ascending copy is safe.
        for offset in 0..n {
            thread.stack[(dest + offset) as usize] = if offset < extra {
                thread.stack[(from + offset) as usize]
            } else {
                Value::Nil
            };
        }
        if count == COUNT_OPEN {
            thread.top = dest + extra;
        }
        self.next_op()
    }

    fn open_len(&self, from: u8) -> Result<i64, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
        let start = frame.base + u32::from(from);
        let len = thread.top.saturating_sub(start);
        Ok(i64::from(len))
    }

    fn push_target(&mut self, target: AssignTarget) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?;
        frame.cold_mut(&mut self.cold_spare).targets.push(target);
        Ok(())
    }

    fn push_local_target(&mut self, reg: u8) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let slot = {
            let frame = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .frames
                .last()
                .ok_or(VmError::Corrupt)?;
            frame.base + u32::from(reg)
        };
        self.push_target(AssignTarget::Register(slot))
    }

    fn push_field_target(&mut self, table: u8, key: u8) -> Result<(), VmError> {
        let table = self.load(table)?;
        let key = self.load(key)?;
        self.push_target(AssignTarget::Field { table, key })
    }

    fn begin_assign(&mut self, src: u8, n: u8) -> Result<(), VmError> {
        if n == COUNT_OPEN {
            return Err(VmError::Corrupt);
        }
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last_mut()
            .ok_or(VmError::Corrupt)?;
        let ndest = frame.targets().len();
        let ndest = u16::try_from(ndest).map_err(|_| VmError::Corrupt)?;
        if ndest == 0 {
            frame.pc = frame.pc.saturating_add(1);
            return Ok(());
        }
        frame.set_pending(
            Some(Pending::Assigning {
                src: frame.base + u32::from(src),
                nvalues: u16::from(n),
                next: ndest,
            }),
            &mut self.cold_spare,
        );
        Ok(())
    }

    fn perform_assign_store(&mut self, journal: &mut Journal) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (value, target) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            let (src, nvalues, next) = match frame.pending() {
                Some(Pending::Assigning { src, nvalues, next }) => (*src, *nvalues, *next),
                _ => return Err(VmError::Corrupt),
            };
            if next == 0 {
                return Err(VmError::Corrupt);
            }
            let index = next - 1;
            let target = frame
                .targets()
                .get(usize::from(index))
                .ok_or(VmError::Corrupt)?
                .clone();
            let value = if u32::from(index) < u32::from(nvalues) {
                thread
                    .stack
                    .get((src + u32::from(index)) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            };
            (value, target)
        };
        match target {
            AssignTarget::Register(slot) => self.write_abs(active, slot, value)?,
            AssignTarget::Field { table, key } => {
                match self.set_value(table, key, value) {
                    Ok(Resolved::Done(_)) => {}
                    Ok(Resolved::Call { function, target }) => {
                        // The cursor moves when the metamethod's call commits.
                        return self.call_meta(
                            MetaEvent::NewIndexAssign,
                            function,
                            &[target, key, value],
                            journal,
                        );
                    }
                    Err(fault) => return Ok(self.fault(fault)),
                }
            }
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        let finished = match frame.pending_mut() {
            Some(Pending::Assigning { next, .. }) => {
                *next = next.saturating_sub(1);
                *next == 0
            }
            _ => return Err(VmError::Corrupt),
        };
        if finished {
            frame.set_pending(None, &mut self.cold_spare);
            frame.clear_targets(&mut self.cold_spare);
            frame.pc = frame.pc.saturating_add(1);
        }
        Ok(Poll::Continue)
    }

    fn make_lua_closure(&mut self, child: u32) -> Result<Value, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (parent_closure, parent_base) = {
            let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
            let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
            (frame.closure, frame.base)
        };
        let parent_proto = self
            .heap
            .closures
            .get(parent_closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let child_proto = *self
            .heap
            .protos
            .get(parent_proto)
            .ok_or(VmError::Corrupt)?
            .children
            .get(child as usize)
            .ok_or(VmError::Corrupt)?;
        let captures = self
            .heap
            .protos
            .get(child_proto)
            .ok_or(VmError::Corrupt)?
            .captures
            .clone();
        let parent_upvalues = self
            .heap
            .closures
            .get(parent_closure)
            .ok_or(VmError::Corrupt)?
            .upvalues
            .clone();
        let count = u32::try_from(captures.len()).map_err(|_| VmError::Corrupt)?;
        self.make_room(
            count + 1,
            cost::OBJECT * u64::from(count + 1) + cost::REF * u64::from(count),
        );
        let mut upvalues = Vec::with_capacity(captures.len());
        for capture in captures {
            let upvalue = match capture {
                Capture::Local(slot) => {
                    let absolute = parent_base + u32::from(slot);
                    if let Some(existing) = self.find_open(active, absolute)? {
                        existing
                    } else {
                        let created = self.alloc_upvalue(UpvalueState::Open {
                            thread: active,
                            slot: absolute,
                        })?;
                        self.heap
                            .threads
                            .get_mut(active)
                            .ok_or(VmError::Corrupt)?
                            .push_open(absolute, created);
                        created
                    }
                }
                Capture::Upvalue(index) => *parent_upvalues
                    .get(index as usize)
                    .ok_or(VmError::Corrupt)?,
            };
            upvalues.push(upvalue);
        }
        let closure = self.alloc_closure(child_proto, upvalues)?;
        Ok(Value::Closure(closure))
    }

    fn find_open(
        &self,
        thread: Handle<ThreadObj>,
        slot: u32,
    ) -> Result<Option<Handle<crate::heap::UpvalueObj>>, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        Ok(object
            .open_upvalues
            .iter()
            .find(|(open_slot, _)| *open_slot == slot)
            .map(|(_, handle)| *handle))
    }

    fn new_thread(&mut self, child: u32) -> Result<Value, VmError> {
        let proto = self.current_proto()?;
        let child_proto = *self
            .heap
            .protos
            .get(proto)
            .ok_or(VmError::Corrupt)?
            .children
            .get(child as usize)
            .ok_or(VmError::Corrupt)?;
        let captures = self
            .heap
            .protos
            .get(child_proto)
            .ok_or(VmError::Corrupt)?
            .captures
            .clone();
        if !captures.is_empty() {
            return Err(VmError::Corrupt);
        }
        self.make_room(2, 2 * cost::OBJECT + cost::THREAD);
        let closure = self.alloc_closure(child_proto, Vec::new())?;
        let thread = self.alloc_thread(closure, Status::LuaSuspended)?;
        Ok(Value::Thread(thread))
    }

    fn read_upvalue(&self, index: u8) -> Result<Value, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let closure = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?
            .closure;
        let upvalue = *self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .upvalues
            .get(index as usize)
            .ok_or(VmError::Corrupt)?;
        match self
            .heap
            .upvalues
            .get(upvalue)
            .ok_or(VmError::Corrupt)?
            .state
        {
            UpvalueState::Closed(value) => Ok(value),
            UpvalueState::Open { thread, slot } => {
                let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
                Ok(object
                    .stack
                    .get(slot as usize)
                    .copied()
                    .unwrap_or(Value::Nil))
            }
        }
    }

    fn write_upvalue(&mut self, index: u8, value: Value) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let closure = self
            .heap
            .threads
            .get(active)
            .ok_or(VmError::Corrupt)?
            .frames
            .last()
            .ok_or(VmError::Corrupt)?
            .closure;
        let upvalue = *self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .upvalues
            .get(index as usize)
            .ok_or(VmError::Corrupt)?;
        let state = self
            .heap
            .upvalues
            .get(upvalue)
            .ok_or(VmError::Corrupt)?
            .state
            .clone();
        match state {
            UpvalueState::Closed(_) => {
                self.heap
                    .upvalues
                    .get_mut(upvalue)
                    .ok_or(VmError::Corrupt)?
                    .state = UpvalueState::Closed(value);
            }
            UpvalueState::Open { thread, slot } => {
                self.write_abs(thread, slot, value)?;
            }
        }
        Ok(())
    }

    /// Close every open upvalue of the running frame whose register is `from`
    /// or above. `Return` passes 0 and closes the whole frame. The cell keeps
    /// its identity; only its state changes from `Open` to `Closed`. Work is
    /// linear in the thread's open list and allocates nothing.
    fn close_upvalues(&mut self, from: u8) -> Result<(), VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let min_slot = thread.frames.last().ok_or(VmError::Corrupt)?.base + u32::from(from);
        self.close_open_upvalues(active, min_slot)
    }

    /// Close every open upvalue of `active` at or above absolute slot
    /// `min_slot`: each cell keeps the register's current value.
    fn close_open_upvalues(
        &mut self,
        active: Handle<ThreadObj>,
        min_slot: u32,
    ) -> Result<(), VmError> {
        let heap = &mut self.heap;
        let thread = heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let mut closed_any = false;
        for &(slot, upvalue) in &thread.open_upvalues {
            if slot >= min_slot {
                let value = thread
                    .stack
                    .get(slot as usize)
                    .copied()
                    .unwrap_or(Value::Nil);
                if let Some(object) = heap.upvalues.get_mut(upvalue) {
                    object.state = UpvalueState::Closed(value);
                }
                closed_any = true;
            }
        }
        if closed_any && let Some(thread) = heap.threads.get_mut(active) {
            thread.close_open_from(min_slot);
        }
        Ok(())
    }

    fn lookup_str(
        &self,
        table: Handle<crate::heap::TableObj>,
        key: &[u8],
    ) -> Result<Value, VmError> {
        let object = self.heap.tables.get(table).ok_or(VmError::Corrupt)?;
        Ok(object
            .table
            .get_view(KeyView::string(key))
            .unwrap_or(Value::Nil))
    }

    /// A compiled chunk as a function value, not yet called (ADR 0031): its
    /// prototypes installed, and its one capture, `_ENV`, a closed cell
    /// holding `env`. Hand-built programs capture nothing. Collects first
    /// when the objects would not fit; nothing is held outside the roots.
    fn instantiate(
        &mut self,
        spec: &ProtoSpec,
        env: Value,
        name: ChunkName,
    ) -> Result<Handle<ClosureObj>, VmError> {
        fn size(spec: &ProtoSpec) -> (u32, u64) {
            spec.children.iter().map(size).fold(
                (
                    1 + spec.byte_consts.len() as u32,
                    cost::OBJECT * (1 + spec.byte_consts.len() as u64)
                        + spec
                            .byte_consts
                            .iter()
                            .map(|bytes| bytes.len() as u64)
                            .sum::<u64>()
                        + cost::REF
                            * (spec.ops.len()
                                + spec.byte_consts.len()
                                + spec.captures.len()
                                + spec.children.len()) as u64
                        + spec.debug.as_ref().map_or(0, |debug| debug.logical_size()),
                ),
                |(count, bytes), (more, extra)| (count.saturating_add(more), bytes + extra),
            )
        }
        let (mut count, mut bytes) = size(spec);
        // The chunk's name, when it is a new string.
        let name_bytes = match &name {
            ChunkName::Object(_) => None,
            ChunkName::Bytes(name) => Some(name.len()),
            ChunkName::Unnamed => spec
                .debug
                .as_ref()
                .and_then(|debug| debug.source.as_ref())
                .map(Vec::len),
        };
        if let Some(len) = name_bytes {
            count = count.saturating_add(1);
            bytes += cost::OBJECT + len as u64;
        }
        // The closure and its upvalues.
        let cells = 1 + spec.captures.len() as u32;
        self.make_room(count + cells, bytes + u64::from(cells) * cost::OBJECT);
        // The chunk's name: a string the caller holds, or a new one, made
        // after any collection so nothing unrooted is held across it.
        let source = match name {
            ChunkName::Object(handle) => Some(handle),
            ChunkName::Bytes(bytes) => Some(self.alloc_string(bytes)?),
            ChunkName::Unnamed => {
                match spec.debug.as_ref().and_then(|debug| debug.source.clone()) {
                    Some(bytes) => Some(self.alloc_string(bytes)?),
                    None => None,
                }
            }
        };
        let proto = self.install(spec, source)?;
        self.chunk_closure(proto, spec, env)
    }
}

/// What a new chunk's functions report as their source (ADR 0040).
pub(crate) enum ChunkName {
    /// The name in the prototypes' debug information, if any.
    Unnamed,
    /// A string the caller holds: `load`'s chunk name, or the chunk itself.
    Object(Handle<crate::heap::StringObj>),
    Bytes(Vec<u8>),
}

impl Runtime {
    /// [`Self::instantiate`] with the globals as `_ENV`, for
    /// `load_function`.
    pub(crate) fn instantiate_chunk(
        &mut self,
        chunk: &crate::CompiledChunk,
    ) -> Result<ObjectId, VmError> {
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        let closure = self.instantiate(&chunk.proto, Value::Table(globals), ChunkName::Unnamed)?;
        self.heap
            .object_id_of_value(Value::Closure(closure))
            .ok_or(VmError::Corrupt)
    }

    /// The closure of an installed chunk: its first upvalue, `_ENV` for a
    /// compiled chunk, a closed cell holding `env`.
    fn chunk_closure(
        &mut self,
        proto: Handle<Proto>,
        spec: &ProtoSpec,
        env: Value,
    ) -> Result<Handle<ClosureObj>, VmError> {
        // The first upvalue is `env`; a function from a binary chunk may
        // have more, which start nil, as Lua's `load` makes them.
        let mut upvalues = Vec::with_capacity(spec.captures.len());
        for index in 0..spec.captures.len() {
            let value = if index == 0 { env } else { Value::Nil };
            upvalues.push(self.alloc_upvalue(UpvalueState::Closed(value))?);
        }
        self.alloc_closure(proto, upvalues)
    }

    fn install(
        &mut self,
        spec: &ProtoSpec,
        source: Option<Handle<crate::heap::StringObj>>,
    ) -> Result<Handle<Proto>, VmError> {
        let mut children = Vec::with_capacity(spec.children.len());
        for child in &spec.children {
            children.push(self.install(child, source)?);
        }
        let mut const_strings = Vec::with_capacity(spec.byte_consts.len());
        for bytes in &spec.byte_consts {
            const_strings.push(self.alloc_string(bytes.clone())?);
        }
        let bytes = cost::OBJECT
            + cost::REF
                * (spec.ops.len() + spec.byte_consts.len() + spec.captures.len() + children.len())
                    as u64
            + spec.debug.as_ref().map_or(0, |debug| debug.logical_size());
        self.ensure_room(bytes)?;
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        self.heap.gc.charge(bytes);
        // The heap keeps the chunk's name once, as `source`.
        let debug = spec.debug.as_ref().map(|debug| {
            let mut debug = debug.clone();
            debug.source = None;
            debug
        });
        self.heap
            .protos
            .alloc(Proto {
                id,
                ops: spec.ops.clone(),
                field_hints: Proto::empty_field_hints(&spec.ops),
                byte_consts: spec.byte_consts.clone(),
                byte_hashes: const_strings
                    .iter()
                    .map(|handle| {
                        self.heap
                            .strings
                            .get(*handle)
                            .expect("installed constant")
                            .hash()
                    })
                    .collect(),
                const_strings,
                captures: spec.captures.clone(),
                children,
                max_reg: spec.max_reg,
                params: spec.params,
                vararg: spec.vararg,
                debug,
                source,
            })
            .map_err(VmError::from)
    }

    fn alloc_closure(
        &mut self,
        proto: Handle<Proto>,
        upvalues: Vec<Handle<crate::heap::UpvalueObj>>,
    ) -> Result<Handle<ClosureObj>, VmError> {
        let bytes = cost::OBJECT + cost::REF * upvalues.len() as u64;
        self.ensure_room(bytes)?;
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        self.heap.gc.charge(bytes);
        self.heap
            .closures
            .alloc(ClosureObj {
                id,
                proto,
                upvalues,
            })
            .map_err(VmError::from)
    }

    fn alloc_upvalue(
        &mut self,
        state: UpvalueState,
    ) -> Result<Handle<crate::heap::UpvalueObj>, VmError> {
        self.ensure_room(cost::OBJECT)?;
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        self.heap.gc.charge(cost::OBJECT);
        self.heap
            .upvalues
            .alloc(crate::heap::UpvalueObj { id, state })
            .map_err(VmError::from)
    }

    fn alloc_thread(
        &mut self,
        closure: Handle<ClosureObj>,
        status: Status,
    ) -> Result<Handle<ThreadObj>, VmError> {
        let inherited = self.inherited_hook();
        let hook_bytes = if inherited.is_some() {
            hooks::HOOK_BYTES
        } else {
            0
        };
        self.ensure_room(cost::THREAD + hook_bytes)?;
        let max_reg = {
            let proto = self
                .heap
                .closures
                .get(closure)
                .ok_or(VmError::Corrupt)?
                .proto;
            self.heap.protos.get(proto).ok_or(VmError::Corrupt)?.max_reg
        };
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        let limit = u32::from(max_reg);
        let thread = ThreadObj {
            id,
            status,
            stack: vec![Value::Nil; usize::from(max_reg)].into(),
            top: limit,
            frames: vec![Frame {
                closure,
                pc: 0,
                base: 0,
                limit,
                // The host sees every value the chunk returns. Zero would discard them.
                nresults: COUNT_OPEN,
                vararg_len: 0,
                flags: 0,
                cold: None,
            }]
            .into(),
            open_upvalues: Vec::new(),
            open_above: 0,
            resumed_by: None,
            host_results: Vec::new(),
            unwind: None,
            error: None,
            // `NewThread` makes coroutines, suspended until resumed.
            coroutine: status == Status::LuaSuspended,
            closing: false,
            tbc: Vec::new(),
            charged_slots: limit,
            charged_held: 0,
        };
        self.heap
            .gc
            .charge(cost::THREAD + cost::STACK_SLOT * u64::from(max_reg));
        let owner = self.heap.threads.alloc(thread).map_err(VmError::from)?;
        self.install_inherited_hook(owner, inherited)?;
        Ok(owner)
    }

    fn alloc_table(&mut self) -> Result<Handle<crate::heap::TableObj>, VmError> {
        self.ensure_room(cost::OBJECT)?;
        self.heap.alloc_table().map_err(VmError::from)
    }

    fn alloc_string(&mut self, bytes: Vec<u8>) -> Result<Handle<crate::heap::StringObj>, VmError> {
        self.ensure_room(cost::OBJECT + bytes.len() as u64)?;
        self.heap.alloc_string(bytes).map_err(VmError::from)
    }

    /// One more object of `bytes` logical bytes must fit under the object
    /// limit and the logical-heap quota (ADR 0025). Strings and tables are
    /// also checked by the heap, which charges them.
    fn ensure_room(&mut self, bytes: u64) -> Result<(), VmError> {
        let live = self.heap.live_objects();
        if live.saturating_add(1) > self.max_objects || !self.heap.gc.fits(bytes) {
            return Err(VmError::MemoryLimit);
        }
        Ok(())
    }

    fn thread_by_id(&self, id: ObjectId) -> Result<Handle<ThreadObj>, VmError> {
        let (index, generation) = self.heap.threads.find_id(id).ok_or(VmError::Corrupt)?;
        Ok(Handle::new(index, generation))
    }

    fn closure_by_id(&self, id: ObjectId) -> Result<Handle<ClosureObj>, VmError> {
        let (index, generation) = self.heap.closures.find_id(id).ok_or(VmError::Corrupt)?;
        Ok(Handle::new(index, generation))
    }
}
