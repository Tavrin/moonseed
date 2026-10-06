//! Native ↔ wasm32 snapshot exchange.
//!
//! Fail-closed: this test is ignored unless the wasm artifact was built and
//! `MOONSEED_WASM_PROBE` points at it. A missing file is a failure, not a skip,
//! when the test is selected.

use std::path::PathBuf;

use moonseed::{Config, HostRegistry, Journal, Runtime, StepOutcome};
use wasmi::{Engine, Linker, Module, Store};

#[test]
#[ignore = "wasm_roundtrip"]
fn wasm_roundtrip() {
    let path = std::env::var("MOONSEED_WASM_PROBE").unwrap_or_else(|_| {
        panic!("MOONSEED_WASM_PROBE is not set; build the probe and pass its .wasm path")
    });
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read wasm probe {}: {error}", path.display()));

    let expected = native_finish_from_fresh();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).expect("parse wasm");
    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate")
        .start(&mut store)
        .expect("start");

    // Native snapshot of a fresh runtime, restored inside wasm, run to completion.
    let native_bytes = {
        let runtime = Runtime::boot_canonical(Config::default(), HostRegistry::proof()).unwrap();
        runtime.snapshot().unwrap()
    };
    call0(&instance, &mut store, "moonseed_restore_clear");
    push_bytes(&instance, &mut store, &native_bytes);
    let code = call1(&instance, &mut store, "moonseed_restore_finish", 1);
    assert_eq!(code, 0, "wasm rejected a native snapshot");
    assert_eq!(call1(&instance, &mut store, "moonseed_run", u64::MAX), 0);
    assert_fields(&instance, &mut store, &expected);

    // A registered host hook, count cursor, and pending delivery exchange in
    // both directions through the existing snapshot ABI.
    let mut hooks = HostRegistry::proof();
    hooks.register_hook("wasm.hook", |cx| {
        let globals = cx.globals();
        let count: i64 = cx.raw_get(&globals, "hook_events")?;
        cx.raw_set(&globals, "hook_events", count + 1)?;
        Ok(moonseed::HookAction::Continue)
    });
    let mut hooked = Runtime::builder()
        .registry(hooks.clone())
        .libraries(moonseed::Libraries::ALL)
        .build()
        .unwrap();
    let globals = hooked.globals();
    globals.raw_set(&mut hooked, "hook_events", 0).unwrap();
    hooked
        .load_main(&moonseed::compile(b"local a=0 for i=1,5 do a=a+i end return a").unwrap())
        .unwrap();
    hooked
        .set_hook(None, "wasm.hook", moonseed::HookMask::LINE, 2)
        .unwrap();
    hooked.run(1, &mut Journal::new()).unwrap();
    let hook_bytes = hooked.snapshot().unwrap();
    call0(&instance, &mut store, "moonseed_restore_clear");
    push_bytes(&instance, &mut store, &hook_bytes);
    assert_eq!(
        call1(&instance, &mut store, "moonseed_restore_finish", 1),
        0
    );
    assert_eq!(call1(&instance, &mut store, "moonseed_run", u64::MAX), 0);
    assert_eq!(call0(&instance, &mut store, "moonseed_snapshot"), 0);
    let wasm_hook = pull_snapshot(&instance, &mut store);
    let mut native_hook = Runtime::from_snapshot(&wasm_hook, &hooks, 1).unwrap();
    hooked
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(native_hook.snapshot().unwrap(), hooked.snapshot().unwrap());
    let globals = native_hook.globals();
    assert!(
        globals
            .raw_get::<_, i64>(&mut native_hook, "hook_events")
            .unwrap()
            > 0
    );
    assert_eq!(native_hook.fuel_consumed(), hooked.fuel_consumed());

    // Wasm's own fresh snapshot, restored natively.
    assert_eq!(call0(&instance, &mut store, "moonseed_reset"), 0);
    assert_eq!(call0(&instance, &mut store, "moonseed_snapshot"), 0);
    let wasm_bytes = pull_snapshot(&instance, &mut store);
    let mut restored = Runtime::from_snapshot(&wasm_bytes, &HostRegistry::proof(), 1)
        .expect("native restore of wasm snapshot");
    let mut journal = Journal::new();
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed));
    let observation = restored.observe(&journal).unwrap();
    assert_eq!(observation, expected);

    // A runtime waiting with userdata of every kind, carried each way: a
    // native snapshot finished inside wasm, a wasm one finished natively.
    let native_ud = moonseed::userdata_exchange_snapshot().unwrap();
    let native_finish = moonseed::userdata_exchange_finish(&native_ud).unwrap();
    call0(&instance, &mut store, "moonseed_restore_clear");
    push_bytes(&instance, &mut store, &native_ud);
    let wasm_finish = instance
        .get_typed_func::<(), i64>(&store, "moonseed_userdata_finish")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_finish, native_finish);
    assert_eq!(
        call0(&instance, &mut store, "moonseed_userdata_snapshot"),
        0
    );
    let wasm_ud = pull_snapshot(&instance, &mut store);
    assert_eq!(wasm_ud, native_ud);
    assert_eq!(
        moonseed::userdata_exchange_finish(&wasm_ud).unwrap(),
        native_finish
    );

    let native_tables = moonseed::table_semantics_fingerprint().unwrap();
    let wasm_tables = instance
        .get_typed_func::<(), i64>(&store, "moonseed_table_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_tables, native_tables);
    assert_ne!(wasm_tables, i64::MIN);

    let native_source = moonseed::source_closure_fingerprint().unwrap();
    let wasm_source = instance
        .get_typed_func::<(), i64>(&store, "moonseed_source_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_source, native_source);
    assert_ne!(wasm_source, i64::MIN);

    let native_branch = moonseed::source_branch_fingerprint().unwrap();
    let wasm_branch = instance
        .get_typed_func::<(), i64>(&store, "moonseed_branch_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_branch, native_branch);
    assert_eq!(native_branch, 10 | 11 << 8 | 11 << 16 | 99 << 24);

    let native_loops = moonseed::source_loops_fingerprint().unwrap();
    let wasm_loops = instance
        .get_typed_func::<(), i64>(&store, "moonseed_loops_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_loops, native_loops);
    let native_tables = moonseed::source_tables_fingerprint().unwrap();
    let wasm_tables = instance
        .get_typed_func::<(), i64>(&store, "moonseed_tables_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_tables, native_tables);
    assert_ne!(native_tables, i64::MIN);

    let native_natives = moonseed::source_natives_fingerprint().unwrap();
    let wasm_natives = instance
        .get_typed_func::<(), i64>(&store, "moonseed_natives_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_natives, native_natives);
    assert_ne!(native_natives, i64::MIN);

    let native_operators = moonseed::source_operators_fingerprint().unwrap();
    let wasm_operators = instance
        .get_typed_func::<(), i64>(&store, "moonseed_operators_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_operators, native_operators);
    assert_ne!(native_operators, i64::MIN);

    let native_errors = moonseed::source_errors_fingerprint().unwrap();
    let wasm_errors = instance
        .get_typed_func::<(), i64>(&store, "moonseed_errors_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_errors, native_errors);
    assert_ne!(native_errors, i64::MIN);

    let native_close = moonseed::source_close_fingerprint().unwrap();
    let wasm_close = instance
        .get_typed_func::<(), i64>(&store, "moonseed_close_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_close, native_close);
    assert_ne!(native_close, i64::MIN);

    let native_generic_for = moonseed::source_generic_for_fingerprint().unwrap();
    let wasm_generic_for = instance
        .get_typed_func::<(), i64>(&store, "moonseed_generic_for_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_generic_for, native_generic_for);
    assert_ne!(native_generic_for, i64::MIN);

    let native_varargs = moonseed::source_varargs_fingerprint().unwrap();
    let wasm_varargs = instance
        .get_typed_func::<(), i64>(&store, "moonseed_varargs_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_varargs, native_varargs);
    assert_ne!(native_varargs, i64::MIN);

    let native_tail_calls = moonseed::source_tail_calls_fingerprint().unwrap();
    let wasm_tail_calls = instance
        .get_typed_func::<(), i64>(&store, "moonseed_tail_calls_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_tail_calls, native_tail_calls);
    assert_ne!(native_tail_calls, i64::MIN);

    let native_syntax = moonseed::source_syntax_fingerprint().unwrap();
    let wasm_syntax = instance
        .get_typed_func::<(), i64>(&store, "moonseed_syntax_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_syntax, native_syntax);
    assert_ne!(native_syntax, i64::MIN);

    let native_goto = moonseed::source_goto_fingerprint().unwrap();
    let wasm_goto = instance
        .get_typed_func::<(), i64>(&store, "moonseed_goto_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_goto, native_goto);
    assert_ne!(native_goto, i64::MIN);

    let native_base = moonseed::source_base_fingerprint().unwrap();
    let wasm_base = instance
        .get_typed_func::<(), i64>(&store, "moonseed_base_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_base, native_base);
    assert_ne!(native_base, i64::MIN);

    for (name, native) in [
        (
            "moonseed_library_fingerprint",
            moonseed::source_library_fingerprint().unwrap(),
        ),
        (
            "moonseed_math_bits_fingerprint",
            moonseed::math_bits_fingerprint().unwrap(),
        ),
        (
            // Includes the UTF-8 construction/scan/codes checkpoint case.
            "moonseed_string_fingerprint",
            moonseed::source_string_fingerprint().unwrap(),
        ),
        (
            "moonseed_debug_fingerprint",
            moonseed::source_debug_fingerprint().unwrap(),
        ),
        (
            "moonseed_coroutine_fingerprint",
            moonseed::source_coroutine_fingerprint().unwrap(),
        ),
        (
            "moonseed_userdata_fingerprint",
            moonseed::source_userdata_fingerprint().unwrap(),
        ),
        (
            "moonseed_gc_semantics_fingerprint",
            moonseed::source_gc_semantics_fingerprint().unwrap(),
        ),
    ] {
        let wasm = instance
            .get_typed_func::<(), i64>(&store, name)
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        assert_eq!(wasm, native, "{name}");
        assert_ne!(native, i64::MIN, "{name}");
    }

    let native_host = moonseed::host_capabilities_fingerprint().unwrap();
    let wasm_host = instance
        .get_typed_func::<(), i64>(&store, "moonseed_host_capabilities_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_ne!(native_host, i64::MIN);
    assert_eq!(wasm_host, native_host);

    let native_gc = moonseed::gc_schedule_fingerprint().unwrap();
    let wasm_gc = instance
        .get_typed_func::<(), i64>(&store, "moonseed_gc_fingerprint")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(wasm_gc, native_gc);
    assert_ne!(native_gc, i64::MIN);

    diagnostic_roundtrip(&instance, &mut store);

    let fold = |values: &[i64]| {
        values.iter().fold(0i64, |acc, value| {
            acc.wrapping_mul(131).wrapping_add(*value)
        })
    };
    assert_eq!(
        native_loops,
        fold(&[
            0,
            1,
            2,
            0,
            1,
            2,
            42,
            77,
            0,
            1,
            2,
            12,
            3,
            i64::MAX,
            3,
            i64::MIN,
            2,
            (1 << 62) + 1,
            1,
            2,
            i64::MIN,
            3,
            0,
            2,
            0,
            1,
            3,
        ])
    );
}

#[allow(deprecated)] // This probe reads the legacy error object for byte parity.
fn diagnostic_roundtrip(instance: &wasmi::Instance, store: &mut Store<()>) {
    let cases: &[(&str, &[u8])] = &[
        ("provenance", b"local victim=nil\nreturn victim.key"),
        ("method receiver", b"local a={b=1}; a.b:m()"),
        ("local environment", b"local _ENV={}; return a+1"),
        ("fractional left operand", b"local a=1.2; local b=2; return a&b"),
        ("parallel assignment", b"local a=3; local b={}; a.x,b.x=1,2"),
        (
            "renamed native",
            b"local f=math.sin; math.sin=nil; math.zzz=f; local _,e=pcall(f,{}); error(e,0)",
        ),
        (
            "error level 2",
            b"local function outer() local function inner() error('boom',2) end inner() end outer()",
        ),
        ("argument", b"table.concat({1}, {})"),
        (
            "compile through load",
            b"local f,e=load('local =', '@inner.lua'); error(e,0)",
        ),
    ];
    for &(name, source) in cases {
        let mut chunk = moonseed::compile(source).unwrap();
        chunk.set_chunk_name(b"@diag.lua");
        let mut native =
            Runtime::load_chunk(Config::default(), HostRegistry::proof(), &chunk).unwrap();
        native.install_standard().unwrap();
        let before = native.snapshot().unwrap();
        let mut journal = Journal::new();
        assert!(
            matches!(
                native.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                StepOutcome::LuaError(_)
            ),
            "{name}"
        );
        let (_, moonseed::HostValue::String(expected)) = native.lua_error().unwrap() else {
            panic!("{name}: native error was not a string");
        };
        assert!(
            !expected.windows(3).any(|part| {
                part[0] == b'0' && matches!(part[1], b'x' | b'X') && part[2].is_ascii_hexdigit()
            }),
            "{name}: address-like diagnostic"
        );

        call0(instance, store, "moonseed_restore_clear");
        push_bytes(instance, store, &before);
        assert_eq!(
            call1(instance, store, "moonseed_restore_finish", 1),
            0,
            "{name}"
        );
        assert_eq!(call0(instance, store, "moonseed_snapshot"), 0, "{name}");
        let wasm_before = pull_snapshot(instance, store);
        let mut native_from_wasm =
            Runtime::from_snapshot(&wasm_before, &HostRegistry::proof(), 1).unwrap();
        assert_eq!(
            call1(instance, store, "moonseed_run", u64::MAX),
            4,
            "{name}"
        );
        assert_eq!(
            call0(instance, store, "moonseed_diag_error_capture"),
            1,
            "{name}"
        );
        let len = call0(instance, store, "moonseed_diag_error_len") as usize;
        let byte = instance
            .get_typed_func::<u32, u32>(&*store, "moonseed_diag_error_byte")
            .unwrap();
        let mut actual = Vec::with_capacity(len);
        for index in 0..len {
            actual.push(byte.call(&mut *store, index as u32).unwrap() as u8);
        }
        assert_eq!(actual, expected, "{name}: wasm error bytes");
        assert!(
            matches!(
                native_from_wasm
                    .run_until_terminal(u64::MAX, &mut Journal::new())
                    .unwrap(),
                StepOutcome::LuaError(_)
            ),
            "{name}"
        );
        assert_eq!(
            native_from_wasm.lua_error().unwrap().1,
            moonseed::HostValue::String(expected),
            "{name}: native restore of wasm checkpoint"
        );
    }
}

fn native_finish_from_fresh() -> moonseed::Observation {
    let mut runtime = Runtime::boot_canonical(Config::default(), HostRegistry::proof()).unwrap();
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let observation = runtime.observe(&journal).unwrap();
    observation.assert_canonical_shape().unwrap();
    observation
}

#[test]
#[ignore = "wasm_roundtrip"]
fn wasm_large_roundtrip() {
    let started = std::time::Instant::now();
    let path = std::env::var("MOONSEED_WASM_PROBE")
        .expect("MOONSEED_WASM_PROBE must point at the built wasm probe");
    let bytes = std::fs::read(&path).expect("read wasm probe");
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).expect("parse wasm");
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine)
        .instantiate(&mut store, &module)
        .expect("instantiate")
        .start(&mut store)
        .expect("start");
    let memory = instance
        .get_memory(&store, "memory")
        .expect("linear memory");
    let allocate = instance
        .get_typed_func::<u32, u32>(&store, "moonseed_restore_alloc")
        .unwrap();
    let finish = instance
        .get_typed_func::<(), i64>(&store, "moonseed_large_finish")
        .unwrap();

    let native = moonseed::large_exchange_snapshot().expect("native large snapshot");
    assert!(native.len() > 2 << 20);
    let usage = Runtime::from_snapshot(&native, &HostRegistry::proof(), 1)
        .expect("native restore")
        .memory();
    assert!(usage.objects >= 40000);
    let expected = moonseed::large_exchange_finish(&native).expect("native finish");
    assert_ne!(expected, i64::MIN);
    let pointer = allocate.call(&mut store, native.len() as u32).unwrap();
    assert_ne!(pointer, 0);
    memory.write(&mut store, pointer as usize, &native).unwrap();
    assert_eq!(finish.call(&mut store, ()).unwrap(), expected);

    assert_eq!(call0(&instance, &mut store, "moonseed_large_snapshot"), 0);
    let len = call0(&instance, &mut store, "moonseed_snapshot_len") as usize;
    assert_eq!(len, native.len());
    let pointer = call0(&instance, &mut store, "moonseed_snapshot_ptr") as usize;
    let mut wasm = vec![0; len];
    memory.read(&store, pointer, &mut wasm).unwrap();
    assert_eq!(wasm, native, "large snapshots differ across targets");
    let pointer = allocate.call(&mut store, len as u32).unwrap();
    assert_ne!(pointer, 0);
    memory.write(&mut store, pointer as usize, &wasm).unwrap();
    let wasm_finish = finish.call(&mut store, ()).unwrap();
    assert_ne!(wasm_finish, i64::MIN);
    assert_eq!(moonseed::large_exchange_finish(&wasm).unwrap(), wasm_finish);
    assert_eq!(wasm_finish, expected);
    eprintln!(
        "large exchange: {} objects, {} logical bytes, {len} snapshot bytes, {:?}",
        usage.objects,
        usage.logical_bytes,
        started.elapsed()
    );
}

fn assert_fields(
    instance: &wasmi::Instance,
    store: &mut Store<()>,
    expected: &moonseed::Observation,
) {
    let mut field = |id: u32| call_field(instance, store, id);
    assert_eq!(field(0), expected.tag);
    assert_eq!(field(1), expected.mark);
    assert_eq!(field(2), expected.inc_result);
    assert_eq!(field(3), expected.get_result);
    assert_eq!(field(4), expected.yielded);
    assert_eq!(field(5), expected.upvalue);
    assert_eq!(field(6) as u64, expected.a_id);
    assert_eq!(field(7) as u64, expected.b_id);
    assert_eq!(field(8) as u64, expected.a_b);
    assert_eq!(field(9) as u64, expected.b_a);
    assert_eq!(field(10) as u64, expected.inc_upvalue);
    assert_eq!(field(11) as u64, expected.get_upvalue);
    assert_eq!(field(12) as u64, expected.fuel_consumed);
    assert_eq!(field(13) as usize, expected.journal.len());
    assert_eq!(field(14) as u64, expected.journal[0].id.sequence);
    assert_eq!(field(15), expected.journal[0].outcome);
    assert_eq!(field(16) as u8, expected.yielder_status);
}

fn call0(instance: &wasmi::Instance, store: &mut Store<()>, name: &str) -> u32 {
    instance
        .get_typed_func::<(), u32>(&*store, name)
        .unwrap()
        .call(store, ())
        .unwrap()
}

fn call1(instance: &wasmi::Instance, store: &mut Store<()>, name: &str, arg: u64) -> u32 {
    instance
        .get_typed_func::<u64, u32>(&*store, name)
        .unwrap()
        .call(store, arg)
        .unwrap()
}

fn call_field(instance: &wasmi::Instance, store: &mut Store<()>, field: u32) -> i64 {
    instance
        .get_typed_func::<u32, i64>(&*store, "moonseed_field")
        .unwrap()
        .call(store, field)
        .unwrap()
}

fn push_bytes(instance: &wasmi::Instance, store: &mut Store<()>, bytes: &[u8]) {
    let push = instance
        .get_typed_func::<u32, ()>(&*store, "moonseed_restore_push_byte")
        .unwrap();
    for byte in bytes {
        push.call(&mut *store, u32::from(*byte)).unwrap();
    }
}

fn pull_snapshot(instance: &wasmi::Instance, store: &mut Store<()>) -> Vec<u8> {
    let len = call0(instance, store, "moonseed_snapshot_len") as usize;
    let byte = instance
        .get_typed_func::<u32, u32>(&*store, "moonseed_snapshot_byte")
        .unwrap();
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        out.push(byte.call(&mut *store, index as u32).unwrap() as u8);
    }
    out
}
