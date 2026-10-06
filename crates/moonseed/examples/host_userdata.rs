//! host userdata through the public embedding API.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut registry = counter_registry();
    registry.typed(
        "example.increment",
        NativePolicy::VmLocal,
        |cx, (object, n): (AnyUserData, i64)| {
            let mut counter = cx.borrow_userdata_mut::<Counter>(&object)?;
            counter.0 += n;
            Ok(counter.0)
        },
    );
    registry.function("example.counter", NativePolicy::VmLocal, |cx| {
        let object = cx.create_userdata(Counter(0), 0)?;
        let methods = cx.create_table()?;
        let increment = cx.make_closure("example.increment", ())?;
        cx.raw_set(&methods, "increment", increment)?;
        let meta = cx.create_table()?;
        cx.raw_set(&meta, "__index", methods)?;
        cx.set_metatable(&object, Some(&meta))?;
        cx.return_values(object)
    });
    let mut rt = Runtime::builder().registry(registry).build()?;
    let constructor = rt.make_closure("example.counter", ())?;
    rt.globals().raw_set(&mut rt, "counter", constructor)?;
    assert_eq!(
        eval::<i64>(
            &mut rt,
            b"local c = counter() c:increment(2) return c:increment(40)"
        )?,
        42
    );
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
