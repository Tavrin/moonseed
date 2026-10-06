//! Running finalizers (ADR 0047, ADR 0048) and closing the runtime.
//!
//! A collection only queues the objects whose finalizers are due
//! (`gc.rs`). They run here, one per step, as calls the VM makes like any
//! other: `__gc(object)` in a [`Boundary::Finalizer`] frame pushed on the
//! active thread at an instruction boundary, above everything the frame
//! below holds. Its results are dropped; an error stops at the frame and
//! becomes a warning; no yield crosses it; no collection starts while it
//! runs. A host wait inside it waits like any other, and a checkpoint
//! holds the frame like any other.
//!
//! Timing (Moonseed's, not a Lua promise): every finalizer a collection
//! queues runs, in queue order, before the interrupted code takes another
//! step; `collectgarbage("collect")` returns after them.

use super::*;
use crate::heap::{Boundary, ExitPhase, ExitState, Task};

/// What closing may allocate past the limits: the frame closure's
/// prototype and closure, and their logical bytes.
pub(crate) const CLOSE_OBJECTS: u32 = 2;
const CLOSE_BYTES: u64 = 1024;

impl Runtime {
    /// Whether the next step starts a finalizer: one is queued, none is
    /// running, and the active thread is where a call can start without
    /// disturbing what it was doing: a Lua frame between instructions,
    /// `collectgarbage`'s frame waiting for the finalizers, or, while the
    /// runtime closes, no frame at all.
    pub(super) fn finalizer_due(&self, active: Handle<ThreadObj>) -> bool {
        let finalizers = &self.heap.finalizers;
        if finalizers.pending.is_empty() || finalizers.running {
            return false;
        }
        let Some(thread) = self.heap.threads.get(active) else {
            return false;
        };
        if thread.status != Status::Ready || thread.unwind.is_some() {
            return false;
        }
        match thread.frames.last() {
            None => finalizers.closing,
            Some(frame) => {
                frame.pending().is_none()
                    && frame.targets().is_empty()
                    && frame.meta().is_none()
                    && match frame.boundary() {
                        None => true,
                        Some(Boundary::Builtin {
                            task: Task::Collect { left, .. },
                            ..
                        }) => *left > 0,
                        Some(_) => false,
                    }
            }
        }
    }

    /// Take the next queued object and call its `__gc`, looked up now
    /// from its current metatable (Lua's `GCTM`). The object may be
    /// registered again from here on. Without a `__gc` nothing is called.
    pub(super) fn start_finalizer(
        &mut self,
        active: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let fin = self
            .heap
            .finalizers
            .pending
            .pop_front()
            .ok_or(VmError::Corrupt)?;
        self.heap.unmark_finalize(fin);
        // One of the finalizers `collectgarbage` waits for.
        if let Some(Boundary::Builtin {
            task: Task::Collect { left, .. },
            ..
        }) = self
            .heap
            .threads
            .get_mut(active)
            .and_then(|thread| thread.frames.last_mut())
            .and_then(|frame| frame.boundary_mut())
        {
            *left = left.saturating_sub(1);
        }
        let object = fin.value();
        let Some(gc) = index::metamethod(&self.heap, object, b"__gc") else {
            self.finish_closing(active)?;
            return Ok(Poll::Continue);
        };
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let saved_top = thread.top;
        let (closure, func) = match thread.frames.last() {
            Some(frame) => (frame.closure, frame.limit.max(saved_top)),
            // Closing a runtime whose run has ended: the frame needs a
            // closure, as every frame does, and runs none.
            None => {
                let func = saved_top.max(u32::try_from(thread.stack.len()).unwrap_or(u32::MAX));
                let closure = self.heap.finalizers.close_closure.ok_or(VmError::Corrupt)?;
                (closure, func)
            }
        };
        let object_thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        count!("frame_pushes");
        object_thread.frames.push(Frame {
            closure,
            pc: 0,
            // The frame's call is at `func` itself, so its own base is
            // there too, below the callee's arguments.
            base: func,
            limit: func,
            nresults: 0,
            vararg_len: 0,
            flags: 0,
            cold: FrameCold::with_boundary(
                Boundary::Finalizer { func, saved_top },
                &mut self.cold_spare,
            ),
        });
        self.heap.finalizers.running = true;
        // From here an error stops at the frame just pushed.
        if let Some(fault) = self.depth_fault(active)? {
            return Ok(self.fault(fault));
        }
        if let Some(fault) = self.slot_fault(active, func + 2)? {
            return Ok(self.fault(fault));
        }
        self.write_abs(active, func, gc)?;
        self.write_abs(active, func + 1, object)?;
        let (function, nargs) = match self.resolve_callable(active, func, 1)? {
            Ok(resolved) => resolved,
            Err(fault) => return Ok(self.fault(fault)),
        };
        self.heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .top = func + 1 + nargs;
        match self.callable(function) {
            Value::Closure(closure) => self.push_lua_frame(closure, func, nargs, 0, false),
            Value::Native(index) if self.is_builtin(index)? => self.defer_call(active),
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// The finalizer frame on top is done: it goes, its slots go, `top`
    /// is what it was, and an error it stopped is a warning:
    /// `error in __gc (message)`, the message a string error's text, as
    /// Lua's `luaE_warnerror` gives it.
    pub(super) fn finish_finalizer(
        &mut self,
        active: Handle<ThreadObj>,
        error: Option<Value>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.pop().ok_or(VmError::Corrupt)?;
        let Some(&Boundary::Finalizer { func, saved_top }) = frame.boundary() else {
            return Err(VmError::Corrupt);
        };
        object.stack.truncate(func as usize);
        object.top = saved_top;
        self.heap.finalizers.running = false;
        // What the finalizer allocated does not make the next automatic
        // collection due at once: the interrupted code gets at least the
        // smallest debt of its own first, so a finalizer that makes
        // garbage and registers its object again cannot take every step
        // (ADR 0048).
        let gc = &mut self.heap.gc;
        gc.sched = gc.sched.min(-(gc.min_debt.min(i64::MAX as u64) as i64));
        if let Some(error) = error {
            self.warn_effect(journal, |heap, piece| {
                piece(b"error in ");
                piece(b"__gc");
                piece(b" (");
                match error {
                    Value::String(handle) => piece(heap.string_bytes(handle).unwrap_or_default()),
                    _ => piece(b"error object is not a string"),
                }
                piece(b")");
            });
        }
        self.finish_closing(active)?;
        Ok(Poll::Continue)
    }

    /// While the runtime closes on a thread whose run had ended, the
    /// thread goes back to how it ended once the queue is empty.
    fn finish_closing(&mut self, active: Handle<ThreadObj>) -> Result<(), VmError> {
        let Some(status) = self.heap.finalizers.closed else {
            return Ok(());
        };
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        if thread.frames.is_empty() && self.heap.finalizers.pending.is_empty() {
            thread.status = status;
            self.heap.finalizers.closed = None;
            self.heap.finalizers.close_closure = None;
            if let Some(exit) = &mut self.heap.finalizers.exit {
                exit.phase = ExitPhase::Terminal;
            }
        }
        Ok(())
    }

    /// Start an uncatchable exit from the OS builtin. No host process is killed.
    pub(super) fn request_exit(
        &mut self,
        status: crate::ExitStatus,
        close: bool,
    ) -> Result<Poll, VmError> {
        self.heap.finalizers.exit = Some(ExitState {
            status,
            close,
            phase: if close {
                ExitPhase::Scopes
            } else {
                ExitPhase::Terminal
            },
        });
        if !close {
            return Ok(Poll::Stop(StepOutcome::ExitRequested { status, close }));
        }
        // lua_close always redirects to the main thread. Other threads keep
        // their pending close variables; shutdown does not call their __close.
        let main = self.heap.entry.ok_or(VmError::Corrupt)?;
        self.heap.active = Some(main);
        let object = self.heap.threads.get_mut(main).ok_or(VmError::Corrupt)?;
        object.status = Status::Ready;
        object.error = None;
        object.unwind = Some(Box::new(crate::heap::Unwind {
            error: None,
            phase: UnwindPhase::Popping { target: None },
        }));
        // Abandon pending hooks, including an exit requested from a hook.
        if let Some(hook) = self.heap.hooks.get_mut(object.id) {
            hook.allow_hook = true;
            hook.pending = None;
            hook.after = hooks::AfterHook::Continue;
            hook.transfer = None;
            hook.restore_cursor = None;
            hook.hook_yield = false;
        }
        self.refresh_hook_trap();
        Ok(Poll::Continue)
    }

    pub(super) fn exit_closing_scopes(&self, thread: Handle<ThreadObj>) -> bool {
        self.heap.entry == Some(thread)
            && self
                .heap
                .finalizers
                .exit
                .is_some_and(|exit| exit.phase == ExitPhase::Scopes)
    }

    /// Main-thread closes finished. luaD_closeprotected passes each close
    /// error to the remaining closes and discards the last error at shutdown;
    /// only __gc errors warn. Queue finalizers after those closes have run.
    pub(super) fn finish_exit_scopes(&mut self, main: Handle<ThreadObj>) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(main).ok_or(VmError::Corrupt)?;
        object.unwind = None;
        object.stack.clear();
        object.top = 0;
        object.host_results.clear();
        object.status = Status::Completed;
        self.heap.finalizers.running = false;
        // A nested exit from __gc may restart main-thread closing, while
        // the finalizer queue is already in shutdown mode.
        if self.heap.finalizers.closing {
            self.heap.finalizers.closed = Some(Status::Completed);
            self.heap
                .threads
                .get_mut(main)
                .ok_or(VmError::Corrupt)?
                .status = Status::Ready;
            self.finish_closing(main)?;
        } else {
            self.begin_close()?;
        }
        let pending = self.heap.finalizers.closed.is_some();
        let exit = self.heap.finalizers.exit.as_mut().ok_or(VmError::Corrupt)?;
        exit.phase = if pending {
            ExitPhase::Finalizers
        } else {
            ExitPhase::Terminal
        };
        Ok(Poll::Continue)
    }

    /// Begin closing the runtime, as `lua_close` does before it frees the
    /// state (ADR 0048): every object registered for finalization joins
    /// the queue, after those already in it, newest registration first,
    /// and nothing registers from now on. [`Runtime::run`] then runs the
    /// finalizers, with their errors as warnings, until it returns the
    /// run's own end again; dropping the runtime afterwards frees the rest,
    /// and only then do host values' Rust `Drop`s run. Lua's `__gc`
    /// semantics at shutdown need this call: dropping a runtime runs no
    /// Lua. The run must have ended (completed or failed). Unstable API.
    pub fn begin_close(&mut self) -> Result<(), VmError> {
        if self.heap.finalizers.closing {
            return Ok(());
        }
        if self.trap.is_some() || self.heap.active != self.heap.entry {
            return Err(VmError::NotRunnable);
        }
        let active = self.heap.active.ok_or(VmError::NotRunnable)?;
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        if !matches!(thread.status, Status::Completed | Status::Failed) || !thread.frames.is_empty()
        {
            return Err(VmError::NotRunnable);
        }
        // One closure for every finalizer frame the close makes, made now,
        // after a collection, so the close itself never runs out of room.
        let close_closure = if self.heap.finalizers.registered.is_empty()
            && self.heap.finalizers.pending.is_empty()
        {
            None
        } else {
            // Closing must not fail for room: the closure (a prototype and
            // a closure) may pass the object limit and the quota by its
            // own size.
            self.collect();
            let (objects, quota) = (self.max_objects, self.heap.gc.quota);
            self.max_objects = objects.saturating_add(CLOSE_OBJECTS);
            self.heap.gc.quota = quota.saturating_add(CLOSE_BYTES);
            let closure = self.trampoline();
            self.max_objects = objects;
            self.heap.gc.quota = quota;
            Some(closure?)
        };
        let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let finalizers = &mut self.heap.finalizers;
        finalizers.close_closure = close_closure;
        finalizers.closing = true;
        let registered = std::mem::take(&mut finalizers.registered);
        finalizers.old_until = 0;
        finalizers.new_from = 0;
        finalizers.pending.extend(registered.into_iter().rev());
        if !finalizers.pending.is_empty() {
            finalizers.closed = Some(thread.status);
            thread.status = Status::Ready;
        }
        Ok(())
    }

    /// Whether [`Runtime::begin_close`] has run. Unstable API.
    pub fn is_closing(&self) -> bool {
        self.heap.finalizers.closing
    }
}
