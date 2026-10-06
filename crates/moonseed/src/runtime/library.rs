//! The `math` and `table` libraries (ADR 0032, ADR 0033).
//!
//! Math functions return at once, except `math.min` and `math.max`, which
//! compare with `<` and so may call `__lt`. Transcendental functions use
//! the portable `libm` crate on every target, so their bits are the same
//! native and on wasm32.
//!
//! A table function, and `math.min` / `math.max`, is a state machine over
//! semantic operations: read `t[i]`, write `t[i]`, take `#t`, compare. An
//! operation that needs no Lua call happens at once; one that calls a
//! metamethod or `table.sort`'s function makes the call from a
//! `Boundary::Builtin` frame, and the machine goes on when it returns. A
//! step runs at most [`BATCH`] operations, then the function goes on in the
//! next step, which costs a unit of fuel (fuel revision 2), so a long loop
//! cannot run inside one quantum. The first step runs in the function's
//! `Call`; a frame is pushed only when a call or a second step needs one.
//! Values the machine keeps live in scratch slots above its arguments, on
//! the stack; the machine itself holds only numbers. As in Lua 5.4.9, no
//! coroutine may yield across these calls.

use std::borrow::Cow;

use super::builtins::text_arg;
use super::*;
use crate::heap::Task;
use crate::library::{
    LibTask, MATH_FUNCTIONS, MAX_SORT_PENDING, MathFn, SortRange, SortState, SortStep, Stage,
    TABLE_FUNCTIONS, TableFn, Wait, Work, unit_float,
};

/// Semantic operations a library function runs per step.
pub(crate) const BATCH: u32 = 32;

/// `table.sort` ranges shorter than this always take the middle pivot
/// (`RANLIMIT` in Lua 5.4.9).
const RANLIMIT: u32 = 100;

/// A semantic operation a library machine asks for.
#[derive(Clone, Copy)]
pub(super) enum Op {
    /// `obj[key]`, into scratch slot `into`.
    Get { obj: Value, key: Value, into: u32 },
    /// `obj[key] = value`.
    Set {
        obj: Value,
        key: Value,
        value: Value,
    },
    /// `#obj`.
    Len { obj: Value },
    /// `a < b`.
    Less { a: Value, b: Value },
    /// `a == b`.
    Equal { a: Value, b: Value },
    /// `f(a, b)`, for its truth: `table.sort`'s order function.
    Order { f: Value, a: Value, b: Value },
    /// `f(...)`, its first result into scratch slot `into`: a string
    /// function's call of Lua (ADR 0034). The arguments wait in
    /// `Runtime::lib_call_args`.
    Call { f: Value, into: u32 },
    /// `f(...)`, its first two results into scratch slots `into` and
    /// `into + 1` (ADR 0039). The arguments wait in `lib_call_args`.
    CallPair { f: Value, into: u32 },
}

/// An operation's result.
pub(super) enum Got {
    Value(Value),
    Truth(bool),
    Stored,
}

/// What a machine does next.
pub(super) enum Next {
    Op(Op),
    Done(Vec<Value>),
    Fault(LuaFault),
    /// A string function did a step's worth of work and goes on in the
    /// next step (ADR 0034).
    Busy,
    /// A Lua error with Lua's own wording (ADR 0034).
    Error(LuaFault, Vec<u8>),
}

impl From<LuaFault> for Next {
    fn from(fault: LuaFault) -> Self {
        Next::Fault(fault)
    }
}

/// The name Lua gives a builtin in messages: its path from the loaded
/// modules, a base function by its global name (`print`, not
/// `base.print`), and `require` by its own.
pub(super) fn lua_name(symbol: &str) -> &str {
    if symbol == crate::package::REQUIRE {
        return "require";
    }
    symbol.strip_prefix("base.").unwrap_or(symbol)
}

/// `luaL_argerror`'s message: the one wording of every builtin's argument
/// errors. `index` is 0-based.
pub(super) fn arg_error_text(name: &[u8], method: bool, index: u32, message: &[u8]) -> Vec<u8> {
    if method && index == 0 {
        let mut text = b"calling '".to_vec();
        text.extend_from_slice(name);
        text.extend_from_slice(b"' on bad self (");
        text.extend_from_slice(message);
        text.push(b')');
        return text;
    }
    let number = if method {
        index
    } else {
        index.saturating_add(1)
    };
    let mut text = format!("bad argument #{number} to '").into_bytes();
    text.extend_from_slice(name);
    text.extend_from_slice(b"' (");
    text.extend_from_slice(message);
    text.push(b')');
    text
}

/// A machine `run_aux` runs: its next part, and how its frame keeps it.
pub(super) trait AuxWork: Sized {
    fn next(runtime: &mut Runtime, ctx: &Ctx, work: &mut Self) -> Result<Next, VmError>;
    fn next_journal(
        runtime: &mut Runtime,
        ctx: &Ctx,
        work: &mut Self,
        _journal: &mut Journal,
    ) -> Result<Next, VmError> {
        Self::next(runtime, ctx, work)
    }
    fn error(runtime: &mut Runtime, ctx: &Ctx, fault: LuaFault, text: Vec<u8>) -> Poll {
        runtime.library_error(ctx.active, fault, text)
    }
    fn wrap(self) -> Work;
    fn wrap_boxed(self: Box<Self>) -> Work;
}

trait AuxStorage<M: AuxWork> {
    fn work_mut(&mut self) -> &mut M;
    fn into_work(self) -> Work;
}

impl<M: AuxWork> AuxStorage<M> for M {
    fn work_mut(&mut self) -> &mut M {
        self
    }
    fn into_work(self) -> Work {
        self.wrap()
    }
}

impl<M: AuxWork> AuxStorage<M> for Box<M> {
    fn work_mut(&mut self) -> &mut M {
        self
    }
    fn into_work(self) -> Work {
        M::wrap_boxed(self)
    }
}

impl AuxWork for crate::strlib::StrWork {
    fn next(runtime: &mut Runtime, ctx: &Ctx, work: &mut Self) -> Result<Next, VmError> {
        runtime.str_next(ctx, work)
    }
    fn wrap(self) -> Work {
        Work::Str(Box::new(self))
    }
    fn wrap_boxed(self: Box<Self>) -> Work {
        Work::Str(self)
    }
}

/// How an operation went.
pub(super) enum Tried {
    Got(Got),
    Call {
        function: Value,
        args: CallArgs,
        wait: Wait,
    },
    Fault(LuaFault),
}

/// Callback operands stay inline for the fixed-arity semantic operations.
/// String/package callbacks retain their variable-length argument vector.
pub(super) enum CallArgs {
    Fixed { values: [Value; 3], len: usize },
    Dynamic(Vec<Value>),
}

impl CallArgs {
    fn two(a: Value, b: Value) -> Self {
        Self::Fixed {
            values: [a, b, Value::Nil],
            len: 2,
        }
    }

    fn three(a: Value, b: Value, c: Value) -> Self {
        Self::Fixed {
            values: [a, b, c],
            len: 3,
        }
    }

    pub(super) fn as_slice(&self) -> &[Value] {
        match self {
            Self::Fixed { values, len } => &values[..*len],
            Self::Dynamic(values) => values,
        }
    }
}

/// Where a library function's arguments and scratch slots are.
#[derive(Clone, Copy)]
pub(super) struct Ctx {
    pub(super) active: Handle<ThreadObj>,
    pub(super) func: u32,
    pub(super) passed: u32,
    pub(super) framed: bool,
}

impl Ctx {
    pub(super) fn scratch_slot(&self, index: u32) -> u32 {
        self.func + 1 + self.passed + index
    }
}

/// A float that fits an integer as that integer, else the float: Lua's
/// `pushnumint`.
fn integral(float: f64) -> Value {
    match crate::compare::float_to_int(float) {
        Some(integer) => Value::Integer(integer),
        None => Value::Float(float),
    }
}

/// `luaL_checknumber`: a number, or a string that reads as one.
pub(super) fn number(heap: &Heap, value: Value) -> Option<f64> {
    let value = match value {
        Value::String(handle) => heap
            .string_bytes(handle)
            .and_then(crate::lex::string_to_number)?,
        other => other,
    };
    match value {
        Value::Integer(integer) => Some(integer as f64),
        Value::Float(float) => Some(float),
        _ => None,
    }
}

/// What `table_like` checks a value can do: read, write, take the length.
const READ: u8 = 1;
const WRITE: u8 = 2;
const LENGTH: u8 = 4;

/// Whether `value` can stand for a table here: Lua's `checktab`. A table
/// can; another value can when its metatable (its type's, ADR 0034) has
/// `__index` to read, `__newindex` to write, and `__len` for the length,
/// as `what` asks.
fn table_like(heap: &Heap, value: Value, what: u8) -> bool {
    if matches!(value, Value::Table(_)) {
        return true;
    }
    let has = |event: &[u8]| index::metamethod(heap, value, event).is_some();
    heap.metatable_of(value).is_some()
        && (what & READ == 0 || has(b"__index"))
        && (what & WRITE == 0 || has(b"__newindex"))
        && (what & LENGTH == 0 || has(b"__len"))
}

/// A deterministic pivot choice for a range whose partitions keep coming
/// out unbalanced. Lua 5.4.9 mixes the clock and the time here; Moonseed
/// mixes the range's bounds, so a sort does the same on every run.
fn randomize(lo: u32, up: u32) -> u32 {
    let mut z = (u64::from(lo) << 32 | u64::from(up)).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)) as u32
}

/// Lua 5.4.9's `choosePivot`, or the middle for a short range or no
/// randomization.
fn pivot(lo: u32, up: u32, rnd: u32) -> u32 {
    let span = up.wrapping_sub(lo);
    if span < RANLIMIT || rnd == 0 {
        return lo.wrapping_add(up) / 2;
    }
    let r4 = span / 4;
    (rnd % r4.wrapping_mul(2).max(1))
        .wrapping_add(lo)
        .wrapping_add(r4)
}

impl Runtime {
    pub(super) fn lib_arg(&self, ctx: &Ctx, index: u32) -> Value {
        if index >= ctx.passed {
            return Value::Nil;
        }
        self.heap
            .threads
            .get(ctx.active)
            .and_then(|thread| thread.stack.get((ctx.func + 1 + index) as usize))
            .copied()
            .unwrap_or(Value::Nil)
    }

    pub(super) fn scratch(&self, ctx: &Ctx, index: u32) -> Value {
        self.heap
            .threads
            .get(ctx.active)
            .and_then(|thread| thread.stack.get(ctx.scratch_slot(index) as usize))
            .copied()
            .unwrap_or(Value::Nil)
    }

    /// Whether argument `index` was given and is not nil.
    pub(super) fn given(&self, ctx: &Ctx, index: u32) -> bool {
        !matches!(self.lib_arg(ctx, index), Value::Nil)
    }

    /// The name Lua's messages give the function running at `ctx`'s call
    /// site, with a search of loaded modules and globals as Lua's fallback
    /// when no call-site name is available (for example through `pcall`).
    pub(super) fn argument_name(&self, ctx: &Ctx) -> (Vec<u8>, bool) {
        if let Some((kind, name)) = self.calling_name_at(ctx.active, ctx.framed) {
            return (name, kind == "method");
        }
        let callee = self
            .heap
            .threads
            .get(ctx.active)
            .and_then(|thread| thread.stack.get(ctx.func as usize))
            .copied()
            .unwrap_or(Value::Nil);
        let symbol = match self.callable(callee) {
            Value::Native(index) => self.heap.natives.get(index as usize).map(String::as_str),
            _ => None,
        };
        let Some(globals) = self.heap.globals else {
            return (b"?".to_vec(), false);
        };
        if let Some(symbol) = symbol {
            let name = lua_name(symbol);
            let target = if let Some((module, field)) = name.split_once('.') {
                let loaded = self.heap.registry.and_then(|registry| {
                    self.heap
                        .table_get_view(registry, crate::table::KeyView::string(b"_LOADED"))
                });
                let module = match loaded {
                    Some(Value::Table(loaded)) => self
                        .heap
                        .table_get_view(loaded, crate::table::KeyView::string(module.as_bytes())),
                    _ => None,
                };
                match module {
                    Some(Value::Table(table)) => Some((table, field)),
                    _ => None,
                }
            } else {
                Some((globals, name))
            };
            if let Some((table, field)) = target
                && self
                    .heap
                    .table_get_view(table, crate::table::KeyView::string(field.as_bytes()))
                    == Some(callee)
            {
                return (name.as_bytes().to_vec(), false);
            }
        }
        if let Some(name) = self.loaded_function_name(callee) {
            return (name, false);
        }
        // luaL_argerror's pushglobalfuncname also searches `_G`. A host
        // native need not have a module symbol matching its global key.
        if let Some(table) = self.heap.tables.get(globals) {
            let mut previous = None;
            while let Ok(Some((key, value))) = table
                .table
                .next_view(previous.and_then(|key| self.heap.key_view(key).ok()))
            {
                if value == callee
                    && let Value::String(key) = key
                    && let Some(bytes) = self.heap.string_bytes(key)
                {
                    return (bytes.to_vec(), false);
                }
                previous = Some(key);
            }
        }
        (b"?".to_vec(), false)
    }

    /// `luaL_argerror` for argument `index` (0-based) of the running
    /// builtin: `bad argument #n to 'name' (message)`.
    pub(super) fn bad_arg(&self, ctx: &Ctx, index: u32, message: &str) -> Next {
        let (name, method) = self.argument_name(ctx);
        Next::Error(
            LuaFault::Argument,
            arg_error_text(&name, method, index, message.as_bytes()),
        )
    }

    /// `luaL_typeerror`: argument `index` is not a `expected`.
    pub(super) fn bad_type(&self, ctx: &Ctx, index: u32, expected: &str) -> Next {
        let got = String::from_utf8_lossy(&self.arg_type_name(ctx, index)).into_owned();
        self.bad_arg(ctx, index, &format!("{expected} expected, got {got}"))
    }

    /// `luaL_checkinteger` of argument `index`, with Lua's errors.
    pub(super) fn int_arg(&self, ctx: &Ctx, index: u32) -> Result<i64, Next> {
        let value = self.lib_arg(ctx, index);
        if let Some(integer) = crate::base::lua_integer(&self.heap, value) {
            return Ok(integer);
        }
        if number(&self.heap, value).is_some() {
            return Err(self.bad_arg(ctx, index, "number has no integer representation"));
        }
        Err(self.bad_type(ctx, index, "number"))
    }

    /// `luaL_optinteger` of argument `index`.
    pub(super) fn opt_int_arg(&self, ctx: &Ctx, index: u32, default: i64) -> Result<i64, Next> {
        if self.given(ctx, index) {
            self.int_arg(ctx, index)
        } else {
            Ok(default)
        }
    }

    /// `luaL_checknumber` of argument `index`, with Lua's errors.
    pub(super) fn num_arg(&self, ctx: &Ctx, index: u32) -> Result<f64, Next> {
        number(&self.heap, self.lib_arg(ctx, index))
            .ok_or_else(|| self.bad_type(ctx, index, "number"))
    }

    /// Install `math` as a global table: its functions, `pi`, `huge`,
    /// `maxinteger`, and `mininteger` (ADR 0032). The registry must have
    /// the functions (see [`crate::register_math`]). The generator is
    /// seeded from the runtime's deterministic entropy, never from the
    /// clock.
    pub fn install_math(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("math")?;
        self.register_module("math", table)?;
        for (name, symbol, _) in MATH_FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        self.set_field(table, "pi", Value::Float(std::f64::consts::PI))?;
        self.set_field(table, "huge", Value::Float(f64::INFINITY))?;
        self.set_field(table, "maxinteger", Value::Integer(i64::MAX))?;
        self.set_field(table, "mininteger", Value::Integer(i64::MIN))?;
        let library = &mut self.heap.library;
        let (n1, n2) = (library.draw(), library.draw());
        library.seed(n1, n2);
        Ok(())
    }

    /// Install `table` as a global table of its functions (ADR 0033). The
    /// registry must have them (see [`crate::register_table`]).
    pub fn install_table(&mut self) -> Result<(), VmError> {
        let table = self.new_library_table("table")?;
        self.register_module("table", table)?;
        for (name, symbol, _) in TABLE_FUNCTIONS {
            let value = self.native_value(symbol)?;
            self.set_field(table, name, value)?;
        }
        Ok(())
    }

    /// Install the base library, `package`, `math`, `table`, `string`,
    /// and `coroutine`: the standard libraries Moonseed has that a sandbox
    /// can keep. `debug` is left out: it breaks the language's guarantees
    /// (ADR 0040), and a host installs it on purpose. The registry must
    /// have them (see [`crate::register_standard`]).
    pub fn install_standard(&mut self) -> Result<(), VmError> {
        self.install_base()?;
        self.install_package()?;
        self.install_math()?;
        self.install_table()?;
        self.install_string()?;
        self.install_utf8()?;
        self.install_coroutine()?;
        self.install_os()
    }

    /// Where `math.randomseed()` with no argument takes its seeds from
    /// (ADR 0032). Host state, not snapshot state. Each seed word is an
    /// external effect committed through the journal, so a replay gets the
    /// same words. Without one, the seeds come from the runtime's
    /// deterministic entropy stream. Unstable API.
    pub fn set_entropy(&mut self, entropy: crate::host::Entropy) {
        self.entropy = Some(entropy);
    }

    /// A new table bound to global `name`, so it is a root before it is
    /// filled.
    pub(super) fn new_library_table(&mut self, name: &str) -> Result<Value, VmError> {
        let table = Value::Table(self.alloc_table()?);
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        self.set_field(Value::Table(globals), name, table)?;
        Ok(table)
    }

    pub(super) fn set_field(
        &mut self,
        table: Value,
        name: &str,
        value: Value,
    ) -> Result<(), VmError> {
        let key = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
        match index::set(&mut self.heap, table, key, value) {
            Ok(_) => Ok(()),
            Err(LuaFault::Memory) => Err(VmError::MemoryLimit),
            Err(_) => Err(VmError::Corrupt),
        }
    }

    /// A single-result builtin whose checked arguments finish inside this
    /// Call's fuel unit. No mutation precedes a fallback: the ordinary path
    /// retains argument errors, coercions, stack checks and resumable work.
    // Preserve the disabled-hook call path's existing inlining under LTO;
    // the added cold hook branch must not move this work out of its caller.
    #[inline(always)]
    pub(super) fn immediate_builtin(
        &self,
        builtin: crate::host::Builtin,
        active: Handle<ThreadObj>,
        func: u32,
        passed: u32,
    ) -> Result<Option<Value>, VmError> {
        use crate::host::Builtin;
        let stack = &self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack;
        let arg = |index: u32| {
            if index < passed {
                stack
                    .get((func + 1 + index) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            }
        };
        let first = arg(0);
        let value = match builtin {
            Builtin::Type if passed != 0 => {
                self.reserved_name(super::builtins::type_index(first))?
            }
            Builtin::Math(MathFn::Abs) => match first {
                Value::Integer(n) => Value::Integer(n.wrapping_abs()),
                Value::Float(n) => Value::Float(n.abs()),
                _ => return Ok(None),
            },
            Builtin::Math(function @ (MathFn::Floor | MathFn::Ceil)) => match first {
                Value::Integer(_) => first,
                Value::Float(n) => integral(if function == MathFn::Floor {
                    libm::floor(n)
                } else {
                    libm::ceil(n)
                }),
                _ => return Ok(None),
            },
            // BATCH comparisons exhaust the machine's budget before Done:
            // at most BATCH-1 comparisons can finish in the original Call.
            Builtin::Math(function @ (MathFn::Min | MathFn::Max))
                if passed != 0 && passed <= BATCH =>
            {
                if !matches!(first, Value::Integer(_) | Value::Float(_)) {
                    return Ok(None);
                }
                let mut best = first;
                for index in 1..passed {
                    let other = arg(index);
                    let (a, b) = if function == MathFn::Max {
                        (best, other)
                    } else {
                        (other, best)
                    };
                    let Some(less) = crate::compare::numbers_only(CmpKind::Lt, a, b) else {
                        return Ok(None);
                    };
                    if less {
                        best = other;
                    }
                }
                best
            }
            Builtin::String(crate::strlib::StrFn::Byte) => {
                let Value::String(subject) = first else {
                    return Ok(None);
                };
                let first = match arg(1) {
                    Value::Nil => 1,
                    Value::Integer(n) => n,
                    _ => return Ok(None),
                };
                let last = match arg(2) {
                    Value::Nil => first,
                    Value::Integer(n) => n,
                    _ => return Ok(None),
                };
                let bytes = self.heap.string_bytes(subject).ok_or(VmError::Corrupt)?;
                let first = crate::strlib::start_position(first, bytes.len());
                let last = crate::strlib::end_position(last, bytes.len());
                if first != last || first == 0 {
                    return Ok(None);
                }
                // string.byte checks argument/scratch room in addition to
                // its final result window, even when only one byte returns.
                if !matches!(
                    self.slot_fault(active, (func + 1 + passed + 1).saturating_add(4)),
                    Ok(None)
                ) {
                    return Ok(None);
                }
                Value::Integer(i64::from(bytes[(first - 1) as usize]))
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// The two iterator builtins can finish without decoding the call site
    /// again or allocating a result vector. Only checked raw reads enter:
    /// errors and an `ipairs` miss with a metatable keep the semantic path.
    pub(super) fn immediate_iterator(
        &self,
        builtin: crate::host::Builtin,
        active: Handle<ThreadObj>,
        func: u32,
        passed: u32,
    ) -> Result<Option<([Value; 2], usize)>, VmError> {
        use crate::host::Builtin;
        let stack = &self.heap.threads.get(active).ok_or(VmError::Corrupt)?.stack;
        let arg = |index: u32| {
            if index < passed {
                stack
                    .get((func + 1 + index) as usize)
                    .copied()
                    .unwrap_or(Value::Nil)
            } else {
                Value::Nil
            }
        };
        let Value::Table(table) = arg(0) else {
            return Ok(None);
        };
        let table = self.heap.tables.get(table).ok_or(VmError::Corrupt)?;
        let pair = match builtin {
            Builtin::IpairsNext => {
                let Value::Integer(control) = arg(1) else {
                    return Ok(None);
                };
                let index = control.wrapping_add(1);
                let value = table.table.get_view(crate::table::KeyView::Integer(index));
                match value {
                    Some(value) => Some((Value::Integer(index), value)),
                    None if table.metatable.is_none() => None,
                    None => return Ok(None),
                }
            }
            Builtin::Next => {
                let key = arg(1);
                let key = if matches!(key, Value::Nil) {
                    None
                } else {
                    let Ok(key) = self.heap.key_view(key) else {
                        return Ok(None);
                    };
                    Some(key)
                };
                let Ok(pair) = table.table.next_view(key) else {
                    return Ok(None);
                };
                pair
            }
            _ => return Ok(None),
        };
        Ok(Some(match pair {
            Some((key, value)) => ([key, value], 2),
            None => ([Value::Nil, Value::Nil], 1),
        }))
    }

    /// A `math` function, called from the active frame's call site.
    pub(super) fn call_math(
        &mut self,
        active: Handle<ThreadObj>,
        function: MathFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        // `min` and `max` compare with `<`, which may call `__lt`.
        if let MathFn::Min | MathFn::Max = function {
            if passed == 0 {
                return Ok(self.finish_next(active, self.bad_arg(&ctx, 0, "value expected")));
            }
            let work = Work::Extreme {
                max: function == MathFn::Max,
                best: 0,
                next: 1,
            };
            return self.run_lib(ctx, work, None, journal);
        }
        match self.math(&ctx, function, journal)? {
            Next::Done(values) => self.base_return(active, &values),
            next => Ok(self.finish_next(active, next)),
        }
    }

    /// Raise the error a machine or a function gave: a class, or a class
    /// and Lua's wording.
    pub(super) fn library_error(
        &mut self,
        active: Handle<ThreadObj>,
        fault: LuaFault,
        text: Vec<u8>,
    ) -> Poll {
        let mut prefixed = self.where_prefix(active);
        prefixed.extend_from_slice(&text);
        let error = self.prefixed(None, prefixed, fault);
        self.throw_on(active, fault, error)
    }

    pub(super) fn finish_next(&mut self, active: Handle<ThreadObj>, next: Next) -> Poll {
        match next {
            Next::Fault(fault) => self.fault(fault),
            Next::Error(fault, text) => self.library_error(active, fault, text),
            _ => self.fault(LuaFault::Native),
        }
    }

    fn math(
        &mut self,
        ctx: &Ctx,
        function: MathFn,
        journal: &mut Journal,
    ) -> Result<Next, VmError> {
        let one = |result: Result<f64, Next>| match result {
            Ok(float) => Next::Done(vec![Value::Float(float)]),
            Err(next) => next,
        };
        let first = self.lib_arg(ctx, 0);
        Ok(match function {
            MathFn::Abs => match first {
                Value::Integer(integer) => Next::Done(vec![Value::Integer(integer.wrapping_abs())]),
                _ => one(self.num_arg(ctx, 0).map(f64::abs)),
            },
            MathFn::Floor | MathFn::Ceil => match first {
                Value::Integer(_) => Next::Done(vec![first]),
                _ => match self.num_arg(ctx, 0) {
                    Ok(float) => Next::Done(vec![integral(if function == MathFn::Floor {
                        libm::floor(float)
                    } else {
                        libm::ceil(float)
                    })]),
                    Err(next) => next,
                },
            },
            MathFn::Fmod => match (first, self.lib_arg(ctx, 1)) {
                (Value::Integer(a), Value::Integer(b)) => match b {
                    0 => self.bad_arg(ctx, 1, "zero"),
                    // `mininteger % -1` would overflow; the answer is 0.
                    -1 => Next::Done(vec![Value::Integer(0)]),
                    _ => Next::Done(vec![Value::Integer(a % b)]),
                },
                _ => match (self.num_arg(ctx, 0), self.num_arg(ctx, 1)) {
                    (Ok(a), Ok(b)) => Next::Done(vec![Value::Float(libm::fmod(a, b))]),
                    (Err(next), _) | (_, Err(next)) => next,
                },
            },
            MathFn::Modf => match first {
                Value::Integer(_) => Next::Done(vec![first, Value::Float(0.0)]),
                _ => match self.num_arg(ctx, 0) {
                    Ok(float) => {
                        // Rounds toward zero; the test is for the infinities.
                        let whole = if float < 0.0 {
                            libm::ceil(float)
                        } else {
                            libm::floor(float)
                        };
                        let part = if float == whole { 0.0 } else { float - whole };
                        Next::Done(vec![integral(whole), Value::Float(part)])
                    }
                    Err(next) => next,
                },
            },
            MathFn::Sqrt => one(self.num_arg(ctx, 0).map(libm::sqrt)),
            MathFn::Sin => one(self.num_arg(ctx, 0).map(libm::sin)),
            MathFn::Cos => one(self.num_arg(ctx, 0).map(libm::cos)),
            MathFn::Tan => one(self.num_arg(ctx, 0).map(libm::tan)),
            MathFn::Asin => one(self.num_arg(ctx, 0).map(libm::asin)),
            MathFn::Acos => one(self.num_arg(ctx, 0).map(libm::acos)),
            MathFn::Exp => one(self.num_arg(ctx, 0).map(libm::exp)),
            MathFn::Atan => one(self.num_arg(ctx, 0).and_then(|y| {
                let x = if self.given(ctx, 1) {
                    self.num_arg(ctx, 1)?
                } else {
                    1.0
                };
                Ok(libm::atan2(y, x))
            })),
            MathFn::Log => one(self.num_arg(ctx, 0).and_then(|x| {
                if !self.given(ctx, 1) {
                    return Ok(libm::log(x));
                }
                let base = self.num_arg(ctx, 1)?;
                Ok(if base == 2.0 {
                    libm::log2(x)
                } else if base == 10.0 {
                    libm::log10(x)
                } else {
                    libm::log(x) / libm::log(base)
                })
            })),
            MathFn::Deg => one(self
                .num_arg(ctx, 0)
                .map(|x| x * (180.0 / std::f64::consts::PI))),
            MathFn::Rad => one(self
                .num_arg(ctx, 0)
                .map(|x| x * (std::f64::consts::PI / 180.0))),
            MathFn::ToInteger => match crate::base::lua_integer(&self.heap, first) {
                Some(integer) => Next::Done(vec![Value::Integer(integer)]),
                None if ctx.passed == 0 => self.bad_arg(ctx, 0, "value expected"),
                None => Next::Done(vec![Value::Nil]),
            },
            MathFn::Type => match first {
                Value::Integer(_) => Next::Done(vec![self.reserved_name(10)?]),
                Value::Float(_) => Next::Done(vec![self.reserved_name(11)?]),
                _ if ctx.passed == 0 => self.bad_arg(ctx, 0, "value expected"),
                _ => Next::Done(vec![Value::Nil]),
            },
            MathFn::Ult => match (self.int_arg(ctx, 0), self.int_arg(ctx, 1)) {
                (Ok(a), Ok(b)) => Next::Done(vec![Value::Bool((a as u64) < (b as u64))]),
                (Err(next), _) | (_, Err(next)) => next,
            },
            // Run as machines by `call_math`.
            MathFn::Min | MathFn::Max => return Err(VmError::Corrupt),
            MathFn::Random => self.random(ctx),
            MathFn::RandomSeed => self.random_seed(ctx, journal)?,
        })
    }

    /// `math.random([m [, n]])`, as Lua 5.4.9's `math_random`. The
    /// generator moves on before the arguments are checked, as in Lua.
    fn random(&mut self, ctx: &Ctx) -> Next {
        let random = self.heap.library.next();
        let (low, up) = match ctx.passed {
            0 => return Next::Done(vec![Value::Float(unit_float(random))]),
            1 => match self.int_arg(ctx, 0) {
                Ok(0) => return Next::Done(vec![Value::Integer(random as i64)]),
                Ok(up) => (1, up),
                Err(next) => return next,
            },
            2 => match (self.int_arg(ctx, 0), self.int_arg(ctx, 1)) {
                (Ok(low), Ok(up)) => (low, up),
                (Err(next), _) | (_, Err(next)) => return next,
            },
            _ => {
                return Next::Error(LuaFault::Argument, b"wrong number of arguments".to_vec());
            }
        };
        if low > up {
            return self.bad_arg(
                ctx,
                if ctx.passed == 1 { 0 } else { 1 },
                "interval is empty",
            );
        }
        let offset = self
            .heap
            .library
            .project(random, (up as u64).wrapping_sub(low as u64));
        Next::Done(vec![Value::Integer(offset.wrapping_add(low as u64) as i64)])
    }

    /// `math.randomseed([x [, y]])`. With no argument the seeds come from
    /// the host's entropy through the journal, or else from the runtime's
    /// deterministic entropy stream (ADR 0032). Returns the two seeds.
    fn random_seed(&mut self, ctx: &Ctx, journal: &mut Journal) -> Result<Next, VmError> {
        let (n1, n2) = if ctx.passed == 0 {
            match self.entropy.take() {
                Some(mut entropy) => {
                    // A panicking source leaves the runtime poisoned.
                    let outer = std::mem::replace(&mut self.in_callback, true);
                    let mut word = || {
                        let effect = EffectId {
                            domain: self.effect_domain,
                            sequence: self.next_sequence,
                        };
                        self.next_sequence = self.next_sequence.saturating_add(1);
                        journal
                            .commit(effect, 0, &mut *entropy)
                            .map(|word| word as u64)
                            .map_err(|_| VmError::Corrupt)
                    };
                    let seeds = (|| Ok::<_, VmError>((word()?, word()?)))();
                    self.in_callback = outer;
                    self.entropy = Some(entropy);
                    seeds?
                }
                // The same two sequence numbers, so later effect ids do not
                // depend on whether the host gave entropy.
                None => {
                    self.next_sequence = self.next_sequence.saturating_add(2);
                    (self.heap.library.draw(), self.heap.library.draw())
                }
            }
        } else {
            match (self.int_arg(ctx, 0), self.opt_int_arg(ctx, 1, 0)) {
                (Ok(n1), Ok(n2)) => (n1 as u64, n2 as u64),
                _ => return Ok(Next::Fault(LuaFault::Argument)),
            }
        };
        self.heap.library.seed(n1, n2);
        Ok(Next::Done(vec![
            Value::Integer(n1 as i64),
            Value::Integer(n2 as i64),
        ]))
    }

    /// A `table` function, called from the active frame's call site.
    pub(super) fn call_table(
        &mut self,
        active: Handle<ThreadObj>,
        function: TableFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        let first = self.lib_arg(&ctx, 0);
        let work = match function {
            TableFn::Insert | TableFn::Remove | TableFn::Sort
                if !table_like(&self.heap, first, READ | WRITE | LENGTH) =>
            {
                return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "table")));
            }
            TableFn::Concat if !table_like(&self.heap, first, READ | LENGTH) => {
                return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "table")));
            }
            TableFn::Insert => Work::Insert {
                stage: Stage::Start,
                pos: 0,
                i: 0,
            },
            TableFn::Remove => Work::Remove {
                stage: Stage::Start,
                size: 0,
                pos: 0,
            },
            TableFn::Move => Work::Move {
                stage: Stage::Start,
                f: 0,
                t: 0,
                n: 0,
                i: 0,
                backward: false,
            },
            TableFn::Concat => Work::Concat {
                stage: Stage::Start,
                i: 0,
                last: 0,
                text: Vec::new(),
            },
            TableFn::Pack => Work::Pack {
                stage: Stage::Start,
                next: 0,
            },
            TableFn::Unpack => Work::Unpack {
                stage: Stage::Start,
                first: 0,
                count: 0,
                got: 0,
            },
            TableFn::Sort => Work::Sort(Box::new(SortState {
                step: SortStep::Length,
                n: 0,
                lo: 0,
                up: 0,
                p: 0,
                i: 0,
                j: 0,
                rnd: 0,
                pending: Vec::new(),
            })),
        };
        self.run_lib(ctx, work, None, journal)
    }

    /// A library frame's call returned: its result is the operation's, and
    /// the machine goes on. Not charged here: `poll` charged the step.
    pub(super) fn finish_lib(
        &mut self,
        ctx: Ctx,
        slot: u32,
        first: Value,
        second: Value,
        saved: LibTask,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let active = ctx.active;
        let object = self.heap.threads.get_mut(active).ok_or(VmError::Corrupt)?;
        let LibTask { work, wait } = saved;
        // The call's slots go; its result is the operation's.
        object.stack.truncate(slot as usize);
        object.top = slot;
        let got = match wait {
            Wait::Nothing => None,
            Wait::Get { into } => {
                self.write_abs(active, ctx.scratch_slot(into), first)?;
                Some(Got::Value(first))
            }
            Wait::Pair { into } => {
                self.write_abs(active, ctx.scratch_slot(into), first)?;
                self.write_abs(active, ctx.scratch_slot(into + 1), second)?;
                Some(Got::Value(first))
            }
            Wait::Set => Some(Got::Stored),
            Wait::Len => Some(Got::Value(first)),
            Wait::Truth => Some(Got::Truth(first.truthy())),
        };
        self.run_lib(ctx, work, got, journal)
    }

    /// Run a library machine for up to [`BATCH`] operations: feed it the
    /// last operation's result, ask it for the next, and do that one,
    /// until it is done, faults, needs a Lua call, or runs out.
    pub(super) fn run_lib(
        &mut self,
        ctx: Ctx,
        work: Work,
        got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("builtin_library_steps");
        // String functions have a loop of their own, so this one stays as
        // small as the table functions need.
        if matches!(
            work,
            Work::Str(_) | Work::Utf8(_) | Work::Os(_) | Work::Package(_) | Work::Debug(_)
        ) {
            return self.run_lib_aux(ctx, work, got, journal);
        }
        let mut held = Some(work);
        let outer = self.heap.working.replace(ctx.active.index);
        let result = if matches!(held, Some(Work::Sort(_))) {
            self.run_lib_steps::<true>(&ctx, &mut held, got, journal)
        } else {
            self.run_lib_steps::<false>(&ctx, &mut held, got, journal)
        };
        self.heap.working = outer;
        // A VM error (a memory or stack limit, which becomes a Lua error)
        // leaves the frame valid too.
        if result.is_err()
            && ctx.framed
            && let Some(work) = held.take()
        {
            self.save_lib(&ctx, work, Wait::Nothing)?;
        }
        result
    }

    // Auxiliary dispatch is cold relative to the table loops below. Keep
    // additions here from changing the inlined sort machine's code generation.
    #[inline(never)]
    fn run_lib_aux(
        &mut self,
        ctx: Ctx,
        work: Work,
        got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        match work {
            Work::Str(work) => self.run_aux_boxed(ctx, work, got, journal),
            Work::Utf8(work) => self.run_aux_boxed(ctx, work, got, journal),
            Work::Os(work) => self.run_aux_boxed(ctx, work, got, journal),
            Work::Package(work) => self.run_aux_boxed(ctx, work, got, journal),
            Work::Debug(work) => self.run_aux_boxed(ctx, work, got, journal),
            _ => Err(VmError::Corrupt),
        }
    }

    /// Run a machine that is not a table function's (a string function,
    /// `require`) for up to [`BATCH`] operations or a step's worth of its
    /// own work: `run_lib`'s loop, kept apart so the table functions' loop
    /// stays small.
    #[inline(never)]
    pub(super) fn run_aux<M: AuxWork>(
        &mut self,
        ctx: Ctx,
        work: M,
        got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        self.run_aux_with(ctx, work, got, journal)
    }

    fn run_aux_boxed<M: AuxWork>(
        &mut self,
        ctx: Ctx,
        work: Box<M>,
        got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        self.run_aux_with(ctx, work, got, journal)
    }

    fn run_aux_with<M: AuxWork, S: AuxStorage<M>>(
        &mut self,
        ctx: Ctx,
        work: S,
        got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        count!("builtin_aux_steps");
        let mut held = Some(work);
        let outer = self.heap.working.replace(ctx.active.index);
        let result = self.run_aux_steps(&ctx, &mut held, got, journal);
        self.heap.working = outer;
        // A VM error (a memory or stack limit, which becomes a Lua error)
        // leaves the frame valid too.
        if result.is_err()
            && ctx.framed
            && let Some(work) = held.take()
        {
            self.save_lib(&ctx, work.into_work(), Wait::Nothing)?;
        }
        result
    }

    fn run_aux_steps<M: AuxWork, S: AuxStorage<M>>(
        &mut self,
        ctx: &Ctx,
        held: &mut Option<S>,
        mut got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let mut budget = BATCH;
        // The result of a call or an operation is in scratch 0, and the
        // machine's next part reads it.
        got.take();
        loop {
            let work = held.as_mut().ok_or(VmError::Corrupt)?;
            if budget == 0 {
                let work = held.take().ok_or(VmError::Corrupt)?;
                self.save_lib(ctx, work.into_work(), Wait::Nothing)?;
                return Ok(Poll::Continue);
            }
            let fail = |held: &mut Option<S>| held.take().map(S::into_work).ok_or(VmError::Corrupt);
            count!("builtin_aux_operations");
            match M::next_journal(self, ctx, work.work_mut(), journal)? {
                Next::Done(values) => {
                    held.take();
                    return if ctx.framed {
                        self.builtin_done(ctx.active, &values)
                    } else {
                        self.base_return(ctx.active, &values)
                    };
                }
                Next::Fault(fault) => {
                    let work = fail(held)?;
                    return self.fail_lib(ctx, work, fault);
                }
                Next::Error(fault, text) => {
                    let work = fail(held)?;
                    if ctx.framed {
                        self.save_lib(ctx, work, Wait::Nothing)?;
                    }
                    return Ok(M::error(self, ctx, fault, text));
                }
                Next::Busy => {
                    let work = fail(held)?;
                    self.save_lib(ctx, work, Wait::Nothing)?;
                    return Ok(Poll::Continue);
                }
                Next::Op(op) => {
                    budget -= 1;
                    match self.try_op(ctx, op)? {
                        Tried::Got(_) => {}
                        Tried::Fault(fault) => {
                            let work = fail(held)?;
                            return self.fail_lib_op(ctx, work, fault, op);
                        }
                        Tried::Call {
                            function,
                            args,
                            wait,
                        } => {
                            let work = fail(held)?;
                            let slot = (
                                ctx.scratch_slot(work.scratch()),
                                if matches!(wait, Wait::Pair { .. }) {
                                    2
                                } else {
                                    1
                                },
                            );
                            self.save_lib(ctx, work, wait)?;
                            return self
                                .call_lib_callback(ctx.active, function, args, slot, journal);
                        }
                    }
                }
            }
        }
    }

    // Specialize sort's next/feed/operation dispatch without changing its
    // batch boundaries, saved state, or the other library machines.
    fn run_lib_steps<const SORT: bool>(
        &mut self,
        ctx: &Ctx,
        held: &mut Option<Work>,
        mut got: Option<Got>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let mut budget = BATCH;
        // A sort invocation stops at a Lua callback or the batch boundary.
        // Re-read both arguments on entry, so a resumed callback or restored
        // frame never inherits facts from an earlier invocation.
        let sort_args = if SORT {
            (self.lib_arg(ctx, 0), self.lib_arg(ctx, 1))
        } else {
            (Value::Nil, Value::Nil)
        };
        loop {
            let work = held.as_mut().ok_or(VmError::Corrupt)?;
            if let Some(result) = got.take()
                && let Err(failure) = if SORT {
                    let Work::Sort(sort) = work else {
                        return Err(VmError::Corrupt);
                    };
                    self.feed_sort(ctx, sort, &result)
                } else {
                    self.feed(ctx, work, result)
                }
            {
                let work = held.take().ok_or(VmError::Corrupt)?;
                return match failure {
                    Next::Error(fault, text) => {
                        if ctx.framed {
                            self.save_lib(ctx, work, Wait::Nothing)?;
                        }
                        Ok(self.library_error(ctx.active, fault, text))
                    }
                    Next::Fault(fault) => self.fail_lib(ctx, work, fault),
                    _ => Err(VmError::Corrupt),
                };
            }
            if budget == 0 {
                let work = held.take().ok_or(VmError::Corrupt)?;
                self.save_lib(ctx, work, Wait::Nothing)?;
                return Ok(Poll::Continue);
            }
            count!("builtin_library_operations");
            let next = if SORT {
                let Work::Sort(sort) = work else {
                    return Err(VmError::Corrupt);
                };
                self.next_sort(ctx, sort, sort_args)?
            } else {
                self.lib_next(ctx, work)?
            };
            match next {
                Next::Done(values) => {
                    held.take();
                    return if ctx.framed {
                        self.builtin_done(ctx.active, &values)
                    } else {
                        self.base_return(ctx.active, &values)
                    };
                }
                Next::Fault(fault) => {
                    let work = held.take().ok_or(VmError::Corrupt)?;
                    return self.fail_lib(ctx, work, fault);
                }
                Next::Error(fault, text) => {
                    let work = held.take().ok_or(VmError::Corrupt)?;
                    if ctx.framed {
                        self.save_lib(ctx, work, Wait::Nothing)?;
                    }
                    return Ok(self.library_error(ctx.active, fault, text));
                }
                // Only string functions make this.
                Next::Busy => return Err(VmError::Corrupt),
                Next::Op(op) => {
                    budget -= 1;
                    let tried = if SORT {
                        self.try_sort_op(ctx, op)?
                    } else {
                        self.try_op(ctx, op)?
                    };
                    match tried {
                        Tried::Got(result) => got = Some(result),
                        Tried::Fault(fault) => {
                            let work = held.take().ok_or(VmError::Corrupt)?;
                            return self.fail_lib_op(ctx, work, fault, op);
                        }
                        Tried::Call {
                            function,
                            args,
                            wait,
                        } => {
                            let work = held.take().ok_or(VmError::Corrupt)?;
                            let slot = (
                                ctx.scratch_slot(work.scratch()),
                                if matches!(wait, Wait::Pair { .. }) {
                                    2
                                } else {
                                    1
                                },
                            );
                            self.save_lib(ctx, work, wait)?;
                            return self
                                .call_lib_callback(ctx.active, function, args, slot, journal);
                        }
                    }
                }
            }
        }
    }

    /// Keep variable-length callback argument storage for the next library
    /// operation, after the callee has copied its arguments into the stack.
    fn call_lib_callback(
        &mut self,
        active: Handle<ThreadObj>,
        function: Value,
        args: CallArgs,
        slot: (u32, u8),
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        match args {
            CallArgs::Fixed { values, len } => {
                self.builtin_call_at(active, function, &values[..len], slot, journal)
            }
            CallArgs::Dynamic(mut values) => {
                let result = self.builtin_call_at(active, function, &values, slot, journal);
                values.clear();
                if values.capacity() > self.lib_call_args.capacity() {
                    self.lib_call_args = values;
                }
                result
            }
        }
    }

    /// Raise a library function's error. A frame gets its work back first:
    /// the unwind pops it a step later, and a checkpoint between holds it.
    pub(super) fn fail_lib(
        &mut self,
        ctx: &Ctx,
        work: Work,
        fault: LuaFault,
    ) -> Result<Poll, VmError> {
        if ctx.framed {
            self.save_lib(ctx, work, Wait::Nothing)?;
        }
        Ok(self.fault(fault))
    }

    fn fail_lib_op(
        &mut self,
        ctx: &Ctx,
        work: Work,
        fault: LuaFault,
        op: Op,
    ) -> Result<Poll, VmError> {
        let Some(text) = self.library_op_message(fault, op) else {
            return self.fail_lib(ctx, work, fault);
        };
        if ctx.framed {
            self.save_lib(ctx, work, Wait::Nothing)?;
        }
        let error = self.prefixed(None, text, fault);
        Ok(self.throw_on(ctx.active, fault, error))
    }

    /// Keep a machine in its frame, pushing the frame when the function
    /// has none yet.
    pub(super) fn save_lib(&mut self, ctx: &Ctx, work: Work, wait: Wait) -> Result<(), VmError> {
        let task = LibTask { work, wait };
        if !ctx.framed {
            let task = if let Some(mut spare) = self.lib_task_spare.take() {
                *spare = task;
                spare
            } else {
                Box::new(task)
            };
            return self.push_builtin(ctx.active, Task::Lib(task));
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
                task: Task::Lib(slot),
                ..
            }) => {
                **slot = task;
                Ok(())
            }
            _ => Err(VmError::Corrupt),
        }
    }

    /// Do one semantic operation, at once or as a call.
    pub(super) fn try_op(&mut self, ctx: &Ctx, op: Op) -> Result<Tried, VmError> {
        let call = |function, args, wait| Tried::Call {
            function,
            args,
            wait,
        };
        Ok(match op {
            Op::Get { obj, key, into } => match index::get(&self.heap, obj, key) {
                Ok(Resolved::Done(value)) => {
                    self.write_abs(ctx.active, ctx.scratch_slot(into), value)?;
                    Tried::Got(Got::Value(value))
                }
                Ok(Resolved::Call { function, target }) => {
                    call(function, CallArgs::two(target, key), Wait::Get { into })
                }
                Err(fault) => Tried::Fault(fault),
            },
            Op::Set { obj, key, value } => match self.set_value(obj, key, value) {
                Ok(Resolved::Done(_)) => Tried::Got(Got::Stored),
                Ok(Resolved::Call { function, target }) => {
                    call(function, CallArgs::three(target, key, value), Wait::Set)
                }
                Err(fault) => Tried::Fault(fault),
            },
            Op::Len { obj } => match index::len(&self.heap, obj) {
                Ok(Resolved::Done(value)) => Tried::Got(Got::Value(value)),
                Ok(Resolved::Call { function, target }) => {
                    call(function, CallArgs::two(target, target), Wait::Len)
                }
                Err(fault) => Tried::Fault(fault),
            },
            Op::Less { a, b } | Op::Equal { a, b } => {
                let kind = if matches!(op, Op::Less { .. }) {
                    CmpKind::Lt
                } else {
                    CmpKind::Eq
                };
                match ops::compare(&self.heap, kind, a, b) {
                    Ok(Truth::Done(bit)) => Tried::Got(Got::Truth(bit)),
                    Ok(Truth::Call(handler)) => call(handler, CallArgs::two(a, b), Wait::Truth),
                    Err(fault) => Tried::Fault(fault),
                }
            }
            Op::Order { f, a, b } => call(f, CallArgs::two(a, b), Wait::Truth),
            Op::Call { f, into } => call(
                f,
                CallArgs::Dynamic(std::mem::take(&mut self.lib_call_args)),
                Wait::Get { into },
            ),
            Op::CallPair { f, into } => call(
                f,
                CallArgs::Dynamic(std::mem::take(&mut self.lib_call_args)),
                Wait::Pair { into },
            ),
        })
    }

    /// Resolve one sort operation using only facts checked here. A live
    /// table key bypasses __index even when a comparator has installed a
    /// metatable. Stores require no metatable so a failed update probe cannot
    /// add a write barrier to a store intercepted by __newindex.
    /// Missing keys and nil stores use the ordinary
    /// resolver, as do comparisons that may run __lt. Nothing is cached
    /// across a Lua call, collection, host pause, or restored checkpoint.
    #[inline(always)]
    fn try_sort_op(&mut self, ctx: &Ctx, op: Op) -> Result<Tried, VmError> {
        match op {
            Op::Get {
                obj: Value::Table(table),
                key: Value::Integer(key),
                into,
            } => {
                if let Some(value) =
                    self.heap.tables.get(table).and_then(|object| {
                        object.table.get_view(crate::table::KeyView::Integer(key))
                    })
                {
                    count!("sort_direct_get");
                    self.write_abs(ctx.active, ctx.scratch_slot(into), value)?;
                    return Ok(Tried::Got(Got::Value(value)));
                }
            }
            Op::Set {
                obj: Value::Table(table),
                key: Value::Integer(key),
                value,
            } if !matches!(value, Value::Nil)
                && self
                    .heap
                    .tables
                    .get(table)
                    .is_some_and(|object| object.metatable.is_none()) =>
            {
                // Updating a live slot allocates nothing and preserves its
                // key and traversal position. Use the same write barrier as
                // Heap::table_insert (the integer key has no GC reference).
                if self
                    .heap
                    .tables
                    .get_mut_storing(table, crate::gc::value_ref(value).is_some())
                    .is_some_and(|object| {
                        object
                            .table
                            .update_view(crate::table::KeyView::Integer(key), value)
                    })
                {
                    count!("sort_direct_set");
                    return Ok(Tried::Got(Got::Stored));
                }
            }
            Op::Less {
                a: Value::Integer(a),
                b: Value::Integer(b),
            } => {
                count!("sort_direct_integer_less");
                return Ok(Tried::Got(Got::Truth(a < b)));
            }
            Op::Less { a, b } => {
                if let Ok(bit) = crate::compare::less_than(&self.heap, a, b) {
                    return Ok(Tried::Got(Got::Truth(bit)));
                }
            }
            _ => {}
        }
        self.try_op(ctx, op)
    }

    /// The length an operation gave, as the table library needs it: an
    /// integer, a float with an integer value, or a string that reads as
    /// one (Lua's `luaL_len`).
    fn length(&self, got: Got) -> Result<i64, LuaFault> {
        match got {
            Got::Value(value) => {
                crate::base::lua_integer(&self.heap, value).ok_or(LuaFault::LengthType)
            }
            _ => Err(LuaFault::LengthType),
        }
    }

    /// Give a machine its last operation's result. `Err` is a Lua error
    /// the function raises.
    fn feed(&mut self, ctx: &Ctx, work: &mut Work, got: Got) -> Result<(), Next> {
        let truth = |got: &Got| matches!(got, Got::Truth(true));
        {
            match work {
                Work::Extreme { best, next, .. } => {
                    if truth(&got) {
                        *best = *next;
                    }
                    *next = next.wrapping_add(1);
                }
                Work::Insert { stage, pos, i } => match stage {
                    Stage::Length => {
                        let end = self.length(got)?.wrapping_add(1);
                        match ctx.passed {
                            2 => {
                                *pos = end;
                                *stage = Stage::Last;
                            }
                            3 => {
                                *pos = self.int_arg(ctx, 1)?;
                                if (*pos as u64).wrapping_sub(1) >= end as u64 {
                                    return Err(self.bad_arg(ctx, 1, "position out of bounds"));
                                }
                                *i = end;
                                *stage = Stage::Read;
                            }
                            _ => {
                                return Err(Next::Error(
                                    LuaFault::Argument,
                                    b"wrong number of arguments to 'insert'".to_vec(),
                                ));
                            }
                        }
                    }
                    Stage::Read => *stage = Stage::Write,
                    Stage::Write => {
                        *i = i.wrapping_sub(1);
                        *stage = Stage::Read;
                    }
                    _ => *stage = Stage::Done,
                },
                Work::Remove { stage, size, pos } => match stage {
                    Stage::Length => {
                        *size = self.length(got)?;
                        *pos = self.opt_int_arg(ctx, 1, *size)?;
                        if *pos != *size && (*pos as u64).wrapping_sub(1) > *size as u64 {
                            return Err(self.bad_arg(ctx, 1, "position out of bounds"));
                        }
                        *stage = Stage::First;
                    }
                    Stage::First => *stage = Stage::Read,
                    Stage::Read => *stage = Stage::Write,
                    Stage::Write => {
                        *pos = pos.wrapping_add(1);
                        *stage = Stage::Read;
                    }
                    _ => *stage = Stage::Done,
                },
                Work::Move {
                    stage,
                    n,
                    i,
                    backward,
                    ..
                } => match stage {
                    Stage::Equal => {
                        *backward = truth(&got);
                        *i = if *backward { n.wrapping_sub(1) } else { 0 };
                        *stage = Stage::Read;
                    }
                    Stage::Read => *stage = Stage::Write,
                    _ => {
                        *i = i.wrapping_add(if *backward { -1 } else { 1 });
                        *stage = Stage::Read;
                    }
                },
                Work::Concat {
                    stage,
                    i,
                    last,
                    text,
                } => match stage {
                    Stage::Length => {
                        *last = self.length(got)?;
                        let separator = self.lib_arg(ctx, 1);
                        if !matches!(separator, Value::Nil)
                            && text_arg(&self.heap, separator).is_none()
                        {
                            return Err(self.bad_type(ctx, 1, "string"));
                        }
                        *i = self.opt_int_arg(ctx, 2, 1)?;
                        *last = self.opt_int_arg(ctx, 3, *last)?;
                        *stage = Stage::Read;
                    }
                    _ => {
                        // `tconcat`'s wording names the index.
                        let bad = |index: i64, value: Value| {
                            Next::Error(
                                LuaFault::ConcatValue,
                                format!(
                                    "invalid value ({}) at index {index} in table for 'concat'",
                                    crate::heap::type_name(value)
                                )
                                .into_bytes(),
                            )
                        };
                        let Got::Value(value) = got else {
                            return Err(bad(*i, Value::Nil));
                        };
                        let piece = text_arg(&self.heap, value)
                            .ok_or_else(|| bad(*i, value))?
                            .into_owned();
                        let more = *i < *last;
                        let separator = if more {
                            text_arg(&self.heap, self.lib_arg(ctx, 1))
                                .map_or(Vec::new(), Cow::into_owned)
                        } else {
                            Vec::new()
                        };
                        let grow = piece.len() + separator.len();
                        if text.len() + grow > self.heap.max_string
                            || !self.heap.gc.fits(grow as u64)
                        {
                            return Err(LuaFault::Memory.into());
                        }
                        self.heap.charge_held(grow as u64);
                        text.extend_from_slice(&piece);
                        text.extend_from_slice(&separator);
                        if more {
                            *i = i.wrapping_add(1);
                        } else {
                            *stage = Stage::Done;
                        }
                    }
                },
                Work::Pack { stage, next } => match stage {
                    Stage::Write => *next = next.wrapping_add(1),
                    _ => *stage = Stage::Done,
                },
                Work::Unpack {
                    stage,
                    first,
                    count,
                    got: read,
                } => match stage {
                    Stage::Length => {
                        let last = self.length(got)?;
                        self.unpack_range(ctx, stage, *first, last, count)?;
                    }
                    _ => *read = read.wrapping_add(1),
                },
                Work::Sort(sort) => self.feed_sort(ctx, sort, &got)?,
                Work::Str(_) | Work::Utf8(_) | Work::Os(_) | Work::Package(_) | Work::Debug(_) => {
                    return Err(LuaFault::Type.into());
                }
            }
            Ok(())
        }
    }

    /// `table.unpack`'s range `first..=last`: its count, checked against
    /// the stack before anything is read.
    fn unpack_range(
        &self,
        ctx: &Ctx,
        stage: &mut Stage,
        first: i64,
        last: i64,
        count: &mut u32,
    ) -> Result<(), LuaFault> {
        if first > last {
            *stage = Stage::Done;
            return Ok(());
        }
        let n = (last as u64).wrapping_sub(first as u64);
        if n >= i32::MAX as u64 {
            return Err(LuaFault::Unpack);
        }
        let n = n as u32 + 1;
        // Room for the results and for a call above them.
        let end = ctx.scratch_slot(n).saturating_add(4);
        match self.slot_fault(ctx.active, end) {
            Ok(None) => {}
            _ => return Err(LuaFault::Unpack),
        }
        *count = n;
        *stage = Stage::Read;
        Ok(())
    }

    /// The machine's next operation, or its end.
    fn lib_next(&mut self, ctx: &Ctx, work: &mut Work) -> Result<Next, VmError> {
        let arg = |runtime: &Self, index| runtime.lib_arg(ctx, index);
        let fault = |f| Ok(Next::Fault(f));
        match work {
            Work::Extreme { max, best, next } => {
                if *next >= ctx.passed {
                    return Ok(Next::Done(vec![arg(self, *best)]));
                }
                let (winner, other) = (arg(self, *best), arg(self, *next));
                let (a, b) = if *max {
                    (winner, other)
                } else {
                    (other, winner)
                };
                Ok(Next::Op(Op::Less { a, b }))
            }
            Work::Insert { stage, pos, i } => {
                let list = arg(self, 0);
                if *stage == Stage::Start {
                    *stage = Stage::Length;
                    return Ok(Next::Op(Op::Len { obj: list }));
                }
                if *stage == Stage::Read && *i <= *pos {
                    *stage = Stage::Last;
                }
                Ok(Next::Op(match stage {
                    Stage::Read => Op::Get {
                        obj: list,
                        key: Value::Integer(i.wrapping_sub(1)),
                        into: 0,
                    },
                    Stage::Write => Op::Set {
                        obj: list,
                        key: Value::Integer(*i),
                        value: self.scratch(ctx, 0),
                    },
                    Stage::Last => Op::Set {
                        obj: list,
                        key: Value::Integer(*pos),
                        value: arg(self, ctx.passed - 1),
                    },
                    _ => return Ok(Next::Done(Vec::new())),
                }))
            }
            Work::Remove { stage, size, pos } => {
                let list = arg(self, 0);
                if *stage == Stage::Start {
                    *stage = Stage::Length;
                    return Ok(Next::Op(Op::Len { obj: list }));
                }
                if *stage == Stage::Read && *pos >= *size {
                    *stage = Stage::Last;
                }
                Ok(Next::Op(match stage {
                    Stage::First => Op::Get {
                        obj: list,
                        key: Value::Integer(*pos),
                        into: 0,
                    },
                    Stage::Read => Op::Get {
                        obj: list,
                        key: Value::Integer(pos.wrapping_add(1)),
                        into: 1,
                    },
                    Stage::Write => Op::Set {
                        obj: list,
                        key: Value::Integer(*pos),
                        value: self.scratch(ctx, 1),
                    },
                    Stage::Last => Op::Set {
                        obj: list,
                        key: Value::Integer(*pos),
                        value: Value::Nil,
                    },
                    _ => return Ok(Next::Done(vec![self.scratch(ctx, 0)])),
                }))
            }
            Work::Move {
                stage,
                f,
                t,
                n,
                i,
                backward,
            } => {
                let source = arg(self, 0);
                let dest_index = if self.given(ctx, 4) { 4 } else { 0 };
                let dest = arg(self, dest_index);
                if *stage == Stage::Start {
                    let (first, end, to) = match (
                        self.int_arg(ctx, 1),
                        self.int_arg(ctx, 2),
                        self.int_arg(ctx, 3),
                    ) {
                        (Ok(first), Ok(end), Ok(to)) => (first, end, to),
                        (Err(next), _, _) | (_, Err(next), _) | (_, _, Err(next)) => {
                            return Ok(next);
                        }
                    };
                    if !table_like(&self.heap, source, READ) {
                        return Ok(self.bad_type(ctx, 0, "table"));
                    }
                    if !table_like(&self.heap, dest, WRITE) {
                        return Ok(self.bad_type(ctx, dest_index, "table"));
                    }
                    if end < first {
                        *stage = Stage::Done;
                        return Ok(Next::Done(vec![dest]));
                    }
                    // Lua's checks, before anything moves.
                    if !(first > 0 || end < i64::MAX + first) {
                        return Ok(self.bad_arg(ctx, 2, "too many elements to move"));
                    }
                    let count = end - first + 1;
                    if to > i64::MAX - count + 1 {
                        return Ok(self.bad_arg(ctx, 3, "destination wrap around"));
                    }
                    (*f, *t, *n) = (first, to, count);
                    // Lua copies backward when the ranges overlap in the
                    // same table; with a second table given, `==` decides.
                    if to > end || to <= first {
                        *i = 0;
                        *stage = Stage::Read;
                    } else if dest_index == 0 {
                        *backward = true;
                        *i = count - 1;
                        *stage = Stage::Read;
                    } else {
                        *stage = Stage::Equal;
                        return Ok(Next::Op(Op::Equal { a: source, b: dest }));
                    }
                }
                let more = if *backward { *i >= 0 } else { *i < *n };
                Ok(match stage {
                    Stage::Read if more => Next::Op(Op::Get {
                        obj: source,
                        key: Value::Integer(f.wrapping_add(*i)),
                        into: 0,
                    }),
                    Stage::Write => Next::Op(Op::Set {
                        obj: dest,
                        key: Value::Integer(t.wrapping_add(*i)),
                        value: self.scratch(ctx, 0),
                    }),
                    _ => Next::Done(vec![dest]),
                })
            }
            Work::Concat {
                stage,
                i,
                last,
                text,
            } => {
                let list = arg(self, 0);
                match stage {
                    Stage::Start => {
                        *stage = Stage::Length;
                        Ok(Next::Op(Op::Len { obj: list }))
                    }
                    Stage::Read if *i <= *last => Ok(Next::Op(Op::Get {
                        obj: list,
                        key: Value::Integer(*i),
                        into: 0,
                    })),
                    _ => {
                        let bytes = std::mem::take(text);
                        Ok(Next::Done(vec![self.new_string(bytes)?]))
                    }
                }
            }
            Work::Pack { stage, next } => {
                if *stage == Stage::Start {
                    self.make_room(1, cost::OBJECT);
                    let table = Value::Table(self.alloc_table()?);
                    self.write_abs(ctx.active, ctx.scratch_slot(0), table)?;
                    *stage = Stage::Write;
                }
                let table = self.scratch(ctx, 0);
                if *stage == Stage::Write && *next >= ctx.passed {
                    *stage = Stage::Last;
                }
                Ok(Next::Op(match stage {
                    Stage::Write => Op::Set {
                        obj: table,
                        key: Value::Integer(i64::from(*next) + 1),
                        value: arg(self, *next),
                    },
                    Stage::Last => Op::Set {
                        obj: table,
                        key: self.new_string(b"n".to_vec())?,
                        value: Value::Integer(i64::from(ctx.passed)),
                    },
                    _ => return Ok(Next::Done(vec![table])),
                }))
            }
            Work::Unpack {
                stage,
                first,
                count,
                got,
            } => {
                let list = arg(self, 0);
                if *stage == Stage::Start {
                    *first = match self.opt_int_arg(ctx, 1, 1) {
                        Ok(first) => first,
                        Err(next) => return Ok(next),
                    };
                    if !self.given(ctx, 2) {
                        *stage = Stage::Length;
                        return Ok(Next::Op(Op::Len { obj: list }));
                    }
                    let last = match self.int_arg(ctx, 2) {
                        Ok(last) => last,
                        Err(next) => return Ok(next),
                    };
                    if let Err(f) = self.unpack_range(ctx, stage, *first, last, count) {
                        return fault(f);
                    }
                }
                if *stage == Stage::Read && *got < *count {
                    return Ok(Next::Op(Op::Get {
                        obj: list,
                        key: Value::Integer(first.wrapping_add(i64::from(*got))),
                        into: *got,
                    }));
                }
                let values = (0..*count).map(|k| self.scratch(ctx, k)).collect();
                Ok(Next::Done(values))
            }
            Work::Sort(sort) => {
                self.next_sort(ctx, sort, (self.lib_arg(ctx, 0), self.lib_arg(ctx, 1)))
            }
            Work::Str(_) | Work::Utf8(_) | Work::Os(_) | Work::Package(_) | Work::Debug(_) => {
                Err(VmError::Corrupt)
            }
        }
    }

    #[inline(always)]
    fn next_sort(
        &self,
        ctx: &Ctx,
        sort: &mut SortState,
        (list, comparator): (Value, Value),
    ) -> Result<Next, VmError> {
        // Scratch operands are live stack values. The borrow ends before
        // try_sort_op can write the table or run Lua.
        let thread = self.heap.threads.get(ctx.active).ok_or(VmError::Corrupt)?;
        let s = |index| {
            thread
                .stack
                .get(ctx.scratch_slot(index) as usize)
                .copied()
                .unwrap_or(Value::Nil)
        };
        let get = |at: u32, into| Op::Get {
            obj: list,
            key: Value::Integer(i64::from(at)),
            into,
        };
        let set = |at: u32, from| Op::Set {
            obj: list,
            key: Value::Integer(i64::from(at)),
            value: s(from),
        };
        let order = |a, b| match comparator {
            Value::Nil => Op::Less { a, b },
            f => Op::Order { f, a, b },
        };
        loop {
            count!("sort_state_transitions");
            let op = match sort.step {
                SortStep::Length => Op::Len { obj: list },
                SortStep::Range => {
                    count!("sort_range_transitions");
                    if sort.lo < sort.up {
                        sort.step = SortStep::GetLo;
                        continue;
                    }
                    let Some(range) = sort.pending.pop() else {
                        return Ok(Next::Done(Vec::new()));
                    };
                    (sort.lo, sort.up, sort.rnd) = (range.lo, range.up, range.rnd);
                    // Lua's balance test, after the smaller part is sorted.
                    if sort.up.wrapping_sub(sort.lo) / 128 > range.smaller {
                        sort.rnd = randomize(sort.lo, sort.up);
                    }
                    continue;
                }
                SortStep::GetLo => get(sort.lo, 0),
                SortStep::GetUp => get(sort.up, 1),
                SortStep::UpLessLo => order(s(1), s(0)),
                SortStep::SetLoUp => set(sort.lo, 1),
                SortStep::SetUpLo => set(sort.up, 0),
                SortStep::GetP => get(sort.p, 0),
                SortStep::GetLo2 => get(sort.lo, 1),
                SortStep::PLessLo => order(s(0), s(1)),
                SortStep::SetPLo => set(sort.p, 1),
                SortStep::SetLoP => set(sort.lo, 0),
                SortStep::GetUp2 => get(sort.up, 1),
                SortStep::UpLessP => order(s(1), s(0)),
                SortStep::SetPUp => set(sort.p, 1),
                SortStep::SetUpP => set(sort.up, 0),
                SortStep::GetPivot => get(sort.p, 0),
                SortStep::GetUpMinus1 => get(sort.up.wrapping_sub(1), 1),
                SortStep::SetPToUpMinus1 => set(sort.p, 1),
                SortStep::SetUpMinus1ToPivot => set(sort.up.wrapping_sub(1), 0),
                SortStep::GetI => get(sort.i, 1),
                SortStep::ILessPivot => order(s(1), s(0)),
                SortStep::GetJ => get(sort.j, 2),
                SortStep::PivotLessJ => order(s(0), s(2)),
                SortStep::SetIJ => set(sort.i, 2),
                SortStep::SetJI => set(sort.j, 1),
                SortStep::SetUpMinus1I => set(sort.up.wrapping_sub(1), 1),
                SortStep::SetIPivot => set(sort.i, 0),
            };
            #[cfg(feature = "counters")]
            match &op {
                Op::Get { .. } => count!("sort_table_gets"),
                Op::Set { .. } => count!("sort_table_sets"),
                Op::Less { .. } | Op::Order { .. } => count!("sort_comparisons"),
                Op::Len { .. } => count!("sort_lengths"),
                _ => unreachable!(),
            }
            return Ok(Next::Op(op));
        }
    }

    /// `table.sort`'s transitions, as Lua 5.4.9's `auxsort` and
    /// `partition` go from one operation to the next.
    #[inline(always)]
    fn feed_sort(&self, ctx: &Ctx, sort: &mut SortState, got: &Got) -> Result<(), Next> {
        let truth = matches!(got, Got::Truth(true));
        // A range is done: the next is taken in `Range`.
        let range_done = |sort: &mut SortState| {
            sort.up = sort.lo;
            sort.step = SortStep::Range;
        };
        let after_ends = |sort: &mut SortState| {
            if sort.up.wrapping_sub(sort.lo) == 1 {
                range_done(sort);
            } else {
                count!("sort_pivot_selections");
                sort.p = pivot(sort.lo, sort.up, sort.rnd);
                sort.step = SortStep::GetP;
            }
        };
        let after_three = |sort: &mut SortState| {
            if sort.up.wrapping_sub(sort.lo) == 2 {
                range_done(sort);
            } else {
                sort.step = SortStep::GetPivot;
            }
        };
        count!("sort_feed_transitions");
        #[cfg(feature = "counters")]
        if matches!(
            sort.step,
            SortStep::SetUpMinus1ToPivot
                | SortStep::GetI
                | SortStep::ILessPivot
                | SortStep::GetJ
                | SortStep::PivotLessJ
                | SortStep::SetIJ
                | SortStep::SetJI
                | SortStep::SetUpMinus1I
                | SortStep::SetIPivot
        ) {
            count!("sort_partition_transitions");
        }
        sort.step = match sort.step {
            SortStep::Length => {
                let n = self.length(match got {
                    Got::Value(value) => Got::Value(*value),
                    _ => Got::Stored,
                })?;
                if n > 1 {
                    if n >= i64::from(i32::MAX) {
                        return Err(self.bad_arg(ctx, 0, "array too big"));
                    }
                    let order = self.lib_arg(ctx, 1);
                    if !(matches!(order, Value::Nil) || order.is_function()) {
                        return Err(self.bad_type(ctx, 1, "function"));
                    }
                    sort.n = n as u32;
                    (sort.lo, sort.up) = (1, n as u32);
                }
                SortStep::Range
            }
            SortStep::GetLo => SortStep::GetUp,
            SortStep::GetUp => SortStep::UpLessLo,
            SortStep::UpLessLo if truth => SortStep::SetLoUp,
            SortStep::UpLessLo | SortStep::SetUpLo => {
                after_ends(sort);
                return Ok(());
            }
            SortStep::SetLoUp => SortStep::SetUpLo,
            SortStep::GetP => SortStep::GetLo2,
            SortStep::GetLo2 => SortStep::PLessLo,
            SortStep::PLessLo if truth => SortStep::SetPLo,
            SortStep::PLessLo => SortStep::GetUp2,
            SortStep::SetPLo => SortStep::SetLoP,
            SortStep::GetUp2 => SortStep::UpLessP,
            SortStep::UpLessP if truth => SortStep::SetPUp,
            SortStep::SetPUp => SortStep::SetUpP,
            SortStep::SetLoP | SortStep::UpLessP | SortStep::SetUpP => {
                after_three(sort);
                return Ok(());
            }
            SortStep::GetPivot => SortStep::GetUpMinus1,
            SortStep::GetUpMinus1 => SortStep::SetPToUpMinus1,
            SortStep::SetPToUpMinus1 => SortStep::SetUpMinus1ToPivot,
            SortStep::SetUpMinus1ToPivot => {
                // `partition`: `i` and `j` move before their first use.
                sort.i = sort.lo.wrapping_add(1);
                sort.j = sort.up.wrapping_sub(1);
                SortStep::GetI
            }
            SortStep::GetI => SortStep::ILessPivot,
            SortStep::ILessPivot if truth => {
                // `a[i] < P`, but `a[up - 1] == P`.
                if sort.i == sort.up.wrapping_sub(1) {
                    return Err(LuaFault::OrderFunction.into());
                }
                sort.i = sort.i.wrapping_add(1);
                SortStep::GetI
            }
            SortStep::ILessPivot => {
                sort.j = sort.j.wrapping_sub(1);
                SortStep::GetJ
            }
            SortStep::GetJ => SortStep::PivotLessJ,
            SortStep::PivotLessJ if truth => {
                // `j < i`, but `a[j] > P`.
                if sort.j < sort.i {
                    return Err(LuaFault::OrderFunction.into());
                }
                sort.j = sort.j.wrapping_sub(1);
                SortStep::GetJ
            }
            SortStep::PivotLessJ if sort.j < sort.i => SortStep::SetUpMinus1I,
            SortStep::PivotLessJ => SortStep::SetIJ,
            SortStep::SetIJ => SortStep::SetJI,
            SortStep::SetJI => {
                sort.i = sort.i.wrapping_add(1);
                SortStep::GetI
            }
            SortStep::SetUpMinus1I => SortStep::SetIPivot,
            SortStep::SetIPivot => {
                // Sort the smaller part first; the larger waits, with the
                // smaller's size for the balance test.
                let p = sort.i;
                let (lower, upper) = (p.wrapping_sub(sort.lo), sort.up.wrapping_sub(p));
                let waiting = if lower < upper {
                    let range = SortRange {
                        lo: p.wrapping_add(1),
                        up: sort.up,
                        smaller: lower,
                        rnd: sort.rnd,
                    };
                    sort.up = p.wrapping_sub(1);
                    range
                } else {
                    let range = SortRange {
                        lo: sort.lo,
                        up: p.wrapping_sub(1),
                        smaller: upper,
                        rnd: sort.rnd,
                    };
                    sort.lo = p.wrapping_add(1);
                    range
                };
                if sort.pending.len() >= MAX_SORT_PENDING {
                    return Err(LuaFault::OrderFunction.into());
                }
                sort.pending.push(waiting);
                SortStep::Range
            }
            SortStep::Range => SortStep::Range,
        };
        Ok(())
    }
}
