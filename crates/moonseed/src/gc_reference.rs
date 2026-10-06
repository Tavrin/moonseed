//! The Phase 3.27 stop-the-world collector, kept as the reference the
//! incremental collector is checked against (ADR 0050): given the same
//! heap and roots, a completed incremental cycle must keep, clear, and
//! queue exactly what this does. Not used by the runtime.
//!
//! One collection, in Lua's order (`atomic` in `lgc.c`):
//! 1. mark from the roots, objects waiting for their finalizers included,
//!    settling ephemerons as their keys are marked;
//! 2. remove dead values from weak-value tables;
//! 3. move registered objects found dead to the finalizer queue, newest
//!    registration first, and mark them and what they reach (resurrection),
//!    settling ephemerons again;
//! 4. remove dead keys from weak-key tables, and dead values from the
//!    weak-value tables step 3 reached;
//! 5. sweep.
//!
//! Strings are values to weak tables: never removed for being weak.
//! Numbers, booleans, light userdata, and builtins are not objects.

use crate::heap::{Frame, Heap, Pending, TraceRef, UpvalueState};
use crate::id::Kind;
use crate::table::Slot;
use crate::value::Value;

#[derive(Default)]
struct Marks {
    strings: Vec<bool>,
    tables: Vec<bool>,
    protos: Vec<bool>,
    upvalues: Vec<bool>,
    closures: Vec<bool>,
    threads: Vec<bool>,
    native_closures: Vec<bool>,
    userdata: Vec<bool>,
}

impl Marks {
    fn mark(&mut self, reference: TraceRef) -> bool {
        let bits = match reference.kind {
            Kind::String => &mut self.strings,
            Kind::Table => &mut self.tables,
            Kind::Proto => &mut self.protos,
            Kind::Upvalue => &mut self.upvalues,
            Kind::Closure => &mut self.closures,
            Kind::Thread => &mut self.threads,
            Kind::NativeClosure => &mut self.native_closures,
            Kind::Userdata => &mut self.userdata,
        };
        let index = reference.index as usize;
        if index >= bits.len() || bits[index] {
            return false;
        }
        bits[index] = true;
        true
    }

    fn is_marked(&self, reference: TraceRef) -> bool {
        let bits = match reference.kind {
            Kind::String => &self.strings,
            Kind::Table => &self.tables,
            Kind::Proto => &self.protos,
            Kind::Upvalue => &self.upvalues,
            Kind::Closure => &self.closures,
            Kind::Thread => &self.threads,
            Kind::NativeClosure => &self.native_closures,
            Kind::Userdata => &self.userdata,
        };
        bits.get(reference.index as usize).copied().unwrap_or(false)
    }
}

pub(crate) fn collect(heap: &mut Heap, extra: &[TraceRef]) {
    debug_assert!(heap.collector.phase == crate::gc::Phase::Pause);
    let mut marks = Marks {
        strings: vec![false; heap.strings.slot_count()],
        tables: vec![false; heap.tables.slot_count()],
        protos: vec![false; heap.protos.slot_count()],
        upvalues: vec![false; heap.upvalues.slot_count()],
        closures: vec![false; heap.closures.slot_count()],
        threads: vec![false; heap.threads.slot_count()],
        native_closures: vec![false; heap.native_closures.slot_count()],
        userdata: vec![false; heap.userdata.slot_count()],
    };

    let mut work = Vec::new();
    for reference in extra {
        push_work(&mut marks, &mut work, *reference);
    }
    for root in &heap.host_roots {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: root.kind,
                index: root.index,
            },
        );
    }
    if let Some(held) = &heap.api_roots {
        for reference in held.borrow().values().filter_map(crate::gc::value_ref) {
            push_work(&mut marks, &mut work, reference);
        }
    }
    for handle in &heap.reserved {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::String,
                index: handle.index,
            },
        );
    }
    for handle in heap.type_metatables.iter().flatten() {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Table,
                index: handle.index,
            },
        );
    }
    if let Some(handle) = heap.registry {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Table,
                index: handle.index,
            },
        );
    }
    if let Some(handle) = heap.globals {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Table,
                index: handle.index,
            },
        );
    }
    if let Some(handle) = heap.active {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Thread,
                index: handle.index,
            },
        );
    }
    if let Some(handle) = heap.entry {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Thread,
                index: handle.index,
            },
        );
    }

    for fin in &heap.finalizers.pending {
        push_work(&mut marks, &mut work, fin.trace());
    }
    if let Some(closure) = heap.finalizers.close_closure {
        push_work(
            &mut marks,
            &mut work,
            TraceRef {
                kind: Kind::Closure,
                index: closure.index,
            },
        );
    }

    let mut weak = Weak::default();
    propagate(heap, &mut marks, &mut work, &mut weak);
    // Every strongly reachable object is marked. Weak values go before
    // finalizers are looked for, so an object being finalized is gone
    // from weak values when its finalizer runs.
    clear_weak(heap, &mut marks, &weak.tables, false, true);
    let first = weak.tables.len();
    // Registered objects found dead wait for their finalizers, newest
    // registration first, and are resurrected until they have run.
    let registered = std::mem::take(&mut heap.finalizers.registered);
    let mut kept = Vec::with_capacity(registered.len());
    let mut dead = Vec::new();
    for fin in registered {
        if marks.is_marked(fin.trace()) {
            kept.push(fin);
        } else {
            dead.push(fin);
        }
    }
    heap.finalizers.registered = kept;
    for fin in dead.into_iter().rev() {
        heap.finalizers.pending.push_back(fin);
        push_work(&mut marks, &mut work, fin.trace());
    }
    propagate(heap, &mut marks, &mut work, &mut weak);
    // Weak keys go after resurrection, so a finalizer still finds what a
    // weak-key table holds for its object. Weak-value tables first reached
    // by resurrection lose their dead values now.
    clear_weak(heap, &mut marks, &weak.tables, true, false);
    clear_weak(heap, &mut marks, &weak.tables[first..], false, true);

    sweep_unmarked(&mut heap.strings, &marks.strings);
    sweep_unmarked(&mut heap.tables, &marks.tables);
    sweep_unmarked(&mut heap.protos, &marks.protos);
    sweep_unmarked(&mut heap.upvalues, &marks.upvalues);
    sweep_unmarked(&mut heap.closures, &marks.closures);
    sweep_unmarked(&mut heap.threads, &marks.threads);
    sweep_unmarked(&mut heap.native_closures, &marks.native_closures);
    // Freeing a userdata drops its host value: Rust `Drop`, not `__gc`.
    sweep_unmarked(&mut heap.userdata, &marks.userdata);
}

/// The weak tables one collection found, and the ephemeron values
/// waiting for their keys.
#[derive(Default)]
struct Weak {
    /// Table slot index, weak keys, weak values; in the order found.
    tables: Vec<(u32, bool, bool)>,
    /// Ephemeron tables reached and not yet traversed.
    deferred: Vec<u32>,
    /// Values of weak-key entries whose key was not marked when the table
    /// was traversed, by key. Marking the key marks them: an ephemeron
    /// settles when its key does, across any number of tables, in time
    /// linear in the entries.
    waiting: std::collections::HashMap<(Kind, u32), Vec<Value>>,
}

/// Trace until nothing is left to mark. An object's ephemeron values are
/// marked when the object is.
fn propagate(heap: &Heap, marks: &mut Marks, work: &mut Vec<TraceRef>, weak: &mut Weak) {
    loop {
        while let Some(reference) = work.pop() {
            if !weak.waiting.is_empty()
                && let Some(values) = weak.waiting.remove(&(reference.kind, reference.index))
            {
                for value in values {
                    trace_value(marks, work, value);
                }
            }
            trace_object(heap, marks, work, weak, reference);
        }
        // Ephemeron tables go last, once everything else reachable is
        // marked, so most of their keys are already settled; a key still
        // unmarked waits, and marks its value if it is marked later.
        if weak.deferred.is_empty() {
            break;
        }
        for index in std::mem::take(&mut weak.deferred) {
            trace_entries(heap, marks, work, weak, index, true, false);
        }
    }
}

/// The object a value is, when a weak reference to it can be cleared:
/// tables, functions with state, threads, and full userdata. Strings are
/// values to weak tables, as in Lua; numbers, booleans, light userdata,
/// and builtins are not objects.
fn weak_object(value: Value) -> Option<TraceRef> {
    let (kind, index) = match value {
        Value::Table(handle) => (Kind::Table, handle.index),
        Value::Closure(handle) => (Kind::Closure, handle.index),
        Value::Thread(handle) => (Kind::Thread, handle.index),
        Value::NativeClosure(handle) => (Kind::NativeClosure, handle.index),
        Value::Userdata(handle) => (Kind::Userdata, handle.index),
        _ => return None,
    };
    Some(TraceRef { kind, index })
}

/// A table's weakness, read from its metatable's `__mode` at each
/// collection, as Lua does: a short string (at most 40 bytes) with `k`
/// for weak keys and `v` for weak values before any zero byte. Anything
/// else is strong.
fn weak_mode(
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

/// A table's entries: strong ones traced; a weak table's traced as its
/// mode says, and the table noted for clearing.
fn trace_table(
    heap: &Heap,
    marks: &mut Marks,
    work: &mut Vec<TraceRef>,
    weak: &mut Weak,
    index: u32,
) {
    let Some(object) = heap.tables.slot_value(index) else {
        return;
    };
    if let Some(metatable) = object.metatable {
        push_work(
            marks,
            work,
            TraceRef {
                kind: Kind::Table,
                index: metatable.index,
            },
        );
    }
    let (weak_keys, weak_values) = weak_mode(heap, object.metatable);
    if weak_keys || weak_values {
        weak.tables.push((index, weak_keys, weak_values));
    }
    if weak_keys && !weak_values {
        weak.deferred.push(index);
        return;
    }
    trace_entries(heap, marks, work, weak, index, weak_keys, weak_values);
}

/// A table's entries, traced as its mode says.
fn trace_entries(
    heap: &Heap,
    marks: &mut Marks,
    work: &mut Vec<TraceRef>,
    weak: &mut Weak,
    index: u32,
    weak_keys: bool,
    weak_values: bool,
) {
    let Some(object) = heap.tables.slot_value(index) else {
        return;
    };
    for slot in object.table.slots() {
        let Slot::Live {
            key_value, value, ..
        } = slot
        else {
            continue;
        };
        let (key_value, value) = (*key_value, *value);
        let key = if weak_keys {
            weak_object(key_value)
        } else {
            None
        };
        let weak_value = if weak_values {
            weak_object(value)
        } else {
            None
        };
        // A key that cannot be cleared is traced; so is a value that
        // cannot (a string is marked here, and kept). In a table weak both
        // ways, such a key goes with its entry when the value goes, so it
        // waits for the value (Phase 3.28; Lua marks it only if the entry
        // survives).
        if key.is_none() {
            match weak_value {
                Some(object) if weak_keys && !marks.is_marked(object) => weak
                    .waiting
                    .entry((object.kind, object.index))
                    .or_default()
                    .push(key_value),
                _ => trace_value(marks, work, key_value),
            }
        }
        if weak_value.is_some() {
            continue;
        }
        match key {
            None => trace_value(marks, work, value),
            // An ephemeron: the value is reachable through the table only
            // if the key is reachable without it.
            Some(key) if marks.is_marked(key) => trace_value(marks, work, value),
            Some(key) => weak
                .waiting
                .entry((key.kind, key.index))
                .or_default()
                .push(value),
        }
    }
}

/// Remove from `tables` the entries whose weak key (`keys`) or weak value
/// (`values`) is an object left unmarked. Each removal is the table's own
/// delete, so a removed key leaves a dead anchor and `next` goes on past
/// it, and the anchor keeps nothing alive.
fn clear_weak(
    heap: &mut Heap,
    marks: &mut Marks,
    tables: &[(u32, bool, bool)],
    keys: bool,
    values: bool,
) {
    let dead = |marks: &Marks, value: Value| {
        weak_object(value).is_some_and(|object| !marks.is_marked(object))
    };
    for &(index, weak_keys, weak_values) in tables {
        let (clear_keys, clear_values) = (keys && weak_keys, values && weak_values);
        if !clear_keys && !clear_values {
            continue;
        }
        let Some(object) = heap.tables.raw_mut(index) else {
            continue;
        };
        let doomed: Vec<crate::table::TableKey> = object
            .table
            .slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Live {
                    key,
                    key_value,
                    value,
                    ..
                } if (clear_keys && dead(marks, *key_value))
                    || (clear_values && dead(marks, *value)) =>
                {
                    Some(key.clone())
                }
                _ => None,
            })
            .collect();
        for key in doomed {
            object.table.insert(key, Value::Nil, Value::Nil);
        }
    }
}

fn push_work(marks: &mut Marks, work: &mut Vec<TraceRef>, reference: TraceRef) {
    if marks.mark(reference) {
        work.push(reference);
    }
}

fn trace_object(
    heap: &Heap,
    marks: &mut Marks,
    work: &mut Vec<TraceRef>,
    weak: &mut Weak,
    reference: TraceRef,
) {
    match reference.kind {
        Kind::String => {}
        Kind::NativeClosure => {
            if let Some(closure) = heap.native_closures.slot_value(reference.index) {
                for value in &closure.values {
                    trace_value(marks, work, *value);
                }
            }
        }
        // A host value holds no Lua values (ADR 0044): the metatable and
        // the user values are a userdata's only edges.
        Kind::Userdata => {
            if let Some(userdata) = heap.userdata.slot_value(reference.index) {
                for value in &userdata.user_values {
                    trace_value(marks, work, *value);
                }
                if let Some(metatable) = userdata.metatable {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::Table,
                            index: metatable.index,
                        },
                    );
                }
            }
        }
        Kind::Table => trace_table(heap, marks, work, weak, reference.index),
        Kind::Proto => {
            if let Some(proto) = heap.protos.slot_value(reference.index) {
                if let Some(source) = proto.source {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::String,
                            index: source.index,
                        },
                    );
                }
                for string in &proto.const_strings {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::String,
                            index: string.index,
                        },
                    );
                }
                for child in proto.children.clone() {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::Proto,
                            index: child.index,
                        },
                    );
                }
            }
        }
        Kind::Upvalue => {
            if let Some(upvalue) = heap.upvalues.slot_value(reference.index) {
                match upvalue.state {
                    UpvalueState::Open { thread, .. } => {
                        push_work(
                            marks,
                            work,
                            TraceRef {
                                kind: Kind::Thread,
                                index: thread.index,
                            },
                        );
                    }
                    UpvalueState::Closed(value) => trace_value(marks, work, value),
                }
            }
        }
        Kind::Closure => {
            if let Some(closure) = heap.closures.slot_value(reference.index) {
                push_work(
                    marks,
                    work,
                    TraceRef {
                        kind: Kind::Proto,
                        index: closure.proto.index,
                    },
                );
                for upvalue in closure.upvalues.clone() {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::Upvalue,
                            index: upvalue.index,
                        },
                    );
                }
            }
        }
        Kind::Thread => {
            if let Some(thread) = heap.threads.slot_value(reference.index) {
                let stack = thread.stack.to_vec();

                let resumed_by = thread.resumed_by;
                let host_results = thread.host_results.clone();
                let open = thread.open_upvalues.clone();
                if let Some((_, error)) = thread.unwind.as_deref().and_then(|unwind| unwind.error) {
                    trace_value(marks, work, error);
                }
                if let Some((_, error)) = thread.error {
                    trace_value(marks, work, error);
                }
                for value in stack {
                    trace_value(marks, work, value);
                }
                for value in host_results {
                    trace_value(marks, work, value);
                }
                for frame in &thread.frames {
                    trace_frame(marks, work, frame);
                }
                if let Some(parent) = resumed_by {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::Thread,
                            index: parent.index,
                        },
                    );
                }
                for (_, upvalue) in open {
                    push_work(
                        marks,
                        work,
                        TraceRef {
                            kind: Kind::Upvalue,
                            index: upvalue.index,
                        },
                    );
                }
            }
        }
    }
}

fn trace_frame(marks: &mut Marks, work: &mut Vec<TraceRef>, frame: &Frame) {
    let Some(cold) = &frame.cold else {
        push_work(
            marks,
            work,
            TraceRef {
                kind: Kind::Closure,
                index: frame.closure.index,
            },
        );
        return;
    };
    if let Some(crate::heap::Boundary::Native {
        error: Some((_, error)),
        ..
    }) = cold.boundary.as_ref()
    {
        trace_value(marks, work, *error);
    }
    if let Some(wait) = &cold.wait_request {
        for value in &wait.payload {
            trace_value(marks, work, *value);
        }
    }
    if let Some(crate::heap::Boundary::Protect {
        handler: Some(handler),
        ..
    }) = cold.boundary.as_ref()
    {
        trace_value(marks, work, *handler);
    }
    // An unwind waiting on the frame's closes holds its error here.
    if let Some(crate::heap::Closing {
        next: crate::heap::CloseNext::Unwind(unwind),
        ..
    }) = cold.meta.as_ref().and_then(|meta| meta.close.as_deref())
        && let Some((_, error)) = unwind.error
    {
        trace_value(marks, work, error);
    }
    push_work(
        marks,
        work,
        TraceRef {
            kind: Kind::Closure,
            index: frame.closure.index,
        },
    );
    if let Some(Pending::Resuming { child, .. }) = cold.pending.as_ref() {
        push_work(
            marks,
            work,
            TraceRef {
                kind: Kind::Thread,
                index: child.index,
            },
        );
    }
    {
        for target in &cold.targets {
            if let crate::heap::AssignTarget::Field { table, key } = target {
                trace_value(marks, work, *table);
                trace_value(marks, work, *key);
            }
        }
    }
}

fn trace_value(marks: &mut Marks, work: &mut Vec<TraceRef>, value: Value) {
    let reference = match value {
        Value::String(handle) => TraceRef {
            kind: Kind::String,
            index: handle.index,
        },
        Value::Table(handle) => TraceRef {
            kind: Kind::Table,
            index: handle.index,
        },
        Value::Closure(handle) => TraceRef {
            kind: Kind::Closure,
            index: handle.index,
        },
        Value::Thread(handle) => TraceRef {
            kind: Kind::Thread,
            index: handle.index,
        },
        Value::NativeClosure(handle) => TraceRef {
            kind: Kind::NativeClosure,
            index: handle.index,
        },
        Value::Userdata(handle) => TraceRef {
            kind: Kind::Userdata,
            index: handle.index,
        },
        Value::Nil
        | Value::Bool(_)
        | Value::Integer(_)
        | Value::Float(_)
        | Value::Native(_)
        | Value::LightUserdata(..) => {
            return;
        }
    };
    // Handles stored in live objects were written while current, so the slot
    // index is the occupant to mark. A stale handle is not traced from a root.
    push_work(marks, work, reference);
}

fn sweep_unmarked<T: crate::heap::LogicalSize>(arena: &mut crate::heap::Arena<T>, marks: &[bool]) {
    let count = arena.slot_count();
    for index in 0..count {
        let marked = marks.get(index).copied().unwrap_or(false);
        if !marked && arena.slot_is_occupied(index as u32) {
            arena.free(index as u32);
        }
    }
}
