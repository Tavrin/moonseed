//! Safe embedding misuse: exact categories, no panics, transactional restore.
use crate::snapshot::{BoundaryImage, EncValue, PayloadImage, PendingImage};
use crate::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn no_panic<T>(operation: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(operation)).expect("safe embedding operation panicked")
}
fn api_error<T>(result: crate::Result<T>, expected: ApiError) {
    match result {
        Err(Error::Api(actual)) => assert_eq!(actual, expected),
        _ => panic!("expected ApiError::{expected:?}"),
    }
}
fn snapshot_error(result: crate::Result<Runtime>, expected: SnapshotError) {
    match result {
        Err(Error::Vm(VmError::Snapshot(actual))) => assert_eq!(actual, expected),
        _ => panic!("expected snapshot error {expected:?}"),
    }
}
fn idle() -> Runtime {
    Runtime::builder().build().unwrap()
}
fn load(rt: &mut Runtime, source: &[u8]) {
    rt.load_main(&compile(source).unwrap()).unwrap();
}
fn function(rt: &mut Runtime) -> Function {
    load(rt, b"return function() return 42 end");
    assert_eq!(
        rt.run(100, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    Function::from_lua(rt.result_values().unwrap()[0].clone(), rt).unwrap()
}
fn waits() -> (Runtime, Function, Journal, WaitKey) {
    let mut registry = HostRegistry::new();
    registry.function("misuse.wait", NativePolicy::VmLocal, |_| {
        Ok(NativeReturn::Wait(WaitRequest {
            operation: "read".into(),
            payload: MultiValue::new(),
        }))
    });
    let mut rt = Runtime::builder().registry(registry).build().unwrap();
    let f = rt.make_closure("misuse.wait", ()).unwrap();
    let mut journal = Journal::new();
    let CallOutcome::Waiting(key) = rt.call::<()>(&f, (), &mut journal, 100).unwrap() else {
        panic!("expected wait");
    };
    (rt, f, journal, key)
}
struct Portable(i64);
impl HostUserdata for Portable {
    const SYMBOL: &'static str = "misuse.Portable";
    fn logical_size(&self) -> u64 {
        8
    }
}
impl PortableUserdata for Portable {
    fn encode(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(i64::from_le_bytes(bytes.try_into().ok()?)))
    }
}
struct Resource(Vec<u8>);
impl HostUserdata for Resource {
    const SYMBOL: &'static str = "misuse.Resource";
    fn logical_size(&self) -> u64 {
        self.0.len() as u64
    }
}
impl RebindUserdata for Resource {
    fn key(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn rebind(key: &[u8], env: &HostEnv) -> std::result::Result<Self, RebindError> {
        if !env.get::<bool>().copied().unwrap_or(false) {
            return Err(RebindError("resource missing"));
        }
        Ok(Self(key.to_vec()))
    }
}
fn resources(key: Vec<u8>) -> (Runtime, AnyUserData) {
    let mut registry = HostRegistry::new();
    registry.register_rebind_userdata::<Resource>();
    let mut rt = Runtime::builder().registry(registry).build().unwrap();
    let value = rt.create_host_userdata(Resource(key), 0).unwrap();
    (rt, value)
}
fn refused_image(rt: &Runtime, image: crate::snapshot::Image, expected: SnapshotError) {
    let before = rt.snapshot().unwrap();
    let bytes = crate::snapshot::encode(&image).unwrap();
    snapshot_error(
        no_panic(|| Runtime::restore(&bytes, &Host::new(rt.registry().clone()))),
        expected,
    );
    assert_eq!(rt.snapshot().unwrap(), before);
}

#[test]
fn wrong_runtime_root_is_an_api_error() {
    let mut rt = idle();
    let table = rt.create_table().unwrap();
    api_error(no_panic(|| table.raw_len(&idle())), ApiError::WrongRuntime);
    let snapshot = rt.snapshot().unwrap();
    let restored = Runtime::restore(&snapshot, &Host::default()).unwrap();
    api_error(
        no_panic(|| table.raw_len(&restored)),
        ApiError::WrongRuntime,
    );
}
#[test]
fn stale_id_cannot_name_a_reused_slot() {
    let mut rt = idle();
    let table = rt.create_table().unwrap();
    let id = table.id();
    let (_, old_slot, _) = rt.heap().find_by_id(id).unwrap();
    drop(table);
    rt.collect();
    assert!(no_panic(|| rt.object(id)).is_none());
    let replacement = rt.create_table().unwrap();
    let (_, new_slot, _) = rt.heap().find_by_id(replacement.id()).unwrap();
    assert_eq!(old_slot, new_slot, "test must exercise actual slot reuse");
    assert_ne!(id, replacement.id());
    assert!(no_panic(|| rt.object(id)).is_none());
}
#[test]
fn wrong_callback_argument_is_a_catchable_lua_argument_error() {
    let mut registry = HostRegistry::new();
    registry.typed(
        "misuse.sum",
        NativePolicy::VmLocal,
        |_cx, (a, b): (i64, i64)| Ok(a + b),
    );
    let mut rt = Runtime::builder()
        .registry(registry)
        .libraries(Libraries::BASE)
        .build()
        .unwrap();
    let f = rt.make_closure("misuse.sum", ()).unwrap();
    rt.globals().raw_set(&mut rt, "sum", &f).unwrap();
    let error = no_panic(|| rt.call::<()>(&f, (1, "two"), &mut Journal::new(), 100)).unwrap_err();
    let Error::Lua(error) = error else {
        panic!("expected Lua error");
    };
    assert_eq!(error.class, LuaFault::Argument);
    assert_eq!(
        error.to_string(),
        "bad argument #2 to 'sum' (number expected, got string)"
    );
    load(&mut rt, b"return pcall(sum, 1, 'two')");
    assert_eq!(
        no_panic(|| rt.run(100, &mut Journal::new())).unwrap(),
        StepOutcome::Completed
    );
    let result = rt.result_values().unwrap();
    assert!(matches!(result[0], Value::Boolean(false)));
    let Value::String(message) = &result[1] else {
        panic!("expected diagnostic");
    };
    assert_eq!(
        message.as_bytes(&rt).unwrap(),
        b"bad argument #2 to 'sum' (number expected, got string)"
    );
}
#[test]
fn unsigned_input_overflow_is_an_api_conversion_error() {
    let mut rt = idle();
    api_error(
        no_panic(|| (i64::MAX as u64 + 1).into_lua(&mut rt)),
        ApiError::Conversion(ConversionError {
            expected: "integer in Lua range",
            actual: LuaType::Number,
            position: None,
        }),
    );
}
#[test]
fn narrow_output_overflow_is_an_api_conversion_error() {
    let mut rt = idle();
    api_error(
        no_panic(|| u8::from_lua(Value::Integer(256), &mut rt)),
        ApiError::Conversion(ConversionError {
            expected: "u8",
            actual: LuaType::Number,
            position: None,
        }),
    );
}
#[test]
fn invalid_utf8_is_an_api_conversion_error() {
    let mut rt = idle();
    let text = rt.create_string(b"\xff\0").unwrap();
    api_error(
        no_panic(|| text.to_str(&rt)),
        ApiError::Conversion(ConversionError {
            expected: "UTF-8 string",
            actual: LuaType::String,
            position: None,
        }),
    );
}
#[test]
fn starting_a_second_call_is_busy() {
    let mut rt = idle();
    let f = function(&mut rt);
    rt.start_call(&f, ()).unwrap();
    api_error(no_panic(|| rt.start_call(&f, ())), ApiError::Busy);
    assert_eq!(
        rt.run(100, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(rt.finish_call::<i64>().unwrap(), 42);
}
#[test]
fn finishing_no_call_is_invalid_state() {
    api_error(
        no_panic(|| idle().finish_call::<()>()),
        ApiError::InvalidCallState,
    );
}
#[test]
fn running_no_call_is_invalid_state() {
    assert_eq!(
        no_panic(|| idle().run(100, &mut Journal::new())),
        Err(VmError::Api(ApiError::InvalidCallState))
    );
}
#[test]
fn finishing_an_unfinished_call_is_invalid_state() {
    let mut rt = idle();
    let f = function(&mut rt);
    rt.start_call(&f, ()).unwrap();
    api_error(
        no_panic(|| rt.finish_call::<()>()),
        ApiError::InvalidCallState,
    );
}
#[test]
fn double_wait_completion_is_already_completed() {
    let (mut rt, _, mut journal, key) = waits();
    rt.complete(key, Completion::Return(vec![])).unwrap();
    api_error(
        no_panic(|| rt.complete(key, Completion::Return(vec![]))),
        ApiError::AlreadyCompleted,
    );
    assert_eq!(rt.run(100, &mut journal).unwrap(), StepOutcome::Completed);
    rt.finish_call::<()>().unwrap();
}
#[test]
fn completion_after_the_waiting_call_errored_is_already_completed() {
    let (mut rt, _, mut journal, key) = waits();
    rt.complete(key, Completion::Error(Value::Integer(7)))
        .unwrap();
    assert_eq!(
        rt.run(100, &mut journal).unwrap(),
        StepOutcome::LuaError(LuaFault::Error)
    );
    let Err(Error::Lua(error)) = rt.finish_call::<()>() else {
        panic!("expected Lua error");
    };
    assert!(matches!(error.value, Value::Integer(7)));
    api_error(
        no_panic(|| rt.complete(key, Completion::Return(vec![]))),
        ApiError::AlreadyCompleted,
    );
}
#[test]
fn unknown_wait_completion_is_not_waiting() {
    api_error(
        no_panic(|| idle().complete(WaitKey(123), Completion::Return(vec![]))),
        ApiError::NotWaiting,
    );
}
#[test]
fn restore_without_a_native_symbol_is_a_snapshot_error() {
    let (rt, _, _, _) = waits();
    snapshot_error(
        no_panic(|| Runtime::restore(&rt.snapshot().unwrap(), &Host::default())),
        SnapshotError::UnknownHostSymbol,
    );
}
#[test]
fn restore_without_a_host_userdata_type_is_a_snapshot_error() {
    let mut registry = HostRegistry::new();
    registry.register_portable_userdata::<Portable>();
    let mut rt = Runtime::builder().registry(registry).build().unwrap();
    let _value = rt.create_host_userdata(Portable(42), 0).unwrap();
    snapshot_error(
        no_panic(|| Runtime::restore(&rt.snapshot().unwrap(), &Host::default())),
        SnapshotError::UnknownUserdataType,
    );
}
#[test]
fn rebind_failure_is_a_snapshot_error_with_no_runtime() {
    let (rt, value) = resources(b"key".to_vec());
    snapshot_error(
        no_panic(|| Runtime::restore(&rt.snapshot().unwrap(), &Host::new(rt.registry().clone()))),
        SnapshotError::Rebind {
            symbol: Resource::SYMBOL,
            object: value.id(),
            error: RebindError("resource missing"),
        },
    );
}
struct MissingModule;
impl ModuleResolver for MissingModule {
    fn resolve(&self, _: &[u8]) -> Resolved {
        Resolved::NotFound(b"host refused module".to_vec())
    }
}
#[test]
fn module_resolver_failure_is_catchable_in_lua() {
    let mut rt = Runtime::builder()
        .libraries(Libraries::BASE | Libraries::PACKAGE)
        .module_resolver(ResolverPolicy::Pure, MissingModule)
        .build()
        .unwrap();
    load(&mut rt, b"return pcall(require, 'game.missing')");
    assert_eq!(
        no_panic(|| rt.run(1000, &mut Journal::new())).unwrap(),
        StepOutcome::Completed
    );
    let result = rt.result_values().unwrap();
    assert!(matches!(result[0], Value::Boolean(false)));
    let Value::String(text) = &result[1] else {
        panic!("expected diagnostic");
    };
    assert!(text.to_str(&rt).unwrap().contains("host refused module"));
    load(&mut rt, b"return require('game.missing')");
    assert_eq!(
        no_panic(|| rt.run(1000, &mut Journal::new())).unwrap(),
        StepOutcome::LuaError(LuaFault::Require)
    );
}
#[test]
fn huge_wait_payload_is_an_api_error() {
    let mut registry = HostRegistry::new();
    registry.function("misuse.huge", NativePolicy::VmLocal, |_| {
        Ok(NativeReturn::Wait(WaitRequest {
            operation: "read".into(),
            payload: MultiValue(vec![Value::Nil; 1025]),
        }))
    });
    let mut rt = Runtime::builder()
        .limits(Limits {
            max_stack_slots: 1024,
            ..Limits::default()
        })
        .registry(registry)
        .build()
        .unwrap();
    let f = rt.make_closure("misuse.huge", ()).unwrap();
    api_error(
        no_panic(|| rt.call::<()>(&f, (), &mut Journal::new(), 100)),
        ApiError::InvalidCallState,
    );
}

#[test]
fn wait_payload_memory_errors_are_catchable_under_both_native_policies() {
    for policy in [NativePolicy::VmLocal, NativePolicy::External] {
        let mut registry = HostRegistry::new();
        registry.function("large_wait", policy, |_| {
            Ok(NativeReturn::Wait(WaitRequest {
                operation: "read".into(),
                payload: MultiValue(vec![Value::Nil; 1024]),
            }))
        });
        let mut rt = Runtime::builder()
            .registry(registry)
            .libraries(Libraries::BASE)
            .limits(Limits {
                max_logical_heap: 12_000,
                max_stack_slots: 1024,
                ..Limits::default()
            })
            .build()
            .unwrap();
        let f = rt.make_closure("large_wait", ()).unwrap();
        rt.globals().raw_set(&mut rt, "f", f).unwrap();
        load(&mut rt, b"return pcall(f)");
        assert_eq!(
            rt.run(10_000, &mut Journal::new()).unwrap(),
            StepOutcome::Completed
        );
        let (ok, error): (bool, String) =
            FromLuaMulti::from_lua_multi(rt.result_values().unwrap(), &mut rt).unwrap();
        assert!(!ok);
        assert_eq!(error, "not enough memory");
        Runtime::restore(&rt.snapshot().unwrap(), &Host::new(rt.registry().clone())).unwrap();
    }
}

struct Buffer(Vec<u8>);
impl HostUserdata for Buffer {
    const SYMBOL: &'static str = "misuse.Buffer";
    fn logical_size(&self) -> u64 {
        self.0.len() as u64
    }
}
impl PortableUserdata for Buffer {
    fn encode(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.to_vec()))
    }
}

#[test]
fn over_quota_userdata_growth_is_refused_at_snapshot_time() {
    let mut registry = HostRegistry::new();
    registry.register_portable_userdata::<Buffer>();
    for (mode, size, collect) in [
        (GcMode::Incremental, 13_000, false),
        (GcMode::Generational, 13_000, false),
        (GcMode::Generational, 11_000, true),
    ] {
        let mut rt = Runtime::builder()
            .registry(registry.clone())
            .config(Config {
                gc_mode: mode,
                max_logical_heap: 12_000,
                ..Config::default()
            })
            .build()
            .unwrap();
        let value = rt.create_host_userdata(Buffer(Vec::new()), 0).unwrap();
        let before = rt.memory().logical_bytes;
        value
            .borrow_mut::<Buffer>(&mut rt)
            .unwrap()
            .0
            .resize(size, 1);
        assert_eq!(rt.memory().logical_bytes, before + size as u64);
        if collect {
            rt.collect();
        }
        assert_eq!(rt.snapshot().err(), Some(SnapshotError::LimitExceeded));
        value.borrow_mut::<Buffer>(&mut rt).unwrap().0.clear();
        rt.collect();
        let restored =
            Runtime::restore(&rt.snapshot().unwrap(), &Host::new(rt.registry().clone())).unwrap();
        assert_eq!(restored.memory().logical_bytes, rt.memory().logical_bytes);
    }
}

#[test]
fn host_userdata_allocations_collect_dropped_handles_before_refusal() {
    let mut registry = HostRegistry::new();
    registry.register_portable_userdata::<Buffer>();
    let mut snapshots = Vec::new();
    for checkpoint in [false, true] {
        let mut rt = Runtime::builder()
            .registry(registry.clone())
            .limits(Limits {
                max_logical_heap: 6000,
                ..Limits::default()
            })
            .build()
            .unwrap();
        for _ in 0..200 {
            drop(rt.create_host_userdata(Buffer(vec![0; 1000]), 0).unwrap());
            if checkpoint {
                rt = Runtime::restore(&rt.snapshot().unwrap(), &Host::new(registry.clone()))
                    .unwrap();
            }
        }
        snapshots.push(rt.snapshot().unwrap());
        rt.set_auto_gc(false);
        assert!(matches!(
            rt.create_host_userdata(Buffer(vec![0; 1000]), 0),
            Err(Error::Lua(LuaError {
                class: LuaFault::Memory,
                ..
            }))
        ));
        rt.collect();
        assert!(rt.create_host_userdata(Buffer(vec![0; 1000]), 0).is_ok());
    }
    assert_eq!(snapshots[0], snapshots[1]);
}
#[test]
fn huge_rebind_key_is_refused_by_snapshot_limits() {
    let (rt, _value) = resources(vec![0; MAX_REBIND_KEY + 1]);
    assert_eq!(
        no_panic(|| rt.snapshot()),
        Err(SnapshotError::LimitExceeded)
    );
}
struct HugeModule;
impl ModuleResolver for HugeModule {
    fn resolve(&self, _: &[u8]) -> Resolved {
        Resolved::Source(vec![b' '; 1025])
    }
}
#[test]
fn huge_resolver_source_is_a_lua_memory_error() {
    let mut rt = Runtime::builder()
        .libraries(Libraries::BASE | Libraries::PACKAGE)
        .limits(Limits {
            max_string_bytes: 1024,
            ..Limits::default()
        })
        .module_resolver(ResolverPolicy::Pure, HugeModule)
        .build()
        .unwrap();
    load(&mut rt, b"return require('game.huge')");
    assert_eq!(
        no_panic(|| rt.run(1000, &mut Journal::new())).unwrap(),
        StepOutcome::LuaError(LuaFault::Memory)
    );
}
#[test]
fn tampered_native_lua_continuation_is_refused_transactionally() {
    let mut registry = HostRegistry::new();
    registry.function("misuse.bridge", NativePolicy::VmLocal, |cx| {
        Ok(NativeReturn::CallLua {
            function: cx.arg(0).to_owned_value()?,
            args: MultiValue::new(),
            tag: 1,
            keep: MultiValue::new(),
        })
    });
    let mut rt = Runtime::builder().registry(registry).build().unwrap();
    let bridge = rt.make_closure("misuse.bridge", ()).unwrap();
    rt.globals().raw_set(&mut rt, "bridge", bridge).unwrap();
    load(&mut rt, b"return bridge(function() while true do end end)");
    assert!(matches!(
        rt.run(30, &mut Journal::new()).unwrap(),
        StepOutcome::Paused(_)
    ));
    let mut image = rt.to_image().unwrap();
    let boundary = image
        .threads
        .iter_mut()
        .flat_map(|t| &mut t.frames)
        .find_map(|f| match &mut f.boundary {
            Some(BoundaryImage::Native { symbol, .. }) => Some(symbol),
            _ => None,
        })
        .unwrap();
    *boundary = u32::MAX;
    refused_image(&rt, image, SnapshotError::UnknownHostSymbol);
}
#[test]
fn tampered_pending_wait_is_refused_transactionally() {
    let (rt, _, _, _) = waits();
    let mut image = rt.to_image().unwrap();
    let frame = image
        .threads
        .iter_mut()
        .flat_map(|t| &mut t.frames)
        .find(|f| f.wait_request.is_some())
        .unwrap();
    frame.pending = PendingImage::None;
    refused_image(&rt, image, SnapshotError::InvalidStructure);
}
#[test]
fn tampered_native_closure_capture_is_refused_transactionally() {
    let (rt, _f, _, _) = waits();
    let mut image = rt.to_image().unwrap();
    image.native_closures[0]
        .values
        .push(EncValue::String(u64::MAX));
    refused_image(&rt, image, SnapshotError::DanglingReference);
}
#[test]
fn tampered_rebindable_userdata_is_refused_transactionally() {
    let (rt, _value) = resources(b"key".to_vec());
    let mut image = rt.to_image().unwrap();
    let PayloadImage::Rebind { symbol, .. } = &mut image.userdata[0].payload else {
        panic!("expected rebind");
    };
    *symbol = "misuse.Unregistered".into();
    refused_image(&rt, image, SnapshotError::UnknownUserdataType);

    // Change the key's encoded length and bytes, then repair the checksum:
    // decoding must reject its bound before attempting any host rebind.
    let original = rt.snapshot().unwrap();
    let mut marker = vec![3];
    marker.extend((Resource::SYMBOL.len() as u32).to_le_bytes());
    marker.extend(Resource::SYMBOL.as_bytes());
    let length = original
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap()
        + marker.len();
    let mut bytes = original.clone();
    bytes[length..length + 4].copy_from_slice(&4097u32.to_le_bytes());
    bytes.splice(length + 4..length + 7, vec![0; 4097]);
    super::recrc(&mut bytes);
    snapshot_error(
        no_panic(|| Runtime::restore(&bytes, &Host::new(rt.registry().clone()))),
        SnapshotError::LimitExceeded,
    );
    assert_eq!(rt.snapshot().unwrap(), original);
}
