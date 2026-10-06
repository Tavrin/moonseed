//! Unstable measurement helpers. Internal development scaffolding; outside the supported feature/API contract.
//!
//! Numbers from these helpers are evidence about the Phase-1 representation.
//! They are not a performance target.

use crate::hostcaps::native::Instant;
use std::hint::black_box;

use crate::heap::{Frame, Heap};
use crate::host::{HostCtx, HostRegistry, HostResult, Journal};
use crate::id::{Handle, StepOutcome};
use crate::program;
use crate::runtime::{Config, Runtime};
use crate::table::{Table, TableKey};
use crate::value::Value;

/// Write an unstable, compile-only listing of every prototype in a chunk.
/// PCs and constant indexes are zero-based; numeric literals live in ops.
/// Byte strings use Rust byte-array notation to preserve arbitrary Lua bytes.
pub fn write_listing(
    chunk: &crate::CompiledChunk,
    output: &mut impl std::io::Write,
) -> std::io::Result<()> {
    fn walk(
        proto: &program::ProtoSpec,
        path: &str,
        output: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        writeln!(
            output,
            "prototype {path}: ops={} max_stack={} params={} vararg={}",
            proto.ops.len(),
            proto.max_reg,
            proto.params,
            proto.vararg
        )?;
        for (pc, op) in proto.ops.iter().enumerate() {
            let line = proto.debug.as_ref().and_then(|debug| debug.lines.get(pc));
            writeln!(output, "  {pc:04} [{}] {op:?}", line.copied().unwrap_or(0))?;
        }
        writeln!(output, "constants (byte strings; other literals inline):")?;
        for (index, bytes) in proto.byte_consts.iter().enumerate() {
            writeln!(output, "  K{index} {bytes:?}")?;
        }
        writeln!(output, "upvalues:")?;
        for (index, capture) in proto.captures.iter().enumerate() {
            let name = proto
                .debug
                .as_ref()
                .and_then(|debug| debug.upvalues.get(index));
            writeln!(output, "  U{index} {capture:?} name={name:?}")?;
        }
        for (index, child) in proto.children.iter().enumerate() {
            walk(child, &format!("{path}.{index}"), output)?;
        }
        Ok(())
    }
    walk(&chunk.proto, "0", output)
}

#[doc(hidden)] // Unstable benchmark helper; outside the embedding API.
pub struct Sample {
    pub name: &'static str,
    pub ns_per_op: u128,
    pub detail: String,
}

/// `MOONSEED_BENCH_ONLY=a,b,c` limits a run to the named loop workloads,
/// for quick comparisons between builds. A trailing `_` selects a prefix.
/// Unset, everything runs.
fn only() -> Option<Vec<String>> {
    let names = crate::hostcaps::native::measurement_env("MOONSEED_BENCH_ONLY").ok()?;
    Some(names.split(',').map(str::to_string).collect())
}

fn wanted(name: &str) -> bool {
    only().is_none_or(|names| {
        names
            .iter()
            .any(|want| want == name || (want.ends_with('_') && name.starts_with(want)))
    })
}

const SKIPPED: &str = "skipped";

#[doc(hidden)] // Unstable benchmark helper; outside the embedding API.
pub fn collect() -> Vec<Sample> {
    if only().is_some() {
        let mut samples = workloads();
        samples.extend(control_flow());
        samples.extend(native_calls());
        samples.extend(embedding_native_rows());
        samples.extend(embedding_rows());
        samples.extend(error_rows());
        samples.extend(close_rows());
        samples.extend(generic_for_rows());
        samples.extend(vararg_rows());
        samples.extend(tail_rows());
        samples.extend(syntax_rows());
        samples.extend(goto_rows());
        samples.extend(base_rows());
        samples.extend(library_rows());
        samples.extend(string_rows());
        samples.extend(debug_rows());
        samples.extend(coroutine_rows());
        samples.extend(userdata_rows());
        samples.extend(gc_rows());
        samples.extend(gc_semantics_rows());
        samples.extend(incremental_rows());
        samples.extend(scale_gc_rows());
        samples.extend(id_lookup_rows());
        samples.retain(|sample| sample.detail != SKIPPED);
        return samples;
    }
    let mut samples = Vec::new();
    samples.push(Sample {
        name: "value_size",
        ns_per_op: 0,
        detail: format!(
            "size_of Value={} Handle<u8>={} Frame={} Op={} align Value={} Op={}",
            std::mem::size_of::<Value>(),
            std::mem::size_of::<Handle<u8>>(),
            std::mem::size_of::<Frame>(),
            std::mem::size_of::<crate::opcode::Op>(),
            std::mem::align_of::<Value>(),
            std::mem::align_of::<crate::opcode::Op>()
        ),
    });
    samples.push(handle_lookup());
    samples.extend(id_lookup_rows());
    samples.push(table_ops());
    samples.extend(traversal_ops());
    samples.push(dispatch());
    samples.push(snapshot_cost());
    samples.extend(anchor_snapshots());
    samples.extend(workloads());
    samples.extend(frontend());
    samples.extend(branches_and_close());
    samples.extend(control_flow());
    samples.extend(native_calls());
    samples.extend(embedding_native_rows());
    samples.extend(embedding_rows());
    samples.extend(error_rows());
    samples.extend(close_rows());
    samples.extend(generic_for_rows());
    samples.extend(vararg_rows());
    samples.extend(tail_rows());
    samples.extend(syntax_rows());
    samples.extend(goto_rows());
    samples.extend(base_rows());
    samples.extend(library_rows());
    samples.extend(string_rows());
    samples.extend(debug_rows());
    samples.extend(coroutine_rows());
    samples.extend(userdata_rows());
    samples.extend(gc_rows());
    samples.extend(gc_semantics_rows());
    samples.extend(incremental_rows());
    samples.extend(scale_gc_rows());
    samples
}

fn handle_lookup() -> Sample {
    let mut heap = Heap::new();
    let mut handles = Vec::with_capacity(1024);
    for index in 0..1024 {
        handles.push(heap.alloc_string(vec![index as u8]).unwrap());
    }
    let iters = 20_000u32;
    let ops = u128::from(iters) * handles.len() as u128;
    let start = Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        for handle in &handles {
            let index = black_box(handle.index);
            acc = acc.wrapping_add(
                heap.strings
                    .slot_value(index)
                    .map(|object| object.bytes.len())
                    .unwrap_or(0),
            );
        }
    }
    let raw_elapsed = start.elapsed().as_nanos();
    black_box(acc);
    let start = Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        for handle in &handles {
            let handle = black_box(*handle);
            acc = acc.wrapping_add(
                heap.strings
                    .get(handle)
                    .map(|object| object.bytes.len())
                    .unwrap_or(0),
            );
        }
    }
    let checked_elapsed = start.elapsed().as_nanos();
    black_box(acc);
    let ratio = checked_elapsed as f64 / raw_elapsed.max(1) as f64;
    Sample {
        name: "handle_lookup",
        ns_per_op: checked_elapsed / ops,
        detail: format!(
            "raw {raw_elapsed} ns, checked {checked_elapsed} ns, {ops} lookups, ratio {ratio:.2}"
        ),
    }
}

fn table_ops() -> Sample {
    let mut table = Table::new();
    let n = 1_000u32;
    let start = Instant::now();
    for key in 0..n {
        table.insert(
            TableKey::Integer(i64::from(key)),
            Value::Integer(i64::from(key)),
            Value::Integer(1),
        );
    }
    let insert_ns = start.elapsed().as_nanos() / u128::from(n);
    let start = Instant::now();
    let mut acc = 0i64;
    for key in 0..n {
        if let Value::Integer(value) = table.get(&TableKey::Integer(i64::from(key))) {
            acc += value;
        }
    }
    black_box(acc);
    let get_ns = start.elapsed().as_nanos() / u128::from(n);
    let start = Instant::now();
    for key in 0..n {
        table.insert(
            TableKey::Integer(i64::from(key)),
            Value::Integer(i64::from(key)),
            Value::Integer(2),
        );
    }
    let update_ns = start.elapsed().as_nanos() / u128::from(n);
    assert_eq!(table.live_len(), n as usize);
    assert_eq!(table.dead_len(), 0);
    let start = Instant::now();
    for key in 0..n {
        table.insert(
            TableKey::Integer(i64::from(key)),
            Value::Integer(i64::from(key)),
            Value::Nil,
        );
    }
    let delete_ns = start.elapsed().as_nanos() / u128::from(n);
    assert_eq!(table.live_len(), 0);
    assert_eq!(table.dead_len(), n as usize);
    assert_eq!(table.slot_len(), n as usize);
    Sample {
        name: "table_1k_integers",
        ns_per_op: get_ns,
        detail: format!(
            "insert {insert_ns} ns, get {get_ns} ns, update {update_ns} ns, delete {delete_ns} ns, dead anchors {}",
            table.dead_len()
        ),
    }
}

fn put_int(table: &mut Table, key: i64) {
    table.insert(
        TableKey::Integer(key),
        Value::Integer(key),
        Value::Integer(1),
    );
}

fn time_next(table: &Table, start: Option<TableKey>, steps: u64) -> u128 {
    let repeats = 20u32;
    let mut samples = Vec::with_capacity(repeats as usize);
    for _ in 0..repeats {
        let began = Instant::now();
        let mut cursor = start.clone();
        let mut seen = 0u64;
        loop {
            let found = table.next(cursor.as_ref()).expect("valid key");
            let Some((Value::Integer(key), _)) = found else {
                break;
            };
            cursor = Some(TableKey::Integer(key));
            seen += 1;
            if steps == 1 {
                break;
            }
        }
        samples.push(began.elapsed().as_nanos());
        assert_eq!(seen, steps);
    }
    samples.sort_unstable();
    samples[samples.len() / 2] / u128::from(steps.max(1))
}

fn traversal_ops() -> Vec<Sample> {
    let n = 4_096i64;
    let mut live = Table::new();
    for key in 1..=n {
        put_int(&mut live, key);
    }
    let walk = time_next(&live, None, n as u64);
    let mut deleted_current = live.clone();
    deleted_current.insert(TableKey::Integer(1), Value::Integer(1), Value::Nil);
    let after_delete = time_next(&deleted_current, Some(TableKey::Integer(1)), 1);
    assert_eq!(deleted_current.dead_len(), 1);

    let mut tombstones = Table::new();
    for key in 1..=n {
        put_int(&mut tombstones, key);
    }
    for key in 1..n {
        tombstones.insert(TableKey::Integer(key), Value::Integer(key), Value::Nil);
    }
    let across = time_next(&tombstones, Some(TableKey::Integer(1)), 1);
    assert_eq!(tombstones.dead_len(), (n - 1) as usize);
    assert_eq!(tombstones.live_len(), 1);

    let mut sequence = Table::new();
    for key in 1..=1_024 {
        put_int(&mut sequence, key);
    }
    let border_repeats = 1_000u32;
    let start = Instant::now();
    for _ in 0..border_repeats {
        assert_eq!(black_box(sequence.raw_border()), 1_024);
    }
    let sequence_ns = start.elapsed().as_nanos() / u128::from(border_repeats);

    let mut hole = Table::new();
    put_int(&mut hole, 1);
    put_int(&mut hole, 3);
    let start = Instant::now();
    for _ in 0..border_repeats {
        assert_eq!(black_box(hole.raw_border()), 1);
    }
    let hole_ns = start.elapsed().as_nanos() / u128::from(border_repeats);

    let mut sparse = Table::new();
    put_int(&mut sparse, 1_000_000_000_000);
    assert_eq!(sparse.slot_len(), 1);
    let start = Instant::now();
    for _ in 0..border_repeats {
        assert_eq!(black_box(sparse.raw_border()), 0);
    }
    let sparse_ns = start.elapsed().as_nanos() / u128::from(border_repeats);

    vec![
        Sample {
            name: "next_live",
            ns_per_op: walk,
            detail: format!("{n} live entries, per step"),
        },
        Sample {
            name: "next_after_delete",
            ns_per_op: after_delete,
            detail: format!(
                "successor of the deleted first key, dead {}",
                deleted_current.dead_len()
            ),
        },
        Sample {
            name: "next_tombstones",
            ns_per_op: across,
            detail: format!(
                "one next across {} dead anchors to the last live key",
                tombstones.dead_len()
            ),
        },
        Sample {
            name: "raw_border_sequence",
            ns_per_op: sequence_ns,
            detail: "1024-key sequence, border 1024".to_string(),
        },
        Sample {
            name: "raw_border_hole",
            ns_per_op: hole_ns,
            detail: "keys 1 and 3, smallest border 1".to_string(),
        },
        Sample {
            name: "raw_border_sparse",
            ns_per_op: sparse_ns,
            detail: format!(
                "key 1000000000000 only, border 0, slots {}",
                sparse.slot_len()
            ),
        },
    ]
}

fn anchor_snapshots() -> Vec<Sample> {
    [false, true]
        .into_iter()
        .map(|delete_all| {
            let spec = program::filled_table(1_000, delete_all);
            let mut runtime = Runtime::boot(Config::default(), HostRegistry::proof(), &spec, false)
                .expect("boot");
            let mut journal = Journal::new();
            runtime
                .run_until_terminal(u64::MAX, &mut journal)
                .expect("run");
            assert_eq!(runtime.entry_slot(1).unwrap(), Value::Integer(1_000));
            let handle = match runtime.entry_slot(0).unwrap() {
                Value::Table(handle) => handle,
                _ => panic!("register 0 is not a table"),
            };
            let (live, dead, slots) = {
                let object = runtime.heap().tables.get(handle).unwrap();
                (object.live_len(), object.dead_len(), object.table.slot_len())
            };
            let runs = 20u32;
            let start = Instant::now();
            let mut bytes = Vec::new();
            for _ in 0..runs {
                bytes = runtime.snapshot().unwrap();
            }
            let encode_ns = start.elapsed().as_nanos() / u128::from(runs);
            let start = Instant::now();
            for _ in 0..runs {
                let restored = Runtime::from_snapshot(
                    &bytes,
                    &HostRegistry::proof(),
                    runtime.effect_domain(),
                )
                .unwrap();
                black_box(restored.fuel_consumed());
            }
            let decode_ns = start.elapsed().as_nanos() / u128::from(runs);
            Sample {
                name: if delete_all {
                    "snapshot_dead_anchors"
                } else {
                    "snapshot_live_table"
                },
                ns_per_op: encode_ns,
                detail: format!(
                    "{} bytes, live {live}, dead {dead}, slots {slots}, encode {encode_ns} ns, decode {decode_ns} ns",
                    bytes.len()
                ),
            }
        })
        .collect()
}

fn dispatch() -> Sample {
    let runs = 200u32;
    let start = Instant::now();
    let mut fuel = 0u64;
    for _ in 0..runs {
        let mut runtime =
            Runtime::boot_canonical(Config::default(), HostRegistry::proof()).unwrap();
        let mut journal = Journal::new();
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        fuel = runtime.fuel_consumed();
    }
    let per_run = start.elapsed().as_nanos() / u128::from(runs);
    let per_op = if fuel == 0 {
        0
    } else {
        per_run / u128::from(fuel)
    };
    Sample {
        name: "canonical_dispatch",
        ns_per_op: per_op,
        detail: format!("{per_run} ns/run, {fuel} charged instructions, includes alloc"),
    }
}

fn snapshot_cost() -> Sample {
    let mut runtime = Runtime::boot_canonical(Config::default(), HostRegistry::proof()).unwrap();
    let mut journal = Journal::new();
    runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
    let objects = runtime.heap().live_objects();
    let runs = 200u32;
    let start = Instant::now();
    let mut bytes = Vec::new();
    for _ in 0..runs {
        bytes = runtime.snapshot().unwrap();
    }
    let encode_ns = start.elapsed().as_nanos() / u128::from(runs);
    let start = Instant::now();
    for _ in 0..runs {
        let restored =
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                .unwrap();
        black_box(restored.fuel_consumed());
    }
    let decode_ns = start.elapsed().as_nanos() / u128::from(runs);
    let per_object = if objects == 0 {
        0
    } else {
        bytes.len() / objects as usize
    };
    Sample {
        name: "snapshot",
        ns_per_op: encode_ns,
        detail: format!(
            "{} bytes, {} live objects, ~{per_object} bytes/object, encode {encode_ns} ns, decode {decode_ns} ns",
            bytes.len(),
            objects
        ),
    }
}

fn tick(_ctx: &mut HostCtx<'_>, arg: i64) -> HostResult {
    HostResult::Ready(arg.wrapping_add(1))
}

fn tick_registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    registry.register("tick", tick);
    registry
}

struct LoopJob<'a> {
    name: &'static str,
    spec: &'a crate::program::ProtoSpec,
    iters: u64,
    reg: u8,
    expected: i64,
    registry: &'a HostRegistry,
    collect_between: bool,
    quantum: u64,
}

/// Warm up once, then time repeated runs. Rewind and any collection sit
/// outside the timer. The register check runs before the first timed run
/// and again after the last, so a shorter program cannot win.
fn time_loop(job: LoopJob<'_>) -> Sample {
    if !wanted(job.name) {
        return Sample {
            name: job.name,
            ns_per_op: 0,
            detail: SKIPPED.to_string(),
        };
    }
    let LoopJob {
        name,
        spec,
        iters,
        reg,
        expected,
        registry,
        collect_between,
        quantum,
    } = job;
    let mut runtime =
        Runtime::boot(Config::default(), registry.clone(), spec, false).expect("boot workload");
    let mut journal = Journal::new();
    runtime
        .run_until_terminal(u64::MAX, &mut journal)
        .expect("warmup");
    assert_eq!(
        runtime.entry_slot(reg).expect("slot"),
        Value::Integer(expected),
        "{name} warmup"
    );
    let repeats = 20u32;
    let mut samples = Vec::with_capacity(repeats as usize);
    for _ in 0..repeats {
        runtime.rewind_entry();
        if collect_between {
            runtime.collect();
        }
        let start = Instant::now();
        let outcome = runtime
            .run_until_terminal(quantum, &mut journal)
            .expect("run");
        samples.push(start.elapsed().as_nanos());
        assert!(
            matches!(outcome, StepOutcome::Completed),
            "{name} {outcome:?}"
        );
    }
    assert_eq!(
        runtime.entry_slot(reg).expect("slot"),
        Value::Integer(expected),
        "{name} timed"
    );
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let per = median / u128::from(iters.max(1));
    Sample {
        name,
        ns_per_op: per,
        detail: format!(
            "median {median} ns/run, min {} max {}, iters {iters}, fuel {}, stack_grows {}, frame_grows {}, quantum {}",
            samples[0],
            samples[samples.len() - 1],
            runtime.fuel_consumed(),
            runtime.stack_grows,
            runtime.frame_grows,
            if quantum == u64::MAX {
                "unbounded"
            } else {
                "1"
            },
        ),
    }
}

fn workloads() -> Vec<Sample> {
    let proof = HostRegistry::proof();
    let ticks = tick_registry();
    let n = 100_000i64;
    let calls = 20_000i64;
    let adds = program::counted_adds(n);
    let branches = program::branchy(n);
    let scalar = program::scalar_calls(calls);
    let nested = program::nested_calls(calls);
    let multi = program::multi_calls(calls);
    let upvalue = program::upvalue_calls(calls);
    let strings = program::string_fields(calls);
    let ints = program::int_fields(calls);
    let churn = program::alloc_churn(2_000);
    let host = program::host_ticks(calls);
    let tiny = program::counted_adds(20_000);
    let samples = vec![
        time_loop(LoopJob {
            name: "arith_loop",
            spec: &adds,
            iters: n as u64,
            reg: 0,
            expected: n,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "branch_loop",
            spec: &branches,
            iters: n as u64,
            reg: 0,
            expected: n,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "scalar_call",
            spec: &scalar,
            iters: calls as u64,
            reg: 1,
            expected: calls,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "nested_call",
            spec: &nested,
            iters: calls as u64,
            reg: 2,
            expected: calls,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "multi_call",
            spec: &multi,
            iters: calls as u64,
            reg: 1,
            expected: calls * 40,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "upvalue_call",
            spec: &upvalue,
            iters: calls as u64,
            reg: 0,
            expected: calls,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "string_field",
            spec: &strings,
            iters: calls as u64,
            reg: 2,
            expected: calls,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "int_field",
            spec: &ints,
            iters: calls as u64,
            reg: 2,
            expected: calls,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "alloc_churn",
            spec: &churn,
            iters: 2_000,
            reg: 1,
            expected: 2_000,
            registry: &proof,
            collect_between: true,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "host_call",
            spec: &host,
            iters: calls as u64,
            reg: 0,
            expected: calls,
            registry: &ticks,
            collect_between: false,
            quantum: u64::MAX,
        }),
        time_loop(LoopJob {
            name: "arith_quantum_1",
            spec: &tiny,
            iters: 20_000,
            reg: 0,
            expected: 20_000,
            registry: &proof,
            collect_between: false,
            quantum: 1,
        }),
    ];
    for name in ["scalar_call", "nested_call", "multi_call"] {
        let sample = samples
            .iter()
            .find(|sample| sample.name == name)
            .expect(name);
        assert!(
            sample.detail == SKIPPED
                || (sample.detail.contains("stack_grows 0")
                    && sample.detail.contains("frame_grows 0")),
            "warmed {name} allocated: {}",
            sample.detail
        );
    }
    samples
}

fn frontend() -> Vec<Sample> {
    let source = include_bytes!("../fixtures/lua/closure_pair.lua");
    let chunk = crate::compile(source).expect("compile closure fixture");
    let hand = program::closure_pair_hand();
    let lex_ns = median_ns(
        (0..200)
            .map(|_| {
                let start = Instant::now();
                let tokens = crate::lex::tokenize(black_box(source)).expect("lex");
                black_box(tokens.len());
                start.elapsed().as_nanos()
            })
            .collect(),
    );
    let compile_ns = median_ns(
        (0..200)
            .map(|_| {
                let start = Instant::now();
                let compiled = crate::compile(black_box(source)).expect("compile");
                black_box(compiled.instruction_count());
                start.elapsed().as_nanos()
            })
            .collect(),
    );
    let compiled_run = time_fresh(&chunk.proto, &[1, 1, 2, 2]);
    let hand_run = time_fresh(&hand, &[1, 1, 2, 2]);
    vec![
        Sample {
            name: "lex_fixture",
            ns_per_op: lex_ns,
            detail: format!("bytes {}", source.len()),
        },
        Sample {
            name: "compile_fixture",
            ns_per_op: compile_ns,
            detail: format!(
                "protos {} instructions {} max_reg {} mapped {}",
                chunk.prototype_count(),
                chunk.instruction_count(),
                chunk.max_registers(),
                chunk.mapped_instructions()
            ),
        },
        Sample {
            name: "exec_compiled_pair",
            ns_per_op: compiled_run.0,
            detail: format!(
                "fresh runtime, boot outside the timer, fuel {}",
                compiled_run.1
            ),
        },
        Sample {
            name: "exec_hand_pair",
            ns_per_op: hand_run.0,
            detail: format!(
                "fresh runtime, boot outside the timer, fuel {} instructions {}",
                hand_run.1,
                count_ops(&hand)
            ),
        },
    ]
}

fn time_fresh(spec: &program::ProtoSpec, expected: &[i64]) -> (u128, u64) {
    let mut samples = Vec::with_capacity(20);
    let mut fuel = 0u64;
    for _ in 0..20 {
        let mut runtime =
            Runtime::boot(Config::default(), HostRegistry::proof(), spec, false).expect("boot");
        let mut journal = Journal::new();
        let start = Instant::now();
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut journal)
            .expect("run");
        samples.push(start.elapsed().as_nanos());
        assert!(matches!(outcome, StepOutcome::Completed));
        fuel = runtime.fuel_consumed();
        assert_eq!(
            runtime.entry_results().expect("results"),
            expected
                .iter()
                .map(|value| Value::Integer(*value))
                .collect::<Vec<_>>()
        );
    }
    (median_ns(samples), fuel)
}

fn branches_and_close() -> Vec<Sample> {
    let proof = HostRegistry::proof();
    let n = 100_000i64;
    let mut samples = Vec::new();
    for (name, cond) in [("if_taken", false), ("if_not_taken", true)] {
        let spec = program::truth_branches(n, cond);
        samples.push(time_loop(LoopJob {
            name,
            spec: &spec,
            iters: n as u64,
            reg: 0,
            expected: n,
            registry: &proof,
            collect_between: false,
            quantum: u64::MAX,
        }));
    }
    // The closing loop allocates `width + 1` objects per iteration and does
    // not collect mid-run, so iterations stay under the default object limit.
    for (width, close_name, open_name) in [
        (1u8, "close_1", "reuse_open_1"),
        (4, "close_4", "reuse_open_4"),
        (32, "close_32", "reuse_open_32"),
        (200, "close_200", "reuse_open_200"),
    ] {
        let loops = 8_000 / (i64::from(width) + 1);
        for (name, close) in [(close_name, true), (open_name, false)] {
            let spec = program::close_loop(width, loops, close);
            samples.push(time_loop(LoopJob {
                name,
                spec: &spec,
                iters: loops as u64,
                reg: width,
                expected: loops,
                registry: &proof,
                collect_between: true,
                quantum: u64::MAX,
            }));
        }
    }
    for (name, source, expected) in [
        (
            "exec_compiled_branch_close",
            &include_bytes!("../fixtures/lua/branch_close.lua")[..],
            &[10, 11, 11, 99][..],
        ),
        (
            "exec_compiled_if_no_capture",
            b"local a = 1 if a then local b = 2 a = b end return a",
            &[2],
        ),
    ] {
        let chunk = crate::compile(source).expect("compile branch fixture");
        let (ns, fuel) = time_fresh(&chunk.proto, expected);
        samples.push(Sample {
            name,
            ns_per_op: ns,
            detail: format!(
                "fresh runtime, fuel {fuel}, protos {} instructions {} max_reg {}",
                chunk.prototype_count(),
                chunk.instruction_count(),
                chunk.max_registers()
            ),
        });
    }
    let hand = program::branch_close_hand(true);
    let (ns, fuel) = time_fresh(&hand, &[10, 11, 11, 99]);
    samples.push(Sample {
        name: "exec_hand_branch_close",
        ns_per_op: ns,
        detail: format!(
            "fresh runtime, fuel {fuel} instructions {} max_reg {}",
            count_ops(&hand),
            hand.max_reg
        ),
    });
    samples
}

/// Source loops, compiled, run in fresh runtimes. `ns_per_op` is per loop
/// iteration. The mix splits charged instructions between the hot tier and
/// `exec`, counted by the runtime in one extra run.
fn control_flow() -> Vec<Sample> {
    const N: i64 = 20_000;
    const C: i64 = 2_000;
    let jobs: Vec<(&str, String, i64, Vec<i64>)> = vec![
        (
            "while_loop",
            format!("local i = 0 while i < {N} do i = i + 1 end return i"),
            N,
            vec![N],
        ),
        (
            "cmp_int_eq",
            format!(
                "local i = 0 local c = 0 while i < {N} do local t = i == c i = i + 1 end return i"
            ),
            N,
            vec![N],
        ),
        (
            "cmp_mixed_eq",
            format!(
                "local i = 0 local c = 0.5 while i < {N} do local t = i == c i = i + 1 end return i"
            ),
            N,
            vec![N],
        ),
        (
            "cmp_int_lt",
            format!(
                "local i = 0 local c = 0 while i < {N} do local t = i < c i = i + 1 end return i"
            ),
            N,
            vec![N],
        ),
        (
            "cmp_int_le",
            format!(
                "local i = 0 local c = 0 while i < {N} do local t = i <= c i = i + 1 end return i"
            ),
            N,
            vec![N],
        ),
        (
            "while_capture",
            format!(
                "local i = 0 local f while i < {C} do local x = i f = function() return x end i = i + 1 end return f(), i"
            ),
            C,
            vec![C - 1, C],
        ),
        (
            "repeat_loop",
            format!("local i = 0 repeat i = i + 1 until i >= {N} return i"),
            N,
            vec![N],
        ),
        (
            "repeat_capture",
            format!(
                "local i = 0 local f repeat local x = i f = function() return x end i = i + 1 until i >= {C} return f(), i"
            ),
            C,
            vec![C - 1, C],
        ),
        (
            "break_one_block",
            format!("local i = 0 while i < {N} do while true do break end i = i + 1 end return i"),
            N,
            vec![N],
        ),
        (
            "break_nested_blocks",
            format!(
                "local i = 0 while i < {N} do while true do do local y = 1 do break end end end i = i + 1 end return i"
            ),
            N,
            vec![N],
        ),
        (
            "elseif_chain",
            format!(
                "local i = 0 local c = 0 while i < {N} do if i == 1000000 then c = c + 2 elseif i == 1000001 then c = c + 3 elseif i == 1000002 then c = c + 4 else c = c + 1 end i = i + 1 end return c"
            ),
            N,
            vec![N],
        ),
        (
            "do_block_exit",
            format!("local i = 0 while i < {N} do do local x = i end i = i + 1 end return i"),
            N,
            vec![N],
        ),
        (
            "for_int",
            format!("local s = 0 for i = 1, {N} do s = s + i end return s"),
            N,
            vec![N * (N + 1) / 2],
        ),
        (
            "while_as_for_int",
            format!("local s = 0 local i = 1 while i <= {N} do s = s + i i = i + 1 end return s"),
            N,
            vec![N * (N + 1) / 2],
        ),
        (
            "for_int_step_2",
            format!(
                "local s = 0 for i = 1, {}, 2 do s = s + 1 end return s",
                2 * N
            ),
            N,
            vec![N],
        ),
        (
            "for_int_step_neg",
            format!("local s = 0 for i = {N}, 1, -1 do s = s + 1 end return s"),
            N,
            vec![N],
        ),
        (
            "for_int_near_max",
            format!(
                "local s = 0 for i = {}, 9223372036854775807 do s = s + 1 end return s",
                i64::MAX - N + 1
            ),
            N,
            vec![N],
        ),
        (
            "for_float",
            format!("local s = 0 for i = 1.0, {N} do s = s + 1 end return s"),
            N,
            vec![N],
        ),
        (
            "for_capture",
            format!("local f for i = 1, {C} do f = function() return i end end return f()"),
            C,
            vec![C],
        ),
        (
            "for_break_inner",
            format!(
                "local s = 0 for i = 1, {N} do for j = 1, 10 do break end s = s + 1 end return s"
            ),
            N,
            vec![N],
        ),
        (
            "for_empty",
            format!("for i = 1, {N} do end return 7"),
            N,
            vec![7],
        ),
        (
            "src_int_key_read",
            format!("local t = {{ 1, 2, 3 }} local s for i = 1, {N} do s = t[2] end return s"),
            N,
            vec![2],
        ),
        (
            "src_field_read",
            format!("local t = {{ x = 1 }} local s for i = 1, {N} do s = t.x end return s"),
            N,
            vec![1],
        ),
        (
            "src_int_key_update",
            format!("local t = {{ 0 }} for i = 1, {N} do t[1] = i end return t[1]"),
            N,
            vec![N],
        ),
        (
            "src_field_update",
            format!("local t = {{ x = 0 }} for i = 1, {N} do t.x = i end return t.x"),
            N,
            vec![N],
        ),
        (
            "src_missing_read",
            format!("local t = {{}} for i = 1, {N} do local s = t.missing end return 7"),
            N,
            vec![7],
        ),
        (
            "src_global_read",
            format!("g = 1 local s for i = 1, {N} do s = g end return s"),
            N,
            vec![1],
        ),
        (
            "src_global_write",
            format!("for i = 1, {N} do g = i end return g"),
            N,
            vec![N],
        ),
        (
            "src_field_chain",
            format!(
                "local t = {{ a = {{ b = {{ c = 3 }} }} }} local s for i = 1, {N} do s = t.a.b.c end return s"
            ),
            N,
            vec![3],
        ),
        (
            "ctor_empty",
            format!("for i = 1, {C} do local t = {{}} end return 7"),
            C,
            vec![7],
        ),
        (
            "ctor_4",
            format!("for i = 1, {C} do local t = {{ 1, 2, 3, 4 }} end return 7"),
            C,
            vec![7],
        ),
        (
            "ctor_32",
            format!(
                "for i = 1, {C} do local t = {{ 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32 }} end return 7"
            ),
            C,
            vec![7],
        ),
        (
            "ctor_mixed",
            format!("for i = 1, {C} do local t = {{ 1, 2, x = 3, y = 4, [10] = 5 }} end return 7"),
            C,
            vec![7],
        ),
        (
            "for_assign_control",
            format!("local s = 0 for i = 1, {N} do s = s + 1 i = 0 end return s"),
            N,
            vec![N],
        ),
    ];
    let mut samples = Vec::new();
    for (name, source, iters, expected) in jobs {
        if !wanted(name) {
            continue;
        }
        let chunk = crate::compile(source.as_bytes()).expect(name);
        let (ns, fuel) = time_fresh(&chunk.proto, &expected);
        let (hot, cold) = op_mix(&chunk.proto);
        samples.push(Sample {
            name,
            ns_per_op: ns / iters as u128,
            detail: format!(
                "median {ns} ns/run, iters {iters}, fuel {fuel}, hot {hot} cold {cold}, instructions {}",
                chunk.instruction_count()
            ),
        });
    }
    samples
}

fn bind_natives(runtime: &mut Runtime) {
    for symbol in ["add", "many", "none", "mark", "park", "second", "upto"] {
        runtime
            .set_global_native(symbol, symbol)
            .expect("proof native");
    }
    runtime.install_base().expect("base functions");
}

/// Lua-to-native calls against a Lua-to-Lua baseline, per loop iteration.
/// Natives are the proof registry's, bound as globals before each run.
fn native_calls() -> Vec<Sample> {
    const N: i64 = 20_000;
    let jobs: Vec<(&str, String, Vec<i64>)> = vec![
        (
            "call_lua_scalar",
            format!(
                "local f = function(a, b) return a + b end local s for i = 1, {N} do s = f(i, 1) end return s"
            ),
            vec![N + 1],
        ),
        (
            "call_native_local",
            format!("local f = add local s for i = 1, {N} do s = f(i, 1) end return s"),
            vec![N + 1],
        ),
        (
            "call_native_global",
            format!("local s for i = 1, {N} do s = add(i, 1) end return s"),
            vec![N + 1],
        ),
        (
            "call_native_table",
            format!("local t = {{ f = add }} local s for i = 1, {N} do s = t.f(i, 1) end return s"),
            vec![N + 1],
        ),
        (
            "call_native_zero",
            format!("local f = none for i = 1, {N} do f() end return 7"),
            vec![7],
        ),
        (
            "call_native_multi",
            format!("local f = many local a, b, c for i = 1, {N} do a, b, c = f() end return a, c"),
            vec![10, 30],
        ),
        (
            "call_native_external",
            format!("local f = mark for i = 1, {N} do f(i) end return 7"),
            vec![7],
        ),
        (
            "meta_index_hit",
            format!(
                "local t = setmetatable({{ x = 1 }}, {{ __index = function() return 2 end }}) local s for i = 1, {N} do s = t.x end return s"
            ),
            vec![1],
        ),
        (
            "meta_miss_no_metatable",
            format!("local t = {{}} for i = 1, {N} do local s = t.missing end return 7"),
            vec![7],
        ),
        (
            "meta_miss_no_index",
            format!(
                "local t = setmetatable({{}}, {{}}) for i = 1, {N} do local s = t.missing end return 7"
            ),
            vec![7],
        ),
        (
            "meta_index_table_1",
            format!(
                "local t = setmetatable({{}}, {{ __index = {{ x = 1 }} }}) local s for i = 1, {N} do s = t.x end return s"
            ),
            vec![1],
        ),
        (
            "meta_index_table_8",
            format!(
                "local cur = {{ x = 1 }} for i = 1, 8 do cur = setmetatable({{}}, {{ __index = cur }}) end local s for i = 1, {N} do s = cur.x end return s"
            ),
            vec![1],
        ),
        (
            "meta_index_table_64",
            format!(
                "local cur = {{ x = 1 }} for i = 1, 64 do cur = setmetatable({{}}, {{ __index = cur }}) end local s for i = 1, {N} do s = cur.x end return s"
            ),
            vec![1],
        ),
        (
            "meta_index_lua",
            format!(
                "local k = 'x' local t = setmetatable({{}}, {{ __index = function(self, key) return 1 end }}) local s for i = 1, {N} do s = t[k] end return s"
            ),
            vec![1],
        ),
        (
            "meta_index_native",
            format!(
                "local t = setmetatable({{}}, {{ __index = second }}) local s for i = 1, {N} do s = t[1] end return s"
            ),
            vec![1],
        ),
        (
            "meta_set_hit",
            format!(
                "local t = setmetatable({{ x = 0 }}, {{ __newindex = function() end }}) for i = 1, {N} do t.x = i end return t.x"
            ),
            vec![N],
        ),
        (
            "meta_set_absent_no_metatable",
            format!(
                "local k = 'x' local t = {{}} for i = 1, {N} do t[k] = i t[k] = nil end return 7"
            ),
            vec![7],
        ),
        (
            "meta_newindex_table",
            format!(
                "local k = 'x' local target = {{}} local t = setmetatable({{}}, {{ __newindex = target }}) for i = 1, {N} do t[k] = i end return target.x"
            ),
            vec![N],
        ),
        (
            "meta_newindex_lua",
            format!(
                "local k = 'x' local t = setmetatable({{}}, {{ __newindex = function(self, key, value) end }}) for i = 1, {N} do t[k] = i end return 7"
            ),
            vec![7],
        ),
        (
            "meta_newindex_native",
            format!(
                "local k = 'x' local t = setmetatable({{}}, {{ __newindex = none }}) for i = 1, {N} do t[k] = i end return 7"
            ),
            vec![7],
        ),
        (
            "raw_get",
            format!(
                "local k = 'x' local t = {{ x = 1 }} local s for i = 1, {N} do s = rawget(t, k) end return s"
            ),
            vec![1],
        ),
        (
            "raw_set",
            format!(
                "local k = 'x' local t = {{ x = 0 }} for i = 1, {N} do rawset(t, k, i) end return t.x"
            ),
            vec![N],
        ),
        (
            "raw_len",
            format!("local t = {{ 1, 2, 3 }} local s for i = 1, {N} do s = rawlen(t) end return s"),
            vec![3],
        ),
        (
            "len_string",
            format!("local text = 'hello' local s for i = 1, {N} do s = #text end return s"),
            vec![5],
        ),
        (
            "len_table_raw",
            format!("local t = {{ 1, 2, 3 }} local s for i = 1, {N} do s = #t end return s"),
            vec![3],
        ),
        (
            "len_table_lua",
            format!(
                "local t = setmetatable({{}}, {{ __len = function() return 4 end }}) local s for i = 1, {N} do s = #t end return s"
            ),
            vec![4],
        ),
        (
            "len_table_native",
            format!(
                "local t = setmetatable({{}}, {{ __len = many }}) local s for i = 1, {N} do s = #t end return s"
            ),
            vec![10],
        ),
        (
            "set_metatable",
            format!(
                "local t = {{}} local mt = {{}} for i = 1, {N} do setmetatable(t, mt) end return 7"
            ),
            vec![7],
        ),
        (
            "get_metatable",
            format!(
                "local t = setmetatable({{}}, {{}}) for i = 1, {N} do local m = getmetatable(t) end return 7"
            ),
            vec![7],
        ),
        // Constant keys (`t.foo`) against the same access with the key in
        // a register (`t[k]`).
        (
            "const_field_hit",
            format!("local t = {{ foo = 1 }} local s for i = 1, {N} do s = t.foo end return s"),
            vec![1],
        ),
        (
            "dyn_field_hit",
            format!(
                "local k = 'foo' local t = {{ foo = 1 }} local s for i = 1, {N} do s = t[k] end return s"
            ),
            vec![1],
        ),
        (
            "const_index_lua",
            format!(
                "local t = setmetatable({{}}, {{ __index = function(self, key) return 1 end }}) local s for i = 1, {N} do s = t.foo end return s"
            ),
            vec![1],
        ),
        (
            "const_index_native",
            format!(
                "local t = setmetatable({{}}, {{ __index = second }}) local s for i = 1, {N} do s = t.foo end return 7"
            ),
            vec![7],
        ),
        (
            "dyn_index_native",
            format!(
                "local k = 'foo' local t = setmetatable({{}}, {{ __index = second }}) local s for i = 1, {N} do s = t[k] end return 7"
            ),
            vec![7],
        ),
        (
            "const_set_insert",
            format!("local t = {{}} for i = 1, {N} do t.foo = i t.foo = nil end return 7"),
            vec![7],
        ),
        // Operators (Phase 3.12). Primitive rows first, then metamethods,
        // which cost a call each and are not expected to match them.
        (
            "op_int_mix",
            format!("local s = 0 for i = 1, {N} do s = (s + i * 3 - 1) % 1000 end return s"),
            vec![(1..=N).fold(0, |s, i| (s + i * 3 - 1).rem_euclid(1000))],
        ),
        (
            "op_int_idiv",
            format!("local s = 0 for i = 1, {N} do s = i // 7 end return s"),
            vec![N / 7],
        ),
        (
            "op_float_mix",
            format!("local s = 0.5 for i = 1, {N} do s = s * 0.5 + i / 4 - 1.5 ^ 2 end return 7"),
            vec![7],
        ),
        (
            "op_bitwise",
            format!(
                "local s = 0 for i = 1, {N} do s = ((s ~ i) & 65535 | i << 3) >> 1 end return s"
            ),
            vec![(1..=N).fold(0i64, |s, i| (((s ^ i) & 65535) | (i << 3)) >> 1)],
        ),
        (
            "op_compare_mixed",
            format!("local c = 0 for i = 1, {N} do local b = i < 2.5 c = c + 1 end return c"),
            vec![N],
        ),
        (
            "op_add_string",
            format!("local s = 0 for i = 1, {N} do s = '1' + s end return s"),
            vec![N],
        ),
        (
            "meta_add_lua",
            format!(
                "local t = setmetatable({{}}, {{ __add = function(a, b) return b end }}) local s for i = 1, {N} do s = t + i end return s"
            ),
            vec![N],
        ),
        (
            "meta_add_native",
            format!(
                "local t = setmetatable({{}}, {{ __add = second }}) local s for i = 1, {N} do s = t + i end return s"
            ),
            vec![N],
        ),
        (
            "meta_add_callable",
            format!(
                "local c = setmetatable({{}}, {{ __call = function(self, a, b) return b end }}) local t = setmetatable({{}}, {{ __add = c }}) local s for i = 1, {N} do s = t + i end return s"
            ),
            vec![N],
        ),
        (
            "meta_eq_lua",
            format!(
                "local mt = {{ __eq = function(a, b) return true end }} local a, b = setmetatable({{}}, mt), setmetatable({{}}, mt) local c = 0 for i = 1, {N} do local e = a == b c = c + 1 end return c"
            ),
            vec![N],
        ),
        (
            "meta_lt_lua",
            format!(
                "local mt = {{ __lt = function(a, b) return true end }} local a, b = setmetatable({{}}, mt), setmetatable({{}}, mt) local c = 0 for i = 1, {N} do local e = a < b c = c + 1 end return c"
            ),
            vec![N],
        ),
        (
            "meta_le_native",
            format!(
                "local mt = {{ __le = second }} local a, b = setmetatable({{}}, mt), setmetatable({{}}, mt) local c = 0 for i = 1, {N} do local e = a <= b c = c + 1 end return c"
            ),
            vec![N],
        ),
        (
            "call_table",
            format!(
                "local t = setmetatable({{}}, {{ __call = function(self, a) return a end }}) local s for i = 1, {N} do s = t(i) end return s"
            ),
            vec![N],
        ),
        (
            "call_table_chain_3",
            format!(
                "local f = function(a, b, c, d) return d end local c1 = setmetatable({{}}, {{ __call = f }}) local c2 = setmetatable({{}}, {{ __call = c1 }}) local c3 = setmetatable({{}}, {{ __call = c2 }}) local s for i = 1, {N} do s = c3(i) end return s"
            ),
            vec![N],
        ),
        (
            "concat_strings",
            format!("local s for i = 1, {N} do s = 'ab' .. 'cd' end return #s"),
            vec![4],
        ),
        (
            "concat_number",
            format!("local s for i = 1, {N} do s = 'n' .. i end return #s"),
            vec![6],
        ),
        (
            "concat_meta_lua",
            format!(
                "local t = setmetatable({{}}, {{ __concat = function(a, b) return 1 end }}) local s for i = 1, {N} do s = t .. 'x' end return s"
            ),
            vec![1],
        ),
        (
            "concat_meta_native",
            format!(
                "local t = setmetatable({{}}, {{ __concat = second }}) local s for i = 1, {N} do s = 'x' .. t end return 7"
            ),
            vec![7],
        ),
    ];
    let mut samples = time_source_jobs(
        jobs.into_iter()
            .map(|(name, source, expected)| (name, source, expected, N, Config::default()))
            .collect(),
    );
    if wanted("call_native_wait") {
        samples.push(native_wait_cycle(
            "call_native_wait",
            "for i = 1, 2000 do local r = park() end return 7",
        ));
    }
    if wanted("meta_index_pending") {
        samples.push(native_wait_cycle(
            "meta_index_pending",
            "local t = setmetatable({}, { __index = park }) for i = 1, 2000 do local r = t.x end return 7",
        ));
    }
    if wanted("meta_add_pending") {
        samples.push(native_wait_cycle(
            "meta_add_pending",
            "local t = setmetatable({}, { __add = park }) for i = 1, 2000 do local r = t + i end return 7",
        ));
    }
    if wanted("meta_chain_2000") {
        samples.push(chain_limit());
    }
    samples
}

/// A source row: name, program, integer results, loop iterations, config.
type SourceJob = (&'static str, String, Vec<i64>, i64, Config);

/// Protected calls and caught errors (Phase 3.13), per loop iteration.
/// `catch_depth_D` raises `D` Lua frames below `pcall`, and `return_depth_100`
/// is the same recursion returning normally, so their difference is the
/// cost of unwinding. 995 is the deepest catch under the 1,000-frame bound.
fn error_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let depth = |d: i64, m: i64, fail: bool| {
        let leaf = if fail { "error('x', 0)" } else { "return 0" };
        let count = if fail {
            "pcall(f, D) == false"
        } else {
            "pcall(f, D)"
        }
        .replace('D', &d.to_string());
        format!(
            "local f f = function(n) if n == 0 then {leaf} end return (f(n - 1)) end \
             local c = 0 for i = 1, {m} do if {count} then c = c + 1 end end return c"
        )
    };
    let quota = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let rows: Vec<SourceJob> = vec![
        (
            "pcall_zero",
            format!("local f = function() end local c = 0 for i = 1, {N} do if pcall(f) then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "pcall_scalar",
            format!("local f = function(a) return a end local s for i = 1, {N} do local ok, v = pcall(f, i) s = v end return s"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "pcall_multi",
            format!("local f = function(a) return a, a, a end local s for i = 1, {N} do local ok, x, y, z = pcall(f, i) s = z end return s"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "pcall_nested",
            format!("local f = function(a) return a end local g = function(a) local ok, v = pcall(f, a) return v end \
                     local s for i = 1, {N} do local ok, v = pcall(g, i) s = v end return s"),
            vec![N],
            N,
            Config::default(),
        ),
        ("catch_depth_1", depth(1, N, true), vec![N], N, Config::default()),
        ("catch_depth_10", depth(10, 5_000, true), vec![5_000], 5_000, Config::default()),
        ("catch_depth_100", depth(100, 1_000, true), vec![1_000], 1_000, Config::default()),
        ("return_depth_100", depth(100, 1_000, false), vec![1_000], 1_000, Config::default()),
        ("catch_depth_995", depth(995, 200, true), vec![200], 200, Config::default()),
        (
            "xpcall_ok",
            format!("local f = function(a) return a end local h = function(m) return m end \
                     local s for i = 1, {N} do local ok, v = xpcall(f, h, i) s = v end return s"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "xpcall_handler",
            format!("local f = function() error('x', 0) end local h = function(m) return 1 end \
                     local c = 0 for i = 1, {N} do local ok, v = xpcall(f, h) c = c + v end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "error_table",
            format!("local e = {{}} local f = function() error(e) end \
                     local c = 0 for i = 1, {N} do local ok, v = pcall(f) if v == e then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "catch_stack_overflow",
            "local f f = function() return (f()) + 1 end \
             local c = 0 for i = 1, 200 do if pcall(f) == false then c = c + 1 end end return c"
                .to_string(),
            vec![200],
            200,
            Config::default(),
        ),
        (
            "catch_memory_size",
            format!("local s = 'xxxxxxxx' for j = 1, 16 do s = s .. s end s = s .. 'x' \
                     local f = function() return s .. s end \
                     local c = 0 for i = 1, {N} do if pcall(f) == false then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "catch_memory_quota",
            "local f = function() local t = {} local j = 0 while true do j = j + 1 t[j] = j end end \
             local c = 0 for i = 1, 100 do if pcall(f) == false then c = c + 1 end end return c"
                .to_string(),
            vec![100],
            100,
            quota,
        ),
        (
            "catch_native_fault",
            format!("local c = 0 for i = 1, {N} do if pcall(add, 'x') == false then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "catch_meta_fault",
            format!("local t = setmetatable({{}}, {{ __add = function() error('x', 0) end }}) \
                     local f = function() return t + 1 end \
                     local c = 0 for i = 1, {N} do if pcall(f) == false then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "catch_meta_native_fault",
            format!("local t = setmetatable({{}}, {{ __add = add }}) local f = function() return t + 1 end \
                     local c = 0 for i = 1, {N} do if pcall(f) == false then c = c + 1 end end return c"),
            vec![N],
            N,
            Config::default(),
        ),
    ];
    time_source_jobs(rows)
}

/// To-be-closed variables (Phase 3.14), per loop iteration. `scope_plain`
/// and `close_nil` leave a scope with no close to run; `call_closer` is the
/// closer called directly, so `close_one` less it is the close list and the
/// `__close` lookup; `close_error_1` compares with `catch_depth_1`, and
/// `close_return` with `call_lua_scalar`.
fn close_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let obj = "local obj = setmetatable({}, { __close = function() end }) ";
    let many = |count: usize| -> String {
        (0..count)
            .map(|i| format!("local c{i} <close> = obj "))
            .collect()
    };
    let rows: Vec<SourceJob> = vec![
        (
            "scope_plain",
            format!(
                "{obj}local c = 0 for i = 1, {N} do do local x = obj c = c + 1 end end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "close_nil",
            format!(
                "local c = 0 for i = 1, {N} do do local x <close> = nil c = c + 1 end end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "call_closer",
            format!(
                "local f = function() end {obj}local c = 0 for i = 1, {N} do f(obj, nil) c = c + 1 end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "close_one",
            format!(
                "{obj}local c = 0 for i = 1, {N} do do local x <close> = obj c = c + 1 end end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "close_native",
            format!(
                "local obj = setmetatable({{}}, {{ __close = none }}) local c = 0 \
                     for i = 1, {N} do do local x <close> = obj c = c + 1 end end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "close_4",
            format!(
                "{obj}local c = 0 for i = 1, 5000 do do {} c = c + 1 end end return c",
                many(4)
            ),
            vec![5_000],
            5_000,
            Config::default(),
        ),
        (
            "close_32",
            format!(
                "{obj}local c = 0 for i = 1, 1000 do do {} c = c + 1 end end return c",
                many(32)
            ),
            vec![1_000],
            1_000,
            Config::default(),
        ),
        (
            "close_error_1",
            format!(
                "{obj}local f = function() local x <close> = obj error('x', 0) end \
                     local c = 0 for i = 1, {N} do if pcall(f) == false then c = c + 1 end end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
        (
            "close_error_32",
            format!(
                "{obj}local f = function() {} error('x', 0) end \
                     local c = 0 for i = 1, 1000 do if pcall(f) == false then c = c + 1 end end return c",
                many(32)
            ),
            vec![1_000],
            1_000,
            Config::default(),
        ),
        (
            "close_return",
            format!(
                "{obj}local g = function() local x <close> = obj return 1 end \
                     local c = 0 for i = 1, {N} do c = c + g() end return c"
            ),
            vec![N],
            N,
            Config::default(),
        ),
    ];
    let mut samples = time_source_jobs(rows);
    if wanted("close_wait") {
        samples.push(native_wait_cycle(
            "close_wait",
            "local t = setmetatable({}, { __close = park }) for i = 1, 2000 do local x <close> = t end return 7",
        ));
    }
    samples
}

/// Generic `for` (Phase 3.15), per iteration unless noted. `gfor_lua`
/// calls a Lua iterator, its closing value nil. `gfor_numeric_call` makes
/// the same call from a numeric `for`, so the difference is the generic
/// loop's own control; `gfor_while` is the loop written with `while`.
/// `gfor_setup_nil` and `gfor_setup_close` are per loop, each ended by its
/// first call, with a nil and a real closing value; `gfor_break` breaks in
/// the first iteration, and `gfor_error`'s iterator raises under `pcall`.
fn generic_for_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    const C: i64 = 2_000;
    let it = "local it = function(s, c) if c < s then return c + 1 end end ";
    let obj = "local obj = setmetatable({}, { __close = function() end }) ";
    let job = |name, source: String| (name, source, vec![N], N, Config::default());
    let rows: Vec<SourceJob> = vec![
        job(
            "gfor_lua",
            format!("{it}local n = 0 for i in it, {N}, 0 do n = n + 1 end return n"),
        ),
        job(
            "gfor_numeric_call",
            format!(
                "{it}local n = 0 for j = 1, {N} do local i = it({N}, j - 1) n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_while",
            format!(
                "{it}local n, c = 0, 0 while true do local i = it({N}, c) \
                 if i == nil then break end c = i n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_native",
            format!("local n = 0 for i in upto, {N} do n = n + 1 end return n"),
        ),
        job(
            "gfor_callable",
            format!(
                "local t = setmetatable({{}}, {{ __call = function(self, s, c) if c < s then return c + 1 end end }}) \
                 local n = 0 for i in t, {N}, 0 do n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_2",
            format!(
                "local it = function(s, c) if c < s then return c + 1, c end end \
                 local n = 0 for i, j in it, {N}, 0 do n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_4",
            format!(
                "local it = function(s, c) if c < s then return c + 1, c, s, 1 end end \
                 local n = 0 for a, b, c, d in it, {N}, 0 do n = n + 1 end return n"
            ),
        ),
        (
            "gfor_capture",
            format!(
                "{it}local f for i in it, {C}, 0 do f = function() return i end end return f()"
            ),
            vec![C],
            C,
            Config::default(),
        ),
        job(
            "gfor_setup_nil",
            format!(
                "{it}local n = 0 for j = 1, {N} do for i in it, 0, 0 do end n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_setup_close",
            format!(
                "{it}{obj}local n = 0 for j = 1, {N} do for i in it, 0, 0, obj do end n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_break",
            format!(
                "{it}local n = 0 for j = 1, {N} do for i in it, 1, 0 do break end n = n + 1 end return n"
            ),
        ),
        job(
            "gfor_error",
            format!(
                "local f = function() for i in function() error('x', 0) end do end end \
                 local n = 0 for j = 1, {N} do if pcall(f) == false then n = n + 1 end end return n"
            ),
        ),
    ];
    time_source_jobs(rows)
}

/// Calls and varargs (Phase 3.16), per loop iteration: one call each.
/// `call_*` call a fixed-parameter function with 0, 2, and 4 arguments;
/// `va_*` a vararg function with 0, 1, 4, and 32 extras; the `va_read`,
/// `va_all`, `va_return`, `va_pass`, and `va_table` rows take 4 extras and
/// read one, read all into locals, return all, pass all on to a call, and
/// build a table of them; `va_copy_*` return all of 0, 1, 32, and 200
/// extras to a call that drops them; `va_nested` and `va_pcall` make a call
/// and a protected call before reading one.
fn vararg_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let args = |count: usize| -> String {
        (1..=count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let job = |name, define: &str, call: String| {
        (
            name,
            format!("{define} local c = 0 for i = 1, {N} do {call} c = c + 1 end return c"),
            vec![N],
            N,
            Config::default(),
        )
    };
    let rows: Vec<SourceJob> = vec![
        job("call_0", "local f = function() return 1 end", "f()".into()),
        job(
            "call_2",
            "local f = function(a, b) return a end",
            "f(1, 2)".into(),
        ),
        job(
            "call_4",
            "local f = function(a, b, c, d) return a end",
            "f(1, 2, 3, 4)".into(),
        ),
        job("va_0", "local f = function(...) return 1 end", "f()".into()),
        job(
            "va_1",
            "local f = function(...) return 1 end",
            "f(1)".into(),
        ),
        job(
            "va_4",
            "local f = function(...) return 1 end",
            format!("f({})", args(4)),
        ),
        job(
            "va_32",
            "local f = function(...) return 1 end",
            format!("f({})", args(32)),
        ),
        job(
            "va_read",
            "local f = function(...) local x = ... return x end",
            format!("f({})", args(4)),
        ),
        job(
            "va_all",
            "local f = function(...) local a, b, c, d = ... return a end",
            format!("f({})", args(4)),
        ),
        job(
            "va_return",
            "local f = function(...) return ... end",
            format!("local w, x, y, z = f({})", args(4)),
        ),
        job(
            "va_pass",
            "local g = function(a, b, c, d) return a end local f = function(...) return g(...) end",
            format!("f({})", args(4)),
        ),
        job(
            "va_table",
            "local f = function(...) local t = { ... } return t end",
            format!("f({})", args(4)),
        ),
        job(
            "va_nested",
            "local g = function() return 1 end local f = function(...) g() return (...) end",
            format!("f({})", args(4)),
        ),
        job(
            "va_copy_0",
            "local f = function(...) return ... end",
            "f()".into(),
        ),
        job(
            "va_copy_1",
            "local f = function(...) return ... end",
            format!("f({})", args(1)),
        ),
        job(
            "va_copy_32",
            "local f = function(...) return ... end",
            format!("f({})", args(32)),
        ),
        job(
            "va_copy_200",
            "local f = function(...) return ... end",
            format!("f({})", args(200)),
        ),
        job(
            "va_200",
            "local f = function(...) return 1 end",
            format!("f({})", args(200)),
        ),
        job(
            "va_pcall",
            "local g = function() return 1 end local f = function(...) pcall(g) return (...) end",
            format!("f({})", args(4)),
        ),
    ];
    time_source_jobs(rows)
}

/// Tail calls (Phase 3.17, ADR 0029). The `hop_*` rows are per hop of a
/// recursion 100 deep, run 2,000 times: `hop_tail*` as tail calls,
/// `hop_call*` the same recursion as `return (f(n - 1))`, a call and a
/// return. `_va` passes four extras through a vararg function;
/// `hop_tail_fixed_va` alternates a fixed and a vararg function, per hop;
/// `_meta` recurses through a table's `__call`; `_upval` makes a closure
/// over `n` on every hop, whose upvalue closes when the frame goes.
/// `hop_tail_deep` is one tail recursion of 200,000 hops. The rest are
/// per call of a Lua function that returns a native's results:
/// `tail_native` / `call_native_ret` for a VM-local native,
/// `tail_external` / `call_external_ret` for an External one, and
/// `tail_pending` / `call_pending_ret` for one that waits on the host.
fn tail_rows() -> Vec<Sample> {
    const N: i64 = 2_000;
    const D: i64 = 100;
    const CALLS: i64 = 20_000;
    let hops = |name, define: &str, extra: &str, per: i64| {
        (
            name,
            format!("{define} local c = 0 for i = 1, {N} do c = c + f({D}{extra}) end return c"),
            vec![N],
            N * D * per,
            Config::default(),
        )
    };
    let recursion =
        |ret: &str| format!("local f f = function(n) if n == 0 then return 1 end return {ret} end");
    let va = |ret: &str| {
        format!("local f f = function(n, ...) if n == 0 then return 1 end return {ret} end")
    };
    let meta = |ret: &str| {
        format!(
            "local f = setmetatable({{}}, {{ __call = function(self, n) \
             if n == 0 then return 1 end return {ret} end }})"
        )
    };
    let upval = |ret: &str| {
        format!(
            "local f f = function(n) local h = function() return n end \
             if n == 0 then return h() + 1 end return {ret} end"
        )
    };
    let calls = |name, define: &str| {
        (
            name,
            format!(
                "{define} local c = 0 for i = 1, {CALLS} do local r = f(i) c = c + 1 end return c"
            ),
            vec![CALLS],
            CALLS,
            Config::default(),
        )
    };
    let rows: Vec<SourceJob> = vec![
        hops("hop_tail", &recursion("f(n - 1)"), "", 1),
        hops("hop_call", &recursion("(f(n - 1))"), "", 1),
        hops("hop_tail_va", &va("f(n - 1, ...)"), ", 1, 2, 3, 4", 1),
        hops("hop_call_va", &va("(f(n - 1, ...))"), ", 1, 2, 3, 4", 1),
        hops(
            "hop_tail_fixed_va",
            "local f, g f = function(n, a, b) if n == 0 then return 1 end return g(n - 1, a, b) end \
             g = function(n, ...) return f(n, ...) end",
            ", 1, 2",
            2,
        ),
        hops("hop_tail_meta", &meta("self(n - 1)"), "", 1),
        hops("hop_call_meta", &meta("(self(n - 1))"), "", 1),
        hops("hop_tail_upval", &upval("f(n - 1)"), "", 1),
        hops("hop_call_upval", &upval("(f(n - 1))"), "", 1),
        (
            "hop_tail_deep",
            format!("{} return f(200000)", recursion("f(n - 1)")),
            vec![1],
            200_000,
            Config::default(),
        ),
        calls("tail_native", "local f = function(a) return add(a, 1) end"),
        calls(
            "call_native_ret",
            "local f = function(a) return (add(a, 1)) end",
        ),
        calls("tail_external", "local f = function(a) return mark(a) end"),
        calls(
            "call_external_ret",
            "local f = function(a) return (mark(a)) end",
        ),
    ];
    let mut samples = time_source_jobs(rows);
    for (name, ret) in [("tail_pending", "park()"), ("call_pending_ret", "(park())")] {
        if wanted(name) {
            samples.push(native_wait_cycle(
                name,
                &format!(
                    "local f = function() return {ret} end for i = 1, 2000 do local r = f() end return 7"
                ),
            ));
        }
    }
    samples
}

/// `and`, `or`, `not`, method calls, and function statements (Phase 3.18),
/// per loop iteration. `logic_base` is the loop with `local x = v`, the
/// cost the logical rows add to. `and_skip` and `or_skip` decide on the
/// left and skip a call; `and_eval` and `or_eval` evaluate the right side,
/// a local. `method_call` is `o:get(1)` and `method_plain` the same call
/// written `o.get(o, 1)`; `method_index` finds `get` through an `__index`
/// table; `method_tail` is a method that returns another method call.
/// `func_stmt` runs `function g() end` each iteration, a closure and a
/// global store; `local_func` calls a `local function`; `local_tail` is
/// per hop of a `local function` tail recursion 100 deep.
fn syntax_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let job = |name, define: &str, body: &str| {
        (
            name,
            format!("{define} local c = 0 for i = 1, {N} do {body} c = c + 1 end return c"),
            vec![N],
            N,
            Config::default(),
        )
    };
    let values = "local v, w, nothing = 1, 2, nil local f = function() return 3 end";
    let object = "local o = { get = function(self, x) return x end }";
    let class = "local C = { get = function(self, x) return x end } C.__index = C \
                 local o = setmetatable({}, C)";
    let rows: Vec<SourceJob> = vec![
        job("logic_base", values, "local x = v"),
        job("not_value", values, "local x = not v"),
        job("and_eval", values, "local x = v and w"),
        job("and_skip", values, "local x = nothing and f()"),
        job("or_skip", values, "local x = v or f()"),
        job("or_eval", values, "local x = nothing or w"),
        job("method_call", object, "local r = o:get(1)"),
        job("method_plain", object, "local r = o.get(o, 1)"),
        job("method_index", class, "local r = o:get(1)"),
        job(
            "method_tail",
            &format!("{object} function o:t(x) return self:get(x) end"),
            "local r = o:t(1)",
        ),
        job("func_stmt", "", "function g() end"),
        job(
            "local_func",
            "local function lf(x) return x end",
            "local r = lf(1)",
        ),
        (
            "local_tail",
            "local function loop(n) if n == 0 then return 1 end return loop(n - 1) end \
             local c = 0 for i = 1, 2000 do c = c + loop(100) end return c"
                .to_string(),
            vec![2000],
            2000 * 100,
            Config::default(),
        ),
    ];
    time_source_jobs(rows)
}

/// Base-library functions (Phase 3.21, ADR 0031), per call, in a numeric
/// `for` body, against `base_none`, the same loop with a local copy.
/// `print_int` writes to no output, so it is the conversion and the
/// journal commit. `ipairs_step` and `pairs_step` are per element of a
/// ten-element table; `load_small` and `load_reader` per compile.
fn base_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    const LOADS: i64 = 2_000;
    let job = |name, body: &str| {
        (
            name,
            format!(
                "local v, s, t = 7, 'str', {{ 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 }} \
                 local m = setmetatable({{}}, {{ __tostring = function() return 'm' end }}) \
                 local c = 0 for i = 1, {N} do {body} c = c + 1 end return c"
            ),
            vec![N],
            N,
            Config::default(),
        )
    };
    let per = |name, source: String, count: i64, per: i64| {
        (name, source, vec![count], count * per, Config::default())
    };
    let rows: Vec<SourceJob> = vec![
        job("base_none", "local x = v"),
        job("type_value", "local x = type(v)"),
        job("assert_true", "local x = assert(v)"),
        job("tostring_int", "local x = tostring(i)"),
        job("tostring_float", "local x = tostring(1.5)"),
        job("tostring_string", "local x = tostring(s)"),
        job("tostring_table", "local x = tostring(t)"),
        job("tostring_meta", "local x = tostring(m)"),
        job("tonumber_string", "local x = tonumber('42')"),
        job("tonumber_base", "local x = tonumber('ff', 16)"),
        job("next_first", "local k = next(t)"),
        job("pairs_setup", "local f, st, k = pairs(t)"),
        job("print_int", "print(i)"),
        per(
            "ipairs_step",
            format!(
                "local t = {{ 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 }} local c = 0 \
                 for i = 1, {LOADS} do for k, v in ipairs(t) do end c = c + 1 end return c"
            ),
            LOADS,
            10,
        ),
        per(
            "pairs_step",
            format!(
                "local t = {{ 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 }} local c = 0 \
                 for i = 1, {LOADS} do for k, v in pairs(t) do end c = c + 1 end return c"
            ),
            LOADS,
            10,
        ),
        per(
            "load_small",
            format!(
                "local c = 0 for i = 1, {LOADS} do local f = load('return 1') c = c + 1 end return c"
            ),
            LOADS,
            1,
        ),
        per(
            "load_reader",
            format!(
                "local parts = {{ 'return ', '1 + ', '2' }} local c = 0 \
                 for i = 1, {LOADS} do local n = 0 \
                 local f = load(function() n = n + 1 return parts[n] end) c = c + 1 end return c"
            ),
            LOADS,
            1,
        ),
    ];
    time_source_jobs(rows)
}

/// The math and table libraries (Phase 3.22, ADR 0032, ADR 0033). Math
/// rows are per call in a 20,000-pass loop, against `base_none`. Table
/// rows are per call, over lists built before the timed loop where the
/// function does not change them; the detail's element count gives ns per
/// element.
fn library_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let math = |name, body: &str| {
        (
            name,
            format!(
                "local v, f = -7, 2.5 local c = 0 for i = 1, {N} do {body} c = c + 1 end return c"
            ),
            vec![N],
            N,
            Config::default(),
        )
    };
    let list = |n: i64| format!("local t = {{}} for i = 1, {n} do t[i] = (i * 7919) % {n} end");
    let table = |name, setup: String, body: &str, calls: i64| {
        (
            name,
            format!("{setup} local c = 0 for i = 1, {calls} do {body} c = c + 1 end return c"),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let rows: Vec<SourceJob> = vec![
        math("math_abs_int", "local x = math.abs(v)"),
        math("math_abs_float", "local x = math.abs(f)"),
        math("math_floor", "local x = math.floor(f)"),
        math("math_sqrt", "local x = math.sqrt(f)"),
        math("math_sin", "local x = math.sin(f)"),
        math("math_cos", "local x = math.cos(f)"),
        math("math_log", "local x = math.log(f)"),
        math("math_random", "local x = math.random()"),
        math("math_random_range", "local x = math.random(1, 1000)"),
        math("math_max", "local x = math.max(v, f, 3)"),
        table(
            "table_pack4",
            String::new(),
            "local p = table.pack(1, 2, 3, 4)",
            5_000,
        ),
        table(
            "table_pack32",
            String::new(),
            &format!("local p = table.pack({})", vec!["1"; 32].join(", ")),
            2_000,
        ),
        table(
            "table_unpack4",
            list(4),
            "local a, b, x, y = table.unpack(t)",
            5_000,
        ),
        table(
            "table_unpack32",
            list(32),
            "local a = table.unpack(t)",
            2_000,
        ),
        table(
            "table_unpack256",
            list(256),
            "local a = table.unpack(t)",
            500,
        ),
        table(
            "table_concat10",
            list(10),
            "local s = table.concat(t, ',')",
            2_000,
        ),
        table(
            "table_concat1000",
            list(1000),
            "local s = table.concat(t, ',')",
            50,
        ),
        table(
            "table_insert_end",
            String::from("local t = {}"),
            "table.insert(t, i)",
            5_000,
        ),
        table(
            "table_insert_front",
            String::from("local t = {}"),
            "table.insert(t, 1, i)",
            1_000,
        ),
        table("table_remove_end", list(5000), "table.remove(t)", 5_000),
        table(
            "table_remove_front",
            list(1000),
            "table.remove(t, 1)",
            1_000,
        ),
        table(
            "table_move100",
            list(101),
            "table.move(t, 1, 100, 2)",
            1_000,
        ),
        table(
            "table_move10000",
            list(10001),
            "table.move(t, 1, 10000, 2)",
            20,
        ),
        table(
            "table_sort10",
            String::new(),
            "local t = { 5, 3, 9, 1, 7, 2, 8, 4, 6, 0 } table.sort(t)",
            2_000,
        ),
        table(
            "table_sort100",
            list(100),
            "local u = table.move(t, 1, 100, 1, {}) table.sort(u)",
            200,
        ),
        table(
            "table_sort1000",
            list(1000),
            "local u = table.move(t, 1, 1000, 1, {}) table.sort(u)",
            20,
        ),
        table(
            "table_sort1000_fn",
            list(1000),
            "local u = table.move(t, 1, 1000, 1, {}) table.sort(u, function(a, b) return a > b end)",
            20,
        ),
    ];
    time_source_jobs(rows)
}

/// `goto` (Phase 3.19), per pass. `goto_forward` skips one statement in a
/// numeric `for` body, against `goto_none`, the same loop without it.
/// `goto_backward` is a loop made of a label and a guarded backward goto,
/// against `goto_while`, the same loop as `while`. `goto_upval` leaves a
/// captured local each pass, so it closes an upvalue (and allocates the
/// closure that captured it); `goto_close` leaves a `<close>` local whose
/// `__close` is an empty Lua function; `goto_nested` leaves three nested
/// blocks, a captured local in the innermost. `goto_quantum1` is
/// `goto_backward` run one instruction per slice.
fn goto_rows() -> Vec<Sample> {
    const N: i64 = 20_000;
    let job = |name, source: String| (name, source, vec![N], N, Config::default());
    let rows: Vec<SourceJob> = vec![
        job(
            "goto_none",
            format!("local c = 0 for i = 1, {N} do c = c + 1 end return c"),
        ),
        job(
            "goto_forward",
            format!(
                "local c = 0 for i = 1, {N} do goto skip c = c - 1 ::skip:: c = c + 1 end return c"
            ),
        ),
        job(
            "goto_while",
            format!("local c = 0 while c < {N} do c = c + 1 end return c"),
        ),
        job(
            "goto_backward",
            format!("local c = 0 ::top:: c = c + 1 if c < {N} then goto top end return c"),
        ),
        job(
            "goto_upval",
            format!(
                "local c, f = 0, nil ::top:: do local x = c f = function() return x end \
                 c = c + 1 if c < {N} then goto top end end return c"
            ),
        ),
        job(
            "goto_close",
            format!(
                "local c = 0 local v = setmetatable({{}}, {{ __close = function() end }}) \
                 ::top:: do local x <close> = v c = c + 1 if c < {N} then goto top end end \
                 return c"
            ),
        ),
        job(
            "goto_nested",
            format!(
                "local c, f = 0, nil ::top:: do local a = 1 do local b = 2 do local x = c \
                 f = function() return x end c = c + 1 if c < {N} then goto top end end end end \
                 return c"
            ),
        ),
    ];
    let mut samples = time_source_jobs(rows);
    if wanted("goto_quantum1") {
        let source = format!("local c = 0 ::top:: c = c + 1 if c < {N} then goto top end return c");
        let chunk = crate::compile(source.as_bytes()).expect("goto loop");
        let mut runs = Vec::with_capacity(20);
        let mut fuel = 0;
        for _ in 0..20 {
            let mut runtime = Runtime::boot(
                Config::default(),
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )
            .expect("boot");
            let mut journal = Journal::new();
            let start = Instant::now();
            while matches!(
                runtime.run(1, &mut journal).expect("run"),
                StepOutcome::Paused(_)
            ) {}
            runs.push(start.elapsed().as_nanos());
            fuel = runtime.fuel_consumed();
        }
        let ns = median_ns(runs);
        samples.push(Sample {
            name: "goto_quantum1",
            ns_per_op: ns / N as u128,
            detail: format!("median {ns} ns/run, iters {N}, fuel {fuel}"),
        });
    }
    samples
}

/// Runs each job's source 20 times from a fresh boot with the proof natives
/// and base functions bound, checks its integer results, and reports the
/// median per iteration.
fn time_source_jobs(jobs: Vec<SourceJob>) -> Vec<Sample> {
    let mut samples = Vec::new();
    for (name, source, expected, iterations, config) in jobs {
        if !wanted(name) {
            continue;
        }
        let chunk = crate::compile(source.as_bytes()).expect(name);
        let mut runs = Vec::with_capacity(20);
        let mut fuel = 0;
        let mut objects = 0;
        let mut cold = 0;
        let mut collections = 0;
        for _ in 0..20 {
            let mut runtime =
                Runtime::boot(config.clone(), HostRegistry::proof(), &chunk.proto, false)
                    .expect("boot");
            bind_natives(&mut runtime);
            // Only rows that use them pay for the libraries at boot.
            if source.contains("math.") || source.contains("table.") {
                runtime.install_math().expect("math");
                runtime.install_table().expect("table");
            }
            if source.contains("string.") {
                runtime.install_string().expect("string");
            }
            if source.contains("coroutine.") {
                runtime.install_coroutine().expect("coroutine");
            }
            if source.contains("newud(") || source.contains("light(") || source.contains("counter_")
            {
                for (name, _) in crate::host::USERDATA_NATIVES {
                    runtime
                        .set_global_native(name, name)
                        .expect("userdata native");
                }
            }
            if source.contains("debug.") || source.contains("require") {
                runtime.install_package().expect("package");
                runtime.install_debug().expect("debug");
            }
            let mut journal = Journal::new();
            let before = runtime.heap().next_object_id;
            let start = Instant::now();
            let outcome = runtime
                .run_until_terminal(u64::MAX, &mut journal)
                .expect("run");
            runs.push(start.elapsed().as_nanos());
            objects = runtime.heap().next_object_id - before;
            cold = runtime.cold_steps;
            collections = runtime.memory().collections;
            assert!(
                matches!(outcome, StepOutcome::Completed),
                "{name} {outcome:?}"
            );
            assert_eq!(
                runtime.entry_results().expect("results"),
                expected
                    .iter()
                    .map(|value| Value::Integer(*value))
                    .collect::<Vec<_>>(),
                "{name}"
            );
            fuel = runtime.fuel_consumed();
        }
        let ns = median_ns(runs);
        samples.push(Sample {
            name,
            ns_per_op: ns / iterations as u128,
            detail: format!(
                "median {ns} ns/run, iters {iterations}, fuel {fuel}, cold steps {cold}, objects allocated {objects}, collections {collections}"
            ),
        });
    }
    samples
}

/// The string library (Phase 3.23), per call. A row's setup makes its
/// strings before the loop; each call's result is dropped, so rows that
/// make strings include their collection.
fn string_rows() -> Vec<Sample> {
    let job = |name, setup: &str, body: &str, calls: i64| -> SourceJob {
        (
            name,
            format!("{setup} local c = 0 for i = 1, {calls} do {body} c = c + 1 end return c"),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let text = "local s = string.rep('abcdefgh', 128)";
    let words = "local s = string.rep('word ', 200)";
    let ints = "local t = {} for i = 1, 1000 do t[i] = i end local fmt = string.rep('i4', 1000)";
    let big = "local src = {} for i = 1, 300 do src[i] = 'x = x + ' .. i end \
               local big = load('local x = 0 ' .. table.concat(src, ' ') .. ' return x')";
    let rows: Vec<SourceJob> = vec![
        job("string_len", text, "local n = string.len(s)", 20_000),
        job(
            "string_sub16",
            text,
            "local r = string.sub(s, 5, 20)",
            20_000,
        ),
        job(
            "string_sub1k",
            text,
            "local r = string.sub(s, 1, 1000)",
            5_000,
        ),
        job(
            "string_reverse1k",
            text,
            "local r = string.reverse(s)",
            5_000,
        ),
        job(
            "string_reverse1m",
            "local s = string.rep('ab', 1 << 19)",
            "local r = string.reverse(s)",
            10,
        ),
        job("string_lower1k", text, "local r = string.lower(s)", 5_000),
        job(
            "string_rep300",
            "",
            "local r = string.rep('abc', 100, ',')",
            5_000,
        ),
        job("string_byte1", text, "local b = string.byte(s, 10)", 20_000),
        job(
            "string_byte16",
            text,
            "local a, b, x, y = string.byte(s, 1, 16)",
            10_000,
        ),
        job(
            "string_find_plain",
            &format!("{text} s = s .. 'needle'"),
            "local a = string.find(s, 'needle', 1, true)",
            2_000,
        ),
        job(
            "string_find_simple",
            &format!("{text} s = s .. 'needle'"),
            "local a = string.find(s, 'ne+dle')",
            2_000,
        ),
        job(
            "string_find_capture",
            "local s = 'name = value'",
            "local a, b, k, v = string.find(s, '(%a+)%s*=%s*(%a+)')",
            20_000,
        ),
        job(
            "string_find_backtrack",
            "local s = string.rep('a', 1000)",
            "local a = string.find(s, 'a*b')",
            5,
        ),
        job(
            "string_match",
            "",
            "local y, m, d = string.match('2024-01-02', '(%d+)-(%d+)-(%d+)')",
            20_000,
        ),
        job(
            "string_gmatch200",
            words,
            "for w in string.gmatch(s, '%a+') do end",
            100,
        ),
        job(
            "string_gsub_string1k",
            text,
            "local r = string.gsub(s, 'a', 'A')",
            500,
        ),
        job(
            "string_gsub_function200",
            words,
            "local r = string.gsub(s, '%a+', function(w) return w end)",
            100,
        ),
        job(
            "string_format_int",
            "",
            "local r = string.format('%d', i)",
            20_000,
        ),
        job(
            "string_format_float",
            "",
            "local r = string.format('%.3f', i / 7)",
            20_000,
        ),
        job(
            "string_format_mixed",
            "",
            "local r = string.format('%s=%d (%5.2f) %q', 'k', i, i / 3, 'v')",
            10_000,
        ),
        job(
            "string_pack_small",
            "",
            "local p = string.pack('i4 i8 d', i, i, 1.5)",
            10_000,
        ),
        job(
            "string_unpack_small",
            "local p = string.pack('i4 i8 d', 1, 2, 1.5)",
            "local a, b, x = string.unpack('i4 i8 d', p)",
            10_000,
        ),
        job(
            "string_pack1k",
            ints,
            "local p = string.pack(fmt, table.unpack(t))",
            50,
        ),
        job(
            "string_unpack1k",
            &format!("{ints} local p = string.pack(fmt, table.unpack(t))"),
            "local x = string.unpack(fmt, p)",
            50,
        ),
        job(
            "string_dump_small",
            "local f = function(a) return a + 1 end",
            "local d = string.dump(f)",
            5_000,
        ),
        job("string_dump_large", big, "local d = string.dump(big)", 200),
        job(
            "string_load_binary",
            &format!("{big} local d = string.dump(big)"),
            "local g = load(d, 'b', 'b')",
            200,
        ),
        job(
            "string_load_text",
            &format!("{big} local text = 'local x = 0 ' .. table.concat(src, ' ') .. ' return x'"),
            "local g = load(text)",
            200,
        ),
    ];
    let mut samples = time_source_jobs(rows);
    if wanted("string_snapshot_mid_gsub") {
        samples.push(snapshot_mid_gsub());
    }
    samples
}

/// `require` and the debug library (Phase 3.24), per call; tracebacks at
/// three depths; and what debug information costs a large function.
fn debug_rows() -> Vec<Sample> {
    let job = |name, setup: &str, body: &str, calls: i64| -> SourceJob {
        (
            name,
            format!("{setup} local c = 0 for i = 1, {calls} do {body} c = c + 1 end return c"),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let deep = |name, depth: u32, calls: i64| -> SourceJob {
        (
            name,
            format!(
                "local function deep(n, k) if n == 0 then local c = 0 \
                   for i = 1, k do local r = debug.traceback('x') c = c + 1 end return c end \
                   local r = deep(n - 1, k) return r end \
                 local r = deep({depth}, {calls}) return r"
            ),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let rows: Vec<SourceJob> = vec![
        job("require_loaded", "", "local m = require('_G')", 20_000),
        job(
            "require_preload",
            "for i = 1, 2000 do package.preload['m' .. i] = function() return i end end \
             local names = {} for i = 1, 2000 do names[i] = 'm' .. i end",
            "local m = require(names[i])",
            2_000,
        ),
        job(
            "debug_getinfo_level",
            "",
            "local t = debug.getinfo(1, 'Sl')",
            10_000,
        ),
        job(
            "debug_getinfo_full",
            "local function f(a, b) return a end",
            "local t = debug.getinfo(f)",
            10_000,
        ),
        job(
            "debug_getlocal",
            "local x, y = 1, 2",
            "local n, v = debug.getlocal(1, 2)",
            20_000,
        ),
        job(
            "debug_getupvalue",
            "local u = 1 local function f() return u end",
            "local n, v = debug.getupvalue(f, 1)",
            20_000,
        ),
        deep("debug_traceback_10", 10, 2_000),
        deep("debug_traceback_100", 100, 1_000),
        deep("debug_traceback_1000", 990, 500),
    ];
    let mut samples = time_source_jobs(rows);
    if wanted("debug_metadata_size") {
        samples.push(debug_metadata_size());
    }
    samples
}

/// The coroutine library (Phase 3.25), per operation: a switch is a
/// resume and the yield or return that answers it.
fn coroutine_rows() -> Vec<Sample> {
    let job = |name, setup: &str, body: &str, calls: i64| -> SourceJob {
        (
            name,
            format!("{setup} local c = 0 for i = 1, {calls} do {body} c = c + 1 end return c"),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let looping =
        "local co = coroutine.create(function() while true do coroutine.yield(1) end end)";
    let mt = "local mt = { __close = function() end }";
    let rows: Vec<SourceJob> = vec![
        job(
            "co_create",
            "local f = function() end",
            "local co = coroutine.create(f)",
            10_000,
        ),
        job(
            "co_first_resume_return",
            "local f = function(a) return a end",
            "local ok, v = coroutine.resume(coroutine.create(f), i)",
            10_000,
        ),
        job(
            "co_resume_yield",
            looping,
            "local ok, v = coroutine.resume(co)",
            20_000,
        ),
        job(
            "co_wrap_yield",
            "local w = coroutine.wrap(function() while true do coroutine.yield(1) end end)",
            "local v = w()",
            20_000,
        ),
        job(
            "co_wrap_return",
            "local f = function(a) return a end",
            "local v = coroutine.wrap(f)(i)",
            10_000,
        ),
        job(
            "co_direct_call",
            "local f = function(a) return a end",
            "local v = f(i)",
            20_000,
        ),
        job(
            "co_status",
            looping,
            "local s = coroutine.status(co)",
            20_000,
        ),
        job("co_running", "", "local t, m = coroutine.running()", 20_000),
        job(
            "co_isyieldable",
            "",
            "local y = coroutine.isyieldable()",
            20_000,
        ),
        job(
            "co_close_empty",
            "local f = function() coroutine.yield() end",
            "local co = coroutine.create(f) coroutine.resume(co) coroutine.close(co)",
            10_000,
        ),
        job(
            "co_close_tbc",
            &format!(
                "{mt} local f = function() local x <close> = setmetatable({{}}, mt) coroutine.yield() end"
            ),
            "local co = coroutine.create(f) coroutine.resume(co) coroutine.close(co)",
            10_000,
        ),
    ];
    let mut samples = time_source_jobs(rows);
    if wanted("co_snapshot_delta") {
        samples.push(coroutine_snapshot_delta());
    }
    samples
}

/// Userdata (Phase 3.26): making them, comparing them, keys, metamethods,
/// user values, host borrows and methods, `upvalueid`, collection, and
/// snapshots. `ud_table_field` is the table baseline beside them.
fn userdata_rows() -> Vec<Sample> {
    let job = |name, setup: &str, body: &str, calls: i64| -> SourceJob {
        (
            name,
            format!("{setup} local c = 0 for i = 1, {calls} do {body} c = c + 1 end return c"),
            vec![calls],
            calls,
            Config::default(),
        )
    };
    let methods = "local C = {} C.__index = C C.get = counter_get local h = counter_new(1, C)";
    let rows: Vec<SourceJob> = vec![
        job("ud_create_0", "", "local u = newud(0)", 5_000),
        job("ud_create_64", "", "local u = newud(64)", 5_000),
        job("ud_create_4k", "", "local u = newud(4096)", 2_000),
        job("ud_create_uv4", "", "local u = newud(0, 4)", 5_000),
        job(
            "ud_eq_full",
            "local a, b = newud(0), newud(0)",
            "local e = a == b",
            20_000,
        ),
        job(
            "ud_eq_light",
            "local a, b = light(1), light(2)",
            "local e = a == b",
            20_000,
        ),
        job(
            "ud_key_full",
            "local u = newud(0) local t = { [u] = 1 }",
            "local v = t[u]",
            20_000,
        ),
        job(
            "ud_key_light",
            "local l = light(1) local t = { [l] = 1 }",
            "local v = t[l]",
            20_000,
        ),
        job(
            "ud_table_field",
            "local t = { x = 1 }",
            "local v = t.x",
            20_000,
        ),
        job(
            "ud_meta_index",
            "local u = debug.setmetatable(newud(0), { __index = { x = 1 } })",
            "local v = u.x",
            20_000,
        ),
        job(
            "ud_meta_newindex",
            "local s = {} local u = debug.setmetatable(newud(0), { __newindex = s })",
            "u.x = i",
            20_000,
        ),
        job(
            "ud_uservalue_get",
            "local u = newud(0, 1) local get = debug.getuservalue",
            "local v = get(u)",
            20_000,
        ),
        job(
            "ud_uservalue_set",
            "local u = newud(0, 1) local set = debug.setuservalue",
            "set(u, i)",
            20_000,
        ),
        job(
            "ud_host_borrow",
            "local h = counter_new(1)",
            "local v = counter_get(h)",
            20_000,
        ),
        job("ud_host_method", methods, "local v = h:get()", 20_000),
        job(
            "ud_upvalueid",
            "local x = 1 local f = function() return x end local id = debug.upvalueid",
            "local v = id(f, 1)",
            20_000,
        ),
    ];
    let mut samples = time_source_jobs(rows);
    if [
        "ud_collect_0",
        "ud_collect_uv4",
        "ud_collect_4k",
        "ud_collect_mt",
    ]
    .iter()
    .any(|name| wanted(name))
    {
        samples.extend(userdata_collect());
    }
    if wanted("ud_snapshot_delta") {
        samples.push(userdata_snapshot_delta());
    }
    samples
}

/// A full collection over 4,000 live userdata of each shape, per object:
/// what tracing userdata costs.
fn userdata_collect() -> Vec<Sample> {
    let shapes = [
        ("ud_collect_0", "newud(0)"),
        ("ud_collect_uv4", "newud(0, 4)"),
        ("ud_collect_4k", "newud(4096)"),
        ("ud_collect_mt", "debug.setmetatable(newud(0), mt)"),
    ];
    let mut samples = Vec::new();
    for (name, make) in shapes {
        let source =
            format!("local mt = {{}} keep = {{}} for i = 1, 4000 do keep[i] = {make} end return 1");
        let chunk = crate::compile(source.as_bytes()).expect(name);
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        bind_natives(&mut runtime);
        runtime.install_debug().expect("debug");
        for (native, _) in crate::host::USERDATA_NATIVES {
            runtime
                .set_global_native(native, native)
                .expect("userdata native");
        }
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        let mut runs = Vec::with_capacity(20);
        for _ in 0..20 {
            let start = Instant::now();
            runtime.collect();
            runs.push(start.elapsed().as_nanos());
        }
        let ns = median_ns(runs);
        samples.push(Sample {
            name,
            ns_per_op: ns / 4000,
            detail: format!(
                "median {ns} ns per full collection of 4000, logical heap {} bytes",
                runtime.memory().logical_bytes
            ),
        });
    }
    samples
}

/// The snapshot bytes 1 and 100 userdata add: 16 bytes and two user
/// values each, or a portable host counter each.
fn userdata_snapshot_delta() -> Sample {
    let size = |make: &str, count: u32| {
        let source = format!("keep = {{}} for i = 1, {count} do keep[i] = {make} end return 1");
        let chunk = crate::compile(source.as_bytes()).expect("ud_snapshot_delta");
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        for (native, _) in crate::host::USERDATA_NATIVES {
            runtime
                .set_global_native(native, native)
                .expect("userdata native");
        }
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        runtime.collect();
        let start = Instant::now();
        let bytes = runtime.snapshot().expect("snapshot").len();
        (bytes, start.elapsed().as_nanos())
    };
    let raw = "newud(16, 2)";
    let host = "counter_new(i)";
    let (none, _) = size(raw, 0);
    let (raw1, _) = size(raw, 1);
    let (raw100, raw_ns) = size(raw, 100);
    let (host1, _) = size(host, 1);
    let (host100, host_ns) = size(host, 100);
    Sample {
        name: "ud_snapshot_delta",
        ns_per_op: 0,
        detail: format!(
            "snapshot {none} bytes empty; raw +{} with one, +{} with 100 ({raw_ns} ns); \
             portable counter +{} with one, +{} with 100 ({host_ns} ns)",
            raw1 - none,
            raw100 - none,
            host1 - none,
            host100 - none
        ),
    }
}

/// The snapshot bytes 1 and 100 suspended coroutines add, each yielded
/// from a small function with two locals.
fn coroutine_snapshot_delta() -> Sample {
    let size = |count: u32| {
        let source = format!(
            "local keep = {{}} for i = 1, {count} do \
               local co = coroutine.create(function(a) local b = a + 1 coroutine.yield(b) end) \
               coroutine.resume(co, i) keep[i] = co end return 1"
        );
        let chunk = crate::compile(source.as_bytes()).expect("co_snapshot_delta");
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        runtime.install_coroutine().expect("coroutine");
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        runtime.collect();
        runtime.snapshot().expect("snapshot").len()
    };
    let (none, one, hundred) = (size(0), size(1), size(100));
    Sample {
        name: "co_snapshot_delta",
        ns_per_op: 0,
        detail: format!(
            "snapshot {none} bytes with no coroutine, +{} with one, +{} with 100",
            one - none,
            hundred - none
        ),
    }
}

/// What a 300-statement function's debug information costs: its logical
/// size, and the snapshot bytes with it and without it.
fn debug_metadata_size() -> Sample {
    let mut body = String::from("local function big(a) local x = 0 ");
    for i in 0..300 {
        body.push_str(&format!("local v{i} = a + {i} x = x + v{i} "));
        if i % 190 == 189 {
            body.push_str("end local function big2(a) local x = 0 ");
        }
    }
    body.push_str("return x end return 1");
    let chunk = crate::compile(body.as_bytes()).expect("debug_metadata_size");
    fn strip(spec: &mut program::ProtoSpec) {
        spec.debug = None;
        spec.children.iter_mut().for_each(strip);
    }
    fn logical(spec: &program::ProtoSpec) -> u64 {
        spec.debug.as_ref().map_or(0, |debug| debug.logical_size())
            + spec.children.iter().map(logical).sum::<u64>()
    }
    let mut stripped = chunk.proto.clone();
    strip(&mut stripped);
    let snapshot = |spec: &program::ProtoSpec| {
        let runtime =
            Runtime::boot(Config::default(), HostRegistry::proof(), spec, false).expect("boot");
        let start = Instant::now();
        let bytes = runtime.snapshot().expect("snapshot").len();
        (bytes, start.elapsed().as_nanos())
    };
    let (with, ns) = snapshot(&chunk.proto);
    let (without, _) = snapshot(&stripped);
    Sample {
        name: "debug_metadata_size",
        ns_per_op: ns,
        detail: format!(
            "{} instructions, debug information {} logical bytes, snapshot {with} bytes with it, {without} without",
            count_ops(&chunk.proto),
            logical(&chunk.proto)
        ),
    }
}

/// The snapshot bytes a `gsub` in progress adds: the same program paused
/// before the call and inside it, 40 KiB into a 64 KiB result.
fn snapshot_mid_gsub() -> Sample {
    let source = "local s = string.rep('ab', 32768) local r = string.gsub(s, 'b', 'c') return #r";
    let chunk = crate::compile(source.as_bytes()).expect("snapshot_mid_gsub");
    let size_after = |fuel: u64| {
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        runtime.install_string().expect("string");
        let mut journal = Journal::new();
        for _ in 0..fuel {
            runtime.run(1, &mut journal).expect("run");
        }
        let start = Instant::now();
        let bytes = runtime.snapshot().expect("snapshot").len();
        (bytes, start.elapsed().as_nanos())
    };
    // The `rep` takes 16 steps; the `gsub`, a step per 256 units of work.
    let (before, _) = size_after(18);
    let (inside, ns) = size_after(300);
    Sample {
        name: "string_snapshot_mid_gsub",
        ns_per_op: ns,
        detail: format!("snapshot {inside} bytes inside the gsub, {before} before it"),
    }
}

/// Garbage-making loops with automatic collection on and off, and full
/// collections of reachable heaps of three sizes. Churn rows are ns per
/// iteration, collections included; pause rows are ns per collection.
/// Weak tables, ephemerons, and finalizers (Phase 3.27). Full-collection
/// pauses over live heaps of each shape (median, p95, p99 of 20); a
/// collection settling ephemeron chains built backwards, in one table and
/// across many; one clearing dead weak entries; and finalizer costs per
/// object, against the same program without `__gc`.
fn gc_semantics_rows() -> Vec<Sample> {
    const N: usize = 4_000;
    let boot = |source: &str| {
        let chunk = crate::compile(source.as_bytes()).expect("gc semantics");
        let config = Config {
            auto_gc: false,
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).expect("boot");
        bind_natives(&mut runtime);
        for (name, _) in crate::host::USERDATA_NATIVES {
            runtime
                .set_global_native(name, name)
                .expect("userdata native");
        }
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        runtime
    };
    let percentile =
        |sorted: &[u128], p: usize| sorted[(sorted.len() * p / 100).min(sorted.len() - 1)];
    let mut samples = Vec::new();
    let live = format!("keep = {{}} for i = 1, {N} do keep[i] = {{}} end");
    let shapes: [(&'static str, String); 6] = [
        ("gc_full_strong", live.clone()),
        (
            "gc_full_weakv",
            format!(
                "{live} w = setmetatable({{}}, {{__mode = 'v'}}) for i = 1, {N} do w[i] = keep[i] end"
            ),
        ),
        (
            "gc_full_ephemeron",
            format!(
                "{live} e = setmetatable({{}}, {{__mode = 'k'}}) for i = 1, {N} do e[keep[i]] = i end"
            ),
        ),
        (
            "gc_full_userdata",
            format!("keep = {{}} for i = 1, {N} do keep[i] = newud(0, 1) end"),
        ),
        (
            "gc_full_finalizable",
            format!(
                "local mt = {{__gc = function() end}} keep = {{}} for i = 1, {N} do keep[i] = setmetatable({{}}, mt) end"
            ),
        ),
        (
            "gc_full_mixed",
            format!(
                "local mt = {{__gc = function() end}} keep = {{}} e = setmetatable({{}}, {{__mode = 'k'}}) \
                 w = setmetatable({{}}, {{__mode = 'v'}}) \
                 for i = 1, {N} / 4 do local t = setmetatable({{}}, mt) local u = newud(8, 1) \
                   keep[#keep + 1] = t keep[#keep + 1] = u e[t] = {{}} w[i] = u end"
            ),
        ),
    ];
    for (name, source) in shapes {
        if !wanted(name) {
            continue;
        }
        let mut runtime = boot(&source);
        runtime.collect();
        let mut runs = Vec::with_capacity(20);
        for _ in 0..20 {
            let start = Instant::now();
            runtime.collect();
            runs.push(start.elapsed().as_nanos());
        }
        runs.sort_unstable();
        samples.push(Sample {
            name,
            ns_per_op: runs[10] / N as u128,
            detail: format!(
                "full collection: median {} ns, p95 {} ns, p99 {} ns; {} objects, logical heap {} bytes",
                runs[10],
                percentile(&runs, 95),
                percentile(&runs, 99),
                runtime.memory().objects,
                runtime.memory().logical_bytes
            ),
        });
    }
    // Ephemeron chains built so every table, traversed in order, finds
    // its key unmarked: settled as keys are marked, never by rescanning.
    let chains: [(&'static str, String, usize); 2] = [
        (
            "gc_ephemeron_chain",
            "head = {} e = setmetatable({}, {__mode = 'k'}) local keys = {head} \
             for i = 2, 4500 do keys[i] = {} end for i = 4500, 2, -1 do e[keys[i - 1]] = keys[i] end"
                .to_string(),
            4500,
        ),
        (
            "gc_ephemeron_tables",
            "head = {} tabs = {} local key = head \
             for i = 1, 1500 do tabs[i] = setmetatable({}, {__mode = 'k'}) end \
             for i = 1500, 1, -1 do local nk = {} tabs[i][key] = nk key = nk end"
                .to_string(),
            1500,
        ),
    ];
    for (name, source, links) in chains {
        if !wanted(name) {
            continue;
        }
        let mut runtime = boot(&source);
        runtime.collect();
        let mut runs = Vec::with_capacity(20);
        for _ in 0..20 {
            let start = Instant::now();
            runtime.collect();
            runs.push(start.elapsed().as_nanos());
        }
        runs.sort_unstable();
        samples.push(Sample {
            name,
            ns_per_op: runs[10] / links as u128,
            detail: format!(
                "{links} links live through ephemerons: median {} ns per full collection, p99 {} ns; {} objects",
                runs[10],
                percentile(&runs, 99),
                runtime.memory().objects
            ),
        });
    }
    // One collection clearing `n` dead weak values, from a fresh heap each
    // time.
    for (name, n) in [
        ("gc_weak_clear_100", 100),
        ("gc_weak_clear_1000", 1000),
        ("gc_weak_clear_9000", 9000),
    ] {
        if !wanted(name) {
            continue;
        }
        let source = format!(
            "w = setmetatable({{}}, {{__mode = 'v'}}) local function fill() for i = 1, {n} do w[i] = {{}} end end fill()"
        );
        let mut runs = Vec::with_capacity(20);
        for _ in 0..20 {
            let mut runtime = boot(&source);
            let start = Instant::now();
            runtime.collect();
            runs.push(start.elapsed().as_nanos());
        }
        runs.sort_unstable();
        samples.push(Sample {
            name,
            ns_per_op: runs[10] / n as u128,
            detail: format!("median {} ns for the collection clearing {n}", runs[10]),
        });
    }
    // Finalizers: the same garbage with a metatable without `__gc`, with
    // an empty Lua `__gc`, a native one, and a second collection after.
    let fin = |gc: &str, after: &str| {
        format!(
            "collectgarbage('stop') local mt = {{{gc}}} \
             local function make() for i = 1, {N} do setmetatable({{}}, mt) end end \
             make() collectgarbage() {after} return 1"
        )
    };
    let rows: Vec<SourceJob> = vec![
        (
            "gc_fin_none",
            fin("", ""),
            vec![1],
            N as i64,
            Config::default(),
        ),
        (
            "gc_fin_lua",
            fin("__gc = function() end", ""),
            vec![1],
            N as i64,
            Config::default(),
        ),
        (
            "gc_fin_native",
            fin("__gc = none", ""),
            vec![1],
            N as i64,
            Config::default(),
        ),
        (
            "gc_fin_second",
            fin("__gc = function() end", "collectgarbage()"),
            vec![1],
            N as i64,
            Config::default(),
        ),
    ];
    samples.extend(time_source_jobs(rows));
    samples
}

fn gc_rows() -> Vec<Sample> {
    const N: i64 = 4_000;
    let churn: [(&'static str, &'static str, String, i64); 4] = [
        (
            "gc_tables_on",
            "gc_tables_off",
            format!(
                "local s = 0 for i = 1, {N} do local t = {{ i, i + 1 }} s = s + t[2] end return s"
            ),
            N * (N + 1) / 2 + N,
        ),
        (
            "gc_closures_on",
            "gc_closures_off",
            format!(
                "local s = 0 for i = 1, {N} do local f = function() return i end s = s + f() end return s"
            ),
            N * (N + 1) / 2,
        ),
        (
            "gc_metamethod_on",
            "gc_metamethod_off",
            format!(
                "local t = setmetatable({{}}, {{ __index = function(self, key) return {{ 1 }} end }}) local s = 0 for i = 1, {N} do s = s + t.foo[1] end return s"
            ),
            N,
        ),
        (
            "gc_retained_on",
            "gc_retained_off",
            format!("local keep = {{}} for i = 1, {N} do keep[i] = {{}} end return #keep"),
            N,
        ),
    ];
    let mut samples = Vec::new();
    for (on, off, source, expected) in churn {
        for (name, auto_gc) in [(on, true), (off, false)] {
            if !wanted(name) {
                continue;
            }
            let chunk = crate::compile(source.as_bytes()).expect(name);
            let mut runs = Vec::with_capacity(20);
            let mut memory = None;
            for _ in 0..20 {
                let config = Config {
                    auto_gc,
                    ..Config::default()
                };
                let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false)
                    .expect("boot");
                bind_natives(&mut runtime);
                let mut journal = Journal::new();
                let start = Instant::now();
                let outcome = runtime
                    .run_until_terminal(u64::MAX, &mut journal)
                    .expect("run");
                runs.push(start.elapsed().as_nanos());
                assert_eq!(outcome, StepOutcome::Completed, "{name}");
                assert_eq!(
                    runtime.entry_results().expect("results"),
                    vec![Value::Integer(expected)],
                    "{name}"
                );
                memory = Some(runtime.memory());
            }
            let ns = median_ns(runs);
            let memory = memory.expect("ran");
            samples.push(Sample {
                name,
                ns_per_op: ns / N as u128,
                detail: format!(
                    "median {ns} ns/run, iters {N}, collections {}, objects at end {}, logical bytes {}",
                    memory.collections, memory.objects, memory.logical_bytes
                ),
            });
        }
    }
    for (name, size) in [
        ("gc_pause_small", 100),
        ("gc_pause_medium", 2_000),
        ("gc_pause_large", 9_000),
    ] {
        if !wanted(name) {
            continue;
        }
        let source = format!("keep = {{}} for i = 1, {size} do keep[i] = {{ i, i }} end return 7");
        let chunk = crate::compile(source.as_bytes()).expect(name);
        let config = Config {
            auto_gc: false,
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).expect("boot");
        bind_natives(&mut runtime);
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        assert_eq!(outcome, StepOutcome::Completed, "{name}");
        let mut pauses = Vec::with_capacity(200);
        for _ in 0..200 {
            let start = Instant::now();
            runtime.collect();
            pauses.push(start.elapsed().as_nanos());
        }
        pauses.sort_unstable();
        let at = |q: usize| pauses[(pauses.len() * q / 100).min(pauses.len() - 1)];
        let memory = runtime.memory();
        samples.push(Sample {
            name,
            ns_per_op: at(50),
            detail: format!(
                "full collection of a reachable heap: median {} p95 {} p99 {} ns, objects {}, logical bytes {}",
                at(50),
                at(95),
                at(99),
                memory.objects,
                memory.logical_bytes
            ),
        });
    }
    samples
}

/// One `t.x` over an `__index` cycle: 2000 raw lookups, then the fault.
fn chain_limit() -> Sample {
    let chunk =
        crate::compile(b"local t = {} setmetatable(t, { __index = t }) return t.x").expect("cycle");
    let mut runs = Vec::with_capacity(20);
    for _ in 0..20 {
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        bind_natives(&mut runtime);
        let mut journal = Journal::new();
        let start = Instant::now();
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut journal)
            .expect("run");
        runs.push(start.elapsed().as_nanos());
        assert_eq!(
            outcome,
            StepOutcome::LuaError(crate::id::LuaFault::MetaChain)
        );
    }
    let ns = median_ns(runs);
    Sample {
        name: "meta_chain_2000",
        ns_per_op: ns,
        detail: format!("one run: setmetatable, then a 2000-step chain that faults; {ns} ns"),
    }
}

/// One native wait per iteration of `source` (2000 of them): run to
/// `Waiting`, `complete_wait`, and run on. The host side is inside the timer.
fn native_wait_cycle(name: &'static str, source: &str) -> Sample {
    const W: i64 = 2_000;
    let chunk = crate::compile(source.as_bytes()).expect("wait loop");
    let mut runs = Vec::with_capacity(20);
    for _ in 0..20 {
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .expect("boot");
        bind_natives(&mut runtime);
        let mut journal = Journal::new();
        let start = Instant::now();
        let mut waits = 0;
        loop {
            match runtime
                .run_until_terminal(u64::MAX, &mut journal)
                .expect("run")
            {
                StepOutcome::Waiting(key) => {
                    runtime.complete_wait(key, 0).expect("complete");
                    waits += 1;
                }
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        runs.push(start.elapsed().as_nanos());
        assert_eq!(waits, W);
    }
    let ns = median_ns(runs);
    Sample {
        name,
        ns_per_op: ns / W as u128,
        detail: format!("median {ns} ns/run, waits {W}"),
    }
}

fn op_mix(spec: &program::ProtoSpec) -> (u64, u64) {
    let mut runtime =
        Runtime::boot(Config::default(), HostRegistry::proof(), spec, false).expect("boot");
    let mut journal = Journal::new();
    runtime
        .run_until_terminal(u64::MAX, &mut journal)
        .expect("run");
    let cold = runtime.cold_steps;
    (runtime.fuel_consumed() - cold, cold)
}

fn median_ns(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn count_ops(spec: &program::ProtoSpec) -> usize {
    spec.ops.len() + spec.children.iter().map(count_ops).sum::<usize>()
}

/// Scale rows start timing after the live set is built and made old.
/// One unrestricted run measures natural slices; a second, 1,000-fuel
/// run observes heap/list peaks, as in `incremental_rows`.
fn scale_gc_rows() -> Vec<Sample> {
    let shapes = [
        (
            "live_100k",
            "keep = {} for i = 1, 100000 do keep[i] = {} end",
            100_000,
        ),
        (
            "live_500k",
            "keep = {} for i = 1, 500000 do keep[i] = {} end",
            500_000,
        ),
        (
            "table_100k",
            "keep = {} for i = 1, 100000 do keep[i] = i end",
            0,
        ),
        (
            "threads_10k",
            "keep = {} local function f() coroutine.yield() end for i = 1, 10000 do local co = coroutine.create(f) assert(coroutine.resume(co)) keep[i] = co end",
            10_000,
        ),
        (
            "weakv_100k",
            "keep = {} w = setmetatable({}, {__mode = 'v'}) for i = 1, 100000 do keep[i] = {} w[i] = keep[i] end",
            100_000,
        ),
        (
            "ephemeron_50k",
            "keep = {} e = setmetatable({}, {__mode = 'k'}) for i = 1, 50000 do keep[i] = {} e[keep[i]] = {keep[i]} end",
            100_000,
        ),
        (
            "finalizable_20k",
            "keep = {} finalized = 0 local mt = {__gc = function() finalized = finalized + 1 end} for i = 1, 20000 do keep[i] = setmetatable({}, mt) end",
            20_000,
        ),
    ];
    let mut samples = Vec::new();
    for (mode, prefix) in [
        (crate::GcMode::Incremental, "scale_gc_inc_"),
        (crate::GcMode::Generational, "scale_gc_gen_"),
    ] {
        for (shape, build, live) in shapes {
            let name: &'static str = Box::leak(format!("{prefix}{shape}").into_boxed_str());
            if !wanted(name) {
                continue;
            }
            let release = if shape == "finalizable_20k" {
                "keep = nil collectgarbage() collectgarbage() assert(finalized == 20000)"
            } else {
                ""
            };
            let source = format!(
                "{build} collectgarbage() park() \
                 for round = 1, 1000 do \
                   local g = {{}} for j = 1, 100 do g[j] = {{j}} end \
                   keep[-round] = {{round}} \
                   if round % 100 == 0 then collectgarbage('step', 0) end \
                   if round % 250 == 0 then collectgarbage() end \
                 end {release}"
            );
            let chunk = crate::compile(source.as_bytes()).expect(name);
            let boot = || {
                let mut runtime = Runtime::boot(
                    Config {
                        gc_mode: mode,
                        ..Config::default()
                    },
                    HostRegistry::proof(),
                    &chunk.proto,
                    false,
                )
                .expect("boot");
                runtime.install_standard().expect("standard");
                runtime.set_global_native("park", "park").expect("park");
                let outcome = runtime
                    .run_until_terminal(u64::MAX, &mut Journal::new())
                    .expect("build");
                let StepOutcome::Waiting(key) = outcome else {
                    panic!("{name}: {outcome:?}")
                };
                assert!(runtime.memory().objects >= live, "{name}");
                runtime.complete_wait(key, 0).expect("resume");
                runtime.gc_slices.clear();
                runtime
            };
            let mut runtime = boot();
            let objects = runtime.memory().objects;
            let before = runtime.memory();
            let start = Instant::now();
            let outcome = runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .expect("run");
            let elapsed = start.elapsed().as_nanos();
            assert_eq!(outcome, StepOutcome::Completed, "{name}");
            let slices = &runtime.gc_slices;
            let young: Vec<_> = slices.iter().filter(|s| s.4).copied().collect();
            let major: Vec<_> = slices.iter().filter(|s| !s.4).copied().collect();
            let major_ns: u64 = major.iter().map(|s| s.1).sum();
            let major_units: u64 = major.iter().map(|s| s.0).sum();
            let majors =
                runtime.heap().gc.collections - (before.collections - before.young_collections);
            let minors = runtime.heap().gc.minors - before.young_collections;
            let major_atomic = major.iter().filter(|s| s.3).map(|s| s.1).max().unwrap_or(0);
            let mut sampled = boot();
            let (outcome, seen) = observe_collector(&mut sampled, 1000, &mut Journal::new());
            assert_eq!(outcome, StepOutcome::Completed, "{name}");
            let quantum_max = sampled.gc_slices.iter().map(|s| s.1).max().unwrap_or(0);
            let denom = u64::from(objects) * majors.max(1);
            samples.push(Sample {
                name,
                ns_per_op: u128::from(major_ns / denom),
                detail: format!(
                    "live {objects}; young {}; major {}; cycles young {minors} major {majors}; \
                     major ns/object/cycle {:.2}, units/object/cycle {:.3}; max major atomic {major_atomic} ns; \
                     quantum max {quantum_max} ns; peak logical {} bytes, objects {}; remembered {}; \
                     sampled metadata {} bytes ({:.2} bytes/live object); run {elapsed} ns",
                    scale_slice_summary(&young), scale_slice_summary(&major),
                    major_ns as f64 / denom as f64, major_units as f64 / denom as f64,
                    seen.peak, seen.objects, seen.remembered, seen.metadata,
                    seen.metadata as f64 / f64::from(objects),
                ),
            });
        }
    }
    samples
}

fn scale_slice_summary(slices: &[(u64, u64, bool, bool, bool)]) -> String {
    let distribution = |mut values: Vec<u64>| {
        values.sort_unstable();
        if values.is_empty() {
            return "0/0/0/0".to_string();
        }
        let p = |percent: usize| values[(values.len() * percent / 100).min(values.len() - 1)];
        format!("{}/{}/{}/{}", p(50), p(95), p(99), values[values.len() - 1])
    };
    format!(
        "{} slices, ns p50/p95/p99/max {}, units p50/p95/p99/max {}, total {} ns/{} units",
        slices.len(),
        distribution(slices.iter().map(|s| s.1).collect()),
        distribution(slices.iter().map(|s| s.0).collect()),
        slices.iter().map(|s| s.1).sum::<u64>(),
        slices.iter().map(|s| s.0).sum::<u64>(),
    )
}

#[derive(Default)]
struct CollectorObservation {
    peak: u64,
    objects: u32,
    remembered: usize,
    metadata: usize,
}

/// Vector lengths, not capacities: two mark/age bytes per arena slot,
/// an approximate queued bitmap, young/again lists, and collector lists.
/// Hash-map buckets, allocator headers and unused capacity are excluded.
fn collector_metadata(heap: &Heap) -> usize {
    fn arena<T>(arena: &crate::heap::Arena<T>) -> usize {
        let slots = arena.slot_count();
        2 * slots
            + slots.div_ceil(64) * 8
            + std::mem::size_of_val(arena.young())
            + std::mem::size_of_val(arena.again())
    }
    let c = &heap.collector;
    arena(&heap.strings)
        + arena(&heap.tables)
        + arena(&heap.protos)
        + arena(&heap.upvalues)
        + arena(&heap.closures)
        + arena(&heap.threads)
        + arena(&heap.native_closures)
        + arena(&heap.userdata)
        + std::mem::size_of_val(c.gray.as_slice())
        + std::mem::size_of_val(c.revisit.as_slice())
        + std::mem::size_of_val(c.touched.as_slice())
        + std::mem::size_of_val(c.weak.as_slice())
        + std::mem::size_of_val(c.ephemerons.as_slice())
        + std::mem::size_of_val(c.pairs.as_slice())
        + c.waiting
            .values()
            .map(|values| std::mem::size_of_val(values.as_slice()))
            .sum::<usize>()
}

fn observe_collector(
    runtime: &mut Runtime,
    quantum: u64,
    journal: &mut Journal,
) -> (StepOutcome, CollectorObservation) {
    let mut seen = CollectorObservation::default();
    let mut step = 0;
    loop {
        let outcome = runtime.run(quantum, journal).expect("run");
        seen.peak = seen.peak.max(runtime.memory().logical_bytes);
        seen.objects = seen.objects.max(runtime.memory().objects);
        let heap = runtime.heap();
        let again = heap.strings.again().len()
            + heap.tables.again().len()
            + heap.protos.again().len()
            + heap.upvalues.again().len()
            + heap.closures.again().len()
            + heap.threads.again().len()
            + heap.native_closures.again().len()
            + heap.userdata.again().len();
        if heap.collector.generational {
            seen.remembered = seen.remembered.max(heap.collector.revisit.len() + again);
        }
        if step % 61 == 0 || !matches!(outcome, StepOutcome::Paused(_)) {
            seen.metadata = seen.metadata.max(collector_metadata(heap));
        }
        step += 1;
        if !matches!(outcome, StepOutcome::Paused(_)) {
            return (outcome, seen);
        }
    }
}

impl Runtime {
    /// Unstable, feature-gated observation for the snapshot scale benchmark.
    /// The phase and whether the running collection is young.
    #[doc(hidden)] // Unstable benchmark helper; outside the embedding API.
    pub fn measurement_collection(&self) -> (&'static str, bool) {
        use crate::gc::Phase;
        let phase = match self.heap().collector.phase {
            Phase::Pause => "pause",
            Phase::Begin => "begin",
            Phase::Propagate => "mark",
            Phase::Atomic(_) => "atomic",
            Phase::Sweep => "sweep",
            Phase::Touched => "touched",
        };
        (phase, self.heap().collector.minor)
    }
}

/// The incremental collector (Phase 3.28, ADR 0050) and generational
/// collection (Phase 3.29, ADR 0051).
///
/// `gc_inc_<workload>` and `gc_gen_<workload>`: a program run to its end
/// with the default parameters, in incremental and in generational mode,
/// and no quantum, so each collector slice is a whole step, an atomic
/// phase, or a whole young collection: ns per slice at the median, with
/// p95, p99 and max, the largest slice of a young collection and of a
/// major (atomic) one, units, the collector's total time against the
/// run's, collections of each kind, and, from a run with a 1,000-fuel
/// quantum, the largest slice, the peak logical heap, and the most objects
/// remembered for the next young collection.
/// `gc_ref_<shape>`: the Phase 3.27
/// stop-the-world collector over the heaps `gc_full_<shape>` collects,
/// for the total cost. `bw_<write>`: a loop of one kind of write, in
/// incremental mode with no cycle running (the dormant barrier), as
/// `bw_<write>_marking` with one stopped while it marks (the barrier
/// armed), and in generational mode to a young (`_gen_young`) and an old
/// (`_gen_old`) object.
fn incremental_rows() -> Vec<Sample> {
    let mut samples = Vec::new();
    let percentile =
        |sorted: &[u64], p: usize| sorted[(sorted.len() * p / 100).min(sorted.len() - 1)];
    let workloads: [(&'static str, &str); 18] = [
        (
            "live_1k",
            "keep = {} for i = 1, 1000 do keep[i] = {i} end \
             for r = 1, 400 do local g = {} for j = 1, 40 do g[j] = {j} end end",
        ),
        (
            "live_9k",
            "keep = {} for i = 1, 9000 do keep[i] = {i} end \
             for r = 1, 400 do local g = {} for j = 1, 20 do g[j] = {j} end end",
        ),
        (
            "large_table",
            "big = {} for i = 1, 300000 do big[i] = i end \
             for r = 1, 400 do local g = {} for j = 1, 40 do g[j] = {j} end end",
        ),
        (
            "stacks",
            "local function deep(n) if n == 0 then return coroutine.yield() end local a, b, c = n, n, n return deep(n - 1) end \
             cos = {} for i = 1, 40 do local co = coroutine.create(deep) coroutine.resume(co, 150) cos[i] = co end \
             for r = 1, 400 do local g = {} for j = 1, 40 do g[j] = {j} end end",
        ),
        (
            "weak_values",
            "keep = {} w = setmetatable({}, {__mode = 'v'}) for i = 1, 4000 do keep[i] = {} w[i] = keep[i] end \
             for r = 1, 400 do local g = {} for j = 1, 40 do g[j] = {j} w[-j] = g[j] end end",
        ),
        (
            "ephemerons",
            "keep = {} e = setmetatable({}, {__mode = 'k'}) for i = 1, 4000 do keep[i] = {} e[keep[i]] = {i} end \
             for r = 1, 400 do local g = {} for j = 1, 40 do g[j] = {j} e[g[j]] = j end end",
        ),
        (
            "finalizable",
            "local mt = {__gc = function() end} keep = {} for i = 1, 2000 do keep[i] = setmetatable({}, mt) end \
             for r = 1, 400 do for j = 1, 10 do setmetatable({}, mt) end end",
        ),
        (
            "mixed",
            "local mt = {__gc = function() end} keep = {} e = setmetatable({}, {__mode = 'k'}) w = setmetatable({}, {__mode = 'v'}) \
             for i = 1, 1000 do local t = setmetatable({}, mt) local u = newud(8, 1) keep[#keep + 1] = t keep[#keep + 1] = u e[t] = {} w[i] = u end \
             big = {} for i = 1, 50000 do big[i] = i end \
             for r = 1, 400 do local g = {} for j = 1, 30 do g[j] = {j} end e[g] = r end",
        ),
        (
            "churn",
            "local s = 0 for i = 1, 60000 do local t = {i, i + 1} s = s + t[2] end",
        ),
        (
            "growing",
            "keep = {} for i = 1, 9000 do keep[i] = {i} if i % 10 == 0 then local g = {} for j = 1, 10 do g[j] = j end end end",
        ),
        (
            "old_graph_churn",
            "old = {} for i = 1, 3000 do local t = {} for j = 1, 100 do t[j] = j end old[i] = t end \
             for r = 1, 2000 do local g = {} for j = 1, 30 do g[j] = {j} end end",
        ),
        (
            "old_mutation",
            "old = {} for i = 1, 3000 do local t = {} for j = 1, 100 do t[j] = j end old[i] = t end \
             for r = 1, 2000 do local g = {} for j = 1, 30 do g[j] = {j} end old[r % 3000 + 1][1] = {r} end",
        ),
        (
            "short_strings",
            "local n = 0 for i = 1, 60000 do local s = tostring(i) .. 'x' n = n + #s end",
        ),
        (
            "short_closures",
            "local n = 0 for i = 1, 60000 do local f = function() return i end n = n + f() end",
        ),
        (
            "short_userdata",
            "local n = 0 for i = 1, 30000 do local u = newud(8, 1) n = n + 1 end",
        ),
        (
            "old_finalizers",
            "local mt = {__gc = function() end} keep = {} for i = 1, 9000 do keep[i] = setmetatable({}, mt) end \
             for r = 1, 2000 do local g = {} for j = 1, 20 do g[j] = {j} end end",
        ),
        (
            "old_threads",
            "cos = {} for i = 1, 1000 do local co = coroutine.create(function() coroutine.yield() end) coroutine.resume(co) cos[i] = co end \
             for r = 1, 2000 do local g = {} for j = 1, 30 do g[j] = {j} end end",
        ),
        (
            "touched_threads",
            "cos = {} for i = 1, 1000 do local co = coroutine.create(function() while true do local v = coroutine.yield() end end) coroutine.resume(co) cos[i] = co end \
             for r = 1, 2000 do local g = {} for j = 1, 30 do g[j] = {j} end if r % 10 == 0 then for k = 1, 10 do coroutine.resume(cos[k], {r}) end end end",
        ),
    ];
    for (mode, prefix) in [
        (crate::GcMode::Incremental, "gc_inc_"),
        (crate::GcMode::Generational, "gc_gen_"),
    ] {
        for (workload, source) in workloads {
            let name: &'static str = Box::leak(format!("{prefix}{workload}").into_boxed_str());
            if !wanted(name) {
                continue;
            }
            let chunk = crate::compile(source.as_bytes()).expect(name);
            let run_boot = || {
                let config = Config {
                    gc_mode: mode,
                    ..Config::default()
                };
                let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false)
                    .expect("boot");
                bind_natives(&mut runtime);
                runtime.install_coroutine().expect("coroutine");
                for (name, _) in crate::host::USERDATA_NATIVES {
                    runtime
                        .set_global_native(name, name)
                        .expect("userdata native");
                }
                runtime
            };
            let run = || {
                let mut runtime = run_boot();
                let start = Instant::now();
                let outcome = runtime
                    .run_until_terminal(u64::MAX, &mut Journal::new())
                    .expect("run");
                let elapsed = start.elapsed().as_nanos() as u64;
                assert_eq!(outcome, StepOutcome::Completed, "{name}");
                (runtime, elapsed)
            };
            // The same run with a quantum of 1,000 fuel: the largest slice
            // the executor shows between two returns to the host, the
            // peak logical heap, and the most objects remembered.
            let (quantum_max, peak, remembered) = {
                let mut runtime = run_boot();
                let mut journal = Journal::new();
                let (_, seen) = observe_collector(&mut runtime, 1000, &mut journal);
                let max = runtime
                    .gc_slices
                    .iter()
                    .map(|slice| slice.1)
                    .max()
                    .unwrap_or(0);
                (max, seen.peak, seen.remembered)
            };
            let mut best: Option<(Runtime, u64)> = None;
            for _ in 0..5 {
                let (runtime, elapsed) = run();
                if best.as_ref().is_none_or(|(_, time)| elapsed < *time) {
                    best = Some((runtime, elapsed));
                }
            }
            let (runtime, total) = best.expect("runs");
            let slices = &runtime.gc_slices;
            let mut ns: Vec<u64> = slices.iter().map(|slice| slice.1).collect();
            ns.sort_unstable();
            let mut units: Vec<u64> = slices.iter().map(|slice| slice.0).collect();
            units.sort_unstable();
            type Slice = (u64, u64, bool, bool, bool);
            let largest = |pick: &dyn Fn(&Slice) -> bool| {
                slices
                    .iter()
                    .filter(|slice| pick(slice))
                    .map(|slice| slice.1)
                    .max()
                    .unwrap_or(0)
            };
            let minor = largest(&|slice| slice.4);
            let atomic = largest(&|slice| slice.3 && !slice.4);
            let young_units: u64 = slices
                .iter()
                .filter(|slice| slice.4)
                .map(|slice| slice.0)
                .sum();
            let gc_ns: u64 = ns.iter().sum();
            if ns.is_empty() {
                ns.push(0);
                units.push(0);
            }
            let gc = &runtime.heap().gc;
            samples.push(Sample {
                name,
                ns_per_op: u128::from(percentile(&ns, 50)),
                detail: format!(
                    "{} slices: p50 {} p95 {} p99 {} max {} ns; units p50 {} max {}; max young slice {} ns, max major atomic slice {} ns; \
                     collector {} of {} ns ({} full cycles, {} young, {} units); largest slice with a 1,000-fuel quantum {} ns; \
                     peak logical heap {} bytes; most remembered {}; units per young collection {}",
                    slices.len(),
                    percentile(&ns, 50),
                    percentile(&ns, 95),
                    percentile(&ns, 99),
                    ns[ns.len() - 1],
                    percentile(&units, 50),
                    units[units.len() - 1],
                    minor,
                    atomic,
                    gc_ns,
                    total,
                    gc.collections,
                    gc.minors,
                    gc.work,
                    quantum_max,
                    peak,
                    remembered,
                    young_units / gc.minors.max(1),
                ),
            });
        }
    }
    // The Phase 3.27 collector over the `gc_full_*` heaps.
    const N: usize = 4_000;
    let live = format!("keep = {{}} for i = 1, {N} do keep[i] = {{}} end");
    let shapes: [(&'static str, String); 3] = [
        ("gc_ref_strong", live.clone()),
        (
            "gc_ref_weakv",
            format!(
                "{live} w = setmetatable({{}}, {{__mode = 'v'}}) for i = 1, {N} do w[i] = keep[i] end"
            ),
        ),
        (
            "gc_ref_ephemeron",
            format!(
                "{live} e = setmetatable({{}}, {{__mode = 'k'}}) for i = 1, {N} do e[keep[i]] = i end"
            ),
        ),
    ];
    for (name, source) in shapes {
        if !wanted(name) {
            continue;
        }
        let chunk = crate::compile(source.as_bytes()).expect(name);
        let config = Config {
            auto_gc: false,
            ..Config::default()
        };
        let mut runtime =
            Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).expect("boot");
        bind_natives(&mut runtime);
        let outcome = runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .expect("run");
        assert_eq!(outcome, StepOutcome::Completed, "{name}");
        runtime.collect();
        let mut runs = Vec::with_capacity(20);
        for _ in 0..20 {
            let start = Instant::now();
            crate::gc_reference::collect(runtime.heap_mut(), &[]);
            runs.push(start.elapsed().as_nanos() as u64);
        }
        runs.sort_unstable();
        samples.push(Sample {
            name,
            ns_per_op: u128::from(runs[10]) / N as u128,
            detail: format!(
                "Phase 3.27 full collection: median {} ns, p99 {} ns",
                runs[10],
                percentile(&runs, 99)
            ),
        });
    }
    // Writes, with no cycle running and with one marking (started by a
    // tiny step, then stopped).
    const W: i64 = 200_000;
    let marking = "collectgarbage('incremental', 200, 4, 6) collectgarbage('step', 0) collectgarbage('stop') ";
    let writes: [(&'static str, &'static str, String, i64); 6] = [
        (
            "bw_table_write",
            "bw_table_write_marking",
            format!("local t = {{0}} for i = 1, {W} do t[1] = i end return t[1]"),
            W,
        ),
        (
            "bw_table_insert",
            "bw_table_insert_marking",
            format!("local t = {{}} for i = 1, {W} do t[i] = i end return #t"),
            W,
        ),
        (
            "bw_global_write",
            "bw_global_write_marking",
            format!("for i = 1, {W} do g = i end return g"),
            W,
        ),
        (
            "bw_upvalue_write",
            "bw_upvalue_write_marking",
            format!(
                "local u = 0 local function run() for i = 1, {W} do u = i end end run() return u"
            ),
            W,
        ),
        (
            "bw_uservalue_set",
            "bw_uservalue_set_marking",
            format!(
                "local u = newud(0, 1) for i = 1, {W} do debug.setuservalue(u, i, 1) end return (debug.getuservalue(u, 1))"
            ),
            W,
        ),
        (
            "bw_register_loop",
            "bw_register_loop_marking",
            format!("local a, b = 0, 1 for i = 1, {W} do a, b = b, a + i end return {W}"),
            W,
        ),
    ];
    let mut jobs: Vec<SourceJob> = Vec::new();
    let incremental = Config {
        gc_mode: crate::GcMode::Incremental,
        ..Config::default()
    };
    for (dormant, armed, source, result) in writes {
        jobs.push((
            dormant,
            source.clone(),
            vec![result],
            W,
            incremental.clone(),
        ));
        jobs.push((
            armed,
            format!("{marking}{source}"),
            vec![result],
            W,
            incremental.clone(),
        ));
        // Generational: the object written to young, and made old by a
        // full collection first.
        let young: &'static str = Box::leak(format!("{dormant}_gen_young").into_boxed_str());
        let old: &'static str = Box::leak(format!("{dormant}_gen_old").into_boxed_str());
        let aged = if source.contains(" for i = 1,") {
            source.replacen(" for i = 1,", " collectgarbage() for i = 1,", 1)
        } else {
            format!("g = 0 collectgarbage() {source}")
        };
        jobs.push((young, source.clone(), vec![result], W, Config::default()));
        jobs.push((old, aged, vec![result], W, Config::default()));
    }
    samples.extend(time_source_jobs(jobs));
    samples
}

/// Public ObjectId reacquisition in a rooted heap at four scales. The runtime
/// and roots are built outside the timer; every timed lookup acquires and drops
/// an owned Value, including the public ownership checks.
fn id_lookup_rows() -> Vec<Sample> {
    let mut samples = Vec::new();
    for (name, n) in [
        ("id_lookup_1k", 1_000usize),
        ("id_lookup_10k", 10_000),
        ("id_lookup_100k", 100_000),
        ("id_lookup_500k", 500_000),
    ] {
        if !wanted(name) {
            continue;
        }
        let mut runtime = Runtime::builder()
            .config(Config {
                auto_gc: false,
                max_objects: n as u32 + 100,
                ..Config::default()
            })
            .build()
            .unwrap();
        let mut roots = Vec::with_capacity(n);
        for index in 0..n {
            roots.push(runtime.create_string(index.to_le_bytes()).unwrap());
        }
        let ids: Vec<_> = roots
            .iter()
            .map(|s| crate::Value::String(s.clone()).id().unwrap())
            .collect();
        black_box(runtime.object(ids[0]).unwrap());
        let lookups = 200_000usize;
        let start = Instant::now();
        for k in 0..lookups {
            let index = k.wrapping_mul(2_654_435_761) % n;
            black_box(runtime.object(black_box(ids[index])).unwrap());
        }
        samples.push(Sample {
            name, ns_per_op: start.elapsed().as_nanos() / lookups as u128,
            detail: format!("{n} rooted strings; public Runtime::object acquire/drop; {lookups} scattered lookups"),
        });
        black_box(roots);
    }
    samples
}

/// Lua-to-host immediate and typed adapters; construction and compilation are
/// outside the timer. Existing row names are kept for longitudinal comparisons.
fn embedding_native_rows() -> Vec<Sample> {
    let mut samples = Vec::new();
    for name in ["embed_native_raw", "embed_native_typed"] {
        if !wanted(name) {
            continue;
        }
        let mut registry = HostRegistry::new();
        if name == "embed_native_raw" {
            registry.function("add", crate::NativePolicy::VmLocal, |cx| {
                let a = cx.arg(0).as_integer().unwrap();
                let b = cx.arg(1).as_integer().unwrap();
                cx.return_values(a + b)
            });
        } else {
            registry.typed(
                "add",
                crate::NativePolicy::VmLocal,
                |_, (a, b): (i64, i64)| Ok(a + b),
            );
        }
        let mut runtime = embed_runtime(registry);
        let add = runtime.make_closure("add", ()).unwrap();
        runtime.globals().raw_set(&mut runtime, "add", add).unwrap();
        let function = embed_load(
            &mut runtime,
            "function entry() local s for i=1,1000 do s=add(i,1) end return s end",
        );
        let mut journal = Journal::new();
        samples.push(embed_time(
            name,
            1_000,
            || {
                let crate::CallOutcome::Done(result) = runtime
                    .call::<i64>(&function, (), &mut journal, u64::MAX)
                    .unwrap()
                else {
                    panic!("immediate call suspended");
                };
                assert_eq!(result, 1_001);
            },
            "1000 immediate crossings/run; warmed public call API",
        ));
    }
    samples
}

/// A portable scalar fixture independent of the VM's internal proof types.
struct EmbedPortable(u64);
impl crate::HostUserdata for EmbedPortable {
    const SYMBOL: &'static str = "bench.Portable.v1";
    fn logical_size(&self) -> u64 {
        8
    }
}
impl crate::PortableUserdata for EmbedPortable {
    fn encode(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(u64::from_le_bytes(bytes.try_into().ok()?)))
    }
}

/// A rebind fixture whose host-owned scalar is never encoded as a pointer.
struct EmbedRebind(u64);
impl crate::HostUserdata for EmbedRebind {
    const SYMBOL: &'static str = "bench.Rebind.v1";
    fn logical_size(&self) -> u64 {
        8
    }
}
impl crate::RebindUserdata for EmbedRebind {
    fn key(&self) -> Vec<u8> {
        b"bench-resource".to_vec()
    }
    fn rebind(key: &[u8], env: &crate::HostEnv) -> Result<Self, crate::RebindError> {
        if key != b"bench-resource" {
            return Err(crate::RebindError("unknown key"));
        }
        env.get::<u64>()
            .copied()
            .map(Self)
            .ok_or(crate::RebindError("missing resource"))
    }
}

fn embed_runtime(registry: HostRegistry) -> Runtime {
    Runtime::builder()
        .registry(registry)
        .libraries(crate::Libraries::STANDARD)
        .build()
        .unwrap()
}

fn embed_load(runtime: &mut Runtime, source: &str) -> crate::Function {
    runtime
        .load_main(&crate::compile(source.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(
        runtime.run(u64::MAX, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    runtime.globals().raw_get(runtime, "entry").unwrap()
}

/// One warmup and twenty independent timed runs; setup remains outside timing.
fn embed_time(name: &'static str, operations: u128, mut run: impl FnMut(), detail: &str) -> Sample {
    run();
    let mut times = Vec::with_capacity(20);
    for _ in 0..20 {
        let start = Instant::now();
        run();
        times.push(start.elapsed().as_nanos() / operations);
    }
    times.sort_unstable();
    Sample {
        name,
        ns_per_op: times[10],
        detail: format!("median of 20; p95 {} ns/op; {detail}", times[18]),
    }
}

struct EmbedModules;
impl crate::ModuleResolver for EmbedModules {
    fn resolve(&self, name: &[u8]) -> crate::Resolved {
        if name == b"bench.mod" {
            crate::Resolved::Source(b"return {value=42}".to_vec())
        } else {
            crate::Resolved::NotFound(b"unknown module".to_vec())
        }
    }
}

/// The embedding corpus uses exclusively exported values, callbacks, borrows,
/// tables, calls, waits, modules and restore. No arena or test-hook paths.
fn embedding_rows() -> Vec<Sample> {
    use crate::{
        AnyUserData, CallOutcome, Completion, Host, MultiValue, NativePolicy, NativeReturn,
        ResumeOutcome, Value as Owned, WaitRequest,
    };
    const N: u128 = 1_000;
    let mut samples = Vec::new();
    for name in [
        "embed_host_lua_call",
        "embed_userdata_borrow",
        "embed_host_method",
        "embed_continuation",
        "embed_string_borrow",
        "embed_string_copy",
        "embed_table_raw_get",
        "embed_table_raw_set",
        "embed_table_get",
        "embed_table_set",
        "embed_root_acquire_drop",
        "embed_wait_complete",
        "embed_module_resolve",
        "embed_restore_portable",
        "embed_restore_rebind",
    ] {
        if !wanted(name) {
            continue;
        }
        let mut registry = HostRegistry::new();
        registry.register_portable_userdata::<EmbedPortable>();
        registry.register_rebind_userdata::<EmbedRebind>();
        registry.function("borrow", NativePolicy::VmLocal, |cx| {
            let object: AnyUserData = cx.argument(0)?;
            let value = cx.borrow_userdata::<EmbedPortable>(&object)?.0;
            cx.return_values(value as i64)
        });
        registry.function("make", NativePolicy::VmLocal, |cx| {
            let object = cx.create_userdata(EmbedPortable(42), 0)?;
            let methods = cx.create_table()?;
            let get = cx.make_closure("borrow", ())?;
            cx.raw_set(&methods, "get", get)?;
            let meta = cx.create_table()?;
            cx.raw_set(&meta, "__index", methods)?;
            cx.set_metatable(object.clone(), Some(&meta))?;
            cx.return_values(object)
        });
        registry.function("bridge", NativePolicy::VmLocal, |cx| {
            if let Some(resume) = cx.resumed() {
                assert_eq!(resume.tag, 7);
                return Ok(match resume.outcome {
                    ResumeOutcome::Returned(values) => NativeReturn::Return(values),
                    ResumeOutcome::Errored(error) => NativeReturn::Error(error.value),
                });
            }
            Ok(NativeReturn::CallLua {
                function: cx.arg(0).to_owned_value()?,
                args: MultiValue(vec![cx.arg(1).to_owned_value()?]),
                tag: 7,
                keep: MultiValue::default(),
            })
        });
        registry.function("await", NativePolicy::VmLocal, |_| {
            Ok(NativeReturn::Wait(WaitRequest {
                operation: "bench.wait".into(),
                payload: MultiValue::default(),
            }))
        });
        // A restore registry needs the same standard symbols as construction.
        crate::register_standard(&mut registry);
        let host_registry = registry.clone();
        let mut runtime = embed_runtime(registry);
        for symbol in ["borrow", "make", "bridge", "await"] {
            let function = runtime.make_closure(symbol, ()).unwrap();
            runtime
                .globals()
                .raw_set(&mut runtime, symbol, function)
                .unwrap();
        }
        let mut journal = Journal::new();
        let sample = match name {
            "embed_host_lua_call" => {
                let function = embed_load(&mut runtime, "function entry(x) return x+1 end");
                embed_time(
                    name,
                    N,
                    || {
                        for i in 0..N as i64 {
                            assert!(
                                matches!(runtime.call::<i64>(&function, (i,), &mut journal, u64::MAX).unwrap(), CallOutcome::Done(n) if n == i+1)
                            );
                        }
                    },
                    "start/run/finish on main thread; one scalar argument/result",
                )
            }
            "embed_userdata_borrow" => {
                let object = runtime.create_host_userdata(EmbedPortable(42), 0).unwrap();
                embed_time(
                    name,
                    N,
                    || {
                        for _ in 0..N {
                            black_box(object.borrow::<EmbedPortable>(&runtime).unwrap().0);
                        }
                    },
                    "public typed immutable host payload borrow",
                )
            }
            "embed_host_method" | "embed_continuation" => {
                let source = if name == "embed_host_method" {
                    "local u=make(); function entry() local s=0 for i=1,1000 do s=s+u:get() end return s end"
                } else {
                    "local f=function(x) return x+1 end; function entry() local s=0 for i=1,1000 do s=s+bridge(f,i) end return s end"
                };
                let function = embed_load(&mut runtime, source);
                let expected = if name == "embed_host_method" {
                    42_000
                } else {
                    501_500
                };
                embed_time(
                    name,
                    N,
                    || {
                        assert!(
                            matches!(runtime.call::<i64>(&function, (), &mut journal, u64::MAX).unwrap(), CallOutcome::Done(n) if n == expected)
                        );
                    },
                    "1000 Lua method/continuation invocations per main-thread run",
                )
            }
            "embed_string_borrow" | "embed_string_copy" => {
                let string = runtime.create_string(vec![b'x'; 256]).unwrap();
                embed_time(
                    name,
                    N,
                    || {
                        for _ in 0..N {
                            if name == "embed_string_borrow" {
                                black_box(string.as_bytes(&runtime).unwrap());
                            } else {
                                black_box(string.as_bytes(&runtime).unwrap().to_vec());
                            }
                        }
                    },
                    "256 raw bytes; borrowing versus owned Vec copy",
                )
            }
            "embed_table_raw_get"
            | "embed_table_raw_set"
            | "embed_table_get"
            | "embed_table_set" => {
                let table = runtime.create_table().unwrap();
                table.raw_set(&mut runtime, 1i64, 42i64).unwrap();
                embed_time(
                    name,
                    N,
                    || {
                        for _ in 0..N {
                            match name {
                                "embed_table_raw_get" => {
                                    black_box(table.raw_get::<_, i64>(&mut runtime, 1i64).unwrap());
                                }
                                "embed_table_raw_set" => {
                                    table.raw_set(&mut runtime, 1i64, 42i64).unwrap()
                                }
                                "embed_table_get" => assert!(matches!(
                                    table
                                        .get::<_, i64>(&mut runtime, 1i64, &mut journal, u64::MAX)
                                        .unwrap(),
                                    CallOutcome::Done(42)
                                )),
                                _ => assert!(matches!(
                                    table
                                        .set(&mut runtime, 1i64, 42i64, &mut journal, u64::MAX)
                                        .unwrap(),
                                    CallOutcome::Done(())
                                )),
                            }
                        }
                    },
                    "same existing integer key; semantic operations use call machinery",
                )
            }
            "embed_root_acquire_drop" => {
                let string = runtime.create_string(b"root").unwrap();
                let id = Owned::String(string.clone()).id().unwrap();
                embed_time(
                    name,
                    N,
                    || {
                        for _ in 0..N {
                            black_box(runtime.object(id).unwrap());
                        }
                    },
                    "acquire owned rooted Value by ObjectId and drop each iteration",
                )
            }
            "embed_wait_complete" => {
                let function = embed_load(&mut runtime, "function entry() return await() end");
                embed_time(
                    name,
                    N,
                    || {
                        for _ in 0..N {
                            let CallOutcome::Waiting(key) = runtime
                                .call::<i64>(&function, (), &mut journal, u64::MAX)
                                .unwrap()
                            else {
                                panic!("no wait");
                            };
                            assert_eq!(runtime.wait(key).unwrap().operation, "bench.wait");
                            runtime
                                .complete(key, Completion::Return(vec![Owned::Integer(42)]))
                                .unwrap();
                            assert_eq!(
                                runtime.run(u64::MAX, &mut journal).unwrap(),
                                StepOutcome::Completed
                            );
                            assert_eq!(runtime.finish_call::<i64>().unwrap(), 42);
                        }
                    },
                    "setup, inspect, complete, resume, finish; unique wait each iteration",
                )
            }
            "embed_module_resolve" => {
                // Rewind outside each timer so every call really resolves, compiles
                // and executes the module rather than hitting package.loaded.
                let mut runtime = Runtime::builder()
                    .registry(host_registry.clone())
                    .libraries(crate::Libraries::STANDARD)
                    .module_resolver(crate::ResolverPolicy::Pure, EmbedModules)
                    .build()
                    .unwrap();
                let function = embed_load(
                    &mut runtime,
                    "function entry() return require('bench.mod').value end",
                );
                let id = function.id().unwrap();
                let bytes = runtime.snapshot().unwrap();
                let host = Host::new(host_registry)
                    .module_resolver(crate::ResolverPolicy::Pure, EmbedModules);
                let mut times = Vec::with_capacity(20);
                for round in 0..21 {
                    let mut fresh = Runtime::restore(&bytes, &host).unwrap();
                    let Some(Owned::Function(function)) = fresh.object(id) else {
                        panic!("missing entry");
                    };
                    let start = Instant::now();
                    assert!(matches!(
                        fresh
                            .call::<i64>(&function, (), &mut Journal::new(), u64::MAX)
                            .unwrap(),
                        CallOutcome::Done(42)
                    ));
                    if round > 0 {
                        times.push(start.elapsed().as_nanos());
                    }
                }
                times.sort_unstable();
                Sample {
                    name,
                    ns_per_op: times[10],
                    detail: format!(
                        "median of 20; p95 {} ns/op; uncached require, host resolve + compile + execute; restore excluded",
                        times[18]
                    ),
                }
            }
            "embed_restore_portable" | "embed_restore_rebind" => {
                let object = if name == "embed_restore_portable" {
                    runtime.create_host_userdata(EmbedPortable(42), 0).unwrap()
                } else {
                    runtime.create_host_userdata(EmbedRebind(42), 0).unwrap()
                };
                runtime
                    .globals()
                    .raw_set(&mut runtime, "object", object)
                    .unwrap();
                let bytes = runtime.snapshot().unwrap();
                let mut env = crate::HostEnv::new();
                env.insert(42u64);
                let host = Host::new(host_registry).host_env(env);
                embed_time(
                    name,
                    100,
                    || {
                        for _ in 0..100 {
                            let mut restored = Runtime::restore(&bytes, &host).unwrap();
                            let object: AnyUserData =
                                restored.globals().raw_get(&mut restored, "object").unwrap();
                            let value = if name == "embed_restore_portable" {
                                object.borrow::<EmbedPortable>(&restored).unwrap().0
                            } else {
                                object.borrow::<EmbedRebind>(&restored).unwrap().0
                            };
                            assert_eq!(value, 42);
                            black_box(restored);
                        }
                    },
                    &format!(
                        "{} snapshot bytes; decode + bind + lookup + drop; one host userdata",
                        bytes.len()
                    ),
                )
            }
            _ => unreachable!(),
        };
        samples.push(sample);
    }
    samples
}
