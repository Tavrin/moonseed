//! Generational collection (Phase 3.29, ADR 0051) from Lua: the corpus and
//! a stress program give Lua 5.4.9's output; the generational invariant
//! holds at every point Lua runs; every quantum, and a checkpoint at every
//! step taken in every phase of young and major collections, decide alike
//! (output, fuel, every collector event); a structure that grows and stays
//! live falls back on whole cycles and returns; quota emergencies reclaim
//! old garbage; restore refuses generational states the runtime cannot
//! make.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::restore;
use super::*;
use crate::gc::{Atomic, Phase};
use crate::heap::age;

type Lines = Rc<RefCell<Vec<u8>>>;

fn boot_lua(source: &[u8], config: Config) -> Runtime {
    let mut chunk = crate::compile(source).unwrap();
    chunk.set_chunk_name(b"@gen.lua");
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
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

/// Old structures written to every round with new objects (tables, a weak
/// table, ephemerons with old and new keys, a coroutine's stack, an
/// upvalue), a shared metatable whose mode flips, finalizers that register
/// again, short-lived garbage, explicit steps, full collections, and mode
/// switches. What it prints is what Lua 5.4.9 prints.
const STRESS: &[u8] = br#"
collectgarbage("generational", 20, 100)
local old = {}
for i = 1, 50 do old[i] = {i} end
local oldweak = setmetatable({}, {__mode = "v"})
local oldeph = setmetatable({}, {__mode = "k"})
local modemt = {__mode = "k"}
local switch = setmetatable({}, modemt)
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
collectgarbage()
for round = 1, 400 do
  local node = {n = round}
  local slot = old[round % 50 + 1]
  slot[2] = node
  oldweak[round] = node
  oldeph[node] = {round}
  oldeph[slot] = node
  switch[node] = round
  if round % 7 == 0 then
    modemt.__mode = modemt.__mode == "k" and "v" or "k"
  end
  if round % 5 == 0 then
    setmetatable({again = round % 10 == 0}, gcmt)
  end
  up = {round}
  old[1][3] = co(round)
  local tmp = {}
  for j = 1, 6 do tmp[j] = {j, tostring(j * round)} end
  if round % 11 == 0 then
    collectgarbage("step", 0)
  end
  if round % 97 == 0 then
    collectgarbage()
  end
  if round % 131 == 0 then
    collectgarbage("incremental")
    collectgarbage("generational")
  end
end
modemt.__mode = "k"
collectgarbage()
collectgarbage()
local nv, nk, ns = 0, 0, 0
for _ in pairs(oldweak) do nv = nv + 1 end
for _ in pairs(oldeph) do nk = nk + 1 end
for _ in pairs(switch) do ns = ns + 1 end
print(nv, nk, ns, finalized > 0, holder()[1], old[1][3][1], old[1][2].n)
"#;

/// Lua 5.4.9's output for [`STRESS`].
const STRESS_OUT: &str = "50\t100\t50\ttrue\t400\t400\t400\n";

/// What a run decided: its output, its fuel, and the collector's.
#[derive(Debug, PartialEq)]
struct Run {
    output: String,
    fuel: u64,
    trace: u64,
    work: u64,
    collections: u64,
    minors: u64,
}

fn summary(runtime: &Runtime, lines: &Lines) -> Run {
    let gc = &runtime.heap().gc;
    Run {
        output: text(lines),
        fuel: runtime.fuel_consumed(),
        trace: gc.trace,
        work: gc.work,
        collections: gc.collections,
        minors: gc.minors,
    }
}

fn straight(source: &[u8], config: Config) -> Run {
    let mut runtime = boot_lua(source, config);
    let lines = attach(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    summary(&runtime, &lines)
}

/// Gate O and V: the corpora write what Lua 5.4.9 writes, booted in
/// either mode: the generational one, and `step` state by state.
#[test]
fn generational_corpus_matches_lua() {
    for (name, minors) in [("corpus_gengc", 10), ("corpus_genstep", 1)] {
        let expected = String::from_utf8(fixture(&format!("{name}.out"))).unwrap();
        for mode in [crate::GcMode::Generational, crate::GcMode::Incremental] {
            let config = Config {
                gc_mode: mode,
                ..Config::default()
            };
            let run = straight(&fixture(&format!("{name}.lua")), config);
            for (index, (a, b)) in run.output.lines().zip(expected.lines()).enumerate() {
                assert_eq!(a, b, "{name} {mode:?} line {}", index + 1);
            }
            assert_eq!(run.output.lines().count(), expected.lines().count());
            assert!(run.minors > minors, "{name} {mode:?} {run:?}");
        }
    }
}

/// `MOONSEED_LUA54` names Lua 5.4.9's interpreter, which starts in
/// generational mode, and `MOONSEED_LUA54_UD` the harness, which starts
/// in incremental mode: both print the corpus's expected output and the
/// stress program's.
#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_generational_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let dir = crate::hostcaps::native::test_support::temp_dir().join(format!(
        "moonseed-gengc-{}",
        crate::hostcaps::native::test_support::id()
    ));
    crate::hostcaps::native::test_support::create_dir_all(&dir).unwrap();
    crate::hostcaps::native::test_support::write(dir.join("stress.lua"), STRESS).unwrap();
    for variable in ["MOONSEED_LUA54", "MOONSEED_LUA54_UD"] {
        let lua = crate::hostcaps::native::test_support::var(variable)
            .unwrap_or_else(|_| panic!("{variable} must be set"));
        for name in ["corpus_gengc", "corpus_genstep"] {
            let output = lua_command(&lua)
                .current_dir(&root)
                .arg(format!("{name}.lua"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{variable}");
            assert_eq!(
                output.stdout,
                fixture(&format!("{name}.out")),
                "{variable} {name}"
            );
        }
        let output = lua_command(&lua)
            .current_dir(&dir)
            .arg("stress.lua")
            .output()
            .unwrap();
        assert!(output.status.success(), "{variable}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), STRESS_OUT);
    }
    crate::hostcaps::native::test_support::remove_dir_all(&dir).unwrap();
}

/// Gates F, I, N: the stress program gives Lua's result, and at every
/// point Lua runs, between young collections no old object refers to a
/// young one the next young collection would not trace; while a major
/// collection marks, no black object refers to a white one.
#[test]
fn heap_changes_keep_the_generational_invariant() {
    let mut runtime = boot_lua(STRESS, Config::default());
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let (mut idle, mut marking) = (0, 0);
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        // In every phase, a young collection's too, and the exact count.
        crate::gc::check_gen_invariant(heap).unwrap();
        crate::gc::check_invariant(heap).unwrap();
        crate::gc::check_usage(heap).unwrap();
        if heap.collector.generational && !heap.collector.minor {
            idle += 1;
        }
        if heap.collector.marking() {
            marking += 1;
        }
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(text(&lines), STRESS_OUT);
    assert!(
        idle > 1000 && marking > 100,
        "checked {idle} and {marking} points"
    );
    assert!(runtime.heap().gc.minors > 20);
}

/// Gate R: every quantum gives the same output, fuel, and collector
/// events (young and major collections, ages promoted, the remembered
/// set, mode switches), for the corpus and the stress program.
#[test]
fn every_quantum_collects_alike_in_generational_mode() {
    for (source, name) in [
        (&fixture("corpus_gengc.lua")[..], "corpus_gengc.lua"),
        (STRESS, "stress.lua"),
    ] {
        let expected = straight(source, Config::default());
        assert!(expected.minors > 10 && expected.collections > 2, "{name}");
        for quantum in [1u64, 2, 3, 7, 1000] {
            let mut runtime = boot_lua(source, Config::default());
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

/// Where a checkpoint was taken: the phase, whether it belongs to a young
/// collection, a sweep making survivors old, one leaving generational
/// form, or generational form between collections.
fn place(runtime: &Runtime) -> String {
    let collector = &runtime.heap().collector;
    format!(
        "{:?}{}{}{}{}",
        collector.phase,
        if collector.minor { " minor" } else { "" },
        if collector.to_old { " to_old" } else { "" },
        if collector.reset { " reset" } else { "" },
        if collector.generational && !collector.minor && !collector.to_old {
            " gen"
        } else {
            ""
        },
    )
}

/// Gate Q: a checkpoint and restore at every step, in every phase of a
/// young collection (the old objects traced again, each atomic step, the
/// young sweep, the touched correction), of a major one (leaving
/// generational form, marking, the sweep making survivors old), and
/// between: the run goes on to the same output, fuel, and collector
/// events, as with sparse checkpoints.
#[test]
fn checkpoints_in_every_generational_phase_continue_alike() {
    let source = String::from_utf8_lossy(STRESS)
        .replace("for round = 1, 400", "for round = 1, 150")
        .replace("round % 97 == 0", "round % 37 == 4")
        .replace("round % 131 == 0", "round % 53 == 9");
    let source = source.as_bytes();
    let expected = straight(source, Config::default());
    for every in [1usize, 13] {
        let mut runtime = boot_lua(source, Config::default());
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
                format!("{:?} gen", Phase::Pause),
                format!("{:?} minor", Phase::Begin),
                format!("{:?} minor", Phase::Sweep),
                format!("{:?} minor", Phase::Touched),
                format!("{:?} to_old", Phase::Sweep),
                format!("{:?} reset", Phase::Sweep),
                format!("{:?}", Phase::Propagate),
            ];
            wanted.extend(
                [Atomic::Mark, Atomic::Values, Atomic::Keys]
                    .iter()
                    .map(|step| format!("{:?} minor", Phase::Atomic(*step))),
            );
            for want in wanted {
                assert!(
                    places.iter().any(|place| place.starts_with(&want)),
                    "no checkpoint in {want}: {places:?}"
                );
            }
        }
    }
}

/// Gate N: a structure that keeps growing and stays live makes a major
/// collection bad, so the collector falls back on whole cycles instead
/// of a useless major every time memory doubles; once the growth stops,
/// a cycle that keeps about as much returns it to young collections.
/// Every state of it is snapshot state.
#[test]
fn bad_majors_fall_back_and_return() {
    let source = b"
        local grow = {}
        for i = 1, 6000 do grow[i] = {i} end
        local sum = 0
        for round = 1, 400 do
          local t = {}
          for j = 1, 20 do t[j] = {j} end
          sum = sum + #t
        end
        print(#grow, sum)";
    // Near an object limit the growth makes majors bad quickly enough for
    // the program to see the fallback and the return.
    let config = Config {
        gc_min_debt: 4096,
        max_objects: 10_000,
        ..Config::default()
    };
    let mut runtime = boot_lua(source, config);
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let (mut fell_back, mut returned, mut fallback_cycles) = (false, false, 0u64);
    let mut last = runtime.heap().gc.collections;
    let mut minors = runtime.heap().gc.minors;
    loop {
        let outcome = runtime.run(50, &mut journal).unwrap();
        let heap = runtime.heap();
        if heap.gc.bad > 0 {
            fell_back = true;
            assert!(!heap.collector.generational || heap.collector.to_old);
            assert_eq!(
                heap.gc.minors, minors,
                "a young collection while falling back"
            );
            if heap.gc.collections != last {
                fallback_cycles += 1;
            }
        } else if fell_back && heap.collector.generational {
            returned = true;
        }
        last = heap.gc.collections;
        minors = heap.gc.minors;
        // Falling back is snapshot state like any other.
        if fell_back && !returned {
            runtime = restore(&runtime);
            let sink = lines.clone();
            runtime.set_output(Box::new(move |bytes| {
                sink.borrow_mut().extend_from_slice(bytes)
            }));
        }
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(text(&lines), "6000\t8000\n");
    assert!(
        fell_back && returned,
        "fell back {fell_back}, returned {returned}"
    );
    // While falling back, whole cycles only, one each time the heap
    // doubles (the pause), as Lua's; young collections once returned.
    assert!(
        (1..=8).contains(&fallback_cycles),
        "{fallback_cycles} cycles while falling back"
    );
    assert!(runtime.heap().gc.minors > 5);
}

/// Gate H (emergency): with old garbage filling the quota, an allocation
/// that would pass it collects old objects too: a full collection, not
/// young ones that cannot free them. The run never fails for memory it
/// could free, and the heap stays generational.
#[test]
fn quota_emergencies_reclaim_old_garbage() {
    let source = b"
        local kept
        for round = 1, 30 do
          local big = {}
          for i = 1, 3000 do big[i] = i end
          collectgarbage('step', 0) collectgarbage('step', 0) collectgarbage('step', 0)
          kept = big
        end
        print(#kept)";
    let config = Config {
        max_logical_heap: 400 * 1024,
        ..Config::default()
    };
    let mut runtime = boot_lua(source, config);
    let lines = attach(&mut runtime);
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(text(&lines), "3000\n");
    assert!(runtime.heap().collector.generational);
    assert!(runtime.heap().gc.collections > 3);
}

/// Gate Q: restore refuses a generational state the runtime cannot make.
#[test]
fn restore_refuses_generational_states_it_cannot_make() {
    let mut runtime = boot_lua(STRESS, Config::default());
    let mut journal = Journal::new();
    // Between young collections, with a touched object, an `OLD1` one, and
    // young ones.
    loop {
        runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        let collector = &heap.collector;
        if collector.generational
            && collector.phase == Phase::Pause
            && heap.gc.minors > 3
            && heap.tables.again().len() > 1
            && collector.revisit.len() > 2
        {
            break;
        }
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
    let c = &image.collector;
    assert!(!c.young.is_empty() && !c.revisit.is_empty() && !c.again.is_empty());
    // Black and old is the default between young collections: an object
    // written in no list.
    let implied = |id: u64| {
        !c.young.contains(&id)
            && !c.marks.iter().any(|entry| entry.0 == id)
            && !c.ages.iter().any(|entry| entry.0 == id)
    };
    let old = image
        .tables
        .iter()
        .map(|table| table.id)
        .find(|id| implied(*id))
        .unwrap();
    let young = c.young[0];
    refuse(
        &|image| image.collector.marks[0].2 = 2,
        "an invalid age (OLD0)",
    );
    refuse(&|image| image.collector.marks[0].2 = 9, "an unknown age");
    refuse(
        &|image| image.collector.young.push(old),
        "an old object on a young list",
    );
    refuse(
        &|image| {
            image.collector.young.remove(0);
        },
        "a young object on no young list",
    );
    refuse(
        &|image| {
            let first = image.collector.young[0];
            image.collector.young.push(first);
        },
        "a young object listed twice",
    );
    refuse(
        &|image| image.collector.again.clear(),
        "a touched object not remembered",
    );
    refuse(
        &|image| {
            let first = image.collector.revisit[0];
            image.collector.revisit.push(first);
        },
        "a remembered object listed twice",
    );
    let remembered = c
        .revisit
        .iter()
        .position(|id| {
            c.marks
                .iter()
                .any(|(marked, _, age)| marked == id && matches!(*age, age::OLD1 | age::TOUCHED2))
        })
        .unwrap();
    refuse(
        &|image| {
            image.collector.revisit.remove(remembered);
        },
        "an OLD1 or TOUCHED2 object not remembered",
    );
    refuse(
        &|image| image.collector.revisit.push(old),
        "an old object remembered for nothing",
    );
    refuse(
        &|image| image.collector.ages.push((old, age::SURVIVAL)),
        "a white object on no young list",
    );
    refuse(
        &|image| {
            for entry in &mut image.collector.marks {
                if entry.0 == young {
                    entry.1 = crate::heap::mark::BLACK;
                }
            }
            if !image.collector.marks.iter().any(|entry| entry.0 == young) {
                image
                    .collector
                    .marks
                    .push((young, crate::heap::mark::BLACK, age::NEW));
            }
        },
        "a young object black between young collections",
    );
    refuse(
        &|image| image.gc.major_base = image.gc.quota + 1,
        "a major baseline past the quota",
    );
    refuse(
        &|image| image.gc.bad = 5,
        "falling back in generational form",
    );
    refuse(
        &|image| image.collector.minor = true,
        "a young collection between collections",
    );
    refuse(
        &|image| image.collector.phase = 5,
        "touched correction outside a young collection",
    );
    refuse(
        &|image| image.collector.decide = 2,
        "a major decision in generational form",
    );
    refuse(&|image| image.collector.decide = 7, "an unknown decision");
    refuse(
        &|image| image.gc.generational = false,
        "generational form in incremental mode",
    );
    // An old table made to hold a young one the next young collection
    // would not trace: restore checks the generational invariant.
    let old_table = image
        .tables
        .iter()
        .find(|table| {
            implied(table.id)
                && table
                    .slots
                    .iter()
                    .any(|slot| matches!(slot.body, crate::snapshot::SlotBody::Live { .. }))
        })
        .map(|table| table.id)
        .unwrap();
    let young_table = *c
        .young
        .iter()
        .find(|id| image.tables.iter().any(|table| table.id == **id))
        .unwrap();
    refuse(
        &|image| {
            let table = image
                .tables
                .iter_mut()
                .find(|table| table.id == old_table)
                .unwrap();
            for slot in &mut table.slots {
                if let crate::snapshot::SlotBody::Live { value, .. } = &mut slot.body {
                    *value = crate::snapshot::EncValue::Table(young_table);
                    break;
                }
            }
        },
        "an old object referring to an untraced young one",
    );
}

/// The logical heap is exact at every step, and each young collection
/// takes it as what it kept: a thread is charged again for what it holds
/// when traced (stack slots, the text builtins build, which shrink without
/// being freed), and a table that compacts gives its dead slots back.
#[test]
fn the_logical_heap_is_exact_through_young_collections() {
    let source = br#"
        local q, head, tail = {}, 1, 0
        local function f(n) if n == 0 then return 0 end return 1 + f(n - 1) end
        local keep = {}
        for round = 1, 3000 do
          local s = ("x"):rep(50) .. round
          local t = table.concat({s, s}, ",")
          tail = tail + 1
          q[tail] = {s}
          if tail - head > 50 then q[head] = nil head = head + 1 end
          f(20)
          if round % 100 == 0 then keep[#keep + 1] = t end
        end
        print(#keep)"#;
    let mut runtime = boot_lua(source, Config::default());
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut seen = runtime.heap().gc.minors;
    let mut minors = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        crate::gc::check_usage(heap).unwrap();
        if heap.gc.minors != seen {
            seen = heap.gc.minors;
            minors += 1;
            assert_eq!(heap.gc.live, heap.gc.used);
            // The running thread was traced: charged what it holds.
            let thread = heap.threads.get(heap.active.unwrap()).unwrap();
            assert_eq!(thread.charged_slots, thread.extent());
            assert_eq!(thread.charged_held, thread.held_bytes());
        }
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert_eq!(text(&lines), "30\n");
    assert!(minors > 10, "{minors} young collections");
}

/// Gate Y: extreme multipliers (a young collection at every chance, or
/// almost never; a major one at every chance, or almost never; the
/// stored byte wrapping to 0) change when collections run, never what
/// the stress program prints.
#[test]
fn extreme_multipliers_print_the_same() {
    for (minor, major) in [(1, 1), (1, 1000), (200, 1), (255, 1000), (256, 4), (100, 0)] {
        let source = String::from_utf8_lossy(STRESS).replace(
            r#"collectgarbage("generational", 20, 100)"#,
            &format!(r#"collectgarbage("generational", {minor}, {major})"#),
        );
        let run = straight(source.as_bytes(), Config::default());
        assert_eq!(run.output, STRESS_OUT, "{minor} {major}");
        assert!(
            run.minors > 0 || run.collections > 5,
            "{minor} {major} {run:?}"
        );
    }
}
