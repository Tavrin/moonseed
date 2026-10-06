//! Numeric `for`: preparation and one step over four registers.
//!
//! The loop keeps its progression in three hidden registers, and a fourth
//! holds the visible control variable:
//!
//! | Register | Integer loop | Float loop |
//! |---|---|---|
//! | `base` | index | index |
//! | `base + 1` | iterations left, as the bits of a `u64` | limit |
//! | `base + 2` | step (integer) | step (float) |
//! | `base + 3` | control variable | control variable |
//!
//! The step's subtype is the mode. Everything is an ordinary `Value`, so a
//! checkpoint needs nothing special. The body never reads the hidden
//! registers, and `advance` rewrites the control variable from the index,
//! so an assignment to it does not change the iteration.
//!
//! The rules follow Lua 5.4's `forprep` and `forloop`. An integer loop runs
//! a precomputed count, so it can never wrap.

use crate::heap::Heap;
use crate::id::LuaFault;
use crate::lex::string_to_number;
use crate::value::Value;

/// 2^63 as a float: the first float above every `i64`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Hidden and visible register values after a successful preparation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ForState {
    pub(crate) index: Value,
    pub(crate) limit: Value,
    pub(crate) step: Value,
    pub(crate) control: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Prep {
    /// The loop runs zero times.
    Skip,
    Run(ForState),
}

/// `for v = init, limit, step`. An integer loop when `init` and `step` are
/// integers, whatever the limit is; otherwise all three become floats.
/// Strings that read as numbers are accepted, and a string never counts as
/// an integer for the mode.
pub(crate) fn prepare(
    heap: &Heap,
    init: Value,
    limit: Value,
    step: Value,
) -> Result<Prep, LuaFault> {
    if let (Value::Integer(init), Value::Integer(step)) = (init, step) {
        if step == 0 {
            return Err(LuaFault::ForZeroStep);
        }
        let Some(limit) = integer_limit(heap, limit, step)? else {
            return Ok(Prep::Skip);
        };
        if (step > 0 && init > limit) || (step < 0 && init < limit) {
            return Ok(Prep::Skip);
        }
        let count = if step > 0 {
            (limit as u64).wrapping_sub(init as u64) / step as u64
        } else {
            // `-(step + 1) + 1` is `-step` without negating `i64::MIN`.
            (init as u64).wrapping_sub(limit as u64) / ((-(step + 1)) as u64 + 1)
        };
        return Ok(Prep::Run(ForState {
            index: Value::Integer(init),
            limit: Value::Integer(count as i64),
            step: Value::Integer(step),
            control: Value::Integer(init),
        }));
    }
    let limit = to_float(heap, limit).ok_or(LuaFault::ForValue)?;
    let step = to_float(heap, step).ok_or(LuaFault::ForValue)?;
    let init = to_float(heap, init).ok_or(LuaFault::ForValue)?;
    if step == 0.0 {
        return Err(LuaFault::ForZeroStep);
    }
    if if step > 0.0 {
        limit < init
    } else {
        init < limit
    } {
        return Ok(Prep::Skip);
    }
    Ok(Prep::Run(ForState {
        index: Value::Float(init),
        limit: Value::Float(limit),
        step: Value::Float(step),
        control: Value::Float(init),
    }))
}

/// The remaining count is unsigned, even though its register is an integer.
#[inline(always)]
pub(crate) fn advance_int(index: i64, count: i64, step: i64) -> Option<(i64, i64)> {
    let count = count as u64;
    if count == 0 {
        None
    } else {
        Some((index.wrapping_add(step), (count - 1) as i64))
    }
}

/// The next state, or `None` when the loop is over. `Err` means the hidden
/// registers do not hold a numeric-for state.
pub(crate) fn advance(index: Value, limit: Value, step: Value) -> Result<Option<ForState>, ()> {
    match (index, limit, step) {
        (Value::Integer(index), Value::Integer(count), Value::Integer(step)) => {
            let Some((index, count)) = advance_int(index, count, step) else {
                return Ok(None);
            };
            let next = Value::Integer(index);
            Ok(Some(ForState {
                index: next,
                limit: Value::Integer(count),
                step: Value::Integer(step),
                control: next,
            }))
        }
        (Value::Float(index), Value::Float(limit), Value::Float(step)) => {
            let Some(next) = advance_float(index, limit, step) else {
                return Ok(None);
            };
            Ok(Some(ForState {
                index: Value::Float(next),
                limit: Value::Float(limit),
                step: Value::Float(step),
                control: Value::Float(next),
            }))
        }
        _ => Err(()),
    }
}

/// Float-loop step, shared with the hot helper without rebuilding its state.
/// Keep the direction test and operand order for NaN and signed zero.
#[inline(always)]
pub(crate) fn advance_float(index: f64, limit: f64, step: f64) -> Option<f64> {
    let next = index + step;
    let more = if step > 0.0 {
        next <= limit
    } else {
        limit <= next
    };
    more.then_some(next)
}

/// The integer limit of an integer loop, or `None` to skip the loop. A
/// float limit is floored for a positive step and ceiled for a negative
/// one. One outside the `i64` range clamps to the end the loop runs toward,
/// or skips the loop when it lies behind the start. NaN is treated as a
/// negative out-of-range limit, as in PUC Lua.
fn integer_limit(heap: &Heap, limit: Value, step: i64) -> Result<Option<i64>, LuaFault> {
    let limit = match limit {
        Value::String(handle) => heap
            .string_bytes(handle)
            .and_then(string_to_number)
            .ok_or(LuaFault::ForValue)?,
        other => other,
    };
    let float = match limit {
        Value::Integer(integer) => return Ok(Some(integer)),
        Value::Float(float) => float,
        _ => return Err(LuaFault::ForValue),
    };
    let rounded = if step < 0 {
        float.ceil()
    } else {
        float.floor()
    };
    if (-TWO_POW_63..TWO_POW_63).contains(&rounded) {
        return Ok(Some(rounded as i64));
    }
    Ok(if 0.0 < float {
        (step > 0).then_some(i64::MAX)
    } else {
        (step < 0).then_some(i64::MIN)
    })
}

fn to_float(heap: &Heap, value: Value) -> Option<f64> {
    match value {
        Value::Integer(integer) => Some(integer as f64),
        Value::Float(float) => Some(float),
        Value::String(handle) => match string_to_number(heap.string_bytes(handle)?)? {
            Value::Integer(integer) => Some(integer as f64),
            Value::Float(float) => Some(float),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Value::{Float as F, Integer as I};

    fn run(init: Value, limit: Value, step: Value) -> ForState {
        match prepare(&Heap::new(), init, limit, step).unwrap() {
            Prep::Run(state) => state,
            Prep::Skip => panic!("skipped"),
        }
    }

    fn skipped(init: Value, limit: Value, step: Value) -> bool {
        prepare(&Heap::new(), init, limit, step).unwrap() == Prep::Skip
    }

    /// Every control value the loop produces, up to `cap` of them.
    fn walk(init: Value, limit: Value, step: Value, cap: usize) -> Vec<Value> {
        let Prep::Run(mut state) = prepare(&Heap::new(), init, limit, step).unwrap() else {
            return Vec::new();
        };
        let mut seen = vec![state.control];
        while seen.len() < cap {
            match advance(state.index, state.limit, state.step).unwrap() {
                Some(next) => {
                    seen.push(next.control);
                    state = next;
                }
                None => break,
            }
        }
        seen
    }

    #[test]
    fn integer_counts_never_wrap() {
        assert_eq!(walk(I(i64::MAX - 2), I(i64::MAX), I(1), 10).len(), 3);
        assert_eq!(walk(I(i64::MIN + 2), I(i64::MIN), I(-1), 10).len(), 3);
        assert_eq!(run(I(0), I(-1), I(i64::MIN)).limit, I(0));
        assert_eq!(
            walk(I(0), I(i64::MIN), I(i64::MIN), 10),
            vec![I(0), I(i64::MIN)]
        );
        assert_eq!(
            run(I(i64::MIN), I(i64::MAX), I(1)).limit,
            I(-1),
            "u64::MAX left"
        );
        assert_eq!(
            walk(I(1), I(i64::MAX), I(1 << 62), 10),
            vec![I(1), I((1 << 62) + 1)]
        );
    }

    #[test]
    fn integer_mode_rounds_and_clamps_the_limit() {
        assert_eq!(walk(I(1), F(3.9), I(1), 10), vec![I(1), I(2), I(3)]);
        assert_eq!(walk(I(3), F(0.5), I(-1), 10), vec![I(3), I(2), I(1)]);
        assert!(skipped(I(1), F(-1e300), I(1)));
        assert!(skipped(I(1), F(1e300), I(-1)));
        assert_eq!(run(I(1), F(1e300), I(1)).limit, I(i64::MAX - 1));
        assert_eq!(run(I(-1), F(-1e300), I(-1)).limit, I(i64::MAX));
        assert!(skipped(I(1), F(f64::NAN), I(1)));
        assert_eq!(run(I(-1), F(f64::NAN), I(-1)).limit, I(i64::MAX));
        assert!(matches!(run(I(1), F(3.0), I(1)).control, I(1)));
    }

    #[test]
    fn float_mode_and_zero_steps() {
        assert_eq!(walk(F(1.0), I(3), I(1), 10), vec![F(1.0), F(2.0), F(3.0)]);
        assert_eq!(walk(I(1), I(3), F(1.0), 10), vec![F(1.0), F(2.0), F(3.0)]);
        assert_eq!(walk(F(1.0), F(f64::NAN), I(1), 10), vec![F(1.0)]);
        assert!(skipped(I(1), I(10), F(f64::NAN)));
        assert_eq!(walk(I(10), I(1), F(f64::NAN), 10), vec![F(10.0)]);
        for step in [I(0), F(0.0), F(-0.0)] {
            let init = if matches!(step, I(_)) { I(1) } else { F(1.0) };
            assert_eq!(
                prepare(&Heap::new(), init, I(9), step),
                Err(LuaFault::ForZeroStep)
            );
        }
        assert_eq!(
            prepare(&Heap::new(), I(1), Value::Nil, I(1)),
            Err(LuaFault::ForValue)
        );
        assert_eq!(advance(I(1), F(2.0), I(1)), Err(()));
    }
}
