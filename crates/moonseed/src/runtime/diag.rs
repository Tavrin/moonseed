//! Cold diagnostic construction. The interpreter calls this only after an
//! instruction or builtin has already failed.

use super::*;
use crate::chunkname::chunk_id;
use crate::debuginfo::NameKind;

pub(super) fn source_line(short_src: &[u8], line: Option<i64>) -> Vec<u8> {
    let mut result = short_src.to_vec();
    if let Some(line) = line {
        result.extend_from_slice(format!(":{line}").as_bytes());
    }
    result
}

pub(super) fn prefix(location: &(Vec<u8>, i64)) -> Vec<u8> {
    let mut result = source_line(&location.0, Some(location.1));
    result.extend_from_slice(b": ");
    result
}

impl Runtime {
    /// Lua's logical level zero is the running native function. A Lua frame
    /// at level one is its caller, as in debug.getinfo and luaL_where.
    pub(super) fn location(
        &self,
        thread: Handle<ThreadObj>,
        level: usize,
    ) -> Option<(Vec<u8>, i64)> {
        let (func, _, passed, _) = self.call_site(thread).ok()?;
        let ctx = library::Ctx {
            active: thread,
            func,
            passed,
            framed: false,
        };
        let levels = self.levels(&ctx, thread).ok()?;
        let selected = levels.get(level)?;
        let index = selected.frame?;
        self.frame_location(thread, index)
    }

    pub(super) fn frame_location(
        &self,
        thread: Handle<ThreadObj>,
        index: usize,
    ) -> Option<(Vec<u8>, i64)> {
        let frames = &self.heap.threads.get(thread)?.frames;
        let frame = frames.get(index)?;
        if frame.boundary().is_some() {
            return None;
        }
        let proto = self.closure_proto(frame.closure).ok()?;
        let source = proto
            .source
            .and_then(|handle| self.heap.string_bytes(handle))
            .unwrap_or(b"=?");
        let pc = super::debug::current_pc(frames, index);
        let line = proto
            .debug
            .as_deref()
            .and_then(|debug| debug.lines.get(pc as usize))
            .map_or(-1, |line| i64::from(*line));
        Some((chunk_id(source), line))
    }

    pub(super) fn where_prefix(&self, thread: Handle<ThreadObj>) -> Vec<u8> {
        self.heap
            .threads
            .get(thread)
            .and_then(|object| object.frames.len().checked_sub(1))
            .and_then(|index| self.frame_location(thread, index))
            .filter(|(_, line)| *line > 0)
            .map_or_else(Vec::new, |location| prefix(&location))
    }

    pub(super) fn nearest_lua_location(&self, thread: Handle<ThreadObj>) -> Option<(Vec<u8>, i64)> {
        let frames = &self.heap.threads.get(thread)?.frames;
        let index = frames.len().checked_sub(1)?;
        let index = match frames[index].boundary() {
            Some(
                Boundary::Protect { .. } | Boundary::Handler { .. } | Boundary::Finalizer { .. },
            ) => {
                return None;
            }
            Some(Boundary::Builtin { .. } | Boundary::Native { .. }) => index.checked_sub(1)?,
            Some(Boundary::Hook { .. } | Boundary::HookNative { .. }) => index.checked_sub(1)?,
            None => index,
        };
        self.frame_location(thread, index)
            .filter(|(_, line)| *line > 0)
    }

    /// Allocate a complete diagnostic or use the class's reserved string.
    /// In particular, a memory fault never attempts this allocation.
    pub(super) fn prefixed(
        &mut self,
        location: Option<(Vec<u8>, i64)>,
        message: Vec<u8>,
        fault: LuaFault,
    ) -> Value {
        if fault == LuaFault::Memory {
            return self.fault_value(fault);
        }
        let mut text = location.map_or_else(Vec::new, |location| prefix(&location));
        if text
            .len()
            .checked_add(message.len())
            .is_none_or(|len| len > self.heap.max_string)
        {
            return self.fault_value(fault);
        }
        text.extend_from_slice(&message);
        self.alloc_string(text)
            .map_or_else(|_| self.fault_value(fault), Value::String)
    }

    fn diagnostic_type(&self, value: Value) -> Vec<u8> {
        if let Some(mt) = self.heap.metatable_of(value)
            && let Some(Value::String(name)) =
                self.heap.table_get_view(mt, KeyView::string(b"__name"))
            && let Some(bytes) = self.heap.string_bytes(name)
        {
            return bytes.to_vec();
        }
        crate::heap::type_name(value).as_bytes().to_vec()
    }

    /// A library machine faults on its own operands, not on the Lua opcode
    /// that called the library. PUC reports these C-side errors without a
    /// Lua source position or register provenance.
    pub(super) fn library_op_message(&self, fault: LuaFault, op: library::Op) -> Option<Vec<u8>> {
        let (action, value) = match (fault, op) {
            (LuaFault::Index, library::Op::Get { obj, .. } | library::Op::Set { obj, .. }) => {
                (b"index".as_slice(), obj)
            }
            (LuaFault::Length, library::Op::Len { obj }) => (b"get length of".as_slice(), obj),
            (LuaFault::BadCall, library::Op::Order { f, .. }) => (b"call".as_slice(), f),
            (LuaFault::Compare, library::Op::Less { a, b }) => {
                let left = self.diagnostic_type(a);
                let right = self.diagnostic_type(b);
                let mut text = if left == right {
                    b"attempt to compare two ".to_vec()
                } else {
                    b"attempt to compare ".to_vec()
                };
                text.extend_from_slice(&left);
                text.extend_from_slice(if left == right { b" values" } else { b" with " });
                if left != right {
                    text.extend_from_slice(&right);
                }
                return Some(text);
            }
            _ => return None,
        };
        let mut text = b"attempt to ".to_vec();
        text.extend_from_slice(action);
        text.extend_from_slice(b" a ");
        text.extend_from_slice(&self.diagnostic_type(value));
        text.extend_from_slice(b" value");
        Some(text)
    }

    fn register_name(
        &self,
        proto: &Proto,
        mut pc: u32,
        mut reg: u8,
    ) -> Option<(NameKind, Vec<u8>)> {
        let debug = proto.debug.as_deref()?;
        // Receiver setup and other temporary copies preserve provenance.
        // Walk backwards in instruction order so repeated copies cannot recurse.
        let (writer, op) = loop {
            if let Some(local) = debug
                .locals
                .iter()
                .rev()
                .find(|local| local.reg == reg && local.start <= pc && pc < local.end)
            {
                return Some((NameKind::Local, local.name.clone()));
            }
            let (writer, op) = self.reg_writer(proto, pc as usize, reg)?;
            if let Op::Move { src, .. } = *op {
                pc = writer as u32;
                reg = src;
            } else {
                break (writer, op);
            }
        };
        match *op {
            Op::GetUpvalue { index, .. } if usize::from(index) < proto.captures.len() => Some((
                NameKind::Upvalue,
                debug
                    .upvalues
                    .get(index as usize)
                    .cloned()
                    .unwrap_or_else(|| b"?".to_vec()),
            )),
            Op::GetField { obj, name, .. } => {
                let key = proto.byte_consts.get(name as usize)?.clone();
                let local = debug.locals.iter().rev().find(|local| {
                    local.reg == obj && local.start <= writer as u32 && (writer as u32) < local.end
                });
                let env = local.map_or_else(
                    || {
                        matches!(self.reg_writer(proto, writer, obj),
                    Some((_, Op::GetUpvalue { index, .. }))
                    if debug.upvalues.get(*index as usize).is_some_and(|name| name == b"_ENV"))
                    },
                    |local| local.name == b"_ENV",
                );
                Some((
                    if env {
                        NameKind::Global
                    } else {
                        NameKind::Field
                    },
                    key,
                ))
            }
            Op::GetTable { key, .. } | Op::Index { key, .. } => {
                let key_is_local = debug.locals.iter().any(|local| {
                    local.reg == key && local.start <= writer as u32 && (writer as u32) < local.end
                });
                let name = match self.reg_writer(proto, writer, key).map(|(_, op)| op) {
                    _ if key_is_local => b"?".to_vec(),
                    Some(Op::LoadBytes { const_index, .. }) => {
                        proto.byte_consts.get(*const_index as usize)?.clone()
                    }
                    Some(Op::LoadInt { .. }) => b"integer index".to_vec(),
                    _ => b"?".to_vec(),
                };
                Some((NameKind::Field, name))
            }
            Op::LoadBytes { const_index, .. } => proto
                .byte_consts
                .get(const_index as usize)
                .cloned()
                .map(|name| (NameKind::Constant, name)),
            _ => None,
        }
    }

    fn reg_writer<'a>(&self, proto: &'a Proto, before: usize, reg: u8) -> Option<(usize, &'a Op)> {
        // PUC's findsetreg scans forward so that a branch target invalidates
        // an earlier writer, even if that writer is the last one in source
        // order. A backward edge similarly makes a later writer ambiguous.
        let mut writer = None;
        let mut join = 0usize;
        for (pc, op) in proto.ops[..before].iter().enumerate() {
            if let Some(offset) = op.jump_offset() {
                let target = pc as i64 + 1 + i64::from(offset);
                if target > pc as i64 && target <= before as i64 {
                    join = join.max(target as usize);
                } else if target >= 0
                    && target <= pc as i64
                    && let Some((written_at, _)) = writer
                    && target as usize <= written_at
                {
                    writer = None;
                }
            }
            if op.writes(reg) {
                writer = (pc >= join).then_some((pc, op));
            }
        }
        writer
    }

    fn varinfo(&self, proto: &Proto, pc: u32, reg: u8) -> Vec<u8> {
        self.register_name(proto, pc, reg)
            .map_or_else(Vec::new, |(kind, name)| {
                let mut out = format!(" ({} '", kind.text()).into_bytes();
                out.extend_from_slice(&name);
                out.extend_from_slice(b"')");
                out
            })
    }

    fn runtime_message(&self, fault: LuaFault, thread: Handle<ThreadObj>) -> Vec<u8> {
        let Some(object) = self.heap.threads.get(thread) else {
            return fault.text().as_bytes().to_vec();
        };
        let Some(frame) = object.frames.last() else {
            return fault.text().as_bytes().to_vec();
        };
        if frame.boundary().is_some() {
            return fault.text().as_bytes().to_vec();
        }
        let Ok(proto) = self.closure_proto(frame.closure) else {
            return fault.text().as_bytes().to_vec();
        };
        let pc = frame.pc;
        let Some(op) = proto.ops.get(pc as usize) else {
            return fault.text().as_bytes().to_vec();
        };
        let value = |reg: u8| {
            object
                .stack
                .get((frame.base + u32::from(reg)) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        };
        let pair = match *op {
            Op::Add { a, b, .. }
            | Op::Arith { a, b, .. }
            | Op::Concat { a, b, .. }
            | Op::Compare { a, b, .. }
            | Op::CompareBranch { a, b, .. } => Some((a, b)),
            _ => None,
        };
        let mut operand = match *op {
            Op::GetTable { table, .. } | Op::SetTable { table, .. } => Some(table),
            Op::Index { obj, .. }
            | Op::SetIndex { obj, .. }
            | Op::GetField { obj, .. }
            | Op::SetField { obj, .. } => Some(obj),
            Op::Len { src, .. } | Op::Neg { src, .. } | Op::BNot { src, .. } => Some(src),
            Op::Call { func, .. } | Op::TailCall { func, .. } => Some(func),
            Op::ArithK { reg, .. } => Some(reg),
            _ => None,
        };
        if let Some((a, b)) = pair {
            operand = Some(match fault {
                LuaFault::Concat
                    if matches!(
                        value(a),
                        Value::String(_) | Value::Integer(_) | Value::Float(_)
                    ) =>
                {
                    b
                }
                LuaFault::NoInteger
                    if matches!(value(a), Value::Integer(_))
                        || matches!(value(a), Value::Float(f) if crate::compare::float_to_int(f).is_some()) =>
                {
                    b
                }
                LuaFault::Arith | LuaFault::Bitwise
                    if matches!(value(a), Value::Integer(_) | Value::Float(_)) =>
                {
                    b
                }
                _ => a,
            });
        }
        let mut selected = operand.map(value).unwrap_or(Value::Nil);
        let mut name_pc = pc;
        if matches!(op, Op::AssignCommit { .. })
            && let Some(Pending::Assigning { next, .. }) = frame.pending()
            && let Some(index) = next.checked_sub(1).map(usize::from)
            && let Some(AssignTarget::Field { table, .. }) = frame.targets().get(index)
        {
            selected = *table;
            // Targets are captured before evaluating the RHS. Recover the
            // source register at its capture PC, never its overwritten value.
            if let Some((at, Op::AssignField { table, .. })) = proto.ops[..pc as usize]
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, op)| matches!(op, Op::AssignField { .. } | Op::AssignLocal { .. }))
                .nth(frame.targets().len() - 1 - index)
            {
                operand = Some(*table);
                name_pc = at as u32;
            }
        }
        let mut chained = false;
        if fault == LuaFault::Index {
            let event = if matches!(
                op,
                Op::SetTable { .. }
                    | Op::SetIndex { .. }
                    | Op::SetField { .. }
                    | Op::AssignCommit { .. }
            ) {
                b"__newindex".as_slice()
            } else {
                b"__index".as_slice()
            };
            for _ in 0..16 {
                let Some(next) = index::metamethod(&self.heap, selected, event) else {
                    break;
                };
                if next.is_function() {
                    break;
                }
                selected = next;
                chained = true;
            }
        }
        let ty = self.diagnostic_type(selected);
        let extra = if chained {
            Vec::new()
        } else {
            operand.map_or_else(Vec::new, |reg| self.varinfo(proto, name_pc, reg))
        };
        let type_error = |action: &[u8]| {
            let mut out = b"attempt to ".to_vec();
            out.extend_from_slice(action);
            out.extend_from_slice(b" a ");
            out.extend_from_slice(&ty);
            out.extend_from_slice(b" value");
            out.extend_from_slice(&extra);
            out
        };
        match fault {
            LuaFault::Index => type_error(b"index"),
            LuaFault::BadCall => {
                let call = proto.debug.as_deref().and_then(|debug| debug.call_name(pc));
                if let Some(call) = call {
                    let mut out = b"attempt to call a ".to_vec();
                    out.extend_from_slice(&ty);
                    out.extend_from_slice(format!(" value ({} '", call.kind.text()).as_bytes());
                    out.extend_from_slice(&call.name);
                    out.extend_from_slice(b"')");
                    out
                } else {
                    type_error(b"call")
                }
            }
            LuaFault::Length => type_error(b"get length of"),
            LuaFault::Arith => type_error(b"perform arithmetic on"),
            LuaFault::Bitwise => type_error(b"perform bitwise operation on"),
            LuaFault::Concat => type_error(b"concatenate"),
            LuaFault::NoInteger => {
                let mut out = b"number".to_vec();
                out.extend_from_slice(&extra);
                out.extend_from_slice(b" has no integer representation");
                out
            }
            LuaFault::Compare => {
                let Some((a, b)) = pair else {
                    return fault.text().as_bytes().to_vec();
                };
                let left = self.diagnostic_type(value(a));
                let right = self.diagnostic_type(value(b));
                let mut out = if left == right {
                    b"attempt to compare two ".to_vec()
                } else {
                    b"attempt to compare ".to_vec()
                };
                out.extend_from_slice(&left);
                out.extend_from_slice(if left == right { b" values" } else { b" with " });
                if left != right {
                    out.extend_from_slice(&right);
                }
                out
            }
            LuaFault::ForValue => {
                let Op::ForPrep { base, .. } = *op else {
                    return fault.text().as_bytes().to_vec();
                };
                for (offset, what) in [(0, "initial value"), (1, "limit"), (2, "step")] {
                    let item = value(base + offset);
                    if !matches!(item, Value::Integer(_) | Value::Float(_)) {
                        return format!(
                            "bad 'for' {what} (number expected, got {})",
                            String::from_utf8_lossy(&self.diagnostic_type(item))
                        )
                        .into_bytes();
                    }
                }
                fault.text().as_bytes().to_vec()
            }
            LuaFault::Close => {
                let Op::MarkClose { reg } = *op else {
                    return fault.text().as_bytes().to_vec();
                };
                let name = proto.debug.as_deref().and_then(|debug| {
                    debug
                        .locals
                        .iter()
                        .rev()
                        .find(|local| local.reg == reg && local.start <= pc && pc < local.end)
                });
                let Some(name) = name else {
                    return fault.text().as_bytes().to_vec();
                };
                let mut text = b"variable '".to_vec();
                text.extend_from_slice(&name.name);
                text.extend_from_slice(b"' got a non-closable value");
                text
            }
            _ => fault.text().as_bytes().to_vec(),
        }
    }

    pub(super) fn diagnostic_fault(&mut self, fault: LuaFault, thread: Handle<ThreadObj>) -> Value {
        if !matches!(
            fault,
            LuaFault::Type
                | LuaFault::NilKey
                | LuaFault::NanKey
                | LuaFault::BadCall
                | LuaFault::Compare
                | LuaFault::ForValue
                | LuaFault::ForZeroStep
                | LuaFault::Index
                | LuaFault::Length
                | LuaFault::Arith
                | LuaFault::Bitwise
                | LuaFault::NoInteger
                | LuaFault::DivideByZero
                | LuaFault::Concat
                | LuaFault::StackOverflow
                | LuaFault::Close
                | LuaFault::Assert
                | LuaFault::ToString
                | LuaFault::ModuloByZero
        ) {
            return self.fault_value(fault);
        }
        let message = self.runtime_message(fault, thread);
        let location = self
            .heap
            .threads
            .get(thread)
            .and_then(|object| object.frames.len().checked_sub(1))
            .and_then(|index| self.frame_location(thread, index))
            .or_else(|| self.location(thread, 1).filter(|(_, line)| *line > 0))
            .or_else(|| self.nearest_lua_location(thread));
        self.prefixed(location, message, fault)
    }

    pub(super) fn metamethod_call_message(&self, function: Value) -> Vec<u8> {
        let name = self
            .heap
            .active
            .and_then(|thread| self.heap.threads.get(thread))
            .and_then(|thread| thread.frames.last())
            .and_then(|frame| {
                if frame
                    .meta()
                    .is_some_and(|meta| meta.event == MetaEvent::Close)
                {
                    return Some("close");
                }
                self.closure_proto(frame.closure)
                    .ok()
                    .and_then(|proto| proto.ops.get(frame.pc as usize))
                    .and_then(super::debug::metamethod_name)
            })
            .unwrap_or("?");
        let mut text = b"attempt to call a ".to_vec();
        text.extend_from_slice(&self.diagnostic_type(function));
        text.extend_from_slice(format!(" value (metamethod '{name}')").as_bytes());
        text
    }
}
