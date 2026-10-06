//! Restore uses only the released embedding API and fresh host registrations.
use moonseed::{
    Error, FromLuaMulti, Function, Host, HostRegistry, Journal, Libraries, NativePolicy, Runtime,
    SnapshotError, StepOutcome, VmError, compile,
};

fn host_registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    registry.typed("dogfood.scale", NativePolicy::VmLocal, |_, value: f64| {
        Ok(value * 2.0)
    });
    registry
}

#[test]
fn builder_math_restores_with_only_fresh_host_symbols() {
    let mut original = Runtime::builder()
        .registry(host_registry())
        .libraries(Libraries::MATH)
        .build()
        .unwrap();
    let scale = original.make_closure("dogfood.scale", ()).unwrap();
    original
        .globals()
        .raw_set(&mut original, "scale", scale)
        .unwrap();
    original
        .load_main(&compile(b"return scale(math.sqrt(441))").unwrap())
        .unwrap();
    let bytes = original.snapshot().unwrap();
    let host = Host::new(host_registry()).effect_domain(original.effect_domain());
    let mut restored = Runtime::restore(&bytes, &host).unwrap();
    assert_eq!(
        restored.run(10_000, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    let values = restored.result_values().unwrap();
    assert_eq!(f64::from_lua_multi(values, &mut restored).unwrap(), 42.0);
    let math: moonseed::Table = restored.globals().raw_get(&mut restored, "math").unwrap();
    let _: Function = math.raw_get(&mut restored, "sqrt").unwrap();
    assert!(matches!(
        Runtime::restore(&bytes, &Host::new(HostRegistry::new())),
        Err(Error::Vm(VmError::Snapshot(
            SnapshotError::UnknownHostSymbol
        )))
    ));
}

#[test]
fn restore_rejects_a_library_the_host_disallows() {
    let original = Runtime::builder()
        .libraries(Libraries::MATH)
        .build()
        .unwrap();
    let bytes = original.snapshot().unwrap();
    // An explicit manual registration cannot bypass the restore allowlist.
    let mut registry = HostRegistry::new();
    moonseed::register_math(&mut registry);
    let host = Host::new(registry).libraries(Libraries::BASE);
    assert!(matches!(
        Runtime::restore(&bytes, &host),
        Err(Error::Vm(VmError::Snapshot(
            SnapshotError::UnknownHostSymbol
        )))
    ));
    assert!(
        Runtime::restore(
            &bytes,
            &Host::new(HostRegistry::new()).libraries(Libraries::MATH)
        )
        .is_ok()
    );
}

#[test]
fn every_builder_library_rebinds_without_manual_registration() {
    for library in [
        Libraries::BASE,
        Libraries::PACKAGE,
        Libraries::COROUTINE,
        Libraries::MATH,
        Libraries::TABLE,
        Libraries::STRING,
        Libraries::UTF8,
        Libraries::DEBUG,
        Libraries::IO,
        Libraries::OS,
        Libraries::ALL,
    ] {
        let original = Runtime::builder().libraries(library).build().unwrap();
        let bytes = original.snapshot().unwrap();
        let restored =
            Runtime::restore(&bytes, &Host::new(HostRegistry::new()).libraries(library)).unwrap();
        assert_eq!(restored.snapshot().unwrap(), bytes);
    }
}

#[test]
fn automatic_libraries_preserve_explicit_restore_registrations() {
    fn registry() -> HostRegistry {
        let mut registry = HostRegistry::new();
        registry.typed("math.abs", NativePolicy::VmLocal, |_, _: i64| Ok(99_i64));
        registry
    }
    // A host symbol can have a library spelling even without installing it.
    // Existing explicit rebinding must retain that host implementation.
    let mut original = Runtime::builder().registry(registry()).build().unwrap();
    let function = original.make_closure("math.abs", ()).unwrap();
    original
        .globals()
        .raw_set(&mut original, "custom", function)
        .unwrap();
    original
        .load_main(&compile(b"return custom(-1)").unwrap())
        .unwrap();
    let bytes = original.snapshot().unwrap();
    let host = Host::new(registry()).libraries(Libraries::MATH);
    let mut restored = Runtime::restore(&bytes, &host).unwrap();
    assert_eq!(
        restored.run(100, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    let values = restored.result_values().unwrap();
    assert_eq!(i64::from_lua_multi(values, &mut restored).unwrap(), 99);
}
