//! Lua IO through the single journaled capability boundary.
use super::library::{Ctx, Next};
use super::*;
use crate::heap::Task;
use crate::hostcaps::{
    CapabilityPoll, CapabilityRequest as Request, CapabilityValue as Answer, HandlePolicy,
    HostIoError, HostIoErrorKind, OpenMode, PipeMode, ProcessStatus, ResourceId, Stream,
};
use crate::iolib::{FILE_CHARGE, FUNCTIONS, FileState, IoFn, IoWork, METHODS};
use crate::userdata::Payload;

pub(crate) fn lines_fits(heap: &Heap, values: &[Value], state: &[i64]) -> bool {
    matches!(state, [0 | 1]) && values.len() <= 251 && values.first().is_some_and(|v| {
        matches!(v, Value::Userdata(h) if heap.userdata.get(*h).is_some_and(|u| matches!(u.payload, Payload::File(_))))
    })
}
fn io_error(text: &[u8]) -> Next {
    Next::Error(LuaFault::Argument, text.to_vec())
}
fn lua_io_message(error: &HostIoError) -> &[u8] {
    let suffix = format!(" (os error {})", error.code);
    error
        .message
        .strip_suffix(suffix.as_bytes())
        .unwrap_or(&error.message)
}
fn bad_fd() -> HostIoError {
    HostIoError {
        kind: HostIoErrorKind::Other,
        code: 9,
        message: b"Bad file descriptor".to_vec(),
    }
}
fn invalid_seek() -> HostIoError {
    HostIoError {
        kind: HostIoErrorKind::InvalidInput,
        code: 22,
        message: b"Invalid argument".to_vec(),
    }
}
fn c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12)
}
fn cstring(bytes: &[u8]) -> &[u8] {
    bytes.split(|b| *b == 0).next().unwrap_or_default()
}
impl Runtime {
    /// Install Lua IO and its shared file metatable. Installation grants no authority.
    pub fn install_io(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("io")?;
        self.register_module("io", table)?;
        for (name, symbol, _) in FUNCTIONS {
            let v = self.native_value(symbol)?;
            self.set_field(table, name, v)?;
        }
        let registry = self.lua_registry()?;
        let mt = self.registry_subtable("FILE*")?;
        self.set_field(Value::Table(mt), "__index", Value::Table(mt))?;
        for (name, symbol, _) in METHODS {
            let v = if name == "__name" {
                Value::String(self.alloc_string(b"FILE*".to_vec())?)
            } else {
                self.native_value(symbol)?
            };
            self.set_field(Value::Table(mt), name, v)?;
        }
        for (kind, name) in [(2, "stdin"), (3, "stdout"), (4, "stderr")] {
            if self.host_capabilities.stdio.is_some() {
                let v = self.make_file(
                    kind,
                    OpenMode::parse(if kind == 2 { b"r" } else { b"w" })
                        .map_err(|_| VmError::Corrupt)?,
                )?;
                self.file_mut(v)?.closed = false;
                // Root each allocation before the next one.
                self.set_field(table, name, v)?;
                if kind < 4 {
                    self.set_field(
                        Value::Table(registry),
                        if kind == 2 { "_IO_input" } else { "_IO_output" },
                        v,
                    )?;
                }
            }
        }
        Ok(())
    }
    fn make_file(&mut self, kind: u8, mode: OpenMode) -> Result<Value, VmError> {
        let policy = if kind == 5 {
            HandlePolicy::Refuse
        } else if kind >= 2 {
            HandlePolicy::Rebind
        } else {
            HandlePolicy::Refuse
        };
        self.ensure_room(crate::heap::userdata_cost(0, FILE_CHARGE))?;
        let h = self
            .heap
            .alloc_userdata(self.max_objects, 0, FILE_CHARGE, || {
                Payload::File(Box::new(FileState {
                    kind,
                    id: ResourceId(0),
                    mode,
                    policy,
                    cursor: 0,
                    closed: true,
                    buffering: if kind == 4 { 0 } else { 1 },
                    write_capacity: if kind == 4 {
                        1
                    } else {
                        crate::iolib::WRITE_CAPACITY
                    },
                    write_buffer: Vec::new(),
                    write_flush: 0,
                    write_next: false,
                    lookahead: None,
                    eof: false,
                    read_buffer: Vec::new(),
                    read_pos: 0,
                }))
            })
            .map_err(VmError::from)?;
        let mt = self.heap.registry.and_then(|r| {
            self.heap
                .table_get_view(r, crate::table::KeyView::string(b"FILE*"))
        });
        let Some(Value::Table(mt)) = mt else {
            return Err(VmError::Corrupt);
        };
        self.heap.set_metatable(Value::Userdata(h), Some(mt));
        Ok(Value::Userdata(h))
    }
    fn file(&self, value: Value) -> Option<&FileState> {
        let Value::Userdata(h) = value else {
            return None;
        };
        match &self.heap.userdata.get(h)?.payload {
            Payload::File(f) => Some(f),
            _ => None,
        }
    }
    fn typed_file(&self, value: Value) -> Option<&FileState> {
        let expected = self.heap.registry.and_then(|r| {
            self.heap
                .table_get_view(r, crate::table::KeyView::string(b"FILE*"))
        });
        let Some(Value::Table(expected)) = expected else {
            return None;
        };
        if self.heap.metatable_of(value) != Some(expected) {
            return None;
        }
        self.file(value)
    }
    fn file_mut(&mut self, value: Value) -> Result<&mut FileState, VmError> {
        let Value::Userdata(h) = value else {
            return Err(VmError::Corrupt);
        };
        match &mut self
            .heap
            .userdata
            .get_mut(h)
            .ok_or(VmError::Corrupt)?
            .payload
        {
            Payload::File(f) => Ok(f),
            _ => Err(VmError::Corrupt),
        }
    }
    /// Replacing read-ahead transfers its charge to the file, not the thread.
    /// The caller reserves room before acquiring external bytes.
    fn file_buffer(&mut self, value: Value, bytes: Vec<u8>) -> Result<(), VmError> {
        let Value::Userdata(h) = value else {
            return Err(VmError::Corrupt);
        };
        let object = self.heap.userdata.get_mut(h).ok_or(VmError::Corrupt)?;
        let Payload::File(file) = &mut object.payload else {
            return Err(VmError::Corrupt);
        };
        let old = object.charge;
        file.read_buffer = bytes;
        file.read_pos = 0;
        let charge = file.charge();
        object.charge = charge;
        if charge > old {
            self.heap.gc.charge(charge - old);
        } else {
            self.heap.give_back(old - charge);
        }
        Ok(())
    }
    /// Transfer pending output charge with the bytes; buffers are heap roots.
    fn file_output(&mut self, value: Value, bytes: Vec<u8>, flush: u32) -> Result<(), VmError> {
        let Value::Userdata(h) = value else {
            return Err(VmError::Corrupt);
        };
        let object = self.heap.userdata.get_mut(h).ok_or(VmError::Corrupt)?;
        let Payload::File(file) = &mut object.payload else {
            return Err(VmError::Corrupt);
        };
        let old = object.charge;
        file.write_buffer = bytes;
        file.write_flush = flush;
        file.write_next = false;
        let charge = file.charge();
        object.charge = charge;
        if charge > old {
            self.heap.gc.charge(charge - old);
        } else {
            self.heap.give_back(old - charge);
        }
        Ok(())
    }
    /// A pending request retains identical bytes/cursor until its journaled
    /// completion is consumed. Appends return the backend's resulting end.
    fn drain_output(
        &mut self,
        value: Value,
        journal: &mut Journal,
    ) -> Result<Option<Result<(), HostIoError>>, VmError> {
        let f = self.file(value).ok_or(VmError::Corrupt)?;
        let n = if f.write_flush == 0 {
            f.write_buffer.len()
        } else {
            f.write_flush as usize
        };
        let cursor = f
            .cursor
            .checked_sub(f.write_buffer.len() as u64)
            .ok_or(VmError::Corrupt)?;
        let bytes = f.write_buffer[..n].to_vec();
        let req = match f.kind {
            5 => Request::ProcessWriteAt {
                id: f.id,
                offset: cursor,
                bytes,
            },
            3 => Request::StdioWriteStdout { bytes },
            4 => Request::StdioWriteStderr { bytes },
            _ if f.mode.append => Request::FilesystemAppend { id: f.id, bytes },
            _ => Request::FilesystemWriteAt {
                id: f.id,
                offset: cursor,
                bytes,
            },
        };
        let append = f.mode.append && f.kind < 2;
        let result = match self.capability(&req, journal)? {
            CapabilityPoll::Waiting(_) => return Ok(None),
            CapabilityPoll::Ready(result) => result,
        };
        let result = match result {
            Ok(Answer::Unsigned(w)) if append || w == n as u64 => {
                if append {
                    let remaining =
                        self.file(value).ok_or(VmError::Corrupt)?.write_buffer.len() - n;
                    self.file_mut(value)?.cursor = w
                        .checked_add(remaining as u64)
                        .filter(|n| *n <= i64::MAX as u64)
                        .ok_or(VmError::Corrupt)?;
                }
                Ok(())
            }
            Ok(Answer::Unsigned(_)) => Err(HostIoError::new(
                HostIoErrorKind::Other,
                b"short write".to_vec(),
            )),
            Err(e) => Err(e),
            _ => return Err(VmError::Corrupt),
        };
        let tail = self.file(value).ok_or(VmError::Corrupt)?.write_buffer[n..].to_vec();
        self.file_output(value, tail, 0)?;
        Ok(Some(result))
    }
    fn default_file(&self, input: bool) -> Value {
        self.heap
            .registry
            .and_then(|r| {
                self.heap.table_get_view(
                    r,
                    crate::table::KeyView::string(if input { b"_IO_input" } else { b"_IO_output" }),
                )
            })
            .unwrap_or(Value::Nil)
    }
    fn file_check(
        &self,
        ctx: &Ctx,
        value: Value,
        arg: u32,
        default: Option<bool>,
    ) -> Result<(), Next> {
        let Some(f) = self.typed_file(value) else {
            if let Some(input) = default {
                return Err(io_error(if input {
                    b"default input file is closed"
                } else {
                    b"default output file is closed"
                }));
            }
            return Err(self.bad_type(ctx, arg, "FILE*"));
        };
        if f.closed {
            return Err(io_error(match default {
                Some(true) => b"default input file is closed",
                Some(false) => b"default output file is closed",
                None => b"attempt to use a closed file",
            }));
        }
        Ok(())
    }
    fn io_release(&mut self, active: Handle<ThreadObj>, n: u64) {
        if let Some(t) = self.heap.threads.get_mut(active) {
            t.charged_held = t.charged_held.saturating_sub(n);
        }
        self.heap.give_back(n);
    }
    fn io_slot(&mut self, ctx: &Ctx, n: u32, v: Value) -> Result<(), VmError> {
        self.write_abs(ctx.active, ctx.scratch_slot(n), v)
    }
    fn io_text(&mut self, ctx: &Ctx, n: u32) -> Result<Result<Vec<u8>, Next>, VmError> {
        Ok(match self.string_arg(ctx, n)? {
            Some(h) => Ok(self.heap.string_bytes(h).ok_or(VmError::Corrupt)?.to_vec()),
            None => Err(self.bad_type(ctx, n, "string")),
        })
    }
    fn io_bad_option(&self, ctx: &Ctx, index: u32, option: &[u8]) -> Next {
        let mut message = b"invalid option '".to_vec();
        message.extend(option);
        message.push(b'\'');
        let (name, method) = self.argument_name(ctx);
        io_error(&super::library::arg_error_text(
            &name, method, index, &message,
        ))
    }
    fn io_lines(&mut self, ctx: &Ctx, start: u32, owned: bool) -> Result<Next, VmError> {
        let count = ctx.passed.saturating_sub(start);
        if count > 250 {
            return Ok(self.bad_arg(ctx, 251, "too many arguments"));
        }
        let file = self.scratch(ctx, 0);
        let mut captures = vec![file];
        captures.extend((start..ctx.passed).map(|i| self.lib_arg(ctx, i)));
        let Value::Native(native) = self.native_value("io.linesstep")? else {
            return Err(VmError::Corrupt);
        };
        let closure = Value::NativeClosure(self.alloc_native_closure(
            native,
            captures,
            vec![owned as i64],
        )?);
        Ok(Next::Done(if owned {
            vec![closure, Value::Nil, Value::Nil, file]
        } else {
            vec![closure]
        }))
    }
    pub(super) fn call_io(
        &mut self,
        active: Handle<ThreadObj>,
        function: IoFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        macro_rules! check {
            ($v:expr) => {
                match $v {
                    Ok(v) => v,
                    Err(n) => return Ok(self.finish_next(active, n)),
                }
            };
        }
        if function == IoFn::Type {
            let bytes = self.typed_file(self.lib_arg(&ctx, 0)).map(|f| {
                if f.closed {
                    b"closed file".as_slice()
                } else {
                    b"file".as_slice()
                }
            });
            let value = match bytes {
                Some(b) => Value::String(self.alloc_string(b.to_vec())?),
                None => Value::Nil,
            };
            return self.base_return(active, &[value]);
        }
        if function == IoFn::LinesStep {
            let callee = self
                .heap
                .threads
                .get(active)
                .and_then(|t| t.stack.get(func as usize))
                .copied()
                .ok_or(VmError::Corrupt)?;
            let Value::NativeClosure(h) = callee else {
                return Err(VmError::Corrupt);
            };
            let c = self.heap.native_closures.get(h).ok_or(VmError::Corrupt)?;
            if !lines_fits(&self.heap, &c.values, &c.state) {
                return Err(VmError::Corrupt);
            }
            let file = c.values[0];
            let count = c.values.len() as u32 - 1;
            let toclose = c.state[0] == 1;
            if self.file(file).is_none_or(|f| f.closed) {
                return Ok(self.finish_next(active, io_error(b"file is already closed")));
            }
            if let Some(line) = self.buffered_line(&ctx, file, 0, count, true)? {
                return self.base_return(active, &[line]);
            }
            self.io_slot(&ctx, 0, file)?;
            return self.start_io(
                ctx,
                IoWork::Read {
                    start: 0,
                    count,
                    got: 0,
                    format: 0,
                    remaining: 0,
                    buffer: Vec::new(),
                    numeral: 0,
                    digits: 0,
                    hex: false,
                    iterator: true,
                    toclose,
                },
                journal,
            );
        }
        let mut start = 1;
        let value;
        if matches!(function, IoFn::Open | IoFn::Tmpfile | IoFn::Popen) {
            let path = if function == IoFn::Tmpfile {
                Vec::new()
            } else {
                check!(self.io_text(&ctx, 0)?)
            };
            let mode = if function == IoFn::Tmpfile {
                b"w+".to_vec()
            } else if !self.given(&ctx, 1) {
                b"r".to_vec()
            } else {
                check!(self.io_text(&ctx, 1)?)
            };
            let mode = cstring(&mode);
            let pipe = function == IoFn::Popen;
            let valid = if pipe {
                matches!(mode, b"r" | b"w")
            } else {
                matches!(mode.first(), Some(b'r' | b'w' | b'a'))
                    && mode[1..]
                        .strip_prefix(b"+")
                        .unwrap_or(&mode[1..])
                        .iter()
                        .all(|b| *b == b'b')
            };
            if !valid {
                return Ok(self.finish_next(active, self.bad_arg(&ctx, 1, "invalid mode")));
            }
            let m = OpenMode {
                read: mode[0] == b'r' || mode.contains(&b'+'),
                write: mode[0] != b'r' || mode.contains(&b'+'),
                append: mode[0] == b'a',
                create: mode[0] != b'r',
                truncate: mode[0] == b'w',
                binary: mode.contains(&b'b'),
            };
            value = self.make_file(
                if pipe {
                    5
                } else if function == IoFn::Tmpfile {
                    1
                } else {
                    0
                },
                m,
            )?;
            self.io_slot(&ctx, 0, value)?;
            let path = cstring(&path).to_vec();
            self.ensure_room(path.len() as u64)?;
            self.heap.charge_held(path.len() as u64);
            return self.start_io(
                ctx,
                IoWork::Open {
                    path: cstring(&path).to_vec(),
                    action: if pipe { 4 } else { 0 },
                },
                journal,
            );
        }
        if matches!(function, IoFn::Input | IoFn::Output) {
            let input = function == IoFn::Input;
            let arg = self.lib_arg(&ctx, 0);
            if !self.given(&ctx, 0) {
                return self.base_return(active, &[self.default_file(input)]);
            }
            if matches!(arg, Value::String(_) | Value::Integer(_) | Value::Float(_)) {
                let path = check!(self.io_text(&ctx, 0)?);
                value = self.make_file(
                    0,
                    OpenMode::parse(if input { b"r" } else { b"w" })
                        .map_err(|_| VmError::Corrupt)?,
                )?;
                self.io_slot(&ctx, 0, value)?;
                let path = cstring(&path).to_vec();
                self.ensure_room(path.len() as u64)?;
                self.heap.charge_held(path.len() as u64);
                return self.start_io(
                    ctx,
                    IoWork::Open {
                        path: cstring(&path).to_vec(),
                        action: if input { 1 } else { 2 },
                    },
                    journal,
                );
            }
            check!(self.file_check(&ctx, arg, 0, None));
            let registry = self.lua_registry()?;
            self.set_field(
                Value::Table(registry),
                if input { "_IO_input" } else { "_IO_output" },
                arg,
            )?;
            return self.base_return(active, &[arg]);
        }
        if function == IoFn::Lines && self.given(&ctx, 0) {
            let path = check!(self.io_text(&ctx, 0)?);
            value = self.make_file(0, OpenMode::parse(b"r").map_err(|_| VmError::Corrupt)?)?;
            self.io_slot(&ctx, 0, value)?;
            let path = cstring(&path).to_vec();
            self.ensure_room(path.len() as u64)?;
            self.heap.charge_held(path.len() as u64);
            return self.start_io(
                ctx,
                IoWork::Open {
                    path: cstring(&path).to_vec(),
                    action: 3,
                },
                journal,
            );
        }
        let default = match function {
            IoFn::Read => Some(true),
            IoFn::Write | IoFn::Flush => Some(false),
            IoFn::Close if ctx.passed == 0 => Some(false),
            IoFn::Lines => Some(true),
            _ => None,
        };
        value = if let Some(input) = default {
            self.default_file(input)
        } else {
            self.lib_arg(&ctx, 0)
        };
        if function == IoFn::Gc {
            let Some(f) = self.typed_file(value) else {
                return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "FILE*")));
            };
            if f.closed || ((2..=4).contains(&f.kind) && !self.heap.finalizers.closing) {
                return self.base_return(active, &[]);
            }
        } else if function == IoFn::ToString {
            let Some(f) = self.typed_file(value) else {
                return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "FILE*")));
            };
            let text = if f.closed {
                b"file (closed)".to_vec()
            } else {
                format!(
                    "file (0x{:08x})",
                    self.heap
                        .object_id_of_value(value)
                        .ok_or(VmError::Corrupt)?
                        .raw()
                )
                .into_bytes()
            };
            let v = Value::String(self.alloc_string(text)?);
            return self.base_return(active, &[v]);
        } else {
            check!(self.file_check(&ctx, value, 0, default));
        }
        if matches!(function, IoFn::Read | IoFn::Write) {
            start = 0;
        }
        if matches!(function, IoFn::Read | IoFn::FileRead)
            && let Some(line) =
                self.buffered_line(&ctx, value, start, passed.saturating_sub(start), false)?
        {
            return self.base_return(active, &[line]);
        }
        self.io_slot(&ctx, 0, value)?;
        let work = match function {
            IoFn::Lines | IoFn::FileLines => {
                let n = self.io_lines(&ctx, 1, false)?;
                return match n {
                    Next::Done(v) => self.base_return(active, &v),
                    n => Ok(self.finish_next(active, n)),
                };
            }
            IoFn::Close | IoFn::FileClose | IoFn::Gc => IoWork::Close {
                quiet: function == IoFn::Gc,
                exhausted: false,
                failed: false,
            },
            IoFn::Flush | IoFn::FileFlush => IoWork::Flush,
            IoFn::Read | IoFn::FileRead => IoWork::Read {
                start,
                count: passed.saturating_sub(start),
                got: 0,
                format: 0,
                remaining: 0,
                buffer: Vec::new(),
                numeral: 0,
                digits: 0,
                hex: false,
                iterator: false,
                toclose: false,
            },
            IoFn::Write | IoFn::FileWrite => IoWork::Write {
                start,
                next: start,
                offset: 0,
                failed: false,
            },
            IoFn::Seek => {
                let whence = if !self.given(&ctx, 1) {
                    b"cur".to_vec()
                } else {
                    check!(self.io_text(&ctx, 1)?)
                };
                let whence = match cstring(&whence) {
                    b"set" => 0,
                    b"cur" => 1,
                    b"end" => 2,
                    _ => {
                        return Ok(
                            self.finish_next(active, self.io_bad_option(&ctx, 1, cstring(&whence)))
                        );
                    }
                };
                IoWork::Seek {
                    whence,
                    offset: check!(self.opt_int_arg(&ctx, 2, 0)),
                }
            }
            IoFn::Setvbuf => {
                let mode = check!(self.io_text(&ctx, 1)?);
                let buffering = match cstring(&mode) {
                    b"no" => 0,
                    b"full" => 1,
                    b"line" => 2,
                    _ => {
                        return Ok(
                            self.finish_next(active, self.io_bad_option(&ctx, 1, cstring(&mode)))
                        );
                    }
                };
                let _size = check!(self.opt_int_arg(&ctx, 2, 1024));
                IoWork::Setvbuf { buffering }
            }
            _ => return Err(VmError::Corrupt),
        };
        self.start_io(ctx, work, journal)
    }
    /// A complete line already in read-ahead needs only its result string.
    /// Longer lines and multi-format calls keep the checkpointed work machine.
    fn buffered_line(
        &mut self,
        ctx: &Ctx,
        value: Value,
        start: u32,
        count: u32,
        iterator: bool,
    ) -> Result<Option<Value>, VmError> {
        if count > 1 {
            return Ok(None);
        }
        let format = if count == 0 {
            1
        } else {
            match self.io_format(ctx, start, iterator)? {
                Ok((format @ (1 | 2), _)) => format,
                _ => return Ok(None),
            }
        };
        let file = self.file(value).ok_or(VmError::Corrupt)?;
        if !file.mode.read || file.lookahead.is_some() || !file.write_buffer.is_empty() {
            return Ok(None);
        }
        let bytes = &file.read_buffer[file.read_pos as usize..];
        let Some(newline) = bytes.iter().position(|b| *b == b'\n') else {
            return Ok(None);
        };
        let len = newline + usize::from(format == 2);
        if len > self.heap.max_string {
            return Err(VmError::MemoryLimit);
        }
        let bytes = bytes[..len].to_vec();
        let result = Value::String(self.alloc_string(bytes)?);
        let file = self.file_mut(value)?;
        file.read_pos += newline as u32 + 1;
        file.cursor = file
            .cursor
            .checked_add(newline as u64 + 1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(VmError::Corrupt)?;
        file.eof = false;
        Ok(Some(result))
    }
    fn start_io(
        &mut self,
        mut ctx: Ctx,
        work: IoWork,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        // A frame exists before any pending operation or external mutation.
        let work = Box::new(work);
        self.save_io(&ctx, work.clone())?;
        ctx.framed = true;
        self.run_io(ctx, work, journal)
    }
    fn save_io(&mut self, ctx: &Ctx, work: Box<IoWork>) -> Result<(), VmError> {
        if !ctx.framed {
            return self.push_builtin(ctx.active, Task::Io(work));
        }
        let object = self
            .heap
            .threads
            .get_mut(ctx.active)
            .ok_or(VmError::Corrupt)?;
        match object
            .frames
            .last_mut()
            .and_then(|frame| frame.boundary_mut())
        {
            Some(Boundary::Builtin {
                task: Task::Io(slot),
                ..
            }) => {
                *slot = work;
                Ok(())
            }
            _ => Err(VmError::Corrupt),
        }
    }
    pub(super) fn finish_io(
        &mut self,
        ctx: Ctx,
        slot: u32,
        work: Box<IoWork>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let object = self
            .heap
            .threads
            .get_mut(ctx.active)
            .ok_or(VmError::Corrupt)?;
        object.stack.truncate(slot as usize);
        object.top = slot;
        self.run_io(ctx, work, journal)
    }
    #[inline(never)]
    pub(super) fn run_io(
        &mut self,
        ctx: Ctx,
        mut work: Box<IoWork>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let outer = self.heap.working.replace(ctx.active.index);
        let next = self.io_next(&ctx, &mut work, journal);
        self.heap.working = outer;
        // Keep all work on an allocation/host failure too, so unwind and snapshots are valid.
        match next {
            Ok(Next::Done(values)) => self.builtin_done(ctx.active, &values),
            Ok(Next::Busy) => {
                self.save_io(&ctx, work)?;
                Ok(Poll::Continue)
            }
            Ok(Next::Error(fault, text)) => {
                self.save_io(&ctx, work)?;
                {
                    let error = self.prefixed(self.nearest_lua_location(ctx.active), text, fault);
                    Ok(self.throw_on(ctx.active, fault, error))
                }
            }
            Ok(Next::Fault(fault)) => {
                self.save_io(&ctx, work)?;
                Ok(self.finish_next(ctx.active, Next::Fault(fault)))
            }
            Ok(Next::Op(_)) => Err(VmError::Corrupt),
            Err(e) => {
                self.save_io(&ctx, work)?;
                Err(e)
            }
        }
    }
    pub(super) fn fileresult(
        &mut self,
        error: HostIoError,
        path: Option<&[u8]>,
    ) -> Result<Vec<Value>, VmError> {
        let mut text = Vec::new();
        if let Some(path) = path {
            text.extend(cstring(path));
            text.extend(b": ");
        }
        text.extend(lua_io_message(&error));
        let message = Value::String(self.alloc_string(text)?);
        Ok(vec![Value::Nil, message, Value::Integer(error.code as i64)])
    }
    fn open_error(&self, path: &[u8], error: &HostIoError) -> Next {
        let mut text = b"cannot open file '".to_vec();
        text.extend(cstring(path));
        text.extend(b"' (");
        text.extend(lua_io_message(error));
        text.push(b')');
        io_error(&text)
    }
    fn write_failure(&mut self, ctx: &Ctx, error: HostIoError) -> Result<(), VmError> {
        let values = self.fileresult(error, None)?;
        self.io_slot(ctx, 1, values[1])?;
        self.io_slot(ctx, 2, values[2])
    }
    fn io_format(
        &mut self,
        ctx: &Ctx,
        index: u32,
        iterator: bool,
    ) -> Result<Result<(u8, u64), Next>, VmError> {
        let value = if iterator {
            let Value::NativeClosure(h) = self
                .heap
                .threads
                .get(ctx.active)
                .and_then(|t| t.stack.get(ctx.func as usize))
                .copied()
                .ok_or(VmError::Corrupt)?
            else {
                return Err(VmError::Corrupt);
            };
            self.heap
                .native_closures
                .get(h)
                .and_then(|c| c.values.get(1 + index as usize))
                .copied()
                .ok_or(VmError::Corrupt)?
        } else {
            self.lib_arg(ctx, index)
        };
        if matches!(value, Value::Integer(_) | Value::Float(_)) {
            let n = match value {
                Value::Integer(n) => Some(n),
                Value::Float(n) => crate::compare::float_to_int(n),
                _ => None,
            };
            return Ok(n
                .map(|n| (if n == 0 { 6 } else { 4 }, n as u64))
                .ok_or_else(|| {
                    self.bad_arg(
                        ctx,
                        if iterator { index + 1 } else { index },
                        "number has no integer representation",
                    )
                }));
        }
        let Some(bytes) = super::builtins::text_arg(&self.heap, value) else {
            return Ok(Err(self.bad_type(
                ctx,
                if iterator { index + 1 } else { index },
                "string",
            )));
        };
        let bytes = bytes.strip_prefix(b"*").unwrap_or(&bytes);
        Ok(match bytes.first() {
            Some(b'l') => Ok((1, 0)),
            Some(b'L') => Ok((2, 0)),
            Some(b'a') => Ok((3, 0)),
            Some(b'n') => Ok((5, 0)),
            _ => Err(self.bad_arg(
                ctx,
                if iterator { index + 1 } else { index },
                "invalid format",
            )),
        })
    }
    fn io_next(
        &mut self,
        ctx: &Ctx,
        work: &mut IoWork,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        let value = self.scratch(ctx, 0);
        let f = self.file(value).ok_or(VmError::Corrupt)?.parameters();
        macro_rules! host {
            ($req:expr) => {
                match self.capability(&$req, journal)? {
                    CapabilityPoll::Waiting(_) => return Ok(Next::Busy),
                    CapabilityPoll::Ready(result) => result,
                }
            };
        }
        // PUC validates the next read format before asking stdio to read
        // (and thereby flush). An invalid format must not expose output.
        if let IoWork::Read {
            start,
            count,
            got,
            format: 0,
            iterator,
            ..
        } = work
            && *count != 0
            && *got < *count
            && let Err(next) = self.io_format(ctx, *start + *got, *iterator)?
        {
            return Ok(next);
        }
        let output = self.file(value).ok_or(VmError::Corrupt)?;
        let drain = match work {
            IoWork::Write { .. } => output.write_flush != 0 || output.write_next,
            IoWork::Setvbuf { buffering } => *buffering == 0,
            IoWork::Close { .. } if (2..=4).contains(&f.kind) => self.heap.finalizers.closing,
            _ => true,
        };
        if !output.write_buffer.is_empty() && drain {
            let Some(result) = self.drain_output(value, journal)? else {
                return Ok(Next::Busy);
            };
            if let Err(e) = result {
                match work {
                    IoWork::Write {
                        failed,
                        next,
                        offset,
                        ..
                    } => {
                        self.write_failure(ctx, e)?;
                        *failed = true;
                        *next += 1;
                        *offset = 0;
                    }
                    IoWork::Close { failed, .. } => {
                        self.write_failure(ctx, e)?;
                        *failed = true;
                    }
                    IoWork::Read { iterator: true, .. } => return Ok(io_error(lua_io_message(&e))),
                    _ => return Ok(Next::Done(self.fileresult(e, None)?)),
                }
            }
            return Ok(Next::Busy);
        }
        match work {
            IoWork::Open { path, action } => {
                // A waiting acquisition has already captured policy. Do not
                // run host metadata again after its resource may be acquired.
                let pending = self
                    .heap
                    .threads
                    .get(ctx.active)
                    .and_then(|t| t.frames.last())
                    .is_some_and(|frame| {
                        matches!(frame.pending(), Some(Pending::Capability { .. }))
                    });
                if f.kind < 2
                    && !pending
                    && let Some(fs) = &self.host_capabilities.filesystem
                {
                    match crate::hostcaps::filesystem_policy(fs.as_ref()) {
                        Ok(policy) => self.file_mut(value)?.policy = policy,
                        Err(error) => {
                            return if (1..=3).contains(action) {
                                Ok(self.open_error(path, &error))
                            } else {
                                Ok(Next::Done(self.fileresult(
                                    error,
                                    if f.kind == 1 { None } else { Some(path) },
                                )?))
                            };
                        }
                    }
                }
                let req = if *action == 4 {
                    Request::ProcessPopen {
                        cmd: path.clone(),
                        mode: if f.mode.read {
                            PipeMode::Read
                        } else {
                            PipeMode::Write
                        },
                    }
                } else if f.kind == 1 {
                    Request::FilesystemTempFile
                } else {
                    Request::FilesystemOpen {
                        path: path.clone(),
                        mode: f.mode,
                    }
                };
                match host!(req) {
                    Ok(Answer::Resource(id)) => {
                        let f = self.file_mut(value)?;
                        f.id = id;
                        f.closed = false;
                    }
                    Ok(_) => return Err(VmError::Corrupt),
                    Err(mut e) => {
                        if *action != 4 && self.host_capabilities.filesystem.is_none() {
                            e.message = b"no filesystem access".to_vec();
                        }
                        return if (1..=3).contains(action) {
                            Ok(self.open_error(path, &e))
                        } else {
                            Ok(Next::Done(self.fileresult(
                                e,
                                if f.kind == 1 { None } else { Some(path) },
                            )?))
                        };
                    }
                }
                let action = *action;
                let n = path.len();
                path.clear();
                self.io_release(ctx.active, n as u64);
                if action == 1 || action == 2 {
                    let r = self.lua_registry()?;
                    self.set_field(
                        Value::Table(r),
                        if action == 1 {
                            "_IO_input"
                        } else {
                            "_IO_output"
                        },
                        value,
                    )?;
                }
                if action == 3 {
                    return self.io_lines(ctx, 1, true);
                }
                Ok(Next::Done(vec![value]))
            }
            IoWork::Close {
                quiet,
                exhausted,
                failed,
            } => {
                if f.closed {
                    return Ok(Next::Done(vec![]));
                }
                if (2..=4).contains(&f.kind) {
                    if *quiet && self.heap.finalizers.closing {
                        *work = IoWork::Flush;
                        return Ok(Next::Busy);
                    }
                    let message =
                        Value::String(self.alloc_string(b"cannot close standard file".to_vec())?);
                    return Ok(Next::Done(if *quiet {
                        vec![]
                    } else {
                        vec![Value::Nil, message]
                    }));
                }
                let result = host!(if f.kind == 5 {
                    Request::ProcessClose { id: f.id }
                } else {
                    Request::FilesystemClose { id: f.id }
                });
                // PUC marks closed even when the backend reports a close failure.
                let f = self.file_mut(value)?;
                f.closed = true;
                f.lookahead = None;
                self.file_buffer(value, Vec::new())?;
                if *quiet || *exhausted {
                    return Ok(Next::Done(vec![]));
                }
                if *failed {
                    return Ok(Next::Done(vec![
                        Value::Nil,
                        self.scratch(ctx, 1),
                        self.scratch(ctx, 2),
                    ]));
                }
                match result {
                    Ok(Answer::Unit) => Ok(Next::Done(vec![Value::Bool(true)])),
                    Ok(Answer::Status(s)) => self.pipe_result(s),
                    Err(e) => Ok(Next::Done(self.fileresult(e, None)?)),
                    _ => Err(VmError::Corrupt),
                }
            }
            IoWork::Flush => {
                let req = match f.kind {
                    0 | 1 => Request::FilesystemFlush { id: f.id },
                    5 => Request::ProcessFlush { id: f.id },
                    _ => Request::StdioFlush {
                        stream: match f.kind {
                            2 => Stream::Stdin,
                            3 => Stream::Stdout,
                            _ => Stream::Stderr,
                        },
                    },
                };
                match host!(req) {
                    Ok(Answer::Unit) => {
                        if f.kind < 2 {
                            let file = self.file_mut(value)?;
                            file.cursor -= file.lookahead.is_some() as u64;
                            file.lookahead = None;
                            self.file_buffer(value, Vec::new())?;
                        }
                        Ok(Next::Done(vec![Value::Bool(true)]))
                    }
                    Err(e) => Ok(Next::Done(self.fileresult(e, None)?)),
                    _ => Err(VmError::Corrupt),
                }
            }
            IoWork::Setvbuf { buffering } => {
                let file = self.file_mut(value)?;
                file.write_next = *buffering == 1
                    && (file.buffering == 2 || file.write_next)
                    && !file.write_buffer.is_empty();
                if *buffering == 0 {
                    file.write_capacity = 1;
                }
                file.buffering = *buffering;
                Ok(Next::Done(vec![Value::Bool(true)]))
            }
            IoWork::Seek { whence, offset } => {
                if f.kind >= 2 {
                    return Ok(Next::Done(self.fileresult(
                        HostIoError {
                            kind: HostIoErrorKind::Other,
                            code: 29,
                            message: b"Illegal seek".to_vec(),
                        },
                        None,
                    )?));
                }
                let base = match *whence {
                    0 => 0,
                    1 => f.cursor - f.lookahead.is_some() as u64,
                    _ => match host!(Request::FilesystemSize { id: f.id }) {
                        Ok(Answer::Unsigned(n)) => n,
                        Err(e) => return Ok(Next::Done(self.fileresult(e, None)?)),
                        _ => return Err(VmError::Corrupt),
                    },
                };
                let pos = i128::from(base) + i128::from(*offset);
                if !(0..i64::MAX as i128).contains(&pos) {
                    return Ok(Next::Done(self.fileresult(invalid_seek(), None)?));
                }
                let f = self.file_mut(value)?;
                f.cursor = pos as u64;
                f.lookahead = None;
                f.eof = false;
                self.file_buffer(value, Vec::new())?;
                Ok(Next::Done(vec![Value::Integer(pos as i64)]))
            }
            IoWork::Write {
                next,
                offset,
                failed,
                ..
            } => {
                if *next >= ctx.passed {
                    return Ok(Next::Done(if *failed {
                        vec![Value::Nil, self.scratch(ctx, 1), self.scratch(ctx, 2)]
                    } else {
                        vec![value]
                    }));
                }
                let arg = self.lib_arg(ctx, *next);
                let bytes = match arg {
                    Value::Float(n) => {
                        let mut spec = crate::strformat::Spec::new(b'g');
                        spec.precision = Some(14);
                        let mut b = Vec::new();
                        if n.is_nan() && n.is_sign_negative() {
                            b.extend(b"-nan");
                        } else {
                            crate::strformat::format_float(&spec, n, &mut b);
                        }
                        std::borrow::Cow::Owned(b)
                    }
                    _ => match super::builtins::text_arg(&self.heap, arg) {
                        Some(b) => b,
                        None => return Ok(self.bad_type(ctx, *next, "string")),
                    },
                };
                if *failed || *offset as usize >= bytes.len() {
                    *next += 1;
                    *offset = 0;
                    return Ok(Next::Busy);
                }
                if !f.mode.write {
                    self.write_failure(ctx, bad_fd())?;
                    *failed = true;
                    *next += 1;
                    *offset = 0;
                    return Ok(Next::Busy);
                }
                let at = *offset as usize;
                let pending = self.file(value).ok_or(VmError::Corrupt)?.write_buffer.len();
                let capacity = f.write_capacity as usize;
                // With no pending prefix libc writes whole capacity blocks
                // directly. Filling an existing buffer exactly stays pending.
                let available = bytes.len() - at;
                let direct = pending == 0
                    && (f.buffering == 0
                        || (available >= capacity && !(f.buffering == 2 && capacity == 1)));
                let n = if f.buffering == 0 {
                    available.min(65536)
                } else if direct {
                    (available / capacity * capacity).min(65536)
                } else {
                    available.min(capacity - pending)
                };
                if n == 0 {
                    self.file_mut(value)?.write_flush = pending as u32;
                    return Ok(Next::Busy);
                }
                let chunk = bytes[at..at + n].to_vec();
                self.ensure_room(n as u64)?;
                // Synchronize read-ahead before the first accepted write.
                if pending == 0 {
                    let f = self.file_mut(value)?;
                    f.cursor -= f.lookahead.is_some() as u64;
                    f.lookahead = None;
                    f.eof = false;
                    self.file_buffer(value, Vec::new())?;
                }
                let mut output = self
                    .file(value)
                    .ok_or(VmError::Corrupt)?
                    .write_buffer
                    .clone();
                output.extend(&chunk);
                let flush = if f.buffering == 0 || direct || available > n {
                    output.len()
                } else if f.buffering == 2 {
                    output
                        .iter()
                        .rposition(|b| *b == b'\n')
                        .map_or(0, |n| n + 1)
                } else {
                    0
                };
                let position = self
                    .file(value)
                    .ok_or(VmError::Corrupt)?
                    .cursor
                    .checked_add(n as u64)
                    .filter(|n| *n <= i64::MAX as u64)
                    .ok_or(VmError::Corrupt)?;
                self.file_output(value, output, flush as u32)?;
                self.file_mut(value)?.cursor = position;
                *offset += n as u64;
                Ok(Next::Busy)
            }
            IoWork::Read {
                start,
                count,
                got,
                format,
                remaining,
                buffer,
                numeral,
                digits,
                hex,
                iterator,
                toclose,
            } => {
                if *format == 0 {
                    if *got >= (*count).max(1) {
                        return Ok(Next::Done(
                            (1..=(*got)).map(|i| self.scratch(ctx, i)).collect(),
                        ));
                    }
                    let spec = if *count == 0 {
                        (1, 0)
                    } else {
                        match self.io_format(ctx, *start + *got, *iterator)? {
                            Ok(s) => s,
                            Err(n) => return Ok(n),
                        }
                    };
                    *format = spec.0;
                    *remaining = spec.1;
                    *numeral = 0;
                    *digits = 0;
                    *hex = false;
                }
                if *format == 4 && *remaining > self.heap.max_string as u64 {
                    return Err(VmError::MemoryLimit);
                }
                if !f.mode.read {
                    let values = self.fileresult(bad_fd(), None)?;
                    if *iterator && let Value::String(h) = values[1] {
                        return Ok(io_error(self.heap.string_bytes(h).ok_or(VmError::Corrupt)?));
                    }
                    return Ok(Next::Done(values));
                }
                let old_len = buffer.len();
                let max = match *format {
                    3 => 65536,
                    4 => (*remaining).min(65536) as usize,
                    1 | 2 => crate::iolib::READ_CHUNK,
                    _ => 1,
                }
                .min(self.heap.max_string.saturating_sub(old_len).max(1));
                let looked = f.lookahead.is_some();
                let file = self.file(value).ok_or(VmError::Corrupt)?;
                let refill = !looked && file.read_pos as usize == file.read_buffer.len();
                if refill {
                    let chunk = if matches!(*format, 3 | 4) {
                        65536
                    } else {
                        crate::iolib::READ_CHUNK
                    };
                    // Reserve buffer plus result work before the journaled read.
                    self.ensure_room(chunk as u64 + max as u64 + cost::OBJECT + 128)?;
                    let req = match f.kind {
                        5 => Request::ProcessReadAt {
                            id: f.id,
                            offset: f.cursor,
                            max: chunk,
                        },
                        2 => Request::StdioReadStdin { max: chunk },
                        _ => Request::FilesystemReadAt {
                            id: f.id,
                            offset: f.cursor,
                            max: chunk,
                        },
                    };
                    match host!(req) {
                        Ok(Answer::Bytes(bytes)) => self.file_buffer(value, bytes)?,
                        Err(e) => {
                            return if *iterator {
                                Ok(io_error(lua_io_message(&e)))
                            } else {
                                Ok(Next::Done(self.fileresult(e, None)?))
                            };
                        }
                        _ => return Err(VmError::Corrupt),
                    }
                } else {
                    self.ensure_room(max as u64 + cost::OBJECT + 128)?;
                }
                let look = [f.lookahead.unwrap_or(0)];
                let file = self.file(value).ok_or(VmError::Corrupt)?;
                let bytes = if looked {
                    &look[..]
                } else {
                    let start = file.read_pos as usize;
                    &file.read_buffer[start..file.read_buffer.len().min(start + max)]
                };
                let eof = bytes.is_empty();
                let mut used = bytes.len();
                let mut done = eof;
                let mut success = true;
                if *format == 5 {
                    let b = bytes.first().copied();
                    let mut accepted = numeral_byte(buffer, numeral, digits, hex, b);
                    if accepted && buffer.len() >= 200 {
                        *numeral = 10;
                        *digits = (*digits).min(200);
                        accepted = false;
                    }
                    if accepted {
                        if let Some(b) = b
                            && (!c_space(b) || *numeral != 0)
                        {
                            buffer.push(b);
                        }
                    } else {
                        done = true;
                        used = 0;
                    }
                    if buffer.len() == 200 && accepted { /* next character determines overflow */ }
                } else if *format == 6 {
                    done = true;
                    success = !eof;
                    used = 0;
                } else if *format == 1 || *format == 2 {
                    if let Some(i) = bytes.iter().position(|b| *b == b'\n') {
                        used = i + 1;
                        done = true;
                        buffer.extend(&bytes[..if *format == 2 { used } else { i }]);
                    } else {
                        buffer.extend(bytes);
                    }
                    success = !buffer.is_empty() || (!eof && done);
                } else {
                    buffer.extend(bytes);
                    if *format == 4 {
                        *remaining = remaining
                            .checked_sub(bytes.len() as u64)
                            .ok_or(VmError::Corrupt)?;
                        done |= *remaining == 0;
                        success = !buffer.is_empty();
                    }
                }
                if buffer.len() > self.heap.max_string {
                    buffer.truncate(old_len);
                    return Err(VmError::MemoryLimit);
                }
                let delta = buffer.len().saturating_sub(old_len);
                let lookahead = bytes.first().copied();
                self.heap.charge_held(delta as u64);
                let file = self.file_mut(value)?;
                if looked {
                    if used > 0 {
                        file.lookahead = None;
                    }
                } else {
                    file.read_pos += used as u32;
                    file.cursor = file
                        .cursor
                        .checked_add(used as u64)
                        .filter(|n| *n <= i64::MAX as u64)
                        .ok_or(VmError::Corrupt)?;
                    if used == 0 && !eof && matches!(*format, 5 | 6) {
                        file.cursor = file
                            .cursor
                            .checked_add(1)
                            .filter(|n| *n <= i64::MAX as u64)
                            .ok_or(VmError::Corrupt)?;
                        file.lookahead = lookahead;
                        file.read_pos += 1;
                    }
                }
                file.eof = eof;
                if !done {
                    return Ok(Next::Busy);
                }
                let result = if *format == 5 {
                    if *numeral == 10 {
                        Value::Nil
                    } else {
                        crate::lex::string_to_number(buffer).unwrap_or(Value::Nil)
                    }
                } else if success {
                    Value::String(self.alloc_string(buffer.clone())?)
                } else {
                    Value::Nil
                };
                let failed = matches!(result, Value::Nil);
                self.io_slot(ctx, *got + 1, result)?;
                let n = buffer.len();
                buffer.clear();
                self.io_release(ctx.active, n as u64);
                *got += 1;
                *format = 0;
                if failed {
                    if *iterator && *got == 1 {
                        if *toclose {
                            *work = IoWork::Close {
                                quiet: true,
                                exhausted: true,
                                failed: false,
                            };
                            return Ok(Next::Busy);
                        }
                        return Ok(Next::Done(vec![]));
                    }
                    return Ok(Next::Done(
                        (1..=(*got)).map(|i| self.scratch(ctx, i)).collect(),
                    ));
                }
                Ok(Next::Busy)
            }
        }
    }
    fn pipe_result(&mut self, status: ProcessStatus) -> Result<Next, VmError> {
        let (ok, name, code) = match status {
            ProcessStatus::Exit(n) => (n == 0, b"exit".as_slice(), n),
            ProcessStatus::Signal(n) => (false, b"signal".as_slice(), n),
        };
        let name = Value::String(self.alloc_string(name.to_vec())?);
        Ok(Next::Done(vec![
            if ok { Value::Bool(true) } else { Value::Nil },
            name,
            Value::Integer(code as i64),
        ]))
    }
}
/// PUC's 200-byte prefix scanner; conversion uses the VM's numeral parser.
/// Returning false leaves the current lookahead byte unconsumed.
fn numeral_byte(
    _buffer: &[u8],
    stage: &mut u8,
    digits: &mut u32,
    hex: &mut bool,
    byte: Option<u8>,
) -> bool {
    let Some(b) = byte else {
        return false;
    };
    loop {
        match *stage {
            0 if c_space(b) => return true,
            0 => {
                *stage = 1;
            }
            1 => {
                *stage = 2;
                if matches!(b, b'-' | b'+') {
                    return true;
                }
            }
            2 => {
                *stage = 4;
                if b == b'0' {
                    *digits += 1;
                    *stage = 3;
                    return true;
                }
            }
            3 => {
                *stage = 4;
                if matches!(b, b'x' | b'X') {
                    *hex = true;
                    *digits = 0;
                    return true;
                }
            }
            4 if if *hex {
                b.is_ascii_hexdigit()
            } else {
                b.is_ascii_digit()
            } =>
            {
                *digits += 1;
                return true;
            }
            4 => {
                *stage = 5;
                if b == b'.' {
                    return true;
                } else {
                    *stage = 6;
                }
            }
            5 if if *hex {
                b.is_ascii_hexdigit()
            } else {
                b.is_ascii_digit()
            } =>
            {
                *digits += 1;
                return true;
            }
            5 => {
                *stage = 6;
            }
            6 => {
                *stage = 7;
                if *digits > 0
                    && if *hex {
                        matches!(b, b'p' | b'P')
                    } else {
                        matches!(b, b'e' | b'E')
                    }
                {
                    return true;
                }
                return false;
            }
            7 => {
                *stage = 8;
                if matches!(b, b'+' | b'-') {
                    return true;
                }
            }
            8 if b.is_ascii_digit() => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests;
