#![allow(deprecated)] // Legacy API compatibility coverage.
//! Snapshot envelope, including collector continuations. Run with
//! `cargo run -p moonseed-bench --release --features measure --bin snapshot-scale`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use moonseed::{Config, GcMode, HostRegistry, Journal, Runtime, StepOutcome};

struct CountingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// Forward the exact layout/pointer contract to System. Count successful
// requested bytes, excluding allocator headers and internal realloc copies.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(ptr, layout);
        }
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(call: impl FnOnce() -> T) -> (T, f64, usize) {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let start = Instant::now();
    let result = call();
    let seconds = start.elapsed().as_secs_f64();
    let extra = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    (result, seconds, extra)
}

fn registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    moonseed::register_standard(&mut registry);
    moonseed::register_userdata_proof(&mut registry);
    registry.register_native("park", moonseed::NativePolicy::VmLocal, |_| {
        moonseed::NativeOutcome::Pending(moonseed::WaitKey(1))
    });
    registry
}

fn build(shape: &str, count: usize, mode: GcMode) -> Runtime {
    let (setup, make) = match shape {
        "strings" => ("local pad = string.rep('s', 2048)", "pad .. i"),
        "tables" => ("", "{i, i, i, i, i, i, i, i}"),
        "closures" => (
            "",
            "assert(load('local x = ... return function() return x end'))(i)",
        ),
        "threads" => (
            "local function deep(n) if n == 0 then return coroutine.yield() end local v = deep(n - 1) return v end \
             local function make() local co = coroutine.create(deep) assert(coroutine.resume(co, 2)) return co end",
            "make()",
        ),
        "userdata" => ("", "newud(2048, 1)"),
        "mixed" => (
            "local pad = string.rep('m', 1024) \
             local function sleeper() coroutine.yield() end \
             local function make(i) local k = i % 5 \
               if k == 0 then return pad .. i \
               elseif k == 1 then return {i, i, i, i, i, i, i, i} \
               elseif k == 2 then return function() return i end \
               elseif k == 3 then local co = coroutine.create(sleeper) assert(coroutine.resume(co)) return co \
               else return newud(1024, 1) end end",
            "make(i)",
        ),
        _ => panic!("unknown shape {shape}"),
    };
    let mode_name = if mode == GcMode::Generational {
        "generational"
    } else {
        "incremental"
    };
    let source = format!(
        "keep = {{}} {setup} for i = 1, {count} do keep[i] = {make} end \
         collectgarbage() collectgarbage('{mode_name}') park() \
         young = {{}} for i = 1, 1000 do young[i] = {{i}} end \
         collectgarbage('step', 0) park()"
    );
    let chunk = moonseed::compile(source.as_bytes()).expect("compile");
    let mut runtime = Runtime::load_chunk(
        Config {
            gc_mode: mode,
            ..Config::default()
        },
        registry(),
        &chunk,
    )
    .expect("boot");
    runtime.install_standard().expect("standard");
    runtime.set_global_native("park", "park").expect("park");
    runtime.set_global_native("newud", "newud").expect("newud");
    let outcome = runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .expect("build");
    assert!(
        matches!(outcome, StepOutcome::Waiting(_)),
        "{shape} {count}: {outcome:?} {:?}",
        runtime.lua_error()
    );
    runtime
}

fn mid_collection(runtime: &mut Runtime, mode: GcMode) {
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run(1, &mut journal).expect("waiting") else {
        panic!("not parked")
    };
    runtime.complete_wait(key, 0).expect("resume");
    runtime.set_auto_gc(false);
    for _ in 0..100_000 {
        let outcome = runtime.run(1, &mut journal).expect("step");
        let (phase, young) = runtime.measurement_collection();
        if (mode == GcMode::Generational && young && phase != "pause")
            || (mode == GcMode::Incremental && phase == "mark" && !young)
        {
            return;
        }
        assert!(
            matches!(outcome, StepOutcome::Paused(_)),
            "missed collection: {outcome:?}"
        );
    }
    panic!("did not reach requested collection phase");
}

fn main() {
    let started = Instant::now();
    let uptime = std::process::Command::new("uptime")
        .output()
        .expect("uptime");
    println!(
        "Load at start: `{}`\n",
        String::from_utf8_lossy(&uptime.stdout).trim()
    );
    println!(
        "Release; one encode/restore per row. Allocator requested bytes; peak extra subtracts live bytes immediately before each call. Restore baseline includes the original runtime and snapshot. Sizes calibrated from 256 objects; near quota targets 60 MiB.\n"
    );
    println!(
        "| Shape/state | Target MiB | Logical bytes | Objects | Snapshot bytes | Bytes/logical byte | Encode s | Restore s | Encode extra bytes | Restore extra bytes |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for shape in [
        "strings",
        "tables",
        "closures",
        "threads",
        "userdata",
        "mixed",
        "mixed-young",
        "mixed-mark",
    ] {
        let base_shape = if shape.starts_with("mixed") {
            "mixed"
        } else {
            shape
        };
        let mode = if shape == "mixed-mark" {
            GcMode::Incremental
        } else {
            GcMode::Generational
        };
        let fixed = build(base_shape, 0, mode).memory().logical_bytes;
        let pilot = build(base_shape, 256, mode).memory().logical_bytes;
        let per_object = (pilot - fixed) as f64 / 256.0;
        for mib in [1u64, 8, 32, 60] {
            let target = mib << 20;
            let count = ((target - fixed) as f64 / per_object) as usize;
            let mut runtime = build(base_shape, count, mode);
            if matches!(shape, "mixed-young" | "mixed-mark") {
                mid_collection(&mut runtime, mode);
            }
            let memory = runtime.memory();
            assert!(
                memory.logical_bytes.abs_diff(target) < (target / 8).max(128 << 10),
                "{shape}: badly sized heap {}",
                memory.logical_bytes
            );
            let (bytes, encode, encode_extra) = measured(|| runtime.snapshot().expect("snapshot"));
            let registry = registry();
            let (restored, restore, restore_extra) = measured(|| {
                Runtime::from_snapshot(&bytes, &registry, runtime.effect_domain()).expect("restore")
            });
            assert_eq!(restored.memory(), memory);
            assert_eq!(
                restored.measurement_collection(),
                runtime.measurement_collection()
            );
            println!(
                "| {shape} | {mib} | {} | {} | {} | {:.4} | {encode:.6} | {restore:.6} | {encode_extra} | {restore_extra} |",
                memory.logical_bytes,
                memory.objects,
                bytes.len(),
                bytes.len() as f64 / memory.logical_bytes as f64,
            );
        }
    }
    println!("\nTotal elapsed: {:.3} s", started.elapsed().as_secs_f64());
}
