//! Regressions from the Phase 3.29 milestone review (ADR 0051): the exact
//! logical heap through compactions, shrinks and sweeps, and the quota it
//! guards; a compaction never stalls a sweep; restore refuses tampered
//! states in every generational phase, and random tampering never
//! restores into a state that later frees what is reachable; a host write
//! between a young collection's units; `collectgarbage` mode calls during
//! a major collection; young collections that look at neither old
//! finalizable objects nor old threads; two torture programs that match
//! Lua 5.4.9 with every invariant checked at every step, and restores.

use std::cell::RefCell;
use std::rc::Rc;

use super::base::restore;
use super::*;
use crate::gc::{Decide, Phase};
use crate::heap::{age, mark};
use crate::snapshot::{EncValue, Image, SlotBody};
use crate::table::KeyView;
use crate::value::Value;

type Lines = Rc<RefCell<Vec<u8>>>;

fn boot_lua(source: &[u8], config: Config) -> Runtime {
    let mut chunk = crate::compile(source).unwrap();
    chunk.set_chunk_name(b"@review.lua");
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    for (name, _) in crate::host::USERDATA_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
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

fn global(runtime: &Runtime, name: &[u8]) -> Option<Value> {
    let heap = runtime.heap();
    heap.globals
        .and_then(|globals| heap.tables.get(globals))
        .and_then(|globals| globals.table.get_view(KeyView::string(name)))
}

fn global_int(runtime: &Runtime, name: &[u8]) -> i64 {
    match global(runtime, name) {
        Some(Value::Integer(n)) => n,
        _ => -1,
    }
}

/// The host sets a global the program polls, through the barrier.
fn set_global_int(runtime: &mut Runtime, name: &[u8], value: i64) {
    let heap = runtime.heap_mut();
    let globals = heap.globals.unwrap();
    assert!(
        heap.tables
            .get_mut(globals)
            .unwrap()
            .table
            .update_view(KeyView::string(name), Value::Integer(value))
    );
}

/// Every invariant restore checks, the exact logical heap, and no stale
/// handle.
fn check_all(runtime: &Runtime) {
    let heap = runtime.heap();
    crate::gc::check_invariant(heap).unwrap();
    crate::gc::check_gen_invariant(heap).unwrap();
    crate::gc::check_usage(heap).unwrap();
    crate::gc::check_references(heap).unwrap();
}

/// Whether a step gets the checks that cost a walk of the heap: inside
/// the window a test is about, and every 61st step otherwise (each
/// collection's end is checked by the runtime's own test hook).
fn sampled(step: &mut u64, window: bool) -> bool {
    *step += 1;
    window || (*step).is_multiple_of(61)
}

fn run_until(source: &[u8], stop: &dyn Fn(&Runtime) -> bool) -> Runtime {
    let mut runtime = boot_lua(source, Config::default());
    let mut journal = Journal::new();
    while !stop(&runtime) {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    runtime
}

/// Old tables, one with `dead` dead slots that the next insert compacts,
/// and churn: a ring of tables kept and garbage. `big.x = 1` runs when
/// `n` reaches `trig`, which the host sets.
fn compact(dead: u32) -> String {
    format!(
        r#"
strs = {{}}
for i = 1, 6000 do strs[i] = "s" .. i end
big = {{}}
for i = 1, {dead} do big[i] = i end
for i = 1, {dead} do big[i] = nil end
ring = {{}}
trig = -1
n = 0
while n < 60000 do
  n = n + 1
  ring[n % 3000 + 1] = {{n, n, n, n}}
  local g = {{n}}
  if n == trig then big.x = 1 end
end
print(n)
"#
    )
}

/// Run `compact(dead)`, compacting `big` as a sweep making survivors old
/// begins (or never, `trigger` false). Returns the steps that sweep took,
/// whether a full collection was ever wanted, and the output; the exact
/// count, and the quota, hold at every step.
fn compact_during_sweep(dead: u32, trigger: bool) -> (u64, bool, String) {
    let mut runtime = boot_lua(compact(dead).as_bytes(), Config::default());
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let (mut set, mut steps, mut sweeping, mut full) = (false, 0u64, false, false);
    let mut step = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        if sampled(&mut step, heap.collector.to_old) {
            crate::gc::check_usage(heap).unwrap();
        }
        assert!(heap.gc.used <= heap.gc.quota);
        full |= heap.gc.full.is_some();
        let in_sweep = heap.collector.to_old && heap.collector.phase == Phase::Sweep;
        if !set && global_int(&runtime, b"n") > 0 && in_sweep && heap.collector.sweep_at.0 == 0 {
            set = true;
            sweeping = true;
            if trigger {
                let n = global_int(&runtime, b"n");
                set_global_int(&mut runtime, b"trig", n + 1);
            }
        }
        if sweeping {
            if in_sweep {
                steps += 1;
            } else {
                sweeping = false;
            }
        }
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(set, "no sweep making survivors old");
    (steps, full, text(&lines))
}

/// A table that compacts while a sweep makes survivors old, before the
/// sweep reaches it: the logical heap is exact at every step (it fell
/// 64,192 bytes below the heap before the review).
#[test]
fn review_compaction_during_sweep_to_old() {
    let (_, _, output) = compact_during_sweep(3000, true);
    assert_eq!(output, "60000\n");
}

/// A compaction of 20,000 dead slots during a major's sweep forgives no
/// collector work: the sweep takes the steps it takes without one, and
/// no emergency collection follows (before the review's fix it stood
/// near one cursor for about 965 steps while garbage piled up).
#[test]
fn review_compaction_does_not_stall_a_sweep() {
    let (with, full_with, output) = compact_during_sweep(20_000, true);
    let (without, full_without, _) = compact_during_sweep(20_000, false);
    assert_eq!(output, "60000\n");
    assert!(!full_with && !full_without, "an emergency collection");
    assert!(
        with <= without + without / 10 + 10,
        "{with} steps against {without}"
    );
}

/// Filling up to the quota after a compaction during a sweep making
/// survivors old: the logical heap, counted exactly, never passes the
/// quota (it passed it by 64,034 bytes before the review).
#[test]
fn review_quota_overshoot() {
    const OVERSHOOT: &str = r#"
strs = {}
for i = 1, 6000 do strs[i] = "s" .. i end
big = {}
for i = 1, 3000 do big[i] = i end
for i = 1, 3000 do big[i] = nil end
ring = {}
trig = -1
go = 0
n = 0
while go == 0 do
  n = n + 1
  ring[n % 3000 + 1] = {n, n, n, n}
  local g = {n}
  if n == trig then big.x = 1 end
end
collectgarbage("stop")
fill = {}
local ok, err = pcall(function()
  local i = 0
  while true do i = i + 1 fill[i] = i end
end)
print(ok, #fill > 0)
"#;
    let quota = 1_600_000u64;
    let config = Config {
        max_logical_heap: quota,
        ..Config::default()
    };
    let mut runtime = boot_lua(OVERSHOOT.as_bytes(), config);
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let (mut armed, mut was_old, mut released) = (false, false, false);
    let mut step = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        // The count is exact, so near the quota it is checked at every
        // step against the objects themselves.
        if sampled(
            &mut step,
            heap.collector.to_old || heap.gc.used + 100_000 > quota,
        ) {
            crate::gc::check_usage(heap).unwrap();
            assert!(
                crate::gc::counted_bytes(heap) <= quota,
                "the logical heap passed the quota"
            );
        }
        assert!(heap.gc.used <= quota);
        let to_old = heap.collector.to_old;
        if !armed
            && global_int(&runtime, b"n") > 0
            && to_old
            && heap.collector.phase == Phase::Sweep
            && heap.collector.sweep_at.0 == 0
        {
            armed = true;
            let n = global_int(&runtime, b"n");
            set_global_int(&mut runtime, b"trig", n + 1);
        }
        if armed && !released && was_old && !to_old {
            released = true;
            set_global_int(&mut runtime, b"go", 1);
        }
        was_old = to_old;
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(released);
    assert_eq!(text(&lines), "false\ttrue\n");
}

/// A host userdata's charge shrinks, from Lua (`set_userdata_charge`) and
/// from the host (`with_userdata_mut`), and a table compacts: during an
/// incremental sweep, and right after a young collection. The logical
/// heap is exact at every step, and each shrink leaves it at once.
#[test]
fn review_shrinks_leave_the_logical_heap_at_once() {
    let source = br#"
        big = {}
        for i = 1, 2000 do big[i] = i end
        for i = 1, 2000 do big[i] = nil end
        c = counter_new(0, nil, 200000)
        n = 0
        shrink = 0
        while n < 12000 do
          n = n + 1
          local g = {n, n}
          if shrink == 1 then counter_grow(c, 1000) shrink = 2 end
          if shrink == 3 then big.x = n shrink = 4 end
        end
        print(n, shrink)
    "#;
    for mode in [crate::GcMode::Incremental, crate::GcMode::Generational] {
        let config = Config {
            gc_mode: mode,
            ..Config::default()
        };
        let mut runtime = boot_lua(source, config);
        let lines = attach(&mut runtime);
        let mut journal = Journal::new();
        let mut host_shrunk = false;
        let mut minors = runtime.heap().gc.minors;
        let mut step = 0;
        loop {
            let outcome = runtime.run(1, &mut journal).unwrap();
            let shrinking = (1..=4).contains(&global_int(&runtime, b"shrink"));
            if sampled(&mut step, shrinking) {
                check_all(&runtime);
            }
            let heap = runtime.heap();
            // During an incremental sweep, or right after a young
            // collection; never inside one, where Lua does not run.
            let now = match mode {
                crate::GcMode::Incremental => heap.collector.phase == Phase::Sweep,
                crate::GcMode::Generational => {
                    heap.gc.minors > minors && heap.collector.phase == Phase::Pause
                }
            };
            minors = heap.gc.minors;
            if !now || heap.collector.holds() {
            } else if global_int(&runtime, b"shrink") == 0 {
                set_global_int(&mut runtime, b"shrink", 1);
            } else if global_int(&runtime, b"shrink") == 2 && !host_shrunk {
                let Some(Value::Userdata(handle)) = global(&runtime, b"c") else {
                    panic!("no counter");
                };
                let id = runtime.heap().userdata.get(handle).unwrap().id;
                let before = runtime.heap().gc.used;
                runtime
                    .with_userdata_mut(id, |counter: &mut crate::host::ProofCounter| {
                        counter.size = 100;
                    })
                    .unwrap();
                assert_eq!(runtime.heap().gc.used, before - 900);
                check_all(&runtime);
                host_shrunk = true;
            } else if global_int(&runtime, b"shrink") == 2 {
                set_global_int(&mut runtime, b"shrink", 3);
            }
            if !matches!(outcome, StepOutcome::Paused(_)) {
                break;
            }
        }
        assert_eq!(text(&lines), "12000\t4\n", "{mode:?}");
    }
}

/// An image with `object` (a table) made to refer to `young`.
fn point_at(image: &mut Image, object: u64, young: u64) {
    let table = image
        .tables
        .iter_mut()
        .find(|table| table.id == object)
        .unwrap();
    let slot = table
        .slots
        .iter_mut()
        .find(|slot| matches!(slot.body, SlotBody::Live { .. }))
        .unwrap();
    if let SlotBody::Live { value, .. } = &mut slot.body {
        *value = EncValue::Table(young);
    }
}

/// A table with a live slot whose mark and age are the defaults: in
/// generational form, black and old.
fn implied_old_table(image: &Image) -> Option<u64> {
    let c = &image.collector;
    image
        .tables
        .iter()
        .find(|table| {
            !c.young.contains(&table.id)
                && !c.marks.iter().any(|entry| entry.0 == table.id)
                && !c.ages.iter().any(|entry| entry.0 == table.id)
                && table
                    .slots
                    .iter()
                    .any(|slot| matches!(slot.body, SlotBody::Live { .. }))
        })
        .map(|table| table.id)
}

/// A young table nothing refers to.
fn lone_young_table(image: &Image) -> Option<u64> {
    let all = format!("{image:?}");
    image
        .collector
        .young
        .iter()
        .find(|id| {
            image.tables.iter().any(|table| table.id == **id)
                && !all.contains(&format!("Table({id})"))
        })
        .copied()
}

fn refused(runtime: &Runtime, image: &Image) -> bool {
    let bytes = crate::snapshot::encode(image).unwrap();
    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).is_err()
}

const STRESS: &[u8] = include_bytes!("../../fixtures/lua/gc_review_stress.lua");
const STRESS_OUT: &str = include_str!("../../fixtures/lua/gc_review_stress.out");
const WEAK: &[u8] = include_bytes!("../../fixtures/lua/gc_review_weak.lua");
const WEAK_OUT: &str = include_str!("../../fixtures/lua/gc_review_weak.out");

/// Restore refuses an old object made to refer to a young one nothing
/// else holds, in every phase of generational form, and a gray or touched
/// object in no list: an image that lies about what a young collection
/// traces is refused before it can free what is reachable. Each untouched
/// image restores.
#[test]
fn review_tampered_minor_and_to_old_images() {
    type Stop<'a> = &'a dyn Fn(&Runtime) -> bool;
    let after = |minors: u64| move |runtime: &Runtime| runtime.heap().gc.minors > minors;
    let minor_in = |phase: Phase| {
        move |runtime: &Runtime| {
            let heap = runtime.heap();
            heap.gc.minors > 3 && heap.collector.minor && heap.collector.phase == phase
        }
    };
    let idle = after(3);
    let begin = minor_in(Phase::Begin);
    let roots = minor_in(Phase::Atomic(crate::gc::Atomic::Roots));
    let mark_step = minor_in(Phase::Atomic(crate::gc::Atomic::Mark));
    let sweep = minor_in(Phase::Sweep);
    let touched = minor_in(Phase::Touched);
    let compact_source = compact(3000);
    let to_old = |runtime: &Runtime| {
        let heap = runtime.heap();
        global_int(runtime, b"n") > 0
            && heap.collector.to_old
            && heap.collector.phase == Phase::Sweep
            && heap.collector.sweep_at.0 >= 2
            && heap.tables.young().len() > 3
    };
    let cases: [(&str, &[u8], Stop); 7] = [
        ("between young collections", STRESS, &|runtime| {
            idle(runtime)
                && runtime.heap().collector.generational
                && runtime.heap().collector.phase == Phase::Pause
        }),
        ("young collection begins", STRESS, &begin),
        ("young collection, atomic roots", STRESS, &roots),
        ("young collection, atomic marking", STRESS, &mark_step),
        ("young collection's sweep", STRESS, &sweep),
        ("young collection's correction", STRESS, &touched),
        (
            "sweep making survivors old",
            compact_source.as_bytes(),
            &to_old,
        ),
    ];
    for (name, source, stop) in cases {
        let runtime = run_until(source, stop);
        let image = runtime.to_image().unwrap();
        assert!(!refused(&runtime, &image), "{name}: the untouched image");
        let from = implied_old_table(&image).unwrap_or_else(|| panic!("{name}: no old table"));
        // A young table nothing else refers to, or one young after the
        // sweep (not a marked survivor, which it makes old).
        let c = &image.collector;
        let stays_young = |id: &u64| {
            image.tables.iter().any(|table| table.id == *id)
                && !c
                    .marks
                    .iter()
                    .any(|entry| entry.0 == *id && entry.2 == age::SURVIVAL)
        };
        let to = lone_young_table(&image)
            .or_else(|| c.young.iter().copied().find(stays_young))
            .unwrap_or_else(|| panic!("{name}: no young table"));
        let mut changed = image.clone();
        point_at(&mut changed, from, to);
        assert!(
            refused(&runtime, &changed),
            "{name}: an old table to a young one"
        );
        // The old table also made gray and touched, in no list.
        let mut gray = changed.clone();
        gray.collector.marks.push((from, mark::GRAY, age::TOUCHED1));
        assert!(refused(&runtime, &gray), "{name}: a gray table in no list");
        // An old table counted `OLD1`, owed a trace it is not listed for.
        let mut old1 = changed;
        old1.collector.marks.push((from, mark::BLACK, age::OLD1));
        assert!(refused(&runtime, &old1), "{name}: an unlisted OLD1 table");
    }
}

/// A program with every kind of edge an old object can gain to a young
/// one, finalizers, weak tables, and both kinds of collection, for random
/// tampering.
const FUZZ: &[u8] = br#"
collectgarbage("generational", 10, 50)
local mt = {__gc = function(o) fin = (fin or 0) + 1 end}
old = {}
for i = 1, 60 do old[i] = {i} end
weak = setmetatable({}, {__mode = "v"})
eph = setmetatable({}, {__mode = "k"})
cos = {}
for i = 1, 5 do
  local co = coroutine.create(function() while true do coroutine.yield({}) end end)
  coroutine.resume(co)
  cos[i] = co
end
local up = {}
local function keep(v) up = v end
for r = 1, 3000 do
  local t = {r}
  old[r % 60 + 1][2] = t
  weak[r % 30] = {r}
  eph[old[r % 60 + 1]] = {r}
  if r % 7 == 0 then setmetatable({}, mt) end
  if r % 11 == 0 then old[r % 60 + 1] = setmetatable({r}, mt) end
  if r % 13 == 0 then keep({r}) end
  coroutine.resume(cos[r % 5 + 1], {r})
  if r == 1500 then collectgarbage() end
end
print(fin ~= nil, #old)
"#;

/// A small deterministic generator for the tampering.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One random lie about the collector's state; which, for the failure
/// message.
fn tamper(image: &mut Image, rng: &mut Rng) -> usize {
    let ids: Vec<u64> = image
        .tables
        .iter()
        .map(|table| table.id)
        .chain(image.threads.iter().map(|thread| thread.id))
        .collect();
    let any = ids[rng.below(ids.len())];
    let c = &mut image.collector;
    let ages = [
        age::NEW,
        age::SURVIVAL,
        age::OLD1,
        age::OLD,
        age::TOUCHED1,
        age::TOUCHED2,
    ];
    let marks = [mark::GRAY, mark::BLACK];
    let kind = rng.below(14);
    match kind {
        0 if !c.marks.is_empty() => {
            let at = rng.below(c.marks.len());
            c.marks[at].1 = marks[rng.below(2)];
        }
        1 if !c.marks.is_empty() => {
            let at = rng.below(c.marks.len());
            c.marks[at].2 = ages[rng.below(ages.len())];
        }
        2 => c
            .marks
            .push((any, marks[rng.below(2)], ages[rng.below(ages.len())])),
        3 if !c.young.is_empty() => {
            let at = rng.below(c.young.len());
            c.young.remove(at);
        }
        4 => c.young.push(any),
        5 if !c.revisit.is_empty() => {
            let at = rng.below(c.revisit.len());
            c.revisit.remove(at);
        }
        6 => c.revisit.push(any),
        7 if !c.again.is_empty() => {
            let at = rng.below(c.again.len());
            c.again.remove(at);
        }
        8 => c.touched.push(any),
        9 if !c.gray.is_empty() => {
            let at = rng.below(c.gray.len());
            c.gray.remove(at);
        }
        10 => {
            let fin = &mut image.finalizers;
            fin.new_from = rng.below(fin.registered.len() + 1) as u32;
            fin.old_until = fin.new_from.min(rng.below(fin.registered.len() + 1) as u32);
        }
        11 => c.unreleased = rng.next() % 4096,
        12 => {
            let at = rng.below(image.threads.len());
            let thread = &mut image.threads[at];
            thread.charged_slots = thread.charged_slots.saturating_sub(1 + rng.below(4) as u32);
            thread.charged_held = thread.charged_held.saturating_sub(rng.next() % 64);
        }
        _ => {
            let young = image.collector.young.clone();
            if !young.is_empty() {
                let to = young[rng.below(young.len())];
                let from = image.tables[rng.below(image.tables.len())].id;
                if image.tables.iter().any(|table| table.id == to)
                    && image
                        .tables
                        .iter()
                        .find(|table| table.id == from)
                        .is_some_and(|table| {
                            table
                                .slots
                                .iter()
                                .any(|slot| matches!(slot.body, SlotBody::Live { .. }))
                        })
                {
                    point_at(image, from, to);
                }
            }
        }
    }
    kind
}

/// Bounded random tampering with ages, marks, the young, remembered,
/// `again` and gray lists, the finalizer boundaries, the bytes a sweep
/// frees, and threads' charges, at points in every phase: an image
/// restore accepts never leads to a stale handle, a broken invariant, or
/// an inexact logical heap, for the next collections it runs.
#[test]
fn review_tampered_images_never_restore_into_stale_handles() {
    let mut points = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let config = Config {
        gc_min_debt: 4096,
        ..Config::default()
    };
    let mut runtime = boot_lua(FUZZ, config);
    let mut journal = Journal::new();
    loop {
        let heap = runtime.heap();
        let c = &heap.collector;
        let place = format!(
            "{:?} {} {} {} {:?}",
            c.phase, c.minor, c.to_old, c.generational, c.decide
        );
        if heap.gc.minors > 2 && seen.insert(place) {
            points.push(runtime.to_image().unwrap());
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(points.len() >= 12, "{} phases", points.len());
    let domain = runtime.effect_domain();
    // `MOONSEED_FUZZ_SEED` explores other seeds.
    let seed = crate::hostcaps::native::test_support::var("MOONSEED_FUZZ_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut rng = Rng(seed);
    let (mut accepted, mut tried) = (0, 0);
    for image in &points {
        for _ in 0..20 {
            let mut changed = image.clone();
            let mut lies = Vec::new();
            for _ in 0..1 + rng.below(2) {
                lies.push(tamper(&mut changed, &mut rng));
            }
            tried += 1;
            let Ok(bytes) = crate::snapshot::encode(&changed) else {
                continue;
            };
            let Ok(mut restored) = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
            else {
                continue;
            };
            accepted += 1;
            let mut journal = Journal::new();
            let start = restored.heap().gc.collections + restored.heap().gc.minors;
            let c = &image.collector;
            let place = (c.phase, c.atomic, c.minor, c.to_old, c.generational);
            for step in 0..20_000 {
                let heap = restored.heap();
                let checked = crate::gc::check_invariant(heap)
                    .and_then(|()| crate::gc::check_gen_invariant(heap))
                    .and_then(|()| crate::gc::check_usage(heap))
                    .and_then(|()| crate::gc::check_references(heap));
                if let Err(error) = checked {
                    let (f, g) = (&image.finalizers, &changed.finalizers);
                    panic!(
                        "lies {lies:?} at {place:?}, step {step} after restore: {error}; \
                         registered {} old {}..{} lied {}..{}; phase now {:?}",
                        f.registered.len(),
                        f.old_until,
                        f.new_from,
                        g.old_until,
                        g.new_from,
                        restored.heap().collector.phase
                    );
                }
                let heap = restored.heap();
                if heap.gc.collections + heap.gc.minors > start + 2 {
                    break;
                }
                match restored.run(1, &mut journal).unwrap() {
                    StepOutcome::Paused(_) => {}
                    _ => break,
                }
            }
        }
    }
    assert!(
        tried > 200 && accepted > 0,
        "{accepted} of {tried} accepted"
    );
}

/// A host write between the units of a young collection's sweep, to an
/// object that collection traced: the write finishes the young collection
/// first, and an object is never twice on a list it is traced from.
#[test]
fn review_host_write_during_minor_sweep() {
    for k in [0usize, 3, 11, 23] {
        let source = format!(
            r#"
            U = newud(4)
            local old = {{}}
            for j = 1, {k} do old[j] = {{}} end
            collectgarbage()
            n = 0
            while n < 20000 do
              n = n + 1
              local g = {{n}}
              for j = 1, {k} do old[j].x = {{}} end
            end
            print(n)
        "#
        );
        let mut runtime = boot_lua(source.as_bytes(), Config::default());
        let lines = attach(&mut runtime);
        let mut journal = Journal::new();
        let mut writes = 0;
        loop {
            let outcome = runtime.run(1, &mut journal).unwrap();
            let heap = runtime.heap();
            let collector = &heap.collector;
            let mut listed: Vec<_> = collector.gray.clone();
            listed.extend(
                heap.userdata
                    .again()
                    .iter()
                    .map(|&index| crate::heap::TraceRef {
                        kind: crate::id::Kind::Userdata,
                        index,
                    }),
            );
            let mut unique = listed.clone();
            unique.sort_by_key(|object| (object.kind as u8, object.index));
            unique.dedup();
            assert_eq!(unique.len(), listed.len(), "an object listed twice");
            if collector.minor
                && collector.phase == Phase::Sweep
                && let Some(Value::Userdata(handle)) = global(&runtime, b"U")
            {
                let id = runtime.heap().userdata.get(handle).unwrap().id;
                runtime
                    .with_userdata_bytes_mut(id, |bytes| bytes[0] = bytes[0].wrapping_add(1))
                    .unwrap();
                assert!(
                    !runtime.heap().collector.minor,
                    "the young collection ran on"
                );
                check_all(&runtime);
                writes += 1;
            }
            if !matches!(outcome, StepOutcome::Paused(_)) {
                break;
            }
        }
        assert_eq!(text(&lines), "20000\n");
        assert!(writes > 0, "k {k}: no write during a young sweep");
    }
}

/// A ring of tables kept and garbage, with `CALL` each round.
fn mode_calls(call: &str) -> String {
    format!(
        r#"
        ring = {{}}
        for n = 1, 40000 do
          ring[n % 3000 + 1] = {{n, n, n, n}}
          local g = {{n}}
          {call}
        end
        print(collectgarbage("generational"))
    "#
    )
}

/// `collectgarbage("generational")` with the mode already generational
/// does what Lua's does, nothing, even while a major collection runs on
/// the incremental machinery: no full collection is asked for unless
/// falling back after a bad major, where Lua's collector is incremental
/// and the call enters generational mode with one (13 full collections
/// against none, all during majors, before the review).
#[test]
fn review_generational_call_during_major_full_requests() {
    let source = mode_calls(r#"collectgarbage("generational")"#);
    let mut runtime = boot_lua(source.as_bytes(), Config::default());
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let (mut majors, mut was_major, mut was_full) = (0, false, false);
    loop {
        let falling_back = runtime.heap().gc.bad > 0;
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        let major = heap.collector.decide == Decide::Major;
        majors += u32::from(major && !was_major);
        was_major = major;
        let full = heap.gc.full.is_some();
        assert!(
            !full || was_full || falling_back,
            "a full collection during a major"
        );
        was_full = full;
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(majors > 2, "{majors} majors");
    assert_eq!(text(&lines), "generational\n");
}

/// A switch to incremental mode while a major collection runs, and back:
/// the major finishes as an incremental cycle, nothing is generational
/// until the switch back, which makes the heap generational again with a
/// full collection, as Lua's.
#[test]
fn review_incremental_call_during_major_normalizes() {
    let source = br#"
        ring = {}
        call = 0
        for n = 1, 40000 do
          ring[n % 3000 + 1] = {n, n, n, n}
          local g = {n}
          if call == 1 then
            first = collectgarbage("incremental")
            call = 2
          end
          if n == 39000 then switching = 1 second = collectgarbage("generational") end
        end
        print(first, second, collectgarbage("incremental"))
    "#;
    let mut runtime = boot_lua(source, Config::default());
    let lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut minors = None;
    let mut step = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        if sampled(
            &mut step,
            minors.is_some() && global_int(&runtime, b"switching") == 1,
        ) {
            check_all(&runtime);
        }
        let heap = runtime.heap();
        if global_int(&runtime, b"call") == 0
            && heap.collector.decide == Decide::Major
            && heap.collector.phase == Phase::Propagate
        {
            set_global_int(&mut runtime, b"call", 1);
        }
        let heap = runtime.heap();
        if global_int(&runtime, b"call") == 2 && global_int(&runtime, b"switching") != 1 {
            // A sweep making survivors old the major had begun finishes,
            // then leaves generational form.
            assert!(
                !heap.gc.generational && (!heap.collector.generational || heap.collector.to_old),
                "{:?} {} {} {:?}",
                heap.collector.phase,
                heap.collector.generational,
                heap.collector.to_old,
                heap.collector.decide
            );
            assert_ne!(heap.collector.decide, Decide::Major);
            assert_eq!(*minors.get_or_insert(heap.gc.minors), heap.gc.minors);
        }
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(minors.is_some());
    assert_eq!(text(&lines), "generational\tincremental\tgenerational\n");
}

/// The collector work young collections do, and how many there are, from
/// the point the program sets `ready` (its old objects made old by a full
/// collection) to its end.
fn young_work(source: &str) -> (u64, u64) {
    let mut runtime = boot_lua(source.as_bytes(), Config::default());
    let mut journal = Journal::new();
    let (mut work, mut minors, mut ready) = (0, 0, false);
    loop {
        let before = (
            runtime.heap().collector.minor,
            runtime.heap().gc.minors,
            runtime.heap().gc.work,
        );
        let outcome = runtime.run(1, &mut journal).unwrap();
        let gc = &runtime.heap().gc;
        ready |= global_int(&runtime, b"ready") == 1;
        if ready && (before.0 || runtime.heap().collector.minor || gc.minors != before.1) {
            work += gc.work - before.2;
            minors += gc.minors - before.1;
        }
        if !matches!(outcome, StepOutcome::Paused(_)) {
            assert_eq!(outcome, StepOutcome::Completed);
            break;
        }
    }
    (work, minors)
}

/// Young collections look at neither old finalizable objects nor old
/// threads: with 9,000 old finalizable tables, or 1,000 suspended old
/// coroutines, or 1,000 coroutines of which 10 are resumed with young
/// values, a young collection does about the work it does when those are
/// plain old tables (each did a unit per finalizable object, and walked
/// every thread's frames, before the review).
#[test]
fn review_old_finalizable_objects_and_threads_cost_young_collections_nothing() {
    // The least minor multiplier: every young collection comes after the
    // same allocation, the minimum debt, whatever the old heap's size.
    let churn = "collectgarbage('generational', 1) collectgarbage() ready = 1 \
                 for r = 1, 4000 do local g = {} for j = 1, 20 do g[j] = {j} end end";
    let cases = [
        (
            "local keep = {} for i = 1, 9000 do keep[i] = {} end",
            "local mt = {__gc = function() end} local keep = {} for i = 1, 9000 do keep[i] = setmetatable({}, mt) end",
        ),
        (
            "local keep = {} for i = 1, 1000 do keep[i] = {{}, {}} end",
            "local keep = {} for i = 1, 1000 do local co = coroutine.create(function() coroutine.yield() end) coroutine.resume(co) keep[i] = co end",
        ),
    ];
    let per = |(work, minors): (u64, u64)| {
        assert!(minors > 10, "{minors} young collections");
        work / minors
    };
    for (plain, old) in cases {
        let plain = per(young_work(&format!("{plain} {churn}")));
        let old_per = per(young_work(&format!("{old} {churn}")));
        assert!(
            old_per <= plain + plain / 10 + 20,
            "{old}: {old_per} units per young collection against {plain}"
        );
    }
    // Ten threads of a thousand resumed with young values: traced when
    // touched, the others never.
    let threads = "local cos = {} for i = 1, 1000 do local co = coroutine.create(function() while true do coroutine.yield() end end) coroutine.resume(co) cos[i] = co end";
    let quiet = per(young_work(&format!("{threads} {churn}")));
    let touched = per(young_work(&format!(
        "{threads} collectgarbage('generational', 1) collectgarbage() ready = 1 \
         for r = 1, 4000 do local g = {{}} for j = 1, 20 do g[j] = {{j}} end \
         if r % 10 == 0 then for k = 1, 10 do coroutine.resume(cos[k], {{r}}) end end end"
    )));
    assert!(touched <= quiet * 2 + 100, "{touched} against {quiet}");
}

/// At quantum 1, every step a collection runs, and every 61st other,
/// checks every invariant, the exact logical heap and stale handles; with
/// `every` > 0, a checkpoint is restored every that many steps. The
/// output is Lua 5.4.9's.
fn torture(source: &[u8], expected: &str, every: usize) {
    let mut runtime = boot_lua(source, Config::default());
    let mut lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut step = 0usize;
    let mut sample = 0;
    loop {
        if every > 0 && step % every == every - 1 {
            let output = text(&lines);
            runtime = restore(&runtime);
            lines = attach(&mut runtime);
            lines.borrow_mut().extend_from_slice(output.as_bytes());
        }
        step += 1;
        let outcome = runtime.run(1, &mut journal).unwrap();
        if sampled(&mut sample, runtime.heap().collector.phase != Phase::Pause) {
            check_all(&runtime);
        }
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?} at step {step}"),
        }
    }
    assert_eq!(text(&lines), expected);
    assert!(runtime.heap().gc.minors > 5);
}

#[test]
fn review_stress_program_keeps_every_invariant() {
    torture(STRESS, STRESS_OUT, 0);
    torture(STRESS, STRESS_OUT, 211);
}

#[test]
fn review_weak_program_keeps_every_invariant() {
    torture(WEAK, WEAK_OUT, 0);
    torture(WEAK, WEAK_OUT, 5);
}

/// Output, fuel, the collector's event hash, its work, and its counts.
fn summary(source: &[u8], quantum: u64, every: usize) -> (String, u64, u64, u64, u64, u64) {
    let mut runtime = boot_lua(source, Config::default());
    let mut lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut step = 0usize;
    loop {
        if every > 0 && step % every == every - 1 {
            let output = text(&lines);
            runtime = restore(&runtime);
            lines = attach(&mut runtime);
            lines.borrow_mut().extend_from_slice(output.as_bytes());
        }
        step += 1;
        match runtime.run(quantum, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    let gc = &runtime.heap().gc;
    let fuel = runtime.fuel_consumed();
    (
        text(&lines),
        fuel,
        gc.trace,
        gc.work,
        gc.collections,
        gc.minors,
    )
}

/// Every quantum, and checkpoints restored at any step, decide alike.
#[test]
fn review_quanta_and_checkpoints_agree() {
    // The stress program is long: fewer checkpoints.
    let weak = [(1, 0), (2, 0), (5, 0), (97, 0), (1, 11), (3, 4), (1000, 3)];
    let stress = [(1, 0), (5, 0), (97, 0), (1, 211), (7, 97), (1000, 3)];
    for (name, source, configs) in [("weak", WEAK, &weak[..]), ("stress", STRESS, &stress[..])] {
        let straight = summary(source, u64::MAX, 0);
        for &(quantum, every) in configs {
            assert_eq!(
                summary(source, quantum, every),
                straight,
                "{name} quantum {quantum} every {every}"
            );
        }
    }
}

/// From the second review: `gsub` with a function replacement grows its
/// result out of its frame, then allocates a capture; near the quota that
/// allocation runs an emergency collection, which traces the thread. The
/// thread keeps what it was charged for the work out of its frame, so the
/// logical heap stays exact and every checkpoint restores (before the
/// fix, it fell about 1.2 KB below the heap at three of these quotas, and
/// the checkpoint was refused).
#[test]
fn review_library_work_out_of_its_frame_stays_charged() {
    let source = br#"
        local s = string.rep("abcdefgh", 4000)
        local n = 0
        local ok, r = pcall(string.gsub, s, "(a)", function(c) n = n + 1 return c end)
        print(ok, n > 0)
    "#;
    for quota in (80_000u64..=92_000).step_by(200) {
        let config = Config {
            max_logical_heap: quota,
            ..Config::default()
        };
        let mut runtime = boot_lua(source, config);
        let mut journal = Journal::new();
        let mut step = 0;
        loop {
            let outcome = runtime.run(1, &mut journal).unwrap();
            step += 1;
            crate::gc::check_usage(runtime.heap())
                .unwrap_or_else(|error| panic!("quota {quota}, step {step}: {error}"));
            if step % 97 == 0 {
                restore(&runtime);
            }
            if !matches!(outcome, StepOutcome::Paused(_)) {
                break;
            }
        }
    }
}
