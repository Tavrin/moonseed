//! Language-level operators: the primitive result, else the metamethod Lua
//! 5.4 selects. The VM calls a selected metamethod through `call_meta`, as
//! an ordinary call, with the original operands (ADR 0023).
//!
//! Selection:
//! - Arithmetic, bitwise, and `..`: the first operand's metamethod for the
//!   event, else the second's. None is the fault `arith` or `concat` named.
//! - Unary `-` and `~`: the operand's `__unm` / `__bnot`, called with the
//!   operand twice.
//! - `==`: raw equality; two distinct tables, or two distinct full
//!   userdata, then try the first's `__eq`, then the second's; none is
//!   `false`. The two need not share a handler. Light userdata never
//!   call `__eq`.
//! - `<` and `<=`: two numbers or two strings compare primitively; anything
//!   else tries `__lt` / `__le` on the first operand, then the second. None
//!   is `LuaFault::Compare`. `<=` never falls back to `__lt`: that is Lua
//!   5.3's `LUA_COMPAT_LT_LE`, not Lua 5.4.
//!
//! A table and a full userdata have their own metatables and any other
//! value its type's (ADR 0034, ADR 0042): a string operand supplies the string metatable's
//! arithmetic metamethods, which convert it to a number.

use crate::arith::{self, Prim};
use crate::compare;
use crate::heap::Heap;
use crate::id::LuaFault;
use crate::index::metamethod;
use crate::opcode::{ArithOp, CmpKind};
use crate::value::Value;

/// A value, or a metamethod to call with the operands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step {
    Done(Value),
    Call(Value),
}

/// A comparison's primitive truth, or a metamethod whose first result's
/// truth is the answer. For `~=` the caller negates either.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Truth {
    Done(bool),
    Call(Value),
}

/// The bytes of a primitive `..`, or `__concat` to call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Joined {
    Bytes(Vec<u8>),
    /// Longer than the string limit: a memory error.
    TooLong,
    /// Not under the quota now (see `concat::Refused::NoRoom`).
    NoRoom(usize),
    Call(Value),
}

/// `a <op> b` for every binary arithmetic and bitwise operator.
pub(crate) fn arith(heap: &Heap, op: ArithOp, a: Value, b: Value) -> Result<Step, LuaFault> {
    match arith::binary(op, a, b)? {
        Prim::Value(value) => Ok(Step::Done(value)),
        Prim::Meta(fault) => either(heap, a, b, op.event()).map(Step::Call).ok_or(fault),
    }
}

/// `-a`. `__unm` is called with `a, a`.
pub(crate) fn negate(heap: &Heap, a: Value) -> Result<Step, LuaFault> {
    unary(heap, arith::negate(a), a, b"__unm")
}

/// `~a`. `__bnot` is called with `a, a`.
pub(crate) fn bnot(heap: &Heap, a: Value) -> Result<Step, LuaFault> {
    unary(heap, arith::bnot(a), a, b"__bnot")
}

fn unary(heap: &Heap, primitive: Prim, a: Value, event: &[u8]) -> Result<Step, LuaFault> {
    match primitive {
        Prim::Value(value) => Ok(Step::Done(value)),
        Prim::Meta(fault) => metamethod(heap, a, event).map(Step::Call).ok_or(fault),
    }
}

/// `a .. b`.
pub(crate) fn concat(heap: &Heap, a: Value, b: Value) -> Result<Joined, LuaFault> {
    match crate::concat::concat(heap, a, b) {
        Some(Ok(bytes)) => return Ok(Joined::Bytes(bytes)),
        Some(Err(crate::concat::Refused::TooLong)) => return Ok(Joined::TooLong),
        Some(Err(crate::concat::Refused::NoRoom(len))) => return Ok(Joined::NoRoom(len)),
        None => {}
    }
    either(heap, a, b, b"__concat")
        .map(Joined::Call)
        .ok_or(LuaFault::Concat)
}

/// `a <kind> b`, before `~=` is negated.
pub(crate) fn compare(heap: &Heap, kind: CmpKind, a: Value, b: Value) -> Result<Truth, LuaFault> {
    let (primitive, event) = match kind {
        CmpKind::Eq | CmpKind::Ne => {
            if compare::equal(heap, a, b) {
                return Ok(Truth::Done(true));
            }
            // Two distinct tables or two distinct full userdata; light
            // userdata compare by token alone, as in Lua.
            if !matches!(
                (a, b),
                (Value::Table(_), Value::Table(_)) | (Value::Userdata(_), Value::Userdata(_))
            ) {
                return Ok(Truth::Done(false));
            }
            return Ok(either(heap, a, b, b"__eq").map_or(Truth::Done(false), Truth::Call));
        }
        CmpKind::Lt => (compare::less_than(heap, a, b), b"__lt"),
        CmpKind::Le => (compare::less_equal(heap, a, b), b"__le"),
    };
    match primitive {
        Ok(bit) => Ok(Truth::Done(bit)),
        Err(fault) => either(heap, a, b, event).map(Truth::Call).ok_or(fault),
    }
}

/// The first operand's metamethod for `event`, else the second's.
fn either(heap: &Heap, a: Value, b: Value, event: &[u8]) -> Option<Value> {
    metamethod(heap, a, event).or_else(|| metamethod(heap, b, event))
}
