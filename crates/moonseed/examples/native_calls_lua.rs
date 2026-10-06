//! Native continuations and table reads that honor Lua __index.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut registry = HostRegistry::new();
    registry.function("example.bridge", NativePolicy::VmLocal, |cx| {
        if let Some(resume) = cx.resumed() {
            assert_eq!(resume.tag, 7);
            assert!(matches!(resume.kept[0], Value::Integer(1)));
            match resume.outcome {
                ResumeOutcome::Returned(values) => Ok(NativeReturn::Return(values)),
                ResumeOutcome::Errored(error) => Ok(NativeReturn::Error(error.value)),
            }
        } else {
            Ok(NativeReturn::CallLua {
                function: cx.arg(0).to_owned_value()?,
                args: MultiValue(vec![Value::Integer(21)]),
                tag: 7,
                keep: MultiValue(vec![Value::Integer(1)]),
            })
        }
    });
    registry.function("example.index", NativePolicy::VmLocal, |cx| {
        if let Some(resume) = cx.resumed() {
            return match resume.outcome {
                ResumeOutcome::Returned(values) => Ok(NativeReturn::Return(values)),
                ResumeOutcome::Errored(error) => Ok(NativeReturn::Error(error.value)),
            };
        }
        let accessor = cx.captures()[0].to_owned_value()?;
        let table: Table = cx.argument(0)?;
        let key = cx.arg(1).to_owned_value()?;
        // A synchronous Rust read sees no __index-generated field.
        assert!(matches!(cx.raw_get::<_, Value>(&table, &key)?, Value::Nil));
        cx.call_lua(accessor, (table, key), 0, ())
    });
    let mut rt = Runtime::builder().registry(registry).build()?;
    let bridge = rt.make_closure("example.bridge", ())?;
    let lua: Function = eval(&mut rt, b"return function(n) return n * 2 end")?;
    assert_eq!(
        done::<i64>(rt.call(&bridge, lua, &mut Journal::new(), 100)?)?,
        42
    );
    let accessor: Function = eval(&mut rt, b"return function(t, key) return t[key] end")?;
    let index = rt.make_closure("example.index", accessor)?;
    let handler: Function = eval(
        &mut rt,
        b"return function(_, key) if key == 'answer' then return 42 end end",
    )?;
    let meta = rt.create_table()?;
    meta.raw_set(&mut rt, "__index", handler)?;
    let table = rt.create_table()?;
    table.set_metatable(&mut rt, Some(&meta))?;
    assert_eq!(
        done::<i64>(rt.call(&index, (table, "answer"), &mut Journal::new(), 100)?)?,
        42
    );
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
