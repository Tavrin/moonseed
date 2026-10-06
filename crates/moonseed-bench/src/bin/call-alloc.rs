//! Count host allocations only between the two warmed benchmark markers.
#![forbid(unsafe_code)]
#![allow(deprecated)] // Match the corpus runner's load and standard setup.

use moonseed::{
    Completion, Config, HostRegistry, Journal, MultiValue, NativePolicy, NativeReturn, Runtime,
    StepOutcome, WaitRequest,
};

fn main() {
    let path = std::env::args().nth(1).expect("usage: call-alloc FILE");
    let source = std::fs::read(path).unwrap();
    let chunk = moonseed::compile(&source).unwrap();
    let mut registry = HostRegistry::new();
    moonseed::register_standard(&mut registry);
    moonseed::register_debug(&mut registry);
    registry.function("call.marker", NativePolicy::VmLocal, |_| {
        Ok(NativeReturn::Wait(WaitRequest {
            operation: "call.marker".into(),
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
    runtime.install_debug().unwrap();
    runtime
        .set_global_native("call_bench_marker", "call.marker")
        .unwrap();
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
    let memory = runtime.memory();
    let allocations = allocation_counter::measure(|| {
        assert!(matches!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Waiting(_)
        ));
    });
    println!(
        "{{\"allocation_requests\":{},\"requested_bytes\":{},\"fuel\":{},\"collections\":{},\"counters\":{}}}",
        allocations.count_total,
        allocations.bytes_total,
        runtime.fuel_consumed() - fuel,
        runtime.memory().collections - memory.collections,
        {
            #[cfg(feature = "counters")]
            {
                runtime.counters().to_json()
            }
            #[cfg(not(feature = "counters"))]
            {
                "null".to_owned()
            }
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooked_allocations(
        mask: &str,
        count: u32,
        iterations: u32,
        host: bool,
    ) -> allocation_counter::AllocationInfo {
        let source = format!(
            "local events=0 local function h() events=events+1 end debug.sethook(h,'{mask}',{count}) local function f() return 1 end for i=1,20 do f() end call_bench_marker() for i=1,{iterations} do f() end debug.sethook() call_bench_marker()"
        );
        let source = if host {
            source.replace(
                &format!("debug.sethook(h,'{mask}',{count})"),
                "hookinstall()",
            )
        } else {
            source
        };
        let chunk = moonseed::compile(source.as_bytes()).unwrap();
        let mut registry = HostRegistry::new();
        moonseed::register_standard(&mut registry);
        moonseed::register_debug(&mut registry);
        if host {
            registry.register_hook("alloc.hook", |_| Ok(moonseed::HookAction::Continue));
            let mut selection = moonseed::HookMask::NONE;
            for (byte, flag) in [
                (b'c', moonseed::HookMask::CALL),
                (b'r', moonseed::HookMask::RETURN),
                (b'l', moonseed::HookMask::LINE),
            ] {
                if mask.as_bytes().contains(&byte) {
                    selection |= flag;
                }
            }
            registry.typed("alloc.install", NativePolicy::VmLocal, move |cx, ()| {
                cx.set_hook(None, "alloc.hook", selection, count as i32)
            });
        }
        registry.function("call.marker", NativePolicy::VmLocal, |_| {
            Ok(NativeReturn::Wait(WaitRequest {
                operation: "call.marker".into(),
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
        runtime.install_debug().unwrap();
        runtime
            .set_global_native("call_bench_marker", "call.marker")
            .unwrap();
        if host {
            runtime
                .set_global_native("hookinstall", "alloc.install")
                .unwrap();
        }
        let mut journal = Journal::new();
        let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
        else {
            panic!("no warmup marker");
        };
        runtime
            .complete(key, Completion::Return(Vec::new()))
            .unwrap();
        allocation_counter::measure(|| {
            assert!(matches!(
                runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                StepOutcome::Waiting(_)
            ));
        })
    }

    #[test]
    fn hook_events_use_recycled_storage_after_warmup() {
        for host in [false, true] {
            for (mask, count) in [("cr", 0), ("l", 0), ("", 1), ("crl", 4)] {
                let short = hooked_allocations(mask, count, 100, host);
                let long = hooked_allocations(mask, count, 1000, host);
                assert_eq!(
                    short.count_total, long.count_total,
                    "mask={mask}, count={count}"
                );
                assert_eq!(short.bytes_total, long.bytes_total);
                // The only measured allocation is the terminal host-wait operation name.
                assert_eq!(long.count_total, 1);
                assert_eq!(long.bytes_total, 11);
            }
        }
    }
}
