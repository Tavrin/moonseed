//! Constant strings, automatic collection, and memory limits.

use super::*;

/// Objects allocated while running `source` to the end: the object-id
/// counter's growth, including objects already collected.
fn allocations(source: &[u8]) -> (u64, Runtime) {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let before = runtime.heap().next_object_id;
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    (runtime.heap().next_object_id - before, runtime)
}

fn loop_source(body: &str, n: u32) -> Vec<u8> {
    format!(
        "local t = setmetatable({{}}, {{ __index = function(self, key) return 1 end }}) \
         local u = {{}} local s = 0 \
         for i = 1, {n} do {body} end return s"
    )
    .into_bytes()
}

#[test]
fn source_constants_do_not_allocate_per_execution() {
    for body in [
        "s = s + t.foo",
        "u.foo = i local x = u.foo",
        "u.foo = nil u.foo = i",
        "local x = 'foo'",
    ] {
        let (short, _) = allocations(&loop_source(body, 10));
        let (long, _) = allocations(&loop_source(body, 1000));
        assert_eq!(
            short, long,
            "{body}: {short} objects for 10 iterations, {long} for 1000"
        );
    }
}

#[test]
fn the_constant_key_fixture_allocates_nothing_per_iteration() {
    let body = "local t = {} for i = 1, N do t.foo = i local x = t.foo end return t.foo";
    let (short, _) = allocations(body.replace('N', "10").as_bytes());
    let (long, runtime) = allocations(body.replace('N', "10000").as_bytes());
    assert_eq!(short, long);
    assert_eq!(print_line(&runtime), "10000\n");
}

#[test]
fn equal_strings_are_equal_whichever_object_holds_them() {
    // Constants of two prototypes are two objects with the same bytes.
    let source = b"local f = function() return 'same' end \
        local a, b = 'same', 'same' local t = {} t[f()] = 1 \
        return a == b, f() == a, t.same, t['same']";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "true\ttrue\t1\t1\n");

    // A string made at run time, not a constant, compares and indexes by
    // its bytes too.
    let source = b"local k = 0 local t = { same = 1 } return k == 'same', t[k], k ~= 'other'";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    run_steps(&mut runtime, &mut journal, 1);
    runtime.store_new_string(0, b"same").unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "true\t1\ttrue\n");
}

#[test]
fn restored_prototypes_keep_their_constant_strings() {
    let source = loop_source("s = s + t.foo local x = 'foo'", 1000);
    let chunk = crate::compile(&source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    runtime.run(50, &mut journal).unwrap();
    let bytes = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    let before = restored.heap().next_object_id;
    let strings = restored.heap().strings.live();
    assert_eq!(
        restored.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(restored.heap().next_object_id, before);
    assert_eq!(restored.heap().strings.live(), strings);
    assert_eq!(print_line(&restored), "1000\n");

    // A prototype constant that names anything but a string is refused.
    let mut image = runtime.to_image().unwrap();
    let table = image.tables[0].id;
    let proto = image
        .protos
        .iter_mut()
        .find(|proto| !proto.const_ids.is_empty())
        .unwrap();
    proto.const_ids[0] = table;
    let bad = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bad, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::DanglingReference,
    );

    // Two constants may not share one string object: each prototype owns
    // its own, and a shared id would let a small snapshot decode into many
    // copies of one string.
    let mut image = runtime.to_image().unwrap();
    let proto = image
        .protos
        .iter_mut()
        .find(|proto| proto.const_ids.len() >= 2)
        .unwrap();
    proto.const_ids[1] = proto.const_ids[0];
    let bad = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bad, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidStructure,
    );
}

fn boot_with(config: Config, source: &[u8]) -> Runtime {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.set_global_native("second", "second").unwrap();
    runtime.install_base().unwrap();
    runtime
}

fn finish(runtime: &mut Runtime) -> Result<StepOutcome, VmError> {
    runtime.run_until_terminal(u64::MAX, &mut Journal::new())
}

/// Garbage-making loops, each far more objects than `max_objects` in all.
const CHURN: &[(&str, &str)] = &[
    (
        "tables",
        "local s = 0 for i = 1, 5000 do local t = { i, i + 1 } s = s + t[2] end return s",
    ),
    (
        "closures",
        "local s = 0 for i = 1, 5000 do local f = function() return i end s = s + f() end return s",
    ),
    (
        "metamethod constants",
        "local t = setmetatable({}, { __index = function(self, key) return { 1 } end }) \
         local s = 0 for i = 1, 5000 do s = s + t.foo[1] end return s",
    ),
    (
        "strings",
        "local s = 0 for i = 1, 5000 do local x = 'n' .. i .. '.' s = s + #x end return s",
    ),
    (
        "generic for",
        "local it = function(n, c) if c == nil then return { 1 } end \
         if c[1] < n then return { c[1] + 1 } end end \
         local s = 0 for t in it, 5000 do s = s + t[1] end return s",
    ),
];

#[test]
fn garbage_loops_finish_under_a_small_object_limit() {
    let small = Config {
        max_objects: 300,
        ..Config::default()
    };
    for (name, source) in CHURN {
        let mut runtime = boot_with(small.clone(), source.as_bytes());
        assert_eq!(
            finish(&mut runtime),
            Ok(StepOutcome::Completed),
            "{name}: {:?}",
            runtime.memory()
        );
        let memory = runtime.memory();
        assert!(memory.collections > 10, "{name}: {memory:?}");
        assert!(memory.objects <= 300, "{name}: {memory:?}");

        // The same loop without automatic collection runs out of objects.
        let mut runtime = boot_with(
            Config {
                auto_gc: false,
                ..small.clone()
            },
            source.as_bytes(),
        );
        assert_eq!(
            finish(&mut runtime),
            Ok(StepOutcome::LuaError(LuaFault::Memory)),
            "{name}"
        );
        // The thread failed; it stays failed.
        assert_eq!(
            runtime.run(1, &mut Journal::new()),
            Ok(StepOutcome::LuaError(LuaFault::Memory))
        );
    }
}

#[test]
fn a_string_past_the_size_bound_is_a_memory_error() {
    let mut runtime = boot_with(
        Config::default(),
        b"local s = 'x' for i = 1, 40 do s = s .. s end return #s",
    );
    assert_eq!(
        finish(&mut runtime),
        Ok(StepOutcome::LuaError(LuaFault::Memory))
    );
    // A string of exactly the bound is fine.
    let mut runtime = boot_with(
        Config::default(),
        b"local s = 'x' for i = 1, 20 do s = s .. s end return #s",
    );
    assert_eq!(finish(&mut runtime), Ok(StepOutcome::Completed));
    assert_eq!(print_line(&runtime), "1048576\n");
}

#[test]
fn keeping_everything_still_reaches_the_object_limit() {
    let mut runtime = boot_with(
        Config {
            max_objects: 300,
            ..Config::default()
        },
        b"local keep = {} for i = 1, 5000 do keep[i] = {} end return #keep",
    );
    assert_eq!(
        finish(&mut runtime),
        Ok(StepOutcome::LuaError(LuaFault::Memory))
    );
    assert!(runtime.memory().collections > 0);
}

/// Every collection's fuel point, following the run across restores.
fn schedule(source: &str, quantum: u64, restore_every: Option<u32>) -> (Vec<u64>, String) {
    let config = Config {
        max_objects: 400,
        gc_min_debt: 2048,
        ..Config::default()
    };
    let mut runtime = boot_with(config, source.as_bytes());
    let mut journal = Journal::new();
    let mut log = Vec::new();
    let mut slices = 0u32;
    loop {
        match runtime.run(quantum, &mut journal) {
            Ok(StepOutcome::Paused(_)) => {}
            Ok(StepOutcome::Completed) => break,
            other => panic!("{other:?} {:?}", runtime.memory()),
        }
        slices += 1;
        if restore_every.is_some_and(|every| slices.is_multiple_of(every)) {
            log.append(&mut runtime.gc_log);
            let bytes = runtime.snapshot().unwrap();
            runtime =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                    .unwrap();
        }
    }
    log.append(&mut runtime.gc_log);
    (log, print_line(&runtime))
}

#[test]
fn collections_run_at_the_same_points_under_every_schedule() {
    for (name, source) in CHURN {
        let (expected, line) = schedule(source, u64::MAX, None);
        assert!(expected.len() > 10, "{name}: {expected:?}");
        for quantum in [1, 2, 3, 7] {
            assert_eq!(
                schedule(source, quantum, None),
                (expected.clone(), line.clone())
            );
        }
        for (quantum, every) in [(1, 97), (7, 13), (50, 1)] {
            assert_eq!(
                schedule(source, quantum, Some(every)),
                (expected.clone(), line.clone()),
                "{name}: quantum {quantum}, restore every {every} slices"
            );
        }
    }
}

pub(super) fn boot_eager(spec: &crate::program::ProtoSpec) -> Runtime {
    // The smallest threshold: a collection after nearly every allocation.
    let config = Config {
        gc_min_debt: 1,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), spec, false).unwrap();
    for name in PROOF_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
    runtime.install_base().unwrap();
    runtime.install_string().unwrap();
    runtime
}

#[test]
fn fixtures_match_under_eager_automatic_collection() {
    for (name, line) in CONTROL_FIXTURES.iter().chain(NATIVE_FIXTURES) {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let runtime = finish_with(boot_eager, &chunk.proto);
        assert_eq!(print_line(&runtime), *line, "{name}");
        assert!(runtime.memory().collections > 0, "{name}");
        quantum_and_checkpoints_with(boot_eager, &chunk.proto, pair_results);
    }
}

#[test]
fn collection_state_is_snapshot_state_and_fails_closed() {
    let mut runtime = boot_with(Config::default(), CHURN[0].1.as_bytes());
    runtime.run(500, &mut Journal::new()).unwrap();
    runtime.set_auto_gc(false);
    let bytes = runtime.snapshot().unwrap();
    let restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    assert_eq!(restored.memory(), runtime.memory());
    assert!(!restored.memory().auto_gc);

    let mut image = runtime.to_image().unwrap();
    image.gc.threshold = 0;
    let bad = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bad, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidStructure,
    );
    // The GC policy revision follows the three revisions after the schema.
    let mut other_policy = bytes.clone();
    other_policy[12] = 99;
    recrc(&mut other_policy);
    expect_snapshot(
        Runtime::from_snapshot(
            &other_policy,
            &HostRegistry::proof(),
            runtime.effect_domain(),
        ),
        SnapshotError::BadVersion,
    );
}

#[test]
fn metamethod_calls_do_not_grow_the_stack() {
    let stack_after = |n: u32| {
        let source = format!(
            "local t = setmetatable({{}}, {{ __index = function(self, key) return {{ 1 }} end, \
             __len = second, __newindex = function() end }}) \
             local s = 0 for i = 1, {n} do s = s + t.foo[1] + #t t.bar = i end return s"
        );
        let mut runtime = boot_with(
            Config {
                auto_gc: false,
                ..Config::default()
            },
            source.as_bytes(),
        );
        finish(&mut runtime).unwrap();
        runtime.collect();
        let thread = runtime.heap().threads.iter().next().unwrap().2;
        (thread.stack.len(), thread.top, runtime.heap().tables.live())
    };
    assert_eq!(stack_after(10), stack_after(200));
}

/// An external native that grows its first argument by eight slots, then
/// waits for the host.
fn grow_then_wait(call: &mut crate::host::NativeCall<'_>) -> crate::host::NativeOutcome {
    use crate::host::NativeValue;
    let sink = call.arg(0);
    for key in 1..=8 {
        let key = NativeValue::wrap(crate::value::Value::Integer(key));
        if call.raw_set(sink, key, key).is_err() {
            return crate::host::NativeOutcome::Fault;
        }
    }
    crate::host::NativeOutcome::Pending(WaitKey(1))
}

#[test]
fn host_calls_during_a_wait_do_not_move_collections() {
    // The native's table arguments are garbage once the call finishes, but
    // live on the stack while it waits. A collection that the native's
    // growth made due must not depend on whether the host calls `run`
    // again before completing the wait.
    let run = |poll_while_waiting: bool| {
        let mut registry = HostRegistry::proof();
        registry.register_native(
            "grow_then_wait",
            crate::host::NativePolicy::External,
            grow_then_wait,
        );
        let chunk = crate::compile(
            b"local s = 0 for i = 1, 60 do s = s + w({}, {}, {}, {}, {}) end return s",
        )
        .unwrap();
        let config = Config {
            max_objects: 2000,
            gc_min_debt: 200,
            ..Config::default()
        };
        let mut runtime = Runtime::boot(config, registry, &chunk.proto, false).unwrap();
        runtime.set_global_native("w", "grow_then_wait").unwrap();
        let mut journal = Journal::new();
        loop {
            match runtime.run(u64::MAX, &mut journal).unwrap() {
                StepOutcome::Waiting(key) => {
                    if poll_while_waiting {
                        assert_eq!(
                            runtime.run(u64::MAX, &mut journal).unwrap(),
                            StepOutcome::Waiting(key)
                        );
                    }
                    runtime.complete_wait(key, 1).unwrap();
                }
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(print_line(&runtime), "60\n");
        (runtime.gc_log.clone(), runtime.memory())
    };
    let direct = run(false);
    assert!(direct.0.len() >= 5, "{direct:?}");
    assert_eq!(run(true), direct);
}

/// Every state the runtime's limits allow snapshots and restores (ADR
/// 0052): a table far past the old 10,000-entry snapshot bound, at every
/// checkpoint of its making.
#[test]
fn snapshots_take_every_state_the_limits_allow() {
    let wide = "local t = {} for i = 1, 30000 do t[i] = i; t['k' .. i] = i end return #t";
    let chunk = crate::compile(wide.as_bytes()).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    loop {
        let bytes = runtime.snapshot().unwrap();
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
        match runtime.run(20_011, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn restore_bounds_every_slot_index() {
    use crate::snapshot::{Image, PendingImage, TargetImage, UpImageState};
    // The hand-built program adds a frame with extra arguments.
    let source = b"local x = 0 \
        local u = setmetatable({}, {__newindex = function(t, k, v) x = x + v end}) \
        local f = function(a, b) local g = function() return x end \
            local i i, u[1], u[2] = a, b, 3 return g() + i end \
        return f(1, 2)";
    let huge = 0x7fff_ffff;
    let frames = |image: &mut Image,
                  edit: &mut dyn FnMut(&mut crate::snapshot::FrameImage) -> bool| {
        let mut changed = false;
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                changed |= edit(frame);
            }
        }
        changed
    };
    type Tamper<'a> = Box<dyn Fn(&mut Image) -> bool + 'a>;
    let variants: Vec<(&str, Tamper)> = vec![
        (
            "top",
            Box::new(|image| {
                image
                    .threads
                    .iter_mut()
                    .for_each(|thread| thread.top = huge);
                true
            }),
        ),
        (
            "base",
            Box::new(move |image| {
                frames(image, &mut |frame| {
                    frame.base = frame.base.wrapping_add(0x7fff_0000);
                    true
                })
            }),
        ),
        (
            "limit",
            Box::new(move |image| {
                frames(image, &mut |frame| {
                    frame.limit = huge;
                    true
                })
            }),
        ),
        (
            "varargs",
            Box::new(move |image| {
                frames(image, &mut |frame| {
                    frame.vararg_len = huge;
                    true
                })
            }),
        ),
        (
            "meta slot",
            Box::new(move |image| {
                frames(image, &mut |frame| {
                    frame.meta.as_mut().map(|meta| meta.slot = huge).is_some()
                })
            }),
        ),
        (
            "assignment source",
            Box::new(move |image| {
                frames(image, &mut |frame| match &mut frame.pending {
                    PendingImage::Assigning { src, .. } => {
                        *src = huge;
                        true
                    }
                    _ => false,
                })
            }),
        ),
        (
            "assignment cursor",
            Box::new(move |image| {
                frames(image, &mut |frame| match &mut frame.pending {
                    PendingImage::Assigning { next, .. } => {
                        *next = u16::MAX;
                        true
                    }
                    _ => false,
                })
            }),
        ),
        (
            "register target",
            Box::new(move |image| {
                frames(image, &mut |frame| {
                    let mut changed = false;
                    for target in &mut frame.targets {
                        if let TargetImage::Register(slot) = target {
                            *slot = huge;
                            changed = true;
                        }
                    }
                    changed
                })
            }),
        ),
        (
            "callee below its caller",
            Box::new(|image| {
                let mut changed = false;
                for thread in &mut image.threads {
                    if let [_, .., last] = thread.frames.as_mut_slice() {
                        last.limit -= last.base;
                        last.base = 0;
                        changed = true;
                    }
                }
                changed
            }),
        ),
        (
            "open upvalue",
            Box::new(|image| {
                let mut changed = false;
                for upvalue in &mut image.upvalues {
                    if let UpImageState::Open { slot, .. } = &mut upvalue.state {
                        *slot = huge;
                        changed = true;
                    }
                }
                changed
            }),
        ),
    ];
    let chunk = crate::compile(source).unwrap();
    let mut hits = vec![0; variants.len()];
    for (spec, expected) in [
        (chunk.proto, "6\n"),
        (crate::program::vararg_after_call_program(), "1\t2\t3\n"),
    ] {
        let mut runtime = boot_natives(&spec);
        let mut journal = Journal::new();
        loop {
            for (index, (name, tamper)) in variants.iter().enumerate() {
                let mut image = runtime.to_image().unwrap();
                if !tamper(&mut image) {
                    continue;
                }
                let bytes = snapshot::encode(&image).unwrap();
                let restored =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain());
                assert!(restored.is_err(), "{name} was accepted");
                hits[index] += 1;
            }
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(print_line(&runtime), expected);
    }
    for ((name, _), hits) in variants.iter().zip(hits) {
        assert!(hits > 0, "{name} never applied");
    }
}
