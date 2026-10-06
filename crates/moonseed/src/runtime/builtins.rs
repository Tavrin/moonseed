//! The base functions the VM implements (ADR 0024, ADR 0031): those that
//! raise errors, call Lua, make strings or native values, or act on the
//! collector or the output.
//!
//! A base function that calls a Lua value (`tostring` and `print` calling
//! `__tostring`, `pairs` calling `__pairs`, `ipairs`'s iterator calling
//! `__index`, `load` calling its reader) pushes a [`Boundary::Builtin`]
//! frame and makes the call above it, as `pcall` does. When the call
//! returns, the frame is on top again and [`Runtime::finish_builtin`] goes
//! on from the result. Nothing waits on the Rust stack, so the call may
//! pause, wait on the host, be checkpointed, and be collected around like
//! any other; whether a coroutine may yield across it is the frame's
//! task's (`Task::yieldable`), as in Lua 5.4.9.
//!
//! A base function that needs no Lua call returns at once, like a native.

use std::borrow::Cow;

use super::library::{Ctx, Next};
use super::*;
use crate::error::CompileErrorKind;
use crate::heap::{RESERVED_NAMES, Task};
use crate::host::Builtin;

/// The chunk name `load` gives a reader's chunk, as Lua's does.
const READER_CHUNKNAME: &[u8] = b"=(load)";

pub(super) enum LoadSource {
    Values(Vec<Value>),
}

/// Direct library callbacks build the ordinary Lua frame in one borrow.
/// Declines leave even the scratch window untouched: callable chains,
/// varargs and reserve/quota cases retain the original checks and faults.
#[inline(never)]
fn fast_builtin_call(
    thread: &mut ThreadObj,
    heap: &mut FrameHeap<'_>,
    function: Value,
    args: &[Value],
    slot: u32,
    wants: u8,
) -> bool {
    let Value::Closure(closure) = function else {
        return false;
    };
    let Some(proto) = heap
        .closures
        .get(closure)
        .and_then(|closure| heap.protos.get(closure.proto))
    else {
        return false;
    };
    if proto.vararg {
        return false;
    }
    let base = slot + 1;
    let scratch_end = base + args.len() as u32;
    let len = thread.stack.len() as u32;
    // Match the two slot_fault checks, before and after scratch growth: the
    // scratch part here, the window part once in the builder's checker.
    let scratch_charge =
        cost::STACK_SLOT * u64::from(scratch_end.saturating_sub(thread.charged_slots));
    if scratch_end > heap.ordinary
        || !heap
            .gc
            .fits(cost::STACK_SLOT * u64::from(scratch_end.saturating_sub(len)))
    {
        return false;
    }
    let Some(admit) = super::check_fixed_frame(
        thread.frames.depth,
        len.max(scratch_end) as usize,
        heap,
        proto,
        base,
        args.len() as u32,
        Some(scratch_charge),
    ) else {
        return false;
    };
    // write_abs charges each new scratch slot before enter_lua records
    // capacity growth. Preserve both the charge and the diagnostic counts.
    if scratch_end > len {
        thread.charge_slots(scratch_end as usize, heap.gc);
        #[cfg(feature = "counters")]
        for _ in len..scratch_end {
            count!("stack_growths");
        }
        thread.stack.grow_to(scratch_end as usize);
    }
    thread.stack[slot as usize] = function;
    thread.stack[base as usize..scratch_end as usize].copy_from_slice(args);
    super::write_fixed_frame(
        thread,
        heap,
        admit,
        closure,
        proto.params,
        base,
        args.len() as u32,
        wants,
        false,
    );
    true
}

/// Return into a library frame with the same result window as return_values.
/// Its suspended task stays canonical until the separately charged resume.
#[inline(never)]
pub(super) fn fast_builtin_return(thread: &mut ThreadObj, base: u8, count: u8) -> FrameStep {
    if thread.frames.len() < 2 {
        return FrameStep::Cold;
    }
    let frame = thread.frames.last().expect("callback callee");
    let caller = &thread.frames[thread.frames.len() - 2];
    if !matches!(
        caller.boundary(),
        Some(Boundary::Builtin { .. } | Boundary::Handler { .. })
    ) || caller.pending().is_some()
        || caller.meta().is_some()
        || thread.open_above > frame.base
        || thread.tbc.last().is_some_and(|slot| *slot >= frame.base)
    {
        return FrameStep::Cold;
    }
    // The common sort order callback returns exactly one Lua value. Its
    // canonical waiting task remains in the boundary; only the result
    // window is specialized, so a checkpoint before the charged resume
    // still sees the same stack and frame state.
    if frame.nresults == 1
        && count == 1
        && matches!(
            caller.boundary(),
            Some(Boundary::Builtin {
                task: Task::Lib(task),
                ..
            }) if matches!(task.wait, crate::library::Wait::Truth)
        )
    {
        let dest = frame.base.saturating_sub(frame.vararg_len + 1) as usize;
        let src = frame.base as usize + usize::from(base);
        let clear_from = dest + 1;
        let clear_end = (frame.limit as usize).max(clear_from);
        if src >= thread.stack.len()
            || clear_from > thread.stack.len()
            || clear_end > thread.stack.len()
        {
            return FrameStep::Cold;
        }
        let result = thread.stack[src];
        thread.frames.pop();
        thread.stack[dest] = result;
        thread.stack[clear_from..clear_end].fill(Value::Nil);
        let limit = thread
            .frames
            .iter()
            .rev()
            .find(|frame| frame.boundary().is_none())
            .map_or(0, |frame| frame.limit as usize);
        thread.stack.truncate(limit.max(clear_from));
        thread.top = clear_from as u32;
        return FrameStep::Continue;
    }
    let dest = frame.base.saturating_sub(frame.vararg_len + 1) as usize;
    let src = frame.base as usize + usize::from(base);
    let produced = if count == COUNT_OPEN {
        (thread.top as usize).saturating_sub(src)
    } else {
        usize::from(count)
    };
    let want = if frame.nresults == COUNT_OPEN {
        produced
    } else {
        usize::from(frame.nresults)
    };
    let clear_from = dest + if want == 0 { 1 } else { want };
    let clear_end = (frame.limit as usize).max(clear_from);
    if dest + want > thread.stack.len() || clear_end > thread.stack.len() {
        return FrameStep::Cold;
    }
    let copied = want.min(produced);
    // As in `fast_return`: a restored `top` past the stack's end declines.
    if src + copied > thread.stack.len() {
        return FrameStep::Cold;
    }
    if copied != 0 {
        thread.stack.copy_within(src..src + copied, dest);
    }
    thread.stack[dest + copied..dest + want].fill(Value::Nil);
    thread.frames.pop();
    thread.stack[clear_from..clear_end].fill(Value::Nil);
    // Boundary frames own no registers. Keep the Lua frame below them,
    // including when several nested library machines are suspended.
    let limit = thread
        .frames
        .iter()
        .rev()
        .find(|frame| frame.boundary().is_none())
        .map_or(0, |frame| frame.limit as usize);
    let new_top = dest + want;
    thread.stack.truncate(limit.max(new_top));
    thread.top = new_top as u32;
    FrameStep::Continue
}

/// A reserved name's index in [`RESERVED_NAMES`].
pub(super) fn type_index(value: Value) -> usize {
    match value {
        Value::Nil => 0,
        Value::Bool(_) => 1,
        Value::Integer(_) | Value::Float(_) => 2,
        Value::String(_) => 3,
        Value::Table(_) => 4,
        Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_) => 5,
        Value::Thread(_) => 6,
        Value::Userdata(_) | Value::LightUserdata(..) => 7,
    }
}

/// The text of a value that has no `__tostring`, as Lua's `luaL_tolstring`
/// gives it: a string itself, a number as `..` writes it, `nil`, `true`,
/// `false`, and for any other value its kind and a deterministic identity.
/// A table's kind is its metatable's `__name` when that is a string. Lua
/// prints an address; Moonseed prints the object's `ObjectId`, and a
/// native function's registry symbol, so the text is the same on every
/// target and across a checkpoint (ADR 0031).
pub(crate) fn plain_text(heap: &Heap, value: Value) -> Cow<'_, [u8]> {
    match value {
        Value::String(handle) => Cow::Borrowed(heap.string_bytes(handle).unwrap_or_default()),
        Value::Nil => Cow::Borrowed(b"nil"),
        Value::Bool(true) => Cow::Borrowed(b"true"),
        Value::Bool(false) => Cow::Borrowed(b"false"),
        Value::Integer(_) | Value::Float(_) => Cow::Owned(
            crate::concat::number_text(value)
                .unwrap_or_default()
                .into_bytes(),
        ),
        Value::Native(index) => {
            let symbol = heap.natives.get(index as usize).map_or("?", String::as_str);
            Cow::Owned(format!("function: builtin: {symbol}").into_bytes())
        }
        Value::Table(_)
        | Value::Closure(_)
        | Value::Thread(_)
        | Value::NativeClosure(_)
        | Value::Userdata(_)
        | Value::LightUserdata(..) => {
            let named = match index::metamethod(heap, value, b"__name") {
                Some(Value::String(handle)) => heap.string_bytes(handle),
                _ => None,
            };
            let mut text = match named {
                Some(name) => name.to_vec(),
                None => RESERVED_NAMES[type_index(value)].as_bytes().to_vec(),
            };
            text.extend_from_slice(b": ");
            text.extend_from_slice(&identity_text(heap, value));
            Cow::Owned(text)
        }
    }
}

/// The deterministic identity `tostring` and `%p` show where Lua shows an
/// address: an object's `ObjectId`, and a light userdata's token
/// (ADR 0043). A host key is 16 hex digits; a token the VM made is the id
/// of the cell it names, as an upvalue cell has no other text.
pub(crate) fn identity_text(heap: &Heap, value: Value) -> Vec<u8> {
    match value {
        Value::LightUserdata(crate::value::LightDomain::Host, bits) => {
            format!("0x{bits:016x}").into_bytes()
        }
        Value::LightUserdata(_, bits) => format!("0x{bits:08x}").into_bytes(),
        _ => {
            let id = heap.object_id_of_value(value).map_or(0, ObjectId::raw);
            format!("0x{id:08x}").into_bytes()
        }
    }
}

/// `luaL_tolstring`'s string or number argument: a string's bytes, or a
/// number's text.
pub(super) fn text_arg(heap: &Heap, value: Value) -> Option<Cow<'_, [u8]>> {
    match value {
        Value::String(handle) => heap.string_bytes(handle).map(Cow::Borrowed),
        Value::Integer(_) | Value::Float(_) => Some(plain_text(heap, value)),
        _ => None,
    }
}

/// `tonumber(s, base)`: Lua's `l_str2int`. Whitespace around, one sign,
/// then digits and letters below `base`; the value wraps as Lua's
/// unsigned arithmetic does.
fn integer_in_base(bytes: &[u8], base: u32) -> Option<i64> {
    let space = |byte: &u8| matches!(byte, b' ' | b'\x0c' | b'\n' | b'\r' | b'\t' | b'\x0b');
    let start = bytes.iter().position(|byte| !space(byte))?;
    let mut text = &bytes[start..];
    let negative = match text.first() {
        Some(b'-') => {
            text = &text[1..];
            true
        }
        Some(b'+') => {
            text = &text[1..];
            false
        }
        _ => false,
    };
    let digits = text
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric())
        .unwrap_or(text.len());
    if digits == 0 || !text[digits..].iter().all(space) {
        return None;
    }
    let mut value: u64 = 0;
    for byte in &text[..digits] {
        let digit = (*byte as char).to_digit(36)?;
        if digit >= base {
            return None;
        }
        value = value
            .wrapping_mul(u64::from(base))
            .wrapping_add(u64::from(digit));
    }
    let value = value as i64;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

impl Runtime {
    /// A base function the VM implements, called from the active frame's
    /// call site like any native.
    pub(super) fn call_base(
        &mut self,
        builtin: Builtin,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let site @ (func, _, passed, _) = self.call_site(active)?;
        let arg = |runtime: &Self, index: u32| -> Value {
            if index < passed {
                runtime
                    .heap
                    .threads
                    .get(active)
                    .and_then(|thread| thread.stack.get((func + 1 + index) as usize))
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            }
        };
        let present = |index: u32| index < passed && !matches!(arg(self, index), Value::Nil);
        match builtin {
            Builtin::Assert => {
                if passed == 0 {
                    return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                }
                if arg(self, 0).truthy() {
                    let mut values = std::mem::take(&mut self.native_results);
                    values.clear();
                    values.extend((0..passed).map(|index| arg(self, index)));
                    let result = self.base_return(active, &values);
                    values.clear();
                    self.native_results = values;
                    return result;
                }
                if passed >= 2 {
                    let mut message = arg(self, 1);
                    if let Value::String(handle) = message {
                        let text = self.heap.string_bytes(handle).unwrap_or_default().to_vec();
                        let location = self.location(active, 1).filter(|(_, line)| *line > 0);
                        message = self.prefixed(location, text, LuaFault::Error);
                    }
                    return Ok(self.throw_on(active, LuaFault::Error, message));
                }
                Ok(self.fault(LuaFault::Assert))
            }
            Builtin::Type => {
                if passed == 0 {
                    return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                }
                let name = self.reserved_name(type_index(arg(self, 0)))?;
                self.base_return(active, &[name])
            }
            Builtin::ToNumber => {
                let value = arg(self, 0);
                if !present(1) {
                    if passed == 0 {
                        return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                    }
                    let number = match value {
                        Value::Integer(_) | Value::Float(_) => value,
                        Value::String(handle) => self
                            .heap
                            .string_bytes(handle)
                            .and_then(crate::lex::string_to_number)
                            .unwrap_or(Value::Nil),
                        _ => Value::Nil,
                    };
                    return self.base_return(active, &[number]);
                }
                let Some(base) = crate::base::lua_integer(&self.heap, arg(self, 1)) else {
                    return self.base_error(active, |r, c| {
                        r.int_arg(c, 1)
                            .err()
                            .unwrap_or(Next::Fault(LuaFault::Argument))
                    });
                };
                let Value::String(handle) = value else {
                    return self.base_error(active, |r, c| r.bad_type(c, 0, "string"));
                };
                if !(2..=36).contains(&base) {
                    return self.base_error(active, |r, c| r.bad_arg(c, 1, "base out of range"));
                }
                let number = self
                    .heap
                    .string_bytes(handle)
                    .and_then(|bytes| integer_in_base(bytes, base as u32))
                    .map_or(Value::Nil, Value::Integer);
                self.base_return(active, &[number])
            }
            Builtin::ToString => {
                if passed == 0 {
                    return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                }
                let value = arg(self, 0);
                if let Some(method) = index::metamethod(&self.heap, value, b"__tostring") {
                    self.push_builtin(active, Task::ToString)?;
                    return self.builtin_call(method, &[value], journal);
                }
                let text = self.text_value(value)?;
                self.base_return(active, &[text])
            }
            Builtin::Os(function) => self.call_os(active, function, journal),
            Builtin::Exit => {
                let status = match arg(self, 0) {
                    Value::Nil | Value::Bool(true) => crate::ExitStatus::Success,
                    Value::Bool(false) => crate::ExitStatus::Failure,
                    value => {
                        let Some(code) = crate::base::lua_integer(&self.heap, value) else {
                            return self.base_error(active, |r, c| {
                                r.int_arg(c, 0)
                                    .err()
                                    .unwrap_or(Next::Fault(LuaFault::Argument))
                            });
                        };
                        crate::ExitStatus::Code(code as i32)
                    }
                };
                self.request_exit(status, arg(self, 1).truthy())
            }
            Builtin::Print => self.print_from(active, 0, 0, false, journal),
            // `luaB_warn`: every argument a string or a number, at least
            // one, sent as one warning in pieces; no `__tostring`.
            Builtin::Warn => {
                for index in 0..passed.max(1) {
                    if index >= passed || text_arg(&self.heap, arg(self, index)).is_none() {
                        return self.base_error(active, |r, c| r.bad_type(c, index, "string"));
                    }
                }
                let args: Vec<Value> = (0..passed).map(|index| arg(self, index)).collect();
                self.warn_effect(journal, |heap, piece| {
                    for value in &args {
                        piece(&text_arg(heap, *value).unwrap_or_default());
                    }
                });
                self.base_return(active, &[])
            }
            Builtin::Next => {
                let Value::Table(table) = arg(self, 0) else {
                    return self.base_error(active, |r, c| r.bad_type(c, 0, "table"));
                };
                let key = arg(self, 1);
                let normalized = if matches!(key, Value::Nil) {
                    None
                } else {
                    match self.heap.key_view(key) {
                        Ok(key) => Some(key),
                        Err(LuaFault::NanKey | LuaFault::NilKey) => {
                            return Ok(self.fault(LuaFault::NextKey));
                        }
                        Err(fault) => return Ok(self.fault(fault)),
                    }
                };
                let found = self
                    .heap
                    .tables
                    .get(table)
                    .ok_or(VmError::Corrupt)?
                    .table
                    .next_view(normalized);
                match found {
                    Ok(Some((key, value))) => self.base_return(active, &[key, value]),
                    // One nil at the end, as Lua returns.
                    Ok(None) => self.base_return(active, &[Value::Nil]),
                    Err(()) => Ok(self.fault(LuaFault::NextKey)),
                }
            }
            Builtin::Pairs => {
                if passed == 0 {
                    return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                }
                let table = arg(self, 0);
                if let Some(method) = index::metamethod(&self.heap, table, b"__pairs") {
                    self.push_builtin(active, Task::Pairs)?;
                    return self.builtin_call(method, &[table], journal);
                }
                let next = self.native_value("base.next")?;
                self.base_return(active, &[next, table, Value::Nil])
            }
            Builtin::Ipairs => {
                if passed == 0 {
                    return self.base_error(active, |r, c| r.bad_arg(c, 0, "value expected"));
                }
                let iterator = self.native_value(crate::base::IPAIRS_NEXT)?;
                self.base_return(active, &[iterator, arg(self, 0), Value::Integer(0)])
            }
            Builtin::IpairsNext => {
                let Some(index) = crate::base::lua_integer(&self.heap, arg(self, 1)) else {
                    return self.base_error(active, |r, c| {
                        r.int_arg(c, 1)
                            .err()
                            .unwrap_or(Next::Fault(LuaFault::Argument))
                    });
                };
                let index = index.wrapping_add(1);
                let obj = arg(self, 0);
                match index::get(&self.heap, obj, Value::Integer(index)) {
                    Err(fault) => {
                        let op = library::Op::Get {
                            obj,
                            key: Value::Integer(index),
                            into: 0,
                        };
                        if let Some(text) = self.library_op_message(fault, op) {
                            let error = self.prefixed(None, text, fault);
                            Ok(self.throw_on(active, fault, error))
                        } else {
                            Ok(self.fault(fault))
                        }
                    }
                    Ok(Resolved::Done(value)) => {
                        self.base_return(active, &ipairs_step(index, value))
                    }
                    Ok(Resolved::Call { function, target }) => {
                        self.push_builtin(active, Task::Ipairs { index })?;
                        self.builtin_call(function, &[target, Value::Integer(index)], journal)
                    }
                }
            }
            Builtin::CollectGarbage => self.collect_garbage(active, arg(self, 0)),
            Builtin::Load => self.load_call(active, passed, journal),
            Builtin::LoadFile | Builtin::DoFile => {
                self.start_file(active, builtin == Builtin::DoFile, journal)
            }
            Builtin::Math(function) => self.call_math(active, function, journal),
            Builtin::Table(function) => self.call_table(active, function, journal),
            Builtin::String(function) => self.call_string(active, function, journal),
            Builtin::Io(function) => self.call_io(active, function, journal),
            Builtin::Utf8(function) => self.call_utf8(active, function, journal),
            Builtin::Package(function) => self.call_package(active, function, journal),
            Builtin::Debug(function) => self.call_debug(active, function, journal),
            Builtin::Coroutine(function) => self.call_coroutine(active, function, site),
            #[cfg(test)]
            Builtin::CapabilitySmoke => Err(VmError::Corrupt),
            Builtin::Pcall | Builtin::Xpcall | Builtin::Error => Err(VmError::Corrupt),
        }
    }

    /// Raise the error `make` words for the base function at the active
    /// frame's call site, with the shared argument-error wording.
    fn base_error(
        &mut self,
        active: Handle<ThreadObj>,
        make: impl FnOnce(&Self, &Ctx) -> Next,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let next = make(self, &ctx);
        Ok(self.finish_next(active, next))
    }

    /// A reserved name from [`RESERVED_NAMES`], as a value.
    pub(super) fn reserved_name(&self, index: usize) -> Result<Value, VmError> {
        self.heap
            .reserved
            .get(LuaFault::ALL.len() + index)
            .map(|handle| Value::String(*handle))
            .ok_or(VmError::Corrupt)
    }

    /// `tostring` of a value with no `__tostring`: a string is itself; nil
    /// and the booleans are reserved strings; anything else is a new
    /// string.
    fn text_value(&mut self, value: Value) -> Result<Value, VmError> {
        match value {
            Value::String(_) => Ok(value),
            Value::Nil => self.reserved_name(0),
            Value::Bool(true) => self.reserved_name(8),
            Value::Bool(false) => self.reserved_name(9),
            _ => {
                let text = plain_text(&self.heap, value).into_owned();
                self.new_string(text)
            }
        }
    }

    /// A string made by a base function. Collects first when it would not
    /// fit: nothing is held outside the roots.
    pub(super) fn new_string(&mut self, bytes: Vec<u8>) -> Result<Value, VmError> {
        self.make_room(1, cost::OBJECT + bytes.len() as u64);
        Ok(Value::String(self.alloc_string(bytes)?))
    }

    /// The base function the active frame is calling returns `values`,
    /// with no boundary frame of its own: as a native's return.
    pub(super) fn base_return(
        &mut self,
        active: Handle<ThreadObj>,
        values: &[Value],
    ) -> Result<Poll, VmError> {
        let (func, nresults, passed, _) = self.call_site(active)?;
        self.native_returned(active, func, nresults, passed, values)
    }

    /// Push the frame of a base function that is about to call Lua: its
    /// call site is the active frame's current call.
    pub(super) fn push_builtin(
        &mut self,
        active: Handle<ThreadObj>,
        task: Task,
    ) -> Result<(), VmError> {
        let (func, nresults, passed, _) = self.call_site(active)?;
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let caller = object.frames.last().ok_or(VmError::Corrupt)?;
        let advance_caller = caller.boundary().is_none() && caller.meta().is_none();
        let closure = caller.closure;
        count!("frame_pushes");
        object.frames.push(Frame {
            closure,
            pc: 0,
            base: func + 1,
            limit: func + 1,
            nresults,
            vararg_len: 0,
            flags: 0,
            cold: FrameCold::with_boundary(
                Boundary::Builtin {
                    func,
                    passed,
                    advance_caller,
                    task,
                },
                &mut self.cold_spare,
            ),
        });
        Ok(())
    }

    /// The call slot of the base-function frame on top, above its
    /// arguments and scratch slots, and the results its task wants.
    pub(super) fn builtin_slot(&self, active: Handle<ThreadObj>) -> Result<(u32, u8), VmError> {
        let frame = self
            .heap
            .threads
            .get(active)
            .and_then(|thread| thread.frames.last())
            .ok_or(VmError::Corrupt)?;
        match frame.boundary() {
            Some(Boundary::Builtin {
                func, passed, task, ..
            }) => Ok((func + 1 + passed + task.scratch(), task.wants())),
            _ => Err(VmError::Corrupt),
        }
    }

    /// Call `function` with `args` from the base-function frame on top.
    /// Its results land at the frame's call slot, and the frame goes on in
    /// [`Self::finish_builtin`]. A value that cannot be called is an error
    /// raised from the frame.
    pub(super) fn builtin_call(
        &mut self,
        function: Value,
        args: &[Value],
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        let slot = self.builtin_slot(active)?;
        self.builtin_call_at(active, function, args, slot, journal)
    }

    /// A library machine knows this window from its saved work and wait.
    pub(super) fn builtin_call_at(
        &mut self,
        active: Handle<ThreadObj>,
        function: Value,
        args: &[Value],
        (slot, wants): (u32, u8),
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        if !self.hook_trap
            && matches!(function, Value::Closure(_))
            && self.try_fast_boundary_call(active, function, args, slot, wants)?
        {
            return Ok(Poll::Continue);
        }
        if let Some(fault) = self.depth_fault(active)? {
            return Ok(self.fault(fault));
        }
        if let Some(fault) = self.slot_fault(active, slot + 1 + args.len() as u32)? {
            return Ok(self.fault(fault));
        }
        self.write_abs(active, slot, function)?;
        for (offset, arg) in args.iter().enumerate() {
            self.write_abs(active, slot + 1 + offset as u32, *arg)?;
        }
        let (function, nargs) = match self.resolve_callable(active, slot, args.len() as u32)? {
            Ok(resolved) => resolved,
            Err(fault) => {
                let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                thread.stack.truncate(slot as usize);
                thread.top = slot;
                return Ok(self.fault(fault));
            }
        };
        self.heap
            .threads
            .get_mut(active)
            .ok_or(VmError::Corrupt)?
            .top = slot + 1 + nargs;
        match self.callable(function) {
            Value::Closure(closure) => self.push_lua_frame(closure, slot, nargs, wants, false),
            Value::Native(index) if self.is_builtin(index)? => self.defer_call(active),
            Value::Native(index) => self.call_native(index, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// Library and message-handler calls share fixed Lua frame setup;
    /// their saved boundaries still decide yieldability and completion.
    pub(super) fn try_fast_boundary_call(
        &mut self,
        active: Handle<ThreadObj>,
        function: Value,
        args: &[Value],
        slot: u32,
        wants: u8,
    ) -> Result<bool, VmError> {
        #[cfg(any(test, debug_assertions))]
        let fast = self.hot_core == HotCoreMode::Full;
        #[cfg(not(any(test, debug_assertions)))]
        let fast = true;
        if fast && matches!(function, Value::Closure(_)) {
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let mut heap = FrameHeap {
                closures: &self.heap.closures,
                protos: &self.heap.protos,
                gc: &mut self.heap.gc,
                ordinary: self.max_stack_slots - self.max_stack_slots / 8,
                stack_grows: &mut self.stack_grows,
                frame_grows: &mut self.frame_grows,
                cold_spare: &mut self.cold_spare,
            };
            if fast_builtin_call(thread, &mut heap, function, args, slot, wants) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Complete a library/handler continuation inside run_hot, with poll's
    /// priorities and fuel rules. Keep the ordinary instruction/frame epoch
    /// small; this helper runs only after its locals have been published.
    #[inline(never)]
    pub(super) fn finish_hot_boundary(
        &mut self,
        quantum: &mut u64,
        journal: &mut Journal,
    ) -> Result<Option<Poll>, VmError> {
        #[cfg(any(test, debug_assertions))]
        if self.hot_core != HotCoreMode::Full {
            return Ok(None);
        }
        let active = self.heap.active.ok_or(VmError::Corrupt)?;
        // Native/protected frames are common in embedding. Reject them
        // before checking the library resume's scheduler and fuel state.
        let Some(ready @ (Ready::Builtin | Ready::Handler)) = self.boundary_ready(active) else {
            return Ok(None);
        };
        if *quantum == 0
            || self.trap.is_some()
            || self
                .fuel_limit
                .is_some_and(|limit| self.fuel_consumed >= limit)
        {
            return Ok(None);
        }
        let thread = self.heap.threads.get(active).ok_or(VmError::Corrupt)?;
        if thread.status != Status::Ready
            || thread.unwind.is_some()
            || thread
                .frames
                .last()
                .is_some_and(|frame| frame.meta().is_some())
        {
            return Ok(None);
        }
        self.gc_schedule();
        if self.gc_wanted() || self.finalizer_due(active) {
            return Ok(None);
        }
        let step = match ready {
            Ready::Builtin => {
                self.fuel_consumed += 1;
                *quantum -= 1;
                self.finish_builtin(active, journal)
            }
            Ready::Handler => self.finish_handler(active),
            _ => Err(VmError::Corrupt),
        }
        .or_else(|error| self.vm_error(error))?;
        Ok(Some(step))
    }

    /// The base-function frame on top is done: it goes, and `values` are
    /// its call's results.
    pub(super) fn builtin_done(
        &mut self,
        active: Handle<ThreadObj>,
        values: &[Value],
    ) -> Result<Poll, VmError> {
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let scratch_end = u32::try_from(object.stack.len())
            .map_err(|_| VmError::Corrupt)?
            .max(object.top);
        let mut frame = object.frames.pop().ok_or(VmError::Corrupt)?;
        let Some(Boundary::Builtin {
            func,
            advance_caller,
            task,
            passed,
        }) = frame.cold.as_mut().and_then(|cold| cold.boundary.take())
        else {
            return Err(VmError::Corrupt);
        };
        let nresults = frame.nresults;
        if let Task::Lib(mut task) = task {
            // A spare may not retain a library machine (or any Values it
            // owns) after its boundary has left the traced heap.
            *task = crate::library::LibTask {
                work: crate::library::Work::Extreme {
                    max: false,
                    best: 0,
                    next: 0,
                },
                wait: crate::library::Wait::Nothing,
            };
            self.lib_task_spare = Some(task);
        }
        // The completed task must release all of its values before the box
        // can serve the next callback. The spare is outside the traced heap.
        frame.recycle_cold(&mut self.cold_spare);
        let produced = u32::try_from(values.len()).map_err(|_| VmError::Corrupt)?;
        if let Some(fault) = self.slot_fault(active, func + Self::wanted(nresults, produced))? {
            return Ok(self.fault(fault));
        }
        if self.hook_trap
            && self.capture_hook_native_return(active, func, nresults, passed, values)?
        {
            return Ok(Poll::Continue);
        }
        self.deliver_native(active, func, nresults, values, scratch_end)?;
        if advance_caller {
            self.bump_pc(active)?;
        }
        Ok(Poll::Continue)
    }

    /// The call a base-function frame made has returned, and the frame is
    /// on top: go on from its result. Not charged: the base function's
    /// call was.
    pub(super) fn finish_builtin(
        &mut self,
        active: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("builtin_resume_steps");
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let (slot, func, passed) = match object.frames.last().and_then(|frame| frame.boundary()) {
            Some(Boundary::Builtin {
                func, passed, task, ..
            }) => (func + 1 + passed + task.scratch(), *func, *passed),
            _ => return Err(VmError::Corrupt),
        };
        let result = |at: u32| object.stack.get(at as usize).copied().unwrap_or(Value::Nil);
        let first = result(slot);
        let results = [first, result(slot + 1), result(slot + 2)];
        let Some(Boundary::Builtin { task, .. }) = object
            .frames
            .last_mut()
            .and_then(|frame| frame.boundary_mut())
        else {
            return Err(VmError::Corrupt);
        };
        // The collector has done the work `collectgarbage` asked for: the
        // finalizers queued by now run above the frame, then it returns.
        if let Task::Collect {
            result,
            wait,
            ended,
            left,
        } = task
        {
            if *wait {
                *wait = false;
                // A step ends a cycle when the collector is left between
                // incremental cycles: never after a young collection, as
                // in Lua.
                let collector = &self.heap.collector;
                *ended = collector.phase == crate::gc::Phase::Pause && !collector.generational;
                *left = u32::try_from(self.heap.finalizers.pending.len()).unwrap_or(u32::MAX);
                if *left > 0 {
                    return Ok(Poll::Continue);
                }
            }
            let (result, ended) = (*result, *ended);
            let value = match result {
                1 => Value::Bool(ended),
                4 => Value::Bool(false),
                2 => self.new_string(b"incremental".to_vec())?,
                3 => self.new_string(b"generational".to_vec())?,
                _ => Value::Integer(0),
            };
            return self.builtin_done(active, &[value]);
        }
        // A library task goes on in place, without a copy.
        if let Task::Lib(task) = task {
            let saved = std::mem::replace(
                &mut **task,
                crate::library::LibTask {
                    work: crate::library::Work::Extreme {
                        max: false,
                        best: 0,
                        next: 0,
                    },
                    wait: crate::library::Wait::Nothing,
                },
            );
            return self.finish_lib(
                Ctx {
                    active,
                    func,
                    passed,
                    framed: true,
                },
                slot,
                first,
                results[1],
                saved,
                journal,
            );
        }
        if matches!(task, Task::HostLoad(_)) {
            return self.finish_hostload(active, journal);
        }
        if matches!(task, Task::DoFile) {
            let values = object.stack[slot as usize..object.top as usize].to_vec();
            return self.builtin_done(active, &values);
        }
        if let Task::Io(work) = task {
            let work = std::mem::replace(work, Box::new(crate::iolib::IoWork::Flush));
            return self.finish_io(
                Ctx {
                    active,
                    func,
                    passed,
                    framed: true,
                },
                slot,
                work,
                journal,
            );
        }
        // A load's source stays in the frame, where a collection counts
        // it, until the reader is done.
        let (task, read) = match task {
            Task::Load { source } => (Task::Load { source: Vec::new() }, source.len()),
            other => (other.clone(), 0),
        };
        match task {
            Task::HostLoad(_) | Task::DoFile | Task::Lib(_) | Task::Io(_) => Err(VmError::Corrupt),
            Task::Collect { .. } => Err(VmError::Corrupt),
            Task::ToString => match first {
                Value::String(_) | Value::Integer(_) | Value::Float(_) => {
                    let text = self.text_value(first)?;
                    self.builtin_done(active, &[text])
                }
                _ => Ok(self.fault(LuaFault::ToString)),
            },
            Task::Pairs => self.builtin_done(active, &results),
            Task::Ipairs { index } => self.builtin_done(active, &ipairs_step(index, first)),
            Task::Print { next } => {
                if !matches!(
                    first,
                    Value::String(_) | Value::Integer(_) | Value::Float(_)
                ) {
                    return Ok(self.fault(LuaFault::ToString));
                }
                // The argument's text replaces it; the call's slots go.
                let (func, _, _, _) = self.call_site_below(active)?;
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                object.stack[(func + 1 + next) as usize] = first;
                object.stack.truncate(slot as usize);
                object.top = slot;
                self.print_from(active, next + 1, next, true, journal)
            }
            Task::Load { .. } => {
                let piece = match first {
                    Value::Nil => None,
                    value => match text_arg(&self.heap, value) {
                        Some(piece) if piece.is_empty() => None,
                        Some(piece) => Some(piece.into_owned()),
                        None => return Ok(self.fault(LuaFault::Reader)),
                    },
                };
                let Some(piece) = piece else {
                    let source = self.take_source(active)?;
                    let (func, _, passed, _) = self.call_site_below(active)?;
                    return match self.load_source(active, func, passed, &source, true)? {
                        LoadSource::Values(values) => self.builtin_done(active, &values),
                    };
                };
                if read == 0 {
                    let (func, _, passed, _) = self.call_site_below(active)?;
                    if let Some(message) =
                        self.mode_failure(active, func, passed, piece.first().copied())?
                    {
                        return self.builtin_done(active, &[Value::Nil, message]);
                    }
                }
                if read + piece.len() > crate::limits::DEFAULT_SOURCE_BYTES {
                    let message = self.new_string(
                        format!(
                            "source exceeds {} bytes",
                            crate::limits::DEFAULT_SOURCE_BYTES
                        )
                        .into_bytes(),
                    )?;
                    return self.builtin_done(active, &[Value::Nil, message]);
                }
                // The source is charged to the logical heap while it is
                // read, and counted by every collection (ADR 0031).
                let charge = piece.len() as u64;
                self.make_room(0, charge);
                if !self.heap.gc.fits(charge) {
                    return Ok(self.fault(LuaFault::Memory));
                }
                self.heap.charge_held(charge);
                let mut source = self.take_source(active)?;
                source.extend_from_slice(&piece);
                self.put_source(active, source)?;
                let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                object.stack.truncate(slot as usize);
                object.top = slot;
                let (func, _, _, _) = self.call_site_below(active)?;
                let reader = self
                    .heap
                    .threads
                    .get(active)
                    .and_then(|thread| thread.stack.get(func as usize + 1))
                    .copied()
                    .unwrap_or(Value::Nil);
                self.builtin_call(reader, &[], journal)
            }
        }
    }

    /// Take a load's source out of its frame.
    fn take_source(&mut self, active: Handle<ThreadObj>) -> Result<Vec<u8>, VmError> {
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        match object
            .frames
            .last_mut()
            .and_then(|frame| frame.boundary_mut())
        {
            Some(Boundary::Builtin {
                task: Task::Load { source },
                ..
            }) => Ok(std::mem::take(source)),
            _ => Err(VmError::Corrupt),
        }
    }

    /// Where the base-function frame on top was called: its `func` and
    /// argument count, as `call_site` gives them for the frame below.
    pub(super) fn call_site_below(
        &self,
        active: Handle<ThreadObj>,
    ) -> Result<(u32, u8, u32, Value), VmError> {
        let frame = self
            .heap
            .threads
            .get(active)
            .and_then(|thread| thread.frames.last())
            .ok_or(VmError::Corrupt)?;
        match frame.boundary() {
            Some(Boundary::Builtin { func, passed, .. }) => {
                Ok((*func, frame.nresults, *passed, Value::Nil))
            }
            _ => Err(VmError::Corrupt),
        }
    }

    /// Put a load's source back in its frame.
    fn put_source(&mut self, active: Handle<ThreadObj>, source: Vec<u8>) -> Result<(), VmError> {
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        match object
            .frames
            .last_mut()
            .and_then(|frame| frame.boundary_mut())
        {
            Some(Boundary::Builtin {
                task: Task::Load { source: slot },
                ..
            }) => {
                *slot = source;
                Ok(())
            }
            _ => Err(VmError::Corrupt),
        }
    }

    /// An error stopped at the `load` frame on top (ADR 0031): `load`
    /// returns nil and the error object, as Lua's does for an error in its
    /// reader.
    pub(super) fn finish_load_error(
        &mut self,
        active: Handle<ThreadObj>,
        error: Value,
    ) -> Result<Poll, VmError> {
        self.builtin_done(active, &[Value::Nil, error])
    }

    /// `print(...)` from argument `next` on. Arguments with no
    /// `__tostring` are written together, as one effect, with the tab
    /// before each but the first and the newline after the last. Before a
    /// `__tostring` call, what comes before its argument is written, so
    /// output and errors come in Lua's order: `print(1, bad)` writes `1`
    /// before `bad`'s error, and a `__tostring` that prints writes after
    /// the arguments before it. `framed` is true when the `print` frame is
    /// already on top.
    fn print_from(
        &mut self,
        active: Handle<ThreadObj>,
        mut next: u32,
        written: u32,
        framed: bool,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = if framed {
            self.call_site_below(active)?
        } else {
            self.call_site(active)?
        };
        let arg = |runtime: &Self, index: u32| {
            runtime
                .heap
                .threads
                .get(active)
                .and_then(|thread| thread.stack.get((func + 1 + index) as usize))
                .copied()
                .unwrap_or(Value::Nil)
        };
        while next < passed {
            let value = arg(self, next);
            if let Some(method) = index::metamethod(&self.heap, value, b"__tostring") {
                self.write_print(func, written, next, false, journal);
                let task = Task::Print { next };
                if framed {
                    let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
                    match object
                        .frames
                        .last_mut()
                        .and_then(|frame| frame.boundary_mut())
                    {
                        Some(Boundary::Builtin { task: slot, .. }) => *slot = task,
                        _ => return Err(VmError::Corrupt),
                    }
                } else {
                    self.push_builtin(active, task)?;
                }
                return self.builtin_call(method, &[value], journal);
            }
            next += 1;
        }
        self.write_print(func, written, passed, true, journal);
        if framed {
            self.builtin_done(active, &[])
        } else {
            self.base_return(active, &[])
        }
    }

    /// Write `print`'s arguments `from..to` of the call at `func`, and the
    /// newline when `end`, as one external effect. The effect id is the
    /// next sequence number, and the journal commits it, so a replay of
    /// the same id writes nothing again (ADR 0031). The journal records
    /// the number of arguments written. The text goes to the output in
    /// pieces: small ones gathered up to 64 KiB, a longer string on its
    /// own, so host memory stays bounded by the longest string.
    fn write_print(&mut self, func: u32, from: u32, to: u32, end: bool, journal: &mut Journal) {
        const GATHER: usize = 64 * 1024;
        if from == to && !end {
            return;
        }
        let Some(active) = self.heap.active else {
            return;
        };
        let effect = EffectId {
            domain: self.effect_domain,
            sequence: self.next_sequence,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        // A panicking sink leaves the runtime poisoned, as a native's would.
        let outer = std::mem::replace(&mut self.in_callback, true);
        let heap = &self.heap;
        let output = &mut self.output;
        let _ = journal.commit(effect, i64::from(to - from), || {
            let Some(output) = output else {
                return 0;
            };
            let mut gathered = Vec::new();
            let stack = heap
                .threads
                .get(active)
                .map_or(&[][..], |thread| thread.stack.as_slice());
            for index in from..to {
                if index > 0 {
                    gathered.push(b'\t');
                }
                let value = stack
                    .get((func + 1 + index) as usize)
                    .copied()
                    .unwrap_or(Value::Nil);
                let text = plain_text(heap, value);
                if gathered.len() + text.len() > GATHER {
                    if !gathered.is_empty() {
                        output(&gathered);
                        gathered.clear();
                    }
                    if text.len() > GATHER {
                        output(&text);
                        continue;
                    }
                }
                gathered.extend_from_slice(&text);
            }
            if end {
                gathered.push(b'\n');
            }
            if !gathered.is_empty() {
                output(&gathered);
            }
            0
        });
        self.in_callback = outer;
    }

    /// `collectgarbage(opt [, ...])` over the incremental collector
    /// (ADR 0031, ADR 0050). `collect` and `step` do their work in bounded,
    /// charged units before they return; `count` is the logical heap in
    /// KiB, not allocator memory; the tuning options are Lua 5.4's;
    /// `generational` is an error until Moonseed has that mode.
    fn collect_garbage(
        &mut self,
        active: Handle<ThreadObj>,
        option: Value,
    ) -> Result<Poll, VmError> {
        let name: Vec<u8> = match option {
            Value::Nil => b"collect".to_vec(),
            Value::String(handle) => self.heap.string_bytes(handle).unwrap_or_default().to_vec(),
            _ => return self.base_error(active, |r, c| r.bad_type(c, 0, "string")),
        };
        const OPTIONS: [&[u8]; 10] = [
            b"collect",
            b"step",
            b"count",
            b"stop",
            b"restart",
            b"isrunning",
            b"incremental",
            b"generational",
            b"setpause",
            b"setstepmul",
        ];
        // Inside a finalizer every option fails, as Lua's `lua_gc` does
        // while the collector is stopped for one.
        let name = name.as_slice();
        if OPTIONS.contains(&name) && self.heap.finalizers.running {
            return self.base_return(active, &[Value::Nil]);
        }
        // An explicit collection sees the stack as Lua's does from a C
        // function: nothing above the call's arguments is live, so stale
        // temporaries there keep nothing alive (ADR 0046).
        if matches!(name, b"collect" | b"step") {
            let (func, _, passed, _) = self.call_site(active)?;
            let thread = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
            let live = ((func + 1 + passed) as usize).min(thread.stack.len());
            thread.stack[live..].fill(Value::Nil);
        }
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        // `luaL_optinteger`, made the C `int` `lua_gc` takes.
        let int = |runtime: &Self, index: u32| {
            runtime
                .opt_int_arg(&ctx, index, 0)
                .map(|value| value as i32)
        };
        let waiting = |result| Task::Collect {
            result,
            wait: true,
            ended: false,
            left: 0,
        };
        let result = match name {
            // A full collection, done in bounded units; the finalizers it
            // queued run before `collectgarbage` returns (ADR 0048,
            // ADR 0050).
            b"collect" => {
                crate::gc::request_full(&mut self.heap);
                self.push_builtin(active, waiting(0))?;
                return Ok(Poll::Continue);
            }
            // One step (`lua_gc(LUA_GCSTEP)`): 0 does a basic step; `n`
            // adds `n` KiB to the allocation the next step is due after,
            // and steps if one is now due. True when a cycle ended. In
            // generational form a step is a whole young collection, or a
            // whole major one when due, as Lua's (ADR 0051).
            b"step" => {
                let n = match int(self, 1) {
                    Ok(n) => n,
                    Err(next) => return Ok(self.finish_next(active, next)),
                };
                let gc = &mut self.heap.gc;
                gc.sched = if n == 0 {
                    0
                } else {
                    gc.sched.saturating_add(i64::from(n) * 1024)
                };
                if n != 0 && gc.sched <= 0 {
                    Value::Bool(false)
                } else {
                    let generational =
                        crate::gc::step(&mut self.heap, u64::MAX, self.max_objects, true);
                    self.push_builtin(active, waiting(if generational { 4 } else { 1 }))?;
                    return Ok(Poll::Continue);
                }
            }
            b"count" => Value::Float(self.heap.gc.used as f64 / 1024.0),
            b"stop" => {
                self.heap.gc.auto = false;
                Value::Integer(0)
            }
            b"restart" => {
                self.heap.gc.sched = 0;
                self.heap.gc.auto = true;
                Value::Integer(0)
            }
            b"isrunning" => Value::Bool(self.heap.gc.auto),
            // Incremental mode; 0 leaves a parameter as it is. The previous
            // mode is returned. Leaving generational form makes the old
            // objects white again, in the steps that follow (ADR 0051).
            b"incremental" => {
                let mut parameters = [0i32; 3];
                for (index, parameter) in parameters.iter_mut().enumerate() {
                    match int(self, 1 + index as u32) {
                        Ok(value) => *parameter = value,
                        Err(next) => return Ok(self.finish_next(active, next)),
                    }
                }
                let [pause, stepmul, stepsize] = parameters;
                let gc = &mut self.heap.gc;
                if pause != 0 {
                    gc.pause = (pause / 4) as u8;
                }
                if stepmul != 0 {
                    gc.stepmul = (stepmul / 4) as u8;
                }
                if stepsize != 0 {
                    gc.stepsize = stepsize as u8;
                }
                let previous = gc.generational;
                gc.generational = false;
                gc.bad = 0;
                let collector = &mut self.heap.collector;
                if collector.generational && !collector.to_old {
                    crate::gc::leave_gen(&mut self.heap);
                } else {
                    // A cycle running finishes as an incremental one; a
                    // sweep making survivors old is followed by leaving.
                    collector.decide = crate::gc::Decide::None;
                }
                let name: &[u8] = if previous {
                    b"generational"
                } else {
                    b"incremental"
                };
                self.new_string(name.to_vec())?
            }
            // Lua 5.4's deprecated setters: the previous value, as stored.
            b"setpause" | b"setstepmul" => {
                let value = match int(self, 1) {
                    Ok(value) => value,
                    Err(next) => return Ok(self.finish_next(active, next)),
                };
                let gc = &mut self.heap.gc;
                let field = if name == b"setpause" {
                    &mut gc.pause
                } else {
                    &mut gc.stepmul
                };
                let previous = i64::from(*field) * 4;
                *field = (value / 4) as u8;
                Value::Integer(previous)
            }
            // Generational mode (`lua_gc(LUA_GCGEN)`): the minor multiplier
            // as a byte, the major one divided by four, 0 leaving either as
            // it is. Entering generational form is a full collection that
            // makes what survives old, done before it returns (ADR 0051).
            b"generational" => {
                let mut parameters = [0i32; 2];
                for (index, parameter) in parameters.iter_mut().enumerate() {
                    match int(self, 1 + index as u32) {
                        Ok(value) => *parameter = value,
                        Err(next) => return Ok(self.finish_next(active, next)),
                    }
                }
                let [minormul, majormul] = parameters;
                let gc = &mut self.heap.gc;
                if minormul != 0 {
                    gc.minormul = minormul as u8;
                }
                if majormul != 0 {
                    gc.majormul = (majormul / 4) as u8;
                }
                let previous = gc.generational;
                let falling_back = gc.bad > 0;
                gc.generational = true;
                gc.bad = 0;
                // Already generational, out of generational form only for a
                // major collection running: nothing to enter, as in Lua.
                if !self.heap.collector.generational && (!previous || falling_back) {
                    crate::gc::request_full(&mut self.heap);
                    // A cycle running enters generational form too.
                    if self.heap.collector.phase != crate::gc::Phase::Pause {
                        self.heap.collector.decide = crate::gc::Decide::ToGen;
                    }
                    self.push_builtin(active, waiting(if previous { 3 } else { 2 }))?;
                    return Ok(Poll::Continue);
                }
                let name: &[u8] = if previous {
                    b"generational"
                } else {
                    b"incremental"
                };
                self.new_string(name.to_vec())?
            }
            other => {
                let message = format!("invalid option '{}'", String::from_utf8_lossy(other));
                return self.base_error(active, |r, c| r.bad_arg(c, 0, &message));
            }
        };
        self.base_return(active, &[result])
    }

    /// `load(chunk [, chunkname [, mode [, env]]])` for text chunks
    /// (ADR 0031). A string (or number) is compiled now; a function is
    /// called for pieces until it returns nil or an empty string, from a
    /// `load` frame that keeps what it has read.
    fn load_call(
        &mut self,
        active: Handle<ThreadObj>,
        passed: u32,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, _, _) = self.call_site(active)?;
        let arg = |runtime: &Self, index: u32| -> Value {
            if index < passed {
                runtime
                    .heap
                    .threads
                    .get(active)
                    .and_then(|thread| thread.stack.get((func + 1 + index) as usize))
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            }
        };
        // The chunk name and mode, when given, are strings or numbers.
        for index in [1, 2] {
            let value = arg(self, index);
            if !matches!(value, Value::Nil) && text_arg(&self.heap, value).is_none() {
                return self.base_error(active, |r, c| r.bad_type(c, index, "string"));
            }
        }
        let chunk = arg(self, 0);
        if let Some(source) = text_arg(&self.heap, chunk) {
            let source = source.into_owned();
            return match self.load_source(active, func, passed, &source, false)? {
                LoadSource::Values(values) => self.base_return(active, &values),
            };
        }
        if !chunk.is_function() {
            return self.base_error(active, |r, c| r.bad_type(c, 0, "function"));
        }
        let reader = chunk;
        self.push_builtin(active, Task::Load { source: Vec::new() })?;
        self.builtin_call(reader, &[], journal)
    }

    /// The message `load` fails with when its mode does not allow a chunk
    /// starting with `first`, or `None` when it does. Lua checks this on
    /// the chunk's first byte, before reading on, and so does a reader's
    /// `load`.
    fn mode_failure(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        passed: u32,
        first: Option<u8>,
    ) -> Result<Option<Value>, VmError> {
        let given = if passed > 2 {
            self.heap
                .threads
                .get(active)
                .and_then(|thread| thread.stack.get((func + 3) as usize))
                .copied()
                .unwrap_or(Value::Nil)
        } else {
            Value::Nil
        };
        self.load_mode_failure(given, first)
    }

    fn load_mode_failure(
        &mut self,
        given: Value,
        first: Option<u8>,
    ) -> Result<Option<Value>, VmError> {
        // Lua reads the mode as a C string: it ends at a zero byte.
        let mut mode = text_arg(&self.heap, given).map_or_else(|| b"bt".to_vec(), Cow::into_owned);
        if let Some(end) = mode.iter().position(|byte| *byte == 0) {
            mode.truncate(end);
        }
        let binary = first == Some(0x1b);
        let (kind, letter) = if binary {
            ("binary", b'b')
        } else {
            ("text", b't')
        };
        let message = if !mode.contains(&letter) {
            let mut message = format!("attempt to load a {kind} chunk (mode is '").into_bytes();
            message.extend_from_slice(&mode);
            message.extend_from_slice(b"')");
            message
        } else {
            return Ok(None);
        };
        self.new_string(message).map(Some)
    }

    /// Compile `source` for the `load` call at `func` and make its
    /// function: the function, nil and a message, or a thrown parser stack
    /// overflow (as `luaE_incCstack` raises through `load`).
    fn load_source(
        &mut self,
        active: Handle<ThreadObj>,
        func: u32,
        passed: u32,
        source: &[u8],
        from_reader: bool,
    ) -> Result<LoadSource, VmError> {
        let arg = |runtime: &Self, index: u32| -> Value {
            if index < passed {
                runtime
                    .heap
                    .threads
                    .get(active)
                    .and_then(|thread| thread.stack.get((func + 1 + index) as usize))
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            }
        };
        let text = |runtime: &Self, index: u32| -> Option<Vec<u8>> {
            text_arg(&runtime.heap, arg(runtime, index)).map(Cow::into_owned)
        };
        let name = text(self, 1).map(Cow::Owned).unwrap_or_else(|| {
            Cow::Borrowed(if from_reader {
                READER_CHUNKNAME
            } else {
                source
            })
        });
        let env = if passed >= 4 {
            arg(self, 3)
        } else {
            Value::Table(self.heap.globals.ok_or(VmError::Corrupt)?)
        };
        let chunk_name = match (arg(self, 1), arg(self, 0)) {
            (Value::String(handle), _) => ChunkName::Object(handle),
            (Value::Nil, Value::String(handle)) if !from_reader => ChunkName::Object(handle),
            _ => ChunkName::Bytes(name.to_vec()),
        };
        self.load_bytes(active, source, &name, arg(self, 2), env, chunk_name)
    }

    pub(super) fn load_bytes(
        &mut self,
        active: Handle<ThreadObj>,
        source: &[u8],
        name: &[u8],
        mode: Value,
        env: Value,
        chunk_name: ChunkName,
    ) -> Result<LoadSource, VmError> {
        if let Some(message) = self.load_mode_failure(mode, source.first().copied())? {
            return Ok(LoadSource::Values(vec![Value::Nil, message]));
        }
        // A binary chunk (ADR 0036): Lua's escape byte first.
        let spec = if source.first() == Some(&0x1b) {
            // What decoding may make: twice the heap's room, as the code
            // is charged to the heap once installed, and the chunk itself.
            let budget = self
                .heap
                .gc
                .headroom()
                .saturating_mul(2)
                .saturating_add(source.len() as u64);
            match crate::chunk::undump(source, budget) {
                Ok(spec) => spec,
                Err(refusal) => {
                    // `lundump.c` names the chunk this way.
                    let name = name.to_vec();
                    let mut message = match name.first() {
                        Some(b'@' | b'=') => name[1..].to_vec(),
                        Some(0x1b) => b"binary string".to_vec(),
                        _ => name,
                    };
                    message.extend_from_slice(b": ");
                    message.extend_from_slice(refusal.as_bytes());
                    return Ok(LoadSource::Values(vec![
                        Value::Nil,
                        self.new_string(message)?,
                    ]));
                }
            }
        } else {
            let c_frames = self
                .heap
                .threads
                .get(active)
                .map(|thread| {
                    thread
                        .frames
                        .iter()
                        .filter(|frame| {
                            matches!(
                                frame.cold.as_ref().and_then(|cold| cold.boundary.as_ref()),
                                Some(Boundary::Protect { .. })
                            )
                        })
                        .count()
                })
                .unwrap_or(0);
            let c_frames = u32::try_from(c_frames).unwrap_or(u32::MAX);
            match crate::compile::compile_for_load(source, c_frames, self.heap.gc.headroom()) {
                Ok(chunk) => chunk.proto,
                Err(error) => {
                    if error.kind == CompileErrorKind::Limit
                        && (error.message == "C stack overflow"
                            || error.message == crate::limits::COMPILE_MEMORY)
                    {
                        let message = self.new_string(error.message.as_bytes().to_vec())?;
                        return Ok(LoadSource::Values(vec![Value::Nil, message]));
                    }
                    let rendered = error.render_bounded(name, self.heap.max_string);
                    let message = match rendered {
                        Some(bytes) => match self.new_string(bytes) {
                            Ok(value) => value,
                            Err(VmError::MemoryLimit) => self.fault_value(LuaFault::Memory),
                            Err(other) => return Err(other),
                        },
                        None => self.fault_value(LuaFault::Memory),
                    };
                    return Ok(LoadSource::Values(vec![Value::Nil, message]));
                }
            }
        };
        let chunk_name = if source.first() == Some(&0x1b) {
            ChunkName::Unnamed
        } else {
            chunk_name
        };
        match self.instantiate(&spec, env, chunk_name) {
            Ok(closure) => Ok(LoadSource::Values(vec![Value::Closure(closure)])),
            Err(VmError::MemoryLimit) => Ok(LoadSource::Values(vec![
                Value::Nil,
                self.fault_value(LuaFault::Memory),
            ])),
            Err(error) => Err(error),
        }
    }
}

/// `ipairs`'s iterator's results for `t[index]`: nothing more to iterate
/// is one nil, which ends a generic `for`.
fn ipairs_step(index: i64, value: Value) -> Vec<Value> {
    if matches!(value, Value::Nil) {
        vec![Value::Nil]
    } else {
        vec![Value::Integer(index), value]
    }
}

#[cfg(test)]
mod tests {
    use super::integer_in_base;
    use crate::chunkname::chunk_id;

    #[test]
    fn chunk_ids_match_lua() {
        assert_eq!(chunk_id(b"=name"), b"name");
        assert_eq!(chunk_id(b"@file.lua"), b"file.lua");
        assert_eq!(chunk_id(b"x = "), b"[string \"x = \"]");
        assert_eq!(
            chunk_id(b"#!shebang\nreturn 1"),
            b"[string \"#!shebang...\"]"
        );
        let long = [b'a'; 80];
        assert_eq!(chunk_id(&long).len(), 9 + 45 + 3 + 2);
        let mut file = b"@".to_vec();
        file.extend_from_slice(&[b'f'; 80]);
        assert_eq!(chunk_id(&file).len(), 59);
    }

    #[test]
    fn integers_in_a_base_follow_l_str2int() {
        assert_eq!(integer_in_base(b"ff", 16), Some(255));
        assert_eq!(integer_in_base(b" -101 ", 2), Some(-5));
        assert_eq!(integer_in_base(b"zz", 36), Some(1295));
        assert_eq!(integer_in_base(b"ffffffffffffffff", 16), Some(-1));
        assert_eq!(integer_in_base(b"10000000000000000", 16), Some(0));
        for bad in [&b""[..], b"-", b"1.5", b"0x10", b"2", b"1\0", b"+ 1"] {
            assert_eq!(
                integer_in_base(bad, if bad == b"2" { 2 } else { 16 }),
                None,
                "{bad:?}"
            );
        }
    }
}
