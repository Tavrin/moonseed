//! Generic `for` (ADR 0027). The source fixtures that match Lua 5.4.9 are
//! the `gfor_*` entries of `NATIVE_FIXTURES`.

use super::*;
use crate::check;
use crate::host::{HostValue, LegacyCompletion};
use crate::opcode::{COUNT_OPEN, Op};

const FIXTURES: [&str; 7] = [
    "gfor_basic.lua",
    "gfor_init.lua",
    "gfor_capture.lua",
    "gfor_close.lua",
    "gfor_error.lua",
    "gfor_callable.lua",
    "gfor_builtin.lua",
];

fn run_source(source: &[u8]) -> Runtime {
    finish_with(boot_natives, &crate::compile(source).unwrap().proto)
}

#[test]
fn the_loop_compiles_to_hidden_values_an_ordinary_call_and_one_decision() {
    let chunk = crate::compile(b"local a = 1 for k, v in upto, 3 do a = k end return a").unwrap();
    let ops = &chunk.proto.ops;
    let at = |op: &Op| ops.iter().position(|other| other == op).unwrap();
    // The four values sit at 1..=4, the fourth is marked once, and the
    // loop starts at its first call.
    let mark = at(&Op::MarkClose { reg: 4 });
    let call = at(&Op::Call {
        func: 5,
        nargs: 2,
        nresults: 2,
    });
    let Op::Jump { offset } = ops[mark + 1] else {
        panic!("no jump to the first call");
    };
    assert_eq!(mark + 2 + offset as usize, call - 3);
    for offset in 0..3u8 {
        assert_eq!(
            ops[call - 3 + usize::from(offset)],
            Op::Move {
                dst: 5 + offset,
                src: 1 + offset,
            }
        );
    }
    assert!(matches!(ops[call + 1], Op::GenericForLoop { base: 1, .. }));
    assert_eq!(ops[call + 2], Op::CloseScope { from: 1 });
    assert_eq!(
        print_line(&run_source(
            b"local a = 1 for k, v in upto, 3 do a = k end return a"
        )),
        "3\n"
    );

    // The loop variables are gone after the loop, and names are required.
    assert_eq!(
        print_line(&run_source(b"for x in upto, 2 do end return x")),
        "nil\n"
    );
    // Too many variables for the frame's registers.
    let names: Vec<String> = (0..250).map(|index| format!("v{index}")).collect();
    let source = format!("for {} in upto do end", names.join(", "));
    assert_eq!(
        crate::compile(source.as_bytes()).unwrap_err().kind,
        crate::CompileErrorKind::Limit
    );
}

/// The fixtures and the yielding iterator use the same fuel however they
/// are paused and restored.
#[test]
fn fuel_is_the_same_under_every_schedule() {
    let specs = FIXTURES
        .iter()
        .map(|name| (*name, crate::compile(&fixture(name)).unwrap().proto))
        .chain([("yield", crate::program::generic_for_yield_program())]);
    for (name, spec) in specs {
        let whole = finish_with(boot_natives, &spec).fuel_consumed();
        let mut runtime = boot_natives(&spec);
        let mut journal = Journal::new();
        loop {
            let bytes = runtime.snapshot().unwrap();
            runtime =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                    .unwrap();
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{name}: {other:?}"),
            }
        }
        assert_eq!(runtime.fuel_consumed(), whole, "{name}");
    }
}

#[test]
fn builtin_iterators_match_in_all_hot_core_modes() {
    let spec = crate::compile(&fixture("gfor_builtin.lua")).unwrap().proto;
    fast_slow_equivalent(|| boot_natives(&spec), pair_results);
}

/// A captured loop variable is a new variable in each iteration.
#[test]
fn a_captured_loop_variable_needs_the_scope_exit_of_each_iteration() {
    let source = b"local fs = {} local n = 0 \
                   for k in upto, 2 do n = n + 1 fs[n] = function() return k end end \
                   return fs[1](), fs[2]()";
    assert_eq!(print_line(&run_source(source)), "1\t2\n");
    // Negative control: without the body's `CloseUpvalues`, both closures
    // share the loop variable's open cell, which the loop's exit closes
    // holding the iterator's final nil.
    let mut chunk = crate::compile(source).unwrap();
    let close = chunk
        .proto
        .ops
        .iter()
        .position(|op| matches!(op, Op::CloseUpvalues { .. }))
        .expect("the body closes its captured loop variable");
    chunk.proto.ops[close] = Op::Jump { offset: 0 };
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "nil\tnil\n");
}

/// The iterator yields inside a coroutine at every call, and the closing
/// value yields as the loop ends. Lua 5.4.9 gives the same line.
#[test]
fn a_yielding_iterator_and_closer_resume_each_call_once() {
    let spec = crate::program::generic_for_yield_program();
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(
        print_line(&runtime),
        "nil\t1\t2\t3\th\t6\th\tnil\tnil\tnil\n"
    );
    let expected = pair_results(&runtime);
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
    collect_every_safe_point_with(boot_natives, &spec, &expected);
}

/// Drive `source` to its end, answering each wait on key 1 with the next
/// of `answers`. With `checkpoint`, restore at every wait and again right
/// after each completion. Gives the printed line and the fuel used.
pub(super) fn drive(
    source: &[u8],
    answers: &[LegacyCompletion],
    checkpoint: bool,
) -> (String, u64) {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    let mut answers = answers.iter();
    let restore = |runtime: &Runtime| {
        Runtime::from_snapshot(
            &runtime.snapshot().unwrap(),
            &HostRegistry::proof(),
            runtime.effect_domain(),
        )
        .unwrap()
    };
    loop {
        match runtime.run_until_terminal(u64::MAX, &mut journal).unwrap() {
            StepOutcome::Completed => break,
            StepOutcome::Waiting(key) => {
                if checkpoint {
                    runtime = restore(&runtime);
                }
                let answer = answers.next().expect("an answer for every wait");
                runtime.complete_legacy(key, answer.clone()).unwrap();
                if checkpoint {
                    runtime = restore(&runtime);
                }
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(answers.next().is_none(), "a wait never came");
    (print_line(&runtime), runtime.fuel_consumed())
}

#[test]
fn waiting_iterators_and_closers_restore_and_finish_once() {
    let int = |value| HostValue::Integer(value);
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        // The iterator waits: its results reach the variables, and the body
        // runs once per completed call.
        (
            b"local n, s, last = 0, 0 for x, y in park do n = n + 1 s = s + x last = y end \
              return n, s, last",
            vec![back(vec![int(5), int(6)]), back(vec![int(7)]), back(vec![])],
            "2\t12\tnil\n",
        ),
        // A waiting iterator whose call fails unwinds through the loop, and
        // the closing value sees the error.
        (
            b"local seen local ok, e = pcall(function() \
              for x in park, nil, nil, setmetatable({}, { __close = function(_, err) seen = err end }) do end \
              end) return ok, e, seen",
            vec![LegacyCompletion::Error(HostValue::String(b"late".to_vec()))],
            "false\tlate\tlate\n",
        ),
        // The closing value waits as the loop ends, after `break`, in a
        // `return`, and in an unwind.
        // The iterator is not called again after its final nil.
        (
            b"local n, calls = 0, 0 local it = function(s, c) calls = calls + 1 return upto(s, c) end \
              for x in it, 3, nil, setmetatable({}, { __close = park }) do n = n + x end \
              return n, calls",
            vec![back(vec![])],
            "6\t4\n",
        ),
        (
            b"local n = 0 for x in upto, 3, nil, setmetatable({}, { __close = park }) do \
              n = n + x if x == 2 then break end end return n",
            vec![back(vec![])],
            "3\n",
        ),
        (
            b"local f = function() for x in upto, 5, nil, setmetatable({}, { __close = park }) do \
              if x == 2 then return x, 20 end end end return f()",
            vec![back(vec![])],
            "2\t20\n",
        ),
        (
            b"local ok, e = pcall(function() \
              for x in upto, 5, nil, setmetatable({}, { __close = park }) do error('x', 0) end end) \
              return ok, e",
            vec![back(vec![])],
            "false\tx\n",
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
fn restored_code_refuses_malformed_generic_for() {
    let chunk =
        crate::compile(b"local s = 0 for k, v in upto, 3 do s = s + k end return s").unwrap();
    let decide = chunk
        .proto
        .ops
        .iter()
        .position(|op| matches!(op, Op::GenericForLoop { .. }))
        .unwrap();
    let call = decide - 1;
    let loop_at = |base, offset| Op::GenericForLoop { base, offset };
    let call_with = |func, nargs, nresults| Op::Call {
        func,
        nargs,
        nresults,
    };
    for (name, at, op) in [
        ("zero variables", call, call_with(5, 2, 0)),
        ("open results", call, call_with(5, 2, COUNT_OPEN)),
        ("three arguments", call, call_with(5, 3, 2)),
        ("call elsewhere", call, call_with(4, 2, 2)),
        ("no call", call, Op::Move { dst: 5, src: 1 }),
        ("forward branch", decide, loop_at(1, 0)),
        ("branch to itself", decide, loop_at(1, -1)),
        ("branch to the call", decide, loop_at(1, -2)),
        ("branch out of the code", decide, loop_at(1, -500)),
        ("base past the registers", decide, loop_at(252, -6)),
        ("first instruction", 0, loop_at(1, -2)),
    ] {
        let mut spec = chunk.proto.clone();
        spec.ops[at] = op;
        assert_eq!(
            check::validate(&spec).unwrap_err().kind,
            crate::CompileErrorKind::InvalidProgram,
            "{name}"
        );
        match restore_booted(&spec) {
            Err(error) => assert_eq!(error, SnapshotError::InvalidBytecode, "{name}"),
            Ok(_) => panic!("{name}: restored invalid code"),
        }
    }
    check::validate(&chunk.proto).unwrap();
}

#[test]
fn iterator_garbage_is_collected_and_retention_reaches_the_quota() {
    let config = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let run = |source: &[u8]| {
        let chunk = crate::compile(source).unwrap();
        let mut runtime =
            Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.install_base().unwrap();
        runtime.set_global_native("upto", "upto").unwrap();
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Completed
        );
        assert!(runtime.memory().logical_bytes <= 256 * 1024);
        print_line(&runtime)
    };
    // Each call makes a table the body drops: 20,000 tables, about 1.3 MB.
    assert_eq!(
        run(
            b"local make = function(s, c) if c == nil then return { 1 } end \
              if c[1] < s then return { c[1] + 1 } end end \
              local last for t in make, 20000 do last = t[1] end return last"
        ),
        "20000\n"
    );
    // The same loop keeping every table fails, and `pcall` catches it.
    assert_eq!(
        run(b"local keep = {} local n = 0 \
              local make = function(s, c) if c == nil then return { 1 } end \
              if c[1] < s then return { c[1] + 1 } end end \
              local ok, e = pcall(function() for t in make, 20000 do n = n + 1 keep[n] = t end end) \
              return ok, e, n < 20000"),
        "false\tnot enough memory\ttrue\n"
    );
}

#[test]
fn the_hidden_values_die_with_their_registers() {
    // The loop's state table and closing value have metatables. After the
    // loop, the next locals reuse their registers, and a collection frees
    // both.
    let with_metatable = |runtime: &Runtime| {
        runtime
            .heap()
            .tables
            .iter()
            .filter(|(_, _, table)| table.metatable.is_some())
            .count()
    };
    let runtime = {
        let mut runtime = run_to_wait(
            b"local f = function() \
              local it = function(s, c) if c == nil then return 1 end end \
              for x in it, setmetatable({}, {}), nil, setmetatable({}, { __close = function() end }) do end \
              local a, b, c, d, e = 1, 2, 3, 4, 5 park() end \
              f()",
        );
        runtime.collect();
        runtime
    };
    assert_eq!(with_metatable(&runtime), 0);
}

#[test]
fn runaway_iterators_stop_at_their_limits() {
    // An iterator that never answers nil runs until the fuel limit.
    let chunk =
        crate::compile(b"local n = 0 for x in add, 1, 0 do n = n + 1 end return n").unwrap();
    let mut runtime = Runtime::boot(
        Config {
            fuel_limit: Some(100_000),
            ..Config::default()
        },
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    runtime.set_global_native("add", "add").unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Terminated(crate::TerminationReason::FuelLimitExceeded)
    );
    // An iterator that recurses, each level in a generic `for` of its own
    // with a closing value, overflows the stack; every level closes, and
    // `pcall` catches it.
    assert_eq!(
        print_line(&run_source(
            b"local closed = 0 local it \
              it = function(s, c) \
                for x in it, s, c, setmetatable({}, { __close = function() closed = closed + 1 end }) do end \
              end \
              local ok, e = pcall(it) return ok, e, closed > 300"
        )),
        "false\t?:1: stack overflow\ttrue\n"
    );
    // A `__call` chain too long to follow.
    assert_eq!(
        print_line(&run_source(
            b"local t = {} setmetatable(t, { __call = t }) \
              local ok = pcall(function() for x in t do end end) return ok"
        )),
        "false\n"
    );
}

#[test]
fn iterations_do_not_grow_stack_or_frame_storage() {
    let grows = |body: &str, n: i64| {
        let runtime = run_source(
            format!(
                "local it = function(s, c) if c < s then return c + 1 end end \
                 local t = setmetatable({{}}, {{ __call = function(_, s, c) return it(s, c) end }}) \
                 local f {body} return {n}"
            )
            .replace("N", &n.to_string())
            .as_bytes(),
        );
        (runtime.stack_grows, runtime.frame_grows)
    };
    for body in [
        "for i in it, N, 0 do end",
        "for i in upto, N do end",
        "for i in t, N, 0 do end",
        "for i in it, N, 0 do f = function() return i end end",
    ] {
        assert_eq!(grows(body, 10), grows(body, 5_000), "{body}");
    }
}
