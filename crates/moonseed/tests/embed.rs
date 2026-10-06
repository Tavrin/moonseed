#![allow(deprecated)] // Legacy API compatibility coverage.
//! A Rust function exposed to Lua through the public, unstable API only.

use moonseed::{
    Config, HostRegistry, HostValue, Journal, NativeCall, NativeOutcome, NativePolicy, Runtime,
    StepOutcome, VmError, compile,
};

/// `scale(x, k)`: `x * k` for integers.
fn scale(call: &mut NativeCall<'_>) -> NativeOutcome {
    match (call.integer(0), call.integer(1)) {
        (Some(x), Some(k)) => {
            call.push_integer(x.wrapping_mul(k));
            NativeOutcome::Ready
        }
        _ => NativeOutcome::Fault,
    }
}

#[test]
fn an_embedder_exposes_a_rust_function() {
    let mut hosts = HostRegistry::new();
    hosts.register_native("example.scale", NativePolicy::VmLocal, scale);
    let chunk = compile(b"local f = scale result = f(6, 7) same = f == scale").unwrap();
    let mut runtime = Runtime::load_chunk(Config::default(), hosts.clone(), &chunk).unwrap();
    runtime.set_global_native("scale", "example.scale").unwrap();
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(runtime.global_integer("result").unwrap(), 42);

    // The function survives a checkpoint by symbol and is rebound on restore.
    let bytes = runtime.snapshot().unwrap();
    let restored = Runtime::from_snapshot(&bytes, &hosts, runtime.effect_domain()).unwrap();
    assert_eq!(restored.global_integer("result").unwrap(), 42);
    assert!(Runtime::from_snapshot(&bytes, &HostRegistry::new(), runtime.effect_domain()).is_err());
}

#[test]
fn a_chunk_takes_arguments_as_its_varargs() {
    let chunk = compile(b"local a, b = ... return ...").unwrap();
    let args = [
        HostValue::Integer(10),
        HostValue::Nil,
        HostValue::Integer(30),
        HostValue::String(b"s".to_vec()),
    ];
    let mut runtime =
        Runtime::load_chunk_with_args(Config::default(), HostRegistry::new(), &chunk, &args)
            .unwrap();
    // Checkpointed before the first instruction, the arguments survive.
    let bytes = runtime.snapshot().unwrap();
    let mut restored =
        Runtime::from_snapshot(&bytes, &HostRegistry::new(), runtime.effect_domain()).unwrap();
    for runtime in [&mut runtime, &mut restored] {
        assert_eq!(runtime.results(), Err(VmError::NotRunnable));
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(runtime.results().unwrap(), args);
    }
    // Without arguments, `...` is empty.
    let mut runtime = Runtime::load_chunk(Config::default(), HostRegistry::new(), &chunk).unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(runtime.results().unwrap(), []);
    // More arguments than the stack bound holds.
    let many = vec![HostValue::Nil; 200_000];
    assert!(matches!(
        Runtime::load_chunk_with_args(Config::default(), HostRegistry::new(), &chunk, &many),
        Err(VmError::StackLimit)
    ));
}
