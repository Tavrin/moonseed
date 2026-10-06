//! The `coroutine` library (ADR 0041).
//!
//! A coroutine is a thread object. `coroutine.create` makes one with no
//! frames and its body in stack slot 0. `resume` links the coroutine to
//! the running thread (`resumed_by`), makes it the running thread, and
//! leaves the resumer's frame on its call; `yield`, a return from the
//! coroutine's last frame, an error nothing in it catches, and the end of
//! a close each give control back. How the resumer receives the outcome
//! is read from the call it is making: `coroutine.resume`, a
//! `coroutine.wrap` function, `coroutine.close`, or, for hand-built
//! bytecode, the `Resume` and `CloseThread` instructions' pending state.
//!
//! A coroutine suspended by `coroutine.yield` stays on that call; the next
//! resume's arguments become its results, as any builtin's are.

use super::library::{Ctx, Next};
use super::*;
use crate::corolib::{COROUTINE_FUNCTIONS, CoFn, MAX_RESUME_DEPTH, WRAP_CALL};

/// How a resumer waits for the coroutine it resumed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParentWait {
    /// The `Resume` or `CloseThread` instruction (hand-built bytecode).
    Op,
    Resume,
    Wrap,
    Close,
}

/// The callee slot, wanted result count, and passed argument count.
type CallWindow = (u32, u8, u32);

/// The trampoline a coroutine whose body is not a Lua function starts in:
/// a vararg function called with the body and the arguments, which calls
/// the body and returns what it returns. Debug levels skip it.
fn trampoline_ops() -> [Op; 3] {
    [
        Op::Vararg {
            dst: 0,
            count: COUNT_OPEN,
        },
        Op::Call {
            func: 0,
            nargs: COUNT_OPEN,
            nresults: COUNT_OPEN,
        },
        Op::Return {
            base: 0,
            count: COUNT_OPEN,
        },
    ]
}

/// Whether `proto` is a coroutine trampoline.
pub(super) fn is_trampoline(proto: &Proto) -> bool {
    proto.debug.is_none()
        && proto.vararg
        && proto.params == 0
        && proto.captures.is_empty()
        && proto.ops == trampoline_ops()
}

impl Runtime {
    /// Install `coroutine` (ADR 0041). The registry must have the
    /// functions (see [`crate::register_coroutine`]).
    pub fn install_coroutine(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("coroutine")?;
        self.register_module("coroutine", table)?;
        for (name, symbol, _) in COROUTINE_FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        Ok(())
    }

    /// A `coroutine` function, called from the active frame's call site.
    pub(super) fn call_coroutine(
        &mut self,
        active: Handle<ThreadObj>,
        function: CoFn,
        site: (u32, u8, u32, Value),
    ) -> Result<Poll, VmError> {
        let (func, _, passed, callee) = site;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let thread_arg = |runtime: &Self| match runtime.lib_arg(&ctx, 0) {
            Value::Thread(thread) => Ok(thread),
            _ => Err(runtime.bad_type(&ctx, 0, "thread")),
        };
        let next = match function {
            CoFn::Create | CoFn::Wrap => {
                let body = self.lib_arg(&ctx, 0);
                if !body.is_function() {
                    self.bad_type(&ctx, 0, "function")
                } else {
                    let co = self.new_coroutine(body)?;
                    if function == CoFn::Create {
                        Next::Done(vec![Value::Thread(co)])
                    } else {
                        let Value::Native(native) = self.native_value(WRAP_CALL)? else {
                            return Err(VmError::Corrupt);
                        };
                        let closure =
                            self.alloc_native_closure(native, vec![Value::Thread(co)], Vec::new())?;
                        Next::Done(vec![Value::NativeClosure(closure)])
                    }
                }
            }
            CoFn::Status => match thread_arg(self) {
                Ok(co) => {
                    let text = self.co_status(active, co)?;
                    Next::Done(vec![Value::String(
                        self.alloc_string(text.as_bytes().to_vec())?,
                    )])
                }
                Err(next) => next,
            },
            CoFn::Running => {
                let main = self.heap.entry == Some(active);
                Next::Done(vec![Value::Thread(active), Value::Bool(main)])
            }
            CoFn::IsYieldable => {
                let target = if passed == 0 {
                    Ok(active)
                } else {
                    thread_arg(self)
                };
                match target {
                    Ok(co) => Next::Done(vec![Value::Bool(self.co_yieldable(co)?)]),
                    Err(next) => next,
                }
            }
            CoFn::Resume => match thread_arg(self) {
                Ok(co) => return self.co_resume(&ctx, co, ParentWait::Resume, 1),
                Err(next) => next,
            },
            CoFn::WrapCall => {
                let Value::NativeClosure(closure) = callee else {
                    return Err(VmError::Corrupt);
                };
                let Some(Value::Thread(co)) = self
                    .heap
                    .native_closures
                    .get(closure)
                    .and_then(|object| object.values.first())
                    .copied()
                else {
                    return Err(VmError::Corrupt);
                };
                return self.co_resume(&ctx, co, ParentWait::Wrap, 0);
            }
            CoFn::Yield => return self.co_yield(active, func, passed),
            CoFn::Close => match thread_arg(self) {
                Ok(co) => return self.co_close(active, co),
                Err(next) => next,
            },
        };
        match next {
            Next::Done(values) => self.base_return(active, &values),
            next => Ok(self.finish_next(active, next)),
        }
    }

    /// A new coroutine: suspended, no frames, `body` in slot 0.
    fn new_coroutine(&mut self, body: Value) -> Result<Handle<ThreadObj>, VmError> {
        let inherited = self.inherited_hook();
        let hook_bytes = if inherited.is_some() {
            super::hooks::HOOK_BYTES
        } else {
            0
        };
        self.ensure_room(cost::THREAD + cost::STACK_SLOT + hook_bytes)?;
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        let thread = ThreadObj {
            id,
            status: Status::LuaSuspended,
            stack: vec![body].into(),
            top: 1,
            frames: Default::default(),
            open_upvalues: Vec::new(),
            open_above: 0,
            resumed_by: None,
            host_results: Vec::new(),
            unwind: None,
            error: None,
            coroutine: true,
            closing: false,
            tbc: Vec::new(),
            charged_slots: 1,
            charged_held: 0,
        };
        self.heap.gc.charge(cost::THREAD + cost::STACK_SLOT);
        let owner = self.heap.threads.alloc(thread).map_err(VmError::from)?;
        self.install_inherited_hook(owner, inherited)?;
        Ok(owner)
    }

    /// Lua's `coroutine.status`, from the thread states: the running
    /// thread, a finished or failed one, one suspended (new or yielded),
    /// and one that resumed another and waits for it.
    pub(super) fn co_status(
        &self,
        active: Handle<ThreadObj>,
        co: Handle<ThreadObj>,
    ) -> Result<&'static str, VmError> {
        if co == active {
            return Ok("running");
        }
        Ok(
            match self.heap.threads.get(co).ok_or(VmError::Corrupt)?.status {
                Status::Completed | Status::Failed => "dead",
                Status::LuaSuspended => "suspended",
                Status::Ready | Status::Waiting => "normal",
            },
        )
    }

    /// Lua's `lua_isyieldable`: not the main thread, and nothing on its
    /// stack that no yield may cross.
    pub(super) fn co_yieldable(&self, co: Handle<ThreadObj>) -> Result<bool, VmError> {
        if self.heap.entry == Some(co) {
            return Ok(false);
        }
        let thread = self.heap.threads.get(co).ok_or(VmError::Corrupt)?;
        Ok(thread.coroutine && !thread.closing && !blocks_yield(thread))
    }

    /// The coroutines in the chain of resumes that ends at `thread`.
    fn resume_depth(&self, thread: Handle<ThreadObj>) -> usize {
        let mut depth = 0;
        let mut at = Some(thread);
        while let Some(current) = at {
            let Some(object) = self.heap.threads.get(current) else {
                break;
            };
            if self.heap.entry == Some(current) || depth > MAX_RESUME_DEPTH {
                break;
            }
            depth += 1;
            at = object.resumed_by;
        }
        depth
    }

    /// Raise `text` in `thread` as a `coroutine.wrap` function raises it:
    /// with the caller's position before a string, as `luaL_where` gives.
    fn wrap_raise(&mut self, thread: Handle<ThreadObj>, fault: LuaFault, error: Value) -> Poll {
        self.heap.active = Some(thread);
        self.refresh_hook_trap();
        let error = match error {
            Value::String(handle) if fault != LuaFault::Memory => {
                let mut text = self.where_prefix(thread);
                if text.is_empty() {
                    error
                } else {
                    text.extend_from_slice(self.heap.string_bytes(handle).unwrap_or_default());
                    match self.alloc_string(text) {
                        Ok(handle) => Value::String(handle),
                        Err(_) => return self.fault(LuaFault::Memory),
                    }
                }
            }
            error => error,
        };
        self.throw_on(thread, fault, error)
    }

    /// A resume that did not start: `false` and the message for
    /// `coroutine.resume`, the error for a `wrap` function.
    fn resume_refused(
        &mut self,
        active: Handle<ThreadObj>,
        how: ParentWait,
        fault: LuaFault,
        text: &str,
    ) -> Result<Poll, VmError> {
        let message = Value::String(self.alloc_string(text.as_bytes().to_vec())?);
        if how == ParentWait::Wrap {
            Ok(self.wrap_raise(active, fault, message))
        } else {
            self.base_return(active, &[Value::Bool(false), message])
        }
    }

    /// `coroutine.resume(co, ...)` and a `wrap` function's call: the
    /// arguments after the first `skip` go to the coroutine.
    fn co_resume(
        &mut self,
        ctx: &Ctx,
        co: Handle<ThreadObj>,
        how: ParentWait,
        skip: u32,
    ) -> Result<Poll, VmError> {
        let active = ctx.active;
        let status = self.heap.threads.get(co).ok_or(VmError::Corrupt)?.status;
        if co == active
            || !matches!(
                status,
                Status::LuaSuspended | Status::Completed | Status::Failed
            )
        {
            return self.resume_refused(
                active,
                how,
                LuaFault::ResumeState,
                "cannot resume non-suspended coroutine",
            );
        }
        if status != Status::LuaSuspended {
            return self.resume_refused(
                active,
                how,
                LuaFault::ResumeState,
                "cannot resume dead coroutine",
            );
        }
        if self.resume_depth(active) >= MAX_RESUME_DEPTH {
            return self.resume_refused(active, how, LuaFault::StackOverflow, "C stack overflow");
        }
        let count = ctx.passed.saturating_sub(skip);
        // Where the arguments land: after the body of a new coroutine, or
        // at the `yield` call a suspended one waits on. Lua checks room for
        // all of them (`lua_checkstack`), however many the call keeps.
        let fresh = self
            .heap
            .threads
            .get(co)
            .ok_or(VmError::Corrupt)?
            .frames
            .is_empty();
        let hook_yield = self
            .heap
            .hooks
            .get(self.heap.threads.get(co).ok_or(VmError::Corrupt)?.id)
            .is_some_and(|h| h.hook_yield);
        let yield_site = if fresh || hook_yield {
            None
        } else {
            self.yield_site(co)?
        };
        let end = if fresh {
            Some(1u32.saturating_add(count))
        } else if let Some((yield_func, _, _)) = yield_site {
            Some(yield_func.saturating_add(count))
        } else {
            None
        };
        if let Some(end) = end
            && self.slot_fault(co, end)?.is_some()
        {
            return self.resume_refused(
                active,
                how,
                LuaFault::StackOverflow,
                "too many arguments to resume",
            );
        }
        {
            let object = self.heap.threads.get_mut(co).ok_or(VmError::Corrupt)?;
            object.status = Status::Ready;
            object.resumed_by = Some(active);
        }
        self.heap.active = Some(co);
        self.refresh_hook_trap();
        if !fresh && yield_site.is_none() {
            // Suspended by the `Yield` instruction, which takes no values.
            return Ok(Poll::Continue);
        }
        // This is scratch, never a suspended state. The source stack remains
        // canonical until native_returned has published the receiving window.
        let mut args = std::mem::take(&mut self.native_results);
        args.clear();
        let stack = &self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack;
        args.extend((skip..ctx.passed).map(|index| {
            stack
                .get((ctx.func + 1 + index) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        }));
        let result = if let Some((func, nresults, passed)) = yield_site {
            self.coroutine_returned(co, func, nresults, passed, &args)
        } else {
            self.start_coroutine(co, &args)
        };
        args.clear();
        self.native_results = args;
        result
    }

    /// The `coroutine.yield` call a suspended `co` waits on, if any;
    /// a `Yield` instruction leaves no receiving call window.
    fn yield_site(&self, co: Handle<ThreadObj>) -> Result<Option<CallWindow>, VmError> {
        let object = self.heap.threads.get(co).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        let (func, nresults, passed, callee) =
            if frame.boundary().is_none() && frame.meta().is_none() {
                let proto = self.closure_proto_of(frame.closure)?;
                let (func, nresults) = match proto.ops.get(frame.pc as usize) {
                    Some(Op::Call { func, nresults, .. }) => (*func, *nresults),
                    Some(Op::TailCall { func, .. }) => (*func, COUNT_OPEN),
                    _ => return Ok(None),
                };
                let func = frame.base + u32::from(func);
                let callee = object
                    .stack
                    .get(func as usize)
                    .copied()
                    .unwrap_or(Value::Nil);
                (func, nresults, object.top.saturating_sub(func + 1), callee)
            } else {
                self.call_site(co)?
            };
        Ok((self.coroutine_builtin(callee) == Some(CoFn::Yield))
            .then_some((func, nresults, passed)))
    }

    fn closure_proto_of(
        &self,
        closure: Handle<crate::heap::ClosureObj>,
    ) -> Result<&Proto, VmError> {
        let proto = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        self.heap.protos.get(proto).ok_or(VmError::Corrupt)
    }

    /// The coroutine function `value` is, if any.
    fn coroutine_builtin(&self, value: Value) -> Option<CoFn> {
        let Value::Native(index) = self.callable(value) else {
            return None;
        };
        let slot = *self.native_slots.get(index as usize)?;
        match self.registry.native(slot)?.builtin {
            Some(crate::host::Builtin::Coroutine(function)) => Some(function),
            _ => None,
        }
    }

    /// Start a new coroutine, now the active thread: its body is called
    /// with `args` as the thread's first frame. A Lua body is that frame;
    /// any other function is called from a trampoline frame.
    pub(super) fn start_coroutine(
        &mut self,
        co: Handle<ThreadObj>,
        args: &[Value],
    ) -> Result<Poll, VmError> {
        let count = u32::try_from(args.len()).map_err(|_| VmError::Corrupt)?;
        let body = {
            let object = self.heap.threads.get_mut(co).ok_or(VmError::Corrupt)?;
            let body = object.stack.first().copied().ok_or(VmError::Corrupt)?;
            object.stack.truncate(1);
            object.charge_slots(1 + args.len(), &mut self.heap.gc);
            object.stack.extend_from_slice(args);
            object.top = 1 + count;
            body
        };
        match body {
            Value::Closure(closure) => self.enter_lua(
                closure,
                1,
                count,
                COUNT_OPEN,
                Entry::Push {
                    advance_caller: false,
                },
            ),
            _ => {
                let trampoline = self.trampoline()?;
                self.enter_lua(
                    trampoline,
                    0,
                    count + 1,
                    COUNT_OPEN,
                    Entry::Push {
                        advance_caller: false,
                    },
                )
            }
        }
    }

    /// A new trampoline closure.
    pub(super) fn trampoline(&mut self) -> Result<Handle<crate::heap::ClosureObj>, VmError> {
        let spec = crate::program::ProtoSpec {
            ops: trampoline_ops().to_vec(),
            byte_consts: Vec::new(),
            captures: Vec::new(),
            children: Vec::new(),
            max_reg: 1,
            params: 0,
            vararg: true,
            debug: None,
        };
        let proto = self.install(&spec, None)?;
        self.alloc_closure(proto, Vec::new())
    }

    /// `coroutine.yield(...)`: the running coroutine suspends on this
    /// call, and its resumer gets the arguments.
    fn co_yield(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        passed: u32,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        if self.heap.entry == Some(active) || !object.coroutine {
            return Ok(self.fault_text(
                LuaFault::YieldAcross,
                b"attempt to yield from outside a coroutine".to_vec(),
            ));
        }
        if object.closing || blocks_yield(object) {
            return Ok(self.fault(LuaFault::YieldAcross));
        }
        let parent = object.resumed_by.ok_or(VmError::Corrupt)?;
        let mut values = std::mem::take(&mut self.native_results);
        values.clear();
        values.extend((0..passed).map(|index| {
            object
                .stack
                .get((func + 1 + index) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        }));
        self.deliver_to_parent(active, parent, values, Status::LuaSuspended)
    }

    /// How `parent` waits for the thread it resumed or closes.
    fn parent_wait(
        &self,
        parent: Handle<ThreadObj>,
    ) -> Result<(ParentWait, Option<CallWindow>), VmError> {
        let object = self.heap.threads.get(parent).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        if matches!(frame.pending(), Some(Pending::Resuming { .. })) {
            return Ok((ParentWait::Op, None));
        }
        let (func, nresults, passed, callee) = self.call_site(parent)?;
        let how = match self.coroutine_builtin(callee) {
            Some(CoFn::Resume) => ParentWait::Resume,
            Some(CoFn::WrapCall) => ParentWait::Wrap,
            Some(CoFn::Close) => ParentWait::Close,
            _ => return Err(VmError::Corrupt),
        };
        Ok((how, Some((func, nresults, passed))))
    }

    /// `child` yielded `values` (`LuaSuspended`) or returned them from its
    /// last frame (`Completed`): control goes back to `parent`.
    pub(super) fn deliver_to_parent(
        &mut self,
        child: Handle<ThreadObj>,
        parent: Handle<ThreadObj>,
        mut values: Vec<Value>,
        child_status: Status,
    ) -> Result<Poll, VmError> {
        let result = self.deliver_parent_values(child, parent, &mut values, child_status);
        values.clear();
        self.native_results = values;
        result
    }

    fn deliver_parent_values(
        &mut self,
        child: Handle<ThreadObj>,
        parent: Handle<ThreadObj>,
        values: &mut Vec<Value>,
        child_status: Status,
    ) -> Result<Poll, VmError> {
        let (how, site) = self.parent_wait(parent)?;
        {
            let object = self.heap.threads.get_mut(child).ok_or(VmError::Corrupt)?;
            object.status = child_status;
            object.resumed_by = None;
            if child_status == Status::Completed {
                object.frames.clear();
            }
        }
        self.heap.active = Some(parent);
        self.refresh_hook_trap();
        match how {
            ParentWait::Op => {
                self.deliver_op(parent, values)?;
                Ok(Poll::Continue)
            }
            ParentWait::Resume | ParentWait::Wrap => {
                let (func, nresults, passed) = site.ok_or(VmError::Corrupt)?;
                if how == ParentWait::Resume {
                    values.insert(0, Value::Bool(true));
                }
                let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
                // Lua checks the resumer's stack for all the results,
                // however many the call keeps; past it they are dropped,
                // and the coroutine stays as it is.
                if self
                    .slot_fault(parent, func.saturating_add(count))?
                    .is_some()
                {
                    let text = b"too many results to resume".to_vec();
                    let message = Value::String(self.alloc_string(text)?);
                    if how == ParentWait::Wrap {
                        return Ok(self.wrap_raise(parent, LuaFault::StackOverflow, message));
                    }
                    values.clear();
                    values.extend_from_slice(&[Value::Bool(false), message]);
                }
                self.coroutine_returned(parent, func, nresults, passed, values)
            }
            ParentWait::Close => Err(VmError::Corrupt),
        }
    }

    /// A transfer into an ordinary Lua call with an already allocated result
    /// window. Publish exactly native_returned's stack, top and pc, with one
    /// borrow and bulk copies. Boundary/metamethod continuations, short restored
    /// stacks and reserve/quota cases retain the general delivery path.
    fn coroutine_returned(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        nresults: u8,
        passed: u32,
        values: &[Value],
    ) -> Result<Poll, VmError> {
        if self.hook_trap {
            return self.native_returned(active, func, nresults, passed, values);
        }
        #[cfg(any(test, debug_assertions))]
        if self.hot_core != super::HotCoreMode::Full {
            return self.native_returned(active, func, nresults, passed, values);
        }
        let produced = u32::try_from(values.len()).map_err(|_| VmError::Corrupt)?;
        let want = Self::wanted(nresults, produced);
        let end = func.saturating_add(want);
        let clear_from = func + want.max(1);
        let clear_end = (func + 1 + passed).max(clear_from);
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::Corrupt)?;
        if frame.boundary().is_some()
            || frame.meta().is_some()
            || clear_end as usize > object.stack.len()
            || end > self.max_stack_slots - self.max_stack_slots / 8
        {
            return self.native_returned(active, func, nresults, passed, values);
        }
        // No growth is needed, so slot_fault's quota check cannot fail and the
        // ordinary stack bound above excludes its error-handling reserve.
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        let copied = want.min(produced) as usize;
        let dest = func as usize;
        // The guard above proved the whole result and scratch window live.
        // Use its physical storage directly, avoiding a second logical slice
        // bound for each of these three ranges.
        object.stack.values[dest..dest + copied].copy_from_slice(&values[..copied]);
        object.stack.values[dest + copied..end as usize].fill(Value::Nil);
        object.stack.values[clear_from as usize..clear_end as usize].fill(Value::Nil);
        object.stack.truncate(frame.limit.max(end) as usize);
        object.top = end;
        frame.clear_pending(&mut self.cold_spare);
        frame.pc = frame.pc.saturating_add(1);
        Ok(Poll::Continue)
    }

    /// The `Resume` instruction's results: `values` at its destination.
    fn deliver_op(&mut self, parent: Handle<ThreadObj>, values: &[Value]) -> Result<(), VmError> {
        let (dest, nresults) = {
            let object = self.heap.threads.get_mut(parent).ok_or(VmError::Corrupt)?;
            let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
            let Some(&Pending::Resuming { dest, nresults, .. }) = frame.pending() else {
                return Err(VmError::Corrupt);
            };
            frame.clear_pending(&mut self.cold_spare);
            frame.pc = frame.pc.saturating_add(1);
            (frame.base + u32::from(dest), nresults)
        };
        let produced = u32::try_from(values.len()).map_err(|_| VmError::Corrupt)?;
        let want = Self::wanted(nresults, produced);
        for offset in 0..want {
            let value = values.get(offset as usize).copied().unwrap_or(Value::Nil);
            self.write_abs(parent, dest + offset, value)?;
        }
        let scratch_end = if want == 0 { dest + 1 } else { dest + want };
        self.finish_result_window(parent, dest, nresults, produced, scratch_end)
    }

    /// `child` failed with an error nothing in it caught. Its stack stays.
    pub(super) fn child_failed(
        &mut self,
        parent: Handle<ThreadObj>,
        child: Handle<ThreadObj>,
        fault: LuaFault,
        error: Value,
    ) -> Result<Poll, VmError> {
        let (how, site) = self.parent_wait(parent)?;
        match how {
            ParentWait::Op => self.raise_in_resumer(parent, fault, error),
            ParentWait::Resume => {
                self.heap.active = Some(parent);
                self.refresh_hook_trap();
                let (func, nresults, passed) = site.ok_or(VmError::Corrupt)?;
                self.native_returned(parent, func, nresults, passed, &[Value::Bool(false), error])
            }
            // A `wrap` function closes the coroutine first, then raises
            // the error that is left (`auxwrap`).
            ParentWait::Wrap => self.start_close(child, parent),
            ParentWait::Close => Err(VmError::Corrupt),
        }
    }

    /// Closing `child` for `parent` ended: `error` is the error left, if
    /// any.
    pub(super) fn close_finished(
        &mut self,
        parent: Handle<ThreadObj>,
        error: Option<(LuaFault, Value)>,
    ) -> Result<Poll, VmError> {
        let (how, site) = self.parent_wait(parent)?;
        match how {
            ParentWait::Op => Err(VmError::Corrupt),
            ParentWait::Close => {
                self.heap.active = Some(parent);
                self.refresh_hook_trap();
                let (func, nresults, passed) = site.ok_or(VmError::Corrupt)?;
                let results = match error {
                    Some((_, error)) => vec![Value::Bool(false), error],
                    None => vec![Value::Bool(true)],
                };
                self.native_returned(parent, func, nresults, passed, &results)
            }
            ParentWait::Wrap => {
                let (fault, error) = error.ok_or(VmError::Corrupt)?;
                Ok(self.wrap_raise(parent, fault, error))
            }
            ParentWait::Resume => Err(VmError::Corrupt),
        }
    }

    /// Start closing `child` for `closer`: its pending to-be-closed values
    /// close with its error, or nil, as `CloseThread` does.
    fn start_close(
        &mut self,
        child: Handle<ThreadObj>,
        closer: Handle<ThreadObj>,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(child).ok_or(VmError::Corrupt)?;
        let error = object.error.take();
        object.status = Status::Ready;
        object.closing = true;
        object.resumed_by = Some(closer);
        object.unwind = Some(Box::new(crate::heap::Unwind {
            error,
            phase: crate::heap::UnwindPhase::Popping { target: None },
        }));
        let id = self.heap.threads.get(child).ok_or(VmError::Corrupt)?.id;
        if let Some(hook) = self.heap.hooks.get_mut(id) {
            hook.hook_yield = false;
        }
        self.heap.active = Some(child);
        self.refresh_hook_trap();
        Ok(Poll::Continue)
    }

    /// `coroutine.close(co)`: a dead or suspended coroutine closes its
    /// pending values and is dead; `true`, or `false` and its error.
    fn co_close(
        &mut self,
        active: Handle<ThreadObj>,
        co: Handle<ThreadObj>,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get(co).ok_or(VmError::Corrupt)?;
        match object.status {
            _ if co == active => Ok(self.fault(LuaFault::CloseRunning)),
            Status::Completed => self.base_return(active, &[Value::Bool(true)]),
            Status::LuaSuspended | Status::Failed if object.frames.is_empty() => {
                let object = self.heap.threads.get_mut(co).ok_or(VmError::Corrupt)?;
                object.stack.clear();
                object.top = 0;
                object.status = Status::Completed;
                object.error = None;
                self.base_return(active, &[Value::Bool(true)])
            }
            Status::LuaSuspended | Status::Failed => self.start_close(co, active),
            Status::Ready | Status::Waiting => Ok(self.fault(LuaFault::CloseNormal)),
        }
    }
}

/// Whether a frame on `thread` is a call no yield may cross: an `xpcall`
/// message handler, or a builtin running without a continuation.
pub(super) fn blocks_yield(thread: &ThreadObj) -> bool {
    thread.frames.iter().any(|frame| match frame.boundary() {
        Some(Boundary::Handler { .. } | Boundary::Finalizer { .. } | Boundary::Hook { .. }) => true,
        Some(Boundary::Builtin { task, .. }) => !task.yieldable(),
        _ => false,
    })
}
