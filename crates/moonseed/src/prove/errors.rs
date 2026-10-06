//! Errors, protected calls, and the logical-heap quota (ADR 0024, ADR 0025).
//! The fixtures that match Lua 5.4.9 are in `NATIVE_FIXTURES`.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::host::{HostValue, LegacyCompletion, NativeCall, NativeOutcome, NativePolicy};

fn source_runtime(source: &[u8]) -> Runtime {
    let chunk = crate::compile(source).unwrap();
    boot_natives(&chunk.proto)
}

fn before_named_index_fault() -> Runtime {
    let mut chunk = crate::compile(b"local victim = nil\nreturn victim.key").unwrap();
    chunk.set_chunk_name(b"@diag.lua");
    let pc = chunk
        .proto
        .ops
        .iter()
        .position(|op| matches!(op, crate::opcode::Op::GetField { .. }))
        .unwrap() as u32;
    let mut runtime = Runtime::boot(
        Config::default(),
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    let mut journal = Journal::new();
    while crate::entry_frame_pc(&runtime) != Some(pc) {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    runtime
}

#[test]
fn named_fault_at_quota_uses_the_reserved_string() {
    let mut runtime = before_named_index_fault();
    runtime.heap_mut().gc.quota = runtime.heap().gc.used;
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Index));
    assert_eq!(
        runtime.lua_error(),
        Some((
            LuaFault::Index,
            HostValue::String(LuaFault::Index.text().as_bytes().to_vec())
        ))
    );
}

#[test]
fn long_diagnostic_names_obey_the_string_limit() {
    let identifier = "v".repeat(1 << 20);
    let key = "k".repeat(1 << 20);
    let sources = [
        format!("local {identifier}=nil; return {identifier}.q"),
        format!(
            "local t={}{{}}{}; return t{}['{key}'].q",
            "{a=".repeat(32),
            "}".repeat(32),
            ".a".repeat(32)
        ),
    ];
    for source in sources {
        let mut chunk = crate::compile(source.as_bytes()).unwrap();
        chunk.set_chunk_name(format!("@{}diag.lua", "a".repeat(1_000_000)).as_bytes());
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .unwrap();
        runtime.heap_mut().max_string = 512;
        assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Index));
        assert_eq!(
            runtime.lua_error(),
            Some((
                LuaFault::Index,
                HostValue::String(LuaFault::Index.text().as_bytes().to_vec())
            ))
        );
    }
}

#[test]
fn checkpoint_before_named_fault_preserves_its_error_string() {
    let mut straight = before_named_index_fault();
    let mut restored = restore(&straight);
    assert_eq!(
        finish(&mut straight),
        StepOutcome::LuaError(LuaFault::Index)
    );
    assert_eq!(
        finish(&mut restored),
        StepOutcome::LuaError(LuaFault::Index)
    );
    assert_eq!(straight.lua_error(), restored.lua_error());
    assert_eq!(
        straight.lua_error(),
        Some((
            LuaFault::Index,
            HostValue::String(
                b"diag.lua:2: attempt to index a nil value (local 'victim')".to_vec()
            )
        ))
    );
}

fn finish(runtime: &mut Runtime) -> StepOutcome {
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap()
}

fn restore(runtime: &Runtime) -> Runtime {
    Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap()
}

/// A fixed, evenly spaced sample of the frozen diagnostic generator. The
/// source and chunk columns are hex so this test needs no Python at runtime.
fn diagnostic_sample() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    fn decode(hex: &str) -> Vec<u8> {
        let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
        assert!(remainder.is_empty(), "odd sample hex length");
        pairs
            .iter()
            .map(|pair| {
                let digit = |byte: u8| (byte as char).to_digit(16).expect("sample hex digit") as u8;
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect()
    }
    include_str!("../../fixtures/diag/determinism-sample.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next().unwrap();
            let source = decode(fields.next().unwrap());
            let chunk = decode(fields.next().unwrap());
            assert!(fields.next().is_none(), "{id}: extra sample field");
            (id, source, chunk)
        })
        .collect()
}

fn lua_quoted(bytes: &[u8]) -> String {
    let mut quoted = String::from("\"");
    for byte in bytes {
        quoted.push_str(&format!("\\{byte:03}"));
    }
    quoted.push('"');
    quoted
}

fn diagnostic_runtime(id: &str, source: &[u8], name: &[u8], gc_mode: crate::GcMode) -> Runtime {
    // A compile-error corpus case is exercised through load(), so it has a
    // runtime error object and can take part in the same checkpoint matrix.
    let source = if id.starts_with("compile.") {
        format!(
            "local f,e=load({}, {}); if f then f() else error(e,0) end",
            lua_quoted(source),
            lua_quoted(name)
        )
        .into_bytes()
    } else {
        source.to_vec()
    };
    let mut chunk = crate::compile(&source).unwrap_or_else(|e| panic!("{id}: {e}"));
    chunk.set_chunk_name(name);
    let mut runtime = Runtime::boot(
        Config {
            gc_mode,
            ..Config::default()
        },
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    runtime.set_global_native("newud", "newud").unwrap();
    runtime.set_global_native("light", "light").unwrap();
    runtime
}

fn diagnostic_error(runtime: &mut Runtime, id: &str) -> (LuaFault, crate::LuaType, HostValue) {
    let error = runtime
        .lua_error()
        .unwrap_or_else(|| panic!("{id}: missing error"));
    let lua_type = match &error.1 {
        HostValue::Nil => crate::LuaType::Nil,
        HostValue::Boolean(_) => crate::LuaType::Boolean,
        HostValue::Integer(_) | HostValue::Number(_) => crate::LuaType::Number,
        HostValue::String(_) => crate::LuaType::String,
        HostValue::Object(object) => runtime
            .object(*object)
            .unwrap_or_else(|| panic!("{id}: missing error object"))
            .lua_type(),
        HostValue::Native(_) => crate::LuaType::Function,
        HostValue::LightUserdata(_) => crate::LuaType::Userdata,
    };
    if let HostValue::String(bytes) = &error.1 {
        // None of these frozen cases includes a hexadecimal source literal.
        // Reject every hex-looking ID, including short object handles.
        assert!(
            !bytes.windows(3).any(|window| {
                window[0] == b'0'
                    && matches!(window[1], b'x' | b'X')
                    && window[2].is_ascii_hexdigit()
            }),
            "{id}: address-like error: {}",
            String::from_utf8_lossy(bytes)
        );
    }
    (error.0, lua_type, error.1)
}

#[test]
fn diagnostic_errors_match_fuel_checkpoints_gc_and_fast_call_modes() {
    use crate::runtime::HotCoreMode;

    let cases = diagnostic_sample();
    assert_eq!(cases.len(), 320);
    for (id, source, name) in cases {
        let mut baseline = diagnostic_runtime(id, &source, &name, crate::GcMode::Incremental);
        assert!(
            matches!(finish(&mut baseline), StepOutcome::LuaError(_)),
            "{id}"
        );
        let expected = diagnostic_error(&mut baseline, id);
        for gc_mode in [crate::GcMode::Incremental, crate::GcMode::Generational] {
            for mode in [
                HotCoreMode::Full,
                HotCoreMode::NoFastCalls,
                HotCoreMode::Off,
            ] {
                for quantum in [1, 2, 3, 7, u64::MAX] {
                    let mut runtime = diagnostic_runtime(id, &source, &name, gc_mode);
                    runtime.hot_core = mode;
                    let mut journal = Journal::new();
                    for slice in 0..10_000 {
                        let before = if quantum == 1 {
                            Some(runtime.snapshot().unwrap())
                        } else {
                            None
                        };
                        match runtime.run(quantum, &mut journal).unwrap() {
                            StepOutcome::Paused(PauseReason::FuelExhausted) => {
                                runtime = restore(&runtime);
                                runtime.hot_core = mode;
                            }
                            StepOutcome::LuaError(_) => {
                                assert_eq!(
                                    diagnostic_error(&mut runtime, id),
                                    expected,
                                    "{id} {gc_mode:?} {mode:?} q={quantum}"
                                );
                                if let Some(bytes) = before {
                                    let mut restored = Runtime::from_snapshot(
                                        &bytes,
                                        &HostRegistry::proof(),
                                        runtime.effect_domain(),
                                    )
                                    .unwrap();
                                    restored.hot_core = mode;
                                    assert!(matches!(
                                        finish(&mut restored),
                                        StepOutcome::LuaError(_)
                                    ));
                                    assert_eq!(
                                        diagnostic_error(&mut restored, id),
                                        expected,
                                        "{id}: pre-fault restore"
                                    );
                                }
                                break;
                            }
                            other => panic!("{id} {gc_mode:?} {mode:?} q={quantum}: {other:?}"),
                        }
                        assert!(slice < 9_999, "{id}: step bound");
                    }
                }
            }
        }
    }
}

#[test]
fn a_protected_call_stays_in_progress_across_a_yield() {
    let spec = crate::program::pcall_yield_program();
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(print_line(&runtime), "41\ttrue\t2\n");
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
}

#[test]
fn a_message_handler_cannot_yield() {
    let spec = crate::program::handler_yield_program();
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(print_line(&runtime), "false\terror in error handling\n");
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
}

static HANDLER_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// A native message handler that records how many frames the failing
/// thread still has, and passes the error on.
fn depth_probe(call: &mut NativeCall<'_>) -> NativeOutcome {
    let active = call.heap.active.expect("active thread");
    let frames = call.heap.threads.get(active).expect("thread").frames.len();
    HANDLER_DEPTH.store(frames, Ordering::SeqCst);
    let error = call.arg(0);
    call.push(error);
    NativeOutcome::Ready
}

#[test]
fn the_message_handler_runs_before_the_stack_unwinds() {
    let mut registry = HostRegistry::proof();
    registry.register_native("probe", NativePolicy::VmLocal, depth_probe);
    let chunk = crate::compile(
        b"local f = function() local g = function() error('x', 0) end g() end \
          return xpcall(f, probe)",
    )
    .unwrap();
    let mut runtime = Runtime::boot(Config::default(), registry, &chunk.proto, false).unwrap();
    runtime.set_global_native("probe", "probe").unwrap();
    runtime.install_base().unwrap();
    assert_eq!(finish(&mut runtime), StepOutcome::Completed);
    assert_eq!(print_line(&runtime), "false\tx\n");
    // The chunk, xpcall's boundary, f, g, and the handler's boundary: g's
    // frame was still there when the handler ran.
    assert_eq!(HANDLER_DEPTH.load(Ordering::SeqCst), 5);
}

#[test]
fn unwinding_closes_upvalues_and_the_fixture_would_notice() {
    let chunk = crate::compile(&fixture("pcall_upvalue.lua")).unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "false\tstop\t10\t5\t99\t410\n");
    let mut broken = boot_natives(&chunk.proto);
    broken.skip_unwind_close = true;
    assert_eq!(finish(&mut broken), StepOutcome::Completed);
    assert_ne!(print_line(&broken), print_line(&runtime));
}

#[test]
fn memory_errors_are_catchable_and_skip_the_message_handler() {
    let config = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"local keep = {} \
          local grow = function() for i = 1, 100000 do keep[i] = 'x' .. i end end \
          local ok, err = pcall(grow) \
          keep = nil \
          local handled = false \
          local okx, errx = xpcall(function() local k = {} for i = 1, 100000 do k[i] = 'y' .. i end end, \
              function(m) handled = true return m end) \
          local after = {} for i = 1, 100 do after[i] = i end \
          return ok, err, okx, errx, handled, #after",
    )
    .unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    assert_eq!(finish(&mut runtime), StepOutcome::Completed);
    assert_eq!(
        print_line(&runtime),
        "false\tnot enough memory\tfalse\tnot enough memory\tfalse\t100\n"
    );
}

#[test]
fn many_legal_strings_cannot_pass_the_quota() {
    // Each string is under the 1 MiB bound; together they are far over an
    // 8 MiB logical heap.
    let config = Config {
        max_logical_heap: 8 * 1024 * 1024,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"local s = 'x' for i = 1, 16 do s = s .. s end \
          local keep = {} for i = 1, 1000 do keep[i] = s .. i end return #keep",
    )
    .unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    assert_eq!(
        finish(&mut runtime),
        StepOutcome::LuaError(LuaFault::Memory)
    );
    assert!(runtime.memory().logical_bytes <= 8 * 1024 * 1024);
    assert_eq!(
        runtime.lua_error(),
        Some((
            LuaFault::Memory,
            HostValue::String(b"not enough memory".to_vec())
        ))
    );
}

#[test]
fn closures_and_upvalues_count_against_the_quota() {
    // A chain of closures, each holding the one before through an upvalue,
    // with no table or string: the 64 KiB quota stops it long before the
    // 10,000-object limit.
    let config = Config {
        max_logical_heap: 64 * 1024,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"local prev for i = 1, 100000 do local p = prev prev = function() return p end end return 1",
    )
    .unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    assert_eq!(
        finish(&mut runtime),
        StepOutcome::LuaError(LuaFault::Memory)
    );
    assert!(runtime.memory().logical_bytes <= 64 * 1024);
    assert!(runtime.memory().objects < 5_000);
}

/// A call must decline before changing its window when the callee's slots
/// alone cross the quota. No allocating Lua opcode is needed in the callee.
#[test]
fn a_lua_callee_window_cannot_pass_the_quota() {
    use crate::opcode::Op;
    use crate::program::ProtoSpec;
    let callee = ProtoSpec {
        max_reg: 255,
        ops: vec![Op::Return { base: 0, count: 0 }],
        byte_consts: Vec::new(),
        captures: Vec::new(),
        children: Vec::new(),
        params: 0,
        vararg: false,
        debug: None,
    };
    let spec = ProtoSpec {
        max_reg: 1,
        ops: vec![
            Op::MakeClosure { dst: 0, child: 0 },
            Op::Call {
                func: 0,
                nargs: 0,
                nresults: 0,
            },
            Op::Return { base: 0, count: 0 },
        ],
        children: vec![callee],
        byte_consts: Vec::new(),
        captures: Vec::new(),
        params: 0,
        vararg: false,
        debug: None,
    };
    let mut runtime = Runtime::boot(
        Config {
            auto_gc: false,
            ..Config::default()
        },
        HostRegistry::proof(),
        &spec,
        false,
    )
    .unwrap();
    assert!(matches!(
        runtime.run(1, &mut Journal::new()).unwrap(),
        StepOutcome::Paused(_)
    ));
    let bytes = runtime.snapshot().unwrap();
    let used = runtime.memory().logical_bytes;
    let quota = used + 255 * crate::heap::cost::STACK_SLOT - 1;
    let boot = || {
        Runtime::from_snapshot_with_limits(
            &bytes,
            &HostRegistry::proof(),
            1,
            crate::runtime::Limits {
                max_logical_heap: quota,
                ..crate::runtime::Limits::default()
            },
        )
        .unwrap()
    };
    fast_slow_equivalent(boot, pair_results);
    let mut runtime = boot();
    assert_eq!(
        finish(&mut runtime),
        StepOutcome::LuaError(LuaFault::Memory)
    );
    assert!(runtime.memory().logical_bytes <= quota);
    let thread = runtime
        .heap()
        .threads
        .get(runtime.heap().entry.unwrap())
        .unwrap();
    assert_eq!(thread.charged_slots, 1, "declined callee charged no slots");
}

#[test]
fn external_native_faults_are_lua_errors() {
    // `mark` is an External native; a string argument makes it fault.
    let mut runtime = source_runtime(b"mark('x') return 7");
    assert_eq!(
        finish(&mut runtime),
        StepOutcome::LuaError(LuaFault::Native)
    );
    let chunk =
        crate::compile(b"local ok = pcall(mark, 'x') local ok2 = pcall(mark, 5) return ok, ok2")
            .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "false\ttrue\n");
    quantum_and_checkpoints_with(boot_natives, &chunk.proto, pair_results);
}

#[test]
fn a_waiting_native_can_complete_with_values_or_an_error() {
    let runtime = run_to_wait(b"local ok, e = pcall(park) return ok, e");
    let mut restored = restore(&runtime);
    restored
        .complete_legacy(
            WaitKey(1),
            LegacyCompletion::Error(HostValue::String(b"boom".to_vec())),
        )
        .unwrap();
    assert_eq!(finish(&mut restored), StepOutcome::Completed);
    assert_eq!(print_line(&restored), "false\tboom\n");

    let runtime = run_to_wait(b"return pcall(park)");
    let mut restored = restore(&runtime);
    restored
        .complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![
                HostValue::Integer(1),
                HostValue::String(b"two".to_vec()),
            ]),
        )
        .unwrap();
    assert_eq!(finish(&mut restored), StepOutcome::Completed);
    assert_eq!(print_line(&restored), "true\t1\ttwo\n");

    // Unprotected, the host's error reaches the host.
    let mut runtime = run_to_wait(b"park() return 1");
    runtime
        .complete_legacy(WaitKey(1), LegacyCompletion::Error(HostValue::Integer(9)))
        .unwrap();
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Error));
    assert_eq!(
        runtime.lua_error(),
        Some((LuaFault::Error, HostValue::Integer(9)))
    );
}

#[test]
fn an_unprotected_error_fails_the_thread_and_keeps_its_object() {
    let mut runtime = source_runtime(b"local t = { x = 1 } error(t)");
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Error));
    let Some((LuaFault::Error, HostValue::Object(id))) = runtime.lua_error() else {
        panic!("{:?}", runtime.lua_error());
    };
    runtime.collect();
    assert!(runtime.contains_id(id), "the failed thread keeps its error");
    // It stays failed, and a snapshot of it restores failed.
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Error));
    let mut restored = restore(&runtime);
    assert_eq!(
        finish(&mut restored),
        StepOutcome::LuaError(LuaFault::Error)
    );
    assert_eq!(restored.lua_error(), runtime.lua_error());

    let mut runtime = source_runtime(b"local t = {} return t + 1");
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Arith));
    assert_eq!(
        runtime.lua_error(),
        Some((
            LuaFault::Arith,
            HostValue::String(
                b"?:1: attempt to perform arithmetic on a table value (local 't')".to_vec()
            )
        ))
    );
    assert_eq!(finish(&mut runtime), StepOutcome::LuaError(LuaFault::Arith));
}

fn unwinding(runtime: &Runtime) -> bool {
    let heap = runtime.heap();
    heap.active
        .and_then(|active| heap.threads.get(active))
        .is_some_and(|thread| thread.unwind.is_some())
}

#[test]
fn snapshots_mid_unwind_restore_and_finish_the_same() {
    let source =
        b"local f f = function(n) if n == 0 then error('deep', 0) end return f(n - 1) + 1 end \
                   local ok, e = pcall(f, 5) return ok, e";
    let mut runtime = source_runtime(source);
    let mut journal = Journal::new();
    while !unwinding(&runtime) {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    let raised = runtime.snapshot().unwrap();
    // Past the target search and two frame pops: no fuel quantum stops
    // there, so a test hook takes the steps.
    for _ in 0..3 {
        runtime.unwind_one_step().unwrap();
    }
    assert!(unwinding(&runtime));
    let popping = runtime.snapshot().unwrap();
    for bytes in [raised, popping] {
        let mut restored =
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        assert_eq!(finish(&mut restored), StepOutcome::Completed);
        assert_eq!(print_line(&restored), "false\tdeep\n");
    }
}

#[test]
fn restore_refuses_impossible_error_states() {
    use crate::snapshot::{BoundaryImage, EncValue};
    let source =
        b"local f f = function(n) if n == 0 then error('deep', 0) end return f(n - 1) + 1 end \
                   local ok, e = pcall(f, 3) return ok, e";
    let mut runtime = source_runtime(source);
    let mut journal = Journal::new();
    while !unwinding(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    runtime.unwind_one_step().unwrap();
    let domain = runtime.effect_domain();
    let check = |change: &dyn Fn(&mut crate::snapshot::Image), expected: SnapshotError| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain),
            expected,
        );
    };
    let entry = |image: &mut crate::snapshot::Image| -> usize {
        image
            .threads
            .iter()
            .position(|thread| thread.unwind.is_some())
            .unwrap()
    };
    // The unwind targets a Lua frame, then a frame that does not exist.
    check(
        &|image| {
            let t = entry(image);
            image.threads[t].unwind.as_mut().unwrap().phase =
                crate::heap::UnwindPhase::Popping { target: Some(0) };
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| {
            let t = entry(image);
            image.threads[t].unwind.as_mut().unwrap().phase =
                crate::heap::UnwindPhase::Popping { target: Some(400) };
        },
        SnapshotError::InvalidStructure,
    );
    // The error object names nothing.
    check(
        &|image| {
            let t = entry(image);
            image.threads[t].unwind.as_mut().unwrap().error =
                Some((LuaFault::Error.tag(), EncValue::Table(9_999_999)));
        },
        SnapshotError::DanglingReference,
    );
    // The protected call's results would not start at its `pcall` slot.
    check(
        &|image| {
            let t = entry(image);
            for frame in &mut image.threads[t].frames {
                if let Some(BoundaryImage::Protect { func, .. }) = &mut frame.boundary {
                    *func += 1;
                }
            }
        },
        SnapshotError::InvalidStructure,
    );
    // A message-handler frame with no xpcall handler behind it.
    check(
        &|image| {
            let t = entry(image);
            let frames = &mut image.threads[t].frames;
            let last = frames.len() - 1;
            frames[last].boundary = Some(BoundaryImage::Handler {
                slot: frames[last].base - 1,
                protect: 1,
                target: 1,
                depth: 1,
                fault: LuaFault::Error.tag(),
            });
            frames[last].limit = frames[last].base;
        },
        SnapshotError::InvalidStructure,
    );
    // A failed thread that still has frames, or no error.
    check(
        &|image| {
            let t = entry(image);
            image.threads[t].status = crate::heap::Status::Failed.tag();
            image.threads[t].unwind = None;
            image.threads[t].error = Some((LuaFault::Error.tag(), EncValue::Nil));
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| {
            let t = entry(image);
            image.threads[t].status = crate::heap::Status::Failed.tag();
            image.threads[t].unwind = None;
            image.threads[t].frames.clear();
        },
        SnapshotError::InvalidStructure,
    );
    // Too few reserved error strings, or one that is not a string.
    check(
        &|image| {
            image.reserved.pop();
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| {
            let table = image.tables[0].id;
            image.reserved[0] = table;
        },
        SnapshotError::InvalidStructure,
    );
}

#[test]
fn a_memory_error_never_reaches_a_message_handler_state() {
    use crate::snapshot::BoundaryImage;
    let source = b"local ok, e = xpcall(function() error('x', 0) end, function(m) local a = 1 local b = 2 return m end) return ok, e";
    let mut runtime = source_runtime(source);
    let mut journal = Journal::new();
    let in_handler = |runtime: &Runtime| {
        let heap = runtime.heap();
        heap.threads.iter().any(|(_, _, thread)| {
            thread.frames.iter().any(|frame| {
                matches!(
                    frame.boundary(),
                    Some(crate::heap::Boundary::Handler { .. })
                )
            })
        })
    };
    while !in_handler(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    let good = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&good, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    assert_eq!(finish(&mut restored), StepOutcome::Completed);
    assert_eq!(print_line(&restored), "false\tx\n");
    let mut image = runtime.to_image().unwrap();
    for thread in &mut image.threads {
        for frame in &mut thread.frames {
            if let Some(BoundaryImage::Handler { fault, .. }) = &mut frame.boundary {
                *fault = LuaFault::Memory.tag();
            }
        }
    }
    let bytes = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidStructure,
    );
}

#[test]
fn pcall_cannot_catch_vm_corruption() {
    let mut runtime =
        source_runtime(b"local ok = pcall(function() local x = 1 return x + 1 end) return ok");
    let mut journal = Journal::new();
    // Step into the protected function: the chunk, the boundary, and it.
    loop {
        runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        let frames = heap.threads.get(heap.active.unwrap()).unwrap().frames.len();
        if frames == 3 {
            break;
        }
    }
    runtime.corrupt_top_frame();
    assert_eq!(runtime.run(u64::MAX, &mut journal), Err(VmError::Corrupt));
}

#[test]
fn varargs_survive_a_call_in_their_frame() {
    let spec = crate::program::vararg_after_call_program();
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(print_line(&runtime), "1\t2\t3\n");
}

#[test]
fn varargs_survive_a_protected_call() {
    for native in [false, true] {
        for keep in [false, true] {
            let spec = crate::program::vararg_pcall_program(native, keep);
            let runtime = finish_with(boot_natives, &spec);
            assert_eq!(
                print_line(&runtime),
                "1\t2\t3\n",
                "native {native} keep {keep}"
            );
            quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
        }
    }
}

#[test]
fn restore_refuses_a_running_frame_without_its_varargs() {
    let spec = crate::program::vararg_after_call_program();
    let mut runtime = boot_natives(&spec);
    let mut journal = Journal::new();
    let domain = runtime.effect_domain();
    let mut refused = 0;
    loop {
        let mut image = runtime.to_image().unwrap();
        for thread in &mut image.threads {
            if let Some(frame) = thread.frames.last()
                && frame.vararg_len > 0
            {
                // The extras sit just below `base`.
                let base = frame.base as usize;
                thread.stack.truncate(base - 1);
                refused += 1;
            }
        }
        let bytes = snapshot::encode(&image).unwrap();
        let restored = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain);
        if refused > 0 {
            expect_snapshot(restored, SnapshotError::InvalidStructure);
            break;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn stores_after_the_memory_is_freed_collect_first() {
    let config = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let run = |source: &[u8]| {
        let chunk = crate::compile(source).unwrap();
        let mut runtime =
            Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.install_base().unwrap();
        assert_eq!(finish(&mut runtime), StepOutcome::Completed);
        print_line(&runtime)
    };
    // After a caught memory error: a field store, an indexed store, and a
    // native `rawset` find the room the failed frames held.
    assert_eq!(
        run(b"local log, other = {}, {} \
              local grow = function() local keep = {} for i = 1, 100000 do keep[i] = i end end \
              local ok, err = pcall(grow) \
              log[1] = err other.x = 5 rawset(other, 1, 6) \
              return ok, log[1], other.x, other[1]"),
        "false\tnot enough memory\t5\t6\n"
    );
    // Without an error: the table filling the quota is dropped, and the
    // next store collects before it would fail.
    assert_eq!(
        run(b"local other = {} local keep = {} local i = 0 \
              local fill = function() i = i + 1 keep[i] = i end \
              while pcall(fill) do end \
              keep = nil other.x = 1 other[2] = 2 \
              return i > 1000, other.x, other[2]"),
        "true\t1\t2\n"
    );
}

#[test]
fn restore_refuses_a_ready_thread_that_still_waits() {
    for source in [
        &b"local ok, v = pcall(park) return ok, v"[..],
        b"local v = park() return v",
    ] {
        let runtime = run_to_wait(source);
        let mut image = runtime.to_image().unwrap();
        for thread in &mut image.threads {
            if thread.status == crate::heap::Status::Waiting.tag() {
                thread.status = crate::heap::Status::Ready.tag();
            }
        }
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    }
}

#[test]
fn a_completion_naming_nothing_is_a_host_error() {
    let mut runtime = run_to_wait(b"local ok, v = pcall(park) return ok, v");
    assert_eq!(
        runtime.complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![HostValue::Object(ObjectId(9_999_999))])
        ),
        Err(WaitError::InvalidValue)
    );
    assert_eq!(
        runtime.complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![HostValue::Native("no.such".into())])
        ),
        Err(WaitError::InvalidValue)
    );
    // The wait is still there.
    runtime
        .complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![HostValue::Integer(7)]),
        )
        .unwrap();
    assert_eq!(finish(&mut runtime), StepOutcome::Completed);
    assert_eq!(print_line(&runtime), "true\t7\n");
}

#[test]
fn a_failed_wait_leaves_no_wait_behind() {
    use crate::heap::{MetaCall, MetaPhase, Pending};
    for source in [
        &b"local ok, e = pcall(park) return ok, e"[..],
        b"local t = setmetatable({}, { __index = park }) local ok, e = pcall(function() return t.x end) return ok, e",
    ] {
        let mut runtime = run_to_wait(source);
        runtime
            .complete_legacy(
                WaitKey(1),
                LegacyCompletion::Error(HostValue::String(b"boom".to_vec())),
            )
            .unwrap();
        let waits = runtime.heap().threads.iter().any(|(_, _, thread)| {
            thread.frames.iter().any(|frame| {
                matches!(
                    frame.pending(),
                    Some(Pending::Waiting { .. } | Pending::NativeWaiting { .. })
                ) || matches!(
                    frame.meta(),
                    Some(MetaCall {
                        phase: MetaPhase::NativeWaiting { .. },
                        ..
                    })
                )
            })
        });
        assert!(!waits, "a frame still waits after the wait failed");
        assert_eq!(finish(&mut runtime), StepOutcome::Completed);
        assert_eq!(print_line(&runtime), "false\tboom\n");
    }
}

#[test]
fn a_global_the_quota_refuses_is_a_memory_limit() {
    let spec = crate::program::vararg_after_call_program();
    let used = boot_natives(&spec).memory().logical_bytes;
    for extra in 0..48 {
        let config = Config {
            max_logical_heap: used + 256 + extra,
            auto_gc: false,
            ..Config::default()
        };
        let mut runtime = Runtime::boot(config, HostRegistry::proof(), &spec, false).unwrap();
        let error = (0..100)
            .find_map(|i| runtime.set_global_native(&format!("g{i}"), "add").err())
            .unwrap();
        assert_eq!(error, VmError::MemoryLimit, "quota {}", used + 256 + extra);
    }
}

#[test]
fn restore_refuses_error_states_the_runtime_cannot_make() {
    use crate::snapshot::{BoundaryImage, Image};
    let check = |runtime: &Runtime, change: &dyn Fn(&mut Image)| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    };
    let waiting = run_to_wait(b"local ok, v = pcall(park) return ok, v");
    // Two classes share one reserved string; a reserved string has another
    // class's text; a prototype owns a reserved string.
    check(&waiting, &|image| image.reserved[1] = image.reserved[0]);
    check(&waiting, &|image| {
        image.reserved.swap(0, 1);
    });
    check(&waiting, &|image| {
        let constant = image
            .protos
            .iter()
            .find_map(|proto| proto.const_ids.first().copied())
            .unwrap();
        image.reserved[0] = constant;
    });
    // A protected call whose caller is not stopped on the matching `Call`.
    check(&waiting, &|image| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if matches!(frame.boundary, Some(BoundaryImage::Protect { .. })) {
                    frame.nresults = frame.nresults.wrapping_add(1);
                }
            }
        }
    });

    // An unwind that skips the nearest protected call.
    let mut nested = source_runtime(
        b"local ok, e = pcall(function() local a, b = pcall(error, 'x', 0) return b end) return ok, e",
    );
    let mut journal = Journal::new();
    while !unwinding(&nested) {
        nested.run(1, &mut journal).unwrap();
    }
    nested.unwind_one_step().unwrap();
    check(&nested, &|image| {
        for thread in &mut image.threads {
            let outer = thread
                .frames
                .iter()
                .position(|frame| matches!(frame.boundary, Some(BoundaryImage::Protect { .. })));
            if let (Some(unwind), Some(outer)) = (thread.unwind.as_mut(), outer) {
                unwind.phase = crate::heap::UnwindPhase::Popping {
                    target: Some(outer as u32),
                };
            }
        }
    });
    assert_eq!(finish(&mut nested), StepOutcome::Completed);
    assert_eq!(print_line(&nested), "true\tx\n");
}
