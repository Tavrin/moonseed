//! The `debug` library's introspection (ADR 0040): levels, function
//! information, locals, upvalues, metatables, the registry, and
//! tracebacks. See `debuglib.rs` for what is left out.
//!
//! A stack level is Lua's: level 0 is the function running, a debug
//! function itself on the running thread, and each level below is the
//! frame that called the one above. A Lua frame is a Lua function; a
//! `pcall` frame or a builtin's frame (ADR 0031) is a C function; an
//! `xpcall` handler's frame marks where the handler was called and is no
//! level. On another thread, level 0 is the builtin its top frame is
//! calling (`coroutine.yield`, `coroutine.resume`, or the one that
//! failed), when there is one.
//!
//! A frame's current instruction is its `pc`, except below a Lua frame its
//! own `Call` pushed, whose `pc` has moved past the call.

use super::library::{AuxWork, Ctx, Next};
use super::*;
use crate::debuginfo::DebugInfo;
use crate::debuglib::{
    DEBUG_FUNCTIONS, DbgFn, DebugWork, LEVELS1, LEVELS2, SEARCH_BATCH, Search, Traceback,
};
use crate::heap::{Boundary, MetaEvent};
use crate::library::Work;

impl AuxWork for DebugWork {
    fn next(runtime: &mut Runtime, ctx: &Ctx, work: &mut Self) -> Result<Next, VmError> {
        match work {
            DebugWork::Traceback(traceback) => runtime.traceback_next(ctx, traceback),
        }
    }
    fn wrap(self) -> Work {
        Work::Debug(Box::new(self))
    }
    fn wrap_boxed(self: Box<Self>) -> Work {
        Work::Debug(self)
    }
}

/// A stack level: a frame, or the builtin the top frame is calling.
#[derive(Clone, Copy)]
pub(super) struct Level {
    pub(super) frame: Option<usize>,
    func: Value,
}

/// What `debug.getinfo` reports about a function, and a level's part.
struct Info {
    /// The chunk name: a string object, or bytes for `=?` and `=[C]`.
    source: Result<Handle<crate::heap::StringObj>, &'static [u8]>,
    short_src: Vec<u8>,
    what: &'static str,
    line_defined: i64,
    last_line_defined: i64,
    current_line: i64,
    nups: i64,
    nparams: i64,
    vararg: bool,
    tail: bool,
    /// `namewhat` and `name`.
    name: Option<(&'static str, Vec<u8>)>,
}

/// Lua's names of the metamethod an instruction calls, for `namewhat`
/// "metamethod".
pub(super) fn metamethod_name(op: &Op) -> Option<&'static str> {
    use crate::opcode::{ArithOp, CmpKind};
    Some(match op {
        Op::GetTable { .. } | Op::GetGlobal { .. } | Op::Index { .. } | Op::GetField { .. } => {
            "index"
        }
        Op::SetTable { .. }
        | Op::SetIndex { .. }
        | Op::SetField { .. }
        | Op::AssignField { .. }
        | Op::AssignCommit { .. } => "newindex",
        Op::Add { .. } => "add",
        Op::Arith { op, .. } | Op::ArithK { op, .. } => match op {
            ArithOp::Add => "add",
            ArithOp::Sub => "sub",
            ArithOp::Mul => "mul",
            ArithOp::Div => "div",
            ArithOp::Idiv => "idiv",
            ArithOp::Mod => "mod",
            ArithOp::Pow => "pow",
            ArithOp::Band => "band",
            ArithOp::Bor => "bor",
            ArithOp::Bxor => "bxor",
            ArithOp::Shl => "shl",
            ArithOp::Shr => "shr",
        },
        Op::Neg { .. } => "unm",
        Op::BNot { .. } => "bnot",
        Op::Len { .. } => "len",
        Op::Concat { .. } => "concat",
        Op::Compare { kind, .. } | Op::CompareBranch { kind, .. } => match kind {
            CmpKind::Eq | CmpKind::Ne => "eq",
            CmpKind::Lt => "lt",
            CmpKind::Le => "le",
        },
        _ => return None,
    })
}

/// The frames an error's unwind is leaving while it runs their `__close`
/// calls: Lua pops them before it closes anything, so they are no levels
/// (ADR 0041). From the frame above the `pcall` that catches the error, or
/// from the bottom when nothing does, up to the frame whose values close.
fn unwound_frames(frames: &[Frame]) -> std::ops::Range<usize> {
    let closing = frames.iter().enumerate().rev().find_map(|(index, frame)| {
        let meta = frame.meta()?;
        match meta.close.as_deref()?.next {
            crate::heap::CloseNext::Unwind(unwind) => Some((index, unwind.phase)),
            _ => None,
        }
    });
    match closing {
        Some((
            index,
            crate::heap::UnwindPhase::Popping {
                target: Some(target),
            },
        )) => (target as usize + 1).min(index + 1)..index + 1,
        Some((index, _)) => 0..index + 1,
        None => 0..0,
    }
}

/// The instruction frame `index` is on.
pub(super) fn current_pc(frames: &[Frame], index: usize) -> u32 {
    let frame = &frames[index];
    match frames.get(index + 1) {
        Some(above) if above.boundary().is_none() && frame.meta().is_none() => {
            frame.pc.saturating_sub(1)
        }
        _ => frame.pc,
    }
}

impl Runtime {
    /// Install `debug` (ADR 0040). The registry must have the functions
    /// (see [`crate::register_debug`]). No standard installer calls this:
    /// the library reaches past every boundary a sandbox draws.
    pub fn install_debug(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("debug")?;
        // The two hook functions cross the old debug table's hash capacity.
        // Reserve the final field count once, before interning its names.
        let Value::Table(handle) = table else {
            return Err(VmError::Corrupt);
        };
        self.heap
            .tables
            .get_mut(handle)
            .ok_or(VmError::Corrupt)?
            .table
            .reserve_hash(DEBUG_FUNCTIONS.len());
        self.register_module("debug", table)?;
        for (name, symbol, _) in DEBUG_FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        Ok(())
    }

    /// A `debug` function, called from the active frame's call site.
    pub(super) fn call_debug(
        &mut self,
        active: Handle<ThreadObj>,
        function: DbgFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let next = match function {
            DbgFn::SetHook => self.db_sethook(&ctx)?,
            DbgFn::GetHook => self.db_gethook(&ctx)?,
            DbgFn::GetRegistry => Next::Done(vec![Value::Table(self.lua_registry()?)]),
            DbgFn::GetMetatable => self.db_getmetatable(&ctx),
            DbgFn::SetMetatable => self.db_setmetatable(&ctx)?,
            DbgFn::GetInfo => self.db_getinfo(&ctx)?,
            DbgFn::GetLocal => self.db_getlocal(&ctx)?,
            DbgFn::SetLocal => self.db_setlocal(&ctx)?,
            DbgFn::GetUpvalue => self.db_upvalue(&ctx, false)?,
            DbgFn::SetUpvalue => self.db_upvalue(&ctx, true)?,
            DbgFn::UpvalueJoin => self.db_upvaluejoin(&ctx)?,
            DbgFn::UpvalueId => self.db_upvalueid(&ctx)?,
            DbgFn::GetUserValue => self.db_getuservalue(&ctx)?,
            DbgFn::SetUserValue => self.db_setuservalue(&ctx)?,
            DbgFn::Traceback => match self.start_traceback(&ctx)? {
                Ok(work) => return self.run_aux(ctx, work, None, journal),
                Err(next) => next,
            },
        };
        match next {
            Next::Done(values) => self.base_return(active, &values),
            next => Ok(self.finish_next(active, next)),
        }
    }

    /// The thread a debug function looks at, and the index of its first
    /// argument after it: Lua's `getthread`.
    fn debug_thread(&self, ctx: &Ctx) -> (Handle<ThreadObj>, u32) {
        match self.lib_arg(ctx, 0) {
            Value::Thread(thread) => (thread, 1),
            _ => (ctx.active, 0),
        }
    }

    /// The levels of `thread`, level 0 first.
    pub(super) fn levels(
        &self,
        ctx: &Ctx,
        thread: Handle<ThreadObj>,
    ) -> Result<Vec<Level>, VmError> {
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let stack_value = |slot: u32| {
            object
                .stack
                .get(slot as usize)
                .copied()
                .unwrap_or(Value::Nil)
        };
        let mut levels = Vec::with_capacity(object.frames.len() + 1);
        if thread == ctx.active {
            // The debug function itself, until a step of its own made it a
            // frame.
            if !ctx.framed
                && !object.frames.last().is_some_and(|frame| matches!(frame.boundary(), Some(Boundary::HookNative { func, .. }) if *func == ctx.func))
            {
                levels.push(Level {
                    frame: None,
                    func: stack_value(ctx.func),
                });
            }
        } else if !object.frames.is_empty()
            && !object
                .frames
                .last()
                .is_some_and(|frame| matches!(frame.boundary(), Some(Boundary::HookNative { .. })))
            && !self
                .heap
                .hooks
                .get(object.id)
                .is_some_and(|hook| hook.hook_yield)
        {
            // The builtin the thread's top frame is calling, directly, from
            // `pcall`, or as a metamethod: a yield, a resume, or the one
            // that raised a failed coroutine's error.
            if let Ok((_, _, _, callee)) = self.call_site(thread)
                && matches!(callee, Value::Native(_) | Value::NativeClosure(_))
            {
                levels.push(Level {
                    frame: None,
                    func: callee,
                });
            }
        }
        let unwound = unwound_frames(&object.frames);
        for (index, frame) in object.frames.iter().enumerate().rev() {
            if unwound.contains(&index) {
                continue;
            }
            let func = match frame.boundary() {
                // A coroutine's trampoline stands for Lua's C entry.
                None if super::coroutine::is_trampoline(self.closure_proto(frame.closure)?) => {
                    continue;
                }
                None => Value::Closure(frame.closure),
                // A finalizer is called from wherever the collection
                // happened, with no level between (Lua's `GCTM`).
                Some(
                    Boundary::Handler { .. } | Boundary::Finalizer { .. } | Boundary::Hook { .. },
                ) => continue,
                Some(Boundary::HookNative { func, callee, .. }) => {
                    if object.frames.get(index+1).is_some_and(|above| matches!(above.boundary(), Some(Boundary::Builtin {func: f,..} | Boundary::Protect {func: f,..} | Boundary::Native {func: f,..}) if f == func)) { continue; }
                    *callee
                }
                Some(
                    Boundary::Protect { func, .. }
                    | Boundary::Builtin { func, .. }
                    | Boundary::Native { func, .. },
                ) => stack_value(*func),
            };
            levels.push(Level {
                frame: Some(index),
                func,
            });
        }
        Ok(levels)
    }

    pub(super) fn closure_proto(
        &self,
        closure: Handle<crate::heap::ClosureObj>,
    ) -> Result<&crate::heap::Proto, VmError> {
        let proto = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        self.heap.protos.get(proto).ok_or(VmError::Corrupt)
    }

    /// The level argument `index`, as Lua's `(int)luaL_checkinteger`, and
    /// the level it names, if any.
    fn level_arg(
        &self,
        ctx: &Ctx,
        thread: Handle<ThreadObj>,
        index: u32,
    ) -> Result<Result<Option<Level>, Next>, VmError> {
        let level = match self.int_arg(ctx, index) {
            Ok(level) => level as i32,
            Err(next) => return Ok(Err(next)),
        };
        let levels = self.levels(ctx, thread)?;
        Ok(Ok(usize::try_from(level)
            .ok()
            .and_then(|level| levels.get(level).copied())))
    }

    /// What the call site below a level calls it: Lua's `getfuncname`.
    fn level_name(
        &self,
        thread: Handle<ThreadObj>,
        level: &Level,
    ) -> Result<Option<(&'static str, Vec<u8>)>, VmError> {
        let frames = &self
            .heap
            .threads
            .get(thread)
            .ok_or(VmError::Corrupt)?
            .frames;
        let caller = match level.frame {
            Some(index) => {
                if frames[index].is_tail() {
                    return Ok(None);
                }
                index.checked_sub(1)
            }
            None => frames.len().checked_sub(1),
        };
        let Some(caller) = caller else {
            return Ok(None);
        };
        // A `__close` an unwind runs is called from C in Lua.
        if unwound_frames(frames).contains(&caller) {
            return Ok(None);
        }
        let frame = &frames[caller];
        if matches!(frame.boundary(), Some(Boundary::Hook { .. })) {
            return Ok(Some(("hook", b"?".to_vec())));
        }
        if matches!(frame.boundary(), Some(Boundary::Finalizer { .. })) {
            return Ok(Some(("metamethod", b"__gc".to_vec())));
        }
        if frame.boundary().is_some() {
            return Ok(None);
        }
        let pc = current_pc(frames, caller);
        let proto = self.closure_proto(frame.closure)?;
        if let Some(meta) = frame.meta() {
            let name = if meta.event == MetaEvent::Close {
                Some("close")
            } else {
                proto.ops.get(pc as usize).and_then(metamethod_name)
            };
            return Ok(name.map(|name| ("metamethod", name.as_bytes().to_vec())));
        }
        if !matches!(
            proto.ops.get(pc as usize),
            Some(Op::Call { .. } | Op::TailCall { .. })
        ) {
            return Ok(None);
        }
        Ok(proto
            .debug
            .as_deref()
            .and_then(|debug| debug.call_name(pc))
            .map(|call| (call.kind.text(), call.name.clone())))
    }

    /// The name a running C function gets from its Lua call site. Use the
    /// same lookup as `debug.getinfo(..., "n")` for that C level.
    pub(super) fn calling_name_at(
        &self,
        thread: Handle<ThreadObj>,
        framed: bool,
    ) -> Option<(&'static str, Vec<u8>)> {
        let frame = if framed {
            self.heap.threads.get(thread)?.frames.len().checked_sub(1)
        } else {
            None
        };
        self.level_name(
            thread,
            &Level {
                frame,
                func: Value::Nil,
            },
        )
        .ok()
        .flatten()
    }

    /// Lua's `lua_getinfo` for `func`, at `level` of `thread` when given.
    fn func_info(
        &self,
        thread: Handle<ThreadObj>,
        func: Value,
        level: Option<&Level>,
    ) -> Result<Info, VmError> {
        let Value::Closure(closure) = func else {
            let nups = match func {
                Value::NativeClosure(handle) => self
                    .heap
                    .native_closures
                    .get(handle)
                    .ok_or(VmError::Corrupt)?
                    .values
                    .len() as i64,
                _ => 0,
            };
            return Ok(Info {
                source: Err(b"=[C]"),
                short_src: b"[C]".to_vec(),
                what: "C",
                line_defined: -1,
                last_line_defined: -1,
                current_line: -1,
                nups,
                nparams: 0,
                vararg: true,
                tail: false,
                name: match level {
                    Some(level) => self.level_name(thread, level)?,
                    None => None,
                },
            });
        };
        let proto = self.closure_proto(closure)?;
        let debug: Option<&DebugInfo> = proto.debug.as_deref();
        let source_bytes: &[u8] = match proto.source {
            Some(handle) => self.heap.string_bytes(handle).ok_or(VmError::Corrupt)?,
            None => b"=?",
        };
        let (line_defined, last_line_defined) = debug.map_or((-1, -1), |debug| {
            (
                i64::from(debug.line_defined),
                i64::from(debug.last_line_defined),
            )
        });
        let mut info = Info {
            source: proto.source.ok_or(b"=?".as_slice()),
            short_src: crate::chunkname::chunk_id(source_bytes),
            what: if line_defined == 0 { "main" } else { "Lua" },
            line_defined,
            last_line_defined,
            current_line: -1,
            nups: self
                .heap
                .closures
                .get(closure)
                .ok_or(VmError::Corrupt)?
                .upvalues
                .len() as i64,
            nparams: i64::from(proto.params),
            vararg: proto.vararg,
            tail: false,
            name: None,
        };
        if let Some(level) = level
            && let Some(index) = level.frame
        {
            let frames = &self
                .heap
                .threads
                .get(thread)
                .ok_or(VmError::Corrupt)?
                .frames;
            info.current_line = self
                .frame_location(thread, index)
                .map_or(-1, |(_, line)| line);
            info.tail = frames[index].is_tail();
            info.name = self.level_name(thread, level)?;
        }
        Ok(info)
    }

    fn db_getmetatable(&self, ctx: &Ctx) -> Next {
        if ctx.passed == 0 {
            return self.bad_arg(ctx, 0, "value expected");
        }
        let metatable = self.heap.metatable_of(self.lib_arg(ctx, 0));
        Next::Done(vec![metatable.map_or(Value::Nil, Value::Table)])
    }

    /// `debug.setmetatable(v, t)`: a table's or a full userdata's own
    /// metatable, or the metatable every value of `v`'s type shares.
    fn db_setmetatable(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let metatable = match self.lib_arg(ctx, 1) {
            Value::Table(table) if ctx.passed > 1 => Some(table),
            Value::Nil if ctx.passed > 1 => None,
            _ => return Ok(self.bad_type(ctx, 1, "nil or table")),
        };
        let value = self.lib_arg(ctx, 0);
        match value {
            Value::Table(_) | Value::Userdata(_) => {
                if !self.heap.set_metatable(value, metatable) {
                    return Err(VmError::Corrupt);
                }
            }
            _ => self.heap.type_metatables[crate::heap::basic_type(value)] = metatable,
        }
        Ok(Next::Done(vec![value]))
    }

    fn db_getinfo(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let (thread, arg) = self.debug_thread(ctx);
        let mut options = if self.given(ctx, arg + 1) {
            match self.string_arg(ctx, arg + 1)? {
                Some(handle) => self
                    .heap
                    .string_bytes(handle)
                    .ok_or(VmError::Corrupt)?
                    .to_vec(),
                None => return Ok(self.bad_type(ctx, arg + 1, "string")),
            }
        } else {
            b"flnSrtu".to_vec()
        };
        // A C string ends at its first zero.
        if let Some(end) = options.iter().position(|byte| *byte == 0) {
            options.truncate(end);
        }
        if options.first() == Some(&b'>') {
            return Ok(self.bad_arg(ctx, arg + 1, "invalid option '>'"));
        }
        let target = self.lib_arg(ctx, arg);
        let (level, func) = if target.is_function() {
            (None, target)
        } else {
            match self.level_arg(ctx, thread, arg)? {
                Err(next) => return Ok(next),
                Ok(None) => return Ok(Next::Done(vec![Value::Nil])),
                Ok(Some(level)) => (Some(level), level.func),
            }
        };
        if !options.iter().all(|option| b"SlnrutLf".contains(option)) {
            return Ok(self.bad_arg(ctx, arg + 1, "invalid option"));
        }
        let info = self.func_info(thread, func, level.as_ref())?;
        let table = self.alloc_table()?;
        let has = |option: u8| options.contains(&option);
        let mut fields: Vec<(&str, Value)> = Vec::new();
        if has(b'S') {
            let source = match info.source {
                Ok(handle) => Value::String(handle),
                Err(bytes) => Value::String(self.alloc_string(bytes.to_vec())?),
            };
            fields.push(("source", source));
            let short_src = Value::String(self.alloc_string(info.short_src.clone())?);
            fields.push(("short_src", short_src));
            fields.push(("linedefined", Value::Integer(info.line_defined)));
            fields.push(("lastlinedefined", Value::Integer(info.last_line_defined)));
            let what = Value::String(self.alloc_string(info.what.as_bytes().to_vec())?);
            fields.push(("what", what));
        }
        if has(b'l') {
            fields.push(("currentline", Value::Integer(info.current_line)));
        }
        if has(b'u') {
            fields.push(("nups", Value::Integer(info.nups)));
            fields.push(("nparams", Value::Integer(info.nparams)));
            fields.push(("isvararg", Value::Bool(info.vararg)));
        }
        if has(b'n') {
            let (namewhat, name) = match &info.name {
                Some((namewhat, name)) => {
                    (*namewhat, Value::String(self.alloc_string(name.clone())?))
                }
                None => ("", Value::Nil),
            };
            fields.push(("name", name));
            let namewhat = Value::String(self.alloc_string(namewhat.as_bytes().to_vec())?);
            fields.push(("namewhat", namewhat));
        }
        if has(b'r') {
            let transfer = level
                .and_then(|l| l.frame)
                .and_then(|i| {
                    let (frame, first, count) = self
                        .heap
                        .hooks
                        .get(self.heap.threads.get(thread)?.id)?
                        .transfer?;
                    (frame as usize == i).then_some((first, count))
                })
                .unwrap_or((0, 0));
            fields.push(("ftransfer", Value::Integer(i64::from(transfer.0))));
            fields.push(("ntransfer", Value::Integer(i64::from(transfer.1))));
        }
        if has(b't') {
            fields.push(("istailcall", Value::Bool(info.tail)));
        }
        if has(b'L') {
            fields.push(("activelines", self.active_lines(func)?));
        }
        if has(b'f') {
            fields.push(("func", func));
        }
        for (name, value) in fields {
            if !matches!(value, Value::Nil) {
                let key = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
                self.raw_set_field(table, key, value)?;
            }
        }
        Ok(Next::Done(vec![Value::Table(table)]))
    }

    /// `activelines`: a table with `true` at each line that has code, or
    /// nil for a C function.
    fn active_lines(&mut self, func: Value) -> Result<Value, VmError> {
        let Value::Closure(closure) = func else {
            return Ok(Value::Nil);
        };
        let mut lines: Vec<u32> = self
            .closure_proto(closure)?
            .debug
            .as_deref()
            .map(|debug| debug.lines.clone())
            .unwrap_or_default();
        lines.sort_unstable();
        lines.dedup();
        let table = self.alloc_table()?;
        for line in lines {
            self.raw_set_field(table, Value::Integer(i64::from(line)), Value::Bool(true))?;
        }
        Ok(Value::Table(table))
    }

    /// Local `n` of `level`: its name and stack slot. Lua's
    /// `luaG_findlocal`, for Lua frames only.
    fn find_local(
        &self,
        thread: Handle<ThreadObj>,
        levels: &[Level],
        position: usize,
        n: i32,
    ) -> Result<Option<(Vec<u8>, u32)>, VmError> {
        let Some(index) = levels[position].frame else {
            return Ok(None);
        };
        let object = self.heap.threads.get(thread).ok_or(VmError::Corrupt)?;
        let frames = &object.frames;
        let frame = &frames[index];
        if matches!(frame.boundary(), Some(Boundary::HookNative { .. })) {
            let slot = frame.base.saturating_add(n as u32).saturating_sub(1);
            return Ok(
                (n > 0 && slot < object.top && (slot as usize) < object.stack.len())
                    .then(|| (b"(C temporary)".to_vec(), slot)),
            );
        }
        if frame.boundary().is_some() {
            return Ok(None);
        }
        let proto = self.closure_proto(frame.closure)?;
        let fits = |slot: u32| (slot as usize) < object.stack.len();
        if n < 0 {
            // `(vararg)`: the extra arguments, below the registers.
            let extra = n.unsigned_abs();
            if !proto.vararg || extra > frame.vararg_len {
                return Ok(None);
            }
            let slot = frame.base - frame.vararg_len + (extra - 1);
            return Ok(fits(slot).then(|| (b"(vararg)".to_vec(), slot)));
        }
        let pc = current_pc(frames, index);
        if let Some(debug) = proto.debug.as_deref() {
            let mut count = n;
            for local in &debug.locals {
                if local.start > pc {
                    break;
                }
                if pc < local.end {
                    count -= 1;
                    if count == 0 {
                        let slot = frame.base + u32::from(local.reg);
                        return Ok(fits(slot).then(|| (local.name.clone(), slot)));
                    }
                }
            }
        }
        // `(temporary)`: any slot of the frame below where the call above
        // it begins.
        let limit = match frames.get(index + 1) {
            Some(above) => match above.boundary() {
                None => (above.base - above.vararg_len).saturating_sub(1),
                Some(
                    Boundary::Protect { func, .. }
                    | Boundary::Builtin { func, .. }
                    | Boundary::Native { func, .. }
                    | Boundary::Finalizer { func, .. },
                ) => *func,
                Some(Boundary::Handler { .. }) => frame.limit,
                Some(Boundary::Hook { func, .. } | Boundary::HookNative { func, .. }) => *func,
            },
            None => match levels.first() {
                Some(Level { frame: None, .. }) => match proto.ops.get(frame.pc as usize) {
                    Some(Op::Call { func, .. } | Op::TailCall { func, .. }) => {
                        frame.base + u32::from(*func)
                    }
                    _ => object.top,
                },
                _ => object.top,
            },
        };
        let limit = self
            .heap
            .hooks
            .get(object.id)
            .and_then(|h| h.transfer)
            .filter(|(observed, _, _)| *observed as usize == index)
            .map_or(limit, |(_, first, count)| {
                if count == 0 {
                    limit
                } else {
                    limit.max(frame.base + first - 1 + count)
                }
            });
        let slot = frame.base.saturating_add(n as u32).saturating_sub(1);
        Ok((n > 0 && slot < limit && fits(slot)).then(|| (b"(temporary)".to_vec(), slot)))
    }

    fn db_getlocal(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let (thread, arg) = self.debug_thread(ctx);
        let n = match self.int_arg(ctx, arg + 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let target = self.lib_arg(ctx, arg);
        if target.is_function() {
            // A function's parameters, by name only.
            let name = match target {
                Value::Closure(closure) => {
                    let proto = self.closure_proto(closure)?;
                    let locals = proto
                        .debug
                        .as_deref()
                        .map_or(&[][..], |debug| &debug.locals);
                    usize::try_from(n)
                        .ok()
                        .filter(|n| (1..=usize::from(proto.params)).contains(n))
                        .and_then(|n| locals.get(n - 1))
                        .map(|local| local.name.clone())
                }
                _ => None,
            };
            let name = match name {
                Some(name) => Value::String(self.alloc_string(name)?),
                None => Value::Nil,
            };
            return Ok(Next::Done(vec![name]));
        }
        let level = match self.int_arg(ctx, arg) {
            Ok(level) => level as i32,
            Err(next) => return Ok(next),
        };
        let levels = self.levels(ctx, thread)?;
        let Some(position) = usize::try_from(level)
            .ok()
            .filter(|level| *level < levels.len())
        else {
            return Ok(self.bad_arg(ctx, arg, "level out of range"));
        };
        match self.find_local(thread, &levels, position, n)? {
            Some((name, slot)) => {
                let value = self
                    .heap
                    .threads
                    .get(thread)
                    .and_then(|object| object.stack.get(slot as usize))
                    .copied()
                    .ok_or(VmError::Corrupt)?;
                let name = Value::String(self.alloc_string(name)?);
                Ok(Next::Done(vec![name, value]))
            }
            None => Ok(Next::Done(vec![Value::Nil])),
        }
    }

    fn db_setlocal(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let (thread, arg) = self.debug_thread(ctx);
        let level = match self.int_arg(ctx, arg) {
            Ok(level) => level as i32,
            Err(next) => return Ok(next),
        };
        let n = match self.int_arg(ctx, arg + 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let levels = self.levels(ctx, thread)?;
        let Some(position) = usize::try_from(level)
            .ok()
            .filter(|level| *level < levels.len())
        else {
            return Ok(self.bad_arg(ctx, arg, "level out of range"));
        };
        if ctx.passed <= arg + 2 {
            return Ok(self.bad_arg(ctx, arg + 2, "value expected"));
        }
        let value = self.lib_arg(ctx, arg + 2);
        match self.find_local(thread, &levels, position, n)? {
            Some((name, slot)) => {
                *self
                    .heap
                    .threads
                    .get_mut(thread)
                    .and_then(|object| object.stack.get_mut(slot as usize))
                    .ok_or(VmError::Corrupt)? = value;
                Ok(Next::Done(vec![Value::String(self.alloc_string(name)?)]))
            }
            None => Ok(Next::Done(vec![Value::Nil])),
        }
    }

    /// `debug.getupvalue(f, n)` and `debug.setupvalue(f, n, v)`. A Lua
    /// function's upvalue has its name, or `(no name)` once stripped; a
    /// builtin's value has the empty name and cannot be set.
    fn db_upvalue(&mut self, ctx: &Ctx, set: bool) -> Result<Next, VmError> {
        if set && ctx.passed < 3 {
            return Ok(self.bad_arg(ctx, 2, "value expected"));
        }
        let n = match self.int_arg(ctx, 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let func = self.lib_arg(ctx, 0);
        if !func.is_function() || ctx.passed == 0 {
            return Ok(self.bad_type(ctx, 0, "function"));
        }
        let Some(index) = usize::try_from(n).ok().and_then(|n| n.checked_sub(1)) else {
            return Ok(Next::Done(Vec::new()));
        };
        let (name, cell) = match func {
            Value::Closure(closure) => {
                let object = self.heap.closures.get(closure).ok_or(VmError::Corrupt)?;
                let Some(cell) = object.upvalues.get(index).copied() else {
                    return Ok(Next::Done(Vec::new()));
                };
                let name = self
                    .closure_proto(closure)?
                    .debug
                    .as_deref()
                    .and_then(|debug| debug.upvalues.get(index))
                    .map_or_else(|| b"(no name)".to_vec(), Clone::clone);
                (name, cell)
            }
            Value::NativeClosure(handle) if !set => {
                let object = self
                    .heap
                    .native_closures
                    .get(handle)
                    .ok_or(VmError::Corrupt)?;
                let Some(value) = object.values.get(index).copied() else {
                    return Ok(Next::Done(Vec::new()));
                };
                let name = Value::String(self.alloc_string(Vec::new())?);
                return Ok(Next::Done(vec![name, value]));
            }
            _ => return Ok(Next::Done(Vec::new())),
        };
        let name = Value::String(self.alloc_string(name)?);
        if set {
            let value = self.lib_arg(ctx, 2);
            self.write_cell(cell, value)?;
            Ok(Next::Done(vec![name]))
        } else {
            Ok(Next::Done(vec![name, self.read_cell(cell)?]))
        }
    }

    fn read_cell(&self, cell: Handle<crate::heap::UpvalueObj>) -> Result<Value, VmError> {
        match self.heap.upvalues.get(cell).ok_or(VmError::Corrupt)?.state {
            crate::heap::UpvalueState::Closed(value) => Ok(value),
            crate::heap::UpvalueState::Open { thread, slot } => self
                .heap
                .threads
                .get(thread)
                .and_then(|object| object.stack.get(slot as usize))
                .copied()
                .ok_or(VmError::Corrupt),
        }
    }

    fn write_cell(
        &mut self,
        cell: Handle<crate::heap::UpvalueObj>,
        value: Value,
    ) -> Result<(), VmError> {
        let object = self.heap.upvalues.get_mut(cell).ok_or(VmError::Corrupt)?;
        match &mut object.state {
            crate::heap::UpvalueState::Closed(held) => *held = value,
            crate::heap::UpvalueState::Open { thread, slot } => {
                let (thread, slot) = (*thread, *slot);
                *self
                    .heap
                    .threads
                    .get_mut(thread)
                    .and_then(|object| object.stack.get_mut(slot as usize))
                    .ok_or(VmError::Corrupt)? = value;
            }
        }
        Ok(())
    }

    /// `debug.upvalueid(f, n)`: a light userdata naming the cell of `f`'s
    /// upvalue `n` (ADR 0043), the cell's own id, so closures sharing a
    /// cell get equal tokens and a closed cell keeps its token. A native
    /// closure's value is named by the closure and the index, as Lua names
    /// a C closure's upvalue. Fail for a builtin or an index out of range.
    fn db_upvalueid(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        use crate::value::LightDomain;
        let n = match self.int_arg(ctx, 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let func = self.lib_arg(ctx, 0);
        if !func.is_function() || ctx.passed == 0 {
            return Ok(self.bad_type(ctx, 0, "function"));
        }
        let index = usize::try_from(n).ok().and_then(|n| n.checked_sub(1));
        let token = match (func, index) {
            (Value::Closure(closure), Some(index)) => {
                let object = self.heap.closures.get(closure).ok_or(VmError::Corrupt)?;
                match object.upvalues.get(index) {
                    Some(cell) => {
                        let id = self.heap.upvalues.get(*cell).ok_or(VmError::Corrupt)?.id;
                        Some(Value::LightUserdata(LightDomain::Upvalue, id.raw()))
                    }
                    None => None,
                }
            }
            (Value::NativeClosure(handle), Some(index)) => {
                let object = self
                    .heap
                    .native_closures
                    .get(handle)
                    .ok_or(VmError::Corrupt)?;
                (index < object.values.len()).then(|| {
                    Value::LightUserdata(
                        LightDomain::NativeValue,
                        object.id.raw() << 8 | index as u64,
                    )
                })
            }
            _ => None,
        };
        Ok(Next::Done(vec![token.unwrap_or(Value::Nil)]))
    }

    /// `debug.getuservalue(u [, n])`: user value `n` and `true`; a lone
    /// nil for a slot `u` lacks; fail for anything not a full userdata.
    fn db_getuservalue(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let n = match self.opt_int_arg(ctx, 1, 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let Value::Userdata(userdata) = self.lib_arg(ctx, 0) else {
            return Ok(Next::Done(vec![Value::Nil]));
        };
        let object = self.heap.userdata.get(userdata).ok_or(VmError::Corrupt)?;
        let held = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| object.user_values.get(index).copied());
        Ok(Next::Done(match held {
            Some(value) => vec![value, Value::Bool(true)],
            None => vec![Value::Nil],
        }))
    }

    /// `debug.setuservalue(u, value [, n])`: `u` after storing `value` in
    /// its user value `n`; fail for a slot `u` lacks. `u` must be a full
    /// userdata.
    fn db_setuservalue(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let n = match self.opt_int_arg(ctx, 2, 1) {
            Ok(n) => n as i32,
            Err(next) => return Ok(next),
        };
        let target = self.lib_arg(ctx, 0);
        let Value::Userdata(userdata) = target else {
            return Ok(self.bad_type(ctx, 0, "userdata"));
        };
        if ctx.passed < 2 {
            return Ok(self.bad_arg(ctx, 1, "value expected"));
        }
        let value = self.lib_arg(ctx, 1);
        let object = self
            .heap
            .userdata
            .get_mut(userdata)
            .ok_or(VmError::Corrupt)?;
        let slot = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| object.user_values.get_mut(index));
        Ok(Next::Done(vec![match slot {
            Some(slot) => {
                *slot = value;
                target
            }
            None => Value::Nil,
        }]))
    }

    /// `debug.upvaluejoin(f1, n1, f2, n2)`: `f1`'s upvalue `n1` becomes the
    /// very cell of `f2`'s upvalue `n2`.
    fn db_upvaluejoin(&mut self, ctx: &Ctx) -> Result<Next, VmError> {
        let mut picked = [None; 2];
        for (which, (func_arg, n_arg)) in [(0u32, 1u32), (2, 3)].into_iter().enumerate() {
            let n = match self.int_arg(ctx, n_arg) {
                Ok(n) => n as i32,
                Err(next) => return Ok(next),
            };
            let func = self.lib_arg(ctx, func_arg);
            if !func.is_function() || ctx.passed <= func_arg {
                return Ok(self.bad_type(ctx, func_arg, "function"));
            }
            // Lua's `lua_upvalueid`: a closure's upvalue that exists.
            let count = match func {
                Value::Closure(closure) => self
                    .heap
                    .closures
                    .get(closure)
                    .ok_or(VmError::Corrupt)?
                    .upvalues
                    .len(),
                Value::NativeClosure(handle) => self
                    .heap
                    .native_closures
                    .get(handle)
                    .ok_or(VmError::Corrupt)?
                    .values
                    .len(),
                _ => 0,
            };
            let Some(index) = usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .filter(|index| *index < count)
            else {
                return Ok(self.bad_arg(ctx, n_arg, "invalid upvalue index"));
            };
            picked[which] = Some((func, index));
        }
        let mut closures = [None; 2];
        for (which, func_arg) in [0u32, 2].into_iter().enumerate() {
            match picked[which] {
                Some((Value::Closure(closure), index)) => closures[which] = Some((closure, index)),
                _ => return Ok(self.bad_arg(ctx, func_arg, "Lua function expected")),
            }
        }
        let [Some((target, n1)), Some((source, n2))] = closures else {
            return Err(VmError::Corrupt);
        };
        let cell = *self
            .heap
            .closures
            .get(source)
            .and_then(|object| object.upvalues.get(n2))
            .ok_or(VmError::Corrupt)?;
        *self
            .heap
            .closures
            .get_mut(target)
            .and_then(|object| object.upvalues.get_mut(n1))
            .ok_or(VmError::Corrupt)? = cell;
        Ok(Next::Done(Vec::new()))
    }

    /// `debug.traceback([thread,] [msg [, level]])`: a message that is not
    /// a string or nil comes back untouched; otherwise the machine builds
    /// the text, a level or a slice of the name search per step.
    fn start_traceback(&mut self, ctx: &Ctx) -> Result<Result<DebugWork, Next>, VmError> {
        let (thread, arg) = self.debug_thread(ctx);
        let message = match self.lib_arg(ctx, arg) {
            Value::Nil => None,
            Value::String(handle) => Some(
                self.heap
                    .string_bytes(handle)
                    .ok_or(VmError::Corrupt)?
                    .to_vec(),
            ),
            value @ (Value::Integer(_) | Value::Float(_)) => Some(
                crate::concat::number_text(value)
                    .unwrap_or_default()
                    .into_bytes(),
            ),
            value => return Ok(Err(Next::Done(vec![value]))),
        };
        let default = if thread == ctx.active { 1 } else { 0 };
        let level = match self.opt_int_arg(ctx, arg + 1, default) {
            Ok(level) => i64::from(level as i32),
            Err(next) => return Ok(Err(next)),
        };
        let last = self.levels(ctx, thread)?.len().saturating_sub(1) as i64;
        let mut text = Vec::new();
        if let Some(mut message) = message {
            // A C string ends at its first zero.
            if let Some(end) = message.iter().position(|byte| *byte == 0) {
                message.truncate(end);
            }
            text = message;
            text.push(b'\n');
        }
        let mut held = Vec::new();
        text.extend_from_slice(b"stack traceback:");
        if !self.traceback_append(&mut held, &text) {
            return Ok(Err(Next::Fault(LuaFault::Memory)));
        }
        Ok(Ok(DebugWork::Traceback(Traceback {
            threaded: arg == 1,
            level,
            last,
            shown: if last - level > LEVELS1 + LEVELS2 {
                LEVELS1
            } else {
                -1
            },
            search: None,
            text: held,
        })))
    }

    fn traceback_next(&mut self, ctx: &Ctx, work: &mut Traceback) -> Result<Next, VmError> {
        let thread = if work.threaded {
            match self.lib_arg(ctx, 0) {
                Value::Thread(thread) => thread,
                _ => return Ok(Next::Fault(LuaFault::Argument)),
            }
        } else {
            ctx.active
        };
        let levels = self.levels(ctx, thread)?;
        loop {
            let Some(level) = usize::try_from(work.level)
                .ok()
                .and_then(|level| levels.get(level).copied())
            else {
                if self.heap.entry == Some(thread)
                    && !self.traceback_append(&mut work.text, b"\n\t[C]: in ?")
                {
                    return Ok(Next::Fault(LuaFault::Memory));
                }
                let text = std::mem::take(&mut work.text);
                return Ok(Next::Done(vec![Value::String(self.alloc_string(text)?)]));
            };
            let search = match work.search {
                Some(search) => search,
                None => {
                    work.shown -= 1;
                    if work.shown == -1 {
                        // Lua's count, one short of the levels skipped.
                        let skip = work.last - work.level - LEVELS2;
                        let line = format!("\n\t...\t(skipping {skip} levels)");
                        if !self.traceback_append(&mut work.text, line.as_bytes()) {
                            return Ok(Next::Fault(LuaFault::Memory));
                        }
                        work.level += 1 + skip;
                        continue;
                    }
                    Search {
                        outer: 0,
                        inner: None,
                    }
                }
            };
            let found = match self.search_loaded(level.func, search)? {
                Searched::Pending(search) => {
                    work.search = Some(search);
                    return Ok(Next::Busy);
                }
                Searched::Found(name) => Some(name),
                Searched::Absent => None,
            };
            work.search = None;
            let info = self.func_info(thread, level.func, Some(&level))?;
            let mut line = b"\n\t".to_vec();
            line.extend_from_slice(&super::diag::source_line(
                &info.short_src,
                (info.current_line > 0).then_some(info.current_line),
            ));
            line.extend_from_slice(b": in ");
            match (found, &info.name) {
                (Some(name), _) => {
                    line.extend_from_slice(b"function '");
                    line.extend_from_slice(&name);
                    line.push(b'\'');
                }
                (None, Some((namewhat, name))) => {
                    line.extend_from_slice(namewhat.as_bytes());
                    line.extend_from_slice(b" '");
                    line.extend_from_slice(name);
                    line.push(b'\'');
                }
                (None, None) if info.what == "main" => line.extend_from_slice(b"main chunk"),
                (None, None) if info.what != "C" => {
                    line.extend_from_slice(b"function <");
                    line.extend_from_slice(&super::diag::source_line(
                        &info.short_src,
                        Some(info.line_defined),
                    ));
                    line.push(b'>');
                }
                (None, None) => line.push(b'?'),
            }
            if info.tail {
                line.extend_from_slice(b"\n\t(...tail calls...)");
            }
            if !self.traceback_append(&mut work.text, &line) {
                return Ok(Next::Fault(LuaFault::Memory));
            }
            work.level += 1;
        }
    }

    /// Add `bytes` to a traceback's text, within the string limit and the
    /// heap quota, charged to the logical heap as it grows; false past
    /// either.
    fn traceback_append(&mut self, text: &mut Vec<u8>, bytes: &[u8]) -> bool {
        if text.len() + bytes.len() > self.heap.max_string || !self.heap.gc.fits(bytes.len() as u64)
        {
            return false;
        }
        self.heap.charge_held(bytes.len() as u64);
        text.extend_from_slice(bytes);
        true
    }

    pub(super) fn loaded_function_name(&self, func: Value) -> Option<Vec<u8>> {
        let mut search = Search {
            outer: 0,
            inner: None,
        };
        loop {
            match self.search_loaded(func, search).ok()? {
                Searched::Found(name) => return Some(name),
                Searched::Absent => return None,
                Searched::Pending(next) => search = next,
            }
        }
    }

    /// Lua's `pushglobalfuncname`: the name `func` has in a loaded module,
    /// `module.field`, or a global's own name, found by going through the
    /// registry's `_LOADED` and the tables in it, raw and in order, string
    /// keys only. Looks at up to [`SEARCH_BATCH`] entries.
    fn search_loaded(&self, func: Value, mut search: Search) -> Result<Searched, VmError> {
        let loaded = self.heap.registry.and_then(|registry| {
            let key = crate::table::KeyView::string(b"_LOADED");
            match self.heap.tables.get(registry)?.table.get_view(key) {
                Some(Value::Table(loaded)) => Some(loaded),
                _ => None,
            }
        });
        let Some(loaded) = loaded else {
            return Ok(Searched::Absent);
        };
        let string_key = |key: &Value| match key {
            Value::String(handle) => self.heap.string_bytes(*handle),
            _ => None,
        };
        let outer_slots = self
            .heap
            .tables
            .get(loaded)
            .ok_or(VmError::Corrupt)?
            .table
            .slots();
        for _ in 0..SEARCH_BATCH {
            let Some(slot) = outer_slots.get(search.outer as usize) else {
                return Ok(Searched::Absent);
            };
            let crate::table::Slot::Live {
                key_value, value, ..
            } = slot
            else {
                search.outer += 1;
                continue;
            };
            let Some(module) = string_key(key_value) else {
                search.outer += 1;
                continue;
            };
            match search.inner {
                None => {
                    if crate::compare::equal(&self.heap, *value, func) {
                        return Ok(Searched::Found(global_name(module.to_vec())));
                    }
                    if matches!(value, Value::Table(_)) {
                        search.inner = Some(0);
                    } else {
                        search.outer += 1;
                    }
                }
                Some(inner) => {
                    let Value::Table(table) = value else {
                        search.inner = None;
                        search.outer += 1;
                        continue;
                    };
                    let slots = self
                        .heap
                        .tables
                        .get(*table)
                        .ok_or(VmError::Corrupt)?
                        .table
                        .slots();
                    let Some(slot) = slots.get(inner as usize) else {
                        search.inner = None;
                        search.outer += 1;
                        continue;
                    };
                    search.inner = Some(inner + 1);
                    if let crate::table::Slot::Live {
                        key_value, value, ..
                    } = slot
                        && let Some(field) = string_key(key_value)
                        && crate::compare::equal(&self.heap, *value, func)
                    {
                        let mut name = module.to_vec();
                        name.push(b'.');
                        name.extend_from_slice(field);
                        return Ok(Searched::Found(global_name(name)));
                    }
                }
            }
        }
        Ok(Searched::Pending(search))
    }
}

enum Searched {
    Found(Vec<u8>),
    Absent,
    Pending(Search),
}

/// A name found in `_LOADED`, without the `_G.` of a global.
fn global_name(name: Vec<u8>) -> Vec<u8> {
    match name.strip_prefix(b"_G.") {
        Some(rest) => rest.to_vec(),
        None => name,
    }
}

impl Runtime {
    fn host_hook_levels(&self, thread: Handle<ThreadObj>) -> Result<Vec<Level>, VmError> {
        self.levels(
            &Ctx {
                active: thread,
                func: 0,
                passed: 0,
                framed: true,
            },
            thread,
        )
    }
    pub(crate) fn api_hook_depth(&self, thread: Handle<ThreadObj>) -> crate::Result<usize> {
        Ok(self.host_hook_levels(thread)?.len())
    }
    pub(crate) fn api_hook_info(
        &self,
        thread: Handle<ThreadObj>,
        position: usize,
    ) -> crate::Result<Option<crate::HookInfo>> {
        let levels = self.host_hook_levels(thread)?;
        let Some(level) = levels.get(position) else {
            return Ok(None);
        };
        let info = self.func_info(thread, level.func, Some(level))?;
        let source = match info.source {
            Ok(handle) => self.heap.string_bytes(handle).ok_or(VmError::Corrupt)?,
            Err(bytes) => bytes,
        }
        .to_vec();
        let (namewhat, name) = info
            .name
            .map_or(("", None), |(what, name)| (what, Some(name)));
        let transfer = level
            .frame
            .and_then(|index| {
                let (frame, first, count) = self
                    .heap
                    .hooks
                    .get(self.heap.threads.get(thread)?.id)?
                    .transfer?;
                (frame as usize == index).then_some((first, count))
            })
            .unwrap_or((0, 0));
        Ok(Some(crate::HookInfo {
            name,
            namewhat,
            source,
            short_src: info.short_src,
            what: info.what,
            currentline: info.current_line,
            linedefined: info.line_defined,
            lastlinedefined: info.last_line_defined,
            istailcall: info.tail,
            ftransfer: transfer.0,
            ntransfer: transfer.1,
            nups: info.nups,
            nparams: info.nparams,
            isvararg: info.vararg,
        }))
    }
    pub(crate) fn api_hook_local(
        &self,
        thread: Handle<ThreadObj>,
        position: usize,
        index: i32,
    ) -> crate::Result<Option<(Vec<u8>, crate::ValueRef<'_>)>> {
        let levels = self.host_hook_levels(thread)?;
        if position >= levels.len() {
            return Ok(None);
        }
        let local = self.find_local(thread, &levels, position, index)?;
        Ok(local.map(|(name, slot)| {
            (
                name,
                crate::ValueRef::new(
                    self,
                    self.heap.threads.get(thread).unwrap().stack[slot as usize],
                ),
            )
        }))
    }
    pub(crate) fn api_hook_set_local(
        &mut self,
        thread: Handle<ThreadObj>,
        position: usize,
        index: i32,
        value: &crate::Value,
    ) -> crate::Result<Option<Vec<u8>>> {
        let value = value.raw(self)?;
        let levels = self.host_hook_levels(thread)?;
        if position >= levels.len() {
            return Ok(None);
        }
        let Some((name, slot)) = self.find_local(thread, &levels, position, index)? else {
            return Ok(None);
        };
        self.heap
            .threads
            .get_mut(thread)
            .ok_or(VmError::Corrupt)?
            .stack[slot as usize] = value;
        Ok(Some(name))
    }
    pub(crate) fn api_hook_upvalue(
        &self,
        thread: Handle<ThreadObj>,
        position: usize,
        index: usize,
    ) -> crate::Result<Option<(Vec<u8>, crate::ValueRef<'_>)>> {
        let levels = self.host_hook_levels(thread)?;
        let Some(index) = index.checked_sub(1) else {
            return Ok(None);
        };
        let Some(level) = levels.get(position) else {
            return Ok(None);
        };
        let (name, value) = match level.func {
            Value::Closure(closure) => {
                let object = self.heap.closures.get(closure).ok_or(VmError::Corrupt)?;
                let Some(cell) = object.upvalues.get(index) else {
                    return Ok(None);
                };
                let name = self
                    .closure_proto(closure)?
                    .debug
                    .as_deref()
                    .and_then(|d| d.upvalues.get(index))
                    .map_or_else(|| b"(no name)".to_vec(), Clone::clone);
                (name, self.read_cell(*cell)?)
            }
            Value::NativeClosure(closure) => {
                let Some(value) = self
                    .heap
                    .native_closures
                    .get(closure)
                    .ok_or(VmError::Corrupt)?
                    .values
                    .get(index)
                else {
                    return Ok(None);
                };
                (Vec::new(), *value)
            }
            _ => return Ok(None),
        };
        Ok(Some((name, crate::ValueRef::new(self, value))))
    }
}
