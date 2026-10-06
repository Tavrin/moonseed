//! The `math` and `table` libraries (Phase 3.22, ADR 0032, ADR 0033).
//! Fixtures give Lua 5.4.9's output under every schedule; generated
//! corpora match it; the generator replays; long table functions cost
//! fuel as they go; no yield crosses them.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::{capture, output_at_every_checkpoint_with, restore, text};
use super::*;
use crate::host::{HostValue, LegacyCompletion};

/// Library fixtures, with everything Lua 5.4.9 writes when it runs each
/// as `print((function() <fixture> end)())`.
const LIBRARY_FIXTURES: &[(&str, &str)] = &[
    (
        "lib_sort_primitive.lua",
        "-7\t9007199254740993\tinteger\ntrue\ttrue\ttrue\ttrue\n1\t4\ttrue\nfalse\ndone\n",
    ),
    (
        "lib_sort_comparator.lua",
        "1,2,3,4,5,6\ttrue\ttrue\t12\nfalse\ttrue\t3\t3,4,1,2,5,6\nfalse\ttrue\ndone\n",
    ),
    (
        "lib_math.lua",
        "8:integer 16:integer 3:integer nil:nil nil:nil nil:nil false:boolean\nnil:nil integer:string float:string false:boolean\n3.0:float 3:integer -9223372036854775808:integer 0.0:float 3:integer 16:integer -4:integer 9.2233720368548e+18:float 1e+308:float NaN\n1:integer -1:integer 1:integer 0:integer false:boolean NaN -1.5:float NaN 1.0:float -0.0:float\n3:integer 0.0:float\n-3:integer -0.5:float\ninf:float 0.0:float\n-inf:float 0.0:float\n0:integer 0.0:float\nNaN NaN\n9.2233720368548e+18:float 0.0:float\n2:integer 0.5:float\ntrue:boolean false:boolean 1:integer b:string 1:integer 1.0:float false:boolean false:boolean\n3.0:float 2.0:float 3.0:float -inf:float NaN inf:float NaN\n3.1415926535898:float inf:float -inf:float 9223372036854775807:integer -9223372036854775808:integer\n1.4142135623731:float 2.0:float 2.718281828459:float 0.8414709848079:float 0.54030230586814:float 1.5574077246549:float 0.5235987755983:float 1.0471975511966:float 0.46364760900081:float 0.78539816339745:float -2.3561944901923:float 180.0:float 3.1415926535898:float inf:float -744.44007192138:float\n3.5:float -0.0:float 9223372036854775807:integer\n0:integer 0:integer 1:integer 1e+300:float -9223372036854775808:integer 4611686018427387904:integer\ndone\n",
    ),
    (
        "lib_random.lua",
        "123:integer 456:integer\n0.59438554497681:float -4076395480158212337:integer 3:integer 2:integer -4658956412393638437:integer\n0.37738499590293:float 1775017273278829961:integer 6:integer -2:integer 604948530922976299:integer\n0.11421040938738:float 2979032669648932270:integer 2:integer -2:integer -4575474236161999343:integer\n7:integer 0:integer\n3:integer false:boolean false:boolean 2:integer\nfalse:boolean true:boolean\n6133480:integer\n1243440074181389794:integer\n5.4589773430942:float\ndone\n",
    ),
    (
        "lib_table.lua",
        "0,1,x,2,3,4\nfalse\tfalse\tfalse\tfalse\n0,1,x,2,3,4,end\nend\t0\t1,x,2,3,4\nnil\tnil\tnil\tfalse\nnil\t1,2\tfalse\nzero\tnil\n\t123\t1-a-2.5\t2, 3\t\ta\nfalse\tfalse\n0\t1\t2\t2\t3\tnil\tnil\n0\t0\tfalse\tfalse\nnil\tnil\t1\n0\t0\n3\t1\tnil\t3\n1,1,2,3,5\n2,3,4,4,5\nnil,nil,1,2,3\tfalse\n1,2\tfalse\tfalse\tfalse\ntrue\tfalse\tfalse\n10,20,30\t10\t20\t30\ng3 s4 g2 s3 g1 s2 s1\ng1 g2 s1 g3 s2 s3\nfalse\ntrue\nfalse\nfalse\tfalse\tfalse\ndone\n",
    ),
    (
        "lib_sort.lua",
        "1,2,3,5,8,9\n9,8,5,3,2,1\na,b,c\nfalse\tfalse\ttrue\tfalse\nfalse\ntrue\t12\t502\n76\t0\t2\ntrue\ndone\n",
    ),
];

/// Boot with the proof natives and every standard library.
fn boot_standard(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_natives(spec);
    runtime.install_math().unwrap();
    runtime.install_table().unwrap();
    runtime
}

const CALLBACK_FIXTURES: &[(&str, &str)] = &[
    (
        "lib_sort_callback.lua",
        "1,2,3,4\n4,3,2,1\nfalse\ttrue\nfalse\ttrue\tdead\nfalse\ttrue\nfalse\t1,2,3\ndone\n",
    ),
    (
        "lib_gsub_callback.lua",
        "[a]-[b]\t2\nab\t2\nfalse\ttrue\t2\nfalse\ttrue\tdead\n17\tnil\t19\n1\ndone\n",
    ),
];

fn boot_callbacks(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_spec(spec);
    runtime.install_standard().unwrap();
    runtime
}

/// Library callbacks keep their task, scratch, results, fuel and GC state
/// identical in all modes, including yields/errors and nested machines.
#[test]
fn library_callbacks_match_at_every_boundary() {
    use crate::runtime::HotCoreMode;
    for (name, expected) in CALLBACK_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let mut straight = boot_callbacks(&chunk.proto);
        let output = capture(&mut straight);
        assert_eq!(
            straight
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(text(&output) + &print_line(&straight), *expected, "{name}");
        fast_slow_equivalent(|| boot_callbacks(&chunk.proto), pair_results);
        for mode in [
            HotCoreMode::Full,
            HotCoreMode::NoFastCalls,
            HotCoreMode::Off,
        ] {
            mode.with(|| {
                let mut runtime = boot_callbacks(&chunk.proto);
                let written = capture(&mut runtime);
                let mut journal = Journal::new();
                loop {
                    runtime = restore(&runtime);
                    let sink = written.clone();
                    runtime.set_output(Box::new(move |bytes| {
                        sink.borrow_mut().extend_from_slice(bytes)
                    }));
                    match runtime.run(1, &mut journal).unwrap() {
                        StepOutcome::Paused(_) => {}
                        StepOutcome::Completed => break,
                        other => panic!("{name}: {other:?}"),
                    }
                }
                assert_eq!(text(&written) + &print_line(&runtime), *expected, "{name}");
                assert_eq!(pair_results(&runtime), pair_results(&straight), "{name}");
                assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed(), "{name}");
            });
        }
    }
}

#[test]
fn library_fixtures_match_lua_under_fuel_gc_and_checkpoints() {
    for (name, expected) in LIBRARY_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let mut runtime = boot_standard(&chunk.proto);
        let written = capture(&mut runtime);
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert!(
            matches!(outcome, StepOutcome::Completed),
            "{name} {outcome:?}"
        );
        assert_eq!(text(&written) + &print_line(&runtime), *expected, "{name}");
        let results = pair_results(&runtime);
        // The fixtures print all they compute, so the output walk, which
        // restores at every step, checks every result; the quanta and the
        // collections run once each.
        for quantum in [1u64, 2, 3, 7] {
            let mut runtime = boot_standard(&chunk.proto);
            let mut journal = Journal::new();
            while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
            assert_eq!(pair_results(&runtime), results, "{name} quantum {quantum}");
        }
        collect_every_safe_point_with(boot_standard, &chunk.proto, &results);
        output_at_every_checkpoint_with(boot_standard, &chunk.proto);
    }
}

/// What Moonseed writes for a program, and what Lua 5.4.9 writes for it.
fn both(lua: &str, source: &str) -> (String, String) {
    both_within(lua, source, 20)
}

fn both_within(lua: &str, source: &str, seconds: u32) -> (String, String) {
    // `lua -e` refuses an argument starting with `-`, as a comment does.
    let output = lua_command_within(lua, seconds)
        .arg("-e")
        .arg(format!("\n{source}"))
        .output()
        .unwrap();
    assert!(output.status.success(), "Lua failed");
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_standard(&chunk.proto);
    let written = capture(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    (text(&written), String::from_utf8(output.stdout).unwrap())
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_library_fixtures_and_corpora() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    for (name, expected) in LIBRARY_FIXTURES.iter().chain(CALLBACK_FIXTURES) {
        let body = String::from_utf8(fixture(name)).unwrap();
        let output = lua_command(&lua)
            .arg("-e")
            .arg(format!("print((function()\n{body}\nend)())"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{name} failed");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            *expected,
            "{name}"
        );
    }
    // The generator and the table functions match exactly. The table
    // corpus sorts large arrays, so PUC Lua gets a longer CPU limit.
    for (name, seconds) in [("corpus_random.lua", 20), ("corpus_table.lua", 240)] {
        let source = String::from_utf8(fixture(name)).unwrap();
        let (ours, theirs) = both_within(&lua, &source, seconds);
        for (index, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
            assert_eq!(a, b, "{name} line {}", index + 1);
        }
        assert_eq!(ours.lines().count(), theirs.lines().count(), "{name}");
    }
    // Math matches exactly, except that a transcendental result may be
    // one unit in the last place from the host's C library (ADR 0032).
    let transcendental = ["sin", "cos", "tan", "asin", "acos", "atan", "exp", "log"];
    let (ours, theirs) = both(
        &lua,
        &String::from_utf8(fixture("corpus_math.lua")).unwrap(),
    );
    assert_eq!(ours.lines().count(), theirs.lines().count());
    let mut near = 0;
    for (index, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
        if a == b {
            continue;
        }
        let close = |x: &str, y: &str| {
            let value = |s: &str| s.strip_suffix(":float").and_then(|s| s.parse::<f64>().ok());
            match (value(x), value(y)) {
                (Some(x), Some(y)) => (x - y).abs() <= 1e-13 * x.abs().max(y.abs()),
                _ => false,
            }
        };
        let fields: Vec<(&str, &str)> = a.split('\t').zip(b.split('\t')).collect();
        let allowed = transcendental.contains(&fields[0].0)
            && fields.iter().all(|(x, y)| x == y || close(x, y));
        assert!(allowed, "corpus_math line {}: {a} vs {b}", index + 1);
        near += 1;
    }
    assert!(near < 20, "{near} lines differ by an ulp");
}

/// No coroutine may yield across a table function or `math.min` /
/// `math.max`: Lua 5.4.9 calls their metamethods and order functions
/// without a continuation.
#[test]
fn no_yield_crosses_the_library() {
    let across = "false\tattempt to yield across a C-call boundary\n";
    let lt = "local lt = { __lt = function() yield('y') return true end }";
    let cases = [
        format!("{lt} return pcall(math.min, setmetatable({{}}, lt), setmetatable({{}}, lt))"),
        "return pcall(table.sort, { 3, 2, 1 }, function(a, b) yield('y') return a < b end)"
            .to_string(),
        format!("{lt} return pcall(table.sort, {{ setmetatable({{}}, lt), setmetatable({{}}, lt) }})"),
        "return pcall(table.insert, setmetatable({}, { __len = function() yield('y') return 1 end }), 1)"
            .to_string(),
        "return pcall(table.concat, setmetatable({}, { __index = function() yield('y') return 'x' end, __len = function() return 2 end }))"
            .to_string(),
        "return pcall(table.unpack, setmetatable({}, { __index = function() yield('y') end }), 1, 2)"
            .to_string(),
        "local mt = { __eq = function() yield('y') return true end } \
         local a, b = setmetatable({ 1, 2 }, mt), setmetatable({}, mt) \
         return pcall(table.move, a, 1, 2, 2, b)"
            .to_string(),
    ];
    for source in &cases {
        let spec = crate::program::source_coroutine_program(source, 1);
        let runtime = finish_with(boot_standard, &spec);
        assert_eq!(print_line(&runtime), across, "{source}");
    }
}

/// A long table function costs a unit of fuel per step of at most
/// `BATCH` operations, so quantum 1 goes through it step by step and fuel
/// grows with its length.
#[test]
fn long_table_functions_cost_fuel_as_they_go() {
    let fill = "local t = {} for i = 1, 5000 do t[i] = (i * 7919) % 5003 end";
    let fuel = |source: &str| {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        finish_with(boot_standard, &chunk.proto).fuel_consumed()
    };
    let base = fuel(&format!("{fill} return 0"));
    let batch = u64::from(crate::runtime::library::BATCH);
    for (call, operations) in [
        ("table.move(t, 1, 5000, 2)", 10_000u64),
        ("table.concat(t, ',')", 5_000),
        ("table.unpack(t, 1, 5000)", 5_000),
        ("table.insert(t, 1, 0)", 10_000),
        ("table.remove(t, 1)", 10_000),
        ("table.sort(t)", 50_000),
    ] {
        let extra = fuel(&format!("{fill} local r = {call} return 0")) - base;
        assert!(extra >= operations / batch, "{call}: {extra}");
    }
    // Quantum 1 moves through a sort a step at a time.
    let chunk =
        crate::compile(format!("{fill} table.sort(t) return t[1], t[5000]").as_bytes()).unwrap();
    let mut runtime = boot_standard(&chunk.proto);
    let mut journal = Journal::new();
    let mut steps = 0;
    while let StepOutcome::Paused(_) = runtime.run(1, &mut journal).unwrap() {
        steps += 1;
    }
    assert_eq!(print_line(&runtime), "1\t5002\n");
    assert!(steps > 1_000, "{steps}");
}

/// The generator's state is snapshot state: a checkpoint between draws
/// continues the same sequence. Seeds come from `Config::entropy` unless
/// the host gives entropy, whose words are journaled effects a replay
/// reads back.
#[test]
fn the_generator_replays_and_is_seeded_without_the_clock() {
    let draws = b"local out = {} for i = 1, 6 do out[i] = math.random(0) end \
        park() for i = 7, 12 do out[i] = math.random(1, 1000) end \
        return table.concat(out, ',')";
    let run = |entropy: u64, checkpoint: bool| {
        let chunk = crate::compile(draws).unwrap();
        let config = Config {
            entropy,
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.install_standard().unwrap();
        runtime.set_global_native("park", "park").unwrap();
        let mut journal = Journal::new();
        let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
        else {
            panic!("no wait");
        };
        if checkpoint {
            runtime = restore(&runtime);
        }
        runtime
            .complete_legacy(key, LegacyCompletion::Return(vec![]))
            .unwrap();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        print_line(&runtime)
    };
    assert_eq!(run(5, false), run(5, true));
    assert_eq!(run(5, false), run(5, false));
    assert_ne!(run(5, false), run(6, false));
    // Host entropy: two journaled words per `randomseed()`.
    let calls = Rc::new(RefCell::new(0));
    let chunk =
        crate::compile(b"local a, b = math.randomseed() park() return a, b, math.random(0)")
            .unwrap();
    let mut runtime = boot_standard(&chunk.proto);
    let counter = calls.clone();
    runtime.set_entropy(Box::new(move || {
        *counter.borrow_mut() += 1;
        1000 + *counter.borrow()
    }));
    let before = runtime.snapshot().unwrap();
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let straight = print_line(&runtime);
    assert!(straight.starts_with("1001\t1002\t"), "{straight}");
    // A replay from before the seeding reads the words back: the host is
    // not asked again.
    let mut replay =
        Runtime::from_snapshot(&before, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    replay.set_entropy(Box::new(|| panic!("asked again")));
    let StepOutcome::Waiting(key) = replay.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    replay
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    replay.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&replay), straight);
    assert_eq!(*calls.borrow(), 2);
}

/// Waits inside a sort's order function, a table function's metamethod,
/// and `math.max`'s `__lt`: restored at the wait and after it, each
/// finishes once, with the same result.
#[test]
fn waits_inside_library_functions_restore() {
    let int = HostValue::Integer;
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        (
            b"local t = { 3, 1, 2 } local n = 0 \
              table.sort(t, function(a, b) n = n + 1 if n == 2 then park() end return a < b end) \
              return t[1], t[2], t[3], n",
            vec![back(vec![])],
            "1\t2\t3\t2\n",
        ),
        (
            b"local log = {} local t = setmetatable({}, { __len = function() return park() end, \
              __newindex = function(t, k, v) log[#log + 1] = k rawset(t, k, v) end }) \
              table.insert(t, 'v') return rawget(t, 3), #log",
            vec![back(vec![int(2)])],
            "v\t1\n",
        ),
        (
            b"local lt = { __lt = function(a, b) return park() end } \
              local a, b = setmetatable({ 1 }, lt), setmetatable({ 2 }, lt) \
              return math.max(a, b)[1]",
            vec![back(vec![HostValue::Boolean(true)])],
            "2\n",
        ),
    ];
    for (source, answers, line) in cases {
        let chunk = crate::compile(source).unwrap();
        for checkpoint in [false, true] {
            let mut runtime = boot_standard(&chunk.proto);
            runtime.set_global_native("park", "park").unwrap();
            let mut journal = Journal::new();
            let mut answers = answers.iter();
            loop {
                match runtime.run_until_terminal(u64::MAX, &mut journal).unwrap() {
                    StepOutcome::Completed => break,
                    StepOutcome::Waiting(key) => {
                        if checkpoint {
                            runtime = restore(&runtime);
                        }
                        runtime
                            .complete_legacy(key, answers.next().unwrap().clone())
                            .unwrap();
                        if checkpoint {
                            runtime = restore(&runtime);
                        }
                    }
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(
                print_line(&runtime),
                line,
                "{}",
                String::from_utf8_lossy(source)
            );
        }
    }
}

/// Restore checks a library frame's task: its counters within its
/// arguments, a scratch slot its wait can fill, a started task, a bounded
/// sort stack; and a generator state that is not all zero.
#[test]
fn restore_refuses_library_states_the_runtime_cannot_make() {
    use crate::heap::Task;
    use crate::library::{LibTask, SortStep, Stage, Wait, Work};
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
    let each = |image: &mut Image, change: &dyn Fn(&mut LibTask)| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if let Some(BoundaryImage::Builtin {
                    task: Task::Lib(task),
                    ..
                }) = &mut frame.boundary
                {
                    change(task);
                }
            }
        }
    };
    let chunk = crate::compile(
        b"local t = { 5, 4, 3, 2, 1 } \
          table.sort(t, function(a, b) local r = park() return a < b end) return t[1]",
    )
    .unwrap();
    let mut sorting = boot_standard(&chunk.proto);
    sorting.set_global_native("park", "park").unwrap();
    assert!(matches!(
        sorting
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(_)
    ));
    restore(&sorting);
    check(&sorting, &|image| {
        each(image, &|task| task.wait = Wait::Get { into: 3 })
    });
    check(&sorting, &|image| {
        each(image, &|task| {
            if let Work::Sort(sort) = &mut task.work {
                sort.lo = 100;
            }
        })
    });
    // A sort stack past its bound is refused when written.
    let mut image = sorting.to_image().unwrap();
    each(&mut image, &|task| {
        if let Work::Sort(sort) = &mut task.work {
            let range = crate::library::SortRange {
                lo: 1,
                up: 2,
                smaller: 0,
                rnd: 0,
            };
            sort.pending = vec![range; crate::library::MAX_SORT_PENDING + 1];
        }
    });
    assert_eq!(
        snapshot::encode(&image).unwrap_err(),
        SnapshotError::LimitExceeded
    );
    check(&sorting, &|image| {
        each(image, &|task| {
            if let Work::Sort(sort) = &mut task.work {
                sort.step = SortStep::Length;
            }
        })
    });
    check(&sorting, &|image| {
        each(image, &|task| {
            task.work = Work::Extreme {
                max: true,
                best: 1,
                next: 1,
            }
        })
    });
    check(&sorting, &|image| {
        each(image, &|task| {
            task.work = Work::Insert {
                stage: Stage::Start,
                pos: 0,
                i: 0,
            }
        })
    });
    check(&sorting, &|image| image.library.rng = [0; 4]);
}

/// A base, math, or table function called by another's frame runs in the
/// next step (`Pending::Deferred`), so chains of them never nest on the
/// Rust stack and each call costs a unit of fuel (review of Phase 3.22:
/// `__newindex = table.insert` recursed a thousand levels on the Rust
/// stack, aborting the host). They end in a catchable stack overflow on a
/// small thread stack, and a checkpoint at a deferred call restores.
#[test]
fn builtins_calling_builtins_do_not_nest() {
    let chains: [&[u8]; 4] = [
        b"local t = setmetatable({ 1, 2, 3 }, { __newindex = table.insert }) \
          return pcall(table.insert, t, 1, 'x')",
        b"local mt = {} mt.__tostring = tostring \
          local ok = pcall(tostring, setmetatable({}, mt)) return ok",
        b"local lt = { __lt = math.max } local a = setmetatable({}, lt) \
          return pcall(math.max, a, setmetatable({}, lt))",
        b"local args = {} for i = 1, 2000 do args[i] = pcall end \
          return (pcall(table.unpack(args)))",
    ];
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            chains
                .iter()
                .map(|source| {
                    let chunk = crate::compile(source).unwrap();
                    let runtime = finish_with(boot_standard, &chunk.proto);
                    (print_line(&runtime), runtime.fuel_consumed())
                })
                .collect::<Vec<_>>()
        })
        .unwrap();
    // The first three overflow; the `pcall` chain catches its innermost
    // failure, as in Lua.
    for ((line, fuel), want) in handle
        .join()
        .unwrap()
        .into_iter()
        .zip(["false", "false", "false", "true"])
    {
        assert!(line.starts_with(want), "{line}");
        assert!(fuel >= 900, "{line} {fuel}");
    }
    // A checkpoint while a call is deferred.
    let chunk = crate::compile(chains[0]).unwrap();
    let mut runtime = boot_standard(&chunk.proto);
    let mut journal = Journal::new();
    let mut restored = 0;
    while let StepOutcome::Paused(_) = runtime.run(1, &mut journal).unwrap() {
        let deferred = {
            let heap = runtime.heap();
            let thread = heap.threads.get(heap.active.unwrap()).unwrap();
            matches!(
                thread.frames.last().and_then(|frame| frame.pending()),
                Some(crate::heap::Pending::Deferred)
            )
        };
        if deferred && restored < 3 {
            runtime = restore(&runtime);
            restored += 1;
        }
    }
    assert_eq!(restored, 3);
    assert!(print_line(&runtime).starts_with("false"));
}

/// Pin the original sort fuel and restore after every quantum, including
/// comparator errors and reentry that replaces raw entries with metamethods.
#[test]
fn sort_paths_preserve_fuel_at_every_step() {
    for (name, fuel) in [
        ("lib_sort_primitive.lua", 307),
        ("lib_sort_comparator.lua", 568),
    ] {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let straight = finish_with(boot_standard, &chunk.proto);
        assert_eq!(straight.fuel_consumed(), fuel, "{name}");
        for checkpoint in [false, true] {
            let mut runtime = boot_standard(&chunk.proto);
            let mut journal = Journal::new();
            loop {
                match runtime.run(1, &mut journal).unwrap() {
                    StepOutcome::Completed => break,
                    StepOutcome::Paused(_) => {
                        if checkpoint {
                            runtime = restore(&runtime);
                        }
                    }
                    other => panic!("{name}: {other:?}"),
                }
            }
            assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed(), "{name}");
            assert_eq!(print_line(&runtime), print_line(&straight), "{name}");
        }
    }
}
