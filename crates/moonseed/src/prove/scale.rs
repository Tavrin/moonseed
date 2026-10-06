//! The scalability envelope (ADR 0052): every state the configured limits
//! allow runs, collects, snapshots and restores, far past the proof-era
//! constants (10,000 objects, 1 MiB strings and snapshots, 10,000 table
//! entries); a smaller host limit the state does not fit refuses the
//! snapshot before any runtime exists; hostile counts are refused before
//! they allocate.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::{GcMode, LegacyCompletion, Limits};

type Lines = Rc<RefCell<Vec<u8>>>;

fn boot(source: &str, config: Config) -> Runtime {
    let mut chunk = crate::compile(source.as_bytes()).unwrap();
    chunk.set_chunk_name(b"@scale.lua");
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
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

fn checked(runtime: &Runtime) {
    let heap = runtime.heap();
    crate::gc::check_usage(heap).unwrap();
    crate::gc::check_invariant(heap).unwrap();
    crate::gc::check_gen_invariant(heap).unwrap();
}

/// Run `source` in quanta of `quantum`, restoring from a snapshot after
/// every `every`th quantum (checking the heap there), to the end; the
/// output.
fn run_restoring(source: &str, config: Config, quantum: u64, every: usize) -> String {
    let mut runtime = boot(source, config);
    let mut lines = attach(&mut runtime);
    let mut journal = Journal::new();
    let mut step = 0;
    loop {
        step += 1;
        if step % every == 0 {
            checked(&runtime);
            let output = text(&lines);
            let bytes = runtime.snapshot().unwrap();
            runtime = Runtime::from_snapshot_with_limits(
                &bytes,
                &HostRegistry::proof(),
                runtime.effect_domain(),
                runtime.limits(),
            )
            .unwrap();
            checked(&runtime);
            lines = attach(&mut runtime);
            lines.borrow_mut().extend_from_slice(output.as_bytes());
        }
        match runtime.run(quantum, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    checked(&runtime);
    text(&lines)
}

const KEEP: &str = "
    local keep = {}
    local function make(i)
      local kind = i % 4
      if kind == 0 then return 'string ' .. i
      elseif kind == 1 then return {i}
      elseif kind == 2 then return function() return i end
      else return coroutine.create(function() return i end) end
    end
";

/// Gate A: 100,000 live objects of each kind, and mixed, under the default
/// limits, checkpointed while they are made.
#[test]
fn a_hundred_thousand_objects_of_every_kind() {
    for (name, make) in [
        ("strings", "'s' .. i"),
        ("tables", "{i}"),
        ("closures", "function() return i end"),
        ("mixed", "make(i)"),
    ] {
        let source = format!(
            "{KEEP} for i = 1, 100000 do keep[i] = {make} end \
             collectgarbage() local m = collectgarbage('count') \
             print(#keep, m > 3000)"
        );
        // Gate Q uses the same constructors at a bounded population: a full
        // image after every opcode at 100,000 objects would be quadratic.
        let small = source.replace("100000", "8");
        fast_slow_equivalent(|| boot(&small, Config::default()), pair_results);
        assert_eq!(
            run_restoring(&source, Config::default(), 250_000, 4),
            "100000\ttrue\n",
            "{name}"
        );
    }
}

/// Gate A: 100,000 objects live at once while a million short-lived ones
/// come and go under generational collection, and incremental.
#[test]
fn short_lived_objects_around_a_large_live_set() {
    let source = "
        local keep = {}
        for i = 1, 100000 do keep[i] = {i} end
        local sum = 0
        for round = 1, 1000 do
          for j = 1, 1000 do local t = {j} sum = sum + t[1] end
          keep[round] = {round}
        end
        print(#keep, sum)";
    for mode in [GcMode::Generational, GcMode::Incremental] {
        let config = Config {
            gc_mode: mode,
            ..Config::default()
        };
        assert_eq!(
            run_restoring(source, config, 2_000_000, 3),
            "100000\t500500000\n",
            "{mode:?}"
        );
    }
}

/// Gate B: strings of 2, 8 and 16 MiB are made, kept, snapshot and
/// restored; a string limit is exact (its length works, one byte more is a
/// catchable error), and past the quota is a memory error, found before
/// the bytes are made.
#[test]
fn strings_of_many_mebibytes() {
    let limit: u64 = 16 << 20;
    let source = format!(
        "local s2 = string.rep('a', 2 << 20)
         local s8 = s2:rep(4)
         local s16 = s8 .. s8
         local at = string.rep('x', {limit})
         local function why(f, ...) local ok, e = pcall(f, ...) return ok or e end
         print(#s2, #s8, #s16, #at,
               why(function() return at .. 'y' end),
               why(string.rep, 'x', {limit} + 1),
               why(function() return s16 .. s16 .. s16 .. s16 end))"
    );
    let config = Config {
        max_string_bytes: limit,
        ..Config::default()
    };
    assert_eq!(
        run_restoring(&source, config, 40, 2),
        format!(
            "{}\t{}\t{}\t{limit}\tnot enough memory\tresulting string too large\tnot enough memory\n",
            2 << 20,
            8 << 20,
            16 << 20
        )
    );
}

/// Gate D: tables of 100,000 entries, dense, hashed, mixed, weak and
/// ephemeron, traversed with `next` across checkpoints, collected in both
/// modes, and churned (a delete then an insert, many times) in time
/// linear in the churn.
#[test]
fn tables_of_a_hundred_thousand_entries() {
    let source = "
        local n = 100000
        local dense, hash, mixed = {}, {}, {}
        local weak = setmetatable({}, {__mode = 'v'})
        local eph = setmetatable({}, {__mode = 'k'})
        local held = {}
        for i = 1, n do
          dense[i] = i
          hash['k' .. i] = i
          mixed[i] = i; mixed[-i] = i
          local v = {i}
          if i % 2 == 0 then held[#held + 1] = v end
          weak[i] = v
          eph[v] = {v}
        end
        local count, sum = 0, 0
        for k, v in pairs(hash) do count = count + 1 sum = sum + v end
        for i = 1, 30000 do hash['k' .. i] = nil; hash['n' .. i] = i end
        collectgarbage()
        local w, e = 0, 0
        for _ in pairs(weak) do w = w + 1 end
        for _ in pairs(eph) do e = e + 1 end
        local h = 0
        for _ in pairs(hash) do h = h + 1 end
        print(#dense, count, sum, #mixed, w, e, h)";
    for mode in [GcMode::Generational, GcMode::Incremental] {
        let config = Config {
            gc_mode: mode,
            ..Config::default()
        };
        assert_eq!(
            run_restoring(source, config, 400_000, 4),
            "100000\t100000\t5000050000\t100000\t50000\t50000\t100000\n",
            "{mode:?}"
        );
    }
}

/// Gate G: restoring into smaller limits the state does not fit refuses
/// the snapshot, each limit on its own; equal or larger limits restore,
/// and the runtime runs under the smaller of each pair, never raised.
#[test]
fn restore_follows_the_host_limits() {
    let source = "
        local keep = {}
        for i = 1, 30000 do keep[i] = {i} end
        local big = string.rep('z', 3 << 20)
        local function deep(n) if n == 0 then coroutine.yield() return 0 end return 1 + deep(n - 1) end
        local co = coroutine.wrap(function() return deep(900) end)
        co()
        park()
        print(#keep, #big, co())";
    let config = Config {
        max_logical_heap: 16 << 20,
        max_string_bytes: 8 << 20,
        ..Config::default()
    };
    let mut runtime = boot(source, config);
    runtime.set_global_native("park", "park").unwrap();
    let mut journal = Journal::new();
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let StepOutcome::Waiting(key) = outcome else {
        panic!("expected a wait: {outcome:?} {:?}", runtime.lua_error());
    };
    let bytes = runtime.snapshot().unwrap();
    let mine = runtime.limits();
    let used = runtime.memory();
    let restore = |limits: Limits| {
        Runtime::from_snapshot_with_limits(&bytes, &HostRegistry::proof(), 1, limits)
    };
    // Each smaller limit the state does not fit.
    for (name, limits) in [
        (
            "objects",
            Limits {
                max_objects: used.objects - 1,
                ..mine
            },
        ),
        (
            "heap",
            Limits {
                max_logical_heap: used.logical_bytes - 1,
                ..mine
            },
        ),
        (
            "string",
            Limits {
                max_string_bytes: (3 << 20) - 1,
                ..mine
            },
        ),
        (
            "stack",
            Limits {
                max_stack_slots: 1_024,
                ..mine
            },
        ),
        (
            "snapshot",
            Limits {
                max_snapshot_bytes: bytes.len() as u64 - 1,
                ..mine
            },
        ),
    ] {
        assert_eq!(
            restore(limits).err(),
            Some(SnapshotError::LimitExceeded),
            "{name}"
        );
    }
    // Smaller limits the state fits, and larger ones: the smaller of each.
    let fits = Limits {
        max_objects: used.objects + 10,
        max_logical_heap: used.logical_bytes + 4096,
        max_string_bytes: 4 << 20,
        ..mine
    };
    let larger = Limits {
        max_objects: mine.max_objects * 2,
        max_logical_heap: mine.max_logical_heap * 2,
        max_string_bytes: mine.max_string_bytes * 2,
        max_stack_slots: 100_000,
        max_snapshot_bytes: mine.max_snapshot_bytes * 2,
    };
    for limits in [fits, mine, larger] {
        let mut restored = restore(limits).unwrap();
        let want = Limits {
            max_objects: mine.max_objects.min(limits.max_objects),
            max_logical_heap: mine.max_logical_heap.min(limits.max_logical_heap),
            max_string_bytes: mine.max_string_bytes.min(limits.max_string_bytes),
            max_stack_slots: mine.max_stack_slots.min(limits.max_stack_slots),
            max_snapshot_bytes: limits.max_snapshot_bytes,
        };
        assert_eq!(restored.limits(), want);
        let lines = attach(&mut restored);
        restored
            .complete_legacy(key, LegacyCompletion::Return(vec![]))
            .unwrap();
        let mut journal = Journal::new();
        assert_eq!(
            restored.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(text(&lines), format!("30000\t{}\t900\n", 3 << 20));
    }
}

/// Gate C: hostile counts in a large snapshot are refused before they
/// allocate: a count no input backs, and a count of items each legal that
/// would decode past the budget a small host heap allows.
#[test]
fn hostile_counts_are_refused_before_they_allocate() {
    let source = "keep = {} for i = 1, 50000 do keep[i] = 's' .. i end";
    let mut runtime = boot(source, Config::default());
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let bytes = runtime.snapshot().unwrap();
    let reseal = |mut forged: Vec<u8>| {
        let body = forged.len() - 4;
        let crc = crate::snapshot::crc32(&forged[..body]);
        forged[body..].copy_from_slice(&crc.to_le_bytes());
        forged
    };
    let restore = |bytes: &[u8], limits: Limits| {
        Runtime::from_snapshot_with_limits(bytes, &HostRegistry::proof(), 1, limits)
    };
    // The string count, forged past what the input holds.
    let at = crate::snapshot::STRING_COUNT_OFFSET;
    for count in [u32::MAX, crate::heap::OBJECTS_CEILING, bytes.len() as u32] {
        let mut forged = bytes.clone();
        forged[at..at + 4].copy_from_slice(&count.to_le_bytes());
        let error = restore(&reseal(forged), Limits::default()).err();
        assert!(
            matches!(
                error,
                Some(SnapshotError::LimitExceeded | SnapshotError::Truncated)
            ),
            "{count}: {error:?}"
        );
    }
    // Every count honest, but more than a small host heap holds: refused
    // by the object limit or the budget, not after decoding it all.
    let small = Limits {
        max_logical_heap: 64 << 10,
        ..Limits::default()
    };
    assert_eq!(
        restore(&bytes, small).err(),
        Some(SnapshotError::LimitExceeded)
    );
    let few = Limits {
        max_objects: 1_000,
        ..Limits::default()
    };
    assert_eq!(
        restore(&bytes, few).err(),
        Some(SnapshotError::LimitExceeded)
    );
}

/// Gate H: a stable 300k-object graph with churn. The quantum and the
/// minimum debt keep collection ends separate; recount/invariant scans
/// run there and every 61st return, rather than on every instruction.
#[test]
fn exact_accounting_with_three_hundred_thousand_objects() {
    let source = "
        keep = {}
        for i = 1, 300001 do keep[i] = {i} end
        park()
        local sum = 0
        for round = 1, 400 do
          local g = {}
          for j = 1, 500 do g[j] = {j} sum = sum + g[j][1] end
          keep[round][2] = {round}
          if round % 80 == 0 then collectgarbage('step', 0) end
          if round % 100 == 0 then collectgarbage() end
        end
        collectgarbage()
        print(#keep, sum)";
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let config = Config {
            auto_gc: false,
            gc_min_debt: 1 << 20,
            gc_mode: mode,
            ..Config::default()
        };
        let mut runtime = boot(source, config);
        runtime.set_global_native("park", "park").unwrap();
        let mut journal = Journal::new();
        let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
        else {
            panic!("build failed: {:?}", runtime.lua_error());
        };
        runtime.collect();
        checked(&runtime);
        assert!(runtime.memory().objects > 300_000);
        assert!(runtime.heap().tables.live() > 65_535);
        assert!(runtime.heap().tables.live() > 10_000);
        runtime.complete_wait(key, 0).unwrap();
        runtime.set_auto_gc(true);
        let lines = attach(&mut runtime);
        let mut step = 0;
        let mut checkpoints = 0;
        let mut ends = 0;
        let initial = runtime.memory();
        loop {
            let before = runtime.memory().collections;
            let outcome = runtime.run(1000, &mut journal).unwrap();
            step += 1;
            let ended = runtime.memory().collections - before;
            assert!(ended <= 1, "quantum hid collection ends: {ended}");
            ends += ended;
            if ended > 0 || step % 61 == 0 || outcome == StepOutcome::Completed {
                checked(&runtime);
            }
            if checkpoints < 3 && step % 61 == 0 {
                let bytes = runtime.snapshot().unwrap();
                runtime =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
                checked(&runtime);
                let sink = lines.clone();
                runtime.set_output(Box::new(move |bytes| {
                    sink.borrow_mut().extend_from_slice(bytes)
                }));
                assert!(runtime.memory().objects > 300_000);
                assert!(runtime.heap().tables.live() > 65_535);
                checkpoints += 1;
            }
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{mode:?}: {other:?} {:?}", runtime.lua_error()),
            }
        }
        assert_eq!(checkpoints, 3);
        assert!(ends > 1);
        if mode == GcMode::Generational {
            assert!(runtime.memory().young_collections > initial.young_collections);
        }
        assert_eq!(text(&lines), "300001\t50100000\n");
        eprintln!(
            "Gate H {mode:?}: {} objects, {} logical bytes, {ends} collection ends, {checkpoints} restores",
            runtime.memory().objects,
            runtime.memory().logical_bytes,
        );
    }
}

/// Gate J: checkpoints retain a near-quota graph and suspended calls,
/// and observe each builtin continuation and active young collection.
#[test]
#[ignore = "slow: milestone validation"]
fn combined_near_quota_stress() {
    use crate::heap::{Boundary, Task};
    use crate::library::Work;
    use crate::strlib::StrWork;

    let source = "
        collectgarbage('stop')
        keep = {}
        for i = 1, 140000 do keep[i] = {i} end
        big = {} for i = 1, 180000 do big[i] = i end
        strings = {} for i = 1, 4 do strings[i] = string.rep('x', 4 << 20) end
        ud = {} for i = 1, 128 do ud[i] = newud(65536, 1) end
        counter = counter_new(7, nil, 1024)
        local function deep(n, x)
          if n == 0 then return coroutine.yield(x) end
          local y = deep(n - 1, x) return y + 1
        end
        cos = {}
        for i = 1, 4000 do
          local co = coroutine.create(deep)
          assert(coroutine.resume(co, 3, i)) cos[i] = co
        end
        collectgarbage()
        collectgarbage('restart')
        park()
        local function replacement(s)
          local g = {} for j = 1, 80 do g[j] = {j} end
          keep[1][2] = g
          collectgarbage('step', 0)
          return 'z'
        end
        local replaced, count = string.gsub(string.rep('a', 256), '.', replacement)
        local sorted = {} for i = 1, 2000 do sorted[i] = 2001 - i end
        table.sort(sorted, function(a, b)
          local g = {a, b} keep[2][2] = g
          return a < b
        end)
        local function chain(n)
          if n == 0 then collectgarbage('step', 0) return 99 end
          local ok, value = pcall(chain, n - 1) assert(ok) return value + 1
        end
        local ok, value = pcall(chain, 12)
        local resumed, answer = coroutine.resume(cos[4000], 10)
        print(#keep, #big, #strings[1], udpeek(ud[1], 0), counter_get(counter),
              #cos, resumed, answer, #replaced, count, sorted[1], sorted[2000], ok, value)";

    let prepare = || {
        let mut runtime = boot(source, Config::default());
        runtime.set_global_native("park", "park").unwrap();
        let StepOutcome::Waiting(key) = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap()
        else {
            panic!("build failed: {:?}", runtime.lua_error());
        };
        checked(&runtime);
        let bytes = runtime.memory().logical_bytes;
        assert!((40 << 20..=60 << 20).contains(&bytes), "heap {bytes}");
        assert!(runtime.heap().threads.live() > 4000);
        runtime.complete_wait(key, 0).unwrap();
        runtime
    };
    let mut straight = prepare();
    let heap_bytes = straight.memory().logical_bytes;
    let lines = attach(&mut straight);
    assert_eq!(
        straight
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    let expected = (text(&lines), straight.fuel_consumed());
    assert_eq!(
        expected.0,
        "140000\t180000\t4194304\t0\t7\t4000\ttrue\t13\t256\t256\t1\t2000\ttrue\t111\n"
    );
    drop(straight);

    for quantum in [137, 4093] {
        let mut runtime = prepare();
        let lines = attach(&mut runtime);
        let mut journal = Journal::new();
        let mut seen = [0usize; 4];
        let mut checkpoints = 0;
        let mut step = 0;
        loop {
            // Small quanta initially expose the Lua callbacks and nested
            // protected frames; later use the chosen varying quantum.
            let q = if seen.contains(&0) {
                quantum % 127 + 17
            } else {
                quantum
            };
            let outcome = runtime.run(q, &mut journal).unwrap();
            step += 1;
            let heap = runtime.heap();
            let mut present = [false; 4];
            present[3] = heap.collector.minor && heap.collector.phase != crate::gc::Phase::Pause;
            if let Some(thread) = heap.active.and_then(|handle| heap.threads.get(handle)) {
                let mut protected = 0;
                for frame in &thread.frames {
                    match frame.boundary() {
                        Some(Boundary::Builtin {
                            task: Task::Lib(task),
                            ..
                        }) => match &task.work {
                            Work::Str(work)
                                if matches!(**work, StrWork::Gsub { site: Some(_), .. }) =>
                            {
                                present[0] = true
                            }
                            Work::Sort(_) => present[1] = true,
                            _ => {}
                        },
                        Some(Boundary::Protect { .. }) => protected += 1,
                        _ => {}
                    }
                }
                present[2] = protected >= 4;
            }
            let special = present
                .iter()
                .zip(seen)
                .any(|(yes, count)| *yes && count < 2);
            let periodic = step % 61 == 0 && checkpoints < 16;
            if special || periodic {
                checked(&runtime);
                let bytes = runtime.snapshot().unwrap();
                runtime =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
                checked(&runtime);
                let sink = lines.clone();
                runtime.set_output(Box::new(move |bytes| {
                    sink.borrow_mut().extend_from_slice(bytes)
                }));
                for (count, yes) in seen.iter_mut().zip(present) {
                    *count += usize::from(yes);
                }
                checkpoints += 1;
            }
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?} {:?}", runtime.lua_error()),
            }
        }
        checked(&runtime);
        assert!(
            seen.iter().all(|count| *count >= 2),
            "continuations/young: {seen:?}"
        );
        assert!(checkpoints >= 8, "checkpoints {checkpoints}");
        assert_eq!(
            (text(&lines), runtime.fuel_consumed()),
            expected,
            "quantum {quantum}"
        );
        eprintln!(
            "Gate J: heap {heap_bytes} bytes, quantum {quantum}, {checkpoints} restores, gsub/sort/pcall/young {seen:?}, fuel {}",
            runtime.fuel_consumed(),
        );
    }
}
