//! Warmed scalar resume/yield measurement. Setup and completion are outside
//! the host-allocation interval. Subtract two iteration counts to remove the
//! fixed loop entry/exit cost from whole-program Callgrind instruction totals.
#![forbid(unsafe_code)]
#![allow(deprecated)] // Same load/setup and result inspection as the corpus runner.

use moonseed::{
    Completion, Config, HostRegistry, HostValue, Journal, MultiValue, NativePolicy, NativeReturn,
    Runtime, StepOutcome, WaitRequest,
};

fn main() {
    let iterations: u32 = std::env::args()
        .nth(1)
        .expect("usage: coroutine-roundtrip ITERATIONS [1|3]")
        .parse()
        .unwrap();
    let scalars = std::env::args().nth(2).unwrap_or_else(|| "1".into());
    let (args, names) = match scalars.as_str() {
        "1" => ("7", "a"),
        "3" => ("7, true, 3.5", "a, b, c"),
        _ => panic!("scalar count must be 1 or 3"),
    };
    let source = format!(
        "local resume, yield = coroutine.resume, coroutine.yield
         local co = coroutine.create(function({names})
           while true do {names} = yield({names}) end
         end)
         for i = 1, 1000 do resume(co, {args}) end
         marker()
         local sum = 0
         for i = 1, {iterations} do
           local ok, {names} = resume(co, {args})
           sum = sum + a
         end
         return sum"
    );
    let chunk = moonseed::compile(source.as_bytes()).unwrap();
    let mut registry = HostRegistry::new();
    moonseed::register_standard(&mut registry);
    registry.function("co.marker", NativePolicy::VmLocal, |_| {
        Ok(NativeReturn::Wait(WaitRequest {
            operation: "co.marker".into(),
            payload: MultiValue::default(),
        }))
    });
    let mut runtime = Runtime::load_chunk(
        Config {
            fuel_limit: None,
            ..Config::default()
        },
        registry,
        &chunk,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    runtime.set_global_native("marker", "co.marker").unwrap();
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("warmup did not reach marker");
    };
    runtime
        .complete(key, Completion::Return(Vec::new()))
        .unwrap();
    #[cfg(feature = "counters")]
    runtime.reset_counters();
    let fuel = runtime.fuel_consumed();
    let run = || {
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Completed
        );
    };
    #[cfg(feature = "alloc-gc")]
    let allocations = allocation_counter::measure(run);
    #[cfg(not(feature = "alloc-gc"))]
    {
        let mut run = run;
        run();
    }
    assert_eq!(
        runtime.results().unwrap(),
        vec![HostValue::Integer(i64::from(iterations) * 7)]
    );
    println!(
        "iterations={iterations}\nscalars={scalars}\nfuel={}\nchecksum={}",
        runtime.fuel_consumed() - fuel,
        i64::from(iterations) * 7
    );
    #[cfg(feature = "alloc-gc")]
    println!(
        "allocation_requests={}\nrequested_bytes={}",
        allocations.count_total, allocations.bytes_total
    );
    #[cfg(feature = "counters")]
    println!("counters={}", runtime.counters().to_json());
}
