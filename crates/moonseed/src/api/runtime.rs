//! Runtime-side embedding operations. This child module can reuse the
//! runtime's allocation and collector boundaries without exposing them.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::api::{
    AnyUserData, ApiError, Error, Function, FunctionKind, Libraries, LuaError, LuaString,
    MultiValue, Result as ApiResult, RuntimeBuilder, Table, Thread, ThreadStatus, UserDataRefMut,
    Value as Owned, ValueRef,
};

fn raw_object(kind: Kind, index: u32, generation: u32) -> Option<Value> {
    Some(match kind {
        Kind::String => Value::String(Handle::new(index, generation)),
        Kind::Table => Value::Table(Handle::new(index, generation)),
        Kind::Closure => Value::Closure(Handle::new(index, generation)),
        Kind::Thread => Value::Thread(Handle::new(index, generation)),
        Kind::NativeClosure => Value::NativeClosure(Handle::new(index, generation)),
        Kind::Userdata => Value::Userdata(Handle::new(index, generation)),
        Kind::Proto | Kind::Upvalue => return None,
    })
}

impl Runtime {
    /// Begin construction with default configuration and no libraries.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    /// Construct an idle runtime with this configuration and registry.
    pub fn new(config: Config, registry: HostRegistry) -> ApiResult<Self> {
        Self::builder().config(config).registry(registry).build()
    }

    pub(crate) fn api_build(mut builder: RuntimeBuilder) -> ApiResult<Self> {
        builder.libraries.register(&mut builder.registry);
        let idle = ProtoSpec {
            ops: vec![Op::Return { base: 0, count: 0 }],
            byte_consts: Vec::new(),
            captures: Vec::new(),
            children: Vec::new(),
            max_reg: 1,
            params: 0,
            vararg: false,
            debug: None,
        };
        let mut runtime = Self::boot(builder.config, builder.registry, &idle, false).map_err(
            |error| match error {
                VmError::MemoryLimit => Error::Lua(LuaError::construction(LuaFault::Memory)),
                VmError::StackLimit => Error::Lua(LuaError::construction(LuaFault::StackOverflow)),
                other => Error::Vm(other),
            },
        )?;
        let entry = runtime.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = runtime
            .heap
            .threads
            .get_mut(entry)
            .ok_or(VmError::Corrupt)?;
        thread.status = Status::Completed;
        thread.frames.clear();
        thread.top = 0;
        runtime.apply_capabilities(&builder.capabilities);
        macro_rules! install {
            ($flag:ident, $method:ident) => {
                if builder.libraries.contains(Libraries::$flag) {
                    runtime
                        .$method()
                        .map_err(|error| runtime.api_error(error))?;
                }
            };
        }
        install!(BASE, install_base);
        install!(PACKAGE, install_package);
        if builder.libraries.contains(Libraries::PACKAGE) {
            runtime
                .set_package_paths(&builder.package_path, &builder.package_cpath)
                .map_err(|error| runtime.api_error(error))?;
        }
        install!(COROUTINE, install_coroutine);
        install!(MATH, install_math);
        install!(TABLE, install_table);
        install!(STRING, install_string);
        install!(UTF8, install_utf8);
        install!(DEBUG, install_debug);
        install!(OS, install_os);
        install!(IO, install_io);
        Ok(runtime)
    }

    pub(crate) fn api_roots(&mut self) {
        self.heap
            .api_roots
            .get_or_insert_with(|| Rc::new(RefCell::new(crate::api::roots::RootTable::default())));
    }

    pub(crate) fn api_owned(&mut self, value: Value) -> ApiResult<Owned> {
        self.api_roots();
        Owned::wrap(value, &self.heap, self.owner, self.heap.api_roots.as_ref())
    }

    pub(crate) fn api_error(&mut self, error: VmError) -> Error {
        let class = match error {
            VmError::MemoryLimit => LuaFault::Memory,
            VmError::StackLimit => LuaFault::StackOverflow,
            VmError::Api(error) => return Error::Api(error),
            other => return Error::Vm(other),
        };
        let raw = self.fault_value(class);
        match self
            .api_owned(raw)
            .and_then(|value| LuaError::new(value, class, self))
        {
            Ok(error) => Error::Lua(error),
            Err(error) => error,
        }
    }

    /// Reacquire and root a Lua object by logical identity. Internal
    /// prototypes and upvalue cells have no public value representation.
    pub fn object(&mut self, id: ObjectId) -> Option<Owned> {
        self.settle_atomic();
        let (kind, index, generation) = self.heap.find_by_id(id)?;
        let value = raw_object(kind, index, generation)?;
        self.api_owned(value).ok()
    }

    /// Borrow an object without rooting. The borrow prevents collection;
    /// an atomic phase is settled before the view is made.
    pub fn object_ref(&mut self, id: ObjectId) -> Option<ValueRef<'_>> {
        self.settle_atomic();
        let (kind, index, generation) = self.heap.find_by_id(id)?;
        let value = raw_object(kind, index, generation)?;
        self.api_roots();
        Some(ValueRef::new(self, value))
    }

    /// Create a rooted byte string, checking the string limit and quota.
    pub fn create_string(&mut self, bytes: impl AsRef<[u8]>) -> ApiResult<LuaString> {
        self.settle_atomic();
        let bytes = bytes.as_ref();
        if bytes.len() > self.heap.max_string {
            return Err(self.api_error(VmError::MemoryLimit));
        }
        if !self.in_callback {
            self.make_room(1, cost::OBJECT + bytes.len() as u64);
        }
        self.ensure_room(cost::OBJECT + bytes.len() as u64)
            .map_err(|error| self.api_error(error))?;
        let handle = self
            .alloc_string(bytes.to_vec())
            .map_err(|error| self.api_error(error))?;
        match self.api_owned(Value::String(handle))? {
            Owned::String(string) => Ok(string),
            _ => Err(VmError::Corrupt.into()),
        }
    }

    /// Create a rooted empty table, checking the quota.
    pub fn create_table(&mut self) -> ApiResult<Table> {
        self.settle_atomic();
        if !self.in_callback {
            self.make_room(1, cost::OBJECT);
        }
        let handle = self.alloc_table().map_err(|error| self.api_error(error))?;
        match self.api_owned(Value::Table(handle))? {
            Owned::Table(table) => Ok(table),
            _ => Err(VmError::Corrupt.into()),
        }
    }

    /// Root this runtime's globals table.
    pub fn globals(&mut self) -> Table {
        self.settle_atomic();
        let globals = self.heap.globals.expect("runtime globals");
        let Owned::Table(table) = self.api_owned(Value::Table(globals)).expect("live globals")
        else {
            unreachable!("globals are a table")
        };
        table
    }

    /// Explicitly install Lua's `arg` table. `pre_args` are in command-line
    /// order: their last element is `arg[-1]`, and their first is `arg[-len]`.
    /// The script occupies `arg[0]`; `args` occupy `arg[1..]`. Byte strings are
    /// preserved. No argument table is installed automatically.
    pub fn install_arg(
        &mut self,
        script: impl AsRef<[u8]>,
        args: &[impl AsRef<[u8]>],
        pre_args: &[impl AsRef<[u8]>],
    ) -> ApiResult<Table> {
        let table = self.create_table()?;
        let script = self.create_string(script)?;
        table.raw_set(self, 0i64, script)?;
        for (i, bytes) in args.iter().enumerate() {
            let value = self.create_string(bytes)?;
            let index = i64::try_from(i)
                .map_err(|_| ApiError::InvalidCallState)?
                .checked_add(1)
                .ok_or(ApiError::InvalidCallState)?;
            table.raw_set(self, index, value)?;
        }
        for (i, bytes) in pre_args.iter().enumerate() {
            let value = self.create_string(bytes)?;
            let index = i64::try_from(i).map_err(|_| ApiError::InvalidCallState)?
                - i64::try_from(pre_args.len()).map_err(|_| ApiError::InvalidCallState)?;
            table.raw_set(self, index, value)?;
        }
        self.globals().raw_set(self, "arg", table.clone())?;
        Ok(table)
    }

    /// Load a chunk onto the idle main thread, preserving globals and its
    /// thread identity. Drive it with `run`. A live chunk is `Busy`.
    pub fn load_main(&mut self, chunk: &crate::CompiledChunk) -> ApiResult<()> {
        if self.in_callback
            || self.callback_failed
            || self.trap.is_some()
            || self.is_closing()
            || self.heap.finalizers.exit.is_some()
        {
            return Err(ApiError::InvalidCallState.into());
        }
        self.settle_atomic();
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        if self.host_call
            || !matches!(thread.status, Status::Completed | Status::Failed)
            || thread.closing
            || self.heap.finalizers.running
        {
            return Err(ApiError::Busy.into());
        }
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        let closure = self
            .instantiate(&chunk.proto, Value::Table(globals), ChunkName::Unnamed)
            .map_err(|error| self.api_error(error))?;
        self.load_main_closure(closure, u32::from(chunk.proto.max_reg))
    }

    pub(super) fn load_main_closure(
        &mut self,
        closure: Handle<ClosureObj>,
        limit: u32,
    ) -> ApiResult<()> {
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let keep = self.api_owned(Value::Closure(closure))?;
        let old = self
            .heap
            .threads
            .get(entry)
            .ok_or(VmError::Corrupt)?
            .charged_slots;
        let growth = cost::STACK_SLOT * u64::from(limit.saturating_sub(old));
        self.make_room(0, growth);
        if !self.heap.gc.fits(growth) {
            return Err(self.api_error(VmError::MemoryLimit));
        }
        let thread = self.heap.threads.get_mut(entry).ok_or(VmError::Corrupt)?;
        let held = thread.charged_held;
        thread.stack.clear();
        thread.stack.grow_to(limit as usize);
        thread.charged_slots = limit;
        let hook_bytes = if let Some(hook) = self.heap.hooks.get_mut(thread.id) {
            hook.allow_hook = true;
            hook.old_pc = None;
            hook.pending = None;
            hook.hook_yield = false;
            hook.instruction = None;
            hook.after = crate::runtime::hooks::AfterHook::Continue;
            hook.transfer = None;
            hook.restore_cursor = None;
            crate::runtime::hooks::HOOK_BYTES
        } else {
            0
        };
        thread.charged_held = hook_bytes;
        thread.top = limit;
        count!("frame_pushes");
        thread.frames.clear();
        thread.frames.push(Frame {
            closure,
            pc: 0,
            base: 0,
            limit,
            nresults: COUNT_OPEN,
            vararg_len: 0,
            flags: 0,
            cold: None,
        });
        thread.open_upvalues.clear();
        thread.open_above = 0;
        thread.host_results.clear();
        thread.unwind = None;
        thread.error = None;
        thread.resumed_by = None;
        thread.tbc.clear();
        thread.status = Status::Ready;
        self.heap.gc.charge(growth);
        self.heap
            .give_back(cost::STACK_SLOT * u64::from(old.saturating_sub(limit)) + held - hook_bytes);
        self.heap.active = Some(entry);
        self.refresh_hook_trap();
        drop(keep);
        Ok(())
    }

    /// Completed main-thread results as rooted values, preserving nil holes.
    pub fn result_values(&mut self) -> ApiResult<MultiValue> {
        self.settle_atomic();
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        if thread.status != Status::Completed {
            return Err(ApiError::InvalidCallState.into());
        }
        let results = thread.host_results.clone();
        results
            .into_iter()
            .map(|value| self.api_owned(value))
            .collect::<ApiResult<Vec<_>>>()
            .map(MultiValue)
    }

    pub(crate) fn api_string<'a>(&'a self, string: &LuaString) -> ApiResult<&'a [u8]> {
        let Value::String(handle) = string.0.value(self.owner)? else {
            return Err(ApiError::WrongType.into());
        };
        self.heap
            .string_bytes(handle)
            .ok_or_else(|| ApiError::Released.into())
    }

    fn api_table(&self, table: &Table) -> ApiResult<Handle<TableObj>> {
        let Value::Table(handle) = table.0.value(self.owner)? else {
            return Err(ApiError::WrongType.into());
        };
        Ok(handle)
    }

    pub(crate) fn api_raw_get(&mut self, table: &Table, key: &Owned) -> ApiResult<Owned> {
        let handle = self.api_table(table)?;
        let key = key.raw(self)?;
        self.settle_atomic();
        let key = self.heap.key_view(key).map_err(|_| ApiError::InvalidKey)?;
        let value = self
            .heap
            .table_get_view(handle, key)
            .ok_or(ApiError::Released)?;
        self.api_owned(value)
    }

    pub(crate) fn api_raw_set(
        &mut self,
        table: &Table,
        key: &Owned,
        value: &Owned,
    ) -> ApiResult<()> {
        let handle = self.api_table(table)?;
        let key = key.raw(self)?;
        let value = value.raw(self)?;
        self.settle_atomic();
        if self.heap.update_string_key(handle, key, value) {
            return Ok(());
        }
        let normalized = self
            .heap
            .normalize_value(key)
            .map_err(|_| ApiError::InvalidKey)?;
        let result = self
            .heap
            .table_insert(handle, normalized.clone(), key, value);
        let result = if matches!(result, Err(crate::heap::InsertError::Memory))
            && !self.in_callback
            && self.collect_for_store()
        {
            self.heap.table_insert(handle, normalized, key, value)
        } else {
            result
        };
        result.map_err(|error| match error {
            crate::heap::InsertError::Memory => self.api_error(VmError::MemoryLimit),
            crate::heap::InsertError::NoTable => ApiError::Released.into(),
        })
    }

    pub(crate) fn api_raw_len(&self, table: &Table) -> ApiResult<i64> {
        let handle = self.api_table(table)?;
        Ok(self
            .heap
            .tables
            .get(handle)
            .ok_or(ApiError::Released)?
            .table
            .raw_border())
    }

    pub(crate) fn api_next(
        &mut self,
        table: &Table,
        key: Option<&Owned>,
    ) -> ApiResult<Option<(Owned, Owned)>> {
        let handle = self.api_table(table)?;
        let key = key.map(|key| key.raw(self)).transpose()?;
        self.settle_atomic();
        let key = key
            .map(|key| self.heap.key_view(key).map_err(|_| ApiError::InvalidKey))
            .transpose()?;
        let pair = self
            .heap
            .tables
            .get(handle)
            .ok_or(ApiError::Released)?
            .table
            .next_view(key)
            .map_err(|_| ApiError::InvalidKey)?;
        pair.map(|(key, value)| Ok((self.api_owned(key)?, self.api_owned(value)?)))
            .transpose()
    }

    pub(crate) fn api_metatable(&mut self, table: &Table) -> ApiResult<Option<Table>> {
        let handle = self.api_table(table)?;
        self.settle_atomic();
        let metatable = self
            .heap
            .tables
            .get(handle)
            .ok_or(ApiError::Released)?
            .metatable;
        metatable
            .map(|handle| match self.api_owned(Value::Table(handle))? {
                Owned::Table(table) => Ok(table),
                _ => Err(VmError::Corrupt.into()),
            })
            .transpose()
    }

    pub(crate) fn api_set_metatable(
        &mut self,
        table: &Table,
        metatable: Option<&Table>,
    ) -> ApiResult<()> {
        let handle = self.api_table(table)?;
        let metatable = metatable.map(|table| self.api_table(table)).transpose()?;
        self.settle_atomic();
        if self.heap.set_metatable(Value::Table(handle), metatable) {
            Ok(())
        } else {
            Err(ApiError::Released.into())
        }
    }

    pub(crate) fn api_function_kind(&self, function: &Function) -> ApiResult<FunctionKind> {
        Ok(match function.0.value(self.owner)? {
            Value::Closure(_) => FunctionKind::Lua,
            Value::NativeClosure(_) => FunctionKind::NativeClosure,
            Value::Native(index) => {
                let symbol = self
                    .heap
                    .natives
                    .get(index as usize)
                    .ok_or(ApiError::Released)?;
                let slot = self
                    .registry
                    .native_slot(symbol)
                    .ok_or(ApiError::UnknownSymbol)?;
                if self
                    .registry
                    .native(slot)
                    .ok_or(ApiError::UnknownSymbol)?
                    .builtin
                    .is_some()
                {
                    FunctionKind::Builtin
                } else {
                    FunctionKind::Native
                }
            }
            _ => return Err(ApiError::WrongType.into()),
        })
    }

    pub(crate) fn api_thread_status(&self, thread: &Thread) -> ApiResult<ThreadStatus> {
        let Value::Thread(handle) = thread.0.value(self.owner)? else {
            return Err(ApiError::WrongType.into());
        };
        Ok(
            match self
                .heap
                .threads
                .get(handle)
                .ok_or(ApiError::Released)?
                .status
            {
                Status::Ready => ThreadStatus::Ready,
                Status::LuaSuspended => ThreadStatus::Suspended,
                Status::Waiting => ThreadStatus::Waiting,
                Status::Completed => ThreadStatus::Completed,
                Status::Failed => ThreadStatus::Failed,
            },
        )
    }

    pub(crate) fn api_userdata<'a, T: crate::HostUserdata>(
        &'a self,
        userdata: &AnyUserData,
    ) -> ApiResult<&'a T> {
        let handle = self.api_userdata_handle(userdata)?;
        self.api_userdata_at(handle)
    }

    fn api_userdata_handle(
        &self,
        userdata: &AnyUserData,
    ) -> ApiResult<Handle<crate::heap::UserdataObj>> {
        let Value::Userdata(hint) = userdata.0.value(self.owner)? else {
            return Err(ApiError::WrongType.into());
        };
        self.heap
            .userdata
            .find_id_hint(userdata.id(), hint)
            .ok_or_else(|| ApiError::Released.into())
    }

    pub(crate) fn api_userdata_at<T: crate::HostUserdata>(
        &self,
        handle: Handle<crate::heap::UserdataObj>,
    ) -> ApiResult<&T> {
        self.heap
            .userdata
            .get(handle)
            .and_then(|object| object.payload.host::<T>())
            .ok_or_else(|| ApiError::WrongType.into())
    }

    pub(crate) fn api_userdata_at_mut<T: crate::HostUserdata>(
        &mut self,
        handle: Handle<crate::heap::UserdataObj>,
    ) -> ApiResult<&mut T> {
        self.heap
            .userdata
            .get_mut_storing(handle, false)
            .and_then(|object| object.payload.host_mut::<T>())
            .ok_or_else(|| ApiError::WrongType.into())
    }

    pub(crate) fn api_userdata_recharge<T: crate::HostUserdata>(
        &mut self,
        handle: Handle<crate::heap::UserdataObj>,
    ) {
        let object = self
            .heap
            .userdata
            .get_mut_storing(handle, false)
            .expect("borrowed userdata");
        let size = object
            .payload
            .host::<T>()
            .expect("borrowed userdata type")
            .logical_size();
        let old = std::mem::replace(&mut object.charge, size);
        if size > old {
            self.heap.gc.charge(size - old);
        } else {
            self.heap.give_back(old - size);
        }
    }

    pub(crate) fn api_userdata_mut<'a, T: crate::HostUserdata>(
        &'a mut self,
        userdata: &AnyUserData,
    ) -> ApiResult<UserDataRefMut<'a, T>> {
        // Reject foreign roots before changing any collector state.
        userdata.0.value(self.owner)?;
        self.settle_atomic();
        let handle = self.api_userdata_handle(userdata)?;
        self.api_userdata_at_mut::<T>(handle)?;
        Ok(UserDataRefMut {
            runtime: self,
            handle,
            marker: std::marker::PhantomData,
        })
    }
}

impl Runtime {
    pub(crate) fn apply_capabilities(&mut self, capabilities: &crate::HostCapabilities) {
        self.host_capabilities = capabilities.clone();
        self.output = capabilities.output.clone().map(|sink| {
            Box::new(move |bytes: &[u8]| {
                if let Ok(mut sink) = sink.try_borrow_mut() {
                    sink(bytes);
                }
            }) as crate::host::Output
        });
        self.warnings = capabilities.warnings.clone().map(|sink| {
            Box::new(move |bytes: &[u8], continuation| {
                if let Ok(mut sink) = sink.try_borrow_mut() {
                    sink(bytes, continuation);
                }
            }) as crate::host::Warnings
        });
        self.entropy = capabilities.entropy.clone().map(|source| {
            Box::new(move || source.try_borrow_mut().map_or(0, |mut source| source()))
                as crate::host::Entropy
        });
    }

    /// Resources supplied by the host, never serialized into the runtime.
    pub fn host_env(&self) -> &crate::HostEnv {
        self.host_capabilities.env()
    }
}
