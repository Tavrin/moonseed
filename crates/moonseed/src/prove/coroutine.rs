//! The coroutine library (Phase 3.25, ADR 0041). A corpus gives Lua
//! 5.4.9's output and keeps it under every schedule; host waits inside
//! coroutines restore; fuel counts every thread; values past the stack
//! bound fail as Lua's resume fails; chains of resumes do not grow the
//! Rust stack; states the runtime cannot make are refused.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::{capture, restore, text};
use super::*;
use crate::host::{HostValue, LegacyCompletion};

/// Boot with every standard library and `debug`.
fn boot_all(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_spec(spec);
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    runtime
}

fn with_park(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_all(spec);
    runtime.set_global_native("park", "park").unwrap();
    runtime
}

fn corpus() -> crate::program::ProtoSpec {
    let mut chunk = crate::compile(&fixture("corpus_coroutine.lua")).unwrap();
    chunk.set_chunk_name(b"@corpus_coroutine.lua");
    chunk.proto
}

fn straight_output(spec: &crate::program::ProtoSpec) -> (String, Runtime) {
    let mut runtime = boot_all(spec);
    let written = capture(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    (text(&written), runtime)
}

fn finish_source(source: &str) -> Runtime {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    finish_with(boot_all, &chunk.proto)
}

/// The corpus writes what Lua 5.4.9 wrote for it (`corpus_coroutine.out`).
#[test]
fn coroutine_corpus_matches_lua() {
    fast_slow_equivalent(|| boot_all(&corpus()), pair_results);
    let (output, _) = straight_output(&corpus());
    let expected = String::from_utf8(fixture("corpus_coroutine.out")).unwrap();
    for (index, (a, b)) in output.lines().zip(expected.lines()).enumerate() {
        assert_eq!(a, b, "line {}", index + 1);
    }
    assert_eq!(output.lines().count(), expected.lines().count());
}

#[test]
fn cmpbr_yield_fixture_at_every_safe_point() {
    let chunk = crate::compile(&fixture("cmpbr_yield.lua")).unwrap();
    assert_eq!(
        print_line(&finish_with(boot_all, &chunk.proto)),
        "7\t7\t8\n"
    );
    quantum_and_checkpoints_with(boot_all, &chunk.proto, pair_results);
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_coroutine_corpus() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("corpus_coroutine.lua")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(fixture("corpus_coroutine.out")).unwrap()
    );
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("-e")
        .arg("print(assert(loadfile('cmpbr_yield.lua'))())")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "7\t7\t8\n");
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("-e")
        .arg("print(assert(loadfile('metamethod_transfer.lua'))())")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "112\t3\n");
}

/// The corpus writes the same, with the same fuel, under small quanta,
/// and with a collection, a checkpoint, and a restore at every step: no
/// resume, yield, close, or wrap runs twice or is lost.
#[test]
fn coroutine_corpus_keeps_its_output_under_every_schedule() {
    crate::runtime::HotCoreMode::Full.with(coroutine_corpus_schedules);
}

#[test]
fn coroutine_corpus_keeps_its_output_without_fast_calls() {
    crate::runtime::HotCoreMode::NoFastCalls.with(coroutine_corpus_schedules);
}

#[test]
fn coroutine_corpus_keeps_its_output_without_hot_core() {
    crate::runtime::HotCoreMode::Off.with(coroutine_corpus_schedules);
}

fn coroutine_corpus_schedules() {
    let spec = corpus();
    let (expected, straight) = straight_output(&spec);
    for quantum in [1u64, 3, 7] {
        let mut runtime = boot_all(&spec);
        let written = capture(&mut runtime);
        let mut journal = Journal::new();
        while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
        assert_eq!(text(&written), expected, "quantum {quantum}");
        assert_eq!(runtime.fuel_consumed(), straight.fuel_consumed());
    }
    let written = Rc::new(RefCell::new(Vec::new()));
    let attach = |runtime: &mut Runtime| {
        let sink = written.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
    };
    let mut runtime = boot_all(&spec);
    attach(&mut runtime);
    let mut journal = Journal::new();
    // A host collection changes what the collector does next, so fuel is
    // compared with a twin that collects alike and is never restored.
    let mut twin = boot_all(&spec);
    let mut twin_journal = Journal::new();
    loop {
        assert!(runtime.native_results.is_empty(), "scratch became a root");
        runtime.collect();
        twin.collect();
        runtime = restore(&runtime);
        attach(&mut runtime);
        let outcome = runtime.run(1, &mut journal).unwrap();
        assert!(runtime.native_results.is_empty(), "scratch was published");
        assert_eq!(twin.run(1, &mut twin_journal).unwrap(), outcome);
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(String::from_utf8_lossy(&written.borrow()), expected);
    assert_eq!(runtime.fuel_consumed(), twin.fuel_consumed());
}

/// A host wait inside a resumed coroutine: `coroutine.resume` has not
/// returned while it waits. Checkpointed before and after the answer, the
/// coroutine goes on to its next yield, return, or error exactly once.
#[test]
fn host_waits_inside_coroutines_restore() {
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let int = HostValue::Integer;
    let cases: Vec<(&str, Vec<LegacyCompletion>, &str)> = vec![
        (
            "local n = 0 local co = coroutine.create(function(a) n = n + 1 \
               local b = park() coroutine.yield(a + b) return park() end) \
             local ok1, r1 = coroutine.resume(co, 1) local ok2, r2 = coroutine.resume(co) \
             return n, ok1, r1, ok2, r2, coroutine.status(co)",
            vec![back(vec![int(10)]), back(vec![int(20)])],
            "1\ttrue\t11\ttrue\t20\tdead\n",
        ),
        (
            "local w = coroutine.wrap(function() local x = park() error(x, 0) end) \
             return pcall(w)",
            vec![back(vec![int(7)])],
            "false\t7\n",
        ),
        (
            "local inner = coroutine.create(function() return park() end) \
             local outer = coroutine.wrap(function() \
               local ok, v = coroutine.resume(inner) return v, coroutine.status(inner) end) \
             return outer()",
            vec![back(vec![int(5)])],
            "5\tdead\n",
        ),
        (
            "local closed = 0 local co = coroutine.create(function() \
               local x <close> = setmetatable({}, { __close = function() closed = closed + park() end }) \
               coroutine.yield() end) \
             coroutine.resume(co) local ok = coroutine.close(co) return ok, closed",
            vec![back(vec![int(3)])],
            "true\t3\n",
        ),
    ];
    for (source, answers, line) in cases {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        for checkpoint in [false, true] {
            let mut runtime = with_park(&chunk.proto);
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
            assert!(answers.next().is_none(), "{source}");
            assert_eq!(print_line(&runtime), line, "{source}");
        }
    }
}

/// Switching threads is not a way around fuel: a loop of resumes runs out
/// of it like any loop.
#[test]
fn fuel_counts_every_thread() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let chunk = crate::compile(
                b"local co = coroutine.wrap(function() while true do coroutine.yield() end end) \
          while true do co() end",
            )
            .unwrap();
            let config = Config {
                fuel_limit: Some(50_000),
                ..Config::default()
            };
            let mut runtime =
                Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
            runtime.install_standard().unwrap();
            assert_eq!(
                runtime
                    .run_until_terminal(u64::MAX, &mut Journal::new())
                    .unwrap(),
                StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
            );
            assert_eq!(runtime.fuel_consumed(), 50_000);
        });
    }
}

/// Arguments that would pass the coroutine's stack bound, and results
/// that would pass the resumer's, fail as Lua's `resume` fails: `false`
/// and "too many arguments to resume" or "too many results to resume",
/// with nothing moved and the coroutine as it was. One value fewer fits.
#[test]
fn transfers_past_the_stack_bound_fail_cleanly() {
    let source = "local t = {} for i = 1, 3000 do t[i] = i end \
        local function deep(k) if k == 0 then return coroutine.yield() end local r = deep(k - 1) return r end \
        local co = coroutine.create(function() while true do deep(40) end end) \
        coroutine.resume(co) \
        local fits = 0 \
        for n = 1, 3000 do \
          local ok, e = coroutine.resume(co, table.unpack(t, 1, n)) \
          if not ok then \
            local again = coroutine.resume(co, 1) \
            return fits, n, e, coroutine.status(co), again \
          end \
          fits = n \
        end";
    let config = Config {
        max_stack_slots: 1_000,
        ..Config::default()
    };
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime =
        Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let line = print_line(&runtime);
    assert_eq!(outcome, StepOutcome::Completed, "{line}");
    let fields: Vec<&str> = line.trim_end().split('\t').collect();
    let fits: u32 = fields[0].parse().unwrap();
    assert_eq!(fields[1].parse::<u32>().unwrap(), fits + 1, "{line}");
    assert_eq!(
        &fields[2..],
        ["too many arguments to resume", "suspended", "true"],
        "{line}"
    );
    // The resumer is the deep side now.
    let source = "local t = {} for i = 1, 3000 do t[i] = i end \
        local co = coroutine.create(function() local n = 0 while true do n = n + 1 coroutine.yield(table.unpack(t, 1, n)) end end) \
        local function deep(k) if k == 0 then return coroutine.resume(co) end local a, b = deep(k - 1) return a, b end \
        local fits = 0 \
        for n = 1, 3000 do \
          local ok, e = deep(40) \
          if not ok then return fits, n, e, coroutine.status(co), (coroutine.resume(co)) end \
          fits = n \
        end";
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let line = print_line(&runtime);
    let fields: Vec<&str> = line.trim_end().split('\t').collect();
    let fits: u32 = fields[0].parse().unwrap();
    assert!(fits > 100, "{line}");
    assert_eq!(fields[1].parse::<u32>().unwrap(), fits + 1, "{line}");
    assert_eq!(
        &fields[2..],
        ["too many results to resume", "suspended", "true"],
        "{line}"
    );
}

/// 196 coroutines resumed one inside another, the most Lua 5.4.9 allows, and a thousand coroutines each resuming and closing
/// others, run on a 256 KiB Rust stack: nothing nests on it.
#[test]
fn resume_chains_do_not_grow_the_rust_stack() {
    let run = || {
        let runtime = finish_source(
            "local function nest(k) if k == 0 then return 'bottom' end \
               local c = coroutine.wrap(nest) local r = c(k - 1) return r end \
             local total = 0 \
             for i = 1, 1000 do \
               local a = coroutine.wrap(function() \
                 local b = coroutine.create(function() coroutine.yield(i) end) \
                 local ok, v = coroutine.resume(b) coroutine.close(b) \
                 return pcall(table.sort, { 2, 1 }, function(x, y) return x < y end) and v end) \
               total = total + a() \
             end \
             return nest(196), total, pcall(nest, 197)",
        );
        print_line(&runtime)
    };
    let line = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
    assert!(line.starts_with("bottom\t500500\tfalse\t"), "{line}");
    assert!(line.ends_with("C stack overflow\n"), "{line}");
}

/// A coroutine is an ordinary object: dropping its wrapper frees it, and
/// its pending `<close>` values are not closed by the collector.
#[test]
fn dropped_coroutines_are_collected_without_closing() {
    let chunk = crate::compile(
        b"closed = false \
          do \
            local w = coroutine.wrap(function() \
              local x <close> = setmetatable({}, { __close = function() closed = true end }) \
              coroutine.yield() end) \
            w() \
            w = nil \
          end \
          collectgarbage() return closed",
    )
    .unwrap();
    let mut runtime = boot_all(&chunk.proto);
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(print_line(&runtime), "false\n");
    runtime.collect();
    // The entry thread is the only thread left.
    assert_eq!(runtime.heap().threads.live(), 1);
}

/// Restore refuses thread graphs no run makes: a cycle of resumers, a
/// running thread outside the chain of resumes, a resumer that does not
/// wait in a call that resumes or closes, a closing coroutine under
/// `coroutine.resume`, and a coroutine never started without a function.
#[test]
fn restore_refuses_coroutine_states_the_runtime_cannot_make() {
    use crate::snapshot::{EncValue, Image};
    let chunk = crate::compile(
        b"local c = coroutine.create(function() park() end) \
          local b = coroutine.wrap(function() coroutine.resume(c) end) \
          local fresh = coroutine.create(print) \
          local a = coroutine.create(function() b() end) \
          coroutine.resume(a) return fresh",
    )
    .unwrap();
    let mut runtime = with_park(&chunk.proto);
    let mut journal = Journal::new();
    // Waiting in `park` inside c, resumed by b's coroutine, resumed by a,
    // resumed by main.
    assert!(matches!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Waiting(_)
    ));
    restore(&runtime);
    let check = |change: &dyn Fn(&mut Image)| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    };
    let chain = |image: &Image| {
        let mut ids = vec![image.active];
        loop {
            let last = *ids.last().unwrap();
            let parent = image
                .threads
                .iter()
                .find(|t| t.id == last)
                .unwrap()
                .resumed_by;
            if parent == 0 {
                break ids;
            }
            ids.push(parent);
        }
    };
    let index = |image: &Image, id: u64| image.threads.iter().position(|t| t.id == id).unwrap();
    // A cycle: main resumed by the waiting coroutine.
    check(&|image| {
        let ids = chain(image);
        let main = index(image, *ids.last().unwrap());
        image.threads[main].resumed_by = ids[0];
    });
    // Two chains: the middle resumer says nobody resumed it.
    check(&|image| {
        let ids = chain(image);
        let middle = index(image, ids[1]);
        image.threads[middle].resumed_by = 0;
    });
    // A suspended coroutine claims a resumer.
    check(&|image| {
        let ids = chain(image);
        let fresh = image
            .threads
            .iter()
            .position(|t| {
                t.frames.is_empty() && t.status == crate::heap::Status::LuaSuspended.tag()
            })
            .unwrap();
        image.threads[fresh].resumed_by = ids[1];
    });
    // The coroutine under `coroutine.resume` claims to be closing.
    check(&|image| {
        let ids = chain(image);
        let waiting = index(image, ids[0]);
        image.threads[waiting].closing = true;
    });
    // A coroutine never started with no function to start in.
    check(&|image| {
        let fresh = image
            .threads
            .iter()
            .position(|t| {
                t.frames.is_empty() && t.status == crate::heap::Status::LuaSuspended.tag()
            })
            .unwrap();
        image.threads[fresh].stack[0] = EncValue::Integer(1);
    });
    // A resumer's call site no longer holds a resuming function.
    check(&|image| {
        let ids = chain(image);
        let parent = index(image, ids[1]);
        let frame = image.threads[parent].frames.len() - 1;
        let base = image.threads[parent].frames[frame].base as usize;
        for slot in base..image.threads[parent].stack.len() {
            if matches!(image.threads[parent].stack[slot], EncValue::Native(_)) {
                image.threads[parent].stack[slot] = EncValue::Nil;
            }
        }
    });
}

/// The review's crafted states are refused too: a `wrap` function's
/// close that carries no error to raise, a finished coroutine still
/// linked to its resumer, and a suspended coroutine whose call is not
/// `coroutine.yield`.
#[test]
fn restore_refuses_crafted_close_and_yield_states() {
    use crate::snapshot::{EncValue, EventImage, Image, MetaImage, NextImage};
    let refused = |runtime: &Runtime, change: &dyn Fn(&mut Image)| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    };
    let waiting = |source: &[u8]| {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = with_park(&chunk.proto);
        assert!(matches!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Waiting(_)
        ));
        restore(&runtime);
        runtime
    };
    // A failed coroutine's close under its `wrap` function, waiting in a
    // `__close`.
    let runtime = waiting(
        b"local w = coroutine.wrap(function() \
            local x <close> = setmetatable({}, { __close = function() park() end }) \
            error('e', 0) end) \
          return pcall(w)",
    );
    refused(&runtime, &|image| {
        let active = image.active;
        let thread = image.threads.iter_mut().find(|t| t.id == active).unwrap();
        if let Some(unwind) = thread.unwind.as_mut() {
            unwind.error = None;
        }
        for frame in &mut thread.frames {
            if let Some(MetaImage {
                event:
                    EventImage::Close {
                        next: NextImage::Unwind(unwind),
                        ..
                    },
                ..
            }) = frame.meta.as_mut()
            {
                unwind.error = None;
            }
        }
    });
    refused(&runtime, &|image| {
        let active = image.active;
        let thread = image.threads.iter_mut().find(|t| t.id == active).unwrap();
        thread.status = crate::heap::Status::Completed.tag();
        thread.frames.clear();
        thread.unwind = None;
        thread.closing = false;
    });
    // A coroutine suspended in `pcall(coroutine.yield)` while main waits.
    let runtime = waiting(
        b"local co = coroutine.create(function() pcall(coroutine.yield, 1) end) \
          coroutine.resume(co) park() return coroutine.resume(co)",
    );
    refused(&runtime, &|image| {
        let print = image
            .natives
            .iter()
            .position(|name| name == "base.print")
            .unwrap() as u32;
        let yield_ = image
            .natives
            .iter()
            .position(|name| name == "coroutine.yield")
            .unwrap() as u32;
        for thread in &mut image.threads {
            if thread.status == crate::heap::Status::LuaSuspended.tag() {
                for value in &mut thread.stack {
                    if matches!(value, EncValue::Native(index) if *index == yield_) {
                        *value = EncValue::Native(print);
                    }
                }
            }
        }
    });
}

/// Coroutine stacks count against the heap quota: values parked in many
/// suspended coroutines end in a catchable "not enough memory", and the
/// logical heap stays under the quota.
#[test]
fn coroutine_stacks_count_against_the_quota() {
    let config = Config {
        max_logical_heap: 4 * 1024 * 1024,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"local t = {} for i = 1, 5000 do t[i] = i end \
          local keep, made = {}, 0 \
          local ok, err = pcall(function() \
            for i = 1, 10000 do \
              local co = coroutine.create(function(...) coroutine.yield() end) \
              local fine, why = coroutine.resume(co, table.unpack(t)) \
              if not fine then error(why, 0) end \
              keep[i] = co made = i \
            end end) \
          return ok, err, made > 10, made < 10000",
    )
    .unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    let line = print_line(&runtime);
    assert!(line.starts_with("false\t"), "{line}");
    assert!(line.ends_with("\ttrue\ttrue\n"), "{line}");
    assert!(runtime.memory().logical_bytes <= 4 * 1024 * 1024);
}

/// Closing is stackless too; it is not a recursive C call/resume chain.
#[test]
fn close_chains_past_the_resume_limit_checkpoint_safely() {
    let run = || {
        let chunk = crate::compile(
            br#"
            local count, co = 0, false
            for i = 1, 240 do
              local previous = co
              co = coroutine.create(function()
                local c <close> = setmetatable({}, {__close = function()
                  count = count + 1
                  if previous then assert(coroutine.close(previous)) end
                end})
                coroutine.yield()
              end)
              assert(coroutine.resume(co))
            end
            assert(coroutine.close(co))
            return count, coroutine.status(co)
        "#,
        )
        .unwrap();
        let mut runtime = boot_all(&chunk.proto);
        let mut journal = Journal::new();
        let mut checkpointed = false;
        loop {
            let outcome = runtime.run(1, &mut journal).unwrap();
            let closing = runtime
                .heap()
                .threads
                .iter()
                .filter(|(_, _, t)| t.closing)
                .count();
            if closing == 240 && !checkpointed {
                let bytes = runtime.snapshot().unwrap();
                runtime = restore(&runtime);
                assert_eq!(runtime.snapshot().unwrap(), bytes);
                checkpointed = true;
            }
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert!(checkpointed);
        assert_eq!(print_line(&runtime), "240\tdead\n");
    };
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
