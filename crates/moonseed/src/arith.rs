//! Primitive arithmetic and bitwise operators: the only definition of what
//! Lua 5.4 computes for numbers without metamethods.
//!
//! Each operation either gives its value, or says the operands are not a
//! primitive case: `ops` then tries their metamethods and raises the fault
//! named here when there are none. Integer division or modulo by zero is an
//! error no metamethod can intercept, as in Lua.
//!
//! Strings are not numbers here: as in Lua 5.4, a string that reads as a
//! number takes part in arithmetic through the string metatable's
//! metamethods (ADR 0034), which convert it and come back here, and
//! bitwise operators do not accept strings. Integer arithmetic wraps. `//` and `%` round toward
//! negative infinity. `/` and `^` always give floats; `x ^ 2` is `x * x`,
//! as in PUC Lua's `luai_numpow`.

use crate::id::LuaFault;
use crate::opcode::ArithOp;
use crate::value::Value;

/// What primitive execution decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Prim {
    Value(Value),
    /// Not a primitive case. Try the operands' metamethods; with none,
    /// raise this fault.
    Meta(LuaFault),
}

/// `a <op> b`. `Err` is an error raised before any metamethod lookup.
pub(crate) fn binary(op: ArithOp, a: Value, b: Value) -> Result<Prim, LuaFault> {
    if op.is_bitwise() {
        return Ok(bitwise(op, a, b));
    }
    let (Some(x), Some(y)) = (arith_number(a), arith_number(b)) else {
        return Ok(Prim::Meta(LuaFault::Arith));
    };
    if matches!((x, y), (Value::Integer(_), Value::Integer(0))) {
        match op {
            ArithOp::Idiv => return Err(LuaFault::DivideByZero),
            ArithOp::Mod => return Err(LuaFault::ModuloByZero),
            _ => {}
        }
    }
    numbers(op, x, y).map_or(Ok(Prim::Meta(LuaFault::Arith)), |value| {
        Ok(Prim::Value(value))
    })
}

/// `x <op> y` for two numbers, without the heap, for the hot tier as well
/// as [`binary`]. `None` when an operand is not a number, a bitwise operand
/// has no integer value, or an integer is divided by zero; the caller then
/// goes through [`binary`], which raises the right fault.
#[inline]
pub(crate) fn numbers(op: ArithOp, x: Value, y: Value) -> Option<Value> {
    if op.is_bitwise() {
        return match bitwise(op, x, y) {
            Prim::Value(value) => Some(value),
            Prim::Meta(_) => None,
        };
    }
    Some(match (x, y) {
        (Value::Integer(x), Value::Integer(y)) => {
            if let Some(value) = int_op(op, x, y) {
                Value::Integer(value)
            } else {
                match op {
                    ArithOp::Idiv => Value::Integer(floor_div(x, y).ok()?),
                    ArithOp::Mod => Value::Integer(floor_mod(x, y).ok()?),
                    _ => Value::Float(float_op(op, x as f64, y as f64)),
                }
            }
        }
        // Match the subtypes once. Arithmetic converts an integer to float;
        // mixed comparison deliberately has different, exact rules.
        (Value::Integer(x), Value::Float(y)) => Value::Float(float_op(op, x as f64, y)),
        (Value::Float(x), Value::Integer(y)) => Value::Float(float_op(op, x, y as f64)),
        (Value::Float(x), Value::Float(y)) => Value::Float(float_op(op, x, y)),
        _ => return None,
    })
}

/// `-a`.
pub(crate) fn negate(a: Value) -> Prim {
    match arith_number(a) {
        Some(Value::Integer(integer)) => Prim::Value(Value::Integer(integer.wrapping_neg())),
        Some(Value::Float(float)) => Prim::Value(Value::Float(-float)),
        _ => Prim::Meta(LuaFault::Arith),
    }
}

/// `~a`.
pub(crate) fn bnot(a: Value) -> Prim {
    match to_bits(a) {
        Ok(integer) => Prim::Value(Value::Integer(!integer)),
        Err(fault) => Prim::Meta(fault),
    }
}

/// A number; anything else is `None`.
fn arith_number(value: Value) -> Option<Value> {
    match value {
        Value::Integer(_) | Value::Float(_) => Some(value),
        _ => None,
    }
}

/// Integer operations that need neither division checks nor float conversion.
#[inline(always)]
pub(crate) fn int_op(op: ArithOp, x: i64, y: i64) -> Option<i64> {
    Some(match op {
        ArithOp::Add => x.wrapping_add(y),
        ArithOp::Sub => x.wrapping_sub(y),
        ArithOp::Mul => x.wrapping_mul(y),
        ArithOp::Band => x & y,
        ArithOp::Bor => x | y,
        ArithOp::Bxor => x ^ y,
        _ => return None,
    })
}

#[inline(always)]
pub(crate) fn float_op(op: ArithOp, x: f64, y: f64) -> f64 {
    match op {
        ArithOp::Add => x + y,
        ArithOp::Sub => x - y,
        ArithOp::Mul => x * y,
        ArithOp::Div => x / y,
        ArithOp::Idiv => (x / y).floor(),
        // PUC's `luai_nummod`: `fmod`, then corrected when the remainder
        // and the divisor have opposite signs.
        ArithOp::Mod => {
            let m = x % y;
            if if m > 0.0 { y < 0.0 } else { m < 0.0 && y > 0.0 } {
                m + y
            } else {
                m
            }
        }
        ArithOp::Pow if y == 2.0 => x * x,
        // The portable `pow`, not the host's, so `^` gives the same bits
        // on every target (ADR 0032).
        ArithOp::Pow => libm::pow(x, y),
        // Bitwise operators never reach float arithmetic.
        _ => f64::NAN,
    }
}

/// Integer `//`, rounding toward negative infinity. `mininteger // -1`
/// wraps to `mininteger`.
pub(crate) fn floor_div(x: i64, y: i64) -> Result<i64, LuaFault> {
    if y == 0 {
        return Err(LuaFault::DivideByZero);
    }
    let quotient = x.wrapping_div(y);
    Ok(if x.wrapping_rem(y) != 0 && (x ^ y) < 0 {
        quotient - 1
    } else {
        quotient
    })
}

/// Integer `%`, with the sign of the divisor.
pub(crate) fn floor_mod(x: i64, y: i64) -> Result<i64, LuaFault> {
    if y == 0 {
        return Err(LuaFault::ModuloByZero);
    }
    let rest = x.wrapping_rem(y);
    Ok(if rest != 0 && (rest ^ y) < 0 {
        rest + y
    } else {
        rest
    })
}

fn bitwise(op: ArithOp, a: Value, b: Value) -> Prim {
    let (x, y) = match (to_bits(a), to_bits(b)) {
        (Ok(x), Ok(y)) => (x, y),
        // Two numbers, one of them not integral.
        (Err(LuaFault::NoInteger), Ok(_) | Err(LuaFault::NoInteger))
        | (Ok(_), Err(LuaFault::NoInteger)) => return Prim::Meta(LuaFault::NoInteger),
        _ => return Prim::Meta(LuaFault::Bitwise),
    };
    Prim::Value(Value::Integer(match op {
        ArithOp::Band => x & y,
        ArithOp::Bor => x | y,
        ArithOp::Bxor => x ^ y,
        ArithOp::Shl => shift_left(x, y),
        _ => shift_left(x, y.wrapping_neg()),
    }))
}

/// A bitwise operand: an integer, or a float with an exact integer value.
fn to_bits(value: Value) -> Result<i64, LuaFault> {
    match value {
        Value::Integer(integer) => Ok(integer),
        Value::Float(float) => crate::compare::float_to_int(float).ok_or(LuaFault::NoInteger),
        _ => Err(LuaFault::Bitwise),
    }
}

/// Lua's `luaV_shiftl`: a negative count shifts right, logically; a count
/// of 64 or more either way gives zero.
pub(crate) fn shift_left(x: i64, y: i64) -> i64 {
    if y <= -64 || y >= 64 {
        0
    } else if y < 0 {
        ((x as u64) >> (-y)) as i64
    } else {
        ((x as u64) << y) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(op: ArithOp, x: i64, y: i64) -> Result<Prim, LuaFault> {
        binary(op, Value::Integer(x), Value::Integer(y))
    }

    fn float(op: ArithOp, x: f64, y: f64) -> f64 {
        match binary(op, Value::Float(x), Value::Float(y)) {
            Ok(Prim::Value(Value::Float(result))) => result,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn integer_division_and_modulo_floor() {
        let pairs = [
            (7, 2, 3, 1),
            (-7, 2, -4, 1),
            (7, -2, -4, -1),
            (-7, -2, 3, -1),
        ];
        for (x, y, q, r) in pairs {
            assert_eq!(floor_div(x, y), Ok(q), "{x} // {y}");
            assert_eq!(floor_mod(x, y), Ok(r), "{x} % {y}");
        }
        assert_eq!(floor_div(i64::MIN, -1), Ok(i64::MIN));
        assert_eq!(floor_mod(i64::MIN, -1), Ok(0));
        assert_eq!(floor_div(1, 0), Err(LuaFault::DivideByZero));
        assert_eq!(floor_mod(1, 0), Err(LuaFault::ModuloByZero));
        assert_eq!(
            int(ArithOp::Add, i64::MAX, 1),
            Ok(Prim::Value(Value::Integer(i64::MIN)))
        );
        assert_eq!(int(ArithOp::Div, 4, 2), Ok(Prim::Value(Value::Float(2.0))));
    }

    #[test]
    fn float_division_modulo_and_power() {
        assert_eq!(float(ArithOp::Idiv, 7.0, 2.0), 3.0);
        assert_eq!(float(ArithOp::Idiv, 5.0, -0.0), f64::NEG_INFINITY);
        assert_eq!(float(ArithOp::Mod, -7.5, 2.0), 0.5);
        assert_eq!(float(ArithOp::Mod, -1.0, -2.5), -1.0);
        assert_eq!(float(ArithOp::Mod, 7.5, -2.0), -0.5);
        assert_eq!(float(ArithOp::Mod, -7.0, f64::INFINITY), f64::INFINITY);
        assert_eq!(
            float(ArithOp::Mod, 7.0, f64::NEG_INFINITY),
            f64::NEG_INFINITY
        );
        assert_eq!(float(ArithOp::Mod, -7.0, f64::NEG_INFINITY), -7.0);
        assert_eq!(float(ArithOp::Mod, 7.0, 2.5), 2.0);
        assert!(float(ArithOp::Mod, 1.0, 0.0).is_nan());
        assert_eq!(float(ArithOp::Pow, 3.0, 2.0), 9.0);
        assert_eq!(float(ArithOp::Pow, 2.0, 0.5), 2f64.sqrt());
    }

    #[test]
    fn shifts_are_defined_for_every_count() {
        assert_eq!(shift_left(1, 63), i64::MIN);
        assert_eq!(shift_left(1, 64), 0);
        assert_eq!(shift_left(2, -1), 1);
        assert_eq!(shift_left(-1, -1), i64::MAX);
        assert_eq!(shift_left(-1, -63), 1);
        assert_eq!(shift_left(-1, -64), 0);
        assert_eq!(shift_left(3, i64::MIN), 0);
        assert_eq!(shift_left(3, i64::MAX), 0);
        let shr = |x: i64, y: i64| match int(ArithOp::Shr, x, y) {
            Ok(Prim::Value(Value::Integer(result))) => result,
            other => panic!("{other:?}"),
        };
        assert_eq!(shr(-1, 1), i64::MAX);
        assert_eq!(shr(1, i64::MIN), 0);
    }

    #[test]
    fn bitwise_operands_are_integral_numbers() {
        let bits = |a: Value, b: Value| binary(ArithOp::Band, a, b);
        assert_eq!(
            bits(Value::Float(6.0), Value::Integer(3)),
            Ok(Prim::Value(Value::Integer(2)))
        );
        assert_eq!(
            bits(Value::Float(1.5), Value::Integer(1)),
            Ok(Prim::Meta(LuaFault::NoInteger))
        );
        assert_eq!(
            bits(Value::Float(2f64.powi(63)), Value::Integer(1)),
            Ok(Prim::Meta(LuaFault::NoInteger))
        );
        assert_eq!(
            bits(Value::Bool(true), Value::Integer(1)),
            Ok(Prim::Meta(LuaFault::Bitwise))
        );
        assert_eq!(bnot(Value::Integer(0)), Prim::Value(Value::Integer(-1)));
    }

    #[test]
    fn strings_are_not_numbers_here() {
        let mut heap = crate::heap::Heap::new();
        let forty = Value::String(heap.alloc_string(b" 0x28 ".to_vec()).unwrap());
        assert_eq!(
            binary(ArithOp::Add, forty, Value::Integer(2)),
            Ok(Prim::Meta(LuaFault::Arith))
        );
        assert_eq!(
            binary(ArithOp::Band, forty, Value::Integer(2)),
            Ok(Prim::Meta(LuaFault::Bitwise))
        );
        assert_eq!(negate(forty), Prim::Meta(LuaFault::Arith));
    }
}
