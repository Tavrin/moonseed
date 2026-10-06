//! Lua's base library (ADR 0024, ADR 0031). The functions that work on
//! metatables, raw access, and arguments (`setmetatable`, `getmetatable`,
//! `rawget`, `rawset`, `rawlen`, `rawequal`, `select`) are ordinary native
//! functions here. The rest are implemented by the VM, in
//! `runtime/builtins.rs`, because they raise errors, call Lua, make
//! strings, or act on the collector or the output.
//!
//! A table has its own metatable; other types share one per type
//! (ADR 0034), and only the string library sets one, for strings.
//! `setmetatable` takes tables only, as Lua's does.

use crate::host::{Builtin, HostRegistry, NativeCall, NativeOutcome, NativePolicy, NativeValue};
use crate::table::KeyView;
use crate::value::Value;

/// The global names and registry symbols of the base functions. With
/// `_G` and `_VERSION`, they are what `Runtime::install_base` binds. Lua
/// 5.4's `loadfile` and `dofile` use explicit host capabilities; `require`,
/// which belongs to the package library.
#[doc(hidden)] // Builtin symbol inventory; use register_base or Libraries::BASE.
pub const BASE_FUNCTIONS: [(&str, &str); 23] = [
    ("assert", "base.assert"),
    ("collectgarbage", "base.collectgarbage"),
    ("error", "base.error"),
    ("getmetatable", "base.getmetatable"),
    ("ipairs", "base.ipairs"),
    ("load", "base.load"),
    ("loadfile", "base.loadfile"),
    ("dofile", "base.dofile"),
    ("next", "base.next"),
    ("pairs", "base.pairs"),
    ("pcall", "base.pcall"),
    ("print", "base.print"),
    ("rawequal", "base.rawequal"),
    ("rawget", "base.rawget"),
    ("rawlen", "base.rawlen"),
    ("rawset", "base.rawset"),
    ("select", "base.select"),
    ("setmetatable", "base.setmetatable"),
    ("tonumber", "base.tonumber"),
    ("tostring", "base.tostring"),
    ("type", "base.type"),
    ("warn", "base.warn"),
    ("xpcall", "base.xpcall"),
];

/// The symbol of the iterator `ipairs` returns. It is not a global.
pub(crate) const IPAIRS_NEXT: &str = "base.ipairs_next";

/// Register the base functions under their `base.*` symbols, and the
/// iterator `ipairs` returns.
pub fn register_base(registry: &mut HostRegistry) {
    registry.register_native("base.setmetatable", NativePolicy::VmLocal, setmetatable);
    registry.register_native("base.getmetatable", NativePolicy::VmLocal, getmetatable);
    registry.register_native("base.rawget", NativePolicy::VmLocal, rawget);
    registry.register_native("base.rawset", NativePolicy::VmLocal, rawset);
    registry.register_native("base.rawlen", NativePolicy::VmLocal, rawlen);
    registry.register_native("base.rawequal", NativePolicy::VmLocal, rawequal);
    registry.register_native("base.select", NativePolicy::VmLocal, select);
    for (symbol, builtin) in [
        ("base.error", Builtin::Error),
        ("base.pcall", Builtin::Pcall),
        ("base.xpcall", Builtin::Xpcall),
        ("base.assert", Builtin::Assert),
        ("base.type", Builtin::Type),
        ("base.tostring", Builtin::ToString),
        ("base.tonumber", Builtin::ToNumber),
        ("base.print", Builtin::Print),
        ("base.next", Builtin::Next),
        ("base.pairs", Builtin::Pairs),
        ("base.ipairs", Builtin::Ipairs),
        (IPAIRS_NEXT, Builtin::IpairsNext),
        ("base.collectgarbage", Builtin::CollectGarbage),
        ("base.load", Builtin::Load),
        ("base.loadfile", Builtin::LoadFile),
        ("base.dofile", Builtin::DoFile),
        ("base.warn", Builtin::Warn),
    ] {
        registry.register_builtin(symbol, builtin);
    }
}

/// The raw `__metatable` field of a value's metatable, if both exist.
fn protection<'a>(call: &NativeCall<'a>, table: NativeValue<'a>) -> Option<NativeValue<'a>> {
    let Value::Table(metatable) = call.metatable(table)?.value else {
        return None;
    };
    let field = call
        .heap
        .tables
        .get(metatable)?
        .table
        .get_view(KeyView::string(b"__metatable"))?;
    Some(NativeValue::wrap(field))
}

/// `setmetatable(t, mt)`: `mt` a table or nil. Refused when the current
/// metatable has a `__metatable` field. Returns `t`.
fn setmetatable(call: &mut NativeCall<'_>) -> NativeOutcome {
    let table = call.arg(0);
    let metatable = call.arg(1);
    if !matches!(table.value, Value::Table(_)) {
        return call.type_error(0, "table");
    }
    if call.arg_count() < 2 {
        return call.type_error(1, "nil or table");
    }
    let metatable = match metatable.value {
        Value::Nil => None,
        Value::Table(_) => Some(metatable),
        _ => return call.type_error(1, "nil or table"),
    };
    if protection(call, table).is_some() {
        return NativeOutcome::Fault;
    }
    if !call.set_metatable(table, metatable) {
        return NativeOutcome::Fault;
    }
    call.push(table);
    NativeOutcome::Ready
}

/// `getmetatable(v)`: the `__metatable` field if the metatable has one,
/// else the metatable, else nil. A string has the string metatable once
/// the string library is installed (ADR 0034).
fn getmetatable(call: &mut NativeCall<'_>) -> NativeOutcome {
    let value = call.arg(0);
    match protection(call, value).or_else(|| call.metatable(value)) {
        Some(result) => call.push(result),
        None => call.push_nil(),
    }
    NativeOutcome::Ready
}

fn rawget(call: &mut NativeCall<'_>) -> NativeOutcome {
    if !matches!(call.arg(0).value, Value::Table(_)) {
        return call.type_error(0, "table");
    }
    if call.arg_count() < 2 {
        return call.arg_error(1, "value expected");
    }
    match call.raw_get(call.arg(0), call.arg(1)) {
        Some(value) => {
            call.push(value);
            NativeOutcome::Ready
        }
        None => NativeOutcome::Fault,
    }
}

/// `rawset(t, k, v)`: returns `t`.
fn rawset(call: &mut NativeCall<'_>) -> NativeOutcome {
    let table = call.arg(0);
    if !matches!(table.value, Value::Table(_)) {
        return call.type_error(0, "table");
    }
    if call.arg_count() < 3 {
        return call.arg_error(call.arg_count().max(1), "value expected");
    }
    match call.raw_set(table, call.arg(1), call.arg(2)) {
        Ok(()) => {
            call.push(table);
            NativeOutcome::Ready
        }
        Err(_) => match call.arg(1).value {
            Value::Nil => call.raw_error("table index is nil"),
            Value::Float(value) if value.is_nan() => call.raw_error("table index is NaN"),
            _ => NativeOutcome::Fault,
        },
    }
}

/// `rawequal(a, b)`: `==` without `__eq`. Both arguments are required.
fn rawequal(call: &mut NativeCall<'_>) -> NativeOutcome {
    if call.arg_count() < 2 {
        return NativeOutcome::Fault;
    }
    let equal = call.raw_equal(call.arg(0), call.arg(1));
    call.push_boolean(equal);
    NativeOutcome::Ready
}

fn rawlen(call: &mut NativeCall<'_>) -> NativeOutcome {
    match call.raw_len(call.arg(0)) {
        Some(length) => {
            call.push_integer(length);
            NativeOutcome::Ready
        }
        None => call.type_error(0, "table or string"),
    }
}

/// `select(n, ...)`: the arguments after the `n`th, counting from the end
/// for a negative `n`; `select('#', ...)`, or any string starting with `#`,
/// their count. `n` is an integer, a float with an integer value, or a
/// string that reads as one, as Lua's `luaL_checkinteger` takes. Zero, and
/// a negative `n` reaching before the first, are errors.
fn select(call: &mut NativeCall<'_>) -> NativeOutcome {
    let count = call.arg_count().saturating_sub(1);
    let first = call.arg(0);
    if call
        .string_bytes(first)
        .is_some_and(|bytes| bytes.first() == Some(&b'#'))
    {
        call.push_integer(count as i64);
        return NativeOutcome::Ready;
    }
    let Some(index) = lua_integer(call.heap, first.value) else {
        let numeric = match first.value {
            Value::Integer(_) | Value::Float(_) => true,
            Value::String(handle) => call
                .heap
                .string_bytes(handle)
                .and_then(crate::lex::string_to_number)
                .is_some(),
            _ => false,
        };
        return if numeric {
            call.arg_error(0, "number has no integer representation")
        } else {
            call.type_error(0, "number")
        };
    };
    // Lua counts `n` itself among the arguments: `select(-1, a, b)` is `b`.
    let total = count as i64 + 1;
    let start = if index < 0 {
        total + index
    } else {
        index.min(total)
    };
    if start < 1 {
        return call.arg_error(0, "index out of range");
    }
    for arg in start as usize..=count {
        call.push_arg(arg);
    }
    NativeOutcome::Ready
}

/// An integer argument as Lua's `luaL_checkinteger` takes one: an integer,
/// a float with an integer value, or a string that reads as either.
pub(crate) fn lua_integer(heap: &crate::heap::Heap, value: Value) -> Option<i64> {
    let number = match value {
        Value::String(handle) => heap
            .string_bytes(handle)
            .and_then(crate::lex::string_to_number),
        other => Some(other),
    };
    match number {
        Some(Value::Integer(integer)) => Some(integer),
        Some(Value::Float(float)) => crate::compare::float_to_int(float),
        _ => None,
    }
}
