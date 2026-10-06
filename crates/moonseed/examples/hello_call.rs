//! Basic execution, host-to-Lua calls, and bounded host profiling.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let events = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let count = events.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("example.profiler", move |_| {
        count.set(count.get() + 1);
        Ok(HookAction::Continue)
    });
    let mut rt = Runtime::builder().registry(registry).build()?;
    rt.set_hook(
        None,
        "example.profiler",
        HookMask::CALL | HookMask::RETURN,
        0,
    )?;
    assert_eq!(eval::<i64>(&mut rt, b"return 6 * 7")?, 42);
    let add: Function = eval(&mut rt, b"return function(a, b) return a + b, 'ok' end")?;
    let result: (i64, String) = done(rt.call(&add, (20i64, 22i64), &mut Journal::new(), 100)?)?;
    assert_eq!(result, (42, "ok".into()));
    assert!(events.get() > 0);
    rt.clear_hook(None)?;
    // The counter is host state; persist it separately if needed on restore.
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
