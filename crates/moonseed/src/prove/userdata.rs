//! Full and light userdata and host objects (Phase 3.26, ADR 0042 to
//! ADR 0045). A corpus gives Lua 5.4.9's output, through a reference
//! harness that makes userdata with Lua's C API, and keeps it under every
//! schedule; user values keep what they hold; host types are checked,
//! charged, and carried or refused by snapshots; restore refuses what the
//! runtime cannot make.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::{capture, restore, text};
use super::*;
use crate::host::{HostValue, ProofCounter, ProofHandle, USERDATA_NATIVES};

/// Boot with every standard library, `debug`, the userdata natives, and
/// `park`.
fn boot_ud(spec: &crate::program::ProtoSpec) -> Runtime {
    boot_ud_with(Config::default(), spec)
}

fn boot_ud_with(config: Config, spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), spec, false).unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    for (name, _) in USERDATA_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
    runtime.set_global_native("park", "park").unwrap();
    runtime
}

fn corpus() -> crate::program::ProtoSpec {
    let mut chunk = crate::compile(&fixture("corpus_userdata.lua")).unwrap();
    chunk.set_chunk_name(b"@corpus_userdata.lua");
    chunk.proto
}

fn straight_output(spec: &crate::program::ProtoSpec) -> (String, Runtime) {
    let mut runtime = boot_ud(spec);
    let written = capture(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    (text(&written), runtime)
}

/// Run `source` to the end and return what it printed.
fn printed(source: &str) -> String {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    straight_output(&chunk.proto).0
}

/// Run `source` until it waits on `park`.
fn waiting(source: &str) -> (Runtime, crate::WaitKey) {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_ud(&chunk.proto);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let StepOutcome::Waiting(key) = outcome else {
        panic!("{outcome:?}");
    };
    (runtime, key)
}

/// The corpus writes what Lua 5.4.9 wrote for it (`corpus_userdata.out`).
#[test]
fn userdata_corpus_matches_lua() {
    let (output, _) = straight_output(&corpus());
    let expected = String::from_utf8(fixture("corpus_userdata.out")).unwrap();
    for (index, (a, b)) in output.lines().zip(expected.lines()).enumerate() {
        assert_eq!(a, b, "line {}", index + 1);
    }
    assert_eq!(output.lines().count(), expected.lines().count());
}

/// `MOONSEED_LUA54_UD` names Lua 5.4.9 built with
/// `tools/lua54_userdata_harness.c`, which gives source the same userdata
/// natives through Lua's C API.
#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_userdata_corpus() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54_UD")
        .expect("MOONSEED_LUA54_UD must point at the userdata harness");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("corpus_userdata.lua")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(fixture("corpus_userdata.out")).unwrap()
    );
}

/// The corpus writes the same, with the same fuel, under small quanta, and
/// with a collection, a checkpoint, and a restore at every step: user
/// values, metatables, byte payloads, light keys, and `upvalueid` tokens
/// all come back as they were.
#[test]
fn userdata_corpus_keeps_its_output_under_every_schedule() {
    let spec = corpus();
    let (expected, straight) = straight_output(&spec);
    for quantum in [1u64, 3, 7] {
        let mut runtime = boot_ud(&spec);
        let written = capture(&mut runtime);
        let mut journal = Journal::new();
        while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
        assert_eq!(text(&written), expected, "quantum {quantum}");
        assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed());
    }
    let written = Rc::new(RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
    };
    let mut runtime = boot_ud(&spec);
    attach(&mut runtime);
    let mut journal = Journal::new();
    // A host collection changes what the collector does next, so fuel is
    // compared with a twin that collects alike and is never restored.
    let mut twin = boot_ud(&spec);
    let mut twin_journal = Journal::new();
    loop {
        runtime.collect();
        twin.collect();
        runtime = restore(&runtime);
        attach(&mut runtime);
        let outcome = runtime.run(1, &mut journal).unwrap();
        assert_eq!(twin.run(1, &mut twin_journal).unwrap(), outcome);
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(String::from_utf8_lossy(&written.borrow()), expected);
    assert_eq!(runtime.fuel_consumed(), twin.fuel_consumed());
}

/// A user value is the only thing keeping what it holds: the holder
/// survives collections with it, and once the userdata goes, so does it.
#[test]
fn user_values_keep_what_they_hold() {
    let output = printed(
        "local u = newud(0, 2) \
         debug.setuservalue(u, string.rep('x', 100000), 1) \
         local holder = {} debug.setuservalue(u, holder, 2) \
         holder.back = u \
         collectgarbage() collectgarbage() \
         print(#debug.getuservalue(u, 1), debug.getuservalue(u, 2) == holder) \
         local with = collectgarbage('count') \
         u, holder = nil, nil \
         collectgarbage() \
         print(with - collectgarbage('count') > 90)",
    );
    assert_eq!(output, "100000\ttrue\ntrue\n");
}

/// A host object composes with Lua: methods are native functions in an
/// ordinary `__index` table, called with the userdata as `self`, and a
/// wrong argument is Lua's argument error naming the host type.
#[test]
fn host_objects_take_methods_through_their_metatable() {
    let output = printed(
        "local Counter = {} Counter.__index = Counter \
         Counter.get = counter_get Counter.add = counter_add \
         local c = counter_new(5, Counter) \
         print(type(c), c:get(), c:add(3):get(), c:add(-10):get()) \
         local d = counter_new(1, Counter) \
         print(c ~= d, getmetatable(c) == getmetatable(d), d:get()) \
         print(pcall(counter_get, newud(0))) \
         print(pcall(counter_get, handle_new(1))) \
         print(pcall(handle_get, c)) \
         print(pcall(counter_get, light(1))) \
         print(pcall(counter_add, c, 'x')) \
         print(pcall(counter_get)) \
         debug.setmetatable(c, { __index = { get = function() return 'replaced' end } }) \
         print(c:get(), d:get(), handle_get(handle_new(9)))",
    );
    assert_eq!(
        output,
        "userdata\t5\t8\t-2\n\
         true\ttrue\t1\n\
         false\tbad argument #1 to 'counter_get' (moonseed.Counter expected, got userdata)\n\
         false\tbad argument #1 to 'counter_get' (moonseed.Counter expected, got userdata)\n\
         false\tbad argument #1 to 'handle_get' (moonseed.Handle expected, got userdata)\n\
         false\tbad argument #1 to 'counter_get' (moonseed.Counter expected, got light userdata)\n\
         false\tbad argument #2 to 'counter_add' (number expected, got string)\n\
         false\tbad argument #1 to 'counter_get' (moonseed.Counter expected, got no value)\n\
         replaced\t1\t9\n"
    );
}

/// A host value is reached only through its own type: the runtime's
/// accessors lend it to a closure, and a byte payload never passes for a
/// host value or the other way round.
#[test]
fn the_host_reaches_payloads_only_by_their_type() {
    let chunk = crate::compile(
        b"local b = newud(4) udpoke(b, 1, 9) \
          return b, counter_new(7), handle_new(3), light(5)",
    )
    .unwrap();
    let mut runtime = boot_ud(&chunk.proto);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let results = runtime.results().unwrap();
    let id = |index: usize| match results[index] {
        HostValue::Object(id) => id,
        ref other => panic!("{other:?}"),
    };
    let (bytes, counter, handle) = (id(0), id(1), id(2));
    assert_eq!(
        runtime.with_userdata_bytes(bytes, <[u8]>::to_vec),
        Some(vec![0, 9, 0, 0])
    );
    runtime.with_userdata_bytes_mut(bytes, |bytes| bytes[3] = 4);
    assert_eq!(
        runtime.with_userdata_bytes(bytes, |bytes| bytes[3]),
        Some(4)
    );
    assert_eq!(
        runtime.with_userdata::<ProofCounter, _>(counter, |c| c.count),
        Some(7)
    );
    runtime.with_userdata_mut::<ProofCounter, _>(counter, |c| c.count = 70);
    assert_eq!(
        runtime.with_userdata::<ProofCounter, _>(counter, |c| c.count),
        Some(70)
    );
    assert!(
        runtime
            .with_userdata::<ProofHandle, _>(counter, |_| ())
            .is_none()
    );
    assert!(
        runtime
            .with_userdata::<ProofCounter, _>(bytes, |_| ())
            .is_none()
    );
    assert!(runtime.with_userdata_bytes(counter, |_| ()).is_none());
    assert_eq!(
        runtime.with_userdata::<ProofHandle, _>(handle, |h| h.0),
        Some(3)
    );
    let HostValue::LightUserdata(light) = results[3] else {
        panic!("{:?}", results[3]);
    };
    assert_eq!(light.host_key(), Some(crate::HostLightKey(5)));
}

/// A portable host value goes through its codec: restored before and
/// after a host wait, the counter keeps its count, its identity, its
/// metatable, and its user value.
#[test]
fn portable_host_userdata_cross_a_checkpoint() {
    let (runtime, key) = waiting(
        "local mt = { __index = { get = counter_get, add = counter_add } } \
         local c = counter_new(40, mt) \
         debug.setuservalue(c, 'kept') \
         local t = { [c] = 'key' } \
         local by = park() \
         c:add(by) \
         return c:get(), t[c], debug.getuservalue(c), getmetatable(c) == mt",
    );
    let mut restored = restore(&runtime);
    restored
        .complete_legacy(
            key,
            crate::host::LegacyCompletion::Return(vec![HostValue::Integer(2)]),
        )
        .unwrap();
    let mut journal = Journal::new();
    assert!(matches!(
        restored.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    ));
    assert_eq!(
        restored.results().unwrap(),
        vec![
            HostValue::Integer(42),
            HostValue::String(b"key".to_vec()),
            HostValue::String(b"kept".to_vec()),
            HostValue::Boolean(true),
        ]
    );
}

/// A host value without a codec is not snapshot state: a snapshot of a
/// heap holding one fails, never dropping it or writing an address. Once
/// nothing holds it and it is collected, the snapshot succeeds.
#[test]
fn snapshots_refuse_host_userdata_without_a_codec() {
    let (mut runtime, key) = waiting("local h = handle_new(1) park() h = nil");
    assert_eq!(
        runtime.snapshot().err(),
        Some(SnapshotError::NonPortableUserdata)
    );
    runtime
        .complete_legacy(key, crate::host::LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    // Unreachable but not yet collected: still refused.
    assert_eq!(
        runtime.snapshot().err(),
        Some(SnapshotError::NonPortableUserdata)
    );
    runtime.collect();
    restore(&runtime);
}

/// Userdata count against the logical heap: their bytes, their user
/// values, and what a host type declares, before anything is made. A
/// program cannot pass the quota through any of them, and a refused one
/// is a memory error `pcall` catches.
#[test]
fn userdata_count_against_the_heap_quota() {
    let config = Config {
        max_logical_heap: 4 << 20,
        ..Config::default()
    };
    let run = |source: &str| {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut runtime = boot_ud_with(config.clone(), &chunk.proto);
        let written = capture(&mut runtime);
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
        text(&written)
    };
    // Each kind of charge, kept alive until the quota refuses one.
    for make in [
        "newud(65536)",
        "newud(0, 4096)",
        "counter_new(1, nil, 65536)",
    ] {
        let output = run(&format!(
            "local keep = {{}} \
             local ok, err = pcall(function() for i = 1, 100000 do keep[i] = {make} end end) \
             print(ok, err, #keep < 100)"
        ));
        assert_eq!(output, "false\tnot enough memory\ttrue\n", "{make}");
    }
    // Bounds past any quota, and growth past it, are refused before they
    // are made or counted.
    let output = run("print(pcall(newud, 1 << 40)) \
         print(pcall(newud, 0, 70000)) \
         print(pcall(counter_new, 1, nil, 1 << 62)) \
         local c = counter_new(1) \
         print(pcall(counter_grow, c, 1 << 30)) \
         print(pcall(counter_grow, c, 1024), collectgarbage('count') > 1)");
    assert_eq!(
        output,
        "false\tnot enough memory\n\
         false\tnot enough memory\n\
         false\tnot enough memory\n\
         false\tnot enough memory\n\
         true\ttrue\n"
    );
}

/// Restore refuses userdata the runtime cannot make or cannot read:
/// a host type that is missing or registered without a codec, codec bytes
/// the type refuses, a value that counts more than was recorded, a byte
/// payload whose charge is not its length, light tokens naming objects
/// that never existed, and payloads past the quota.
#[test]
fn restore_refuses_userdata_it_cannot_make() {
    use crate::snapshot::{EncValue, Image, PayloadImage};
    let (runtime, _) = waiting(
        "local c = counter_new(3) local b = newud(4, 1) \
         debug.setuservalue(b, debug.upvalueid(function() return c end, 1)) \
         park()",
    );
    restore(&runtime);
    let domain = runtime.effect_domain();
    let refused = |registry: &HostRegistry, change: &dyn Fn(&mut Image), expected| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(Runtime::from_snapshot(&bytes, registry, domain), expected);
    };
    let unchanged = |_: &mut Image| {};
    fn host(image: &mut Image) -> &mut crate::snapshot::UserdataImage {
        image
            .userdata
            .iter_mut()
            .find(|u| matches!(u.payload, PayloadImage::Host { .. }))
            .unwrap()
    }
    fn bytes(image: &mut Image) -> &mut crate::snapshot::UserdataImage {
        image
            .userdata
            .iter_mut()
            .find(|u| matches!(u.payload, PayloadImage::Bytes(_)))
            .unwrap()
    }
    // The type is missing, or registered as refusing snapshots.
    let mut without = HostRegistry::proof();
    without.register_userdata::<ProofHandle>();
    let mut bare = HostRegistry::new();
    crate::library::register_standard(&mut bare);
    crate::debuglib::register_debug(&mut bare);
    for (symbol, function) in USERDATA_NATIVES {
        bare.register_native(symbol, crate::NativePolicy::VmLocal, function);
    }
    bare.register_native(
        "park",
        crate::NativePolicy::VmLocal,
        crate::host::native_park,
    );
    refused(&bare, &unchanged, SnapshotError::UnknownUserdataType);
    refused(
        &HostRegistry::proof(),
        &|image| {
            if let PayloadImage::Host { symbol, .. } = &mut host(image).payload {
                *symbol = "moonseed.Handle".into();
            }
        },
        SnapshotError::UserdataPolicyMismatch,
    );
    let proof = HostRegistry::proof();
    // The codec refuses the bytes, or the value counts more than recorded.
    refused(
        &proof,
        &|image| {
            if let PayloadImage::Host { bytes, .. } = &mut host(image).payload {
                bytes.pop();
            }
        },
        SnapshotError::UserdataDecode,
    );
    refused(
        &proof,
        &|image| host(image).charge -= 1,
        SnapshotError::UserdataCharge,
    );
    // A byte payload counts exactly its length.
    refused(
        &proof,
        &|image| bytes(image).charge += 1,
        SnapshotError::InvalidStructure,
    );
    // Payloads past the quota.
    refused(
        &proof,
        &|image| host(image).charge = image_quota_plus_one(),
        SnapshotError::LimitExceeded,
    );
    // A metatable that is not a table, a user value naming nothing.
    refused(
        &proof,
        &|image| {
            let string = image.strings[0].0;
            bytes(image).metatable = string;
        },
        SnapshotError::InvalidStructure,
    );
    refused(
        &proof,
        &|image| bytes(image).user_values[0] = EncValue::Table(1 << 40),
        SnapshotError::DanglingReference,
    );
    // A token the VM made names an object that existed before the image.
    refused(
        &proof,
        &|image| {
            let next = image.next_object_id;
            bytes(image).user_values[0] = EncValue::Light(crate::value::LightDomain::Upvalue, next);
        },
        SnapshotError::InvalidStructure,
    );
    refused(
        &proof,
        &|image| {
            bytes(image).user_values[0] = EncValue::Light(crate::value::LightDomain::Upvalue, 0);
        },
        SnapshotError::InvalidStructure,
    );
    refused(
        &proof,
        &|image| {
            bytes(image).user_values[0] =
                EncValue::Light(crate::value::LightDomain::NativeValue, 1 << 8 | 200);
        },
        SnapshotError::InvalidStructure,
    );
    // A host key is any number; it never equals a token the VM made.
    let mut image = runtime.to_image().unwrap();
    let EncValue::Light(_, bits) = bytes(&mut image).user_values[0] else {
        panic!("no token");
    };
    bytes(&mut image).user_values[0] = EncValue::Light(crate::value::LightDomain::Host, bits);
    Runtime::from_snapshot(&snapshot::encode(&image).unwrap(), &proof, domain).unwrap();
}

fn image_quota_plus_one() -> u64 {
    Config::default().max_logical_heap + 1
}

/// A truncated or tampered userdata section fails as a structure error,
/// never by reserving what a count claims.
#[test]
fn tampered_userdata_sections_fail_closed() {
    let (runtime, _) = waiting("local b = newud(3, 2) udpoke(b, 0, 7) park()");
    let bytes = runtime.snapshot().unwrap();
    let registry = HostRegistry::proof();
    let domain = runtime.effect_domain();
    // The payload's length sits just before its three bytes.
    let at = bytes
        .windows(7)
        .position(|window| window == [3, 0, 0, 0, 7, 0, 0])
        .unwrap();
    for (len, expected) in [
        (u32::MAX, SnapshotError::LimitExceeded),
        (1 << 19, SnapshotError::Truncated),
    ] {
        let mut tampered = bytes.clone();
        tampered[at..at + 4].copy_from_slice(&len.to_le_bytes());
        recrc(&mut tampered);
        expect_snapshot(
            Runtime::from_snapshot(&tampered, &registry, domain),
            expected,
        );
    }
    // A shorter payload misreads what follows: refused, by whichever
    // check the misread bytes meet first.
    let mut tampered = bytes.clone();
    tampered[at..at + 4].copy_from_slice(&2u32.to_le_bytes());
    recrc(&mut tampered);
    assert!(Runtime::from_snapshot(&tampered, &registry, domain).is_err());
    let mut cut = bytes[..bytes.len() - 10].to_vec();
    recrc(&mut cut);
    assert!(Runtime::from_snapshot(&cut, &registry, domain).is_err());
}

/// Userdata count against `Config::max_objects` like every other object,
/// and restore refuses a heap past the limit.
#[test]
fn userdata_count_against_the_object_limit() {
    let config = Config {
        max_objects: 1000,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"keep = {} \
          local ok, err = pcall(function() for i = 1, 3000 do keep[i] = newud(0) end end) \
          print(ok, err, #keep < 1000)",
    )
    .unwrap();
    let mut runtime = boot_ud_with(config, &chunk.proto);
    let written = capture(&mut runtime);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(text(&written), "false\tnot enough memory\ttrue\n");
    assert!(runtime.memory().objects <= 1000);
    let mut image = runtime.to_image().unwrap();
    image.max_objects = 10;
    expect_snapshot(
        Runtime::from_snapshot(
            &snapshot::encode(&image).unwrap(),
            &HostRegistry::proof(),
            runtime.effect_domain(),
        ),
        SnapshotError::LimitExceeded,
    );
}

/// A host value that grew without reporting it is refused by the
/// snapshot, as restore would refuse it; growth made through
/// `with_userdata_mut` is counted, so that snapshot restores.
#[test]
fn unreported_growth_is_refused_when_the_snapshot_is_taken() {
    let chunk = crate::compile(b"return counter_new(1)").unwrap();
    let mut runtime = boot_ud(&chunk.proto);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let HostValue::Object(id) = runtime.results().unwrap()[0] else {
        panic!("no counter");
    };
    let before = runtime.memory().logical_bytes;
    runtime.with_userdata_mut::<ProofCounter, _>(id, |c| c.size = 4096);
    assert!(runtime.memory().logical_bytes >= before + 4096 - 16);
    restore(&runtime);
    // A native that grows the value without `set_userdata_charge`.
    let (mut runtime, _) = waiting("local c = counter_new(1) park() return c");
    // Grow it behind the runtime's back, as a native's `userdata_mut`
    // without `set_userdata_charge` would.
    let (index, generation) = runtime
        .heap()
        .userdata
        .iter()
        .map(|(index, generation, _)| (index, generation))
        .next()
        .unwrap();
    runtime
        .heap_mut()
        .userdata
        .get_mut(crate::id::Handle::new(index, generation))
        .unwrap()
        .payload
        .host_mut::<ProofCounter>()
        .unwrap()
        .size = 4096;
    assert_eq!(
        runtime.snapshot().err(),
        Some(SnapshotError::UserdataCharge)
    );
}

/// A light userdata token the VM made cannot be handed to a runtime: it
/// names a cell of the runtime that made it. Host keys come in.
#[test]
fn only_host_keys_come_in_from_the_host() {
    let chunk =
        crate::compile(b"local x = 1 return debug.upvalueid(function() return x end, 1)").unwrap();
    let mut maker = boot_ud(&chunk.proto);
    maker
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let token = maker.results().unwrap()[0].clone();
    assert!(matches!(token, HostValue::LightUserdata(light) if light.host_key().is_none()));
    let (mut runtime, key) = waiting("return park() == light(5)");
    assert!(
        runtime
            .complete_legacy(key, crate::host::LegacyCompletion::Return(vec![token]))
            .is_err()
    );
    let host = HostValue::LightUserdata(crate::LightUserdata::host(crate::HostLightKey(5)));
    runtime
        .complete_legacy(key, crate::host::LegacyCompletion::Return(vec![host]))
        .unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(runtime.results().unwrap(), vec![HostValue::Boolean(true)]);
}
