//! Reject impossible prototypes before they are installed in a runtime.
//!
//! Compiler output and every prototype restored from a snapshot pass
//! through [`check_code`] and [`check_captures`]. Hand-built proof programs
//! are not checked when booted, but their snapshots are on restore.

use crate::error::{CompileError, CompileErrorKind};
use crate::limits::{MAX_CONSTS, MAX_FUNC_NEST, MAX_INSTRUCTIONS, MAX_PROTOS, MAX_UPVALUES};
use crate::opcode::{COUNT_OPEN, Capture, Op};
use crate::program::ProtoSpec;
use crate::span::Span;

/// Why a prototype was rejected. The compiler reports it as
/// `InvalidProgram`; restore reports it as `SnapshotError::InvalidBytecode`.
pub(crate) type Reject = &'static str;

/// What the bytecode check needs from one prototype, whether it is a
/// compiled `ProtoSpec` or a decoded snapshot image.
pub(crate) struct CodeView<'a> {
    pub(crate) ops: &'a [Op],
    pub(crate) consts: usize,
    pub(crate) captures: &'a [Capture],
    /// Captures of each child prototype, by child index.
    pub(crate) children: Vec<&'a [Capture]>,
    pub(crate) max_reg: u8,
    pub(crate) params: u8,
    /// Only a vararg prototype reads extra arguments.
    pub(crate) vararg: bool,
}

impl<'a> CodeView<'a> {
    fn of_spec(spec: &'a ProtoSpec) -> Self {
        Self {
            ops: &spec.ops,
            consts: spec.byte_consts.len(),
            captures: &spec.captures,
            children: spec
                .children
                .iter()
                .map(|child| child.captures.as_slice())
                .collect(),
            max_reg: spec.max_reg,
            params: spec.params,
            vararg: spec.vararg,
        }
    }
}

pub(crate) fn validate(spec: &ProtoSpec) -> Result<(), CompileError> {
    let mut count = 0usize;
    validate_proto(spec, None, &mut count, 1, 0).map_err(invalid)
}

/// [`validate`] for a function read from a binary chunk (ADR 0036): its
/// root may have any number of upvalues, which `load` makes fresh.
pub(crate) fn validate_binary(spec: &ProtoSpec) -> Result<(), CompileError> {
    let mut count = 0usize;
    validate_proto(spec, None, &mut count, MAX_UPVALUES, 0).map_err(invalid)
}

fn validate_proto(
    spec: &ProtoSpec,
    parent: Option<&CodeView<'_>>,
    count: &mut usize,
    root_captures: usize,
    depth: u32,
) -> Result<(), Reject> {
    if depth >= MAX_FUNC_NEST {
        return Err("functions nested too deeply");
    }
    *count += 1;
    if *count > MAX_PROTOS {
        return Err("too many prototypes");
    }
    let view = CodeView::of_spec(spec);
    if parent.is_none() && spec.captures.len() > root_captures {
        return Err("the chunk captures more than its environment");
    }
    check_captures(spec.captures.as_slice(), parent)?;
    check_code(&view)?;
    if let Some(debug) = &spec.debug {
        check_debug(debug, spec.ops.len(), spec.captures.len(), spec.max_reg)?;
    }
    for child in &spec.children {
        validate_proto(child, Some(&view), count, root_captures, depth + 1)?;
    }
    Ok(())
}

/// Debug information (ADR 0040) that matches its code: a line for every
/// instruction or none, a name for every upvalue or none, and locals within
/// the code and its registers.
pub(crate) fn check_debug(
    debug: &crate::debuginfo::DebugInfo,
    ops: usize,
    captures: usize,
    max_reg: u8,
) -> Result<(), Reject> {
    let fits = (debug.lines.is_empty() || debug.lines.len() == ops)
        && (debug.upvalues.is_empty() || debug.upvalues.len() == captures)
        && debug.locals.iter().all(|local| {
            local.start <= local.end && local.end as usize <= ops && local.reg < max_reg
        })
        && debug.calls.iter().all(|call| (call.pc as usize) < ops)
        && debug.calls.windows(2).all(|pair| pair[0].pc < pair[1].pc);
    if fits {
        Ok(())
    } else {
        Err("debug information does not match the code")
    }
}

/// Captures of a prototype instantiated by `parent`'s `MakeClosure`.
pub(crate) fn check_captures(
    captures: &[Capture],
    parent: Option<&CodeView<'_>>,
) -> Result<(), Reject> {
    let Some(parent) = parent else {
        return Ok(());
    };
    for capture in captures {
        match capture {
            Capture::Local(reg) if *reg >= parent.max_reg => {
                return Err("capture register is outside the parent");
            }
            Capture::Upvalue(index) if *index as usize >= parent.captures.len() => {
                return Err("capture upvalue is outside the parent");
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every structural rule on one prototype's code: counts, register window,
/// and each instruction's operands and targets.
pub(crate) fn check_code(view: &CodeView<'_>) -> Result<(), Reject> {
    if view.ops.is_empty() || view.ops.len() > MAX_INSTRUCTIONS {
        return Err("instruction count is out of range");
    }
    if view.max_reg == 0 || view.params > view.max_reg {
        return Err("register window is inconsistent");
    }
    if view.consts > MAX_CONSTS || view.captures.len() > MAX_UPVALUES {
        return Err("constant or upvalue count is out of range");
    }
    if view.children.len() > MAX_PROTOS {
        return Err("too many child prototypes");
    }
    for (pc, op) in view.ops.iter().enumerate() {
        check_op(view, pc, op)?;
    }
    Ok(())
}

fn check_op(spec: &CodeView<'_>, pc: usize, op: &Op) -> Result<(), Reject> {
    let max = spec.max_reg;
    match *op {
        Op::LoadNil { dst }
        | Op::LoadInt { dst, .. }
        | Op::LoadFloat { dst, .. }
        | Op::LoadBool { dst, .. }
        | Op::NewTable { dst }
        | Op::GetGlobal { dst }
        | Op::AssignLocal { reg: dst } => need(max, dst),
        Op::VarargLen { dst } => {
            vararg(spec)?;
            need(max, dst)
        }
        Op::LoadBytes { dst, const_index } => {
            need(max, dst)?;
            constant(spec, const_index)
        }
        Op::Move { dst, src } => {
            need(max, dst)?;
            need(max, src)
        }
        Op::Add { dst, a, b }
        | Op::GetTable {
            dst,
            table: a,
            key: b,
        } => {
            need(max, dst)?;
            need(max, a)?;
            need(max, b)
        }
        Op::SetTable { table, key, src } => {
            need(max, table)?;
            need(max, key)?;
            need(max, src)
        }
        Op::MakeClosure { dst, child } | Op::NewThread { dst, child } => {
            need(max, dst)?;
            if child as usize >= spec.children.len() {
                return Err("child prototype index");
            }
            if matches!(op, Op::NewThread { .. })
                && spec
                    .children
                    .get(child as usize)
                    .is_some_and(|captures| !captures.is_empty())
            {
                return Err("thread prototype has captures");
            }
            Ok(())
        }
        Op::GetUpvalue { dst, index } => {
            need(max, dst)?;
            upvalue(spec, index)
        }
        Op::SetUpvalue { index, src } => {
            need(max, src)?;
            upvalue(spec, index)
        }
        Op::Call {
            func,
            nargs,
            nresults,
        } => {
            call_window(max, func, nargs)?;
            if nresults != COUNT_OPEN {
                window(max, func, nresults)?;
            }
            Ok(())
        }
        Op::Return { base, count } | Op::Yield { base, count } => window(max, base, count),
        Op::CallHost { dst, symbol, arg } => {
            need(max, dst)?;
            need(max, arg)?;
            constant(spec, u32::from(symbol))
        }
        Op::Resume {
            dest,
            thread,
            nresults,
        } => {
            need(max, dest)?;
            need(max, thread)?;
            if nresults != COUNT_OPEN {
                window(max, dest, nresults)?;
            }
            Ok(())
        }
        Op::Vararg { dst, count } => {
            vararg(spec)?;
            window(max, dst, count)
        }
        Op::AssignCommit { src, n } => window(max, src, n),
        Op::OpenLen { dst, from } => {
            need(max, dst)?;
            need(max, from)
        }
        Op::AssignField { table, key } => {
            need(max, table)?;
            need(max, key)
        }
        Op::Jump { offset } => jump(spec, pc, offset),
        Op::JumpIfFalse { src, offset } => {
            need(max, src)?;
            jump(spec, pc, offset)
        }
        Op::CloseUpvalues { from } | Op::CloseScope { from } | Op::MarkClose { reg: from } => {
            need(max, from)
        }
        Op::CloseThread { dst, thread } => {
            need(max, dst.checked_add(1).ok_or("register window")?)?;
            need(max, thread)
        }
        Op::Len { dst, src } => {
            need(max, dst)?;
            need(max, src)
        }
        Op::Index { dst, obj, key } => {
            need(max, dst)?;
            need(max, obj)?;
            need(max, key)
        }
        Op::SetIndex { obj, key, src } => {
            need(max, obj)?;
            need(max, key)?;
            need(max, src)
        }
        Op::GetField { dst, obj, name } => {
            need(max, dst)?;
            need(max, obj)?;
            constant(spec, name)
        }
        Op::SetField { obj, name, src } => {
            need(max, obj)?;
            need(max, src)?;
            constant(spec, name)
        }
        Op::SetList { table, src, start } => {
            need(max, table)?;
            need(max, src)?;
            if start == 0 {
                return Err("list start is zero");
            }
            Ok(())
        }
        Op::Neg { dst, src } | Op::BNot { dst, src } => {
            need(max, dst)?;
            need(max, src)
        }
        Op::Arith { dst, a, b, .. } | Op::Concat { dst, a, b } => {
            need(max, dst)?;
            need(max, a)?;
            need(max, b)
        }
        Op::ArithK { dst, reg, .. } => {
            need(max, dst)?;
            need(max, reg)
        }
        Op::ForPrep { base, offset } | Op::ForLoop { base, offset } => {
            need(max, base.checked_add(3).ok_or("for register")?)?;
            jump(spec, pc, offset)
        }
        Op::GenericForLoop { base, offset } => generic_for_loop(spec, pc, base, offset),
        Op::TailCall { func, nargs } => tail_call(spec, pc, func, nargs),
        Op::Compare { dst, a, b, .. } => {
            need(max, dst)?;
            need(max, a)?;
            need(max, b)
        }
        Op::JumpIfLt { a, b, offset } | Op::CompareBranch { a, b, offset, .. } => {
            need(max, a)?;
            need(max, b)?;
            jump(spec, pc, offset)
        }
        Op::Next { dst, table, key } => {
            need(max, dst)?;
            need(max, dst.checked_add(1).ok_or("next register")?)?;
            need(max, table)?;
            need(max, key)
        }
        Op::RawLen { dst, table } => {
            need(max, dst)?;
            need(max, table)
        }
        Op::Halt => Ok(()),
    }
}

/// A generic `for`'s decision follows its iterator call: the four hidden
/// values at `base..base + 4`, then the call of the iterator's copy at
/// `base + 4` with two arguments, state and control, wanting one result per
/// loop variable. At least one variable, and never an open count. The
/// branch goes back to the body, before the call.
fn generic_for_loop(spec: &CodeView<'_>, pc: usize, base: u8, offset: i32) -> Result<(), Reject> {
    let func = base.checked_add(4).ok_or("for register")?;
    need(spec.max_reg, func)?;
    let call = pc.checked_sub(1).and_then(|at| spec.ops.get(at));
    match call {
        Some(Op::Call {
            func: at,
            nargs: 2,
            nresults,
        }) if *at == func && *nresults != 0 && *nresults != COUNT_OPEN => {}
        _ => return Err("generic for without its iterator call"),
    }
    // `pc + 1 + offset` must lie before the call at `pc - 1`.
    if offset > -3 {
        return Err("generic for does not branch back to its body");
    }
    jump(spec, pc, offset)
}

/// A tail call's argument window, and the `Return` of its open results
/// that follows it: a thread's first frame returns what a native it
/// tail-calls gives (ADR 0029).
fn tail_call(spec: &CodeView<'_>, pc: usize, func: u8, nargs: u8) -> Result<(), Reject> {
    call_window(spec.max_reg, func, nargs)?;
    match spec.ops.get(pc + 1) {
        Some(Op::Return { base, count }) if *base == func && *count == COUNT_OPEN => Ok(()),
        _ => Err("tail call without its return"),
    }
}

fn vararg(spec: &CodeView<'_>) -> Result<(), Reject> {
    if spec.vararg {
        Ok(())
    } else {
        Err("extra arguments read by a function that takes none")
    }
}

fn need(max: u8, reg: u8) -> Result<(), Reject> {
    if reg < max {
        Ok(())
    } else {
        Err("register out of range")
    }
}

fn window(max: u8, base: u8, count: u8) -> Result<(), Reject> {
    if count == COUNT_OPEN || count == 0 {
        return need(max, base);
    }
    let last = u16::from(base)
        .checked_add(u16::from(count) - 1)
        .ok_or("result window overflow")?;
    if base < max && last < u16::from(max) {
        Ok(())
    } else {
        Err("result window is outside the frame")
    }
}

fn call_window(max: u8, func: u8, nargs: u8) -> Result<(), Reject> {
    need(max, func)?;
    if nargs == COUNT_OPEN {
        return Ok(());
    }
    let last = u16::from(func)
        .checked_add(u16::from(nargs))
        .ok_or("argument window overflow")?;
    if last < u16::from(max) {
        Ok(())
    } else {
        Err("argument window is outside the frame")
    }
}

fn constant(spec: &CodeView<'_>, index: u32) -> Result<(), Reject> {
    if (index as usize) < spec.consts {
        Ok(())
    } else {
        Err("constant index")
    }
}

fn upvalue(spec: &CodeView<'_>, index: u8) -> Result<(), Reject> {
    if (index as usize) < spec.captures.len() {
        Ok(())
    } else {
        Err("upvalue index")
    }
}

fn jump(spec: &CodeView<'_>, pc: usize, offset: i32) -> Result<(), Reject> {
    let dest = i64::try_from(pc)
        .ok()
        .and_then(|pc| pc.checked_add(1))
        .and_then(|next| next.checked_add(i64::from(offset)));
    match dest {
        Some(dest) if dest >= 0 && dest < spec.ops.len() as i64 => Ok(()),
        _ => Err("jump leaves the prototype"),
    }
}

fn invalid(message: Reject) -> CompileError {
    CompileError::new(CompileErrorKind::InvalidProgram, Span::new(0, 0), message)
}
