//! async wait through the public embedding API.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut registry = HostRegistry::new();
    registry.function("example.read", NativePolicy::VmLocal, |cx| {
        Ok(NativeReturn::Wait(WaitRequest {
            operation: "read.asset".into(),
            payload: MultiValue(vec![cx.arg(0).to_owned_value()?]),
        }))
    });
    let mut rt = Runtime::builder().registry(registry).build()?;
    let read = rt.make_closure("example.read", ())?;
    let mut journal = Journal::new();
    let CallOutcome::Waiting(key) = rt.call::<i64>(&read, "answer", &mut journal, 100)? else {
        return Err("expected a host wait".into());
    };
    let request = rt.wait(key).ok_or("missing wait")?;
    assert_eq!(request.operation, "read.asset");
    let Value::String(name) = &request.payload[0] else {
        return Err("expected a string".into());
    };
    assert_eq!(name.to_str(&rt)?, "answer");
    // Submit work here; complete only after the host has its result.
    rt.complete(key, Completion::Return(vec![Value::Integer(42)]))?;
    assert_eq!(rt.run(100, &mut journal)?, StepOutcome::Completed);
    assert_eq!(rt.finish_call::<i64>()?, 42);
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
