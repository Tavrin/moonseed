//! The embedding surface (Phase 3.31): object lookup by id.

use super::*;
use crate::api::{
    AnyUserData, ApiError, Coerce, Error, FromLua, FromLuaMulti, Function, FunctionKind, IntoLua,
    IntoLuaMulti, Libraries, LuaError, LuaString, MultiValue, Table, Thread, ThreadStatus, Value,
    Variadic,
};
use crate::{GcMode, LegacyCompletion};

/// The object `id` by a scan of every arena, as lookups were made before
/// the id index: what the index must agree with.
fn scanned(heap: &crate::heap::Heap, id: ObjectId) -> Option<(crate::id::Kind, u32, u32)> {
    use crate::id::Kind;
    macro_rules! scan {
        ($arena:ident, $kind:expr) => {
            for (index, generation, object) in heap.$arena.iter() {
                if object.id == id {
                    return Some(($kind, index, generation));
                }
            }
        };
    }
    scan!(strings, Kind::String);
    scan!(tables, Kind::Table);
    scan!(protos, Kind::Proto);
    scan!(upvalues, Kind::Upvalue);
    scan!(closures, Kind::Closure);
    scan!(threads, Kind::Thread);
    scan!(native_closures, Kind::NativeClosure);
    scan!(userdata, Kind::Userdata);
    None
}

/// Gate B: the id index agrees with a scan for every id ever made, live
/// or freed, through churn that frees objects and reuses their slots in
/// both collector modes, and after restore; a freed object's id never
/// finds the object that took its slot.
#[test]
fn ids_find_their_object_and_only_it() {
    let source = "
        keep = {}
        for round = 1, 40 do
          for i = 1, 300 do
            local kind = i % 4
            local v
            if kind == 0 then v = 's' .. round .. ':' .. i
            elseif kind == 1 then v = {i}
            elseif kind == 2 then v = function() return i end
            else v = coroutine.create(function() return i end) end
            if i % 3 == 0 then keep[#keep + 1] = v end
          end
          if round % 10 == 0 then for j = 1, #keep, 2 do keep[j] = false end end
          park()
        end";
    for mode in [GcMode::Generational, GcMode::Incremental] {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let config = Config {
            gc_mode: mode,
            gc_min_debt: 4096,
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.install_standard().unwrap();
        runtime.set_global_native("park", "park").unwrap();
        let mut journal = Journal::new();
        // The index exists from the first lookup on; ids made before and
        // after it are both covered.
        assert!(!runtime.contains_id(ObjectId(u64::MAX)));
        let mut round = 0;
        loop {
            match runtime.run_until_terminal(u64::MAX, &mut journal).unwrap() {
                StepOutcome::Waiting(key) => {
                    round += 1;
                    let heap = runtime.heap();
                    let mut live = 0;
                    for raw in 1..heap.next_object_id {
                        let id = ObjectId(raw);
                        let want = scanned(heap, id);
                        live += usize::from(want.is_some());
                        assert_eq!(heap.find_by_id(id), want, "{mode:?} round {round} id {raw}");
                    }
                    assert!(live > 100);
                    if round % 13 == 0 {
                        runtime = super::base::restore(&runtime);
                        runtime.set_global_native("park", "park").unwrap_or(());
                    }
                    runtime
                        .complete_legacy(key, LegacyCompletion::Return(vec![]))
                        .unwrap();
                }
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(round, 40);
    }
}

fn idle(mode: GcMode) -> Runtime {
    Runtime::builder()
        .config(Config {
            auto_gc: false,
            gc_mode: mode,
            ..Config::default()
        })
        .build()
        .unwrap()
}

fn wrong<T>(result: crate::api::Result<T>) {
    assert!(matches!(result, Err(Error::Api(ApiError::WrongRuntime))));
}

#[test]
fn owned_roots_survive_both_collectors_and_last_drop_releases() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let mut runtime = idle(mode);
        let string = runtime.create_string(b"held").unwrap();
        let id = string.id();
        let copy = string.clone();
        drop(string);
        for _ in 0..3 {
            runtime.collect();
            assert_eq!(copy.as_bytes(&runtime).unwrap(), b"held");
            crate::gc::check_invariant(runtime.heap()).unwrap();
            crate::gc::check_gen_invariant(runtime.heap()).unwrap();
        }
        drop(copy);
        runtime.collect();
        assert!(runtime.object(id).is_none());
        let new = runtime.create_string(b"replacement").unwrap();
        assert_ne!(new.id(), id);
        assert!(runtime.object(id).is_none());
        assert!(runtime.object(ObjectId(u64::MAX)).is_none());
    }
}

#[test]
fn a_root_added_during_marking_is_rescanned_atomically() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let mut runtime = idle(mode);
        let string = runtime.create_string(b"white until rooted").unwrap();
        let id = string.id();
        drop(string);
        crate::gc::request_full(runtime.heap_mut());
        let max = runtime.limits().max_objects;
        while runtime.heap().collector.phase != crate::gc::Phase::Propagate {
            crate::gc::work(runtime.heap_mut(), &[], 1, max);
        }
        let owned = runtime.object(id).unwrap();
        while runtime.heap().gc.full.is_some() {
            crate::gc::work(runtime.heap_mut(), &[], 1, max);
            crate::gc::check_invariant(runtime.heap()).unwrap();
            crate::gc::check_gen_invariant(runtime.heap()).unwrap();
        }
        assert_eq!(
            LuaString::from_lua(owned, &mut runtime)
                .unwrap()
                .as_bytes(&runtime)
                .unwrap(),
            b"white until rooted"
        );
    }
}

#[test]
fn an_atomic_lookup_finishes_the_phase_before_naming_a_dead_object() {
    let mut runtime = idle(GcMode::Incremental);
    let string = runtime.create_string(b"unreachable").unwrap();
    let id = string.id();
    drop(string);
    crate::gc::request_full(runtime.heap_mut());
    let max = runtime.limits().max_objects;
    while !matches!(runtime.heap().collector.phase, crate::gc::Phase::Atomic(_)) {
        crate::gc::work(runtime.heap_mut(), &[], 1, max);
    }
    assert!(runtime.object(id).is_none());
    crate::gc::check_invariant(runtime.heap()).unwrap();
}

#[test]
fn rooted_children_survive_old_table_writes_and_young_collections() {
    let mut runtime = idle(GcMode::Generational);
    let table = runtime.create_table().unwrap();
    runtime.collect();
    let string = runtime.create_string(b"young").unwrap();
    let id = string.id();
    table.raw_set(&mut runtime, 1, string).unwrap();
    let max = runtime.limits().max_objects;
    let room = runtime.gc_room();
    crate::gc::step(runtime.heap_mut(), room, max, true);
    while runtime.heap().collector.holds() || runtime.heap().gc.owed != 0 {
        crate::gc::work(runtime.heap_mut(), &[], 7, max);
    }
    assert!(runtime.contains_id(id));
    assert_eq!(
        table.raw_get::<_, String>(&mut runtime, 1).unwrap(),
        "young"
    );
    crate::gc::check_gen_invariant(runtime.heap()).unwrap();
}

#[test]
fn views_root_without_needing_a_second_runtime_borrow() {
    let mut runtime = idle(GcMode::Incremental);
    let string = runtime.create_string(b"borrow").unwrap();
    let view = runtime.object_ref(string.id()).unwrap();
    assert_eq!(view.as_string().unwrap().as_bytes(), b"borrow");
    drop(string);
    let owned = view.to_owned_value().unwrap();
    let id = owned.id().unwrap();
    runtime.collect();
    assert!(runtime.contains_id(id));
    drop(owned);
    runtime.collect();
    assert!(!runtime.contains_id(id));
}

#[test]
fn owned_values_can_be_cloned_and_dropped_after_the_runtime() {
    let string = {
        let mut runtime = idle(GcMode::Incremental);
        runtime.create_string(b"outlives its runtime").unwrap()
    };
    let copy = string.clone();
    drop(string);
    wrong(copy.as_bytes(&idle(GcMode::Incremental)));
    drop(copy);
}

#[test]
fn references_reject_other_and_restored_runtimes() {
    let mut runtime = idle(GcMode::Incremental);
    let string = runtime.create_string(b"owner").unwrap();
    let table = runtime.create_table().unwrap();
    let mut other = idle(GcMode::Incremental);
    wrong(string.as_bytes(&other));
    wrong(table.raw_get::<_, Value>(&mut other, 1));
    wrong(table.raw_set(&mut other, 1, 2));
    wrong(table.raw_len(&other));
    wrong(table.next(&mut other, None));
    wrong(table.metatable(&mut other));
    wrong(table.set_metatable(&mut other, None));
    wrong(other.globals().raw_set(&mut other, 1, &table));
    wrong(other.globals().raw_set(&mut other, &table, 1));
    wrong(other.globals().set_metatable(&mut other, Some(&table)));
    let globals = runtime.globals();
    globals.raw_set(&mut runtime, "saved", &table).unwrap();
    let snapshot = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&snapshot, runtime.registry(), runtime.effect_domain()).unwrap();
    wrong(table.raw_len(&restored));
    let reacquired = restored.object(table.id()).unwrap();
    assert_eq!(
        Table::from_lua(reacquired, &mut restored).unwrap().id(),
        table.id()
    );
}

#[test]
fn strings_keep_bytes_and_refuse_invalid_utf8() {
    let mut runtime = idle(GcMode::Incremental);
    let string = runtime.create_string(b"a\0\xff").unwrap();
    assert_eq!(string.as_bytes(&runtime).unwrap(), b"a\0\xff");
    assert!(matches!(
        string.to_str(&runtime),
        Err(Error::Api(ApiError::Conversion(_)))
    ));
    assert!(String::from_lua(Value::String(string.clone()), &mut runtime).is_err());
    assert_eq!(
        Vec::<u8>::from_lua(Value::String(string), &mut runtime).unwrap(),
        b"a\0\xff"
    );
    let string = "é".into_lua(&mut runtime).unwrap();
    assert_eq!(String::from_lua(string, &mut runtime).unwrap(), "é");
    let string = b"bytes".as_slice().into_lua(&mut runtime).unwrap();
    assert_eq!(Vec::<u8>::from_lua(string, &mut runtime).unwrap(), b"bytes");
}

#[test]
fn every_integer_conversion_checks_its_boundaries() {
    let mut runtime = idle(GcMode::Incremental);
    macro_rules! signed {
        ($($ty:ty),+) => { $(
            for integer in [<$ty>::MIN, <$ty>::MAX] {
                let value = integer.into_lua(&mut runtime).unwrap();
                assert_eq!(<$ty>::from_lua(value, &mut runtime).unwrap(), integer);
            }
        )+ };
    }
    signed!(i8, i16, i32, i64, isize);
    macro_rules! unsigned {
        ($($ty:ty),+) => { $(
            let max = (<$ty>::MAX as u64).min(i64::MAX as u64) as $ty;
            for integer in [0 as $ty, max] {
                let value = integer.into_lua(&mut runtime).unwrap();
                assert_eq!(<$ty>::from_lua(value, &mut runtime).unwrap(), integer);
            }
            assert!(<$ty>::from_lua(Value::Integer(-1), &mut runtime).is_err());
        )+ };
    }
    unsigned!(u8, u16, u32, u64, usize);
    assert!((i64::MAX as u64 + 1).into_lua(&mut runtime).is_err());
    assert!(i32::from_lua(Value::Integer(i64::MAX), &mut runtime).is_err());
    assert!(i8::from_lua(Value::Integer(128), &mut runtime).is_err());
    assert!(u8::from_lua(Value::Integer(256), &mut runtime).is_err());
    assert!(i64::from_lua(Value::Number(1.0), &mut runtime).is_err());
}

#[test]
fn floats_booleans_unit_and_option_are_strict() {
    let mut runtime = idle(GcMode::Incremental);
    for number in [0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY] {
        let value = number.into_lua(&mut runtime).unwrap();
        assert_eq!(
            f64::from_lua(value, &mut runtime).unwrap().to_bits(),
            number.to_bits()
        );
    }
    assert!(
        f64::from_lua(f64::NAN.into_lua(&mut runtime).unwrap(), &mut runtime)
            .unwrap()
            .is_nan()
    );
    assert_eq!(
        f32::from_lua(1.5f32.into_lua(&mut runtime).unwrap(), &mut runtime).unwrap(),
        1.5
    );
    assert!(f32::from_lua(Value::Number(f64::MAX), &mut runtime).is_err());
    assert_eq!(f64::from_lua(Value::Integer(7), &mut runtime).unwrap(), 7.0);
    assert!(bool::from_lua(Value::Integer(1), &mut runtime).is_err());
    assert!(bool::from_lua(true.into_lua(&mut runtime).unwrap(), &mut runtime).unwrap());
    <()>::from_lua(().into_lua(&mut runtime).unwrap(), &mut runtime).unwrap();
    assert!(<()>::from_lua(Value::Boolean(false), &mut runtime).is_err());
    assert_eq!(
        Option::<i32>::from_lua(None::<i32>.into_lua(&mut runtime).unwrap(), &mut runtime).unwrap(),
        None
    );
    assert_eq!(
        Option::<i32>::from_lua(Some(2).into_lua(&mut runtime).unwrap(), &mut runtime).unwrap(),
        Some(2)
    );
}

#[test]
fn multi_values_preserve_zero_lone_nil_and_holes() {
    let mut runtime = idle(GcMode::Incremental);
    assert!(().into_lua_multi(&mut runtime).unwrap().is_empty());
    let lone = Value::Nil.into_lua_multi(&mut runtime).unwrap();
    assert_eq!(lone.len(), 1);
    assert!(matches!(lone[0], Value::Nil));
    let holes = MultiValue(vec![Value::Nil, Value::Integer(2), Value::Nil]);
    let holes =
        MultiValue::from_lua_multi(holes.into_lua_multi(&mut runtime).unwrap(), &mut runtime)
            .unwrap();
    assert_eq!(holes.len(), 3);
    assert!(matches!(holes[0], Value::Nil));
    assert!(matches!(holes[2], Value::Nil));
    assert!(
        Option::<i32>::from_lua_multi(MultiValue::new(), &mut runtime)
            .unwrap()
            .is_none()
    );
    assert!(
        Variadic::<Value>::from_lua_multi(MultiValue::new(), &mut runtime)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tuples_through_twelve_and_variadics_report_positions() {
    let mut runtime = idle(GcMode::Incremental);
    macro_rules! tuple {
        ($($value:expr),+) => {{
            let tuple = ($($value,)+);
            let values = tuple.into_lua_multi(&mut runtime).unwrap();
            let read = FromLuaMulti::from_lua_multi(values, &mut runtime).unwrap();
            assert_eq!(tuple, read);
        }};
    }
    tuple!(1i32);
    tuple!(1i32, 2i32);
    tuple!(1i32, 2i32, 3i32);
    tuple!(1i32, 2i32, 3i32, 4i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32, 6i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32, 8i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32, 8i32, 9i32);
    tuple!(1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32, 8i32, 9i32, 10i32);
    tuple!(
        1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32, 8i32, 9i32, 10i32, 11i32
    );
    tuple!(
        1i32, 2i32, 3i32, 4i32, 5i32, 6i32, 7i32, 8i32, 9i32, 10i32, 11i32, 12i32
    );
    let values = (1, None::<i32>, 3).into_lua_multi(&mut runtime).unwrap();
    assert_eq!(
        <(i32, Option<i32>, i32)>::from_lua_multi(values, &mut runtime).unwrap(),
        (1, None, 3)
    );
    let values = Variadic(vec![1, 2, 3])
        .into_lua_multi(&mut runtime)
        .unwrap();
    assert_eq!(
        Variadic::<i32>::from_lua_multi(values, &mut runtime)
            .unwrap()
            .0,
        [1, 2, 3]
    );
    let error = <(i32, i32)>::from_lua_multi(
        MultiValue(vec![Value::Integer(1), Value::Nil]),
        &mut runtime,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Api(ApiError::Conversion(error)) if error.position == Some(2)));
    let error = Variadic(vec![1u64, u64::MAX])
        .into_lua_multi(&mut runtime)
        .unwrap_err();
    assert!(matches!(error, Error::Api(ApiError::Conversion(error)) if error.position == Some(2)));
}

#[test]
fn coercion_uses_luas_numeric_parser_and_formatter() {
    let mut runtime = idle(GcMode::Incremental);
    let text = " 0x10 ".into_lua(&mut runtime).unwrap();
    assert!(i32::from_lua(text.clone(), &mut runtime).is_err());
    assert_eq!(Coerce::<i32>::from_lua(text, &mut runtime).unwrap().0, 16);
    let text = "0x1.8p1".into_lua(&mut runtime).unwrap();
    assert_eq!(Coerce::<f64>::from_lua(text, &mut runtime).unwrap().0, 3.0);
    assert_eq!(
        Coerce::<i8>::from_lua(Value::Number(2.0), &mut runtime)
            .unwrap()
            .0,
        2
    );
    assert!(Coerce::<i8>::from_lua(Value::Number(2.5), &mut runtime).is_err());
    assert!(Coerce::<u64>::from_lua(Value::Number(9223372036854775808.0), &mut runtime).is_err());
    assert_eq!(
        Coerce::<String>::from_lua(Value::Number(3.0), &mut runtime)
            .unwrap()
            .0,
        "3.0"
    );
    assert_eq!(
        Coerce::<Vec<u8>>::from_lua(Value::Integer(4), &mut runtime)
            .unwrap()
            .0,
        b"4"
    );
    assert!(
        !Coerce::<bool>::from_lua(Value::Nil, &mut runtime)
            .unwrap()
            .0
    );
    assert!(
        Coerce::<bool>::from_lua(Value::Integer(0), &mut runtime)
            .unwrap()
            .0
    );
    assert!(matches!(
        Coerce(4).into_lua(&mut runtime).unwrap(),
        Value::Integer(4)
    ));
}

#[test]
fn raw_table_reads_writes_and_traversal_keep_exact_accounting() {
    let mut runtime = idle(GcMode::Incremental);
    let table = runtime.create_table().unwrap();
    let before = runtime.memory().logical_bytes;
    table.raw_set(&mut runtime, 1, 10).unwrap();
    assert_eq!(
        runtime.memory().logical_bytes - before,
        crate::heap::cost::ENTRY
    );
    table.raw_set(&mut runtime, 1.0, 11).unwrap();
    assert_eq!(
        runtime.memory().logical_bytes - before,
        crate::heap::cost::ENTRY
    );
    assert_eq!(table.raw_len(&runtime).unwrap(), 1);
    assert_eq!(table.raw_get::<_, i32>(&mut runtime, 1).unwrap(), 11);
    table.raw_set(&mut runtime, 2, 20).unwrap();
    table.raw_set(&mut runtime, false, 30).unwrap();
    let (key, value) = table.next(&mut runtime, None).unwrap().unwrap();
    assert!(matches!(key, Value::Integer(1)));
    assert!(matches!(value, Value::Integer(11)));
    table.raw_set(&mut runtime, 1, ()).unwrap();
    assert!(matches!(
        table.next(&mut runtime, Some(&key)).unwrap().unwrap().0,
        Value::Integer(2)
    ));
    assert!(
        table
            .raw_get::<_, Option<i32>>(&mut runtime, 1)
            .unwrap()
            .is_none()
    );
    assert!(table.raw_set(&mut runtime, (), 1).is_err());
    assert!(table.raw_get::<_, Value>(&mut runtime, f64::NAN).is_err());
    assert!(table.next(&mut runtime, Some(&Value::Nil)).is_err());
    assert!(table.next(&mut runtime, Some(&Value::Integer(99))).is_err());
    let key = runtime.create_string(b"key").unwrap();
    table.raw_set(&mut runtime, &key, "value").unwrap();
    assert_eq!(
        table.raw_get::<_, String>(&mut runtime, &key).unwrap(),
        "value"
    );
    for index in 10..100 {
        table.raw_set(&mut runtime, index, index).unwrap();
    }
    for index in 10..100 {
        table.raw_set(&mut runtime, index, ()).unwrap();
    }
    table.raw_set(&mut runtime, 100, 1).unwrap();
    crate::gc::check_usage(runtime.heap()).unwrap();
}

#[test]
fn raw_metatables_bypass_protection_and_never_run_metamethods() {
    let mut runtime = Runtime::builder()
        .libraries(Libraries::BASE)
        .build()
        .unwrap();
    let chunk = crate::compile(b"t = setmetatable({}, {__metatable = false, __index = function() error('called') end, __newindex = function() error('called') end, __len = function() return 99 end})").unwrap();
    runtime.load_main(&chunk).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let globals = runtime.globals();
    let table = globals.raw_get::<_, Table>(&mut runtime, "t").unwrap();
    assert!(table.metatable(&mut runtime).unwrap().is_some());
    table.raw_set(&mut runtime, 1, 5).unwrap();
    assert_eq!(table.raw_len(&runtime).unwrap(), 1);
    assert!(
        table
            .raw_get::<_, Option<Value>>(&mut runtime, "absent")
            .unwrap()
            .is_none()
    );
    table.set_metatable(&mut runtime, None).unwrap();
    assert!(table.metatable(&mut runtime).unwrap().is_none());
    crate::gc::check_usage(runtime.heap()).unwrap();
}

#[test]
fn quota_and_string_limit_failures_are_lua_memory_errors() {
    let mut runtime = Runtime::builder()
        .config(Config {
            auto_gc: false,
            max_string_bytes: 1024,
            ..Config::default()
        })
        .build()
        .unwrap();
    let error = runtime.create_string(vec![0; 1025]).unwrap_err();
    assert!(
        matches!(&error, Error::Lua(error) if error.class == LuaFault::Memory && matches!(error.value, Value::String(_)))
    );
    assert_eq!(error.to_string(), LuaFault::Memory.text());
    let table = runtime.create_table().unwrap();
    let before = runtime.memory().logical_bytes;
    runtime.heap_mut().gc.quota = before;
    assert!(
        matches!(table.raw_set(&mut runtime, 1, 1), Err(Error::Lua(error)) if error.class == LuaFault::Memory)
    );
    assert_eq!(runtime.memory().logical_bytes, before);
    assert!(
        matches!(runtime.create_table(), Err(Error::Lua(error)) if error.class == LuaFault::Memory)
    );
    assert!(
        matches!(runtime.create_string(b"x"), Err(Error::Lua(error)) if error.class == LuaFault::Memory)
    );
    crate::gc::check_usage(runtime.heap()).unwrap();
    assert!(
        matches!(Runtime::builder().limits(crate::Limits { max_logical_heap: 1, ..crate::Limits::default() }).build(), Err(Error::Lua(error)) if error.class == LuaFault::Memory)
    );
}

#[test]
fn function_kinds_thread_state_and_owned_wrappers_round_trip() {
    let mut runtime = Runtime::builder()
        .registry(HostRegistry::proof())
        .libraries(Libraries::STANDARD)
        .build()
        .unwrap();
    runtime.set_global_native("add", "add").unwrap();
    let chunk = crate::compile(
        b"f = function() end; co = coroutine.create(f); iterator = string.gmatch('a', '.')",
    )
    .unwrap();
    runtime.load_main(&chunk).unwrap();
    let main = Thread::from_lua(
        runtime.object(runtime.entry_id().unwrap()).unwrap(),
        &mut runtime,
    )
    .unwrap();
    assert_eq!(main.status(&runtime).unwrap(), ThreadStatus::Ready);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(main.status(&runtime).unwrap(), ThreadStatus::Completed);
    let globals = runtime.globals();
    for (name, kind) in [
        ("f", FunctionKind::Lua),
        ("add", FunctionKind::Native),
        ("pcall", FunctionKind::Builtin),
        ("iterator", FunctionKind::NativeClosure),
    ] {
        let function = globals.raw_get::<_, Function>(&mut runtime, name).unwrap();
        assert_eq!(function.kind(&runtime).unwrap(), kind);
        assert_eq!(
            function.id().is_some(),
            matches!(kind, FunctionKind::Lua | FunctionKind::NativeClosure)
        );
        let value = function.clone().into_lua(&mut runtime).unwrap();
        assert_eq!(
            Function::from_lua(value, &mut runtime)
                .unwrap()
                .kind(&runtime)
                .unwrap(),
            kind
        );
        wrong(function.kind(&idle(GcMode::Incremental)));
    }
    let thread = globals.raw_get::<_, Thread>(&mut runtime, "co").unwrap();
    assert_eq!(thread.status(&runtime).unwrap(), ThreadStatus::Suspended);
    wrong(thread.status(&idle(GcMode::Incremental)));
    assert_eq!(
        Thread::from_lua(thread.clone().into_lua(&mut runtime).unwrap(), &mut runtime)
            .unwrap()
            .id(),
        thread.id()
    );
    let light = crate::LightUserdata::host(crate::HostLightKey(9));
    let value = crate::LightUserdata::into_lua(light, &mut runtime).unwrap();
    assert_eq!(
        crate::LightUserdata::from_lua(value, &mut runtime).unwrap(),
        light
    );
}

#[test]
fn typed_userdata_borrows_check_owner_type_and_charge_growth() {
    let mut runtime = Runtime::builder()
        .registry(HostRegistry::proof())
        .build()
        .unwrap();
    runtime
        .set_global_native("counter_new", "counter_new")
        .unwrap();
    runtime
        .load_main(&crate::compile(b"return counter_new(4)").unwrap())
        .unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let value = runtime.result_values().unwrap().remove(0);
    let userdata = AnyUserData::from_lua(value, &mut runtime).unwrap();
    assert_eq!(
        userdata
            .borrow::<crate::ProofCounter>(&runtime)
            .unwrap()
            .count,
        4
    );
    assert!(matches!(
        userdata.borrow::<crate::ProofHandle>(&runtime),
        Err(Error::Api(ApiError::WrongType))
    ));
    wrong(userdata.borrow::<crate::ProofCounter>(&idle(GcMode::Incremental)));
    let before = runtime.memory().logical_bytes;
    {
        let mut value = userdata
            .borrow_mut::<crate::ProofCounter>(&mut runtime)
            .unwrap();
        value.count = 8;
        value.size += 10;
    }
    assert_eq!(runtime.memory().logical_bytes, before + 10);
    crate::gc::check_usage(runtime.heap()).unwrap();
    {
        let mut value = userdata
            .borrow_mut::<crate::ProofCounter>(&mut runtime)
            .unwrap();
        value.size -= 10;
    }
    assert_eq!(runtime.memory().logical_bytes, before);
    let mut other = idle(GcMode::Incremental);
    wrong(userdata.borrow_mut::<crate::ProofCounter>(&mut other));
    assert!(matches!(
        userdata.borrow_mut::<crate::ProofHandle>(&mut runtime),
        Err(Error::Api(ApiError::WrongType))
    ));
}

#[test]
fn builder_selects_libraries_and_load_reuses_the_main_thread() {
    let mut runtime = Runtime::builder()
        .libraries(Libraries::MATH | Libraries::TABLE)
        .build()
        .unwrap();
    let globals = runtime.globals();
    assert!(
        globals
            .raw_get::<_, Option<Table>>(&mut runtime, "math")
            .unwrap()
            .is_some()
    );
    assert!(
        globals
            .raw_get::<_, Option<Value>>(&mut runtime, "print")
            .unwrap()
            .is_none()
    );
    let id = runtime.entry_id().unwrap();
    let chunk = crate::compile(b"return 1, nil, 3").unwrap();
    runtime.load_main(&chunk).unwrap();
    assert!(matches!(
        runtime.load_main(&chunk),
        Err(Error::Api(ApiError::Busy))
    ));
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(runtime.result_values().unwrap().len(), 3);
    let second = crate::compile(b"return nil").unwrap();
    runtime.load_main(&second).unwrap();
    assert_eq!(runtime.entry_id().unwrap(), id);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(runtime.result_values().unwrap().len(), 1);
    runtime.load_main(&crate::compile(b"").unwrap()).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(runtime.result_values().unwrap().is_empty());
    crate::gc::check_usage(runtime.heap()).unwrap();
}

#[test]
fn builder_capabilities_and_all_libraries_work_together() {
    let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::<u8>::new()));
    let output = written.clone();
    let warnings = written.clone();
    let mut runtime = Runtime::builder()
        .libraries(Libraries::ALL)
        .output(move |bytes| output.borrow_mut().extend(bytes))
        .warnings(move |bytes, _| warnings.borrow_mut().extend(bytes))
        .entropy(|| 7)
        .build()
        .unwrap();
    runtime.load_main(&crate::compile(b"print('output'); warn('warning'); math.randomseed(); return debug, package, coroutine, string, table, math").unwrap()).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(&*written.borrow(), b"output\nwarning");
    assert_eq!(runtime.result_values().unwrap().len(), 6);
    crate::gc::check_usage(runtime.heap()).unwrap();
}

#[test]
fn debug_shows_logical_identity_and_never_arena_details() {
    let mut runtime = idle(GcMode::Incremental);
    let table = runtime.create_table().unwrap();
    let value = Value::Table(table.clone());
    for text in [
        format!("{table:?}"),
        format!("{value:?}"),
        format!("{:?}", runtime.object_ref(table.id()).unwrap()),
    ] {
        assert!(text.contains("ObjectId"));
        for hidden in ["Handle", "index", "generation", "mark", "age", "OwnerToken"] {
            assert!(!text.contains(hidden), "{text}");
        }
    }
    let root = runtime.root_id(table.id()).unwrap();
    let native = crate::host::NativeValue::wrap(value.raw(&runtime).unwrap());
    for text in [format!("{root:?}"), format!("{native:?}")] {
        for hidden in ["Handle", "index", "generation", "mark", "age", "OwnerToken"] {
            assert!(!text.contains(hidden), "{text}");
        }
    }
    runtime.release_root(root).unwrap();
}

#[test]
fn lua_errors_keep_arbitrary_objects_and_display_their_original_message() {
    let mut runtime = idle(GcMode::Incremental);
    let table = runtime.create_table().unwrap();
    let id = table.id();
    let error = LuaError::new(Value::Table(table), LuaFault::Error, &runtime).unwrap();
    assert_eq!(error.to_string(), "error object is a table value");
    runtime.collect();
    assert!(runtime.contains_id(id));
    drop(error);
    runtime.collect();
    assert!(!runtime.contains_id(id));
    let string = runtime.create_string(b"message").unwrap();
    let error = LuaError::new(Value::String(string), LuaFault::Error, &runtime).unwrap();
    assert_eq!(error.to_string(), "message");
}

fn execution_registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    registry.typed(
        "sum",
        crate::NativePolicy::VmLocal,
        |_, (a, b): (i64, i64)| Ok(a + b),
    );
    registry.function("await", crate::NativePolicy::VmLocal, |cx| {
        Ok(crate::NativeReturn::Wait(crate::WaitRequest {
            operation: "fetch".into(),
            payload: MultiValue(vec![cx.arg(0).to_owned_value()?]),
        }))
    });
    registry.function("capture", crate::NativePolicy::VmLocal, |cx| {
        let value = cx.captures()[0].as_integer().unwrap();
        cx.set_capture(0, value + 1)?;
        cx.return_values(value)
    });
    registry
}

fn execution_runtime(registry: HostRegistry, source: &str) -> Runtime {
    let mut runtime = Runtime::builder()
        .registry(registry)
        .libraries(Libraries::STANDARD)
        .build()
        .unwrap();
    for name in ["sum", "await", "capture", "bridge"] {
        runtime.set_global_native(name, name).unwrap_or(());
    }
    runtime
        .load_main(&crate::compile(source.as_bytes()).unwrap())
        .unwrap();
    runtime
}

fn execution_restore(runtime: &Runtime) -> Runtime {
    Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        runtime.registry(),
        runtime.effect_domain(),
    )
    .unwrap()
}

#[test]
fn typed_native_argument_errors_use_luas_wording() {
    let mut runtime = execution_runtime(execution_registry(), "return pcall(sum, 1, 'two')");
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    let result = runtime.result_values().unwrap();
    assert!(matches!(result[0], Value::Boolean(false)));
    let Value::String(message) = &result[1] else {
        panic!("{result:?}");
    };
    assert_eq!(
        message.as_bytes(&runtime).unwrap(),
        b"bad argument #2 to 'sum' (number expected, got string)"
    );
}

#[test]
fn host_argument_errors_share_builtin_call_site_wording() {
    fn legacy(call: &mut crate::NativeCall<'_>) -> crate::NativeOutcome {
        call.type_error(0, "number")
    }
    let mut registry = execution_registry();
    registry.register_native("legacy", crate::NativePolicy::VmLocal, legacy);
    let source = "local function probe(f) local ok,e=pcall(function() local _=f('x') end); return e end; return probe(math.abs), probe(sum), probe(legacy)";
    let mut runtime = execution_runtime(registry, source);
    runtime.set_global_native("legacy", "legacy").unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let results = runtime.result_values().unwrap();
    let texts: Vec<_> = results
        .iter()
        .map(|value| match value {
            Value::String(message) => message.as_bytes(&runtime).unwrap().to_vec(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(texts[0], texts[1]);
    assert_eq!(texts[0], texts[2]);
    assert!(texts[0].ends_with(b"bad argument #1 to 'f' (number expected, got string)"));
}

#[test]
fn native_closure_capture_writes_are_traced_and_snapshotted() {
    let mut runtime = execution_runtime(execution_registry(), "return 0");
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let function = runtime.make_closure("capture", (41,)).unwrap();
    let id = function.id().unwrap();
    let main = runtime.entry_id().unwrap();
    assert!(matches!(
        runtime
            .call::<i64>(&function, (), &mut Journal::new(), 100)
            .unwrap(),
        crate::CallOutcome::Done(41)
    ));
    runtime.collect();
    let mut restored = execution_restore(&runtime);
    let function = Function::from_lua(restored.object(id).unwrap(), &mut restored).unwrap();
    assert!(matches!(
        restored
            .call::<i64>(&function, (), &mut Journal::new(), 100)
            .unwrap(),
        crate::CallOutcome::Done(42)
    ));
    assert_eq!(restored.entry_id().unwrap(), main);
    assert_eq!(restored.heap().threads.live(), 1);
    assert!(matches!(
        restored.make_closure("capture", Variadic(vec![Value::Nil; 256])),
        Err(Error::Api(ApiError::InvalidCallState))
    ));
}

fn bridge_registry(
    counts: std::rc::Rc<std::cell::Cell<(u32, u32)>>,
    policy: crate::NativePolicy,
) -> HostRegistry {
    let mut registry = execution_registry();
    registry.function("bridge", policy, move |cx| {
        let (starts, resumes) = counts.get();
        if let Some(resume) = cx.resumed() {
            counts.set((starts, resumes + 1));
            assert_eq!(resume.tag, u32::MAX);
            assert!(matches!(resume.kept[0], Value::Integer(70)));
            match resume.outcome {
                crate::ResumeOutcome::Returned(values) => Ok(crate::NativeReturn::Return(values)),
                crate::ResumeOutcome::Errored(error) => Ok(crate::NativeReturn::Error(error.value)),
            }
        } else {
            counts.set((starts + 1, resumes));
            Ok(crate::NativeReturn::CallLua {
                function: cx.arg(0).to_owned_value()?,
                args: MultiValue(vec![cx.arg(1).to_owned_value()?]),
                tag: u32::MAX,
                keep: MultiValue(vec![Value::Integer(70)]),
            })
        }
    });
    registry
}

#[test]
fn continuation_yields_waits_and_restores_at_every_quantum_once() {
    let source = "local co = coroutine.create(function()
        return bridge(function(x) coroutine.yield(3) local y = await('payload') return x+y, nil, 9 end, 4)
        end)
        local ok, x = coroutine.resume(co) assert(ok and x == 3)
        return coroutine.resume(co)";
    for policy in [crate::NativePolicy::VmLocal, crate::NativePolicy::External] {
        let mut fuels = Vec::new();
        for checkpoint in [false, true] {
            let counts = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
            let mut runtime = execution_runtime(bridge_registry(counts.clone(), policy), source);
            let mut journal = Journal::new();
            loop {
                let outcome = runtime
                    .run(if checkpoint { 1 } else { u64::MAX }, &mut journal)
                    .unwrap();
                crate::gc::check_invariant(runtime.heap()).unwrap();
                crate::gc::check_gen_invariant(runtime.heap()).unwrap();
                if checkpoint {
                    runtime = execution_restore(&runtime);
                }
                match outcome {
                    StepOutcome::Paused(_) => {}
                    StepOutcome::Waiting(key) => {
                        let wait = runtime.wait(key).unwrap();
                        assert_eq!(wait.operation, "fetch");
                        assert_eq!(
                            String::from_lua(wait.payload[0].clone(), &mut runtime).unwrap(),
                            "payload"
                        );
                        runtime
                            .complete(key, crate::Completion::Return(vec![Value::Integer(8)]))
                            .unwrap();
                        if checkpoint {
                            runtime = execution_restore(&runtime);
                        }
                    }
                    StepOutcome::Completed => break,
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(counts.get(), (1, 1));
            assert_eq!(
                <(bool, i64, Option<i64>, i64)>::from_lua_multi(
                    runtime.result_values().unwrap(),
                    &mut runtime
                )
                .unwrap(),
                (true, 12, None, 9)
            );
            fuels.push(runtime.fuel_consumed());
        }
        assert_eq!(fuels[0], fuels[1]);
    }
}

#[test]
fn continuation_catches_the_called_lua_error_object() {
    let counts = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
    let mut runtime = execution_runtime(
        bridge_registry(counts.clone(), crate::NativePolicy::VmLocal),
        "local e = {} local ok, result = pcall(bridge, function() error(e) end) return ok, result == e",
    );
    let mut journal = Journal::new();
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        runtime = execution_restore(&runtime);
        if outcome == StepOutcome::Completed {
            break;
        }
        assert!(matches!(outcome, StepOutcome::Paused(_)), "{outcome:?}");
    }
    assert_eq!(counts.get(), (1, 1));
    assert_eq!(
        <(bool, bool)>::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap(),
        (false, true)
    );
}

#[test]
fn continuation_calls_native_and_callable_values_without_rust_recursion() {
    let counts = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
    let mut runtime = execution_runtime(
        bridge_registry(counts.clone(), crate::NativePolicy::VmLocal),
        "local c = setmetatable({}, {__call=function(_, x) return x+2 end}) return bridge(c, 7), bridge(capture)",
    );
    // Use a closure as the native called by the second continuation.
    let native = runtime.make_closure("capture", (40,)).unwrap();
    runtime
        .globals()
        .raw_set(&mut runtime, "capture", native)
        .unwrap();
    let mut journal = Journal::new();
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        runtime = execution_restore(&runtime);
        if outcome == StepOutcome::Completed {
            break;
        }
        assert!(matches!(outcome, StepOutcome::Paused(_)), "{outcome:?}");
    }
    assert_eq!(counts.get(), (2, 2));
    assert_eq!(
        <(i64, i64)>::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap(),
        (9, 40)
    );
}

#[test]
fn main_calls_have_typed_values_busy_errors_and_no_thread_allocation() {
    let mut runtime = execution_runtime(
        execution_registry(),
        "return function(a,b) return a+b,nil,'ok' end, function() error({}) end",
    );
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let (function, error): (Function, Function) =
        FromLuaMulti::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap();
    runtime.start_call(&function, (2, 5)).unwrap();
    assert!(matches!(
        runtime.start_call(&function, ()),
        Err(Error::Api(ApiError::Busy))
    ));
    assert!(matches!(
        runtime.load_main(&crate::compile(b"return 0").unwrap()),
        Err(Error::Api(ApiError::Busy))
    ));
    assert!(matches!(
        runtime.finish_call::<()>(),
        Err(Error::Api(ApiError::InvalidCallState))
    ));
    let mut journal = Journal::new();
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        runtime = execution_restore(&runtime);
        if outcome == StepOutcome::Completed {
            break;
        }
    }
    assert_eq!(
        runtime.finish_call::<(i64, Option<i64>, String)>().unwrap(),
        (7, None, "ok".into())
    );
    let error =
        Function::from_lua(runtime.object(error.id().unwrap()).unwrap(), &mut runtime).unwrap();
    let Err(Error::Lua(error)) = runtime.call::<()>(&error, (), &mut journal, 100) else {
        panic!("Lua error");
    };
    assert_eq!(error.value.lua_type(), crate::LuaType::Table);
    assert_eq!(runtime.heap().threads.live(), 1);
    assert!(matches!(
        runtime.finish_call::<()>(),
        Err(Error::Api(ApiError::InvalidCallState))
    ));
}

#[test]
fn host_wait_completion_keys_and_main_call_survive_restore() {
    let mut runtime = execution_runtime(
        execution_registry(),
        "return function(x) return await(x) end",
    );
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let function: Function =
        FromLuaMulti::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap();
    let crate::CallOutcome::Waiting(key) = runtime
        .call::<MultiValue>(&function, (11,), &mut journal, 100)
        .unwrap()
    else {
        panic!("waiting");
    };
    runtime = execution_restore(&runtime);
    assert!(matches!(
        runtime.wait(key).unwrap().payload[0],
        Value::Integer(11)
    ));
    assert!(matches!(
        runtime.complete(crate::WaitKey(12345), crate::Completion::Return(vec![])),
        Err(Error::Api(ApiError::NotWaiting))
    ));
    let mut foreign = idle(GcMode::Incremental);
    let foreign = foreign.create_table().unwrap();
    wrong(runtime.complete(key, crate::Completion::Return(vec![Value::Table(foreign)])));
    assert!(runtime.wait(key).is_some());
    runtime
        .complete(
            key,
            crate::Completion::Return(vec![Value::Integer(5), Value::Nil]),
        )
        .unwrap();
    assert!(runtime.wait(key).is_none());
    runtime.run_until_terminal(100, &mut journal).unwrap();
    let result = runtime.finish_call::<MultiValue>().unwrap();
    assert_eq!(result.len(), 2);
    assert!(matches!(result[1], Value::Nil));
    let function = Function::from_lua(
        runtime.object(function.id().unwrap()).unwrap(),
        &mut runtime,
    )
    .unwrap();
    let crate::CallOutcome::Waiting(second) = runtime
        .call::<()>(&function, (), &mut journal, 100)
        .unwrap()
    else {
        panic!("waiting");
    };
    assert_ne!(key, second);
    runtime = execution_restore(&runtime);
    assert!(matches!(
        runtime.complete(key, crate::Completion::Return(vec![])),
        Err(Error::Api(ApiError::AlreadyCompleted))
    ));
    let error = runtime.create_table().unwrap();
    let id = error.id();
    runtime
        .complete(second, crate::Completion::Error(Value::Table(error)))
        .unwrap();
    assert!(matches!(
        runtime.complete(second, crate::Completion::Return(vec![])),
        Err(Error::Api(ApiError::AlreadyCompleted))
    ));
    runtime = execution_restore(&runtime);
    assert!(matches!(
        runtime.run_until_terminal(100, &mut journal).unwrap(),
        StepOutcome::LuaError(_)
    ));
    let Err(Error::Lua(error)) = runtime.finish_call::<()>() else {
        panic!("Lua error");
    };
    assert_eq!(error.value.id(), Some(id));
    assert!(matches!(
        runtime.complete(second, crate::Completion::Return(vec![])),
        Err(Error::Api(ApiError::AlreadyCompleted))
    ));
}

#[test]
fn semantic_table_calls_use_the_main_call_wait_protocol() {
    let mut runtime = execution_runtime(
        execution_registry(),
        "return setmetatable({}, {
        __index=function(_,k) return await(k) end,
        __newindex=function(t,k,v) rawset(t,k,v+1) end})",
    );
    let mut journal = Journal::new();
    runtime.run_until_terminal(1000, &mut journal).unwrap();
    let table: Table =
        FromLuaMulti::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap();
    let crate::CallOutcome::Waiting(key) = table
        .get::<_, i64>(&mut runtime, "missing", &mut journal, 100)
        .unwrap()
    else {
        panic!("waiting");
    };
    runtime = execution_restore(&runtime);
    runtime
        .complete(key, crate::Completion::Return(vec![Value::Integer(6)]))
        .unwrap();
    runtime.run_until_terminal(100, &mut journal).unwrap();
    assert_eq!(runtime.finish_call::<i64>().unwrap(), 6);
    let table = Table::from_lua(runtime.object(table.id()).unwrap(), &mut runtime).unwrap();
    assert!(matches!(
        table.set(&mut runtime, "x", 20, &mut journal, 100).unwrap(),
        crate::CallOutcome::Done(())
    ));
    assert_eq!(table.raw_get::<_, i64>(&mut runtime, "x").unwrap(), 21);
}

#[test]
fn restore_refuses_impossible_native_continuation_frames() {
    let counts = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
    let mut runtime = execution_runtime(
        bridge_registry(counts, crate::NativePolicy::VmLocal),
        "return bridge(function() return await() end)",
    );
    runtime
        .run_until_terminal(1000, &mut Journal::new())
        .unwrap();
    let image = runtime.to_image().unwrap();
    for case in 0..4 {
        let mut bad = image.clone();
        let frame = bad
            .threads
            .iter_mut()
            .flat_map(|t| t.frames.iter_mut())
            .find(|f| {
                matches!(
                    f.boundary,
                    Some(crate::snapshot::BoundaryImage::Native { .. })
                )
            })
            .unwrap();
        let Some(crate::snapshot::BoundaryImage::Native {
            symbol,
            kept,
            resuming,
            error,
            ..
        }) = frame.boundary.as_mut()
        else {
            unreachable!()
        };
        match case {
            0 => *symbol = u32::MAX,
            1 => *kept = u32::MAX,
            2 => *resuming = true,
            _ => *error = Some((255, crate::snapshot::EncValue::Nil)),
        }
        let bytes = crate::snapshot::encode(&bad).unwrap();
        assert!(
            Runtime::from_snapshot(&bytes, runtime.registry(), runtime.effect_domain()).is_err()
        );
    }
    let bytes = runtime.snapshot().unwrap();
    assert!(matches!(
        Runtime::from_snapshot(&bytes, &execution_registry(), runtime.effect_domain()),
        Err(crate::SnapshotError::UnknownHostSymbol)
    ));
}

#[test]
fn external_native_metamethod_continuations_restore_their_call_phase() {
    let mut registry = execution_registry();
    registry.function("meta_bridge", crate::NativePolicy::External, |cx| {
        if let Some(resume) = cx.resumed() {
            match resume.outcome {
                crate::ResumeOutcome::Returned(values) => Ok(crate::NativeReturn::Return(values)),
                crate::ResumeOutcome::Errored(error) => Ok(crate::NativeReturn::Error(error.value)),
            }
        } else {
            Ok(crate::NativeReturn::CallLua {
                function: cx.captures()[0].to_owned_value()?,
                args: MultiValue(vec![cx.arg(1).to_owned_value()?]),
                tag: 42,
                keep: MultiValue::new(),
            })
        }
    });
    registry.function("make_bridge", crate::NativePolicy::VmLocal, |cx| {
        let capture = cx.arg(0).to_owned_value()?;
        let function = cx.make_closure("meta_bridge", capture)?;
        cx.return_values(function)
    });
    let mut runtime = execution_runtime(
        registry,
        "local t = setmetatable({}, {__index = make_bridge(function(k) return await(k) end)}) return t.x",
    );
    runtime
        .set_global_native("make_bridge", "make_bridge")
        .unwrap();
    let mut journal = Journal::new();
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        runtime = execution_restore(&runtime);
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Waiting(key) => runtime
                .complete(key, crate::Completion::Return(vec![Value::Integer(14)]))
                .unwrap(),
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        i64::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap(),
        14
    );
}

#[test]
fn native_capture_object_writes_use_both_collector_barriers() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let mut registry = execution_registry();
        registry.function("replace", crate::NativePolicy::VmLocal, |cx| {
            let previous = cx.captures()[0].to_owned_value()?;
            let new = cx.create_string(b"young capture")?;
            cx.set_capture(0, new)?;
            cx.return_values(previous)
        });
        let mut runtime = Runtime::builder()
            .registry(registry)
            .config(Config {
                gc_mode: mode,
                ..Config::default()
            })
            .build()
            .unwrap();
        let first = runtime.create_string(b"first").unwrap();
        let function = runtime.make_closure("replace", first).unwrap();
        for _ in 0..3 {
            runtime.collect();
        }
        assert!(
            matches!(runtime.call::<String>(&function, (), &mut Journal::new(), 1000).unwrap(), crate::CallOutcome::Done(text) if text == "first")
        );
        runtime.collect();
        crate::gc::check_invariant(runtime.heap()).unwrap();
        crate::gc::check_gen_invariant(runtime.heap()).unwrap();
        let mut restored = execution_restore(&runtime);
        let function = Function::from_lua(
            restored.object(function.id().unwrap()).unwrap(),
            &mut restored,
        )
        .unwrap();
        assert!(
            matches!(restored.call::<String>(&function, (), &mut Journal::new(), 1000).unwrap(), crate::CallOutcome::Done(text) if text == "young capture")
        );
    }
}

#[test]
fn convenience_calls_pause_for_fuel_and_reject_a_running_chunk() {
    let mut runtime = execution_runtime(execution_registry(), "return 0");
    let function = runtime.make_closure("capture", 10).unwrap();
    assert!(matches!(
        runtime.start_call(&function, ()),
        Err(Error::Api(ApiError::Busy))
    ));
    let mut journal = Journal::new();
    runtime.run_until_terminal(100, &mut journal).unwrap();
    assert!(matches!(
        runtime.call::<i64>(&function, (), &mut journal, 0).unwrap(),
        crate::CallOutcome::OutOfFuel
    ));
    runtime = execution_restore(&runtime);
    runtime.run_until_terminal(100, &mut journal).unwrap();
    assert_eq!(runtime.finish_call::<i64>().unwrap(), 10);
}

#[test]
fn custom_callback_conversions_cannot_reenter_or_checkpoint_the_executor() {
    struct Reenter;
    impl FromLua for Reenter {
        fn from_lua(_: Value, runtime: &mut Runtime) -> crate::Result<Self> {
            assert_eq!(
                runtime.run(1, &mut Journal::new()),
                Err(VmError::Api(ApiError::InvalidCallState))
            );
            assert!(runtime.snapshot().is_err());
            Ok(Self)
        }
    }
    let mut registry = execution_registry();
    registry.typed(
        "reenter",
        crate::NativePolicy::VmLocal,
        |_, (_arg,): (Reenter,)| Ok(17),
    );
    let mut runtime = execution_runtime(registry, "return reenter(1)");
    runtime.set_global_native("reenter", "reenter").unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(100, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        runtime.result_values().unwrap()[0]
            .as_ref(&runtime)
            .unwrap()
            .as_integer(),
        Some(17)
    );
}

#[test]
fn native_argument_max_index_returns_conversion_error_without_poisoning() {
    let mut registry = execution_registry();
    registry.function("missing", crate::NativePolicy::VmLocal, |cx| {
        for (index, position) in [
            (0, Some(1)),
            (usize::MAX - 1, Some(usize::MAX)),
            (usize::MAX, None),
        ] {
            let Err(Error::Api(ApiError::Conversion(error))) = cx.argument::<i64>(index) else {
                panic!("expected conversion failure");
            };
            assert_eq!(error.position, position);
            assert_eq!(error.actual, crate::LuaType::Nil);
        }
        assert_eq!(cx.argument::<Option<i64>>(usize::MAX)?, None);
        cx.return_values(17)
    });
    let mut runtime = execution_runtime(registry, "return missing()");
    runtime.set_global_native("missing", "missing").unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(100, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert!(runtime.snapshot().is_ok());
    assert_eq!(
        runtime.result_values().unwrap()[0]
            .as_ref(&runtime)
            .unwrap()
            .as_integer(),
        Some(17)
    );
}

#[test]
fn callback_api_errors_never_become_lua_results_or_reinvoke_after_restore() {
    let counts = std::rc::Rc::new(std::cell::Cell::new(0));
    let held = counts.clone();
    let mut registry = execution_registry();
    registry.function("misuse", crate::NativePolicy::VmLocal, move |_| {
        held.set(held.get() + 1);
        Err(ApiError::UnknownSymbol.into())
    });
    let mut runtime = execution_runtime(registry, "return pcall(misuse)");
    runtime.set_global_native("misuse", "misuse").unwrap();
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run(100, &mut journal),
        Err(VmError::Api(ApiError::UnknownSymbol))
    );
    runtime = execution_restore(&runtime);
    assert_eq!(
        runtime.run(100, &mut journal),
        Err(VmError::Api(ApiError::InvalidCallState))
    );
    assert_eq!(counts.get(), 1);
    assert!(matches!(
        runtime.result_values(),
        Err(Error::Api(ApiError::InvalidCallState))
    ));
}

#[test]
fn external_typed_native_journal_replays_the_same_effect_once() {
    let fresh = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = fresh.clone();
    let mut registry = execution_registry();
    registry.typed(
        "effect",
        crate::NativePolicy::External,
        move |cx, (arg,): (i64,)| {
            let id = cx.effect().unwrap();
            Ok(cx
                .journal()
                .unwrap()
                .commit(id, arg, || {
                    count.set(count.get() + 1);
                    arg + 1
                })
                .unwrap())
        },
    );
    let mut runtime = execution_runtime(registry, "return effect(8)");
    runtime.set_global_native("effect", "effect").unwrap();
    let mut journal = Journal::new();
    while runtime.at_prepared().is_none() {
        runtime.run(1, &mut journal).unwrap();
    }
    let prepared = runtime.snapshot().unwrap();
    for _ in 0..2 {
        runtime =
            Runtime::from_snapshot(&prepared, runtime.registry(), runtime.effect_domain()).unwrap();
        assert_eq!(
            runtime.run_until_terminal(100, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(
            i64::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap(),
            9
        );
    }
    assert_eq!(fresh.get(), 1);
    assert_eq!(journal.entries().len(), 1);
}

#[test]
fn legacy_wait_keys_keep_all_bits_and_remain_reusable() {
    fn wait(_: &mut crate::NativeCall<'_>) -> crate::NativeOutcome {
        crate::NativeOutcome::Pending(crate::WaitKey((1 << 63) | 2000))
    }
    let mut registry = execution_registry();
    registry.register_native("legacy_wait", crate::NativePolicy::VmLocal, wait);
    let mut runtime = execution_runtime(registry, "return legacy_wait(), legacy_wait()");
    runtime
        .set_global_native("legacy_wait", "legacy_wait")
        .unwrap();
    let mut journal = Journal::new();
    let key = crate::WaitKey((1 << 63) | 2000);
    for value in [10, 11] {
        assert_eq!(
            runtime.run_until_terminal(1000, &mut journal).unwrap(),
            StepOutcome::Waiting(key)
        );
        runtime = execution_restore(&runtime);
        assert!(runtime.wait(key).is_none());
        runtime.complete_wait(key, value).unwrap();
        assert_eq!(
            runtime.complete_wait(key, value),
            Err(crate::WaitError::AlreadyCompleted)
        );
        runtime = execution_restore(&runtime);
    }
    assert_eq!(
        runtime.run_until_terminal(1000, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        <(i64, i64)>::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap(),
        (10, 11)
    );
}

#[test]
fn typed_native_object_results_keep_nil_holes_and_survive_collection() {
    let mut registry = execution_registry();
    registry.typed(
        "objects",
        crate::NativePolicy::VmLocal,
        |cx, (bytes,): (Vec<u8>,)| {
            let table = cx.create_table()?;
            let text = cx.create_string(&bytes)?;
            cx.raw_set(&table, "bytes", &text)?;
            Ok((table, Value::Nil, text))
        },
    );
    let mut runtime = execution_runtime(registry, "return objects('x\\0\\255')");
    runtime.set_global_native("objects", "objects").unwrap();
    runtime
        .run_until_terminal(1000, &mut Journal::new())
        .unwrap();
    runtime.collect();
    runtime = execution_restore(&runtime);
    let results = runtime.result_values().unwrap();
    assert_eq!(results.len(), 3);
    assert!(matches!(results[1], Value::Nil));
    let (table, _, bytes): (Table, Value, Vec<u8>) =
        FromLuaMulti::from_lua_multi(results, &mut runtime).unwrap();
    assert_eq!(bytes, b"x\0\xff");
    assert_eq!(
        table.raw_get::<_, Vec<u8>>(&mut runtime, "bytes").unwrap(),
        bytes
    );
    crate::gc::check_usage(runtime.heap()).unwrap();
}

#[test]
fn typed_results_stay_rooted_during_later_allocating_conversions() {
    struct Allocate(bool);
    impl IntoLua for Allocate {
        fn into_lua(self, runtime: &mut Runtime) -> crate::Result<Value> {
            let source = (0..37)
                .map(|i| format!("'constant{i}'"))
                .collect::<Vec<_>>()
                .join(",");
            let chunk = crate::compile(format!("return {source}").as_bytes()).unwrap();
            if self.0 {
                for _ in 0..200 {
                    let _ = runtime.load_function(&chunk);
                }
                Ok(Value::Nil)
            } else {
                let id = runtime.load_function(&chunk)?;
                Ok(runtime.object(id).unwrap())
            }
        }
    }
    for mode in [GcMode::Incremental, GcMode::Generational] {
        for churn in [false, true] {
            let mut registry = HostRegistry::new();
            registry.typed("objects", crate::NativePolicy::VmLocal, move |_, (): ()| {
                Ok(("hello", Allocate(churn)))
            });
            let mut runtime = Runtime::builder()
                .registry(registry)
                .config(Config {
                    max_objects: 120,
                    gc_mode: mode,
                    ..Config::default()
                })
                .build()
                .unwrap();
            let function = runtime.make_closure("objects", ()).unwrap();
            let crate::CallOutcome::Done(values) = runtime
                .call::<MultiValue>(&function, (), &mut Journal::new(), 100_000)
                .unwrap()
            else {
                panic!("call did not finish");
            };
            assert_eq!(values.len(), 2);
            assert_eq!(
                Vec::<u8>::from_lua(values[0].clone(), &mut runtime).unwrap(),
                b"hello"
            );
            runtime.collect();
            runtime = execution_restore(&runtime);
            assert_eq!(
                Vec::<u8>::from_lua(runtime.result_values().unwrap()[0].clone(), &mut runtime)
                    .unwrap(),
                b"hello"
            );
            crate::gc::check_usage(runtime.heap()).unwrap();
        }
    }
}

#[test]
fn callback_context_creates_values_and_reconciles_mutable_userdata_borrows() {
    let mut registry = execution_registry();
    registry.register_portable_userdata::<crate::ProofCounter>();
    registry.function("counter", crate::NativePolicy::VmLocal, |cx| {
        let Value::UserData(object) = cx.captures()[0].to_owned_value()? else {
            return Err(ApiError::WrongType.into());
        };
        let mut guard = cx.borrow_userdata_mut::<crate::ProofCounter>(&object)?;
        guard.count += 1;
        guard.size = 32;
        let count = guard.count;
        drop(guard);
        cx.return_values(count)
    });
    registry.function("make", crate::NativePolicy::VmLocal, |cx| {
        assert_eq!(cx.string_bytes(0), Some(&b"x\0\xff"[..]));
        let table = cx.create_table()?;
        let text = cx.create_string(b"x\0\xff")?;
        cx.raw_set(&table, 1, text)?;
        assert_eq!(cx.raw_len(&table)?, 1);
        let meta = cx.create_table()?;
        cx.set_metatable(&table, Some(&meta))?;
        assert_eq!(cx.metatable(&table)?.unwrap().id(), meta.id());
        let object = cx.create_userdata(
            crate::ProofCounter {
                count: 40,
                size: 16,
            },
            0,
        )?;
        assert_eq!(
            cx.borrow_userdata::<crate::ProofCounter>(&object)?.count,
            40
        );
        let function = cx.make_closure("counter", object)?;
        cx.raw_set(&table, "count", function)?;
        cx.return_values(table)
    });
    let mut runtime = execution_runtime(registry, "return make('x\\0\\255')");
    runtime.set_global_native("make", "make").unwrap();
    let mut journal = Journal::new();
    runtime.run_until_terminal(1000, &mut journal).unwrap();
    let table: Table =
        FromLuaMulti::from_lua_multi(runtime.result_values().unwrap(), &mut runtime).unwrap();
    assert_eq!(
        table.raw_get::<_, Vec<u8>>(&mut runtime, 1).unwrap(),
        b"x\0\xff"
    );
    let function: Function = table.raw_get(&mut runtime, "count").unwrap();
    assert!(matches!(
        runtime
            .call::<i64>(&function, (), &mut journal, 1000)
            .unwrap(),
        crate::CallOutcome::Done(41)
    ));
    runtime = execution_restore(&runtime);
    let function = Function::from_lua(
        runtime.object(function.id().unwrap()).unwrap(),
        &mut runtime,
    )
    .unwrap();
    assert!(matches!(
        runtime
            .call::<i64>(&function, (), &mut journal, 1000)
            .unwrap(),
        crate::CallOutcome::Done(42)
    ));
}

#[test]
fn reused_host_call_storage_survives_collection_and_continuation_restore() {
    let mut registry = execution_registry();
    registry.function("bridge", crate::NativePolicy::VmLocal, |cx| {
        if let Some(resume) = cx.resumed_ref() {
            assert_eq!(resume.tag, 23);
            let crate::ResumeOutcome::Returned(values) = &resume.outcome else {
                panic!("unexpected continuation error");
            };
            assert!(matches!(values[1], Value::Nil));
            let result = (values[0].clone(), Value::Nil, resume.kept[0].clone());
            return cx.return_values(result);
        }
        let function = cx.arg(0).to_owned_value()?;
        let table = cx.create_table()?;
        cx.raw_set(&table, "answer", 42)?;
        cx.call_lua(function, (table.clone(), Value::Nil), 23, table)
    });
    let mut runtime = execution_runtime(
        registry,
        "
        function entry()
          return bridge(function(t, hole) return t, hole end)
        end",
    );
    let mut journal = Journal::new();
    runtime.run_until_terminal(1000, &mut journal).unwrap();
    let mut fuels = Vec::new();
    for round in 0..4 {
        if round == 2 {
            // The weak trampoline must not keep otherwise dead Lua objects alive.
            runtime.collect();
        }
        let function: Function = runtime.globals().raw_get(&mut runtime, "entry").unwrap();
        let before = runtime.fuel_consumed();
        runtime.start_call(&function, ()).unwrap();
        drop(function);
        if round == 3 {
            loop {
                let object = runtime
                    .heap()
                    .threads
                    .get(runtime.heap().entry.unwrap())
                    .unwrap();
                if object.frames.iter().any(|frame| {
                    matches!(frame.boundary(), Some(crate::heap::Boundary::Native { .. }))
                }) {
                    break;
                }
                assert!(matches!(
                    runtime.run(1, &mut journal).unwrap(),
                    StepOutcome::Paused(_)
                ));
            }
            runtime.collect(); // The kept table exists only in the continuation's stack.
            runtime = execution_restore(&runtime);
        }
        runtime.run_until_terminal(1000, &mut journal).unwrap();
        let (first, hole, kept): (Table, Value, Table) = runtime.finish_call().unwrap();
        assert!(matches!(hole, Value::Nil));
        if round >= 2 {
            runtime.collect(); // Result roots outlive the recycled conversion storage.
        }
        assert_eq!(first.raw_get::<_, i64>(&mut runtime, "answer").unwrap(), 42);
        assert_eq!(kept.raw_get::<_, i64>(&mut runtime, "answer").unwrap(), 42);
        fuels.push(runtime.fuel_consumed() - before);
    }
    assert!(fuels.iter().all(|fuel| *fuel == fuels[0]));
}

#[test]
fn userdata_location_hints_reject_reuse_and_fall_back_after_restore() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let mut runtime = Runtime::builder()
            .config(Config {
                auto_gc: false,
                gc_mode: mode,
                ..Config::default()
            })
            .registry(HostRegistry::proof())
            .build()
            .unwrap();
        let make = |runtime: &mut Runtime, count| {
            runtime
                .create_host_userdata(crate::ProofCounter { count, size: 16 }, 0)
                .unwrap()
        };
        let handle = |runtime: &Runtime, value: &AnyUserData| {
            let crate::value::Value::Userdata(handle) = value.0.value(runtime.owner()).unwrap()
            else {
                panic!("userdata handle")
            };
            handle
        };
        let garbage = make(&mut runtime, 1);
        let stale = handle(&runtime, &garbage);
        let dead_id = garbage.id();
        let kept = make(&mut runtime, 2);
        let hint = handle(&runtime, &kept);
        drop(garbage);
        runtime.collect();
        let replacement = make(&mut runtime, 3);
        let reused = handle(&runtime, &replacement);
        assert_eq!(stale.index, reused.index);
        assert_ne!(stale.generation, reused.generation);
        assert!(
            runtime
                .heap()
                .userdata
                .find_id_hint(dead_id, stale)
                .is_none()
        );
        assert_eq!(
            runtime
                .heap()
                .userdata
                .find_id_hint(replacement.id(), stale),
            Some(reused)
        );
        // Matching index and generation still cannot substitute another identity.
        assert_eq!(
            runtime.heap().userdata.find_id_hint(kept.id(), reused),
            Some(hint)
        );
        drop(replacement);
        runtime.collect();
        let bytes = runtime.snapshot().unwrap();
        let mut restored =
            Runtime::from_snapshot(&bytes, runtime.registry(), runtime.effect_domain()).unwrap();
        wrong(kept.borrow::<crate::ProofCounter>(&restored));
        let owned = restored.object(kept.id()).unwrap();
        let restored_kept = AnyUserData::from_lua(owned, &mut restored).unwrap();
        let relocated = handle(&restored, &restored_kept);
        assert_ne!(hint, relocated);
        assert_eq!(
            restored.heap().userdata.find_id_hint(kept.id(), hint),
            Some(relocated)
        );
        assert!(
            restored
                .heap()
                .userdata
                .find_id_hint(dead_id, stale)
                .is_none()
        );
        assert_eq!(
            restored_kept
                .borrow::<crate::ProofCounter>(&restored)
                .unwrap()
                .count,
            2
        );
        restored_kept
            .borrow_mut::<crate::ProofCounter>(&mut restored)
            .unwrap()
            .count = 4;
        assert_eq!(
            restored_kept
                .borrow::<crate::ProofCounter>(&restored)
                .unwrap()
                .count,
            4
        );
    }
}
