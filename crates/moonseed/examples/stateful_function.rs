//! stateful function through the public embedding API.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut registry = counter_registry();
    registry.function("example.state", NativePolicy::VmLocal, |cx| {
        let captures = cx.captures();
        let Value::UserData(object) = captures[0].to_owned_value()? else {
            return Err(ApiError::WrongType.into());
        };
        let Value::Table(table) = captures[1].to_owned_value()? else {
            return Err(ApiError::WrongType.into());
        };
        let value = {
            let mut counter = cx.borrow_userdata_mut::<Counter>(&object)?;
            counter.0 += 1;
            counter.0
        };
        let label: String = cx.raw_get(&table, "label")?;
        cx.return_values((value, label))
    });
    let mut rt = Runtime::builder().registry(registry).build()?;
    let state = rt.create_host_userdata(Counter(40), 0)?;
    let table = rt.create_table()?;
    table.raw_set(&mut rt, "label", "tick")?;
    let function = rt.make_closure("example.state", (state, table))?;
    for n in [41, 42] {
        let result: (i64, String) = done(rt.call(&function, (), &mut Journal::new(), 100)?)?;
        assert_eq!(result, (n, "tick".into()));
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
