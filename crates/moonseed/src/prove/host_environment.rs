use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{
    AnyUserData, EffectId, Error, Host, HostCapabilities, HostEnv, HostRegistry, HostUserdata,
    Journal, Libraries, ModuleResolver, NativeCall, NativeOutcome, NativePolicy, RebindError,
    RebindUserdata, Resolved, ResolverPolicy, Runtime, SnapshotError, StepOutcome, UserdataPolicy,
    VmError,
};

struct Resource {
    value: Cell<i64>,
    secret: &'static [u8],
}
struct World(HashMap<Vec<u8>, Rc<Resource>>);
struct Entity {
    key: Vec<u8>,
    resource: Rc<Resource>,
}
impl HostUserdata for Entity {
    const SYMBOL: &'static str = "test.RebindEntity";
    fn logical_size(&self) -> u64 {
        8
    }
}
impl RebindUserdata for Entity {
    fn key(&self) -> Vec<u8> {
        self.key.clone()
    }
    fn rebind(key: &[u8], env: &HostEnv) -> Result<Self, RebindError> {
        let world = env.get::<World>().ok_or(RebindError("missing World"))?;
        let resource = world
            .0
            .get(key)
            .ok_or(RebindError("missing entity"))?
            .clone();
        Ok(Self {
            key: key.to_vec(),
            resource,
        })
    }
}
fn entity(call: &mut NativeCall<'_>) -> NativeOutcome {
    let n = call.integer(0).unwrap_or(7);
    let key = if n <= 255 {
        vec![n as u8]
    } else {
        vec![b'K'; n as usize]
    };
    let entity = Entity {
        key,
        resource: Rc::new(Resource {
            value: Cell::new(42),
            secret: b"HOST-RESOURCE-SECRET-NOT-SERIALIZED",
        }),
    };
    let Ok(value) = call.new_host_userdata(entity, 0) else {
        return NativeOutcome::Fault;
    };
    call.push(value);
    NativeOutcome::Ready
}
fn entity_value(call: &mut NativeCall<'_>) -> NativeOutcome {
    let Some(entity) = call.userdata_ref::<Entity>(call.arg(0)) else {
        return NativeOutcome::Fault;
    };
    let value = entity.resource.value.get();
    call.push_integer(value);
    NativeOutcome::Ready
}
fn registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    registry.register_rebind_userdata::<Entity>();
    registry.register_native("entity", NativePolicy::VmLocal, entity);
    registry.register_native("entity_value", NativePolicy::VmLocal, entity_value);
    registry
}
fn entity_runtime(source: &[u8]) -> Runtime {
    let mut runtime = Runtime::builder()
        .registry(registry())
        .libraries(Libraries::STANDARD)
        .build()
        .unwrap();
    runtime.set_global_native("entity", "entity").unwrap();
    runtime
        .set_global_native("entity_value", "entity_value")
        .unwrap();
    execute(&mut runtime, source, &mut Journal::new());
    runtime
}
fn execute(runtime: &mut Runtime, source: &[u8], journal: &mut Journal) {
    runtime.load_main(&crate::compile(source).unwrap()).unwrap();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, journal).unwrap(),
        StepOutcome::Completed
    );
}
fn userdata(runtime: &mut Runtime, name: &str) -> AnyUserData {
    runtime.globals().raw_get(runtime, name).unwrap()
}
fn snapshot_error(result: crate::Result<Runtime>) -> SnapshotError {
    match result {
        Err(Error::Vm(VmError::Snapshot(error))) => error,
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("restore accepted"),
    }
}

#[test]
fn rebind_round_trip_keeps_identity_behavior_and_only_the_key() {
    let mut runtime = entity_runtime(b"u = entity(7)");
    let u = userdata(&mut runtime, "u");
    let original = u.borrow::<Entity>(&runtime).unwrap().resource.clone();
    let bytes = runtime.snapshot().unwrap();
    let image = crate::snapshot::decode_within(&bytes, &crate::Limits::default()).unwrap();
    assert!(
        matches!(&image.userdata[0].payload, crate::snapshot::PayloadImage::Rebind { symbol, key } if symbol == Entity::SYMBOL && key == &[7])
    );
    assert!(
        !bytes
            .windows(original.secret.len())
            .any(|window| window == original.secret)
    );
    let pointer = (Rc::as_ptr(&original) as usize).to_le_bytes();
    assert!(!bytes.windows(pointer.len()).any(|window| window == pointer));
    let mut env = HostEnv::new();
    env.insert(World(HashMap::from([(vec![7], original.clone())])));
    env.insert(123u32);
    assert_eq!(env.get::<u32>(), Some(&123));
    assert!(env.get::<u64>().is_none());
    let host = Host::new(registry()).host_env(env);
    let mut restored = Runtime::restore(&bytes, &host).unwrap();
    let rebound = userdata(&mut restored, "u");
    assert_eq!(rebound.id(), u.id());
    assert!(Rc::ptr_eq(
        &rebound.borrow::<Entity>(&restored).unwrap().resource,
        &original
    ));
    original.value.set(73);
    execute(
        &mut restored,
        b"return entity_value(u)",
        &mut Journal::new(),
    );
    assert!(matches!(
        restored.result_values().unwrap()[0],
        crate::Value::Integer(73)
    ));
    assert!(restored.host_env().get::<World>().is_some());
}

#[test]
fn missing_rebind_resource_aborts_and_drops_already_rebound_values() {
    let mut runtime = entity_runtime(b"u = entity(7); v = entity(8)");
    let u = userdata(&mut runtime, "u");
    let v = userdata(&mut runtime, "v");
    let original = u.borrow::<Entity>(&runtime).unwrap().resource.clone();
    let mut env = HostEnv::new();
    env.insert(World(HashMap::from([(vec![7], original.clone())])));
    let host = Host::new(registry()).host_env(env);
    let count = Rc::strong_count(&original);
    let error = snapshot_error(Runtime::restore(&runtime.snapshot().unwrap(), &host));
    assert_eq!(
        error,
        SnapshotError::Rebind {
            symbol: Entity::SYMBOL,
            object: v.id(),
            error: RebindError("missing entity")
        }
    );
    assert_eq!(Rc::strong_count(&original), count);
    assert_eq!(
        u.borrow::<Entity>(&runtime).unwrap().resource.value.get(),
        42
    );
}

#[test]
fn huge_rebind_key_is_refused_at_snapshot() {
    let runtime = entity_runtime(b"u = entity(4097)");
    assert_eq!(runtime.snapshot(), Err(SnapshotError::LimitExceeded));
    assert!(entity_runtime(b"u = entity(4096)").snapshot().is_ok());
}

#[test]
fn restore_requires_native_symbols_before_rebinding() {
    let runtime = entity_runtime(b"u = entity(7)");
    assert_eq!(
        snapshot_error(Runtime::restore(
            &runtime.snapshot().unwrap(),
            &Host::default()
        )),
        SnapshotError::UnknownHostSymbol
    );
}

#[test]
fn restore_requires_host_types_before_rebinding() {
    let runtime = entity_runtime(b"u = entity(7)");
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    registry.register_native("entity", NativePolicy::VmLocal, entity);
    registry.register_native("entity_value", NativePolicy::VmLocal, entity_value);
    assert_eq!(
        snapshot_error(Runtime::restore(
            &runtime.snapshot().unwrap(),
            &Host::new(registry)
        )),
        SnapshotError::UnknownUserdataType
    );
}

#[test]
fn restore_checks_all_policies_before_rebinding() {
    let runtime = entity_runtime(b"u = entity(7)");
    let mut registry = registry();
    assert_eq!(
        registry.userdata_policy(Entity::SYMBOL),
        Some(UserdataPolicy::Rebind)
    );
    registry.register_userdata::<Entity>();
    assert_eq!(
        snapshot_error(Runtime::restore(
            &runtime.snapshot().unwrap(),
            &Host::new(registry)
        )),
        SnapshotError::UserdataPolicyMismatch
    );
}

#[test]
fn restore_checks_effect_domain_before_rebinding() {
    let runtime = entity_runtime(b"u = entity(7)");
    assert_eq!(
        snapshot_error(Runtime::restore(
            &runtime.snapshot().unwrap(),
            &Host::new(registry()).effect_domain(2)
        )),
        SnapshotError::EffectDomainMismatch
    );
}

#[test]
fn print_restores_and_executes_without_a_sink() {
    let mut runtime = Runtime::builder()
        .libraries(Libraries::BASE)
        .output(|_| {})
        .build()
        .unwrap();
    runtime
        .load_main(&crate::compile(b"print('discarded'); return 9").unwrap())
        .unwrap();
    let mut restored = Runtime::restore(
        &runtime.snapshot().unwrap(),
        &Host::new(runtime.registry().clone()),
    )
    .unwrap();
    assert_eq!(
        restored
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert!(matches!(
        restored.result_values().unwrap()[0],
        crate::Value::Integer(9)
    ));
}

#[test]
fn malformed_rebind_key_is_refused() {
    let runtime = entity_runtime(b"u = entity(7)");
    let original = runtime.snapshot().unwrap();
    let mut marker = vec![3];
    marker.extend((Entity::SYMBOL.len() as u32).to_le_bytes());
    marker.extend(Entity::SYMBOL.as_bytes());
    let tag = original
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    let key_length = tag + marker.len();
    let mut bytes = original.clone();
    bytes[key_length..key_length + 4].copy_from_slice(&4097u32.to_le_bytes());
    bytes.splice(key_length + 4..key_length + 5, vec![7; 4097]);
    super::recrc(&mut bytes);
    assert_eq!(
        snapshot_error(Runtime::restore(&bytes, &Host::new(registry()))),
        SnapshotError::LimitExceeded
    );
}

#[test]
fn unknown_userdata_policy_is_refused() {
    let runtime = entity_runtime(b"u = entity(7)");
    let mut bytes = runtime.snapshot().unwrap();
    let mut marker = vec![3];
    marker.extend((Entity::SYMBOL.len() as u32).to_le_bytes());
    marker.extend(Entity::SYMBOL.as_bytes());
    let tag = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    bytes[tag] = 250;
    super::recrc(&mut bytes);
    assert_eq!(
        snapshot_error(Runtime::restore(&bytes, &Host::new(registry()))),
        SnapshotError::InvalidTag
    );
}

struct Resolver {
    modules: HashMap<Vec<u8>, Resolved>,
    calls: Rc<Cell<usize>>,
}
impl ModuleResolver for Resolver {
    fn resolve(&self, name: &[u8]) -> Resolved {
        self.calls.set(self.calls.get() + 1);
        self.modules
            .get(name)
            .cloned()
            .unwrap_or_else(|| Resolved::NotFound(b"no host module".to_vec()))
    }
}
fn resolver(name: &[u8], resolved: Resolved, calls: &Rc<Cell<usize>>) -> Resolver {
    Resolver {
        modules: HashMap::from([(name.to_vec(), resolved)]),
        calls: calls.clone(),
    }
}
fn resolver_runtime(policy: ResolverPolicy, resolver: Resolver) -> Runtime {
    Runtime::builder()
        .libraries(Libraries::STANDARD)
        .module_resolver(policy, resolver)
        .build()
        .unwrap()
}

#[test]
fn pure_resolver_source_keeps_preload_loader_data_and_loaded_protocol() {
    let calls = Rc::new(Cell::new(0));
    let mut runtime = resolver_runtime(
        ResolverPolicy::Pure,
        resolver(
            b"source",
            Resolved::Source(b"local name, data = ...; return {name, data, 17}".to_vec()),
            &calls,
        ),
    );
    execute(&mut runtime, b"package.preload.pre = function(n,d) return n .. d end; assert(require('pre') == 'pre:preload:'); local m, data = require('source'); assert(m[1] == 'source' and m[2] == 'source' and m[3] == 17 and data == 'source'); assert(require('source') == m)", &mut Journal::new());
    assert_eq!(calls.get(), 1);
}

#[test]
fn pure_resolver_binary_uses_validated_load_decoder() {
    let calls = Rc::new(Cell::new(0));
    let chunk = crate::compile(b"local name = ...; return name .. '-binary'").unwrap();
    let binary = crate::chunk::dump(&chunk.proto, false, 1 << 20).unwrap();
    let mut runtime = resolver_runtime(
        ResolverPolicy::Pure,
        resolver(b"bin", Resolved::Binary(binary), &calls),
    );
    execute(
        &mut runtime,
        b"assert(require('bin') == 'bin-binary')",
        &mut Journal::new(),
    );
    assert_eq!(calls.get(), 1);
}

fn native_loader(call: &mut NativeCall<'_>) -> NativeOutcome {
    call.push_arg(0);
    NativeOutcome::Ready
}
#[test]
fn pure_resolver_native_loaders_use_registered_symbols() {
    let calls = Rc::new(Cell::new(0));
    let mut registry = HostRegistry::new();
    registry.register_native("test.loader", NativePolicy::VmLocal, native_loader);
    let mut runtime = Runtime::builder()
        .registry(registry)
        .libraries(Libraries::STANDARD)
        .module_resolver(
            ResolverPolicy::Pure,
            resolver(b"native", Resolved::Native("test.loader".into()), &calls),
        )
        .build()
        .unwrap();
    execute(
        &mut runtime,
        b"local value, data = require('native'); assert(value == 'native' and data == 'native')",
        &mut Journal::new(),
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn not_found_lists_every_searcher_diagnostic_in_luas_format() {
    let calls = Rc::new(Cell::new(0));
    let mut runtime = resolver_runtime(
        ResolverPolicy::Pure,
        resolver(
            b"missing",
            Resolved::NotFound(b"no host module".to_vec()),
            &calls,
        ),
    );
    execute(&mut runtime, b"package.searchers[3] = function() return 'last diagnostic' end; local ok, err = pcall(require, 'missing'); assert(not ok); return err", &mut Journal::new());
    let error = runtime.result_values().unwrap().remove(0);
    let crate::Value::String(error) = error else {
        panic!("not a message")
    };
    assert!(error.as_bytes(&runtime).unwrap().ends_with(b"module 'missing' not found:\n\tno field package.preload['missing']\n\tno host module\n\tlast diagnostic"));
}

#[test]
fn resolver_loader_failures_are_catchable_require_errors() {
    for resolved in [
        Resolved::Source(b"local =".to_vec()),
        Resolved::Binary(b"bad binary".to_vec()),
        Resolved::Native("not.registered".into()),
    ] {
        let calls = Rc::new(Cell::new(0));
        let mut runtime =
            resolver_runtime(ResolverPolicy::Pure, resolver(b"bad", resolved, &calls));
        execute(
            &mut runtime,
            b"local ok, err = pcall(require, 'bad'); assert(not ok and type(err) == 'string')",
            &mut Journal::new(),
        );
    }
}

#[test]
fn external_resolution_survives_checkpoint_before_loader_use_and_replays_once() {
    let calls = Rc::new(Cell::new(0));
    let mut runtime = resolver_runtime(
        ResolverPolicy::External,
        resolver(
            b"external",
            Resolved::Source(b"used = (used or 0) + 1; return 41".to_vec()),
            &calls,
        ),
    );
    runtime.load_main(&crate::compile(b"assert(require('external') == 41); package.loaded.external = nil; assert(require('external') == 41); assert(used == 2)").unwrap()).unwrap();
    let before = runtime.snapshot().unwrap();
    let mut journal = Journal::new();
    for _ in 0..1000 {
        runtime.run(1, &mut journal).unwrap();
        if calls.get() == 1 {
            break;
        }
    }
    assert_eq!(calls.get(), 1);
    assert!(
        runtime
            .globals()
            .raw_get::<_, Option<i64>>(&mut runtime, "used")
            .unwrap()
            .is_none()
    );
    let checkpoint = runtime.snapshot().unwrap();
    let mut persisted = Journal::new();
    for record in journal.entries() {
        persisted.seed_record(record.clone()).unwrap();
    }
    assert_eq!(persisted, journal);
    journal = persisted;
    let changed = Rc::new(Cell::new(0));
    let host = Host::new(runtime.registry().clone()).module_resolver(
        ResolverPolicy::External,
        resolver(
            b"external",
            Resolved::Source(b"error('changed source')".to_vec()),
            &changed,
        ),
    );
    let mut restored = Runtime::restore(&checkpoint, &host).unwrap();
    assert_eq!(
        restored.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(changed.get(), 0);
    assert_eq!(journal.entries().len(), 1);
    assert!(
        journal.entries()[0]
            .bytes
            .as_ref()
            .unwrap()
            .ends_with(b"return 41")
    );
    // Re-executing from before the effect also uses the same recorded answer.
    let mut restored = Runtime::restore(&before, &host).unwrap();
    assert_eq!(
        restored.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(changed.get(), 0);
    assert_eq!(journal.entries().len(), 1);
    assert_eq!(restored.next_sequence(), 2);
}

#[test]
fn byte_and_integer_journal_commits_are_exactly_once() {
    let mut journal = Journal::new();
    let id = EffectId {
        domain: 1,
        sequence: 1,
    };
    assert_eq!(journal.commit(id, 8, || vec![0, 255]), Ok(vec![0, 255]));
    assert_eq!(
        journal.commit(id, 8, || -> Vec<u8> { panic!("repeated") }),
        Ok(vec![0, 255])
    );
    assert_eq!(journal.commit(id, 8, || 1), Err(crate::JournalError));
    let id = EffectId {
        domain: 1,
        sequence: 2,
    };
    assert_eq!(journal.commit(id, 9, || 10), Ok(10));
    assert_eq!(journal.commit(id, 9, || 20), Ok(10));
    assert_eq!(journal.commit(id, 9, || vec![1]), Err(crate::JournalError));
    assert_eq!(journal.entries().len(), 2);
}

#[test]
fn external_continuation_effect_is_stable_after_memory_error_and_restore() {
    use crate::{FromLuaMulti, Function, LuaFault, MultiValue, NativeReturn, Value, WaitRequest};
    for checkpoint in [false, true] {
        let effects = Rc::new(Cell::new(0));
        let counter = effects.clone();
        let mut registry = HostRegistry::new();
        registry.function("bridge", NativePolicy::External, move |cx| {
            let effect = cx.effect().unwrap();
            cx.journal()
                .unwrap()
                .commit(effect, 0, || {
                    counter.set(counter.get() + 1);
                    0i64
                })
                .unwrap();
            let resume = cx.resumed();
            if resume.as_ref().is_some_and(|r| r.tag == 1) {
                return Ok(NativeReturn::Wait(WaitRequest {
                    operation: "read".into(),
                    payload: MultiValue(vec![Value::Nil; 1024]),
                }));
            }
            Ok(NativeReturn::CallLua {
                function: cx.arg(0).to_owned_value()?,
                args: MultiValue::new(),
                tag: u32::from(resume.is_some()),
                keep: MultiValue::new(),
            })
        });
        let mut rt = Runtime::builder()
            .registry(registry)
            .limits(crate::Limits {
                max_logical_heap: 12_000,
                max_stack_slots: 1024,
                ..crate::Limits::default()
            })
            .build()
            .unwrap();
        execute(
            &mut rt,
            b"return function() return 1 end",
            &mut Journal::new(),
        );
        let lua = Function::from_lua_multi(rt.result_values().unwrap(), &mut rt).unwrap();
        let bridge = rt.make_closure("bridge", ()).unwrap();
        rt.start_call(&bridge, lua).unwrap();
        let mut journal = Journal::new();
        let outcome = loop {
            if checkpoint {
                rt = Runtime::restore(&rt.snapshot().unwrap(), &Host::new(rt.registry().clone()))
                    .unwrap();
            }
            match rt.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                outcome => break outcome,
            }
        };
        assert_eq!(outcome, StepOutcome::LuaError(LuaFault::Memory));
        rt = Runtime::restore(&rt.snapshot().unwrap(), &Host::new(rt.registry().clone())).unwrap();
        assert_eq!(rt.run(10_000, &mut journal).unwrap(), outcome);
        assert!(
            matches!(rt.finish_call::<()>(), Err(Error::Lua(error)) if error.class == LuaFault::Memory)
        );
        assert_eq!(effects.get(), 1);
        assert_eq!(journal.entries().len(), 1);
        assert_eq!(rt.next_sequence(), 2);
    }
}

#[test]
fn shared_capabilities_apply_to_builder_and_restore() {
    let output = Rc::new(Cell::new(0));
    let warnings = Rc::new(Cell::new(0));
    let entropy = Rc::new(Cell::new(0));
    let (o, w, e) = (output.clone(), warnings.clone(), entropy.clone());
    let capabilities = HostCapabilities::default()
        .output(move |_| o.set(o.get() + 1))
        .warnings(move |_, _| w.set(w.get() + 1))
        .entropy(move || {
            e.set(e.get() + 1);
            7
        });
    let mut runtime = Runtime::builder()
        .libraries(Libraries::STANDARD)
        .capabilities(capabilities.clone())
        .build()
        .unwrap();
    runtime
        .load_main(&crate::compile(b"print('a'); warn('b'); math.randomseed()").unwrap())
        .unwrap();
    let host = Host::new(runtime.registry().clone()).capabilities(capabilities);
    let mut restored = Runtime::restore(&runtime.snapshot().unwrap(), &host).unwrap();
    assert_eq!(
        restored
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert!(output.get() > 0);
    assert!(warnings.get() > 0);
    assert_eq!(entropy.get(), 2);
}
