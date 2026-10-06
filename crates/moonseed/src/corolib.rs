//! The `coroutine` library's functions (ADR 0041). The VM side is
//! `runtime/coroutine.rs`.
//!
//! The library exposes the thread machinery the VM already has: threads
//! with frame stacks of their own, `Resume`'s resumer link, a failed
//! coroutine that keeps its stack, and `CloseThread`. Resuming and
//! yielding switch the running thread; nothing nests on the Rust stack.

use crate::host::{Builtin, HostRegistry};

/// A `coroutine` function, or the function `coroutine.wrap` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoFn {
    Create,
    Resume,
    Yield,
    Wrap,
    /// What a `coroutine.wrap` function runs: a native closure whose one
    /// value is its coroutine.
    WrapCall,
    Status,
    Running,
    IsYieldable,
    Close,
}

/// The library's fields, their registry symbols, and their functions.
pub(crate) const COROUTINE_FUNCTIONS: [(&str, &str, CoFn); 8] = [
    ("create", "coroutine.create", CoFn::Create),
    ("resume", "coroutine.resume", CoFn::Resume),
    ("yield", "coroutine.yield", CoFn::Yield),
    ("wrap", "coroutine.wrap", CoFn::Wrap),
    ("status", "coroutine.status", CoFn::Status),
    ("running", "coroutine.running", CoFn::Running),
    ("isyieldable", "coroutine.isyieldable", CoFn::IsYieldable),
    ("close", "coroutine.close", CoFn::Close),
];

/// The symbol of the functions `coroutine.wrap` makes.
pub(crate) const WRAP_CALL: &str = "coroutine.wrapped";

/// Register the `coroutine` functions.
pub fn register_coroutine(registry: &mut HostRegistry) {
    for (_, symbol, function) in COROUTINE_FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Coroutine(function));
    }
    registry.register_builtin(WRAP_CALL, Builtin::Coroutine(CoFn::WrapCall));
}

/// The most coroutines one chain of resumes may hold. Lua 5.4.9 counts
/// resumes and C calls together against 200 and reports "C stack
/// overflow" on the 197th nested resume of plain Lua functions; Moonseed
/// counts the coroutines in the chain, and stops at the same depth.
pub(crate) const MAX_RESUME_DEPTH: usize = 196;
