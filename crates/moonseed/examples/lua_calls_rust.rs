//! lua calls rust through the public embedding API.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut registry = HostRegistry::new();
    registry.typed(
        "example.scale",
        NativePolicy::VmLocal,
        |_cx, (x, scale): (i64, i64)| Ok(x.wrapping_mul(scale)),
    );
    let mut rt = Runtime::builder().registry(registry).build()?;
    let scale = rt.make_closure("example.scale", ())?;
    rt.globals().raw_set(&mut rt, "scale", scale)?;
    assert_eq!(eval::<i64>(&mut rt, b"return scale(6, 7)")?, 42);
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
