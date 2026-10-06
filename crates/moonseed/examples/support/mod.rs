//! Helpers used by the embedding examples.
#![allow(dead_code)] // Each executable uses a different subset of these helpers.
use moonseed::*;
#[derive(Debug)]
pub struct ExampleError(String);
impl From<&str> for ExampleError {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}
impl From<String> for ExampleError {
    fn from(value: String) -> Self {
        Self(value)
    }
}
macro_rules! diagnostic {
    ($($ty:ty),*) => { $(impl From<$ty> for ExampleError {
        fn from(value: $ty) -> Self { Self(format!("{value:?}")) }
    })* };
}
diagnostic!(moonseed::Error, CompileError, VmError, SnapshotError);
impl std::fmt::Display for ExampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ExampleError {}
pub type ExampleResult<T = ()> = std::result::Result<T, ExampleError>;
pub fn eval<R: FromLuaMulti>(rt: &mut Runtime, source: &[u8]) -> ExampleResult<R> {
    rt.load_main(&compile(source)?)?;
    match rt.run(10_000, &mut Journal::new())? {
        StepOutcome::Completed => Ok(R::from_lua_multi(rt.result_values()?, rt)?),
        other => Err(format!("chunk did not complete: {other:?}").into()),
    }
}
pub fn done<R>(outcome: CallOutcome<R>) -> ExampleResult<R> {
    match outcome {
        CallOutcome::Done(value) => Ok(value),
        other => Err(match other {
            CallOutcome::Waiting(_) => "call is waiting",
            CallOutcome::OutOfFuel => "call ran out of fuel",
            CallOutcome::ExitRequested { .. } => "call requested exit",
            _ => "unsupported call outcome",
        }
        .into()),
    }
}
pub struct Counter(pub i64);
impl HostUserdata for Counter {
    const SYMBOL: &'static str = "example.Counter";
    fn logical_size(&self) -> u64 {
        8
    }
}
impl PortableUserdata for Counter {
    fn encode(&self) -> Vec<u8> {
        self.0.to_le_bytes().to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(i64::from_le_bytes(bytes.try_into().ok()?)))
    }
}
pub fn counter_registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    registry.register_portable_userdata::<Counter>();
    registry.function("example.next", NativePolicy::VmLocal, |cx| {
        let Value::UserData(counter) = cx.captures()[0].to_owned_value()? else {
            return Err(ApiError::WrongType.into());
        };
        let value = {
            let mut state = cx.borrow_userdata_mut::<Counter>(&counter)?;
            state.0 = state.0.wrapping_add(1);
            state.0
        };
        cx.return_values(value)
    });
    registry
}
