//! Host module resolution and a read-only in-memory filesystem sandbox.
use moonseed::*;
mod support;
use support::*;
struct Modules;
impl ModuleResolver for Modules {
    fn resolve(&self, name: &[u8]) -> Resolved {
        match name {
            b"game.foo" => Resolved::Source(b"return {answer = 42}".to_vec()),
            _ => Resolved::NotFound(b"module not provided by host".to_vec()),
        }
    }
}
fn main() -> ExampleResult {
    let mut rt = Runtime::builder()
        .libraries(Libraries::BASE | Libraries::PACKAGE)
        .module_resolver(ResolverPolicy::Pure, Modules)
        .build()?;
    assert_eq!(
        eval::<i64>(&mut rt, b"return require('game.foo').answer")?,
        42
    );
    let capabilities = memory_filesystem(
        [(b"answer.lua".to_vec(), b"return 42".to_vec())],
        MemoryOptions {
            read_only: true,
            ..MemoryOptions::default()
        },
    )
    .map_err(|error| format!("VFS: {error:?}"))?;
    let mut sandbox = Runtime::builder()
        .libraries(Libraries::BASE | Libraries::PACKAGE | Libraries::IO | Libraries::OS)
        .capabilities(capabilities)
        .package_paths(b"?.lua", b"")
        .build()?;
    // No process/environment/stdio authority was supplied. Writes are denied.
    let (answer, denied, environment): (i64, bool, Value) = eval(
        &mut sandbox,
        br#"
        local file = io.open('new.lua', 'w')
        return require('answer'), file == nil, os.getenv('MOONSEED_EXAMPLE')
    "#,
    )?;
    assert_eq!(answer, 42);
    assert!(denied);
    assert!(matches!(environment, Value::Nil));
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
