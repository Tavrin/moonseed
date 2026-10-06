//! One cold boundary for every host capability operation. Builtin work calls
//! this again with the same request after a wait, before advancing its state.
use super::*;
use crate::hostcaps::protocol::{decode, encode};
use crate::hostcaps::{
    CapabilityPoll, CapabilityRequest, CapabilityValue, Completion, EffectClass, HostIoError,
    HostIoErrorKind,
};

impl Runtime {
    pub(super) fn capability_builtin_ready(&self, active: Handle<ThreadObj>) -> bool {
        self.heap
            .threads
            .get(active)
            .and_then(|thread| thread.frames.last())
            .is_some_and(|frame| {
                matches!(
                    frame.pending(),
                    Some(Pending::Capability {
                        completed: true,
                        ..
                    })
                ) && matches!(frame.boundary(), Some(Boundary::Builtin { .. }))
            })
    }

    /// Invoke one bounded capability operation through the journal. Both reads
    /// and mutations record exact request bytes and final success/failure.
    /// Replay mismatches are VM errors and never invoke the capability.
    ///
    /// Builtins must retain their work phase on Waiting, return to the executor,
    /// and re-enter this helper with the same request after completion. Completion
    /// resumes that phase, without delivering Lua results or moving any cursor.
    /// This helper requires an active ready thread/frame; it performs no fuel
    /// charge. Each read/write request is limited to 64 KiB. Host panics become
    /// structured errors. Exactly-once means journal deduplication, not atomic
    /// crash durability of an external mutation and the embedder's journal.
    pub fn capability(
        &mut self,
        request: &CapabilityRequest,
        journal: &mut Journal,
    ) -> Result<CapabilityPoll, VmError> {
        self.settle_atomic();
        let active = self.heap.active.ok_or(VmError::NotRunnable)?;
        let object = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last().ok_or(VmError::NotRunnable)?;
        let retained = match frame.pending() {
            Some(Pending::Capability {
                sequence,
                wait_key,
                completed,
            }) => Some((*sequence, *wait_key, *completed)),
            None => None,
            _ => return Err(VmError::NotRunnable),
        };
        if !request.bounded() {
            return Err(VmError::Api(crate::ApiError::InvalidCallState));
        }
        if request.encoded_len() > self.heap.max_string {
            return Err(VmError::MemoryLimit);
        }
        let bytes = request.request_bytes();
        let arg = match request.class() {
            EffectClass::JournaledRead => 0,
            EffectClass::ExactlyOnceMutation => 1,
        };
        if let Some((sequence, key, completed)) = retained {
            let wait = frame.wait_request().ok_or(VmError::Corrupt)?;
            if wait.operation != request.operation()
                || wait.payload.len() != 3
                || self.capability_request_bytes(active)? != bytes
            {
                return Err(VmError::Corrupt);
            }
            if !completed {
                return Ok(CapabilityPoll::Waiting(WaitKey(key)));
            }
            let Value::String(value) = wait.payload[2] else {
                return Err(VmError::Corrupt);
            };
            let outcome = self
                .heap
                .string_bytes(value)
                .ok_or(VmError::Corrupt)?
                .to_vec();
            let result = decode(&outcome)?;
            if !request.valid_result(&result) {
                return Err(VmError::Corrupt);
            }
            let id = EffectId {
                domain: self.effect_domain,
                sequence,
            };
            let committed = journal
                .commit_request(id, arg, &bytes, || outcome)
                .map_err(|_| VmError::Corrupt)?;
            let result = decode(&committed)?;
            if !request.valid_result(&result) {
                return Err(VmError::Corrupt);
            }
            self.clear_wait_request(active);
            self.heap
                .threads
                .get_mut(active)
                .ok_or(VmError::Corrupt)?
                .frames
                .last_mut()
                .ok_or(VmError::Corrupt)?
                .set_pending(None, &mut self.cold_spare);
            return Ok(CapabilityPoll::Ready(result));
        }
        if object.status != Status::Ready {
            return Err(VmError::NotRunnable);
        }
        let sequence = self.next_sequence;
        if sequence >= 1 << 63 {
            return Err(VmError::Corrupt);
        }
        let id = EffectId {
            domain: self.effect_domain,
            sequence,
        };
        if let Some(outcome) = journal
            .replay_request::<Vec<u8>>(id, arg, &bytes)
            .map_err(|_| VmError::Corrupt)?
        {
            let result = decode(&outcome)?;
            if !request.valid_result(&result) {
                return Err(VmError::Corrupt);
            }
            self.next_sequence += 1;
            return Ok(CapabilityPoll::Ready(result));
        }
        // Prepare rooted request storage before invoking a host mutation. There
        // is no Lua allocation between a Ready result and its journal commit.
        let request_raw = Value::String(self.alloc_string(bytes.clone())?);
        let _request_root = self.api_owned(request_raw).map_err(execution::api_vm)?;
        let held = request.operation().len() as u64 + 3 * cost::STACK_SLOT;
        self.ensure_room(held)?;
        self.api_roots();
        let thread_root = self
            .api_owned(Value::Thread(active))
            .map_err(execution::api_vm)?;
        self.next_sequence += 1;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            request.invoke(&self.host_capabilities)
        }))
        .unwrap_or_else(|_| {
            Completion::Ready(Err(HostIoError::new(
                HostIoErrorKind::Other,
                b"host capability panicked".to_vec(),
            )))
        });
        match result {
            Completion::Ready(result) => {
                if !request.valid_result(&result) {
                    return Err(VmError::Corrupt);
                }
                let committed = journal
                    .commit_request(id, arg, &bytes, || encode(&result))
                    .map_err(|_| VmError::Corrupt)?;
                Ok(CapabilityPoll::Ready(decode(&committed)?))
            }
            Completion::Pending(token) => {
                let key = sequence | (1 << 63);
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
                frame.set_pending(
                    Some(Pending::Capability {
                        sequence,
                        wait_key: key,
                        completed: false,
                    }),
                    &mut self.cold_spare,
                );
                frame.set_wait_request(
                    Some(crate::heap::HostWait {
                        operation: request.operation().to_owned(),
                        payload: vec![request_raw, Value::Integer(token.0 as i64), Value::Nil],
                    }),
                    &mut self.cold_spare,
                );
                object.charged_held += held;
                object.status = Status::Waiting;
                self.heap.gc.charge(held);
                self.wait_index.insert(key, active);
                self.wait_roots.insert(key, (thread_root, true));
                self.last_completed_wait = None;
                Ok(CapabilityPoll::Waiting(WaitKey(key)))
            }
        }
    }
    fn capability_request_bytes(&self, thread: Handle<ThreadObj>) -> Result<&[u8], VmError> {
        let wait = self
            .heap
            .threads
            .get(thread)
            .and_then(|t| t.frames.last())
            .and_then(|f| f.wait_request())
            .ok_or(VmError::Corrupt)?;
        let Some(Value::String(request)) = wait.payload.first() else {
            return Err(VmError::Corrupt);
        };
        self.heap.string_bytes(*request).ok_or(VmError::Corrupt)
    }
    /// Complete a capability wait once, retaining the typed final outcome in
    /// checkpointed VM state. This does not deliver Lua results: the builtin
    /// resumes its work phase and calls `capability` again, which commits the
    /// outcome to its supplied journal without invoking host code. A snapshot
    /// taken between completion and that step retains the outcome. Wrong types,
    /// oversized results, and duplicate/unknown keys are rejected transactionally.
    pub fn complete_capability(
        &mut self,
        key: WaitKey,
        result: Result<CapabilityValue, HostIoError>,
    ) -> crate::Result<()> {
        if self.completed_waits.contains(&key.raw()) || self.last_completed_wait == Some(key.raw())
        {
            return Err(crate::ApiError::AlreadyCompleted.into());
        }
        let thread = *self
            .wait_index
            .get(&key.raw())
            .ok_or(crate::ApiError::NotWaiting)?;
        let frame = self
            .heap
            .threads
            .get(thread)
            .and_then(|t| t.frames.last())
            .ok_or(VmError::Corrupt)?;
        let Some(Pending::Capability {
            sequence,
            wait_key,
            completed: false,
        }) = frame.pending()
        else {
            return Err(crate::ApiError::InvalidCallState.into());
        };
        let (sequence, wait_key) = (*sequence, *wait_key);
        if wait_key != key.raw() {
            return Err(VmError::Corrupt.into());
        }
        let request = CapabilityRequest::from_bytes(self.capability_request_bytes(thread)?)?;
        if !request.valid_result(&result) {
            return Err(crate::ApiError::InvalidCallState.into());
        }
        let value = self.create_string(encode(&result))?;
        let raw = crate::api::Value::String(value).raw(self)?;
        let object = self.heap.threads.get_mut(thread).ok_or(VmError::Corrupt)?;
        let frame = object.frames.last_mut().ok_or(VmError::Corrupt)?;
        frame
            .cold
            .as_mut()
            .and_then(|c| c.wait_request.as_mut())
            .ok_or(VmError::Corrupt)?
            .payload[2] = raw;
        frame.set_pending(
            Some(Pending::Capability {
                sequence,
                wait_key,
                completed: true,
            }),
            &mut self.cold_spare,
        );
        object.status = Status::Ready;
        self.heap.active = Some(thread);
        self.wait_finished(key);
        Ok(())
    }
}
