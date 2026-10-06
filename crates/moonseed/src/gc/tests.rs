use crate::gc::collect;
use crate::heap::{Heap, HostRoot, UpvalueState};
use crate::id::{Handle, Kind};
use crate::value::Value;

#[test]
fn unreachable_cycle_is_collected_and_a_root_is_not() {
    let mut heap = Heap::new();
    let a = heap.alloc_table().unwrap();
    let b = heap.alloc_table().unwrap();
    let a_id = heap.tables.get(a).unwrap().id;
    let b_id = heap.tables.get(b).unwrap().id;
    let key_a = heap.normalize_value(Value::Table(a)).unwrap();
    let key_b = heap.normalize_value(Value::Table(b)).unwrap();
    assert!(
        heap.table_insert(a, key_b, Value::Table(b), Value::Integer(1))
            .is_ok()
    );
    assert!(
        heap.table_insert(b, key_a, Value::Table(a), Value::Integer(1))
            .is_ok()
    );
    collect(&mut heap, &[]);
    assert_eq!(heap.tables.live(), 0);
    assert!(heap.find_by_id(a_id).is_none());
    assert!(heap.find_by_id(b_id).is_none());
}

#[test]
fn host_root_keeps_object_and_release_allows_collection() {
    let mut heap = Heap::new();
    let string = heap.alloc_string(b"kept".to_vec()).unwrap();
    let id = heap.strings.get(string).unwrap().id;
    heap.host_roots.push(HostRoot {
        kind: Kind::String,
        index: string.index,
        generation: string.generation,
        id,
    });
    collect(&mut heap, &[]);
    assert!(heap.strings.get(string).is_some());
    heap.host_roots.clear();
    collect(&mut heap, &[]);
    assert!(heap.strings.get(string).is_none());
    assert!(heap.find_by_id(id).is_none());
}

#[test]
fn stale_handle_is_rejected_after_reuse() {
    let mut heap = Heap::new();
    let first = heap.alloc_string(b"a".to_vec()).unwrap();
    collect(&mut heap, &[]);
    let second = heap.alloc_string(b"b".to_vec()).unwrap();
    assert!(heap.strings.get(first).is_none());
    assert_eq!(heap.strings.get(second).unwrap().bytes.as_slice(), b"b");
    assert_ne!(
        (first.index, first.generation),
        (second.index, second.generation)
    );
}

#[test]
fn generation_exhaustion_retires_the_slot() {
    let mut heap = Heap::new();
    let first = heap.alloc_string(b"a".to_vec()).unwrap();
    let index = first.index;
    heap.strings.force_generation_max(index);
    heap.strings.free(index);
    let second = heap.alloc_string(b"b".to_vec()).unwrap();
    assert_ne!(second.index, index);
    assert!(heap.strings.get(Handle::new(index, u32::MAX)).is_none());
}

#[test]
fn equal_string_bytes_are_one_key_and_tables_are_not() {
    let mut heap = Heap::new();
    let left = heap.alloc_string(b"hi".to_vec()).unwrap();
    let right = heap.alloc_string(b"hi".to_vec()).unwrap();
    let table = heap.alloc_table().unwrap();
    let key_left = heap.normalize_value(Value::String(left)).unwrap();
    let key_right = heap.normalize_value(Value::String(right)).unwrap();
    assert!(
        heap.table_insert(table, key_left, Value::String(left), Value::Integer(1))
            .is_ok()
    );
    assert!(
        heap.table_insert(table, key_right, Value::String(right), Value::Integer(2))
            .is_ok()
    );
    assert_eq!(heap.tables.get(table).unwrap().live_len(), 1);

    let a = heap.alloc_table().unwrap();
    let b = heap.alloc_table().unwrap();
    let key_a = heap.normalize_value(Value::Table(a)).unwrap();
    let key_b = heap.normalize_value(Value::Table(b)).unwrap();
    assert!(
        heap.table_insert(a, key_a, Value::Table(a), Value::Integer(1))
            .is_ok()
    );
    assert!(
        heap.table_insert(a, key_b, Value::Table(b), Value::Integer(2))
            .is_ok()
    );
    assert_eq!(heap.tables.get(a).unwrap().live_len(), 2);
}

#[test]
fn a_prototype_owns_its_constant_strings() {
    let mut heap = Heap::new();
    let foo = heap.alloc_string(b"foo".to_vec()).unwrap();
    let name = heap.alloc_string(b"=chunk".to_vec()).unwrap();
    let id = heap.alloc_id().unwrap();
    let proto = heap
        .protos
        .alloc(crate::heap::Proto {
            id,
            ops: vec![crate::opcode::Op::Halt],
            field_hints: Box::default(),
            byte_consts: vec![b"foo".to_vec()],
            byte_hashes: vec![crate::hashutil::string_hash(b"foo")],
            const_strings: vec![foo],
            captures: Vec::new(),
            children: Vec::new(),
            max_reg: 1,
            params: 0,
            vararg: false,
            debug: None,
            source: Some(name),
        })
        .unwrap();
    heap.host_roots.push(HostRoot {
        kind: Kind::Proto,
        index: proto.index,
        generation: proto.generation,
        id,
    });
    collect(&mut heap, &[]);
    assert_eq!(heap.strings.live(), 2);
    heap.host_roots.clear();
    collect(&mut heap, &[]);
    assert_eq!((heap.protos.live(), heap.strings.live()), (0, 0));
}

#[test]
fn rooted_closure_keeps_an_open_upvalue_thread() {
    let mut heap = Heap::new();
    let proto = {
        let id = heap.alloc_id().unwrap();
        heap.protos
            .alloc(crate::heap::Proto {
                id,
                ops: vec![crate::opcode::Op::Halt],
                field_hints: Box::default(),
                byte_consts: Vec::new(),
                byte_hashes: Vec::new(),
                const_strings: Vec::new(),
                captures: Vec::new(),
                children: Vec::new(),
                max_reg: 1,
                params: 0,
                vararg: false,
                debug: None,
                source: None,
            })
            .unwrap()
    };
    let thread_id = heap.alloc_id().unwrap();
    let closure_id = heap.alloc_id().unwrap();
    let up_id = heap.alloc_id().unwrap();
    let thread = heap
        .threads
        .alloc(crate::heap::ThreadObj {
            id: thread_id,
            status: crate::heap::Status::LuaSuspended,
            stack: vec![Value::Integer(5)].into(),
            top: 1,
            charged_slots: 1,
            charged_held: 0,
            frames: Default::default(),
            open_upvalues: Vec::new(),
            open_above: 0,
            resumed_by: None,
            host_results: Vec::new(),
            unwind: None,
            error: None,
            coroutine: true,
            closing: false,
            tbc: Vec::new(),
        })
        .unwrap();
    let upvalue = heap
        .upvalues
        .alloc(crate::heap::UpvalueObj {
            id: up_id,
            state: UpvalueState::Open { thread, slot: 0 },
        })
        .unwrap();
    let closure = heap
        .closures
        .alloc(crate::heap::ClosureObj {
            id: closure_id,
            proto,
            upvalues: vec![upvalue],
        })
        .unwrap();
    heap.host_roots.push(HostRoot {
        kind: Kind::Closure,
        index: closure.index,
        generation: closure.generation,
        id: closure_id,
    });
    collect(&mut heap, &[]);
    assert!(
        heap.threads.get(thread).is_some(),
        "open upvalue dropped its thread"
    );
    assert!(heap.upvalues.get(upvalue).is_some());
    assert!(heap.closures.get(closure).is_some());
}

#[test]
fn abandoned_self_referential_thread_is_collected() {
    let mut heap = Heap::new();
    let thread_id = heap.alloc_id().unwrap();
    let thread = heap
        .threads
        .alloc(crate::heap::ThreadObj {
            id: thread_id,
            status: crate::heap::Status::LuaSuspended,
            stack: Default::default(),
            top: 0,
            charged_slots: 0,
            charged_held: 0,
            frames: Default::default(),
            open_upvalues: Vec::new(),
            open_above: 0,
            resumed_by: None,
            host_results: Vec::new(),
            unwind: None,
            error: None,
            coroutine: true,
            closing: false,
            tbc: Vec::new(),
        })
        .unwrap();
    let table = heap.alloc_table().unwrap();
    let key = heap.normalize_value(Value::Integer(1)).unwrap();
    assert!(
        heap.table_insert(table, key, Value::Integer(1), Value::Thread(thread))
            .is_ok()
    );
    heap.threads
        .get_mut(thread)
        .unwrap()
        .stack
        .extend_from_slice(&[Value::Table(table)]);
    collect(&mut heap, &[]);
    assert!(heap.threads.get(thread).is_none());
    assert!(heap.tables.get(table).is_none());
}

#[test]
fn dead_object_key_is_not_a_root() {
    let mut heap = Heap::new();
    let table = heap.alloc_table().unwrap();
    let key_obj = heap.alloc_table().unwrap();
    let key_id = heap.tables.get(key_obj).unwrap().id;
    let table_id = heap.tables.get(table).unwrap().id;
    let key = heap.normalize_value(Value::Table(key_obj)).unwrap();
    assert!(
        heap.table_insert(table, key.clone(), Value::Table(key_obj), Value::Integer(1))
            .is_ok()
    );
    assert!(
        heap.table_insert(table, key, Value::Table(key_obj), Value::Nil)
            .is_ok()
    );
    heap.globals = Some(table);
    collect(&mut heap, &[]);
    assert!(heap.find_by_id(table_id).is_some());
    assert!(heap.find_by_id(key_id).is_none());
    assert_eq!(heap.tables.get(table).unwrap().dead_len(), 1);
}

#[test]
fn dead_string_anchor_keeps_bytes_and_not_the_string_object() {
    let mut heap = Heap::new();
    let table = heap.alloc_table().unwrap();
    let text = heap.alloc_string(b"k".to_vec()).unwrap();
    let text_id = heap.strings.get(text).unwrap().id;
    let key = heap.normalize_value(Value::String(text)).unwrap();
    assert!(
        heap.table_insert(table, key.clone(), Value::String(text), Value::Integer(1))
            .is_ok()
    );
    let two = heap.normalize_value(Value::Integer(2)).unwrap();
    assert!(
        heap.table_insert(table, two, Value::Integer(2), Value::Integer(20))
            .is_ok()
    );
    assert!(
        heap.table_insert(table, key, Value::String(text), Value::Nil)
            .is_ok()
    );
    heap.globals = Some(table);
    collect(&mut heap, &[]);
    assert!(heap.find_by_id(text_id).is_none());
    let again = heap.alloc_string(b"k".to_vec()).unwrap();
    let key = heap.normalize_value(Value::String(again)).unwrap();
    let (next_key, next_value) = heap
        .tables
        .get(table)
        .unwrap()
        .table
        .next(Some(&key))
        .unwrap()
        .unwrap();
    assert_eq!(next_key, Value::Integer(2));
    assert_eq!(next_value, Value::Integer(20));
}

/// Root a table through the extra roots `collect` takes.
fn root(handle: Handle<crate::heap::TableObj>) -> crate::heap::TraceRef {
    crate::heap::TraceRef {
        kind: Kind::Table,
        index: handle.index,
    }
}

fn set(heap: &mut Heap, table: Handle<crate::heap::TableObj>, key: Value, value: Value) {
    let normalized = heap.normalize_value(key).unwrap();
    heap.table_insert(table, normalized, key, value).unwrap();
}

fn get(heap: &Heap, table: Handle<crate::heap::TableObj>, key: Value) -> Value {
    heap.table_get(table, &heap.normalize_value(key).unwrap())
        .unwrap()
}

/// A table whose metatable has `__mode = mode`.
fn weak(heap: &mut Heap, mode: &[u8]) -> Handle<crate::heap::TableObj> {
    let table = heap.alloc_table().unwrap();
    let metatable = heap.alloc_table().unwrap();
    let name = Value::String(heap.alloc_string(b"__mode".to_vec()).unwrap());
    let mode = Value::String(heap.alloc_string(mode.to_vec()).unwrap());
    set(heap, metatable, name, mode);
    heap.set_metatable(Value::Table(table), Some(metatable));
    table
}

/// A weak-value table loses an object nothing else holds, keeps a
/// string and a number, and keeps its keys strong.
#[test]
fn weak_values_go_and_strings_stay() {
    let mut heap = Heap::new();
    let t = weak(&mut heap, b"v");
    let object = heap.alloc_table().unwrap();
    let string = heap.alloc_string(b"string".to_vec()).unwrap();
    let key = heap.alloc_table().unwrap();
    set(&mut heap, t, Value::Integer(1), Value::Table(object));
    set(&mut heap, t, Value::Integer(2), Value::String(string));
    set(&mut heap, t, Value::Integer(3), Value::Integer(3));
    set(&mut heap, t, Value::Table(key), Value::Integer(4));
    collect(&mut heap, &[root(t)]);
    assert_eq!(get(&heap, t, Value::Integer(1)), Value::Nil);
    assert!(heap.tables.get(object).is_none());
    assert_eq!(get(&heap, t, Value::Integer(2)), Value::String(string));
    assert!(heap.strings.get(string).is_some());
    assert_eq!(get(&heap, t, Value::Integer(3)), Value::Integer(3));
    assert!(heap.tables.get(key).is_some());
}

/// Ephemerons settle across tables in whatever order they are found:
/// a chain lives while its first key does, and goes with it, value to
/// key cycles included.
#[test]
fn ephemeron_chains_settle_across_tables() {
    let mut heap = Heap::new();
    let tables: Vec<_> = (0..3).map(|_| weak(&mut heap, b"k")).collect();
    let keys: Vec<_> = (0..4).map(|_| heap.alloc_table().unwrap()).collect();
    // Built backwards: the last table holds the first link.
    for (link, table) in tables.iter().rev().enumerate() {
        set(
            &mut heap,
            *table,
            Value::Table(keys[link]),
            Value::Table(keys[link + 1]),
        );
    }
    // The chain's end points back at its start.
    set(&mut heap, keys[3], Value::Integer(1), Value::Table(keys[0]));
    let mut roots: Vec<_> = tables.iter().copied().map(root).collect();
    collect(&mut heap, &[roots.clone(), vec![root(keys[0])]].concat());
    assert!(keys.iter().all(|key| heap.tables.get(*key).is_some()));
    roots.truncate(3);
    collect(&mut heap, &roots);
    assert!(keys.iter().all(|key| heap.tables.get(*key).is_none()));
    for table in &tables {
        assert_eq!(heap.tables.get(*table).unwrap().table.live_len(), 0);
    }
}

/// Registered objects found dead wait for their finalizers newest
/// registration first and survive the collection that found them;
/// once their finalizers have run, the next collection frees them.
#[test]
fn dead_registered_objects_wait_newest_first() {
    let mut heap = Heap::new();
    let metatable = heap.alloc_table().unwrap();
    let gc = Value::String(heap.alloc_string(b"__gc".to_vec()).unwrap());
    set(&mut heap, metatable, gc, Value::Bool(true));
    let objects: Vec<_> = (0..3).map(|_| heap.alloc_table().unwrap()).collect();
    for object in &objects {
        heap.set_metatable(Value::Table(*object), Some(metatable));
    }
    // A metatable given again does not register twice.
    heap.set_metatable(Value::Table(objects[0]), Some(metatable));
    assert_eq!(heap.finalizers.registered.len(), 3);
    collect(&mut heap, &[root(metatable)]);
    let pending: Vec<_> = heap.finalizers.pending.iter().copied().collect();
    assert_eq!(
        pending,
        objects
            .iter()
            .rev()
            .map(|object| crate::heap::FinRef::Table(*object))
            .collect::<Vec<_>>()
    );
    assert!(
        objects
            .iter()
            .all(|object| heap.tables.get(*object).is_some())
    );
    // As if the finalizers ran: they leave the queue unregistered.
    while let Some(fin) = heap.finalizers.pending.pop_front() {
        heap.unmark_finalize(fin);
    }
    collect(&mut heap, &[root(metatable)]);
    assert!(
        objects
            .iter()
            .all(|object| heap.tables.get(*object).is_none())
    );
}

// The incremental collector against the Phase 3.27 reference
// (ADR 0050): random heaps, random step boundaries, and legal mutations
// between steps.

use crate::gc::{Phase, check_invariant, request_full, work};
use crate::heap::{FinRef, TableObj, mark};
use crate::table::Slot;

/// xorshift64*: the same seed builds the same heap.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// A value a graph operation names: a number, or the `i`th object made.
#[derive(Clone, Copy, Debug)]
enum V {
    Int(i64),
    Obj(usize),
}

/// What a test heap is made of, and every change made to it, so a second
/// heap can be made identical (same ids, same handles).
#[derive(Clone, Debug)]
enum Op {
    Table,
    Text(u8),
    Userdata(usize),
    /// A closure over `n` closed upvalues holding the values.
    Closure(Vec<V>),
    /// A suspended thread whose stack holds the values.
    Thread(Vec<V>),
    Set(usize, V, V),
    Meta(usize, Option<usize>),
    /// Set `__mode` of a metatable (made by `MetaTable`), or remove it.
    Mode(usize, Option<&'static [u8]>),
    /// A metatable with `__gc` (when true) and a mode.
    MetaTable(bool, Option<&'static [u8]>),
    Root(usize),
    Unroot(usize),
    Global(usize),
    TypeMeta(usize, usize),
    UserValue(usize, usize, V),
    /// Close-write the `n`th upvalue of a closure.
    Upvalue(usize, usize, V),
    StackSlot(usize, usize, V),
}

struct Graph {
    objects: Vec<Value>,
    proto: Option<Handle<crate::heap::Proto>>,
}

fn value(graph: &Graph, v: V) -> Value {
    match v {
        V::Int(n) => Value::Integer(n),
        V::Obj(i) => graph.objects[i],
    }
}

fn apply(heap: &mut Heap, graph: &mut Graph, op: &Op) {
    let string =
        |heap: &mut Heap, text: &[u8]| Value::String(heap.alloc_string(text.to_vec()).unwrap());
    match op {
        Op::Table => {
            let table = heap.alloc_table().unwrap();
            graph.objects.push(Value::Table(table));
        }
        Op::Text(n) => {
            let text = string(heap, format!("s{n}").as_bytes());
            graph.objects.push(text);
        }
        Op::Userdata(values) => {
            let userdata = heap
                .alloc_userdata(crate::heap::MAX_OBJECTS, *values, 0, || {
                    crate::userdata::Payload::Bytes(Box::new([]))
                })
                .unwrap();
            graph.objects.push(Value::Userdata(userdata));
        }
        Op::Closure(values) => {
            // The shared prototype, made again once collected: a handle
            // to a freed one would be stale.
            if graph
                .proto
                .is_some_and(|proto| heap.protos.get(proto).is_none())
            {
                graph.proto = None;
            }
            let proto = *graph.proto.get_or_insert_with(|| {
                let id = heap.alloc_id().unwrap();
                heap.protos
                    .alloc(crate::heap::Proto {
                        id,
                        ops: vec![crate::opcode::Op::Halt],
                        field_hints: Box::default(),
                        byte_consts: Vec::new(),
                        byte_hashes: Vec::new(),
                        const_strings: Vec::new(),
                        captures: Vec::new(),
                        children: Vec::new(),
                        max_reg: 1,
                        params: 0,
                        vararg: false,
                        debug: None,
                        source: None,
                    })
                    .unwrap()
            });
            let mut upvalues = Vec::new();
            for v in values {
                let id = heap.alloc_id().unwrap();
                let state = UpvalueState::Closed(value(graph, *v));
                upvalues.push(
                    heap.upvalues
                        .alloc(crate::heap::UpvalueObj { id, state })
                        .unwrap(),
                );
            }
            let id = heap.alloc_id().unwrap();
            let closure = heap
                .closures
                .alloc(crate::heap::ClosureObj {
                    id,
                    proto,
                    upvalues,
                })
                .unwrap();
            graph.objects.push(Value::Closure(closure));
        }
        Op::Thread(values) => {
            let stack: Vec<Value> = values.iter().map(|v| value(graph, *v)).collect();
            let id = heap.alloc_id().unwrap();
            let thread = heap
                .threads
                .alloc(crate::heap::ThreadObj {
                    id,
                    status: crate::heap::Status::LuaSuspended,
                    top: stack.len() as u32,
                    charged_slots: stack.len() as u32,
                    charged_held: 0,
                    stack: stack.into(),
                    frames: Default::default(),
                    open_upvalues: Vec::new(),
                    open_above: 0,
                    resumed_by: None,
                    host_results: Vec::new(),
                    unwind: None,
                    error: None,
                    coroutine: true,
                    closing: false,
                    tbc: Vec::new(),
                })
                .unwrap();
            graph.objects.push(Value::Thread(thread));
        }
        Op::Set(t, k, v) => {
            if let Value::Table(table) = graph.objects[*t] {
                let key = value(graph, *k);
                if let Ok(normal) = heap.normalize_value(key) {
                    let _ = heap.table_insert(table, normal, key, value(graph, *v));
                }
            }
        }
        Op::Meta(t, mt) => {
            let metatable = mt.and_then(|mt| match graph.objects[mt] {
                Value::Table(table) => Some(table),
                _ => None,
            });
            heap.set_metatable(graph.objects[*t], metatable);
        }
        Op::Mode(mt, mode) => {
            if let Value::Table(table) = graph.objects[*mt] {
                let name = string(heap, b"__mode");
                let mode = match mode {
                    Some(mode) => string(heap, mode),
                    None => Value::Nil,
                };
                let key = heap.normalize_value(name).unwrap();
                heap.table_insert(table, key, name, mode).unwrap();
            }
        }
        Op::MetaTable(gc, mode) => {
            let table = heap.alloc_table().unwrap();
            graph.objects.push(Value::Table(table));
            if *gc {
                let name = string(heap, b"__gc");
                let key = heap.normalize_value(name).unwrap();
                heap.table_insert(table, key, name, Value::Bool(true))
                    .unwrap();
            }
            if let Some(mode) = mode {
                let at = graph.objects.len() - 1;
                apply(heap, graph, &Op::Mode(at, Some(mode)));
            }
        }
        Op::Root(i) => {
            let object = graph.objects[*i];
            let Some(id) = heap.object_id_of_value(object) else {
                return;
            };
            let (kind, index, generation) = heap.find_by_id(id).unwrap();
            heap.host_roots.push(HostRoot {
                kind,
                index,
                generation,
                id,
            });
        }
        Op::Unroot(i) => {
            let Some(id) = heap.object_id_of_value(graph.objects[*i]) else {
                return;
            };
            if let Some(at) = heap.host_roots.iter().position(|root| root.id == id) {
                heap.host_roots.remove(at);
            }
        }
        Op::Global(t) => {
            if let Value::Table(table) = graph.objects[*t] {
                heap.globals = Some(table);
            }
        }
        Op::TypeMeta(basic, t) => {
            if let Value::Table(table) = graph.objects[*t] {
                heap.type_metatables[*basic] = Some(table);
            }
        }
        Op::UserValue(u, n, v) => {
            let v = value(graph, *v);
            if let Value::Userdata(userdata) = graph.objects[*u]
                && let Some(object) = heap.userdata.get_mut(userdata)
                && let Some(slot) = object.user_values.get_mut(*n)
            {
                *slot = v;
            }
        }
        Op::Upvalue(c, n, v) => {
            let v = value(graph, *v);
            if let Value::Closure(closure) = graph.objects[*c]
                && let Some(upvalue) = heap
                    .closures
                    .get(closure)
                    .and_then(|closure| closure.upvalues.get(*n).copied())
                && let Some(object) = heap.upvalues.get_mut(upvalue)
            {
                object.state = UpvalueState::Closed(v);
            }
        }
        Op::StackSlot(th, n, v) => {
            let v = value(graph, *v);
            if let Value::Thread(thread) = graph.objects[*th]
                && let Some(object) = heap.threads.get_mut(thread)
                && let Some(slot) = object.stack.get_mut(*n)
            {
                *slot = v;
            }
        }
    }
}

fn pick(rng: &mut Rng, graph: &Graph) -> V {
    if rng.chance(25) || graph.objects.is_empty() {
        V::Int(rng.below(8) as i64)
    } else {
        V::Obj(rng.below(graph.objects.len()))
    }
}

/// One random change of the kinds Lua makes: a store, a metatable, a
/// mode, a root, an upvalue, a stack slot, a user value, a new object.
fn random_op(rng: &mut Rng, graph: &Graph, metas: &[usize]) -> Op {
    let n = graph.objects.len();
    let any = |rng: &mut Rng| rng.below(n);
    match rng.below(12) {
        0..=3 => Op::Set(any(rng), pick(rng, graph), pick(rng, graph)),
        4 => Op::Meta(
            any(rng),
            (!metas.is_empty() && rng.chance(80)).then(|| metas[rng.below(metas.len())]),
        ),
        5 if !metas.is_empty() => {
            let modes: [Option<&'static [u8]>; 4] = [None, Some(b"k"), Some(b"v"), Some(b"kv")];
            Op::Mode(metas[rng.below(metas.len())], modes[rng.below(4)])
        }
        6 => Op::Root(any(rng)),
        7 => Op::Unroot(any(rng)),
        8 => Op::UserValue(any(rng), rng.below(2), pick(rng, graph)),
        9 => Op::Upvalue(any(rng), rng.below(2), pick(rng, graph)),
        10 => Op::StackSlot(any(rng), rng.below(3), pick(rng, graph)),
        _ => match rng.below(3) {
            0 => Op::Table,
            1 => Op::Text(rng.below(4) as u8),
            _ => Op::Closure(vec![pick(rng, graph)]),
        },
    }
}

/// A random heap: shared metatables (weak modes, `__gc`), tables,
/// strings, userdata, closures with closed upvalues, suspended threads,
/// cycles, roots, globals and a type metatable.
fn random_graph(rng: &mut Rng) -> (Vec<Op>, Vec<usize>) {
    let mut ops = Vec::new();
    let mut metas = Vec::new();
    let mut count = 0usize;
    let modes: [Option<&'static [u8]>; 4] = [None, Some(b"k"), Some(b"v"), Some(b"kv")];
    for _ in 0..rng.below(5) + 1 {
        ops.push(Op::MetaTable(rng.chance(50), modes[rng.below(4)]));
        metas.push(count);
        count += 1;
    }
    let objects = 10 + rng.below(50);
    let graph_len = |count: usize| Graph {
        objects: vec![Value::Nil; count],
        proto: None,
    };
    for _ in 0..objects {
        let graph = graph_len(count);
        ops.push(match rng.below(10) {
            0..=5 => Op::Table,
            6 => Op::Text(rng.below(6) as u8),
            7 => Op::Userdata(rng.below(3)),
            8 => Op::Closure((0..rng.below(3) + 1).map(|_| pick(rng, &graph)).collect()),
            _ => Op::Thread((0..rng.below(4) + 1).map(|_| pick(rng, &graph)).collect()),
        });
        count += 1;
    }
    for _ in 0..objects * 3 {
        let graph = graph_len(count);
        ops.push(match rng.below(8) {
            0..=4 => Op::Set(rng.below(count), pick(rng, &graph), pick(rng, &graph)),
            5 => Op::Meta(rng.below(count), Some(metas[rng.below(metas.len())])),
            _ => Op::UserValue(rng.below(count), rng.below(3), pick(rng, &graph)),
        });
    }
    for _ in 0..rng.below(4) + 1 {
        ops.push(Op::Root(rng.below(count)));
    }
    ops.push(Op::Global(rng.below(count)));
    if rng.chance(30) {
        ops.push(Op::TypeMeta(
            rng.below(crate::heap::BASIC_TYPES),
            rng.below(count),
        ));
    }
    (ops, metas)
}

fn make(ops: &[Op]) -> (Heap, Graph) {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    let mut graph = Graph {
        objects: Vec::new(),
        proto: None,
    };
    for op in ops {
        apply(&mut heap, &mut graph, op);
    }
    (heap, graph)
}

/// What a collection decided, by object id: what lives, what each live
/// table holds, what waits for its finalizer (in order), what is
/// registered (in order).
#[derive(Debug, PartialEq)]
struct Outcome {
    live: std::collections::BTreeSet<u64>,
    tables: std::collections::BTreeMap<u64, Vec<(String, String)>>,
    pending: Vec<u64>,
    registered: Vec<u64>,
}

fn outcome(heap: &Heap) -> Outcome {
    let repr = |value: Value| match heap.object_id_of_value(value) {
        Some(id) => format!("#{}", id.raw()),
        None => format!("{value:?}"),
    };
    let mut live = std::collections::BTreeSet::new();
    macro_rules! ids {
        ($($arena:ident),*) => {
            $(for (_, _, object) in heap.$arena.iter() {
                live.insert(object.id.raw());
            })*
        };
    }
    ids!(
        strings,
        tables,
        protos,
        upvalues,
        closures,
        threads,
        native_closures,
        userdata
    );
    let mut tables = std::collections::BTreeMap::new();
    for (_, _, table) in heap.tables.iter() {
        let mut entries: Vec<(String, String)> = table
            .table
            .slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Live {
                    key_value, value, ..
                } => Some((repr(*key_value), repr(*value))),
                Slot::Dead { .. } => None,
            })
            .collect();
        entries.sort();
        tables.insert(table.id.raw(), entries);
    }
    let fin = |list: &mut dyn Iterator<Item = &FinRef>| -> Vec<u64> {
        list.map(|fin| heap.object_id_of_value(fin.value()).unwrap().raw())
            .collect()
    };
    Outcome {
        live,
        pending: fin(&mut heap.finalizers.pending.iter()),
        registered: fin(&mut heap.finalizers.registered.iter()),
        tables,
    }
}

/// Run a full incremental cycle in random slices. Between slices, while
/// nothing is decided yet, `mutate` may change the heap; the invariant
/// holds at every point Lua could run.
fn incremental(heap: &mut Heap, rng: &mut Rng, mut mutate: impl FnMut(&mut Heap, &mut Rng)) {
    request_full(heap);
    let mut slices = 0;
    while heap.gc.full.is_some() {
        let budget = 1 + rng.below(40) as u64;
        work(heap, &[], budget, crate::heap::MAX_OBJECTS);
        if matches!(heap.collector.phase, Phase::Propagate | Phase::Begin) {
            check_invariant(heap).unwrap();
            mutate(heap, rng);
        }
        slices += 1;
        assert!(slices < 1_000_000, "the cycle does not end");
    }
}

/// Gate 0: with nothing changed between slices, a completed incremental
/// cycle decides exactly what the reference collector decides, and its
/// estimate of what it kept is exact.
#[test]
fn completed_cycles_match_the_reference_collector() {
    for seed in 1..=3000u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let (ops, _) = random_graph(&mut rng);
        let (mut reference, _) = make(&ops);
        let (mut heap, _) = make(&ops);
        crate::gc_reference::collect(&mut reference, &[]);
        incremental(&mut heap, &mut rng, |_, _| {});
        assert_eq!(outcome(&heap), outcome(&reference), "seed {seed}");
        assert_eq!(heap.gc.live, crate::gc::logical_size(&heap), "seed {seed}");
        // And again, from what the first collection left.
        crate::gc_reference::collect(&mut reference, &[]);
        incremental(&mut heap, &mut rng, |_, _| {});
        assert_eq!(outcome(&heap), outcome(&reference), "seed {seed} again");
    }
}

/// Gate O: with changes between slices, the cycle frees nothing the
/// final graph reaches, queues only objects it does not reach, and
/// clears no weak entry the reference keeps; with nothing changing after,
/// both settle on the same heap (the queue's order aside).
#[test]
fn mutated_cycles_never_free_what_the_reference_keeps() {
    for seed in 1..=3000u64 {
        let mut rng = Rng(seed.wrapping_mul(0xd1b5_4a32_d192_ed03) | 1);
        let (ops, metas) = random_graph(&mut rng);
        let (mut heap, mut graph) = make(&ops);
        let mut log = Vec::new();
        incremental(&mut heap, &mut rng, |heap, rng| {
            for _ in 0..rng.below(4) {
                let op = random_op(rng, &graph, &metas);
                apply(heap, &mut graph, &op);
                log.push(op);
            }
        });
        let (mut reference, mut replay) = make(&ops);
        for op in &log {
            apply(&mut reference, &mut replay, op);
        }
        crate::gc_reference::collect(&mut reference, &[]);
        let (ours, theirs) = (outcome(&heap), outcome(&reference));
        assert!(
            theirs.live.is_subset(&ours.live),
            "seed {seed}: freed early"
        );
        let ours_pending: std::collections::BTreeSet<_> = ours.pending.iter().collect();
        let theirs_pending: std::collections::BTreeSet<_> = theirs.pending.iter().collect();
        assert!(
            ours_pending.is_subset(&theirs_pending),
            "seed {seed}: finalized a live object"
        );
        // What the final graph reaches strongly: the reference with no
        // finalizers to resurrect anything.
        let (mut strong, mut replay) = make(&ops);
        for op in &log {
            apply(&mut strong, &mut replay, op);
        }
        strong.finalizers.registered.clear();
        crate::gc_reference::collect(&mut strong, &[]);
        let strong = outcome(&strong).live;
        let reached = |repr: &String| {
            repr.strip_prefix('#')
                .is_none_or(|id| strong.contains(&id.parse::<u64>().unwrap()))
        };
        // An entry whose key and value the final graph reaches strongly is
        // never cleared. (A table kept for a path a change removed is
        // live for the cycle, so its dead weak values go with the strong
        // ones, where the reference, finalizing it, clears them later.)
        for (table, entries) in &theirs.tables {
            let kept = &ours.tables[table];
            for entry in entries {
                if reached(&entry.0) && reached(&entry.1) {
                    assert!(
                        kept.contains(entry),
                        "seed {seed}: table #{table} lost {entry:?}"
                    );
                }
            }
        }
        // What a cycle kept for a change it saw may keep more, one level
        // a cycle (a weak value kept holds its strong key), and finalize
        // later (an object kept is then held by one waiting for its
        // finalizer): with nothing changing, and the finalizers run (as
        // ones that do nothing), both settle where they agree.
        let settle = |collect: &mut dyn FnMut() -> Outcome| {
            let mut last = collect();
            for _ in 0..30 {
                let next = collect();
                if next == last {
                    break;
                }
                last = next;
            }
            last
        };
        let run_finalizers = |heap: &mut Heap| {
            while let Some(fin) = heap.finalizers.pending.pop_front() {
                heap.unmark_finalize(fin);
            }
        };
        let ours = settle(&mut || {
            incremental(&mut heap, &mut rng, |_, _| {});
            run_finalizers(&mut heap);
            outcome(&heap)
        });
        let theirs = settle(&mut || {
            crate::gc_reference::collect(&mut reference, &[]);
            run_finalizers(&mut reference);
            outcome(&reference)
        });
        assert_eq!(ours.live, theirs.live, "seed {seed}: settled");
        assert_eq!(ours.pending, theirs.pending, "seed {seed}: settled");
        assert_eq!(ours.registered, theirs.registered, "seed {seed}: settled");
        // Settled, a table holds what the reference's holds, but for the
        // entries cleared early from a table kept for a removed path.
        for (table, entries) in &theirs.tables {
            let kept = &ours.tables[table];
            assert!(
                kept.iter().all(|entry| entries.contains(entry)),
                "seed {seed}"
            );
            for entry in entries {
                if reached(&entry.0) && reached(&entry.1) {
                    assert!(
                        kept.contains(entry),
                        "seed {seed}: settled #{table} {entry:?}"
                    );
                }
            }
        }
    }
}

/// A heap with a root `parent` table traced (black) and `holder`, a
/// table not yet traced, holding `child`; `child` is reachable from
/// nothing else. Returns them with the cycle stopped there.
fn black_parent_and_white_child(
    child: impl FnOnce(&mut Heap) -> Value,
) -> (Heap, Handle<TableObj>, Handle<TableObj>, Value) {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    let parent = heap.alloc_table().unwrap();
    let holder = heap.alloc_table().unwrap();
    let child = child(&mut heap);
    set(&mut heap, holder, Value::Integer(1), child);
    for table in [holder, parent] {
        let id = heap.tables.get(table).unwrap().id;
        heap.host_roots.push(HostRoot {
            kind: Kind::Table,
            index: table.index,
            generation: table.generation,
            id,
        });
    }
    request_full(&mut heap);
    // The parent, rooted last, is traced first.
    while heap.mark_of(root(parent)) != Some(mark::BLACK) {
        work(&mut heap, &[], 1, crate::heap::MAX_OBJECTS);
    }
    assert!(heap.mark_of(root(holder)) == Some(mark::GRAY));
    (heap, parent, holder, child)
}

fn finish(heap: &mut Heap) {
    while heap.gc.full.is_some() {
        work(heap, &[], 7, crate::heap::MAX_OBJECTS);
    }
}

fn alive(heap: &Heap, value: Value) -> bool {
    heap.object_id_of_value(value)
        .is_some_and(|id| heap.find_by_id(id).is_some())
}

/// Gate B: a reference written into an object already traced keeps its
/// target, for every kind of write; with the barrier off (the negative
/// control) the same program frees an object still reachable.
#[test]
fn barriers_keep_what_a_traced_object_is_given() {
    type Write = fn(&mut Heap, Handle<TableObj>, Value);
    let writes: [(&str, Write); 2] = [
        ("table value", |heap, parent, child| {
            set(heap, parent, Value::Integer(1), child)
        }),
        ("table key", |heap, parent, child| {
            set(heap, parent, child, Value::Integer(1))
        }),
    ];
    for barrier in [true, false] {
        for (name, write) in writes {
            for make_child in [
                (|heap: &mut Heap| Value::Table(heap.alloc_table().unwrap()))
                    as fn(&mut Heap) -> Value,
                |heap: &mut Heap| {
                    Value::Userdata(
                        heap.alloc_userdata(crate::heap::MAX_OBJECTS, 0, 0, || {
                            crate::userdata::Payload::Bytes(Box::new([]))
                        })
                        .unwrap(),
                    )
                },
            ] {
                let (mut heap, parent, holder, child) = black_parent_and_white_child(make_child);
                heap.force_barriers(barrier);
                write(&mut heap, parent, child);
                set(&mut heap, holder, Value::Integer(1), Value::Nil);
                heap.force_barriers(true);
                finish(&mut heap);
                assert_eq!(alive(&heap, child), barrier, "{name}, barrier {barrier}");
            }
        }
    }
}

/// The other edges Lua writes: a userdata's user value, a closed
/// upvalue, a thread's stack slot. Each holder is traced first, then
/// given the child; the negative control frees it.
#[test]
fn barriers_cover_user_values_upvalues_and_stacks() {
    for kind in 0..3 {
        for barrier in [true, false] {
            let mut heap = Heap::new();
            heap.gc.auto = false;
            let mut graph = Graph {
                objects: Vec::new(),
                proto: None,
            };
            // 0: the object written to; 1: the child; 2: its holder.
            let first = match kind {
                0 => Op::Userdata(1),
                1 => Op::Closure(vec![V::Int(0)]),
                _ => Op::Thread(vec![V::Int(0)]),
            };
            for op in [
                first,
                Op::Table,
                Op::Table,
                Op::Set(2, V::Int(1), V::Obj(1)),
            ] {
                apply(&mut heap, &mut graph, &op);
            }
            // The holder is rooted first, so the written object is traced
            // before it.
            apply(&mut heap, &mut graph, &Op::Root(2));
            apply(&mut heap, &mut graph, &Op::Root(0));
            request_full(&mut heap);
            // What is written to: the userdata, the closure's upvalue, the
            // thread.
            let written = match graph.objects[0] {
                Value::Closure(closure) => crate::heap::TraceRef {
                    kind: Kind::Upvalue,
                    index: heap.closures.get(closure).unwrap().upvalues[0].index,
                },
                other => crate::gc::value_ref(other).unwrap(),
            };
            while heap.mark_of(written) != Some(mark::BLACK) || heap.collector.scan.is_some() {
                work(&mut heap, &[], 1, crate::heap::MAX_OBJECTS);
            }
            let holder = crate::gc::value_ref(graph.objects[2]).unwrap();
            assert_ne!(heap.mark_of(holder), Some(mark::BLACK));
            heap.force_barriers(barrier);
            let op = match kind {
                0 => Op::UserValue(0, 0, V::Obj(1)),
                1 => Op::Upvalue(0, 0, V::Obj(1)),
                _ => Op::StackSlot(0, 0, V::Obj(1)),
            };
            apply(&mut heap, &mut graph, &op);
            apply(&mut heap, &mut graph, &Op::Set(2, V::Int(1), V::Int(0)));
            heap.force_barriers(true);
            finish(&mut heap);
            assert_eq!(
                alive(&heap, graph.objects[1]),
                barrier,
                "kind {kind}, barrier {barrier}"
            );
        }
    }
}

/// Roots made during a cycle (a host root, the globals, a type
/// metatable) keep their object: the atomic phase grays every root
/// again. Without that (the negative control) the object goes.
#[test]
fn roots_made_while_marking_are_marked_again() {
    for kind in 0..3 {
        for remark in [true, false] {
            let (mut heap, _, holder, child) =
                black_parent_and_white_child(|heap| Value::Table(heap.alloc_table().unwrap()));
            heap.collector.skip_root_remark = !remark;
            let Value::Table(table) = child else {
                unreachable!()
            };
            match kind {
                0 => {
                    let id = heap.tables.get(table).unwrap().id;
                    heap.host_roots.push(HostRoot {
                        kind: Kind::Table,
                        index: table.index,
                        generation: table.generation,
                        id,
                    });
                }
                1 => heap.globals = Some(table),
                _ => heap.type_metatables[0] = Some(table),
            }
            set(&mut heap, holder, Value::Integer(1), Value::Nil);
            finish(&mut heap);
            assert_eq!(
                alive(&heap, child),
                remark,
                "root kind {kind}, remark {remark}"
            );
        }
    }
}

/// Gate C: a large table is traced a bounded number of entries at a
/// time, resuming where it stopped, and an object made during the cycle
/// survives it, in every phase.
#[test]
fn large_tables_are_traced_in_pieces_and_new_objects_survive() {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    let big = heap.alloc_table().unwrap();
    for i in 0..10_000 {
        // Some entries are objects; the rest numbers.
        let value = if i % 4 == 0 {
            Value::Table(heap.alloc_table().unwrap())
        } else {
            Value::Integer(i)
        };
        set(&mut heap, big, Value::Integer(i), value);
    }
    heap.globals = Some(big);
    request_full(&mut heap);
    let mut pieces = 0;
    let mut kept = Vec::new();
    while heap.gc.full.is_some() {
        let before = heap.collector.scan;
        let used = work(&mut heap, &[], 16, crate::heap::MAX_OBJECTS);
        assert!(used <= 16 + 300, "a slice of {used} units");
        if before.is_some_and(|scan| scan.object.index == big.index) {
            pieces += 1;
        }
        // A new object in whatever phase this is, kept by the big table.
        if heap.collector.phase != Phase::Pause && !heap.collector.in_atomic() {
            let new = heap.alloc_table().unwrap();
            set(
                &mut heap,
                big,
                Value::Integer(-1 - kept.len() as i64),
                Value::Table(new),
            );
            kept.push(Value::Table(new));
        }
    }
    assert!(pieces >= 9_000 / 16, "traced in {pieces} pieces");
    assert!(kept.iter().all(|object| alive(&heap, *object)));
}

/// Gate H: a full collection asked for at any point of a cycle (what an
/// emergency collection or `collectgarbage()` does) abandons or finishes
/// that cycle without stale marks or lists, and then decides as the
/// reference does from the same heap.
#[test]
fn full_collections_from_any_phase_match_the_reference() {
    let mut phases = std::collections::BTreeSet::new();
    for seed in 1..=1500u64 {
        let mut rng = Rng(seed.wrapping_mul(0xa076_1d64_78bd_642f) | 1);
        let (ops, _) = random_graph(&mut rng);
        let (mut reference, _) = make(&ops);
        let (mut heap, _) = make(&ops);
        // A cycle begun, stopped somewhere.
        heap.gc.owed = 1 + rng.below(400) as u64;
        crate::gc::begin(&mut heap);
        work(&mut heap, &[], u64::MAX, crate::heap::MAX_OBJECTS);
        phases.insert(format!("{:?}", heap.collector.phase));
        crate::gc::full(&mut heap, &[], crate::heap::MAX_OBJECTS);
        assert_eq!(heap.collector.phase, Phase::Pause, "seed {seed}");
        assert!(heap.collector.gray.is_empty() && heap.collector.weak.is_empty());
        // As many cycles as completed: the step may have finished one,
        // and a cycle past its atomic phase finishes before the full one.
        for _ in 0..heap.gc.collections {
            crate::gc_reference::collect(&mut reference, &[]);
        }
        assert_eq!(outcome(&heap), outcome(&reference), "seed {seed}");
    }
    assert!(phases.len() >= 3, "{phases:?}");
}

mod generational;
