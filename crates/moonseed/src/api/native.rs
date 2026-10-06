use super::{
    AnyUserData, ApiError, FromLua, FromLuaMulti, Function, IntoLua, IntoLuaMulti, LuaError,
    LuaString, MultiValue, Result, Table, UserDataRefMut, Value, ValueRef,
};
use crate::value::Value as Raw;
use crate::{EffectId, HostUserdata, Journal, Runtime, WaitKey};

pub(crate) type TypedCallback = dyn Fn(&mut NativeContext<'_>) -> Result<()>;

pub(crate) type Callback = dyn Fn(&mut NativeContext<'_>) -> Result<NativeReturn>;

/// A native's results or its next execution request. Calls never recurse
/// into the executor: Lua runs after the callback and its borrows end.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum NativeReturn {
    /// Return an exact sequence, including nil holes.
    Return(MultiValue),
    /// Raise an arbitrary Lua error object.
    Error(Value),
    /// Suspend until the host completes the VM-issued key.
    Wait(WaitRequest),
    /// Call a callable Lua value and resume this symbol in a new VM step.
    /// Yields may cross this frame whenever the containing thread may yield.
    CallLua {
        /// A function or a value with `__call`.
        function: Value,
        /// The call's arguments.
        args: MultiValue,
        /// An application-defined continuation discriminator.
        tag: u32,
        /// Values kept alive until the continuation resumes.
        keep: MultiValue,
    },
}

impl From<MultiValue> for NativeReturn {
    fn from(values: MultiValue) -> Self {
        Self::Return(values)
    }
}

/// A host operation's name and exact Lua payload.
#[derive(Clone, Debug)]
pub struct WaitRequest {
    /// Stable operation name, preserved in snapshots.
    pub operation: String,
    /// Values the host can inspect through `Runtime::wait`.
    pub payload: MultiValue,
}

/// The pending operation associated with a wait key.
pub type WaitInfo = WaitRequest;

/// How a native's own Lua call ended.
#[derive(Clone, Debug)]
pub enum ResumeOutcome {
    /// The call returned these exact values.
    Returned(MultiValue),
    /// The frame caught the call's Lua error.
    Errored(LuaError),
}

/// Snapshot-backed continuation state supplied on reinvocation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Resume {
    /// The tag supplied by `CallLua`.
    pub tag: u32,
    /// The called function's return or error.
    pub outcome: ResumeOutcome,
    /// The values retained by `CallLua`.
    pub kept: MultiValue,
}

/// How the host ends a wait. Object values are owner checked.
#[derive(Clone, Debug)]
pub enum Completion {
    /// Return the exact values.
    Return(Vec<Value>),
    /// Raise this unchanged Lua error object.
    Error(Value),
}

/// The result of driving a main-thread call with a fuel bound.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CallOutcome<R> {
    /// The call completed and converted its results.
    Done(R),
    /// The call is waiting for this host operation.
    Waiting(WaitKey),
    /// Lua requested exit; the runtime is terminal.
    ExitRequested {
        /// The requested status.
        status: crate::ExitStatus,
        /// Whether runtime closing completed before returning.
        close: bool,
    },
    /// The fuel bound ended; continue with `run` and `finish_call`.
    OutOfFuel,
}

/// One callback invocation. Borrowed arguments add no roots. Mutable
/// userdata borrows hold the context, so they cannot cross a Lua call.
///
/// ```compile_fail,E0502
/// use moonseed::{NativeContext, NativeReturn, AnyUserData, HostUserdata, MultiValue};
/// struct Counter(i64);
/// impl HostUserdata for Counter {
///     const SYMBOL: &'static str = "doctest.Counter";
///     fn logical_size(&self) -> u64 { 8 }
/// }
/// fn held(cx: &mut NativeContext<'_>) -> moonseed::Result<NativeReturn> {
///     let object: AnyUserData = cx.argument(0)?;
///     let mut guard = cx.borrow_userdata_mut::<Counter>(&object)?;
///     let call = NativeReturn::CallLua {
///         function: cx.arg(1).to_owned_value()?,
///         args: MultiValue::new(), tag: 0, keep: MultiValue::new(),
///     };
///     guard.0 += 1;
///     Ok(call)
/// }
/// ```
pub struct NativeContext<'cx> {
    pub(crate) runtime: &'cx mut Runtime,
    pub(crate) args: &'cx [Raw],
    pub(crate) callee: Raw,
    pub(crate) effect: Option<EffectId>,
    pub(crate) journal: Option<&'cx mut Journal>,
    pub(crate) resume: Option<&'cx Resume>,
}

impl NativeContext<'_> {
    /// The argument count, including nil holes.
    pub fn arg_count(&self) -> usize {
        self.args.len()
    }
    /// Borrow argument `index` (zero-based), or nil when absent.
    #[inline]
    pub fn arg(&self, index: usize) -> ValueRef<'_> {
        ValueRef::new(
            self.runtime,
            self.args.get(index).copied().unwrap_or(Raw::Nil),
        )
    }
    /// Borrow a string argument's exact bytes, without rooting or coercion.
    pub fn string_bytes(&self, index: usize) -> Option<&[u8]> {
        self.arg(index).as_string().map(|string| string.as_bytes())
    }
    /// Extract a typed argument, positioning failures one-based.
    /// If the one-based index cannot fit in `usize`, the failure is unpositioned.
    #[inline]
    pub fn argument<T: FromLua>(&mut self, index: usize) -> Result<T> {
        <T as FromLua>::from_context(self, index)
            .map_err(|error| positioned(error, index.checked_add(1)))
    }
    /// Extract the invocation's arguments as a typed sequence.
    #[inline]
    pub fn arguments<A: FromLuaMulti>(&mut self) -> Result<A> {
        <A as FromLuaMulti>::from_context(self)
    }
    /// Turn a positioned conversion failure into a catchable Lua argument error.
    /// Use this when converting arguments in a `HostRegistry::function` callback.
    pub fn argument_error(&mut self, error: super::Error) -> super::Error {
        self.runtime.api_argument_error(self.callee, error)
    }
    /// Prepare a custom Lua argument error; indexes are zero-based.
    pub fn bad_argument(&mut self, index: usize, detail: &str) -> Result<super::LuaError> {
        self.runtime.api_bad_argument(index, detail.as_bytes())
    }
    #[inline]
    pub(crate) fn write_results<R: IntoLuaMulti>(&mut self, values: R) -> Result<()> {
        values.write_native(self)
    }
    #[inline]
    pub(crate) fn push_result(&mut self, value: Value) -> Result<()> {
        let raw = value.raw(self.runtime)?;
        self.runtime.native_results.push(raw);
        Ok(())
    }
    pub(crate) fn take_results(&mut self) -> Result<MultiValue> {
        let raw = std::mem::take(&mut self.runtime.native_results);
        raw.iter()
            .map(|value| self.runtime.api_owned(*value))
            .collect::<Result<Vec<_>>>()
            .map(MultiValue)
    }
    /// Borrow all native closure captures, in slot order.
    pub fn captures(&self) -> Vec<ValueRef<'_>> {
        self.runtime
            .api_captures(self.callee)
            .iter()
            .map(|raw| ValueRef::new(self.runtime, *raw))
            .collect()
    }
    /// Replace a zero-based capture using the collector's write barrier.
    pub fn set_capture<V: IntoLua>(&mut self, index: usize, value: V) -> Result<()> {
        let value = value.into_lua(self.runtime)?;
        self.runtime.api_set_capture(self.callee, index, &value)
    }
    /// An owned copy of the continuation supplied when the native's Lua call ends.
    /// Use [`Self::resumed_ref`] to inspect it without copying its value buffers.
    pub fn resumed(&self) -> Option<Resume> {
        self.resume.cloned()
    }
    /// Borrow the continuation without copying its kept or returned values.
    pub fn resumed_ref(&self) -> Option<&Resume> {
        self.resume
    }
    /// Request a Lua call using reusable argument and continuation buffers.
    pub fn call_lua(
        &mut self,
        function: impl IntoLua,
        args: impl IntoLuaMulti,
        tag: u32,
        keep: impl IntoLuaMulti,
    ) -> Result<NativeReturn> {
        Ok(NativeReturn::CallLua {
            function: function.into_lua(self.runtime)?,
            args: args.into_lua_multi(self.runtime)?,
            tag,
            keep: keep.into_lua_multi(self.runtime)?,
        })
    }
    /// Convert any result sequence into a native return.
    #[inline]
    pub fn return_values<R: IntoLuaMulti>(&mut self, values: R) -> Result<NativeReturn> {
        values
            .into_lua_multi(self.runtime)
            .map(NativeReturn::Return)
    }
    /// Create a rooted byte string.
    pub fn create_string(&mut self, bytes: impl AsRef<[u8]>) -> Result<LuaString> {
        self.runtime.create_string(bytes)
    }
    /// Create a rooted empty table.
    pub fn create_table(&mut self) -> Result<Table> {
        self.runtime.create_table()
    }
    /// Create registered host userdata.
    pub fn create_userdata<T: HostUserdata>(
        &mut self,
        value: T,
        user_values: usize,
    ) -> Result<AnyUserData> {
        self.runtime.create_host_userdata(value, user_values)
    }
    /// Create a native closure with at most 255 captures.
    pub fn make_closure(&mut self, symbol: &str, captures: impl IntoLuaMulti) -> Result<Function> {
        self.runtime.make_closure(symbol, captures)
    }
    /// Borrow a typed host userdata payload.
    pub fn borrow_userdata<'a, T: HostUserdata>(&'a self, value: &AnyUserData) -> Result<&'a T> {
        value.borrow(self.runtime)
    }
    /// Borrow a typed mutable payload, holding the context until dropped.
    pub fn borrow_userdata_mut<'a, T: HostUserdata>(
        &'a mut self,
        value: &AnyUserData,
    ) -> Result<UserDataRefMut<'a, T>> {
        value.borrow_mut(self.runtime)
    }
    /// Read a raw table entry without running Lua.
    /// Read a table without running `__index`. Ownership and conversion errors
    /// are [`ApiError`]s. For semantic indexing, pass a Lua accessor
    /// (`function(t, k) return t[k] end`) to [`Self::call_lua`] and handle
    /// [`ResumeOutcome`] on reinvocation. No Rust borrow crosses that call;
    /// its arguments/kept values and continuation are checkpoint state.
    pub fn raw_get<K: IntoLua, V: FromLua>(&mut self, table: &Table, key: K) -> Result<V> {
        table.raw_get(self.runtime, key)
    }
    /// Write a raw table entry without running Lua.
    pub fn raw_set<K: IntoLua, V: IntoLua>(
        &mut self,
        table: &Table,
        key: K,
        value: V,
    ) -> Result<()> {
        table.raw_set(self.runtime, key, value)
    }
    /// Read the raw table border.
    pub fn raw_len(&self, table: &Table) -> Result<i64> {
        table.raw_len(self.runtime)
    }
    /// Read a value's actual metatable, bypassing protection.
    pub fn metatable<V: IntoLua>(&mut self, object: V) -> Result<Option<Table>> {
        let object = object.into_lua(self.runtime)?;
        self.runtime.api_value_metatable(&object)
    }
    /// Set or clear a table's or userdata's actual metatable.
    pub fn set_metatable<V: IntoLua>(&mut self, object: V, meta: Option<&Table>) -> Result<()> {
        let object = object.into_lua(self.runtime)?;
        self.runtime.api_value_set_metatable(&object, meta)
    }
    /// Effect identity, available only to external natives.
    pub fn effect(&self) -> Option<EffectId> {
        self.effect
    }
    /// Journal, available only to external natives.
    pub fn journal(&mut self) -> Option<&mut Journal> {
        self.journal.as_deref_mut()
    }
}

fn positioned(error: super::Error, index: Option<usize>) -> super::Error {
    if let super::Error::Api(ApiError::Conversion(mut error)) = error {
        error.position = index;
        error.into()
    } else {
        error
    }
}

impl NativeContext<'_> {
    /// Root the thread executing this native.
    pub fn current_thread(&mut self) -> Result<Value> {
        let thread = self.runtime.heap().active.ok_or(crate::VmError::Corrupt)?;
        self.runtime.api_owned(Raw::Thread(thread))
    }
    /// Root the globals table.
    pub fn globals(&mut self) -> Table {
        self.runtime.globals()
    }
    /// Install a hook; no thread selects the currently executing thread.
    pub fn set_hook(
        &mut self,
        thread: Option<&Value>,
        symbol: &str,
        mask: super::HookMask,
        count: i32,
    ) -> Result<()> {
        let current = self.current_thread()?;
        self.runtime
            .set_hook(Some(thread.unwrap_or(&current)), symbol, mask, count)
    }
    /// Remove a hook; no thread selects the currently executing thread.
    pub fn clear_hook(&mut self, thread: Option<&Value>) -> Result<()> {
        let current = self.current_thread()?;
        self.runtime.clear_hook(Some(thread.unwrap_or(&current)))
    }
    /// Inspect a hook; no thread selects the currently executing thread.
    pub fn get_hook(&mut self, thread: Option<&Value>) -> Result<Option<super::HookSettings>> {
        let current = self.current_thread()?;
        self.runtime.get_hook(Some(thread.unwrap_or(&current)))
    }
}
