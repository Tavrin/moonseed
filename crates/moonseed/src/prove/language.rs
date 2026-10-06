//! The language-core audit (Phase 3.20): the compiler's limits at their
//! exact boundaries. See `docs/LUA_LANGUAGE_AUDIT.md`.

/// The largest `n` for which `source(n)` compiles, found by bisection over
/// `1..=hi`; `n + 1` must fail with `Limit`.
fn largest(source: impl Fn(usize) -> String, hi: usize, limits: &crate::CompileLimits) -> usize {
    let ok = |n: usize| crate::compile_with_limits(source(n).as_bytes(), limits).is_ok();
    assert!(ok(1));
    assert!(!ok(hi), "{hi} still compiles");
    let (mut good, mut bad) = (1, hi);
    while bad - good > 1 {
        let mid = (good + bad) / 2;
        if ok(mid) {
            good = mid;
        } else {
            bad = mid;
        }
    }
    let error = crate::compile_with_limits(source(good + 1).as_bytes(), limits).unwrap_err();
    assert_eq!(error.kind, crate::CompileErrorKind::Limit, "{error:?}");
    good
}

/// Configured compiler limits and the parser, local, and upvalue
/// bounds, with one more refused as `Limit`. Small code limits keep bisection
/// cheap; Phase 3.30's defaults are exercised by the compiler's large tests.
/// A literal is now bounded only by the source, including its ten-byte syntax.
#[test]
fn frontend_limits_hold_at_their_boundaries() {
    let limits = crate::CompileLimits {
        max_instructions: 10_000,
        max_constants: 4_096,
        max_functions: 256,
        max_source_bytes: (1 << 16) + 10,
    };
    // A name, the source for size `n`, a size that fails, and the largest
    // that compiles.
    type Case = (&'static str, Box<dyn Fn(usize) -> String>, usize, usize);
    let cases: Vec<Case> = vec![
        (
            "string literal bytes",
            Box::new(|n| format!("return #\"{}\"", "a".repeat(n))),
            70_000,
            65_536,
        ),
        (
            "nested parentheses",
            Box::new(|n| format!("return {}1{}", "(".repeat(n), ")".repeat(n))),
            300,
            196,
        ),
        (
            "nested functions",
            Box::new(|n| {
                format!(
                    "return {}1{}",
                    "(function() return ".repeat(n),
                    " end)()".repeat(n)
                )
            }),
            100,
            65,
        ),
        (
            "locals",
            Box::new(|n| {
                (0..n)
                    .map(|i| format!("local a{i} = {i} "))
                    .collect::<String>()
                    + "return a0"
            }),
            300,
            200,
        ),
        (
            "upvalues of one function",
            Box::new(|n| {
                let outer = n.min(150);
                let inner = n - outer;
                let mut source: String = (0..outer).map(|i| format!("local a{i} = 1 ")).collect();
                source.push_str("local f = function() ");
                source.extend((0..inner).map(|i| format!("local b{i} = 1 ")));
                let names: Vec<String> = (0..outer)
                    .map(|i| format!("a{i}"))
                    .chain((0..inner).map(|i| format!("b{i}")))
                    .collect();
                source.push_str(&format!(
                    "return function() return {{ {} }} end end return f()()",
                    names.join(", ")
                ));
                source
            }),
            300,
            255,
        ),
        (
            "functions directly inside one function",
            Box::new(|n| {
                "local t = {} ".to_string()
                    + &(0..n)
                        .map(|i| format!("t[{i}] = function() return {i} end "))
                        .collect::<String>()
                    + "return #t"
            }),
            400,
            255,
        ),
        (
            "functions in a chunk, the chunk's own included",
            Box::new(|n| {
                let direct = n.min(150);
                let mut source = String::from("local f = function() ");
                source.extend((0..n - direct).map(|i| format!("local g{i} = function() end ")));
                source.push_str("end ");
                source.extend((1..direct).map(|i| format!("local h{i} = function() end ")));
                source
            }),
            400,
            255,
        ),
        (
            "constants of one function",
            Box::new(|n| {
                "local t = {} ".to_string()
                    + &(0..n).map(|i| format!("t.k{i} = 1 ")).collect::<String>()
            }),
            5_000,
            4_096,
        ),
        (
            "instructions of one function",
            Box::new(|n| "local x = 0 ".to_string() + &"x = x + 1 ".repeat(n)),
            12_000,
            // ArithK into the result slot and a provisional local
            // store. The store folds only after the emission limit is checked.
            4_999,
        ),
    ];
    for (name, source, hi, expected) in cases {
        assert_eq!(largest(source, hi, &limits), expected, "{name}");
    }
}

/// PUC accepts long flat operator/suffix chains, while right-associative
/// concatenation still consumes parser C levels.
#[test]
fn long_flat_chains_compile_without_host_stack_growth() {
    // An operator or suffix, and the source for a chain of `n` of them.
    type Chain = (&'static str, fn(usize) -> String);
    let chains: [Chain; 9] = [
        ("function name", |n| {
            format!("function a{}() end", ".a".repeat(n))
        }),
        ("+", |n| format!("return {}1", "1 + ".repeat(n))),
        ("or", |n| format!("return {}1", "nil or ".repeat(n))),
        ("and", |n| format!("return {}1", "1 and ".repeat(n))),
        ("==", |n| format!("return {}1", "1 == ".repeat(n))),
        (".a", |n| {
            format!("local t = {{}} return t{}", ".a".repeat(n))
        }),
        ("[1]", |n| {
            format!("local t = {{}} return t{}", "[1]".repeat(n))
        }),
        ("()", |n| format!("local f return f{}", "()".repeat(n))),
        (":m()", |n| format!("local o return o{}", ":m()".repeat(n))),
    ];
    for (name, chain) in chains {
        assert!(crate::compile(chain(150).as_bytes()).is_ok(), "{name}");
        for n in [302, 1_000, 1_500, 100_000] {
            assert!(crate::compile(chain(n).as_bytes()).is_ok(), "{name} {n}");
        }
    }
    for n in [302, 1_000] {
        let source = format!("return {}'x'", "'x' .. ".repeat(n));
        assert_eq!(
            crate::compile(source.as_bytes()).unwrap_err().kind,
            crate::CompileErrorKind::Limit
        );
    }
    // Chains inside parentheses inside chains.
    let mut nested = String::from("1");
    for _ in 0..60 {
        nested = format!("{}({nested})", "1 + ".repeat(60));
    }
    assert!(crate::compile(format!("return {nested}").as_bytes()).is_ok());
}
