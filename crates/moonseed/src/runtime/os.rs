//! OS library: all observations/mutations use the capability journal boundary.
use super::library::{AuxWork, Ctx, Next, Op};
use super::*;
use crate::civil::{self, Civil, CivilError, Field, LocalOffset, TimeFields};
use crate::hostcaps::{
    CapabilityPoll, CapabilityRequest as Request, CapabilityValue as Answer, HostIoError,
    ProcessStatus,
};
use crate::library::{Wait, Work};
use crate::oslib::{FUNCTIONS, OsFn, OsWork};

const FIELDS: [&str; 9] = [
    "year", "month", "day", "hour", "min", "sec", "isdst", "yday", "wday",
];
// PUC's setallfields order is observable through __newindex.
const WRITE_ORDER: [usize; 9] = [0, 1, 2, 3, 4, 5, 7, 8, 6];
impl AuxWork for OsWork {
    fn next(_: &mut Runtime, _: &Ctx, _: &mut Self) -> Result<Next, VmError> {
        Err(VmError::Corrupt)
    }
    fn next_journal(
        runtime: &mut Runtime,
        ctx: &Ctx,
        work: &mut Self,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        runtime.os_next(ctx, work, journal)
    }
    fn error(runtime: &mut Runtime, ctx: &Ctx, fault: LuaFault, text: Vec<u8>) -> Poll {
        let location = runtime
            .nearest_lua_location(ctx.active)
            .filter(|(_, line)| *line > 0);
        let error = runtime.prefixed(location, text, fault);
        runtime.throw_on(ctx.active, fault, error)
    }
    fn wrap(self) -> Work {
        Work::Os(Box::new(self))
    }
    fn wrap_boxed(self: Box<Self>) -> Work {
        Work::Os(self)
    }
}
fn civil_error(error: CivilError) -> Next {
    Next::Error(LuaFault::Argument, error.message())
}
impl Runtime {
    /// Install the OS table and package.loaded entry without granting authority.
    pub fn install_os(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("os")?;
        self.register_module("os", table)?;
        for (name, symbol, _) in FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        let exit = self.native_value("os.exit")?;
        self.set_field(table, "exit", exit)
    }
    pub(super) fn call_os(
        &mut self,
        active: Handle<ThreadObj>,
        function: OsFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let mut ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let work = OsWork::new(function);
        // Capability waits must already have a resumable builtin boundary.
        self.save_lib(&ctx, Work::Os(Box::new(work.clone())), Wait::Nothing)?;
        ctx.framed = true;
        self.run_aux(ctx, work, None, journal)
    }
    fn os_bad_bytes(&self, ctx: &Ctx, index: u32, message: &[u8]) -> Next {
        let (name, method) = self.argument_name(ctx);
        Next::Error(
            LuaFault::Argument,
            super::library::arg_error_text(&name, method, index, message),
        )
    }
    fn os_text(&mut self, ctx: &Ctx, index: u32) -> Result<Result<Vec<u8>, Next>, VmError> {
        let Some(value) = self.string_arg(ctx, index)? else {
            return Ok(Err(self.bad_type(ctx, index, "string")));
        };
        let bytes = self.heap.string_bytes(value).ok_or(VmError::Corrupt)?;
        Ok(Ok(bytes
            .split(|b| *b == 0)
            .next()
            .unwrap_or_default()
            .to_vec()))
    }
    /// The common Lua fileresult authority; IO can reuse it for its failure tuples.
    pub(super) fn file_result(
        &mut self,
        error: HostIoError,
        path: Option<&[u8]>,
    ) -> Result<Next, VmError> {
        let mut message = Vec::new();
        if let Some(path) = path {
            message.extend(path);
            message.extend(b": ");
        }
        message.extend(&error.message);
        let text = self.new_string(message)?;
        Ok(Next::Done(vec![
            Value::Nil,
            text,
            Value::Integer(error.code as i64),
        ]))
    }
    fn os_fields(&self, ctx: &Ctx) -> TimeFields {
        let field = |i| {
            let value = self.scratch(ctx, i);
            if value == Value::Nil {
                Field::Missing
            } else {
                crate::base::lua_integer(&self.heap, value)
                    .map_or(Field::NonInteger, Field::Integer)
            }
        };
        let isdst = self.scratch(ctx, 6);
        TimeFields {
            year: field(0),
            month: field(1),
            day: field(2),
            hour: field(3),
            min: field(4),
            sec: field(5),
            isdst: if isdst == Value::Nil {
                None
            } else {
                Some(isdst.truthy())
            },
        }
    }
    fn os_zone(&self, ctx: &Ctx) -> Result<LocalOffset<'_>, VmError> {
        let Value::Integer(seconds) = self.scratch(ctx, 7) else {
            return Err(VmError::Corrupt);
        };
        let name = match self.scratch(ctx, 9) {
            Value::String(s) => self.heap.string_bytes(s).ok_or(VmError::Corrupt)?,
            _ => b"UTC",
        };
        Ok(LocalOffset {
            seconds: i32::try_from(seconds).map_err(|_| VmError::Corrupt)?,
            isdst: self.scratch(ctx, 8).truthy(),
            name,
        })
    }
    fn os_write_fields(&mut self, ctx: &Ctx, civil: Civil) -> Result<(), VmError> {
        let values = [
            civil.year,
            civil.month as i64,
            civil.day as i64,
            civil.hour as i64,
            civil.min as i64,
            civil.sec as i64,
            0,
            civil.yday as i64,
            civil.wday as i64,
        ];
        for (i, value) in values.into_iter().enumerate() {
            self.write_abs(
                ctx.active,
                ctx.scratch_slot(i as u32),
                if i == 6 {
                    Value::Bool(civil.isdst)
                } else {
                    Value::Integer(value)
                },
            )?;
        }
        Ok(())
    }
    fn os_next(
        &mut self,
        ctx: &Ctx,
        work: &mut OsWork,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        macro_rules! checked {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(next) => return Ok(next),
                }
            };
        }
        macro_rules! call {
            ($request:expr) => {
                match self.capability(&$request, journal)? {
                    CapabilityPoll::Waiting(_) => return Ok(Next::Busy),
                    CapabilityPoll::Ready(result) => result,
                }
            };
        }
        macro_rules! observation {
            ($request:expr) => {
                match call!($request) {
                    Ok(answer) => answer,
                    Err(error) => return Ok(Next::Error(LuaFault::Error, error.message)),
                }
            };
        }
        match work.function {
            OsFn::Difftime => {
                let a = checked!(self.int_arg(ctx, 0));
                let b = checked!(self.int_arg(ctx, 1));
                Ok(Next::Done(vec![Value::Float(civil::difftime(a, b))]))
            }
            OsFn::Setlocale => {
                let locale = if self.given(ctx, 0) {
                    Some(checked!(self.os_text(ctx, 0)?))
                } else {
                    None
                };
                let category = if self.given(ctx, 1) {
                    checked!(self.os_text(ctx, 1)?)
                } else {
                    b"all".to_vec()
                };
                if ![
                    b"all".as_slice(),
                    b"collate",
                    b"ctype",
                    b"monetary",
                    b"numeric",
                    b"time",
                ]
                .contains(&category.as_slice())
                {
                    let mut message = b"invalid option '".to_vec();
                    message.extend(&category);
                    message.push(b'\'');
                    return Ok(self.os_bad_bytes(ctx, 1, &message));
                }
                let value = if locale
                    .as_deref()
                    .is_none_or(|s| [b"".as_slice(), b"C", b"POSIX"].contains(&s))
                {
                    self.new_string(b"C".to_vec())?
                } else {
                    Value::Nil
                };
                Ok(Next::Done(vec![value]))
            }
            OsFn::Clock => {
                if self.host_capabilities.clock.is_none() {
                    return Ok(Next::Error(
                        LuaFault::Error,
                        b"time source not available".to_vec(),
                    ));
                }
                let Answer::Number(n) = observation!(Request::ClockCpuSeconds) else {
                    return Err(VmError::Corrupt);
                };
                Ok(Next::Done(vec![Value::Float(n)]))
            }
            OsFn::Getenv => {
                let name = checked!(self.os_text(ctx, 0)?);
                if self.host_capabilities.environment.is_none() {
                    return Ok(Next::Done(vec![Value::Nil]));
                }
                let Answer::OptionalBytes(bytes) = observation!(Request::EnvironmentGet { name })
                else {
                    return Err(VmError::Corrupt);
                };
                Ok(Next::Done(vec![match bytes {
                    Some(bytes) => self.new_string(bytes)?,
                    None => Value::Nil,
                }]))
            }
            OsFn::Remove | OsFn::Rename => {
                let path = checked!(self.os_text(ctx, 0)?);
                let request = if work.function == OsFn::Remove {
                    Request::FilesystemRemove { path: path.clone() }
                } else {
                    Request::FilesystemRename {
                        from: path.clone(),
                        to: checked!(self.os_text(ctx, 1)?),
                    }
                };
                let result = if self.host_capabilities.filesystem.is_none() {
                    Err(HostIoError::new(
                        crate::HostIoErrorKind::PermissionDenied,
                        b"no filesystem access".to_vec(),
                    ))
                } else {
                    call!(request)
                };
                match result {
                    Ok(Answer::Unit) => Ok(Next::Done(vec![Value::Bool(true)])),
                    Err(error) => self.file_result(
                        error,
                        if work.function == OsFn::Remove {
                            Some(&path)
                        } else {
                            None
                        },
                    ),
                    _ => Err(VmError::Corrupt),
                }
            }
            OsFn::Tmpname => {
                if self.host_capabilities.filesystem.is_none() {
                    return self.file_result(
                        HostIoError::new(
                            crate::HostIoErrorKind::PermissionDenied,
                            b"no filesystem access".to_vec(),
                        ),
                        None,
                    );
                }
                match call!(Request::FilesystemTempName) {
                    Ok(Answer::Bytes(bytes)) => Ok(Next::Done(vec![self.new_string(bytes)?])),
                    Err(_) => Ok(Next::Error(
                        LuaFault::Error,
                        b"unable to generate a unique filename".to_vec(),
                    )),
                    _ => Err(VmError::Corrupt),
                }
            }
            OsFn::Execute => {
                let cmd = if self.given(ctx, 0) {
                    Some(checked!(self.os_text(ctx, 0)?))
                } else {
                    None
                };
                if let Some(cmd) = cmd {
                    match call!(Request::ProcessExecute { cmd }) {
                        Ok(Answer::Status(status)) => {
                            let (kind, n) = match status {
                                ProcessStatus::Exit(n) => (b"exit".as_slice(), n),
                                ProcessStatus::Signal(n) => (b"signal".as_slice(), n),
                            };
                            let kind = self.new_string(kind.to_vec())?;
                            Ok(Next::Done(vec![
                                if status == ProcessStatus::Exit(0) {
                                    Value::Bool(true)
                                } else {
                                    Value::Nil
                                },
                                kind,
                                Value::Integer(n as i64),
                            ]))
                        }
                        Err(error) => self.file_result(error, None),
                        _ => Err(VmError::Corrupt),
                    }
                } else {
                    let available = if self.host_capabilities.process.is_none() {
                        false
                    } else {
                        match observation!(Request::ProcessShellAvailable) {
                            Answer::Boolean(b) => b,
                            _ => return Err(VmError::Corrupt),
                        }
                    };
                    Ok(Next::Done(vec![Value::Bool(available)]))
                }
            }
            OsFn::Time | OsFn::Date => {
                let time_table = work.function == OsFn::Time && self.given(ctx, 0);
                if time_table && !matches!(self.lib_arg(ctx, 0), Value::Table(_)) {
                    return Ok(self.bad_type(ctx, 0, "table"));
                }
                if time_table {
                    return self.os_time_table(ctx, work, journal);
                }
                // Validate format before observing a clock, then explicit time.
                if work.function == OsFn::Date
                    && work.stage == 0
                    && self.given(ctx, 0)
                    && self.string_arg(ctx, 0)?.is_none()
                {
                    return Ok(self.bad_type(ctx, 0, "string"));
                }
                if work.stage == 0 {
                    work.seconds = if work.function == OsFn::Date && self.given(ctx, 1) {
                        checked!(self.int_arg(ctx, 1))
                    } else {
                        if self.host_capabilities.clock.is_none() {
                            return Ok(Next::Error(
                                LuaFault::Error,
                                b"time source not available".to_vec(),
                            ));
                        }
                        let Answer::Integer(n) = observation!(Request::ClockNowSeconds) else {
                            return Err(VmError::Corrupt);
                        };
                        n
                    };
                    if work.function == OsFn::Time {
                        return Ok(Next::Done(vec![Value::Integer(work.seconds)]));
                    }
                    work.stage = 1;
                }
                self.os_date(ctx, work, journal)
            }
        }
    }
    fn os_time_table(
        &mut self,
        ctx: &Ctx,
        work: &mut OsWork,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        if work.stage < 7 {
            // Check the preceding field before reading the next, matching PUC's error priority.
            if work.stage > 0 {
                let i = (work.stage - 1) as u32;
                let value = self.scratch(ctx, i);
                let name = FIELDS[i as usize];
                let n = crate::base::lua_integer(&self.heap, value);
                if let Some(n) = n {
                    let delta = if i == 0 {
                        1900
                    } else if i == 1 {
                        1
                    } else {
                        0
                    };
                    if n < i32::MIN as i64 + delta || n > i32::MAX as i64 + delta {
                        return Ok(civil_error(CivilError::OutOfBound(name)));
                    }
                } else if value != Value::Nil {
                    return Ok(civil_error(CivilError::NonInteger(name)));
                } else if i < 3 {
                    return Ok(civil_error(CivilError::Missing(name)));
                }
            }
            let i = work.stage;
            work.stage += 1;
            let key = self.new_string(FIELDS[i as usize].as_bytes().to_vec())?;
            return Ok(Next::Op(Op::Get {
                obj: self.lib_arg(ctx, 0),
                key,
                into: i as u32,
            }));
        }
        if work.stage == 7 {
            let mut wall = None;
            let result = civil::normalize_time(
                self.os_fields(ctx),
                |n, _| {
                    wall = Some(n);
                    Ok(n)
                },
                |_| Ok(civil::UTC),
            );
            if let Err(error) = result
                && !matches!(
                    error,
                    CivilError::TimeRange {
                        normalized: Some(_)
                    }
                )
            {
                return Ok(civil_error(error));
            }
            work.seconds = wall.ok_or(VmError::Corrupt)?;
            work.stage = 8;
        }
        if work.stage == 8 {
            let isdst = self.os_fields(ctx).isdst;
            if self.host_capabilities.civil.is_some() {
                match self.capability(
                    &Request::CivilTimeUtcSeconds {
                        local_seconds: work.seconds,
                        isdst,
                    },
                    journal,
                )? {
                    CapabilityPoll::Waiting(_) => return Ok(Next::Busy),
                    CapabilityPoll::Ready(Ok(Answer::Integer(n))) => work.seconds = n,
                    CapabilityPoll::Ready(Err(error)) => {
                        return Ok(Next::Error(LuaFault::Error, error.message));
                    }
                    _ => return Err(VmError::Corrupt),
                }
            } else if isdst == Some(true) {
                work.seconds = work.seconds.checked_sub(3600).ok_or(VmError::Corrupt)?;
            }
            work.stage = 9;
        }
        if work.stage == 9 {
            if let Some(next) = self.os_offset(ctx, work.seconds, journal)? {
                return Ok(next);
            }
            work.stage = 10;
        }
        if work.stage == 10 {
            let civil = match civil::local_civil(work.seconds, self.os_zone(ctx)?) {
                Ok(c) => c,
                Err(_) => return Ok(civil_error(CivilError::TimeRange { normalized: None })),
            };
            self.os_write_fields(ctx, civil)?;
            work.stage = 11;
        }
        if work.stage < 20 {
            let i = WRITE_ORDER[(work.stage - 11) as usize];
            work.stage += 1;
            let key = self.new_string(FIELDS[i].as_bytes().to_vec())?;
            return Ok(Next::Op(Op::Set {
                obj: self.lib_arg(ctx, 0),
                key,
                value: self.scratch(ctx, i as u32),
            }));
        }
        if work.seconds == -1 {
            return Ok(civil_error(CivilError::TimeRange { normalized: None }));
        }
        Ok(Next::Done(vec![Value::Integer(work.seconds)]))
    }
    fn os_offset(
        &mut self,
        ctx: &Ctx,
        seconds: i64,
        journal: &mut Journal,
    ) -> Result<Option<Next>, VmError> {
        let offset = if self.host_capabilities.civil.is_some() {
            match self.capability(
                &Request::CivilTimeLocalOffset {
                    utc_seconds: seconds,
                },
                journal,
            )? {
                CapabilityPoll::Waiting(_) => return Ok(Some(Next::Busy)),
                CapabilityPoll::Ready(Ok(Answer::Offset(offset))) => offset,
                CapabilityPoll::Ready(Err(error)) => {
                    return Ok(Some(Next::Error(LuaFault::Error, error.message)));
                }
                _ => return Err(VmError::Corrupt),
            }
        } else {
            crate::CivilOffset {
                seconds: 0,
                isdst: false,
            }
        };
        self.write_abs(
            ctx.active,
            ctx.scratch_slot(7),
            Value::Integer(offset.seconds as i64),
        )?;
        self.write_abs(ctx.active, ctx.scratch_slot(8), Value::Bool(offset.isdst))?;
        Ok(None)
    }
    fn os_date(
        &mut self,
        ctx: &Ctx,
        work: &mut OsWork,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        let handle = if self.given(ctx, 0) {
            Some(self.string_arg(ctx, 0)?.ok_or(VmError::Corrupt)?)
        } else {
            None
        };
        let format = handle
            .and_then(|h| self.heap.string_bytes(h))
            .unwrap_or(b"%c");
        let utc = format.starts_with(b"!");
        let body = format.strip_prefix(b"!").unwrap_or(format);
        let table = body.starts_with(b"*t") && body.get(2).is_none_or(|b| *b == 0);
        if work.stage == 1 {
            if utc {
                self.write_abs(ctx.active, ctx.scratch_slot(7), Value::Integer(0))?;
                self.write_abs(ctx.active, ctx.scratch_slot(8), Value::Bool(false))?;
            } else if let Some(next) = self.os_offset(ctx, work.seconds, journal)? {
                return Ok(next);
            }
            work.stage = 2;
        }
        if work.stage == 2 {
            let name = if utc {
                b"GMT".to_vec()
            } else if self.host_capabilities.civil.is_none() {
                b"UTC".to_vec()
            } else {
                match self.capability(
                    &Request::CivilTimeZoneName {
                        utc_seconds: work.seconds,
                    },
                    journal,
                )? {
                    CapabilityPoll::Waiting(_) => return Ok(Next::Busy),
                    CapabilityPoll::Ready(Ok(Answer::Bytes(bytes))) => bytes,
                    CapabilityPoll::Ready(Err(error)) => {
                        return Ok(Next::Error(LuaFault::Error, error.message));
                    }
                    _ => return Err(VmError::Corrupt),
                }
            };
            let name = self.new_string(name)?;
            self.write_abs(ctx.active, ctx.scratch_slot(9), name)?;
            work.stage = 3;
        }
        if work.stage >= 4 {
            if work.stage < 13 {
                let i = (work.stage - 4) as usize;
                work.stage += 1;
                let key = self.new_string(FIELDS[i].as_bytes().to_vec())?;
                return Ok(Next::Op(Op::Set {
                    obj: self.scratch(ctx, 9),
                    key,
                    value: self.scratch(ctx, i as u32),
                }));
            }
            return Ok(Next::Done(vec![self.scratch(ctx, 9)]));
        }
        let civil = match civil::local_civil(work.seconds, self.os_zone(ctx)?) {
            Ok(civil) => civil,
            Err(error) => return Ok(civil_error(error)),
        };
        if table {
            if work.stage == 3 {
                let zone = self.os_zone(ctx)?;
                let civil = match civil::date(b"*t", work.seconds, |_| Ok(zone)) {
                    Ok(civil::DateOutput::Table(c)) => c,
                    _ => return Err(VmError::Corrupt),
                };
                self.os_write_fields(ctx, civil)?;
                let table = Value::Table(self.alloc_table()?);
                self.write_abs(ctx.active, ctx.scratch_slot(9), table)?;
                work.stage = 4;
            }
            if work.stage < 13 {
                let i = (work.stage - 4) as usize;
                work.stage += 1;
                let key = self.new_string(FIELDS[i].as_bytes().to_vec())?;
                return Ok(Next::Op(Op::Set {
                    obj: self.scratch(ctx, 9),
                    key,
                    value: self.scratch(ctx, i as u32),
                }));
            }
            return Ok(Next::Done(vec![self.scratch(ctx, 9)]));
        }
        let format = handle
            .and_then(|h| self.heap.string_bytes(h))
            .unwrap_or(b"%c");
        let format = if utc { &format[1..] } else { format };
        let start = work.pos as usize;
        if start == format.len() {
            let bytes = std::mem::take(&mut work.out);
            self.heap.give_back(bytes.len() as u64);
            self.heap
                .threads
                .get_mut(ctx.active)
                .ok_or(VmError::Corrupt)?
                .charged_held -= bytes.len() as u64;
            return Ok(Next::Done(vec![self.new_string(bytes)?]));
        }
        let end = if format[start] == b'%' {
            (start
                + if matches!(format.get(start + 1), Some(b'E' | b'O')) {
                    3
                } else {
                    2
                })
            .min(format.len())
        } else {
            (start..format.len().min(start + 32))
                .find(|i| format[*i] == b'%')
                .unwrap_or(format.len().min(start + 32))
        };
        let zone = self.os_zone(ctx)?;
        let piece = match civil::strftime(&format[start..end], &civil, zone) {
            Ok(piece) => piece,
            Err(CivilError::InvalidConversion(_)) => {
                let suffix = format[start + 1..]
                    .split(|b| *b == 0)
                    .next()
                    .unwrap_or_default();
                let error = CivilError::InvalidConversion(suffix.to_vec());
                return Ok(self.os_bad_bytes(ctx, 0, &error.message()));
            }
            Err(error) => return Ok(civil_error(error)),
        };
        if work.out.len().saturating_add(piece.len()) > self.heap.max_string {
            return Err(VmError::MemoryLimit);
        }
        self.ensure_room(piece.len() as u64)?;
        self.heap.charge_held(piece.len() as u64);
        work.out.extend(piece);
        work.pos = end as u32;
        Ok(Next::Busy)
    }
}
