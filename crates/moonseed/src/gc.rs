//! Lua 5.4's incremental mark and sweep (ADR 0050), with its weak tables,
//! ephemerons, and finalization (ADR 0046, ADR 0047).
//!
//! The collector is a state machine, [`Collector`], that is snapshot
//! state: it does bounded units of work at the runtime's safe points, and
//! nothing about a cycle in progress lives anywhere else.
//!
//! - **Pause**: no cycle.
//! - **Begin**: the next unit grays the roots.
//! - **Propagate**: gray objects are traced a bounded number of references
//!   at a time; a large table, stack, or prototype resumes where it
//!   stopped. Lua runs between slices: [`crate::heap::Arena::get_mut`] is
//!   the write barrier that keeps the marking correct.
//! - **Atomic**: Lua does not run until it is over, though the runtime may
//!   pause, and be checkpointed, between its units. In Lua's `atomic`
//!   order: gray the roots again and everything written to since it was
//!   traced, and trace, settling ephemerons; read the weak modes again;
//!   clear weak values; queue the dead registered objects, newest
//!   registration first, and trace them (resurrection); clear weak keys,
//!   and the weak values of tables resurrection reached first.
//! - **Sweep**: the whites swap, so an object still marked with the old one
//!   is dead. A bounded number of objects at a time are freed or made
//!   white again.
//!
//! The finalizers a cycle queues run after its atomic phase (ADR 0048).
//! Strings are values to weak tables: never removed for being weak.
//! Numbers, booleans, light userdata, and builtins are not objects.

use std::collections::HashMap;

use crate::heap::{Frame, Heap, Pending, TraceRef, UpvalueState, age, cost, mark};
use crate::id::Kind;
use crate::table::Slot;
use crate::value::Value;

/// Work units one unit of fuel pays for (ADR 0050).
pub(crate) const WORK_PER_FUEL: u32 = 4;

/// References gathered at most before they are marked.
const CHUNK: u64 = 256;

/// Where a cycle is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Pause,
    Begin,
    Propagate,
    Atomic(Atomic),
    Sweep,
    /// The end of a young collection: the old objects it traced because
    /// they were written to become `TOUCHED2`, to be traced once more.
    Touched,
}

/// What a cycle decides at the end of its atomic phase, in generational
/// mode (ADR 0051): whether its sweep makes what survives old.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Decide {
    /// Nothing: an incremental cycle.
    #[default]
    None,
    /// Enter generational form: `collectgarbage("generational")`, or a
    /// full collection in generational mode.
    ToGen,
    /// A major collection: back to young collections if it freed at
    /// least half the growth that called for it, else fall back on
    /// whole cycles (Lua's bad collection).
    Major,
    /// A whole cycle while falling back: back to young collections if
    /// what it kept grew less than an eighth since the bad one.
    Fallback,
}

impl Decide {
    pub(crate) fn tag(self) -> u8 {
        match self {
            Decide::None => 0,
            Decide::ToGen => 1,
            Decide::Major => 2,
            Decide::Fallback => 3,
        }
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Decide::None,
            1 => Decide::ToGen,
            2 => Decide::Major,
            3 => Decide::Fallback,
            _ => return None,
        })
    }
}

/// The arenas, in the order the collector goes through them.
pub(crate) const KINDS: [Kind; 8] = [
    Kind::String,
    Kind::Table,
    Kind::Proto,
    Kind::Upvalue,
    Kind::Closure,
    Kind::Thread,
    Kind::NativeClosure,
    Kind::Userdata,
];

/// A young-list entry a young collection's sweep has freed or promoted.
pub(crate) const TOMB: u32 = u32::MAX;

/// The steps of the atomic phase, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Atomic {
    /// Gray the roots again, and what was written to or made since the
    /// cycle began.
    Roots,
    /// Trace until nothing is gray and every ephemeron has settled.
    Mark,
    /// Read each weak table's mode again: a table whose mode changed is
    /// traced again with its present one.
    Modes,
    /// Trace what `Modes` grayed.
    Remark,
    /// Remove dead values from weak-value tables.
    Values,
    /// Queue the registered objects left white for their finalizers.
    Separate,
    /// Trace them, and what they reach.
    Resurrect,
    /// Remove dead keys from weak-key tables, and dead values from the
    /// weak-value tables resurrection reached first.
    Keys,
    /// Trace anything grayed since (only the host can), then swap whites.
    Final,
}

impl Atomic {
    pub(crate) const ALL: [Atomic; 9] = [
        Atomic::Roots,
        Atomic::Mark,
        Atomic::Modes,
        Atomic::Remark,
        Atomic::Values,
        Atomic::Separate,
        Atomic::Resurrect,
        Atomic::Keys,
        Atomic::Final,
    ];

    pub(crate) fn tag(self) -> u8 {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0) as u8
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.get(usize::from(tag)).copied()
    }
}

/// How the object being scanned is traced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum How {
    /// A thread or a prototype: every reference, by position.
    Object,
    /// A table's entries, as its mode was when it was reached.
    Entries { keys: bool, values: bool },
    /// An ephemeron table: a value is traced once its key is marked.
    Ephemeron,
}

/// An object whose references are being traced: `pos` is the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scan {
    pub(crate) object: TraceRef,
    pub(crate) pos: u32,
    pub(crate) how: How,
}

/// A weak table a cycle reached, with the mode it had then. Both flags
/// false: superseded, because the mode changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WeakTable {
    pub(crate) table: u32,
    pub(crate) keys: bool,
    pub(crate) values: bool,
}

/// The collector's state (ADR 0050). Snapshot state, but for
/// `sweep_at`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Collector {
    pub(crate) phase: Phase,
    /// The current white: 0 or 1.
    pub(crate) white: u8,
    /// Gray objects to trace, last first.
    pub(crate) gray: Vec<TraceRef>,
    pub(crate) scan: Option<Scan>,
    /// Weak tables reached this cycle, in order.
    pub(crate) weak: Vec<WeakTable>,
    /// The first entry of `weak` resurrection reached.
    pub(crate) late: u32,
    /// Ephemeron tables reached and not yet traced.
    pub(crate) ephemerons: Vec<u32>,
    /// Values of ephemeron entries whose key was white when the table
    /// was traced, by key: marking the key marks them, so ephemerons
    /// settle in time linear in their entries (ADR 0046).
    pub(crate) waiting: HashMap<(Kind, u32), Vec<Value>>,
    /// Position in `weak` of the atomic step going through it.
    pub(crate) cursor: u32,
    /// Slot in that table.
    pub(crate) inner: u32,
    /// Logical bytes of the objects traced this cycle, as they were then.
    pub(crate) marked_bytes: u64,
    /// `GcState::debt` when the cycle began; from the swap of whites on,
    /// when the sweep began.
    pub(crate) debt_base: u64,
    /// Logical bytes allocated while the cycle marked: objects made then
    /// survive it.
    pub(crate) marking_debt: u64,
    /// `GcState::work` when the cycle began.
    pub(crate) work_base: u64,
    /// Objects the sweep has still to pass.
    pub(crate) sweep_left: u64,
    /// The sweep only makes marked objects white: a cycle abandoned for a
    /// full collection, which frees nothing and does not count.
    pub(crate) reset: bool,
    /// The sweep's position: arena, then slot. Not snapshot state.
    pub(crate) sweep_at: (u8, u32),
    /// Scratch for tracing a table's entries. Not snapshot state.
    pub(crate) pairs: Vec<(Value, Value)>,
    /// The heap is in generational form (ADR 0051): ages hold, young
    /// objects are white and old ones black or gray, and writes are
    /// barriers between collections.
    pub(crate) generational: bool,
    /// The cycle running is a young collection.
    pub(crate) minor: bool,
    /// The sweep running makes what survives old (Lua's `sweep2old`).
    pub(crate) to_old: bool,
    /// What the cycle running decides at the end of its atomic phase.
    pub(crate) decide: Decide,
    /// Old objects the next young collection traces: `OLD1` and
    /// `TOUCHED2` ones (and `TOUCHED1` ones written to since, gray and in
    /// `again`).
    pub(crate) revisit: Vec<TraceRef>,
    /// Touched objects the running young collection traced, to become
    /// `TOUCHED2` at its end.
    pub(crate) touched: Vec<TraceRef>,
    /// Logical bytes of the objects the running sweep has freed: they stay
    /// in `GcState::used` until it ends, so a sweep restored, which never
    /// had the dead objects, gives back as much at the same point. Counts
    /// those it has still to free in a snapshot.
    pub(crate) unreleased: u64,
    /// Objects the running young collection has promoted.
    pub(crate) promoted: u64,
    /// Test hook: the atomic phase does not gray the roots again
    /// (negative control).
    #[cfg(test)]
    pub(crate) skip_root_remark: bool,
    /// Test hook: every allocation was charged (a runtime's heap, not one
    /// built by hand), so each collection's end checks the exact count.
    #[cfg(test)]
    pub(crate) audit: bool,
}

impl Collector {
    pub(crate) fn in_atomic(&self) -> bool {
        matches!(self.phase, Phase::Atomic(_))
    }

    /// New objects are marked: a cycle is marking.
    pub(crate) fn marking(&self) -> bool {
        matches!(self.phase, Phase::Propagate | Phase::Atomic(_))
            || (self.minor && self.phase == Phase::Begin)
    }

    /// Lua does not run until the collector is done: an atomic phase, or
    /// a young collection.
    pub(crate) fn holds(&self) -> bool {
        self.in_atomic() || (self.minor && self.phase != Phase::Pause)
    }
}

/// Units of work, counted exactly: a batch that cannot be split runs once
/// begun, and is counted whole.
struct Budget {
    left: u64,
    used: u64,
}

/// Spend `units`: they come off the budget and the work owed, and count
/// in the work done, at once, so the collector's counts are the same
/// wherever its work is split.
fn spend(heap: &mut Heap, budget: &mut Budget, units: u64) {
    budget.left = budget.left.saturating_sub(units);
    budget.used = budget.used.saturating_add(units);
    heap.gc.owed = heap.gc.owed.saturating_sub(units);
    count!("gc_work_units", units);
    heap.gc.work = heap.gc.work.saturating_add(units);
}

/// Do up to `budget` units of the work wanted: the steps scheduled
/// (`GcState::owed`), all of an atomic phase begun, and a full collection
/// requested (`GcState::full`). A step ends early when its cycle does.
/// Returns the units done, which may pass `budget` by one batch.
pub(crate) fn work(heap: &mut Heap, extra: &[TraceRef], budget: u64, max_objects: u32) -> u64 {
    count!("gc_steps");
    let mut budget = Budget {
        left: budget,
        used: 0,
    };
    let mut buf = Vec::new();
    loop {
        let phase = heap.collector.phase;
        let wanted = heap.gc.full.is_some() || heap.gc.owed > 0 || heap.collector.holds();
        if !wanted || budget.left == 0 {
            break;
        }
        // A step does the units it owes, no more, however large the
        // budget: what Lua does next cannot depend on the quantum. An
        // atomic phase, a young collection, and a full collection go on
        // regardless.
        let left = budget.left;
        let before = budget.used;
        if heap.gc.full.is_none() && !heap.collector.holds() {
            budget.left = budget.left.min(heap.gc.owed);
        }
        match phase {
            Phase::Pause => match heap.gc.full {
                // In generational form the old objects are made white
                // first, so the full cycle traces them.
                Some(target) if heap.gc.collections < target && heap.collector.generational => {
                    leave_gen(heap);
                }
                // A generational step is due: decided now, when the
                // collector works, not when it was scheduled.
                None if heap.collector.generational => {
                    heap.gc.owed = 0;
                    if heap.gc.major_due(max_objects) {
                        leave_gen(heap);
                        heap.collector.decide = Decide::Major;
                        let room = room(heap, max_objects);
                        heap.gc.schedule_step(room);
                    } else {
                        start_minor(heap);
                    }
                }
                Some(target) if heap.gc.collections < target => {
                    heap.collector.phase = Phase::Begin;
                }
                _ => {
                    heap.gc.full = None;
                    heap.gc.owed = 0;
                    break;
                }
            },
            Phase::Begin if heap.collector.minor => begin_minor(heap, &mut budget),
            Phase::Begin => begin_marking(heap, extra, &mut budget),
            Phase::Propagate => {
                if !propagate(heap, &mut budget, &mut buf) {
                    enter(heap, Phase::Atomic(Atomic::Roots));
                }
            }
            Phase::Atomic(step) => atomic(heap, extra, step, &mut budget, &mut buf, max_objects),
            Phase::Sweep if heap.collector.minor => sweep_young(heap, &mut budget),
            Phase::Sweep => sweep(heap, &mut budget, max_objects),
            Phase::Touched => correct(heap, &mut budget, max_objects),
        }
        budget.left = left.saturating_sub(budget.used - before);
    }
    budget.used
}

/// Schedule a cycle to begin, if none is running.
pub(crate) fn begin(heap: &mut Heap) {
    if heap.collector.phase == Phase::Pause {
        heap.collector.phase = Phase::Begin;
    }
}

/// Ask for a full collection (Lua's `luaC_fullgc`): one complete cycle
/// that begins once whatever cycle is running is over. A cycle still
/// marking is abandoned, its marks made white again without freeing
/// anything (Lua's `entersweep`), so the full cycle is the only one that
/// decides, and finalizers are queued as one collection would queue
/// them. One past its atomic phase finishes first.
pub(crate) fn request_full(heap: &mut Heap) {
    // A full collection is not a major one: in generational mode it
    // returns to generational form whatever it frees, as Lua's; while
    // falling back it is an incremental one (Lua's `fullinc`).
    if matches!(heap.collector.decide, Decide::Major | Decide::Fallback) {
        heap.collector.decide = Decide::None;
    }
    // A young collection running finishes first: it does not count.
    let counts = if heap.collector.minor && heap.collector.phase != Phase::Pause {
        false
    } else {
        match heap.collector.phase {
            Phase::Begin => heap.collector.phase = Phase::Pause,
            Phase::Propagate => abort(heap),
            _ => {}
        }
        match heap.collector.phase {
            Phase::Pause => false,
            Phase::Sweep => !heap.collector.reset,
            _ => true,
        }
    };
    let target = heap
        .gc
        .collections
        .saturating_add(if counts { 2 } else { 1 });
    heap.gc.full = Some(heap.gc.full.map_or(target, |old| old.max(target)));
}

/// A full collection now, all of it. Returns the units done.
pub(crate) fn full(heap: &mut Heap, extra: &[TraceRef], max_objects: u32) -> u64 {
    request_full(heap);
    work(heap, extra, u64::MAX, max_objects)
}

/// A full collection with no limits, for tests that build a heap by hand.
#[cfg(test)]
pub(crate) fn collect(heap: &mut Heap, extra: &[TraceRef]) {
    full(heap, extra, crate::heap::MAX_OBJECTS);
}

fn enter(heap: &mut Heap, phase: Phase) {
    heap.collector.phase = phase;
    let code = match phase {
        Phase::Pause => 1,
        Phase::Begin => 2,
        Phase::Propagate => 3,
        Phase::Atomic(step) => 16 + u64::from(step.tag()),
        Phase::Sweep => 4 + u64::from(heap.collector.reset),
        Phase::Touched => 6,
    };
    let code = code
        | u64::from(heap.collector.minor) << 8
        | u64::from(heap.collector.generational) << 9
        | u64::from(heap.collector.to_old) << 10;
    heap.gc.note(code);
    set_policy(heap);
}

/// What the phase means to the arenas: writes are barriers while
/// marking, and always in generational form; a new object is black (a
/// string) or gray, to be traced in the atomic phase, while marking, and
/// white otherwise; in generational form it goes on its arena's young
/// list.
fn set_policy(heap: &mut Heap) {
    let white = heap.collector.white;
    let generational = heap.collector.generational;
    let marking = heap.collector.marking();
    let (barrier, strings, others) = if marking {
        (true, mark::BLACK, mark::GRAY)
    } else {
        (generational, white, white)
    };
    macro_rules! policy {
        ($($arena:ident: $mark:expr),*) => {
            $(
                heap.$arena.set_policy(barrier, marking, $mark);
                heap.$arena.set_gen(generational);
            )*
        };
    }
    policy!(
        strings: strings,
        tables: others,
        protos: others,
        upvalues: others,
        closures: others,
        threads: others,
        native_closures: others,
        userdata: others
    );
}

/// Bring the arenas' policies in line with a restored collector.
pub(crate) fn restore_policy(heap: &mut Heap) {
    set_policy(heap);
}

fn begin_marking(heap: &mut Heap, extra: &[TraceRef], budget: &mut Budget) {
    // A full collection in generational mode returns to generational
    // form; a major collection, and any while falling back, decide.
    let generational = heap.gc.generational;
    heap.collector.decide = match heap.collector.decide {
        Decide::Major => Decide::Major,
        // An explicit step while falling back (Lua's `stepgenfull`).
        Decide::Fallback if generational && heap.gc.bad > 0 => Decide::Fallback,
        _ if heap.gc.full.is_some() => {
            if generational && heap.gc.bad == 0 {
                Decide::ToGen
            } else {
                Decide::None
            }
        }
        _ if generational && heap.gc.bad > 0 => Decide::Fallback,
        _ => Decide::None,
    };
    heap.collector.debt_base = heap.gc.debt;
    heap.collector.work_base = heap.gc.work;
    heap.collector.marked_bytes = 0;
    enter(heap, Phase::Propagate);
    let roots = roots(heap, extra);
    spend(heap, budget, roots.len().max(1) as u64);
    for root in roots {
        mark(heap, root);
    }
}

/// Every root: host roots and pinned values, the reserved strings, the
/// type metatables, the registry, the globals, the active and entry
/// threads, the objects waiting for their finalizers, and the closure
/// closing frames name.
fn roots(heap: &Heap, extra: &[TraceRef]) -> Vec<TraceRef> {
    let table = |index| TraceRef {
        kind: Kind::Table,
        index,
    };
    let thread = |index| TraceRef {
        kind: Kind::Thread,
        index,
    };
    let mut roots: Vec<TraceRef> = extra.to_vec();
    roots.extend(heap.host_roots.iter().map(|root| TraceRef {
        kind: root.kind,
        index: root.index,
    }));
    if let Some(held) = &heap.api_roots {
        roots.extend(held.borrow().values().filter_map(value_ref));
    }
    roots.extend(heap.reserved.iter().map(|handle| TraceRef {
        kind: Kind::String,
        index: handle.index,
    }));
    roots.extend(
        heap.type_metatables
            .iter()
            .flatten()
            .map(|handle| table(handle.index)),
    );
    roots.extend(heap.registry.map(|handle| table(handle.index)));
    roots.extend(heap.globals.map(|handle| table(handle.index)));
    roots.extend(heap.active.map(|handle| thread(handle.index)));
    roots.extend(heap.entry.map(|handle| thread(handle.index)));
    roots.extend(heap.finalizers.pending.iter().map(|fin| fin.trace()));
    roots.extend(heap.finalizers.close_closure.map(|handle| TraceRef {
        kind: Kind::Closure,
        index: handle.index,
    }));
    roots
}

/// Gray a white object: a string has no references, so it goes straight
/// to black.
#[inline(always)]
fn mark(heap: &mut Heap, object: TraceRef) {
    let index = object.index;
    let grayed = match object.kind {
        Kind::String => {
            if heap.strings.mark_white(index, mark::BLACK) {
                let size = heap
                    .strings
                    .slot_value(index)
                    .map_or(0, |string| cost::OBJECT + string.bytes.len() as u64);
                heap.collector.marked_bytes = heap.collector.marked_bytes.saturating_add(size);
            }
            return;
        }
        Kind::Table => heap.tables.mark_white(index, mark::GRAY),
        Kind::Proto => heap.protos.mark_white(index, mark::GRAY),
        Kind::Upvalue => heap.upvalues.mark_white(index, mark::GRAY),
        Kind::Closure => heap.closures.mark_white(index, mark::GRAY),
        Kind::Thread => heap.threads.mark_white(index, mark::GRAY),
        Kind::NativeClosure => heap.native_closures.mark_white(index, mark::GRAY),
        Kind::Userdata => heap.userdata.mark_white(index, mark::GRAY),
    };
    if grayed {
        heap.collector.gray.push(object);
    }
}

fn mark_value(heap: &mut Heap, value: Value) {
    if let Some(object) = value_ref(value) {
        mark(heap, object);
    }
}

/// The object a value is, if it is one.
pub(crate) fn value_ref(value: Value) -> Option<TraceRef> {
    let (kind, index) = match value {
        Value::String(handle) => (Kind::String, handle.index),
        Value::Table(handle) => (Kind::Table, handle.index),
        Value::Closure(handle) => (Kind::Closure, handle.index),
        Value::Thread(handle) => (Kind::Thread, handle.index),
        Value::NativeClosure(handle) => (Kind::NativeClosure, handle.index),
        Value::Userdata(handle) => (Kind::Userdata, handle.index),
        Value::Nil
        | Value::Bool(_)
        | Value::Integer(_)
        | Value::Float(_)
        | Value::Native(_)
        | Value::LightUserdata(..) => return None,
    };
    Some(TraceRef { kind, index })
}

/// The object a value is, when a weak reference to it can be cleared:
/// tables, functions with state, threads, and full userdata. Strings are
/// values to weak tables, as in Lua; numbers, booleans, light userdata,
/// and builtins are not objects.
pub(crate) fn weak_object(value: Value) -> Option<TraceRef> {
    value_ref(value).filter(|object| object.kind != Kind::String)
}

/// A table's weakness, read from its metatable's `__mode` each time the
/// table is traced, as Lua does: a short string (at most 40 bytes) with
/// `k` for weak keys and `v` for weak values before any zero byte.
/// Anything else is strong.
pub(crate) fn weak_mode(
    heap: &Heap,
    metatable: Option<crate::id::Handle<crate::heap::TableObj>>,
) -> (bool, bool) {
    let mode = metatable
        .and_then(|metatable| heap.tables.get(metatable))
        .and_then(|table| {
            table
                .table
                .get_view(crate::table::KeyView::string(b"__mode"))
        });
    let Some(Value::String(mode)) = mode else {
        return (false, false);
    };
    let Some(bytes) = heap.string_bytes(mode).filter(|bytes| bytes.len() <= 40) else {
        return (false, false);
    };
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
    (bytes.contains(&b'k'), bytes.contains(&b'v'))
}

/// An object's logical size, by the costs in [`crate::heap::cost`].
pub(crate) fn size_of(heap: &Heap, object: TraceRef) -> u64 {
    use crate::heap::LogicalSize;
    crate::heap::on_arena!(heap, object.kind, arena => {
        arena.slot_value(object.index).map_or(0, LogicalSize::logical_size)
    })
}

/// Charge a thread for what it holds beyond its object now, its stack
/// slots, the source a `load` reading keeps in its frame (ADR 0031), the
/// text `table.concat` and the string functions build (ADR 0033,
/// ADR 0034), as a collection begins to trace it: what it holds shrinks
/// without anything being freed (ADR 0051).
fn measure_thread(heap: &mut Heap, index: u32) {
    let Some(thread) = heap.threads.raw_mut(index) else {
        return;
    };
    let slots = thread.extent();
    // Library work out of its frame holds bytes no frame shows: the
    // thread keeps what it was charged.
    let held = if heap.working == Some(index) {
        thread.charged_held
    } else {
        thread.held_bytes()
            + u64::from(heap.hooks.get(thread.id).is_some()) * crate::runtime::hooks::HOOK_BYTES
    };
    debug_assert!(
        slots <= thread.charged_slots && held <= thread.charged_held,
        "thread {index} holds more than it was charged: {slots}/{} slots, {held}/{} bytes",
        thread.charged_slots,
        thread.charged_held
    );
    let before = cost::STACK_SLOT * u64::from(thread.charged_slots) + thread.charged_held;
    thread.charged_slots = slots;
    thread.charged_held = held;
    let after = cost::STACK_SLOT * u64::from(slots) + held;
    heap.gc.used = heap.gc.used.saturating_sub(before).saturating_add(after);
}

/// The exact heap counts every object's logical size, and what the
/// running sweep freed, exactly (ADR 0051): for restore and tests.
pub(crate) fn counted_bytes(heap: &Heap) -> u64 {
    let mut total = heap.collector.unreleased;
    for kind in KINDS {
        total = total
            .saturating_add(crate::heap::on_arena!(heap, kind, arena => arena.physical_bytes()));
    }
    total
}

/// The logical heap is what is counted, and every thread is charged at
/// least what it holds.
#[cfg(test)]
pub(crate) fn check_usage(heap: &Heap) -> Result<(), String> {
    let counted = counted_bytes(heap);
    if heap.gc.used != counted {
        return Err(format!("used {} but counted {counted}", heap.gc.used));
    }
    for (index, _, thread) in heap.threads.iter() {
        let held = thread.held_bytes()
            + if heap.hooks.get(thread.id).is_some() {
                crate::runtime::hooks::HOOK_BYTES
            } else {
                0
            };
        if thread.extent() > thread.charged_slots || held > thread.charged_held {
            return Err(format!(
                "thread {index} holds more than it was charged: {}/{} slots, {}/{} bytes",
                thread.extent(),
                thread.charged_slots,
                held,
                thread.charged_held
            ));
        }
    }
    Ok(())
}

/// Logical size of every live object in the heap.
#[cfg(test)]
pub(crate) fn logical_size(heap: &Heap) -> u64 {
    let mut total = 0u64;
    macro_rules! sum {
        ($arena:ident, $kind:expr) => {
            for (index, _, _) in heap.$arena.iter() {
                total += size_of(heap, TraceRef { kind: $kind, index });
            }
        };
    }
    sum!(strings, Kind::String);
    sum!(tables, Kind::Table);
    sum!(protos, Kind::Proto);
    sum!(upvalues, Kind::Upvalue);
    sum!(closures, Kind::Closure);
    sum!(threads, Kind::Thread);
    sum!(native_closures, Kind::NativeClosure);
    sum!(userdata, Kind::Userdata);
    total
}

/// Trace until nothing is left to trace, or the budget is spent: false
/// when nothing is left. An object's waiting ephemeron values are marked
/// when the object is; ephemeron tables are traced once nothing else is
/// gray, so most of their keys are settled by then.
fn propagate(heap: &mut Heap, budget: &mut Budget, buf: &mut Vec<TraceRef>) -> bool {
    loop {
        if budget.left == 0 {
            return true;
        }
        if let Some(scan) = heap.collector.scan {
            scan_some(heap, scan, budget, buf);
            continue;
        }
        if let Some(object) = heap.collector.gray.pop() {
            // Traced already: an entry that cannot recur, as `again` holds
            // a slot once, but costs nothing if it did.
            if heap.mark_of(object) != Some(mark::GRAY) {
                continue;
            }
            if !heap.collector.waiting.is_empty()
                && let Some(values) = heap.collector.waiting.remove(&(object.kind, object.index))
            {
                spend(heap, budget, values.len() as u64);
                for value in values {
                    mark_value(heap, value);
                }
            }
            start(heap, object, budget, buf);
            continue;
        }
        if let Some(table) = heap.collector.ephemerons.pop() {
            spend(heap, budget, 1);
            heap.collector.scan = Some(Scan {
                object: TraceRef {
                    kind: Kind::Table,
                    index: table,
                },
                pos: 0,
                how: How::Ephemeron,
            });
            continue;
        }
        return false;
    }
}

/// Begin tracing a gray object: it goes black, and its size counts.
/// Small objects are traced at once; a table, thread or prototype goes on
/// in [`scan_some`].
fn start(heap: &mut Heap, object: TraceRef, budget: &mut Budget, buf: &mut Vec<TraceRef>) {
    spend(heap, budget, 1);
    let index = object.index;
    if object.kind == Kind::Thread {
        measure_thread(heap, index);
    }
    if object.kind != Kind::Table {
        heap.set_mark(object, mark::BLACK);
        let size = size_of(heap, object);
        heap.collector.marked_bytes = heap.collector.marked_bytes.saturating_add(size);
    }
    match object.kind {
        Kind::String => {}
        Kind::Table => {
            heap.tables.set_mark(index, mark::BLACK);
            let Some(table) = heap.tables.slot_value(index) else {
                return;
            };
            let slots = table.table.slot_len();
            heap.collector.marked_bytes = heap
                .collector
                .marked_bytes
                .saturating_add(cost::OBJECT + cost::ENTRY * slots as u64);
            let metatable = table.metatable;
            let empty = slots == 0;
            let (keys, values) = weak_mode(heap, metatable);
            if let Some(metatable) = metatable {
                mark(
                    heap,
                    TraceRef {
                        kind: Kind::Table,
                        index: metatable.index,
                    },
                );
            }
            if keys || values {
                heap.collector.weak.push(WeakTable {
                    table: index,
                    keys,
                    values,
                });
            }
            if keys && !values {
                heap.collector.ephemerons.push(index);
            } else if !empty {
                heap.collector.scan = Some(Scan {
                    object,
                    pos: 0,
                    how: How::Entries { keys, values },
                });
            }
        }
        Kind::Thread | Kind::Proto => {
            heap.collector.scan = Some(Scan {
                object,
                pos: 0,
                how: How::Object,
            });
        }
        Kind::Closure | Kind::Upvalue | Kind::NativeClosure | Kind::Userdata => {
            small_references(heap, object, buf);
            spend(heap, budget, buf.len() as u64);
            for reference in buf.drain(..) {
                mark(heap, reference);
            }
        }
    }
}

/// The references of an object with few of them: a closure's prototype
/// and upvalues (at most 255), an upvalue's thread or value, a native
/// closure's values (at most 16), a userdata's metatable and user values
/// (a host value holds no Lua values, ADR 0044).
fn small_references(heap: &Heap, object: TraceRef, buf: &mut Vec<TraceRef>) {
    let index = object.index;
    match object.kind {
        Kind::Closure => {
            if let Some(closure) = heap.closures.slot_value(index) {
                buf.push(TraceRef {
                    kind: Kind::Proto,
                    index: closure.proto.index,
                });
                buf.extend(closure.upvalues.iter().map(|upvalue| TraceRef {
                    kind: Kind::Upvalue,
                    index: upvalue.index,
                }));
            }
        }
        Kind::Upvalue => {
            if let Some(upvalue) = heap.upvalues.slot_value(index) {
                match upvalue.state {
                    UpvalueState::Open { thread, .. } => buf.push(TraceRef {
                        kind: Kind::Thread,
                        index: thread.index,
                    }),
                    UpvalueState::Closed(value) => buf.extend(value_ref(value)),
                }
            }
        }
        Kind::NativeClosure => {
            if let Some(closure) = heap.native_closures.slot_value(index) {
                buf.extend(closure.values.iter().copied().filter_map(value_ref));
            }
        }
        Kind::Userdata => {
            if let Some(userdata) = heap.userdata.slot_value(index) {
                buf.extend(userdata.user_values.iter().copied().filter_map(value_ref));
                buf.extend(userdata.metatable.map(|metatable| TraceRef {
                    kind: Kind::Table,
                    index: metatable.index,
                }));
            }
        }
        Kind::String | Kind::Table | Kind::Thread | Kind::Proto => {}
    }
}

/// Trace the next references of the object being scanned, one unit each,
/// at most [`CHUNK`] at a time.
fn scan_some(heap: &mut Heap, scan: Scan, budget: &mut Budget, buf: &mut Vec<TraceRef>) {
    let take = budget.left.clamp(1, CHUNK) as usize;
    let from = scan.pos as usize;
    let index = scan.object.index;
    let (end, total) = match (scan.object.kind, scan.how) {
        // A strong table: every key and value.
        (
            Kind::Table,
            How::Entries {
                keys: false,
                values: false,
            },
        ) => {
            let slots = heap
                .tables
                .slot_value(index)
                .map_or(&[][..], |table| table.table.slots());
            let end = from.saturating_add(take).min(slots.len());
            for slot in slots.get(from..end).unwrap_or_default() {
                if let Slot::Live {
                    key_value, value, ..
                } = slot
                {
                    buf.extend(value_ref(*key_value));
                    buf.extend(value_ref(*value));
                }
            }
            (end, slots.len())
        }
        (Kind::Table, How::Entries { .. } | How::Ephemeron) => {
            let (keys, values) = match scan.how {
                How::Entries { keys, values } => (keys, values),
                _ => (true, false),
            };
            let mut pairs = std::mem::take(&mut heap.collector.pairs);
            pairs.clear();
            let slots = heap
                .tables
                .slot_value(index)
                .map_or(&[][..], |table| table.table.slots());
            let end = from.saturating_add(take).min(slots.len());
            for slot in slots.get(from..end).unwrap_or_default() {
                if let Slot::Live {
                    key_value, value, ..
                } = slot
                {
                    pairs.push((*key_value, *value));
                }
            }
            let total = slots.len();
            // Each entry is marked before the next is decided, so what an
            // entry decides cannot depend on where the work was split.
            for &(key_value, value) in &pairs {
                let key = if keys { weak_object(key_value) } else { None };
                let weak_value = if values { weak_object(value) } else { None };
                // A key that cannot be cleared is traced; so is a value
                // that cannot (a string is marked here, and kept). In a
                // table weak both ways, such a key goes with its entry
                // when the value goes, so it waits for the value.
                if key.is_none()
                    && let Some(object) = value_ref(key_value)
                {
                    match weak_value {
                        Some(holder)
                            if keys && heap.mark_of(holder).is_some_and(mark::is_white) =>
                        {
                            heap.collector
                                .waiting
                                .entry((holder.kind, holder.index))
                                .or_default()
                                .push(key_value);
                        }
                        _ => mark(heap, object),
                    }
                }
                if weak_value.is_some() {
                    continue;
                }
                match key {
                    None => {
                        if let Some(object) = value_ref(value) {
                            mark(heap, object);
                        }
                    }
                    // An ephemeron: the value is reachable through the
                    // table only if the key is reachable without it.
                    Some(object) => {
                        if heap
                            .mark_of(object)
                            .is_some_and(|found| !mark::is_white(found))
                        {
                            if let Some(object) = value_ref(value) {
                                mark(heap, object);
                            }
                        } else if value_ref(value).is_some() {
                            heap.collector
                                .waiting
                                .entry((object.kind, object.index))
                                .or_default()
                                .push(value);
                        }
                    }
                }
            }
            heap.collector.pairs = pairs;
            (end, total)
        }
        (Kind::Thread, _) => {
            let (end, total, units) = thread_references(heap, index, from, budget.left, buf);
            // Positions are not all one unit: counted here, not below.
            spend(heap, budget, units);
            heap.collector.scan = (end < total).then_some(Scan {
                pos: end as u32,
                ..scan
            });
            for reference in buf.drain(..) {
                mark(heap, reference);
            }
            return;
        }
        (Kind::Proto, _) => proto_references(heap, index, from, take, buf),
        _ => (from, from),
    };
    spend(heap, budget, end.saturating_sub(from) as u64);
    heap.collector.scan = (end < total).then_some(Scan {
        pos: end as u32,
        ..scan
    });
    for reference in buf.drain(..) {
        mark(heap, reference);
    }
}

/// A thread's references from position `from` until `units` are spent
/// (the position that spends the last is finished): its stack slots, its
/// open upvalues (one position, a unit each, as a restore rebuilds their
/// list in another order), its frames, its host results, and last what it
/// holds besides (its resumer, its error, an unwind's error). Returns the
/// position reached, the total, and the units spent.
fn thread_references(
    heap: &Heap,
    index: u32,
    from: usize,
    units: u64,
    buf: &mut Vec<TraceRef>,
) -> (usize, usize, u64) {
    let Some(thread) = heap.threads.slot_value(index) else {
        return (from, from, 0);
    };
    let stack = thread.stack.len();
    let open = stack + 1;
    let frames = open + thread.frames.len();
    let results = frames + thread.host_results.len();
    let total = results + 1;
    let mut spent = 0u64;
    let mut pos = from;
    while pos < total && spent < units {
        spent += 1;
        if pos < stack {
            buf.extend(value_ref(thread.stack[pos]));
        } else if pos < open {
            spent += thread.open_upvalues.len() as u64;
            buf.extend(thread.open_upvalues.iter().map(|(_, upvalue)| TraceRef {
                kind: Kind::Upvalue,
                index: upvalue.index,
            }));
        } else if pos < frames {
            frame_references(&thread.frames[pos - open], buf);
        } else if pos < results {
            buf.extend(value_ref(thread.host_results[pos - frames]));
        } else {
            if let Some(hook) = heap.hooks.get(thread.id) {
                buf.extend(value_ref(hook.target.value()));
                for value in hook.names {
                    buf.extend(value_ref(value));
                }
            }
            if let Some(parent) = thread.resumed_by {
                buf.push(TraceRef {
                    kind: Kind::Thread,
                    index: parent.index,
                });
            }
            if let Some((_, error)) = thread.unwind.as_deref().and_then(|unwind| unwind.error) {
                buf.extend(value_ref(error));
            }
            if let Some((_, error)) = thread.error {
                buf.extend(value_ref(error));
            }
        }
        pos += 1;
    }
    (pos, total, spent)
}

fn frame_references(frame: &Frame, buf: &mut Vec<TraceRef>) {
    let Some(cold) = &frame.cold else {
        buf.push(TraceRef {
            kind: Kind::Closure,
            index: frame.closure.index,
        });
        return;
    };
    if let Some(crate::heap::Boundary::HookNative { callee, .. }) = cold.boundary.as_ref() {
        buf.extend(value_ref(*callee));
    }
    if let Some(crate::heap::Boundary::Native {
        error: Some((_, error)),
        ..
    }) = cold.boundary.as_ref()
    {
        buf.extend(value_ref(*error));
    }
    if let Some(wait) = &cold.wait_request {
        for value in &wait.payload {
            buf.extend(value_ref(*value));
        }
    }
    if let Some(crate::heap::Boundary::Protect {
        handler: Some(handler),
        ..
    }) = cold.boundary.as_ref()
    {
        buf.extend(value_ref(*handler));
    }
    // An unwind waiting on the frame's closes holds its error here.
    if let Some(crate::heap::Closing {
        next: crate::heap::CloseNext::Unwind(unwind),
        ..
    }) = cold.meta.as_ref().and_then(|meta| meta.close.as_deref())
        && let Some((_, error)) = unwind.error
    {
        buf.extend(value_ref(error));
    }
    buf.push(TraceRef {
        kind: Kind::Closure,
        index: frame.closure.index,
    });
    if let Some(Pending::Resuming { child, .. }) = cold.pending.as_ref() {
        buf.push(TraceRef {
            kind: Kind::Thread,
            index: child.index,
        });
    }
    {
        for target in &cold.targets {
            if let crate::heap::AssignTarget::Field { table, key } = target {
                buf.extend(value_ref(*table));
                buf.extend(value_ref(*key));
            }
        }
    }
}

/// A prototype's references from position `from`: its chunk name, its
/// constant strings, its children.
fn proto_references(
    heap: &Heap,
    index: u32,
    from: usize,
    take: usize,
    buf: &mut Vec<TraceRef>,
) -> (usize, usize) {
    let Some(proto) = heap.protos.slot_value(index) else {
        return (from, from);
    };
    let constants = 1 + proto.const_strings.len();
    let total = constants + proto.children.len();
    let end = from.saturating_add(take).min(total);
    for pos in from..end {
        if pos == 0 {
            buf.extend(proto.source.map(|source| TraceRef {
                kind: Kind::String,
                index: source.index,
            }));
        } else if pos < constants {
            buf.push(TraceRef {
                kind: Kind::String,
                index: proto.const_strings[pos - 1].index,
            });
        } else {
            buf.push(TraceRef {
                kind: Kind::Proto,
                index: proto.children[pos - constants].index,
            });
        }
    }
    (end, total)
}

fn atomic(
    heap: &mut Heap,
    extra: &[TraceRef],
    step: Atomic,
    budget: &mut Budget,
    buf: &mut Vec<TraceRef>,
    max_objects: u32,
) {
    match step {
        Atomic::Roots => {
            #[allow(unused_mut)]
            let mut roots = roots(heap, extra);
            #[cfg(test)]
            if heap.collector.skip_root_remark {
                roots.clear();
            }
            spend(heap, budget, roots.len().max(1) as u64);
            for root in roots {
                mark(heap, root);
            }
            enter(heap, Phase::Atomic(Atomic::Mark));
        }
        Atomic::Mark | Atomic::Remark | Atomic::Resurrect => {
            if step == Atomic::Mark {
                let again = drain_again(heap, budget.left);
                spend(heap, budget, again);
                if budget.left == 0 {
                    return;
                }
            }
            if propagate(heap, budget, buf) {
                return;
            }
            heap.collector.cursor = 0;
            heap.collector.inner = 0;
            let next = match step {
                Atomic::Mark => Atomic::Modes,
                Atomic::Remark => Atomic::Values,
                _ => Atomic::Keys,
            };
            enter(heap, Phase::Atomic(next));
        }
        Atomic::Modes => {
            if modes(heap, budget) {
                enter(heap, Phase::Atomic(Atomic::Remark));
            }
        }
        Atomic::Values => {
            if clear(heap, budget, false) {
                enter(heap, Phase::Atomic(Atomic::Separate));
            }
        }
        Atomic::Separate => {
            separate(heap, budget);
            enter(heap, Phase::Atomic(Atomic::Resurrect));
        }
        Atomic::Keys => {
            if clear(heap, budget, true) {
                enter(heap, Phase::Atomic(Atomic::Final));
            }
        }
        Atomic::Final => {
            let again = drain_again(heap, budget.left);
            spend(heap, budget, again);
            if budget.left == 0 || propagate(heap, budget, buf) {
                return;
            }
            flip(heap, max_objects);
        }
    }
}

/// Gray again objects written to or made since they were traced, at
/// most `limit` of them, a unit each: they wait in the arenas' `again`
/// lists, taken last first. An entry no longer gray (a young object a
/// young collection's sweep made white since) is dropped for nothing.
/// Returns how many; fewer than `limit` when the lists are empty.
fn drain_again(heap: &mut Heap, limit: u64) -> u64 {
    let mut count = 0u64;
    macro_rules! drain {
        ($arena:ident, $kind:expr) => {
            while count < limit {
                let Some(index) = heap.$arena.pop_again() else {
                    break;
                };
                if heap.$arena.mark_of(index) == Some(mark::GRAY) {
                    count += 1;
                    let object = TraceRef { kind: $kind, index };
                    uncount(heap, object);
                    heap.collector.gray.push(object);
                    // An old object written to since the last young
                    // collection: traced once more by the next.
                    if heap.collector.minor && heap.$arena.age_of(index) == age::TOUCHED1 {
                        heap.collector.touched.push(object);
                    }
                }
            }
        };
    }
    drain!(strings, Kind::String);
    drain!(tables, Kind::Table);
    drain!(protos, Kind::Proto);
    drain!(upvalues, Kind::Upvalue);
    drain!(closures, Kind::Closure);
    drain!(threads, Kind::Thread);
    drain!(native_closures, Kind::NativeClosure);
    drain!(userdata, Kind::Userdata);
    count
}

/// An object about to be traced again: it was counted when first traced
/// (or, made while marking, in the bytes allocated since the cycle
/// began), and is counted again when traced, so it counts once.
fn uncount(heap: &mut Heap, object: TraceRef) {
    let size = size_of(heap, object);
    heap.collector.marked_bytes = heap.collector.marked_bytes.saturating_sub(size);
}

/// Read each weak table's mode again, one unit each. A changed mode
/// supersedes the entry, and the table is traced again with its present
/// mode, so a table made strong late keeps what it holds. True when done.
fn modes(heap: &mut Heap, budget: &mut Budget) -> bool {
    while budget.left > 0 {
        let cursor = heap.collector.cursor as usize;
        let Some(&entry) = heap.collector.weak.get(cursor) else {
            return true;
        };
        heap.collector.cursor += 1;
        spend(heap, budget, 1);
        if !entry.keys && !entry.values {
            continue;
        }
        let metatable = heap
            .tables
            .slot_value(entry.table)
            .and_then(|table| table.metatable);
        if weak_mode(heap, metatable) == (entry.keys, entry.values) {
            continue;
        }
        heap.collector.weak[cursor].keys = false;
        heap.collector.weak[cursor].values = false;
        let table = TraceRef {
            kind: Kind::Table,
            index: entry.table,
        };
        if heap.mark_of(table) == Some(mark::BLACK) {
            heap.set_mark(table, mark::GRAY);
            uncount(heap, table);
            heap.collector.gray.push(table);
        }
    }
    heap.collector.cursor as usize >= heap.collector.weak.len()
}

/// Remove dead entries from the weak tables, one unit per slot: values
/// (`keys` false), or keys and the values of tables resurrection reached
/// first (`keys` true). A removal is the table's own delete, so the key
/// leaves a dead anchor and `next` goes on past it, and no slot moves.
/// True when done.
fn clear(heap: &mut Heap, budget: &mut Budget, keys: bool) -> bool {
    let dead = |heap: &Heap, value: Value| {
        weak_object(value).is_some_and(|object| heap.mark_of(object).is_some_and(mark::is_white))
    };
    let mut doomed = Vec::new();
    while budget.left > 0 {
        let cursor = heap.collector.cursor as usize;
        let Some(&entry) = heap.collector.weak.get(cursor) else {
            return true;
        };
        let (clear_keys, clear_values) = if keys {
            (
                entry.keys,
                entry.values && cursor >= heap.collector.late as usize,
            )
        } else {
            (false, entry.values)
        };
        let from = heap.collector.inner as usize;
        let slots = heap
            .tables
            .slot_value(entry.table)
            .map_or(&[][..], |table| table.table.slots());
        if (!clear_keys && !clear_values) || from >= slots.len() {
            spend(heap, budget, 1);
            heap.collector.cursor += 1;
            heap.collector.inner = 0;
            continue;
        }
        let len = slots.len();
        let end = from
            .saturating_add(budget.left.min(CHUNK) as usize)
            .min(len);
        for (offset, slot) in slots[from..end].iter().enumerate() {
            if let Slot::Live {
                key_value, value, ..
            } = slot
                && ((clear_keys && dead(heap, *key_value)) || (clear_values && dead(heap, *value)))
            {
                doomed.push(from + offset);
            }
        }
        spend(heap, budget, (end - from) as u64);
        if let Some(table) = heap.tables.raw_mut(entry.table) {
            for slot in doomed.drain(..) {
                table.table.delete_slot(slot);
            }
        }
        if end < len {
            heap.collector.inner = end as u32;
        } else {
            heap.collector.cursor += 1;
            heap.collector.inner = 0;
        }
    }
    false
}

/// Move the registered objects left white to the finalizer queue, newest
/// registration first (Lua's `separatetobefnz`), and gray them: they are
/// resurrected until their finalizers have run. One batch, a unit per
/// registered object looked at: every one, but in a young collection only
/// those registered since the one before it, as the others are old (Lua
/// stops at `finobjold1`). There are at most as many as objects.
fn separate(heap: &mut Heap, budget: &mut Budget) {
    heap.collector.late = heap.collector.weak.len() as u32;
    let fin = &mut heap.finalizers;
    let from = if heap.collector.minor {
        (fin.old_until as usize).min(fin.registered.len())
    } else {
        0
    };
    let (len, new_from) = (fin.registered.len(), fin.new_from as usize);
    spend(heap, budget, (len - from).max(1) as u64);
    let mut dead = Vec::new();
    let (mut kept, mut removed_old) = (from, 0);
    for at in from..len {
        let entry = heap.finalizers.registered[at];
        if heap.mark_of(entry.trace()).is_some_and(mark::is_white) {
            if at < new_from {
                removed_old += 1;
            }
            dead.push(entry);
        } else {
            heap.finalizers.registered[kept] = entry;
            kept += 1;
        }
    }
    let fin = &mut heap.finalizers;
    fin.registered.truncate(kept);
    fin.new_from = (fin.new_from as usize).saturating_sub(removed_old) as u32;
    for fin in dead.into_iter().rev() {
        heap.finalizers.pending.push_back(fin);
        mark(heap, fin.trace());
    }
}

/// The atomic phase is over: what is white is dead. The whites swap, so a
/// new object (the new white) is never mistaken for one. A young
/// collection sweeps its young lists; another cycle in generational mode
/// decides here whether its sweep makes what survives old.
fn flip(heap: &mut Heap, max_objects: u32) {
    let debt = heap.gc.debt;
    let collector = &mut heap.collector;
    if !collector.minor {
        collector.marking_debt = debt.saturating_sub(collector.debt_base);
        collector.debt_base = debt;
    }
    collector.white ^= 1;
    collector.weak.clear();
    collector.ephemerons.clear();
    collector.waiting.clear();
    collector.scan = None;
    collector.cursor = 0;
    collector.inner = 0;
    collector.late = 0;
    collector.reset = false;
    collector.sweep_at = (0, 0);
    if !collector.minor {
        let kept = collector
            .marked_bytes
            .saturating_add(collector.marking_debt);
        let gc = &heap.gc;
        let to_old = match collector.decide {
            Decide::None => false,
            Decide::ToGen => true,
            // Lua's `genstep`: good when at least half the growth since
            // the last major collection was freed.
            Decide::Major => {
                kept < gc
                    .major_base
                    .saturating_add(gc.major_growth(max_objects) / 2)
            }
            // Lua's `stepgenfull`: good when what is kept grew less than
            // an eighth since the bad collection.
            Decide::Fallback => kept < gc.bad.saturating_add(gc.bad / 8),
        };
        let decide = collector.decide;
        collector.to_old = to_old;
        collector.generational = to_old;
        if to_old {
            heap.gc.bad = 0;
        } else if matches!(decide, Decide::Major | Decide::Fallback) {
            heap.gc.bad = kept.max(1);
        }
        // What is registered now survives the sweep, old if it makes
        // survivors old.
        let fin = &mut heap.finalizers;
        let known = if to_old {
            fin.registered.len() as u32
        } else {
            0
        };
        fin.old_until = known;
        fin.new_from = known;
        heap.gc
            .note(0x400 | u64::from(decide.tag()) << 1 | u64::from(to_old));
    }
    enter(heap, Phase::Sweep);
    macro_rules! condemn {
        ($($arena:ident),*) => {
            $(heap.$arena.condemn();)*
        };
    }
    condemn!(
        strings,
        tables,
        protos,
        upvalues,
        closures,
        threads,
        native_closures,
        userdata
    );
    heap.collector.sweep_left = if heap.collector.minor {
        KINDS.iter().map(|&kind| young_len(heap, kind) as u64).sum()
    } else {
        u64::from(heap.physical_objects())
    };
}

fn young_len(heap: &Heap, kind: Kind) -> usize {
    crate::heap::on_arena!(heap, kind, arena => arena.young().len())
}

/// Abandon a cycle that is marking: nothing is decided yet, so making
/// every marked object white again frees nothing.
fn abort(heap: &mut Heap) {
    let collector = &mut heap.collector;
    collector.gray.clear();
    collector.scan = None;
    collector.weak.clear();
    collector.ephemerons.clear();
    collector.waiting.clear();
    collector.cursor = 0;
    collector.inner = 0;
    collector.late = 0;
    collector.marked_bytes = 0;
    collector.reset = true;
    collector.sweep_at = (0, 0);
    drain_again(heap, u64::MAX);
    heap.collector.gray.clear();
    let marked = heap.strings.marked()
        + heap.tables.marked()
        + heap.protos.marked()
        + heap.upvalues.marked()
        + heap.closures.marked()
        + heap.threads.marked()
        + heap.native_closures.marked()
        + heap.userdata.marked();
    heap.collector.sweep_left = u64::from(marked);
    enter(heap, Phase::Sweep);
}

/// Sweep, one unit per object passed: a dead one is freed (a userdata's
/// host value dropped: Rust `Drop`, not `__gc`), any other made white. The
/// count of objects left is exact; after a restore, which leaves dead
/// objects out, the units they would have taken are still counted, so the
/// sweep ends where it would have.
fn sweep(heap: &mut Heap, budget: &mut Budget, max_objects: u32) {
    let take = budget.left.min(heap.collector.sweep_left);
    let passed = sweep_slots(heap, take);
    // Units no object is left for: dead objects a restore left out.
    let phantom = take.saturating_sub(passed);
    heap.collector.sweep_left -= passed + phantom;
    spend(heap, budget, passed + phantom);
    if heap.collector.sweep_left == 0 {
        end_cycle(heap, max_objects);
    }
}

/// Pass up to `limit` objects in slot order, arena by arena, from where
/// the sweep is. Returns how many were passed.
fn sweep_slots(heap: &mut Heap, limit: u64) -> u64 {
    let white = heap.collector.white;
    let resets = heap.collector.reset;
    let (mut arena, mut slot) = heap.collector.sweep_at;
    let mut passed = 0;
    let mut freed = heap.collector.unreleased;
    let to_old = heap.collector.to_old;
    while arena < 8 && passed < limit {
        let left = limit - passed;
        let kind = KINDS[usize::from(arena)];
        let done = if to_old {
            crate::heap::on_arena!(mut heap, kind, a => a.sweep_old_some(&mut slot, white, left, &mut freed))
        } else {
            crate::heap::on_arena!(mut heap, kind, a => a.sweep_some(&mut slot, white, left, resets, &mut freed))
        };
        passed += done;
        if done < left {
            arena += 1;
            slot = 0;
        }
    }
    heap.collector.sweep_at = (arena, slot);
    heap.collector.unreleased = freed;
    passed
}

/// A sweep has ended: what it freed leaves the logical heap.
fn release(heap: &mut Heap) {
    heap.hooks.reap(&heap.threads);
    let freed = std::mem::take(&mut heap.collector.unreleased);
    heap.gc.used = heap.gc.used.saturating_sub(freed);
}

fn end_cycle(heap: &mut Heap, max_objects: u32) {
    // Objects a sweep never reached: none, but after a restore, which
    // leaves dead objects out.
    sweep_slots(heap, u64::MAX);
    release(heap);
    #[cfg(test)]
    audit(heap);
    macro_rules! settle {
        ($($arena:ident),*) => {
            $(heap.$arena.clear_dead();)*
        };
    }
    settle!(
        strings,
        tables,
        protos,
        upvalues,
        closures,
        threads,
        native_closures,
        userdata
    );
    let reset = heap.collector.reset;
    let to_old = heap.collector.to_old;
    heap.collector.reset = false;
    heap.collector.to_old = false;
    heap.collector.sweep_at = (0, 0);
    enter(heap, Phase::Pause);
    if !reset {
        count!("gc_cycles");
        // What the cycle kept: what it traced, as it was then, and what
        // was made while it marked. What was made while it swept is the
        // next cycle's debt.
        let kept = heap
            .collector
            .marked_bytes
            .saturating_add(heap.collector.marking_debt);
        let swept = heap.gc.debt.saturating_sub(heap.collector.debt_base);
        let objects = heap.live_objects();
        heap.gc.cycle_work = heap.gc.work.saturating_sub(heap.collector.work_base);
        heap.collector.decide = Decide::None;
        if to_old {
            // The heap now, but what was made while the sweep ran: the
            // base for the next major collection.
            let live = heap.gc.used.saturating_sub(swept);
            heap.gc.major_done(live, swept, objects, max_objects);
        } else {
            heap.gc.cycle_done(kept, swept, objects, max_objects);
        }
        heap.gc.note(0x100 | u64::from(to_old));
    }
    heap.collector.marked_bytes = 0;
    heap.collector.marking_debt = 0;
    heap.collector.debt_base = 0;
    heap.collector.work_base = 0;
    // Incremental mode was chosen while the sweep made the heap old.
    if to_old && !heap.gc.generational {
        leave_gen(heap);
    }
}

/// Schedule collector work once a step is due (ADR 0050, ADR 0051). In
/// generational form: a young collection, done whole before Lua goes on,
/// or a major collection once memory has grown past the major multiplier
/// since the last one, an incremental cycle in steps (`whole`, for
/// `collectgarbage("step")`: all of it, as Lua's). Otherwise, an
/// incremental step.
///
/// Returns whether a whole step was a generational one, Lua's `genstep`
/// not falling back: it leaves the collector in generational mode, so
/// `collectgarbage("step")` returns false.
pub(crate) fn step(heap: &mut Heap, room: u64, max_objects: u32, whole: bool) -> bool {
    if heap.collector.holds() {
        return false;
    }
    let collector = &heap.collector;
    if whole && heap.gc.generational && !collector.generational {
        if heap.gc.bad > 0 {
            // Falling back after a bad major (Lua's `stepgenfull`): a
            // whole cycle that decides whether to return to young
            // collections; one running finishes first, deciding so too.
            let resetting = collector.phase == Phase::Sweep && collector.reset;
            if collector.phase == Phase::Pause || resetting {
                heap.collector.decide = Decide::Fallback;
            }
            want_cycle(heap);
            return false;
        }
        // A major collection running: all of it, as Lua's.
        want_cycle(heap);
        return true;
    }
    if collector.generational && collector.phase == Phase::Pause {
        if !whole {
            // Decided by the work loop, when it runs.
            heap.gc.owed = heap.gc.owed.max(1);
        } else if heap.gc.sched > 0 && heap.gc.major_due(max_objects) {
            // Lua's `genstep` does a major collection only with debt to
            // pay: `step(0)`, which clears the debt, is a young one.
            leave_gen(heap);
            heap.collector.decide = Decide::Major;
            want_cycle(heap);
        } else {
            start_minor(heap);
        }
        return whole;
    }
    heap.gc.schedule_step(room);
    begin(heap);
    false
}

/// Work on until the cycle running has completed, or the next one when
/// none counts (none running, or a sweep that only makes marks white).
fn want_cycle(heap: &mut Heap) {
    let target = heap.gc.collections.saturating_add(1);
    heap.gc.full = Some(heap.gc.full.map_or(target, |old| old.max(target)));
}

/// Logical bytes left before the quota or the object limit (each object
/// costs at least `cost::OBJECT`), whichever is nearer.
fn room(heap: &Heap, max_objects: u32) -> u64 {
    let objects = max_objects.saturating_sub(heap.live_objects());
    heap.gc.headroom().min(cost::OBJECT * u64::from(objects))
}

/// Begin a young collection (Lua's `youngcollection`).
pub(crate) fn start_minor(heap: &mut Heap) {
    let collector = &mut heap.collector;
    collector.minor = true;
    collector.promoted = 0;
    collector.work_base = heap.gc.work;
    heap.gc.owed = 0;
    enter(heap, Phase::Begin);
}

/// The first units of a young collection: the old objects to trace again
/// (Lua's `markold`), a unit each, become `OLD` and gray. Those written to
/// since are gray already, and stay touched.
fn begin_minor(heap: &mut Heap, budget: &mut Budget) {
    while budget.left > 0 {
        spend(heap, budget, 1);
        let Some(object) = heap.collector.revisit.pop() else {
            enter(heap, Phase::Atomic(Atomic::Roots));
            return;
        };
        if matches!(heap.age_of(object), age::OLD1 | age::TOUCHED2) {
            heap.set_age(object, age::OLD);
            if heap.mark_of(object) == Some(mark::BLACK) {
                heap.set_mark(object, mark::GRAY);
                heap.collector.gray.push(object);
            }
        }
    }
}

/// A young collection's sweep, a unit per object on the young lists at
/// the end of its atomic phase (Lua's `sweepgen`): a dead one is freed; a
/// new one survives as `SURVIVAL`, white again; a `SURVIVAL` one becomes
/// `OLD1`, black, for the next young collection to trace (a string, with
/// nothing to trace, `OLD`), or touched if written to since it was traced.
fn sweep_young(heap: &mut Heap, budget: &mut Budget) {
    let take = budget.left.min(heap.collector.sweep_left);
    let passed = sweep_young_lists(heap, take);
    let phantom = take.saturating_sub(passed);
    heap.collector.sweep_left -= passed + phantom;
    spend(heap, budget, passed + phantom);
    if heap.collector.sweep_left == 0 {
        release(heap);
        // What is left on the lists was made since the atomic phase.
        for kind in KINDS {
            crate::heap::on_arena!(mut heap, kind, arena => arena.young_mut().retain(|index| *index != TOMB));
        }
        macro_rules! settle {
            ($($arena:ident),*) => {
                $(heap.$arena.clear_dead();)*
            };
        }
        settle!(
            strings,
            tables,
            protos,
            upvalues,
            closures,
            threads,
            native_closures,
            userdata
        );
        heap.collector.sweep_at = (0, 0);
        enter(heap, Phase::Touched);
    }
}

fn sweep_young_lists(heap: &mut Heap, limit: u64) -> u64 {
    let white = heap.collector.white;
    let (mut arena, mut pos) = heap.collector.sweep_at;
    let mut passed = 0;
    let mut freed = heap.collector.unreleased;
    let mut promoted = heap.collector.promoted;
    let mut revisit = Vec::new();
    while arena < 8 && passed < limit {
        let kind = KINDS[usize::from(arena)];
        let left = limit - passed;
        let done = crate::heap::on_arena!(mut heap, kind, a => a.sweep_young_some(
            &mut pos,
            white,
            left,
            kind != Kind::String,
            &mut freed,
            &mut promoted,
            &mut revisit,
        ));
        heap.collector
            .revisit
            .extend(revisit.drain(..).map(|index| TraceRef { kind, index }));
        passed += done;
        if done < left {
            arena += 1;
            pos = 0;
        }
    }
    heap.collector.unreleased = freed;
    heap.collector.promoted = promoted;
    heap.collector.sweep_at = (arena, pos);
    passed
}

/// The end of a young collection (Lua's `correctgraylist`): each touched
/// object it traced, a unit each, becomes `TOUCHED2`, to be traced by the
/// next one too, unless written to again since.
fn correct(heap: &mut Heap, budget: &mut Budget, max_objects: u32) {
    while budget.left > 0 {
        spend(heap, budget, 1);
        let Some(object) = heap.collector.touched.pop() else {
            end_minor(heap, max_objects);
            return;
        };
        if heap.age_of(object) == age::TOUCHED1 && heap.mark_of(object) == Some(mark::BLACK) {
            heap.set_age(object, age::TOUCHED2);
            heap.collector.revisit.push(object);
        }
    }
}

/// Test hook: the exact count holds as a collection ends.
#[cfg(test)]
fn audit(heap: &Heap) {
    if heap.collector.audit {
        check_usage(heap).unwrap_or_else(|error| panic!("logical heap: {error}"));
    }
}

fn end_minor(heap: &mut Heap, max_objects: u32) {
    count!("gc_cycles");
    count!("gc_minor_cycles");
    #[cfg(test)]
    audit(heap);
    // What the young collection kept: the heap, exactly.
    let objects = heap.live_objects();
    heap.gc.minor_done(heap.gc.used, objects, max_objects);
    // What was registered since the young collection before this one is
    // old now, or was found dead.
    let fin = &mut heap.finalizers;
    fin.old_until = fin.new_from;
    fin.new_from = fin.registered.len() as u32;
    let promoted = heap.collector.promoted;
    let remembered = heap.collector.revisit.len() as u64;
    heap.collector.minor = false;
    heap.collector.promoted = 0;
    heap.collector.work_base = 0;
    heap.collector.marked_bytes = 0;
    enter(heap, Phase::Pause);
    heap.gc.note(0x200 ^ promoted << 16 ^ remembered << 40);
}

/// Leave generational form (Lua's `enterinc`): the young and remembered
/// lists go, and a sweep that frees nothing makes every old object white
/// and every age new, a unit per old object, so the next cycle traces
/// everything.
pub(crate) fn leave_gen(heap: &mut Heap) {
    if !heap.collector.generational {
        return;
    }
    let collector = &mut heap.collector;
    collector.generational = false;
    collector.revisit.clear();
    collector.touched.clear();
    collector.reset = true;
    heap.finalizers.old_until = 0;
    heap.finalizers.new_from = 0;
    let collector = &mut heap.collector;
    collector.sweep_at = (0, 0);
    collector.marked_bytes = 0;
    for kind in KINDS {
        crate::heap::on_arena!(mut heap, kind, arena => {
            arena.young_mut().clear();
            arena.clear_again();
        });
    }
    let marked = heap.strings.marked()
        + heap.tables.marked()
        + heap.protos.marked()
        + heap.upvalues.marked()
        + heap.closures.marked()
        + heap.threads.marked()
        + heap.native_closures.marked()
        + heap.userdata.marked();
    heap.collector.sweep_left = u64::from(marked);
    enter(heap, Phase::Sweep);
}

/// The tri-color invariant, for restore and tests: while a cycle marks,
/// no black object refers to a white one (but the object being scanned,
/// an ephemeron table waiting, and what weak tables hold weakly).
pub(crate) fn check_invariant(heap: &Heap) -> Result<(), String> {
    let collector = &heap.collector;
    if (collector.marking() && !collector.generational)
        || (collector.minor && collector.in_atomic())
    {
        let waiting: std::collections::HashSet<u32> =
            heap.collector.ephemerons.iter().copied().collect();
        let mut buf = Vec::new();
        macro_rules! check {
            ($arena:ident, $kind:expr) => {
                for (index, _, _) in heap.$arena.iter() {
                    let object = TraceRef { kind: $kind, index };
                    if heap.mark_of(object) != Some(mark::BLACK) {
                        continue;
                    }
                    // The object being scanned is black with references
                    // not traced yet, and so is an ephemeron table waiting.
                    if heap
                        .collector
                        .scan
                        .is_some_and(|scan| scan.object == object)
                        || ($kind == Kind::Table && waiting.contains(&index))
                    {
                        continue;
                    }
                    all_references(heap, object, &mut buf);
                    for child in buf.drain(..) {
                        if heap.mark_of(child).is_some_and(mark::is_white) {
                            return Err(format!("black {object:?} refers to white {child:?}"));
                        }
                    }
                }
            };
        }
        check!(strings, Kind::String);
        check!(tables, Kind::Table);
        check!(protos, Kind::Proto);
        check!(upvalues, Kind::Upvalue);
        check!(closures, Kind::Closure);
        check!(threads, Kind::Thread);
        check!(native_closures, Kind::NativeClosure);
        check!(userdata, Kind::Userdata);
    }
    // C1: a frame holds cold storage exactly while one exceptional field is
    // live, so presence is the data and an empty box cannot be mistaken for
    // a continuation.
    for (index, _, thread) in heap.threads.iter() {
        for (depth, frame) in thread.frames.iter().enumerate() {
            if frame.cold.as_ref().is_some_and(|cold| cold.is_empty()) {
                return Err(format!(
                    "thread {index} frame {depth} holds empty cold storage"
                ));
            }
        }
        // I4: frame slots kept beyond the live depth hold no cold state.
        if !thread.frames.stale_slots_hold_no_cold() {
            return Err(format!(
                "thread {index} keeps cold storage beyond frame depth {}",
                thread.frames.len()
            ));
        }
        // The open-upvalue summary is exactly the recomputation.
        let open_above = thread
            .open_upvalues
            .iter()
            .map(|(slot, _)| slot + 1)
            .max()
            .unwrap_or(0);
        if thread.open_above != open_above {
            return Err(format!(
                "thread {index} open_above {} but open slots end at {open_above}",
                thread.open_above
            ));
        }
    }
    Ok(())
}

/// The generational invariant (ADR 0051), for restore and tests, in every
/// phase of generational form: no young object can be freed while an old
/// one refers to it, weakly or not, unless a young collection traces the
/// old one first. Ages and marks agree with the lists that make them
/// mean something, and every gray object waits in one.
///
/// - **Between collections** and **as a young collection begins**: young
///   objects are white and listed, old ones marked; an `OLD1` or
///   `TOUCHED2` object is remembered (`revisit`), a `TOUCHED1` one gray.
///   A black object refers to no white one unless remembered.
/// - **In a young collection's atomic phase**: the tri-color invariant,
///   weak tables not reached this cycle included; nothing is `OLD1` or
///   `TOUCHED2` any more, and a `TOUCHED1` object is gray or on `touched`.
/// - **In its sweep and correction**: nothing is gray; `TOUCHED1` objects
///   are on `touched`, `OLD1` and `TOUCHED2` ones remembered. A black
///   object not on one of those lists, nor a young survivor the sweep has
///   still to pass, refers to no white object and no new one (which the
///   sweep makes white).
/// - **In a sweep making survivors old**: survivors are new and marked
///   until reached, then old and black or touched and gray; objects made
///   since are new, white, and listed. No black object refers to a white
///   one.
///
/// In every phase but a sweep making survivors old, an old object not
/// written to since the last young collection (`OLD`, `OLD1`, `TOUCHED2`)
/// refers to no new object, which a sweep would make young: only one a
/// running sweep made `OLD1` may, until the sweep passes the new one.
///
/// Registered objects before `Finalizers::old_until` are old, and those
/// up to `new_from` survived a young collection (are old once a running
/// one's sweep has passed them).
pub(crate) fn check_gen_invariant(heap: &Heap) -> Result<(), String> {
    use std::collections::HashSet;
    let c = &heap.collector;
    let fin = &heap.finalizers;
    if !c.generational {
        if fin.old_until != 0 || fin.new_from != 0 {
            return Err("registered objects known old out of generational form".into());
        }
        return Ok(());
    }
    if fin.old_until > fin.new_from || fin.new_from as usize > fin.registered.len() {
        return Err("registered boundaries out of order".into());
    }
    let atomic = c.minor && c.in_atomic();
    let late = c.minor && matches!(c.phase, Phase::Sweep | Phase::Touched);
    let early = !c.to_old && !atomic && !late;
    let revisit: HashSet<TraceRef> = c.revisit.iter().copied().collect();
    let touched: HashSet<TraceRef> = c.touched.iter().copied().collect();
    let mut gray: HashSet<TraceRef> = c.gray.iter().copied().collect();
    let mut young = HashSet::new();
    for kind in KINDS {
        crate::heap::on_arena!(heap, kind, arena => {
            gray.extend(arena.again().iter().map(|&index| TraceRef { kind, index }));
            young.extend(
                arena.young().iter().filter(|&&index| index != TOMB).map(|&index| TraceRef { kind, index }),
            );
        });
    }
    let scanned = c.scan.map(|scan| scan.object);
    let waiting: HashSet<u32> = c.ephemerons.iter().copied().collect();
    let reached: HashSet<u32> = c.weak.iter().map(|entry| entry.table).collect();
    let white = |object: TraceRef| heap.mark_of(object).is_some_and(mark::is_white);
    let mut buf = Vec::new();
    for kind in KINDS {
        let indices: Vec<u32> = crate::heap::on_arena!(heap, kind, arena => arena.iter().map(|(index, _, _)| index).collect());
        for index in indices {
            let object = TraceRef { kind, index };
            let found = heap.mark_of(object).unwrap_or(c.white);
            let object_age = heap.age_of(object);
            let fail = |what: &str| {
                Err(format!(
                    "{object:?} (mark {found}, age {object_age}) {what}"
                ))
            };
            let is_young = object_age <= age::SURVIVAL;
            if c.to_old {
                // Only what was made since the atomic phase is young.
                let listed = young.contains(&object);
                let fits = match (mark::is_white(found), object_age) {
                    (true, age::NEW) => listed,
                    (false, age::NEW) => !listed,
                    (false, age::OLD) => found == mark::BLACK && !listed,
                    (false, age::TOUCHED1) => found == mark::GRAY && !listed,
                    _ => false,
                };
                if !fits {
                    return fail("does not fit a sweep making survivors old");
                }
            } else if is_young != young.contains(&object) {
                return fail("is listed young or not against its age");
            } else if !is_young && mark::is_white(found) {
                return fail("is old and white");
            }
            if found == mark::GRAY && (late || !gray.contains(&object)) {
                return fail("is gray and waits nowhere");
            }
            // Nothing is made while a young collection runs: past its
            // atomic phase a white object is a survivor its sweep passed.
            if late && mark::is_white(found) && object_age == age::NEW {
                return fail("is new after a young collection's atomic phase");
            }
            let fits = match object_age {
                age::OLD1 | age::TOUCHED2 => !atomic && revisit.contains(&object),
                age::TOUCHED1 if early => found == mark::GRAY,
                age::TOUCHED1 if atomic => found == mark::GRAY || touched.contains(&object),
                age::TOUCHED1 if late => touched.contains(&object),
                _ => true,
            };
            if !fits {
                return fail("is not on the list its age needs");
            }
            // An old object refers to no new one: only a written one
            // (`TOUCHED1`) can, and an `OLD1` or `TOUCHED2` one only as a
            // young collection's sweep made it so, the new one not passed
            // yet. A new object is young after the next sweep.
            let old_parent = !c.to_old
                && matches!(object_age, age::OLD | age::OLD1 | age::TOUCHED2)
                && !(late && revisit.contains(&object));
            if old_parent {
                every_reference(heap, object, &mut buf);
                for child in buf.drain(..) {
                    if heap.age_of(child) == age::NEW {
                        return Err(format!(
                            "old {object:?} (age {object_age}) refers to new {child:?}"
                        ));
                    }
                }
            }
            if found != mark::BLACK {
                continue;
            }
            // A black object a young collection still traces, or whose
            // references the sweep keeps young-safe.
            let covered = if atomic {
                scanned == Some(object) || (kind == Kind::Table && waiting.contains(&index))
            } else if c.to_old {
                false
            } else {
                revisit.contains(&object)
                    || (late && (touched.contains(&object) || young.contains(&object)))
            };
            if covered {
                continue;
            }
            // Weak references count: a table no young collection traces
            // is never cleared of a young object it holds.
            if atomic && kind == Kind::Table && reached.contains(&index) {
                all_references(heap, object, &mut buf);
            } else {
                every_reference(heap, object, &mut buf);
            }
            for child in buf.drain(..) {
                if white(child) || (late && heap.age_of(child) == age::NEW) {
                    return Err(format!(
                        "black {object:?} (age {object_age}) refers to young {child:?} (age {})",
                        heap.age_of(child)
                    ));
                }
            }
        }
    }
    // Before `old_until`, old; up to `new_from`, registered before the
    // last young collection, so survivors of it: old after the next.
    let (old_until, new_from) = (fin.old_until as usize, fin.new_from as usize);
    for (at, entry) in fin.registered[..new_from].iter().enumerate() {
        let object = entry.trace();
        let object_age = heap.age_of(object);
        let fits = if at < old_until {
            !white(object) && (c.to_old || object_age > age::SURVIVAL)
        } else if late {
            // Old once the sweep has passed it: a survivor it has still
            // to pass, not one it made a survivor.
            object_age > age::SURVIVAL || (object_age == age::SURVIVAL && !white(object))
        } else {
            object_age != age::NEW
        };
        if !fits {
            return Err(format!(
                "registered {object:?} (age {object_age}, white {}) at {at} is younger than its place ({old_until}..{new_from})",
                white(object)
            ));
        }
    }
    Ok(())
}

/// Every reference a live object holds, weak ones too, and every object
/// the finalizer lists name, is to a live object: no handle is stale. For
/// tests.
#[cfg(test)]
pub(crate) fn check_references(heap: &Heap) -> Result<(), String> {
    let c = &heap.collector;
    let sweeping = c.phase == Phase::Sweep && !c.reset;
    let dead = |object: TraceRef| {
        heap.mark_of(object)
            .is_none_or(|found| sweeping && mark::is_white(found) && found != c.white)
    };
    let mut buf = Vec::new();
    for kind in KINDS {
        let indices: Vec<u32> = crate::heap::on_arena!(heap, kind, arena => arena.iter().map(|(index, _, _)| index).collect());
        for index in indices {
            let object = TraceRef { kind, index };
            every_reference(heap, object, &mut buf);
            for child in buf.drain(..) {
                if dead(child) {
                    return Err(format!("{object:?} refers to freed {child:?}"));
                }
            }
        }
    }
    let fin = &heap.finalizers;
    for entry in fin.registered.iter().chain(fin.pending.iter()) {
        if dead(entry.trace()) {
            return Err(format!("a finalizer list names freed {:?}", entry.trace()));
        }
    }
    Ok(())
}

/// Every reference an object holds, weak ones too.
fn every_reference(heap: &Heap, object: TraceRef, buf: &mut Vec<TraceRef>) {
    if object.kind != Kind::Table {
        all_references(heap, object, buf);
        return;
    }
    let Some(table) = heap.tables.slot_value(object.index) else {
        return;
    };
    buf.extend(table.metatable.map(|metatable| TraceRef {
        kind: Kind::Table,
        index: metatable.index,
    }));
    for slot in table.table.slots() {
        if let Slot::Live {
            key_value, value, ..
        } = slot
        {
            buf.extend(value_ref(*key_value));
            buf.extend(value_ref(*value));
        }
    }
}

/// Every reference an object holds that its tracing would follow, weak
/// tables' as their mode says (ephemeron values whose key is white
/// excluded).
fn all_references(heap: &Heap, object: TraceRef, buf: &mut Vec<TraceRef>) {
    let index = object.index;
    match object.kind {
        Kind::Table => {
            let Some(table) = heap.tables.slot_value(index) else {
                return;
            };
            // The mode the table was traced with; a change waits for the
            // atomic phase.
            let (keys, values) = heap
                .collector
                .weak
                .iter()
                .rev()
                .find(|entry| entry.table == index && (entry.keys || entry.values))
                .map_or_else(
                    || weak_mode(heap, table.metatable),
                    |entry| (entry.keys, entry.values),
                );
            buf.extend(table.metatable.map(|metatable| TraceRef {
                kind: Kind::Table,
                index: metatable.index,
            }));
            for slot in table.table.slots() {
                if let Slot::Live {
                    key_value, value, ..
                } = slot
                {
                    let key = weak_object(*key_value);
                    let held = values
                        && keys
                        && weak_object(*value)
                            .is_some_and(|value| heap.mark_of(value) != Some(mark::BLACK));
                    if !(keys && key.is_some()) && !held {
                        buf.extend(value_ref(*key_value));
                    }
                    let ephemeral =
                        keys && key.is_some_and(|key| heap.mark_of(key) != Some(mark::BLACK));
                    if !(values && weak_object(*value).is_some()) && !ephemeral {
                        buf.extend(value_ref(*value));
                    }
                }
            }
        }
        Kind::Thread => {
            thread_references(heap, index, 0, u64::MAX, buf);
        }
        Kind::Proto => {
            proto_references(heap, index, 0, usize::MAX, buf);
        }
        _ => small_references(heap, object, buf),
    }
}

#[cfg(test)]
mod tests;
