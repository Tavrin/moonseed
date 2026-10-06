//! Synchronous, symbol-bound host debug hooks.
use std::ops::{BitOr, BitOrAssign};

use super::{IntoLua, Result, Table, Value, ValueRef};
use crate::Runtime;

pub(crate) type HookCallback = dyn Fn(&mut HookContext<'_>) -> Result<HookAction>;

/// Event selection. A positive count independently enables count events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookMask(pub(crate) u8);
impl HookMask {
    /// No call, return, or line events.
    pub const NONE: Self = Self(0);
    /// Calls, including tail calls.
    pub const CALL: Self = Self(1);
    /// Returns.
    pub const RETURN: Self = Self(2);
    /// Source line changes and backward jumps.
    pub const LINE: Self = Self(4);
    /// Whether all requested events are selected.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}
impl BitOr for HookMask {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
impl BitOrAssign for HookMask {
    fn bitor_assign(&mut self, other: Self) {
        *self = *self | other;
    }
}

/// The delivered event, independent of execution fuel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    /// A function activation has entered.
    Call,
    /// A function activation is returning.
    Return,
    /// A new line or backward jump is about to execute.
    Line,
    /// The instruction countdown expired.
    Count,
    /// A Lua activation replaced its caller.
    TailCall,
}
impl HookEvent {
    /// Lua's event name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Return => "return",
            Self::Line => "line",
            Self::Count => "count",
            Self::TailCall => "tail call",
        }
    }
}

/// A synchronous callback's action. No Rust continuation or wait is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookAction {
    /// Continue execution.
    Continue,
    /// Yield zero values from a line/count event in a yieldable coroutine.
    Yield,
}

/// Installed hook target. Lua hooks and inherited wrappers are distinguishable.
#[derive(Clone, Debug)]
pub enum HookFunction {
    /// A registered host symbol.
    Host(String),
    /// A rooted Lua or native function installed by debug.sethook.
    Lua(Value),
    /// An inherited debug wrapper without a function in this thread.
    InheritedLua,
}

/// The installed configuration; transient countdowns remain VM state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HookSettings {
    /// Callback identity.
    pub function: HookFunction,
    /// Call, return and line selection.
    pub mask: HookMask,
    /// Base instruction interval (nonpositive disables count events).
    pub count: i32,
}

/// Getinfo-style inspection of one activation. Byte strings stay exact.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HookInfo {
    /// Function name inferred from its caller.
    pub name: Option<Vec<u8>>,
    /// Name category, or the empty string.
    pub namewhat: &'static str,
    /// Lua, main, or C.
    pub what: &'static str,
    /// Exact chunk name.
    pub source: Vec<u8>,
    /// Lua's abbreviated chunk name.
    pub short_src: Vec<u8>,
    /// Current source line, or -1.
    pub currentline: i64,
    /// First definition line, or -1.
    pub linedefined: i64,
    /// Last definition line, or -1.
    pub lastlinedefined: i64,
    /// Whether this activation was entered through a tail call.
    pub istailcall: bool,
    /// First transferred local, one-based, or zero.
    pub ftransfer: u32,
    /// Number of transferred values, including nil holes.
    pub ntransfer: u32,
    /// Fixed parameter count.
    pub nparams: i64,
    /// Upvalue count.
    pub nups: i64,
    /// Whether the function is vararg.
    pub isvararg: bool,
}

/// One host hook invocation. Level zero is the interrupted activation.
/// Views cannot outlive this borrow; owned values may be retained as roots.
/// Inspection and raw table operations never call Lua or wait.
/// Callbacks are synchronous and host-bounded. Return [`HookAction::Yield`] only
/// for line/count events in a yieldable coroutine; other yields raise Lua errors.
/// Hook symbols/settings/continuations are checkpoint state, callback code and
/// counters are host state. Missing symbols fail restore, including masked hooks.
/// A panic propagates and poisons execution/snapshots. See [`crate::HostRegistry`]
/// and [`Runtime::set_hook`].
///
/// ```
/// use moonseed::{HookAction, HookMask, HostRegistry, Runtime};
/// let mut registry = HostRegistry::new();
/// registry.register_hook("example.profile", |cx| {
///     let _ = (cx.event(), cx.line(), cx.info(0)?);
///     Ok(HookAction::Continue)
/// });
/// let mut rt = Runtime::builder().registry(registry).build()?;
/// rt.set_hook(None, "example.profile", HookMask::CALL | HookMask::RETURN, 0)?;
/// # Ok::<(), moonseed::Error>(())
/// ```
pub struct HookContext<'cx> {
    pub(crate) runtime: &'cx mut Runtime,
    pub(crate) thread: crate::id::Handle<crate::heap::ThreadObj>,
    pub(crate) event: HookEvent,
    pub(crate) line: Option<u32>,
}
impl HookContext<'_> {
    /// The delivered event.
    pub fn event(&self) -> HookEvent {
        self.event
    }
    /// The line argument; absent for other events and stripped chunks.
    pub fn line(&self) -> Option<u32> {
        self.line
    }
    /// Inspect a zero-based activation level.
    pub fn info(&self, level: usize) -> Result<Option<HookInfo>> {
        self.runtime.api_hook_info(self.thread, level)
    }
    /// Number of inspectable activation levels.
    pub fn depth(&self) -> Result<usize> {
        self.runtime.api_hook_depth(self.thread)
    }
    /// Read a one-based local; negative indexes read varargs.
    pub fn local(&self, level: usize, index: i32) -> Result<Option<(Vec<u8>, ValueRef<'_>)>> {
        self.runtime.api_hook_local(self.thread, level, index)
    }
    /// Replace a local with the same rules as debug.setlocal.
    pub fn set_local(
        &mut self,
        level: usize,
        index: i32,
        value: impl IntoLua,
    ) -> Result<Option<Vec<u8>>> {
        let value = value.into_lua(self.runtime)?;
        self.runtime
            .api_hook_set_local(self.thread, level, index, &value)
    }
    /// Read a one-based upvalue of an activation's function.
    pub fn upvalue(&self, level: usize, index: usize) -> Result<Option<(Vec<u8>, ValueRef<'_>)>> {
        self.runtime.api_hook_upvalue(self.thread, level, index)
    }
    /// Root the interrupted thread without exposing internal handles.
    pub fn thread(&mut self) -> Result<Value> {
        self.runtime
            .api_owned(crate::value::Value::Thread(self.thread))
    }
    /// Root the globals table.
    pub fn globals(&mut self) -> Table {
        self.runtime.globals()
    }
    /// Create a rooted empty table.
    pub fn create_table(&mut self) -> Result<Table> {
        self.runtime.create_table()
    }
    /// Read a raw table entry without Lua execution.
    pub fn raw_get<K: IntoLua, V: super::FromLua>(&mut self, table: &Table, key: K) -> Result<V> {
        table.raw_get(self.runtime, key)
    }
    /// Write a raw table entry without Lua execution.
    pub fn raw_set<K: IntoLua, V: IntoLua>(
        &mut self,
        table: &Table,
        key: K,
        value: V,
    ) -> Result<()> {
        table.raw_set(self.runtime, key, value)
    }
    /// Read a raw array border.
    pub fn raw_len(&self, table: &Table) -> Result<i64> {
        table.raw_len(self.runtime)
    }
    /// Create a rooted byte string.
    pub fn create_string(&mut self, bytes: impl AsRef<[u8]>) -> Result<super::LuaString> {
        self.runtime.create_string(bytes)
    }
    /// Prepare a catchable Lua error without executing Lua.
    pub fn error(&mut self, value: impl IntoLua) -> Result<super::LuaError> {
        let value = value.into_lua(self.runtime)?;
        super::LuaError::new(value, crate::LuaFault::Error, self.runtime)
    }
    /// Replace this thread's host hook. Suppression lasts until this callback ends.
    pub fn set_hook(&mut self, symbol: &str, mask: HookMask, count: i32) -> Result<()> {
        self.runtime.api_set_hook(self.thread, symbol, mask, count)
    }
    /// Remove this thread's hook; suppression lasts until the callback ends.
    pub fn clear_hook(&mut self) -> Result<()> {
        self.runtime.api_clear_hook(self.thread)
    }
}
