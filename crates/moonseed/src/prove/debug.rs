//! The registry, `package`, `require`, debug information, and the `debug`
//! library (Phase 3.24, ADR 0039, ADR 0040). Two corpora give Lua 5.4.9's
//! output and keep it under every schedule; installers agree on module
//! identity in any order; suspended and failed coroutines are
//! introspected; tracebacks restore mid-search; states the runtime cannot
//! make are refused.

use super::base::{capture, restore, text};
use super::*;

/// Boot with every library, `package` and `debug` included.
fn boot_all(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_spec(spec);
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    runtime
}

/// Exercise the new compiler output with tiny quanta and a restore after every
/// instruction, including suspended arithmetic metamethods and scope cleanup.
fn destination_schedules(source: &str) {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    fast_slow_equivalent(|| boot_all(&chunk.proto), pair_results);
    let reference = finish_with(boot_all, &chunk.proto);
    let expected = pair_results(&reference);
    let fuel = reference.fuel_consumed();
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        for quantum in [1, 2, 3, 7] {
            let mut runtime = boot_all(&chunk.proto);
            runtime.hot_core = mode;
            let mut journal = Journal::new();
            loop {
                let outcome = runtime.run(quantum, &mut journal).unwrap();
                if quantum == 1 {
                    runtime = Runtime::from_snapshot(
                        &runtime.snapshot().unwrap(),
                        &HostRegistry::proof(),
                        runtime.effect_domain(),
                    )
                    .unwrap();
                    runtime.hot_core = mode;
                }
                match outcome {
                    StepOutcome::Paused(_) => {}
                    StepOutcome::Completed => break,
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(pair_results(&runtime), expected, "quantum {quantum}");
            assert_eq!(runtime.fuel_consumed(), fuel, "quantum {quantum}");
        }
    }
}

#[test]
fn yielding_metamethod_transfers_restore_each_step_in_all_modes() {
    let source = include_str!("../../fixtures/lua/metamethod_transfer.lua");
    let chunk = crate::compile(source.as_bytes()).unwrap();
    assert_eq!(print_line(&finish_with(boot_all, &chunk.proto)), "112\t3\n");
    destination_schedules(source);
}

#[test]
fn destination_loop_aliases_and_close_checkpoint_each_step() {
    destination_schedules(
        r#"
        local sum, a, b = 0, 3, 7
        for i = 1, 6 do sum = sum + (i & 3) end
        a = b + a
        a, b = b + 1, a + 1
        assert(a == 8 and b == 11 and sum == 9)
        local closed = 0
        do
            local guard <close> = setmetatable({}, {__close = function() closed = closed + 1 end})
            sum = sum + 1
        end
        assert(closed == 1 and sum == 10)
        return sum, a, b, closed
    "#,
    );
}

#[test]
fn arithk_debug_line_and_locals_survive_yielding_handler() {
    destination_schedules(
        "local co = coroutine.create(function()\n\
         local x = 10\n\
         local t = setmetatable({}, {__sub = function(a, b)\n\
             assert(a == 3 and type(b) == 'table')\n\
             local info = debug.getinfo(1, 'n')\n\
             assert(info.namewhat == 'metamethod' and info.name == 'sub')\n\
             assert(debug.getinfo(2, 'l').currentline == 11)\n\
             local name, value = debug.getlocal(2, 1)\n\
             assert(name == 'x' and value == 10) coroutine.yield(value) return 42\n\
         end})\n\
         x = 3 - t\n\
         return x\n\
         end)\n\
         local ok, old = coroutine.resume(co) assert(ok and old == 10)\n\
         local ok2, result = coroutine.resume(co) assert(ok2 and result == 42)\n\
         return old, result",
    );
}

#[test]
fn cmpbr_debug_condition_line_and_locals_survive_yielding_handler() {
    destination_schedules(
        "local co = coroutine.create(function()\n\
         local x = 10\n\
         local t = setmetatable({}, {__lt = function(a, b)\n\
             assert(a == 3 and type(b) == 'table')\n\
             local info = debug.getinfo(1, 'n')\n\
             assert(info.namewhat == 'metamethod' and info.name == 'lt')\n\
             assert(debug.getinfo(2, 'l').currentline == 11)\n\
             local name, value = debug.getlocal(2, 1)\n\
             assert(name == 'x' and value == 10) coroutine.yield(value) return 0\n\
         end})\n\
         if not (3 < t) then error('wrong branch') end\n\
         return x\n\
         end)\n\
         local ok, old = coroutine.resume(co) assert(ok and old == 10)\n\
         local ok2, result = coroutine.resume(co) assert(ok2 and result == 10)\n\
         return old, result",
    );
}

#[test]
fn cmpbr_compound_conditions_keep_both_scope_exit_edges() {
    destination_schedules(
        r#"
        local closed, round = 0, 0
        local saved = {}
        repeat
            round = round + 1
            local x = round
            saved[round] = function() return x end
            local guard <close> = setmetatable({}, {__close = function()
                closed = closed + 1
            end})
        until round >= 2 and closed == 1
        assert(closed == 2 and saved[1]() == 1 and saved[2]() == 2)
        local i = 0
        while i < 2 and not (i < 0) do
            local guard <close> = setmetatable({}, {__close = function()
                closed = closed + 1
            end})
            i = i + 1
        end
        assert(i == 2 and closed == 4)
        return closed, saved[1](), saved[2]()
    "#,
    );
}

#[test]
fn destination_captured_local_is_not_written_during_rhs() {
    destination_schedules(
        r#"
        local x = 10
        local mt = {__add = function() assert(x == 10) x = 20 return 4 end}
        x = (setmetatable({}, mt) + 1) + x
        assert(x == 24)
        return x
    "#,
    );
}

#[test]
fn destination_debug_observer_yields_before_arithmetic_commit() {
    destination_schedules(
        r#"
        local mt = {__add = function()
            local name, value = debug.getlocal(2, 1)
            assert(name == 'x' and value == 10)
            coroutine.yield(value)
            name, value = debug.getlocal(2, 1)
            assert(name == 'x' and value == 10)
            return 42
        end}
        local co = coroutine.create(function()
            local x = 10
            x = x + setmetatable({}, mt)
            assert(x == 42)
            return x
        end)
        local ok, old = coroutine.resume(co)
        assert(ok and old == 10)
        local ok2, result = coroutine.resume(co)
        assert(ok2 and result == 42)
        return old, result
    "#,
    );
}

#[test]
fn destination_fault_handler_sees_uncommitted_local() {
    destination_schedules(
        r#"
        local observed = false
        local mt = {__add = function() error('failed RHS') end}
        local ok = xpcall(function()
            local x = 10
            x = (x + 1) + setmetatable({}, mt)
        end, function(err)
            for level = 1, 6 do
                if debug.getinfo(level) then
                    for index = 1, 10 do
                        local name, value = debug.getlocal(level, index)
                        if name == 'x' then assert(value == 10) observed = true end
                    end
                end
            end
            return err
        end)
        assert(not ok and observed)
        return observed
    "#,
    );
}

/// A corpus compiled as the file Lua runs, `@name`.
fn corpus(name: &str) -> crate::program::ProtoSpec {
    let mut chunk = crate::compile(&fixture(name)).unwrap();
    chunk.set_chunk_name(format!("@{name}").as_bytes());
    chunk.proto
}

/// What a corpus writes, run straight.
fn straight_output(spec: &crate::program::ProtoSpec) -> (String, Runtime) {
    let mut runtime = boot_all(spec);
    let written = capture(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    (text(&written), runtime)
}

const CORPORA: [&str; 2] = ["corpus_debug", "corpus_package"];

/// Each corpus writes what Lua 5.4.9 wrote for it (`<name>.out`, made by
/// the oracle test's binary).
#[test]
fn debug_and_package_corpora_match_lua() {
    for name in CORPORA {
        let spec = corpus(&format!("{name}.lua"));
        let (output, _) = straight_output(&spec);
        let expected = String::from_utf8(fixture(&format!("{name}.out"))).unwrap();
        for (index, (a, b)) in output.lines().zip(expected.lines()).enumerate() {
            assert_eq!(a, b, "{name} line {}", index + 1);
        }
        assert_eq!(output.lines().count(), expected.lines().count(), "{name}");
    }
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_debug_and_package_corpora() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    for name in CORPORA {
        let output = lua_command(&lua)
            .current_dir(&root)
            .arg(format!("{name}.lua"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{name} failed");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(fixture(&format!("{name}.out"))).unwrap(),
            "{name}"
        );
    }
}

/// The corpora write the same under small quanta, and the package corpus
/// the same with a collection, a checkpoint, and a restore at every step:
/// a loader or searcher is never called twice.
#[test]
fn corpora_keep_their_output_under_every_schedule() {
    for name in CORPORA {
        let spec = corpus(&format!("{name}.lua"));
        let (expected, straight) = straight_output(&spec);
        for quantum in [1u64, 3, 7] {
            let mut runtime = boot_all(&spec);
            let written = capture(&mut runtime);
            let mut journal = Journal::new();
            while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
            assert_eq!(text(&written), expected, "{name} quantum {quantum}");
            assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed(), "{name}");
        }
    }
    let spec = corpus("corpus_package.lua");
    let (expected, _) = straight_output(&spec);
    let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
    };
    let mut runtime = boot_all(&spec);
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
    assert_eq!(String::from_utf8_lossy(&written.borrow()), expected);
}

/// Every installer puts its table in `_LOADED`, whatever order they run
/// in and whichever run at all: `require` finds the global itself.
#[test]
fn installers_agree_on_module_identity_in_any_order() {
    let check = "local out = {} \
        for _, name in ipairs({ '_G', 'string', 'math', 'table', 'package', 'debug' }) do \
          local global = rawget(_G, name) \
          local loaded = package.loaded[name] \
          out[#out + 1] = name .. '=' .. tostring(global ~= nil and global == loaded) \
        end \
        return table.concat(out, ' '), package.loaded == debug.getregistry()._LOADED";
    type Installer = fn(&mut Runtime) -> Result<(), crate::VmError>;
    let base: Installer = Runtime::install_base;
    let package: Installer = Runtime::install_package;
    let string: Installer = Runtime::install_string;
    let math: Installer = Runtime::install_math;
    let table: Installer = Runtime::install_table;
    let debug: Installer = Runtime::install_debug;
    let all = "_G=true string=true math=true table=true package=true debug=true\ttrue\n";
    for order in [
        vec![base, string, math, table, package, debug],
        vec![package, debug, base, string, math, table],
        vec![debug, table, math, string, package, base],
    ] {
        let chunk = crate::compile(check.as_bytes()).unwrap();
        let mut runtime = boot_spec(&chunk.proto);
        for install in order {
            install(&mut runtime).unwrap();
        }
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(print_line(&runtime), all);
    }
    // A sandbox with part of the libraries: what is not installed is not
    // loaded either.
    let chunk = crate::compile(
        b"return require('_G') == _G, require('table') == table, \
          pcall(require, 'string'), rawget(_G, 'debug')",
    )
    .unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_base().unwrap();
    runtime.install_table().unwrap();
    runtime.install_package().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "true\ttrue\tfalse\tnil\n");
    // The standard set leaves `debug` out.
    let chunk = crate::compile(b"return rawget(_G, 'debug'), package.loaded.debug").unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_standard().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "nil\tnil\n");
}

/// A suspended coroutine's levels are its own frames: its locals, lines,
/// and traceback. A failed one keeps them, with the builtin that raised
/// the error at level 0, as Lua's `error` is.
#[test]
fn suspended_and_failed_coroutines_are_introspected() {
    let co_source =
        "local function inner(a)\n  local b = a * 2\n  yield(b)\n  error('boom')\nend\ninner(21)";
    let look = "local l1, v1 = debug.getlocal(co, 1, 1) \
        local l2, v2 = debug.getlocal(co, 1, 2) \
        local i0 = debug.getinfo(co, 0, 'Slnf') \
        local i1 = debug.getinfo(co, 1, 'lnS') \
        return l1, v1, l2, v2, i0.what, i0.func == yield, i1.currentline, i1.name, i1.namewhat, \
          debug.getinfo(co, 5) == nil, debug.traceback(co, 'hi')";
    let src = "[string \"local function inner(a)...\"]";
    let spec = crate::program::coroutine_then_program(co_source, 1, look);
    let runtime = finish_with(boot_all, &spec);
    assert_eq!(
        print_line(&runtime),
        format!(
            "a\t21\tb\t42\tLua\ttrue\t3\tinner\tlocal\ttrue\thi\nstack traceback:\n\
             \t?: in function 'yield'\n\t{src}:3: in local 'inner'\n\t{src}:6: in main chunk\n\
             \t?: in function <?:-1>\n"
        )
    );
    // Resumed again, it fails in `error`: level 0 is that builtin.
    let look = "local i0 = debug.getinfo(co, 0, 'Sf') \
        local l1, v1 = debug.getlocal(co, 1, 2) \
        debug.setlocal(co, 1, 2, 'set') \
        return i0.what, i0.func == error, l1, v1, select(2, debug.getlocal(co, 1, 2)), \
          debug.traceback(co)";
    let spec = crate::program::coroutine_then_program(co_source, 2, look);
    let runtime = finish_with(boot_all, &spec);
    assert_eq!(
        print_line(&runtime),
        format!(
            "C\ttrue\tb\t42\tset\tstack traceback:\n\t[C]: in function 'error'\n\
             \t{src}:4: in local 'inner'\n\t{src}:6: in main chunk\n\t?: in function <?:-1>\n"
        )
    );
}

/// Past 22 levels a traceback shows the first 10 and the last 11 and
/// skips the rest, with Lua's count.
#[test]
fn deep_tracebacks_skip_the_middle_as_lua_does() {
    let chunk = crate::compile(
        b"local function deep(n) if n == 0 then local r = debug.traceback('deep') return r end \
          local r = deep(n - 1) return r end local r = deep(40) return r",
    )
    .unwrap();
    let runtime = finish_with(boot_all, &chunk.proto);
    let output = print_line(&runtime);
    let lines: Vec<&str> = output.lines().collect();
    // "deep", the header, 10 levels, the skip, and 11 levels. The levels
    // are 0 (traceback) to 42 (the main chunk): Lua skips from level 11,
    // saying `last - level - 11` levels.
    assert_eq!(lines.len(), 25, "{output}");
    assert_eq!(lines[12], "\t...\t(skipping 20 levels)");
    assert_eq!(lines[23], "\t?:1: in main chunk");
    assert_eq!(lines[24], "\t[C]: in ?");
}

/// A traceback that searches a large `package.loaded` for names does it
/// in steps, each checkpointed and restored: the same text and fuel.
#[test]
fn tracebacks_restore_in_the_middle_of_the_name_search() {
    let chunk = crate::compile(
        b"for i = 1, 2000 do _G['g' .. i] = i end \
          local function f() local r = debug.traceback('x') return r end \
          local r = f() return r",
    )
    .unwrap();
    let straight = finish_with(boot_all, &chunk.proto);
    let mut runtime = boot_all(&chunk.proto);
    let mut journal = Journal::new();
    let mut steps_in_traceback = 0;
    loop {
        runtime = restore(&runtime);
        let in_traceback = runtime.to_image().unwrap().threads.iter().any(|thread| {
            thread.frames.iter().any(|frame| {
                matches!(
                    &frame.boundary,
                    Some(crate::snapshot::BoundaryImage::Builtin {
                        task: crate::heap::Task::Lib(task),
                        ..
                    }) if matches!(task.work, crate::library::Work::Debug(_))
                )
            })
        });
        steps_in_traceback += usize::from(in_traceback);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(steps_in_traceback >= 7, "{steps_in_traceback}");
    assert_eq!(pair_results(&runtime), pair_results(&straight));
    assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed());
    assert_eq!(
        print_line(&straight),
        "x\nstack traceback:\n\t?:1: in local 'f'\n\t?:1: in main chunk\n\t[C]: in ?\n"
    );
}

/// `debug.setlocal` may put anything in a loop's state: a Lua error, never
/// a host one. Lua's own behaviour there is undefined.
#[test]
fn a_changed_loop_state_is_a_lua_error() {
    let chunk = crate::compile(
        b"return pcall(function() for i = 1, 3 do debug.setlocal(1, 1, 'x') end end)",
    )
    .unwrap();
    let runtime = finish_with(boot_all, &chunk.proto);
    assert_eq!(
        print_line(&runtime),
        "false\t?:1: 'for' loop state is not a number\n"
    );
}

/// Restore refuses debug states the runtime cannot make: lines that do
/// not match their code, a chunk name that is not a string, a tail-call
/// mark on a boundary frame, and a traceback whose counters or arguments
/// are not ones a run makes.
#[test]
fn restore_refuses_debug_states_the_runtime_cannot_make() {
    use crate::debuglib::DebugWork;
    use crate::heap::Task;
    use crate::library::Work;
    use crate::snapshot::{BoundaryImage, Image};
    let mut chunk = crate::compile(
        b"for i = 1, 2000 do _G['g' .. i] = i end \
          local function f() local r = debug.traceback('x') return r end \
          return f()",
    )
    .unwrap();
    chunk.set_chunk_name(b"=refuse");
    let mut runtime = boot_all(&chunk.proto);
    let mut journal = Journal::new();
    // Into the name search.
    let in_traceback = |runtime: &Runtime| {
        runtime.to_image().unwrap().threads.iter().any(|thread| {
            thread.frames.iter().any(|frame| {
                matches!(
                    &frame.boundary,
                    Some(BoundaryImage::Builtin { task: Task::Lib(task), .. })
                        if matches!(task.work, Work::Debug(_))
                )
            })
        })
    };
    while !in_traceback(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    restore(&runtime);
    let check = |change: &dyn Fn(&mut Image), error: SnapshotError| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            error,
        );
    };
    let each = |image: &mut Image, change: &dyn Fn(&mut crate::debuglib::Traceback)| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if let Some(BoundaryImage::Builtin {
                    task: Task::Lib(task),
                    ..
                }) = &mut frame.boundary
                    && let Work::Debug(work) = &mut task.work
                {
                    let DebugWork::Traceback(traceback) = &mut **work;
                    change(traceback);
                }
            }
        }
    };
    check(
        &|image| {
            let debug = image.protos[0].debug.as_mut().unwrap();
            debug.lines.pop();
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| image.protos[0].source = image.threads[0].id,
        SnapshotError::DanglingReference,
    );
    check(
        &|image| {
            let frame = image.threads[0]
                .frames
                .iter_mut()
                .find(|frame| frame.boundary.is_some())
                .unwrap();
            frame.tail = true;
        },
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| each(image, &|work| work.threaded = true),
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| each(image, &|work| work.last = 5000),
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| each(image, &|work| work.level = work.last + 2),
        SnapshotError::InvalidStructure,
    );
    check(
        &|image| each(image, &|work| work.shown = 11),
        SnapshotError::InvalidStructure,
    );
}

/// Lines break where Lua's lexer breaks them: `\r\n` and `\n\r` are one
/// break each, and a lone `\r` is one too.
#[test]
fn lines_count_every_break_lua_counts() {
    let chunk = crate::compile(
        b"local a = 1\r\nlocal b = 2\n\rlocal c = 3\rlocal d = 4\n\
          return debug.getinfo(1, 'l').currentline",
    )
    .unwrap();
    let runtime = finish_with(boot_all, &chunk.proto);
    assert_eq!(print_line(&runtime), "5\n");
}

/// Names are kept whole: a checkpoint at every step and a dump and load
/// give a 300-byte local's and upvalue's full names, as Lua does.
#[test]
fn long_names_survive_checkpoints_and_dumps() {
    let name = "n".repeat(300);
    let source = format!(
        "local {name} = 1 local function f() return {name} end \
         local a = #debug.getlocal(1, 1) local b = #debug.getupvalue(f, 1) \
         local g = load(string.dump(f)) return a, b, #debug.getupvalue(g, 1)"
    );
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let straight = finish_with(boot_all, &chunk.proto);
    assert_eq!(print_line(&straight), "300\t300\t300\n");
    let mut runtime = boot_all(&chunk.proto);
    let mut journal = Journal::new();
    loop {
        runtime = restore(&runtime);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(pair_results(&runtime), pair_results(&straight));
}

/// Public diagnostics reached only after accepted reference-suite blockers.
#[test]
fn suite_later_section_diagnostics_survive_checkpoints() {
    let source = br#"
        local _, msg = load('syntax error')
        assert(msg == "[string \"syntax error\"]:1: syntax error near 'error'", msg)
        for _, source in ipairs({'foo', '(foo)', 'a.b'}) do
          local _, msg = load(source, '@prefix.lua')
          assert(msg == 'prefix.lua:1: syntax error near <eof>', msg)
        end
        local _, msg = pcall(assert(load("\n\n for k,v in \n 3 \n do \n end", "@iterator.lua")))
        assert(msg:match("^iterator.lua:4:"), msg)
        local a = {x = nil}
        local _, msg = pcall(assert(load("function a.x.y ()\na=a+1\nend", "@function.lua", nil, {a=a})))
        assert(msg:match("^function.lua:1:"), msg)
        local function badlocal()
          local victim <close> = {}
        end
        local ok, msg = pcall(badlocal)
        assert(not ok and msg:find("variable 'victim' got a non%-closable value"), msg)
        for _, replacement in ipairs({false, 4}) do
          local function badclose()
            local victim <close> = setmetatable({}, {__close = print})
            getmetatable(victim).__close = replacement or nil
          end
          local ok, msg = pcall(badclose)
          local kind = replacement and 'number' or 'nil'
          assert(not ok and msg:find("attempt to call a " .. kind .. " value %(metamethod 'close'%)"), msg)
        end
        local observed
        do
          local garbage = setmetatable({}, {__gc = function()
            local info = debug.getinfo(1, 'n')
            observed = {info.namewhat, info.name}
          end})
        end
        collectgarbage()
        assert(observed and observed[1] == 'metamethod' and observed[2] == '__gc')
        return true
    "#;
    let chunk = crate::compile(source).unwrap();
    let straight = finish_with(boot_all, &chunk.proto);
    assert_eq!(print_line(&straight), "true\n");
    quantum_and_checkpoints_with(boot_all, &chunk.proto, pair_results);
}
