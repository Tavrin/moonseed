//! Owned and borrowed embedding values (ADR 0053).
//!
//! Object references belong to one runtime, including across restore. Raw
//! table operations never call Lua. Semantic operations and host calls run
//! on the main thread with a fuel bound; native calls use VM continuations.
//! Borrowed Rust strings and byte slices convert into Lua; use `StrRef`
//! for borrowed reads and `String` or `Vec<u8>` for owned reads.

mod builder;
mod conversion;
pub(crate) mod error;
pub(crate) mod hooks;
mod host;
pub use hooks::{
    HookAction, HookContext, HookEvent, HookFunction, HookInfo, HookMask, HookSettings,
};
pub(crate) mod native;
pub(crate) mod roots;
mod value;
pub use native::{
    CallOutcome, Completion, NativeContext, NativeReturn, Resume, ResumeOutcome, WaitInfo,
    WaitRequest,
};

pub use builder::{Libraries, RuntimeBuilder};
pub use conversion::{Coerce, FromLua, FromLuaMulti, IntoLua, IntoLuaMulti, MultiValue, Variadic};
pub use error::{ApiError, ConversionError, Error, LuaError, Result};
pub use host::{Host, HostCapabilities, HostEnv, ModuleResolver, Resolved, ResolverPolicy};
pub use value::{
    AnyUserData, Function, FunctionKind, LuaString, StrRef, Table, Thread, ThreadStatus,
    UserDataRefMut, Value, ValueRef,
};
