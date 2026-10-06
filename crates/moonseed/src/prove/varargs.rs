//! Varargs, the vararg frame layout, and the stack bound (ADR 0028). The
//! source fixtures that match Lua 5.4.9 are the `vararg_*` entries of
//! `NATIVE_FIXTURES`.

use super::generic_for::drive;
use super::*;
use crate::check;
use crate::host::{HostValue, LegacyCompletion};

/// A callee's frame must not reach a vararg caller's extras, however many
/// registers it has. Under the Phase 3.15 layout, which kept them above the
/// caller's registers, a callee of four registers or more cleared them.
#[test]
fn a_nested_call_does_not_overwrite_the_callers_extras() {
    for callee_regs in [1u8, 4, 20, 200] {
        let spec = crate::program::vararg_overlap_program(callee_regs);
        let runtime = finish_spec(&spec);
        assert_eq!(
            print_line(&runtime),
            "41\t42\n",
            "callee with {callee_regs} registers"
        );
        quantum_and_checkpoints(&spec, pair_results);
    }
}

fn compile_error(source: &str) -> crate::CompileError {
    match crate::compile(source.as_bytes()) {
        Err(error) => error,
        Ok(_) => panic!("compiled: {source}"),
    }
}

#[test]
fn dots_belong_to_the_function_they_are_directly_in() {
    // `...` of an enclosing function is not an upvalue.
    for (source, at) in [
        ("local f = function() return ... end", "..."),
        (
            "local f = function(...) local g = function() return ... end end",
            "...",
        ),
        ("local f = function(a) local t = { ... } end", "..."),
        ("local f = function() local x = (...) end", "..."),
    ] {
        let error = compile_error(source);
        assert_eq!(error.kind, crate::CompileErrorKind::Syntax, "{source}");
        assert!(
            error.message.contains("outside a vararg function"),
            "{source}"
        );
        let start = source.rfind(at).unwrap() as u32;
        assert_eq!(error.span.start, start, "{source}");
    }
    for source in [
        "local f = function(..., a) end",
        "local f = function(a, ..., b) end",
        "local f = function(,) end",
        "local f = function(a,) end",
        "local f = function(... a) end",
    ] {
        assert_eq!(
            compile_error(source).kind,
            crate::CompileErrorKind::Syntax,
            "{source}"
        );
    }
    // A chunk is vararg; a nested vararg function has its own `...`.
    let chunk =
        crate::compile(b"local g = function(...) return select('#', ...) end return g(1, 2), ...")
            .unwrap();
    assert!(chunk.proto.vararg);
    assert!(chunk.proto.children[0].vararg);
    assert!(
        !crate::compile(b"local g = function(a) end")
            .unwrap()
            .proto
            .children[0]
            .vararg
    );
    // The same source compiles to the same prototype.
    let source =
        b"local f = function(a, b, ...) local x, y = ... return { ..., a } end return f(...)";
    assert_eq!(
        crate::compile(source).unwrap().proto,
        crate::compile(source).unwrap().proto
    );
}

#[test]
fn restore_refuses_extras_a_prototype_cannot_have() {
    // `Vararg` and `VarargLen` only in a vararg prototype.
    let mut chunk = crate::compile(b"local f = function(...) return ... end return f(1)").unwrap();
    chunk.proto.children[0].vararg = false;
    assert_eq!(
        check::validate(&chunk.proto).unwrap_err().kind,
        crate::CompileErrorKind::InvalidProgram
    );
    match restore_booted(&chunk.proto) {
        Err(error) => assert_eq!(error, SnapshotError::InvalidBytecode),
        Ok(_) => panic!("restored a non-vararg prototype reading extras"),
    }
    // A waiting frame's extras must fit below its base and be on the stack.
    let runtime = run_to_wait(b"local f = function(...) park() return ... end return f(1, 2)");
    let refuse = |edit: &dyn Fn(&mut crate::snapshot::ThreadImage)| {
        let mut image = runtime.to_image().unwrap();
        let thread = image
            .threads
            .iter_mut()
            .find(|thread| thread.frames.iter().any(|frame| frame.vararg_len > 0))
            .unwrap();
        edit(thread);
        let bytes = snapshot::encode(&image).unwrap();
        assert!(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                .is_err()
        );
    };
    refuse(&|thread| {
        let frame = thread.frames.last_mut().unwrap();
        frame.vararg_len = frame.base + 1;
    });
    refuse(&|thread| {
        let base = thread.frames.last().unwrap().base;
        thread.stack.truncate(base as usize - 1);
    });
    // Extras on a prototype that takes none.
    let runtime = run_to_wait(b"local f = function(a) park() end f(1)");
    let mut image = runtime.to_image().unwrap();
    for thread in &mut image.threads {
        if let Some(frame) = thread.frames.last_mut()
            && frame.base > 1
        {
            frame.vararg_len = 1;
        }
    }
    let bytes = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidStructure,
    );
}

fn with_metatable(runtime: &Runtime) -> usize {
    runtime
        .heap()
        .tables
        .iter()
        .filter(|(_, _, table)| table.metatable.is_some())
        .count()
}

fn restore(runtime: &Runtime) -> Runtime {
    Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap()
}

/// `f()`, `f(nil)`, and `f(nil, nil)` differ: the count of extras is frame
/// state, kept exactly through a checkpoint.
#[test]
fn the_count_of_extras_is_exact_frame_state() {
    for (args, count) in [
        ("", 0u32),
        ("nil", 1),
        ("nil, nil", 2),
        ("1, nil, 3, nil", 4),
    ] {
        let source = format!(
            "local f = function(...) park() return select('#', ...), ... end return f({args})"
        );
        let runtime = run_to_wait(source.as_bytes());
        let waiting = |runtime: &Runtime| {
            let heap = runtime.heap();
            let entry = heap.threads.get(heap.entry.unwrap()).unwrap();
            entry.frames.last().unwrap().vararg_len
        };
        assert_eq!(waiting(&runtime), count, "{args}");
        let mut restored = restore(&runtime);
        assert_eq!(waiting(&restored), count, "{args}");
        restored.complete_wait(WaitKey(1), 0).unwrap();
        restored
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        let values: Vec<&str> = args.split(", ").filter(|arg| !arg.is_empty()).collect();
        let mut line = count.to_string();
        for value in values {
            line.push('\t');
            line.push_str(value);
        }
        assert_eq!(print_line(&restored), format!("{line}\n"), "{args}");
    }
}

/// A Lua-visible yield inside the vararg frame, and a yielding `__close`
/// between `return ...` and the return. Lua 5.4.9 gives the same line.
#[test]
fn a_yield_and_a_yielding_close_keep_the_extras() {
    let spec = crate::program::vararg_coroutine_program();
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(print_line(&runtime), "a\t1\tnil\t3\ta\tnil\n");
    let expected = pair_results(&runtime);
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
    collect_every_safe_point_with(boot_natives, &spec, &expected);
}

#[test]
fn waits_keep_the_extras() {
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        (
            b"local f = function(a, ...) local w = park() return w, a, ... end return f(1, nil, 3)",
            vec![back(vec![HostValue::Integer(9)])],
            "9\t1\tnil\t3\n",
        ),
        // `return ...` through a `__close` that waits.
        (
            b"local f = function(...) local x <close> = setmetatable({}, { __close = park }) \
              return ... end return f(1, nil, 3)",
            vec![back(vec![])],
            "1\tnil\t3\n",
        ),
        // The extras passed on to a native that waits.
        (
            b"local f = function(...) local n = select('#', park(...)) return n, ... end \
              return f('a', nil)",
            vec![back(vec![HostValue::Nil, HostValue::Nil, HostValue::Nil])],
            "3\ta\tnil\n",
        ),
    ];
    for (source, answers, line) in cases {
        let (straight, fuel) = drive(source, &answers, false);
        assert_eq!(straight, line, "{}", String::from_utf8_lossy(source));
        assert_eq!(
            drive(source, &answers, true),
            (straight, fuel),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn extras_are_roots_while_their_frame_lives() {
    // A table held only by `...` survives collections during a nested call
    // and a wait.
    let mut runtime = run_to_wait(
        b"local f = function(...) local g = function() local junk = {} return 1 end g() \
          park() return (...)[1] end return f(setmetatable({ 7 }, {}))",
    );
    runtime.collect();
    assert_eq!(with_metatable(&runtime), 1);
    let mut runtime = restore(&runtime);
    runtime.collect();
    runtime.complete_wait(WaitKey(1), 0).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "7\n");
    // Once the frame has returned, they are garbage.
    for source in [
        &b"local f = function(...) return select('#', ...) end \
           local n = f(setmetatable({}, {}), setmetatable({}, {})) park() return n"[..],
        // A function without `...` drops extra arguments at the call.
        b"local f = function(a) park() return a end \
          return f(1, setmetatable({}, {}), setmetatable({}, {}))",
    ] {
        let mut runtime = run_to_wait(source);
        runtime.collect();
        assert_eq!(
            with_metatable(&runtime),
            0,
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

/// A 1 MiB string passed three times, and 200 tables as extras.
#[test]
fn large_and_many_extras() {
    let tables: Vec<String> = (0..200).map(|i| format!("{{ {i} }}")).collect();
    let source = format!(
        "local s = 'x' for i = 1, 20 do s = s .. s end \
         local f = function(...) return select('#', ...), #(...) end \
         local g = function(...) local t = (select(select('#', ...), ...)) return select('#', ...), t[1] end \
         local n, len = f(s, s, s) \
         local m, last = g({}) \
         return n, len, m, last",
        tables.join(", ")
    );
    let runtime = run_source(source.as_bytes());
    assert_eq!(print_line(&runtime), "3\t1048576\t200\t199\n");
}

fn run_source(source: &[u8]) -> Runtime {
    finish_with(boot_natives, &crate::compile(source).unwrap().proto)
}

/// A stack that reaches its bound: the growth stops with a catchable
/// "stack overflow", and every state on the way checkpoints.
#[test]
fn the_stack_bound_stops_growth_and_every_state_checkpoints() {
    let locals: String = (0..150).map(|i| format!("local v{i} = n ")).collect();
    let cases = [
        (
            "local grow grow = function(...) return grow(1, ...) end \
             local ok, e = pcall(grow) return ok, e"
                .to_string(),
            "false\t?:1: stack overflow\n",
        ),
        (
            format!(
                "local f f = function(n) {locals} if n == 0 then return 0 end return f(n - 1) + v0 end \
                 local ok, e = pcall(f, 80) return ok, e"
            ),
            "false\t?:1: stack overflow\n",
        ),
    ];
    let config = Config {
        max_stack_slots: 4_000,
        ..Config::default()
    };
    for (source, line) in cases {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        fast_slow_equivalent(
            || {
                let mut runtime =
                    Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false)
                        .unwrap();
                runtime.install_base().unwrap();
                runtime
            },
            pair_results,
        );
        let mut runtime =
            Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.install_base().unwrap();
        let mut journal = Journal::new();
        let mut deepest = 0;
        loop {
            deepest = deepest.max(
                runtime
                    .heap()
                    .threads
                    .iter()
                    .map(|(_, _, thread)| thread.stack.len())
                    .max()
                    .unwrap_or(0),
            );
            runtime = restore(&runtime);
            match runtime.run(37, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(print_line(&runtime), line, "{source}");
        assert!(deepest > 3_000 && deepest <= 4_000, "{deepest}: {source}");
    }
}

#[test]
fn a_snapshot_keeps_its_stack_bound_and_holds_threads_to_it() {
    let proto = crate::compile(b"return 1").unwrap().proto;
    for (asked, kept) in [(5u32, 1_024u32), (4_000, 4_000), (u32::MAX, 100_000)] {
        let config = Config {
            max_stack_slots: asked,
            ..Config::default()
        };
        let runtime = Runtime::boot(config, HostRegistry::proof(), &proto, false).unwrap();
        assert_eq!(runtime.to_image().unwrap().max_stack_slots, kept);
    }
    // About 2,400 slots in use while waiting.
    let locals: String = (0..20).map(|i| format!("local v{i} = n ")).collect();
    let runtime = run_to_wait(
        format!(
            "local f f = function(n) {locals} if n == 0 then park() return 0 end \
             return f(n - 1) + v0 end return f(100)"
        )
        .as_bytes(),
    );
    let heap = runtime.heap();
    let stack = heap.threads.get(heap.entry.unwrap()).unwrap().stack.len();
    assert!(stack > 2_000, "{stack}");
    assert_eq!(
        restore(&runtime).to_image().unwrap().max_stack_slots,
        50_000
    );
    // A bound the stack does not fit, and bounds outside the range.
    for bound in [1_024, 0, 100_001] {
        let mut image = runtime.to_image().unwrap();
        image.max_stack_slots = bound;
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::LimitExceeded,
        );
    }
}

/// A host completion with more values than the stack holds fails the call
/// with "stack overflow", which `pcall` catches.
#[test]
fn a_completion_past_the_stack_bound_fails_the_call() {
    let mut runtime =
        run_to_wait(b"local ok, e = pcall(function() return select('#', park()) end) return ok, e");
    runtime
        .complete_legacy(
            WaitKey(1),
            LegacyCompletion::Return(vec![HostValue::Nil; 60_000]),
        )
        .unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "false\tstack overflow\n");
}

/// Runs `source` with the proof natives under a stack bound of `bound`;
/// the printed results, or the host error.
fn run_bounded(source: &str, bound: u32) -> Result<String, VmError> {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let config = Config {
        max_stack_slots: bound,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    for name in PROOF_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
    runtime.install_base().unwrap();
    match runtime.run_until_terminal(u64::MAX, &mut Journal::new())? {
        StepOutcome::Completed => Ok(print_line(&runtime)),
        other => Ok(format!("{other:?}")),
    }
}

fn arg_list(count: usize) -> String {
    (1..=count)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn native_nils(call: &mut crate::host::NativeCall<'_>) -> crate::host::NativeOutcome {
    for _ in 0..60_000 {
        call.push_nil();
    }
    crate::host::NativeOutcome::Ready
}

/// From the milestone review: each of these reached the stack bound in a
/// step that did not turn it into a Lua error, and `run` returned
/// `Err(StackLimit)` to the host, or growth other than a frame used the
/// eighth kept for error handling.
#[test]
fn the_stack_bound_is_always_a_lua_error() {
    // Extras forwarded through three calls, not tail calls, fill the stack
    // near the bound; the message handler still has its room.
    let mut overflowed = 0;
    for pad in 195..=216 {
        let pads = "0, ".repeat(pad);
        let source = format!(
            "local h = function(e) return 'handled: ' .. e end \
             local g = function(...) return 1 end \
             local f3 = function(...) return (g({pads}...)) end \
             local f2 = function(...) return (f3(...)) end \
             local f1 = function(...) return (f2(...)) end \
             local ok, e = xpcall(f1, h, {}) return ok, e",
            arg_list(200)
        );
        let line = run_bounded(&source, 1_024).unwrap();
        assert!(
            line == "false\thandled: ?:1: stack overflow\n" || line == "true\t1\n",
            "{pad}: {line}"
        );
        overflowed += usize::from(line.starts_with("false"));
    }
    assert!(overflowed > 10, "{overflowed}");
    // A `<close>` in every frame of a recursion inside an unwind's close,
    // or inside a message handler, at every register width near the bound.
    for pad in 0..40 {
        let locals: String = (0..pad).map(|i| format!("local p{i} = 0 ")).collect();
        let deep = format!(
            "local mt = {{ __close = function() end }} \
             local deep deep = function(n) {locals} local x <close> = setmetatable({{}}, mt) \
               return deep(n + 1) + 1 end "
        );
        // Lua 5.4.9 gives these two messages for the same programs.
        for (tail, line) in [
            (
                "local outer = function() \
                   local y <close> = setmetatable({}, { __close = function() deep(0) end }) \
                   error('boom') end \
                 local ok, e = pcall(outer) return ok, e",
                "false\t?:1: stack overflow\n",
            ),
            (
                "local h = function(e) return deep(0) end \
                 local ok, e = xpcall(error, h, 'boom') return ok, e",
                "false\terror in error handling\n",
            ),
        ] {
            for bound in [1_024, 50_000] {
                let got = run_bounded(&format!("{deep}{tail}"), bound).unwrap();
                assert_eq!(got, line, "{pad} {bound}: {tail}");
            }
        }
    }
    // A multiple assignment's `__newindex` from a handler at the bound.
    for pad in (0..12).step_by(3) {
        let locals: String = (0..pad).map(|i| format!("local p{i} = 0 ")).collect();
        for depth in (1..200).step_by(3) {
            let source = format!(
                "local t = setmetatable({{}}, {{ __newindex = function(t, k, v) end }}) \
                 local deep deep = function(n) {locals} if n == 0 then t.a, t.b = 1, 2 return 0 end \
                   return deep(n - 1) + 1 end \
                 local h = function(e) return deep({depth}) end \
                 local ok, e = xpcall(error, h, 'x') return ok"
            );
            assert_eq!(
                run_bounded(&source, 1_024).unwrap(),
                "false\n",
                "{pad} {depth}"
            );
        }
    }
    // A native with more results than the stack holds, run in the step
    // (VM-local) or from a prepared state (External).
    for policy in [
        crate::host::NativePolicy::VmLocal,
        crate::host::NativePolicy::External,
    ] {
        let mut registry = HostRegistry::proof();
        registry.register_native("nils", policy, native_nils);
        let chunk = crate::compile(
            b"local ok, e = pcall(function() return select('#', nils()) end) return ok, e",
        )
        .unwrap();
        let mut runtime = Runtime::boot(Config::default(), registry, &chunk.proto, false).unwrap();
        runtime.set_global_native("nils", "nils").unwrap();
        runtime.install_base().unwrap();
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(
            print_line(&runtime),
            "false\t?:1: stack overflow\n",
            "{policy:?}"
        );
    }
    // Chunk arguments may fill only the ordinary part: a handler still runs.
    let chunk =
        crate::compile(b"local h = function(e) return e end return xpcall(error, h, 'x')").unwrap();
    let config = Config {
        max_stack_slots: 1_024,
        ..Config::default()
    };
    let room = 1_024 - 1_024 / 8 - usize::from(chunk.proto.max_reg);
    for (count, fits) in [(room, true), (room + 1, false)] {
        let args = vec![HostValue::Integer(1); count];
        let loaded =
            Runtime::load_chunk_with_args(config.clone(), HostRegistry::proof(), &chunk, &args);
        let Ok(mut runtime) = loaded else {
            assert!(!fits, "{count}");
            continue;
        };
        assert!(fits, "{count}");
        runtime.install_base().unwrap();
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(print_line(&runtime), "false\tx\n");
    }
}
