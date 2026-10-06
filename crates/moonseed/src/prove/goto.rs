//! Labels and `goto` (ADR 0030), and the compiler's register bound. The
//! source fixtures that match Lua 5.4.9 are the `goto_*` entries of
//! `NATIVE_FIXTURES`.

use super::*;

fn widest(spec: &crate::program::ProtoSpec) -> u8 {
    spec.children.iter().map(widest).fold(spec.max_reg, u8::max)
}

fn list(item: &str, n: usize) -> String {
    vec![item; n].join(", ")
}

fn numbered(prefix: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{prefix}{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each form that places values in consecutive registers, at the largest
/// size that compiles and one past it. The largest fits the 250-register
/// limit and runs; one more is a `Limit` error, found before any
/// instruction names a register past the limit. Before Phase 3.19 an
/// argument list could reach register 251.
#[test]
fn every_register_window_stops_at_the_limit() {
    // A name, the source for `n` values, and the fewest registers the
    // largest must reach: generic `for` stops at the 200-local limit first.
    type Case = (&'static str, fn(usize) -> String, u8);
    let cases: [Case; 7] = [
        (
            "call arguments",
            |n| {
                format!(
                    "local f = function(...) return select('#', ...) end return f({})",
                    list("1", n)
                )
            },
            245,
        ),
        (
            "call arguments from a local",
            |n| {
                format!(
                    "local a = 1 local f = function(...) return select('#', ...) end return f({})",
                    list("a", n)
                )
            },
            245,
        ),
        (
            "method arguments",
            |n| {
                format!(
                    "local o = {{ m = function(self, ...) return select('#', ...) end }} return o:m({})",
                    list("1", n)
                )
            },
            245,
        ),
        (
            "return list",
            |n| format!("return select('#', {})", list("1", n)),
            245,
        ),
        (
            "assignment",
            |n| format!("{} = {} return g0", numbered("g", n), list("1", n)),
            245,
        ),
        (
            "values before `...`",
            |n| format!("return select('#', {}, ...)", list("1", n)),
            245,
        ),
        (
            "generic for variables",
            |n| {
                format!(
                    "local c = 0 for {} in upto, 1 do c = c + 1 end return c",
                    numbered("v", n)
                )
            },
            200,
        ),
    ];
    for (name, source, fewest) in cases {
        let mut largest = None;
        for n in 1..300 {
            match crate::compile(source(n).as_bytes()) {
                Ok(chunk) => {
                    assert!(widest(&chunk.proto) <= 250, "{name} {n}");
                    largest = Some((n, chunk));
                }
                Err(error) => {
                    assert_eq!(
                        error.kind,
                        crate::CompileErrorKind::Limit,
                        "{name} {n}: {error:?}"
                    );
                    break;
                }
            }
        }
        let (n, chunk) = largest.expect(name);
        assert!(n < 299, "{name} never reached the limit");
        assert!(
            widest(&chunk.proto) >= fewest,
            "{name}: {n} values in {} registers",
            widest(&chunk.proto)
        );
        let runtime = finish_with(boot_natives, &chunk.proto);
        assert!(!print_line(&runtime).is_empty(), "{name}");
    }
}

/// The code of the chunk's first nested function, or the chunk's own.
fn code(source: &str) -> Vec<crate::opcode::Op> {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    match chunk.proto.children.first() {
        Some(child) => child.ops.clone(),
        None => chunk.proto.ops,
    }
}

/// A goto is two slots: a `Jump` when it leaves nothing to close, and
/// otherwise the close its left locals need, then the `Jump`, decided once
/// every capture is known. A goto that stays inside a generic `for`'s body
/// leaves its closing value alone.
#[test]
fn a_goto_compiles_to_a_jump_and_only_the_close_it_needs() {
    use crate::opcode::Op;
    let slots = |ops: &[Op]| {
        let site = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    Op::Jump { .. } | Op::CloseScope { .. } | Op::CloseUpvalues { .. }
                )
            })
            .unwrap();
        (ops[site], ops[site + 1])
    };
    // Nothing captured or closed: a bare jump.
    let (first, second) = slots(&code(
        "local f = function() do local x = 1 goto out end ::out:: end",
    ));
    assert!(matches!(first, Op::Jump { .. }), "{first:?}");
    assert!(matches!(second, Op::Jump { .. }));
    // `x` is captured only after the goto in the source; the second pass
    // through `inner` reaches the goto with its upvalue open.
    let ops = code(
        "local f = function() do local x = 1 ::inner:: if g then goto out end \
         h = function() return x end goto inner end ::out:: end",
    );
    // The goto's own slots: the close, then the jump. The block's exit
    // has a `CloseUpvalues` too, but no jump after it.
    assert!(
        ops.windows(2)
            .any(|pair| matches!(pair, [Op::CloseUpvalues { .. }, Op::Jump { .. }])),
        "{ops:?}"
    );
    // A `<close>` local left: `CloseScope`, then the jump.
    let ops = code("local f = function() do local c <close> = nil goto out end ::out:: end");
    let at = ops
        .iter()
        .position(|op| matches!(op, Op::CloseScope { .. }))
        .unwrap();
    assert!(matches!(ops[at + 1], Op::Jump { .. }));
    // `continue` inside a generic `for` body closes nothing of the loop.
    let ops = code(
        "local f = function(t) for k in t, nil, nil, nil do if k then goto continue end \
         ::continue:: end end",
    );
    let body_gotos = ops
        .iter()
        .filter(|op| matches!(op, Op::CloseScope { .. }))
        .count();
    let loop_exits = code("local f = function(t) for k in t, nil, nil, nil do end end")
        .iter()
        .filter(|op| matches!(op, Op::CloseScope { .. }))
        .count();
    assert_eq!(body_gotos, loop_exits);
    // The same source compiles to the same prototype.
    let source = b"local n = 0 ::a:: do local x <close> = nil local y = 1 \
        local f = function() return y end if n < 3 then n = n + 1 goto a end \
        goto b end ::b:: return n";
    assert_eq!(
        crate::compile(source).unwrap().proto,
        crate::compile(source).unwrap().proto
    );
}

/// Lua 5.4.9 refuses each of these; so does the compiler, with a syntax
/// error naming the label, and the local a goto would enter.
#[test]
fn illegal_gotos_are_syntax_errors() {
    for (source, says) in [
        ("goto nowhere", "no visible label 'nowhere'"),
        ("goto inside do ::inside:: end", "no visible label 'inside'"),
        (
            "::outer:: local f = function() goto outer end",
            "no visible label 'outer'",
        ),
        (
            "goto inner local f = function() ::inner:: end",
            "no visible label 'inner'",
        ),
        ("::x:: ::x::", "label 'x' already defined"),
        ("::x:: do ::x:: end", "label 'x' already defined"),
        ("goto L local x = 1 ::L:: x = 2", "scope of local 'x'"),
        (
            "goto L local c <close> = nil ::L:: return",
            "scope of local 'c'",
        ),
        (
            "goto L local function f() end ::L:: return f",
            "scope of local 'f'",
        ),
        (
            "do goto L; local x = 10; ::L:: return x end",
            "scope of local 'x'",
        ),
        (
            "repeat goto L; local x = 10; ::L:: until true",
            "scope of local 'x'",
        ),
        (
            "for i = 1, 2 do goto L end local y = 1 ::L:: return",
            "scope of local 'y'",
        ),
    ] {
        match crate::compile(source.as_bytes()) {
            Err(error) => {
                assert_eq!(error.kind, crate::CompileErrorKind::Syntax, "{source}");
                assert!(error.message.contains(says), "{source}: {}", error.message);
            }
            Ok(_) => panic!("compiled: {source}"),
        }
    }
    // Trailing labels count the block's locals out; siblings and a later
    // outer label may reuse a name.
    for source in [
        "do goto L; local x = 10; ::L:: end",
        "do goto L; local x = 10; ::L:: ; ::M:: ; end",
        "do ::x:: end do ::x:: end ::x::",
        "while g do goto E end local z ::E::",
    ] {
        assert!(crate::compile(source.as_bytes()).is_ok(), "{source}");
    }
}

/// A `__close` that waits, run because a goto leaves its scope: restored
/// at the wait and after the completion, it closes once and the goto
/// lands once.
#[test]
fn a_waiting_close_on_a_goto_restores_and_lands_once() {
    use crate::host::LegacyCompletion;
    let source = b"local n, closes, landed = 0, 0, 0 \
        ::again:: \
        do \
          local v <close> = setmetatable({}, { __close = function() closes = closes + 1 park() end }) \
          n = n + 1 \
          if n < 3 then goto again end \
        end \
        landed = landed + 1 \
        return n, closes, landed";
    let answers = vec![LegacyCompletion::Return(vec![]); 3];
    let (straight, fuel) = super::generic_for::drive(source, &answers, false);
    assert_eq!(straight, "3\t3\t1\n");
    assert_eq!(
        super::generic_for::drive(source, &answers, true),
        (straight, fuel)
    );
}

/// `::a:: goto a` makes progress one instruction at a time, pauses on a
/// finite quantum, restores in the middle, and ends at the fuel limit.
#[test]
fn an_endless_goto_loop_is_bounded_by_fuel() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let chunk = crate::compile(b"local n = 0 ::a:: n = n + 1 goto a").unwrap();
            let config = Config {
                fuel_limit: Some(10_000),
                ..Config::default()
            };
            let mut runtime =
                Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
            let mut journal = Journal::new();
            for _ in 0..100 {
                assert!(matches!(
                    runtime.run(1, &mut journal).unwrap(),
                    StepOutcome::Paused(_)
                ));
            }
            let mut runtime = Runtime::from_snapshot(
                &runtime.snapshot().unwrap(),
                &HostRegistry::proof(),
                runtime.effect_domain(),
            )
            .unwrap();
            assert!(matches!(
                runtime.run(1_000, &mut journal).unwrap(),
                StepOutcome::Paused(_)
            ));
            assert_eq!(
                runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
            );
            assert_eq!(runtime.fuel_consumed(), 10_000);
            assert!(runtime.memory().objects < 100);
        });
    }
}

/// A goto loop that allocates a table on every pass runs 50,000 passes
/// under a 256 KiB heap quota: what a pass leaves behind is garbage.
#[test]
fn a_goto_loop_leaves_only_garbage_behind() {
    let chunk = crate::compile(
        b"local n, keep = 0, nil ::again:: do local t = { n } local f = function() return t end \
          keep = f n = n + 1 if n < 50000 then goto again end end return keep()[1]",
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
    assert_eq!(print_line(&runtime), "49999\n");
    assert!(runtime.memory().collections > 0);
}
