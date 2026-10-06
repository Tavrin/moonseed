//! Checkpoint portable host state and a suspended Lua coroutine.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut rt = Runtime::builder()
        .registry(counter_registry())
        .libraries(Libraries::MATH | Libraries::COROUTINE)
        .build()?;
    assert_eq!(
        eval::<i64>(
            &mut rt,
            br#"
        co = coroutine.create(function()
            local answer = coroutine.yield(20)
            return answer + 2
        end)
        local ok, value = coroutine.resume(co)
        return value
    "#
        )?,
        20
    );
    let state = rt.create_host_userdata(Counter(40), 0)?;
    let next = rt.make_closure("example.next", state)?;
    rt.globals().raw_set(&mut rt, "next", &next)?;
    assert_eq!(
        done::<i64>(rt.call(&next, (), &mut Journal::new(), 100)?)?,
        41
    );
    let snapshot = rt.snapshot()?;
    // Fresh host-only registry: Moonseed re-registers its allowed libraries.
    let host = Host::new(counter_registry())
        .libraries(Libraries::MATH | Libraries::COROUTINE)
        .effect_domain(rt.effect_domain());
    let mut restored = Runtime::restore(&snapshot, &host)?;
    // The old root belongs to rt. Acquire a new root in restored.
    let next: Function = restored.globals().raw_get(&mut restored, "next")?;
    assert_eq!(
        done::<i64>(restored.call(&next, (), &mut Journal::new(), 100)?)?,
        42
    );
    let (ok, answer, status): (bool, i64, String) = eval(
        &mut restored,
        br#"
        local ok, answer = coroutine.resume(co, math.floor(40.5))
        return ok, answer, coroutine.status(co)
    "#,
    )?;
    assert!(ok);
    assert_eq!(answer, 42);
    assert_eq!(status, "dead");
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
