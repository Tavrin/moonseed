//! tables and strings through the public embedding API.
use moonseed::*;
mod support;
use support::*;
fn main() -> ExampleResult {
    let mut rt = Runtime::builder().build()?;
    let table = rt.create_table()?;
    table.raw_set(&mut rt, "answer", 42)?;
    table.raw_set(&mut rt, "bytes", b"a\0\xff".as_slice())?;
    let exchange: Function = eval(&mut rt, b"return function(t) return t.answer, t.bytes end")?;
    let (answer, bytes): (i64, Vec<u8>) =
        done(rt.call(&exchange, &table, &mut Journal::new(), 100)?)?;
    assert_eq!(answer, 42);
    assert_eq!(bytes, b"a\0\xff");
    assert_eq!(table.raw_get::<_, Vec<u8>>(&mut rt, "bytes")?, bytes);
    let text = rt.create_string(&bytes)?;
    assert!(matches!(
        text.to_str(&rt),
        Err(Error::Api(ApiError::Conversion(_)))
    ));
    table.raw_set(&mut rt, "numeric_text", "42.5")?;
    assert!(matches!(
        table.raw_get::<_, f64>(&mut rt, "numeric_text"),
        Err(Error::Api(ApiError::Conversion(_)))
    ));
    let Coerce(number): Coerce<f64> = table.raw_get(&mut rt, "numeric_text")?;
    assert_eq!(number, 42.5);
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
