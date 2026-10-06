//! Weak tables, ephemerons, finalizers, and warnings (Phase 3.27,
//! ADR 0046 to ADR 0049). A corpus gives Lua 5.4.9's output and keeps it
//! under every schedule; weak references survive a snapshot until a
//! collection decides; a finalizer waits on the host and restores exactly
//! once; finalizable garbage cannot loop the collector; closing runs the
//! finalizers in order before Rust drops host values; restore refuses
//! finalization states the runtime cannot make.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::host::{
    HostValue, LegacyCompletion, NativeCall, NativeOutcome, NativePolicy, USERDATA_NATIVES,
};

type Lines = Rc<RefCell<Vec<u8>>>;

/// Send `print` and warnings, as `[warn] text` lines, to one buffer.
fn attach(runtime: &mut Runtime, lines: &Lines) {
    let sink = lines.clone();
    runtime.set_output(Box::new(move |bytes| {
        sink.borrow_mut().extend_from_slice(bytes)
    }));
    let sink = lines.clone();
    let piece = Rc::new(RefCell::new(Vec::new()));
    runtime.set_warnings(Box::new(move |bytes, more| {
        piece.borrow_mut().extend_from_slice(bytes);
        if !more {
            let mut out = sink.borrow_mut();
            out.extend_from_slice(b"[warn] ");
            out.append(&mut piece.borrow_mut());
            out.push(b'\n');
        }
    }));
}

fn boot_gc(registry: HostRegistry, spec: &crate::program::ProtoSpec, config: Config) -> Runtime {
    let mut runtime = Runtime::boot(config, registry, spec, false).unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    for (name, _) in USERDATA_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
    runtime.set_global_native("park", "park").unwrap();
    runtime
}

fn corpus() -> crate::program::ProtoSpec {
    let mut chunk = crate::compile(&fixture("corpus_gc.lua")).unwrap();
    chunk.set_chunk_name(b"@corpus_gc.lua");
    chunk.proto
}

fn text(lines: &Lines) -> String {
    String::from_utf8_lossy(&lines.borrow()).into_owned()
}

/// Run to the end, then close as `lua.c` does at exit.
fn run_and_close(runtime: &mut Runtime, journal: &mut Journal) {
    let outcome = runtime.run_until_terminal(u64::MAX, journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    runtime.begin_close().unwrap();
    let outcome = runtime.run_until_terminal(u64::MAX, journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
}

fn printed(source: &str) -> String {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    let lines = Lines::default();
    attach(&mut runtime, &lines);
    run_and_close(&mut runtime, &mut Journal::new());
    text(&lines)
}

/// The corpus, closed at the end, writes what Lua 5.4.9 wrote for it.
#[test]
fn gc_corpus_matches_lua() {
    let mut runtime = boot_gc(HostRegistry::proof(), &corpus(), Config::default());
    let lines = Lines::default();
    attach(&mut runtime, &lines);
    run_and_close(&mut runtime, &mut Journal::new());
    let expected = String::from_utf8(fixture("corpus_gc.out")).unwrap();
    let output = text(&lines);
    for (index, (a, b)) in output.lines().zip(expected.lines()).enumerate() {
        assert_eq!(a, b, "line {}", index + 1);
    }
    assert_eq!(output.lines().count(), expected.lines().count());
}

/// `MOONSEED_LUA54_UD` names Lua 5.4.9 built with
/// `tools/lua54_userdata_harness.c`, which prints warnings as the tests do.
#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_the_gc_corpus() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54_UD")
        .expect("MOONSEED_LUA54_UD must point at the userdata harness");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let output = lua_command(&lua)
        .current_dir(&root)
        .arg("corpus_gc.lua")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(fixture("corpus_gc.out")).unwrap()
    );
}

/// The corpus writes the same, warnings included, with the same fuel,
/// under small quanta and with a checkpoint and restore at every step:
/// finalizer frames, queues, registrations, and weak tables all restore.
#[test]
fn gc_corpus_keeps_its_output_under_every_schedule() {
    let spec = corpus();
    let (expected, fuel) = {
        let mut runtime = boot_gc(HostRegistry::proof(), &spec, Config::default());
        let lines = Lines::default();
        attach(&mut runtime, &lines);
        run_and_close(&mut runtime, &mut Journal::new());
        (text(&lines), runtime.fuel_consumed())
    };
    for quantum in [1u64, 3, 7] {
        let mut runtime = boot_gc(HostRegistry::proof(), &spec, Config::default());
        let lines = Lines::default();
        attach(&mut runtime, &lines);
        let mut journal = Journal::new();
        while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
        runtime.begin_close().unwrap();
        while let StepOutcome::Paused(_) = runtime.run(quantum, &mut journal).unwrap() {}
        assert_eq!(text(&lines), expected, "quantum {quantum}");
        assert_eq!(runtime.fuel_consumed(), fuel, "quantum {quantum}");
    }
    let lines = Lines::default();
    let mut runtime = boot_gc(HostRegistry::proof(), &spec, Config::default());
    attach(&mut runtime, &lines);
    let mut journal = Journal::new();
    let mut closed = false;
    loop {
        runtime = super::base::restore(&runtime);
        attach(&mut runtime, &lines);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed if !closed => {
                closed = true;
                runtime.begin_close().unwrap();
            }
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(text(&lines), expected);
    assert_eq!(runtime.fuel_consumed(), fuel);
}

/// A snapshot keeps what weak tables hold: an object only a weak table
/// reaches is there after restore until a collection clears it, and the
/// restore adds no root that would keep it.
#[test]
fn weak_references_survive_a_snapshot_until_a_collection() {
    let chunk = crate::compile(
        b"collectgarbage('stop') \
          local w = setmetatable({}, {__mode = 'v'}) \
          local k = setmetatable({}, {__mode = 'k'}) \
          local function fill() w[1] = {} k[{}] = 'v' end fill() \
          park() \
          local before = (w[1] ~= nil) and next(k) ~= nil \
          collectgarbage() \
          return before, w[1] == nil, next(k) == nil",
    )
    .unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    let StepOutcome::Waiting(key) = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap()
    else {
        panic!("no wait");
    };
    let mut restored = super::base::restore(&runtime);
    restored
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    restored
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(
        restored.results().unwrap(),
        vec![HostValue::Boolean(true); 3]
    );
}

/// A finalizer may wait on the host. Checkpointed while it waits, the run
/// resumes the finalizer, finishes it once, and goes on; nothing else ran
/// in between, and its warning is sent once.
#[test]
fn a_finalizer_waits_on_the_host_and_restores_once() {
    let chunk = crate::compile(
        b"collectgarbage('stop') local log = {} \
          local function make() setmetatable({}, {__gc = function() \
            log[#log + 1] = 'before' local x = park() log[#log + 1] = 'got ' .. x \
            error('after wait', 0) end}) end \
          make() log[#log + 1] = 'made' \
          collectgarbage() log[#log + 1] = 'collected' \
          return table.concat(log, ',')",
    )
    .unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    let lines = Lines::default();
    attach(&mut runtime, &lines);
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    for _ in 0..2 {
        let mut restored = super::base::restore(&runtime);
        let lines = Lines::default();
        attach(&mut restored, &lines);
        let mut replay = journal.clone();
        restored
            .complete_legacy(key, LegacyCompletion::Return(vec![HostValue::Integer(5)]))
            .unwrap();
        assert!(matches!(
            restored.run_until_terminal(u64::MAX, &mut replay).unwrap(),
            StepOutcome::Completed
        ));
        assert_eq!(
            restored.results().unwrap(),
            vec![HostValue::String(b"made,before,got 5,collected".to_vec())]
        );
        assert_eq!(text(&lines), "[warn] error in __gc (after wait)\n");
    }
}

/// A `__gc` that cannot be called is an error in the finalizer, so a
/// warning; the call's wording is Moonseed's (Phase 3.29).
#[test]
fn a_non_function_gc_warns() {
    let output = printed(
        "collectgarbage('stop') \
         local function make() setmetatable({}, {__gc = 42}) setmetatable({}, {__gc = false}) \
           setmetatable({}, {__gc = setmetatable({}, {__call = function(_, o) print('callable', type(o)) end})}) end \
         make() collectgarbage() print('done')",
    );
    assert_eq!(
        output,
        "callable\ttable\n\
         [warn] error in __gc (attempt to call a non-callable value)\n\
         [warn] error in __gc (attempt to call a non-callable value)\n\
         done\n"
    );
}

/// Finalizable garbage under the heap quota: an allocation the quota
/// refuses collects once and fails, so finalizers that resurrect what they
/// finalize end in a catchable memory error, and none of these programs
/// loops the collector. Objects that register themselves again are
/// finalized at every collection, as in Lua: work that grows with them,
/// bounded by fuel.
#[test]
fn finalizable_garbage_cannot_loop_the_collector() {
    let config = Config {
        max_logical_heap: 256 << 10,
        fuel_limit: Some(50_000_000),
        ..Config::default()
    };
    for (body, count) in [
        ("", 3000),
        ("keep[#keep + 1] = o", 3000),
        // Near the quota every allocation collects, and each collection
        // finalizes every such object again: kept small.
        ("setmetatable(o, getmetatable(o))", 300),
        ("error('every time')", 3000),
        ("collectgarbage()", 3000),
    ] {
        let source = format!(
            "keep = {{}} local mt = {{__gc = function(o) {body} end}} \
             local ok, err = pcall(function() for i = 1, {count} do setmetatable({{('x'):rep(100) .. i}}, mt) end end) \
             print(ok, err)"
        );
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, config.clone());
        let lines = Lines::default();
        attach(&mut runtime, &lines);
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert!(
            matches!(outcome, StepOutcome::Completed),
            "{body}: {outcome:?}"
        );
        let output = text(&lines);
        let last = output.lines().last().unwrap_or_default();
        // Resurrecting into `keep` may itself fail for memory inside the
        // finalizer, a warning, after which the object can go.
        if body.starts_with("keep") {
            assert!(
                last == "false\tnot enough memory" || last == "true\tnil",
                "{body}: {last}"
            );
        } else {
            assert_eq!(last, "true\tnil", "{body}");
        }
    }
}

/// Ten thousand finalizable objects, each finalizer erroring, and long
/// ephemeron chains, finish under the default limits.
#[test]
fn many_finalizers_and_long_ephemeron_chains_finish() {
    let output = printed(
        "collectgarbage('stop') local n = 0 \
         local mt = {__gc = function() n = n + 1 error('x', 0) end} \
         local function make() for i = 1, 3000 do setmetatable({}, mt) end end \
         make() collectgarbage() print(n) \
         local e = setmetatable({}, {__mode = 'k'}) local head = {} \
         local function chain() local k = head for i = 1, 3000 do local nk = {} e[k] = nk k = nk end end \
         chain() collectgarbage() local c = 0 for _ in pairs(e) do c = c + 1 end print(c) \
         head = nil collectgarbage() c = 0 for _ in pairs(e) do c = c + 1 end print(c)",
    );
    assert!(output.contains("\n3000\n"), "{output}");
    assert_eq!(output.matches("[warn] error in __gc (x)").count(), 3000);
    assert!(output.ends_with("3000\n0\n"), "{output}");
}

thread_local! {
    static DROPS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

/// A host value that records its Rust `Drop`.
struct Tracked(i64);

impl crate::userdata::HostUserdata for Tracked {
    const SYMBOL: &'static str = "test.Tracked";
    fn logical_size(&self) -> u64 {
        8
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        DROPS.with(|drops| drops.borrow_mut().push(self.0));
    }
}

fn tracked_new(call: &mut NativeCall<'_>) -> NativeOutcome {
    let n = call.integer(0).unwrap_or(0);
    let metatable = call.arg(1);
    let Ok(value) = call.new_host_userdata(Tracked(n), 0) else {
        return NativeOutcome::Fault;
    };
    call.set_metatable(value, Some(metatable));
    call.push(value);
    NativeOutcome::Ready
}

fn tracked_get(call: &mut NativeCall<'_>) -> NativeOutcome {
    let this = call.arg(0);
    match call.userdata_ref::<Tracked>(this) {
        Some(tracked) => {
            let n = tracked.0;
            call.push_integer(n);
            NativeOutcome::Ready
        }
        None => call.type_error(0, "test.Tracked"),
    }
}

/// Closing runs every registered finalizer, newest registration first,
/// once each, its errors as warnings, ignoring registrations made while
/// closing; a host value's `__gc` reads it while it is alive, and its
/// Rust `Drop` comes after, when a collection frees it or the runtime
/// goes, never in place of `__gc`.
#[test]
fn closing_runs_finalizers_before_rust_drops() {
    DROPS.with(|drops| drops.borrow_mut().clear());
    let mut registry = HostRegistry::proof();
    registry.register_userdata::<Tracked>();
    registry.register_native("tracked_new", NativePolicy::VmLocal, tracked_new);
    registry.register_native("tracked_get", NativePolicy::VmLocal, tracked_get);
    let chunk = crate::compile(
        b"collectgarbage('stop') \
          local mt = {__gc = function(u) print('gc', tracked_get(u)) end} \
          local function make() tracked_new(1, mt) end make() \
          collectgarbage() print('first cycle') \
          collectgarbage() print('second cycle') \
          keep = tracked_new(2, mt) \
          setmetatable({}, {__gc = function(o) print('closing table') setmetatable(o, getmetatable(o)) error('late', 0) end}) \
          print('end')",
    )
    .unwrap();
    let mut runtime = Runtime::boot(Config::default(), registry, &chunk.proto, false).unwrap();
    runtime.install_standard().unwrap();
    for name in ["tracked_new", "tracked_get"] {
        runtime.set_global_native(name, name).unwrap();
    }
    let lines = Lines::default();
    attach(&mut runtime, &lines);
    let mut journal = Journal::new();
    assert!(matches!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    ));
    // The first collection runs `__gc`; the second frees the value.
    assert_eq!(DROPS.with(|drops| drops.borrow().clone()), vec![1]);
    runtime.begin_close().unwrap();
    assert!(runtime.is_closing());
    assert!(matches!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    ));
    assert_eq!(
        text(&lines),
        "gc\t1\nfirst cycle\nsecond cycle\nend\nclosing table\n[warn] error in __gc (late)\ngc\t2\n"
    );
    assert_eq!(DROPS.with(|drops| drops.borrow().clone()), vec![1]);
    drop(runtime);
    assert_eq!(DROPS.with(|drops| drops.borrow().clone()), vec![1, 2]);
}

/// A host value waiting for its finalizer is still runtime state: a
/// snapshot of it without a codec is refused, not left out.
#[test]
fn a_pending_non_portable_value_still_refuses_a_snapshot() {
    let chunk = crate::compile(
        b"collectgarbage('stop') \
          local mt = {__gc = function(h) park() end} \
          local function make() debug.setmetatable(handle_new(1), mt) end make() \
          collectgarbage()",
    )
    .unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    assert!(matches!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(_)
    ));
    assert_eq!(
        runtime.snapshot().err(),
        Some(SnapshotError::NonPortableUserdata)
    );
}

/// Restore refuses finalization states the runtime cannot make.
#[test]
fn restore_refuses_finalization_states_it_cannot_make() {
    use crate::snapshot::Image;
    let chunk = crate::compile(
        b"collectgarbage('stop') \
          keep = setmetatable({}, {__gc = function() end}) \
          local function make() setmetatable({}, {__gc = function() park() end}) end make() \
          collectgarbage()",
    )
    .unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    assert!(matches!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(_)
    ));
    super::base::restore(&runtime);
    let domain = runtime.effect_domain();
    let refused = |change: &dyn Fn(&mut Image), expected| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain),
            expected,
        );
    };
    let registered = runtime.to_image().unwrap().finalizers.registered[0];
    let string = runtime.to_image().unwrap().strings[0].0;
    refused(
        &|image| image.finalizers.pending.push(1 << 40),
        SnapshotError::DanglingReference,
    );
    refused(
        &|image| image.finalizers.pending.push(string),
        SnapshotError::InvalidStructure,
    );
    refused(
        &|image| image.finalizers.pending.push(registered),
        SnapshotError::InvalidStructure,
    );
    refused(
        &|image| image.finalizers.registered.push(registered),
        SnapshotError::InvalidStructure,
    );
    refused(
        &|image| image.finalizers.running = false,
        SnapshotError::InvalidStructure,
    );
    refused(
        &|image| image.finalizers.closing = true,
        SnapshotError::InvalidStructure,
    );
    refused(
        &|image| image.finalizers.closed = 4,
        SnapshotError::InvalidStructure,
    );
}

/// Warnings are effects: a replayed effect id sends nothing again.
#[test]
fn a_replayed_warning_is_not_sent_twice() {
    let chunk = crate::compile(b"warn('a', 'b') park() warn('c')").unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    let lines = Lines::default();
    attach(&mut runtime, &lines);
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    let checkpoint = runtime.snapshot().unwrap();
    runtime
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(text(&lines), "[warn] ab\n[warn] c\n");
    // Restored from before `warn('c')` with the finished journal: the
    // effect is committed, so nothing is sent.
    let mut restored =
        Runtime::from_snapshot(&checkpoint, &HostRegistry::proof(), runtime.effect_domain())
            .unwrap();
    let again = Lines::default();
    attach(&mut restored, &again);
    restored
        .complete_legacy(key, LegacyCompletion::Return(vec![]))
        .unwrap();
    restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(text(&again), "");
}

/// From the milestone review. `collectgarbage` returns once the
/// finalizers its own collection queued have run, as Lua's does, even
/// when they make garbage and register their object again; and such a
/// finalizer cannot keep the program from going on, since the code it
/// interrupted gets the smallest debt of its own before the next
/// automatic collection.
#[test]
fn finalizers_that_come_back_cannot_starve_the_program() {
    let output = printed(
        "local mt local n = 0 \
         mt = {__gc = function(o) n = n + 1 setmetatable(o, mt) \
           local junk = {} for i = 1, 3000 do junk[i] = {i} end end} \
         local function make() setmetatable({}, mt) end make() \
         collectgarbage() print('returned after', n) \
         local steps = 0 \
         for i = 1, 200 do local t = {} for j = 1, 100 do t[j] = {} end steps = steps + 1 end \
         print('main progressed', steps, n > 1)",
    );
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines[0], "returned after\t1");
    assert_eq!(lines[1], "main progressed\t200\ttrue");
}

/// From the milestone review: a finalizer may make more garbage than the
/// object limit allows at once; the allocations it needs collect, as
/// Lua's emergency collections run inside a finalizer.
#[test]
fn a_finalizer_may_make_garbage_past_the_object_limit() {
    let output = printed(
        "local function make() setmetatable({}, {__gc = function() \
           local n = 0 for i = 1, 30000 do local t = {i} n = n + #t end print('fin made', n) end}) end \
         make() collectgarbage() print('after')",
    );
    assert_eq!(output, "fin made\t30000\nafter\n");
}

/// From the milestone review: closing near the object limit makes one
/// closure before it starts, so it runs every finalizer.
#[test]
fn closing_near_the_object_limit_runs_every_finalizer() {
    // The printer is registered first, so it runs last.
    let output = printed(
        "printer = setmetatable({}, {__gc = function() print('last', count == made and 'all' or count, made > 9000) end}) \
         local mt = {__gc = function() count = (count or 0) + 1 end} \
         keep = {} made = 0 \
         pcall(function() for i = 1, 20000 do keep[i] = setmetatable({}, mt) made = i end end)",
    );
    assert_eq!(output, "last\tall\ttrue\n");
}

/// From the milestone review: a snapshot taken while `collectgarbage`,
/// called with extra arguments, waits for a finalizer, and one taken while
/// a failed run closes, restore; restored at every step, both write what
/// a straight run writes.
#[test]
fn snapshots_restore_inside_collect_and_while_closing_a_failed_run() {
    for source in [
        "collectgarbage('stop') \
         local function make() setmetatable({}, {__gc = function() print('fin') end}) end make() \
         print(collectgarbage('collect', 0, 'extra')) print('end')",
        "collectgarbage('stop') \
         A = setmetatable({}, {__gc = function() print('close A', collectgarbage()) \
           setmetatable({}, {__gc = function() print('never') end}) end}) \
         B = setmetatable({}, {__gc = function() error('close err') end}) \
         error('main err')",
    ] {
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let straight = {
            let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
            let lines = Lines::default();
            attach(&mut runtime, &lines);
            let mut journal = Journal::new();
            let end = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
            runtime.begin_close().unwrap();
            assert_eq!(
                runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                end
            );
            text(&lines)
        };
        let lines = Lines::default();
        let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
        let mut journal = Journal::new();
        let mut closed = false;
        loop {
            runtime = super::base::restore(&runtime);
            attach(&mut runtime, &lines);
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed | StepOutcome::LuaError(_) if !closed => {
                    closed = true;
                    runtime.begin_close().unwrap();
                }
                StepOutcome::Completed | StepOutcome::LuaError(_) => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(text(&lines), straight, "{source}");
    }
}

/// From the milestone review: a finalizer frame at the bottom of a thread
/// belongs only to the entry thread while the runtime closes.
#[test]
fn restore_refuses_a_bottom_finalizer_frame_outside_closing() {
    use crate::snapshot::Image;
    let chunk = crate::compile(
        b"collectgarbage('stop') A = setmetatable({}, {__gc = function() park() end})",
    )
    .unwrap();
    let mut runtime = boot_gc(HostRegistry::proof(), &chunk.proto, Config::default());
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    runtime.begin_close().unwrap();
    assert!(matches!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(_)
    ));
    super::base::restore(&runtime);
    let domain = runtime.effect_domain();
    for change in [
        (|image: &mut Image| image.finalizers.closing = false) as fn(&mut Image),
        |image| image.finalizers.closed = 0,
        |image| image.finalizers.close_closure = 0,
    ] {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        expect_snapshot(
            Runtime::from_snapshot(
                &snapshot::encode(&image).unwrap(),
                &HostRegistry::proof(),
                domain,
            ),
            SnapshotError::InvalidStructure,
        );
    }
}
