//! Generational collection (ADR 0051): ages, the old-to-young barrier,
//! young collections against the reference collector, and majors.

use super::*;
use crate::gc::{check_gen_invariant, start_minor};
use crate::heap::{TraceRef, age};

const MAX: u32 = crate::heap::MAX_OBJECTS;

/// Make the heap generational: a full collection whose survivors are old.
fn enter_gen(heap: &mut Heap) {
    heap.gc.generational = true;
    crate::gc::full(heap, &[], MAX);
    assert!(heap.collector.generational);
    check_gen_invariant(heap).unwrap();
}

/// One young collection, in slices of random size. Returns its units.
fn minor(heap: &mut Heap, rng: &mut Rng) -> u64 {
    start_minor(heap);
    let mut units = 0;
    while heap.collector.holds() {
        units += work(heap, &[], 1 + rng.below(40) as u64, MAX);
    }
    assert!(heap.collector.generational && heap.collector.phase == Phase::Pause);
    units
}

fn every_object(heap: &Heap) -> Vec<TraceRef> {
    let mut all = Vec::new();
    for kind in crate::gc::KINDS {
        let indices: Vec<u32> = crate::heap::on_arena!(heap, kind, arena => {
            arena.iter().map(|(index, _, _)| index).collect()
        });
        all.extend(indices.into_iter().map(|index| TraceRef { kind, index }));
    }
    all
}

fn run_finalizers(heap: &mut Heap) {
    while let Some(fin) = heap.finalizers.pending.pop_front() {
        heap.unmark_finalize(fin);
    }
}

/// What happens to a generational test heap after it is built.
#[derive(Clone, Debug)]
enum Step {
    Op(Op),
    /// A young collection, sliced by this seed.
    Minor(u64),
    Finalize,
}

fn replay(ops: &[Op], steps: &[Step]) -> (Heap, Graph) {
    let (mut heap, mut graph) = make(ops);
    enter_gen(&mut heap);
    forget_dead(&heap, &mut graph);
    for step in steps {
        match step {
            Step::Op(op) => apply(&mut heap, &mut graph, op),
            Step::Minor(seed) => {
                minor(&mut heap, &mut Rng(*seed));
                forget_dead(&heap, &mut graph);
            }
            Step::Finalize => run_finalizers(&mut heap),
        }
    }
    (heap, graph)
}

/// Objects a collection freed are numbers to later changes, so they
/// never write a dangling handle.
fn forget_dead(heap: &Heap, graph: &mut Graph) {
    for object in &mut graph.objects {
        if !alive(heap, *object) {
            *object = Value::Integer(-1);
        }
    }
}

fn age_of(heap: &Heap, value: Value) -> u8 {
    heap.age_of(crate::gc::value_ref(value).unwrap())
}

/// Gate L: a young collection decides exactly what the reference
/// collector decides when every old object is a root. Old objects live
/// through it, traced with their modes when the young collection traces
/// them; everything else follows Lua's rules. Changes between young
/// collections are random writes of every kind, to old objects and
/// young, and new objects; after each, and after each collection, the
/// generational invariant holds.
#[test]
fn young_collections_match_the_reference_with_old_objects_as_roots() {
    // What the young collections did, over every seed: freed, touched
    // objects traced, finalizers queued.
    let (mut freed, mut touched, mut queued) = (0usize, 0usize, 0usize);
    for seed in 1..=3000u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let (ops, metas) = random_graph(&mut rng);
        let (mut heap, mut graph) = make(&ops);
        enter_gen(&mut heap);
        forget_dead(&heap, &mut graph);
        let mut steps = Vec::new();
        for round in 0..5 {
            for _ in 0..rng.below(30) {
                let op = random_op(&mut rng, &graph, &metas);
                apply(&mut heap, &mut graph, &op);
                steps.push(Step::Op(op));
            }
            check_gen_invariant(&heap).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
            let (mut twin, _) = replay(&ops, &steps);
            let old: Vec<TraceRef> = every_object(&twin)
                .into_iter()
                .filter(|object| age::is_old(twin.age_of(*object)))
                .collect();
            crate::gc_reference::collect(&mut twin, &old);
            let slices = rng.next() | 1;
            let before = every_object(&heap).len();
            touched += every_object(&heap)
                .iter()
                .filter(|object| heap.age_of(**object) == age::TOUCHED1)
                .count();
            minor(&mut heap, &mut Rng(slices));
            freed += before - every_object(&heap).len();
            queued += heap.finalizers.pending.len();
            forget_dead(&heap, &mut graph);
            steps.push(Step::Minor(slices));
            assert_eq!(outcome(&heap), outcome(&twin), "seed {seed} round {round}");
            check_gen_invariant(&heap).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
            if rng.chance(50) {
                run_finalizers(&mut heap);
                steps.push(Step::Finalize);
            }
        }
        // A full collection in generational form decides what the
        // reference does, and leaves the heap generational.
        let (mut twin, _) = replay(&ops, &steps);
        crate::gc_reference::collect(&mut twin, &[]);
        incremental(&mut heap, &mut rng, |_, _| {});
        assert!(heap.collector.generational, "seed {seed}");
        assert_eq!(outcome(&heap), outcome(&twin), "seed {seed} full");
        check_gen_invariant(&heap).unwrap();
        assert!(
            every_object(&heap)
                .iter()
                .all(|object| heap.age_of(*object) == age::OLD),
            "seed {seed}"
        );
    }
    assert!(
        freed > 10_000 && touched > 10_000 && queued > 1_000,
        "{freed} {touched} {queued}"
    );
}

/// Gate F: an old object given a young one keeps it through young
/// collections, for each kind of write, and the ages go as Lua's
/// (`gengc.lua`): the parent touched, then `TOUCHED2`, then old; the
/// child new, `SURVIVAL`, `OLD1`, old. With the barrier off (the
/// negative control), the first young collection frees the child.
#[test]
fn old_objects_keep_young_ones_given_them() {
    /// Writes the child into the parent; returns the object written to.
    type Write = fn(&mut Heap, Value, Value) -> TraceRef;
    type Make = fn(&mut Heap) -> Value;
    let table: Make = |heap| Value::Table(heap.alloc_table().unwrap());
    let userdata: Make = |heap| {
        let userdata = heap
            .alloc_userdata(MAX, 1, 0, || crate::userdata::Payload::Bytes(Box::new([])))
            .unwrap();
        Value::Userdata(userdata)
    };
    let closure: Make = |heap| {
        let mut graph = Graph {
            objects: Vec::new(),
            proto: None,
        };
        apply(heap, &mut graph, &Op::Closure(vec![V::Int(0)]));
        graph.objects[0]
    };
    let thread: Make = |heap| {
        let mut graph = Graph {
            objects: Vec::new(),
            proto: None,
        };
        apply(heap, &mut graph, &Op::Thread(vec![V::Int(0)]));
        graph.objects[0]
    };
    let cases: [(&str, Make, Make, Write, bool); 8] = [
        (
            "old table, young table value",
            table,
            table,
            |heap, parent, child| {
                let Value::Table(table) = parent else {
                    panic!()
                };
                set(heap, table, Value::Integer(1), child);
                crate::gc::value_ref(parent).unwrap()
            },
            true,
        ),
        (
            "old table, young table key",
            table,
            table,
            |heap, parent, child| {
                let Value::Table(table) = parent else {
                    panic!()
                };
                set(heap, table, child, Value::Integer(1));
                crate::gc::value_ref(parent).unwrap()
            },
            false,
        ),
        (
            "old table, young metatable",
            table,
            table,
            |heap, parent, child| {
                let Value::Table(child) = child else { panic!() };
                heap.set_metatable(parent, Some(child));
                crate::gc::value_ref(parent).unwrap()
            },
            false,
        ),
        (
            "old userdata, young closure user value",
            userdata,
            closure,
            |heap, parent, child| {
                let Value::Userdata(userdata) = parent else {
                    panic!()
                };
                heap.userdata.get_mut(userdata).unwrap().user_values[0] = child;
                crate::gc::value_ref(parent).unwrap()
            },
            true,
        ),
        (
            "old userdata, young metatable",
            userdata,
            table,
            |heap, parent, child| {
                let Value::Table(child) = child else { panic!() };
                heap.set_metatable(parent, Some(child));
                crate::gc::value_ref(parent).unwrap()
            },
            false,
        ),
        (
            "old closed upvalue, young table",
            closure,
            table,
            |heap, parent, child| {
                let Value::Closure(closure) = parent else {
                    panic!()
                };
                let upvalue = heap.closures.get(closure).unwrap().upvalues[0];
                heap.upvalues.get_mut(upvalue).unwrap().state = UpvalueState::Closed(child);
                TraceRef {
                    kind: Kind::Upvalue,
                    index: upvalue.index,
                }
            },
            true,
        ),
        (
            "old thread stack, young userdata",
            thread,
            userdata,
            |heap, parent, child| {
                let Value::Thread(thread) = parent else {
                    panic!()
                };
                heap.threads.get_mut(thread).unwrap().stack[0] = child;
                crate::gc::value_ref(parent).unwrap()
            },
            true,
        ),
        (
            "old thread stack, young string",
            thread,
            |heap| Value::String(heap.alloc_string(b"young".to_vec()).unwrap()),
            |heap, parent, child| {
                let Value::Thread(thread) = parent else {
                    panic!()
                };
                heap.threads.get_mut(thread).unwrap().stack[0] = child;
                crate::gc::value_ref(parent).unwrap()
            },
            false,
        ),
    ];
    for (name, make_parent, make_child, write, control) in cases {
        for barrier in [true, false] {
            if !barrier && !control {
                continue;
            }
            let mut heap = Heap::new();
            heap.gc.auto = false;
            let parent = make_parent(&mut heap);
            let id = heap.object_id_of_value(parent).unwrap();
            let (kind, index, generation) = heap.find_by_id(id).unwrap();
            heap.host_roots.push(HostRoot {
                kind,
                index,
                generation,
                id,
            });
            enter_gen(&mut heap);
            assert_eq!(age_of(&heap, parent), age::OLD, "{name}");
            let child = make_child(&mut heap);
            assert_eq!(age_of(&heap, child), age::NEW, "{name}");
            heap.force_barriers(barrier);
            let written = write(&mut heap, parent, child);
            heap.force_barriers(true);
            // The age of the object written to.
            let parent_age = |heap: &Heap| heap.age_of(written);
            let mut rng = Rng(7);
            if !barrier {
                minor(&mut heap, &mut rng);
                assert!(!alive(&heap, child), "{name}: the control kept the child");
                continue;
            }
            assert_eq!(parent_age(&heap), age::TOUCHED1, "{name}");
            check_gen_invariant(&heap).unwrap();
            let ages = [
                (age::TOUCHED2, age::SURVIVAL),
                (age::OLD, age::OLD1),
                (age::OLD, age::OLD),
                (age::OLD, age::OLD),
            ];
            for (round, (want_parent, child_age)) in ages.into_iter().enumerate() {
                minor(&mut heap, &mut rng);
                assert!(alive(&heap, child), "{name}: round {round}");
                check_gen_invariant(&heap).unwrap();
                let child_age = if matches!(child, Value::String(_)) && child_age == age::OLD1 {
                    age::OLD
                } else {
                    child_age
                };
                assert_eq!(
                    (parent_age(&heap), age_of(&heap, child)),
                    (want_parent, child_age),
                    "{name}: round {round}"
                );
            }
        }
    }
}

/// Gate H: roots are not aged objects but traced by every young
/// collection: a young object held only by the globals, the registry, a
/// type metatable, a host root, or the finalizer queue lives.
#[test]
fn young_objects_held_by_roots_live() {
    type Hold = fn(&mut Heap, Handle<TableObj>);
    let holds: [(&str, Hold); 5] = [
        ("globals", |heap, table| heap.globals = Some(table)),
        ("registry", |heap, table| heap.registry = Some(table)),
        ("type metatable", |heap, table| {
            heap.type_metatables[0] = Some(table)
        }),
        ("host root", |heap, table| {
            let id = heap.tables.get(table).unwrap().id;
            heap.host_roots.push(HostRoot {
                kind: Kind::Table,
                index: table.index,
                generation: table.generation,
                id,
            });
        }),
        ("finalizer queue", |heap, table| {
            heap.finalizers.pending.push_back(FinRef::Table(table));
        }),
    ];
    for (name, hold) in holds {
        let mut heap = Heap::new();
        heap.gc.auto = false;
        enter_gen(&mut heap);
        let table = heap.alloc_table().unwrap();
        let inner = heap.alloc_table().unwrap();
        set(&mut heap, table, Value::Integer(1), Value::Table(inner));
        hold(&mut heap, table);
        let mut rng = Rng(3);
        for round in 0..4 {
            minor(&mut heap, &mut rng);
            assert!(alive(&heap, Value::Table(inner)), "{name} {round}");
            check_gen_invariant(&heap).unwrap();
        }
    }
}

/// Gate K, from `gengc.lua`: an old all-weak table given a young object
/// is touched; two young collections later it is old again; given another
/// young object nothing else holds, the next young collection clears it.
#[test]
fn old_weak_tables_are_cleared_by_young_collections() {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    let t = weak(&mut heap, b"kv");
    heap.host_roots.push(HostRoot {
        kind: Kind::Table,
        index: t.index,
        generation: t.generation,
        id: heap.tables.get(t).unwrap().id,
    });
    enter_gen(&mut heap);
    let mut rng = Rng(5);
    let young = Value::Table(heap.alloc_table().unwrap());
    set(&mut heap, t, Value::Integer(1), young);
    assert_eq!(age_of(&heap, Value::Table(t)), age::TOUCHED1);
    assert_eq!(heap.mark_of(root(t)), Some(mark::GRAY));
    minor(&mut heap, &mut rng);
    assert_eq!(age_of(&heap, Value::Table(t)), age::TOUCHED2);
    assert_eq!(heap.mark_of(root(t)), Some(mark::BLACK));
    assert_eq!(get(&heap, t, Value::Integer(1)), Value::Nil);
    minor(&mut heap, &mut rng);
    assert_eq!(age_of(&heap, Value::Table(t)), age::OLD);
    let young = Value::Table(heap.alloc_table().unwrap());
    set(&mut heap, t, Value::Integer(1), young);
    minor(&mut heap, &mut rng);
    assert_eq!(get(&heap, t, Value::Integer(1)), Value::Nil);
    assert!(!alive(&heap, young));
}

/// Gate J: a young object registered for finalization and dropped is
/// found by the next young collection, resurrected for its finalizer,
/// and freed by a young collection after that: no major needed. An old
/// one dropped waits for a major.
#[test]
fn young_finalizable_objects_need_no_major() {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    enter_gen(&mut heap);
    let mut graph = Graph {
        objects: Vec::new(),
        proto: None,
    };
    apply(&mut heap, &mut graph, &Op::MetaTable(true, None));
    let meta = graph.objects[0];
    heap.host_roots.push(HostRoot {
        kind: Kind::Table,
        index: crate::gc::value_ref(meta).unwrap().index,
        generation: 0,
        id: heap.object_id_of_value(meta).unwrap(),
    });
    let old = heap.alloc_table().unwrap();
    let Value::Table(meta) = meta else { panic!() };
    heap.set_metatable(Value::Table(old), Some(meta));
    let holder = heap.alloc_table().unwrap();
    set(&mut heap, holder, Value::Integer(1), Value::Table(old));
    heap.host_roots.push(HostRoot {
        kind: Kind::Table,
        index: holder.index,
        generation: holder.generation,
        id: heap.tables.get(holder).unwrap().id,
    });
    let mut rng = Rng(9);
    for _ in 0..3 {
        minor(&mut heap, &mut rng);
    }
    assert_eq!(age_of(&heap, Value::Table(old)), age::OLD);
    // The old one dropped, and a new one made and dropped.
    set(&mut heap, holder, Value::Integer(1), Value::Nil);
    let young = heap.alloc_table().unwrap();
    heap.set_metatable(Value::Table(young), Some(meta));
    minor(&mut heap, &mut rng);
    let pending: Vec<_> = heap
        .finalizers
        .pending
        .iter()
        .map(|fin| fin.value())
        .collect();
    assert_eq!(pending, vec![Value::Table(young)]);
    run_finalizers(&mut heap);
    minor(&mut heap, &mut rng);
    assert!(!alive(&heap, Value::Table(young)));
    assert!(alive(&heap, Value::Table(old)));
    // A full collection finds the old one.
    crate::gc::full(&mut heap, &[], MAX);
    let pending: Vec<_> = heap
        .finalizers
        .pending
        .iter()
        .map(|fin| fin.value())
        .collect();
    assert_eq!(pending, vec![Value::Table(old)]);
    assert!(heap.collector.generational);
}

/// Gate E (hard): a young collection does not trace the old heap. With
/// 8,000 old tables of 10 entries and a few young objects, it takes units
/// for the young objects, the roots and the touched objects only.
#[test]
fn young_collections_do_not_trace_the_old_heap() {
    let mut heap = Heap::new();
    heap.gc.auto = false;
    let top = heap.alloc_table().unwrap();
    heap.globals = Some(top);
    for i in 0..8_000 {
        let table = heap.alloc_table().unwrap();
        for j in 0..10 {
            set(&mut heap, table, Value::Integer(j), Value::Integer(i));
        }
        set(&mut heap, top, Value::Integer(i), Value::Table(table));
    }
    enter_gen(&mut heap);
    let mut rng = Rng(11);
    let small = heap.alloc_table().unwrap();
    let holder = heap.alloc_table().unwrap();
    for i in 0..100 {
        let table = heap.alloc_table().unwrap();
        set(&mut heap, holder, Value::Integer(i), Value::Table(table));
    }
    // One old object written: the old table of tables is not.
    let Value::Table(old) = get(&heap, top, Value::Integer(5)) else {
        panic!()
    };
    set(&mut heap, old, Value::Integer(2), Value::Table(holder));
    let _ = small;
    let units = minor(&mut heap, &mut rng);
    assert!(units < 1_000, "a young collection took {units} units");
    assert!(alive(&heap, Value::Table(holder)));
    assert!(!alive(&heap, Value::Table(small)));
}
