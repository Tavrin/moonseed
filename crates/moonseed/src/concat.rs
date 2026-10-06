//! Primitive `..` and the text of a number.
//!
//! Lua converts numbers to strings for concatenation but leaves the format
//! unspecified. Moonseed uses PUC Lua's: integers as `%d`, floats as
//! `%.14g`, and a float whose text would read as an integer gets `.0`
//! (`3.0`, `-0.0`, `1e+100` stays as it is). The digits come from
//! `string.format`'s formatter (ADR 0034), not the C library, so no locale
//! and no target affects them. Every NaN is `nan`; PUC Lua on glibc prints the
//! sign of the NaN (`-nan`), which depends on how the target produced it.

use std::borrow::Cow;

use crate::heap::{Heap, cost};
use crate::value::Value;

/// Why a `..` result is not made.
pub(crate) enum Refused {
    /// Longer than the runtime's string limit (`Heap::max_string`).
    TooLong,
    /// Its string would not fit under the logical-heap quota now: a
    /// collection may make room. Carries the result's length.
    NoRoom(usize),
}

/// The bytes of `a .. b`, or `None` when an operand is not a string or a
/// number (then `__concat` is tried). A result past the string limit or
/// the quota is found before anything is copied.
pub(crate) fn concat(heap: &Heap, a: Value, b: Value) -> Option<Result<Vec<u8>, Refused>> {
    let (a, b) = (piece(heap, a)?, piece(heap, b)?);
    let len = a.len().saturating_add(b.len());
    if len > heap.max_string {
        return Some(Err(Refused::TooLong));
    }
    if !heap.gc.fits(cost::OBJECT + len as u64) {
        return Some(Err(Refused::NoRoom(len)));
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&a);
    out.extend_from_slice(&b);
    Some(Ok(out))
}

fn piece(heap: &Heap, value: Value) -> Option<Cow<'_, [u8]>> {
    match value {
        Value::String(handle) => heap.string_bytes(handle).map(Cow::Borrowed),
        Value::Integer(_) | Value::Float(_) => {
            number_text(value).map(|text| Cow::Owned(text.into_bytes()))
        }
        _ => None,
    }
}

/// The text of a number, as `..` produces it.
pub(crate) fn number_text(value: Value) -> Option<String> {
    Some(match value {
        Value::Integer(integer) => integer.to_string(),
        Value::Float(float) => float_text(float),
        _ => return None,
    })
}

fn float_text(float: f64) -> String {
    if float.is_nan() {
        return "nan".to_string();
    }
    if float.is_infinite() {
        return if float > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut text = general_14(float);
    if text
        .bytes()
        .all(|byte| byte == b'-' || byte.is_ascii_digit())
    {
        text.push_str(".0");
    }
    text
}

/// C's `%.14g` for a finite float, from `string.format`'s formatter, the
/// one number formatter (ADR 0034).
fn general_14(float: f64) -> String {
    let mut spec = crate::strformat::Spec::new(b'g');
    spec.precision = Some(14);
    let mut out = Vec::new();
    crate::strformat::format_float(&spec, float, &mut out);
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_like_puc_lua() {
        let cases = [
            (1.5, "1.5"),
            (3.0, "3.0"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (0.1, "0.1"),
            (2f64.powi(53), "9.007199254741e+15"),
            (1e100, "1e+100"),
            (1e15, "1e+15"),
            (1e14, "1e+14"),
            (123_456_789_012_345.0, "1.2345678901234e+14"),
            (12_345_678_901_234.0, "12345678901234.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (2f64.sqrt(), "1.4142135623731"),
            (-1.25e-300, "-1.25e-300"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (-f64::NAN, "nan"),
            (1.0 / 3.0, "0.33333333333333"),
            (99_999_999_999_999.5, "1e+14"),
        ];
        for (float, text) in cases {
            assert_eq!(float_text(float), text, "{float:e}");
        }
    }

    #[test]
    fn integers_print_in_full() {
        assert_eq!(
            number_text(Value::Integer(i64::MIN)).unwrap(),
            "-9223372036854775808"
        );
        assert_eq!(number_text(Value::Integer(12)).unwrap(), "12");
    }
}
