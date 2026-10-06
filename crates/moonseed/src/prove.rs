use crate::id::{LuaFault, ObjectId, RootError};
use crate::snapshot::{self, STRING_COUNT_OFFSET};
use crate::{
    Config, EffectId, HostRegistry, Journal, PauseReason, Runtime, SnapshotError, StepOutcome,
    TerminationReason, VmError, WaitError, WaitKey,
};

mod base;
mod close;
mod coroutine;
mod debug;
mod embed;
mod embed_misuse;
mod errors;
mod exit;
mod finalize;
mod generational;
mod generic_for;
mod goto;
mod hooks;
mod host_checkpoint;
mod host_environment;
mod incremental;
mod language;
mod library;
mod memory;
mod operators;
mod os;
mod review;
mod scale;
mod string;
mod syntax;
mod tail_calls;
mod userdata;
mod utf8;
mod varargs;

/// The Lua oracle under a 1 GB address-space cap and 20 s of CPU time, so
/// a fixture that loops or allocates without end in PUC Lua fails the test
/// instead of taking the machine with it.
fn lua_command(lua: &str) -> crate::hostcaps::native::test_support::Command {
    lua_command_within(lua, 20)
}

/// The Lua oracle with a CPU limit of its own, for the few corpora that
/// take PUC Lua itself seconds: `corpus_table.lua` takes 10 s on a quiet
/// machine and has taken 70 s on a loaded one.
fn lua_command_within(lua: &str, seconds: u32) -> crate::hostcaps::native::test_support::Command {
    let mut command = crate::hostcaps::native::test_support::Command::new("sh");
    command
        .arg("-c")
        .arg(format!(
            "ulimit -v 1000000 && ulimit -t {seconds} && exec timeout 60 \"$0\" \"$@\""
        ))
        .arg(lua);
    command
}

fn expect_snapshot(result: Result<Runtime, SnapshotError>, expected: SnapshotError) {
    match result {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected {expected:?}"),
    }
}

fn canonical() -> (Runtime, Journal) {
    let runtime = Runtime::boot_canonical(Config::default(), HostRegistry::proof()).expect("boot");
    (runtime, Journal::new())
}

#[test]
fn uninterrupted_canonical_program() {
    let (mut runtime, mut journal) = canonical();
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut journal)
        .expect("run");
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    let observation = runtime.observe(&journal).expect("observe");
    observation.assert_canonical_shape().expect("shape");
}

#[test]
fn fuel_slices_match_uninterrupted() {
    let (mut reference, mut reference_journal) = canonical();
    reference
        .run_until_terminal(u64::MAX, &mut reference_journal)
        .unwrap();
    let expected = reference.observe(&reference_journal).unwrap();
    for quantum in [1u64, 2, 3, 7] {
        let (mut runtime, mut journal) = canonical();
        let mut pauses = 0u32;
        let outcome = loop {
            match runtime.run(quantum, &mut journal).unwrap() {
                StepOutcome::Paused(PauseReason::FuelExhausted) => pauses += 1,
                other => break other,
            }
        };
        assert!(
            matches!(outcome, StepOutcome::Completed),
            "q={quantum} {outcome:?}"
        );
        let observation = runtime.observe(&journal).unwrap();
        assert_eq!(observation, expected, "quantum {quantum}");
        if quantum == 1 {
            assert!(pauses > 0);
        }
    }
}

#[test]
fn quantum_zero_does_not_move() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let (mut runtime, mut journal) = canonical();
            let outcome = runtime.run(0, &mut journal).unwrap();
            assert_eq!(outcome, StepOutcome::Paused(PauseReason::FuelExhausted));
            assert_eq!(runtime.fuel_consumed(), 0);
            let again = runtime.run(0, &mut journal).unwrap();
            assert_eq!(again, StepOutcome::Paused(PauseReason::FuelExhausted));
            assert_eq!(runtime.fuel_consumed(), 0);
            assert!(journal.entries().is_empty());
        });
    }
}

fn reference_observation() -> crate::Observation {
    let (mut runtime, mut journal) = canonical();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    runtime.observe(&journal).unwrap()
}

fn finish_like_reference(runtime: &Runtime, journal: &Journal, expected: &crate::Observation) {
    let mut restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    let mut journal = journal.clone();
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(restored.observe(&journal).unwrap(), *expected);
}

#[test]
fn checkpoint_every_safe_point_matches() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let expected = reference_observation();
            let (mut runtime, mut journal) = canonical();
            let mut steps = 0u32;
            loop {
                finish_like_reference(&runtime, &journal, &expected);
                if runtime.at_prepared().is_some() {
                    let fuel = runtime.fuel_consumed();
                    runtime.commit_prepared(&mut journal).unwrap();
                    assert_eq!(
                        runtime.fuel_consumed(),
                        fuel,
                        "resuming a host call charged fuel again"
                    );
                } else {
                    match runtime.run(1, &mut journal).unwrap() {
                        StepOutcome::Paused(PauseReason::FuelExhausted) => {}
                        StepOutcome::Completed => {
                            finish_like_reference(&runtime, &journal, &expected);
                            break;
                        }
                        other => panic!("canonical schedule produced {other:?}"),
                    }
                }
                steps += 1;
                assert!(steps < 10_000, "safe-point walker made no progress");
            }
            assert!(steps > 10, "walker stopped after only {steps} steps");
        });
    }
}

#[test]
fn collection_at_safe_points_preserves_observation() {
    let expected = reference_observation();
    let (mut runtime, mut journal) = canonical();
    loop {
        runtime.collect();
        finish_like_reference(&runtime, &journal, &expected);
        if runtime.at_prepared().is_some() {
            runtime.commit_prepared(&mut journal).unwrap();
            continue;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => {
                runtime.collect();
                finish_like_reference(&runtime, &journal, &expected);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn restore_preserves_shared_upvalue_and_cycle() {
    let (mut runtime, mut journal) = canonical();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let bytes = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    let observation = restored.observe(&journal).unwrap();
    observation.assert_canonical_shape().unwrap();
    assert_eq!(observation.inc_upvalue, observation.get_upvalue);
    assert_eq!(observation.a_b, observation.b_id);
    assert_eq!(observation.b_a, observation.a_id);
    let inc = ObjectId(observation.inc_id);
    let get = ObjectId(observation.get_id);
    assert_eq!(restored.call_closure(inc, &mut journal).unwrap(), 2);
    assert_eq!(restored.call_closure(get, &mut journal).unwrap(), 2);
    let yielder = ObjectId(observation.yielder_id);
    let outcome = restored
        .resume_thread(yielder, u64::MAX, &mut journal)
        .unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(restored.thread_integers(yielder).unwrap(), vec![43]);
}

#[test]
fn negative_observation_comparator_rejects_a_flipped_integer() {
    let expected = reference_observation();
    let mut flipped = expected.clone();
    flipped.tag = 8;
    assert_ne!(flipped, expected);
    assert!(flipped.assert_canonical_shape().is_err());
}

#[test]
fn mark_commits_once_across_prepared_and_torn_journal() {
    let (mut runtime, mut journal) = canonical();
    loop {
        if let Some(sequence) = runtime.at_prepared() {
            let bytes = runtime.snapshot().unwrap();
            let domain = runtime.effect_domain();
            let mut fresh = Journal::new();
            let mut restored =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
            restored.run_until_terminal(u64::MAX, &mut fresh).unwrap();
            assert_eq!(fresh.entries().len(), 1);
            assert_eq!(fresh.entries()[0].id.sequence, sequence);
            assert_eq!(fresh.entries()[0].outcome, sequence as i64);

            let mut torn = Journal::new();
            torn.seed(EffectId { domain, sequence }, 1, 99);
            let mut restored =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
            restored.run_until_terminal(u64::MAX, &mut torn).unwrap();
            assert_eq!(torn.entries().len(), 1);
            assert_eq!(torn.entries()[0].outcome, 99);
            assert_eq!(restored.observe(&torn).unwrap().mark, 99);

            let fuel = runtime.fuel_consumed();
            runtime.commit_prepared(&mut journal).unwrap();
            assert_eq!(runtime.fuel_consumed(), fuel);
            let bytes = runtime.snapshot().unwrap();
            let mut empty = Journal::new();
            let mut restored =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
            restored.run_until_terminal(u64::MAX, &mut empty).unwrap();
            assert!(
                empty.entries().is_empty(),
                "committed call was replayed onto an empty journal"
            );
            assert_eq!(restored.observe(&empty).unwrap().mark, sequence as i64);
            return;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            other => panic!("left the host call with {other:?}"),
        }
    }
}

#[test]
fn effect_domain_mismatch_rejects_without_touching_the_source() {
    let (mut runtime, mut journal) = canonical();
    runtime.run(4, &mut journal).unwrap();
    let bytes = runtime.snapshot().unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain() + 1),
        SnapshotError::EffectDomainMismatch,
    );
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed));
    runtime
        .observe(&journal)
        .unwrap()
        .assert_canonical_shape()
        .unwrap();
}

#[test]
fn waiting_survives_snapshot_and_is_not_a_fuel_pause() {
    let mut runtime = Runtime::boot_park(Config::default(), HostRegistry::proof()).unwrap();
    let mut journal = Journal::new();
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(outcome, StepOutcome::Waiting(WaitKey(1)));
    assert!(journal.entries().is_empty());
    assert_eq!(
        runtime.run(0, &mut journal).unwrap(),
        StepOutcome::Waiting(WaitKey(1))
    );
    assert_eq!(
        runtime.run(5, &mut journal).unwrap(),
        StepOutcome::Waiting(WaitKey(1))
    );
    assert_eq!(
        runtime.complete_wait(WaitKey(9), 1),
        Err(WaitError::UnknownKey)
    );

    let bytes = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    restored.complete_wait(WaitKey(1), 7).unwrap();
    assert_eq!(
        restored.complete_wait(WaitKey(1), 7),
        Err(WaitError::AlreadyCompleted)
    );
    let outcome = restored.run_until_terminal(1, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(restored.global_integer("result").unwrap(), 7);

    let mut direct = Runtime::boot_park(Config::default(), HostRegistry::proof()).unwrap();
    assert_eq!(
        direct
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(WaitKey(1))
    );
    direct.complete_wait(WaitKey(1), 7).unwrap();
    direct
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(direct.global_integer("result").unwrap(), 7);
}

#[test]
fn complete_wait_on_a_running_thread_is_not_waiting() {
    let (mut runtime, _) = canonical();
    assert_eq!(
        runtime.complete_wait(WaitKey(1), 1),
        Err(WaitError::NotWaiting)
    );
}

#[test]
fn host_yield_is_distinct_from_fuel_pause() {
    let mut runtime = Runtime::boot_yield(Config::default(), HostRegistry::proof()).unwrap();
    let id = runtime.entry_id().unwrap();
    let mut journal = Journal::new();
    let outcome = runtime.resume_thread(id, u64::MAX, &mut journal).unwrap();
    assert_eq!(outcome, StepOutcome::LuaYielded);
    assert_eq!(runtime.thread_integers(id).unwrap(), vec![42]);
    let outcome = runtime.resume_thread(id, 1, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(runtime.thread_integers(id).unwrap(), vec![43]);
    assert!(journal.entries().is_empty());
}

#[test]
fn roots_are_runtime_local_and_strong() {
    let (mut runtime, mut journal) = canonical();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let root = runtime.root_id(runtime.entry_id().unwrap()).unwrap();
    runtime.collect();
    assert!(runtime.root_alive(&root).unwrap());
    let bytes = runtime.snapshot().unwrap();
    let restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    assert_eq!(restored.root_alive(&root), Err(RootError::ForeignRuntime));
    let (other, _) = canonical();
    assert_eq!(other.root_alive(&root), Err(RootError::ForeignRuntime));
    runtime.release_root(root).unwrap();
    assert_eq!(runtime.root_alive(&root), Err(RootError::Released));
}

#[test]
fn malformed_snapshots_name_the_failing_check() {
    let (mut runtime, mut journal) = canonical();
    runtime.run(3, &mut journal).unwrap();
    let bytes = runtime.snapshot().unwrap();
    let domain = runtime.effect_domain();
    let registry = HostRegistry::proof();

    expect_snapshot(
        Runtime::from_snapshot(b"XXXX", &registry, domain),
        SnapshotError::BadMagic,
    );
    expect_snapshot(
        Runtime::from_snapshot(&bytes[..4], &registry, domain),
        SnapshotError::Truncated,
    );
    expect_snapshot(
        Runtime::from_snapshot(&bytes[..6], &registry, domain),
        SnapshotError::Truncated,
    );

    let mut bad_version = bytes.clone();
    bad_version[4] = 99;
    bad_version[5] = 0;
    expect_snapshot(
        Runtime::from_snapshot(&bad_version, &registry, domain),
        SnapshotError::BadVersion,
    );

    let mut bad_crc = bytes.clone();
    bad_crc[40] ^= 0x5a;
    expect_snapshot(
        Runtime::from_snapshot(&bad_crc, &registry, domain),
        SnapshotError::Checksum,
    );

    let mut image = runtime.to_image().unwrap();
    image.strings.push(image.strings[0].clone());
    let dup = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&dup, &registry, domain),
        SnapshotError::DuplicateObjectId,
    );

    let mut image = runtime.to_image().unwrap();
    image.globals = 999_999;
    let dangling = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&dangling, &registry, domain),
        SnapshotError::DanglingReference,
    );

    let mut image = runtime.to_image().unwrap();
    image.threads[0].frames[0].pc = 1_000_000;
    let bad_pc = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bad_pc, &registry, domain),
        SnapshotError::InvalidProgramCounter,
    );

    let mut image = runtime.to_image().unwrap();
    let mut symbols = Vec::new();
    for proto in &image.protos {
        for op in &proto.ops {
            if let crate::opcode::Op::CallHost { symbol, .. } = *op {
                symbols.push(proto.const_ids[symbol as usize]);
            }
        }
    }
    for (id, bytes) in &mut image.strings {
        if symbols.contains(id) {
            *bytes = b"missing".to_vec();
        }
    }
    let unknown = snapshot::encode(&image).unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&unknown, &registry, domain),
        SnapshotError::UnknownHostSymbol,
    );

    let mut huge = bytes.clone();
    huge[STRING_COUNT_OFFSET..STRING_COUNT_OFFSET + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let body = huge.len() - 4;
    let crc = snapshot::crc32(&huge[..body]);
    huge[body..].copy_from_slice(&crc.to_le_bytes());
    expect_snapshot(
        Runtime::from_snapshot(&huge, &registry, domain),
        SnapshotError::LimitExceeded,
    );

    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
}

#[test]
fn memory_limit_trips_during_boot() {
    let config = Config {
        max_objects: 2,
        ..Config::default()
    };
    match Runtime::boot_canonical(config, HostRegistry::proof()) {
        Err(VmError::MemoryLimit) => {}
        Err(other) => panic!("expected memory limit, got {other:?}"),
        Ok(_) => panic!("boot succeeded under a tiny object cap"),
    }
}

#[test]
fn hard_fuel_limit_is_terminal() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let config = Config {
                fuel_limit: Some(8),
                ..Config::default()
            };
            let mut runtime = Runtime::boot_spin(config, HostRegistry::proof()).unwrap();
            let mut journal = Journal::new();
            let outcome = runtime.run_until_terminal(3, &mut journal).unwrap();
            assert_eq!(
                outcome,
                StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
            );
            assert_eq!(runtime.fuel_consumed(), 8);
            let again = runtime.run(10, &mut journal).unwrap();
            assert_eq!(
                again,
                StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
            );
            assert_eq!(runtime.fuel_consumed(), 8);
        });
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Observed {
    Nil,
    Bool(bool),
    Float(u64),
    Str(Vec<u8>),
    Int(i64),
    Closure(u64),
    Table(u64),
    Other,
}

fn observe_reg(runtime: &Runtime, reg: u8) -> Observed {
    match runtime.entry_slot(reg).unwrap() {
        crate::value::Value::Nil => Observed::Nil,
        crate::value::Value::Integer(value) => Observed::Int(value),
        crate::value::Value::Closure(_) => {
            Observed::Closure(runtime.slot_object_id(reg).unwrap().unwrap().raw())
        }
        crate::value::Value::Table(_) => {
            Observed::Table(runtime.slot_object_id(reg).unwrap().unwrap().raw())
        }
        _ => Observed::Other,
    }
}

fn result_image(runtime: &Runtime) -> Vec<Observed> {
    let mut regs: Vec<u8> = (0..=16).collect();
    regs.push(20);
    regs.push(21);
    regs.into_iter()
        .map(|reg| observe_reg(runtime, reg))
        .collect()
}

fn result_image_error(image: &[Observed]) -> Option<&'static str> {
    let expected = [
        Observed::Int(4),
        Observed::Int(2),
        Observed::Int(3),
        Observed::Int(10),
        Observed::Nil,
        Observed::Int(30),
        Observed::Int(1),
        Observed::Int(10),
        Observed::Int(10),
        Observed::Nil,
        Observed::Int(30),
        Observed::Nil,
        Observed::Int(10),
        Observed::Nil,
        Observed::Int(7),
        Observed::Nil,
        Observed::Nil,
    ];
    if image.len() != 19 {
        return Some("width");
    }
    if image[..17] != expected {
        return Some("values");
    }
    if !matches!(image[17], Observed::Closure(_)) {
        return Some("function overwritten");
    }
    if image[18] != Observed::Nil {
        return Some("dead argument retained");
    }
    if image[4] != Observed::Nil || image[13] != Observed::Nil {
        return Some("nil hole");
    }
    None
}

fn boot_spec(spec: &crate::program::ProtoSpec) -> Runtime {
    Runtime::boot(Config::default(), HostRegistry::proof(), spec, false).unwrap()
}

fn finish_observation(
    runtime: &Runtime,
    journal: &Journal,
    observe: &impl Fn(&Runtime) -> Vec<Observed>,
    expected: &[Observed],
) {
    let mut restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    let mut journal = journal.clone();
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(observe(&restored), expected);
}

/// Gate Q: compare every published state at both single-step and batched
/// boundaries. Keep the modes alive together so this needs only one snapshot
/// per mode per boundary, rather than retaining a program-sized history.
fn fast_slow_equivalent(boot: impl Fn() -> Runtime, observe: impl Fn(&Runtime) -> Vec<Observed>) {
    use crate::runtime::HotCoreMode;
    for quantum in [1, 7] {
        let mut runs = [
            HotCoreMode::Full,
            HotCoreMode::NoFastCalls,
            HotCoreMode::Off,
        ]
        .map(|mode| {
            let mut runtime = boot();
            runtime.hot_core = mode;
            let output = base::capture(&mut runtime);
            (runtime, Journal::new(), output)
        });
        for boundary in 0..1_000_000 {
            let outcomes = runs
                .each_mut()
                .map(|(runtime, journal, _)| runtime.run(quantum, journal).expect("Gate Q run"));
            let bytes = runs[0].0.snapshot().expect("Gate Q snapshot");
            let observed = observe(&runs[0].0);
            for index in 1..runs.len() {
                let left = &runs[0].0;
                let right = &runs[index].0;
                assert_eq!(outcomes[0], outcomes[index], "q={quantum} step={boundary}");
                assert!(
                    bytes == right.snapshot().expect("Gate Q snapshot"),
                    "Gate Q snapshot q={quantum} step={boundary} mode={:?}",
                    right.hot_core
                );
                assert_eq!(left.fuel_consumed(), right.fuel_consumed());
                assert_eq!(observed, observe(right));
                assert_eq!(left.gc_log, right.gc_log, "collection fuel schedule");
                assert_eq!(
                    left.memory(),
                    right.memory(),
                    "collection fingerprint inputs"
                );
                assert_eq!(runs[0].1.entries(), runs[index].1.entries());
                assert_eq!(
                    *runs[0].2.borrow(),
                    *runs[index].2.borrow(),
                    "printed output"
                );
            }
            if !matches!(outcomes[0], StepOutcome::Paused(_)) {
                assert!(
                    matches!(
                        outcomes[0],
                        StepOutcome::Completed
                            | StepOutcome::LuaError(_)
                            | StepOutcome::Terminated(_)
                    ),
                    "unresolved host wait: {:?}",
                    outcomes[0]
                );
                break;
            }
            assert!(boundary < 999_999, "Gate Q made no terminal progress");
        }
    }
}

/// Seeded call shapes exercise the common frame builder and its exceptional
/// entries. Each boundary is restored, so the comparison includes the image's
/// stack, frames, collector state and roots as well as the returned values.
#[test]
fn random_call_shapes_match_all_tiers_after_each_checkpoint() {
    use crate::runtime::HotCoreMode;

    let mut seed = 0x333a_b1c0_5eed_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for case in 0..21 {
        let sampled_args = (next() % 7) as usize;
        let args = if case == 0 { 0 } else { sampled_args };
        let results = (next() % 7) as usize;
        let sampled_ups = (next() % 5) as usize;
        let ups = if case == 0 { 1 } else { sampled_ups };
        let chain = 1 + (next() % 3) as usize;
        let values = (0..args)
            .map(|i| {
                if i % 3 == 1 {
                    "nil".to_string()
                } else {
                    (i + 1).to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let captures = (0..ups)
            .map(|i| format!("local u{i} = {}\n", i + 1))
            .collect::<String>();
        let capture_sum = if ups == 0 {
            "0".to_string()
        } else {
            (0..ups)
                .map(|i| format!("u{i}"))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let chain_code = (0..chain)
            .map(|_| "target = setmetatable({}, {__call = target})\n")
            .collect::<String>();
        let slots = (0..results)
            .map(|i| format!("r{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let bind = if results == 0 {
            format!("open({values})")
        } else {
            format!("local {slots} = open({values})")
        };
        let mut source = format!(
            "{captures}\
             local function f(...) local function inner(...) return {capture_sum}, ... end return inner(...) end\n\
             local target = f\n{chain_code}\
             local mt = {{__add = function(a,b) return a.v + b.v end,\n\
                         __index = function(_,k) return #k end}}\n\
             local a = setmetatable({{v=3}}, mt)\n\
             local b = setmetatable({{v=4}}, mt)\n\
             local function open(...) return target(...) end\n\
             {bind}\n\
             return {slots}{comma}(a+b), a.key, select('#', open({values}))",
            comma = if results == 0 { "" } else { ", " },
        );
        if case == 18 {
            source = "local function rec(n) if n == 0 then return 1 end \
                      local r = rec(n - 1) return r + 1 end \
                      return pcall(rec, 995)"
                .to_string();
        } else if case == 19 {
            let pad = (0..90)
                .map(|i| format!("a{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            source = format!(
                "local function rec(n) local {pad} \
                 if n == 0 then return 1 end \
                 local r = rec(n - 1) return r + 1 end \
                 return pcall(rec, 9)"
            );
        } else if case == 20 {
            source = "local t = setmetatable({}, {__call = function() error('boom') end}) \
                      return t()"
                .to_string();
        }
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let config = Config {
            max_stack_slots: if case == 19 {
                1_024
            } else {
                Config::default().max_stack_slots
            },
            ..Config::default()
        };
        for quantum in [1, 7] {
            let mut runs = [
                HotCoreMode::Full,
                HotCoreMode::NoFastCalls,
                HotCoreMode::Off,
            ]
            .map(|mode| {
                let mut runtime =
                    Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false)
                        .unwrap();
                runtime.install_standard().unwrap();
                runtime.hot_core = mode;
                (runtime, Journal::new())
            });
            for step in 0..10_000 {
                let outcomes = runs
                    .each_mut()
                    .map(|(runtime, journal)| runtime.run(quantum, journal).unwrap());
                let images = runs
                    .each_ref()
                    .map(|(runtime, _)| runtime.snapshot().unwrap());
                for i in 1..3 {
                    assert_eq!(
                        outcomes[0], outcomes[i],
                        "case {case} q={quantum} step={step}"
                    );
                    assert_eq!(images[0], images[i], "case {case} q={quantum} step={step}");
                    assert_eq!(runs[0].0.fuel_consumed(), runs[i].0.fuel_consumed());
                    assert_eq!(runs[0].0.gc_log, runs[i].0.gc_log);
                    assert_eq!(runs[0].0.memory(), runs[i].0.memory());
                    assert_eq!(pair_results(&runs[0].0), pair_results(&runs[i].0));
                }
                for (runtime, _) in &mut runs {
                    let mode = runtime.hot_core;
                    *runtime = Runtime::from_snapshot(
                        &images[0],
                        &HostRegistry::proof(),
                        runtime.effect_domain(),
                    )
                    .unwrap();
                    runtime.hot_core = mode;
                }
                if !matches!(outcomes[0], StepOutcome::Paused(_)) {
                    assert!(matches!(
                        outcomes[0],
                        StepOutcome::Completed | StepOutcome::LuaError(_)
                    ));
                    if case == 20 {
                        assert!(matches!(outcomes[0], StepOutcome::LuaError(_)));
                    }
                    break;
                }
                assert!(step < 9_999, "case {case} did not finish");
            }
        }
    }
}

fn quantum_and_checkpoints(
    spec: &crate::program::ProtoSpec,
    observe: impl Fn(&Runtime) -> Vec<Observed>,
) {
    quantum_and_checkpoints_with(boot_spec, spec, observe);
}

fn quantum_and_checkpoints_with(
    boot: fn(&crate::program::ProtoSpec) -> Runtime,
    spec: &crate::program::ProtoSpec,
    observe: impl Fn(&Runtime) -> Vec<Observed>,
) {
    fast_slow_equivalent(|| boot(spec), &observe);
    let expected = {
        let mut runtime = boot(spec);
        let mut journal = Journal::new();
        let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
        observe(&runtime)
    };
    for quantum in [1u64, 2, 3, 7] {
        let mut runtime = boot(spec);
        let mut journal = Journal::new();
        let outcome = loop {
            match runtime.run(quantum, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                other => break other,
            }
        };
        assert!(
            matches!(outcome, StepOutcome::Completed),
            "q={quantum} {outcome:?}"
        );
        assert_eq!(observe(&runtime), expected, "quantum {quantum}");
    }
    let mut runtime = boot(spec);
    let mut journal = Journal::new();
    let mut steps = 0u32;
    loop {
        finish_observation(&runtime, &journal, &observe, &expected);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => {
                finish_observation(&runtime, &journal, &observe, &expected);
                break;
            }
            other => panic!("{other:?}"),
        }
        steps += 1;
        assert!(steps < 10_000, "walker made no progress");
    }
}

#[test]
fn multi_result_adjustment_matches_the_lua_fixture() {
    let spec = crate::program::results_program();
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let image = result_image(&runtime);
    assert_eq!(result_image_error(&image), None, "{image:?}");
    quantum_and_checkpoints(&spec, result_image);
}

#[test]
fn broken_result_window_is_rejected() {
    let mut runtime = boot_spec(&crate::program::results_program());
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let image = result_image(&runtime);
    assert_eq!(result_image_error(&image), None, "{image:?}");
    let mut broken = image.clone();
    broken[4] = Observed::Int(30);
    assert_ne!(broken, image);
    assert_eq!(result_image_error(&broken), Some("values"));
}

#[test]
fn parallel_assignment_uses_the_old_index_across_checkpoints() {
    let spec = crate::program::assign_program();
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    let mut saw_boundary = false;
    let mut saw_between = false;
    loop {
        match runtime.assign_remaining() {
            Some(2) => {
                saw_boundary = true;
                assert_eq!(
                    runtime.entry_slot(0).unwrap(),
                    crate::value::Value::Integer(1)
                );
                assert_eq!(runtime.entry_field_integer(1, 1).unwrap(), None);
                assert_eq!(runtime.entry_field_integer(1, 2).unwrap(), None);
                let bytes = runtime.snapshot().unwrap();
                let mut restored =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
                assert_eq!(restored.assign_remaining(), Some(2));
                assert_eq!(
                    restored.entry_slot(0).unwrap(),
                    crate::value::Value::Integer(1)
                );
                let mut restored_journal = Journal::new();
                restored
                    .run_until_terminal(u64::MAX, &mut restored_journal)
                    .unwrap();
                assert_eq!(
                    restored.entry_slot(0).unwrap(),
                    crate::value::Value::Integer(2)
                );
                assert_eq!(restored.entry_field_integer(1, 1).unwrap(), Some(99));
                assert_eq!(restored.entry_field_integer(1, 2).unwrap(), None);
            }
            Some(1) => {
                saw_between = true;
                assert_eq!(
                    runtime.entry_slot(0).unwrap(),
                    crate::value::Value::Integer(1)
                );
                assert_eq!(runtime.entry_field_integer(1, 1).unwrap(), Some(99));
                let bytes = runtime.snapshot().unwrap();
                let mut restored =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
                let mut restored_journal = Journal::new();
                restored
                    .run_until_terminal(1, &mut restored_journal)
                    .unwrap();
                assert_eq!(
                    restored.entry_slot(0).unwrap(),
                    crate::value::Value::Integer(2)
                );
                assert_eq!(restored.entry_field_integer(1, 1).unwrap(), Some(99));
                assert_eq!(restored.entry_field_integer(1, 2).unwrap(), None);
            }
            _ => {}
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(saw_boundary, "no pre-store checkpoint");
    assert!(saw_between, "no between-store checkpoint");
    assert_eq!(
        runtime.entry_slot(0).unwrap(),
        crate::value::Value::Integer(2)
    );
    assert_eq!(
        runtime.entry_slot(2).unwrap(),
        crate::value::Value::Integer(99)
    );
    assert_eq!(runtime.entry_slot(3).unwrap(), crate::value::Value::Nil);
    quantum_and_checkpoints(&spec, |runtime| {
        vec![
            observe_reg(runtime, 0),
            observe_reg(runtime, 2),
            observe_reg(runtime, 3),
        ]
    });
}

#[test]
fn aliased_assignment_stores_right_to_left() {
    let mut runtime = boot_spec(&crate::program::assign_order_program());
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(
        runtime.entry_slot(7).unwrap(),
        crate::value::Value::Integer(2)
    );
}

#[test]
fn collection_keeps_pending_assignment_keys_and_result_windows() {
    let mut runtime = boot_spec(&crate::program::assign_key_program());
    let mut journal = Journal::new();
    let mut key_id = None;
    loop {
        runtime.collect();
        if runtime.assign_remaining() == Some(1) {
            let id = runtime.assign_target_id(0).expect("pending key");
            runtime.collect();
            assert!(
                runtime.contains_id(id),
                "pending assignment key was not a root"
            );
            key_id = Some(id);
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    let key_id = key_id.expect("never reached the assignment boundary");
    runtime.collect();
    assert!(runtime.contains_id(key_id));

    let spec = crate::program::box_program();
    let expected = {
        let mut runtime = boot_spec(&spec);
        let mut journal = Journal::new();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        vec![observe_reg(&runtime, 0)]
    };
    assert!(matches!(expected[0], Observed::Table(_)));
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    loop {
        runtime.collect();
        finish_observation(
            &runtime,
            &journal,
            &|runtime| vec![observe_reg(runtime, 0)],
            &expected,
        );
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => {
                runtime.collect();
                assert_eq!(vec![observe_reg(&runtime, 0)], expected);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

fn tick(ctx: &mut crate::host::HostCtx<'_>, arg: i64) -> crate::host::HostResult {
    let _ = ctx;
    crate::host::HostResult::Ready(arg.wrapping_add(1))
}

fn run_to_end(spec: &crate::program::ProtoSpec, registry: HostRegistry) -> Runtime {
    let mut runtime = Runtime::boot(Config::default(), registry, spec, false).unwrap();
    let mut journal = Journal::new();
    let outcome = loop {
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            other => break other,
        }
    };
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    runtime
}

#[test]
fn loops_keep_their_results_at_quantum_one() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let proof = HostRegistry::proof();
            let cases = [
                ("adds", crate::program::counted_adds(20), 0u8, 20i64),
                ("branch", crate::program::branchy(20), 0, 20),
                ("scalar", crate::program::scalar_calls(12), 1, 12),
                ("nested", crate::program::nested_calls(12), 2, 12),
                ("multi", crate::program::multi_calls(8), 1, 320),
                ("upvalue", crate::program::upvalue_calls(12), 0, 12),
                ("string", crate::program::string_fields(12), 2, 12),
                ("int", crate::program::int_fields(12), 2, 12),
                ("churn", crate::program::alloc_churn(12), 1, 12),
            ];
            for (name, spec, reg, expected) in cases {
                let runtime = run_to_end(&spec, proof.clone());
                assert_eq!(
                    runtime.entry_slot(reg).unwrap(),
                    crate::value::Value::Integer(expected),
                    "{name}"
                );
            }
            let mut registry = HostRegistry::new();
            registry.register("tick", tick);
            let runtime = run_to_end(&crate::program::host_ticks(12), registry);
            assert_eq!(
                runtime.entry_slot(0).unwrap(),
                crate::value::Value::Integer(12)
            );
        });
    }
}

#[test]
fn warmed_calls_do_not_grow_stack_or_frame_storage() {
    for (spec, reg, expected) in [
        (crate::program::scalar_calls(16), 1u8, 16i64),
        (crate::program::nested_calls(16), 2, 16),
        (crate::program::multi_calls(16), 1, 640),
    ] {
        let mut runtime = boot_spec(&spec);
        let mut journal = Journal::new();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        runtime.rewind_entry();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert_eq!(runtime.stack_grows, 0, "stack {reg}");
        assert_eq!(runtime.frame_grows, 0, "frame {reg}");
        assert_eq!(
            runtime.entry_slot(reg).unwrap(),
            crate::value::Value::Integer(expected)
        );
    }
}

/// A slot the stack keeps above its logical length after a return is not a
/// root: the table only a finished callee's register held is collected, and
/// a later call that reaches the slot reads nil. Control: the same table in a
/// live slot below the length survives.
#[test]
fn stale_slot_beyond_logical_extent_is_not_a_root() {
    let pad = (1..=24)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    for mode in ["incremental", "generational"] {
        let source = format!(
            r#"
            collectgarbage("{mode}")
            local weak = setmetatable({{}}, {{__mode = "v"}})
            local function leaf() local {pad} local t = {{}} weak[1] = t end
            local function keeps() local t = {{}} weak[2] = t return t end
            local function exposes() local {pad} local t return t end
            leaf()
            local held = keeps()
            collectgarbage()
            collectgarbage()
            return weak[1] == nil, weak[2] == held, exposes() == nil
            "#
        );
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut runtime = boot_natives(&chunk.proto);
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(
            pair_results(&runtime),
            vec![Observed::Bool(true); 3],
            "{mode}"
        );
        let heap = runtime.heap();
        let thread = heap.threads.get(heap.entry.unwrap()).unwrap();
        assert!(
            thread.stack.physical_len() > 20 + thread.stack.len(),
            "{mode}: the callee's slots were retained"
        );
    }
}

/// I4: frame slots kept beyond the live depth never hold cold state, after
/// boundary frames (pcall) return through every path; `check_invariant`
/// rejects a stale slot that does.
#[test]
fn frames_beyond_depth_hold_no_cold() {
    let chunk = crate::compile(
        b"local function f(n) if n > 0 then return f(n - 1) + 1 end return 0 end \
          local s = 0 \
          for i = 1, 3 do \
            local ok, v = pcall(f, i) s = s + v \
            ok = pcall(error, 'x') \
          end \
          return s",
    )
    .unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_standard().unwrap();
    let mut journal = Journal::new();
    let mut deepest = 0;
    loop {
        let outcome = runtime.run(1, &mut journal).unwrap();
        let heap = runtime.heap();
        let thread = heap.threads.get(heap.entry.unwrap()).unwrap();
        deepest = deepest.max(thread.frames.physical_len());
        assert!(thread.frames.stale_slots_hold_no_cold());
        crate::gc::check_invariant(heap).unwrap();
        if !matches!(outcome, StepOutcome::Paused(_)) {
            break;
        }
    }
    assert!(deepest >= 5);
    let entry = runtime.heap().entry.unwrap();
    let thread = runtime.heap_mut().threads.get_mut(entry).unwrap();
    let depth = thread.frames.len();
    thread.frames.slots[depth].cold = Some(Box::new(crate::heap::FrameCold {
        targets: vec![crate::heap::AssignTarget::Register(0)],
        ..Default::default()
    }));
    assert!(crate::gc::check_invariant(runtime.heap()).is_err());
}

/// A protected call's error Value passes through cold frame state. Once its
/// frame is recycled, that Value is no longer a root; a live-slot control
/// still survives both collector modes.
#[test]
fn recycled_cold_box_does_not_keep_an_error_value_alive() {
    for mode in ["incremental", "generational"] {
        let source = format!(
            r#"
            collectgarbage("{mode}")
            local weak = setmetatable({{}}, {{__mode = "v"}})
            local function call()
                local dead = {{}}
                weak[1] = dead
                local ok, err = pcall(function() error(dead) end)
                assert(not ok and err == dead)
            end
            call()
            local live = {{}}
            weak[2] = live
            collectgarbage()
            collectgarbage()
            return weak[1] == nil, weak[2] == live
            "#
        );
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut runtime = boot_natives(&chunk.proto);
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(
            pair_results(&runtime),
            vec![Observed::Bool(true); 2],
            "{mode}"
        );
    }
}

/// Restore builds storage exactly as large as the image: no retained slot
/// or frame exists that the image did not carry.
#[test]
fn restored_stack_and_frames_are_physically_exact() {
    for name in [
        "closure_pair.lua",
        "pcall_nested.lua",
        "vararg_frames.lua",
        "close_basic.lua",
        "corpus_coroutine.lua",
    ] {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let mut runtime = boot_spec(&chunk.proto);
        runtime.install_standard().unwrap();
        runtime.set_output(Box::new(|_| {}));
        let mut journal = Journal::new();
        for _ in 0..64 {
            let outcome = runtime.run(5, &mut journal).unwrap();
            let bytes = runtime.snapshot().unwrap();
            let restored =
                Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                    .unwrap();
            for (_, _, thread) in restored.heap().threads.iter() {
                assert_eq!(thread.stack.physical_len(), thread.stack.len(), "{name}");
                assert_eq!(thread.frames.physical_len(), thread.frames.len(), "{name}");
            }
            if !matches!(outcome, StepOutcome::Paused(_)) {
                break;
            }
        }
    }
}

/// Frame fields in an image must describe the call chain actually present.
/// The raw flag mutation also checks that unused bits cannot activate cold
/// state during decode.
#[test]
fn restore_refuses_malformed_activation_records() {
    let runtime = run_to_wait(b"local function f(a) park() return a end local x = f(3) return x");
    let image = runtime.to_image().unwrap();
    let thread_index = image
        .threads
        .iter()
        .position(|thread| thread.id == image.entry)
        .unwrap();
    assert!(image.threads[thread_index].frames.len() >= 2);
    let domain = runtime.effect_domain();
    let restore = |image: &crate::snapshot::Image| {
        let bytes = snapshot::encode(image).unwrap();
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain)
    };
    assert!(restore(&image).is_ok());
    type FrameEdit = fn(&mut crate::snapshot::ThreadImage);
    let edits: &[(&str, FrameEdit)] = &[
        ("inactive cold state", |thread| {
            thread.frames[0].pending = crate::snapshot::PendingImage::Resuming {
                child: 0,
                dest: 0,
                nresults: 1,
            };
        }),
        ("invalid result contract", |thread| {
            thread.frames[1].nresults = 6;
        }),
        ("impossible base", |thread| {
            thread.frames[1].base = 0;
        }),
        ("impossible top", |thread| {
            thread.top = 100_001;
        }),
        ("frame extent beyond stack bound", |thread| {
            thread.frames[1].limit = 100_001;
        }),
        ("stored stack charge below logical extent", |thread| {
            thread.charged_slots = 0;
        }),
        ("mismatched caller pc", |thread| {
            thread.frames[0].pc += 1;
        }),
    ];
    for &(name, edit) in edits {
        let mut changed = image.clone();
        edit(&mut changed.threads[thread_index]);
        assert!(restore(&changed).is_err(), "accepted {name}");
    }

    let frame = &image.threads[thread_index].frames[0];
    let mut header = Vec::new();
    header.extend(frame.closure.to_le_bytes());
    header.extend(frame.pc.to_le_bytes());
    header.extend(frame.base.to_le_bytes());
    header.extend(frame.limit.to_le_bytes());
    header.push(frame.nresults);
    header.extend(frame.vararg_len.to_le_bytes());
    header.push(u8::from(frame.tail));
    let mut bytes = snapshot::encode(&image).unwrap();
    let matches = bytes
        .windows(header.len())
        .enumerate()
        .filter_map(|(at, window)| (window == header).then_some(at))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "frame header must be unique in this fixture"
    );
    bytes[matches[0] + header.len() - 1] = 0x80;
    recrc(&mut bytes);
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain),
        SnapshotError::InvalidTag,
    );
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_matches_recorded_fixture_output() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    let version = lua_command(&lua)
        .arg("-v")
        .output()
        .unwrap_or_else(|error| panic!("run {lua}: {error}"));
    let banner = format!(
        "{}{}",
        String::from_utf8_lossy(&version.stdout),
        String::from_utf8_lossy(&version.stderr)
    );
    assert!(
        banner.contains("Lua 5.4"),
        "oracle is not Lua 5.4: {banner}"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    for (file, expected) in [
        (
            "multi_result.lua",
            "4 2 3 nil 30 1 10 10 nil 30 nil 10 nil 7\n",
        ),
        ("parallel_assign.lua", "2 99 nil 2\n"),
    ] {
        let output = lua_command(&lua)
            .arg(root.join(file))
            .output()
            .unwrap_or_else(|error| panic!("run {lua}: {error}"));
        assert!(
            output.status.success(),
            "{file} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{file}"
        );
    }
}

fn run_steps(runtime: &mut Runtime, journal: &mut Journal, steps: u32) {
    for _ in 0..steps {
        match runtime.run(1, journal).unwrap() {
            StepOutcome::Paused(_) => {}
            other => panic!("step produced {other:?}"),
        }
    }
}

fn table_obs(runtime: &Runtime) -> Vec<Observed> {
    [10u8, 12, 14, 16, 18, 19, 20, 21, 22, 23, 26]
        .into_iter()
        .map(|reg| observe_reg(runtime, reg))
        .collect()
}

fn expected_table_obs() -> Vec<Observed> {
    vec![
        Observed::Int(1),
        Observed::Int(2),
        Observed::Int(3),
        Observed::Nil,
        Observed::Int(3),
        Observed::Int(1),
        Observed::Int(0),
        Observed::Int(0),
        Observed::Int(2),
        Observed::Int(0),
        Observed::Int(2),
    ]
}

fn dead_len(runtime: &Runtime, reg: u8) -> usize {
    let crate::value::Value::Table(handle) = runtime.entry_slot(reg).unwrap() else {
        panic!("register {reg} is not a table");
    };
    runtime.heap().tables.get(handle).unwrap().dead_len()
}

#[test]
fn table_traversal_and_borders_match_across_fuel_and_checkpoints() {
    let proof = crate::program::table_semantics_program();
    let spec = proof.spec;
    quantum_and_checkpoints(&spec, table_obs);
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(table_obs(&runtime), expected_table_obs());
    assert_eq!(dead_len(&runtime, 0), 1);

    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    run_steps(&mut runtime, &mut journal, proof.resume_at);
    assert_eq!(observe_reg(&runtime, 10), Observed::Int(1));
    assert_eq!(runtime.entry_field_integer(0, 1).unwrap(), None);
    assert_eq!(runtime.entry_field_integer(0, 2).unwrap(), Some(20));
    assert_eq!(dead_len(&runtime, 0), 1);
    let domain = runtime.effect_domain();
    let bytes = runtime.snapshot().unwrap();
    drop(runtime);
    let mut restored = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
    let outcome = restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(table_obs(&restored), expected_table_obs());

    let expected = expected_table_obs();
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    loop {
        runtime.collect();
        let saved = runtime.snapshot().unwrap();
        let mut fresh =
            Runtime::from_snapshot(&saved, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        let mut replay = journal.clone();
        let outcome = fresh.run_until_terminal(u64::MAX, &mut replay).unwrap();
        assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
        assert_eq!(table_obs(&fresh), expected);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => {
                runtime.collect();
                assert_eq!(table_obs(&runtime), expected);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn table_fingerprint_matches_uninterrupted_registers() {
    let proof = crate::program::table_semantics_program();
    let mut runtime = boot_spec(&proof.spec);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let packed = crate::table_semantics_fingerprint().unwrap();
    let mut again = 0i64;
    for (shift, reg) in [10u8, 12, 14, 16, 18, 19, 20, 21, 22, 23, 26]
        .into_iter()
        .enumerate()
    {
        let piece = match runtime.entry_slot(reg).unwrap() {
            crate::value::Value::Nil => 0,
            crate::value::Value::Integer(integer) => integer,
            other => panic!("{other:?}"),
        };
        again |= piece << (shift * 4);
    }
    assert_eq!(packed, again);
    assert_eq!(table_obs(&runtime), expected_table_obs());
}

#[test]
fn next_rejects_an_unknown_key_a_non_table_and_nan() {
    for (spec, fault) in [
        (crate::program::invalid_next_program(), LuaFault::NextKey),
        (crate::program::next_type_program(), LuaFault::Type),
        (crate::program::next_nan_program(), LuaFault::NextKey),
    ] {
        let mut runtime = boot_spec(&spec);
        let mut journal = Journal::new();
        let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert_eq!(outcome, StepOutcome::LuaError(fault));
    }
}

#[test]
fn updates_and_other_deletes_do_not_break_traversal() {
    let mut runtime = boot_spec(&crate::program::update_during_next_program());
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(observe_reg(&runtime, 3), Observed::Int(1));
    assert_eq!(observe_reg(&runtime, 5), Observed::Int(2));
    assert_eq!(observe_reg(&runtime, 7), Observed::Int(99));
    assert_eq!(observe_reg(&runtime, 8), Observed::Int(21));

    let mut runtime = boot_spec(&crate::program::delete_other_program());
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(observe_reg(&runtime, 3), Observed::Int(1));
    assert_eq!(observe_reg(&runtime, 5), Observed::Int(3));
    assert_eq!(observe_reg(&runtime, 7), Observed::Int(3));
    assert_eq!(observe_reg(&runtime, 9), Observed::Nil);

    let mut runtime = boot_spec(&crate::program::reinsert_program());
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(observe_reg(&runtime, 3), Observed::Int(2));
    assert_eq!(observe_reg(&runtime, 5), Observed::Int(1));
    assert_eq!(observe_reg(&runtime, 7), Observed::Nil);
    assert_eq!(observe_reg(&runtime, 8), Observed::Int(11));
    assert_eq!(dead_len(&runtime, 0), 0);
}

#[test]
fn deleted_object_key_is_collected_and_restores() {
    let proof = crate::program::object_key_program();
    let mut runtime = boot_spec(&proof.spec);
    let mut journal = Journal::new();
    run_steps(&mut runtime, &mut journal, proof.before_drop);
    assert_eq!(observe_reg(&runtime, 5), Observed::Int(2));
    let key_id = runtime.slot_object_id(1).unwrap().unwrap();
    runtime.collect();
    assert!(runtime.heap().find_by_id(key_id).is_some());
    let domain = runtime.effect_domain();
    let bytes = runtime.snapshot().unwrap();
    let mut restored = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
    assert!(restored.heap().find_by_id(key_id).is_some());
    assert_eq!(observe_reg(&restored, 5), Observed::Int(2));
    run_steps(&mut restored, &mut journal, 1);
    restored.collect();
    assert!(restored.heap().find_by_id(key_id).is_none());
    assert_eq!(dead_len(&restored, 0), 1);
    let bytes = restored.snapshot().unwrap();
    let again = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), domain).unwrap();
    assert!(again.heap().find_by_id(key_id).is_none());
    assert_eq!(dead_len(&again, 0), 1);
}

#[test]
fn deleted_string_key_matches_by_bytes_after_the_object_is_collected() {
    let proof = crate::program::string_key_program();
    let mut runtime = boot_spec(&proof.spec);
    let mut journal = Journal::new();
    // The key is a string object of its own, not the prototype's constant,
    // so nothing else keeps it alive once the register drops it.
    run_steps(&mut runtime, &mut journal, 2);
    let string_id = runtime.store_new_string(1, b"k").unwrap();
    run_steps(&mut runtime, &mut journal, proof.before_drop - 2);
    run_steps(&mut runtime, &mut journal, 1);
    runtime.collect();
    assert!(runtime.heap().find_by_id(string_id).is_none());
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    assert_eq!(observe_reg(&runtime, 5), Observed::Int(2));
    assert!(runtime.heap().find_by_id(string_id).is_none());
}

fn recrc(bytes: &mut [u8]) {
    let body = bytes.len() - 4;
    let crc = snapshot::crc32(&bytes[..body]);
    bytes[body..].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn malformed_traversal_anchors_fail_closed() {
    let proof = crate::program::table_semantics_program();
    let mut runtime = boot_spec(&proof.spec);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let domain = runtime.effect_domain();
    let registry = HostRegistry::proof();
    let image = runtime.to_image().unwrap();

    let mut duplicate_ordinal = image.clone();
    let table = duplicate_ordinal
        .tables
        .iter_mut()
        .find(|table| table.slots.len() > 1)
        .unwrap();
    table.slots[1].ordinal = table.slots[0].ordinal;
    expect_snapshot(
        Runtime::from_snapshot(
            &snapshot::encode(&duplicate_ordinal).unwrap(),
            &registry,
            domain,
        ),
        SnapshotError::InvalidStructure,
    );

    let mut gap = image.clone();
    let table = gap
        .tables
        .iter_mut()
        .find(|table| table.slots.len() > 1)
        .unwrap();
    table.slots[1].ordinal = 5;
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&gap).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut duplicate_key = image.clone();
    let table = duplicate_key
        .tables
        .iter_mut()
        .find(|table| {
            table.slots.iter().any(|slot| {
                matches!(
                    slot.body,
                    snapshot::SlotBody::Dead(snapshot::DeadKey::Integer(1))
                )
            })
        })
        .unwrap();
    let extra = table.slots[1].clone();
    table.slots.push(extra);
    for (index, slot) in table.slots.iter_mut().enumerate() {
        slot.ordinal = index as u32;
    }
    expect_snapshot(
        Runtime::from_snapshot(
            &snapshot::encode(&duplicate_key).unwrap(),
            &registry,
            domain,
        ),
        SnapshotError::InvalidStructure,
    );

    let mut nil_value = image.clone();
    let table = nil_value
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Live { .. }))
        })
        .unwrap();
    for slot in &mut table.slots {
        if let snapshot::SlotBody::Live { value, .. } = &mut slot.body {
            *value = snapshot::EncValue::Nil;
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&nil_value).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut nan = image.clone();
    let table = nan
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Dead(_)))
        })
        .unwrap();
    for slot in &mut table.slots {
        if matches!(slot.body, snapshot::SlotBody::Dead(_)) {
            slot.body = snapshot::SlotBody::Dead(snapshot::DeadKey::Float(f64::NAN.to_bits()));
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&nan).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut zero = image.clone();
    let table = zero
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Dead(_)))
        })
        .unwrap();
    for slot in &mut table.slots {
        if matches!(slot.body, snapshot::SlotBody::Dead(_)) {
            slot.body = snapshot::SlotBody::Dead(snapshot::DeadKey::Object(0));
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&zero).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut future = image.clone();
    let next_id = future.next_object_id;
    let table = future
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Dead(_)))
        })
        .unwrap();
    for slot in &mut table.slots {
        if matches!(slot.body, snapshot::SlotBody::Dead(_)) {
            slot.body = snapshot::SlotBody::Dead(snapshot::DeadKey::Object(next_id));
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&future).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let string_id = image.strings[0].0;
    let mut wrong_kind = image.clone();
    let table = wrong_kind
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Dead(_)))
        })
        .unwrap();
    for slot in &mut table.slots {
        if matches!(slot.body, snapshot::SlotBody::Dead(_)) {
            slot.body = snapshot::SlotBody::Dead(snapshot::DeadKey::Object(string_id));
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(&snapshot::encode(&wrong_kind).unwrap(), &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut dangling_live = image.clone();
    let table = dangling_live
        .tables
        .iter_mut()
        .find(|table| {
            table
                .slots
                .iter()
                .any(|slot| matches!(slot.body, snapshot::SlotBody::Live { .. }))
        })
        .unwrap();
    for slot in &mut table.slots {
        if let snapshot::SlotBody::Live { key, .. } = &mut slot.body {
            *key = snapshot::EncValue::Table(999_999);
            break;
        }
    }
    expect_snapshot(
        Runtime::from_snapshot(
            &snapshot::encode(&dangling_live).unwrap(),
            &registry,
            domain,
        ),
        SnapshotError::DanglingReference,
    );

    let bytes = snapshot::encode(&image).unwrap();
    let pattern = [0u8, 0, 0, 0, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0];
    let mut found = Vec::new();
    for index in 8..bytes.len().saturating_sub(pattern.len()) {
        if bytes[index..index + pattern.len()] != pattern {
            continue;
        }
        let live = u32::from_le_bytes(bytes[index - 8..index - 4].try_into().unwrap());
        let dead = u32::from_le_bytes(bytes[index - 4..index].try_into().unwrap());
        if live == 2 && dead == 1 {
            found.push(index);
        }
    }
    assert_eq!(found.len(), 1, "dead-key header was not unique: {found:?}");
    let at = found[0];

    let mut swapped = bytes.clone();
    swapped[at - 8..at - 4].copy_from_slice(&1u32.to_le_bytes());
    swapped[at - 4..at].copy_from_slice(&2u32.to_le_bytes());
    recrc(&mut swapped);
    expect_snapshot(
        Runtime::from_snapshot(&swapped, &registry, domain),
        SnapshotError::InvalidStructure,
    );

    let mut absurd = bytes.clone();
    absurd[at - 4..at].copy_from_slice(&u32::MAX.to_le_bytes());
    recrc(&mut absurd);
    expect_snapshot(
        Runtime::from_snapshot(&absurd, &registry, domain),
        SnapshotError::LimitExceeded,
    );

    let mut bad_tag = bytes.clone();
    // Tag 7 is a light userdata key (schema 15); 8 is no key.
    bad_tag[at + 5] = 8;
    recrc(&mut bad_tag);
    expect_snapshot(
        Runtime::from_snapshot(&bad_tag, &registry, domain),
        SnapshotError::InvalidTag,
    );
}

#[test]
fn source_closure_matches_hand_bytecode_under_fuel_gc_and_checkpoints() {
    let source = include_bytes!("../fixtures/lua/closure_pair.lua");
    let compiled = crate::compile(source).unwrap();
    let hand = crate::program::closure_pair_hand();
    let expected = vec![
        Observed::Int(1),
        Observed::Int(1),
        Observed::Int(2),
        Observed::Int(2),
    ];
    assert_eq!(pair_results(&finish_spec(&compiled.proto)), expected);
    assert_eq!(pair_results(&finish_spec(&hand)), expected);
    assert!(saw_shared_closure_upvalue(&compiled.proto));
    assert!(saw_shared_closure_upvalue(&hand));
    quantum_and_checkpoints(&compiled.proto, pair_results);
    quantum_and_checkpoints(&hand, pair_results);
    collect_every_safe_point(&compiled.proto, &expected);
}

fn finish_spec(spec: &crate::program::ProtoSpec) -> Runtime {
    finish_with(boot_spec, spec)
}

fn finish_with(
    boot: fn(&crate::program::ProtoSpec) -> Runtime,
    spec: &crate::program::ProtoSpec,
) -> Runtime {
    let mut runtime = boot(spec);
    let mut journal = Journal::new();
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
    runtime
}

fn pair_results(runtime: &Runtime) -> Vec<Observed> {
    runtime
        .entry_results()
        .unwrap()
        .into_iter()
        .map(|value| match value {
            crate::value::Value::Integer(integer) => Observed::Int(integer),
            crate::value::Value::Bool(bit) => Observed::Bool(bit),
            crate::value::Value::Float(float) => Observed::Float(float.to_bits()),
            crate::value::Value::String(handle) => {
                Observed::Str(runtime.heap().string_bytes(handle).unwrap().to_vec())
            }
            crate::value::Value::Nil => Observed::Nil,
            _ => Observed::Other,
        })
        .collect()
}

/// The entry results as Lua's `print` writes integers, booleans, and nil.
fn print_line(runtime: &Runtime) -> String {
    let fields: Vec<String> = pair_results(runtime)
        .into_iter()
        .map(|value| match value {
            Observed::Int(integer) => integer.to_string(),
            Observed::Bool(bit) => bit.to_string(),
            Observed::Str(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            // The text `..` gives a number: Lua's `%.14g`.
            Observed::Float(bits) => {
                crate::concat::number_text(crate::value::Value::Float(f64::from_bits(bits)))
                    .unwrap_or_default()
            }
            Observed::Nil => "nil".to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    format!("{}\n", fields.join("\t"))
}

/// Source fixtures for comparisons and structured control flow, with the
/// line Lua 5.4.9 prints for `print((function() <fixture> end)())`.
const CONTROL_FIXTURES: &[(&str, &str)] = &[
    ("do_scope.lua", "10\t99\n"),
    ("elseif_chain.lua", "20\t99\n"),
    ("while_capture.lua", "0\t1\t2\n"),
    ("break_block.lua", "42\t77\t0\n"),
    ("break_nested.lua", "3\t6\n"),
    ("repeat_capture.lua", "0\t1\t2\n"),
    ("repeat_scope.lua", "4\t2\t3\n"),
    (
        "compare.lua",
        "true\tfalse\ttrue\ttrue\ttrue\tfalse\tfalse\ttrue\ttrue\tfalse\ttrue\ttrue\ttrue\ttrue\ttrue\ttrue\ttrue\tfalse\ttrue\tfalse\ttrue\ttrue\n",
    ),
    ("for_basic.lua", "15\t22\t30\t0\t6\t6\n"),
    ("for_scope.lua", "5\t11\t6\n"),
    ("for_capture.lua", "1\t2\t12\n"),
    ("for_float.lua", "3\t3.0\t3\t3.0\t5\t6\n"),
    ("for_strings.lua", "3\t3.0\t16\t2\t3\t3.0\n"),
    (
        "for_bounds.lua",
        "3\t9223372036854775807\t3\t-9223372036854775808\t2\t4611686018427387905\t1\t2\t-9223372036854775808\t3\t0\t2\t0\t1\t3\n",
    ),
    ("for_eval_once.lua", "4\t3\t0\t5\n"),
    ("neg.lua", "-5\t5\t-2.5\t-9223372036854775808\t3\n"),
    (
        "table_ctor.lua",
        "true\ttrue\ttrue\t10\t20\t1\t10\tnil\t30\t10\tnil\t10\t2\tnil\tnil\t3\n",
    ),
    ("table_index.lua", "10\t20\t42\tnil\t12\t5\tnil\t20\ttrue\n"),
    ("assign_index.lua", "2\t99\tnil\t5\t7\t1\t2\t2\t1\t2\n"),
    ("globals.lua", "42\t42\t7\t7\tnil\n"),
    ("env_shadow.lua", "1\t3\t1\n"),
    ("env_init.lua", "7\n"),
    ("env_param.lua", "5\t6\n"),
    ("table_keys.lua", "1\t2\t5\t6\n"),
];

fn fixture(name: &str) -> Vec<u8> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    crate::hostcaps::native::test_support::read(root.join(name)).unwrap()
}

fn saw_shared_closure_upvalue(spec: &crate::program::ProtoSpec) -> bool {
    let mut runtime = boot_spec(spec);
    let mut journal = Journal::new();
    loop {
        if two_closures_share_one_upvalue(&runtime) {
            return true;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => return two_closures_share_one_upvalue(&runtime),
            other => panic!("{other:?}"),
        }
    }
}

fn two_closures_share_one_upvalue(runtime: &Runtime) -> bool {
    let heap = runtime.heap();
    let Some(entry) = heap.entry else {
        return false;
    };
    let Some(thread) = heap.threads.get(entry) else {
        return false;
    };
    let mut seen = Vec::new();
    for value in &thread.stack {
        let crate::value::Value::Closure(handle) = value else {
            continue;
        };
        let Some(closure) = heap.closures.get(*handle) else {
            continue;
        };
        let Some(upvalue) = closure.upvalues.first() else {
            continue;
        };
        let Some(object) = heap.upvalues.get(*upvalue) else {
            continue;
        };
        if closure.upvalues.len() == 1 {
            seen.push((closure.id.raw(), object.id.raw()));
        }
    }
    seen.iter().enumerate().any(|(index, (closure, upvalue))| {
        seen[index + 1..]
            .iter()
            .any(|(other, other_upvalue)| other != closure && other_upvalue == upvalue)
    })
}

fn collect_every_safe_point(spec: &crate::program::ProtoSpec, expected: &[Observed]) {
    collect_every_safe_point_with(boot_spec, spec, expected);
}

fn collect_every_safe_point_with(
    boot: fn(&crate::program::ProtoSpec) -> Runtime,
    spec: &crate::program::ProtoSpec,
    expected: &[Observed],
) {
    let mut runtime = boot(spec);
    let mut journal = Journal::new();
    loop {
        runtime.collect();
        finish_observation(&runtime, &journal, &pair_results, expected);
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => {
                runtime.collect();
                assert_eq!(pair_results(&runtime), expected);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

const BRANCH_CLOSE: &[u8] = include_bytes!("../fixtures/lua/branch_close.lua");
const BRANCH_THRESHOLD: &[u8] = include_bytes!("../fixtures/lua/branch_threshold.lua");

fn ints_obs(values: &[i64]) -> Vec<Observed> {
    values.iter().map(|value| Observed::Int(*value)).collect()
}

#[test]
fn branch_close_matches_hand_bytecode_under_fuel_gc_and_checkpoints() {
    let compiled = crate::compile(BRANCH_CLOSE).unwrap();
    let hand = crate::program::branch_close_hand(true);
    let expected = ints_obs(&[10, 11, 11, 99]);
    assert_eq!(pair_results(&finish_spec(&compiled.proto)), expected);
    assert_eq!(pair_results(&finish_spec(&hand)), expected);
    assert!(saw_shared_closure_upvalue(&compiled.proto));
    quantum_and_checkpoints(&compiled.proto, pair_results);
    quantum_and_checkpoints(&hand, pair_results);
    collect_every_safe_point(&compiled.proto, &expected);
    collect_every_safe_point(&hand, &expected);

    let threshold = crate::compile(BRANCH_THRESHOLD).unwrap();
    let expected = ints_obs(&[3, 2]);
    assert_eq!(pair_results(&finish_spec(&threshold.proto)), expected);
    quantum_and_checkpoints(&threshold.proto, pair_results);
    collect_every_safe_point(&threshold.proto, &expected);
}

#[test]
fn missing_close_is_caught_by_the_comparator() {
    let expected = ints_obs(&[10, 11, 11, 99]);
    let open = pair_results(&finish_spec(&crate::program::branch_close_hand(false)));
    assert_ne!(open, expected);
    let mut compiled = crate::compile(BRANCH_CLOSE).unwrap();
    let mut removed = 0;
    for op in &mut compiled.proto.ops {
        if let crate::opcode::Op::CloseUpvalues { from } = *op {
            *op = crate::opcode::Op::Move {
                dst: from,
                src: from,
            };
            removed += 1;
        }
    }
    assert_eq!(removed, 1);
    assert_ne!(pair_results(&finish_spec(&compiled.proto)), expected);
}

/// Open or closed state of the first upvalue of the closure in `reg`.
fn upvalue_is_open(runtime: &Runtime, reg: u8) -> Option<(u64, bool)> {
    let crate::value::Value::Closure(handle) = runtime.entry_slot(reg).ok()? else {
        return None;
    };
    let heap = runtime.heap();
    let upvalue = *heap.closures.get(handle)?.upvalues.first()?;
    let object = heap.upvalues.get(upvalue)?;
    let open = matches!(object.state, crate::heap::UpvalueState::Open { .. });
    Some((object.id.raw(), open))
}

fn entry_pc(runtime: &Runtime) -> u32 {
    let heap = runtime.heap();
    let entry = heap.entry.unwrap();
    heap.threads
        .get(entry)
        .unwrap()
        .frames
        .first()
        .map(|frame| frame.pc)
        .unwrap_or(u32::MAX)
}

#[test]
fn snapshots_keep_open_and_closed_upvalues_apart() {
    let spec = crate::program::branch_close_hand(true);
    let close_pc = crate::program::BRANCH_CLOSE_PC;
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    let mut saw = (false, false);
    loop {
        let restored = Runtime::from_snapshot(
            &runtime.snapshot().unwrap(),
            &HostRegistry::proof(),
            runtime.effect_domain(),
        )
        .unwrap();
        let pc = entry_pc(&runtime);
        if runtime
            .heap()
            .threads
            .get(runtime.heap().entry.unwrap())
            .unwrap()
            .frames
            .len()
            == 1
            && (close_pc..=close_pc + 6).contains(&pc)
        {
            let (inc_cell, inc_open) = upvalue_is_open(&restored, 0).unwrap();
            let (get_cell, get_open) = upvalue_is_open(&restored, 1).unwrap();
            assert_eq!(
                inc_cell, get_cell,
                "pc {pc}: closures lost their shared cell"
            );
            assert_eq!(inc_open, get_open);
            assert_eq!(upvalue_is_open(&runtime, 1).unwrap().1, get_open);
            if pc <= close_pc {
                assert!(get_open, "pc {pc}: restore closed an open upvalue");
                saw.0 = true;
            } else {
                assert!(!get_open, "pc {pc}: close did not survive restore");
                saw.1 = true;
            }
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(saw, (true, true));
}

#[test]
fn closed_cell_keeps_its_table_until_no_closure_holds_it() {
    let spec = crate::program::closed_heap_program();
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    let mut table_id = None;
    let mut cell_id = None;
    let mut released = false;
    loop {
        runtime.collect();
        let pc = entry_pc(&runtime);
        if pc == 2 {
            table_id = runtime.slot_object_id(1).unwrap();
        }
        if pc == 4 {
            cell_id = upvalue_is_open(&runtime, 0).map(|(id, open)| {
                assert!(!open);
                id
            });
        }
        if (5..=8).contains(&pc) {
            let table = table_id.unwrap();
            assert!(
                runtime.heap().find_by_id(table).is_some(),
                "pc {pc}: closed value was collected with its register cleared"
            );
        }
        if pc == 9 {
            let cell = ObjectId(cell_id.unwrap());
            assert!(
                runtime.heap().find_by_id(cell).is_none(),
                "closed cell outlived every closure"
            );
            assert!(runtime.heap().find_by_id(table_id.unwrap()).is_some());
            released = true;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    match &runtime.entry_results().unwrap()[..] {
        [crate::value::Value::Table(_)] => {}
        other => panic!("{other:?}"),
    }
    assert!(released);
}

#[test]
fn snapshot_from_an_older_instruction_set_is_rejected() {
    let chunk = crate::compile(b"local a, b = 1, 2 if a < b then return 11 end return 22").unwrap();
    let runtime = boot_spec(&chunk.proto);
    let bytes = runtime.snapshot().unwrap();
    assert_eq!(
        u16::from_le_bytes([bytes[6], bytes[7]]),
        snapshot::BYTECODE_REVISION
    );
    for revision in [
        snapshot::BYTECODE_REVISION - 1,
        snapshot::BYTECODE_REVISION + 1,
    ] {
        let mut forged = bytes.clone();
        forged[6..8].copy_from_slice(&revision.to_le_bytes());
        recrc(&mut forged);
        expect_snapshot(
            Runtime::from_snapshot(&forged, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::BadVersion,
        );
    }
}

fn raw_spec(ops: Vec<crate::opcode::Op>, max_reg: u8) -> crate::program::ProtoSpec {
    crate::program::ProtoSpec {
        ops,
        byte_consts: Vec::new(),
        captures: Vec::new(),
        children: Vec::new(),
        max_reg,
        params: 0,
        vararg: false,
        debug: None,
    }
}

/// Boot `spec` without validation, snapshot it, and restore the bytes.
fn restore_booted(spec: &crate::program::ProtoSpec) -> Result<Runtime, SnapshotError> {
    let runtime = boot_spec(spec);
    let bytes = runtime.snapshot().unwrap();
    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
}

#[test]
fn restored_code_passes_the_bytecode_validator() {
    use crate::opcode::{Capture, Op};
    let ret = Op::Return { base: 0, count: 0 };
    let mut bad = vec![
        (
            "register",
            raw_spec(vec![Op::LoadInt { dst: 40, value: 1 }, ret], 2),
        ),
        ("jump", raw_spec(vec![Op::Jump { offset: 100 }, ret], 2)),
        (
            "backward jump",
            raw_spec(vec![Op::Jump { offset: -5 }, ret], 2),
        ),
        (
            "child",
            raw_spec(vec![Op::MakeClosure { dst: 0, child: 3 }, ret], 2),
        ),
        (
            "upvalue",
            raw_spec(vec![Op::GetUpvalue { dst: 0, index: 0 }, ret], 2),
        ),
        (
            "call window",
            raw_spec(
                vec![
                    Op::Call {
                        func: 0,
                        nargs: 9,
                        nresults: 1,
                    },
                    ret,
                ],
                2,
            ),
        ),
        (
            "return window",
            raw_spec(vec![Op::Return { base: 1, count: 4 }], 2),
        ),
        (
            "close threshold",
            raw_spec(vec![Op::CloseUpvalues { from: 9 }, ret], 2),
        ),
        (
            "branch operand",
            raw_spec(vec![Op::JumpIfFalse { src: 50, offset: 0 }, ret], 2),
        ),
        (
            "constant",
            raw_spec(
                vec![
                    Op::LoadBytes {
                        dst: 0,
                        const_index: 0,
                    },
                    ret,
                ],
                2,
            ),
        ),
    ];
    let mut child = raw_spec(vec![ret], 1);
    child.captures = vec![Capture::Local(30)];
    let mut parent = raw_spec(vec![ret], 2);
    parent.children = vec![child];
    bad.push((
        "compare",
        raw_spec(
            vec![
                Op::Compare {
                    kind: crate::opcode::CmpKind::Lt,
                    dst: 0,
                    a: 0,
                    b: 60,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push((
        "for base",
        raw_spec(vec![Op::ForPrep { base: 0, offset: 0 }, ret], 3),
    ));
    bad.push((
        "for base overflow",
        raw_spec(
            vec![
                Op::ForLoop {
                    base: 254,
                    offset: -1,
                },
                ret,
            ],
            255,
        ),
    ));
    bad.push((
        "for jump",
        raw_spec(vec![Op::ForPrep { base: 0, offset: 9 }, ret], 4),
    ));
    bad.push((
        "for backward jump",
        raw_spec(
            vec![
                Op::ForLoop {
                    base: 0,
                    offset: -3,
                },
                ret,
            ],
            4,
        ),
    ));
    bad.push(("neg", raw_spec(vec![Op::Neg { dst: 0, src: 7 }, ret], 2)));
    bad.push((
        "index",
        raw_spec(
            vec![
                Op::Index {
                    dst: 0,
                    obj: 0,
                    key: 9,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push((
        "set index",
        raw_spec(
            vec![
                Op::SetIndex {
                    obj: 9,
                    key: 0,
                    src: 0,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push((
        "field constant",
        raw_spec(
            vec![
                Op::GetField {
                    dst: 0,
                    obj: 0,
                    name: 3,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push((
        "set field constant",
        raw_spec(
            vec![
                Op::SetField {
                    obj: 0,
                    name: 0,
                    src: 1,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push((
        "list start",
        raw_spec(
            vec![
                Op::SetList {
                    table: 0,
                    src: 1,
                    start: 0,
                },
                ret,
            ],
            2,
        ),
    ));
    bad.push(("capture", parent));
    for (name, spec) in &bad {
        match restore_booted(spec) {
            Err(error) => assert_eq!(error, SnapshotError::InvalidBytecode, "{name}"),
            Ok(_) => panic!("{name}: restored invalid code"),
        }
    }

    // Dead code after `Return` is still checked, and the source runtime is
    // untouched by the failed restore.
    let spec = raw_spec(
        vec![
            Op::LoadInt { dst: 0, value: 7 },
            Op::Return { base: 0, count: 1 },
            Op::LoadInt { dst: 40, value: 1 },
        ],
        1,
    );
    let mut runtime = boot_spec(&spec);
    let bytes = runtime.snapshot().unwrap();
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidBytecode,
    );
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(pair_results(&runtime), vec![Observed::Int(7)]);

    // An unknown opcode tag with a valid checksum is a decode error.
    let marker = 0x1122_3344_5566_7788i64;
    let runtime = boot_spec(&raw_spec(
        vec![
            Op::LoadInt {
                dst: 0,
                value: marker,
            },
            Op::Return { base: 0, count: 1 },
        ],
        1,
    ));
    let mut bytes = runtime.snapshot().unwrap();
    let mut needle = vec![2u8, 0];
    needle.extend(marker.to_le_bytes());
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("encoded LoadInt");
    bytes[at] = 250;
    recrc(&mut bytes);
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidTag,
    );

    // A comparison kind outside Eq/Ne/Lt/Le is a decode error too.
    let runtime = boot_spec(&raw_spec(
        vec![
            Op::Compare {
                kind: crate::opcode::CmpKind::Le,
                dst: 3,
                a: 7,
                b: 5,
            },
            ret,
        ],
        8,
    ));
    let mut bytes = runtime.snapshot().unwrap();
    let needle = [34u8, 3, 3, 7, 5];
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("encoded Compare");
    bytes[at + 1] = 9;
    recrc(&mut bytes);
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
        SnapshotError::InvalidTag,
    );
}

#[test]
fn control_flow_fixtures_match_lua_under_fuel_gc_and_checkpoints() {
    for (name, line) in CONTROL_FIXTURES {
        let chunk =
            crate::compile(&fixture(name)).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let runtime = finish_spec(&chunk.proto);
        assert_eq!(print_line(&runtime), *line, "{name}");
        let expected = pair_results(&runtime);
        quantum_and_checkpoints(&chunk.proto, pair_results);
        collect_every_safe_point(&chunk.proto, &expected);
    }
}

#[test]
fn leaving_blocks_and_loops_closes_before_the_register_is_reused() {
    use crate::opcode::Op;
    for (name, from, reused) in [
        ("do_scope.lua", 1, (1, 99)),
        ("elseif_chain.lua", 2, (2, 99)),
        ("break_block.lua", 2, (2, 77)),
    ] {
        let ops = crate::compile(&fixture(name)).unwrap().proto.ops;
        assert!(ops.contains(&Op::CloseUpvalues { from }), "{name}");
        assert!(
            ops.contains(&Op::LoadInt {
                dst: reused.0,
                value: reused.1
            }),
            "{name}: later local does not reuse the register"
        );
    }
    let ops = crate::compile(&fixture("break_block.lua"))
        .unwrap()
        .proto
        .ops;
    let close = ops
        .iter()
        .position(|op| *op == Op::CloseUpvalues { from: 2 })
        .unwrap();
    assert!(
        matches!(ops[close + 1], Op::Jump { .. }),
        "break closes, then jumps"
    );

    for name in [
        "while_capture.lua",
        "repeat_capture.lua",
        "do_scope.lua",
        "for_capture.lua",
    ] {
        let right = pair_results(&finish_spec(&crate::compile(&fixture(name)).unwrap().proto));
        let mut chunk = crate::compile(&fixture(name)).unwrap();
        for op in &mut chunk.proto.ops {
            if let Op::CloseUpvalues { from } = *op {
                *op = Op::Move {
                    dst: from,
                    src: from,
                };
            }
        }
        assert_ne!(pair_results(&finish_spec(&chunk.proto)), right, "{name}");
    }
}

#[test]
fn break_needs_an_enclosing_loop_in_the_same_function() {
    let error = crate::compile(b"local a = 1 break").unwrap_err();
    assert_eq!(error.kind, crate::CompileErrorKind::Syntax);
    assert_eq!(error.span, crate::Span::new(12, 17));
    assert_eq!(
        crate::compile(b"while true do local f = function() break end end")
            .unwrap_err()
            .kind,
        crate::CompileErrorKind::Syntax
    );
}

#[test]
fn an_infinite_loop_pauses_and_hits_the_hard_fuel_limit() {
    for mode in [
        crate::runtime::HotCoreMode::Full,
        crate::runtime::HotCoreMode::NoFastCalls,
        crate::runtime::HotCoreMode::Off,
    ] {
        mode.with(|| {
            let chunk = crate::compile(b"while true do end").unwrap();
            let mut runtime = boot_spec(&chunk.proto);
            let mut journal = Journal::new();
            for _ in 0..3 {
                assert_eq!(
                    runtime.run(1_000, &mut journal).unwrap(),
                    StepOutcome::Paused(PauseReason::FuelExhausted)
                );
            }
            assert_eq!(runtime.fuel_consumed(), 3_000);
            let config = Config {
                fuel_limit: Some(10_000),
                ..Config::default()
            };
            let mut runtime =
                Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
            assert_eq!(
                runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
            );
            assert_eq!(runtime.fuel_consumed(), 10_000);
        });
    }
}

#[test]
fn loop_iterations_do_not_accumulate_heap_objects() {
    let source = b"local i = 0 local keep while i < 6 do local tmp = function() return i end local x = i keep = function() return x end i = i + 1 end return keep(), i";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    let mut most = (0, 0);
    loop {
        runtime.collect();
        let heap = runtime.heap();
        most = (
            most.0.max(heap.closures.live()),
            most.1.max(heap.upvalues.live()),
        );
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        pair_results(&runtime),
        vec![Observed::Int(5), Observed::Int(6)]
    );
    // The entry closure, `tmp`, the kept closure, and the one being made.
    assert!(most.0 <= 4, "closures accumulated: {}", most.0);
    // `_ENV`, `i`, the kept `x`, and the next iteration's `x`.
    assert!(most.1 <= 4, "upvalues accumulated: {}", most.1);
}

#[test]
fn ordering_unsupported_types_is_a_compare_fault() {
    let chunk = crate::compile(b"local f = function() end return f < f").unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::LuaError(LuaFault::Compare)
    );
    let chunk = crate::compile(b"local f = function() end return f == f, f ~= f").unwrap();
    assert_eq!(
        pair_results(&finish_spec(&chunk.proto)),
        vec![Observed::Bool(true), Observed::Bool(false)]
    );
}

#[test]
fn numeric_for_errors_are_lua_faults() {
    for (source, fault) in [
        (&b"for i = 1, 10, 0 do end"[..], LuaFault::ForZeroStep),
        (b"for i = 1.0, 10.0, 0.0 do end", LuaFault::ForZeroStep),
        (b"for i = 1.0, 10.0, -0.0 do end", LuaFault::ForZeroStep),
        (
            b"local z = 0 for i = 1, 10, z do end",
            LuaFault::ForZeroStep,
        ),
        (b"for i = 1, 'abc' do end", LuaFault::ForValue),
        (b"for i = 'x', 3 do end", LuaFault::ForValue),
        (b"for i = 1, 3, true do end", LuaFault::ForValue),
        (
            b"local f = function() end for i = f, 3 do end",
            LuaFault::ForValue,
        ),
        (b"local f = function() end return -f", LuaFault::Arith),
        (b"return -'x'", LuaFault::Arith),
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = boot_spec(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::LuaError(fault),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

/// A loop's state that is not numbers, which only `debug.setlocal` makes
/// in a run, is a Lua error (ADR 0040).
#[test]
fn a_for_loop_over_non_numbers_is_a_lua_error() {
    use crate::opcode::Op;
    let spec = raw_spec(
        vec![
            Op::LoadNil { dst: 0 },
            Op::ForLoop {
                base: 0,
                offset: -2,
            },
            Op::Return { base: 0, count: 0 },
        ],
        4,
    );
    let mut runtime = boot_spec(&spec);
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal),
        Ok(StepOutcome::LuaError(LuaFault::Type))
    );
}

#[test]
fn huge_and_endless_numeric_loops_stay_under_fuel() {
    for source in [
        &b"for i = 1, 9223372036854775807 do end"[..],
        b"for i = 1.0, 1e300, 1e-300 do end",
        b"for i = 0x8000000000000000, 9223372036854775807 do end",
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = boot_spec(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run(5_000, &mut journal).unwrap(),
            StepOutcome::Paused(PauseReason::FuelExhausted)
        );
        let config = Config {
            fuel_limit: Some(20_000),
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Terminated(TerminationReason::FuelLimitExceeded)
        );
    }
}

#[test]
fn numeric_for_keeps_its_hidden_state_in_four_registers() {
    use crate::opcode::Op;
    let chunk = crate::compile(b"local a = 0 for i = 1, 3 do a = a + i end return a").unwrap();
    let ops = &chunk.proto.ops;
    let prep = ops
        .iter()
        .position(|op| matches!(op, Op::ForPrep { base: 1, .. }))
        .expect("ForPrep over registers 1..=4");
    assert!(
        matches!(ops[prep - 1], Op::LoadInt { dst: 3, value: 1 }),
        "default step"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::ForLoop { base: 1, .. }))
    );
    // After the loop the hidden registers are free again.
    let chunk = crate::compile(b"for i = 1, 2 do end local x = 7 return x").unwrap();
    assert!(chunk.proto.ops.contains(&Op::LoadInt { dst: 0, value: 7 }));
}

#[test]
fn numeric_loop_iterations_do_not_accumulate_heap_objects() {
    let source = b"local keep for i = 1, 6 do local tmp = function() return i end keep = function() return i end end return keep()";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    let mut most = (0, 0);
    loop {
        runtime.collect();
        let heap = runtime.heap();
        most = (
            most.0.max(heap.closures.live()),
            most.1.max(heap.upvalues.live()),
        );
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(pair_results(&runtime), vec![Observed::Int(6)]);
    assert!(most.0 <= 4, "closures accumulated: {}", most.0);
    // `_ENV`, the kept `i`, and the next iteration's `i`.
    assert!(most.1 <= 3, "upvalues accumulated: {}", most.1);
}

#[test]
fn indexed_parallel_assignment_records_the_old_key() {
    use crate::opcode::Op;
    let chunk =
        crate::compile(b"local i = 1 local t = {} i, t[i] = 2, 99 return i, t[1], t[2]").unwrap();
    let ops = &chunk.proto.ops;
    assert!(ops.iter().any(|op| matches!(op, Op::AssignField { .. })));
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::AssignCommit { n: 2, .. }))
    );
    // Stop with the destinations recorded and no store done, restore, and
    // check the recorded key is still the old index 1.
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    loop {
        if runtime.assign_remaining() == Some(2) {
            break;
        }
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    let mut restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    restored.collect();
    restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&restored), "2\t99\tnil\n");
}

#[test]
fn indexing_faults_and_nil_keys() {
    for (source, fault) in [
        (&b"local a = 1 return a.x"[..], LuaFault::Index),
        (b"local a = 1 return a[1]", LuaFault::Index),
        (b"local a = true a.x = 1", LuaFault::Index),
        (b"local a = 1 local k = 2 a[k] = 1", LuaFault::Index),
        (b"return missing.field", LuaFault::Index),
        (b"local t = {} t[nil] = 1", LuaFault::NilKey),
        (b"local t = { [nil] = 1 }", LuaFault::NilKey),
        (
            b"local t = {} local a, b = 1, 2 t[nil], a = 1, 2",
            LuaFault::NilKey,
        ),
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = boot_spec(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::LuaError(fault),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
    let chunk = crate::compile(b"local t = { 1 } return t[nil], t.missing").unwrap();
    assert_eq!(print_line(&finish_spec(&chunk.proto)), "nil\tnil\n");
    for source in [&b"return {}[1]"[..], b"return 1()", b"x.y, 1 = 2, 3"] {
        assert_eq!(
            crate::compile(source).unwrap_err().kind,
            crate::CompileErrorKind::Syntax,
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn the_chunk_environment_is_the_runtime_globals_table() {
    let chunk = crate::compile(&fixture("globals.lua")).unwrap();
    assert_eq!(
        chunk.proto.captures,
        vec![crate::opcode::Capture::Upvalue(0)]
    );
    let runtime = finish_spec(&chunk.proto);
    assert_eq!(runtime.global_integer("x").unwrap(), 42);
    assert_eq!(runtime.global_integer("y").unwrap(), 7);
    let restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    assert_eq!(restored.global_integer("x").unwrap(), 42);
    let mut two = chunk.proto.clone();
    two.captures.push(crate::opcode::Capture::Upvalue(0));
    assert!(crate::check::validate(&two).is_err());
}

/// Boot with the proof registry's native functions bound as globals.
/// The proof natives the fixtures call, each bound as a global of its
/// symbol's name.
const PROOF_NATIVES: [&str; 8] = [
    "add", "sub", "many", "none", "mark", "park", "second", "upto",
];

fn boot_natives(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_spec(spec);
    for name in PROOF_NATIVES {
        runtime.set_global_native(name, name).unwrap();
    }
    runtime.install_base().unwrap();
    runtime
}

/// [`boot_natives`] with the string library, through whose metatable
/// strings convert to numbers in arithmetic (ADR 0034).
fn boot_natives_strings(spec: &crate::program::ProtoSpec) -> Runtime {
    let mut runtime = boot_natives(spec);
    runtime.install_string().unwrap();
    runtime
}

/// Lua definitions matching the proof natives, for the Lua 5.4.9 oracle.
const NATIVE_PRELUDE: &str = "add = function(a, b) return a + b end\nsub = function(a, b) return a - b end\nmany = function() return 10, nil, 30 end\nnone = function() end\nsecond = function(a, b) return b end\nupto = function(n, i) if i == nil then i = 0 end i = i + 1 if i <= n then return i end end\n";

/// Fixtures that recurse to the frame limit. They match Lua like the rest,
/// but a checkpoint at every step of a thousand-frame stack is quadratic, so
/// they run on a sparser schedule (`deep_fixtures_under_sparse_checkpoints`).
const DEEP_FIXTURES: &[(&str, &str)] = &[
    (
        "error_overflow.lua",
        "false\tfalse\tfalse\tfalse\tm\tfalse\t2\n",
    ),
    // 100,000 tail hops of each kind (ADR 0029). Lua 5.4.9 overflows the
    // non-tail recursion near 200,000 calls, Moonseed at 1,000.
    (
        "tail_deep.lua",
        "100000\t3\t1\tnil\t3\tfalse\tdone\t2\tfalse\n",
    ),
    // Method and `local function` tail recursion, 100,000 hops each.
    ("methods_deep.lua", "true\t42\t100000\tfalse\n"),
];

const NATIVE_FIXTURES: &[(&str, &str)] = &[
    ("gfor_builtin.lua", "iterator windows ok\n"),
    ("cmpbr.lua", "4095\t5\ttrue\t0\n"),
    (
        "arithk.lua",
        "true\t13\t1\t3.75\t0.066666666666667\t42\t42\t12\t21\t34\t43\n",
    ),
    (
        "const_locals.lua",
        "(2) 10 20;(3) 1 nil nil;(1) nil;(1) 11;(2) 42 43;(3) 42 7 one;(1) 2;(1) 5;(2) 1 3;(2) 1 3;(1) 12;(1) 6;\n",
    ),
    (
        "goto_basic.lua",
        "(1) 3;(2) 1 2;(3) 0 1 2;(3) 9 12 5;(3) 1 2 nil;(2) 3 30;(1) 12;(1) 3;(1) 2*3;(3) 1 11 21;\n",
    ),
    (
        "goto_close.lua",
        "(1) c0c1c2;(1) cba;(3) false Eb ba;(2) 1 1;(2) 8 1234;\n",
    ),
    (
        "logic_ops.lua",
        "(4) nil false 42 42;(4) 42 x 0 ;(6) true true false false false false;(5) false 1 3 false acefgh;(7) true false 9 false true true true;(1) 10;(1) 10;(1) nil;(1) 10;(1) 1;(2) 10 2;(3) true true y;(1) 4;(1) 6;(3) true false yes;(4) false 1 dflt eq;(1) true;(1) 7;(1) nil;(2) big m;(3) 10 true false;\n",
    ),
    (
        "methods.lua",
        "(2) 1 42;(4) 5 8 6 2;(2) hello! true;(2) true 9;(3) 3 q? long?;(3) lit 7 2;(3) 1 nil 3;(3) 10 nil 30;(2) 10 4;(3) 42 true 1;(4) 2 3 true true;(1) 3628800;(1) inner;(1) 10;(1) outer;(4) answer meth 42 true;(2) 2 deep;(5) true 1 2 2 nil;(1) 3;(1) false;(1) false;(3) 5 7 1;(2) A B;\n",
    ),
    (
        "tail_basic.lua",
        "(3) 10 nil 30;(2) 10 nil;(1) 10;(1) 10;(3) 10 nil 30;(3) 10 1 nil;(3) 10 nil 30;(2) nil 10;(1) 6;(2) 1 2;(3) 2 1 nil;(2) 1 nil;(4) 3 10 nil 30;(5) 4 1 10 nil 30;(1) 0;(1) 10;(4) 1 10 nil 30;(2) 10 1;(1) 42;(3) 2 1 false;(1) nil;(1) 5;gx(1) r;yg(1) r;(2) 0 51;\n",
    ),
    (
        "tail_pcall.lua",
        "false\tboom\tfalse\tHboom\tfalse\tin!\ttrue\tfalse\tboom\tfalse\tfour\t42\tfalse\terror in error handling\n",
    ),
    (
        "vararg_basic.lua",
        "(2) nil nil;(2) 1 nil;(2) 1 2;(4) 1 2 3 4;(4) 1 2 nil nil;(3) 0 1 2;(2) nil 7;(3) 1 nil nil;(3) 1 2 3;(1) 5;(1) nil;(1) 3;(3) 1 nil 3;(2) 1 x;(1) 3;(1) 2;(3) 3 1 3;(3) 1 5 nil;(3) nil 2 nil;(1) 3;(2) b c;(0);(1) b;(1) b;(1) 0;(1) 2;(5) true true true true true;(2) 4 nil;(2) no yes;\n",
    ),
    (
        "vararg_frames.lua",
        "(5) 41 42 41 42 69;(13) key! 100 3 false false yh false mm 4 true true s nil;[closed](3) 1 nil 3;(3) 1 2 3;(2) 2 0;(1) 6;(1) 15;(4) false 2 a nil;\n",
    ),
    (
        "gfor_basic.lua",
        "6\t3\t10\t1\t2\tfalse\tnil\tfalse\t3\t0\t3\t3\t6\t12\t10\tnil\tnil\t20\t21\tnil\t30\t31\t32\t40\tnil\t42\n",
    ),
    (
        "gfor_init.lua",
        "5\ttrue\t5\tnil\ttrue\t9\t3\t5\t1\t7\t11\t13\t17\n",
    ),
    ("gfor_capture.lua", "1\tfirst\t2\tsecond\t12\t13\t23\n"),
    (
        "gfor_close.lua",
        "3\t200\t6\tfalse\t0\t15\tb1\tb2\tH1\tbb\tH2\tI1\tafter1\tI2\tafter2\tO\tr1\tr2\tH3\tF\tnew\n",
    ),
    (
        "gfor_error.lua",
        "false\tboom\t0\tfalse\ttrue\tfalse\tce\tfalse\tHx4\t16\tH\tboom\tB\ttrue\tH2\ttrue\tO\ttrue\tC\tit\tO3\tce\th\tx4\tH4\tHx4\n",
    ),
    (
        "gfor_callable.lua",
        "3\t8\tit\tS\tit\t6\t1\t10\tnil\t30\t5\t0\tit\ttrue\tfalse\tx\n",
    ),
    (
        "close_basic.lua",
        "10\tc\tb\ta\tz\tw\tf\tf\tq\tk\tu\t1\t2\ttrue\n",
    ),
    (
        "close_error.lua",
        "false\teb\tfalse\teb2\tfalse\tfalse\tfalse\t14\tc\torig\tb\tec\ta\teb\tc2\tnil\tb2\tnil\ta2\teb2\ta5\n",
    ),
    (
        "close_xpcall.lua",
        "false\tHorig\tfalse\tHeb\tfalse\tHeb3\t18\th\torig\ta\tHorig\th\to2\tb2\tHo2\th\teb\ta2\tHeb\tb3\tnil\th\teb3\ta3\tHeb3\n",
    ),
    (
        "close_meta.lua",
        "false\t3\tsecond\tcalled\ttrue\t2\t2\ttrue\n",
    ),
    (
        "native_calls.lua",
        "42\t42\ttrue\t42\tfalse\ttrue\t1\tnil\t2\n",
    ),
    (
        "native_results.lua",
        "10\tnil\t30\t10\tnil\t10\tnil\t30\tnil\tnil\t3\t10\tnil\t30\n",
    ),
    (
        "meta_index.lua",
        "42\tanswer\t7\t1\t5\tnil\tnil\tnil\tkey\n",
    ),
    ("meta_newindex.lua", "nil\t42\t7\tnil\t2\t2\t99\tnil\t1\n"),
    ("meta_len.lua", "99\t3\t2\t3\t0\t5\tnil\n"),
    (
        "meta_protect.lua",
        "locked\tlocked\ttrue\tnil\tnil\tnil\ttrue\n",
    ),
    ("meta_mutate.lua", "1\t2\t2\tnil\ttrue\n"),
    (
        "meta_unary.lua",
        "true\ttrue\ttrue\tneg\ttrue\ttrue\ttrue\ttrue\ttrue\n",
    ),
    (
        "meta_call.lua",
        "42\ttrue\ttrue\ttrue\t3\t3\t1\ttrue\ttrue\ttrue\t1\t2\tnil\t101\tfield\tnil\t9\n",
    ),
    (
        "arith.lua",
        "3\t-4\t-1\t1\t3.5\t2.0\t1024.0\t1.4142135623731\t3.0\t0.5\t0.0\ttrue\ttrue\t0\t-2\tinf\tinf\t-inf\ttrue\t9007199254740993\ttrue\t-2\t10.0\t0.0\t-0.0\t42\t16\t10.0\t-2\t3\t1\t4.0\t3.5\t6.5\t-1.0\t-0.5\tinf\t-7.0\n",
    ),
    (
        "bitwise.lua",
        "2\t7\t5\t-1\t-6\t-9223372036854775808\t0\t0\t1\t9223372036854775807\t1\t0\t0\t0\t2\t2\t-2\t240\t4611686018427387904\n",
    ),
    (
        "meta_arith.lua",
        "lhs\trhs\trhs\tlhs\t1\tadd\tsub\tnil\tdiv\tidiv\tmod\tpow\tband\tbor\tbxor\tshl\tshr\tband\tconcat\t5\t1.5\t7\n",
    ),
    (
        "meta_compare.lua",
        "true\t0\ttrue\tfalse\ttrue\ttrue\tfalse\tfalse\ttrue\tfalse\t5\ttrue\ttrue\tfalse\tfalse\ttrue\tfalse\tfalse\ttrue\ttrue\ttrue\n",
    ),
    (
        "pcall_basic.lua",
        "true\tnil\ttrue\t10\ttrue\t10\tnil\t30\tfalse\ttrue\t42\tfalse\tnil\tfalse\tfalse\tfalse\t123\ttrue\t42\ttrue\t2\t1\n",
    ),
    (
        "pcall_nested.lua",
        "true\tfalse\tinner\tfalse\tab\ttrue\tfalse\n",
    ),
    ("pcall_upvalue.lua", "false\tstop\t10\t5\t99\t410\n"),
    (
        "xpcall_basic.lua",
        "false\thandled a\ttrue\t42\t22\tfalse\tfinal b\tfalse\terror in error handling\tfalse\tnil\tfalse\tx\tfalse\n",
    ),
    (
        "error_meta.lua",
        "idx\tnidx\tnil\tlen\tcall\tadd\teq\tlt\tcat\tunm\tfalse\n",
    ),
    (
        "concat.lua",
        "12\t1.52\t-0.0\t9.2233720368548e+18\t9.007199254741e+15\t1e+100\t9223372036854775807\t-9223372036854775808\tinf\t-inf\t3.0\t0.1\t1e+15|\tabc\tT+x\tx+T\taT+b\t1+T\t5\ttrue\tn3\n",
    ),
];

#[test]
fn native_functions_are_values_under_fuel_gc_and_checkpoints() {
    for (name, line) in NATIVE_FIXTURES {
        let chunk = crate::compile(&fixture(name)).unwrap();
        let runtime = finish_with(boot_natives_strings, &chunk.proto);
        assert_eq!(print_line(&runtime), *line, "{name}");
        let expected = pair_results(&runtime);
        quantum_and_checkpoints_with(boot_natives_strings, &chunk.proto, pair_results);
        collect_every_safe_point_with(boot_natives_strings, &chunk.proto, &expected);
    }
}

#[test]
fn native_faults_and_non_callables() {
    for (source, fault) in [
        (&b"return add(1, nil)"[..], LuaFault::Native),
        (b"local x = 1 return x()", LuaFault::BadCall),
        (b"return missing()", LuaFault::BadCall),
        (b"return add < sub", LuaFault::Compare),
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = boot_natives(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::LuaError(fault),
            "{}",
            String::from_utf8_lossy(source)
        );
    }
    let mut runtime = boot_spec(&crate::compile(b"return 1").unwrap().proto);
    assert_eq!(
        runtime.set_global_native("nope", "no.such.symbol"),
        Err(VmError::UnknownNative)
    );
}

#[test]
fn vm_local_natives_take_no_effect_id() {
    let chunk = crate::compile(b"return add(40, 2), add(1, 1)").unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let before = runtime.next_sequence();
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "42\t2\n");
    assert_eq!(runtime.next_sequence(), before);
    assert!(journal.entries().is_empty());
}

#[test]
fn an_external_native_commits_once_across_snapshots() {
    let chunk = crate::compile(b"local x = mark(7) return x").unwrap();
    // Stop in the prepared state: the effect id exists, the host has not run.
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    while runtime.at_prepared().is_none() {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    let sequence = runtime.at_prepared().unwrap();
    let effect = EffectId {
        domain: runtime.effect_domain(),
        sequence,
    };
    let prepared = runtime.snapshot().unwrap();
    let domain = runtime.effect_domain();
    let restore =
        |bytes: &[u8]| Runtime::from_snapshot(bytes, &HostRegistry::proof(), domain).unwrap();
    // Restored before the call, onto an empty journal: commits once.
    let mut fresh = restore(&prepared);
    let mut empty = Journal::new();
    fresh.run_until_terminal(u64::MAX, &mut empty).unwrap();
    assert_eq!(pair_results(&fresh), vec![Observed::Int(sequence as i64)]);
    assert_eq!(empty.entries().len(), 1);
    // A torn journal already holds the outcome: the replay returns it.
    let mut torn = Journal::new();
    torn.seed(effect, 7, 99);
    let mut replay = restore(&prepared);
    replay.run_until_terminal(u64::MAX, &mut torn).unwrap();
    assert_eq!(pair_results(&replay), vec![Observed::Int(99)]);
    assert_eq!(torn.entries().len(), 1);
    // Past the call, onto an empty journal: the native does not run again.
    runtime.run(1, &mut journal).unwrap();
    assert!(runtime.at_prepared().is_none());
    let after = runtime.snapshot().unwrap();
    let mut later = restore(&after);
    let mut untouched = Journal::new();
    later.run_until_terminal(u64::MAX, &mut untouched).unwrap();
    assert_eq!(pair_results(&later), vec![Observed::Int(sequence as i64)]);
    assert!(untouched.entries().is_empty());
}

#[test]
fn a_waiting_native_call_survives_restore_and_completes_once() {
    let chunk = crate::compile(b"local r = park() return r + 1").unwrap();
    let mut journal = Journal::new();
    let expected_fuel = {
        let mut runtime = boot_natives(&chunk.proto);
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Waiting(WaitKey(1))
        );
        runtime.complete_wait(WaitKey(1), 41).unwrap();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert_eq!(pair_results(&runtime), vec![Observed::Int(42)]);
        runtime.fuel_consumed()
    };
    let mut runtime = boot_natives(&chunk.proto);
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Waiting(WaitKey(1))
    );
    let waiting = runtime.snapshot().unwrap();
    drop(runtime);
    let mut restored = Runtime::from_snapshot(&waiting, &HostRegistry::proof(), 1).unwrap();
    assert_eq!(
        restored.run(10, &mut journal).unwrap(),
        StepOutcome::Waiting(WaitKey(1)),
        "fuel does not clear the wait"
    );
    assert_eq!(
        restored.complete_wait(WaitKey(2), 41),
        Err(WaitError::UnknownKey)
    );
    restored.complete_wait(WaitKey(1), 41).unwrap();
    assert_eq!(
        restored.complete_wait(WaitKey(1), 41),
        Err(WaitError::AlreadyCompleted)
    );
    let completed = restored.snapshot().unwrap();
    let mut again = Runtime::from_snapshot(&completed, &HostRegistry::proof(), 1).unwrap();
    again.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(pair_results(&again), vec![Observed::Int(42)]);
    assert_eq!(again.fuel_consumed(), expected_fuel, "no double charge");
    assert!(journal.entries().is_empty());
}

#[test]
fn successive_waits_may_reuse_a_key() {
    let chunk = crate::compile(b"local a = park() local b = park() return a, b").unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    for (index, result) in [5, 6].into_iter().enumerate() {
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Waiting(WaitKey(1)),
            "wait {index}"
        );
        runtime.complete_wait(WaitKey(1), result).unwrap();
        assert_eq!(
            runtime.complete_wait(WaitKey(1), result),
            Err(WaitError::AlreadyCompleted)
        );
    }
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&runtime), "5\t6\n");
}

#[test]
fn native_snapshots_fail_closed() {
    let chunk = crate::compile(b"local f = sub return add(1, 2)").unwrap();
    let runtime = boot_natives(&chunk.proto);
    let bytes = runtime.snapshot().unwrap();
    let domain = runtime.effect_domain();
    // A registry without the natives cannot restore them.
    let mut legacy = HostRegistry::new();
    legacy.register("mark", crate::host::mark);
    legacy.register("park", crate::host::park);
    expect_snapshot(
        Runtime::from_snapshot(&bytes, &legacy, domain),
        SnapshotError::UnknownHostSymbol,
    );
    let find = |bytes: &[u8], needle: &[u8]| {
        bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap()
    };
    // The natives section is the only place "add" is followed by "sub".
    let section = find(&bytes, b"\x03\x00\x00\x00add\x03\x00\x00\x00sub") + 7;
    // Rename "sub" to an unregistered symbol.
    let mut renamed = bytes.clone();
    renamed[section + 6] = b'x';
    recrc(&mut renamed);
    expect_snapshot(
        Runtime::from_snapshot(&renamed, &HostRegistry::proof(), domain),
        SnapshotError::UnknownHostSymbol,
    );
    // Rename "sub" to "add": a duplicate symbol.
    let mut duplicate = bytes.clone();
    duplicate[section + 4..section + 7].copy_from_slice(b"add");
    recrc(&mut duplicate);
    expect_snapshot(
        Runtime::from_snapshot(&duplicate, &HostRegistry::proof(), domain),
        SnapshotError::InvalidStructure,
    );
    // A native value whose index is past the symbol table.
    let mut dangling = bytes.clone();
    let index = runtime
        .heap()
        .natives
        .iter()
        .position(|symbol| symbol == "sub")
        .unwrap() as u32;
    let mut needle = vec![8u8];
    needle.extend(index.to_le_bytes());
    let at = find(&dangling, &needle);
    dangling[at + 1..at + 5].copy_from_slice(&40u32.to_le_bytes());
    recrc(&mut dangling);
    expect_snapshot(
        Runtime::from_snapshot(&dangling, &HostRegistry::proof(), domain),
        SnapshotError::DanglingReference,
    );
    // A VM-local wait recorded as if it had an effect id, and as prepared.
    let chunk = crate::compile(b"local r = park() return r").unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let waiting = runtime.snapshot().unwrap();
    let mut pending = vec![6u8, 0];
    pending.extend(0u64.to_le_bytes());
    pending.extend(1u64.to_le_bytes());
    let at = find(&waiting, &pending);
    let mut with_effect = waiting.clone();
    with_effect[at + 1] = 1;
    recrc(&mut with_effect);
    expect_snapshot(
        Runtime::from_snapshot(&with_effect, &HostRegistry::proof(), domain),
        SnapshotError::InvalidStructure,
    );
    let mut prepared = waiting.clone();
    prepared.splice(
        at..at + pending.len(),
        [5u8].into_iter().chain(7u64.to_le_bytes()),
    );
    recrc(&mut prepared);
    expect_snapshot(
        Runtime::from_snapshot(&prepared, &HostRegistry::proof(), domain),
        SnapshotError::InvalidStructure,
    );
}

fn run_to_wait(source: &[u8]) -> Runtime {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Waiting(WaitKey(1))
    );
    runtime
}

#[test]
fn pending_native_metamethods_restore_and_finish_once() {
    for (source, result, line) in [
        (&b"local t = setmetatable({}, { __index = park }) local v = t.k return v, rawget(t, 'k')"[..], 41, "41\tnil\n"),
        (b"local t = setmetatable({ 1 }, { __len = park }) return #t", 7, "7\n"),
        (b"local seen = {} local t = setmetatable({}, { __newindex = park }) t.k = 5 return rawget(t, 'k')", 0, "nil\n"),
        // `__close` waiting at a scope's exit, in an unwind, and in a return,
        // with an older value still to close after it.
        (b"local n = 0 do local a <close> = setmetatable({}, { __close = function() n = n + 1 end }) \
            local b <close> = setmetatable({}, { __close = park }) end return n", 0, "1\n"),
        (b"local n = 0 local ok, e = pcall(function() \
            local a <close> = setmetatable({}, { __close = function(_, err) n = err end }) \
            local b <close> = setmetatable({}, { __close = park }) error('x', 0) end) return ok, e, n", 0, "false\tx\tx\n"),
        (b"local f = function() local b <close> = setmetatable({}, { __close = park }) return 1, 2 end \
            return f()", 0, "1\t2\n"),
    ] {
        let runtime = run_to_wait(source);
        let bytes = runtime.snapshot().unwrap();
        drop(runtime);
        let mut restored = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), 1).unwrap();
        restored.collect();
        let mut journal = Journal::new();
        assert_eq!(
            restored.run(5, &mut journal).unwrap(),
            StepOutcome::Waiting(WaitKey(1))
        );
        restored.complete_wait(WaitKey(1), result).unwrap();
        // A checkpoint between completion and the instruction's commit.
        let completed = restored.snapshot().unwrap();
        let mut again = Runtime::from_snapshot(&completed, &HostRegistry::proof(), 1).unwrap();
        again.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert_eq!(print_line(&again), line, "{}", String::from_utf8_lossy(source));
    }
}

#[test]
fn metatable_faults() {
    for (source, fault) in [
        (
            &b"local t = {} setmetatable(t, { __index = t }) return t.missing"[..],
            LuaFault::MetaChain,
        ),
        (
            b"local t = {} setmetatable(t, { __newindex = t }) t.x = 1",
            LuaFault::MetaChain,
        ),
        (
            b"local t = setmetatable({}, { __metatable = 1 }) setmetatable(t, {})",
            LuaFault::Native,
        ),
        (b"setmetatable(1, {})", LuaFault::Argument),
        (b"setmetatable({}, 1)", LuaFault::Argument),
        (b"return #1", LuaFault::Length),
        (
            b"local t = setmetatable({}, { __len = {} }) return #t",
            LuaFault::BadCall,
        ),
        (
            b"local t = setmetatable({}, { __index = 1 }) return t.x",
            LuaFault::Index,
        ),
        (
            b"local t = setmetatable({}, { __index = function() local f f() end }) return t.x",
            LuaFault::BadCall,
        ),
        (b"rawget(1, 2)", LuaFault::Argument),
        (b"rawset({}, nil, 1)", LuaFault::Native),
    ] {
        let chunk = crate::compile(source).unwrap();
        let mut runtime = boot_natives(&chunk.proto);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::LuaError(fault),
            "{}",
            String::from_utf8_lossy(source)
        );
        // A faulted run still snapshots and restores cleanly.
        let bytes = runtime.snapshot().unwrap();
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), 1).unwrap();
    }
}

#[test]
fn metatables_are_gc_edges_and_cycles_die() {
    // Garbage is made inside a call, whose registers are cleared on return.
    let source = b"keep = setmetatable({}, {}) local make = function() local loose = {} setmetatable(loose, loose) local pair = setmetatable({}, setmetatable({}, {})) end make() return 1";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = finish_with(boot_natives, &chunk.proto);
    runtime.collect();
    // The globals table, `keep`, `keep`'s metatable, and the registry and
    // its `_LOADED` (ADR 0039).
    assert_eq!(runtime.heap().tables.live(), 5);
}

#[test]
fn shared_metatables_survive_restore_as_one_object() {
    let source = b"local mt = { __index = function() return 1 end } a = setmetatable({}, mt) b = setmetatable({}, mt) return 1";
    let chunk = crate::compile(source).unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    let restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    let heap = restored.heap();
    let globals = heap.globals.unwrap();
    let metatable_of = |name: &[u8]| {
        let crate::value::Value::Table(table) = heap
            .tables
            .get(globals)
            .unwrap()
            .table
            .get_view(crate::table::KeyView::string(name))
            .unwrap()
        else {
            panic!("not a table");
        };
        heap.tables
            .get(heap.tables.get(table).unwrap().metatable.unwrap())
            .unwrap()
            .id
    };
    assert_eq!(metatable_of(b"a"), metatable_of(b"b"));
}

#[test]
fn a_metamethod_continuation_fails_closed_when_tampered() {
    let source = b"local t = setmetatable({}, { __index = function(self, key) local a = 1 return key end }) return t.k";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = boot_natives(&chunk.proto);
    let mut journal = Journal::new();
    // Step until the metamethod's frame is running above the Index frame.
    loop {
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
        let heap = runtime.heap();
        let frames = &heap.threads.get(heap.active.unwrap()).unwrap().frames;
        if frames.len() == 2 {
            break;
        }
    }
    let meta = {
        let heap = runtime.heap();
        let frames = &heap.threads.get(heap.active.unwrap()).unwrap().frames;
        frames[0].meta().cloned().unwrap()
    };
    let crate::heap::MetaEvent::Store { dst } = meta.event else {
        panic!("not an index");
    };
    let bytes = runtime.snapshot().unwrap();
    let mut needle = vec![1u8, dst];
    needle.extend(meta.slot.to_le_bytes());
    needle.extend([2u8, 0]);
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("encoded continuation");
    let mut wrong_args = bytes.clone();
    wrong_args[at + 6] = 3;
    recrc(&mut wrong_args);
    expect_snapshot(
        Runtime::from_snapshot(&wrong_args, &HostRegistry::proof(), 1),
        SnapshotError::InvalidStructure,
    );
    let mut wrong_event = bytes.clone();
    wrong_event[at] = 4;
    recrc(&mut wrong_event);
    expect_snapshot(
        Runtime::from_snapshot(&wrong_event, &HostRegistry::proof(), 1),
        SnapshotError::InvalidStructure,
    );
    let mut low_slot = bytes.clone();
    low_slot[at + 2..at + 6].copy_from_slice(&0u32.to_le_bytes());
    recrc(&mut low_slot);
    expect_snapshot(
        Runtime::from_snapshot(&low_slot, &HostRegistry::proof(), 1),
        SnapshotError::InvalidStructure,
    );
    // Untouched, it restores and finishes.
    let mut restored = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), 1).unwrap();
    restored.run_until_terminal(u64::MAX, &mut journal).unwrap();
    assert_eq!(print_line(&restored), "k\n");
}

fn is_lua_border(present: &[i64], border: i64) -> bool {
    let has = |key: i64| present.contains(&key);
    let left = border == 0 || has(border);
    let right = border == i64::MAX || !has(border + 1);
    left && right
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_oracle_checks_sequences_exactly_and_borders_by_predicate() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54")
        .expect("MOONSEED_LUA54 must point at a Lua 5.4 binary");
    // Lua 5.4's stock Makefile defines `LUA_COMPAT_5_3`, which brings back
    // Lua 5.3's `__le`-from-`__lt` fallback. The oracle is the language the
    // 5.4 manual specifies, without it.
    let compat = lua_command(&lua)
        .arg("-e")
        .arg("local mt = { __lt = function() return true end } print(pcall(function() return setmetatable({}, mt) <= setmetatable({}, mt) end))")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&compat.stdout).starts_with("false"),
        "MOONSEED_LUA54 must be built without LUA_COMPAT_5_3 (remove -DLUA_COMPAT_5_3 from src/Makefile)"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let run = |file: &str| {
        let output = lua_command(&lua)
            .arg(root.join(file))
            .output()
            .unwrap_or_else(|error| panic!("run {lua}: {error}"));
        assert!(
            output.status.success(),
            "{file} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let closure =
        crate::hostcaps::native::test_support::read_to_string(root.join("closure_pair.lua"))
            .unwrap();
    let wrapper = format!("local a, b, c, d = (function()\n{closure}\nend)()\nprint(a, b, c, d)");
    let output = lua_command(&lua)
        .arg("-e")
        .arg(&wrapper)
        .output()
        .unwrap_or_else(|error| panic!("run {lua}: {error}"));
    assert!(
        output.status.success(),
        "closure fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "1\t1\t2\t2\n");
    for (file, want) in [
        ("branch_close.lua", "10\t11\t11\t99\n"),
        ("branch_threshold.lua", "3\t2\n"),
    ] {
        let body = crate::hostcaps::native::test_support::read_to_string(root.join(file)).unwrap();
        let output = lua_command(&lua)
            .arg("-e")
            .arg(format!("print((function()\n{body}\nend)())"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{file} failed");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), want, "{file}");
    }
    let lexical = r#"
local function check(src, want)
  local ok = load(src) ~= nil
  if ok ~= want then
    io.stderr:write("lexical mismatch\n")
    os.exit(1)
  end
end
check("return 3..4", false)
check("return 0x1.fp10", true)
check("return '\\u{7FFFFFFF}'", true)
check("return '\\u{80000000}'", false)
check("return [==", false)
print("lexical-ok")
"#;
    let output = lua_command(&lua).arg("-e").arg(lexical).output().unwrap();
    assert!(
        output.status.success(),
        "lexical oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "lexical-ok\n");
    for (name, line) in NATIVE_FIXTURES.iter().chain(DEEP_FIXTURES) {
        let body = crate::hostcaps::native::test_support::read_to_string(root.join(name)).unwrap();
        let output = lua_command(&lua)
            .arg("-e")
            .arg(format!(
                "{NATIVE_PRELUDE}print((function()\n{body}\nend)())"
            ))
            .output()
            .unwrap();
        assert!(output.status.success(), "{name} failed");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), *line, "{name}");
    }
    for (name, line) in CONTROL_FIXTURES {
        let body = crate::hostcaps::native::test_support::read_to_string(root.join(name)).unwrap();
        let output = lua_command(&lua)
            .arg("-e")
            .arg(format!("print((function()\n{body}\nend)())"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{name} failed");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), *line, "{name}");
    }
    use crate::value::Value;
    let numbers: &[(&str, Value)] = &[
        ("0", Value::Integer(0)),
        ("1", Value::Integer(1)),
        ("-1", Value::Integer(-1)),
        ("9007199254740992", Value::Integer(1 << 53)),
        ("9007199254740993", Value::Integer((1 << 53) + 1)),
        ("-9007199254740993", Value::Integer(-(1 << 53) - 1)),
        ("math.maxinteger", Value::Integer(i64::MAX)),
        ("math.mininteger", Value::Integer(i64::MIN)),
        ("0.0", Value::Float(0.0)),
        ("-0.0", Value::Float(-0.0)),
        ("1.5", Value::Float(1.5)),
        ("-1.5", Value::Float(-1.5)),
        ("9007199254740992.0", Value::Float(9_007_199_254_740_992.0)),
        (
            "9223372036854775808.0",
            Value::Float(9_223_372_036_854_775_808.0),
        ),
        (
            "-9223372036854775808.0",
            Value::Float(-9_223_372_036_854_775_808.0),
        ),
        ("1e300", Value::Float(1e300)),
        ("-1e300", Value::Float(-1e300)),
        ("(1/0)", Value::Float(f64::INFINITY)),
        ("(-1/0)", Value::Float(f64::NEG_INFINITY)),
        ("(0/0)", Value::Float(f64::NAN)),
    ];
    for (source, needle) in [
        ("for i = 1, 10, 0 do end", "'for' step is zero"),
        ("for i = 1.0, 10.0, -0.0 do end", "'for' step is zero"),
        ("for i = 1, 'abc' do end", "bad 'for' limit"),
        ("for i = 'x', 3 do end", "bad 'for' initial value"),
        ("for i = 1, 3, true do end", "bad 'for' step"),
        (
            "local t = {} setmetatable(t, { __index = t }) return t.x",
            "'__index' chain too long",
        ),
        (
            "local t = setmetatable({}, { __metatable = 1 }) setmetatable(t, {})",
            "cannot change a protected metatable",
        ),
        ("return 1 // 0", "attempt to divide by zero"),
        ("return 1 % 0", "attempt to perform 'n%0'"),
        (
            "local t = {} return t + 1",
            "attempt to perform arithmetic on a table value",
        ),
        (
            "return '3' & 1",
            "attempt to perform bitwise operation on a string value",
        ),
        ("return 1.5 & 1", "number has no integer representation"),
        ("return 1 .. {}", "attempt to concatenate a table value"),
        ("return 1 < '2'", "attempt to compare number with string"),
        (
            "local mt = { __lt = function() return true end } local a, b = setmetatable({}, mt), setmetatable({}, mt) return a <= b",
            "attempt to compare two table values",
        ),
        ("local t = {} return t()", "attempt to call a table value"),
        (
            "local f f = function(n) return f(n + 1) + 1 end return f(1)",
            "stack overflow",
        ),
        (
            "local t = setmetatable({}, { __add = 5 }) return t + 1",
            "attempt to call a number value",
        ),
    ] {
        let output = lua_command(&lua)
            .arg("-e")
            .arg(format!("print(select(2, pcall(load([[{source}]]))))"))
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(needle), "{source}: {text}");
    }
    let strings = [
        "3",
        " -0x10 ",
        "+5",
        "- 5",
        "1e",
        "0x",
        "inf",
        "nan",
        "1 2",
        "",
        "  ",
        ".5",
        "5.",
        "0x1p4",
        "-0x8000000000000000",
        "-9223372036854775808",
        "-09223372036854775808",
        "9223372036854775808",
        "99999999999999999999",
        "\t2.5\n",
        "0x10p-2",
        "1e-3",
        "  0x7fffffffffffffff",
        "0xffffffffffffffff",
        "1E+2",
    ];
    let mut script = String::new();
    for text in strings {
        script.push_str(&format!(
            "do local v = tonumber({text:?}) if v == nil then print('nil') elseif math.type(v) == 'integer' then print('i', v) else print('f', string.format('%.17g', v)) end end\n"
        ));
    }
    let output = lua_command(&lua).arg("-e").arg(&script).output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), strings.len());
    for (line, source) in text.lines().zip(strings) {
        let ours = crate::lex::string_to_number(source.as_bytes());
        let fields: Vec<&str> = line.split('\t').collect();
        match (fields.as_slice(), ours) {
            (["nil"], None) => {}
            (["i", value], Some(Value::Integer(integer))) => {
                assert_eq!(value.parse::<i64>().unwrap(), integer, "{source:?}");
            }
            (["f", value], Some(Value::Float(float))) => {
                assert_eq!(
                    value.parse::<f64>().unwrap().to_bits(),
                    float.to_bits(),
                    "{source:?}"
                );
            }
            other => panic!("{source:?}: lua {line}, moonseed {other:?}"),
        }
    }
    let heap = crate::heap::Heap::new();
    let flag = |bit: bool| if bit { 't' } else { 'f' };
    let mut script = String::from("local r = {}\n");
    let mut want = String::new();
    for (left, a) in numbers {
        for (right, b) in numbers {
            script.push_str(&format!(
                "r[#r+1] = (({left}) < ({right}) and 't' or 'f') .. (({left}) <= ({right}) and 't' or 'f') .. (({left}) == ({right}) and 't' or 'f')\n"
            ));
            want.push(flag(crate::compare::less_than(&heap, *a, *b).unwrap()));
            want.push(flag(crate::compare::less_equal(&heap, *a, *b).unwrap()));
            want.push(flag(crate::compare::equal(&heap, *a, *b)));
        }
    }
    script.push_str("io.write(table.concat(r))\n");
    let output = lua_command(&lua).arg("-e").arg(&script).output().unwrap();
    assert!(output.status.success(), "numeric comparison oracle failed");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), want);
    assert_eq!(run("table_sequence.lua"), "3 0\n");
    assert_eq!(run("table_next.lua"), "ok\n");
    let hole: i64 = run("table_hole.lua").trim().parse().unwrap();
    let missing: i64 = run("table_missing.lua").trim().parse().unwrap();
    assert!(is_lua_border(&[1, 3], hole), "lua hole border {hole}");
    assert!(
        is_lua_border(&[2, 3], missing),
        "lua missing border {missing}"
    );

    let proof = crate::program::table_semantics_program();
    let mut runtime = boot_spec(&proof.spec);
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let moon_hole = match observe_reg(&runtime, 19) {
        Observed::Int(value) => value,
        other => panic!("{other:?}"),
    };
    let moon_missing = match observe_reg(&runtime, 20) {
        Observed::Int(value) => value,
        other => panic!("{other:?}"),
    };
    assert!(is_lua_border(&[1, 3], moon_hole), "{moon_hole}");
    assert!(is_lua_border(&[2, 3], moon_missing), "{moon_missing}");
    assert_eq!(moon_hole, 1);
    assert_eq!(moon_missing, 0);
}

/// The existing cross-target GC fixture includes a restore midway. The mode
/// scope applies to boot and restore, while staying local to this test thread.
#[test]
fn fast_slow_gc_schedule_fingerprint() {
    use crate::runtime::HotCoreMode;
    let expected = crate::gc_schedule_fingerprint().unwrap();
    for mode in [
        HotCoreMode::Full,
        HotCoreMode::NoFastCalls,
        HotCoreMode::Off,
    ] {
        assert_eq!(mode.with(crate::gc_schedule_fingerprint).unwrap(), expected);
    }
}

/// A restored `top` past the stack's end (snapshot validation accepts it;
/// the slow path reads the missing slots as nil) must make the fast return
/// paths decline, not panic: every mode finishes with the same result
/// (Phase 3.32 review R1).
#[test]
fn open_return_with_restored_top_past_the_stack_declines() {
    use crate::opcode::{COUNT_OPEN, Op};
    use crate::runtime::HotCoreMode;
    let chunk = crate::compile(
        b"local function g() return 1, 2 end \
          local function f() local x = 5 return x, g() end \
          return select('#', f())",
    )
    .unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_standard().unwrap();
    let mut journal = Journal::new();
    loop {
        let heap = runtime.heap();
        let thread = heap.threads.get(heap.active.unwrap()).unwrap();
        let frame = thread.frames.last().unwrap();
        let proto = heap.closures.get(frame.closure).unwrap().proto;
        let op = heap.protos.get(proto).unwrap().ops.get(frame.pc as usize);
        if thread.frames.len() == 2
            && matches!(
                op,
                Some(Op::Return {
                    count: COUNT_OPEN,
                    ..
                })
            )
            && frame.pending().is_none()
            && frame.meta().is_none()
        {
            break;
        }
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    let mut image = runtime.to_image().unwrap();
    let active = runtime.heap().threads.get(runtime.heap().active.unwrap());
    let id = active.unwrap().id.raw();
    let thread = image.threads.iter_mut().find(|t| t.id == id).unwrap();
    thread.top = thread.stack.len() as u32 + 1;
    let bytes = snapshot::encode(&image).unwrap();
    let mut results = Vec::new();
    for mode in [
        HotCoreMode::Off,
        HotCoreMode::NoFastCalls,
        HotCoreMode::Full,
    ] {
        let mut restored =
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        restored.hot_core = mode;
        let outcome = restored
            .run_until_terminal(1_000, &mut Journal::new())
            .unwrap();
        results.push((outcome, print_line(&restored)));
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0], results[2]);
}

/// A restored frame carries cold storage exactly when its image has an
/// exceptional field (C1): the plain frames come back with `cold == None`,
/// the `pcall` boundary with its boundary, and the bytes round-trip.
#[test]
fn restored_frames_hold_cold_storage_only_where_the_image_does() {
    let chunk = crate::compile(
        b"local function f() local x = 1 return x + 1 end \
          return pcall(f)",
    )
    .unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    runtime.install_standard().unwrap();
    let mut journal = Journal::new();
    loop {
        let heap = runtime.heap();
        let thread = heap.threads.get(heap.active.unwrap()).unwrap();
        if thread.frames.len() == 3 && thread.frames[1].boundary().is_some() {
            break;
        }
        assert!(matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
    }
    let bytes = runtime.snapshot().unwrap();
    let restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    let heap = restored.heap();
    let frames = &heap.threads.get(heap.active.unwrap()).unwrap().frames;
    assert_eq!(frames.len(), 3);
    assert!(frames[0].cold.is_none());
    assert!(matches!(
        frames[1].boundary(),
        Some(crate::heap::Boundary::Protect { .. })
    ));
    assert!(frames[2].cold.is_none());
    assert!(
        frames
            .iter()
            .all(|frame| frame.cold.as_ref().is_none_or(|cold| !cold.is_empty()))
    );
    assert_eq!(restored.snapshot().unwrap(), bytes);
}

/// `__call`, metamethod and library-callback entries go through the same
/// frame builder as a plain call: their frame and stack images match the
/// slow tier at every boundary (Gate Q).
#[test]
fn fast_slow_builder_entries_match() {
    let chunk = crate::compile(
        br#"
        local calls = 0
        local callable = setmetatable({}, {
            __call = function(self, a, b) calls = calls + 1 return a + b end,
        })
        local mt = {
            __add = function(x, y) return x.v + y.v end,
            __index = function(t, k) return #k end,
        }
        local a, b = setmetatable({ v = 1 }, mt), setmetatable({ v = 2 }, mt)
        local sum = 0
        for i = 1, 6 do
            sum = sum + callable(i, 1) + (a + b) + a.key
            local t = { 3, 1, 2 }
            table.sort(t, function(x, y) return x < y end)
            local _, n = string.gsub("abc", "%w", function(c) return c end)
            sum = sum + t[1] + n
        end
        return sum, calls
        "#,
    )
    .unwrap();
    fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);

    // The metamethod builder at its edges: the three tiers agree on outcome,
    // fuel and the final image on both sides of each edge.
    let tiers_agree = |spec: &crate::program::ProtoSpec, config: &Config| -> String {
        use crate::runtime::HotCoreMode;
        let runs = [
            HotCoreMode::Full,
            HotCoreMode::NoFastCalls,
            HotCoreMode::Off,
        ]
        .map(|mode| {
            let mut runtime =
                Runtime::boot(config.clone(), HostRegistry::proof(), spec, false).unwrap();
            if runtime.install_standard().is_err() {
                return (String::from("install"), 0, None);
            }
            runtime.hot_core = mode;
            let outcome = runtime.run_until_terminal(u64::MAX, &mut Journal::new());
            let image = runtime.snapshot().ok();
            (format!("{outcome:?}"), runtime.fuel_consumed(), image)
        });
        assert_eq!(runs[0], runs[1]);
        assert_eq!(runs[0], runs[2]);
        runs[0].0.clone()
    };
    // Depth: the metamethod's frame is the last ordinary one, or one too many.
    let depth = crate::compile(
        b"local a = setmetatable({}, {__add = function(x, y) return 1 end}) \
          local function rec(n) if n == 0 then return a + a end local r = rec(n - 1) return r end \
          local ok = {} \
          for d = 990, 1000 do ok[#ok + 1] = pcall(rec, d) end \
          return table.unpack(ok)",
    )
    .unwrap();
    assert!(tiers_agree(&depth.proto, &Config::default()).contains("Completed"));
    let mut runtime = boot_natives(&depth.proto);
    runtime.install_standard().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let results = pair_results(&runtime);
    assert!(results.contains(&Observed::Bool(true)) && results.contains(&Observed::Bool(false)));
    // Quota: from the final heap upwards, slot by slot; below the edge the
    // three-slot scratch window fits and the metamethod's 60-register window
    // does not.
    let pad = (1..=60)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let quota = crate::compile(
        format!(
            "local a = setmetatable({{}}, {{__add = function(x, y) local {pad} return 1 end}}) \
             local s = 0 for i = 1, 3 do s = s + (a + a) end return s"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut runtime = Runtime::boot(
        Config::default(),
        HostRegistry::proof(),
        &quota.proto,
        false,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    let used = runtime.memory().logical_bytes;
    let (mut completed, mut refused) = (false, false);
    for slots in 0..=200 {
        let config = Config {
            max_logical_heap: used + 16 * slots,
            ..Config::default()
        };
        match tiers_agree(&quota.proto, &config).as_str() {
            "install" => {}
            outcome if outcome.contains("Completed") => completed = true,
            _ => refused = true,
        }
    }
    assert!(completed && refused);
}

#[test]
fn execution_hints_are_disposable_across_restore_and_metatables() {
    let chunk = crate::compile(
        br#"
        local nan = { x = 0 / 0 }
        for i = 1, 3 do assert(nan.x ~= nan.x) end
        local a = { x = 1, y = 2, z = 3 }
        local b = { z = 3, y = 2, x = 1 }
        local calls = 0
        local mt = {
            __index = function() calls = calls + 1 return 7 end,
            __newindex = function(t, k, v) calls = calls + 1 rawset(t, k, v) end
        }
        local function field(t, v) t.x = v return t.x end
        local sum = 0
        for i = 1, 12 do
            local t = i % 2 == 0 and a or b
            sum = sum + field(t, i)
            t.x = nil
            t.y = nil
            t.z = nil
            setmetatable(t, mt)
            sum = sum + t.x
            sum = sum + field(t, i + 1)
            setmetatable(t, { __index = { x = 99 + i }, __newindex = {} })
            sum = sum + field(t, i + 2)
            t.x = nil
            sum = sum + t.x
            t.x = i
            sum = sum + t.x
            setmetatable(t, nil)
            t.y = i
            t.z = i
        end
        return sum, calls
        "#,
    )
    .unwrap();
    fast_slow_equivalent(|| boot_natives(&chunk.proto), pair_results);
    let mut warm = boot_natives(&chunk.proto);
    let mut cleared = boot_natives(&chunk.proto);
    let mut warm_journal = Journal::new();
    let mut cleared_journal = Journal::new();
    let mut populated = false;
    let mut event_populated = false;
    loop {
        for hint in &cleared.heap().event_hints {
            hint.set(u32::MAX);
        }
        for (_, _, proto) in cleared.heap().protos.iter() {
            for hint in &proto.field_hints {
                hint.name.set(u32::MAX);
                hint.index.set(u32::MAX);
            }
        }
        let outcome = warm.run(29, &mut warm_journal).unwrap();
        assert_eq!(outcome, cleared.run(29, &mut cleared_journal).unwrap());
        event_populated |= warm
            .heap()
            .event_hints
            .iter()
            .any(|hint| hint.get() != u32::MAX);
        populated |= warm.heap().protos.iter().any(|(_, _, proto)| {
            proto
                .field_hints
                .iter()
                .any(|hint| hint.name.get() != u32::MAX || hint.index.get() != u32::MAX)
        });
        let bytes = warm.snapshot().unwrap();
        assert_eq!(bytes, cleared.snapshot().unwrap());
        assert_eq!(warm.fuel_consumed(), cleared.fuel_consumed());
        assert_eq!(warm.memory(), cleared.memory());
        assert_eq!(warm.gc_log, cleared.gc_log);
        // Restoring drops all hints while keeping the exact published state.
        cleared = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), 1).unwrap();
        assert!(
            cleared
                .heap()
                .event_hints
                .iter()
                .all(|hint| hint.get() == u32::MAX)
        );
        assert!(cleared.heap().protos.iter().all(|(_, _, proto)| {
            proto
                .field_hints
                .iter()
                .all(|hint| hint.name.get() == u32::MAX && hint.index.get() == u32::MAX)
        }));
        assert_eq!(bytes, cleared.snapshot().unwrap());
        if !matches!(outcome, StepOutcome::Paused(_)) {
            assert_eq!(outcome, StepOutcome::Completed);
            break;
        }
    }
    assert!(populated, "exercise populated instruction hints");
    assert!(event_populated, "exercise populated event hints");
}

/// A legitimate restored frame can have an implicit nil tail. A Jump must
/// leave that tail implicit; a Move extends only through its destination.
#[test]
fn fast_slow_restored_short_stack() {
    use crate::opcode::Op;
    use crate::program::ProtoSpec;
    let spec = ProtoSpec {
        max_reg: 8,
        ops: vec![
            Op::Jump { offset: 0 },
            Op::Move { dst: 3, src: 7 },
            Op::LoadInt { dst: 1, value: 42 },
            Op::Return { base: 1, count: 3 },
        ],
        byte_consts: Vec::new(),
        captures: Vec::new(),
        children: Vec::new(),
        params: 0,
        vararg: false,
        debug: None,
    };
    let runtime = boot_spec(&spec);
    let mut image = runtime.to_image().unwrap();
    let thread = image
        .threads
        .iter_mut()
        .find(|t| !t.frames.is_empty())
        .unwrap();
    thread.stack.truncate(1);
    thread.top = 1;
    let bytes = snapshot::encode(&image).unwrap();
    let boot =
        || Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    fast_slow_equivalent(boot, pair_results);
    let mut restored = boot();
    let mut journal = Journal::new();
    restored.run(1, &mut journal).unwrap();
    let thread = restored
        .heap()
        .threads
        .get(restored.heap().active.unwrap())
        .unwrap();
    assert_eq!(thread.stack.len(), 1, "Jump must not pad the stack");
    restored.run(1, &mut journal).unwrap();
    let thread = restored
        .heap()
        .threads
        .get(restored.heap().active.unwrap())
        .unwrap();
    assert_eq!(thread.stack.len(), 4, "Move grows only to its destination");
    assert_eq!(thread.stack[3], crate::value::Value::Nil);
}

/// A restored image whose callee is complete but whose caller's window
/// ends past the stack: the in-loop frame switch would land on a short
/// window, so the fast return declines before any write and the slow path
/// returns, matching the other tiers at every boundary.
#[test]
fn fast_return_declines_into_a_restored_short_caller() {
    let pad = (1..=24)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("local function f() return 7 end local r = f() local {pad} = 1 return r");
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = boot_spec(&chunk.proto);
    let mut journal = Journal::new();
    loop {
        let heap = runtime.heap();
        let thread = heap.threads.get(heap.active.unwrap()).unwrap();
        if let [_, callee] = &thread.frames[..] {
            let closure = heap.closures.get(callee.closure).unwrap();
            let proto = heap.protos.get(closure.proto).unwrap();
            if matches!(
                proto.ops[callee.pc as usize],
                crate::opcode::Op::Return { .. }
            ) {
                break;
            }
        }
        runtime.run(1, &mut journal).unwrap();
    }
    let mut image = runtime.to_image().unwrap();
    let thread = image
        .threads
        .iter_mut()
        .find(|t| t.frames.len() == 2)
        .unwrap();
    let callee_limit = thread.frames[1].limit;
    assert!(thread.frames[0].limit > callee_limit);
    thread.stack.truncate(callee_limit as usize);
    thread.top = thread.top.min(callee_limit);
    let bytes = snapshot::encode(&image).unwrap();
    let boot =
        || Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
    fast_slow_equivalent(boot, pair_results);
    let mut restored = boot();
    let cold = restored.cold_steps;
    restored.run(1, &mut journal).unwrap();
    assert_eq!(
        restored.cold_steps,
        cold + 1,
        "the return ran on the slow path"
    );
    let heap = restored.heap();
    assert_eq!(
        heap.threads.get(heap.active.unwrap()).unwrap().frames.len(),
        1
    );
}
