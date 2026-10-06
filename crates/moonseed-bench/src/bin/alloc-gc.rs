//! Measurement only: allocation requests, fuel, GC and byte-for-byte snapshot witnesses.
//! `alloc-gc FILE OUTPUT_PREFIX [--no-gc]`, or `alloc-gc --fingerprint`.
#![forbid(unsafe_code)]
#![allow(deprecated)] // Match the corpus runner's legacy load/standard/debug setup.

use std::cell::RefCell;
use std::rc::Rc;

use moonseed::{Config, HostRegistry, Journal, Runtime, StepOutcome};

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--fingerprint"] {
        println!("{}", moonseed::gc_schedule_fingerprint().unwrap());
        return;
    }
    assert!(args.len() == 2 || (args.len() == 3 && args[2] == "--no-gc"));
    let no_gc = args.len() == 3;
    let chunk = moonseed::compile(&std::fs::read(&args[0]).unwrap()).unwrap();
    let mut registry = HostRegistry::new();
    moonseed::register_standard(&mut registry);
    moonseed::register_debug(&mut registry);
    let config = Config {
        auto_gc: !no_gc,
        // Attribution runs must not invoke emergency collection at the quota.
        max_logical_heap: if no_gc {
            1 << 30
        } else {
            Config::default().max_logical_heap
        },
        ..Config::default()
    };
    let mut runtime = Runtime::load_chunk(config, registry, &chunk).unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    let boot_collections = runtime.memory().collections;
    let output = Rc::new(RefCell::new(Vec::with_capacity(8192)));
    let sink = output.clone();
    runtime.set_output(Box::new(move |bytes| {
        sink.borrow_mut().extend_from_slice(bytes)
    }));
    let mut journal = Journal::new();
    let mut outcome = StepOutcome::Completed;
    let early = allocation_counter::measure(|| {
        outcome = runtime.run(1000, &mut journal).unwrap();
    });
    // Snapshot construction is excluded from the allocation interval.
    std::fs::write(
        format!("{}.early.snapshot", args[1]),
        runtime.snapshot().unwrap(),
    )
    .unwrap();
    let rest = allocation_counter::measure(|| {
        if matches!(outcome, StepOutcome::Paused(_)) {
            outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        }
    });
    assert_eq!(outcome, StepOutcome::Completed);
    let memory = runtime.memory();
    if no_gc {
        assert_eq!(
            memory.collections, boot_collections,
            "GC-disabled attribution collected"
        );
    }
    std::fs::write(
        format!("{}.terminal.snapshot", args[1]),
        runtime.snapshot().unwrap(),
    )
    .unwrap();
    std::fs::write(format!("{}.stdout", args[1]), &*output.borrow()).unwrap();
    std::fs::write(
        format!("{}.txt", args[1]),
        format!(
            "allocation_requests={}\nrequested_bytes={}\nfuel={}\nobjects={}\nlogical_bytes={}\ndebt={}\nthreshold={}\ncollections={}\nboot_collections={}\nyoung_collections={}\nauto_gc={}\n",
            early.count_total + rest.count_total,
            early.bytes_total + rest.bytes_total,
            runtime.fuel_consumed(),
            memory.objects,
            memory.logical_bytes,
            memory.debt,
            memory.threshold,
            memory.collections,
            boot_collections,
            memory.young_collections,
            memory.auto_gc,
        ),
    ).unwrap();
}
