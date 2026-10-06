//! Proper tail calls (ADR 0029). The source fixtures that match Lua 5.4.9
//! are the `tail_*` entries of `NATIVE_FIXTURES` and `DEEP_FIXTURES`.

use super::generic_for::drive;
use super::*;
use crate::host::{HostValue, LegacyCompletion, NativeCall, NativeOutcome, NativePolicy};
use crate::opcode::{COUNT_OPEN, Op};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run_source(source: &[u8]) -> Runtime {
    finish_with(boot_natives, &crate::compile(source).unwrap().proto)
}

fn restore(runtime: &Runtime) -> Runtime {
    Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap()
}

/// The primary regression test: under the Phase 3.16 runtime this reached
/// the 1,000-frame limit and failed with "stack overflow".
#[test]
fn deep_tail_recursion_reproducer() {
    let source = b"local f\n\
        f = function(n, acc)\n\
            if n == 0 then return acc end\n\
            return f(n - 1, acc + 1)\n\
        end\n\
        return f(10000, 0)";
    let small = std::str::from_utf8(source).unwrap().replace("10000", "16");
    let small = crate::compile(small.as_bytes()).unwrap();
    fast_slow_equivalent(|| boot_natives(&small.proto), pair_results);
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(outcome, StepOutcome::Completed, "{outcome:?}");
    assert_eq!(print_line(&runtime), "10000\n");
}

/// A C callee in tail position leaves its Lua caller live. Repeated
/// `pcall` therefore consumes the ordinary 1,000-frame budget.
#[test]
fn native_tail_recursion_counts_the_lua_frame() {
    let runtime = run_source(
        b"local depth, overflow = 0, nil local f \
          f = function() depth = depth + 1 local ok, r = pcall(f) \
            if not ok then overflow = r return depth end return r end \
          return f(), overflow",
    );
    assert_eq!(print_line(&runtime), "500\t?:1: stack overflow\n");
}

#[test]
fn native_tail_callers_appear_in_debug_and_wrap_diagnostics() {
    for (source, expected) in [
        (
            &b"local function f() return debug.getinfo(1, 'S').what end return f()"[..],
            "Lua\n",
        ),
        (
            b"local function f() return debug.traceback('mark') end \
              local t = f() return t:find(\"in local 'f'\", 1, true) ~= nil",
            "true\n",
        ),
        (
            b"local w = coroutine.wrap(function() error('wrapped', 0) end) \
              local function f() return w() end local ok, e = pcall(f) return ok, e",
            "false\t?:1: wrapped\n",
        ),
        (
            b"local function f() return error('boom', 0) end \
              local ok, what = xpcall(f, function() return debug.getinfo(2, 'S').what end) \
              return ok, what",
            "false\tLua\n",
        ),
    ] {
        let chunk = crate::compile(source).unwrap();
        let runtime = finish_with(
            |spec| {
                let mut runtime = boot_spec(spec);
                runtime.install_standard().unwrap();
                runtime.install_debug().unwrap();
                runtime
            },
            &chunk.proto,
        );
        assert_eq!(
            print_line(&runtime),
            expected,
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

/// Every function's code in `source`, the chunk first, then its nested
/// functions depth first.
fn functions(source: &str) -> Vec<Vec<Op>> {
    fn walk(spec: &crate::program::ProtoSpec, out: &mut Vec<Vec<Op>>) {
        out.push(spec.ops.clone());
        for child in &spec.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&crate::compile(source.as_bytes()).unwrap().proto, &mut out);
    out
}

/// The tail calls in `ops`; each is followed by the `Return` of its open
/// window.
fn tail_calls(ops: &[Op]) -> usize {
    let mut count = 0;
    for (pc, op) in ops.iter().enumerate() {
        if let Op::TailCall { func, .. } = op {
            assert_eq!(
                ops[pc + 1],
                Op::Return {
                    base: *func,
                    count: COUNT_OPEN
                }
            );
            count += 1;
        }
    }
    count
}

#[test]
fn only_one_bare_call_returned_outside_close_scopes_is_a_tail_call() {
    let f = |ret: &str| {
        functions(&format!(
            "local g, a, b local f = function(...) {ret} end return 0"
        ))[1]
            .clone()
    };
    for ret in [
        "return g()",
        "return g(a, b)",
        "return g(...)",
        "return a.g(1)",
        "return a[b](g())",
        "return g(a)(b)",
        "if a then return g(a) end",
        "do local x <close> = nil end return g()",
        "local x = 1 local h = function() return x end return g(h)",
    ] {
        assert_eq!(tail_calls(&f(ret)), 1, "{ret}");
    }
    for ret in [
        "return (g())",
        "return 1, g()",
        "return g(), 1",
        "return 2 * g()",
        "g()",
        "return",
        "return g",
        "local x = g() return x",
        "local x <close> = nil return g()",
        "local x <close> = nil do return g() end",
        "do local x <close> = nil if a then return g() end end",
        "for k in g do return g(k) end",
    ] {
        assert_eq!(tail_calls(&f(ret)), 0, "{ret}");
    }
    // A `<close>` local of the enclosing function does not reach into a
    // nested one.
    let code =
        functions("local g local x <close> = nil local f = function() return g() end return g()");
    assert_eq!(tail_calls(&code[0]), 0);
    assert_eq!(tail_calls(&code[1]), 1);
}

/// Runs `source` one instruction at a time and gives the printed result,
/// the fuel, and the most frames, stack slots, and live objects the entry
/// thread held after any step.
fn peaks(source: &str) -> (String, u64, usize, usize, u32) {
    let mut runtime = boot_natives(&crate::compile(source.as_bytes()).unwrap().proto);
    let mut journal = Journal::new();
    let (mut frames, mut slots, mut objects) = (0, 0, 0);
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        let entry = heap.threads.get(heap.entry.unwrap()).unwrap();
        frames = frames.max(entry.frames.len());
        slots = slots.max(entry.stack.len());
        objects = objects.max(runtime.memory().objects);
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    (
        print_line(&runtime),
        runtime.fuel_consumed(),
        frames,
        slots,
        objects,
    )
}

/// 100,000 tail hops under the 1,000-frame limit. The most frames, stack
/// slots, and objects any step sees do not depend on the number of hops,
/// and a run of one instruction per slice uses the same fuel as one run.
#[test]
fn tail_recursion_runs_in_constant_space() {
    // A name, a source with `N` hops, and the line it prints.
    type Case = (&'static str, &'static str, fn(u32) -> String);
    let cases: [Case; 7] = [
        (
            "self",
            "local f f = function(n, acc) if n == 0 then return acc end \
             return f(n - 1, acc + 1) end return f(N, 0)",
            |n| n.to_string(),
        ),
        (
            "vararg",
            "local f f = function(n, ...) if n == 0 then return select('#', ...), ... end \
             return f(n - 1, ...) end return f(N, 1, nil, 3)",
            |_| "3\t1\tnil\t3".into(),
        ),
        (
            "fixed and vararg, small and large",
            "local f, g f = function(n, a, b) if n == 0 then return a + b end \
             return g(n - 1, a, b, 1, 2, 3) end \
             g = function(n, ...) local x1, x2, x3, x4, x5, x6, x7, x8 = ... \
             return f(n, ...) end return f(N, 2, 3)",
            |_| "5".into(),
        ),
        (
            "callable",
            "local mt = {} local a = setmetatable({}, mt) \
             local b = setmetatable({}, { __call = function(self, n) \
               if n == 0 then return 'done' end return a(n - 1) end }) \
             mt.__call = function(self, n) return b(n) end return a(N)",
            |_| "done".into(),
        ),
        (
            "mutual",
            "local even, odd \
             even = function(n) if n == 0 then return true end return odd(n - 1) end \
             odd = function(n) if n == 0 then return false end return even(n - 1) end \
             return even(N)",
            |_| "true".into(),
        ),
        (
            "native at the end",
            "local f f = function(n) if n == 0 then return many() end return f(n - 1) end \
             return f(N)",
            |_| "10\tnil\t30".into(),
        ),
        (
            "after 990 ordinary calls",
            "local loop loop = function(n, acc) if n == 0 then return acc end \
             return loop(n - 1, acc + 1) end \
             local nest nest = function(d) if d == 0 then return loop(N, 0) end \
             local r = nest(d - 1) return r end return nest(990)",
            |n| n.to_string(),
        ),
    ];
    for (name, source, line) in cases {
        let run = |n: u32| peaks(&source.replace('N', &n.to_string()));
        let (small, small_fuel, frames, slots, objects) = run(1_000);
        let (large, fuel, large_frames, large_slots, large_objects) = run(100_000);
        assert_eq!(small, format!("{}\n", line(1_000)), "{name}");
        assert_eq!(large, format!("{}\n", line(100_000)), "{name}");
        assert_eq!(
            (frames, slots, objects),
            (large_frames, large_slots, large_objects),
            "{name}"
        );
        assert!(small_fuel < fuel, "{name}");
        let source = source.replace('N', "100000");
        let straight = run_source(source.as_bytes());
        assert_eq!(straight.fuel_consumed(), fuel, "{name}");
        eprintln!("{name}: {frames} frames, {slots} slots, {objects} objects, fuel {fuel}");
    }
}

/// The same recursions, not in tail position, overflow and can be caught;
/// a `<close>` in scope keeps every frame, and each value is closed.
#[test]
fn calls_that_are_not_tail_calls_still_overflow() {
    for ret in [
        "return (f(n - 1))",
        "return f(n - 1), 1",
        "return 0 + f(n - 1)",
        "local r = f(n - 1) return r",
    ] {
        let source = format!(
            "local f f = function(n) if n == 0 then return 0 end {ret} end \
             local ok, e = pcall(f, 100000) return ok, e"
        );
        if ret == "return (f(n - 1))" {
            let chunk = crate::compile(source.as_bytes()).unwrap();
            fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);
        }
        assert_eq!(
            print_line(&run_source(source.as_bytes())),
            "false\t?:1: stack overflow\n",
            "{ret}"
        );
    }
    let runtime = run_source(
        b"local marked, closed = 0, 0 \
          local mt = { __close = function() closed = closed + 1 end } \
          local f f = function(n) local x <close> = setmetatable({}, mt) marked = marked + 1 \
            if n == 0 then return 0 end return f(n - 1) end \
          local ok, e = pcall(f, 100000) local deep = marked == closed \
          marked, closed = 0, 0 local r = f(50) \
          return ok, e, deep, marked, r, closed",
    );
    assert_eq!(
        print_line(&runtime),
        "false\t?:1: stack overflow\ttrue\t51\t0\t51\n"
    );
}

/// The replaced frame's captured locals close before its registers are
/// reused; without that, `saved` would read `g`'s register instead.
#[test]
fn captured_locals_close_before_the_frame_is_replaced() {
    let source = b"local saved \
        local g = function() return saved() end \
        local f = function() local x = 42 saved = function() return x end return g() end \
        return f()";
    assert_eq!(print_line(&run_source(source)), "42\n");
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    runtime.skip_tail_close = true;
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_ne!(print_line(&runtime), "42\n");
    // The same when the callee is a native.
    let source = b"local saved \
        local f = function() local x = 42 saved = function() return x end return add(1, 2) end \
        local three = f() return three, saved()";
    assert_eq!(print_line(&run_source(source)), "3\t42\n");
}

/// Compiled code puts the callee just above the arguments it moves; these
/// frames move a window that overlaps its destination in other ways: a
/// callee in the frame's first register, and a vararg frame's extras
/// copied up and moved back down past where they were.
#[test]
fn overlapping_windows_move_intact() {
    let args: Vec<String> = (1..=200).map(|i| i.to_string()).collect();
    let mut chunk = crate::compile(
        format!(
            "local f = function(a, b, c) end \
             local v = function(...) end \
             return f(second, 10, 20), f(second, nil, nil), f(add, 1, 2), \
               v(second, nil, 7), v(select, '#', {})",
            args.join(", ")
        )
        .as_bytes(),
    )
    .unwrap();
    // `f`: `return a(b, c)` with the callee in register 0. The code no
    // longer matches its lines, so they go.
    chunk.proto.children[0].debug = None;
    chunk.proto.children[1].debug = None;
    chunk.proto.children[0].ops = vec![
        Op::TailCall { func: 0, nargs: 2 },
        Op::Return {
            base: 0,
            count: COUNT_OPEN,
        },
    ];
    // `v`: `return (...)(select(2, ...))`, the callee the first extra.
    chunk.proto.children[1].ops = vec![
        Op::Vararg {
            dst: 0,
            count: COUNT_OPEN,
        },
        Op::TailCall {
            func: 0,
            nargs: COUNT_OPEN,
        },
        Op::Return {
            base: 0,
            count: COUNT_OPEN,
        },
    ];
    crate::check::validate(&chunk.proto).unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "20\tnil\t3\t7\t200\n");
    quantum_and_checkpoints_with(boot_natives, &chunk.proto, pair_results);
}

/// A native tail call keeps the Lua frame until its compiled `Return`;
/// the native's results still reach that frame's caller.
#[test]
fn native_tail_calls_answer_the_callers_call() {
    for (source, line) in [
        (
            &b"local f = function(...) return add(...) end return f(1, 2)"[..],
            "3\n",
        ),
        (
            b"local f = function() return many() end local a, b = f() return b, a",
            "nil\t10\n",
        ),
        (
            b"local f = function() return add(1, nil) end return (pcall(f))",
            "false\n",
        ),
        (
            b"local f = function() return error('boom', 0) end return pcall(f)",
            "false\tboom\n",
        ),
        (
            b"local g = function(a) return a + 1 end local f = function(x) return pcall(g, x) end \
              return f(1)",
            "true\t2\n",
        ),
        (
            b"local mt = {} local t = setmetatable({}, mt) mt.__call = t \
              local f = function() return t() end return (pcall(f))",
            "false\n",
        ),
        // Through a metamethod and inside a protected call.
        (
            b"local t = setmetatable({}, { __index = function(t, k) return add(k, 1) end }) \
              return t[41]",
            "42\n",
        ),
        (
            b"local f = function() return pcall(many) end return pcall(f)",
            "true\ttrue\t10\tnil\t30\n",
        ),
    ] {
        assert_eq!(
            print_line(&run_source(source)),
            line,
            "{}",
            String::from_utf8_lossy(source)
        );
    }
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let int = HostValue::Integer;
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        (
            b"local f = function(...) return park(...) end local a, b, c = f(1) return a, b, c",
            vec![back(vec![int(7), int(8)])],
            "7\t8\tnil\n",
        ),
        (
            b"local f = function() return park() end return pcall(f)",
            vec![LegacyCompletion::Error(HostValue::String(b"x".to_vec()))],
            "false\tx\n",
        ),
        (
            b"local t = setmetatable({}, { __len = function() return park() end }) return #t",
            vec![back(vec![int(5), int(6)])],
            "5\n",
        ),
        // Fewer arguments than the caller's own call passed (review).
        (
            b"local f = function() return park() end local r = f(1, 2, 3) return r",
            vec![back(vec![int(7)])],
            "7\n",
        ),
        (
            b"local n = 0 for x in function(s, c) return park() end do n = n + x break end \
              return n",
            vec![back(vec![int(7)])],
            "7\n",
        ),
        // The main chunk also calls the native from its own frame.
        (
            b"local t = setmetatable({}, {}) return park(t)",
            vec![back(vec![int(1), HostValue::Nil, int(3)])],
            "1\tnil\t3\n",
        ),
    ];
    for (source, answers, line) in cases {
        let (straight, fuel) = drive(source, &answers, false);
        assert_eq!(straight, line, "{}", String::from_utf8_lossy(source));
        assert_eq!(drive(source, &answers, true), (straight, fuel));
    }
    // While `park` waits, its tail-calling Lua frame is still present.
    let runtime =
        run_to_wait(b"local f = function(...) return park(...) end local r = f(1) return r");
    let heap = runtime.heap();
    let entry = heap.threads.get(heap.entry.unwrap()).unwrap();
    assert_eq!(entry.frames.len(), 2);
    assert!(matches!(
        entry.frames[1].pending(),
        Some(crate::heap::Pending::NativeWaiting { .. })
    ));
}

/// Images written before the C-callee correction have the native waiting
/// directly in the caller's `Call` slot. That shape is still executable.
#[test]
fn an_erased_native_tail_call_snapshot_still_runs() {
    let source = b"local f = function(x) return park(x) end local r = f(7) return r";
    let runtime = run_to_wait(source);
    let mut image = runtime.to_image().unwrap();
    let thread = image
        .threads
        .iter_mut()
        .find(|thread| thread.id == image.entry)
        .unwrap();
    assert_eq!(thread.frames.len(), 2);
    let tail = thread.frames.pop().unwrap();
    let tail_func = match functions(std::str::from_utf8(source).unwrap())[1][tail.pc as usize] {
        Op::TailCall { func, .. } => func,
        _ => panic!("expected tail call"),
    };
    let tail_slot = tail.base + u32::from(tail_func);
    let caller = &mut thread.frames[0];
    let caller_func =
        match functions(std::str::from_utf8(source).unwrap())[0][caller.pc as usize - 1] {
            Op::Call { func, .. } => func,
            _ => panic!("expected caller call"),
        };
    let caller_slot = caller.base + u32::from(caller_func);
    let window = thread.stack[tail_slot as usize..thread.top as usize].to_vec();
    for slot in &mut thread.stack[caller_slot as usize..] {
        *slot = crate::snapshot::EncValue::Nil;
    }
    thread.stack[caller_slot as usize..caller_slot as usize + window.len()]
        .clone_from_slice(&window);
    thread.top = caller_slot + window.len() as u32;
    caller.pc -= 1;
    caller.pending = tail.pending;
    caller.wait_request = tail.wait_request;
    let bytes = snapshot::encode(&image).unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    restored.complete_wait(WaitKey(1), 19).unwrap();
    assert_eq!(
        restored
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(print_line(&restored), "19\n");
}

/// An External native tail-called stops with its Lua frame intact, and
/// commits its effect once across a checkpoint there.
#[test]
fn an_external_tail_call_commits_once() {
    // `f` passes `mark` fewer arguments than it was given (review).
    let chunk =
        crate::compile(b"local f = function(x, y) return mark(x) end local a = f(5, 6) return a")
            .unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    while runtime.at_prepared().is_none() {
        runtime.run(1, &mut journal).unwrap();
    }
    {
        let heap = runtime.heap();
        assert_eq!(
            heap.threads.get(heap.entry.unwrap()).unwrap().frames.len(),
            2
        );
    }
    let mut restored = restore(&runtime);
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(outcome, StepOutcome::Completed);
    assert_eq!(journal.entries().len(), 1);
    let mut again = restore(&runtime);
    again.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(journal.entries().len(), 1);
    assert_eq!(print_line(&again), print_line(&restored));
}

static HANDLER_FRAMES: AtomicUsize = AtomicUsize::new(0);

fn frames_probe(call: &mut NativeCall<'_>) -> NativeOutcome {
    let active = call.heap.active.expect("active thread");
    let frames = call.heap.threads.get(active).expect("thread").frames.len();
    HANDLER_FRAMES.store(frames, Ordering::SeqCst);
    let error = call.arg(0);
    call.push(error);
    NativeOutcome::Ready
}

/// The message handler runs on the failing stack after a Lua tail call
/// replaced its caller: the chunk, the
/// `xpcall`, `g`, and the handler's boundary.
#[test]
fn a_message_handler_sees_the_erased_stack() {
    let mut registry = HostRegistry::proof();
    registry.register_native("probe", NativePolicy::VmLocal, frames_probe);
    for (source, frames) in [
        (
            &b"local g = function() error('boom', 0) end \
               local f = function() return g() end return xpcall(f, probe)"[..],
            4,
        ),
        (
            b"local g = function() error('boom', 0) end \
              local f = function() local r = g() return r end return xpcall(f, probe)",
            5,
        ),
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime =
            Runtime::boot(Config::default(), registry.clone(), &chunk.proto, false).unwrap();
        runtime.set_global_native("probe", "probe").unwrap();
        runtime.install_base().unwrap();
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(print_line(&runtime), "false\tboom\n");
        assert_eq!(HANDLER_FRAMES.load(Ordering::SeqCst), frames);
    }
}

/// A coroutine whose `g`, reached by `f`'s tail call, raises keeps the
/// frames that are really there: its body's and `g`'s.
#[test]
fn a_failed_coroutine_keeps_no_erased_frame() {
    let spec = crate::program::close_case_program(crate::program::CloseCase::TailError);
    let mut runtime = boot_natives(&spec);
    let mut journal = Journal::new();
    let failed = |runtime: &Runtime| {
        runtime
            .heap()
            .threads
            .iter()
            .any(|(_, _, thread)| thread.status == crate::heap::Status::Failed)
    };
    while !failed(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    let heap = runtime.heap();
    let (_, _, thread) = heap
        .threads
        .iter()
        .find(|(_, _, thread)| thread.status == crate::heap::Status::Failed)
        .unwrap();
    assert_eq!(thread.frames.len(), 2);
    for frame in &thread.frames {
        let proto = heap.closures.get(frame.closure).unwrap().proto;
        let ops = &heap.protos.get(proto).unwrap().ops;
        assert!(!ops.iter().any(|op| matches!(op, Op::TailCall { .. })));
    }
}

/// Hand-built code that tail-calls with a value still to close fails
/// closed at run time, and a snapshot of such a frame is refused.
#[test]
fn a_tail_call_never_skips_a_close() {
    let mut chunk = crate::compile(
        b"local g = function() return 1 end \
          local f = function() local x <close> = setmetatable({}, { __close = function() end }) \
            return g() end \
          return f()",
    )
    .unwrap();
    let ops = &mut chunk.proto.children[1].ops;
    let at = ops
        .iter()
        .position(|op| {
            matches!(
                op,
                Op::Call {
                    nresults: COUNT_OPEN,
                    ..
                }
            )
        })
        .unwrap();
    let Op::Call { func, nargs, .. } = ops[at] else {
        unreachable!()
    };
    ops[at] = Op::TailCall { func, nargs };
    crate::check::validate(&chunk.proto).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    let on_tail_call = |runtime: &Runtime| {
        let heap = runtime.heap();
        let entry = heap.threads.get(heap.entry.unwrap()).unwrap();
        let frame = entry.frames.last().unwrap();
        let proto = heap.closures.get(frame.closure).unwrap().proto;
        // The chunk's own `return f()` is a tail call with nothing to close.
        !entry.tbc.is_empty()
            && matches!(
                heap.protos.get(proto).unwrap().ops[frame.pc as usize],
                Op::TailCall { .. }
            )
    };
    while !on_tail_call(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    let bytes = snapshot::encode(&runtime.to_image().unwrap()).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidStructure,
    );
    assert_eq!(runtime.run(1, &mut journal), Err(VmError::Corrupt));
}

/// Restore refuses tail-call code without its window or its return, and
/// states no run reaches: a frame that does not answer the call below
/// it, a fabricated tail-call wait with the wrong call window, and a
/// first frame whose native call has lost its arguments.
#[test]
fn restore_refuses_impossible_tail_call_states() {
    for (ops, max_reg) in [
        (
            vec![
                Op::TailCall { func: 3, nargs: 0 },
                Op::Return {
                    base: 3,
                    count: COUNT_OPEN,
                },
            ],
            2,
        ),
        (
            vec![
                Op::TailCall { func: 0, nargs: 5 },
                Op::Return {
                    base: 0,
                    count: COUNT_OPEN,
                },
            ],
            2,
        ),
        (
            vec![
                Op::TailCall { func: 0, nargs: 0 },
                Op::Return { base: 0, count: 1 },
            ],
            2,
        ),
        (
            vec![
                Op::TailCall { func: 0, nargs: 0 },
                Op::Return {
                    base: 1,
                    count: COUNT_OPEN,
                },
            ],
            2,
        ),
        (vec![Op::TailCall { func: 0, nargs: 0 }], 2),
    ] {
        let spec = raw_spec(ops, max_reg);
        assert!(crate::check::validate(&spec).is_err());
        match restore_booted(&spec) {
            Err(error) => assert_eq!(error, SnapshotError::InvalidBytecode),
            Ok(_) => panic!("restored malformed tail call"),
        }
    }
    let tamper = |source: &[u8], edit: &dyn Fn(&mut crate::snapshot::ThreadImage)| {
        let runtime = run_to_wait(source);
        let mut image = runtime.to_image().unwrap();
        let entry = image.entry;
        let thread = image
            .threads
            .iter_mut()
            .find(|thread| thread.id == entry)
            .unwrap();
        edit(thread);
        let bytes = snapshot::encode(&image).unwrap();
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
    };
    // The untouched states restore.
    let replaced = &b"local g = function(a, ...) park() return a end \
                     local f = function(...) return g(...) end local r = f(1, 2, 3) return r"[..];
    let second = &b"local f = function() local x = park() return park() end \
                   local r = f() return r"[..];
    let first = &b"return park(1, 2)"[..];
    let tail_pc = functions(std::str::from_utf8(second).unwrap())[1]
        .iter()
        .position(|op| matches!(op, Op::TailCall { .. }))
        .unwrap() as u32;
    for source in [replaced, second, first] {
        assert!(tamper(source, &|_| {}).is_ok());
    }
    type Edit<'a> = &'a dyn Fn(&mut crate::snapshot::ThreadImage);
    let refused: [(&[u8], Edit<'_>); 6] = [
        // The replacement frame's extras no longer end at its call slot.
        (replaced, &|thread| thread.frames[1].vararg_len -= 1),
        (replaced, &|thread| {
            let frame = &mut thread.frames[1];
            frame.vararg_len += 1;
            frame.base += 1;
            frame.limit += 1;
        }),
        // It returns a different count than the call wants.
        (replaced, &|thread| thread.frames[1].nresults = 2),
        // The frame below is no longer just past its `Call`.
        (replaced, &|thread| thread.frames[0].pc += 1),
        // Changing a waiting ordinary call into a tail call leaves the
        // pending call window inconsistent.
        (second, &|thread| thread.frames[1].pc = tail_pc),
        // The first frame's native call has no callee window.
        (first, &|thread| thread.top = 0),
    ];
    for (source, edit) in refused {
        assert!(
            tamper(source, edit).is_err(),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

/// A tail hop is one instruction: `TailCall`, where a call and its return
/// were two. Fuel is otherwise the same instruction for instruction.
#[test]
fn a_tail_hop_costs_one_instruction() {
    let fuel = |ret: &str| {
        run_source(
            format!(
                "local f f = function(n) if n == 0 then return 0 end {ret} end return (f(500))"
            )
            .as_bytes(),
        )
        .fuel_consumed()
    };
    assert_eq!(fuel("return (f(n - 1))") - fuel("return f(n - 1)"), 500);
}

fn with_metatable(runtime: &Runtime) -> usize {
    runtime
        .heap()
        .tables
        .iter()
        .filter(|(_, _, table)| table.metatable.is_some())
        .count()
}

/// Nothing of the erased frame stays a root: its locals, the arguments a
/// callee without `...` drops, and a thread's first frame's registers
/// around the native it calls. Tail recursion that allocates on every hop
/// runs under a small heap quota.
#[test]
fn the_erased_frame_leaves_no_roots() {
    for source in [
        &b"local g = function(a) park() return a end \
           local f = function() return g(1, setmetatable({}, {}), setmetatable({}, {})) end \
           return f()"[..],
        b"local g = function() park() return 1 end \
          local f = function() local t = setmetatable({}, {}) return g() end return f()",
        b"local f = function() local t = setmetatable({}, {}) return park() end \
          local r = f() return r",
        b"local t = setmetatable({}, {}) return park()",
    ] {
        let mut runtime = run_to_wait(source);
        runtime.collect();
        assert_eq!(
            with_metatable(&runtime),
            0,
            "{}",
            String::from_utf8_lossy(source)
        );
        let mut restored = restore(&runtime);
        restored.collect();
        assert_eq!(with_metatable(&restored), 0);
    }
    let chunk = crate::compile(
        b"local f f = function(n, t) if n == 0 then return t[1] end \
          return f(n - 1, { n, t[1] + 1 }) end return f(100000, { 0, 0 })",
    )
    .unwrap();
    let config = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(print_line(&runtime), "1\n");
    assert!(runtime.memory().collections > 0);
}

/// A chunk is a vararg function, and its tail call's results are the
/// chunk's, before and after a checkpoint.
#[test]
fn a_chunk_tail_calls_with_its_arguments() {
    let chunk = crate::compile(
        b"local f = function(a, ...) return select('#', ...), a, ... end return f(...)",
    )
    .unwrap();
    let args = [
        HostValue::Integer(1),
        HostValue::Nil,
        HostValue::String(b"s".to_vec()),
    ];
    let expected = vec![
        HostValue::Integer(2),
        HostValue::Integer(1),
        HostValue::Nil,
        HostValue::String(b"s".to_vec()),
    ];
    for steps in [0u64, 1, 3, 5] {
        let mut runtime =
            Runtime::load_chunk_with_args(Config::default(), HostRegistry::proof(), &chunk, &args)
                .unwrap();
        runtime.install_base().unwrap();
        let mut journal = Journal::new();
        if steps > 0 {
            runtime.run(steps, &mut journal).unwrap();
        }
        let mut runtime = restore(&runtime);
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(runtime.results().unwrap(), expected, "{steps}");
    }
}
