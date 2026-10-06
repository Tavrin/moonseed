//! Cold hook event machinery. The hot interpreter observes only `hook_trap`.
use super::library::{Ctx, Next};
use super::*;

pub(crate) const HOOK_BYTES: u64 = 256;
pub(crate) const CALL: u8 = 1;
pub(crate) const RETURN: u8 = 2;
pub(crate) const LINE: u8 = 4;
pub(crate) const COUNT: u8 = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) enum HookTarget {
    None,
    Lua(Value),
    /// The inherited debug.sethook wrapper has no child-thread function.
    InheritedLua,
    Host(u32),
}
impl HookTarget {
    pub(crate) fn value(self) -> Value {
        match self {
            Self::Lua(value) => value,
            _ => Value::Nil,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Call,
    Return,
    Line,
    Count,
    TailCall,
}
impl Event {
    fn mask(self) -> u8 {
        match self {
            Self::Call | Self::TailCall => CALL,
            Self::Return => RETURN,
            Self::Line => LINE,
            Self::Count => COUNT,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Call => 0,
            Self::Return => 1,
            Self::Line => 2,
            Self::Count => 3,
            Self::TailCall => 4,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum AfterHook {
    Continue,
    Return { src: u32, produced: u32 },
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingEvent {
    pub(crate) event: Event,
    pub(crate) line: Option<u32>,
    pub(crate) frame: u32,
    pub(crate) transfer: (u32, u32),
    pub(crate) after: AfterHook,
}
#[derive(Clone, Debug)]
pub(crate) struct HookState {
    /// Weak owner metadata; traced only when this thread is reached.
    pub(crate) owner: Handle<ThreadObj>,
    pub(crate) target: HookTarget,
    pub(crate) mask: u8,
    pub(crate) base_count: i32,
    pub(crate) remaining_count: i32,
    pub(crate) allow_hook: bool,
    pub(crate) old_pc: Option<(usize, u32, Option<u32>)>,
    pub(crate) pending: Option<PendingEvent>,
    pub(crate) hook_yield: bool,
    pub(crate) names: [Value; 5],
    pub(crate) instruction: Option<(usize, u32, u8)>,
    pub(crate) after: AfterHook,
    pub(crate) boundary_spare: Option<Box<FrameCold>>,
    pub(crate) transfer: Option<(u32, u32, u32)>,
    pub(crate) restore_cursor: Option<(usize, u32, Option<u32>)>,
}

/// Empty heaps allocate no hook storage and thread arena slots keep their ABI.
/// Entries do not root their owner: GC traces a state's values from its live
/// thread, then removes entries whose generation-checked owner was swept.
type HookMap = std::collections::HashMap<ObjectId, Box<HookState>>;
#[derive(Clone, Debug, Default)]
pub(crate) struct ThreadHooks(Option<Box<HookMap>>);
impl ThreadHooks {
    pub(crate) fn get(&self, id: ObjectId) -> Option<&HookState> {
        self.0.as_deref()?.get(&id).map(Box::as_ref)
    }
    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Option<&mut HookState> {
        self.0.as_deref_mut()?.get_mut(&id).map(Box::as_mut)
    }
    pub(crate) fn insert(&mut self, id: ObjectId, state: HookState) {
        self.0
            .get_or_insert_with(Box::default)
            .insert(id, Box::new(state));
    }
    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.as_deref().is_none_or(|map| map.is_empty())
    }
    pub(crate) fn reap(&mut self, threads: &crate::heap::Arena<ThreadObj>) {
        if let Some(map) = &mut self.0 {
            map.retain(|_, state| threads.get(state.owner).is_some());
            if map.is_empty() {
                self.0 = None;
            }
        }
    }
}

type InheritedHook = (HookTarget, u8, i32, [Value; 5]);
impl HookState {
    fn new(
        owner: Handle<ThreadObj>,
        target: HookTarget,
        mask: u8,
        count: i32,
        names: [Value; 5],
    ) -> Self {
        Self {
            owner,
            target,
            mask,
            base_count: count,
            remaining_count: count,
            allow_hook: true,
            old_pc: None,
            pending: None,
            hook_yield: false,
            names,
            instruction: None,
            after: AfterHook::Continue,
            boundary_spare: None,
            transfer: None,
            restore_cursor: None,
        }
    }
}

impl Runtime {
    /// PUC copies the hook pointer/mask/base count, but not the registry's
    /// weak per-thread Lua function. The child starts a fresh countdown.
    pub(super) fn inherited_hook(&self) -> Option<InheritedHook> {
        if self.heap.hooks.is_empty() {
            return None;
        }
        let parent = self.heap.threads.get(self.heap.active?)?;
        let hook = self.heap.hooks.get(parent.id)?;
        let target = match hook.target {
            HookTarget::None => return None,
            HookTarget::Lua(_) | HookTarget::InheritedLua => HookTarget::InheritedLua,
            HookTarget::Host(symbol) => HookTarget::Host(symbol),
        };
        Some((target, hook.mask, hook.base_count, hook.names))
    }
    pub(super) fn install_inherited_hook(
        &mut self,
        owner: Handle<ThreadObj>,
        inherited: Option<InheritedHook>,
    ) -> Result<(), VmError> {
        if let Some((target, mask, count, names)) = inherited {
            let thread = self.heap.threads.get_mut(owner).ok_or(VmError::Corrupt)?;
            let id = thread.id;
            thread.charged_held += HOOK_BYTES;
            self.heap.gc.charge(HOOK_BYTES);
            self.heap
                .hooks
                .insert(id, HookState::new(owner, target, mask, count, names));
        }
        Ok(())
    }
    #[inline(always)]
    pub(crate) fn refresh_hook_trap(&mut self) {
        if self.heap.hooks.is_empty() {
            self.hook_trap = false;
            return;
        }
        self.hook_trap = self
            .heap
            .active
            .and_then(|h| self.heap.threads.get(h))
            .and_then(|t| self.heap.hooks.get(t.id))
            .is_some_and(|h| {
                h.mask != 0
                    || h.pending.is_some()
                    || h.hook_yield
                    || matches!(h.after, AfterHook::Return { .. })
            });
    }
    pub(super) fn hooks_allowed(&self) -> bool {
        !self.heap.finalizers.running
            && self
                .heap
                .active
                .and_then(|h| self.heap.threads.get(h))
                .and_then(|t| self.heap.hooks.get(t.id))
                .is_some_and(|h| {
                    h.allow_hook && !matches!(h.target, HookTarget::None | HookTarget::InheritedLua)
                })
    }
    fn event_enabled(&self, event: Event) -> bool {
        self.hooks_allowed()
            && self
                .heap
                .active
                .and_then(|h| self.heap.threads.get(h))
                .and_then(|t| self.heap.hooks.get(t.id))
                .is_some_and(|h| {
                    h.mask & event.mask() != 0 && !matches!(h.target, HookTarget::None)
                })
    }
    pub(super) fn db_sethook(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let (thread, arg) = match self.lib_arg(ctx, 0) {
            Value::Thread(t) => (t, 1),
            _ => (ctx.active, 0),
        };
        let function = self.lib_arg(ctx, arg);
        let (target, mask, count) = if matches!(function, Value::Nil) {
            (HookTarget::None, 0, 0)
        } else {
            let string = match self.string_arg(ctx, arg + 1)? {
                Some(s) => s,
                None => return Ok(self.bad_type(ctx, arg + 1, "string")),
            };
            if !function.is_function() {
                return Ok(self.bad_type(ctx, arg, "function"));
            }
            let count = if self.given(ctx, arg + 2) {
                match self.int_arg(ctx, arg + 2) {
                    Ok(n) => n as i32,
                    Err(next) => return Ok(next),
                }
            } else {
                0
            };
            let bytes = self.heap.string_bytes(string).ok_or(VmError::Corrupt)?;
            let bytes = &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())];
            let mask = (u8::from(bytes.contains(&b'c')) * CALL)
                | (u8::from(bytes.contains(&b'r')) * RETURN)
                | (u8::from(bytes.contains(&b'l')) * LINE)
                | (u8::from(count > 0) * COUNT);
            (
                if mask == 0 {
                    HookTarget::None
                } else {
                    HookTarget::Lua(function)
                },
                mask,
                count,
            )
        };
        let id = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?.id;
        if self.heap.hooks.get(id).is_none() && mask != 0 {
            self.ensure_room(HOOK_BYTES)?;
            self.heap.hooks.insert(
                id,
                HookState::new(thread, HookTarget::None, 0, 0, [Value::Nil; 5]),
            );
            self.heap.gc.charge(HOOK_BYTES);
            self.heap
                .threads
                .get_mut(thread)
                .ok_or(VmError::Corrupt)?
                .charged_held += HOOK_BYTES;
        }
        // An inherited wrapper has no callback names or boundary storage yet.
        // Allocate these only on explicit installation, before any delivery.
        if mask != 0 {
            for (index, name) in ["call", "return", "line", "count", "tail call"]
                .iter()
                .enumerate()
            {
                if self.heap.hooks.get(id).ok_or(VmError::Corrupt)?.names[index] == Value::Nil {
                    let value = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
                    self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
                    self.heap.hooks.get_mut(id).ok_or(VmError::Corrupt)?.names[index] = value;
                }
            }
            let hook = self.heap.hooks.get_mut(id).ok_or(VmError::Corrupt)?;
            if hook.boundary_spare.is_none() {
                hook.boundary_spare = Some(Box::default());
            }
        }
        let cursor = self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames
            .iter()
            .enumerate()
            .rev()
            .find(|(_, f)| f.boundary().is_none())
            .map(|(depth, f)| {
                let line = self
                    .heap
                    .closures
                    .get(f.closure)
                    .and_then(|c| self.heap.protos.get(c.proto))
                    .and_then(|p| p.debug.as_deref())
                    .and_then(|d| d.lines.get(f.pc as usize))
                    .copied();
                (depth, f.pc, line)
            });
        // Access through the thread barrier before replacing rooted values.
        let id = self
            .heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .id;
        if let Some(hook) = self.heap.hooks.get_mut(id) {
            hook.target = target;
            hook.mask = mask;
            hook.base_count = count;
            hook.remaining_count = count;
            // Cursor follows the currently running instruction, as lua_sethook does.
            hook.old_pc = cursor;
        }
        self.refresh_hook_trap();
        Ok(Next::Done(Vec::new()))
    }
    pub(super) fn db_gethook(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let thread = match self.lib_arg(ctx, 0) {
            Value::Thread(t) => t,
            _ => ctx.active,
        };
        let id = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?.id;
        let hook = self.heap.hooks.get(id);
        let Some(hook) = hook.filter(|h| !matches!(h.target, HookTarget::None)) else {
            return Ok(Next::Done(vec![Value::Nil]));
        };
        let (target, mask, count) = (hook.target, hook.mask, hook.base_count);
        let function = match target {
            HookTarget::Lua(value) => value,
            HookTarget::Host(_) => self.new_string(b"external hook".to_vec())?,
            HookTarget::None | HookTarget::InheritedLua => Value::Nil,
        };
        let mut bytes = Vec::with_capacity(3);
        for (bit, byte) in [(CALL, b'c'), (RETURN, b'r'), (LINE, b'l')] {
            if mask & bit != 0 {
                bytes.push(byte);
            }
        }
        let mask = self.new_string(bytes)?;
        Ok(Next::Done(vec![
            function,
            mask,
            Value::Integer(i64::from(count)),
        ]))
    }
    pub(super) fn lua_hook_entry(
        &mut self,
        active: Handle<ThreadObj>,
        tail: bool,
    ) -> Result<(), VmError> {
        // Lua resets the line cursor at every Lua entry, even while the
        // callback suppresses event delivery or call hooks are disabled.
        if let Some(hook) = self
            .heap
            .threads
            .get_mut(active)
            .and_then(|t| self.heap.hooks.get_mut(t.id))
        {
            hook.old_pc = None;
        }
        if !self.event_enabled(Event::Call) {
            return Ok(());
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
        let params = self
            .heap
            .closures
            .get(frame.closure)
            .and_then(|c| self.heap.protos.get(c.proto))
            .ok_or(VmError::Corrupt)?
            .params;
        let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
        hook.instruction = None;
        hook.old_pc = None;
        hook.pending = Some(PendingEvent {
            event: if tail { Event::TailCall } else { Event::Call },
            line: None,
            frame: thread.frames.len() as u32 - 1,
            transfer: if params == 0 {
                (0, 0)
            } else {
                (1, u32::from(params))
            },
            after: AfterHook::Continue,
        });
        Ok(())
    }
    pub(super) fn queue_lua_return(
        &mut self,
        active: Handle<ThreadObj>,
        src: u32,
        produced: u32,
    ) -> Result<bool, VmError> {
        if !self.event_enabled(Event::Return) {
            return Ok(false);
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        if frame.flags & 2 != 0 {
            frame.flags &= !2;
            return Ok(false);
        }
        frame.flags |= 2;
        let transfer = if produced == 0 {
            (0, 0)
        } else {
            (src - frame.base + 1, produced)
        };
        self.heap
            .hooks
            .get_mut(thread.id)
            .ok_or(VmError::Corrupt)?
            .pending = Some(PendingEvent {
            event: Event::Return,
            line: None,
            frame: thread.frames.len() as u32 - 1,
            transfer,
            after: AfterHook::Return { src, produced },
        });
        Ok(true)
    }
    pub(super) fn poll_hook(
        &mut self,
        quantum: &mut u64,
        journal: &mut Journal,
    ) -> Result<Option<Poll>, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let after = self
            .heap
            .threads
            .get_mut(active)
            .and_then(|t| self.heap.hooks.get_mut(t.id))
            .map(|h| std::mem::replace(&mut h.after, AfterHook::Continue));
        if let Some(AfterHook::Return { src, produced }) = after {
            return self.return_values(active, src, produced).map(Some);
        }
        let pending = self
            .heap
            .threads
            .get(active)
            .and_then(|t| self.heap.hooks.get(t.id))
            .and_then(|h| h.pending);
        if let Some(event) = pending {
            return self.deliver_hook(active, event, quantum, journal).map(Some);
        }
        Ok(None)
    }
    /// The single event delivery point. All bookkeeping is independent of hook Lua fuel.
    fn deliver_hook(
        &mut self,
        active: Handle<ThreadObj>,
        event: PendingEvent,
        quantum: &mut u64,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        // A host may replace or clear a paused pending event before delivery.
        if !self.event_enabled(event.event) {
            let hook = self
                .heap
                .threads
                .get(active)
                .and_then(|t| self.heap.hooks.get_mut(t.id))
                .ok_or(VmError::Corrupt)?;
            hook.pending = None;
            hook.after = event.after;
            self.refresh_hook_trap();
            return Ok(Poll::Continue);
        }
        if *quantum == 0 {
            return Ok(Poll::Stop(StepOutcome::Paused(PauseReason::FuelExhausted)));
        }
        if self
            .fuel_limit
            .is_some_and(|limit| self.fuel_consumed >= limit)
        {
            self.trap = Some(TerminationReason::FuelLimitExceeded);
            return Ok(Poll::Stop(StepOutcome::Terminated(
                TerminationReason::FuelLimitExceeded,
            )));
        }
        self.fuel_consumed += 1;
        *quantum -= 1;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
        hook.pending = None;
        hook.allow_hook = false;
        // traceexec commits the interrupted PC after a line callback, but
        // count callbacks leave the PC reached by their suppressed body.
        hook.restore_cursor = if matches!(event.event, Event::Line) {
            hook.old_pc
        } else {
            None
        };

        if let HookTarget::Host(symbol) = hook.target {
            hook.transfer = Some((event.frame, event.transfer.0, event.transfer.1));
            let callback = self
                .registry
                .hook(symbol)
                .ok_or(VmError::Corrupt)?
                .1
                .clone();
            self.api_roots();
            self.in_callback = true;
            let action = callback(&mut crate::HookContext {
                runtime: self,
                thread: active,
                event: event.event.into(),
                line: event.line,
            });
            self.in_callback = false;
            let yield_error = if matches!(action, Ok(crate::HookAction::Yield)) {
                let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
                if !matches!(event.event, Event::Line | Event::Count)
                    || thread.closing
                    || coroutine::blocks_yield(thread)
                {
                    Some(LuaFault::YieldAcross.text().as_bytes())
                } else if self.heap.entry == Some(active) || !thread.coroutine {
                    Some(b"attempt to yield from outside a coroutine".as_slice())
                } else {
                    None
                }
            } else {
                None
            };
            if yield_error.is_some() || matches!(action, Err(crate::Error::Lua(_))) {
                let mut message = self.where_prefix(active);
                if let Some(text) = yield_error {
                    message.extend_from_slice(text);
                }
                // Errors leave suppression in force while xpcall's handler
                // runs, then normal Hook unwinding restores the interrupted VM.
                let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
                let frame = thread
                    .frames
                    .get(event.frame as usize)
                    .ok_or(VmError::Corrupt)?;
                let func = frame.limit.max(thread.top).max(thread.stack.len() as u32);
                let closure = frame.closure;
                thread.frames.push(Frame {
                    closure,
                    pc: 0,
                    base: func,
                    limit: func,
                    nresults: 0,
                    vararg_len: 0,
                    flags: 0,
                    cold: FrameCold::with_boundary(
                        Boundary::Hook {
                            func,
                            saved_top: thread.top,
                            target: event.frame,
                            instruction: hook.instruction.take(),
                            after: event.after,
                        },
                        &mut hook.boundary_spare,
                    ),
                });
                return if yield_error.is_some() {
                    Ok(self.fault_text(LuaFault::YieldAcross, message))
                } else {
                    self.callback_error(action.err().unwrap())
                };
            }
            let hook = self
                .heap
                .threads
                .get(active)
                .and_then(|t| self.heap.hooks.get_mut(t.id))
                .ok_or(VmError::Corrupt)?;
            hook.allow_hook = true;
            hook.transfer = None;
            hook.restore_cursor = None;
            hook.after = event.after;
            self.refresh_hook_trap();
            return match action {
                Ok(crate::HookAction::Continue) => Ok(Poll::Continue),
                Ok(crate::HookAction::Yield) => self.yield_host_hook(active, event.event),
                Err(error) => self.callback_error(error),
            };
        }

        let target = hook.target.value();
        let name = hook.names[event.event.index()];
        let saved_top = thread.top;
        let frame = thread
            .frames
            .get_mut(event.frame as usize)
            .ok_or(VmError::Corrupt)?;
        let func = frame.limit.max(saved_top).max(thread.stack.len() as u32);
        hook.transfer = Some((event.frame, event.transfer.0, event.transfer.1));
        let closure = frame.closure;
        let instruction = hook.instruction.take();
        thread.frames.push(Frame {
            closure,
            pc: 0,
            base: func,
            limit: func,
            nresults: 0,
            vararg_len: 0,
            flags: 0,
            cold: FrameCold::with_boundary(
                Boundary::Hook {
                    func,
                    saved_top,
                    target: event.frame,
                    instruction,
                    after: event.after,
                },
                &mut hook.boundary_spare,
            ),
        });
        // Reserve the hook's argument window before write_abs can grow it.
        // Keep the Hook boundary on failure so unwinding restores suppression.
        if let Some(fault) = self.slot_fault(active, func + 3)? {
            return Ok(self.fault(fault));
        }
        self.write_abs(active, func, target)?;
        self.write_abs(active, func + 1, name)?;
        self.write_abs(
            active,
            func + 2,
            event
                .line
                .map_or(Value::Nil, |n| Value::Integer(i64::from(n))),
        )?;
        self.heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .top = func + 3;
        let result = match self.callable(target) {
            Value::Closure(closure) => self.push_lua_frame(closure, func, 2, 0, false),
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        };
        result.or_else(|error| self.vm_error(error))
    }
    pub(super) fn finish_hook(&mut self, active: Handle<ThreadObj>) -> Result<Poll, VmError> {
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let mut frame = thread.frames.pop().ok_or(VmError::Corrupt)?;
        let Some(Boundary::Hook {
            func,
            saved_top,
            target,
            instruction,
            after,
        }) = frame.boundary().copied_hook()
        else {
            return Err(VmError::Corrupt);
        };
        let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
        frame.recycle_cold(&mut hook.boundary_spare);
        let _ = target;
        hook.transfer = None;
        if let Some(cursor) = hook.restore_cursor.take() {
            hook.old_pc = Some(cursor);
        }
        thread.stack.truncate(func as usize);
        thread.top = saved_top;
        hook.allow_hook = true;
        hook.after = after;
        hook.instruction = instruction;
        if thread.unwind.is_some() {
            hook.after = AfterHook::Continue;
            hook.pending = None;
        }
        self.refresh_hook_trap();
        Ok(Poll::Continue)
    }
    pub(super) fn trace_hook_instruction(
        &mut self,
        quantum: &mut u64,
        journal: &mut Journal,
    ) -> Result<Option<Poll>, VmError> {
        let allowed = self.hooks_allowed();
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let depth = thread.frames.len() - 1;
        let frame = thread.frames.last().ok_or(VmError::Corrupt)?;
        let pc = frame.pc;
        let mut line = self
            .heap
            .closures
            .get(frame.closure)
            .and_then(|c| self.heap.protos.get(c.proto))
            .and_then(|p| p.debug.as_deref())
            .and_then(|d| d.lines.get(pc as usize))
            .copied();
        let branch = self
            .heap
            .closures
            .get(frame.closure)
            .and_then(|c| self.heap.protos.get(c.proto))
            .and_then(|p| p.ops.get(pc as usize))
            .is_some_and(|op| matches!(op, Op::Jump { .. } | Op::JumpIfFalse { .. }));
        let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
        if hook.hook_yield {
            hook.hook_yield = false;
            hook.instruction = None;
            self.refresh_hook_trap();
            return Ok(None);
        }
        // Forward logical joins retain the source position of the operand
        // evaluated on that edge. Their lowering span can begin before it.
        if branch
            && let Some((old_depth, old_pc, old_line)) = hook.old_pc
            && old_depth == depth
            && pc > old_pc
            && line < old_line
        {
            line = old_line;
        }
        if !hook
            .instruction
            .is_some_and(|(d, p, _)| (d, p) == (depth, pc))
        {
            hook.instruction = Some((depth, pc, 1));
            if hook.mask & COUNT != 0 {
                hook.remaining_count -= 1;
                if hook.remaining_count <= 0 {
                    hook.remaining_count = hook.base_count;
                    if !allowed {
                        hook.instruction = Some((depth, pc, 2));
                    } else {
                        // traceexec captures line selection before invoking a
                        // count hook. Enabling lines inside it starts on the
                        // following instruction, not this same delivery site.
                        if hook.mask & LINE == 0 {
                            hook.instruction = Some((depth, pc, 2));
                        }
                        let event = PendingEvent {
                            event: Event::Count,
                            line: None,
                            frame: depth as u32,
                            transfer: (0, 0),
                            after: AfterHook::Continue,
                        };
                        hook.pending = Some(event);
                        return self.deliver_hook(active, event, quantum, journal).map(Some);
                    }
                }
            }
        }
        if !allowed {
            if hook.mask & LINE != 0 {
                hook.old_pc = Some((depth, pc, line));
            }
            return Ok(None);
        }
        if hook.instruction.is_some_and(|(_, _, stage)| stage == 2) {
            return Ok(None);
        }
        hook.instruction = Some((depth, pc, 2));
        let old = hook.old_pc.map(|(d, p, l)| {
            if d == depth {
                (p, l)
            } else {
                // Lua's cursor is per thread. A suppressed count callback
                // can leave its PC here; interpret it in the current proto,
                // clamping an invalid PC to the entry as luaG_traceexec does.
                let proto = self
                    .heap
                    .closures
                    .get(frame.closure)
                    .and_then(|c| self.heap.protos.get(c.proto));
                let p = proto.map_or(
                    0,
                    |proto| if (p as usize) < proto.ops.len() { p } else { 0 },
                );
                let l = proto
                    .and_then(|p0| p0.debug.as_deref())
                    .and_then(|d| d.lines.get(p as usize))
                    .copied();
                (p, l)
            }
        });
        hook.old_pc = Some((depth, pc, line));
        if hook.mask & LINE != 0 && old.is_none_or(|(p, l)| pc <= p || l != line) {
            let event = PendingEvent {
                event: Event::Line,
                line,
                frame: depth as u32,
                transfer: (0, 0),
                after: AfterHook::Continue,
            };
            hook.pending = Some(event);
            return self.deliver_hook(active, event, quantum, journal).map(Some);
        }
        Ok(None)
    }
    pub(super) fn hook_native_running(&self) -> bool {
        self.heap
            .active
            .and_then(|h| self.heap.threads.get(h))
            .and_then(|t| t.frames.last())
            .is_some_and(|f| matches!(f.boundary(), Some(Boundary::HookNative { phase: 2, .. })))
    }
    pub(super) fn enter_hook_native(
        &mut self,
        index: u32,
        site: Option<(u32, u8, u32)>,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let (func, wants, passed, callee) = match site {
            Some((f, w, p)) => (
                f,
                w,
                p,
                self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack[f as usize],
            ),
            None => self.call_site(active)?,
        };
        let _ = index;
        let call = self.event_enabled(Event::Call);
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let caller = thread.frames.last().ok_or(VmError::Corrupt)?;
        let closure = caller.closure;
        let advance_caller = caller.boundary().is_none() && caller.meta().is_none();
        thread.frames.push(Frame {
            closure,
            pc: 0,
            base: func + 1,
            limit: func + 1 + passed,
            nresults: wants,
            vararg_len: 0,
            flags: 0,
            cold: FrameCold::with_boundary(
                Boundary::HookNative {
                    func,
                    passed,
                    callee,
                    advance_caller,
                    phase: 1,
                    produced: 0,
                    result: 0,
                },
                &mut self.cold_spare,
            ),
        });
        if call {
            self.heap
                .hooks
                .get_mut(thread.id)
                .ok_or(VmError::Corrupt)?
                .pending = Some(PendingEvent {
                event: Event::Call,
                line: None,
                frame: thread.frames.len() as u32 - 1,
                transfer: if passed == 0 { (0, 0) } else { (1, passed) },
                after: AfterHook::Continue,
            });
        }
        Ok(Poll::Continue)
    }
    pub(super) fn step_hook_native(
        &mut self,
        active: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _passed, callee, advance, phase, produced, result) = {
            let frame = self
                .heap
                .threads
                .get(active)
                .and_then(|t| t.frames.last())
                .ok_or(VmError::Corrupt)?;
            let Some(&Boundary::HookNative {
                func,
                passed,
                callee,
                advance_caller,
                phase,
                produced,
                result,
            }) = frame.boundary()
            else {
                return Err(VmError::Corrupt);
            };
            (
                func,
                passed,
                callee,
                advance_caller,
                phase,
                produced,
                result,
            )
        };
        if phase == 1 {
            if let Some(Boundary::HookNative { phase, .. }) = self
                .heap
                .threads
                .get_mut(active)
                .and_then(|t| t.frames.last_mut())
                .and_then(|f| f.boundary_mut())
            {
                *phase = 2;
            }
            let Value::Native(index) = self.callable(callee) else {
                return Err(VmError::Corrupt);
            };
            return self.call_native_at(index, None, journal);
        }
        if phase == 2 {
            // A multi-step builtin has completed into the wrapper's open window.
            let produced = self
                .heap
                .threads
                .get(active)
                .ok_or(VmError::Corrupt)?
                .top
                .saturating_sub(func);
            let result = self.hook_native_result_slot(active, produced)?;
            self.move_hook_results(active, func, result, produced)?;
            return self.prepare_native_return_hook(active, result, produced);
        }
        if phase == 3 {
            return self.prepare_native_return_hook(active, result, produced);
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let mut frame = thread.frames.pop().ok_or(VmError::Corrupt)?;
        let wants = frame.nresults;
        frame.recycle_cold(&mut self.cold_spare);
        self.place_results(active, result, produced, func, wants)?;
        self.finish_result_window(active, func, wants, produced, result + produced)?;
        let frame = self
            .heap
            .threads
            .get_mut(active)
            .and_then(|t| t.frames.last_mut())
            .ok_or(VmError::Corrupt)?;
        if let Some(meta) = frame.meta_mut() {
            meta.phase = MetaPhase::Running;
            self.commit_meta(active)?;
        } else {
            frame.clear_pending(&mut self.cold_spare);
            if advance {
                frame.pc = frame.pc.saturating_add(1);
            }
        }
        self.correct_hook_cursor(active, advance)?;
        Ok(Poll::Continue)
    }
    /// PUC native results may be existing arguments or newly pushed values.
    fn hook_native_result_slot(
        &self,
        active: Handle<ThreadObj>,
        produced: u32,
    ) -> Result<u32, VmError> {
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let Some(&Boundary::HookNative {
            func,
            passed,
            callee,
            ..
        }) = thread.frames.last().and_then(|f| f.boundary())
        else {
            return Err(VmError::Corrupt);
        };
        let Value::Native(index) = self.callable(callee) else {
            return Err(VmError::Corrupt);
        };
        let symbol = self
            .heap
            .natives
            .get(index as usize)
            .ok_or(VmError::Corrupt)?
            .as_str();
        let offset = match symbol {
            "base.select" => {
                if matches!(
                    thread.stack.get((func + 1) as usize),
                    Some(Value::String(_))
                ) {
                    passed + 1
                } else {
                    passed.saturating_sub(produced) + 1
                }
            }
            "base.assert" | "base.setmetatable" | "base.rawset" | "debug.setmetatable"
            | "table.pack" | "string.gmatch" => 1,
            "base.next" | "base.rawget" => 2,
            "string.pack" => passed + 2,
            "base.load"
                if thread
                    .stack
                    .get((func + 1) as usize)
                    .is_some_and(|v| v.is_function()) =>
            {
                6
            }
            "string.dump" => 2,
            "base.pcall" => {
                if thread.stack.get(func as usize) == Some(&Value::Bool(false)) {
                    3
                } else {
                    1
                }
            }
            "base.xpcall" => {
                if thread.stack.get(func as usize) == Some(&Value::Bool(false)) {
                    5
                } else {
                    3
                }
            }
            "coroutine.resume" => 2,
            "coroutine.yield" | "coroutine.wrap.call" => 1,
            _ => passed + 1,
        };
        Ok(func + offset)
    }
    pub(super) fn correct_hook_cursor(
        &mut self,
        active: Handle<ThreadObj>,
        advanced: bool,
    ) -> Result<(), VmError> {
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let Some(frame) = thread.frames.last().filter(|f| f.boundary().is_none()) else {
            return Ok(());
        };
        let pc = if advanced {
            frame.pc.saturating_sub(1)
        } else {
            frame.pc
        };
        let line = self
            .heap
            .closures
            .get(frame.closure)
            .and_then(|c| self.heap.protos.get(c.proto))
            .and_then(|p| p.debug.as_deref())
            .and_then(|d| d.lines.get(pc as usize))
            .copied();
        let cursor = (thread.frames.len() - 1, pc, line);
        if let Some(hook) = self
            .heap
            .threads
            .get_mut(active)
            .and_then(|t| self.heap.hooks.get_mut(t.id))
        {
            hook.old_pc = Some(cursor);
        }
        Ok(())
    }
    fn move_hook_results(
        &mut self,
        active: Handle<ThreadObj>,
        src: u32,
        dest: u32,
        produced: u32,
    ) -> Result<(), VmError> {
        if let Some(fault) = self.slot_fault(active, dest + produced)? {
            self.fault(fault);
            return Ok(());
        }
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        grow_stack(thread, (dest + produced) as usize, &mut self.heap.gc);
        thread
            .stack
            .copy_within(src as usize..(src + produced) as usize, dest as usize);
        thread.top = dest + produced;
        Ok(())
    }
    fn prepare_native_return_hook(
        &mut self,
        active: Handle<ThreadObj>,
        result: u32,
        produced: u32,
    ) -> Result<Poll, VmError> {
        let enabled = self.event_enabled(Event::Return);
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = thread.frames.last_mut().ok_or(VmError::Corrupt)?;
        let Some(Boundary::HookNative {
            phase,
            produced: n,
            result: r,
            ..
        }) = frame.boundary_mut()
        else {
            return Err(VmError::Corrupt);
        };
        *phase = 4;
        *n = produced;
        *r = result;
        let transfer = if produced == 0 {
            (0, 0)
        } else {
            (result - frame.base + 1, produced)
        };
        if enabled {
            self.heap
                .hooks
                .get_mut(thread.id)
                .ok_or(VmError::Corrupt)?
                .pending = Some(PendingEvent {
                event: Event::Return,
                line: None,
                frame: thread.frames.len() as u32 - 1,
                transfer,
                after: AfterHook::Continue,
            });
        }
        Ok(Poll::Continue)
    }
    /// A C boundary may have entered while hooks were off. Keep its raw
    /// result window until a newly enabled return hook has observed it.
    pub(super) fn wrap_native_return(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        wants: u8,
    ) -> Result<bool, VmError> {
        if !self.event_enabled(Event::Return) || self.hook_native_running() {
            return Ok(false);
        }
        self.enter_hook_native(0, Some((func, wants, 0)))?;
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        self.heap
            .hooks
            .get_mut(thread.id)
            .ok_or(VmError::Corrupt)?
            .pending = None;
        let Some(Boundary::HookNative { phase, .. }) =
            thread.frames.last_mut().and_then(|f| f.boundary_mut())
        else {
            return Err(VmError::Corrupt);
        };
        *phase = 2;
        Ok(true)
    }
    pub(super) fn capture_hook_native_return(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        wants: u8,
        passed: u32,
        values: &[Value],
    ) -> Result<bool, VmError> {
        let wrapped = self
            .heap
            .threads
            .get(active)
            .and_then(|t| t.frames.last())
            .is_some_and(|f| matches!(f.boundary(), Some(Boundary::HookNative { phase: 2, .. })));
        if !wrapped {
            if !self.event_enabled(Event::Return) {
                return Ok(false);
            }
            self.enter_hook_native(0, Some((func, wants, passed)))?;
            self.heap
                .threads
                .get_mut(active)
                .and_then(|t| self.heap.hooks.get_mut(t.id))
                .ok_or(VmError::Corrupt)?
                .pending = None;
        }
        let result = self.hook_native_result_slot(active, values.len() as u32)?;
        for (i, value) in values.iter().enumerate() {
            self.write_abs(active, result + i as u32, *value)?;
        }
        self.heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .top = result + values.len() as u32;
        self.prepare_native_return_hook(active, result, values.len() as u32)?;
        Ok(true)
    }
}

// Clone only this small boundary, without cloning task buffers in other variants.
trait HookCopy {
    fn copied_hook(self) -> Option<Boundary>;
}
impl HookCopy for Option<&Boundary> {
    fn copied_hook(self) -> Option<Boundary> {
        match self? {
            &Boundary::Hook {
                func,
                saved_top,
                target,
                instruction,
                after,
            } => Some(Boundary::Hook {
                func,
                saved_top,
                target,
                instruction,
                after,
            }),
            _ => None,
        }
    }
}

impl From<Event> for crate::HookEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::Call => Self::Call,
            Event::Return => Self::Return,
            Event::Line => Self::Line,
            Event::Count => Self::Count,
            Event::TailCall => Self::TailCall,
        }
    }
}

impl Runtime {
    fn api_hook_thread(&self, thread: Option<&crate::Value>) -> crate::Result<Handle<ThreadObj>> {
        match thread {
            None => self.heap.entry.ok_or_else(|| VmError::Corrupt.into()),
            Some(value) => match value.raw(self)? {
                Value::Thread(thread) => Ok(thread),
                _ => Err(crate::ApiError::WrongType.into()),
            },
        }
    }
    /// Install a registered host hook on the main thread, or a rooted thread.
    /// Count is an instruction interval; nonpositive values disable count events.
    pub fn set_hook(
        &mut self,
        thread: Option<&crate::Value>,
        symbol: &str,
        mask: crate::HookMask,
        count: i32,
    ) -> crate::Result<()> {
        let thread = self.api_hook_thread(thread)?;
        self.api_set_hook(thread, symbol, mask, count)
    }
    /// Clear the main thread's hook, or a rooted thread's hook.
    pub fn clear_hook(&mut self, thread: Option<&crate::Value>) -> crate::Result<()> {
        let thread = self.api_hook_thread(thread)?;
        self.api_clear_hook(thread)
    }
    /// Inspect the installed configuration, preserving target identity.
    pub fn get_hook(
        &mut self,
        thread: Option<&crate::Value>,
    ) -> crate::Result<Option<crate::HookSettings>> {
        let thread = self.api_hook_thread(thread)?;
        self.api_get_hook(thread)
    }
    pub(crate) fn api_get_hook(
        &mut self,
        thread: Handle<ThreadObj>,
    ) -> crate::Result<Option<crate::HookSettings>> {
        let id = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?.id;
        let Some(hook) = self.heap.hooks.get(id) else {
            return Ok(None);
        };
        let (target, mask, count) = (hook.target, hook.mask, hook.base_count);
        let function = match target {
            HookTarget::None => return Ok(None),
            HookTarget::InheritedLua => crate::HookFunction::InheritedLua,
            HookTarget::Lua(value) => crate::HookFunction::Lua(self.api_owned(value)?),
            HookTarget::Host(slot) => crate::HookFunction::Host(
                self.registry
                    .hook(slot)
                    .ok_or(VmError::Corrupt)?
                    .0
                    .to_owned(),
            ),
        };
        Ok(Some(crate::HookSettings {
            function,
            mask: crate::HookMask(mask & 7),
            count,
        }))
    }
    pub(crate) fn api_clear_hook(&mut self, thread: Handle<ThreadObj>) -> crate::Result<()> {
        let id = self
            .heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .id;
        if let Some(hook) = self.heap.hooks.get_mut(id) {
            if let Some(event) = hook.pending.take() {
                hook.after = event.after;
            }
            hook.target = HookTarget::None;
            hook.mask = 0;
            hook.base_count = 0;
            hook.remaining_count = 0;
        }
        self.refresh_hook_trap();
        Ok(())
    }
    pub(crate) fn api_set_hook(
        &mut self,
        thread: Handle<ThreadObj>,
        symbol: &str,
        mask: crate::HookMask,
        count: i32,
    ) -> crate::Result<()> {
        let slot = self
            .registry
            .hook_slot(symbol)
            .ok_or(crate::ApiError::UnknownSymbol)?;
        let mask = mask.0 | (u8::from(count > 0) * COUNT);
        if mask == 0 {
            return self.api_clear_hook(thread);
        }
        let id = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?.id;
        if self.heap.hooks.get(id).is_none() {
            if !self.in_callback {
                self.make_room(0, HOOK_BYTES);
            }
            self.ensure_room(HOOK_BYTES)
                .map_err(|e| self.api_error(e))?;
            self.heap.hooks.insert(
                id,
                HookState::new(thread, HookTarget::None, 0, 0, [Value::Nil; 5]),
            );
            self.heap
                .threads
                .get_mut(thread)
                .ok_or(VmError::Corrupt)?
                .charged_held += HOOK_BYTES;
            self.heap.gc.charge(HOOK_BYTES);
        }
        let cursor = self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames
            .iter()
            .enumerate()
            .rev()
            .find(|(_, frame)| frame.boundary().is_none())
            .map(|(depth, frame)| {
                let line = self
                    .heap
                    .closures
                    .get(frame.closure)
                    .and_then(|c| self.heap.protos.get(c.proto))
                    .and_then(|p| p.debug.as_deref())
                    .and_then(|d| d.lines.get(frame.pc as usize))
                    .copied();
                (depth, frame.pc, line)
            });
        let id = self
            .heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .id;
        let hook = self.heap.hooks.get_mut(id).ok_or(VmError::Corrupt)?;
        if hook
            .pending
            .is_some_and(|event| mask & event.event.mask() == 0)
        {
            hook.after = hook.pending.take().ok_or(VmError::Corrupt)?.after;
        }
        hook.target = HookTarget::Host(slot);
        hook.mask = mask;
        hook.base_count = count;
        hook.remaining_count = count;
        hook.old_pc = cursor;
        self.refresh_hook_trap();
        Ok(())
    }
    fn yield_host_hook(
        &mut self,
        active: Handle<ThreadObj>,
        event: Event,
    ) -> Result<Poll, VmError> {
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        if !matches!(event, Event::Line | Event::Count)
            || thread.closing
            || coroutine::blocks_yield(thread)
        {
            return Ok(self.fault(LuaFault::YieldAcross));
        }
        if self.heap.entry == Some(active) || !thread.coroutine {
            return Ok(self.fault_text(
                LuaFault::YieldAcross,
                b"attempt to yield from outside a coroutine".to_vec(),
            ));
        }
        let parent = thread.resumed_by.ok_or(VmError::Corrupt)?;
        let hook = self.heap.hooks.get_mut(thread.id).ok_or(VmError::Corrupt)?;
        hook.hook_yield = true;
        // Count yield skips the simultaneous line event, as luaG_traceexec does.
        if let Some((depth, pc, _)) = hook.instruction {
            hook.instruction = Some((depth, pc, 2));
        }
        self.deliver_to_parent(active, parent, Vec::new(), Status::LuaSuspended)
    }
}

impl Runtime {
    pub(crate) fn host_registry(&self) -> &HostRegistry {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot() -> Runtime {
        let chunk = crate::compile(b"local n=0 for i=1,25 do n=n+i end return n").unwrap();
        Runtime::boot(
            Config {
                fuel_limit: None,
                ..Config::default()
            },
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .unwrap()
    }

    #[test]
    fn inherited_host_symbol_resets_countdown_and_transient_state() {
        let mut runtime = boot();
        let parent = runtime.heap.active.unwrap();
        runtime
            .install_inherited_hook(
                parent,
                Some((
                    HookTarget::Host(77),
                    CALL | RETURN | COUNT,
                    9,
                    [Value::Nil; 5],
                )),
            )
            .unwrap();
        let id = runtime.heap.threads.get(parent).unwrap().id;
        let state = runtime.heap.hooks.get_mut(id).unwrap();
        state.remaining_count = 3;
        state.allow_hook = false;
        state.old_pc = Some((0, 1, Some(1)));
        let closure = runtime.heap.threads.get(parent).unwrap().frames[0].closure;
        let child = runtime.alloc_thread(closure, Status::LuaSuspended).unwrap();
        let id = runtime.heap.threads.get(child).unwrap().id;
        let state = runtime.heap.hooks.get(id).unwrap();
        assert!(matches!(state.target, HookTarget::Host(77)));
        assert_eq!(state.mask, CALL | RETURN | COUNT);
        assert_eq!((state.base_count, state.remaining_count), (9, 9));
        assert_eq!(state.owner, child);
        assert!(state.allow_hook);
        assert!(state.old_pc.is_none() && state.pending.is_none() && state.transfer.is_none());
        runtime.heap.active = Some(child);
        runtime.refresh_hook_trap();
        assert!(runtime.hook_trap);
        crate::gc::check_usage(&runtime.heap).unwrap();
    }

    #[test]
    fn inherited_lua_wrapper_traps_without_delivery_fuel() {
        let mut off = boot();
        let mut inherited = boot();
        let owner = inherited.heap.active.unwrap();
        inherited
            .install_inherited_hook(
                owner,
                Some((
                    HookTarget::InheritedLua,
                    CALL | RETURN | LINE | COUNT,
                    4,
                    [Value::Nil; 5],
                )),
            )
            .unwrap();
        inherited.refresh_hook_trap();
        assert!(inherited.hook_trap);
        assert!(!inherited.hooks_allowed());
        for runtime in [&mut off, &mut inherited] {
            let mut journal = Journal::new();
            while matches!(
                runtime.run(1, &mut journal).unwrap(),
                StepOutcome::Paused(_)
            ) {}
        }
        assert_eq!(off.results().unwrap(), inherited.results().unwrap());
        assert_eq!(off.fuel_consumed(), inherited.fuel_consumed());
        assert!(off.heap.hooks.0.is_none());
        let id = inherited.heap.threads.get(owner).unwrap().id;
        let state = inherited.heap.hooks.get(id).unwrap();
        assert!(state.old_pc.is_some());
        assert!((1..=4).contains(&state.remaining_count));
        assert!(state.pending.is_none());
    }
    #[test]
    fn hook_arguments_respect_quota_and_raise_catchable_errors() {
        for stack_limit in [false, true] {
            let source = "local ok,e=pcall(function() debug.sethook(type,'c') local function f() end f() end) debug.sethook() return ok,e";
            let chunk = crate::compile(source.as_bytes()).unwrap();
            let mut runtime = Runtime::boot(
                Config {
                    fuel_limit: None,
                    ..Config::default()
                },
                HostRegistry::proof(),
                &chunk.proto,
                false,
            )
            .unwrap();
            runtime.install_standard().unwrap();
            runtime.install_debug().unwrap();
            runtime.set_auto_gc(false);
            let mut journal = Journal::new();
            let mut found = false;
            for _ in 0..1000 {
                runtime.run(1, &mut journal).unwrap();
                let active = runtime.heap.active.unwrap();
                let thread = runtime.heap.threads.get(active).unwrap();
                if runtime
                    .heap
                    .hooks
                    .get(thread.id)
                    .is_some_and(|h| h.pending.is_some())
                {
                    let quota = runtime.heap.gc.quota;
                    let bound = runtime.max_stack_slots;
                    if stack_limit {
                        runtime.max_stack_slots = thread.stack.len() as u32;
                    } else {
                        runtime.heap.gc.quota = runtime.heap.gc.used;
                    }
                    assert!(runtime.run(1, &mut journal).is_ok());
                    assert!(runtime.heap.gc.used <= runtime.heap.gc.quota);
                    runtime.heap.gc.quota = quota;
                    runtime.max_stack_slots = bound;
                    for _ in 0..1000 {
                        if matches!(
                            runtime.run(1, &mut journal).unwrap(),
                            StepOutcome::Completed
                        ) {
                            break;
                        }
                    }
                    let expected = if stack_limit {
                        b"stack overflow".as_slice()
                    } else {
                        b"not enough memory".as_slice()
                    };
                    assert!(
                        matches!(runtime.results().unwrap().as_slice(), [crate::host::HostValue::Boolean(false), crate::host::HostValue::String(message)] if message.ends_with(expected))
                    );
                    found = true;
                    break;
                }
            }
            assert!(found);
        }
    }
}
