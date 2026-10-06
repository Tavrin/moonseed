//! Primitive Lua comparison: the only definition of raw `==`, and of `<`
//! and `<=` on two numbers or two strings. `ops::compare` adds `__eq`,
//! `__lt`, and `__le` where these give no primitive answer.
//!
//! Mixed integer/float comparisons are exact. An integer is never converted
//! to `f64` to compare it, because above 2^53 that conversion rounds. The
//! float is converted to an integer bound instead, with its floor or
//! ceiling, as Lua 5.4 does.

use crate::heap::Heap;
use crate::id::LuaFault;
use crate::opcode::CmpKind;
use crate::value::Value;

/// 2^63 as a float: the first float above every `i64`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Two numbers, without the heap, for the hot dispatch tier. `None` means
/// the operands are not two numbers, and `ops::compare` decides. It uses the
/// same number functions as [`less_than`] and [`equal`], so both tiers give
/// the same answer.
#[inline(always)]
pub(crate) fn numbers_only(kind: CmpKind, a: Value, b: Value) -> Option<bool> {
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => return Some(int_op(kind, x, y)),
        (Value::Float(x), Value::Float(y)) => return Some(float_op(kind, x, y)),
        _ => {}
    }
    match kind {
        CmpKind::Eq => number_equal(a, b),
        CmpKind::Ne => number_equal(a, b).map(|bit| !bit),
        CmpKind::Lt => numbers(a, b, int_lt_float, float_lt_int, |x, y| x < y, |x, y| x < y),
        CmpKind::Le => numbers(
            a,
            b,
            int_le_float,
            float_le_int,
            |x, y| x <= y,
            |x, y| x <= y,
        ),
    }
}

/// Same-type numeric comparisons, shared with the hot core.
#[inline(always)]
pub(crate) fn int_op(kind: CmpKind, x: i64, y: i64) -> bool {
    match kind {
        CmpKind::Eq => x == y,
        CmpKind::Ne => x != y,
        CmpKind::Lt => x < y,
        CmpKind::Le => x <= y,
    }
}

#[inline(always)]
pub(crate) fn float_op(kind: CmpKind, x: f64, y: f64) -> bool {
    match kind {
        CmpKind::Eq => x == y,
        CmpKind::Ne => x != y,
        CmpKind::Lt => x < y,
        CmpKind::Le => x <= y,
    }
}

fn number_equal(a: Value, b: Value) -> Option<bool> {
    Some(match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Integer(i), Value::Float(f)) | (Value::Float(f), Value::Integer(i)) => {
            float_to_int(f).is_some_and(|exact| exact == i)
        }
        _ => return None,
    })
}

/// Raw equality: numbers by value, strings by bytes, everything else by
/// identity.
pub(crate) fn equal(heap: &Heap, a: Value, b: Value) -> bool {
    if let Some(bit) = number_equal(a, b) {
        return bit;
    }
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => {
            x == y
                || heap
                    .string_bytes(x)
                    .is_some_and(|x| heap.string_bytes(y) == Some(x))
        }
        (Value::Table(x), Value::Table(y)) => x == y,
        (Value::Closure(x), Value::Closure(y)) => x == y,
        (Value::Thread(x), Value::Thread(y)) => x == y,
        (Value::Native(x), Value::Native(y)) => x == y,
        (Value::NativeClosure(x), Value::NativeClosure(y)) => x == y,
        (Value::Userdata(x), Value::Userdata(y)) => x == y,
        (Value::LightUserdata(d, x), Value::LightUserdata(e, y)) => d == e && x == y,
        _ => false,
    }
}

/// `a < b`. Numbers and strings only; strings order by bytes.
pub(crate) fn less_than(heap: &Heap, a: Value, b: Value) -> Result<bool, LuaFault> {
    if let Some(ordered) = numbers(a, b, int_lt_float, float_lt_int, |x, y| x < y, |x, y| x < y) {
        return Ok(ordered);
    }
    strings(heap, a, b).map(|(x, y)| x < y)
}

/// `a <= b`. Numbers and strings only; strings order by bytes.
pub(crate) fn less_equal(heap: &Heap, a: Value, b: Value) -> Result<bool, LuaFault> {
    if let Some(ordered) = numbers(
        a,
        b,
        int_le_float,
        float_le_int,
        |x, y| x <= y,
        |x, y| x <= y,
    ) {
        return Ok(ordered);
    }
    strings(heap, a, b).map(|(x, y)| x <= y)
}

fn numbers(
    a: Value,
    b: Value,
    int_float: fn(i64, f64) -> bool,
    float_int: fn(f64, i64) -> bool,
    ints: fn(i64, i64) -> bool,
    floats: fn(f64, f64) -> bool,
) -> Option<bool> {
    Some(match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => ints(x, y),
        (Value::Float(x), Value::Float(y)) => floats(x, y),
        (Value::Integer(x), Value::Float(y)) => int_float(x, y),
        (Value::Float(x), Value::Integer(y)) => float_int(x, y),
        _ => return None,
    })
}

fn strings(heap: &Heap, a: Value, b: Value) -> Result<(&[u8], &[u8]), LuaFault> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Ok((
            heap.string_bytes(x).ok_or(LuaFault::Compare)?,
            heap.string_bytes(y).ok_or(LuaFault::Compare)?,
        )),
        _ => Err(LuaFault::Compare),
    }
}

/// The integer equal to `f`, if there is one.
pub(crate) fn float_to_int(f: f64) -> Option<i64> {
    (f.floor() == f && (-TWO_POW_63..TWO_POW_63).contains(&f)).then_some(f as i64)
}

/// `i < f` is `i < ceil(f)`, with the ceiling clamped to the `i64` range.
fn int_lt_float(i: i64, f: f64) -> bool {
    if f.is_nan() {
        return false;
    }
    let bound = f.ceil();
    if bound >= TWO_POW_63 {
        true
    } else if bound < -TWO_POW_63 {
        false
    } else {
        i < bound as i64
    }
}

/// `i <= f` is `i <= floor(f)`.
fn int_le_float(i: i64, f: f64) -> bool {
    if f.is_nan() {
        return false;
    }
    let bound = f.floor();
    if bound >= TWO_POW_63 {
        true
    } else if bound < -TWO_POW_63 {
        false
    } else {
        i <= bound as i64
    }
}

/// `f < i` is `floor(f) < i`.
fn float_lt_int(f: f64, i: i64) -> bool {
    if f.is_nan() {
        return false;
    }
    let bound = f.floor();
    if bound >= TWO_POW_63 {
        false
    } else if bound < -TWO_POW_63 {
        true
    } else {
        (bound as i64) < i
    }
}

/// `f <= i` is `ceil(f) <= i`.
fn float_le_int(f: f64, i: i64) -> bool {
    if f.is_nan() {
        return false;
    }
    let bound = f.ceil();
    if bound >= TWO_POW_63 {
        false
    } else if bound < -TWO_POW_63 {
        true
    } else {
        (bound as i64) <= i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: i64 = i64::MAX;
    const MIN: i64 = i64::MIN;
    const P53: i64 = 1 << 53;

    fn lt(a: Value, b: Value) -> bool {
        less_than(&Heap::new(), a, b).unwrap()
    }

    fn le(a: Value, b: Value) -> bool {
        less_equal(&Heap::new(), a, b).unwrap()
    }

    fn eq(a: Value, b: Value) -> bool {
        equal(&Heap::new(), a, b)
    }

    use Value::{Float as F, Integer as I};

    #[test]
    fn mixed_numbers_are_exact() {
        assert!(eq(I(1), F(1.0)));
        assert!(eq(F(-0.0), I(0)));
        assert!(!eq(I(P53 + 1), F(P53 as f64)));
        assert!(eq(I(P53), F(P53 as f64)));
        assert!(lt(F(P53 as f64), I(P53 + 1)));
        assert!(!lt(I(P53 + 1), F(P53 as f64)));
        assert!(le(I(P53), F(P53 as f64)));
        assert!(!eq(I(MAX), F(TWO_POW_63)));
        assert!(lt(I(MAX), F(TWO_POW_63)));
        assert!(!le(F(TWO_POW_63), I(MAX)));
        assert!(eq(I(MIN), F(-TWO_POW_63)));
        assert!(le(I(MIN), F(-TWO_POW_63)));
        assert!(!lt(I(MIN), F(-TWO_POW_63)));
        assert!(lt(F(-1e300), I(MIN)));
        assert!(lt(I(MAX), F(f64::INFINITY)));
        assert!(lt(F(f64::NEG_INFINITY), I(MIN)));
        assert!(lt(I(2), F(2.5)) && !lt(F(2.5), I(2)) && le(F(2.5), I(3)));
        assert!(lt(F(-2.5), I(-2)) && le(I(-3), F(-2.5)) && !le(I(-2), F(-2.5)));
    }

    #[test]
    fn nan_is_unordered_and_unequal() {
        let nan = F(f64::NAN);
        assert!(!eq(nan, nan));
        for other in [nan, I(0), F(0.0), I(MAX)] {
            assert!(!lt(nan, other) && !le(nan, other));
            assert!(!lt(other, nan) && !le(other, nan));
        }
    }

    #[test]
    fn identity_types_and_faults() {
        let mut heap = Heap::new();
        let a = heap.alloc_string(b"a\0b".to_vec()).unwrap();
        let b = heap.alloc_string(b"a\0b".to_vec()).unwrap();
        let c = heap.alloc_string(b"a\0c".to_vec()).unwrap();
        assert!(equal(&heap, Value::String(a), Value::String(b)));
        assert!(!equal(&heap, Value::String(a), Value::String(c)));
        assert!(less_than(&heap, Value::String(a), Value::String(c)).unwrap());
        let t = heap.alloc_table().unwrap();
        let u = heap.alloc_table().unwrap();
        assert!(equal(&heap, Value::Table(t), Value::Table(t)));
        assert!(!equal(&heap, Value::Table(t), Value::Table(u)));
        assert!(!equal(&heap, Value::Nil, Value::Bool(false)));
        assert!(!equal(&heap, I(1), Value::String(a)));
        assert_eq!(
            less_than(&heap, Value::Table(t), Value::Table(u)),
            Err(LuaFault::Compare)
        );
        assert_eq!(
            less_equal(&heap, I(1), Value::String(a)),
            Err(LuaFault::Compare)
        );
        assert_eq!(
            less_than(&heap, Value::Bool(false), Value::Bool(true)),
            Err(LuaFault::Compare)
        );
    }
}
