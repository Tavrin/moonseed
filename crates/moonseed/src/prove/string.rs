//! The string library (Phase 3.23, ADR 0034, ADR 0035, ADR 0036).
//! Fixtures give Lua 5.4.9's output under every schedule; generated
//! corpora match it; long string functions cost fuel as they go; no yield
//! crosses them where Lua forbids one; their states restore, and states
//! the runtime cannot make are refused.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::{capture, output_at_every_checkpoint_with, restore, text};
use super::*;
use crate::host::{HostValue, LegacyCompletion};

/// String library fixtures, with everything Lua 5.4.9 writes when it runs
/// each as `print((function() <fixture> end)())`.
const STRING_FIXTURES: &[(&str, &str)] = &[
    (
        "lib_immediate.lua",
        "7\tnil\tnil\t1\n9.007199254741e+15\t9007199254740993\n-inf\ttrue\n99\tnil\tnil\t98\n97\t50\t98\n32\t33\n3\nfalse\tbad argument #1 to 'abs' (number expected, got table)\nfalse\tbad argument #1 to 'floor' (number expected, got boolean)\nfalse\tbad argument #1 to 'type' (value expected)\nfalse\tbad argument #1 to 'byte' (string expected, got table)\n9\tnil\t4.0\t3\ndone\n",
    ),
    (
        "lib_string.lua",
        "true true nil nil\n\"ello\" \"ell\" \"llo\" \"x\" \"\" \"\"\n\"abc\" \"abc\" \"c\" \"\"\n4 3 3 4 \"\\0\\1\\0\\1\\0\"\n\"MIXED \\200\" \"mixed \\200\" \"d\\0cba\" \"\"\n\"ababab\" \"ab, ab, ab\" \"\" \"\" \"x\" \"\"\n65 66 67 65 66 67\nnil nil nil 255 0\n\"\" \"Hi\\0\\255\" \"A\"\nfalse \"bad argument #1 to 'string.char' (value out of range)\"\nfalse \"bad argument #1 to 'string.char' (value out of range)\"\nfalse \"bad argument #1 to 'string.rep' (string expected, got no value)\"\nfalse \"bad argument #2 to 'string.sub' (number has no integer representation)\"\nfalse \"bad argument #1 to 'string.upper' (string expected, got table)\"\nfalse \"bad argument #2 to 'string.byte' (number expected, got string)\"\n\"12\" \"111\" \"5.1\"\n11 32 3.0 -2 3 8.0 1.5 4\nfalse \"attempt to add a 'string' with a 'number'\"\nfalse \"attempt to add a 'table' with a 'string'\"\nfalse \"attempt to sub a 'string' with a 'table'\"\nfalse\nfalse\n\"right\"\n\"left\"\n\"patched\" \"patched\" 2\nfalse\n42\n\"foo!\" \"1!\"\n\"Q\" false\n\"RESTORED\"\nfalse\ndone\n",
    ),
    (
        "lib_pattern.lua",
        "5 3 3 4 \"l\" \"l\"\n2 2 2 2\nnil nil 4 1 0\nnil 1 3 2 3\n\"key\" 3 nil\n\"trim\" \"2024\" \"01\" \"02\"\n\"quick\" \"[[x]]\" \"(a(b)c)\"\n\"\" \"hello\" \"aaab\" \"aaa\"\n\"ab\" \"xyz\" \"a\" \"b\" \"c\"\n1 \"W W\" 2\n\"\\0\" 2 \"\\0b\"\n\"\\195\\169\" \"x9_\" \"a-\"\n\"]]\" \"^a\" \"bc\" \"A\"\n\"a=1,b=2\" \"one,two,three\"\n\",,,\" \"aaa\" \",,\"\n\"^a,^a\" \"3,4\" \"b,c\"\n\"\" \"3,4\"\n\"function\" \"x\" \"y\" nil\nfalse \"a\" \"a\" \"a\"\n\"hell0 w0rld\" \"-h-e-l-l-o-\" \"aabbcc\" 3\n\"<hello> <world>\" \"a[3]c\" \"a%c\" 1\n\"aBc\" \"1bc\" \"abC\" 3\n\"A.B.C.\" 3\n\"abc\" 3\n\"7,7\" \"1a2b3\" 3\n\"heLlo\" \"hello\" \"hello\" 0\n\"^hello\" \"baa\" \"-\" \"e\" 1\n\"/a/b/c/\" \"ab c\" \"12945\" \"12\" 1\n\"a<b>c\" 3\n\"a-b-c\" 2\nfalse \"invalid replacement value (a table)\"\nfalse \"invalid replacement value (a table)\"\nfalse \"invalid capture index %2\"\nfalse \"invalid use of '%' in replacement string\"\nfalse \"invalid use of '%' in replacement string\"\nfalse \"bad argument #3 to 'string.gsub' (string/function/table expected, got boolean)\"\nfalse \"stop\"\nfalse \"malformed pattern (missing ']')\"\nfalse \"unfinished capture\"\nfalse \"malformed pattern (ends with '%')\"\nfalse \"invalid pattern capture\"\nfalse \"invalid capture index %1\"\nfalse \"malformed pattern (missing arguments to '%b')\"\nfalse \"missing '[' after '%f' in pattern\"\nfalse \"invalid capture index %1\"\nfalse \"too many captures\"\nfalse \"pattern too complex\"\n1 3001\n1 4001\n5 1 2\ndone\n",
    ),
    (
        "lib_format.lua",
        "\"42 -7 3 10 ff FF A\" \"    1|2    |00003|+4| 5\"\n\"007||010|0xff|0XFF\" \"9223372036854775807 -9223372036854775808\" \"18446744073709551615\"\n\"3.141590 3.141590e+04 0.0001 1.000000E+20 1E-20\" \"0 2 2 -0\"\n\"     3.142|2.50e+00  |+0.05| 7|1.00000|2.\"\n\"0.10000000000000000555\" \"0.10000000000000001\" \"1e+15 1e+16 1.23457e+11\" 101\n\"0x1p+0 0X1P-1 0x1.555p-2 0x2p+0\" \"0x0.0000000000001p-1022 -0x0p+0\" \" -0.0|\"\n\"inf -inf inf -inf\" \"  inf|-inf  |\"\n\"nil true 12.5 7\" \"       abc|abc       |ab|    a|\"\n\"a\\0b\" false \"bad argument #2 to 'string.format' (string contains zeros)\"\n120 \"yyy\" 101\n\"[TT] [   TT] [TT   |]\" \"42\"\nfalse \"'__tostring' must return a string\"\n\"\\\"a\\\\\\\nb\\\\0c\\\\\\\"d\\\\\\\\e\\\\13\\\\1\\\\0012\\200\\\"\" \"0x1.5555555555555p-2\" \"0x8000000000000000\" \"255\"\n\"1e9999 -1e9999 0x1p+53 -0x0p+0\" \"nil true false\"\nfalse \"bad argument #2 to 'string.format' (value has no literal form)\"\nfalse \"specifier '%q' cannot have modifiers\"\nfalse \"bad argument #2 to 'string.format' (number has no integer representation)\"\ntrue false \"bad argument #2 to 'string.format' (number expected, got string)\"\nfalse \"bad argument #2 to 'string.format' (no value)\"\nfalse \"invalid conversion '%y' to 'format'\"\nfalse \"invalid conversion specification: '%100d'\"\nfalse \"invalid conversion specification: '%.100f'\"\nfalse \"invalid conversion specification: '%#d'\"\nfalse \"invalid conversion specification: '%-+ #0.3c'\"\nfalse \"invalid conversion '%' to 'format'\"\nfalse \"invalid conversion '%l' to 'format'\"\nfalse \"invalid format (too long)\"\n\"    A|B    |\" \"%|%%\" \"no items\" \"ab\"\n\"9.2233720368548e+18\" \"2.001\" \"2.67\" \"0.000000e+00\" \"100000\" \"1e-05\"\n\"   ab|\" \"|\" \"ffffffffffffffff\" \"1000000000000000000000\"\ndone\n",
    ),
    (
        "lib_pack.lua",
        "\"64000000fffe68656c6c6f000278793ff8000000000000\" 100 -2 \"hello\" \"xy\" 1.5 24\n\"fffffefffffffdffffffffffffff0300000000000000fcffffffffffffff04000000000000000500000000000000\"\n\"0102000300000400000000050000000000000600000000000000f9fffffffffffffffff8ffffffffffffffffffffffffffffff\"\n\"ffffffff01000000000000000002000000000000000000000000000000\"\n\"0100000000000001010000000100000002000000030000000000000000001040\"\n\"0000c03f00000000000002c0000000000000f07f\" \"cdcccc3d\" \"6162000000\" \"010071010000000000000072\"\n\"0001000200000000\" \"01000001\" \"\"\n24 61 8 0\nfalse \"bad argument #1 to 'string.packsize' (variable-length format)\"\nfalse \"bad argument #1 to 'string.packsize' (variable-length format)\"\nfalse \"integral size (17) out of limits [1,16]\"\nfalse \"integral size (0) out of limits [1,16]\"\nfalse \"bad argument #2 to 'string.pack' (integer overflow)\"\nfalse \"bad argument #2 to 'string.pack' (unsigned overflow)\"\nfalse \"invalid format option 'q'\"\nfalse \"missing size for format option 'c'\"\nfalse \"bad argument #2 to 'string.pack' (string longer than given size)\"\nfalse \"bad argument #2 to 'string.pack' (string contains zeros)\"\nfalse \"bad argument #2 to 'string.pack' (string length does not fit in given size)\"\nfalse \"bad argument #1 to 'string.pack' (invalid next option for option 'X')\"\nfalse \"bad argument #1 to 'string.pack' (format asks for alignment not power of 2)\"\nfalse \"bad argument #2 to 'string.pack' (number expected, got string)\"\nfalse \"bad argument #2 to 'string.pack' (number expected, got nil)\"\nfalse \"bad argument #2 to 'string.pack' (number expected, got table)\"\n255 513 258 -128 2\n-1 false \"16-byte integer does not fit into Lua Integer\"\n1 false \"9-byte integer does not fit into Lua Integer\"\nfalse \"bad argument #2 to 'string.unpack' (data string too short)\"\nfalse \"bad argument #3 to 'string.unpack' (initial position out of string)\"\n121 121 false \"bad argument #2 to 'string.unpack' (data string too short)\"\n\"ab\" false \"bad argument #2 to 'string.unpack' (unfinished string for format 'z')\"\n\"abc\" false \"bad argument #2 to 'string.unpack' (data string too short)\"\n\"bcd\" 7 -7 5\n0.10000000149012 0.1 13\n201\n1 2 9\n1 -9223372036854775808 -1 9\ndone\n",
    ),
    (
        "lib_dump.lua",
        "\"string\" 27 1 2 nil 4\n4 true\ntrue nil nil\ntrue nil nil\ntrue true\ntrue false\nfalse \"unable to dump given function\"\nfalse \"bad argument #1 to 'string.dump' (function expected, got number)\"\nfalse \"bad argument #1 to 'string.dump' (function expected, got no value)\"\nnil \"attempt to load a binary chunk (mode is 't')\"\nnil \"attempt to load a text chunk (mode is 'b')\"\n1\n2 3 4\n5 7 true\nfalse\n3\nnil \"attempt to load a text chunk (mode is 'b')\"\nfalse\ndone\n",
    ),
];

/// Boot with the proof natives and every standard library.
fn boot_strings(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_natives(spec);
    runtime.install_math().unwrap();
    runtime.install_table().unwrap();
    runtime.install_string().unwrap();
    runtime
}

/// Run `source` to the end, the strings library installed; its results.
fn finish_strings(source: &str) -> Runtime {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    finish_with(boot_strings, &chunk.proto)
}

#[test]
fn string_fixtures_match_lua_under_fuel_gc_and_checkpoints() {
    for (name, expected) in STRING_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let mut runtime = boot_strings(&chunk.proto);
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
        for quantum in [1u64, 2, 3, 7] {
            let mut runtime = boot_strings(&chunk.proto);
            let mut journal = Journal::new();
            while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
            assert_eq!(pair_results(&runtime), results, "{name} quantum {quantum}");
        }
        restored_and_collected_at_every_step(&chunk.proto, expected, &results);
        // The walks that finish the run from every step are quadratic in
        // its length: the shortest fixture takes them.
        if *name == "lib_dump.lua" {
            collect_every_safe_point_with(boot_strings, &chunk.proto, &results);
            output_at_every_checkpoint_with(boot_strings, &chunk.proto);
        }
    }
}

/// Run to the end, collecting, checkpointing, and restoring at every step
/// on the way: the output and the results are the straight run's.
fn restored_and_collected_at_every_step(
    spec: &crate::program::ProtoSpec,
    expected: &str,
    results: &[Observed],
) {
    let written = Rc::new(RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
    };
    let mut runtime = boot_strings(spec);
    attach(&mut runtime);
    let mut journal = Journal::new();
    loop {
        runtime.collect();
        runtime = restore(&runtime);
        attach(&mut runtime);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    let output = String::from_utf8_lossy(&written.borrow()).into_owned();
    assert_eq!(output + &print_line(&runtime), expected);
    assert_eq!(pair_results(&runtime), results);
}

/// What Moonseed writes for a program, and what Lua 5.4.9 writes for it.
fn both(lua: &str, source: &str) -> (String, String) {
    let output = lua_command(lua)
        .arg("-e")
        .arg(format!("\n{source}"))
        .output()
        .unwrap();
    assert!(output.status.success(), "Lua failed");
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    let written = capture(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    (
        text(&written),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_string_fixtures_and_corpora() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    for (name, expected) in STRING_FIXTURES {
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
    for name in [
        "corpus_string.lua",
        "corpus_pattern.lua",
        "corpus_format.lua",
        "corpus_pack.lua",
        // Long numerals through `tonumber` and `load` (Phase 3.24).
        "corpus_numeral.lua",
    ] {
        let (ours, theirs) = both(&lua, &String::from_utf8(fixture(name)).unwrap());
        for (index, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
            assert_eq!(a, b, "{name} line {}", index + 1);
        }
        assert_eq!(ours.lines().count(), theirs.lines().count(), "{name}");
    }
}

/// Lua 5.4.9 lets a yield through a string metatable's `__index` or a
/// patched `__add` written in Lua, and through `__concat`; not through a
/// call the string library makes: `%s`'s `__tostring`, a `gsub`
/// replacement function or table, or the other operand's metamethod the
/// string metatable's `__add` falls back to.
#[test]
fn yields_cross_the_string_library_where_lua_lets_them() {
    let across = "false\tattempt to yield across a C-call boundary\n";
    for source in [
        "return pcall(string.format, '%s', setmetatable({}, { __tostring = function() yield('y') return 't' end }))",
        "return pcall(string.gsub, 'abc', 'b', function() yield('y') return 'B' end)",
        "return pcall(string.gsub, 'abc', 'b', setmetatable({}, { __index = function() yield('y') return 'B' end }))",
        "return pcall(function() return 'a' + setmetatable({}, { __add = function() yield('y') return 1 end }) end)",
    ] {
        let spec = crate::program::source_coroutine_program(source, 1);
        let runtime = finish_with(boot_strings, &spec);
        assert_eq!(print_line(&runtime), across, "{source}");
    }
    for source in [
        "local mt = getmetatable('') local old = mt.__index \
         mt.__index = function(s, k) yield('y') return k end \
         local r = ('x').foo mt.__index = old return r",
        "local mt = getmetatable('') local old = mt.__add \
         mt.__add = function() yield('y') return 'sum' end \
         local r = 'a' + 1 mt.__add = old return r",
    ] {
        let spec = crate::program::source_coroutine_program(source, 2);
        let runtime = finish_with(boot_strings, &spec);
        let line = print_line(&runtime);
        assert!(line.starts_with("y\t"), "{source}: {line}");
        assert!(
            line.contains("foo") || line.contains("sum"),
            "{source}: {line}"
        );
    }
}

/// A long string function costs a unit of fuel per step, each step a
/// bounded amount of work, so quantum 1 goes through it step by step and
/// fuel grows with its length.
#[test]
fn long_string_functions_cost_fuel_as_they_go() {
    let fuel = |source: &str| finish_strings(source).fuel_consumed();
    let setup = "local s = string.rep('ab', 50000) local n = 0";
    let base = fuel(&format!("{setup} return 0"));
    for (call, least) in [
        ("string.rep('x', 1 << 20)", 200u64),
        ("s:upper()", 20),
        ("s:reverse()", 20),
        ("s:sub(2)", 20),
        ("s:gsub('b', 'c')", 100),
        ("s:find('ac')", 50),
        ("s:find('a(b*)c')", 100),
        (
            "string.format(string.rep('%d', 10000), table.unpack((function() local t = {} for i = 1, 10000 do t[i] = i end return t end)()))",
            30,
        ),
        (
            "string.pack(string.rep('i4', 5000), table.unpack((function() local t = {} for i = 1, 5000 do t[i] = i end return t end)()))",
            10,
        ),
    ] {
        let extra = fuel(&format!("{setup} local r = {call} return 0")) - base;
        assert!(extra >= least, "{call}: {extra}");
    }
    // Quantum 1 moves through a long search a step at a time, to the same
    // result.
    let chunk =
        crate::compile(format!("{setup} return s:find('a(b*)c'), #s:gsub('a', 'xy')").as_bytes())
            .unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    let mut journal = Journal::new();
    let mut steps = 0;
    while let StepOutcome::Paused(_) = runtime.run(1, &mut journal).unwrap() {
        steps += 1;
    }
    assert_eq!(print_line(&runtime), "nil\t150000\n");
    assert!(steps > 500, "{steps}");
}

/// A pattern that backtracks without end, on a long subject, is stopped
/// by fuel, a step at a time, not by the host: the matcher's stack is
/// Lua's 200 levels, never the Rust stack.
#[test]
fn pathological_patterns_are_bounded_by_fuel() {
    let source = "local s = string.rep('a', 20000) \
                  return string.find(s, string.rep('a*', 60) .. 'b')";
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    let mut journal = Journal::new();
    let mut paused = 0;
    let started = crate::hostcaps::native::Instant::now();
    while paused < 20_000 {
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => paused += 1,
            other => panic!("{other:?}"),
        }
    }
    // Each step does a bounded amount of matching.
    assert!(started.elapsed() < crate::hostcaps::native::Duration::from_secs(20));
    // Deep enough to hit Lua's bound: a catchable error, not a crash.
    let deep = finish_strings(
        "return pcall(string.find, string.rep('a', 500), string.rep('a?', 250) .. string.rep('a', 250))",
    );
    assert_eq!(print_line(&deep), "false\tpattern too complex\n");
}

/// A checkpoint at every step of a search, a `gsub`, a `format`, and a
/// `gmatch` loop, each restored and run on: the same results and the same
/// fuel as the straight run, since a restored matcher goes on where it was.
#[test]
fn string_work_restores_at_every_step() {
    for source in [
        "local s = string.rep('ab', 300) .. 'c' return s:find('(a(b)-)+c'), select(2, s:gsub('(b)', '%1%1'))",
        "local s = string.rep('x y ', 200) local n = 0 for w in s:gmatch('%a') do n = n + 1 end return n",
        "return string.format(string.rep('%5.2f|%q|', 50), table.unpack((function() local t = {} for i = 1, 100 do t[i] = i % 2 == 0 and 'q' .. i or i / 3 end return t end)()))",
        "local t = {} for i = 1, 300 do t[i] = i end local p = string.pack(string.rep('j', 300), table.unpack(t)) return #p, select('#', string.unpack(string.rep('j', 300), p))",
    ] {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let straight = finish_with(boot_strings, &chunk.proto);
        let mut runtime = boot_strings(&chunk.proto);
        let mut journal = Journal::new();
        loop {
            runtime = restore(&runtime);
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(pair_results(&runtime), pair_results(&straight), "{source}");
        assert_eq!(
            runtime.fuel_consumed(),
            straight.fuel_consumed(),
            "{source}"
        );
    }
}

/// Waits inside a `gsub` replacement and a `%s` conversion restore, with
/// the matcher and the text built so far.
#[test]
fn waits_inside_string_functions_restore() {
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let text_value = |bytes: &[u8]| HostValue::String(bytes.to_vec());
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        (
            b"return string.gsub('a-b-c', '%a', function(c) if c == 'b' then return park() end return c:upper() end)",
            vec![back(vec![text_value(b"<b>")])],
            "A-<b>-C\t3\n",
        ),
        (
            b"local t = setmetatable({}, { __tostring = function() return park() end }) \
              return string.format('[%5s] %d', t, 7)",
            vec![back(vec![text_value(b"ab")])],
            "[   ab] 7\n",
        ),
        (
            b"local r = setmetatable({}, { __index = function(t, k) return park() end }) \
              return string.gsub('xy', '.', r)",
            vec![back(vec![HostValue::Boolean(false)]), back(vec![text_value(b"Y")])],
            "xY\t2\n",
        ),
    ];
    for (source, answers, line) in cases {
        let chunk = crate::compile(source).unwrap();
        for checkpoint in [false, true] {
            let mut runtime = boot_strings(&chunk.proto);
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
            assert_eq!(print_line(&runtime), line, "{source:?}");
        }
    }
}

/// The string metatable is one table, shared by every string, a root, and
/// snapshot state: a change to it is seen after a restore.
#[test]
fn the_string_metatable_is_shared_and_restored() {
    let chunk = crate::compile(
        b"local mt = getmetatable('') mt.__index = function(s, k) return k .. '!' end \
          park() return ('x').y, getmetatable('a') == mt, rawequal(mt, getmetatable('b'))",
    )
    .unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    runtime.set_global_native("park", "park").unwrap();
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("expected a wait");
    };
    runtime.collect();
    let mut runtime = restore(&runtime);
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "y!\ttrue\ttrue\n");
}

/// `gmatch` returns a function of its own each time, a native closure
/// that keeps its subject, pattern, and position; it survives a
/// checkpoint mid-loop, and once unreachable it is collected.
#[test]
fn gmatch_iterators_are_functions_with_identity() {
    let runtime = finish_strings(
        "local a, b = string.gmatch('xyz', '.'), string.gmatch('xyz', '.') \
         local t = { [a] = 1 } local c = a \
         return type(a), a == b, a == c and rawequal(a, c) and not (a ~= c), t[a], t[b], \
         a(), a(), b(), a(), a()",
    );
    assert_eq!(
        print_line(&runtime),
        "function\tfalse\ttrue\t1\tnil\tx\ty\tx\tz\n"
    );
    let chunk = crate::compile(
        b"local it = string.gmatch('one two three', '%a+') local first = it() \
          park() return first, it(), it(), it()",
    )
    .unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    runtime.set_global_native("park", "park").unwrap();
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("expected a wait");
    };
    let mut runtime = restore(&runtime);
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "one\ttwo\tthree\n");
    let mut runtime =
        // Made inside a call, whose registers are cleared on return.
        finish_strings(
            "local function make() for i = 1, 50 do local it = string.gmatch('ab', '.') it() end end \
             make() return 1",
        );
    runtime.collect();
    assert_eq!(runtime.heap().native_closures.live(), 0);
}

/// Results that would pass the stack or the string limit are catchable
/// errors found before anything is made.
#[test]
fn string_results_past_their_limits_are_catchable() {
    let runtime = finish_strings(
        "local s = string.rep('x', 200000) \
         local function why(ok, message) return message end \
         return why(pcall(string.byte, s, 1, -1)), why(pcall(string.rep, 'x', 1 << 31)), \
         why(pcall(string.rep, 'ab', math.maxinteger)), \
         why(pcall(string.format, '%s%s', s:rep(100), s:rep(100))), \
         why(pcall(string.unpack, string.rep('b', 200000), s))",
    );
    assert_eq!(
        print_line(&runtime),
        "stack overflow (string slice too long)\tresulting string too large\tresulting string too large\tnot enough memory\tstack overflow (too many results)\n"
    );
}

/// Restore refuses string states the runtime cannot make: a type's
/// metatable in a slot only tables and userdata have, a `gmatch` iterator
/// whose position is past its subject, and a function's work that
/// disagrees with its arguments.
#[test]
fn restore_refuses_string_states_the_runtime_cannot_make() {
    use crate::heap::Task;
    use crate::library::Work;
    use crate::snapshot::{BoundaryImage, Image};
    use crate::strlib::{Build, StrWork};
    let check = |runtime: &Runtime, change: &dyn Fn(&mut Image), error: SnapshotError| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            error,
        );
    };
    let each = |image: &mut Image, change: &dyn Fn(&mut StrWork)| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if let Some(BoundaryImage::Builtin {
                    task: Task::Lib(task),
                    ..
                }) = &mut frame.boundary
                    && let Work::Str(work) = &mut task.work
                {
                    change(work);
                }
            }
        }
    };
    let chunk = crate::compile(
        b"local it = string.gmatch('abc', '.') it() \
          return string.rep('xy', 100000), it()",
    )
    .unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    // Into the middle of `rep`.
    let mut journal = Journal::new();
    for _ in 0..60 {
        runtime.run(1, &mut journal).unwrap();
    }
    restore(&runtime);
    let string_meta = |image: &Image| image.type_metatables[4];
    check(
        &runtime,
        &|image| image.type_metatables[5] = string_meta(image),
        SnapshotError::InvalidStructure,
    );
    check(
        &runtime,
        &|image| image.type_metatables[3] = image.strings[0].0,
        SnapshotError::InvalidStructure,
    );
    check(
        &runtime,
        &|image| image.native_closures[0].state[0] = 99,
        SnapshotError::InvalidStructure,
    );
    check(
        &runtime,
        &|image| image.native_closures[0].values.truncate(1),
        SnapshotError::InvalidStructure,
    );
    check(
        &runtime,
        &|image| {
            each(image, &|work| {
                if let StrWork::Build { build, .. } = work {
                    *build = Build::Sub { start: 150_000 };
                }
            })
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &runtime,
        &|image| {
            each(image, &|work| {
                if let StrWork::Build { out, total, .. } = work {
                    out.resize(*total as usize, b'x');
                }
            })
        },
        SnapshotError::InvalidStructure,
    );
}

/// Every damaged `string.dump` result, loaded, is a function or a clean
/// failure: never a panic, never a function built from unchecked code.
#[test]
fn damaged_binary_chunks_load_cleanly_or_fail() {
    let runtime = finish_strings(
        "local f = function(a, b) local t = { a, b } for i = 1, 3 do t[i] = (t[i] or 0) + i end \
         return function() return t[1] + t[2] + t[3] end end \
         local d = string.dump(f) local seed, loaded, refused = 7, 0, 0 \
         for trial = 1, 3000 do \
           seed = (seed * 1103515245 + 12345) % 2147483648 \
           local at = seed % #d + 1 \
           local kind = trial % 3 \
           local e \
           if kind == 0 then e = d:sub(1, at - 1) .. string.char(seed % 256) .. d:sub(at + 1) \
           elseif kind == 1 then e = d:sub(1, at) \
           else e = d:sub(1, at) .. string.rep('\\255', seed % 9) .. d:sub(at + 1) end \
           local g = load(e, 'x', 'b') \
           if g then loaded = loaded + 1 else refused = refused + 1 end \
         end \
         return loaded + refused, load(d, 'x', 'b')(1, 2)()",
    );
    assert_eq!(print_line(&runtime), "3000\t9\n");
}

/// A `gsub` string replacement stops before its result passes the string
/// limit, however many `%0` it repeats (found by the milestone review:
/// the expansion used to grow without bound first). A string metamethod
/// called with one argument reads that argument twice, as Lua's does.
#[test]
fn replacements_stop_at_the_limit_and_metamethod_quirks_match() {
    let runtime = finish_strings(
        "local s = string.rep('a', 1 << 19) \
         local ok, err = pcall(string.gsub, s, '.+', string.rep('%0', (1 << 19) - 1)) \
         local mt = getmetatable('') \
         return ok, err, mt.__unm('5'), mt.__add('5')",
    );
    assert_eq!(print_line(&runtime), "false\tnot enough memory\t-5\t10\n");
}

/// A snapshot changed to hold a `gsub` replacement no call could pass, or
/// a formatter state no run makes, restores into a Lua error, never a host
/// error (found by the milestone review).
#[test]
fn changed_string_work_ends_in_a_lua_error() {
    use crate::heap::Task;
    use crate::snapshot::{BoundaryImage, EncValue};
    let chunk = crate::compile(b"return string.gsub(string.rep('ab', 30000), 'b', 'c')").unwrap();
    let mut runtime = boot_strings(&chunk.proto);
    let mut journal = Journal::new();
    // Until the `gsub` frame is at work (collector work comes first).
    let working = |runtime: &Runtime| {
        let active = runtime.heap().active.unwrap();
        let thread = runtime.heap().threads.get(active).unwrap();
        thread.frames.iter().any(|frame| match frame.boundary() {
            Some(crate::heap::Boundary::Builtin {
                func,
                task: Task::Lib(_),
                ..
            }) => thread.stack.len() > (func + 3) as usize,
            _ => false,
        })
    };
    for _ in 0..100_000 {
        runtime.run(1, &mut journal).unwrap();
        if working(&runtime) {
            break;
        }
    }
    let mut image = runtime.to_image().unwrap();
    let mut changed = false;
    for thread in &mut image.threads {
        let slots: Vec<u32> = thread
            .frames
            .iter()
            .filter_map(|frame| match &frame.boundary {
                Some(BoundaryImage::Builtin {
                    func,
                    task: Task::Lib(_),
                    ..
                }) => Some(*func),
                _ => None,
            })
            .collect();
        for func in slots {
            thread.stack[(func + 3) as usize] = EncValue::Nil;
            changed = true;
        }
    }
    assert!(changed);
    let bytes = snapshot::encode(&image).unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(outcome, StepOutcome::LuaError(LuaFault::Argument));
}

/// Immediate calls and their fallback errors keep the same fuel total when
/// every VM step is separately scheduled and restored.
#[test]
fn immediate_builtin_fuel_survives_every_step_restore() {
    let chunk = crate::compile(&fixture("lib_immediate.lua")).unwrap();
    let mut straight = boot_strings(&chunk.proto);
    assert_eq!(
        straight
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    let expected = straight.fuel_consumed();
    let mut stepped = boot_strings(&chunk.proto);
    let mut journal = Journal::new();
    loop {
        stepped = restore(&stepped);
        match stepped.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(stepped.fuel_consumed(), expected);
    assert_eq!(pair_results(&stepped), pair_results(&straight));
}
