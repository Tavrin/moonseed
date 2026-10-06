//! The incremental collector (Phase 3.28, ADR 0050) from Lua: the
//! `collectgarbage` corpus gives Lua 5.4.9's output; a program that
//! changes its heap through every phase of tiny steps gives Lua's result,
//! keeps the collector's invariant at every point Lua runs, and decides
//! alike (output, fuel, every collector event) under every quantum and
//! with a checkpoint at every step, taken in every phase; collector work
//! is paid in fuel; restore refuses collector states the runtime cannot
//! make.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::restore;
use super::*;
use crate::gc::{Atomic, Phase};

type Lines = Rc<RefCell<Vec<u8>>>;

fn boot_lua(source: &[u8], name: &str) -> Runtime {
    let mut chunk = crate::compile(source).unwrap();
    chunk.set_chunk_name(format!("@{name}").as_bytes());
    let mut runtime = Runtime::boot(
        Config::default(),
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    runtime
}

fn attach(runtime: &mut Runtime) -> Lines {
    let lines = Lines::default();
    let sink = lines.clone();
    runtime.set_output(Box::new(move |bytes| {
        sink.borrow_mut().extend_from_slice(bytes)
    }));
    lines
}

fn text(lines: &Lines) -> String {
    String::from_utf8_lossy(&lines.borrow()).into_owned()
}

/// A program that changes its heap while tiny steps (64 bytes apart)
/// carry cycles through every phase: weak values, weak keys, a shared
/// metatable whose mode flips, finalizers that register again, a closed
/// upvalue and a coroutine's stack written each round, explicit steps,
/// and full collections that abandon cycles in progress. What it prints
/// is what Lua 5.4.9 prints.
const STRESS: &[u8] = br#"
collectgarbage("incremental", 200, 100, 6)
local keep = {}
local weakv = setmetatable({}, {__mode = "v"})
local weakk = setmetatable({}, {__mode = "k"})
local modemt = {__mode = "k"}
local switch = setmetatable({}, modemt)
local many = {}
for i = 1, 12 do many[i] = setmetatable({{}}, modemt) end
local finalized = 0
local gcmt = {}
gcmt.__gc = function(o)
  finalized = finalized + 1
  if o.again then
    o.again = nil
    setmetatable(o, gcmt)
  end
end
local up = {}
local function holder() return up end
local co = coroutine.wrap(function(x)
  while true do
    local t = {x}
    x = coroutine.yield(t)
  end
end)
for round = 1, 400 do
  local node = {n = round}
  keep[#keep + 1] = node
  weakv[round] = node
  weakk[node] = {round}
  switch[node] = round
  if round % 7 == 0 then
    modemt.__mode = modemt.__mode == "k" and "v" or "k"
  end
  if round % 5 == 0 then
    setmetatable({again = round % 10 == 0}, gcmt)
  end
  if round % 3 == 0 then
    table.remove(keep, 1)
  end
  up = {round}
  keep[#keep][1] = co(round)
  if round % 11 == 0 then
    collectgarbage("step", 0)
  end
  if round % 97 == 0 then
    collectgarbage()
  end
end
modemt.__mode = "k"
collectgarbage()
collectgarbage()
local nv, nk, ns = 0, 0, 0
for _ in pairs(weakv) do nv = nv + 1 end
for _ in pairs(weakk) do nk = nk + 1 end
for _ in pairs(switch) do ns = ns + 1 end
print(#keep, nv, nk, ns, finalized > 0, holder()[1], keep[#keep][1][1])
"#;

/// Lua 5.4.9's output for [`STRESS`].
const STRESS_OUT: &str = "267\t267\t267\t267\ttrue\t400\t400\n";

/// What a run decided: its output, its fuel, and the collector's.
#[derive(Debug, PartialEq)]
struct Run {
    output: String,
    fuel: u64,
    trace: u64,
    work: u64,
    collections: u64,
    threshold: u64,
}

fn summary(runtime: &Runtime, lines: &Lines) -> Run {
    let gc = &runtime.heap().gc;
    Run {
        output: text(lines),
        fuel: runtime.fuel_consumed(),
        trace: gc.trace,
        work: gc.work,
        collections: gc.collections,
        threshold: gc.threshold,
    }
}

fn straight(source: &[u8], name: &str) -> Run {
    let mut runtime = boot_lua(source, name);
    let lines = attach(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    summary(&runtime, &lines)
}

/// The corpus writes what Lua 5.4.9 wrote for it.
#[test]
fn incremental_corpus_matches_lua() {
    let run = straight(&fixture("corpus_incgc.lua"), "corpus_incgc.lua");
    let expected = String::from_utf8(fixture("corpus_incgc.out")).unwrap();
    for (index, (a, b)) in run.output.lines().zip(expected.lines()).enumerate() {
        assert_eq!(a, b, "line {}", index + 1);
    }
    assert_eq!(run.output.lines().count(), expected.lines().count());
}

/// `MOONSEED_LUA54_UD` names the Lua 5.4.9 harness; the corpus and the
/// stress program give it what Moonseed's tests expect.
#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_incremental_corpus() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54_UD")
        .expect("MOONSEED_LUA54_UD must point at the userdata harness");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("corpus_incgc.lua")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, fixture("corpus_incgc.out"));
    let dir = crate::hostcaps::native::test_support::temp_dir().join(format!(
        "moonseed-incgc-{}",
        crate::hostcaps::native::test_support::id()
    ));
    crate::hostcaps::native::test_support::create_dir_all(&dir).unwrap();
    crate::hostcaps::native::test_support::write(dir.join("stress.lua"), STRESS).unwrap();
    let output = lua_command(&lua)
        .current_dir(&dir)
        .arg("stress.lua")
        .output()
        .unwrap();
    crate::hostcaps::native::test_support::remove_dir_all(&dir).unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), STRESS_OUT);
}

/// Gate N: the stress program gives Lua's result, and at every point Lua
/// runs, no black object refers to a white one.
#[test]
fn heap_changes_in_every_phase_keep_the_invariant() {
    let mut runtime = boot_lua(STRESS, "stress.lua");
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut checked = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        crate::gc::check_usage(runtime.heap()).unwrap();
        if runtime.heap().collector.marking() && !runtime.heap().collector.in_atomic() {
            crate::gc::check_invariant(runtime.heap()).unwrap();
            checked += 1;
        }
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(text(&lines), STRESS_OUT);
    assert!(checked > 1000, "checked {checked} points");
}

/// Gate M: every quantum gives the same output, the same fuel, and the
/// same collector events, for the corpus and the stress program; the
/// executor's quantum may split a step's work but never decides it.
#[test]
fn every_quantum_collects_alike() {
    for (source, name) in [
        (&fixture("corpus_incgc.lua")[..], "corpus_incgc.lua"),
        (STRESS, "stress.lua"),
    ] {
        let expected = straight(source, name);
        assert!(expected.collections > 2, "{name}");
        for quantum in [1u64, 2, 3, 7, 1000] {
            let mut runtime = boot_lua(source, name);
            let lines = attach(&mut runtime);
            let mut journal = Journal::new();
            while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
            assert_eq!(
                summary(&runtime, &lines),
                expected,
                "{name} quantum {quantum}"
            );
        }
    }
}

/// Where a checkpoint was taken: the collector's phase (with the atomic
/// step), whether an object was being scanned, a reset sweep, a finalizer
/// running, a full collection asked for.
fn place(runtime: &Runtime) -> String {
    let heap = runtime.heap();
    let collector = &heap.collector;
    format!(
        "{:?}{}{}{}{}",
        collector.phase,
        if collector.scan.is_some() {
            " scan"
        } else {
            ""
        },
        if collector.reset { " reset" } else { "" },
        if heap.finalizers.running {
            " finalizer"
        } else {
            ""
        },
        if heap.gc.full.is_some() { " full" } else { "" },
    )
}

/// Gate L: a checkpoint and restore at every step, in every phase, the
/// atomic steps one by one, a scan in progress, a sweep that only resets,
/// a finalizer running: the run goes on to the same output, fuel, and
/// collector events, as it does with sparse checkpoints. (A shorter run
/// of the stress program: a restore at each of its steps is slow.)
#[test]
fn checkpoints_in_every_phase_continue_alike() {
    let source = String::from_utf8_lossy(STRESS)
        .replace("for round = 1, 400", "for round = 1, 120")
        .replace("round % 7 == 0", "round % 2 == 0")
        .replace("round % 97 == 0", "round % 9 == 4");
    let source = source.as_bytes();
    fast_slow_equivalent(|| boot_lua(source, "stress.lua"), pair_results);
    let expected = straight(source, "stress.lua");
    for every in [1usize, 13] {
        let mut runtime = boot_lua(source, "stress.lua");
        let mut lines = attach(&mut runtime);
        let mut journal = Journal::new();
        let mut places = std::collections::BTreeSet::new();
        let mut step = 0usize;
        loop {
            if step.is_multiple_of(every) {
                places.insert(place(&runtime));
                let output = text(&lines);
                runtime = restore(&runtime);
                lines = attach(&mut runtime);
                lines.borrow_mut().extend_from_slice(output.as_bytes());
            }
            step += 1;
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(summary(&runtime, &lines), expected, "every {every}");
        if every == 1 {
            let mut wanted = vec![
                format!("{:?}", Phase::Pause),
                format!("{:?}", Phase::Begin),
                format!("{:?} scan", Phase::Propagate),
                format!("{:?}", Phase::Propagate),
                format!("{:?}", Phase::Sweep),
            ];
            // The tables sharing the metatable whose mode flips make
            // `Modes` and `Remark` take several slices.
            wanted.extend(
                Atomic::ALL
                    .iter()
                    // Roots and Separate are one batch each, begun and
                    // ended in one slice.
                    .filter(|step| {
                        !matches!(step, Atomic::Roots | Atomic::Separate | Atomic::Final)
                    })
                    .map(|step| format!("{:?}", Phase::Atomic(*step))),
            );
            for want in wanted {
                assert!(
                    places.iter().any(|place| place.starts_with(&want)),
                    "no checkpoint in {want}: {places:?}"
                );
            }
            for marker in [" reset", " finalizer", " full"] {
                assert!(
                    places.iter().any(|place| place.contains(marker)),
                    "no checkpoint with{marker}: {places:?}"
                );
            }
        }
    }
}

/// Gate D: collector work is paid in fuel. A full collection of a heap
/// of 10,000 entries costs at least its work divided by
/// `WORK_PER_FUEL`; a program that allocates too little to start a
/// cycle costs what it costs with the collector stopped.
#[test]
fn collector_work_is_paid_in_fuel() {
    let source = b"local t = {} for i = 1, 10000 do t[i] = {} end \
                   local before = collectgarbage('count') collectgarbage() return t";
    let mut runtime = boot_lua(source, "fuel.lua");
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let paid = runtime.fuel_consumed();
    let work = runtime.heap().gc.work;
    assert!(work > 20_000, "work {work}");
    let mut stopped = boot_lua(source, "fuel.lua");
    stopped.set_auto_gc(false);
    stopped
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(
        paid >= stopped.fuel_consumed()
            + (work - stopped.heap().gc.work) / u64::from(crate::gc::WORK_PER_FUEL),
        "paid {paid}, stopped {}",
        stopped.fuel_consumed()
    );
    // Too little allocation to start a cycle: no work, the same fuel.
    let small = b"local s = 0 for i = 1, 1000 do s = s + i end return s";
    let config = Config {
        gc_min_debt: 1 << 30,
        ..Config::default()
    };
    let fuel = |auto: bool| {
        let chunk = crate::compile(small).unwrap();
        let mut runtime =
            Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false).unwrap();
        runtime.set_auto_gc(auto);
        // Booting in generational mode is a full collection, the host's.
        let booted = runtime.heap().gc.work;
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(runtime.heap().gc.work, booted);
        runtime.fuel_consumed()
    };
    assert_eq!(fuel(true), fuel(false));
}

/// Gate C: with a quantum of one, a slice of collector work is at most
/// what one unit of fuel pays for and one batch that cannot be split,
/// however large the table being traced.
#[test]
fn a_large_table_never_makes_a_large_slice() {
    let source = b"local t = {} for i = 1, 10000 do t[i] = i % 7 == 0 and {} or i end \
                   for round = 1, 3 do collectgarbage() end return #t";
    let mut runtime = boot_lua(source, "large.lua");
    let mut journal = Journal::new();
    let mut largest = 0;
    loop {
        let before = runtime.heap().gc.work;
        let outcome = runtime.run(1, &mut journal).unwrap();
        largest = largest.max(runtime.heap().gc.work - before);
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(
        largest <= u64::from(crate::gc::WORK_PER_FUEL) + 300,
        "a slice of {largest} units"
    );
}

/// A host root asked for while the collector is in its atomic phase
/// waits for the decision: an object found dead is not found; a live one
/// is rooted and kept.
#[test]
fn a_root_made_in_the_atomic_phase_waits_for_the_decision() {
    let mut runtime = boot_lua(STRESS, "stress.lua");
    let mut journal = Journal::new();
    while !runtime.heap().collector.in_atomic() {
        runtime.run(1, &mut journal).unwrap();
    }
    let entry = runtime.entry_id().unwrap();
    let root = runtime.root_id(entry).unwrap();
    assert!(!runtime.heap().collector.in_atomic());
    assert!(runtime.root_alive(&root).unwrap());
}

/// Restore refuses collector states the collector cannot reach.
#[test]
fn restore_refuses_collector_states_it_cannot_make() {
    let mut runtime = boot_lua(STRESS, "stress.lua");
    let mut journal = Journal::new();
    while !(runtime.heap().collector.phase == Phase::Propagate
        && runtime.heap().collector.scan.is_some()
        && !runtime.heap().collector.gray.is_empty())
    {
        runtime.run(1, &mut journal).unwrap();
    }
    let image = runtime.to_image().unwrap();
    let registry = HostRegistry::proof();
    let domain = runtime.effect_domain();
    let refuse = |change: &dyn Fn(&mut crate::snapshot::Image), why: &str| {
        let mut changed = image.clone();
        change(&mut changed);
        let bytes = crate::snapshot::encode(&changed).unwrap();
        assert!(
            Runtime::from_snapshot(&bytes, &registry, domain).is_err(),
            "accepted: {why}"
        );
    };
    let missing = image.next_object_id + 7;
    refuse(&|image| image.collector.phase = 9, "unknown phase");
    refuse(
        &|image| image.collector.atomic = 3,
        "an atomic step outside atomic",
    );
    refuse(&|image| image.collector.white = 2, "a third white");
    refuse(
        &|image| image.collector.gray.push(missing),
        "a missing gray object",
    );
    refuse(
        &|image| {
            let first = image.collector.gray[0];
            image.collector.gray.push(first);
        },
        "a gray object twice",
    );
    refuse(
        &|image| {
            let (id, _, _) = image.collector.marks[0];
            image.collector.marks.push((id, 3, 0));
        },
        "an object marked twice",
    );
    refuse(
        &|image| image.collector.marks.push((missing, 3, 0)),
        "a missing marked object",
    );
    refuse(&|image| image.collector.marks[0].1 = 7, "an unknown mark");
    // Found by the milestone review: gray objects in no list (never traced,
    // so what only they reach would be freed), and a full collection no
    // run asks for (the collector would cycle for ever).
    refuse(
        &|image| {
            image.collector.gray.clear();
            image.collector.again.clear();
        },
        "gray objects in no list",
    );
    refuse(
        &|image| image.gc.full = Some(u64::MAX),
        "an unreachable full target",
    );
    refuse(
        &|image| {
            if let Some(scan) = &mut image.collector.scan {
                scan.1 = u32::MAX;
            }
            image.collector.marks.retain(|_| true);
            for (id, mark, _) in &mut image.collector.marks {
                if Some(*id) == image.collector.scan.map(|scan| scan.0) {
                    *mark = crate::heap::mark::BLACK;
                }
            }
        },
        "a scan past its object",
    );
    refuse(
        &|image| image.collector.ephemerons.push(missing),
        "a missing ephemeron table",
    );
    refuse(
        &|image| image.collector.cursor = 5,
        "an atomic cursor outside atomic",
    );
    refuse(
        &|image| image.collector.sweep_left = 3,
        "a sweep count outside the sweep",
    );
    refuse(
        &|image| image.gc.prepaid = crate::gc::WORK_PER_FUEL,
        "prepaid work past a unit",
    );
    refuse(
        &|image| {
            image.collector.phase = 0;
        },
        "lists in the pause",
    );
    refuse(
        &|image| {
            let key = image.reserved[0];
            image.collector.waiting = vec![(key, vec![crate::snapshot::EncValue::Integer(1)])];
        },
        "values waiting on a string",
    );
    // Unchanged, it restores.
    let bytes = crate::snapshot::encode(&image).unwrap();
    assert!(Runtime::from_snapshot(&bytes, &registry, domain).is_ok());
}

/// Found by the milestone review: the host calls a closure, or resumes a
/// thread, by id while a run is paused in an atomic phase. The phase
/// finishes first, so the call never makes a reference the phase has
/// passed: whatever the call returns, the heap holds nothing dead and a
/// checkpoint restores.
#[test]
fn host_calls_in_the_atomic_phase_finish_it_first() {
    let source: &[u8] = b"collectgarbage('incremental', 200, 100, 6) \
        do local weak = setmetatable({}, {__mode = 'v'}) KEEP = {x = 7} weak[1] = KEEP \
          F = function() WEAK = weak return 7 end end \
        CO = coroutine.create(function() while true do local t = {} coroutine.yield(t) end end) \
        coroutine.resume(CO) \
        local keep = {} local wv = setmetatable({}, {__mode = 'v'}) \
        for i = 1, 2000 do keep[i] = {} wv[i] = keep[i] end \
        F, KEEP, CO = nil, nil, nil \
        for round = 1, 100000 do local t = {round} end";
    for call in 0..2 {
        let mut runtime = boot_lua(source, "host.lua");
        let mut journal = Journal::new();
        let (mut closure, mut thread) = (None, None);
        loop {
            let outcome = runtime.run(1, &mut journal).unwrap();
            assert!(matches!(outcome, StepOutcome::Paused(_)), "{outcome:?}");
            let heap = runtime.heap();
            let globals = heap.tables.get(heap.globals.unwrap()).unwrap();
            let global = |name: &[u8]| globals.table.get_view(crate::table::KeyView::string(name));
            match (global(b"F"), global(b"CO")) {
                (Some(crate::value::Value::Closure(f)), Some(crate::value::Value::Thread(co))) => {
                    closure = Some(heap.closures.get(f).unwrap().id);
                    thread = Some(heap.threads.get(co).unwrap().id);
                }
                (None, None)
                    if closure.is_some()
                        && matches!(
                            heap.collector.phase,
                            Phase::Atomic(Atomic::Values | Atomic::Keys | Atomic::Resurrect)
                        ) =>
                {
                    break;
                }
                _ => {}
            }
        }
        if call == 0 {
            let _ = runtime.call_closure(closure.unwrap(), &mut journal);
        } else {
            let _ = runtime.resume_thread(thread.unwrap(), 1000, &mut journal);
        }
        assert!(!runtime.heap().collector.in_atomic());
        restore(&runtime);
    }
}

/// What a cycle estimates it kept is an upper bound on what the heap
/// holds when it ends, and a close one: an object traced again, or made
/// while the cycle marked, counts once (an overcount found by the
/// milestone review made the quota refuse early).
#[test]
fn the_live_estimate_is_a_close_upper_bound() {
    let mut runtime = boot_lua(STRESS, "stress.lua");
    let mut journal = Journal::new();
    let mut cycles = runtime.heap().gc.collections;
    let mut worst = 0f64;
    let mut seen = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        if heap.gc.collections != cycles {
            cycles = heap.gc.collections;
            let actual = crate::gc::logical_size(heap);
            let estimate = heap.gc.live.saturating_add(heap.gc.debt);
            assert!(estimate >= actual, "estimate {estimate} below {actual}");
            worst = worst.max(estimate as f64 / actual as f64);
            seen += 1;
        }
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(seen > 5, "{seen} cycles");
    assert!(worst < 1.2, "estimate {worst:.2} times the heap");
}
