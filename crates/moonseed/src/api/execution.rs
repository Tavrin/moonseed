//! Host execution shares ordinary call windows and VM continuation frames.
use super::*;
use crate::api::{
    ApiError, CallOutcome, Completion, Error, FromLuaMulti, Function, IntoLuaMulti, LuaError,
    MultiValue, NativeContext, NativeReturn, Result as ApiResult, Resume, ResumeOutcome,
    Value as Owned, WaitInfo,
};
use std::rc::Rc;

impl Runtime {
    pub(crate) fn take_owned_buffer(&mut self) -> Vec<Owned> {
        let index = (0..self.native_owned_buffers.len())
            .max_by_key(|&i| self.native_owned_buffers[i].capacity())
            .unwrap();
        std::mem::take(&mut self.native_owned_buffers[index])
    }

    pub(crate) fn recycle_owned_buffer(&mut self, mut values: Vec<Owned>) {
        values.clear(); // Drop roots before caching host storage.
        let index = (0..self.native_owned_buffers.len())
            .min_by_key(|&i| self.native_owned_buffers[i].capacity())
            .unwrap();
        if values.capacity() > self.native_owned_buffers[index].capacity() {
            self.native_owned_buffers[index] = values;
        }
    }

    /// Make a registered host native closure. Captures are snapshot state;
    /// builtin closure shapes are reserved for the VM.
    pub fn make_closure(
        &mut self,
        symbol: &str,
        captures: impl IntoLuaMulti,
    ) -> ApiResult<Function> {
        let slot = self
            .registry
            .native_slot(symbol)
            .ok_or(ApiError::UnknownSymbol)?;
        if self
            .registry
            .native(slot)
            .is_none_or(|entry| entry.builtin.is_some())
        {
            return Err(ApiError::UnknownSymbol.into());
        }
        let captures = captures.into_lua_multi(self)?;
        if captures.len() > 255 {
            return Err(ApiError::InvalidCallState.into());
        }
        let values = captures
            .iter()
            .map(|v| v.raw(self))
            .collect::<ApiResult<Vec<_>>>()?;
        let Value::Native(native) = self.native_value(symbol)? else {
            return Err(VmError::Corrupt.into());
        };
        if !self.in_callback {
            self.make_room(1, crate::heap::native_closure_cost(values.len(), 0));
        }
        let handle = self
            .alloc_native_closure(native, values, Vec::new())
            .map_err(|e| self.api_error(e))?;
        let Owned::Function(function) = self.api_owned(Value::NativeClosure(handle))? else {
            return Err(VmError::Corrupt.into());
        };
        Ok(function)
    }

    /// Create a rooted full userdata holding a registered Rust payload.
    /// With automatic collection on, host allocations collect before
    /// refusing for lack of room. Callback borrows defer collection.
    pub fn create_host_userdata<T: crate::HostUserdata>(
        &mut self,
        value: T,
        user_values: usize,
    ) -> ApiResult<crate::AnyUserData> {
        if !self.registry.has_userdata_type::<T>() {
            return Err(ApiError::Unregistered.into());
        }
        let size = value.logical_size();
        self.settle_atomic();
        if !self.in_callback {
            self.make_room(1, crate::heap::userdata_cost(user_values, size));
        }
        let handle = self
            .heap
            .alloc_userdata(self.max_objects, user_values, size, || {
                crate::userdata::Payload::Host {
                    symbol: T::SYMBOL,
                    value: Box::new(value),
                }
            })
            .map_err(|e| self.api_error(VmError::from(e)))?;
        let Owned::UserData(value) = self.api_owned(Value::Userdata(handle))? else {
            return Err(VmError::Corrupt.into());
        };
        Ok(value)
    }

    pub(crate) fn api_value_metatable(&mut self, value: &Owned) -> ApiResult<Option<crate::Table>> {
        let raw = value.raw(self)?;
        let Some(meta) = self.heap.metatable_of(raw) else {
            return Ok(None);
        };
        match self.api_owned(Value::Table(meta))? {
            Owned::Table(table) => Ok(Some(table)),
            _ => Err(VmError::Corrupt.into()),
        }
    }

    pub(crate) fn api_value_set_metatable(
        &mut self,
        value: &Owned,
        meta: Option<&crate::Table>,
    ) -> ApiResult<()> {
        let raw = value.raw(self)?;
        let meta = meta
            .map(|table| Owned::Table(table.clone()).raw(self))
            .transpose()?;
        let meta = match meta {
            Some(Value::Table(table)) => Some(table),
            None => None,
            _ => return Err(ApiError::WrongType.into()),
        };
        if self.heap.set_metatable(raw, meta) {
            Ok(())
        } else {
            Err(ApiError::WrongType.into())
        }
    }

    pub(crate) fn api_captures(&self, callee: Value) -> &[Value] {
        match callee {
            Value::NativeClosure(handle) => self
                .heap
                .native_closures
                .get(handle)
                .map_or(&[], |c| c.values.as_slice()),
            _ => &[],
        }
    }

    pub(crate) fn api_set_capture(
        &mut self,
        callee: Value,
        index: usize,
        value: &Owned,
    ) -> ApiResult<()> {
        let raw = value.raw(self)?;
        let Value::NativeClosure(handle) = callee else {
            return Err(ApiError::InvalidCallState.into());
        };
        let slot = self
            .heap
            .native_closures
            .get_mut(handle)
            .and_then(|c| c.values.get_mut(index))
            .ok_or(ApiError::InvalidCallState)?;
        *slot = raw;
        Ok(())
    }

    /// Inspect a pending host operation. Returned object values are rooted
    /// in this runtime. Legacy integer waits have no operation payload.
    pub fn wait(&self, key: WaitKey) -> Option<WaitInfo> {
        let thread = *self.wait_index.get(&key.raw())?;
        let request = self
            .heap
            .threads
            .get(thread)?
            .frames
            .last()?
            .wait_request()?;
        let payload = request
            .payload
            .iter()
            .map(|v| Owned::wrap(*v, &self.heap, self.owner, self.heap.api_roots.as_ref()))
            .collect::<ApiResult<Vec<_>>>()
            .ok()?;
        Some(WaitInfo {
            operation: request.operation.clone(),
            payload: MultiValue(payload),
        })
    }

    /// Complete a pending wait once, checking every value's ownership.
    /// Unknown keys are `NotWaiting`; every completed VM-issued key remains
    /// `AlreadyCompleted`, including after another wait and after restore.
    pub fn complete(&mut self, key: WaitKey, completion: Completion) -> ApiResult<()> {
        if self.completed_waits.contains(&key.raw()) || self.last_completed_wait == Some(key.raw())
        {
            return Err(ApiError::AlreadyCompleted.into());
        }
        if !self.wait_index.contains_key(&key.raw()) {
            return Err(ApiError::NotWaiting.into());
        }
        if let Some(thread) = self.wait_index.get(&key.raw())
            && self
                .heap
                .threads
                .get(*thread)
                .and_then(|t| t.frames.last())
                .is_some_and(|f| matches!(f.pending(), Some(Pending::Capability { .. })))
        {
            return Err(ApiError::InvalidCallState.into());
        }
        let (values, error) = match completion {
            Completion::Return(values) => (values, false),
            Completion::Error(value) => (vec![value], true),
        };
        let raw = values
            .iter()
            .map(|v| v.raw(self))
            .collect::<ApiResult<Vec<_>>>()?;
        self.finish_wait_raw(key, &raw, error)
            .map_err(|e| match e {
                WaitError::AlreadyCompleted => ApiError::AlreadyCompleted,
                _ => ApiError::NotWaiting,
            })?;
        Ok(())
    }

    pub(super) fn index_wait(
        &mut self,
        key: u64,
        thread: Handle<ThreadObj>,
    ) -> Result<(), VmError> {
        if self.wait_index.contains_key(&key) {
            return Err(VmError::Api(ApiError::InvalidCallState));
        }
        let vm_allocated = self
            .heap
            .threads
            .get(thread)
            .and_then(|object| object.frames.last())
            .is_some_and(|frame| frame.wait_request().is_some());
        let root = self.api_owned(Value::Thread(thread)).map_err(api_vm)?;
        self.wait_index.insert(key, thread);
        self.wait_roots.insert(key, (root, vm_allocated));
        Ok(())
    }

    pub(super) fn wait_finished(&mut self, key: WaitKey) {
        self.wait_index.remove(&key.raw());
        if self
            .wait_roots
            .remove(&key.raw())
            .is_some_and(|(_, vm_allocated)| vm_allocated)
        {
            self.completed_waits.insert(key.raw());
        }
        self.last_completed_wait = Some(key.raw());
    }

    pub(super) fn clear_wait_request(&mut self, thread: Handle<ThreadObj>) {
        if let Some(object) = self.heap.threads.get_mut(thread)
            && let Some(wait) = object
                .frames
                .last_mut()
                .and_then(|f| f.take_wait_request(&mut self.cold_spare))
        {
            let bytes = wait.operation.len() as u64 + cost::STACK_SLOT * wait.payload.len() as u64;
            object.charged_held = object.charged_held.saturating_sub(bytes);
            self.heap.give_back(bytes);
        }
    }

    pub(crate) fn api_argument_error(&mut self, _callee: Value, error: Error) -> Error {
        let Error::Api(ApiError::Conversion(error)) = error else {
            return error;
        };
        let Some(active) = self.heap.active else {
            return VmError::Corrupt.into();
        };
        let Ok((func, _, passed, _)) = self.call_site(active) else {
            return VmError::Corrupt.into();
        };
        let ctx = library::Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let (name, method) = self.argument_name(&ctx);
        let mut text = self.where_prefix(active);
        text.extend(library::arg_error_text(
            &name,
            method,
            error.position.unwrap_or(1).saturating_sub(1) as u32,
            format!(
                "{} expected, got {}",
                lua_expected(error.expected),
                crate::api::error::type_name(error.actual)
            )
            .as_bytes(),
        ));
        match self
            .create_string(text)
            .and_then(|value| LuaError::new(Owned::String(value), LuaFault::Argument, self))
        {
            Ok(error) => Error::Lua(error),
            Err(error) => error,
        }
    }

    pub(crate) fn api_bad_argument(
        &mut self,
        index: usize,
        detail: &[u8],
    ) -> crate::Result<LuaError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = library::Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let (name, method) = self.argument_name(&ctx);
        let mut text = self.where_prefix(active);
        text.extend(library::arg_error_text(&name, method, index as u32, detail));
        let value = self.create_string(text)?;
        LuaError::new(Owned::String(value), LuaFault::Argument, self)
    }

    pub(super) fn callback_error(&mut self, error: Error) -> Result<Poll, VmError> {
        match error {
            Error::Lua(error) => {
                let raw = error.value.raw(self).map_err(api_vm)?;
                let active = self.heap.active.ok_or(VmError::Corrupt)?;
                self.throw_on(active, error.class, raw);
                Ok(Poll::Continue)
            }
            other => Err(api_vm(other)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub(super) fn run_typed(
        &mut self,
        active: Handle<ThreadObj>,
        callee: Value,
        func: u32,
        wants: u8,
        passed: u32,
        callback: Rc<crate::api::native::TypedCallback>,
        effect: Option<EffectId>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let mut args = std::mem::take(&mut self.native_args);
        args.clear();
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        args.extend((0..passed).map(|i| {
            thread
                .stack
                .get((func + 1 + i) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        }));
        self.api_roots();
        self.native_results.clear();
        self.in_callback = true;
        let result = callback(&mut NativeContext {
            runtime: self,
            args: &args,
            callee,
            effect,
            journal: effect.map(|_| &mut *journal),
            resume: None,
        });
        self.in_callback = false;
        args.clear();
        self.native_args = args;
        if let Err(error) = result {
            self.native_results.clear();
            return self.callback_error(error);
        }
        let mut raw = std::mem::take(&mut self.native_results);
        let result = self.native_returned(active, func, wants, passed, &raw);
        raw.clear();
        self.native_results = raw;
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_callback(
        &mut self,
        active: Handle<ThreadObj>,
        callee: Value,
        func: u32,
        wants: u8,
        passed: u32,
        callback: Rc<crate::api::native::Callback>,
        effect: Option<EffectId>,
        resume: Option<Resume>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let Value::Native(symbol) = self.callable(callee) else {
            return Err(VmError::Corrupt);
        };
        let mut args = std::mem::take(&mut self.native_args);
        args.clear();
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        args.extend((0..passed).map(|i| {
            thread
                .stack
                .get((func + 1 + i) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        }));
        // Root support is initialized once, rather than once per argument.
        self.api_roots();
        let resuming = resume.is_some();
        self.in_callback = true;
        let outcome = callback(&mut NativeContext {
            runtime: self,
            args: &args,
            callee,
            effect,
            journal: effect.map(|_| &mut *journal),
            resume: resume.as_ref(),
        });
        self.in_callback = false;
        if let Some(resume) = resume {
            self.recycle_owned_buffer(resume.kept.into_vec());
            if let ResumeOutcome::Returned(values) = resume.outcome {
                self.recycle_owned_buffer(values.into_vec());
            }
        }
        args.clear();
        self.native_args = args;
        if resuming {
            let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let mut frame = object.frames.pop().ok_or(VmError::Corrupt)?;
            frame.recycle_cold(&mut self.cold_spare);
            object.stack.truncate((func + 1 + passed) as usize);
            object.top = func + 1 + passed;
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return self.callback_error(error),
        };
        match outcome {
            NativeReturn::Return(values) => {
                let mut raw = std::mem::take(&mut self.native_results);
                raw.clear();
                for value in &values.0 {
                    raw.push(value.raw(self).map_err(api_vm)?);
                }
                let result = self.native_returned(active, func, wants, passed, &raw);
                raw.clear();
                self.native_results = raw;
                self.recycle_owned_buffer(values.into_vec());
                result
            }
            NativeReturn::Error(error) => {
                let error = error.raw(self).map_err(api_vm)?;
                self.throw_on(active, LuaFault::Error, error);
                Ok(Poll::Continue)
            }
            NativeReturn::Wait(request) => {
                if request.operation.len() > 4096
                    || request.payload.len() as u64 > u64::from(self.max_stack_slots)
                {
                    return Err(VmError::Api(ApiError::InvalidCallState));
                }
                let payload = request
                    .payload
                    .iter()
                    .map(|v| v.raw(self))
                    .collect::<ApiResult<Vec<_>>>()
                    .map_err(api_vm)?;
                let bytes =
                    request.operation.len() as u64 + cost::STACK_SLOT * payload.len() as u64;
                self.ensure_room(bytes)?;
                let key = loop {
                    let sequence = self.next_sequence;
                    if sequence >= (1 << 63) {
                        return Err(VmError::Api(ApiError::InvalidCallState));
                    }
                    self.next_sequence += 1;
                    let key = sequence | (1 << 63);
                    if !self.wait_index.contains_key(&key) && !self.completed_waits.contains(&key) {
                        break key;
                    }
                };
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                let sequence = effect.map(|id| id.sequence);
                if let Some(meta) = frame.meta_mut() {
                    meta.phase = MetaPhase::NativeWaiting {
                        sequence,
                        wait_key: key,
                    };
                } else {
                    frame.set_pending(
                        Some(Pending::NativeWaiting {
                            sequence,
                            wait_key: key,
                        }),
                        &mut self.cold_spare,
                    );
                }
                frame.set_wait_request(
                    Some(crate::heap::HostWait {
                        operation: request.operation,
                        payload,
                    }),
                    &mut self.cold_spare,
                );
                object.charged_held += bytes;
                self.heap.gc.charge(bytes);
                count!("host_waits");
                object.status = Status::Waiting;
                self.index_wait(key, active)?;
                self.last_completed_wait = None;
                Ok(Poll::Continue)
            }
            NativeReturn::CallLua {
                function,
                args,
                tag,
                keep,
            } => {
                let function = function.raw(self).map_err(api_vm)?;
                // Validate before mutating the frame. Rooted owned values stay
                // alive until every raw value has reached the Lua stack.
                for value in args.iter().chain(keep.iter()) {
                    value.check(self).map_err(api_vm)?;
                }
                let kept = u32::try_from(keep.len()).map_err(|_| VmError::StackLimit)?;
                let call_slot = func
                    .checked_add(1)
                    .and_then(|n| n.checked_add(passed))
                    .and_then(|n| n.checked_add(kept))
                    .ok_or(VmError::StackLimit)?;
                if let Some(fault) = self.depth_fault(active)? {
                    return Ok(self.fault(fault));
                }
                let nargs = u32::try_from(args.len()).map_err(|_| VmError::StackLimit)?;
                let end = call_slot
                    .checked_add(1)
                    .and_then(|n| n.checked_add(nargs))
                    .ok_or(VmError::StackLimit)?;
                if let Some(fault) = self.slot_fault(active, end)? {
                    return Ok(self.fault(fault));
                }
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let caller = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                caller.set_pending(None, &mut self.cold_spare);
                if let Some(meta) = caller.meta_mut() {
                    meta.phase = MetaPhase::Running;
                }
                let advance_caller = caller.boundary().is_none() && caller.meta().is_none();
                let closure = caller.closure;
                count!("frame_pushes");
                object.frames.push(Frame {
                    closure,
                    pc: 0,
                    base: func + 1,
                    limit: func + 1,
                    nresults: wants,
                    vararg_len: 0,
                    flags: 0,
                    cold: FrameCold::with_boundary(
                        Boundary::Native {
                            func,
                            passed,
                            advance_caller,
                            symbol,
                            tag,
                            kept,
                            sequence: effect.map(|id| id.sequence),
                            error: None,
                            resuming: false,
                        },
                        &mut self.cold_spare,
                    ),
                });
                for (i, value) in keep.iter().enumerate() {
                    self.write_abs(
                        active,
                        func + 1 + passed + i as u32,
                        value.raw(self).map_err(api_vm)?,
                    )?;
                }
                self.write_abs(active, call_slot, function)?;
                for (i, value) in args.iter().enumerate() {
                    self.write_abs(
                        active,
                        call_slot + 1 + i as u32,
                        value.raw(self).map_err(api_vm)?,
                    )?;
                }
                self.recycle_owned_buffer(args.into_vec());
                self.recycle_owned_buffer(keep.into_vec());
                let (function, nargs) = match self.resolve_callable(active, call_slot, nargs)? {
                    Ok(call) => call,
                    Err(fault) => return Ok(self.fault(fault)),
                };
                self.heap
                    .threads
                    .get_mut(active)
                    .ok_or(VmError::Corrupt)?
                    .top = call_slot + 1 + nargs;
                match self.callable(function) {
                    Value::Closure(closure) => {
                        self.push_lua_frame(closure, call_slot, nargs, COUNT_OPEN, false)
                    }
                    Value::Native(_) => self.defer_call(active),
                    _ => Err(VmError::Corrupt),
                }
            }
        }
    }

    pub(super) fn finish_native(
        &mut self,
        active: Handle<ThreadObj>,
        effect: Option<EffectId>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        let Some(Boundary::Native {
            func,
            passed,
            symbol,
            tag,
            kept,
            error,
            resuming,
            sequence,
            ..
        }) = frame.boundary()
        else {
            return Err(VmError::Corrupt);
        };
        let (func, passed, symbol, tag, kept, error, resuming, sequence, wants) = (
            *func,
            *passed,
            *symbol,
            *tag,
            *kept,
            *error,
            *resuming,
            *sequence,
            frame.nresults,
        );
        let callee = object
            .stack
            .get(func as usize)
            .copied()
            .ok_or(VmError::Corrupt)?;
        let slot = *self
            .native_slots
            .get(symbol as usize)
            .ok_or(VmError::Corrupt)?;
        let entry = self.registry.native(slot).ok_or(VmError::Corrupt)?;
        if entry.policy == NativePolicy::External && !resuming {
            let sequence = sequence.ok_or(VmError::Corrupt)?;
            let frame = self
                .heap
                .threads
                .get_mut(active)
                .and_then(|t| t.frames.last_mut())
                .ok_or(VmError::Corrupt)?;
            frame.set_pending(
                Some(Pending::NativePrepared { sequence }),
                &mut self.cold_spare,
            );
            if let Some(Boundary::Native { resuming, .. }) = frame.boundary_mut() {
                *resuming = true;
            }
            return Ok(Poll::Continue);
        }
        count!("host_continuations");
        count!("lua_to_host_calls");
        let callback = entry.callback.clone().ok_or(VmError::Corrupt)?;
        let call = func + 1 + passed + kept;
        let mut kept_values = self.take_owned_buffer();
        for index in func + 1 + passed..call {
            let raw = self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack[index as usize];
            kept_values.push(self.api_owned(raw).map_err(api_vm)?);
        }
        let kept = MultiValue(kept_values);
        let outcome = match error {
            Some((class, raw)) => {
                let value = self.api_owned(raw).map_err(api_vm)?;
                ResumeOutcome::Errored(LuaError::new(value, class, self).map_err(api_vm)?)
            }
            None => {
                let mut values = self.take_owned_buffer();
                let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
                let end = if object.top as usize <= object.stack.len() {
                    object.top
                } else {
                    call
                };
                for index in call..end {
                    let raw = self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack
                        [index as usize];
                    values.push(self.api_owned(raw).map_err(api_vm)?);
                }
                ResumeOutcome::Returned(MultiValue(values))
            }
        };
        self.run_callback(
            active,
            callee,
            func,
            wants,
            passed,
            callback,
            effect,
            Some(Resume { tag, outcome, kept }),
            journal,
        )
    }

    fn idle_main(&self) -> ApiResult<()> {
        if self.in_callback {
            return Err(ApiError::Busy.into());
        }
        if self.callback_failed {
            return Err(ApiError::InvalidCallState.into());
        }
        if self.trap.is_some() || self.is_closing() || self.heap.finalizers.exit.is_some() {
            return Err(ApiError::InvalidCallState.into());
        }
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        if self.host_call
            || !matches!(thread.status, Status::Completed | Status::Failed)
            || thread.closing
            || self.heap.finalizers.running
        {
            return Err(ApiError::Busy.into());
        }
        Ok(())
    }

    /// Install a call on the idle main thread. Drive with `run`, then
    /// consume its exact results or Lua error through `finish_call`.
    pub fn start_call(&mut self, function: &Function, args: impl IntoLuaMulti) -> ApiResult<()> {
        #[cfg(feature = "counters")]
        let _scope = self.counter_scope();
        count!("host_to_lua_calls");
        self.idle_main()?;
        let function = Owned::Function(function.clone());
        function.check(self)?;
        let args = args.into_lua_multi(self)?;
        let mut values = args.into_vec();
        if values.capacity() == 0 {
            values = self.take_owned_buffer();
        }
        values.insert(0, function);
        self.start_main_ops(
            &[
                Op::Call {
                    func: 0,
                    nargs: COUNT_OPEN,
                    nresults: COUNT_OPEN,
                },
                Op::Return {
                    base: 0,
                    count: COUNT_OPEN,
                },
            ],
            1,
            values,
        )
    }

    fn start_main_ops(&mut self, ops: &[Op], registers: u8, values: Vec<Owned>) -> ApiResult<()> {
        self.idle_main()?;
        if values.len() as u64 > u64::from(self.max_stack_slots - self.max_stack_slots / 8) {
            return Err(self.api_error(VmError::StackLimit));
        }
        for value in &values {
            value.check(self)?;
        }
        self.settle_atomic();
        let is_call = matches!(
            ops,
            [
                Op::Call {
                    func: 0,
                    nargs: COUNT_OPEN,
                    nresults: COUNT_OPEN
                },
                Op::Return {
                    base: 0,
                    count: COUNT_OPEN
                }
            ]
        );
        // This is a weak cache, not a root or canonical state. Reject objects
        // already marked dead, even if the sweep has not freed their slots.
        let cached = if is_call {
            self.main_call_closure
                .filter(|handle| {
                    self.heap
                        .closures
                        .get(*handle)
                        .is_some_and(|closure| self.heap.closures.find_id(closure.id).is_some())
                })
                .or_else(|| {
                    // A restored heap may already contain the trampoline. Recover
                    // it without adding snapshot fields or retaining extra roots.
                    self.heap
                        .closures
                        .iter()
                        .find_map(|(index, generation, closure)| {
                            let proto = self.heap.protos.get(closure.proto)?;
                            (closure.upvalues.is_empty()
                                && proto.ops == ops
                                && proto.max_reg == registers
                                && proto.params == 0
                                && !proto.vararg
                                && proto.byte_consts.is_empty()
                                && proto.captures.is_empty()
                                && proto.children.is_empty()
                                && proto.debug.is_none()
                                && proto.source.is_none())
                            .then_some(Handle::new(index, generation))
                        })
                })
        } else {
            None
        };
        let closure = if let Some(closure) = cached {
            closure
        } else {
            let proto = ProtoSpec {
                ops: ops.to_vec(),
                byte_consts: Vec::new(),
                captures: Vec::new(),
                children: Vec::new(),
                max_reg: registers,
                params: 0,
                vararg: false,
                debug: None,
            };
            let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
            self.instantiate(&proto, Value::Table(globals), ChunkName::Unnamed)
                .map_err(|error| self.api_error(error))?
        };
        if is_call {
            self.main_call_closure = Some(closure);
        }
        self.load_main_closure(closure, u32::from(registers))?;
        let active = self.heap.entry.ok_or(VmError::Corrupt)?;
        if let Some(fault) = self.slot_fault(active, values.len() as u32)? {
            let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            object.frames.clear();
            object.status = Status::Completed;
            object.top = 0;
            return Err(self.api_error(match fault {
                LuaFault::Memory => VmError::MemoryLimit,
                _ => VmError::StackLimit,
            }));
        }
        for (i, value) in values.iter().enumerate() {
            self.write_abs(active, i as u32, value.raw(self)?)?;
        }
        self.heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .top = values.len() as u32;
        self.recycle_owned_buffer(values);
        self.host_call = true;
        Ok(())
    }

    /// Consume a completed host call. A Lua failure retains its original
    /// error object; an unfinished call is `InvalidCallState`.
    pub fn finish_call<R: FromLuaMulti>(&mut self) -> ApiResult<R> {
        if self.callback_failed || !self.host_call || self.heap.finalizers.exit.is_some() {
            return Err(ApiError::InvalidCallState.into());
        }
        let entry = self.heap.entry.ok_or(VmError::Corrupt)?;
        let object = self.heap.threads.get(entry).ok_or(VmError::Corrupt)?;
        match object.status {
            Status::Completed => {
                let len = object.host_results.len();
                let mut values = self.take_owned_buffer();
                for index in 0..len {
                    let raw = self
                        .heap
                        .threads
                        .get(entry)
                        .ok_or(VmError::Corrupt)?
                        .host_results[index];
                    values.push(self.api_owned(raw)?);
                }
                let values = MultiValue(values);
                self.host_call = false;
                R::from_lua_multi(values, self)
            }
            Status::Failed => {
                let (class, raw) = object.error.ok_or(VmError::Corrupt)?;
                let value = self.api_owned(raw)?;
                self.host_call = false;
                Err(LuaError::new(value, class, self)?.into())
            }
            _ => Err(ApiError::InvalidCallState.into()),
        }
    }

    /// Start and drive a call for at most `fuel` execution units. This
    /// never blocks on a host operation and allocates no Lua thread.
    pub fn call<R: FromLuaMulti>(
        &mut self,
        function: &Function,
        args: impl IntoLuaMulti,
        journal: &mut Journal,
        fuel: u64,
    ) -> ApiResult<CallOutcome<R>> {
        self.start_call(function, args)?;
        self.drive_call(journal, fuel)
    }

    fn drive_call<R: FromLuaMulti>(
        &mut self,
        journal: &mut Journal,
        fuel: u64,
    ) -> ApiResult<CallOutcome<R>> {
        match self.run(fuel, journal).map_err(Error::from)? {
            StepOutcome::Completed | StepOutcome::LuaError(_) => {
                self.finish_call().map(CallOutcome::Done)
            }
            StepOutcome::ExitRequested { status, close } => {
                self.host_call = false;
                Ok(CallOutcome::ExitRequested { status, close })
            }
            StepOutcome::Waiting(key) => Ok(CallOutcome::Waiting(key)),
            StepOutcome::LuaYielded => Err(ApiError::InvalidCallState.into()),
            StepOutcome::Paused(_) => Ok(CallOutcome::OutOfFuel),
            StepOutcome::Terminated(reason) => Err(VmError::from(reason).into()),
        }
    }

    pub(crate) fn api_table_get<R: FromLuaMulti>(
        &mut self,
        table: &crate::Table,
        key: Owned,
        journal: &mut Journal,
        fuel: u64,
    ) -> ApiResult<CallOutcome<R>> {
        self.start_main_ops(
            &[
                Op::Index {
                    dst: 0,
                    obj: 0,
                    key: 1,
                },
                Op::Return { base: 0, count: 1 },
            ],
            2,
            vec![Owned::Table(table.clone()), key],
        )?;
        self.drive_call(journal, fuel)
    }
    pub(crate) fn api_table_set(
        &mut self,
        table: &crate::Table,
        key: Owned,
        value: Owned,
        journal: &mut Journal,
        fuel: u64,
    ) -> ApiResult<CallOutcome<()>> {
        self.start_main_ops(
            &[
                Op::SetIndex {
                    obj: 0,
                    key: 1,
                    src: 2,
                },
                Op::Return { base: 0, count: 0 },
            ],
            3,
            vec![Owned::Table(table.clone()), key, value],
        )?;
        self.drive_call(journal, fuel)
    }
}

pub(super) fn api_vm(error: Error) -> VmError {
    match error {
        Error::Api(error) => VmError::Api(error),
        Error::Vm(error) => error,
        Error::Lua(_) => VmError::Corrupt,
    }
}
fn lua_expected(expected: &str) -> &str {
    match expected {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => "number",
        "Function" => "function",
        "Table" => "table",
        "LuaString" => "string",
        "Thread" => "thread",
        "AnyUserData" => "userdata",
        other => other,
    }
}
