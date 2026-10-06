//! Lua 5.4 UTF-8 builtins on the resumable library engine.
use super::library::{AuxWork, Ctx, Next};
use super::*;
use crate::library::Work;
use crate::utf8::{continuation, decode, relative};
use crate::utf8lib::{CHARPATTERN, FUNCTIONS, LAX_STEP, SEQUENCES, STRICT_STEP, Utf8Fn, Utf8Work};

impl AuxWork for Utf8Work {
    fn next(runtime: &mut Runtime, ctx: &Ctx, work: &mut Self) -> Result<Next, VmError> {
        runtime.utf8_next(ctx, work)
    }
    fn wrap(self) -> Work {
        Work::Utf8(Box::new(self))
    }
    fn wrap_boxed(self: Box<Self>) -> Work {
        Work::Utf8(self)
    }
}
fn invalid() -> Next {
    Next::Error(LuaFault::Argument, b"invalid UTF-8 code".to_vec())
}
impl Runtime {
    /// Install the six-field UTF-8 module in globals and package.loaded.
    pub fn install_utf8(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("utf8")?;
        self.register_module("utf8", table)?;
        for (name, symbol, _) in FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        let pattern = self.new_string(CHARPATTERN.to_vec())?;
        self.set_field(table, "charpattern", pattern)
    }
    pub(super) fn call_utf8(
        &mut self,
        active: Handle<ThreadObj>,
        function: Utf8Fn,
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
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(next) => return Ok(self.finish_next(active, next)),
                }
            };
        }
        if function == Utf8Fn::Char {
            return self.run_aux(
                ctx,
                Utf8Work::Char {
                    next: 0,
                    out: Vec::new(),
                },
                None,
                journal,
            );
        }
        let Some(subject) = self.string_arg(&ctx, 0)? else {
            return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "string")));
        };
        let len = self
            .heap
            .string_bytes(subject)
            .ok_or(VmError::Corrupt)?
            .len();
        let work = match function {
            Utf8Fn::Len | Utf8Fn::Codepoint => {
                let first = relative(check!(self.opt_int_arg(&ctx, 1, 1)), len);
                let last = relative(
                    check!(self.opt_int_arg(
                        &ctx,
                        2,
                        if function == Utf8Fn::Len { -1 } else { first }
                    )),
                    len,
                );
                let points = function == Utf8Fn::Codepoint;
                if first < 1 || (!points && first - 1 > len as i64) {
                    return Ok(self.finish_next(
                        active,
                        self.bad_arg(
                            &ctx,
                            1,
                            if points {
                                "out of bounds"
                            } else {
                                "initial position out of bounds"
                            },
                        ),
                    ));
                }
                if last > len as i64 {
                    return Ok(self.finish_next(
                        active,
                        self.bad_arg(
                            &ctx,
                            2,
                            if points {
                                "out of bounds"
                            } else {
                                "final position out of bounds"
                            },
                        ),
                    ));
                }
                if first > last {
                    return self
                        .base_return(active, if points { &[] } else { &[Value::Integer(0)] });
                }
                if points {
                    let n = (last - first + 1) as u32;
                    if !matches!(
                        self.slot_fault(active, ctx.scratch_slot(n).saturating_add(4)),
                        Ok(None)
                    ) {
                        return Ok(self.finish_next(
                            active,
                            Next::Error(
                                LuaFault::StringSlice,
                                b"stack overflow (string slice too long)".to_vec(),
                            ),
                        ));
                    }
                }
                Utf8Work::Scan {
                    pos: (first - 1) as u32,
                    end: last as u32,
                    count: 0,
                    strict: !self.lib_arg(&ctx, 3).truthy(),
                    points,
                }
            }
            Utf8Fn::Offset => {
                let n = check!(self.int_arg(&ctx, 1));
                let first = relative(
                    check!(self.opt_int_arg(&ctx, 2, if n >= 0 { 1 } else { len as i64 + 1 })),
                    len,
                );
                if first < 1 || first - 1 > len as i64 {
                    return Ok(
                        self.finish_next(active, self.bad_arg(&ctx, 2, "position out of bounds"))
                    );
                }
                let pos = (first - 1) as u32;
                if n != 0
                    && self
                        .heap
                        .string_bytes(subject)
                        .and_then(|s| s.get(pos as usize))
                        .is_some_and(|b| continuation(*b))
                {
                    return Ok(self.finish_next(
                        active,
                        Next::Error(
                            LuaFault::Argument,
                            b"initial position is a continuation byte".to_vec(),
                        ),
                    ));
                }
                Utf8Work::Offset {
                    pos,
                    remaining: if n > 0 { n - 1 } else { n },
                    direction: n.signum(),
                    seeking: false,
                }
            }
            Utf8Fn::Codes => {
                if self
                    .heap
                    .string_bytes(subject)
                    .and_then(|s| s.first())
                    .is_some_and(|b| continuation(*b))
                {
                    return Ok(
                        self.finish_next(active, self.bad_arg(&ctx, 0, "invalid UTF-8 code"))
                    );
                }
                let iterator = self.native_value(if self.lib_arg(&ctx, 1).truthy() {
                    LAX_STEP
                } else {
                    STRICT_STEP
                })?;
                return self.base_return(
                    active,
                    &[iterator, Value::String(subject), Value::Integer(0)],
                );
            }
            Utf8Fn::StrictStep | Utf8Fn::LaxStep => {
                // lua_tointeger, with no argument error and unsigned negative handling.
                let pos =
                    crate::base::lua_integer(&self.heap, self.lib_arg(&ctx, 1)).unwrap_or(0) as u64;
                if pos >= len as u64 {
                    return self.base_return(active, &[]);
                }
                Utf8Work::Iterate {
                    pos: pos as u32,
                    strict: function == Utf8Fn::StrictStep,
                }
            }
            Utf8Fn::Char => return Err(VmError::Corrupt),
        };
        self.run_aux(ctx, work, None, journal)
    }
    pub(super) fn utf8_next(&mut self, ctx: &Ctx, work: &mut Utf8Work) -> Result<Next, VmError> {
        if let Utf8Work::Char { next, out } = work {
            for _ in 0..SEQUENCES {
                if *next == ctx.passed {
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?]));
                }
                let code = match self.int_arg(ctx, *next) {
                    Ok(code) => code,
                    Err(next) => return Ok(next),
                };
                if !(0..=0x7fffffff).contains(&code) {
                    return Ok(self.bad_arg(ctx, *next, "value out of range"));
                }
                let bytes = crate::utf8::encode(code as u32);
                if out
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|total| total > self.heap.max_string)
                {
                    return Ok(Next::Fault(LuaFault::StringTooLarge));
                }
                if !self.heap.gc.fits(bytes.len() as u64) {
                    return Ok(Next::Fault(LuaFault::Memory));
                }
                self.heap.charge_held(bytes.len() as u64);
                out.extend(bytes);
                *next += 1;
            }
            if *next == ctx.passed {
                let bytes = std::mem::take(out);
                return Ok(Next::Done(vec![self.new_string(bytes)?]));
            }
            return Ok(Next::Busy);
        }
        let Value::String(subject) = self.lib_arg(ctx, 0) else {
            return Err(VmError::Corrupt);
        };
        match work {
            Utf8Work::Scan {
                pos,
                end,
                count,
                strict,
                points,
            } => {
                for _ in 0..SEQUENCES {
                    if *pos >= *end {
                        break;
                    }
                    let bytes = self.heap.string_bytes(subject).ok_or(VmError::Corrupt)?;
                    let Some((code, next)) = decode(bytes, *pos as usize, *strict) else {
                        return Ok(if *points {
                            invalid()
                        } else {
                            Next::Done(vec![Value::Nil, Value::Integer(i64::from(*pos) + 1)])
                        });
                    };
                    if *points {
                        self.write_abs(
                            ctx.active,
                            ctx.scratch_slot(*count),
                            Value::Integer(i64::from(code)),
                        )?;
                    }
                    *count += 1;
                    *pos = next as u32;
                }
                if *pos < *end {
                    return Ok(Next::Busy);
                }
                Ok(Next::Done(if *points {
                    (0..*count).map(|i| self.scratch(ctx, i)).collect()
                } else {
                    vec![Value::Integer(i64::from(*count))]
                }))
            }
            Utf8Work::Offset {
                pos,
                remaining,
                direction,
                seeking,
            } => {
                let bytes = self.heap.string_bytes(subject).ok_or(VmError::Corrupt)?;
                let len = bytes.len() as u32;
                for _ in 0..crate::strlib::BYTE_BATCH {
                    let cont = bytes.get(*pos as usize).is_some_and(|b| continuation(*b));
                    if *direction == 0 {
                        if *pos == 0 || !cont {
                            return Ok(Next::Done(vec![Value::Integer(i64::from(*pos) + 1)]));
                        }
                        *pos -= 1;
                    } else if *seeking && cont && (*direction > 0 || *pos > 0) {
                        *pos = if *direction > 0 { *pos + 1 } else { *pos - 1 };
                    } else {
                        if *seeking {
                            *remaining -= *direction;
                            *seeking = false;
                        }
                        if *remaining == 0 {
                            return Ok(Next::Done(vec![Value::Integer(i64::from(*pos) + 1)]));
                        }
                        if (*direction < 0 && *pos == 0) || (*direction > 0 && *pos == len) {
                            return Ok(Next::Done(vec![Value::Nil]));
                        }
                        *pos = if *direction > 0 { *pos + 1 } else { *pos - 1 };
                        *seeking = true;
                    }
                }
                Ok(Next::Busy)
            }
            Utf8Work::Iterate { pos, strict } => {
                let bytes = self.heap.string_bytes(subject).ok_or(VmError::Corrupt)?;
                for _ in 0..crate::strlib::BYTE_BATCH {
                    if !bytes.get(*pos as usize).is_some_and(|b| continuation(*b)) {
                        break;
                    }
                    *pos += 1;
                }
                if *pos as usize >= bytes.len() {
                    return Ok(Next::Done(Vec::new()));
                }
                if continuation(bytes[*pos as usize]) {
                    return Ok(Next::Busy);
                }
                let Some((code, next)) = decode(bytes, *pos as usize, *strict) else {
                    return Ok(invalid());
                };
                if bytes.get(next).is_some_and(|b| continuation(*b)) {
                    return Ok(invalid());
                }
                Ok(Next::Done(vec![
                    Value::Integer(i64::from(*pos) + 1),
                    Value::Integer(i64::from(code)),
                ]))
            }
            Utf8Work::Char { .. } => Err(VmError::Corrupt),
        }
    }
}
