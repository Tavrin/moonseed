//! Typed arenas and checked handles.
//!
//! Generations bump when a slot is freed. They are an arena detail: snapshots
//! store [`ObjectId`](crate::id::ObjectId) and restore into fresh slots.

use std::marker::PhantomData;

use crate::id::{Handle, Kind, ObjectId, TerminationReason};
use crate::opcode::{Capture, Op};
use crate::table::{Table, TableKey};
use crate::value::Value;

/// Default bound on live objects (`Config::max_objects`). The smallest
/// object is 32 logical bytes, so the default 64 MiB quota admits about
/// two million; an object's slot is 32 to 200 bytes of host memory
/// besides what it owns, so this half of that keeps a full heap's slots
/// near 200 MB at most.
pub(crate) const MAX_OBJECTS: u32 = 1 << 20;

/// The most live objects a runtime may be configured for, in all arenas
/// together: 32-bit slot indexes with room for as many dead objects
/// again, and a sum over the arenas that cannot overflow.
pub(crate) const OBJECTS_CEILING: u32 = 1 << 28;

/// Reserved strings after the error messages (ADR 0031): the type names,
/// the text of the booleans, and the names `math.type` gives (ADR 0032).
pub(crate) const RESERVED_NAMES: [&str; 12] = [
    "nil", "boolean", "number", "string", "table", "function", "thread", "userdata", "true",
    "false", "integer", "float",
];

/// The text of every reserved string, in `Heap::reserved` order: each
/// error class's message in `LuaFault::ALL` order, then
/// [`RESERVED_NAMES`].
pub(crate) fn reserved_texts() -> impl Iterator<Item = &'static str> {
    crate::id::LuaFault::ALL
        .iter()
        .map(|fault| fault.text())
        .chain(RESERVED_NAMES)
}

/// Lua's basic types, by their C tag (`LUA_TNIL` .. `LUA_TTHREAD`): the
/// index of a type's entry in [`Heap::type_metatables`].
pub(crate) const BASIC_TYPES: usize = 9;

/// Lua's tag for a value's basic type (`lua_type`).
#[inline]
pub(crate) fn basic_type(value: Value) -> usize {
    match value {
        Value::Nil => 0,
        Value::Bool(_) => 1,
        Value::Integer(_) | Value::Float(_) => 3,
        Value::String(_) => 4,
        Value::Table(_) => 5,
        Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_) => 6,
        Value::LightUserdata(..) => 2,
        Value::Userdata(_) => 7,
        Value::Thread(_) => 8,
    }
}

/// Lua's name for a value's type, as `type` gives it.
pub(crate) fn type_name(value: Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Bool(_) => "boolean",
        Value::Integer(_) | Value::Float(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_) => "function",
        Value::Thread(_) => "thread",
        Value::Userdata(_) | Value::LightUserdata(..) => "userdata",
    }
}

/// The `basic_type` tags whose values each have their own metatable:
/// tables and full userdata. Light userdata share one, as in Lua.
pub(crate) const PER_OBJECT_TYPES: [usize; 2] = [5, 7];

/// The logical size of a full userdata with `user_values` user values and
/// a payload that counts `charge` bytes (ADR 0042).
pub(crate) fn userdata_cost(user_values: usize, charge: u64) -> u64 {
    cost::OBJECT
        .saturating_add(cost::USER_VALUE.saturating_mul(user_values as u64))
        .saturating_add(charge)
}

/// The logical size of a native closure keeping `values` values and
/// `state` numbers (ADR 0035).
pub(crate) fn native_closure_cost(values: usize, state: usize) -> u64 {
    cost::OBJECT + cost::REF * (values + state) as u64
}

/// Logical allocation costs, in logical bytes (ADR 0021). They are fixed
/// numbers, not measurements, so every target charges the same amounts.
pub(crate) mod cost {
    /// Every object.
    pub(crate) const OBJECT: u64 = 32;
    /// Each table slot, live or a dead anchor. A string adds its length.
    pub(crate) const ENTRY: u64 = 32;
    /// Each reference a closure or prototype holds: upvalues, instructions,
    /// constants, captures, children.
    pub(crate) const REF: u64 = 8;
    /// A thread, with its frames.
    pub(crate) const THREAD: u64 = 256;
    /// Each slot of a thread's stack (ADR 0041): source can make threads,
    /// so their stacks count against the heap quota.
    pub(crate) const STACK_SLOT: u64 = 16;
    /// Each user value of a full userdata (ADR 0042), a value slot like a
    /// stack slot. A byte payload adds its length, a host value what its
    /// type declares.
    pub(crate) const USER_VALUE: u64 = 16;
}

/// The longest string any runtime may be configured for, and so the
/// longest a snapshot, a binary chunk or a library state can hold: lengths
/// stay `u32` on every target.
pub(crate) const STRING_CEILING: usize = 1 << 30;

/// Default bound on one string (`Config::max_string_bytes`): the ceiling,
/// so by default the logical-heap quota is what bounds a string. Every
/// string is checked against both before its bytes are allocated.
pub(crate) const MAX_STRING_BYTES: usize = STRING_CEILING;

/// What `Config::max_string_bytes` is clamped to: room for any error
/// message Moonseed builds, up to the ceiling.
pub(crate) const STRING_BYTES_RANGE: std::ops::RangeInclusive<usize> = 1_024..=STRING_CEILING;

/// Default smallest debt between automatic collections.
pub(crate) const GC_MIN_DEBT: u64 = 64 * 1024;

/// Default hard quota on the logical heap, in logical bytes (ADR 0025). A
/// provisional default for embedding untrusted scripts, not a Lua
/// compatibility promise: logical bytes are not process memory, and a table
/// of integers uses about five times its logical size. Trusted hosts raise
/// it through `Config::max_logical_heap`.
pub(crate) const MAX_LOGICAL_HEAP: u64 = 64 * 1024 * 1024;

/// Lua 5.4's default pause, step multiplier and step size, as `lua_gc`
/// stores them: the first two divided by four in a byte, the last the log2
/// of the allocation between steps.
pub(crate) const GC_PAUSE: u8 = 200 / 4;
pub(crate) const GC_STEPMUL: u8 = 100 / 4;
pub(crate) const GC_STEPSIZE: u8 = 13;
/// Lua 5.4's default minor multiplier, as `lua_gc` stores it (a byte), and
/// major multiplier, divided by four.
pub(crate) const GC_MINORMUL: u8 = 20;
pub(crate) const GC_MAJORMUL: u8 = 100 / 4;

/// When the collector works, and how much (ADR 0021, ADR 0050). Snapshot
/// state.
#[derive(Clone, Debug)]
pub(crate) struct GcState {
    /// Automatic collection enabled (`collectgarbage("stop")` clears it).
    pub(crate) auto: bool,
    /// Logical bytes allocated since the last cycle ended.
    pub(crate) debt: u64,
    /// Debt at which the next cycle starts, as set when the last one
    /// ended.
    pub(crate) threshold: u64,
    /// Lower bound for `threshold` while the pause is above 100.
    pub(crate) min_debt: u64,
    /// Logical size of what the last cycle kept, an estimate for the
    /// schedule.
    pub(crate) live: u64,
    /// The logical heap, exactly: every object's logical size, those a
    /// sweep has freed counted until it ends (`Collector::unreleased`).
    /// The quota is on it. Not snapshot state: restore counts it again.
    pub(crate) used: u64,
    /// Cycles completed so far, automatic or requested.
    pub(crate) collections: u64,
    /// Hard quota on `used`, in logical bytes (ADR 0025). An allocation
    /// that would pass it is refused before it happens.
    pub(crate) quota: u64,
    /// `setpause`, divided by four, as Lua stores it.
    pub(crate) pause: u8,
    /// `setstepmul`, divided by four: work units per KiB allocated.
    pub(crate) stepmul: u8,
    /// Log2 of the logical bytes allocated between steps.
    pub(crate) stepsize: u8,
    /// Lua's `GCdebt`: bytes allocated past the point at which the next
    /// step is due; a step is due once it is positive.
    pub(crate) sched: i64,
    /// Work units the steps scheduled so far still owe.
    pub(crate) owed: u64,
    /// A full collection is wanted: work goes on until this many cycles
    /// have completed.
    pub(crate) full: Option<u64>,
    /// Work units already paid for in fuel and not done yet.
    pub(crate) prepaid: u32,
    /// Work units done, every cycle together.
    pub(crate) work: u64,
    /// Work units the last completed cycle took.
    pub(crate) cycle_work: u64,
    /// A hash of the collector's events: cycle starts, phase changes and
    /// the work done at each, cycle ends. Two runs that collected alike
    /// agree on it.
    pub(crate) trace: u64,
    /// The mode `collectgarbage` last chose: generational (ADR 0051).
    pub(crate) generational: bool,
    /// The minor multiplier, as `lua_gc` stores it: a young collection is
    /// due once this percent of what the last one kept is allocated.
    pub(crate) minormul: u8,
    /// The major multiplier, divided by four: a major collection is due
    /// once memory grows this percent past what the last one kept.
    pub(crate) majormul: u8,
    /// What the last major collection kept, logical bytes, and objects.
    pub(crate) major_base: u64,
    pub(crate) major_objects: u32,
    /// Generational mode is falling back on whole cycles (Lua's
    /// `lastatomic`): what the last bad collection kept, or 0.
    pub(crate) bad: u64,
    /// Young collections completed so far.
    pub(crate) minors: u64,
}

impl GcState {
    pub(crate) fn new(auto: bool, min_debt: u64, max_objects: u32, quota: u64) -> Self {
        let min_debt = min_debt.max(1);
        let threshold = next_threshold(0, GC_PAUSE, min_debt, 0, max_objects, quota);
        Self {
            auto,
            debt: 0,
            threshold,
            min_debt,
            live: 0,
            used: 0,
            collections: 0,
            quota,
            pause: GC_PAUSE,
            stepmul: GC_STEPMUL,
            stepsize: GC_STEPSIZE,
            sched: -(threshold.min(i64::MAX as u64) as i64),
            owed: 0,
            full: None,
            prepaid: 0,
            work: 0,
            cycle_work: 0,
            trace: 0,
            generational: false,
            minormul: GC_MINORMUL,
            majormul: GC_MAJORMUL,
            major_base: 0,
            major_objects: 0,
            bad: 0,
            minors: 0,
        }
    }

    /// Whether `amount` more logical bytes fit under the quota: the exact
    /// heap, never the collector's estimate.
    #[inline]
    pub(crate) fn fits(&self, amount: u64) -> bool {
        self.used.saturating_add(amount) <= self.quota
    }

    /// Logical bytes left under the quota.
    #[inline]
    pub(crate) fn headroom(&self) -> u64 {
        self.quota.saturating_sub(self.used)
    }

    /// An object grew, or was made, by `amount` logical bytes: the heap
    /// grows, and so does the allocation the schedule counts.
    #[inline]
    pub(crate) fn charge(&mut self, amount: u64) {
        self.used = self.used.saturating_add(amount);
        self.debt = self.debt.saturating_add(amount);
        self.sched = self
            .sched
            .saturating_add(amount.min(i64::MAX as u64) as i64);
    }

    /// An automatic step is due.
    #[inline]
    pub(crate) fn due(&self) -> bool {
        self.auto && self.sched > 0
    }

    /// Logical bytes between steps.
    pub(crate) fn step_bytes(&self) -> u64 {
        if self.stepsize < 63 {
            1u64 << self.stepsize
        } else {
            u64::MAX
        }
    }

    /// Work units `bytes` of allocation owe: the step multiplier's
    /// documented meaning, "how many elements it marks or sweeps for each
    /// kilobyte of memory allocated" (a unit is a reference traced or an
    /// object swept), at least one. Lua's own `incstep` does 64 times as
    /// much per byte, so its default step finishes a small heap's cycle;
    /// Moonseed keeps steps small (ADR 0050).
    pub(crate) fn work_for(&self, bytes: u64) -> u64 {
        let stepmul = (u64::from(self.stepmul) * 4) | 1;
        (u128::from(bytes).saturating_mul(u128::from(stepmul)) / 1024)
            .min(u128::from(u64::MAX))
            .max(1) as u64
    }

    /// Schedule one step (Lua's `incstep`): the work owed for what was
    /// allocated past the last one, and a step's worth more; the next
    /// step comes after another step's worth of allocation. With `room`
    /// logical bytes left under the quota and the object limit, a step
    /// does at least its share of finishing a cycle (as much work as the
    /// last one took) within half that room, so the sweep frees what it
    /// can before the limits are reached.
    pub(crate) fn schedule_step(&mut self, room: u64) {
        let bytes = u64::try_from(self.sched.max(0))
            .unwrap_or(0)
            .saturating_add(self.step_bytes());
        let share = (u128::from(self.cycle_work)
            .saturating_mul(u128::from(bytes))
            .saturating_mul(2)
            / u128::from(room.max(1)))
        .min(u128::from(u64::MAX)) as u64;
        let work = self.work_for(bytes).max(share);
        self.owed = self.owed.saturating_add(work);
        self.sched = -(self.step_bytes().min(i64::MAX as u64) as i64);
    }

    /// A cycle ended keeping an estimated `live` logical bytes and
    /// `objects` objects, with `debt` bytes made while it swept: the next
    /// starts once the debt reaches `pause` percent of what it kept (Lua's
    /// `setpause`), within the limits.
    pub(crate) fn cycle_done(&mut self, live: u64, debt: u64, objects: u32, max_objects: u32) {
        self.collections = self.collections.saturating_add(1);
        self.live = live;
        self.debt = debt;
        self.owed = 0;
        self.threshold = next_threshold(
            live,
            self.pause,
            self.min_debt,
            objects,
            max_objects,
            self.quota,
        );
        self.sched = debt.min(i64::MAX as u64) as i64 - self.threshold.min(i64::MAX as u64) as i64;
    }

    /// A young collection ended keeping an estimated `live` logical bytes
    /// and `objects` objects: the next is due once the minor multiplier's
    /// percent of that is allocated (Lua's `setminordebt`), at least
    /// `min_debt`, within the limits.
    pub(crate) fn minor_done(&mut self, live: u64, objects: u32, max_objects: u32) {
        self.minors = self.minors.saturating_add(1);
        self.live = live;
        self.debt = 0;
        self.owed = 0;
        self.threshold = self.minor_threshold(live, objects, max_objects);
        self.sched = -(self.threshold.min(i64::MAX as u64) as i64);
    }

    /// A collection that made the heap generational ended keeping `live`
    /// bytes and `objects` objects, with `debt` bytes made while it swept:
    /// the base for the next major collection, and a young one next.
    pub(crate) fn major_done(&mut self, live: u64, debt: u64, objects: u32, max_objects: u32) {
        self.collections = self.collections.saturating_add(1);
        self.live = live;
        self.debt = debt;
        self.owed = 0;
        self.major_base = live;
        self.major_objects = objects;
        self.threshold = self.minor_threshold(live, objects, max_objects);
        self.sched = debt.min(i64::MAX as u64) as i64 - self.threshold.min(i64::MAX as u64) as i64;
    }

    /// The minor multiplier's percent of `live`, but at least `min_debt`,
    /// the least allocation between automatic collections, within half
    /// the room left under the limits.
    fn minor_threshold(&self, live: u64, objects: u32, max_objects: u32) -> u64 {
        let debt =
            (u128::from(live) * u128::from(self.minormul) / 100).min(u128::from(u64::MAX)) as u64;
        let debt = debt.max(self.min_debt);
        let headroom = u64::from(max_objects.saturating_sub(objects));
        debt.min(cost::OBJECT * headroom / 2)
            .min(self.quota.saturating_sub(live) / 2)
            .max(cost::OBJECT)
    }

    /// How far past what the last major collection kept memory may grow
    /// before the next: the major multiplier's percent (Lua's
    /// `genmajormul`), at least twice `min_debt` (young collections come
    /// no sooner than `min_debt`, and one must be able to come first),
    /// within three quarters of the room left under the limits then, the
    /// room under the object limit counted at the average size of the
    /// objects it kept. Young collections come within half the room left,
    /// so near the limits they go on until old objects fill about half of
    /// it.
    pub(crate) fn major_growth(&self, max_objects: u32) -> u64 {
        let percent = u128::from(self.majormul) * 4;
        let growth = (u128::from(self.major_base) * percent / 100).min(u128::from(u64::MAX)) as u64;
        let headroom = u64::from(max_objects.saturating_sub(self.major_objects));
        let average = (self.major_base / u64::from(self.major_objects.max(1))).max(cost::OBJECT);
        growth
            .max(self.min_debt.saturating_mul(2))
            .min(average.saturating_mul(headroom) / 4 * 3)
            .min(self.quota.saturating_sub(self.major_base) / 4 * 3)
    }

    /// A major collection is due: the heap grew past the major multiplier
    /// since the last major one (Lua's `genstep`).
    pub(crate) fn major_due(&self, max_objects: u32) -> bool {
        self.used
            > self
                .major_base
                .saturating_add(self.major_growth(max_objects))
    }

    /// Fold a collector event into [`GcState::trace`].
    pub(crate) fn note(&mut self, event: u64) {
        self.trace = (self.trace ^ event)
            .wrapping_mul(0x0100_0000_01b3)
            .rotate_left(7)
            ^ self.work;
    }
}

/// The debt at which the next cycle starts: `pause` percent of what the
/// last one kept, less what it kept (Lua's `setpause`); at least `min_debt`
/// while the pause is above 100; and early enough that half the remaining
/// room under the object limit, and half the remaining room under the
/// logical quota, is still free. Every object costs at least
/// [`cost::OBJECT`].
pub(crate) fn next_threshold(
    live: u64,
    pause: u8,
    min_debt: u64,
    objects: u32,
    max_objects: u32,
    quota: u64,
) -> u64 {
    let pause = u64::from(pause) * 4;
    let base = (u128::from(live) * u128::from(pause) / 100).min(u128::from(u64::MAX)) as u64;
    let mut debt = base.saturating_sub(live);
    if pause > 100 {
        debt = debt.max(min_debt);
    }
    let headroom = u64::from(max_objects.saturating_sub(objects));
    debt.min(cost::OBJECT * headroom / 2)
        .min(quota.saturating_sub(live) / 2)
        .max(cost::OBJECT)
}

/// Why a table insert did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InsertError {
    /// The handle names no live table.
    NoTable,
    /// The new slot would pass the logical-heap quota.
    Memory,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    Ready = 1,
    LuaSuspended = 2,
    Waiting = 3,
    Completed = 4,
    /// Ended by an error nothing caught; the error is in
    /// `ThreadObj::error`. A coroutine keeps its frames and stack, its
    /// to-be-closed values still pending, until `CloseThread` closes it, as
    /// in Lua 5.4. Any other thread was unwound first, and has no frames.
    Failed = 5,
}

impl Status {
    pub(crate) fn from_u8(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::Ready,
            2 => Self::LuaSuspended,
            3 => Self::Waiting,
            4 => Self::Completed,
            5 => Self::Failed,
            _ => return None,
        })
    }

    pub(crate) fn tag(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Pending {
    Prepared {
        sequence: u64,
        symbol: String,
        arg: i64,
        dest: u8,
    },
    Waiting {
        sequence: u64,
        symbol: String,
        arg: i64,
        dest: u8,
        wait_key: u64,
    },
    Resuming {
        child: Handle<ThreadObj>,
        dest: u8,
        /// `COUNT_OPEN` or an exact count, including zero.
        nresults: u8,
    },
    /// A `Call` on an external native, stopped before the native runs. The
    /// frame's `pc` is on the `Call`; its arguments are still in registers.
    NativePrepared { sequence: u64 },
    /// A `Call` on a native that returned `Pending`, waiting for
    /// `complete_wait`. `sequence` is the effect id of an external native.
    NativeWaiting {
        sequence: Option<u64>,
        wait_key: u64,
    },
    /// A capability op waiting for typed host completion, or ready to be
    /// consumed by the builtin's next work step. Request/token/result are rooted
    /// in HostWait payload slots; no Lua result window is advanced on completion.
    Capability {
        sequence: u64,
        wait_key: u64,
        completed: bool,
    },
    /// Right-to-left stores still waiting. `next == ndest` means nothing has
    /// been stored. Destinations live on the frame so they stay roots.
    Assigning { src: u32, nvalues: u16, next: u16 },
    /// A boundary frame's call of a base, math, or table function, made in
    /// the next step rather than inside the one that asked for it, so such
    /// functions calling each other never nest on the Rust stack, and each
    /// call costs a unit of fuel (ADR 0033). The function and its
    /// arguments are in place at the frame's call slot.
    Deferred,
}

/// A destination captured before any assignment store.
///
/// Register targets are absolute stack slots. Field targets own copies of the
/// table and key, so later stores cannot change which slot was addressed.
#[derive(Clone, Debug)]
pub(crate) enum AssignTarget {
    Register(u32),
    Field { table: Value, key: Value },
}

/// Canonical Lua activation; exceptional continuation state is allocated on demand.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub(crate) closure: Handle<ClosureObj>,
    pub(crate) pc: u32,
    pub(crate) base: u32,
    pub(crate) limit: u32,
    /// Extra arguments below the registers (ADR 0028); zero for fixed frames.
    pub(crate) vararg_len: u32,
    /// COUNT_OPEN means all results; zero discards them.
    pub(crate) nresults: u8,
    /// Tail history and a cold return-hook continuation; other bits are reserved.
    pub(crate) flags: u8,
    /// Present exactly when at least one exceptional field is live.
    pub(crate) cold: Option<Box<FrameCold>>,
}

// Native layout is not the portable snapshot layout. wasm32 has 4-byte pointers.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Frame>() == 40);

#[derive(Clone, Debug, Default)]
pub(crate) struct FrameCold {
    pub(crate) pending: Option<Pending>,
    pub(crate) wait_request: Option<HostWait>,
    pub(crate) targets: Vec<AssignTarget>,
    pub(crate) meta: Option<MetaCall>,
    pub(crate) boundary: Option<Boundary>,
}

impl FrameCold {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.wait_request.is_none()
            && self.targets.is_empty()
            && self.meta.is_none()
            && self.boundary.is_none()
    }

    /// Drop every owned value before retaining storage outside the traced heap.
    fn clear(&mut self) {
        self.pending = None;
        self.wait_request = None;
        self.targets.clear();
        self.meta = None;
        self.boundary = None;
    }

    /// Restore the semantic view without manufacturing an empty cold payload.
    pub(crate) fn into_box(self) -> Option<Box<Self>> {
        if self.is_empty() {
            None
        } else {
            Some(Box::new(self))
        }
    }

    pub(crate) fn with_boundary(
        boundary: Boundary,
        spare: &mut Option<Box<Self>>,
    ) -> Option<Box<Self>> {
        let mut cold = spare.take().unwrap_or_default();
        debug_assert!(cold.is_empty());
        cold.boundary = Some(boundary);
        Some(cold)
    }
}

impl Frame {
    pub(crate) const TAIL: u8 = 1;

    pub(crate) fn is_tail(&self) -> bool {
        self.flags & Self::TAIL != 0
    }

    /// Callers install a live field before the next legal safe point.
    pub(crate) fn cold_mut(&mut self, spare: &mut Option<Box<FrameCold>>) -> &mut FrameCold {
        self.cold.get_or_insert_with(|| {
            let cold = spare.take().unwrap_or_default();
            debug_assert!(cold.is_empty());
            cold
        })
    }

    pub(crate) fn release_cold(&mut self, spare: &mut Option<Box<FrameCold>>) {
        if self.cold.as_ref().is_some_and(|cold| cold.is_empty()) {
            self.recycle_cold(spare);
        }
    }

    /// Used only when the activation has finished, or its whole state is discarded.
    pub(crate) fn recycle_cold(&mut self, spare: &mut Option<Box<FrameCold>>) {
        if let Some(mut cold) = self.cold.take() {
            cold.clear();
            *spare = Some(cold);
        }
    }

    pub(crate) fn pending(&self) -> Option<&Pending> {
        self.cold.as_ref()?.pending.as_ref()
    }
    pub(crate) fn pending_mut(&mut self) -> Option<&mut Pending> {
        self.cold.as_mut()?.pending.as_mut()
    }
    pub(crate) fn meta(&self) -> Option<&MetaCall> {
        self.cold.as_ref()?.meta.as_ref()
    }
    pub(crate) fn meta_mut(&mut self) -> Option<&mut MetaCall> {
        self.cold.as_mut()?.meta.as_mut()
    }
    pub(crate) fn boundary(&self) -> Option<&Boundary> {
        self.cold.as_ref()?.boundary.as_ref()
    }
    pub(crate) fn boundary_mut(&mut self) -> Option<&mut Boundary> {
        self.cold.as_mut()?.boundary.as_mut()
    }
    pub(crate) fn wait_request(&self) -> Option<&HostWait> {
        self.cold.as_ref()?.wait_request.as_ref()
    }
    pub(crate) fn targets(&self) -> &[AssignTarget] {
        self.cold.as_ref().map_or(&[], |cold| &cold.targets)
    }

    pub(crate) fn set_pending(
        &mut self,
        value: Option<Pending>,
        spare: &mut Option<Box<FrameCold>>,
    ) {
        if value.is_some() {
            self.cold_mut(spare).pending = value;
        } else {
            self.clear_pending(spare);
        }
    }

    /// Completing an ordinary call usually has no cold state. Keep that
    /// check separate from installation so it stays small in return paths.
    #[inline(always)]
    pub(crate) fn clear_pending(&mut self, spare: &mut Option<Box<FrameCold>>) {
        if let Some(cold) = &mut self.cold {
            cold.pending = None;
            self.release_cold(spare);
        }
    }
    pub(crate) fn set_meta(&mut self, value: Option<MetaCall>, spare: &mut Option<Box<FrameCold>>) {
        if value.is_some() {
            self.cold_mut(spare).meta = value;
        } else if let Some(cold) = &mut self.cold {
            cold.meta = None;
            self.release_cold(spare);
        }
    }
    pub(crate) fn set_wait_request(
        &mut self,
        value: Option<HostWait>,
        spare: &mut Option<Box<FrameCold>>,
    ) {
        if value.is_some() {
            self.cold_mut(spare).wait_request = value;
        } else if let Some(cold) = &mut self.cold {
            cold.wait_request = None;
            self.release_cold(spare);
        }
    }
    pub(crate) fn clear_targets(&mut self, spare: &mut Option<Box<FrameCold>>) {
        if let Some(cold) = &mut self.cold {
            cold.targets.clear();
            self.release_cold(spare);
        }
    }
    pub(crate) fn take_meta(&mut self, spare: &mut Option<Box<FrameCold>>) -> Option<MetaCall> {
        let value = self.cold.as_mut()?.meta.take();
        self.release_cold(spare);
        value
    }
    pub(crate) fn take_wait_request(
        &mut self,
        spare: &mut Option<Box<FrameCold>>,
    ) -> Option<HostWait> {
        let value = self.cold.as_mut()?.wait_request.take();
        self.release_cold(spare);
        value
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HostWait {
    pub(crate) operation: String,
    pub(crate) payload: Vec<Value>,
}

/// A frame that marks a boundary instead of running code (ADR 0024).
#[derive(Clone, Debug)]
pub(crate) enum Boundary {
    /// `pcall(f, ...)` / `xpcall(f, h, ...)`. The protected function runs
    /// above this frame with its results returned to `func + 1`; the
    /// frame's `nresults` is what the caller wants from `pcall`.
    Protect {
        /// The slot of the `pcall` value: `true` / `false` goes here, and
        /// the call's results follow it.
        func: u32,
        /// The caller made an ordinary `Call`, whose `pc` moves on when
        /// the protected call ends. Otherwise the caller is a metamethod
        /// call or another boundary, which finishes the call itself.
        advance_caller: bool,
        /// `xpcall`'s message handler.
        handler: Option<Value>,
    },
    /// An `xpcall` message handler runs above this frame, on top of the
    /// failing frames, before any of them is unwound. Its first result is
    /// written at `slot` and becomes the error object.
    Handler {
        slot: u32,
        /// Index of the `Protect` frame whose handler this is.
        protect: u32,
        /// Index of the frame the unwind stops at once the handler
        /// returns: `protect`, or a `load` frame above it that caught the
        /// error, since `load` keeps the message handler of the `xpcall`
        /// it runs in, as Lua's protected parser does (ADR 0031).
        target: u32,
        /// 1 for the first call; more when the handler itself failed.
        depth: u8,
        /// The class of the error being handled.
        fault: crate::id::LuaFault,
    },
    /// A base function that called a Lua value and goes on once it
    /// returns (ADR 0031). Its arguments stay where they were passed, at
    /// `func + 1 ..`; the call it makes is at `func + 1 + passed`, and that
    /// call's results land there. Its results go to `func`, as a native's
    /// do.
    Builtin {
        func: u32,
        /// How many arguments the base function got.
        passed: u32,
        /// As for `Protect`.
        advance_caller: bool,
        task: Task,
    },
    /// A host native's call. Arguments and kept values remain below the
    /// call slot, as in a builtin frame; errors stop here and yields pass.
    Native {
        func: u32,
        passed: u32,
        advance_caller: bool,
        symbol: u32,
        tag: u32,
        kept: u32,
        /// The original external call's effect sequence, reused on resume.
        sequence: Option<u64>,
        error: Option<(crate::id::LuaFault, Value)>,
        /// External continuation invocation prepared for its next step.
        resuming: bool,
    },
    /// A finalizer's call (ADR 0048): `__gc` at `func`, its object at
    /// `func + 1`. Pushed at an instruction boundary, above the frame
    /// below's registers and whatever `top` held; it touches nothing
    /// below `func`. Its call's results are dropped; an error stops here
    /// and becomes a warning. No yield crosses it.
    Finalizer {
        func: u32,
        /// `top` before the call, put back when it ends.
        saved_top: u32,
    },
    /// Non-yieldable Lua hook trampoline. Invisible to debug stack levels.
    Hook {
        func: u32,
        saved_top: u32,
        target: u32,
        instruction: Option<(usize, u32, u8)>,
        after: crate::runtime::hooks::AfterHook,
    },
    /// A Lua-visible native activation, used only on hooked threads.
    HookNative {
        func: u32,
        passed: u32,
        callee: Value,
        advance_caller: bool,
        phase: u8,
        produced: u32,
        result: u32,
    },
}

impl Boundary {
    /// Whether an error raised above stops at this frame: a protected
    /// call, or `load` reading.
    pub(crate) fn catches(&self) -> bool {
        match self {
            Self::Protect { .. } => true,
            Self::Handler { .. } | Self::Hook { .. } | Self::HookNative { .. } => false,
            Self::Builtin { task, .. } => task.catches(),
            Self::Native { .. } | Self::Finalizer { .. } => true,
        }
    }
}

/// What a [`Boundary::Builtin`] frame waits for, and what it does with
/// the result (ADR 0031).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Task {
    /// `tostring(v)`: `__tostring` is running; its result must be a string
    /// or a number.
    ToString,
    /// `print(...)`: the `__tostring` of argument `next` is running. The
    /// arguments before it have been written out.
    Print { next: u32 },
    /// `pairs(t)`: `__pairs` is running; its first three results are
    /// `pairs`'s.
    Pairs,
    /// `ipairs`'s iterator: `__index` is running for the integer `index`.
    Ipairs { index: i64 },
    /// `load` with a reader function: the reader is running, and `source`
    /// is what it gave so far. Charged to the logical heap by its length.
    Load { source: Vec<u8> },
    /// Checkpointed file loading or filesystem path search.
    HostLoad(Box<crate::runtime::hostload::HostLoad>),
    /// The chunk called by dofile; preserves all results and permits yields.
    DoFile,
    /// A table function, or `math.min` / `math.max` (ADR 0033).
    Lib(Box<crate::library::LibTask>),
    /// `collectgarbage("collect")`, `("step")`, or a change of mode that
    /// needs a full collection. While `wait`, the collector does the work
    /// asked for, in bounded units, before anything else (ADR 0050); then
    /// the `left` finalizers queued by then run above this frame before it
    /// returns (ADR 0048). It returns by `result`: 0 for `collect`, a
    /// step's `ended` (whether its cycle ended) for 1, the previous mode's
    /// name for 2 (incremental) and 3 (generational), false for 4 (a
    /// generational step, which ends no cycle, ADR 0051). Finalizers queued
    /// later, by collections their own garbage causes, run after it
    /// returns.
    Collect {
        result: u8,
        wait: bool,
        ended: bool,
        left: u32,
    },
    /// Bounded file IO, separate from the table/string work machine.
    Io(Box<crate::iolib::IoWork>),
}

impl Task {
    /// The results the call it makes wants.
    pub(crate) fn wants(&self) -> u8 {
        match self {
            Self::Pairs => 3,
            Self::DoFile => crate::opcode::COUNT_OPEN,
            Self::Lib(task) if matches!(task.wait, crate::library::Wait::Pair { .. }) => 2,
            _ => 1,
        }
    }

    /// Scratch slots the frame keeps between its arguments and the call
    /// it makes.
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Lib(task) => task.work.scratch(),
            Self::Io(work) => work.scratch(),
            _ => 0,
        }
    }

    /// Bytes the frame holds outside the heap and charges to it: a
    /// reader's source, or `table.concat`'s text.
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Load { source } => source.len(),
            Self::HostLoad(work) => work.held_bytes(),
            Self::Io(work) => work.held_bytes(),
            Self::Lib(task) => match &task.work {
                crate::library::Work::Concat { text, .. } => text.len(),
                crate::library::Work::Str(work) => work.held_bytes(),
                crate::library::Work::Utf8(work) => work.held_bytes(),
                crate::library::Work::Os(work) => work.out.len(),
                crate::library::Work::Package(work) => work.held_bytes(),
                crate::library::Work::Debug(work) => work.held_bytes(),
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Whether a coroutine may yield across the frame. Lua 5.4.9 calls
    /// `__pairs` and `dofile` with continuations; both permit yields.
    /// The remaining ordinary library callbacks are non-yieldable (ADR 0031).
    pub(crate) fn yieldable(&self) -> bool {
        matches!(self, Self::Pairs | Self::DoFile)
    }

    /// Whether an error in the call stops at the frame: `load` returns it.
    pub(crate) fn catches(&self) -> bool {
        matches!(self, Self::Load { .. })
    }
}

/// An error on its way to a protected call or out of the thread (ADR 0024),
/// or a thread being closed (ADR 0026).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Unwind {
    /// The error's class and object. `None` only while `CloseThread` closes
    /// a thread that had not failed and no close has raised yet.
    pub(crate) error: Option<(crate::id::LuaFault, Value)>,
    pub(crate) phase: UnwindPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnwindPhase {
    /// Just raised: no frame has been touched. The next step finds the
    /// target, and calls an `xpcall` message handler first when there is
    /// one.
    Raised,
    /// Popping frames, one per step, down to the `Protect` frame at
    /// `target`, or all of them when there is none. Each pop is where a
    /// scope's close actions will run once `__close` exists.
    Popping { target: Option<u32> },
}

/// An instruction's metamethod call in progress (ADR 0019).
#[derive(Clone, Debug)]
pub(crate) struct MetaCall {
    pub(crate) event: MetaEvent,
    /// Absolute stack slot of the function. Its arguments follow it; its
    /// first result replaces it.
    pub(crate) slot: u32,
    pub(crate) nargs: u8,
    pub(crate) phase: MetaPhase,
    /// `Some` exactly for `MetaEvent::Close`: which values close and what
    /// follows. Boxed apart, so every other event stays a few bytes.
    pub(crate) close: Option<Box<Closing>>,
}

/// A frame's closes (ADR 0026): the values at or above slot `from` close,
/// one call each, newest first, and then the frame does `next`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Closing {
    pub(crate) from: u32,
    pub(crate) next: CloseNext,
}

/// How a metamethod call finishes its instruction (ADR 0023). The
/// instruction at the frame's `pc` says which operation it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaEvent {
    /// The first result goes to register `dst`: `__index`, `__len`,
    /// arithmetic and bitwise operators, `__unm`, `__bnot`, `__concat`.
    Store { dst: u8 },
    /// The first result's truth goes to register `dst` as a boolean,
    /// negated for `~=`: `__eq`, `__lt`, `__le`.
    /// `dst == COUNT_OPEN` instead finishes the CompareBranch at the saved PC;
    /// that sentinel cannot name a register and is checked on restore.
    Truth { dst: u8, negate: bool },
    /// `SetIndex` / `SetField`: the result is dropped.
    NewIndex,
    /// A store of `AssignCommit`: the result is dropped and the assignment
    /// cursor moves.
    NewIndexAssign,
    /// A `__close` call, or the step between two (ADR 0026); `MetaCall`'s
    /// `close` says which values and what follows. The result is dropped.
    Close,
}

/// What a frame does once its to-be-closed values are closed (ADR 0026).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CloseNext {
    /// `CloseScope`: continue after the instruction. Closers get nil.
    Advance,
    /// `Return`: return the values at `src..src + produced`. Closers get nil.
    Return { src: u32, produced: u32 },
    /// An unwind stopped at this frame: resume it, which pops the frame.
    /// Closers get its error, or nil when it has none.
    Unwind(Unwind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaPhase {
    /// A Lua function is running above this frame, or the result is in
    /// the slot. Once this frame is on top again, it commits.
    Running,
    /// An external native, prepared with an effect id, not yet run.
    NativePrepared { sequence: u64 },
    /// A native waiting for `complete_wait`.
    NativeWaiting {
        sequence: Option<u64>,
        wait_key: u64,
    },
    /// A `Close` event between two calls: no call is running, and the next
    /// step closes the next value or finishes.
    Idle,
}

#[derive(Clone, Debug)]
pub(crate) struct StringObj {
    pub(crate) id: ObjectId,
    pub(crate) bytes: Vec<u8>,
    /// Derived lazily; strings never used as keys need not hash their bytes.
    pub(crate) hash: std::cell::OnceCell<u64>,
}

impl StringObj {
    #[inline]
    pub(crate) fn hash(&self) -> u64 {
        let hash = *self
            .hash
            .get_or_init(|| crate::hashutil::string_hash(&self.bytes));
        debug_assert_eq!(hash, crate::hashutil::string_hash(&self.bytes));
        hash
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TableObj {
    pub(crate) id: ObjectId,
    pub(crate) table: Table,
    /// A strong reference, traced by the collector and written to
    /// snapshots by `ObjectId`.
    pub(crate) metatable: Option<Handle<TableObj>>,
    /// Registered for finalization, or waiting for its finalizer (Lua's
    /// `FINALIZEDBIT`, ADR 0047). Rebuilt on restore from
    /// [`Finalizers`].
    pub(crate) finalize: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FieldHints {
    pub(crate) name: std::cell::Cell<u32>,
    pub(crate) index: std::cell::Cell<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct Proto {
    pub(crate) id: ObjectId,
    pub(crate) ops: Vec<Op>,
    /// Disposable slot hints indexed by pc. No handles or values, never
    /// traced, serialized or logically charged. Every use validates the
    /// live slot's key; deletion, compaction and another table are misses.
    pub(crate) field_hints: Box<[FieldHints]>,
    /// Constant bytes, for lookups that only need to borrow them.
    pub(crate) byte_consts: Vec<Vec<u8>>,
    /// Derived from const_strings at install/restore, never serialized or charged.
    pub(crate) byte_hashes: Vec<u64>,
    /// The same constants as string objects, made when the prototype is
    /// installed and owned by it: `const_strings[i]` holds
    /// `byte_consts[i]`. Loading a constant or passing it as a key uses
    /// these, so running code never allocates a constant (ADR 0020).
    pub(crate) const_strings: Vec<Handle<StringObj>>,
    pub(crate) captures: Vec<Capture>,
    pub(crate) children: Vec<Handle<Proto>>,
    pub(crate) max_reg: u8,
    pub(crate) params: u8,
    pub(crate) vararg: bool,
    /// Debug information, `None` when stripped or hand-built (ADR 0040).
    /// Its `source` is always `None` here: the chunk's name is `source`.
    pub(crate) debug: Option<Box<crate::debuginfo::DebugInfo>>,
    /// The chunk's name, shared by its prototypes; `None` is Lua's `=?`.
    pub(crate) source: Option<Handle<StringObj>>,
}

impl Proto {
    pub(crate) fn empty_field_hints(ops: &[Op]) -> Box<[FieldHints]> {
        // Prototypes without field instructions need no allocation. Otherwise
        // physical storage is exactly eight bytes per instruction, plus the
        // boxed-slice pointer/length in Proto. u32::MAX starts as a miss.
        if ops
            .iter()
            .any(|op| matches!(op, Op::GetField { .. } | Op::SetField { .. }))
        {
            vec![
                FieldHints {
                    name: std::cell::Cell::new(u32::MAX),
                    index: std::cell::Cell::new(u32::MAX),
                };
                ops.len()
            ]
            .into_boxed_slice()
        } else {
            Box::default()
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum UpvalueState {
    Open {
        thread: Handle<ThreadObj>,
        slot: u32,
    },
    Closed(Value),
}

#[derive(Clone, Debug)]
pub(crate) struct UpvalueObj {
    pub(crate) id: ObjectId,
    pub(crate) state: UpvalueState,
}

#[derive(Clone, Debug)]
pub(crate) struct ClosureObj {
    pub(crate) id: ObjectId,
    pub(crate) proto: Handle<Proto>,
    pub(crate) upvalues: Vec<Handle<UpvalueObj>>,
}

/// A function the VM implements with values and state of its own
/// (ADR 0035). The builtin reads them from the callee when called.
#[derive(Clone, Debug)]
pub(crate) struct NativeClosureObj {
    pub(crate) id: ObjectId,
    /// The builtin that runs it: an index into `Heap::natives`.
    pub(crate) native: u32,
    /// Values it keeps alive: traced, written to snapshots by id.
    pub(crate) values: Vec<Value>,
    /// Numbers it keeps between calls.
    pub(crate) state: Vec<i64>,
}

/// A thread's value stack: the logical slots `values[..len]` and, above
/// them, storage kept from deeper calls. Every reader goes through `Deref`,
/// so a slot at or above `len` is never read, traced, serialised or
/// inspected; `grow_to` nils such slots before they become visible again.
#[derive(Clone, Default)]
pub(crate) struct Stack {
    /// Physical high-water; only `grow_to` lengthens it.
    pub(crate) values: Vec<Value>,
    /// The logical length, exactly what a `Vec` truncated and resized at the
    /// same points would hold.
    pub(crate) len: usize,
}

impl Stack {
    // Inherent forms of the hottest slice methods: the same answers as
    // through `Deref`, without building the logical slice first.
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub(crate) fn get(&self, index: usize) -> Option<&Value> {
        if index < self.len {
            self.values.get(index)
        } else {
            None
        }
    }

    #[inline(always)]
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        if index < self.len {
            self.values.get_mut(index)
        } else {
            None
        }
    }

    /// Lower the logical length; the storage stays.
    #[inline(always)]
    pub(crate) fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    /// Raise the logical length to `len`, nil from the old end. Returns
    /// whether the physical storage grew.
    #[inline(always)]
    pub(crate) fn grow_to(&mut self, len: usize) -> bool {
        if len <= self.len {
            return false;
        }
        if len > self.values.len() {
            self.grow_physical(len);
            return true;
        }
        self.values[self.len..len].fill(Value::Nil);
        self.len = len;
        false
    }

    #[cold]
    #[inline(never)]
    fn grow_physical(&mut self, len: usize) {
        self.values[self.len..].fill(Value::Nil);
        grow_values(&mut self.values, len);
        self.len = len;
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn extend_from_slice(&mut self, values: &[Value]) -> bool {
        let old = self.len;
        let grew = self.grow_to(old + values.len());
        self.values[old..self.len].copy_from_slice(values);
        grew
    }

    pub(crate) fn as_slice(&self) -> &[Value] {
        self
    }

    #[cfg(test)]
    pub(crate) fn physical_len(&self) -> usize {
        self.values.len()
    }
}

/// Lengthen the physical storage with nils; the caller nils any stale slot
/// below the old physical end before raising the logical length over it.
#[cold]
#[inline(never)]
pub(crate) fn grow_values(values: &mut Vec<Value>, len: usize) {
    values.resize(len, Value::Nil);
}

impl From<Vec<Value>> for Stack {
    fn from(values: Vec<Value>) -> Self {
        let len = values.len();
        Self { values, len }
    }
}

impl std::ops::Deref for Stack {
    type Target = [Value];
    #[inline(always)]
    fn deref(&self) -> &[Value] {
        &self.values[..self.len]
    }
}

impl std::ops::DerefMut for Stack {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [Value] {
        &mut self.values[..self.len]
    }
}

impl<'a> IntoIterator for &'a Stack {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl std::fmt::Debug for Stack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// A thread's frames: the live activations `slots[..depth]` and, above them,
/// slots kept from deeper calls. A slot at or above `depth` holds no cold
/// state (I4) and is never traced, serialised or inspected; its fields are
/// overwritten before it is live again.
#[derive(Clone, Default)]
pub(crate) struct Frames {
    pub(crate) slots: Vec<Frame>,
    pub(crate) depth: usize,
}

impl Frames {
    // Inherent forms of the hottest slice methods, as for `Stack`.
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.depth
    }

    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        self.depth == 0
    }

    // `depth - 1` wraps to `usize::MAX` at depth 0, which no slot has: one
    // compare answers both questions.
    #[inline(always)]
    pub(crate) fn last(&self) -> Option<&Frame> {
        self.slots.get(self.depth.wrapping_sub(1))
    }

    #[inline(always)]
    pub(crate) fn last_mut(&mut self) -> Option<&mut Frame> {
        self.slots.get_mut(self.depth.wrapping_sub(1))
    }

    #[inline(always)]
    pub(crate) fn get(&self, index: usize) -> Option<&Frame> {
        if index < self.depth {
            self.slots.get(index)
        } else {
            None
        }
    }

    /// Push a frame; returns whether the physical storage grew.
    #[inline(always)]
    pub(crate) fn push(&mut self, frame: Frame) -> bool {
        let depth = self.depth;
        self.depth = depth + 1;
        if let Some(slot) = self.slots.get_mut(depth) {
            debug_assert!(slot.cold.is_none());
            *slot = frame;
            false
        } else {
            self.slots.push(frame);
            true
        }
    }

    /// Push an ordinary fixed-arity activation, writing its fields in place.
    /// Returns whether the physical storage grew.
    #[inline(always)]
    pub(crate) fn push_hot(
        &mut self,
        closure: Handle<ClosureObj>,
        base: u32,
        limit: u32,
        nresults: u8,
    ) -> bool {
        let depth = self.depth;
        if let Some(slot) = self.slots.get_mut(depth) {
            debug_assert!(slot.cold.is_none());
            slot.closure = closure;
            slot.pc = 0;
            slot.base = base;
            slot.limit = limit;
            slot.vararg_len = 0;
            slot.nresults = nresults;
            slot.flags = 0;
            self.depth = depth + 1;
            false
        } else {
            self.push_physical(Frame {
                closure,
                pc: 0,
                base,
                limit,
                vararg_len: 0,
                nresults,
                flags: 0,
                cold: None,
            });
            true
        }
    }

    #[cold]
    #[inline(never)]
    fn push_physical(&mut self, frame: Frame) {
        self.slots.push(frame);
        self.depth = self.slots.len();
    }

    /// Remove the top frame, moving its cold state out with it.
    pub(crate) fn pop(&mut self) -> Option<Frame> {
        let depth = self.depth.checked_sub(1)?;
        self.depth = depth;
        let slot = &mut self.slots[depth];
        Some(Frame {
            cold: slot.cold.take(),
            ..*slot
        })
    }

    /// Remove the top frame of a finished hot activation: its cold state,
    /// if any, is dropped; nothing is copied.
    #[inline(always)]
    pub(crate) fn pop_hot(&mut self) {
        let depth = self.depth - 1;
        self.depth = depth;
        let slot = &mut self.slots[depth];
        if slot.cold.is_some() {
            slot.cold = None;
        }
    }

    pub(crate) fn truncate(&mut self, depth: usize) {
        if depth < self.depth {
            for slot in &mut self.slots[depth..self.depth] {
                slot.cold = None;
            }
            self.depth = depth;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.truncate(0);
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [Frame] {
        self
    }

    #[cfg(test)]
    pub(crate) fn physical_len(&self) -> usize {
        self.slots.len()
    }

    /// I4: every slot beyond the live frames is free of cold state.
    pub(crate) fn stale_slots_hold_no_cold(&self) -> bool {
        self.slots[self.depth..]
            .iter()
            .all(|slot| slot.cold.is_none())
    }
}

impl From<Vec<Frame>> for Frames {
    fn from(slots: Vec<Frame>) -> Self {
        let depth = slots.len();
        Self { slots, depth }
    }
}

impl std::ops::Deref for Frames {
    type Target = [Frame];
    #[inline(always)]
    fn deref(&self) -> &[Frame] {
        &self.slots[..self.depth]
    }
}

impl std::ops::DerefMut for Frames {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [Frame] {
        &mut self.slots[..self.depth]
    }
}

impl<'a> IntoIterator for &'a Frames {
    type Item = &'a Frame;
    type IntoIter = std::slice::Iter<'a, Frame>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl std::fmt::Debug for Frames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// [`ThreadObj::charge_slots`] on the field alone, for code that holds the
/// thread's other fields borrowed. Returns whether the charge grew.
#[inline(always)]
pub(crate) fn charge_slots_to(charged_slots: &mut u32, len: usize, gc: &mut GcState) -> bool {
    let len = u32::try_from(len).unwrap_or(u32::MAX);
    if len > *charged_slots {
        gc.charge(cost::STACK_SLOT * u64::from(len - *charged_slots));
        *charged_slots = len;
        return true;
    }
    false
}

#[derive(Clone, Debug)]
pub(crate) struct ThreadObj {
    pub(crate) id: ObjectId,
    pub(crate) status: Status,
    pub(crate) stack: Stack,
    /// Exclusive end of the open value region. Interior nils below `top` count.
    pub(crate) top: u32,
    pub(crate) frames: Frames,
    pub(crate) open_upvalues: Vec<(u32, Handle<UpvalueObj>)>,
    /// One past the highest open slot, 0 with none open: a return above it
    /// closes nothing. Derived from `open_upvalues`, recomputed on restore,
    /// never serialised.
    pub(crate) open_above: u32,
    pub(crate) resumed_by: Option<Handle<ThreadObj>>,
    pub(crate) host_results: Vec<Value>,
    /// An error being unwound in this thread.
    pub(crate) unwind: Option<Box<Unwind>>,
    /// Set with `Status::Failed`: the class and object of the error that
    /// ended the thread.
    pub(crate) error: Option<(crate::id::LuaFault, Value)>,
    /// Made by `NewThread`. An error nothing catches leaves a coroutine's
    /// stack in place; the entry thread and host calls unwind instead.
    pub(crate) coroutine: bool,
    /// `CloseThread` is closing this thread: its own boundaries do not catch,
    /// its closes cannot yield, and the unwind's end answers the closer.
    pub(crate) closing: bool,
    /// Stack slots of the active to-be-closed values, in declaration order,
    /// so strictly increasing (ADR 0026). Nil and false are never listed.
    pub(crate) tbc: Vec<u32>,
    /// What the thread is charged beyond its object (ADR 0051): stack
    /// slots, as many as the stack has had, and bytes its builtins hold
    /// while they work. Never less than what it holds; a collection
    /// tracing the thread measures both again.
    pub(crate) charged_slots: u32,
    pub(crate) charged_held: u64,
}

impl ThreadObj {
    /// Charge stack slots up to `len`, past what the thread is charged
    /// for already.
    #[inline]
    /// The only way to open an upvalue on this thread.
    pub(crate) fn push_open(&mut self, slot: u32, upvalue: Handle<UpvalueObj>) {
        self.open_upvalues.push((slot, upvalue));
        self.open_above = self.open_above.max(slot + 1);
    }

    /// Forget every open upvalue at or above `min_slot` (the cells are the
    /// caller's to close) and recompute the summary over what remains.
    pub(crate) fn close_open_from(&mut self, min_slot: u32) {
        self.open_upvalues.retain(|(slot, _)| *slot < min_slot);
        self.open_above = self
            .open_upvalues
            .iter()
            .map(|(slot, _)| slot + 1)
            .max()
            .unwrap_or(0);
    }

    pub(crate) fn charge_slots(&mut self, len: usize, gc: &mut GcState) {
        charge_slots_to(&mut self.charged_slots, len, gc);
    }

    /// The stack slots it holds: its stack, and the registers its frames
    /// may write, which a call charged and the stack reaches again as they
    /// are written.
    pub(crate) fn extent(&self) -> u32 {
        let stack = u32::try_from(self.stack.len()).unwrap_or(u32::MAX);
        self.frames
            .iter()
            .map(|frame| frame.limit)
            .fold(stack, u32::max)
    }

    /// The bytes its builtins hold while they work.
    pub(crate) fn held_bytes(&self) -> u64 {
        self.frames
            .iter()
            .map(|frame| {
                let task = match frame.boundary() {
                    Some(Boundary::Builtin { task, .. }) => task.held_bytes() as u64,
                    _ => 0,
                };
                task + frame.wait_request().map_or(0, |wait| {
                    wait.operation.len() as u64 + cost::STACK_SLOT * wait.payload.len() as u64
                })
            })
            .sum()
    }
}

/// A full userdata (ADR 0042).
#[derive(Debug)]
pub(crate) struct UserdataObj {
    pub(crate) id: ObjectId,
    pub(crate) metatable: Option<Handle<TableObj>>,
    /// Fixed at creation; nil until set. Traced.
    pub(crate) user_values: Box<[Value]>,
    pub(crate) payload: crate::userdata::Payload,
    /// The logical bytes the payload counts: a byte payload's length, or
    /// what the host declared for its value.
    pub(crate) charge: u64,
    /// As [`TableObj::finalize`].
    pub(crate) finalize: bool,
}

/// An object finalization can apply to: a table or a full userdata
/// (ADR 0047).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinRef {
    Table(Handle<TableObj>),
    Userdata(Handle<UserdataObj>),
}

impl FinRef {
    pub(crate) fn value(self) -> Value {
        match self {
            Self::Table(handle) => Value::Table(handle),
            Self::Userdata(handle) => Value::Userdata(handle),
        }
    }

    pub(crate) fn trace(self) -> TraceRef {
        match self {
            Self::Table(handle) => TraceRef {
                kind: Kind::Table,
                index: handle.index,
            },
            Self::Userdata(handle) => TraceRef {
                kind: Kind::Userdata,
                index: handle.index,
            },
        }
    }
}

/// Finalization state (ADR 0047). Snapshot state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Finalizers {
    /// Objects registered for finalization and not found dead yet, in
    /// registration order, oldest first (Lua's `finobj`, newest first).
    /// Not roots.
    pub(crate) registered: Vec<FinRef>,
    /// In generational form, where `registered` is known old (ADR 0051):
    /// the entries before `old_until` are old objects, which a young
    /// collection cannot find dead, and those from `new_from` on were
    /// registered since the last young collection (Lua's `finobjold1` and
    /// `finobjsur`, by position). A young collection looks at the entries
    /// from `old_until` on, those registered since the one before it.
    /// Both 0 outside generational form.
    pub(crate) old_until: u32,
    pub(crate) new_from: u32,
    /// Objects found dead whose finalizers have not run, in the order they
    /// run: each collection appends what it found, newest registration
    /// first (Lua's `tobefnz`). Roots.
    pub(crate) pending: std::collections::VecDeque<FinRef>,
    /// A finalizer is running: no collection starts, and
    /// `collectgarbage` fails (Lua's `GCSTPGC`).
    pub(crate) running: bool,
    /// `Runtime::begin_close` has run: every registered object is
    /// pending, and nothing registers any more (Lua's `GCSTPCLS`).
    pub(crate) closing: bool,
    /// While closing a runtime whose run had ended: how its entry thread
    /// ended, restored once the queue is empty.
    pub(crate) closed: Option<Status>,
    /// While closing: the closure every finalizer frame on the frameless
    /// entry thread names, as every frame names one. A root.
    pub(crate) close_closure: Option<Handle<ClosureObj>>,
    /// Pending exit, including the terminal state after its outcome.
    pub(crate) exit: Option<ExitState>,
}

/// Exit is control state, never a Lua error or a protected-call result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitPhase {
    Scopes = 1,
    Finalizers = 2,
    Terminal = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitState {
    pub(crate) status: crate::ExitStatus,
    pub(crate) close: bool,
    pub(crate) phase: ExitPhase,
}

/// An object's logical size, by the costs in [`cost`] (ADR 0021).
pub(crate) trait LogicalSize: Identified {
    fn logical_size(&self) -> u64;
}

/// An object's logical identity, for the arena's id index.
pub(crate) trait Identified {
    fn object_id(&self) -> ObjectId;
    #[cfg(feature = "counters")]
    fn allocation_bytes(&self) -> u64;
}

macro_rules! identified {
    ($($ty:ty),*) => {
        $(impl Identified for $ty {
            #[cfg(feature = "counters")]
            fn allocation_bytes(&self) -> u64 { self.logical_size() }
            fn object_id(&self) -> ObjectId {
                self.id
            }
        })*
    };
}

identified!(
    StringObj,
    TableObj,
    Proto,
    UpvalueObj,
    ClosureObj,
    ThreadObj,
    NativeClosureObj,
    UserdataObj
);

impl LogicalSize for StringObj {
    fn logical_size(&self) -> u64 {
        cost::OBJECT + self.bytes.len() as u64
    }
}

impl LogicalSize for TableObj {
    fn logical_size(&self) -> u64 {
        cost::OBJECT + cost::ENTRY * self.table.slot_len() as u64
    }
}

impl LogicalSize for Proto {
    fn logical_size(&self) -> u64 {
        let refs =
            self.ops.len() + self.byte_consts.len() + self.captures.len() + self.children.len();
        cost::OBJECT
            + cost::REF * refs as u64
            + self.debug.as_ref().map_or(0, |debug| debug.logical_size())
    }
}

impl LogicalSize for UpvalueObj {
    fn logical_size(&self) -> u64 {
        cost::OBJECT
    }
}

impl LogicalSize for ClosureObj {
    fn logical_size(&self) -> u64 {
        cost::OBJECT + cost::REF * self.upvalues.len() as u64
    }
}

impl LogicalSize for ThreadObj {
    fn logical_size(&self) -> u64 {
        cost::THREAD + cost::STACK_SLOT * u64::from(self.charged_slots) + self.charged_held
    }
}

impl LogicalSize for NativeClosureObj {
    fn logical_size(&self) -> u64 {
        native_closure_cost(self.values.len(), self.state.len())
    }
}

impl LogicalSize for UserdataObj {
    fn logical_size(&self) -> u64 {
        userdata_cost(self.user_values.len(), self.charge)
    }
}

/// An object's collector mark (ADR 0050). Two whites take turns: after a
/// cycle's atomic phase the previous white is dead, so a sweep can tell
/// an object it must free from one made since, without visiting either
/// first.
pub(crate) mod mark {
    /// The first white; the other is 1.
    pub(crate) const WHITE0: u8 = 0;
    /// Reached, its references not yet traced (or written to since).
    pub(crate) const GRAY: u8 = 2;
    /// Reached and traced.
    pub(crate) const BLACK: u8 = 3;
    /// No object in the slot.
    pub(crate) const FREE: u8 = 4;

    #[inline]
    pub(crate) fn is_white(mark: u8) -> bool {
        mark < GRAY
    }
}

/// An object's age in generational mode (ADR 0051), Lua 5.4's ages but
/// `OLD0`, which only Lua's forward barrier makes. Young objects are
/// white between collections; old ones are black, or gray when written
/// to since the last collection.
pub(crate) mod age {
    /// Made since the last young collection.
    pub(crate) const NEW: u8 = 0;
    /// Survived one young collection.
    pub(crate) const SURVIVAL: u8 = 1;
    /// Survived two: old, but what it refers to may still be young, so
    /// the next young collection traces it.
    pub(crate) const OLD1: u8 = 3;
    /// Old, and everything it refers to is old.
    pub(crate) const OLD: u8 = 4;
    /// Old and written to since the last young collection: gray, in the
    /// arena's `again` list.
    pub(crate) const TOUCHED1: u8 = 5;
    /// Old, written to before the last young collection: the next one
    /// traces it again, since what it was given then is young still.
    pub(crate) const TOUCHED2: u8 = 6;

    #[inline]
    pub(crate) fn is_old(age: u8) -> bool {
        age > SURVIVAL
    }

    pub(crate) fn valid(age: u8) -> bool {
        matches!(age, NEW | SURVIVAL | OLD1 | OLD | TOUCHED1 | TOUCHED2)
    }
}

struct Slot<T> {
    generation: u32,
    /// When true the slot is never recycled. Prevents generation wrap.
    retired: bool,
    value: Option<T>,
}

/// Objects of one kind, by slot. Every mutable access goes through
/// [`Arena::get_mut`], which is the collector's write barrier (ADR 0050):
/// while a cycle marks, an object already traced (black) that is about
/// to change goes back to gray and onto `again`, to be traced again in
/// the atomic phase. Only the collector reads and writes past it, through
/// [`Arena::raw_mut`].
pub(crate) struct Arena<T> {
    slots: Vec<Slot<T>>,
    /// Each slot's collector mark ([`mark`]), [`mark::FREE`] when the
    /// slot is empty: dense, so tracing reads marks without touching the
    /// objects. Not Lua values.
    marks: Vec<u8>,
    /// Each slot's age ([`age`]): meaningful in generational mode, and
    /// [`age::NEW`] otherwise.
    ages: Vec<u8>,
    free: Vec<u32>,
    live: u32,
    /// Objects whose mark is gray or black.
    marked: u32,
    /// Objects left dead by the last atomic phase and not swept yet: not
    /// counted as live objects, never handed out again.
    dead: u32,
    /// A cycle is marking, or the heap is in generational form:
    /// [`Arena::get_mut`] is a barrier.
    barrier: bool,
    /// A cycle is marking: a write that gives an object no reference is a
    /// barrier too, as it may move what a scan in progress has passed.
    strict: bool,
    /// The mark a new object gets: white outside marking, gray (and onto
    /// `again`) or black while marking, the new white while sweeping.
    alloc_mark: u8,
    /// Slots gone back to gray since the cycle began, in order: written
    /// to after being traced, or made while marking. In generational
    /// mode, the old objects written to since the last young collection.
    /// Each slot at most once (`queued`).
    again: Vec<u32>,
    /// One bit per slot: the slot is on `again`. Kept whoever holds the
    /// slot, so an entry left by an object since freed or made white is
    /// never doubled: when the slot's object is gray again, the entry
    /// already there serves.
    queued: Vec<u64>,
    /// Each object's slot by its id, made on the first lookup by id and
    /// kept at every allocation and free from then on (Phase 3.31): a host
    /// that never looks objects up by id pays nothing for it. Never
    /// iterated, so its order is never semantic; not snapshot state.
    by_id: std::cell::RefCell<Option<std::collections::HashMap<ObjectId, u32>>>,
    /// In generational mode, the young objects, in the order they were
    /// made, but those freed or promoted since by a young collection's
    /// sweep in progress ([`TOMB`]).
    young: Vec<u32>,
    /// New objects go on `young`: the heap is in generational form.
    generational: bool,
    _ty: PhantomData<fn() -> T>,
}

impl<T> Arena<T> {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            marks: Vec::new(),
            ages: Vec::new(),
            free: Vec::new(),
            live: 0,
            marked: 0,
            dead: 0,
            barrier: false,
            strict: false,
            alloc_mark: mark::WHITE0,
            again: Vec::new(),
            queued: Vec::new(),
            by_id: std::cell::RefCell::new(None),
            young: Vec::new(),
            generational: false,
            _ty: PhantomData,
        }
    }

    /// Live objects: dead objects waiting for the sweep do not count.
    pub(crate) fn live(&self) -> u32 {
        self.live - self.dead
    }

    /// Every object in the arena, dead ones waiting for the sweep included.
    pub(crate) fn physical(&self) -> u32 {
        self.live
    }

    pub(crate) fn alloc(&mut self, value: T) -> Result<Handle<T>, TerminationReason>
    where
        T: Identified,
    {
        if self.live() >= OBJECTS_CEILING {
            return Err(TerminationReason::MemoryLimit);
        }
        let id = value.object_id();
        let mark = self.alloc_mark;
        let index = if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none() && !slot.retired);
            slot.value = Some(value);
            index
        } else {
            // Dead objects waiting for the sweep may hold up to as many
            // slots again as the live ones.
            if self.slots.len() >= 2 * OBJECTS_CEILING as usize {
                return Err(TerminationReason::MemoryLimit);
            }
            self.slots.push(Slot {
                generation: 1,
                retired: false,
                value: Some(value),
            });
            self.marks.push(mark::FREE);
            self.ages.push(age::NEW);
            (self.slots.len() - 1) as u32
        };
        self.marks[index as usize] = mark;
        self.ages[index as usize] = age::NEW;
        if let Some(by_id) = self.by_id.get_mut() {
            by_id.insert(id, index);
        }
        if self.generational {
            self.young.push(index);
        }
        self.live += 1;
        if !mark::is_white(mark) {
            self.marked += 1;
            if mark == mark::GRAY {
                self.push_again(index);
            }
        }
        #[cfg(feature = "counters")]
        crate::counters::allocated(
            std::any::type_name::<T>(),
            self.slots[index as usize]
                .value
                .as_ref()
                .expect("allocated")
                .allocation_bytes(),
        );
        Ok(Handle::new(index, self.slots[index as usize].generation))
    }

    pub(crate) fn get(&self, handle: Handle<T>) -> Option<&T> {
        let slot = self.slots.get(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.value.as_ref()
    }

    /// The object, to change it: the write barrier.
    #[inline]
    pub(crate) fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        self.get_mut_storing(handle, true)
    }

    /// The object, to change it in a way that gives it a reference
    /// (`references`) or none (a number stored, an entry removed). Between
    /// collections in generational form a change that gives no reference
    /// cannot make an old object refer to a young one, so it is no
    /// barrier, as in Lua, whose barrier checks that the value stored is
    /// collectable; while a cycle marks every change is.
    #[inline]
    pub(crate) fn get_mut_storing(
        &mut self,
        handle: Handle<T>,
        references: bool,
    ) -> Option<&mut T> {
        let index = handle.index as usize;
        if self.slots.get(index)?.generation != handle.generation {
            return None;
        }
        if self.barrier && (references || self.strict) && self.marks[index] == mark::BLACK {
            self.touch(handle.index);
        }
        self.slots[index].value.as_mut()
    }

    /// The barrier taken: a black object about to change goes back to
    /// gray and onto `again`, and an old one is touched, so the next young
    /// collection traces it, and the one after that again.
    #[cold]
    #[inline(never)]
    fn touch(&mut self, index: u32) {
        self.marks[index as usize] = mark::GRAY;
        queue(&mut self.queued, &mut self.again, index);
        let age = &mut self.ages[index as usize];
        if age::is_old(*age) {
            *age = age::TOUCHED1;
        }
    }

    /// Free slot `index`'s object: its logical size.
    pub(crate) fn free(&mut self, index: u32) -> u64
    where
        T: LogicalSize,
    {
        let dead = self.dead_mark();
        let Some(slot) = self.slots.get_mut(index as usize) else {
            return 0;
        };
        let Some(value) = slot.value.take() else {
            return 0;
        };
        let size = value.logical_size();
        if let Some(by_id) = self.by_id.get_mut() {
            by_id.remove(&value.object_id());
        }
        drop(value);
        self.live = self.live.saturating_sub(1);
        let mark = std::mem::replace(&mut self.marks[index as usize], mark::FREE);
        if !mark::is_white(mark) {
            self.marked = self.marked.saturating_sub(1);
        } else if Some(mark) == dead {
            self.dead = self.dead.saturating_sub(1);
        }
        if slot.generation == u32::MAX {
            slot.retired = true;
            return size;
        }
        slot.generation += 1;
        self.free.push(index);
        size
    }

    /// The logical size of every object in the arena, dead ones waiting
    /// for the sweep included.
    pub(crate) fn physical_bytes(&self) -> u64
    where
        T: LogicalSize,
    {
        self.slots
            .iter()
            .filter_map(|slot| slot.value.as_ref())
            .map(LogicalSize::logical_size)
            .sum()
    }

    /// The white dead objects have while some wait for the sweep.
    fn dead_mark(&self) -> Option<u8> {
        (self.dead > 0).then_some(self.alloc_mark ^ 1)
    }

    /// Resolve a disposable host hint, checking identity as well as generation
    /// and liveness. Restored arenas can reuse both the index and generation
    /// for a different object. The fallback retains the randomized id index.
    #[inline]
    pub(crate) fn find_id_hint(&self, id: ObjectId, hint: Handle<T>) -> Option<Handle<T>>
    where
        T: Identified,
    {
        if self.get(hint).is_some_and(|value| value.object_id() == id)
            && self.dead_mark() != Some(self.marks[hint.index as usize])
        {
            return Some(hint);
        }
        self.find_id(id)
            .map(|(index, generation)| Handle::new(index, generation))
    }

    /// Every live object, by slot: dead objects waiting for the sweep are
    /// not there for anyone but the collector.
    /// The slot and generation of the object `id`, as [`Arena::iter`]
    /// would list it: in expected constant time once the arena's id index
    /// exists, which the first call makes (Phase 3.31).
    pub(crate) fn find_id(&self, id: ObjectId) -> Option<(u32, u32)>
    where
        T: Identified,
    {
        let mut by_id = self.by_id.borrow_mut();
        let index = *by_id
            .get_or_insert_with(|| {
                self.slots
                    .iter()
                    .enumerate()
                    .filter_map(|(index, slot)| {
                        slot.value
                            .as_ref()
                            .map(|value| (value.object_id(), index as u32))
                    })
                    .collect()
            })
            .get(&id)?;
        let slot = &self.slots[index as usize];
        let value = slot.value.as_ref()?;
        // A dead object waiting for the sweep is not listed.
        (value.object_id() == id && self.dead_mark() != Some(self.marks[index as usize]))
            .then_some((index, slot.generation))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, u32, &T)> {
        let dead = self.dead_mark();
        self.slots
            .iter()
            .zip(&self.marks)
            .enumerate()
            .filter_map(move |(index, (slot, mark))| {
                if dead == Some(*mark) {
                    return None;
                }
                slot.value
                    .as_ref()
                    .map(|value| (index as u32, slot.generation, value))
            })
    }

    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn slot_is_occupied(&self, index: u32) -> bool {
        self.slots
            .get(index as usize)
            .and_then(|slot| slot.value.as_ref())
            .is_some()
    }

    pub(crate) fn slot_value(&self, index: u32) -> Option<&T> {
        self.slots
            .get(index as usize)
            .and_then(|slot| slot.value.as_ref())
    }

    /// The collector's access, past the barrier.
    pub(crate) fn raw_mut(&mut self, index: u32) -> Option<&mut T> {
        self.slots
            .get_mut(index as usize)
            .and_then(|slot| slot.value.as_mut())
    }

    /// Slot `index`'s mark, if it holds an object.
    #[inline]
    pub(crate) fn mark_of(&self, index: u32) -> Option<u8> {
        self.marks
            .get(index as usize)
            .copied()
            .filter(|mark| *mark != mark::FREE)
    }

    /// Set slot `index`'s mark, keeping the counts.
    #[inline]
    pub(crate) fn set_mark(&mut self, index: u32, to: u8) {
        let Some(mark) = self.marks.get_mut(index as usize) else {
            return;
        };
        if *mark == mark::FREE {
            return;
        }
        match (mark::is_white(*mark), mark::is_white(to)) {
            (true, false) => self.marked += 1,
            (false, true) => self.marked -= 1,
            _ => {}
        }
        *mark = to;
    }

    /// Make slot `index`'s object `to` (gray or black) if it is white:
    /// true when it was.
    #[inline]
    pub(crate) fn mark_white(&mut self, index: u32, to: u8) -> bool {
        match self.marks.get_mut(index as usize) {
            Some(mark) if mark::is_white(*mark) => {
                *mark = to;
                self.marked += 1;
                true
            }
            _ => false,
        }
    }

    /// Objects whose mark is gray or black.
    pub(crate) fn marked(&self) -> u32 {
        self.marked
    }

    /// What the collector's phase means here: whether writes are a
    /// barrier, and the mark of a new object.
    pub(crate) fn set_policy(&mut self, barrier: bool, strict: bool, alloc_mark: u8) {
        self.barrier = barrier;
        self.strict = strict;
        self.alloc_mark = alloc_mark;
    }

    /// The atomic phase has decided: every white object is dead.
    pub(crate) fn condemn(&mut self) {
        self.dead = self.live - self.marked;
    }

    /// No dead objects are left to sweep (the sweep has passed them, or a
    /// restore never made them).
    pub(crate) fn clear_dead(&mut self) {
        self.dead = 0;
    }

    /// Pass up to `limit` objects from slot `*from` on that the sweep has
    /// not passed: free each dead one (the old white), make the others
    /// `white`, the sizes freed added to `freed`. Returns how many were
    /// passed; fewer than `limit` when the arena has no more. A sweep that
    /// `resets` (leaving generational form) makes every age it goes past
    /// new.
    pub(crate) fn sweep_some(
        &mut self,
        from: &mut u32,
        white: u8,
        limit: u64,
        resets: bool,
        freed: &mut u64,
    ) -> u64
    where
        T: LogicalSize,
    {
        let mut passed = 0;
        while passed < limit {
            let Some(&mark) = self.marks.get(*from as usize) else {
                break;
            };
            let index = *from;
            *from += 1;
            if resets {
                self.ages[index as usize] = age::NEW;
            }
            if mark == mark::FREE || mark == white {
                continue;
            }
            if mark::is_white(mark) {
                *freed = freed.saturating_add(self.free(index));
            } else {
                self.marks[index as usize] = white;
                self.marked -= 1;
            }
            passed += 1;
        }
        passed
    }

    /// The sweep that ends a collection entering generational form (Lua's
    /// `sweep2old`): up to `limit` objects from slot `*from` on, each dead
    /// one freed and every other made old, still black, or touched if
    /// written to since the atomic phase (gray, on `again`), the sizes
    /// freed added to `freed`. Objects made since (`white`) are young
    /// already, and objects no longer new were passed already (a restored
    /// sweep starts over its slots). Returns how many were passed.
    pub(crate) fn sweep_old_some(
        &mut self,
        from: &mut u32,
        white: u8,
        limit: u64,
        freed: &mut u64,
    ) -> u64
    where
        T: LogicalSize,
    {
        let mut passed = 0;
        while passed < limit {
            let Some(&mark) = self.marks.get(*from as usize) else {
                break;
            };
            let index = *from;
            *from += 1;
            if mark == mark::FREE || mark == white || self.ages[index as usize] != age::NEW {
                continue;
            }
            if mark::is_white(mark) {
                *freed = freed.saturating_add(self.free(index));
            } else {
                self.ages[index as usize] = if mark == mark::GRAY {
                    age::TOUCHED1
                } else {
                    age::OLD
                };
            }
            passed += 1;
        }
        passed
    }

    /// A young collection's sweep (Lua's `sweepgen`), up to `limit`
    /// objects on the young list from `*pos` on, a unit each: a dead one
    /// (the old white) is freed, its size added to `freed`; a new one survives as `SURVIVAL`, white again; a `SURVIVAL`
    /// one becomes `OLD1`, black, onto `revisit` (`OLD` with nothing to
    /// trace, `references` false), or touched if written to since it was
    /// traced. Freed and promoted entries become [`crate::gc::TOMB`];
    /// objects made since the atomic phase (`white`), or passed already (a
    /// restored sweep starts over), are left. Returns how many were passed;
    /// fewer than `limit` when the list has no more.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sweep_young_some(
        &mut self,
        pos: &mut u32,
        white: u8,
        limit: u64,
        references: bool,
        freed: &mut u64,
        promoted: &mut u64,
        revisit: &mut Vec<u32>,
    ) -> u64
    where
        T: LogicalSize,
    {
        let mut passed = 0;
        while passed < limit {
            let Some(&index) = self.young.get(*pos as usize) else {
                break;
            };
            let at = *pos as usize;
            *pos += 1;
            if index == crate::gc::TOMB {
                continue;
            }
            let Some(&found) = self.marks.get(index as usize) else {
                self.young[at] = crate::gc::TOMB;
                continue;
            };
            if found == mark::FREE {
                self.young[at] = crate::gc::TOMB;
                continue;
            }
            if found == white {
                continue;
            }
            passed += 1;
            if mark::is_white(found) {
                *freed = freed.saturating_add(self.free(index));
                self.young[at] = crate::gc::TOMB;
                continue;
            }
            let age = &mut self.ages[index as usize];
            match *age {
                age::NEW => {
                    *age = age::SURVIVAL;
                    self.marks[index as usize] = white;
                    self.marked -= 1;
                }
                age::SURVIVAL => {
                    self.young[at] = crate::gc::TOMB;
                    *promoted += 1;
                    if !references {
                        *age = age::OLD;
                    } else if found == mark::BLACK {
                        *age = age::OLD1;
                        revisit.push(index);
                    } else {
                        *age = age::TOUCHED1;
                    }
                }
                _ => self.young[at] = crate::gc::TOMB,
            }
        }
        passed
    }

    /// Slot `index`'s age.
    #[inline]
    pub(crate) fn age_of(&self, index: u32) -> u8 {
        self.ages.get(index as usize).copied().unwrap_or(age::NEW)
    }

    #[inline]
    pub(crate) fn set_age(&mut self, index: u32, to: u8) {
        if let Some(age) = self.ages.get_mut(index as usize) {
            *age = to;
        }
    }

    /// Whether new objects go on the young list (generational form).
    pub(crate) fn set_gen(&mut self, on: bool) {
        self.generational = on;
    }

    pub(crate) fn young(&self) -> &[u32] {
        &self.young
    }

    pub(crate) fn young_mut(&mut self) -> &mut Vec<u32> {
        &mut self.young
    }

    pub(crate) fn clear_again(&mut self) {
        for index in std::mem::take(&mut self.again) {
            self.queued[(index / 64) as usize] &= !(1u64 << (index % 64));
        }
    }

    pub(crate) fn pop_again(&mut self) -> Option<u32> {
        let index = self.again.pop()?;
        self.queued[(index / 64) as usize] &= !(1u64 << (index % 64));
        Some(index)
    }

    pub(crate) fn again(&self) -> &[u32] {
        &self.again
    }

    /// Put slot `index` on `again`, unless it is there already.
    pub(crate) fn push_again(&mut self, index: u32) {
        queue(&mut self.queued, &mut self.again, index);
    }

    /// Test hook: writes stop being barriers (negative controls).
    #[cfg(test)]
    pub(crate) fn force_barrier(&mut self, on: bool) {
        self.barrier = on;
    }

    /// Test hook: force the next free of `index` to retire the slot.
    #[cfg(test)]
    pub(crate) fn force_generation_max(&mut self, index: u32) {
        if let Some(slot) = self.slots.get_mut(index as usize) {
            slot.generation = u32::MAX;
        }
    }
}

/// Put slot `index` on an arena's `again` list, unless its bit in
/// `queued` says it is there already.
#[inline]
fn queue(queued: &mut Vec<u64>, again: &mut Vec<u32>, index: u32) {
    let (word, bit) = ((index / 64) as usize, 1u64 << (index % 64));
    if queued.len() <= word {
        queued.resize(word + 1, 0);
    }
    if queued[word] & bit == 0 {
        queued[word] |= bit;
        again.push(index);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TraceRef {
    pub(crate) kind: Kind,
    pub(crate) index: u32,
}

pub(crate) struct HostRoot {
    pub(crate) kind: Kind,
    pub(crate) index: u32,
    pub(crate) generation: u32,
    pub(crate) id: ObjectId,
}

/// Run `$body` with `$arena` bound to the arena of objects of `$kind`,
/// shared or (`mut`) mutable.
macro_rules! on_arena {
    ($heap:expr, $kind:expr, $arena:ident => $body:expr) => {
        match $kind {
            Kind::String => {
                let $arena = &$heap.strings;
                $body
            }
            Kind::Table => {
                let $arena = &$heap.tables;
                $body
            }
            Kind::Proto => {
                let $arena = &$heap.protos;
                $body
            }
            Kind::Upvalue => {
                let $arena = &$heap.upvalues;
                $body
            }
            Kind::Closure => {
                let $arena = &$heap.closures;
                $body
            }
            Kind::Thread => {
                let $arena = &$heap.threads;
                $body
            }
            Kind::NativeClosure => {
                let $arena = &$heap.native_closures;
                $body
            }
            Kind::Userdata => {
                let $arena = &$heap.userdata;
                $body
            }
        }
    };
    (mut $heap:expr, $kind:expr, $arena:ident => $body:expr) => {
        match $kind {
            Kind::String => {
                let $arena = &mut $heap.strings;
                $body
            }
            Kind::Table => {
                let $arena = &mut $heap.tables;
                $body
            }
            Kind::Proto => {
                let $arena = &mut $heap.protos;
                $body
            }
            Kind::Upvalue => {
                let $arena = &mut $heap.upvalues;
                $body
            }
            Kind::Closure => {
                let $arena = &mut $heap.closures;
                $body
            }
            Kind::Thread => {
                let $arena = &mut $heap.threads;
                $body
            }
            Kind::NativeClosure => {
                let $arena = &mut $heap.native_closures;
                $body
            }
            Kind::Userdata => {
                let $arena = &mut $heap.userdata;
                $body
            }
        }
    };
}
pub(crate) use on_arena;

pub(crate) struct Heap {
    pub(crate) strings: Arena<StringObj>,
    pub(crate) tables: Arena<TableObj>,
    /// Disposable event-name slot candidates. Collisions only cause misses:
    /// every table probe validates the live key bytes before reading a value.
    /// These are execution state, never traced, charged or serialized.
    pub(crate) event_hints: [std::cell::Cell<u32>; 32],
    pub(crate) protos: Arena<Proto>,
    pub(crate) upvalues: Arena<UpvalueObj>,
    pub(crate) closures: Arena<ClosureObj>,
    pub(crate) threads: Arena<ThreadObj>,
    pub(crate) hooks: crate::runtime::hooks::ThreadHooks,
    pub(crate) native_closures: Arena<NativeClosureObj>,
    pub(crate) userdata: Arena<UserdataObj>,
    pub(crate) host_roots: Vec<HostRoot>,
    /// Owned embedding roots. Created only when the host uses the value API;
    /// host state, never serialized.
    pub(crate) api_roots: Option<std::rc::Rc<std::cell::RefCell<crate::api::roots::RootTable>>>,
    pub(crate) next_object_id: u64,
    pub(crate) globals: Option<Handle<TableObj>>,
    /// Lua's registry (ADR 0039): made when the first library is installed,
    /// with the main thread at 1, the globals at 2, and the `_LOADED` and
    /// `_PRELOAD` tables `package` shares. A root.
    pub(crate) registry: Option<Handle<TableObj>>,
    /// Native-function symbols this heap's values refer to, in first-use
    /// order. `Value::Native(i)` is `natives[i]`. Never shrinks.
    pub(crate) natives: Vec<String>,
    pub(crate) active: Option<Handle<ThreadObj>>,
    pub(crate) entry: Option<Handle<ThreadObj>>,
    /// The thread whose library work runs out of its frame (taken out to
    /// run, put back or finished within the step): what it holds is not
    /// in any frame, so a collection then does not lower its charge
    /// (ADR 0051). Never set between steps; not snapshot state.
    pub(crate) working: Option<u32>,
    pub(crate) gc: GcState,
    /// The standard libraries' state: `math.random`'s generator and the
    /// entropy that seeds it (ADR 0032).
    pub(crate) library: crate::library::LibraryState,
    /// The metatable of each basic type whose values share one, by
    /// [`basic_type`] (ADR 0034). Tables have their own, and userdata does
    /// not exist, so those entries stay `None`. A root.
    pub(crate) type_metatables: [Option<Handle<TableObj>>; BASIC_TYPES],
    /// Objects registered for finalization and waiting for it (ADR 0047).
    pub(crate) finalizers: Finalizers,
    /// The incremental collector's state (ADR 0050).
    pub(crate) collector: crate::gc::Collector,
    /// The reserved strings, made when the runtime is created, in
    /// [`reserved_texts`] order: each error class's error object, so
    /// raising an error never allocates and a memory error can always be
    /// raised, then the names `type` and `tostring` give.
    pub(crate) reserved: Vec<Handle<StringObj>>,
    /// The longest string this runtime makes (`Config::max_string_bytes`,
    /// clamped to [`STRING_BYTES_RANGE`]). Snapshot state.
    pub(crate) max_string: usize,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            strings: Arena::new(),
            tables: Arena::new(),
            event_hints: std::array::from_fn(|_| std::cell::Cell::new(u32::MAX)),
            protos: Arena::new(),
            upvalues: Arena::new(),
            closures: Arena::new(),
            threads: Arena::new(),
            hooks: Default::default(),
            native_closures: Arena::new(),
            userdata: Arena::new(),
            host_roots: Vec::new(),
            api_roots: None,
            next_object_id: 1,
            globals: None,
            registry: None,
            natives: Vec::new(),
            active: None,
            entry: None,
            working: None,
            gc: GcState::new(true, GC_MIN_DEBT, MAX_OBJECTS, MAX_LOGICAL_HEAP),
            library: crate::library::LibraryState::new(0),
            type_metatables: [None; BASIC_TYPES],
            finalizers: Finalizers::default(),
            collector: crate::gc::Collector::default(),
            reserved: Vec::new(),
            max_string: MAX_STRING_BYTES,
        }
    }

    /// The collector's mark of an object, if the slot holds one.
    pub(crate) fn mark_of(&self, object: TraceRef) -> Option<u8> {
        let index = object.index;
        match object.kind {
            Kind::String => self.strings.mark_of(index),
            Kind::Table => self.tables.mark_of(index),
            Kind::Proto => self.protos.mark_of(index),
            Kind::Upvalue => self.upvalues.mark_of(index),
            Kind::Closure => self.closures.mark_of(index),
            Kind::Thread => self.threads.mark_of(index),
            Kind::NativeClosure => self.native_closures.mark_of(index),
            Kind::Userdata => self.userdata.mark_of(index),
        }
    }

    pub(crate) fn set_mark(&mut self, object: TraceRef, to: u8) {
        let index = object.index;
        match object.kind {
            Kind::String => self.strings.set_mark(index, to),
            Kind::Table => self.tables.set_mark(index, to),
            Kind::Proto => self.protos.set_mark(index, to),
            Kind::Upvalue => self.upvalues.set_mark(index, to),
            Kind::Closure => self.closures.set_mark(index, to),
            Kind::Thread => self.threads.set_mark(index, to),
            Kind::NativeClosure => self.native_closures.set_mark(index, to),
            Kind::Userdata => self.userdata.set_mark(index, to),
        }
    }

    /// An object's age (ADR 0051).
    pub(crate) fn age_of(&self, object: TraceRef) -> u8 {
        on_arena!(self, object.kind, arena => arena.age_of(object.index))
    }

    pub(crate) fn set_age(&mut self, object: TraceRef, to: u8) {
        on_arena!(mut self, object.kind, arena => arena.set_age(object.index, to))
    }

    /// Test hook: turn every arena's write barrier on or off.
    #[cfg(test)]
    pub(crate) fn force_barriers(&mut self, on: bool) {
        self.strings.force_barrier(on);
        self.tables.force_barrier(on);
        self.protos.force_barrier(on);
        self.upvalues.force_barrier(on);
        self.closures.force_barrier(on);
        self.threads.force_barrier(on);
        self.native_closures.force_barrier(on);
        self.userdata.force_barrier(on);
    }

    /// The id of the object in a slot.
    pub(crate) fn id_of(&self, object: TraceRef) -> Option<ObjectId> {
        let index = object.index;
        Some(match object.kind {
            Kind::String => self.strings.slot_value(index)?.id,
            Kind::Table => self.tables.slot_value(index)?.id,
            Kind::Proto => self.protos.slot_value(index)?.id,
            Kind::Upvalue => self.upvalues.slot_value(index)?.id,
            Kind::Closure => self.closures.slot_value(index)?.id,
            Kind::Thread => self.threads.slot_value(index)?.id,
            Kind::NativeClosure => self.native_closures.slot_value(index)?.id,
            Kind::Userdata => self.userdata.slot_value(index)?.id,
        })
    }

    /// Every object in the heap, dead ones waiting for the sweep
    /// included.
    pub(crate) fn physical_objects(&self) -> u32 {
        self.strings.physical()
            + self.tables.physical()
            + self.protos.physical()
            + self.upvalues.physical()
            + self.closures.physical()
            + self.threads.physical()
            + self.native_closures.physical()
            + self.userdata.physical()
    }

    pub(crate) fn live_objects(&self) -> u32 {
        self.strings.live()
            + self.tables.live()
            + self.protos.live()
            + self.upvalues.live()
            + self.closures.live()
            + self.threads.live()
            + self.native_closures.live()
            + self.userdata.live()
    }

    pub(crate) fn alloc_id(&mut self) -> Result<ObjectId, TerminationReason> {
        let raw = self.next_object_id;
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .ok_or(TerminationReason::ObjectIdExhausted)?;
        Ok(ObjectId(raw))
    }

    pub(crate) fn alloc_string(
        &mut self,
        bytes: Vec<u8>,
    ) -> Result<Handle<StringObj>, TerminationReason> {
        let charge = cost::OBJECT + bytes.len() as u64;
        if bytes.len() > self.max_string || !self.gc.fits(charge) {
            return Err(TerminationReason::MemoryLimit);
        }
        let id = self.alloc_id()?;
        let handle = self.strings.alloc(StringObj {
            id,
            hash: std::cell::OnceCell::new(),
            bytes,
        })?;
        self.gc.charge(charge);
        Ok(handle)
    }

    pub(crate) fn alloc_table(&mut self) -> Result<Handle<TableObj>, TerminationReason> {
        if !self.gc.fits(cost::OBJECT) {
            return Err(TerminationReason::MemoryLimit);
        }
        let id = self.alloc_id()?;
        let handle = self.tables.alloc(TableObj {
            id,
            table: Table::new(),
            metatable: None,
            finalize: false,
        })?;
        self.gc.charge(cost::OBJECT);
        Ok(handle)
    }

    /// The metatable Lua would find for `value`: a table's or a full
    /// userdata's own, or its basic type's (ADR 0034, ADR 0042). The one
    /// authority for metamethod lookup.
    #[inline]
    pub(crate) fn metatable_of(&self, value: Value) -> Option<Handle<TableObj>> {
        if let Value::Table(table) = value {
            return self.tables.get(table)?.metatable;
        }
        self.other_metatable(value)
    }

    /// A full userdata's metatable, or a value's type's: out of line, so
    /// the table case stays one test on every metamethod lookup.
    #[inline(never)]
    fn other_metatable(&self, value: Value) -> Option<Handle<TableObj>> {
        if let Value::Userdata(userdata) = value {
            return self.userdata.get(userdata)?.metatable;
        }
        self.type_metatables[basic_type(value)]
    }

    /// Set the own metatable of a table or a full userdata: the one place
    /// that does, so the finalization rule applies everywhere (ADR 0047).
    /// The object is registered for finalization when the metatable it
    /// gets has a non-nil `__gc` now, it is not registered or waiting
    /// already, and the runtime is not closing (Lua's
    /// `luaC_checkfinalizer`). A `__gc` added to the metatable later does
    /// not register it. False when `object` has no own metatable.
    pub(crate) fn set_metatable(
        &mut self,
        object: Value,
        metatable: Option<Handle<TableObj>>,
    ) -> bool {
        let finalizer = metatable.is_some_and(|metatable| {
            self.tables.get(metatable).is_some_and(|table| {
                !matches!(
                    table.table.get_view(crate::table::KeyView::string(b"__gc")),
                    None | Some(Value::Nil)
                )
            })
        }) && !self.finalizers.closing;
        let (registered, fin) = match object {
            Value::Table(handle) => match self.tables.get_mut(handle) {
                Some(table) => {
                    table.metatable = metatable;
                    (&mut table.finalize, FinRef::Table(handle))
                }
                None => return false,
            },
            Value::Userdata(handle) => match self.userdata.get_mut(handle) {
                Some(userdata) => {
                    userdata.metatable = metatable;
                    (&mut userdata.finalize, FinRef::Userdata(handle))
                }
                None => return false,
            },
            _ => return false,
        };
        if finalizer && !*registered {
            *registered = true;
            self.finalizers.registered.push(fin);
        }
        true
    }

    /// Clear an object's finalization mark as its finalizer is about to
    /// run, so the finalizer may register it again.
    pub(crate) fn unmark_finalize(&mut self, fin: FinRef) {
        match fin {
            FinRef::Table(handle) => {
                if let Some(table) = self.tables.get_mut(handle) {
                    table.finalize = false;
                }
            }
            FinRef::Userdata(handle) => {
                if let Some(userdata) = self.userdata.get_mut(handle) {
                    userdata.finalize = false;
                }
            }
        }
    }

    /// Make a full userdata with `user_values` nil user values and the
    /// payload `make` gives, counting `charge` payload bytes. The object
    /// limit and the quota are checked before `make` runs, so a refused
    /// byte payload is never allocated.
    pub(crate) fn alloc_userdata(
        &mut self,
        max_objects: u32,
        user_values: usize,
        charge: u64,
        make: impl FnOnce() -> crate::userdata::Payload,
    ) -> Result<Handle<UserdataObj>, TerminationReason> {
        if user_values > crate::userdata::MAX_USER_VALUES
            || self.live_objects().saturating_add(1) > max_objects
        {
            return Err(TerminationReason::MemoryLimit);
        }
        let cost = userdata_cost(user_values, charge);
        if !self.gc.fits(cost) {
            return Err(TerminationReason::MemoryLimit);
        }
        let id = self.alloc_id()?;
        let handle = self.userdata.alloc(UserdataObj {
            id,
            metatable: None,
            user_values: vec![Value::Nil; user_values].into_boxed_slice(),
            payload: make(),
            charge,
            finalize: false,
        })?;
        self.gc.charge(cost);
        Ok(handle)
    }

    pub(crate) fn string_bytes(&self, handle: Handle<StringObj>) -> Option<&[u8]> {
        self.strings
            .get(handle)
            .map(|object| object.bytes.as_slice())
    }

    pub(crate) fn object_id_of_value(&self, value: Value) -> Option<ObjectId> {
        Some(match value {
            Value::String(handle) => self.strings.get(handle)?.id,
            Value::Table(handle) => self.tables.get(handle)?.id,
            Value::Closure(handle) => self.closures.get(handle)?.id,
            Value::Thread(handle) => self.threads.get(handle)?.id,
            Value::NativeClosure(handle) => self.native_closures.get(handle)?.id,
            Value::Userdata(handle) => self.userdata.get(handle)?.id,
            _ => return None,
        })
    }

    /// Borrow key bytes for reads and traversal; only storage needs ownership.
    pub(crate) fn key_view(
        &self,
        value: Value,
    ) -> Result<crate::table::KeyView<'_>, crate::id::LuaFault> {
        use crate::table::KeyView;
        let bytes = if let Value::String(handle) = value {
            let object = self.strings.get(handle).ok_or(crate::id::LuaFault::Type)?;
            Some((object.bytes.as_slice(), object.hash()))
        } else {
            None
        };
        if let Some(view) = crate::table::value_view(value, bytes) {
            return Ok(view);
        }
        match value {
            Value::Nil => Err(crate::id::LuaFault::NilKey),
            Value::Float(_) => Err(crate::id::LuaFault::NanKey),
            _ => self
                .object_id_of_value(value)
                .map(KeyView::Object)
                .ok_or(crate::id::LuaFault::Type),
        }
    }

    pub(crate) fn table_get_view(
        &self,
        table: Handle<TableObj>,
        key: crate::table::KeyView<'_>,
    ) -> Option<Value> {
        Some(
            self.tables
                .get(table)?
                .table
                .get_view(key)
                .unwrap_or(Value::Nil),
        )
    }

    pub(crate) fn normalize_value(&self, value: Value) -> Result<TableKey, crate::id::LuaFault> {
        let bytes = if let Value::String(handle) = value {
            let object = self.strings.get(handle).ok_or(crate::id::LuaFault::Type)?;
            Some((object.bytes.as_slice(), object.hash()))
        } else {
            None
        };
        let object_id = match value {
            Value::Table(_)
            | Value::Closure(_)
            | Value::Thread(_)
            | Value::NativeClosure(_)
            | Value::Userdata(_) => Some(
                self.object_id_of_value(value)
                    .ok_or(crate::id::LuaFault::Type)?,
            ),
            _ => None,
        };
        crate::table::normalize_key(value, bytes, object_id)
    }

    pub(crate) fn table_get(&self, table: Handle<TableObj>, key: &TableKey) -> Option<Value> {
        Some(self.tables.get(table)?.table.get(key))
    }

    /// Existing string keys (and no-op deletes) need no owned key bytes.
    /// Mirrors table_insert's write barrier; deletion preserves slot count.
    pub(crate) fn update_string_key(
        &mut self,
        table: Handle<TableObj>,
        key: Value,
        value: Value,
    ) -> bool {
        let Value::String(handle) = key else {
            return false;
        };
        let Some(string) = self.strings.get(handle) else {
            return false;
        };
        let view = crate::table::KeyView::cached_string(&string.bytes, string.hash());
        let Some(object) = self.tables.get_mut_storing(table, true) else {
            return false;
        };
        if matches!(value, Value::Nil) {
            object.table.delete_view(view);
            true
        } else {
            object.table.update_view(view, value)
        }
    }

    pub(crate) fn table_insert(
        &mut self,
        table: Handle<TableObj>,
        key: TableKey,
        key_value: Value,
        value: Value,
    ) -> Result<(), InsertError> {
        let references =
            crate::gc::value_ref(key_value).is_some() || crate::gc::value_ref(value).is_some();
        let Some(object) = self.tables.get_mut_storing(table, references) else {
            return Err(InsertError::NoTable);
        };
        // A key that is not live may add a slot; refuse it before the table
        // changes if that slot would not fit. The lookup runs only near the
        // quota.
        if !matches!(value, Value::Nil)
            && !self.gc.fits(cost::ENTRY)
            && matches!(object.table.get(&key), Value::Nil)
        {
            return Err(InsertError::Memory);
        }
        let before = object.table.slot_len();
        object.table.insert(key, key_value, value);
        let after = object.table.slot_len();
        self.gc
            .charge(cost::ENTRY * after.saturating_sub(before) as u64);
        if after < before {
            self.give_back(cost::ENTRY * (before - after) as u64);
        }
        Ok(())
    }

    /// Bytes an object no longer holds (a table compacted, a userdata's
    /// payload shrunk): the heap is that much smaller now. The schedule
    /// is left alone: no allocation is undone, and no collector work
    /// owed is forgiven (ADR 0051).
    pub(crate) fn give_back(&mut self, amount: u64) {
        self.gc.used = self.gc.used.saturating_sub(amount);
    }

    /// Charge `amount` bytes a builtin of the running thread holds while
    /// it works (ADR 0051).
    pub(crate) fn charge_held(&mut self, amount: u64) {
        self.gc.charge(amount);
        if let Some(thread) = self
            .active
            .and_then(|handle| self.threads.raw_mut(handle.index))
        {
            thread.charged_held = thread.charged_held.saturating_add(amount);
        } else {
            debug_assert!(false, "held bytes charged with no thread running");
        }
    }

    pub(crate) fn find_by_id(&self, id: ObjectId) -> Option<(Kind, u32, u32)> {
        let found = |kind: Kind, found: Option<(u32, u32)>| {
            found.map(|(index, generation)| (kind, index, generation))
        };
        found(Kind::String, self.strings.find_id(id))
            .or_else(|| found(Kind::Table, self.tables.find_id(id)))
            .or_else(|| found(Kind::Proto, self.protos.find_id(id)))
            .or_else(|| found(Kind::Upvalue, self.upvalues.find_id(id)))
            .or_else(|| found(Kind::Closure, self.closures.find_id(id)))
            .or_else(|| found(Kind::Thread, self.threads.find_id(id)))
            .or_else(|| found(Kind::NativeClosure, self.native_closures.find_id(id)))
            .or_else(|| found(Kind::Userdata, self.userdata.find_id(id)))
    }
}

impl TableObj {
    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn live_len(&self) -> usize {
        self.table.live_len()
    }

    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn dead_len(&self) -> usize {
        self.table.dead_len()
    }
}
