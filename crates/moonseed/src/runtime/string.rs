//! The `string` library and the string metatable (ADR 0034, ADR 0035).
//!
//! String functions run on the table library's machine engine
//! (`runtime/library.rs`): a function that makes a long result builds it
//! `strlib::BYTE_BATCH` bytes a step, each further step costing a unit of fuel,
//! and one that calls Lua (a `gsub` replacement, `%s`'s `__tostring`, a
//! string metamethod falling back to the other operand's) makes the call
//! from its `Boundary::Builtin` frame. As in Lua 5.4.9, no coroutine may
//! yield across these calls.
//!
//! An argument Lua reads with `luaL_checklstring` may be a number; it is
//! converted to a string in its stack slot, as `lua_tolstring` does, so
//! later steps read the same string.

use super::library::{Ctx, Next, Op};
use super::*;
use crate::strlib::{
    Build, ENGINE_BUDGET, STRING_ARITH, STRING_FUNCTIONS, Seek, StrArith, StrFn, StrWork,
    build_part, end_position, start_position,
};

impl Runtime {
    /// Install `string` as a global table of its functions, and the string
    /// metatable, shared by every string, with `__index` the `string`
    /// table and Lua 5.4's arithmetic metamethods (ADR 0034). The registry
    /// must have them (see [`crate::register_string`]).
    pub fn install_string(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("string")?;
        self.register_module("string", table)?;
        for (name, symbol, _) in STRING_FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        let metatable = self.alloc_table()?;
        // A root before it is filled.
        self.heap.type_metatables
            [crate::heap::basic_type(Value::String(crate::id::Handle::new(0, 0)))] =
            Some(metatable);
        for (event, symbol, _) in STRING_ARITH {
            let value = self.native_value(symbol)?;
            self.set_field(Value::Table(metatable), event, value)?;
        }
        self.set_field(Value::Table(metatable), "__index", table)
    }

    /// A `string` function or string metamethod, called from the active
    /// frame's call site.
    pub(super) fn call_string(
        &mut self,
        active: Handle<ThreadObj>,
        function: StrFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        match self.start_string(&ctx, function)? {
            Started::Done(values) => self.base_return(active, &values),
            Started::Fault(fault) => Ok(self.fault(fault)),
            Started::Error(fault, text) => Ok(self.library_error(active, fault, text)),
            Started::Work(work) => {
                // Only a suspended machine needs Work's box. The first step
                // already has the string machine by value.
                count!("builtin_library_steps");
                self.run_aux(ctx, work, None, journal)
            }
        }
    }

    /// The string argument `index`, as `luaL_checklstring` reads it: a
    /// string, or a number converted to one in its slot. `None` for
    /// anything else.
    pub(super) fn string_arg(
        &mut self,
        ctx: &Ctx,
        index: u32,
    ) -> Result<Option<Handle<crate::heap::StringObj>>, VmError> {
        let value = self.lib_arg(ctx, index);
        match value {
            Value::String(handle) => Ok(Some(handle)),
            Value::Integer(_) | Value::Float(_) => {
                let text = crate::concat::number_text(value).unwrap_or_default();
                let handle = self.alloc_string(text.into_bytes())?;
                self.write_abs(ctx.active, ctx.func + 1 + index, Value::String(handle))?;
                Ok(Some(handle))
            }
            _ => Ok(None),
        }
    }

    fn bytes_of(&self, handle: Handle<crate::heap::StringObj>) -> &[u8] {
        self.heap.string_bytes(handle).unwrap_or_default()
    }

    /// Lua's name for an argument's type in an error: its metatable's
    /// `__name` when that is a string, else its type, or `no value`.
    pub(super) fn arg_type_name(&self, ctx: &Ctx, index: u32) -> Vec<u8> {
        if index >= ctx.passed {
            return b"no value".to_vec();
        }
        let value = self.lib_arg(ctx, index);
        if let Some(Value::String(name)) = index::metamethod(&self.heap, value, b"__name") {
            return self.bytes_of(name).to_vec();
        }
        if let Value::LightUserdata(..) = value {
            return b"light userdata".to_vec();
        }
        crate::heap::RESERVED_NAMES[type_name_index(value)]
            .as_bytes()
            .to_vec()
    }

    /// `luaL_argerror`'s message for argument `index` of `function`.
    pub(super) fn arg_error(&self, ctx: &Ctx, index: u32, message: &[u8]) -> (LuaFault, Vec<u8>) {
        let (name, method) = self.argument_name(ctx);
        (
            LuaFault::Argument,
            super::library::arg_error_text(&name, method, index, message),
        )
    }

    /// `luaL_typeerror`: argument `index` is not a `expected`.
    pub(super) fn type_error(
        &self,
        ctx: &Ctx,
        _function: StrFn,
        index: u32,
        expected: &str,
    ) -> (LuaFault, Vec<u8>) {
        let mut message = format!("{expected} expected, got ").into_bytes();
        message.extend(self.arg_type_name(ctx, index));
        self.arg_error(ctx, index, &message)
    }

    /// `luaL_checkinteger` of argument `index`, with Lua's error.
    pub(super) fn check_int(&self, ctx: &Ctx, function: StrFn, index: u32) -> Checked<i64> {
        let value = self.lib_arg(ctx, index);
        if let Some(integer) = crate::base::lua_integer(&self.heap, value) {
            return Ok(integer);
        }
        if super::library::number(&self.heap, value).is_some() {
            return Err(self.arg_error(ctx, index, b"number has no integer representation"));
        }
        Err(self.type_error(ctx, function, index, "number"))
    }

    /// `luaL_optinteger`.
    pub(super) fn opt_int(
        &self,
        ctx: &Ctx,
        function: StrFn,
        index: u32,
        default: i64,
    ) -> Checked<i64> {
        if self.given(ctx, index) {
            self.check_int(ctx, function, index)
        } else {
            Ok(default)
        }
    }

    /// `luaL_checklstring` of argument `index`, with Lua's error.
    pub(super) fn check_string(
        &mut self,
        ctx: &Ctx,
        function: StrFn,
        index: u32,
    ) -> Result<Checked<Handle<crate::heap::StringObj>>, VmError> {
        Ok(match self.string_arg(ctx, index)? {
            Some(handle) => Ok(handle),
            None => Err(self.type_error(ctx, function, index, "string")),
        })
    }

    /// Check a function's arguments and do it, or start its machine.
    fn start_string(&mut self, ctx: &Ctx, function: StrFn) -> Result<Started, VmError> {
        use Started::Done;
        macro_rules! check {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err((fault, text)) => return Ok(Started::Error(fault, text)),
                }
            };
        }
        macro_rules! string {
            ($index:expr) => {
                check!(self.check_string(ctx, function, $index)?)
            };
        }
        macro_rules! int {
            ($index:expr) => {
                check!(self.check_int(ctx, function, $index))
            };
            ($index:expr, $default:expr) => {
                check!(self.opt_int(ctx, function, $index, $default))
            };
        }
        Ok(match function {
            StrFn::Len => {
                let subject = string!(0);
                Done(vec![Value::Integer(self.bytes_of(subject).len() as i64)])
            }
            StrFn::Sub => {
                let subject = string!(0);
                let len = self.bytes_of(subject).len();
                let start = start_position(int!(1), len);
                let end = end_position(int!(2, -1), len);
                if start > end {
                    return self.new_string(Vec::new()).map(|empty| Done(vec![empty]));
                }
                let total = (end - start + 1) as u32;
                self.start_build(
                    Build::Sub {
                        start: (start - 1) as u32,
                    },
                    total,
                )?
            }
            StrFn::Reverse | StrFn::Lower | StrFn::Upper => {
                let subject = string!(0);
                let total = self.bytes_of(subject).len() as u32;
                let build = match function {
                    StrFn::Reverse => Build::Reverse,
                    StrFn::Lower => Build::Lower,
                    _ => Build::Upper,
                };
                self.start_build(build, total)?
            }
            StrFn::Rep => {
                let subject = string!(0);
                let n = int!(1);
                let sep = if self.given(ctx, 2) {
                    Some(string!(2))
                } else {
                    None
                };
                let len = self.bytes_of(subject).len() as u64;
                let sep_len = sep.map_or(0, |sep| self.bytes_of(sep).len() as u64);
                if n <= 0 {
                    return self.new_string(Vec::new()).map(|empty| Done(vec![empty]));
                }
                // n copies and n - 1 separators, checked before anything
                // is made: past the string limit, Lua's error for a result
                // past its own size limit.
                let total = (len + sep_len)
                    .checked_mul(n as u64)
                    .map(|all| all - sep_len)
                    .filter(|total| *total <= self.heap.max_string as u64);
                let Some(total) = total else {
                    return Ok(Started::Error(
                        LuaFault::StringTooLarge,
                        b"resulting string too large".to_vec(),
                    ));
                };
                self.start_build(
                    Build::Rep {
                        len: len as u32,
                        sep: sep_len as u32,
                    },
                    total as u32,
                )?
            }
            StrFn::Byte => {
                let subject = string!(0);
                let len = self.bytes_of(subject).len();
                let first = int!(1, 1);
                let last = end_position(int!(2, first), len);
                let first = start_position(first, len);
                if first > last {
                    return Ok(Done(Vec::new()));
                }
                let n = last - first + 1;
                // Room for every result before any is made.
                let end = ctx
                    .scratch_slot(u32::try_from(n).unwrap_or(u32::MAX))
                    .saturating_add(4);
                if !matches!(self.slot_fault(ctx.active, end), Ok(None)) {
                    // `luaL_checkstack`'s wording.
                    return Ok(Started::Error(
                        LuaFault::StringSlice,
                        b"stack overflow (string slice too long)".to_vec(),
                    ));
                }
                let bytes = self.bytes_of(subject);
                Done(
                    bytes[(first - 1) as usize..last as usize]
                        .iter()
                        .map(|byte| Value::Integer(i64::from(*byte)))
                        .collect(),
                )
            }
            StrFn::Char => {
                let mut bytes = Vec::with_capacity(ctx.passed as usize);
                for index in 0..ctx.passed {
                    let code = int!(index);
                    let Ok(byte) = u8::try_from(code) else {
                        let (fault, text) = self.arg_error(ctx, index, b"value out of range");
                        return Ok(Started::Error(fault, text));
                    };
                    bytes.push(byte);
                }
                Done(vec![self.new_string(bytes)?])
            }
            StrFn::Arith(op) => self.string_arith(ctx, op)?,
            StrFn::Format => {
                string!(0);
                Started::Work(StrWork::Format {
                    formatter: Box::new(crate::strformat::Formatter::new(
                        ctx.passed.saturating_sub(1),
                    )),
                    out: Vec::new(),
                    waiting: false,
                    debt: 0,
                })
            }
            StrFn::Pack => {
                string!(0);
                Started::Work(StrWork::Pack {
                    packer: Box::new(crate::strpack::Packer::new()),
                    out: Vec::new(),
                    debt: 0,
                })
            }
            StrFn::PackSize => {
                string!(0);
                Started::Work(StrWork::PackSize {
                    counter: Box::new(crate::strpack::SizeCounter::new()),
                    debt: 0,
                })
            }
            StrFn::Unpack => {
                string!(0);
                let data = string!(1);
                let len = self.bytes_of(data).len();
                let pos = start_position(int!(2, 1), len) - 1;
                if pos > len as u64 {
                    let (fault, text) = self.arg_error(ctx, 2, b"initial position out of string");
                    return Ok(Started::Error(fault, text));
                }
                Started::Work(StrWork::Unpack {
                    unpacker: Box::new(crate::strpack::Unpacker::new(pos as u32)),
                    count: 0,
                    debt: 0,
                })
            }
            StrFn::Dump => {
                let function = self.lib_arg(ctx, 0);
                if !function.is_function() {
                    let (fault, text) = self.type_error(ctx, StrFn::Dump, 0, "function");
                    return Ok(Started::Error(fault, text));
                }
                let strip = self.lib_arg(ctx, 1).truthy();
                let spec = match function {
                    Value::Closure(closure) => self.closure_spec(closure)?,
                    _ => None,
                };
                let Some(spec) = spec else {
                    return Ok(Started::Error(
                        LuaFault::DumpFunction,
                        b"unable to dump given function".to_vec(),
                    ));
                };
                match crate::chunk::dump(&spec, strip, self.string_room(0)) {
                    Some(bytes) => Done(vec![self.new_string(bytes)?]),
                    None => Started::Fault(LuaFault::Memory),
                }
            }
            StrFn::Find | StrFn::Match => {
                let subject = string!(0);
                let pattern = string!(1);
                let len = self.bytes_of(subject).len();
                let init = start_position(int!(2, 1), len) - 1;
                // Starting past the end finds nothing.
                if init > len as u64 {
                    return Ok(Done(vec![Value::Nil]));
                }
                let find = function == StrFn::Find;
                let plain = find
                    && (self.lib_arg(ctx, 3).truthy()
                        || crate::strpat::no_specials(self.bytes_of(pattern)));
                let seek = if plain {
                    Seek::Plain(Box::new(crate::strpat::PlainSearch::new(init as u32)))
                } else {
                    Seek::Pattern(Box::new(crate::strpat::Search::new(
                        self.bytes_of(pattern),
                        init as u32,
                    )))
                };
                Started::Work(StrWork::Find {
                    find,
                    seek,
                    debt: 0,
                })
            }
            StrFn::Gmatch => {
                let subject = string!(0);
                let pattern = string!(1);
                let len = self.bytes_of(subject).len() as u64;
                // A start past the end is one past it, as in Lua.
                let init = (start_position(int!(2, 1), len as usize) - 1).min(len + 1);
                let Value::Native(native) = self.native_value(crate::strlib::GMATCH_STEP)? else {
                    return Err(VmError::Corrupt);
                };
                // Lua's iterator keeps a third upvalue, the userdata
                // holding its match state; Moonseed's state is in the
                // closure's numbers, so its userdata holds nothing.
                self.make_room(2, crate::heap::userdata_cost(0, 0));
                let state = Value::Userdata(
                    self.heap
                        .alloc_userdata(self.max_objects, 0, 0, || {
                            crate::userdata::Payload::Bytes(Box::default())
                        })
                        .map_err(VmError::from)?,
                );
                let closure = self.alloc_native_closure(
                    native,
                    vec![Value::String(subject), Value::String(pattern), state],
                    gmatch_state(&crate::strpat::Gmatch::new(init as u32)).to_vec(),
                )?;
                Done(vec![Value::NativeClosure(closure)])
            }
            StrFn::GmatchStep => {
                let Some(closure) = self.called_closure(ctx) else {
                    return Ok(Started::Fault(LuaFault::Argument));
                };
                let state = &self
                    .heap
                    .native_closures
                    .get(closure)
                    .ok_or(VmError::Corrupt)?
                    .state;
                let gmatch = gmatch_resume(state).ok_or(VmError::Corrupt)?;
                Started::Work(StrWork::GmatchStep {
                    gmatch: Box::new(gmatch),
                    debt: 0,
                })
            }
            StrFn::Gsub => {
                let subject = string!(0);
                let pattern = string!(1);
                let len = self.bytes_of(subject).len() as i64;
                let max = int!(3, len + 1);
                match self.lib_arg(ctx, 2) {
                    // A number replacement is its string (`lua_tolstring`).
                    Value::Integer(_) | Value::Float(_) => {
                        string!(2);
                    }
                    Value::String(_) | Value::Table(_) => {}
                    value if value.is_function() => {}
                    _ => {
                        let (fault, text) =
                            self.type_error(ctx, function, 2, "string/function/table");
                        return Ok(Started::Error(fault, text));
                    }
                }
                let engine = crate::strpat::Gsub::new(self.bytes_of(pattern), max);
                Started::Work(StrWork::Gsub {
                    engine: Box::new(engine),
                    out: Vec::new(),
                    changed: false,
                    site: None,
                    debt: 0,
                })
            }
        })
    }

    /// Start a built result of `total` bytes: refused at once when it
    /// cannot fit the string limit or the quota; made at once when it is
    /// short.
    fn start_build(&mut self, build: Build, total: u32) -> Result<Started, VmError> {
        if total as usize > self.heap.max_string || !self.heap.gc.fits(u64::from(total)) {
            return Ok(Started::Fault(LuaFault::Memory));
        }
        Ok(Started::Work(StrWork::Build {
            build,
            total,
            out: Vec::with_capacity(total as usize),
        }))
    }

    /// A string metamethod for arithmetic: both operands convert to
    /// numbers, as `lstrlib.c`'s `tonum` does, else the second operand's
    /// own metamethod is called, unless it is a string (`trymt`).
    fn string_arith(&mut self, ctx: &Ctx, op: StrArith) -> Result<Started, VmError> {
        let (a, b) = (self.lib_arg(ctx, 0), self.lib_arg(ctx, 1));
        let x = self.string_number(a);
        // Called with one argument, C's `tonum(L, 2)` reads the number
        // `tonum(L, 1)` just pushed: `getmetatable("").__unm("5")` is -5.
        let y = if ctx.passed < 2 && x.is_some() {
            x
        } else {
            self.string_number(b)
        };
        if let (Some(x), Some(y)) = (x, y) {
            let result = match op {
                StrArith::Unm => crate::arith::negate(x),
                _ => match crate::arith::binary(arith_op(op), x, y) {
                    Ok(result) => result,
                    Err(fault) => return Ok(Started::Fault(fault)),
                },
            };
            return Ok(match result {
                crate::arith::Prim::Value(value) => Started::Done(vec![value]),
                crate::arith::Prim::Meta(fault) => Started::Fault(fault),
            });
        }
        let event = STRING_ARITH
            .iter()
            .find(|(_, _, which)| *which == op)
            .map_or("", |(event, _, _)| event);
        let method = match b {
            Value::String(_) => None,
            _ => index::metamethod(&self.heap, b, event.as_bytes()),
        };
        Ok(match method {
            // `trymt`'s message.
            None => {
                let name = |value| crate::heap::RESERVED_NAMES[type_name_index(value)].to_string();
                let text = format!(
                    "attempt to {} a '{}' with a '{}'",
                    &event[2..],
                    name(a),
                    name(b)
                );
                Started::Error(LuaFault::Arith, text.into_bytes())
            }
            Some(_) => Started::Work(StrWork::Arith { op, called: false }),
        })
    }

    /// A number operand, or a string that reads as one in full.
    fn string_number(&self, value: Value) -> Option<Value> {
        match value {
            Value::Integer(_) | Value::Float(_) => Some(value),
            Value::String(handle) => crate::lex::string_to_number(self.heap.string_bytes(handle)?),
            _ => None,
        }
    }

    /// A string machine's next part.
    pub(super) fn str_next(&mut self, ctx: &Ctx, work: &mut StrWork) -> Result<Next, VmError> {
        match work {
            StrWork::Build { build, total, out } => {
                let build = *build;
                if out.len() >= *total as usize {
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?]));
                }
                let subject = match self.lib_arg(ctx, 0) {
                    Value::String(handle) => handle,
                    _ => return Err(VmError::Corrupt),
                };
                let sep = match (build, self.lib_arg(ctx, 2)) {
                    (Build::Rep { .. }, Value::String(handle)) => Some(handle),
                    _ => None,
                };
                let before = out.len();
                let subject = self.heap.string_bytes(subject).unwrap_or_default();
                let sep = sep
                    .and_then(|sep| self.heap.string_bytes(sep))
                    .unwrap_or_default();
                build_part(build, *total, subject, sep, out);
                self.heap.charge_held((out.len() - before) as u64);
                if out.len() >= *total as usize {
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?]));
                }
                Ok(Next::Busy)
            }
            // The second operand's metamethod, found again in the step that
            // checked for it; its result is in scratch 0.
            StrWork::Arith { op, called } => {
                if *called {
                    return Ok(Next::Done(vec![self.scratch(ctx, 0)]));
                }
                let (a, b) = (self.lib_arg(ctx, 0), self.lib_arg(ctx, 1));
                let event = STRING_ARITH
                    .iter()
                    .find(|(_, _, which)| which == op)
                    .map_or("", |(event, _, _)| event);
                let Some(f) = index::metamethod(&self.heap, b, event.as_bytes()) else {
                    return Err(VmError::Corrupt);
                };
                *called = true;
                self.lib_call_args = vec![a, b];
                Ok(Next::Op(Op::Call { f, into: 0 }))
            }
            StrWork::Format {
                formatter,
                out,
                waiting,
                debt,
            } => self.format_next(ctx, formatter, out, waiting, debt),
            StrWork::Pack { packer, out, debt } => self.pack_next(ctx, packer, out, debt),
            StrWork::PackSize { counter, debt } => {
                let mut budget = ENGINE_BUDGET + *debt;
                if budget <= 0 {
                    *debt = budget;
                    return Ok(Next::Busy);
                }
                let fmt = self.string_at(ctx, 0)?;
                let result = counter.run(self.bytes_of(fmt), &mut budget);
                *debt = budget.min(0);
                Ok(match result {
                    Ok(None) => Next::Busy,
                    Ok(Some(size)) => Next::Done(vec![Value::Integer(size as i64)]),
                    Err(error) => pack_error(self, ctx, StrFn::PackSize, error)?,
                })
            }
            StrWork::Unpack {
                unpacker,
                count,
                debt,
            } => self.unpack_next(ctx, unpacker, count, debt),
            StrWork::Find { find, seek, debt } => self.find_next(ctx, *find, seek, debt),
            StrWork::GmatchStep { gmatch, debt } => self.gmatch_next(ctx, gmatch, debt),
            StrWork::Gsub {
                engine,
                out,
                changed,
                site,
                debt,
            } => self.gsub_next(ctx, engine, out, changed, site, debt),
        }
    }

    /// The native closure a call is running (ADR 0035), from its call slot.
    fn called_closure(&self, ctx: &Ctx) -> Option<Handle<crate::heap::NativeClosureObj>> {
        match self
            .heap
            .threads
            .get(ctx.active)?
            .stack
            .get(ctx.func as usize)
        {
            Some(Value::NativeClosure(handle)) => Some(*handle),
            _ => None,
        }
    }

    pub(super) fn alloc_native_closure(
        &mut self,
        native: u32,
        values: Vec<Value>,
        state: Vec<i64>,
    ) -> Result<Handle<crate::heap::NativeClosureObj>, VmError> {
        let bytes = crate::heap::native_closure_cost(values.len(), state.len());
        self.ensure_room(bytes)?;
        let id = self.heap.alloc_id().map_err(VmError::from)?;
        self.heap.gc.charge(bytes);
        self.heap
            .native_closures
            .alloc(crate::heap::NativeClosureObj {
                id,
                native,
                values,
                state,
            })
            .map_err(VmError::from)
    }

    /// The results of a match of `[start, end)`: its captures, or the whole
    /// match when it has none and `whole` is set (`push_captures`).
    fn captures(
        &mut self,
        matcher: &crate::strpat::Matcher,
        subject: Handle<crate::heap::StringObj>,
        start: u32,
        end: u32,
        whole: bool,
    ) -> Result<Result<Vec<Value>, Next>, VmError> {
        let mut values = Vec::with_capacity(matcher.result_count(whole));
        match self.append_captures(matcher, subject, start, end, whole, &mut values)? {
            Ok(()) => Ok(Ok(values)),
            Err(next) => Ok(Err(next)),
        }
    }

    /// Append directly to the result window's buffer, including any find
    /// positions already there, without an intermediate capture vector.
    fn append_captures(
        &mut self,
        matcher: &crate::strpat::Matcher,
        subject: Handle<crate::heap::StringObj>,
        start: u32,
        end: u32,
        whole: bool,
        values: &mut Vec<Value>,
    ) -> Result<Result<(), Next>, VmError> {
        for index in 0..matcher.result_count(whole) {
            match self.capture(matcher, subject, index, start, end)? {
                Ok(value) => values.push(value),
                Err(next) => return Ok(Err(next)),
            }
        }
        Ok(Ok(()))
    }

    /// One capture as a value (`get_onecapture`).
    fn capture(
        &mut self,
        matcher: &crate::strpat::Matcher,
        subject: Handle<crate::heap::StringObj>,
        index: usize,
        start: u32,
        end: u32,
    ) -> Result<Result<Value, Next>, VmError> {
        use crate::strpat::CaptureValue;
        Ok(Ok(match matcher.capture_value(index, start, end) {
            Ok(CaptureValue::Position(position)) => Value::Integer(position),
            Ok(CaptureValue::Bytes { start, end }) => {
                let bytes = self
                    .bytes_of(subject)
                    .get(start as usize..end as usize)
                    .ok_or(VmError::Corrupt)?
                    .to_vec();
                self.new_string(bytes)?
            }
            Err(error) => return Ok(Err(pattern_error(error))),
        }))
    }

    /// `string.find` and `string.match`'s search.
    fn find_next(
        &mut self,
        ctx: &Ctx,
        find: bool,
        seek: &mut Seek,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strpat::SearchOutcome;
        let (subject, pattern) = (self.string_at(ctx, 0)?, self.string_at(ctx, 1)?);
        let mut budget = ENGINE_BUDGET + *debt;
        if budget <= 0 {
            *debt = budget;
            return Ok(Next::Busy);
        }
        let (subject_bytes, pattern_bytes) = (
            self.heap.string_bytes(subject).unwrap_or_default(),
            self.heap.string_bytes(pattern).unwrap_or_default(),
        );
        let outcome = match seek {
            Seek::Plain(search) => search.run(subject_bytes, pattern_bytes, &mut budget),
            Seek::Pattern(search) => search.run(subject_bytes, pattern_bytes, &mut budget),
        };
        *debt = budget.min(0);
        let (start, end) = match outcome {
            SearchOutcome::Pending => return Ok(Next::Busy),
            SearchOutcome::NotFound => return Ok(Next::Done(vec![Value::Nil])),
            SearchOutcome::Error(error) => return Ok(pattern_error(error)),
            SearchOutcome::Found { start, end } => (start, end),
        };
        let captures = match seek {
            Seek::Pattern(search) => search.matcher().result_count(!find),
            Seek::Plain(_) => 0,
        };
        let mut values = Vec::with_capacity(usize::from(find) * 2 + captures);
        if find {
            values.push(Value::Integer(i64::from(start) + 1));
            values.push(Value::Integer(i64::from(end)));
        }
        if let Seek::Pattern(search) = seek
            && let Err(next) =
                self.append_captures(search.matcher(), subject, start, end, !find, &mut values)?
        {
            return Ok(next);
        }
        Ok(Next::Done(values))
    }

    /// A call of `gmatch`'s iterator: the next match's captures, or no
    /// values once there is none. The closure keeps where the next call
    /// starts; a call that fails leaves it as it was, as in Lua.
    fn gmatch_next(
        &mut self,
        ctx: &Ctx,
        gmatch: &mut crate::strpat::Gmatch,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strpat::SearchOutcome;
        let closure = self.called_closure(ctx).ok_or(VmError::Corrupt)?;
        let (subject, pattern) = match self
            .heap
            .native_closures
            .get(closure)
            .map(|closure| closure.values.as_slice())
        {
            Some([Value::String(subject), Value::String(pattern), _]) => (*subject, *pattern),
            _ => return Err(VmError::Corrupt),
        };
        let mut budget = ENGINE_BUDGET + *debt;
        if budget <= 0 {
            *debt = budget;
            return Ok(Next::Busy);
        }
        let outcome = gmatch.run(
            self.heap.string_bytes(subject).unwrap_or_default(),
            self.heap.string_bytes(pattern).unwrap_or_default(),
            &mut budget,
        );
        *debt = budget.min(0);
        let values = match outcome {
            SearchOutcome::Pending => return Ok(Next::Busy),
            SearchOutcome::Error(error) => return Ok(pattern_error(error)),
            SearchOutcome::NotFound => Vec::new(),
            SearchOutcome::Found { start, end } => {
                match self.captures(gmatch.matcher(), subject, start, end, true)? {
                    Ok(values) => values,
                    Err(next) => return Ok(next),
                }
            }
        };
        self.heap
            .native_closures
            .get_mut(closure)
            .ok_or(VmError::Corrupt)?
            .state
            .copy_from_slice(&gmatch_state(gmatch));
        Ok(Next::Done(values))
    }

    /// `string.gsub`'s next part: copy unmatched text, and replace each
    /// match by a string's expansion, or by what a table or function gives
    /// for it (`add_value`).
    fn gsub_next(
        &mut self,
        ctx: &Ctx,
        engine: &mut crate::strpat::Gsub,
        out: &mut Vec<u8>,
        changed: &mut bool,
        site: &mut Option<(u32, u32)>,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strpat::SearchOutcome;
        let (subject, pattern) = (self.string_at(ctx, 0)?, self.string_at(ctx, 1)?);
        let replacement = self.lib_arg(ctx, 2);
        let mut budget = ENGINE_BUDGET + *debt;
        // A table or function gave the replacement of `site`, in scratch 0:
        // nil or false keeps the match.
        if let Some((start, end)) = site.take() {
            let value = self.scratch(ctx, 0);
            let before = out.len();
            if !value.truthy() {
                out.extend_from_slice(
                    self.bytes_of(subject)
                        .get(start as usize..end as usize)
                        .ok_or(VmError::Corrupt)?,
                );
            } else if let Some(text) = super::builtins::text_arg(&self.heap, value) {
                out.extend_from_slice(&text);
                *changed = true;
            } else {
                let mut text = b"invalid replacement value (a ".to_vec();
                text.extend(crate::heap::RESERVED_NAMES[type_name_index(value)].as_bytes());
                text.push(b')');
                return Ok(Next::Error(LuaFault::ReplacementValue, text));
            }
            if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                return Ok(Next::Fault(fault));
            }
        }
        loop {
            if budget <= 0 {
                *debt = budget;
                return Ok(Next::Busy);
            }
            let outcome = engine.run(
                self.heap.string_bytes(subject).unwrap_or_default(),
                self.heap.string_bytes(pattern).unwrap_or_default(),
                &mut budget,
            );
            let before = out.len();
            let (start, end) = match outcome {
                SearchOutcome::Pending => continue,
                SearchOutcome::Error(error) => return Ok(pattern_error(error)),
                SearchOutcome::NotFound => {
                    *debt = budget.min(0);
                    let count = Value::Integer(engine.count());
                    // Nothing replaced: the subject itself, as in Lua.
                    if !*changed {
                        return Ok(Next::Done(vec![Value::String(subject), count]));
                    }
                    out.extend_from_slice(
                        self.bytes_of(subject)
                            .get(engine.unmatched_from() as usize..)
                            .unwrap_or_default(),
                    );
                    if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                        return Ok(Next::Fault(fault));
                    }
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?, count]));
                }
                SearchOutcome::Found { start, end } => (start, end),
            };
            out.extend_from_slice(
                self.bytes_of(subject)
                    .get(engine.unmatched_from() as usize..start as usize)
                    .ok_or(VmError::Corrupt)?,
            );
            let op = match replacement {
                Value::String(repl) => {
                    // The expansion stops before it passes the string
                    // limit or the quota, not after.
                    let limit = self.string_room(out.len());
                    let expanded = crate::strpat::expand_replacement(
                        self.heap.string_bytes(repl).unwrap_or_default(),
                        self.heap.string_bytes(subject).unwrap_or_default(),
                        engine.matcher(),
                        start,
                        end,
                        out,
                        limit,
                    );
                    match expanded {
                        Ok(true) => {}
                        Ok(false) => return Ok(Next::Fault(LuaFault::Memory)),
                        Err(error) => return Ok(pattern_error(error)),
                    }
                    *changed = true;
                    None
                }
                Value::Table(_) => match self.capture(engine.matcher(), subject, 0, start, end)? {
                    Ok(key) => Some(Op::Get {
                        obj: replacement,
                        key,
                        into: 0,
                    }),
                    Err(next) => return Ok(next),
                },
                function if function.is_function() => {
                    let mut args = std::mem::take(&mut self.lib_call_args);
                    args.clear();
                    match self.append_captures(
                        engine.matcher(),
                        subject,
                        start,
                        end,
                        true,
                        &mut args,
                    )? {
                        Ok(()) => {
                            self.lib_call_args = args;
                            Some(Op::Call {
                                f: function,
                                into: 0,
                            })
                        }
                        Err(next) => {
                            args.clear();
                            self.lib_call_args = args;
                            return Ok(next);
                        }
                    }
                }
                // Only a changed snapshot gets here: the call checked it.
                _ => return Ok(Next::Fault(LuaFault::Argument)),
            };
            if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                return Ok(Next::Fault(fault));
            }
            if let Some(op) = op {
                *site = Some((start, end));
                *debt = budget.min(0);
                return Ok(Next::Op(op));
            }
        }
    }

    /// A Lua function's prototype tree as compiled code (ADR 0036), or
    /// `None` when it nests deeper than the compiler allows, which only a
    /// hand-built program could. Built without recursion.
    fn closure_spec(
        &self,
        closure: Handle<crate::heap::ClosureObj>,
    ) -> Result<Option<crate::program::ProtoSpec>, VmError> {
        use crate::program::ProtoSpec;
        let root = self
            .heap
            .closures
            .get(closure)
            .ok_or(VmError::Corrupt)?
            .proto;
        let spec_of = |handle| -> Result<(ProtoSpec, Vec<Handle<crate::heap::Proto>>), VmError> {
            let proto = self.heap.protos.get(handle).ok_or(VmError::Corrupt)?;
            Ok((
                ProtoSpec {
                    ops: proto.ops.clone(),
                    byte_consts: proto.byte_consts.clone(),
                    captures: proto.captures.clone(),
                    children: Vec::new(),
                    max_reg: proto.max_reg,
                    params: proto.params,
                    vararg: proto.vararg,
                    debug: proto.debug.clone(),
                },
                proto.children.clone(),
            ))
        };
        // Each entry: a prototype being built and the children it still
        // has to take, in order.
        let (mut spec, children) = spec_of(root)?;
        // The function's chunk name goes with it, as Lua's `dump` writes
        // the root's source.
        let source = self.heap.protos.get(root).ok_or(VmError::Corrupt)?.source;
        if let (Some(debug), Some(source)) = (spec.debug.as_mut(), source) {
            debug.source = Some(
                self.heap
                    .strings
                    .get(source)
                    .ok_or(VmError::Corrupt)?
                    .bytes
                    .to_vec(),
            );
        }
        let mut stack = vec![(spec, children.into_iter())];
        loop {
            let (_, children) = stack.last_mut().ok_or(VmError::Corrupt)?;
            if let Some(child) = children.next() {
                if stack.len() >= crate::limits::MAX_FUNC_NEST as usize {
                    return Ok(None);
                }
                let (spec, children) = spec_of(child)?;
                stack.push((spec, children.into_iter()));
                continue;
            }
            let (done, _) = stack.pop().ok_or(VmError::Corrupt)?;
            match stack.last_mut() {
                Some((parent, _)) => parent.children.push(done),
                None => return Ok(Some(done)),
            }
        }
    }

    /// The string an argument holds once the function has started: checked
    /// and converted then, so anything else is a corrupt state.
    fn string_at(&self, ctx: &Ctx, index: u32) -> Result<Handle<crate::heap::StringObj>, VmError> {
        match self.lib_arg(ctx, index) {
            Value::String(handle) => Ok(handle),
            _ => Err(VmError::Corrupt),
        }
    }

    /// The longest a buffer of `held` bytes, already charged, may grow
    /// to: the string limit, and what the quota leaves.
    pub(crate) fn string_room(&self, held: usize) -> usize {
        let room = usize::try_from(self.heap.gc.headroom()).unwrap_or(usize::MAX);
        self.heap.max_string.min(held.saturating_add(room))
    }

    /// Account for a held buffer that grew from `before` bytes: within the
    /// string limit and the quota, charged to the logical heap. The growth
    /// costs budget as a bulk copy does.
    fn held_grew(&mut self, before: usize, after: usize, budget: &mut i64) -> Result<(), LuaFault> {
        let grow = after.saturating_sub(before);
        if after > self.heap.max_string || !self.heap.gc.fits(grow as u64) {
            return Err(LuaFault::Memory);
        }
        self.heap.charge_held(grow as u64);
        *budget -= (grow / 64) as i64;
        Ok(())
    }

    /// `string.format`'s next part.
    fn format_next(
        &mut self,
        ctx: &Ctx,
        formatter: &mut crate::strformat::Formatter,
        out: &mut Vec<u8>,
        waiting: &mut bool,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strformat::{Literal, Need, Step};
        let fmt = self.string_at(ctx, 0)?;
        let mut budget = ENGINE_BUDGET + *debt;
        // `%s`'s `__tostring` returned: its result must be a string or a
        // number (`luaL_tolstring`).
        if *waiting {
            *waiting = false;
            let value = self.scratch(ctx, 0);
            let Some(text) = super::builtins::text_arg(&self.heap, value) else {
                return Ok(Next::Fault(LuaFault::ToString));
            };
            // The piece is at most the argument, a string or a number
            // already made, or a width under 100: checked as it grows.
            let before = out.len();
            let given = formatter.give_string(self.bytes_of(fmt), &text, out);
            if let Err(error) = given {
                return format_error(self, ctx, formatter, self.bytes_of(fmt), error);
            }
            if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                return Ok(Next::Fault(fault));
            }
        }
        loop {
            if budget <= 0 {
                *debt = budget;
                return Ok(Next::Busy);
            }
            let before = out.len();
            let step = formatter.next(self.bytes_of(fmt), out, &mut budget);
            if let Err(fault) = self.held_grew(before, out.len(), &mut 0) {
                return Ok(Next::Fault(fault));
            }
            let (arg, need) = match step {
                Step::Pending => continue,
                Step::Done => {
                    *debt = budget.min(0);
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?]));
                }
                Step::Error(error) => {
                    return format_error(self, ctx, formatter, self.bytes_of(fmt), error);
                }
                Step::Need { arg, need } => (arg, need),
            };
            // C numbers the format string 1.
            let index = arg.saturating_sub(1);
            let value = self.lib_arg(ctx, index);
            let before = out.len();
            let fmt_bytes = self.heap.string_bytes(fmt).unwrap_or_default();
            let given = match need {
                Need::Integer => match self.check_int(ctx, StrFn::Format, index) {
                    Ok(n) => formatter.give_integer(fmt_bytes, n, out),
                    Err((fault, text)) => return Ok(Next::Error(fault, text)),
                },
                Need::Number => match super::library::number(&self.heap, value) {
                    Some(x) => formatter.give_number(fmt_bytes, x, out),
                    None => {
                        let (fault, text) = self.type_error(ctx, StrFn::Format, index, "number");
                        return Ok(Next::Error(fault, text));
                    }
                },
                Need::String => {
                    if let Some(method) = index::metamethod(&self.heap, value, b"__tostring") {
                        *waiting = true;
                        *debt = budget.min(0);
                        self.lib_call_args.clear();
                        self.lib_call_args.push(value);
                        return Ok(Next::Op(Op::Call { f: method, into: 0 }));
                    }
                    let text = super::builtins::plain_text(&self.heap, value);
                    formatter.give_string(fmt_bytes, &text, out)
                }
                Need::Pointer => {
                    let token = self.pointer_token(value);
                    formatter.give_pointer(fmt_bytes, token.as_deref(), out)
                }
                Need::Literal => {
                    let literal = match value {
                        Value::String(handle) => Literal::String(self.bytes_of(handle)),
                        Value::Integer(n) => Literal::Integer(n),
                        Value::Float(x) => Literal::Float(x),
                        Value::Nil => Literal::Nil,
                        Value::Bool(bit) => Literal::Bool(bit),
                        _ => Literal::Other,
                    };
                    formatter.give_literal(fmt_bytes, literal, out)
                }
            };
            if let Err(error) = given {
                return format_error(self, ctx, formatter, self.bytes_of(fmt), error);
            }
            if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                return Ok(Next::Fault(fault));
            }
        }
    }

    /// `%p`'s text for a value (ADR 0034): what `tostring` shows after the
    /// type's name, a deterministic identity, never an address. `None`
    /// where Lua's `lua_topointer` is NULL: numbers, booleans, nil. Lua
    /// keeps one copy of each string of at most 40 bytes, so equal short
    /// strings have one address; Moonseed keeps copies, so a short
    /// string's token comes from its bytes instead: 16 hex digits with the
    /// top bit set, which no object's id reaches.
    fn pointer_token(&self, value: Value) -> Option<Vec<u8>> {
        match value {
            Value::Native(index) => {
                let symbol = self.heap.natives.get(index as usize)?;
                Some(format!("builtin: {symbol}").into_bytes())
            }
            Value::String(handle) if self.bytes_of(handle).len() <= SHORT_STRING => {
                use std::hash::{BuildHasher, Hasher};
                let mut hasher = crate::hashutil::StableBuildHasher.build_hasher();
                hasher.write(self.bytes_of(handle));
                Some(format!("0x{:016x}", hasher.finish() | 1 << 63).into_bytes())
            }
            Value::LightUserdata(..) => Some(super::builtins::identity_text(&self.heap, value)),
            _ => {
                let id = self.heap.object_id_of_value(value)?;
                Some(format!("0x{:08x}", id.raw()).into_bytes())
            }
        }
    }

    /// `string.pack`'s next part.
    fn pack_next(
        &mut self,
        ctx: &Ctx,
        packer: &mut crate::strpack::Packer,
        out: &mut Vec<u8>,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strpack::{Need, PackStep};
        let fmt = self.string_at(ctx, 0)?;
        let mut budget = ENGINE_BUDGET + *debt;
        loop {
            if budget <= 0 {
                *debt = budget;
                return Ok(Next::Busy);
            }
            let before = out.len();
            let limit = self.string_room(out.len());
            let step = packer.next(self.bytes_of(fmt), out, limit, &mut budget);
            if let Err(fault) = self.held_grew(before, out.len(), &mut 0) {
                return Ok(Next::Fault(fault));
            }
            let (arg, need) = match step {
                PackStep::Pending => continue,
                PackStep::Done => {
                    *debt = budget.min(0);
                    let bytes = std::mem::take(out);
                    return Ok(Next::Done(vec![self.new_string(bytes)?]));
                }
                PackStep::Error(error) => return pack_error(self, ctx, StrFn::Pack, error),
                PackStep::Need { arg, need } => (arg, need),
            };
            let index = arg.saturating_sub(1);
            let before = out.len();
            let fmt_bytes = self.heap.string_bytes(fmt).unwrap_or_default().to_vec();
            let limit = self.string_room(out.len());
            // `str_pack` pushes a nil after its arguments, so a missing one
            // reads as nil, not as no value.
            let missing = |expected: &str| {
                let text = format!("{expected} expected, got nil");
                let (fault, text) = self.arg_error(ctx, index, text.as_bytes());
                Ok(Next::Error(fault, text))
            };
            let given = match need {
                _ if index >= ctx.passed => {
                    return missing(match need {
                        Need::String => "string",
                        _ => "number",
                    });
                }
                Need::Integer => match self.check_int(ctx, StrFn::Pack, index) {
                    Ok(n) => packer.give_integer(&fmt_bytes, n, out, limit),
                    Err((fault, text)) => return Ok(Next::Error(fault, text)),
                },
                Need::Number => {
                    match super::library::number(&self.heap, self.lib_arg(ctx, index)) {
                        Some(x) => packer.give_number(&fmt_bytes, x, out, limit),
                        None => {
                            let (fault, text) = self.type_error(ctx, StrFn::Pack, index, "number");
                            return Ok(Next::Error(fault, text));
                        }
                    }
                }
                Need::String => match self.check_string(ctx, StrFn::Pack, index)? {
                    Ok(handle) => {
                        let bytes = self.heap.string_bytes(handle).unwrap_or_default();
                        packer.give_string(&fmt_bytes, bytes, out, limit)
                    }
                    Err((fault, text)) => return Ok(Next::Error(fault, text)),
                },
            };
            if let Err(error) = given {
                return pack_error(self, ctx, StrFn::Pack, error);
            }
            if let Err(fault) = self.held_grew(before, out.len(), &mut budget) {
                return Ok(Next::Fault(fault));
            }
        }
    }

    /// `string.unpack`'s next part: each value goes to the next scratch
    /// slot once the stack has room for it.
    fn unpack_next(
        &mut self,
        ctx: &Ctx,
        unpacker: &mut crate::strpack::Unpacker,
        count: &mut u32,
        debt: &mut i64,
    ) -> Result<Next, VmError> {
        use crate::strpack::{UnpackStep, Unpacked};
        let (fmt, data) = (self.string_at(ctx, 0)?, self.string_at(ctx, 1)?);
        let mut budget = ENGINE_BUDGET + *debt;
        loop {
            if budget <= 0 {
                *debt = budget;
                return Ok(Next::Busy);
            }
            let step = unpacker.next(self.bytes_of(fmt), self.bytes_of(data), &mut budget);
            let value = match step {
                UnpackStep::Pending => continue,
                UnpackStep::Error(error) => return pack_error(self, ctx, StrFn::Unpack, error),
                UnpackStep::Done { next } => {
                    *debt = budget.min(0);
                    let mut values: Vec<Value> =
                        (0..*count).map(|index| self.scratch(ctx, index)).collect();
                    values.push(Value::Integer(next));
                    return Ok(Next::Done(values));
                }
                UnpackStep::Value(value) => value,
            };
            // `luaL_checkstack(L, 2, "too many results")`.
            let end = ctx.scratch_slot(count.saturating_add(2)).saturating_add(4);
            if !matches!(self.slot_fault(ctx.active, end), Ok(None)) {
                return Ok(Next::Error(
                    LuaFault::Unpack,
                    b"stack overflow (too many results)".to_vec(),
                ));
            }
            let value = match value {
                Unpacked::Integer(n) => Value::Integer(n),
                Unpacked::Number(x) => Value::Float(x),
                Unpacked::Bytes { start, end } => {
                    let bytes = self
                        .bytes_of(data)
                        .get(start as usize..end as usize)
                        .ok_or(VmError::Corrupt)?
                        .to_vec();
                    budget -= (bytes.len() / 64) as i64;
                    Value::String(self.alloc_string(bytes)?)
                }
            };
            self.write_abs(ctx.active, ctx.scratch_slot(*count), value)?;
            *count += 1;
            unpacker.accept();
        }
    }
}

/// A pattern error as Lua raises it.
fn pattern_error(error: crate::strpat::PatternError) -> Next {
    use crate::strpat::PatternError;
    let fault = match error {
        PatternError::EndsWithPercent => LuaFault::PatternEnd,
        PatternError::MissingBracket => LuaFault::PatternBracket,
        PatternError::MissingBalanceArgs => LuaFault::PatternBalance,
        PatternError::MissingFrontierBracket => LuaFault::PatternFrontier,
        PatternError::InvalidCaptureIndex(_) => LuaFault::CaptureIndex,
        PatternError::InvalidPatternCapture => LuaFault::PatternCapture,
        PatternError::UnfinishedCapture => LuaFault::UnfinishedCapture,
        PatternError::TooManyCaptures => LuaFault::TooManyCaptures,
        PatternError::TooComplex => LuaFault::PatternTooComplex,
        PatternError::InvalidReplacement => LuaFault::ReplacementEscape,
    };
    Next::Error(fault, error.message().into_bytes())
}

/// A `gmatch` iterator's state between calls, as its closure keeps it
/// (ADR 0035): the next start, the end of the last match or -1, and 1
/// once exhausted.
fn gmatch_state(gmatch: &crate::strpat::Gmatch) -> [i64; 3] {
    let (src, lastmatch, exhausted) = gmatch.between_calls();
    [
        i64::from(src),
        lastmatch.map_or(-1, i64::from),
        i64::from(exhausted),
    ]
}

/// The iterator [`gmatch_state`] describes, or `None` for another shape.
fn gmatch_resume(state: &[i64]) -> Option<crate::strpat::Gmatch> {
    let [src, lastmatch, exhausted] = *state else {
        return None;
    };
    let src = u32::try_from(src).ok()?;
    let lastmatch = match lastmatch {
        -1 => None,
        end => Some(u32::try_from(end).ok()?),
    };
    let exhausted = match exhausted {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some(crate::strpat::Gmatch::resume(src, lastmatch, exhausted))
}

/// Whether restore may make a `gmatch` iterator with these values and
/// numbers: two strings and a userdata, and a state within the subject.
pub(crate) fn gmatch_fits(heap: &crate::heap::Heap, values: &[Value], state: &[i64]) -> bool {
    let [Value::String(subject), Value::String(_), Value::Userdata(_)] = values else {
        return false;
    };
    let Some(len) = heap.string_bytes(*subject).map(<[u8]>::len) else {
        return false;
    };
    let len = len as i64;
    match state {
        [src, lastmatch, exhausted] => {
            (0..=len + 1).contains(src)
                && (*lastmatch == -1 || (0..=len).contains(lastmatch))
                && (0..=1).contains(exhausted)
        }
        _ => false,
    }
}

/// A formatter error as Lua raises it.
fn format_error(
    runtime: &Runtime,
    ctx: &Ctx,
    formatter: &crate::strformat::Formatter,
    fmt: &[u8],
    error: crate::strformat::FormatError,
) -> Result<Next, VmError> {
    use crate::strformat::FormatError;
    let text = formatter.message(fmt, error);
    let fault = match error {
        FormatError::NoValue { arg }
        | FormatError::NoLiteral { arg }
        | FormatError::StringContainsZeros { arg } => {
            let (fault, text) = runtime.arg_error(ctx, arg.saturating_sub(1), &text);
            return Ok(Next::Error(fault, text));
        }
        FormatError::InvalidFormat => LuaFault::FormatString,
        FormatError::InvalidConversion => LuaFault::FormatConversion,
        FormatError::InvalidSpecification => LuaFault::FormatSpecification,
        FormatError::QModifiers => LuaFault::FormatQuote,
        // Only a changed snapshot gets here.
        FormatError::Misuse => LuaFault::FormatString,
    };
    Ok(Next::Error(fault, text))
}

/// A pack engine error as Lua raises it.
fn pack_error(
    runtime: &Runtime,
    ctx: &Ctx,
    _function: StrFn,
    error: crate::strpack::PackError,
) -> Result<Next, VmError> {
    use crate::strpack::PackError;
    let text = error.message();
    if let Some(arg) = error.arg() {
        let (fault, text) = runtime.arg_error(ctx, arg.saturating_sub(1), &text);
        return Ok(Next::Error(fault, text));
    }
    let fault = match error {
        PackError::InvalidOption(_) => LuaFault::PackOption,
        PackError::SizeOutOfLimits(_) => LuaFault::PackSize,
        PackError::MissingCSize => LuaFault::PackMissingSize,
        PackError::DoesNotFit(_) => LuaFault::PackIntegerFit,
        PackError::TooLarge => return Ok(Next::Fault(LuaFault::Memory)),
        // A protocol error: only a changed snapshot gets here.
        _ => LuaFault::Argument,
    };
    Ok(Next::Error(fault, text))
}

/// Lua 5.4's longest short string (`LUAI_MAXSHORTLEN`).
const SHORT_STRING: usize = 40;

/// A checked argument, or the Lua error for it.
type Checked<T> = Result<T, (LuaFault, Vec<u8>)>;

/// What checking a string function's arguments came to.
enum Started {
    Done(Vec<Value>),
    Fault(LuaFault),
    /// A Lua error with Lua's wording.
    Error(LuaFault, Vec<u8>),
    Work(StrWork),
}

/// A value's type's index in `RESERVED_NAMES`.
fn type_name_index(value: Value) -> usize {
    match crate::heap::basic_type(value) {
        0 => 0,
        1 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        6 => 5,
        2 | 7 => 7,
        _ => 6,
    }
}

fn arith_op(op: StrArith) -> crate::opcode::ArithOp {
    use crate::opcode::ArithOp;
    match op {
        StrArith::Add => ArithOp::Add,
        StrArith::Sub => ArithOp::Sub,
        StrArith::Mul => ArithOp::Mul,
        StrArith::Mod => ArithOp::Mod,
        StrArith::Pow => ArithOp::Pow,
        StrArith::Div => ArithOp::Div,
        StrArith::Idiv => ArithOp::Idiv,
        StrArith::Unm => ArithOp::Sub,
    }
}
