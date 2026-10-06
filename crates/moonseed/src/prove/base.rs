//! Lua's base library (Phase 3.21, ADR 0031): `_G`, `_VERSION`, `type`,
//! `assert`, `tostring`, `print`, `tonumber`, `next`, `pairs`, `ipairs`,
//! `collectgarbage`, and `load`. The fixtures print, and their output is
//! Lua 5.4.9's for `print((function() <fixture> end)())`.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::host::{HostValue, LegacyCompletion};

/// Base-library fixtures, with everything Lua 5.4.9 writes when it runs
/// each as `print((function() <fixture> end)())`.
const BASE_FIXTURES: &[(&str, &str)] = &[
    (
        "base_env.lua",
        "true\tLua 5.4\ttrue\nother\tother\tother\ntrue\ttrue\nnil\tboolean\tnumber\tnumber\tstring\ttable\nfunction\tfunction\tfunction\ttable\nfalse\ntrue\ttable\n4\t10\t20\tnil\t30\n1\tv\tm\nfalse\tassertion failed!\nfalse\t42\n2\tfalse\tnil\nfalse\tmessage\nfalse\ttrue\nfalse\ndone\n",
    ),
    (
        "base_tostring.lua",
        "nil\ttrue\tfalse\t1\t-7\n0.5\t-0.0\t1e+100\t9.2233720368548e+18\tinf\t-inf\n3.0\t1e+15\t1.2345678901234e+14\t-3.5\tstr\ntrue\ttrue\ttrue\nT\tT\n42\nfalse\t'__tostring' must return a string\nfalse\t'__tostring' must return a string\na\nN\n1\nfalse\nc2\tc3\tc1\t1\nvia call\n1\tnil\ttrue\tT\t2.5\n1\t2false\t'__tostring' must return a string\nafalse\tE\n1inner\n\tx\t3\n\na\tb\n0\n\t\t\ndone\n",
    ),
    (
        "base_tonumber.lua",
        "nil\tnil\tnil\tnil\n3\t3.5\t3\t3.0\t0\n16\t100.0\t-7\tnil\tnil\nnil\tnil\tnil\tnil\t0.5\n5.0\t16.0\t10\tnil\t0.5\n9223372036854775807\t-1\t9223372036854775807\n9.2233720368548e+18\t-9223372036854775808\t-9.2233720368548e+18\n255\t255\t1295\t-5\t4\nnil\tnil\tnil\tnil\tnil\n9223372036854775807\t-1\t0\nnil\t1\tnil\tnil\tnil\n7766279631452241919\t8\t1\t16\n35\tnil\t10\t7\nfalse\nfalse\nfalse\nfalse\nfalse\n2\n1\t1\ndone\n",
    ),
    (
        "base_iter.lua",
        "false\nfalse\n1\tnil\tnil\nfalse\tinvalid key to 'next'\nfalse\tinvalid key to 'next'\n3\t31\ntrue\ttrue\tnil\t3\nfalse\n3\t1\tnil\n1\t2\t3\n3\nfalse\nc\tnil\tnil\n1\tone\n7\tnil\n30\nfalse\ntrue\t0\t3\ttrue\n1\t2\t1\tnil\nfalse\ntrue\t2\t20\ntrue\t2\t20\nfalse\ntrue\tnil\nfalse\n1\t2\n2\t4\n3\t6\n1\ta\n2\tb\n1\n2\nfalse\n5\t5\ndone\n",
    ),
    (
        "base_gc.lua",
        "0\n1\t0\nnumber\ttrue\ntrue\n0\tfalse\nfalse\t200\n0\ttrue\nfalse\nfalse\ntrue\t0\nfalse\nfalse\ntrue\t0\ntrue\ndone\n",
    ),
    (
        "base_load.lua",
        "true\tnil\tboom\ntrue\n2\t3\ntrue\tnil\treader function must return a string\n3\t4\nnil\tattempt to load a text chunk (mode is 'b')\nnil\tattempt to load a binary chunk (mode is 't')\nfalse\ntrue\tnil\tattempt to load a text chunk (mode is '5')\nnil\tattempt to load a text chunk (mode is 'q')\n1\t2\t3\n5\nfunction\tfalse\n7\t8\nfunction\tfunction\t0\ntrue\n11\nnil\t12\n2\t1\n1\tnil\n55\n42\n42\n1\nnil\tattempt to load a binary chunk (mode is 't')\n1\nnil\tattempt to load a text chunk (mode is 'b')\ntrue\tnil\tH:rd\ntrue\tnil\trd\ntrue\ttrue\tnil\trd\ntrue\tnil\ttable\ndone\n",
    ),
];

pub(super) type Written = Rc<RefCell<Vec<u8>>>;

/// Send the runtime's `print` output to a buffer.
pub(super) fn capture(runtime: &mut Runtime) -> Written {
    let written = Rc::new(RefCell::new(Vec::new()));
    let sink = written.clone();
    runtime.set_output(Box::new(move |bytes| {
        sink.borrow_mut().extend_from_slice(bytes)
    }));
    written
}

pub(super) fn restore(runtime: &Runtime) -> Runtime {
    Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap()
}

pub(super) fn text(written: &Written) -> String {
    String::from_utf8_lossy(&written.borrow()).into_owned()
}

/// Run to the end with a checkpoint at every step. Restored with the
/// journal as it was at the checkpoint, a run writes exactly what the
/// straight run wrote after that point; restored with the journal of the
/// finished run, it writes nothing, because every write it would make is
/// an effect already committed (ADR 0031).
pub(super) fn output_at_every_checkpoint(spec: &crate::program::ProtoSpec) {
    output_at_every_checkpoint_with(boot_natives, spec);
}

/// [`output_at_every_checkpoint`], booting with `boot`.
pub(super) fn output_at_every_checkpoint_with(
    boot: fn(&crate::program::ProtoSpec) -> Runtime,
    spec: &crate::program::ProtoSpec,
) {
    let (full, finished) = {
        let mut runtime = boot(spec);
        let written = capture(&mut runtime);
        let mut journal = Journal::new();
        let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
        (text(&written), journal)
    };
    let mut walker = boot(spec);
    let seen = capture(&mut walker);
    let mut journal = Journal::new();
    loop {
        let mut restored = restore(&walker);
        let rest = capture(&mut restored);
        restored
            .run_until_terminal(u64::MAX, &mut journal.clone())
            .unwrap();
        assert_eq!(text(&seen) + &text(&rest), full);
        let mut replayed = restore(&walker);
        let again = capture(&mut replayed);
        replayed
            .run_until_terminal(u64::MAX, &mut finished.clone())
            .unwrap();
        assert_eq!(text(&again), "");
        match walker.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(text(&seen), full);
}

#[test]
fn base_fixtures_match_lua_under_fuel_gc_and_checkpoints() {
    for (name, expected) in BASE_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let mut runtime = boot_natives(&chunk.proto);
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
        quantum_and_checkpoints_with(boot_natives, &chunk.proto, pair_results);
        collect_every_safe_point_with(boot_natives, &chunk.proto, &results);
        output_at_every_checkpoint(&chunk.proto);
    }
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_base_fixtures() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    for (name, expected) in BASE_FIXTURES {
        let body = crate::hostcaps::native::test_support::read_to_string(root.join(name)).unwrap();
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
}

/// Whether a coroutine may yield across a base function is Lua 5.4.9's
/// answer (ADR 0031): `__pairs` may yield; `__tostring` under `tostring`
/// or `print`, `__index` under `ipairs`'s iterator, and a `load` reader
/// may not. `load` returns that error, as Lua's does. An `__index` called
/// by an instruction may yield, as before.
#[test]
fn yields_cross_base_functions_as_in_lua_5_4_9() {
    let across = "false\tattempt to yield across a C-call boundary";
    let cases = [
        (
            "return pcall(tostring, setmetatable({}, { __tostring = function() yield('y') return 's' end }))",
            1,
            across.to_string(),
        ),
        (
            "return pcall(print, setmetatable({}, { __tostring = function() yield('y') return 's' end }))",
            1,
            across.to_string(),
        ),
        (
            "return pcall(function() for i in ipairs(setmetatable({}, { __index = function() yield('i') end })) do end end)",
            1,
            across.to_string(),
        ),
        (
            "return load(function() yield('l') return nil end)",
            1,
            "nil\tattempt to yield across a C-call boundary".to_string(),
        ),
        (
            "local n = 0 for k in pairs(setmetatable({}, { __pairs = function(t) yield('p') return next, { 1, 2 }, nil end })) do n = n + 1 end return n",
            2,
            "p\tnil\t2\tnil".to_string(),
        ),
        (
            "local t = setmetatable({}, { __index = function() yield('x') return 1 end }) return t.a",
            2,
            "x\tnil\t1\tnil".to_string(),
        ),
    ];
    for (source, resumes, line) in cases {
        let spec = crate::program::source_coroutine_program(source, resumes);
        let runtime = finish_with(boot_natives, &spec);
        assert_eq!(print_line(&runtime), format!("{line}\n"), "{source}");
        let results = pair_results(&runtime);
        quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
        collect_every_safe_point_with(boot_natives, &spec, &results);
    }
}

/// Drive `source` to its end with output captured, answering each wait
/// on key 1 with the next of `answers`, and restoring at each wait and
/// after each answer. Gives what was written and the result line.
fn drive_output(source: &[u8], answers: &[LegacyCompletion], checkpoint: bool) -> (String, String) {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let written = capture(&mut runtime);
    let mut journal = Journal::new();
    let mut answers = answers.iter();
    let reopen = |runtime: &Runtime| {
        let mut restored = restore(runtime);
        let sink = written.clone();
        restored.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
        restored
    };
    loop {
        match runtime.run_until_terminal(u64::MAX, &mut journal).unwrap() {
            StepOutcome::Completed => break,
            StepOutcome::Waiting(key) => {
                if checkpoint {
                    runtime = reopen(&runtime);
                }
                let answer = answers.next().expect("an answer for every wait");
                runtime.complete_legacy(key, answer.clone()).unwrap();
                if checkpoint {
                    runtime = reopen(&runtime);
                }
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(answers.next().is_none(), "a wait never came");
    (text(&written), print_line(&runtime))
}

/// A `__tostring` under `print` or `tostring`, and a `load` reader, that
/// wait on the host: restored at the wait and after it, the output comes
/// once, in order, and a reader's pieces are kept.
#[test]
fn waits_inside_base_functions_restore_without_repeating_output() {
    let back =
        |text: &str| LegacyCompletion::Return(vec![HostValue::String(text.as_bytes().to_vec())]);
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str, &str)> = vec![
        (
            b"print('a', setmetatable({}, { __tostring = function() print('in') return park() end }), 'c') return 1",
            vec![back("B")],
            // Lua writes an argument's tab after converting it.
            "ain\n\tB\tc\n",
            "1\n",
        ),
        (
            b"local w = setmetatable({}, { __tostring = function() return park() end }) \
              print(w, 2, w) return tostring(w)",
            vec![back("x"), back("y"), back("z")],
            "x\t2\ty\n",
            "z\n",
        ),
        (
            b"local n = 0 local f = load(function() n = n + 1 return park() end, '=parked') \
              return f(), n",
            vec![back("return 4"), back("0 + 2"), LegacyCompletion::Return(vec![])],
            "",
            "42\t3\n",
        ),
        (
            b"local seen = 0 for k, v in pairs(setmetatable({}, { __pairs = function(t) park() return next, { 5, 6 }, nil end })) do seen = seen + v end \
              for i, v in ipairs(setmetatable({}, { __index = function(t, i) if i < 3 then return park() end end })) do seen = seen + i end \
              return seen",
            vec![back("p"), back("a"), back("b")],
            "",
            "14\n",
        ),
    ];
    for (source, answers, output, line) in cases {
        let straight = drive_output(source, &answers, false);
        assert_eq!(
            straight,
            (output.to_string(), line.to_string()),
            "{}",
            String::from_utf8_lossy(source)
        );
        assert_eq!(drive_output(source, &answers, true), straight);
    }
}

/// What `tostring` gives a value without `__tostring`: the kind, or a
/// string `__name`, and the object's `ObjectId`; a native function's
/// registry symbol. The same object gives the same text after a restore.
#[test]
fn default_text_is_a_deterministic_identity() {
    let chunk = crate::compile(
        b"local t = {} local named = setmetatable({}, { __name = 'Point' }) \
          local a = tostring(t) park() \
          return a, a == tostring(t), tostring(named), tostring(setmetatable({}, { __name = 42 })), \
          tostring(print), tostring(function() end)",
    )
    .unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    let mut runtime = restore(&runtime);
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let line = print_line(&runtime);
    let fields: Vec<&str> = line.trim_end().split('\t').collect();
    let hex = |text: &str, prefix: &str| {
        let digits = text
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            digits.len() >= 8 && digits.bytes().all(|b| b.is_ascii_hexdigit()),
            "{text}"
        );
    };
    hex(fields[0], "table: 0x");
    assert_eq!(fields[1], "true");
    hex(fields[2], "Point: 0x");
    hex(fields[3], "table: 0x");
    assert_eq!(fields[4], "function: builtin: base.print");
    hex(fields[5], "function: 0x");
}

/// A sandbox installs only part of the base library.
#[test]
fn a_sandbox_installs_part_of_the_base_library() {
    let chunk =
        crate::compile(b"return print == nil, load == nil, type(1), _G == nil, _VERSION").unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_base_only(&["type", "_VERSION"]).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "true\ttrue\tnumber\ttrue\tLua 5.4\n");
    let mut runtime = boot_spec(&chunk.proto);
    assert_eq!(
        runtime.install_base_only(&["require"]),
        Err(VmError::UnknownNative)
    );
}

/// `collectgarbage("stop")` survives a checkpoint; the collector modes
/// Moonseed does not have are errors of their own class; `count` is the
/// logical heap.
#[test]
fn collectgarbage_state_and_gaps() {
    let chunk = crate::compile(b"collectgarbage('stop') park() return collectgarbage('isrunning')")
        .unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    let mut runtime = restore(&runtime);
    assert!(!runtime.memory().auto_gc);
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "false\n");
    // The mode the runtime boots in is generational; each change returns
    // the mode before it (ADR 0051).
    let chunk = crate::compile(
        b"return collectgarbage('incremental'), collectgarbage('generational'), \
          collectgarbage('generational'), collectgarbage('incremental')",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(
        print_line(&runtime),
        "generational\tincremental\tgenerational\tgenerational\n"
    );
    let chunk = crate::compile(b"return collectgarbage('count') * 1024").unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    let memory = runtime.memory();
    assert_eq!(
        pair_results(&runtime),
        vec![Observed::Float((memory.logical_bytes as f64).to_bits())]
    );
}

/// A reader cannot grow the source past the source limit, nor past the
/// heap quota: `load` stops and returns the failure, and the memory is
/// free again once it has.
#[test]
fn a_reader_is_bounded_by_the_source_limit_and_the_heap_quota() {
    let big = b"local s = 'x' for i = 1, 16 do s = s .. s end \
        local calls = 0 local f, message = load(function() calls = calls + 1 return s end) \
        return f, message, calls";
    let runtime = finish_with(boot_natives, &crate::compile(big).unwrap().proto);
    assert_eq!(
        print_line(&runtime),
        format!(
            "nil\tsource exceeds {} bytes\t{}\n",
            crate::limits::DEFAULT_SOURCE_BYTES,
            crate::limits::DEFAULT_SOURCE_BYTES / (1 << 16) + 1
        )
    );
    let chunk = crate::compile(
        b"local s = 'x' for i = 1, 16 do s = s .. s end \
          local f, message = load(function() return s end) \
          s = nil collectgarbage() \
          return f, message, collectgarbage('count') < 256",
    )
    .unwrap();
    let config = Config {
        max_logical_heap: 512 * 1024,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "nil\tnot enough memory\ttrue\n");
}

/// A compile error names the chunk as Lua's does; a binary chunk is a
/// clean failure; `load_function` makes a chunk's function for the host.
#[test]
fn load_failures_and_the_host_api() {
    let chunk = crate::compile(
        b"local a, b = load('x = ', '=chunk') local c, d = load('\\n\\nx = ') \
          local e, f = load('\\27Lua') local g, h = load(function() return nil end, '@f.lua', 'b') \
          return a, b, c, d, e, f, g, h",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    let line = print_line(&runtime);
    let fields: Vec<&str> = line.trim_end().split('\t').collect();
    assert_eq!(fields[0], "nil");
    assert!(fields[1].starts_with("chunk:1: "), "{line}");
    assert!(fields[3].starts_with("[string \"...\"]:3: "), "{line}");
    assert_eq!(
        fields[5],
        "binary string: bad binary format (not a Moonseed chunk)"
    );
    assert_eq!(fields[7], "attempt to load a text chunk (mode is 'b')");
    let mut runtime = boot_spec(&crate::program::park_program());
    let id = runtime
        .load_function(&crate::compile(b"return 40 + 2").unwrap())
        .unwrap();
    assert_eq!(runtime.call_closure(id, &mut Journal::new()).unwrap(), 42);
}

#[test]
fn protected_load_counts_its_c_frame_at_the_parser_limit() {
    let accepted = format!("return {}1{}", "(".repeat(194), ")".repeat(194));
    let rejected = format!("return {}1{}", "(".repeat(195), ")".repeat(195));
    let source = format!(
        "local a, accepted = pcall(load, '{accepted}', '=depth') \
         local b, rejected, message = pcall(load, '{rejected}', '=depth') \
         return a, type(accepted), b, type(rejected), message"
    );
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(
        print_line(&runtime),
        "true\tfunction\ttrue\tnil\tC stack overflow\n"
    );
}

/// An error token can fit as source text while its complete diagnostic is
/// larger than the configured string bound. `load` returns its reserved
/// memory error rather than trying to install an oversized string.
#[test]
fn load_compile_diagnostic_respects_the_string_limit() {
    let chunk = crate::compile(
        b"local bad = 'if true ' .. string.rep('x', 1010) .. ' end' \
          local f, message = load(bad, '=short') return f, message",
    )
    .unwrap();
    let config = Config {
        max_string_bytes: 1_024,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    runtime.install_string().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "nil\tnot enough memory\n");
}

/// Guest source compiles within an AST allocation budget drawn from the heap's
/// headroom. Before it, a 15 MB dense chunk under the default quota made a
/// syntax tree of over 4 GB before any compiler limit was reached.
#[test]
fn load_syntax_tree_is_bounded_by_the_heap_quota() {
    let chunk = crate::compile(
        b"local small = load(string.rep('x=1 ', 1000)) \
          local f, message = load(string.rep('f{}', 300000)) \
          return type(small), f, message",
    )
    .unwrap();
    let config = Config {
        max_logical_heap: 4 << 20,
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    runtime.install_string().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "function\tnil\tnot enough memory\n");

    // The exact security-review reproducer: 15.2 MB under a 64 MiB heap.
    let chunk = crate::compile(
        br#"local f, message = load(("a={{{{{{{{}}}}}}}}\n"):rep(800000))
            return f, message"#,
    )
    .unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    runtime.install_base().unwrap();
    runtime.install_string().unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(print_line(&runtime), "nil\tnot enough memory\n");
}

/// Official big.lua's large constructor must fit both the default and suite
/// configurations. Separators retain no AST storage; charging 512 bytes for
/// each token incorrectly refused this program after the security fix.
#[test]
fn load_large_constructor_fits_default_and_suite_heap() {
    let chunk = crate::compile(
        br#"local lim = 2^18 + 1000
            local prog = { "local y = {0" }
            for i = 1, lim do prog[#prog + 1] = i end
            prog[#prog + 1] = "}\n"
            prog[#prog + 1] = "X = y\n"
            prog[#prog + 1] = ("assert(X[%d] == %d)"):format(lim - 1, lim - 2)
            prog[#prog + 1] = "return 0"
            prog = table.concat(prog, ";")
            local env = {string = string, assert = assert}
            local f = assert(load(prog, nil, nil, env))
            assert(f() == 0)
            assert(env.X[lim] == lim - 1 and env.X[lim + 1] == lim)
            return 0"#,
    )
    .unwrap();
    for config in [
        Config::default(),
        Config {
            fuel_limit: Some(200_000_000),
            ..Config::default()
        },
    ] {
        let mut runtime = Runtime::builder()
            .config(config)
            .libraries(crate::Libraries::ALL)
            .build()
            .unwrap();
        runtime.load_main(&chunk).unwrap();
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(print_line(&runtime), "0\n");
    }
}

/// Every combination of leading space, sign, numeral, and trailing byte,
/// converted with no base and with seven bases: 4,000 strings, 32,000
/// conversions, compared with Lua 5.4.9.
const TONUMBER_CORPUS: &str = r#"
local pre = { "", " ", "\t", "\n\v " }
local sign = { "", "-", "+", "- ", "--" }
local body = { "0", "1", "10", "7f", "ff", "FF", "z", "Z", "zz", "1e2", "1E+2", "1.5", ".5", "5.", ".",
  "0x10", "0x", "0X1p4", "0x.8", "0x1P-2", "inf", "nan", "1e999", "9223372036854775807",
  "9223372036854775808", "18446744073709551615", "18446744073709551616", "99999999999999999999",
  "1e", "e1", "", "1 2", "12abc", "0b101", "\u{663}", "007", "0x7fffffffffffffff", "0xffffffffffffffffff",
  "1e-400", "36" }
local post = { "", " ", "\0", "x", "\v\f\r" }
for _, a in ipairs(pre) do
  for _, b in ipairs(sign) do
    for _, c in ipairs(body) do
      for _, d in ipairs(post) do
        local s = a .. b .. c .. d
        print(tonumber(s), tonumber(s, 2), tonumber(s, 3), tonumber(s, 8), tonumber(s, 10),
          tonumber(s, 16), tonumber(s, 35), tonumber(s, 36))
      end
    end
  end
end
return "done"
"#;

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_a_tonumber_corpus() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    let output = lua_command(&lua)
        .arg("-e")
        .arg(format!("print((function()\n{TONUMBER_CORPUS}\nend)())"))
        .output()
        .unwrap();
    assert!(output.status.success(), "the corpus failed in Lua");
    let chunk = crate::compile(TONUMBER_CORPUS.as_bytes()).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let written = capture(&mut runtime);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let ours = text(&written) + &print_line(&runtime);
    let theirs = String::from_utf8(output.stdout).unwrap();
    assert_eq!(ours.lines().count(), 4_001);
    for (index, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
        assert_eq!(a, b, "line {}", index + 1);
    }
    assert_eq!(ours, theirs);
}

/// Restore checks a base-function frame as it checks a protected call's:
/// it sits on the call it stands for, its task's cursor stays within its
/// arguments, the frame above answers the call its task makes, and a
/// `load` holds at most the source limit.
#[test]
fn restore_refuses_base_frames_the_runtime_cannot_make() {
    use crate::heap::Task;
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
    let each = |image: &mut Image, change: &dyn Fn(&mut crate::snapshot::FrameImage)| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if matches!(frame.boundary, Some(BoundaryImage::Builtin { .. })) {
                    change(frame);
                }
            }
        }
    };
    // Not a tail call, so the Lua `__tostring` frame stays above `print`'s.
    let printing = run_to_wait(
        b"local w = setmetatable({}, { __tostring = function() local s = park() return s end }) \
          print(1, 2, w) return 0",
    );
    restore(&printing);
    let task = |change: fn(&mut Task, &mut u32, &mut u32, &mut bool)| {
        move |frame: &mut crate::snapshot::FrameImage| {
            if let Some(BoundaryImage::Builtin {
                func,
                passed,
                advance_caller,
                task,
            }) = &mut frame.boundary
            {
                change(task, func, passed, advance_caller);
            }
        }
    };
    // The cursor past the arguments.
    check(&printing, &|image| {
        each(
            image,
            &task(|task, _, _, _| *task = Task::Print { next: 3 }),
        )
    });
    // Not on its caller's `Call`.
    check(&printing, &|image| {
        each(image, &task(|_, func, _, _| *func += 1))
    });
    check(&printing, &|image| {
        each(image, &task(|_, _, _, advance| *advance = !*advance))
    });
    check(&printing, &|image| {
        each(image, &|frame| {
            frame.nresults = frame.nresults.wrapping_add(1)
        })
    });
    // A task whose call wants three results, under a frame giving one.
    check(&printing, &|image| {
        each(image, &task(|task, _, _, _| *task = Task::Pairs))
    });
    // A load source too large for the snapshot is refused before restore.
    let reading = run_to_wait(b"return load(function() return park() end)");
    restore(&reading);
    let mut image = reading.to_image().unwrap();
    each(
        &mut image,
        &task(|task, _, _, _| {
            *task = Task::Load {
                source: vec![b' '; crate::limits::DEFAULT_SOURCE_BYTES + 1],
            }
        }),
    );
    assert_eq!(
        snapshot::encode(&image).unwrap_err(),
        SnapshotError::LimitExceeded
    );
}

/// `print` hands the output its text in pieces no longer than 64 KiB or
/// the longest string, never one buffer of everything (review of Phase
/// 3.21: a `print` of 16,000 one-mebibyte strings aborted the host).
#[test]
fn print_writes_in_bounded_pieces() {
    let chunk = crate::compile(
        b"local s = 'x' for i = 1, 15 do s = s .. s end \
          local big = s .. s .. s \
          print(s, s, s, s, s, 1, big, 2) return #s, #big",
    )
    .unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let pieces: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = pieces.clone();
    runtime.set_output(Box::new(move |bytes| sink.borrow_mut().push(bytes.len())));
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "32768\t98304\n");
    let pieces = pieces.borrow();
    let total: usize = pieces.iter().sum();
    assert_eq!(total, 5 * 32_768 + 1 + 98_304 + 1 + 7 + 1);
    assert!(pieces.iter().all(|len| *len <= 98_304), "{pieces:?}");
    assert!(pieces.len() > 2, "{pieces:?}");
}

/// A `load` reader that is a native returns at once; each further call
/// is still a step that costs fuel and quantum, so it cannot run without
/// end inside one quantum (review of Phase 3.21).
#[test]
fn each_reader_call_costs_fuel() {
    let chunk = crate::compile(b"return load(many)").unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    for _ in 0..50 {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    assert!(runtime.fuel_consumed() <= 50, "{}", runtime.fuel_consumed());
    let config = Config {
        fuel_limit: Some(10_000),
        ..Config::default()
    };
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    runtime.set_global_native("many", "many").unwrap();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
    );
}
