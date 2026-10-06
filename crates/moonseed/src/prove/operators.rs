//! Operators, `__call`, and their metamethods: faults, bounds, pending
//! native handlers, and restore checks. The fixtures that match Lua 5.4.9
//! are in `NATIVE_FIXTURES`.

use super::*;

fn boot_cmpbr_yield(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_natives(spec);
    runtime.install_standard().unwrap();
    runtime
}

#[test]
fn cmpbr_encoding_bounds_and_branch_senses() {
    use crate::opcode::{CmpKind, Op};
    assert_eq!(std::mem::size_of::<Op>(), 16);
    for kind in [CmpKind::Eq, CmpKind::Ne, CmpKind::Lt, CmpKind::Le] {
        for sense in [false, true] {
            for offset in [i32::MIN, -2, 0, i32::MAX] {
                let op = Op::CompareBranch {
                    kind,
                    a: 0,
                    b: 1,
                    sense,
                    offset,
                };
                let mut bytes = Vec::new();
                op.encode(&mut bytes);
                let mut input = bytes.as_slice();
                assert_eq!(Op::decode(&mut input).unwrap(), op);
                assert!(input.is_empty());
                for field in [1, 4] {
                    let mut malformed = bytes.clone();
                    malformed[field] = 255;
                    assert!(Op::decode(&mut malformed.as_slice()).is_err());
                }
                if matches!(offset, i32::MIN | i32::MAX) {
                    assert!(crate::check::validate(&raw_spec(vec![op, Op::Halt], 2)).is_err());
                }
            }
            for (a, b) in [(2, 1), (0, 2)] {
                let spec = raw_spec(
                    vec![
                        Op::CompareBranch {
                            kind,
                            a,
                            b,
                            sense,
                            offset: 0,
                        },
                        Op::Halt,
                    ],
                    2,
                );
                assert!(crate::check::validate(&spec).is_err());
                assert!(restore_booted(&spec).is_err());
            }
        }
    }
    for sense in [false, true] {
        // Test both the taken and fallthrough edges, including a backward
        // jump from a compiled repeat loop in cmpbr.lua.
        for condition in ["1 < 2", "2 <= 1"] {
            let source = format!(
                "if {}({condition}) then return 11 end return 22",
                if sense { "not " } else { "" }
            );
            let chunk = crate::compile(source.as_bytes()).unwrap();
            let expected = if (condition == "1 < 2") != sense {
                "11\n"
            } else {
                "22\n"
            };
            assert_eq!(
                print_line(&finish_with(boot_natives, &chunk.proto)),
                expected
            );
            quantum_and_checkpoints_with(boot_natives, &chunk.proto, pair_results);
        }
    }
}

#[test]
fn cmpbr_yielding_handlers_branch_once_after_resumption() {
    for (operator, event, truth) in [
        ("==", "__eq", true),
        ("~=", "__eq", false),
        ("<", "__lt", true),
        (">", "__lt", true),
        ("<=", "__le", true),
        (">=", "__le", true),
    ] {
        for (reply, reply_truth) in [
            ("false", false),
            ("0", true),
            ("nil", false),
            ("'yes'", true),
        ] {
            let expected = if truth == reply_truth { 11 } else { 22 };
            let source = format!(
                "local calls = 0 local co = coroutine.create(function() local a, b \
                 local mt = {{ {event} = function() calls = calls + 1 a, b = nil, false \
                 coroutine.yield('seen') return {reply} end }} \
                 a, b = setmetatable({{}}, mt), setmetatable({{}}, mt) \
                 if not (a {operator} b) then return 22 end return 11 end) \
                 local ok, seen = coroutine.resume(co) assert(ok and seen == 'seen' and calls == 1) \
                 local ok2, answer = coroutine.resume(co) assert(ok2 and answer == {expected} and calls == 1) \
                 return answer, calls"
            );
            let chunk = crate::compile(source.as_bytes()).unwrap();
            quantum_and_checkpoints_with(boot_cmpbr_yield, &chunk.proto, pair_results);
        }
    }
}

#[test]
fn cmpbr_errors_match_value_comparisons() {
    for operator in ["<", "<=", ">", ">="] {
        for bad in ["nil", "false", "{}", "'2'"] {
            let run = |branch: bool| {
                let expression = format!("a {operator} b");
                let body = if branch {
                    format!("if {expression} then return true end return false")
                } else {
                    format!("return {expression}")
                };
                let source = format!("local a, b = 1, {bad} return pcall(function() {body} end)");
                let chunk = crate::compile(source.as_bytes()).unwrap();
                fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);
                print_line(&finish_with(boot_natives, &chunk.proto))
            };
            assert_eq!(run(true), run(false), "{operator} {bad}");
        }
    }
}

#[test]
fn arithk_operand_order_and_fallback_at_every_safe_point() {
    for (operator, event) in [
        ("+", "__add"),
        ("-", "__sub"),
        ("*", "__mul"),
        ("/", "__div"),
        ("//", "__idiv"),
        ("%", "__mod"),
        ("^", "__pow"),
        ("&", "__band"),
        ("|", "__bor"),
        ("~", "__bxor"),
        ("<<", "__shl"),
        (">>", "__shr"),
    ] {
        let source = format!(
            "local t local n = 0 t = setmetatable({{}}, {{ {event} = function(a, b) \
             n = n + 1 if a == t then assert(b == 2) return 12 end \
             assert(a == 3 and b == t) return 21 end }}) \
             local a, b = t {operator} 2, 3 {operator} t return a, b, n"
        );
        let chunk = crate::compile(source.as_bytes()).unwrap();
        assert!(
            chunk
                .proto
                .ops
                .iter()
                .any(|op| matches!(op, crate::opcode::Op::ArithK { reverse: true, .. }))
        );
        assert_eq!(
            print_line(&finish_with(boot_natives, &chunk.proto)),
            "12\t21\t2\n"
        );
        quantum_and_checkpoints_with(boot_natives, &chunk.proto, pair_results);
    }
}

#[test]
fn arithk_errors_match_register_operands() {
    for operator in [
        "+", "-", "*", "/", "//", "%", "^", "&", "|", "~", "<<", ">>",
    ] {
        for bad in ["nil", "false", "{}", "'bad'"] {
            for reverse in [false, true] {
                let expression = if reverse {
                    format!("k {operator} x")
                } else {
                    format!("x {operator} k")
                };
                let immediate = expression.replace('k', "2");
                let run = |expression: &str| {
                    let source = format!(
                        "local x = {bad} local k = 2 return pcall(function() return {expression} end)"
                    );
                    let chunk = crate::compile(source.as_bytes()).unwrap();
                    fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);
                    print_line(&finish_with(boot_natives, &chunk.proto))
                };
                assert_eq!(
                    run(&immediate),
                    run(&expression),
                    "{bad} {operator} reverse={reverse}"
                );
            }
        }
    }
}

#[test]
fn arithk_encoding_and_register_bounds() {
    use crate::opcode::{ArithOp, Op};
    assert_eq!(std::mem::size_of::<Op>(), 16);
    for constant in [i64::MIN, i64::MAX, 0] {
        for reverse in [false, true] {
            let op = Op::ArithK {
                op: ArithOp::Sub,
                dst: 0,
                reg: 1,
                constant,
                reverse,
            };
            let mut bytes = Vec::new();
            op.encode(&mut bytes);
            let mut input = bytes.as_slice();
            assert_eq!(Op::decode(&mut input).unwrap(), op);
            assert!(input.is_empty());
            for field in [1, 12] {
                let mut malformed = bytes.clone();
                malformed[field] = 255;
                assert!(Op::decode(&mut malformed.as_slice()).is_err());
            }
            for (dst, reg) in [(2, 1), (0, 2)] {
                let spec = raw_spec(
                    vec![
                        Op::ArithK {
                            op: ArithOp::Add,
                            dst,
                            reg,
                            constant,
                            reverse,
                        },
                        Op::Return { base: 0, count: 0 },
                    ],
                    2,
                );
                assert!(crate::check::validate(&spec).is_err());
                assert!(restore_booted(&spec).is_err());
            }
        }
    }
}

fn outcome(source: &[u8]) -> StepOutcome {
    let chunk = crate::compile(source).unwrap();
    fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);
    let mut runtime = boot_natives(&chunk.proto);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap()
}

#[test]
fn operator_faults() {
    for (source, fault) in [
        (&b"return 1 // 0"[..], LuaFault::DivideByZero),
        (b"return 1 % 0", LuaFault::ModuloByZero),
        (b"local t = {} return t + 1", LuaFault::Arith),
        (b"return 'abc' + 1", LuaFault::Arith),
        (b"return -{}", LuaFault::Arith),
        (b"return '3' & 1", LuaFault::Bitwise),
        (b"return {} | 1", LuaFault::Bitwise),
        (b"return 1.5 & 1", LuaFault::NoInteger),
        (b"return ~1.5", LuaFault::NoInteger),
        (b"return 2 ^ 63 | 0", LuaFault::NoInteger),
        (b"return 1 .. {}", LuaFault::Concat),
        (b"return 1 < '2'", LuaFault::Compare),
        (b"return {} < {}", LuaFault::Compare),
        // Lua 5.4 does not fall back from `<=` to `__lt`.
        (
            b"local mt = { __lt = function() return true end } \
              local a, b = setmetatable({}, mt), setmetatable({}, mt) return a <= b",
            LuaFault::Compare,
        ),
        (b"local t = {} return t()", LuaFault::BadCall),
        (b"local t = setmetatable({}, {}) return t()", LuaFault::BadCall),
        (
            b"local t = setmetatable({}, { __add = 5 }) return t + 1",
            LuaFault::BadCall,
        ),
        (
            b"local a, b = {}, {} setmetatable(a, { __call = b }) setmetatable(b, { __call = a }) return a()",
            LuaFault::CallChain,
        ),
        (
            b"local a, b = {}, {} setmetatable(a, { __call = b }) setmetatable(b, { __call = a }) \
              local t = setmetatable({}, { __add = a }) return t + 1",
            LuaFault::CallChain,
        ),
        (b"return rawequal(1)", LuaFault::Native),
    ] {
        assert_eq!(
            outcome(source),
            StepOutcome::LuaError(fault),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn runaway_recursion_faults_instead_of_exhausting_memory() {
    for source in [
        &b"local f f = function(n) return f(n + 1) + 1 end return f(1)"[..],
        // `__eq` comparing two distinct tables calls itself.
        b"local a = {} local b = setmetatable({}, { __eq = function(x, y) return x == a end }) return b == a",
        b"local t t = setmetatable({}, { __index = function(self, k) return t[k] end }) return t.x",
    ] {
        assert_eq!(
            outcome(source),
            StepOutcome::LuaError(LuaFault::StackOverflow),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
    // Just under the bound, the recursion finishes, and a snapshot taken at
    // its deepest point restores.
    let source = format!(
        "local f f = function(n) if n == {} then return 0 end return f(n + 1) + 1 end return f(1)",
        crate::runtime::MAX_CALL_DEPTH - 1
    );
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    let mut deepest = None;
    loop {
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
        let depth = runtime.heap().threads.iter().next().unwrap().2.frames.len();
        if depth == crate::runtime::MAX_CALL_DEPTH - 1 && deepest.is_none() {
            deepest = Some(runtime.snapshot().unwrap());
        }
    }
    assert_eq!(
        print_line(&runtime),
        format!("{}\n", crate::runtime::MAX_CALL_DEPTH - 2)
    );
    let bytes = deepest.expect("reached the deepest frame");
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    restored
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&restored), print_line(&runtime));
}

#[test]
fn call_chains_stop_at_the_bound() {
    let chain = |steps: u32| {
        format!(
            "local current = function(first) return 'end' end \
             for i = 1, {steps} do current = setmetatable({{}}, {{ __call = current }}) end \
             return current()"
        )
    };
    let chunk = crate::compile(chain(crate::runtime::MAX_CALL_CHAIN).as_bytes()).unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "end\n");
    assert_eq!(
        outcome(chain(crate::runtime::MAX_CALL_CHAIN + 1).as_bytes()),
        StepOutcome::LuaError(LuaFault::CallChain)
    );
}

/// Each program stops in `park` (a native that waits for key 1) inside an
/// operator's handler or a `__call`. The completion value is its result.
const PENDING: &[(&str, i64, &str)] = &[
    (
        "local t = setmetatable({}, { __add = park }) local v = t + 1 return v",
        41,
        "41\n",
    ),
    (
        "local mt = { __eq = park } local a, b = setmetatable({}, mt), setmetatable({}, mt) return a ~= b",
        0,
        "false\n",
    ),
    (
        "local t = setmetatable({}, { __lt = park }) return 1 < t",
        0,
        "true\n",
    ),
    (
        "local t = setmetatable({}, { __concat = park }) return 'x' .. t",
        7,
        "7\n",
    ),
    (
        "local t = setmetatable({}, { __unm = park }) return -t",
        3,
        "3\n",
    ),
    (
        "local t = setmetatable({}, { __call = park }) local a, b = t(1, 2) return a, b",
        5,
        "5\tnil\n",
    ),
    (
        "local c = setmetatable({}, { __call = park }) local t = setmetatable({}, { __add = c }) return t + 1",
        9,
        "9\n",
    ),
    (
        "local t = setmetatable({}, { __sub = park }) return 3 - t",
        17,
        "17\n",
    ),
    (
        "local t = setmetatable({}, { __lt = park }) if 1 < t then return 11 end return 22",
        0,
        "11\n",
    ),
    (
        "local t = setmetatable({}, { __le = park }) if not (1 <= t) then return 11 end return 22",
        0,
        "22\n",
    ),
    (
        "local mt = { __eq = park } local a, b = setmetatable({}, mt), setmetatable({}, mt) \
         if a ~= b then return 11 end return 22",
        0,
        "22\n",
    ),
];

#[test]
fn pending_native_operator_handlers_restore_and_finish_once() {
    for (source, result, line) in PENDING {
        let runtime = run_to_wait(source.as_bytes());
        let bytes = runtime.snapshot().unwrap();
        let mut restored =
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        restored.collect();
        assert_eq!(
            restored.complete_wait(WaitKey(2), *result),
            Err(WaitError::UnknownKey),
            "{source}"
        );
        restored.complete_wait(WaitKey(1), *result).unwrap();
        assert_eq!(
            restored.complete_wait(WaitKey(1), *result),
            Err(WaitError::AlreadyCompleted),
            "{source}"
        );
        // The handler's result is committed in its own uncharged step; a
        // snapshot between completion and commit finishes the same way.
        let committed = restored.snapshot().unwrap();
        let mut journal = Journal::new();
        loop {
            match restored.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{source}: {other:?}"),
            }
        }
        assert_eq!(print_line(&restored), *line, "{source}");
        let mut again =
            Runtime::from_snapshot(&committed, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        again
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(print_line(&again), *line, "{source}");
    }
}

#[test]
fn pending_handlers_survive_eager_collection() {
    for (source, result, line) in PENDING {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut runtime = super::memory::boot_eager(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Waiting(WaitKey(1)),
            "{source}"
        );
        runtime.complete_wait(WaitKey(1), *result).unwrap();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert_eq!(print_line(&runtime), *line, "{source}");
        assert!(runtime.memory().collections > 0, "{source}");
    }
}

#[test]
fn operator_continuations_fail_closed_when_tampered() {
    use crate::heap::MetaEvent;
    use crate::snapshot::{EventImage, MetaImage};
    let tamper = |source: &str, change: &dyn Fn(&mut MetaImage)| {
        let runtime = run_to_wait(source.as_bytes());
        let mut image = runtime.to_image().unwrap();
        let meta = image
            .threads
            .iter_mut()
            .flat_map(|thread| thread.frames.iter_mut())
            .find_map(|frame| frame.meta.as_mut())
            .expect("a pending metamethod call");
        change(meta);
        let bytes = snapshot::encode(&image).unwrap();
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
    };
    let add = PENDING[0].0;
    let ne = PENDING[1].0;
    let through_call = PENDING[6].0;
    // Untampered, each restores.
    for source in [add, ne, through_call] {
        assert!(tamper(source, &|_| {}).is_ok(), "{source}");
    }
    type Change<'a> = &'a dyn Fn(&mut MetaImage);
    let cb = PENDING[10].0;
    assert!(tamper(cb, &|_| {}).is_ok());
    let refused: [(&str, Change); 9] = [
        (add, &|meta| meta.nargs = 1),
        (add, &|meta| {
            meta.nargs = 3 + crate::runtime::MAX_CALL_CHAIN as u8
        }),
        (add, &|meta| {
            meta.event = EventImage::Plain(MetaEvent::Truth {
                dst: 0,
                negate: false,
            })
        }),
        (add, &|meta| {
            meta.event = EventImage::Plain(MetaEvent::NewIndex)
        }),
        (ne, &|meta| {
            if let EventImage::Plain(MetaEvent::Truth { negate, .. }) = &mut meta.event {
                *negate = !*negate;
            }
        }),
        (ne, &|meta| {
            if let EventImage::Plain(MetaEvent::Truth { dst, .. }) = &mut meta.event {
                *dst = dst.wrapping_add(1);
            }
        }),
        (through_call, &|meta| meta.slot = 0),
        (cb, &|meta| {
            if let EventImage::Plain(MetaEvent::Truth { dst, .. }) = &mut meta.event {
                *dst = 0;
            }
        }),
        (cb, &|meta| {
            if let EventImage::Plain(MetaEvent::Truth { negate, .. }) = &mut meta.event {
                *negate = !*negate;
            }
        }),
    ];
    for (source, change) in refused {
        expect_snapshot(tamper(source, change), SnapshotError::InvalidStructure);
    }
}

#[test]
fn deep_fixtures_under_sparse_checkpoints() {
    for (name, line) in DEEP_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let runtime = finish_with(boot_natives, &chunk.proto);
        assert_eq!(print_line(&runtime), *line, "{name}");
        let eager = finish_with(super::memory::boot_eager, &chunk.proto);
        assert_eq!(print_line(&eager), *line, "{name} eager");
        let mut runtime = boot_natives(&chunk.proto);
        let mut journal = Journal::new();
        let mut slices = 0u32;
        loop {
            match runtime.run(7, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{name}: {other:?}"),
            }
            slices += 1;
            if slices.is_multiple_of(211) {
                let bytes = runtime.snapshot().unwrap();
                runtime =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
            }
        }
        assert_eq!(print_line(&runtime), *line, "{name} restored");
    }
}
