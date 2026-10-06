//! Compile the supported source subset to a detached prototype.
//!
//! Nothing here mutates a `Runtime`. [`compile`] validates the prototype
//! before returning it, so a failure leaves no heap behind.

use crate::ast::{BinOp, Block, ChainStep, Expr, Name, Stmt, TableField, Target, UnOp};
use crate::check;
use crate::error::{CompileError, CompileErrorKind};
use crate::limits::{
    MAX_CONSTS, MAX_FUNC_NEST, MAX_INSTRUCTIONS, MAX_LOCALS, MAX_PROTOS, MAX_REGISTERS,
    MAX_SOURCE_BYTES, MAX_UPVALUES,
};
use crate::opcode::{ArithOp, COUNT_OPEN, Capture, CmpKind, Op};
use crate::parse;
use crate::program::ProtoSpec;
use crate::span::Span;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(crate) struct CodeMap {
    pub(crate) spans: Vec<Span>,
    pub(crate) children: Vec<CodeMap>,
}

/// Compiler output bounds. Larger values are clamped to the structural ceilings;
/// memory grows with the input, never with these limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileLimits {
    /// Instructions per function; ceiling: 1 << 24.
    pub max_instructions: usize,
    /// Distinct byte constants per function; ceiling: 1 << 24.
    pub max_constants: usize,
    /// Functions per chunk, including the root; ceiling: 1 << 20.
    pub max_functions: usize,
    /// Source bytes, including any literal; ceiling: 1 << 30.
    pub max_source_bytes: usize,
}

impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            max_instructions: 1 << 20,
            max_constants: 1 << 20,
            max_functions: 1 << 16,
            max_source_bytes: crate::limits::DEFAULT_SOURCE_BYTES,
        }
    }
}

impl CompileLimits {
    fn clamped(&self) -> Self {
        Self {
            max_instructions: self.max_instructions.min(MAX_INSTRUCTIONS),
            max_constants: self.max_constants.min(MAX_CONSTS),
            max_functions: self.max_functions.min(MAX_PROTOS),
            max_source_bytes: self.max_source_bytes.min(MAX_SOURCE_BYTES),
        }
    }
}

/// Detached compiled chunk. Instruction spans are debug metadata: they are
/// not installed into the VM and they are not part of a snapshot.
#[derive(Debug)]
pub struct CompiledChunk {
    pub(crate) proto: ProtoSpec,
    pub(crate) map: CodeMap,
}

impl CompiledChunk {
    /// Count compiled functions, including the root function.
    pub fn prototype_count(&self) -> usize {
        proto_count(&self.proto)
    }

    /// Count instructions across all compiled functions.
    pub fn instruction_count(&self) -> usize {
        op_count(&self.proto)
    }

    /// The root function's register requirement.
    pub fn max_registers(&self) -> u8 {
        self.proto.max_reg
    }

    /// Name the chunk, as Lua's `chunkname`: `@file.lua` for a file,
    /// `=name` for a name shown as is, anything else for source text.
    /// `debug.getinfo` and tracebacks show it (ADR 0040); an unnamed
    /// chunk is `=?`.
    pub fn set_chunk_name(&mut self, name: &[u8]) {
        if let Some(debug) = self.proto.debug.as_mut() {
            debug.source = Some(name.to_vec());
        }
    }

    /// True when some instruction in the root prototype was emitted from a
    /// source range containing `offset`.
    pub fn root_span_covers(&self, offset: u32) -> bool {
        self.map.spans.iter().any(|span| span.contains(offset))
    }

    /// Instruction spans kept on the chunk, including nested prototypes.
    /// These spans are not VM state.
    pub fn mapped_instructions(&self) -> usize {
        fn walk(map: &CodeMap) -> usize {
            map.spans.len() + map.children.iter().map(walk).sum::<usize>()
        }
        walk(&self.map)
    }
}

/// Compile with the default source and code limits.
pub fn compile(source: &[u8]) -> Result<CompiledChunk, CompileError> {
    compile_with_limits(source, &CompileLimits::default())
}

/// Compile with configurable bounds, clamped to the structural ceilings.
pub fn compile_with_limits(
    source: &[u8],
    limits: &CompileLimits,
) -> Result<CompiledChunk, CompileError> {
    compile_with_limits_inner(source, limits, 0, u64::MAX)
        .map_err(|error| error.with_source(source))
}

/// Compile guest-supplied source (`load`, files, resolver modules): the
/// syntax tree is bounded by `heap_room`, the logical-heap headroom, so the
/// compiler's transient memory stays proportional to the quota (ADR 0052).
pub(crate) fn compile_for_guest(
    source: &[u8],
    limits: &CompileLimits,
    c_frames: u32,
    heap_room: u64,
) -> Result<CompiledChunk, CompileError> {
    let max_ast_bytes = heap_room.saturating_mul(4);
    compile_with_limits_inner(source, limits, c_frames, max_ast_bytes)
        .map_err(|error| error.with_source(source))
}

/// A protected `load`/`xpcall` frame consumes PUC's C-call depth while the
/// parser runs. The host `compile` API starts with no such Lua frames.
pub(crate) fn compile_for_load(
    source: &[u8],
    c_frames: u32,
    heap_room: u64,
) -> Result<CompiledChunk, CompileError> {
    compile_for_guest(
        source,
        &CompileLimits::default(),
        c_frames.saturating_add(1),
        heap_room,
    )
}

fn compile_with_limits_inner(
    source: &[u8],
    limits: &CompileLimits,
    c_frames: u32,
    max_ast_bytes: u64,
) -> Result<CompiledChunk, CompileError> {
    let limits = limits.clamped();
    let (chunk, diagnostic_near) = parse::parse_with_diagnostics_at_depth(
        source,
        limits.max_source_bytes,
        c_frames,
        max_ast_bytes,
    )?;
    if limits.max_functions == 0 {
        return Err(limit(chunk.span, "too many functions in the chunk"));
    }
    let mut compiler = Compiler {
        funcs: vec![FnBuild::new(chunk.span)],
        protos: 1,
        lines: LineIndex::new(source),
        diagnostic_near,
        limits,
    };
    // A chunk is the body of a vararg function, as in Lua.
    compiler.funcs[0].vararg = true;
    // The chunk's one external variable. `Runtime::load` binds it.
    compiler.funcs[0].captures.push(CapRec {
        name: b"_ENV".to_vec(),
        kind: Capture::Upvalue(0),
        readonly: false,
    });
    compiler.block(0, &chunk.body)?;
    compiler.funcs[0].patch_gotos(&compiler.lines)?;
    // Every function ends with a `Return`, reached or not, on its last
    // line, as Lua's does: `activelines` shows it.
    // Main's implicit return follows the last parsed token, not trailing
    // whitespace at EOF; function bodies already use their closing end token.
    let return_line = if chunk.body.stmts.is_empty() {
        0
    } else {
        chunk.body.span.end.saturating_sub(1)
    };
    let return_pc = compiler.funcs[0].ops.len();
    compiler.funcs[0].line_at.push((return_pc, return_line));
    compiler.emit(0, Op::Return { base: 0, count: 0 }, chunk.span)?;
    let built = compiler
        .funcs
        .pop()
        .ok_or_else(|| invalid(chunk.span, "missing function"))?;
    let (proto, map) = built.finish(&compiler.lines, true, compiler.limits.max_instructions)?;
    check::validate(&proto)?;
    Ok(CompiledChunk { proto, map })
}

/// Where each line of the source starts, to give instructions their
/// lines (ADR 0040).
struct LineIndex {
    starts: Vec<usize>,
    source: Vec<u8>,
}

impl LineIndex {
    /// Lines break as Lua's lexer breaks them (`span::line_col`): at
    /// `\n`, `\r`, `\r\n`, or `\n\r`, each one break.
    fn new(source: &[u8]) -> Self {
        let mut starts = vec![0];
        let mut index = 0;
        while index < source.len() {
            let byte = source[index];
            index += 1;
            if byte == b'\n' || byte == b'\r' {
                if source
                    .get(index)
                    .is_some_and(|next| (*next == b'\n' || *next == b'\r') && *next != byte)
                {
                    index += 1;
                }
                starts.push(index);
            }
        }
        Self {
            starts,
            source: source.to_vec(),
        }
    }

    /// Where the next token after byte `offset` starts: past white space
    /// and comments. Lua gives an operator's and a call's instruction the
    /// line of that token (the operator, or the arguments' opening).
    fn token_after(&self, offset: u32) -> u32 {
        let source = &self.source;
        let mut at = offset as usize;
        loop {
            while source
                .get(at)
                .is_some_and(|byte| b" \t\r\n\x0b\x0c".contains(byte))
            {
                at += 1;
            }
            if !source[at.min(source.len())..].starts_with(b"--") {
                break;
            }
            at += 2;
            // A long comment: `--[`, some `=`, `[`, to the matching close.
            let level = source[at.min(source.len())..]
                .strip_prefix(b"[")
                .map(|rest| rest.iter().take_while(|byte| **byte == b'=').count())
                .filter(|level| source.get(at + 1 + level) == Some(&b'['));
            match level {
                Some(level) => {
                    let mut close = vec![b']'];
                    close.extend(std::iter::repeat_n(b'=', level));
                    close.push(b']');
                    let body = at + 2 + level;
                    at = source[body.min(source.len())..]
                        .windows(close.len())
                        .position(|window| window == close.as_slice())
                        .map_or(source.len(), |found| body + found + close.len());
                }
                None => {
                    while source
                        .get(at)
                        .is_some_and(|byte| *byte != b'\n' && *byte != b'\r')
                    {
                        at += 1;
                    }
                }
            }
        }
        u32::try_from(at).unwrap_or(u32::MAX)
    }

    /// The 1-based line of byte `offset`.
    fn line(&self, offset: u32) -> u32 {
        let line = self
            .starts
            .partition_point(|start| *start <= offset as usize);
        u32::try_from(line).unwrap_or(crate::debuginfo::MAX_LINE)
    }
}

// PUC folds literal arithmetic; its resulting instruction is attributed to the
// final expression token. Keep Moonseed's instructions and fuel, sharing that line.
fn literal_arithmetic(expr: &Expr) -> bool {
    let arithmetic = |op: BinOp| matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul);
    match expr {
        Expr::Integer { .. } | Expr::Float { .. } => true,
        Expr::Paren { inner, .. } => literal_arithmetic(inner),
        Expr::Binary {
            op, left, right, ..
        } => arithmetic(*op) && literal_arithmetic(left) && literal_arithmetic(right),
        Expr::Chain { first, rest, .. } => {
            literal_arithmetic(first)
                && rest.iter().all(|step| {
                    matches!(step,
                    ChainStep::Binary { op, right, .. }
                    if arithmetic(*op) && literal_arithmetic(right))
                })
        }
        _ => false,
    }
}

struct Compiler {
    limits: CompileLimits,
    lines: LineIndex,
    diagnostic_near: parse::DiagnosticNear,
    funcs: Vec<FnBuild>,
    /// Functions compiled so far, the chunk's own included. The validator
    /// refuses more than `MAX_PROTOS`; the compiler stops first, with a
    /// `Limit` error.
    protos: usize,
}

struct FnBuild {
    locals: Vec<LocalRec>,
    /// Every local declared, in order, with the instructions it is active
    /// over, for debug information (ADR 0040).
    debug_locals: Vec<crate::debuginfo::LocalInfo>,
    /// The name of each call's function, where the source gives one.
    debug_calls: Vec<crate::debuginfo::CallName>,
    /// Instructions whose line is not their construct's end: a byte
    /// offset whose line they take, as Lua fixes a call's and an
    /// operator's line.
    line_at: Vec<(usize, u32)>,
    captures: Vec<CapRec>,
    ops: Vec<Op>,
    spans: Vec<Span>,
    consts: Vec<Vec<u8>>,
    /// Lookup only; `consts` retains source-order indexes in the prototype.
    const_lookup: HashMap<Vec<u8>, u32>,
    children: Vec<ProtoSpec>,
    child_maps: Vec<CodeMap>,
    nact: u8,
    free: u8,
    high: u8,
    params: u8,
    /// The function takes `...`: extra arguments are kept, and `...` may be
    /// used directly in its body.
    vararg: bool,
    span: Span,
    /// Enclosing loops, innermost last.
    loops: Vec<LoopCtx>,
    /// Every local declared so far, in order, kept after its scope ends
    /// so a `goto` can decide its cleanup once every capture is known.
    decls: Vec<Decl>,
    /// Labels visible here: in the current block and those enclosing it,
    /// declared so far. A block's labels go when it ends.
    labels: Vec<LabelRec>,
    /// Forward gotos waiting for their label, in source order.
    gotos: Vec<GotoRec>,
    /// Gotos resolved to a label, patched when the function ends.
    edges: Vec<GotoEdge>,
    /// Where the labels, pending gotos, and locals stood when each
    /// enclosing block began, innermost last. The function's own block has
    /// none: it starts with nothing.
    blocks: Vec<BlockMark>,
    /// Final arithmetic results followed by a single-local assignment.
    /// Capture flags are not final until the whole function is compiled.
    destinations: Vec<(usize, usize)>,
}

/// A declared local, after its scope too. `captured` and `close` are final
/// once the scope ends.
struct Decl {
    reg: u8,
    captured: bool,
    close: bool,
}

struct LabelRec {
    name: Vec<u8>,
    line: u32,
    /// Where the label is: its jumps' target.
    pc: usize,
    /// The locals in scope at the label: their count in `locals`. A
    /// trailing label counts its block's locals out.
    frontier: usize,
}

struct GotoRec {
    name: Vec<u8>,
    span: Span,
    /// The first of the goto's two instruction slots.
    site: usize,
    /// The locals in scope at the goto, as `decls` indexes.
    active: Vec<usize>,
    /// How many of `locals` the goto is still in scope of: lowered to each
    /// block's start as the goto leaves the block. Below a label's, the
    /// goto would enter a local's scope.
    frontier: usize,
}

struct GotoEdge {
    span: Span,
    site: usize,
    target: usize,
    /// The locals the jump leaves, as `decls` indexes, oldest first.
    left: Vec<usize>,
}

#[derive(Clone, Copy)]
struct BlockMark {
    labels: usize,
    gotos: usize,
    locals: usize,
}

/// A loop being compiled. `outer` is the scope just outside the loop body:
/// `break` closes everything declared since it, then jumps to the exit.
struct LoopCtx {
    outer: Scope,
    /// Jumps to the loop's exit made by the loop's own code.
    breaks: Vec<usize>,
    /// `break` statements: two slots each, and the locals in scope there.
    /// Like a goto's, their cleanup is decided when the function ends: a
    /// backward goto can run a capture that follows a `break` in the source
    /// before the `break` does (ADR 0030).
    exits: Vec<(usize, Span, Vec<usize>)>,
}

struct LocalRec {
    name: Vec<u8>,
    reg: u8,
    /// Index in `FnBuild::debug_locals`.
    debug: usize,
    /// Index in `FnBuild::decls`.
    decl: usize,
    /// A nested function captured this local, so leaving its scope must
    /// close the upvalue before the register is reused.
    captured: bool,
    /// A `<close>` local: leaving its scope calls its `__close` (ADR 0026).
    close: bool,
    /// A `<const>` or `<close>` local: no assignment after its declaration.
    /// Checked only here, in the compiler: bytecode and snapshots do not
    /// carry it.
    readonly: bool,
}

/// Compile-time lexical block: where the local list and the register
/// frontier stood on entry. Leaving the block returns both to these values.
#[derive(Clone, Copy)]
struct Scope {
    locals: usize,
    nact: u8,
}

struct CapRec {
    name: Vec<u8>,
    kind: Capture,
    /// The captured variable is a `<const>` or `<close>` local, so read-only here too.
    readonly: bool,
}

#[derive(Clone, Copy)]
enum Place {
    Local(u8),
    Upvalue(u8),
}

impl FnBuild {
    fn new(span: Span) -> Self {
        Self {
            locals: Vec::new(),
            debug_locals: Vec::new(),
            debug_calls: Vec::new(),
            line_at: Vec::new(),
            captures: Vec::new(),
            ops: Vec::new(),
            spans: Vec::new(),
            consts: Vec::new(),
            const_lookup: HashMap::new(),
            children: Vec::new(),
            child_maps: Vec::new(),
            nact: 0,
            free: 0,
            high: 0,
            params: 0,
            vararg: false,
            span,
            loops: Vec::new(),
            decls: Vec::new(),
            labels: Vec::new(),
            gotos: Vec::new(),
            edges: Vec::new(),
            blocks: Vec::new(),
            destinations: Vec::new(),
        }
    }

    /// Record the final flags of the locals from index `from` on, whose
    /// scope is ending.
    fn retire_locals(&mut self, from: usize) {
        for local in &self.locals[from..] {
            let decl = &mut self.decls[local.decl];
            decl.captured |= local.captured;
            decl.close |= local.close;
        }
    }

    /// Fill in every goto's two slots, now that every local's flags are
    /// final: a `Jump`, or the close its left locals need and a `Jump`
    /// (ADR 0030). A goto no label answered is an error.
    fn patch_gotos(&mut self, lines: &LineIndex) -> Result<(), CompileError> {
        if let Some(pending) = self.gotos.first() {
            return Err(CompileError::new(
                CompileErrorKind::Syntax,
                pending.span,
                format!(
                    "no visible label '{}' for <goto> at line {}",
                    String::from_utf8_lossy(&pending.name),
                    lines.line(pending.span.start)
                ),
            )
            .with_diagnostic_line(lines.line(self.span.end)));
        }
        self.retire_locals(0);
        for edge in std::mem::take(&mut self.edges) {
            let left = || edge.left.iter().map(|decl| &self.decls[*decl]);
            let cleanup = left().next().and_then(|first| {
                exit_op(
                    first.reg,
                    left().any(|decl| decl.close),
                    left().any(|decl| decl.captured),
                )
            });
            let jump = |pc: usize| {
                i64::try_from(edge.target)
                    .ok()
                    .zip(i64::try_from(pc + 1).ok())
                    .and_then(|(target, next)| i32::try_from(target - next).ok())
                    .map(|offset| Op::Jump { offset })
                    .ok_or_else(|| limit(edge.span, "jump distance"))
            };
            self.ops[edge.site] = match cleanup {
                Some(op) => op,
                None => jump(edge.site)?,
            };
            self.ops[edge.site + 1] = jump(edge.site + 1)?;
        }
        Ok(())
    }

    /// The locals from index `from` on go out of scope here.
    fn end_locals(&mut self, from: usize) {
        let pc = self.ops.len() as u32;
        for local in &self.locals[from..] {
            if let Some(info) = self.debug_locals.get_mut(local.debug) {
                info.end = pc;
            }
        }
    }

    fn finish(
        mut self,
        lines: &LineIndex,
        main: bool,
        max_instructions: usize,
    ) -> Result<(ProtoSpec, CodeMap), CompileError> {
        if self.ops.len() > max_instructions {
            return Err(limit(self.span, "too many instructions"));
        }
        self.end_locals(0);
        let mut op_lines: Vec<u32> = self
            .spans
            .iter()
            .zip(&self.ops)
            .map(|(span, op)| match op {
                Op::Call { .. }
                | Op::TailCall { .. }
                | Op::ForPrep { .. }
                | Op::ForLoop { .. }
                | Op::GenericForLoop { .. }
                | Op::NewTable { .. } => lines.line(span.start),
                _ => lines.line(span.end.saturating_sub(1).max(span.start)),
            })
            .collect();
        for (pc, offset) in &self.line_at {
            if let Some(line) = op_lines.get_mut(*pc) {
                *line = lines.line(*offset);
            }
        }
        self.finish_destinations(&mut op_lines);
        let debug = crate::debuginfo::DebugInfo {
            source: None,
            // A chunk is defined at line 0, as in Lua.
            line_defined: if main { 0 } else { lines.line(self.span.start) },
            last_line_defined: if main {
                0
            } else {
                lines.line(self.span.end.saturating_sub(1))
            },
            // An instruction's line is where the construct that made it
            // ends, as Lua's is the last token read when it was emitted;
            // Lua fixes a loop's on its `for`, a call's on its arguments'
            // opening, and an operator's on the operator.
            lines: op_lines,
            locals: std::mem::take(&mut self.debug_locals)
                .into_iter()
                .filter(|local| !local.name.is_empty())
                .collect(),
            upvalues: self
                .captures
                .iter()
                .map(|capture| capture.name.clone())
                .collect(),
            calls: std::mem::take(&mut self.debug_calls),
        };
        if self.ops.len() != self.spans.len() || self.children.len() != self.child_maps.len() {
            return Err(invalid(self.span, "debug map does not match code"));
        }
        let max_reg = self.high.max(self.nact).max(self.params).max(1);
        // Every path checks the limit before it names a register; this is
        // the backstop, never the check.
        if max_reg > MAX_REGISTERS {
            return Err(invalid(self.span, "register limit passed"));
        }
        Ok((
            ProtoSpec {
                ops: self.ops,
                byte_consts: self.consts,
                captures: self
                    .captures
                    .into_iter()
                    .map(|capture| capture.kind)
                    .collect(),
                children: self.children,
                max_reg,
                params: self.params,
                vararg: self.vararg,
                debug: Some(Box::new(debug)),
            },
            CodeMap {
                spans: self.spans,
                children: self.child_maps,
            },
        ))
    }

    /// Fold only recorded scalar assignments, never arbitrary adjacent Moves.
    /// Both operands have been evaluated before the final arithmetic operation;
    /// it reads them before storing, including after a successful metamethod.
    /// A fault or suspended callback therefore still sees the old local. A later
    /// capture (possibly reached first through a backedge) vetoes the fold.
    fn finish_destinations(&mut self, lines: &mut Vec<u32>) {
        if self.destinations.is_empty() {
            return;
        }
        let len = self.ops.len();
        let mut entries = vec![false; len + 1];
        for (pc, op) in self.ops.iter_mut().enumerate() {
            if let Some(offset) = branch_offset(op) {
                entries[(pc as i64 + 1 + i64::from(*offset)) as usize] = true;
            }
        }
        let mut removed = vec![false; len];
        for &(pc, decl) in &self.destinations {
            let local = &self.decls[decl];
            if local.captured || local.close || entries[pc + 1] {
                continue;
            }
            let Op::Move { dst, src } = self.ops[pc + 1] else {
                continue;
            };
            if dst != local.reg {
                continue;
            }
            if let Op::Add { dst: result, .. }
            | Op::Arith { dst: result, .. }
            | Op::ArithK { dst: result, .. } = &mut self.ops[pc]
                && *result == src
            {
                *result = dst;
                removed[pc + 1] = true;
            }
        }
        // Map instruction boundaries, including the end boundary used by local
        // lifetimes. No removed instruction is an alternate branch entry.
        let mut map = Vec::with_capacity(len + 1);
        let mut next = 0;
        for &remove in &removed {
            map.push(next);
            next += usize::from(!remove);
        }
        map.push(next);
        for (pc, op) in self.ops.iter_mut().enumerate() {
            if let Some(offset) = branch_offset(op) {
                let target = (pc as i64 + 1 + i64::from(*offset)) as usize;
                *offset = (map[target] as i64 - map[pc] as i64 - 1) as i32;
            }
        }
        for local in &mut self.debug_locals {
            local.start = map[local.start as usize] as u32;
            local.end = map[local.end as usize] as u32;
        }
        for call in &mut self.debug_calls {
            call.pc = map[call.pc as usize] as u32;
        }
        let mut pc = 0;
        self.ops.retain(|_| {
            let keep = !removed[pc];
            pc += 1;
            keep
        });
        pc = 0;
        self.spans.retain(|_| {
            let keep = !removed[pc];
            pc += 1;
            keep
        });
        pc = 0;
        lines.retain(|_| {
            let keep = !removed[pc];
            pc += 1;
            keep
        });
    }
}

fn branch_offset(op: &mut Op) -> Option<&mut i32> {
    match op {
        Op::Jump { offset }
        | Op::JumpIfFalse { offset, .. }
        | Op::CompareBranch { offset, .. }
        | Op::JumpIfLt { offset, .. }
        | Op::ForPrep { offset, .. }
        | Op::ForLoop { offset, .. }
        | Op::GenericForLoop { offset, .. } => Some(offset),
        _ => None,
    }
}

fn integer_literal(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Integer { value, .. } => Some(*value),
        Expr::Paren { inner, .. } => integer_literal(inner),
        _ => None,
    }
}

fn comparison_kind(op: BinOp) -> (CmpKind, bool) {
    match op {
        BinOp::Eq => (CmpKind::Eq, false),
        BinOp::Ne => (CmpKind::Ne, false),
        BinOp::Lt => (CmpKind::Lt, false),
        BinOp::Le => (CmpKind::Le, false),
        BinOp::Gt => (CmpKind::Lt, true),
        BinOp::Ge => (CmpKind::Le, true),
        _ => unreachable!("comparison operator"),
    }
}

fn condition_has_comparison(expr: &Expr) -> bool {
    match expr {
        Expr::Paren { inner, .. }
        | Expr::Unary {
            op: UnOp::Not,
            expr: inner,
            ..
        } => condition_has_comparison(inner),
        Expr::Binary {
            op: BinOp::And | BinOp::Or,
            left,
            right,
            ..
        } => condition_has_comparison(left) || condition_has_comparison(right),
        Expr::Binary { op, .. } => matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        ),
        _ => false,
    }
}

fn arithmetic_result(expr: &Expr) -> bool {
    match expr {
        Expr::Paren { inner, .. } => arithmetic_result(inner),
        Expr::Binary { op, .. } => matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Idiv
                | BinOp::Mod
                | BinOp::Pow
                | BinOp::Band
                | BinOp::Bor
                | BinOp::Bxor
                | BinOp::Shl
                | BinOp::Shr
        ),
        _ => false,
    }
}

/// PUC omits the last `<const>` local from LocVar when its initializer is
/// known at compile time. Moonseed retains its register, but not its
/// diagnostic local name.
fn debugless_const(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Nil { .. }
            | Expr::Bool { .. }
            | Expr::Integer { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
    )
}

impl Compiler {
    fn near_at(&self, span: Span) -> Option<Vec<u8>> {
        self.diagnostic_near
            .iter()
            .find(|(offset, _)| *offset == span.start)
            .map(|(_, near)| near.clone())
    }

    fn block(&mut self, depth: usize, block: &Block) -> Result<(), CompileError> {
        for stmt in &block.stmts {
            self.set_free(depth, self.funcs[depth].nact);
            self.stmt(depth, stmt)?;
        }
        Ok(())
    }

    fn stmt(&mut self, depth: usize, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::Empty { .. } => Ok(()),
            Stmt::Local {
                names,
                values,
                span,
                close,
                consts,
            } => {
                self.place_values(depth, values, names.len(), *span)?;
                for name in names {
                    self.define(depth, name)?;
                }
                let first = self.funcs[depth].locals.len() - names.len();
                for index in consts.iter().chain(close) {
                    self.funcs[depth].locals[first + index].readonly = true;
                }
                if names.len() == values.len()
                    && let Some(last) = names.len().checked_sub(1)
                    && consts.contains(&last)
                    && debugless_const(&values[last])
                {
                    let debug = self.funcs[depth].locals[first + last].debug;
                    self.funcs[depth].debug_locals[debug].name.clear();
                }
                // The value is checked and listed only once every name of
                // the list is in scope, as in Lua.
                if let Some(index) = close {
                    let func = &mut self.funcs[depth];
                    let local = first + index;
                    func.locals[local].close = true;
                    let reg = func.locals[local].reg;
                    self.emit(depth, Op::MarkClose { reg }, names[*index].span)?;
                }
                Ok(())
            }
            Stmt::Assign {
                targets,
                values,
                span,
                ..
            } => self.assign(depth, targets, values, *span),
            // `local f; f = function ... end`: the local is in scope before
            // the closure is made, so the body captures it, and the closure
            // is stored straight into its register.
            Stmt::LocalFunction { name, func, span } => {
                let Expr::Function {
                    params,
                    vararg,
                    body,
                    ..
                } = func
                else {
                    return Err(invalid(*span, "local function without a body"));
                };
                self.define(depth, name)?;
                let reg = self.funcs[depth].nact - 1;
                let child = self.function(depth, params, *vararg, body, func.span())?;
                self.emit(depth, Op::MakeClosure { dst: reg, child }, *span)?;
                Ok(())
            }
            Stmt::Return { values, span } => self.ret(depth, values, *span),
            Stmt::If {
                span,
                arms,
                else_block,
            } => self.if_stmt(depth, arms, else_block.as_ref(), *span),
            Stmt::Do { body, .. } => self.scoped_block(depth, body).map(|_| ()),
            Stmt::While { span, cond, body } => self.while_stmt(depth, cond, body, *span),
            Stmt::Repeat { span, body, cond } => self.repeat_stmt(depth, body, cond, *span),
            Stmt::NumericFor {
                span,
                name,
                init,
                limit,
                step,
                body,
            } => self.numeric_for(depth, name, [init, limit], step.as_ref(), body, *span),
            Stmt::GenericFor {
                span,
                names,
                values,
                body,
            } => self.generic_for(depth, names, values, body, *span),
            Stmt::Label {
                span,
                diagnostic_span,
                name,
                last,
            } => self.label(depth, name, *last, *span, *diagnostic_span),
            Stmt::Goto { span, name } => self.goto(depth, name, *span),
            Stmt::Break { span } => {
                let outer = self.funcs[depth]
                    .loops
                    .last()
                    .map(|ctx| ctx.outer)
                    .ok_or_else(|| {
                        CompileError::new(
                            CompileErrorKind::Syntax,
                            *span,
                            format!("break outside loop at line {}", self.lines.line(span.start)),
                        )
                        .with_diagnostic_line(self.lines.line(self.funcs[depth].span.end))
                    })?;
                let func = &mut self.funcs[depth];
                let site = func.ops.len();
                let left = func.locals[outer.locals..]
                    .iter()
                    .map(|local| local.decl)
                    .collect();
                func.loops
                    .last_mut()
                    .ok_or_else(|| invalid(*span, "missing loop"))?
                    .exits
                    .push((site, *span, left));
                self.emit(depth, Op::Jump { offset: 0 }, *span)?;
                self.emit(depth, Op::Jump { offset: 0 }, *span)?;
                Ok(())
            }
            Stmt::Call { call, .. } => {
                self.call_expr(depth, call, 0)?;
                self.set_free(depth, self.funcs[depth].nact);
                Ok(())
            }
        }
    }

    /// `if` / `elseif` / `else`. Each condition is a single-value context
    /// and each block is a lexical scope. A block that falls through closes
    /// its captured locals, then jumps past the rest of the chain. A block
    /// ending in `return` needs neither: `Return` closes the whole frame.
    fn if_stmt(
        &mut self,
        depth: usize,
        arms: &[(Expr, Block)],
        else_block: Option<&Block>,
        span: Span,
    ) -> Result<(), CompileError> {
        let mut to_end = Vec::new();
        for (index, (cond, block)) in arms.iter().enumerate() {
            // PUC emits a truth-value TEST on the then token (or the
            // immediately following break); comparisons retain their own line.
            let test_span = if condition_has_comparison(cond) {
                cond.span()
            } else if let Some(Stmt::Break { span }) = block.stmts.first() {
                *span
            } else {
                Span::new(block.span.start, block.span.start + 1)
            };
            let skips = self.condition_jumps_at(depth, cond, false, test_span)?;
            // Lines as Lua's: the test on its condition's, the jump past
            // the other arms on the arm's last.
            self.set_free(depth, self.funcs[depth].nact);
            let returns = self.scoped_block(depth, block)?;
            let last = index + 1 == arms.len() && else_block.is_none();
            if !returns && !last {
                to_end.push(self.emit_jump(depth, Op::Jump { offset: 0 }, block.span)?);
            }
            for skip in skips {
                self.patch_here(depth, skip, span)?;
            }
        }
        if let Some(else_block) = else_block {
            self.scoped_block(depth, else_block)?;
        }
        for jump in to_end {
            self.patch_here(depth, jump, span)?;
        }
        Ok(())
    }

    /// `while cond do body end`. The body is a new scope on every iteration,
    /// so its exit (the close of captured locals) runs before the backedge.
    fn while_stmt(
        &mut self,
        depth: usize,
        cond: &Expr,
        body: &Block,
        span: Span,
    ) -> Result<(), CompileError> {
        let top = self.funcs[depth].ops.len();
        let exits = self.condition_jumps(depth, cond, false)?;
        if matches!(cond, Expr::Bool { value: true, .. }) {
            for pc in top..self.funcs[depth].ops.len() {
                self.funcs[depth].line_at.push((pc, body.span.start));
            }
        }
        self.set_free(depth, self.funcs[depth].nact);
        self.push_loop(depth);
        let returns = self.scoped_block(depth, body)?;
        if !returns {
            self.emit_back(depth, Op::Jump { offset: 0 }, top, body.span)?;
        }
        for exit in exits {
            self.patch_here(depth, exit, span)?;
        }
        self.pop_loop(depth, span)
    }

    /// `repeat body until cond`. `cond` is compiled inside the body's scope,
    /// so it sees the body's locals. The scope is left after the test: on
    /// the false edge before jumping back, on the true edge before falling
    /// out. With no captured local both edges are one backward branch.
    fn repeat_stmt(
        &mut self,
        depth: usize,
        body: &Block,
        cond: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        let top = self.funcs[depth].ops.len();
        self.push_loop(depth);
        let scope = self.enter_scope(depth);
        self.block(depth, body)?;
        self.set_free(depth, self.funcs[depth].nact);
        let test_span = if condition_has_comparison(cond) {
            cond.span()
        } else {
            span
        };
        let again = self.condition_jumps_at(depth, cond, false, test_span)?;
        if self.scope_exit(depth, scope).is_some() {
            self.emit_scope_exit(depth, scope, span)?;
            let done = self.emit_jump(depth, Op::Jump { offset: 0 }, span)?;
            for jump in again {
                self.patch_here(depth, jump, span)?;
            }
            self.emit_scope_exit(depth, scope, span)?;
            self.emit_back(depth, Op::Jump { offset: 0 }, top, span)?;
            self.funcs[depth]
                .loops
                .last_mut()
                .ok_or_else(|| invalid(span, "missing loop"))?
                .breaks
                .push(done);
        } else {
            for jump in again {
                self.patch_to(depth, jump, top, span)?;
            }
        }
        self.leave_scope(depth, scope);
        self.pop_loop(depth, span)
    }

    /// `for name = init, limit, step do body end`. The three control
    /// expressions are evaluated once, left to right, into `base..base + 3`
    /// before any loop local exists, so they cannot see `name`. Those three
    /// registers become hidden locals holding the loop's progression; `name`
    /// is the first local of the body scope, at `base + 3`. `ForPrep` skips
    /// the loop or starts it; the body's scope exit (closing a captured
    /// `name` or body local) runs before each `ForLoop`.
    fn numeric_for(
        &mut self,
        depth: usize,
        name: &Name,
        [init, limit]: [&Expr; 2],
        step: Option<&Expr>,
        body: &Block,
        span: Span,
    ) -> Result<(), CompileError> {
        let outer = self.enter_scope(depth);
        let base = self.funcs[depth].nact;
        self.expr_to(depth, init, base)?;
        self.expr_to(depth, limit, slot(base, 1, span)?)?;
        let step_reg = slot(base, 2, span)?;
        match step {
            Some(step) => self.expr_to(depth, step, step_reg)?,
            None => {
                self.emit(
                    depth,
                    Op::LoadInt {
                        dst: step_reg,
                        value: 1,
                    },
                    limit.span(),
                )?;
                self.set_free(depth, slot(base, 3, span)?);
            }
        }
        for _ in 0..3 {
            self.define(
                depth,
                &Name {
                    bytes: b"(for state)".to_vec(),
                    span,
                },
            )?;
        }
        let prep = self.emit_jump(depth, Op::ForPrep { base, offset: 0 }, span)?;
        self.push_loop(depth);
        let body_top = self.funcs[depth].ops.len();
        let scope = self.enter_scope(depth);
        self.define(depth, name)?;
        self.block(depth, body)?;
        if !matches!(body.stmts.last(), Some(Stmt::Return { .. })) {
            self.emit_scope_exit(depth, scope, body.span)?;
        }
        self.leave_scope(depth, scope);
        self.emit_back(depth, Op::ForLoop { base, offset: 0 }, body_top, span)?;
        self.patch_here(depth, prep, span)?;
        self.pop_loop(depth, span)?;
        self.leave_scope(depth, outer);
        Ok(())
    }

    /// `for n1, ..., nk in explist do body end` (ADR 0027). `explist` is
    /// adjusted to exactly four values, as a `local` list would be, into
    /// `base..base + 4` before any loop local exists: iterator, state,
    /// control, closing value. They become hidden locals, and the fourth is
    /// a hidden `<close>` local for the whole loop. Each iteration copies
    /// the iterator, state, and control to `base + 4`, calls the copy with an
    /// ordinary `Call` wanting `k` results, which land on the loop
    /// variables, and `GenericForLoop` ends the loop on a nil first result
    /// or saves it as the next control. The loop variables are the body
    /// scope's first locals, so the body's scope exit closes captured ones
    /// before the next call overwrites them. Leaving the loop, by its end
    /// or by `break`, is one scope exit from `base`, which closes the
    /// closing value after any body local.
    fn generic_for(
        &mut self,
        depth: usize,
        names: &[Name],
        values: &[Expr],
        body: &Block,
        span: Span,
    ) -> Result<(), CompileError> {
        let outer = self.enter_scope(depth);
        let base = self.place_values(depth, values, 4, span)?;
        if base != outer.nact {
            return Err(invalid(span, "generic for state is misplaced"));
        }
        for _ in 0..4 {
            self.define(
                depth,
                &Name {
                    bytes: b"(for state)".to_vec(),
                    span,
                },
            )?;
        }
        let closing = slot(base, 3, span)?;
        self.funcs[depth]
            .locals
            .last_mut()
            .ok_or_else(|| invalid(span, "missing for state"))?
            .close = true;
        {
            let pc = self.funcs[depth].ops.len();
            self.funcs[depth].line_at.push((pc, span.start));
        }
        self.emit(depth, Op::MarkClose { reg: closing }, span)?;
        {
            let pc = self.funcs[depth].ops.len();
            self.funcs[depth].line_at.push((pc, span.start));
        }
        let prep = self.emit_jump(depth, Op::Jump { offset: 0 }, span)?;
        // `break` leaves the hidden values too.
        self.funcs[depth].loops.push(LoopCtx {
            outer,
            breaks: Vec::new(),
            exits: Vec::new(),
        });
        let body_top = self.funcs[depth].ops.len();
        let scope = self.enter_scope(depth);
        for name in names {
            self.define(depth, name)?;
        }
        self.block(depth, body)?;
        if !matches!(body.stmts.last(), Some(Stmt::Return { .. })) {
            self.emit_scope_exit(depth, scope, body.span)?;
        }
        self.leave_scope(depth, scope);
        self.patch_here(depth, prep, span)?;
        let call = slot(base, 4, span)?;
        for offset in 0..3 {
            {
                let pc = self.funcs[depth].ops.len();
                self.funcs[depth].line_at.push((pc, span.start));
            }
            self.emit(
                depth,
                Op::Move {
                    dst: slot(call, offset, span)?,
                    src: slot(base, offset, span)?,
                },
                span,
            )?;
        }
        let args_end = slot(call, 3, span)?;
        if args_end > MAX_REGISTERS {
            return Err(limit(span, "register limit"));
        }
        self.set_free(depth, args_end);
        let nresults = u8::try_from(names.len()).map_err(|_| limit(span, "too many variables"))?;
        self.note_call(
            depth,
            Some((
                crate::debuginfo::NameKind::ForIterator,
                b"for iterator".to_vec(),
            )),
        );
        // PUC attributes the iterator call to the first expression after
        // `in`, which can be on a different line from `for`.
        let iterator_span = values.first().map_or(span, Expr::span);
        let pc = self.funcs[depth].ops.len();
        self.funcs[depth].line_at.push((pc, iterator_span.start));
        self.emit(
            depth,
            Op::Call {
                func: call,
                nargs: 2,
                nresults,
            },
            iterator_span,
        )?;
        self.emit_back(
            depth,
            Op::GenericForLoop { base, offset: 0 },
            body_top,
            span,
        )?;
        self.emit_scope_exit(depth, outer, span)?;
        self.pop_loop(depth, span)?;
        self.leave_scope(depth, outer);
        Ok(())
    }

    fn push_loop(&mut self, depth: usize) {
        let outer = self.mark(depth);
        self.funcs[depth].loops.push(LoopCtx {
            outer,
            breaks: Vec::new(),
            exits: Vec::new(),
        });
    }

    /// Patch the innermost loop's `break`s to the next instruction.
    fn pop_loop(&mut self, depth: usize, span: Span) -> Result<(), CompileError> {
        let ctx = self.funcs[depth]
            .loops
            .pop()
            .ok_or_else(|| invalid(span, "missing loop"))?;
        for jump in ctx.breaks {
            self.patch_here(depth, jump, span)?;
        }
        let func = &mut self.funcs[depth];
        let target = func.ops.len();
        for (site, span, left) in ctx.exits {
            func.edges.push(GotoEdge {
                span,
                site,
                target,
                left,
            });
        }
        Ok(())
    }

    /// Emit a branch aimed back at `target`.
    fn emit_back(
        &mut self,
        depth: usize,
        mut op: Op,
        target: usize,
        span: Span,
    ) -> Result<(), CompileError> {
        let pc = self.funcs[depth].ops.len();
        let offset = i64::try_from(target)
            .ok()
            .zip(i64::try_from(pc + 1).ok())
            .and_then(|(target, next)| i32::try_from(target - next).ok())
            .ok_or_else(|| limit(span, "jump distance"))?;
        *branch_offset(&mut op).ok_or_else(|| invalid(span, "backward edge is not a jump"))? =
            offset;
        self.emit(depth, op, span)?;
        Ok(())
    }

    /// Compile `block` in its own scope and leave it. Returns true when the
    /// block ends in `return`, so no fallthrough exit was emitted.
    fn scoped_block(&mut self, depth: usize, block: &Block) -> Result<bool, CompileError> {
        let scope = self.enter_scope(depth);
        self.block(depth, block)?;
        let returns = matches!(block.stmts.last(), Some(Stmt::Return { .. }));
        if !returns {
            self.emit_scope_exit(depth, scope, block.span)?;
        }
        self.leave_scope(depth, scope);
        Ok(returns)
    }

    /// Begin a block: a lexical scope for locals and labels. Every block
    /// ends with `leave_scope`.
    fn enter_scope(&mut self, depth: usize) -> Scope {
        let scope = self.mark(depth);
        let func = &mut self.funcs[depth];
        func.blocks.push(BlockMark {
            labels: func.labels.len(),
            gotos: func.gotos.len(),
            locals: scope.locals,
        });
        scope
    }

    /// Where the locals and registers stand, for an exit edge to leave.
    fn mark(&self, depth: usize) -> Scope {
        let func = &self.funcs[depth];
        Scope {
            locals: func.locals.len(),
            nact: func.nact,
        }
    }

    /// Emit what a control edge needs to leave every local declared since
    /// `scope`. Every exit edge, fallthrough or jump, goes through here.
    fn emit_scope_exit(
        &mut self,
        depth: usize,
        scope: Scope,
        span: Span,
    ) -> Result<(), CompileError> {
        if let Some(op) = self.scope_exit(depth, scope) {
            self.emit(depth, op, span)?;
        }
        Ok(())
    }

    /// The instruction that leaves every local declared since `scope`:
    /// `CloseScope` from the scope's first register when one of them is a
    /// `<close>` local, which closes captured locals too; `CloseUpvalues`
    /// when one is only captured by a closure; nothing otherwise.
    fn scope_exit(&self, depth: usize, scope: Scope) -> Option<Op> {
        let locals = &self.funcs[depth].locals[scope.locals..];
        exit_op(
            scope.nact,
            locals.iter().any(|local| local.close),
            locals.iter().any(|local| local.captured),
        )
    }

    /// Forget the scope's locals and labels and return the register
    /// frontier to the scope's entry, so later locals reuse its registers.
    /// Gotos still waiting for a label leave the block: they are no longer
    /// in scope of its locals.
    fn leave_scope(&mut self, depth: usize, scope: Scope) {
        let func = &mut self.funcs[depth];
        if let Some(mark) = func.blocks.pop() {
            func.labels.truncate(mark.labels);
            for pending in &mut func.gotos[mark.gotos..] {
                pending.frontier = pending.frontier.min(scope.locals);
            }
        }
        func.retire_locals(scope.locals);
        func.end_locals(scope.locals);
        func.locals.truncate(scope.locals);
        func.nact = scope.nact;
        self.set_free(depth, scope.nact);
    }

    /// `::name::`. A label is visible in the rest of its block and the
    /// blocks inside it, not in nested functions, and may not repeat a
    /// visible one. It answers the gotos of its block waiting for it,
    /// including those that left blocks inside it, unless one would enter
    /// the scope of a local declared since it (ADR 0030).
    fn label(
        &mut self,
        depth: usize,
        name: &Name,
        last: bool,
        span: Span,
        diagnostic_span: Span,
    ) -> Result<(), CompileError> {
        let func = &mut self.funcs[depth];
        if let Some(first) = func.labels.iter().find(|label| label.name == name.bytes) {
            return Err(CompileError::new(
                CompileErrorKind::Syntax,
                span,
                format!(
                    "label '{}' already defined on line {}",
                    String::from_utf8_lossy(&name.bytes),
                    first.line
                ),
            )
            .with_diagnostic_line(self.lines.line(diagnostic_span.start)));
        }
        let block = func.blocks.last().copied().unwrap_or(BlockMark {
            labels: 0,
            gotos: 0,
            locals: 0,
        });
        let frontier = if last {
            block.locals
        } else {
            func.locals.len()
        };
        let pc = func.ops.len();
        func.labels.push(LabelRec {
            name: name.bytes.clone(),
            line: self.lines.line(span.start),
            pc,
            frontier,
        });
        let mut index = block.gotos;
        while index < func.gotos.len() {
            if func.gotos[index].name != name.bytes {
                index += 1;
                continue;
            }
            let pending = func.gotos.remove(index);
            if pending.frontier < frontier {
                let local = &func.locals[pending.frontier];
                return Err(CompileError::new(
                    CompileErrorKind::Syntax,
                    pending.span,
                    format!(
                        "<goto {}> at line {} jumps into the scope of local '{}'",
                        String::from_utf8_lossy(&name.bytes),
                        self.lines.line(pending.span.start),
                        String::from_utf8_lossy(&local.name)
                    ),
                )
                .with_diagnostic_line(self.lines.line(diagnostic_span.start)));
            }
            func.edges.push(GotoEdge {
                span: pending.span,
                site: pending.site,
                target: pc,
                left: pending.active.get(frontier..).unwrap_or_default().to_vec(),
            });
        }
        Ok(())
    }

    /// `goto name`: two slots, filled when the function ends. A visible
    /// label resolves it now; otherwise it waits for a label later in its
    /// block or an enclosing one.
    fn goto(&mut self, depth: usize, name: &Name, span: Span) -> Result<(), CompileError> {
        let func = &mut self.funcs[depth];
        let site = func.ops.len();
        let active: Vec<usize> = func.locals.iter().map(|local| local.decl).collect();
        match func
            .labels
            .iter()
            .rev()
            .find(|label| label.name == name.bytes)
        {
            Some(label) => {
                let (target, frontier) = (label.pc, label.frontier);
                func.edges.push(GotoEdge {
                    span,
                    site,
                    target,
                    left: active.get(frontier..).unwrap_or_default().to_vec(),
                });
            }
            None => func.gotos.push(GotoRec {
                name: name.bytes.clone(),
                span,
                site,
                frontier: active.len(),
                active,
            }),
        }
        self.emit(depth, Op::Jump { offset: 0 }, span)?;
        self.emit(depth, Op::Jump { offset: 0 }, span)?;
        Ok(())
    }

    fn emit_jump(&mut self, depth: usize, op: Op, span: Span) -> Result<usize, CompileError> {
        let pc = self.funcs[depth].ops.len();
        self.emit(depth, op, span)?;
        Ok(pc)
    }

    /// Point the jump at `pc` to the next instruction to be emitted.
    fn patch_here(&mut self, depth: usize, pc: usize, span: Span) -> Result<(), CompileError> {
        let target = self.funcs[depth].ops.len();
        self.patch_to(depth, pc, target, span)
    }

    fn patch_to(
        &mut self,
        depth: usize,
        pc: usize,
        target: usize,
        span: Span,
    ) -> Result<(), CompileError> {
        let func = &mut self.funcs[depth];
        let offset = i64::try_from(target)
            .ok()
            .zip(i64::try_from(pc + 1).ok())
            .and_then(|(target, next)| i32::try_from(target - next).ok())
            .ok_or_else(|| limit(span, "jump distance"))?;
        *branch_offset(&mut func.ops[pc])
            .ok_or_else(|| invalid(span, "patched instruction is not a jump"))? = offset;
        Ok(())
    }

    fn ret(&mut self, depth: usize, values: &[Expr], span: Span) -> Result<(), CompileError> {
        if values.is_empty() {
            self.emit(depth, Op::Return { base: 0, count: 0 }, span)?;
            return Ok(());
        }
        // `return f(args)`, one call and not in parentheses, is a tail call
        // unless a `<close>` local is in scope: its close runs after the
        // call returns, so the frame must stay (ADR 0029). A generic `for`'s
        // closing value is such a local.
        if let [call @ Expr::Call { .. }] = values
            && !self.funcs[depth].locals.iter().any(|local| local.close)
        {
            let name = self.call_name(depth, call);
            let (func, nargs) = self.call_window(depth, call)?;
            self.note_call(depth, name);
            self.note_call_line(depth, call);
            self.emit(depth, Op::TailCall { func, nargs }, call.span())?;
            self.emit(
                depth,
                Op::Return {
                    base: func,
                    count: COUNT_OPEN,
                },
                span,
            )?;
            return Ok(());
        }
        if let [Expr::Chain { first, rest, .. }] = values
            && !self.funcs[depth].locals.iter().any(|local| local.close)
            && let Some((
                ChainStep::Call {
                    span: call_span,
                    method,
                    args,
                },
                prefix,
            )) = rest.split_last()
        {
            let reg = self.chain_expr(depth, first, prefix, None, 1)?;
            if reg >= self.funcs[depth].nact {
                self.set_free(depth, reg);
            }
            let call = Expr::Call {
                span: *call_span,
                func: Box::new(Expr::Resolved {
                    span: prefix.last().map_or(first.span(), ChainStep::span),
                    reg,
                }),
                method: method.clone(),
                args: args.clone(),
            };
            let name = self.call_name(depth, &call);
            let (func, nargs) = self.call_window(depth, &call)?;
            self.note_call(depth, name);
            self.note_call_line(depth, &call);
            self.emit(depth, Op::TailCall { func, nargs }, *call_span)?;
            self.emit(
                depth,
                Op::Return {
                    base: func,
                    count: COUNT_OPEN,
                },
                span,
            )?;
            return Ok(());
        }
        if let Some((base, count)) = self.direct_locals(depth, values)? {
            self.emit(depth, Op::Return { base, count }, span)?;
            return Ok(());
        }
        let base = self.funcs[depth].free;
        if is_multret(values.last().expect("return has a value")) && values.len() > 1 {
            for (index, expr) in values.iter().take(values.len() - 1).enumerate() {
                self.expr_to(depth, expr, slot(base, index, expr.span())?)?;
            }
            self.multi(depth, values.last().expect("last"), COUNT_OPEN)?;
            self.emit(
                depth,
                Op::Return {
                    base,
                    count: COUNT_OPEN,
                },
                span,
            )?;
            return Ok(());
        }
        if is_multret(&values[0]) && values.len() == 1 {
            self.multi(depth, &values[0], COUNT_OPEN)?;
            self.emit(
                depth,
                Op::Return {
                    base,
                    count: COUNT_OPEN,
                },
                span,
            )?;
            return Ok(());
        }
        self.place_values(depth, values, values.len(), span)?;
        let count =
            u8::try_from(values.len()).map_err(|_| limit(span, "too many return values"))?;
        self.emit(depth, Op::Return { base, count }, span)?;
        Ok(())
    }

    fn direct_locals(
        &mut self,
        depth: usize,
        values: &[Expr],
    ) -> Result<Option<(u8, u8)>, CompileError> {
        let mut regs = Vec::with_capacity(values.len());
        for expr in values {
            let Expr::Name { bytes, span } = expr else {
                return Ok(None);
            };
            match self.resolve(depth, bytes, *span)? {
                Some(Place::Local(reg)) => regs.push(reg),
                _ => return Ok(None),
            }
        }
        let Some(&start) = regs.first() else {
            return Ok(None);
        };
        let consecutive = regs
            .iter()
            .enumerate()
            .all(|(index, reg)| usize::from(*reg) == usize::from(start) + index);
        if !consecutive {
            return Ok(None);
        }
        let count =
            u8::try_from(regs.len()).map_err(|_| limit(values[0].span(), "return width"))?;
        Ok(Some((start, count)))
    }

    fn place_values(
        &mut self,
        depth: usize,
        values: &[Expr],
        nslots: usize,
        span: Span,
    ) -> Result<u8, CompileError> {
        let base = self.funcs[depth].free;
        if values.is_empty() {
            for index in 0..nslots {
                let reg = slot(base, index, span)?;
                self.emit(depth, Op::LoadNil { dst: reg }, span)?;
                self.set_free(
                    depth,
                    reg.checked_add(1)
                        .ok_or_else(|| limit(span, "register limit"))?,
                );
            }
            return Ok(base);
        }
        let last = values.len() - 1;
        for (index, expr) in values.iter().enumerate().take(last) {
            if index < nslots {
                self.expr_to(depth, expr, slot(base, index, expr.span())?)?;
            } else {
                self.discard(depth, expr)?;
            }
        }
        let filled = last.min(nslots);
        let remaining = nslots - filled;
        let last_expr = &values[last];
        if remaining == 0 {
            self.discard(depth, last_expr)?;
        } else if is_multret(last_expr) {
            let dest = slot(base, filled, last_expr.span())?;
            if self.funcs[depth].free != dest {
                return Err(invalid(
                    last_expr.span(),
                    "call was not placed on its results",
                ));
            }
            let nresults =
                u8::try_from(remaining).map_err(|_| limit(last_expr.span(), "too many results"))?;
            self.multi(depth, last_expr, nresults)?;
        } else {
            let first = self.funcs[depth].ops.len();
            self.expr_to(depth, last_expr, slot(base, filled, last_expr.span())?)?;
            self.note_result_line(depth, first, last_expr.span().end);
            for offset in 1..remaining {
                let reg = slot(base, filled + offset, last_expr.span())?;
                self.emit(depth, Op::LoadNil { dst: reg }, last_expr.span())?;
                self.set_free(
                    depth,
                    reg.checked_add(1)
                        .ok_or_else(|| limit(last_expr.span(), "register limit"))?,
                );
            }
        }
        Ok(base)
    }

    fn discard(&mut self, depth: usize, expr: &Expr) -> Result<(), CompileError> {
        let saved = self.funcs[depth].free;
        if let Expr::Vararg { span } = expr {
            // Nothing to evaluate, but it must be legal here.
            self.vararg_allowed(depth, *span)?;
        } else if is_multret(expr) {
            self.multi(depth, expr, 0)?;
        } else {
            let _ = self.expr_r(depth, expr)?;
        }
        self.set_free(depth, saved);
        Ok(())
    }

    fn expr_to(&mut self, depth: usize, expr: &Expr, want: u8) -> Result<(), CompileError> {
        #[cfg(test)]
        crate::limits::note_stack();
        if want >= MAX_REGISTERS {
            return Err(limit(expr.span(), "register limit"));
        }
        if self.funcs[depth].free != want {
            return Err(invalid(
                expr.span(),
                "expression was not compiled into its register",
            ));
        }
        let start_pc = self.funcs[depth].ops.len();
        let result = match expr {
            Expr::Paren { inner, .. } => self.expr_to(depth, inner, want),
            Expr::Call { .. } | Expr::Vararg { .. } => self.multi(depth, expr, 1).map(|_| ()),
            Expr::Chain { rest, .. } if matches!(rest.last(), Some(ChainStep::Call { .. })) => {
                self.multi(depth, expr, 1).map(|_| ())
            }
            _ => {
                let got = self.expr_destination(depth, expr, Some(want))?;
                if got != want {
                    self.emit(
                        depth,
                        Op::Move {
                            dst: want,
                            src: got,
                        },
                        expr.span(),
                    )?;
                }
                let next = want
                    .checked_add(1)
                    .ok_or_else(|| limit(expr.span(), "register limit"))?;
                self.set_free(depth, next);
                Ok(())
            }
        };
        if literal_arithmetic(expr) && !matches!(expr, Expr::Integer { .. } | Expr::Float { .. }) {
            let offset = expr.span().end.saturating_sub(1);
            for pc in start_pc..self.funcs[depth].ops.len() {
                self.funcs[depth].line_at.push((pc, offset));
            }
        }
        result
    }

    fn expr_r(&mut self, depth: usize, expr: &Expr) -> Result<u8, CompileError> {
        self.expr_destination(depth, expr, None)
    }

    /// Only the final arithmetic operation may use the requested free slot.
    /// Its operands keep their ordinary temporary lifetimes and evaluation order.
    fn expr_destination(
        &mut self,
        depth: usize,
        expr: &Expr,
        want: Option<u8>,
    ) -> Result<u8, CompileError> {
        #[cfg(test)]
        crate::limits::note_stack();
        match expr {
            Expr::Resolved { reg, .. } => Ok(*reg),
            Expr::Chain { first, rest, .. } => self.chain_expr(depth, first, rest, want, 1),
            Expr::Nil { span } => {
                let reg = self.alloc(depth, *span)?;
                self.emit(depth, Op::LoadNil { dst: reg }, *span)?;
                Ok(reg)
            }
            Expr::Bool { span, value } => {
                let reg = self.alloc(depth, *span)?;
                self.emit(
                    depth,
                    Op::LoadBool {
                        dst: reg,
                        value: *value,
                    },
                    *span,
                )?;
                Ok(reg)
            }
            Expr::Integer { span, value } => {
                let reg = self.alloc(depth, *span)?;
                self.emit(
                    depth,
                    Op::LoadInt {
                        dst: reg,
                        value: *value,
                    },
                    *span,
                )?;
                Ok(reg)
            }
            Expr::Float { span, bits } => {
                let reg = self.alloc(depth, *span)?;
                self.emit(
                    depth,
                    Op::LoadFloat {
                        dst: reg,
                        bits: *bits,
                    },
                    *span,
                )?;
                Ok(reg)
            }
            Expr::Str { span, bytes } => {
                let const_index = self.const_index(depth, bytes, *span)?;
                let reg = self.alloc(depth, *span)?;
                self.emit(
                    depth,
                    Op::LoadBytes {
                        dst: reg,
                        const_index,
                    },
                    *span,
                )?;
                Ok(reg)
            }
            Expr::Name { span, bytes } => match self.resolve(depth, bytes, *span)? {
                Some(Place::Local(reg)) => Ok(reg),
                Some(Place::Upvalue(index)) => {
                    let reg = self.alloc(depth, *span)?;
                    self.emit(depth, Op::GetUpvalue { dst: reg, index }, *span)?;
                    Ok(reg)
                }
                None => self.global_get(depth, bytes, *span),
            },
            Expr::Index { span, base, key } => self.index_expr(depth, base, key, *span),
            Expr::Table { span, fields } => self.table_expr(depth, fields, *span),
            Expr::Paren { inner, .. } => self.expr_r(depth, inner),
            Expr::Function {
                span,
                params,
                vararg,
                body,
            } => {
                let child = self.function(depth, params, *vararg, body, *span)?;
                let reg = self.alloc(depth, *span)?;
                self.emit(depth, Op::MakeClosure { dst: reg, child }, *span)?;
                Ok(reg)
            }
            Expr::Call { .. } | Expr::Vararg { .. } => self.multi(depth, expr, 1),
            Expr::Binary {
                op:
                    op @ (BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Idiv
                    | BinOp::Mod
                    | BinOp::Pow
                    | BinOp::Band
                    | BinOp::Bor
                    | BinOp::Bxor
                    | BinOp::Shl
                    | BinOp::Shr
                    | BinOp::Concat),
                left,
                right,
                span,
            } => {
                let immediate = if *op == BinOp::Concat {
                    None
                } else {
                    integer_literal(right)
                        .map(|k| (left.as_ref(), k, false))
                        .or_else(|| integer_literal(left).map(|k| (right.as_ref(), k, true)))
                };
                if let Some((expr, constant, reverse)) = immediate {
                    // Literals have no effects. Evaluate the remaining operand
                    // before writing the destination, as the register form does.
                    let operand_pc = self.funcs[depth].ops.len();
                    let reg = self.expr_r(depth, expr)?;
                    if !reverse {
                        self.note_infix_operand(depth, operand_pc, left.span().end);
                    }
                    let dst = if let Some(want) = want {
                        want
                    } else if reg >= self.funcs[depth].nact {
                        reg
                    } else {
                        self.alloc(depth, *span)?
                    };
                    let kind = match op {
                        BinOp::Add => ArithOp::Add,
                        BinOp::Sub => ArithOp::Sub,
                        BinOp::Mul => ArithOp::Mul,
                        BinOp::Div => ArithOp::Div,
                        BinOp::Idiv => ArithOp::Idiv,
                        BinOp::Mod => ArithOp::Mod,
                        BinOp::Pow => ArithOp::Pow,
                        BinOp::Band => ArithOp::Band,
                        BinOp::Bor => ArithOp::Bor,
                        BinOp::Bxor => ArithOp::Bxor,
                        BinOp::Shl => ArithOp::Shl,
                        _ => ArithOp::Shr,
                    };
                    self.line_after(depth, left.span().end);
                    self.emit(
                        depth,
                        Op::ArithK {
                            op: kind,
                            dst,
                            reg,
                            constant,
                            reverse,
                        },
                        *span,
                    )?;
                    let next = dst
                        .checked_add(1)
                        .ok_or_else(|| limit(*span, "register limit"))?;
                    self.set_free(depth, next);
                    return Ok(dst);
                }
                let operand_pc = self.funcs[depth].ops.len();
                let a = self.expr_r(depth, left)?;
                self.note_infix_operand(depth, operand_pc, left.span().end);
                let b = self.expr_r(depth, right)?;
                let dst = if let Some(want) = want.filter(|_| *op != BinOp::Concat) {
                    want
                } else if a >= self.funcs[depth].nact {
                    a
                } else {
                    self.alloc(depth, *span)?
                };
                let arith = |op| Op::Arith { op, dst, a, b };
                let code = match op {
                    BinOp::Add => Op::Add { dst, a, b },
                    BinOp::Concat => Op::Concat { dst, a, b },
                    BinOp::Sub => arith(ArithOp::Sub),
                    BinOp::Mul => arith(ArithOp::Mul),
                    BinOp::Div => arith(ArithOp::Div),
                    BinOp::Idiv => arith(ArithOp::Idiv),
                    BinOp::Mod => arith(ArithOp::Mod),
                    BinOp::Pow => arith(ArithOp::Pow),
                    BinOp::Band => arith(ArithOp::Band),
                    BinOp::Bor => arith(ArithOp::Bor),
                    BinOp::Bxor => arith(ArithOp::Bxor),
                    BinOp::Shl => arith(ArithOp::Shl),
                    _ => arith(ArithOp::Shr),
                };
                self.line_after(depth, left.span().end);
                self.emit(depth, code, *span)?;
                let next = dst
                    .checked_add(1)
                    .ok_or_else(|| limit(*span, "register limit"))?;
                self.set_free(depth, next);
                Ok(dst)
            }
            Expr::Binary {
                op: op @ (BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge),
                left,
                right,
                span,
            } => {
                let (kind, swap) = comparison_kind(*op);
                let left_reg = self.expr_r(depth, left)?;
                let right_reg = self.expr_r(depth, right)?;
                let (a, b) = if swap {
                    (right_reg, left_reg)
                } else {
                    (left_reg, right_reg)
                };
                let dst = if left_reg >= self.funcs[depth].nact {
                    left_reg
                } else {
                    self.alloc(depth, *span)?
                };
                self.emit(depth, Op::Compare { kind, dst, a, b }, *span)?;
                let next = dst
                    .checked_add(1)
                    .ok_or_else(|| limit(*span, "register limit"))?;
                self.set_free(depth, next);
                Ok(dst)
            }
            Expr::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                left,
                right,
                span,
            } => self.logical(depth, *op == BinOp::And, left, right, *span),
            Expr::Unary {
                span,
                op: op @ (UnOp::Neg | UnOp::Bnot),
                expr,
            } => {
                let src = self.expr_r(depth, expr)?;
                let dst = if src >= self.funcs[depth].nact {
                    src
                } else {
                    self.alloc(depth, *span)?
                };
                let code = if *op == UnOp::Neg {
                    Op::Neg { dst, src }
                } else {
                    Op::BNot { dst, src }
                };
                self.line_after(depth, span.start);
                self.emit(depth, code, *span)?;
                let next = dst
                    .checked_add(1)
                    .ok_or_else(|| limit(*span, "register limit"))?;
                self.set_free(depth, next);
                Ok(dst)
            }
            Expr::Unary {
                span,
                op: UnOp::Len,
                expr,
            } => {
                let src = self.expr_r(depth, expr)?;
                let dst = self.temp_or_alloc(depth, src, *span)?;
                self.line_after(depth, span.start);
                self.finish_at(depth, dst, Op::Len { dst, src }, *span)
            }
            Expr::Unary {
                span,
                op: UnOp::Not,
                expr,
            } => self.not_expr(depth, expr, *span),
        }
    }

    /// A condition whose result is used only by a branch. Select from the
    /// expression tree before emitting code, so no existing PC/debug range
    /// or alternate entry is removed. Value-producing comparisons keep Compare.
    fn condition_jumps(
        &mut self,
        depth: usize,
        expr: &Expr,
        sense: bool,
    ) -> Result<Vec<usize>, CompileError> {
        self.condition_jumps_at(depth, expr, sense, expr.span())
    }

    fn condition_jumps_at(
        &mut self,
        depth: usize,
        expr: &Expr,
        sense: bool,
        span: Span,
    ) -> Result<Vec<usize>, CompileError> {
        if condition_has_comparison(expr) {
            match expr {
                Expr::Paren { inner, .. } => {
                    return self.condition_jumps_at(depth, inner, sense, span);
                }
                Expr::Unary {
                    op: UnOp::Not,
                    expr,
                    ..
                } => return self.condition_jumps_at(depth, expr, !sense, span),
                Expr::Binary {
                    op: op @ (BinOp::And | BinOp::Or),
                    left,
                    right,
                    ..
                } => {
                    // And's false edges (or Or's true edges) share a target.
                    // The opposite sense skips the right operand when the left
                    // already decides the condition. No logical value is needed.
                    let direct = (*op == BinOp::And) != sense;
                    let mut left_jumps =
                        self.condition_jumps(depth, left, if direct { sense } else { !sense })?;
                    self.set_free(depth, self.funcs[depth].nact);
                    let right_jumps = self.condition_jumps(depth, right, sense)?;
                    if direct {
                        left_jumps.extend(right_jumps);
                        return Ok(left_jumps);
                    }
                    for jump in left_jumps {
                        self.patch_here(depth, jump, expr.span())?;
                    }
                    return Ok(right_jumps);
                }
                _ => {}
            }
        }
        let op = self.condition_op(depth, expr, sense)?;
        if let Op::JumpIfFalse { src, .. } = op
            && sense
        {
            self.emit(depth, Op::JumpIfFalse { src, offset: 1 }, span)?;
            return Ok(vec![self.emit_jump(depth, Op::Jump { offset: 0 }, span)?]);
        }
        Ok(vec![self.emit_jump(depth, op, span)?])
    }

    fn condition_op(&mut self, depth: usize, expr: &Expr, sense: bool) -> Result<Op, CompileError> {
        if let Expr::Binary {
            op: op @ (BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge),
            left,
            right,
            ..
        } = expr
        {
            let (kind, swap) = comparison_kind(*op);
            // Evaluate operands left to right, then swap for > / >=, just
            // as the value-producing path does.
            let left = self.expr_r(depth, left)?;
            let right = self.expr_r(depth, right)?;
            let (a, b) = if swap { (right, left) } else { (left, right) };
            return Ok(Op::CompareBranch {
                kind,
                a,
                b,
                sense,
                offset: 0,
            });
        }
        let src = self.expr_r(depth, expr)?;
        Ok(Op::JumpIfFalse { src, offset: 0 })
    }

    /// `a and b`, `a or b`: one value, in a new temporary. `a` is evaluated
    /// once, into it. When `a` decides the result (a false `a` for `and`, a
    /// true one for `or`), a jump skips `b`; otherwise `b` overwrites it.
    /// Lua truth decides, and no metamethod is involved. A call on either
    /// side gives one value.
    fn logical(
        &mut self,
        depth: usize,
        and: bool,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<u8, CompileError> {
        let dst = self.funcs[depth].free;
        let operand_pc = self.funcs[depth].ops.len();
        self.expr_to(depth, left, dst)?;
        self.note_infix_operand(depth, operand_pc, left.span().end);
        let branch_start = self.funcs[depth].ops.len();
        let skip = if and {
            self.emit_jump(
                depth,
                Op::JumpIfFalse {
                    src: dst,
                    offset: 0,
                },
                span,
            )?
        } else {
            // There is no jump on a true value: a false one jumps over the
            // jump that skips `b`.
            self.emit(
                depth,
                Op::JumpIfFalse {
                    src: dst,
                    offset: 1,
                },
                span,
            )?;
            self.emit_jump(depth, Op::Jump { offset: 0 }, span)?
        };
        for pc in branch_start..self.funcs[depth].ops.len() {
            self.funcs[depth].line_at.push((pc, left.span().start));
        }
        self.set_free(depth, dst);
        let first = self.funcs[depth].ops.len();
        self.expr_to(depth, right, dst)?;
        self.note_result_line(depth, first, right.span().end);
        self.patch_here(depth, skip, span)?;
        Ok(dst)
    }

    /// `not a`: `true` or `false` by Lua truth, in a new temporary. No
    /// metamethod is involved.
    fn not_expr(&mut self, depth: usize, expr: &Expr, span: Span) -> Result<u8, CompileError> {
        let src = self.expr_r(depth, expr)?;
        let dst = self.temp_or_alloc(depth, src, span)?;
        self.emit(depth, Op::JumpIfFalse { src, offset: 2 }, span)?;
        self.emit(depth, Op::LoadBool { dst, value: false }, span)?;
        self.emit(depth, Op::Jump { offset: 1 }, span)?;
        self.emit(depth, Op::LoadBool { dst, value: true }, span)?;
        let next = dst
            .checked_add(1)
            .ok_or_else(|| limit(span, "register limit"))?;
        self.set_free(depth, next);
        Ok(dst)
    }

    /// Lower a flat left spine in source order. A temporary result can be
    /// reused as the next call/logical destination, so the register window
    /// does not grow with chain length.
    fn chain_expr(
        &mut self,
        depth: usize,
        first: &Expr,
        rest: &[ChainStep],
        want: Option<u8>,
        nresults: u8,
    ) -> Result<u8, CompileError> {
        fn needs_slot(step: &ChainStep) -> bool {
            matches!(
                step,
                ChainStep::Call { .. }
                    | ChainStep::Binary {
                        op: BinOp::And | BinOp::Or,
                        ..
                    }
            )
        }
        let mut reg = self.expr_r(depth, first)?;
        let mut preceding_span = first.span();
        for (index, step) in rest.iter().enumerate() {
            if needs_slot(step) {
                let slot = if reg >= self.funcs[depth].nact {
                    reg
                } else {
                    self.funcs[depth].free
                };
                self.set_free(depth, slot);
            }
            let value = Expr::Resolved {
                span: preceding_span,
                reg,
            };
            let expr = match step {
                ChainStep::Binary { span, op, right } => Expr::Binary {
                    span: *span,
                    op: *op,
                    left: Box::new(value),
                    right: Box::new(right.clone()),
                },
                ChainStep::Index { span, key } => Expr::Index {
                    span: *span,
                    base: Box::new(value),
                    key: Box::new(key.clone()),
                },
                ChainStep::Call { span, method, args } => Expr::Call {
                    span: *span,
                    func: Box::new(value),
                    method: method.clone(),
                    args: args.clone(),
                },
            };
            let last = index + 1 == rest.len();
            reg = if last && matches!(step, ChainStep::Call { .. }) && nresults != 1 {
                self.call_expr(depth, &expr, nresults)?
            } else {
                self.expr_destination(depth, &expr, if last { want } else { None })?
            };
            preceding_span = step.span();
        }
        Ok(reg)
    }

    /// A call or `...` producing `nresults` values at the first free
    /// register, or all of them with `COUNT_OPEN`, which leaves `top` past
    /// the last. Both are adjusted by the same result-window rules.
    fn multi(&mut self, depth: usize, expr: &Expr, nresults: u8) -> Result<u8, CompileError> {
        if let Expr::Chain { first, rest, .. } = expr {
            return self.chain_expr(depth, first, rest, None, nresults);
        }
        let Expr::Vararg { span } = expr else {
            return self.call_expr(depth, expr, nresults);
        };
        self.vararg_allowed(depth, *span)?;
        let dst = self.funcs[depth].free;
        self.emit(
            depth,
            Op::Vararg {
                dst,
                count: nresults,
            },
            *span,
        )?;
        let end = if nresults == COUNT_OPEN {
            u16::from(dst) + 1
        } else {
            u16::from(dst) + u16::from(nresults)
        };
        let end = u8::try_from(end)
            .ok()
            .filter(|end| *end <= MAX_REGISTERS)
            .ok_or_else(|| limit(*span, "register limit"))?;
        self.set_free(depth, end);
        if nresults == COUNT_OPEN {
            self.set_free(depth, dst);
        }
        Ok(dst)
    }

    /// `...` names the extra arguments of the function it is directly in,
    /// which must be vararg. It is not an upvalue: a nested function has
    /// its own, or none.
    fn vararg_allowed(&self, depth: usize, span: Span) -> Result<(), CompileError> {
        if self.funcs[depth].vararg {
            Ok(())
        } else {
            Err(CompileError::new(
                CompileErrorKind::Syntax,
                span,
                "cannot use '...' outside a vararg function",
            )
            .with_optional_near(self.near_at(span)))
        }
    }

    fn call_expr(&mut self, depth: usize, expr: &Expr, nresults: u8) -> Result<u8, CompileError> {
        if let Expr::Chain { first, rest, .. } = expr {
            return self.chain_expr(depth, first, rest, None, nresults);
        }
        let name = self.call_name(depth, expr);
        let (base, nargs) = self.call_window(depth, expr)?;
        let span = expr.span();
        self.note_call(depth, name);
        self.note_call_line(depth, expr);
        self.emit(
            depth,
            Op::Call {
                func: base,
                nargs,
                nresults,
            },
            span,
        )?;
        if nresults == COUNT_OPEN {
            self.set_free(depth, base);
        } else {
            let next = base
                .checked_add(nresults)
                .ok_or_else(|| limit(span, "result window"))?;
            if next > MAX_REGISTERS {
                return Err(limit(span, "register limit"));
            }
            self.set_free(depth, next);
        }
        Ok(base)
    }

    /// How the source names the function `call` calls, by Lua's
    /// `getobjname` rules (ADR 0040): a name, a local, an upvalue, a field
    /// with a constant key, a method, or a string constant. Parentheses
    /// change nothing. A field of `_ENV` is a global.
    fn call_name(
        &self,
        depth: usize,
        call: &Expr,
    ) -> Option<(crate::debuginfo::NameKind, Vec<u8>)> {
        use crate::debuginfo::NameKind;
        let Expr::Call { func, method, .. } = call else {
            return None;
        };
        if let Some(method) = method {
            return Some((NameKind::Method, method.bytes.clone()));
        }
        let mut func: &Expr = func;
        while let Expr::Paren { inner, .. } = func {
            func = inner;
        }
        match func {
            Expr::Name { bytes, .. } => {
                if let Some(local) = self.funcs[depth]
                    .locals
                    .iter()
                    .rev()
                    .find(|local| local.name == *bytes)
                    && self.funcs[depth].debug_locals[local.debug].name.is_empty()
                {
                    return None;
                }
                Some((self.name_kind(depth, bytes), bytes.clone()))
            }
            Expr::Str { bytes, .. } => Some((NameKind::Constant, bytes.clone())),
            Expr::Index { base, key, .. } => {
                let mut base: &Expr = base;
                while let Expr::Paren { inner, .. } = base {
                    base = inner;
                }
                match &**key {
                    // Lua's `GETI`: an integer key that fits its operand.
                    Expr::Integer { value, .. } if (0..=255).contains(value) => {
                        Some((NameKind::Field, b"integer index".to_vec()))
                    }
                    Expr::Str { bytes, .. } => {
                        let env = matches!(base, Expr::Name { bytes, .. } if bytes == b"_ENV");
                        let kind = if env {
                            NameKind::Global
                        } else {
                            NameKind::Field
                        };
                        Some((kind, bytes.clone()))
                    }
                    _ => Some((NameKind::Field, b"?".to_vec())),
                }
            }
            _ => None,
        }
    }

    /// Whether `name` is a local of the function at `depth`, an upvalue,
    /// or a global, without capturing anything.
    fn name_kind(&self, depth: usize, name: &[u8]) -> crate::debuginfo::NameKind {
        use crate::debuginfo::NameKind;
        for level in (0..=depth).rev() {
            let func = &self.funcs[level];
            if func.locals.iter().any(|local| local.name == name) {
                return if level == depth {
                    NameKind::Local
                } else {
                    NameKind::Upvalue
                };
            }
            if func.captures.iter().any(|capture| capture.name == name) {
                return NameKind::Upvalue;
            }
        }
        NameKind::Global
    }

    /// The next instruction takes the line of the token after byte `end`.
    fn line_after(&mut self, depth: usize, end: u32) {
        let offset = self.lines.token_after(end);
        let func = &mut self.funcs[depth];
        func.line_at.push((func.ops.len(), offset));
    }

    /// Assignment results and logical RHS values are discharged on the last
    /// expression token. Conditions and constructor fields use their next token.
    fn note_result_line(&mut self, depth: usize, first: usize, end: u32) {
        let func = &mut self.funcs[depth];
        if let Some(pc) = func.ops.len().checked_sub(1).filter(|pc| *pc >= first)
            && matches!(func.ops[pc], Op::GetField { .. } | Op::Index { .. })
        {
            func.line_at.push((pc, end.saturating_sub(1)));
        }
    }

    /// PUC discharges an indexed/global left operand at its infix token.
    /// Calls keep their own execution lines; only the final read and its
    /// compiler-generated constant/upvalue operands move to this debug position.
    fn note_infix_operand(&mut self, depth: usize, first: usize, end: u32) {
        let offset = self.lines.token_after(end);
        if self.lines.line(end.saturating_sub(1)) == self.lines.line(offset) {
            return;
        }
        let func = &mut self.funcs[depth];
        let Some(pc) = func.ops.len().checked_sub(1).filter(|pc| *pc >= first) else {
            return;
        };
        let operands = match func.ops[pc] {
            Op::Index { obj, key, .. } => [Some(obj), Some(key)],
            Op::GetField { obj, .. } => [Some(obj), None],
            _ => return,
        };
        func.line_at.push((pc, offset));
        for producer in (first..pc).rev() {
            if matches!(func.ops[producer], Op::LoadInt { dst, .. } | Op::LoadBytes { dst, .. } | Op::GetUpvalue { dst, .. } if operands.contains(&Some(dst)))
            {
                func.line_at.push((producer, offset));
            }
        }
    }

    /// The call about to be emitted is on the line its arguments open on,
    /// right after the called expression or the method name.
    fn note_call_line(&mut self, depth: usize, call: &Expr) {
        if let Expr::Call { func, method, .. } = call {
            let end = method
                .as_ref()
                .map_or(func.span().end, |name| name.span.end);
            self.line_after(depth, end);
        }
    }

    /// Record the name of the call about to be emitted.
    fn note_call(&mut self, depth: usize, name: Option<(crate::debuginfo::NameKind, Vec<u8>)>) {
        let Some((kind, name)) = name else {
            return;
        };
        let func = &mut self.funcs[depth];
        func.debug_calls.push(crate::debuginfo::CallName {
            pc: func.ops.len() as u32,
            kind,
            name,
        });
    }

    /// The callee and arguments of a call at the first free register: the
    /// register, and the argument count, `COUNT_OPEN` when the last
    /// argument is a call or `...`.
    ///
    /// `receiver:name(args)` evaluates the receiver once, puts it in the
    /// first argument's register, and looks `name` up in it with
    /// `GetField`, the ordinary indexing that follows `__index`, into the
    /// callee's register.
    fn call_window(&mut self, depth: usize, expr: &Expr) -> Result<(u8, u8), CompileError> {
        let Expr::Call {
            func,
            method,
            args,
            span,
        } = expr
        else {
            return Err(invalid(expr.span(), "expected a call"));
        };
        let base = self.funcs[depth].free;
        let first = match method {
            None => {
                self.expr_to(depth, func, base)?;
                1
            }
            Some(name) => {
                // The receiver first, as Lua does, so a chain of method
                // calls reuses the same registers.
                let receiver = slot(base, 1, name.span)?;
                if receiver >= MAX_REGISTERS {
                    return Err(limit(name.span, "register limit"));
                }
                let index = self.const_index(depth, &name.bytes, name.span)?;
                let held = self.expr_r(depth, func)?;
                // Copy the receiver before the lookup, as Lua's `SELF`
                // does: an `__index` that reassigns the receiver's local
                // does not change `self`. The copy is from wherever the
                // receiver is held, a local or any temporary.
                if held != receiver {
                    self.emit(
                        depth,
                        Op::Move {
                            dst: receiver,
                            src: held,
                        },
                        name.span,
                    )?;
                }
                self.emit(
                    depth,
                    Op::GetField {
                        dst: base,
                        obj: receiver,
                        name: index,
                    },
                    name.span,
                )?;
                self.set_free(depth, receiver + 1);
                2
            }
        };
        let mut open = false;
        for (index, arg) in args.iter().enumerate() {
            let arg_start = self.funcs[depth].ops.len();
            let last = index + 1 == args.len();
            if last && is_multret(arg) {
                if self.funcs[depth].free != slot(base, index + first, arg.span())? {
                    return Err(invalid(arg.span(), "open argument was misplaced"));
                }
                self.multi(depth, arg, COUNT_OPEN)?;
                open = true;
                break;
            }
            self.expr_to(depth, arg, slot(base, index + first, arg.span())?)?;
            if last
                && matches!(
                    arg,
                    Expr::Integer { .. }
                        | Expr::Float { .. }
                        | Expr::Str { .. }
                        | Expr::Bool { .. }
                        | Expr::Nil { .. }
                )
            {
                let offset = self.lines.token_after(arg.span().end);
                for pc in arg_start..self.funcs[depth].ops.len() {
                    self.funcs[depth].line_at.push((pc, offset));
                }
            }
        }
        let nargs = if open {
            COUNT_OPEN
        } else {
            u8::try_from(args.len() + first - 1).map_err(|_| limit(*span, "too many arguments"))?
        };
        Ok((base, nargs))
    }

    fn function(
        &mut self,
        depth: usize,
        params: &[Name],
        vararg: bool,
        body: &Block,
        span: Span,
    ) -> Result<u32, CompileError> {
        if self.funcs.len() >= usize::try_from(MAX_FUNC_NEST).unwrap_or(usize::MAX) {
            return Err(limit(span, "function nesting limit"));
        }
        self.protos += 1;
        if self.protos > self.limits.max_functions {
            return Err(limit(span, "too many functions in the chunk"));
        }
        let mut child = FnBuild::new(span);
        child.params =
            u8::try_from(params.len()).map_err(|_| limit(span, "too many parameters"))?;
        child.vararg = vararg;
        self.funcs.push(child);
        let child_depth = self.funcs.len() - 1;
        for param in params {
            self.define(child_depth, param)?;
        }
        self.block(child_depth, body)?;
        self.funcs[child_depth].patch_gotos(&self.lines)?;
        self.emit(child_depth, Op::Return { base: 0, count: 0 }, span)?;
        let built = self
            .funcs
            .pop()
            .ok_or_else(|| invalid(span, "missing nested function"))?;
        let (proto, map) = built.finish(&self.lines, false, self.limits.max_instructions)?;
        let parent = &mut self.funcs[depth];
        if parent.children.len() >= self.limits.max_functions {
            return Err(limit(span, "too many nested functions"));
        }
        let index = u32::try_from(parent.children.len()).map_err(|_| limit(span, "child index"))?;
        parent.children.push(proto);
        parent.child_maps.push(map);
        Ok(index)
    }

    /// Where `name` lives: a local, an upvalue, or `None` for a free name,
    /// which means `_ENV.name`. Capturing a parent's local marks it captured.
    fn resolve(
        &mut self,
        depth: usize,
        name: &[u8],
        span: Span,
    ) -> Result<Option<Place>, CompileError> {
        if let Some(reg) = self.funcs[depth]
            .locals
            .iter()
            .rev()
            .find(|local| local.name == name)
            .map(|local| local.reg)
        {
            return Ok(Some(Place::Local(reg)));
        }
        if let Some(index) = self.funcs[depth]
            .captures
            .iter()
            .position(|capture| capture.name == name)
        {
            return Ok(Some(Place::Upvalue(
                u8::try_from(index).map_err(|_| limit(span, "upvalue index"))?,
            )));
        }
        if depth == 0 {
            return Ok(None);
        }
        let Some(parent) = self.resolve(depth - 1, name, span)? else {
            return Ok(None);
        };
        let readonly = self.readonly(depth - 1, parent);
        let kind = match parent {
            Place::Local(reg) => {
                if let Some(local) = self.funcs[depth - 1]
                    .locals
                    .iter_mut()
                    .rev()
                    .find(|local| local.reg == reg)
                {
                    local.captured = true;
                }
                Capture::Local(reg)
            }
            Place::Upvalue(index) => Capture::Upvalue(index),
        };
        if self.funcs[depth].captures.len() >= MAX_UPVALUES {
            let function_line = self.lines.line(self.funcs[depth].span.start);
            return Err(limit(
                span,
                &format!(
                    "too many upvalues (limit is {MAX_UPVALUES}) in function at line {function_line}"
                ),
            )
            .with_optional_near(self.near_at(span)));
        }
        let captures = &mut self.funcs[depth].captures;
        let index = u8::try_from(captures.len()).map_err(|_| limit(span, "upvalue index"))?;
        captures.push(CapRec {
            name: name.to_vec(),
            kind,
            readonly,
        });
        Ok(Some(Place::Upvalue(index)))
    }

    /// Whether `place` names a `<const>` or `<close>` local, directly or as an
    /// upvalue. The one test every assignment to a name goes through.
    fn readonly(&self, depth: usize, place: Place) -> bool {
        let func = &self.funcs[depth];
        match place {
            Place::Local(reg) => func
                .locals
                .iter()
                .rev()
                .find(|local| local.reg == reg)
                .is_some_and(|local| local.readonly),
            Place::Upvalue(index) => func
                .captures
                .get(usize::from(index))
                .is_some_and(|capture| capture.readonly),
        }
    }

    /// `name` resolved as an assignment target. A `<close>` local cannot be
    /// assigned, as in Lua.
    fn assignable(&mut self, depth: usize, name: &Name) -> Result<Option<Place>, CompileError> {
        let place = self.resolve(depth, &name.bytes, name.span)?;
        if place.is_some_and(|place| self.readonly(depth, place)) {
            return Err(CompileError::new(
                CompileErrorKind::Syntax,
                name.span,
                format!(
                    "attempt to assign to const variable '{}'",
                    String::from_utf8_lossy(&name.bytes)
                ),
            ));
        }
        Ok(place)
    }

    /// A register holding the `_ENV` visible here: its local register, or a
    /// new temporary loaded from its upvalue.
    fn env_reg(&mut self, depth: usize, span: Span) -> Result<u8, CompileError> {
        match self.resolve(depth, b"_ENV", span)? {
            Some(Place::Local(reg)) => Ok(reg),
            Some(Place::Upvalue(index)) => {
                let reg = self.alloc(depth, span)?;
                self.emit(depth, Op::GetUpvalue { dst: reg, index }, span)?;
                Ok(reg)
            }
            None => Err(invalid(span, "no _ENV in scope")),
        }
    }

    /// `reg` itself when it is a temporary, otherwise a new temporary.
    fn temp_or_alloc(&mut self, depth: usize, reg: u8, span: Span) -> Result<u8, CompileError> {
        if reg >= self.funcs[depth].nact {
            Ok(reg)
        } else {
            self.alloc(depth, span)
        }
    }

    /// Emit `op` writing `dst`, then free everything above `dst`.
    fn finish_at(&mut self, depth: usize, dst: u8, op: Op, span: Span) -> Result<u8, CompileError> {
        self.emit(depth, op, span)?;
        let next = dst
            .checked_add(1)
            .ok_or_else(|| limit(span, "register limit"))?;
        self.set_free(depth, next);
        Ok(dst)
    }

    /// A free name read: `_ENV.name`, language-level indexing.
    fn global_get(&mut self, depth: usize, name: &[u8], span: Span) -> Result<u8, CompileError> {
        let env = self.env_reg(depth, span)?;
        let key = self.const_index(depth, name, span)?;
        let dst = self.temp_or_alloc(depth, env, span)?;
        self.finish_at(
            depth,
            dst,
            Op::GetField {
                dst,
                obj: env,
                name: key,
            },
            span,
        )
    }

    /// `base[key]`, or `GetField` for a string-constant key.
    fn index_expr(
        &mut self,
        depth: usize,
        base: &Expr,
        key: &Expr,
        span: Span,
    ) -> Result<u8, CompileError> {
        let obj = self.expr_r(depth, base)?;
        if let Expr::Str { bytes, .. } = key {
            let name = self.const_index(depth, bytes, span)?;
            let dst = self.temp_or_alloc(depth, obj, span)?;
            self.line_after(depth, span.end);
            return self.finish_at(depth, dst, Op::GetField { dst, obj, name }, span);
        }
        let key = self.expr_r(depth, key)?;
        let dst = if obj >= self.funcs[depth].nact {
            obj
        } else {
            self.temp_or_alloc(depth, key, span)?
        };
        self.finish_at(depth, dst, Op::Index { dst, obj, key }, span)
    }

    /// A constructor. Fields are evaluated and stored in source order, one at
    /// a time, into a table that sits in a register the whole time. List
    /// fields count from 1; a final list field that is a call stores all its
    /// results with `SetList`. The table is fresh, so raw stores are the
    /// same as language-level ones.
    fn table_expr(
        &mut self,
        depth: usize,
        fields: &[TableField],
        span: Span,
    ) -> Result<u8, CompileError> {
        let table = self.alloc(depth, span)?;
        self.emit(depth, Op::NewTable { dst: table }, span)?;
        let after = table
            .checked_add(1)
            .ok_or_else(|| limit(span, "register limit"))?;
        let mut position: i64 = 1;
        for (index, field) in fields.iter().enumerate() {
            let last = index + 1 == fields.len();
            match field {
                TableField::List(expr) if last && is_multret(expr) => {
                    let src = self.funcs[depth].free;
                    self.multi(depth, expr, COUNT_OPEN)?;
                    let start =
                        u32::try_from(position).map_err(|_| limit(span, "constructor length"))?;
                    self.emit(depth, Op::SetList { table, src, start }, expr.span())?;
                }
                TableField::List(expr) => {
                    let src = self.expr_r(depth, expr)?;
                    let key = self.alloc(depth, expr.span())?;
                    self.emit(
                        depth,
                        Op::LoadInt {
                            dst: key,
                            value: position,
                        },
                        expr.span(),
                    )?;
                    self.emit(depth, Op::SetTable { table, key, src }, expr.span())?;
                    position = position
                        .checked_add(1)
                        .ok_or_else(|| limit(span, "constructor length"))?;
                }
                TableField::Keyed {
                    key: Expr::Str { bytes, .. },
                    value,
                } => {
                    let src = self.expr_r(depth, value)?;
                    let name = self.const_index(depth, bytes, value.span())?;
                    self.emit(
                        depth,
                        Op::SetField {
                            obj: table,
                            name,
                            src,
                        },
                        value.span(),
                    )?;
                }
                TableField::Keyed { key, value } => {
                    let key = self.expr_r(depth, key)?;
                    let src = self.expr_r(depth, value)?;
                    self.emit(depth, Op::SetTable { table, key, src }, value.span())?;
                }
            }
            self.set_free(depth, after);
        }
        if fields
            .iter()
            .any(|field| matches!(field, TableField::List(_)))
        {
            let pc = self.funcs[depth].ops.len() - 1;
            // This store uses the closing delimiter's line. Reuse its
            // existing span instead of allocating an override entry.
            self.funcs[depth].spans[pc].end = span.end;
        }
        Ok(table)
    }

    /// An assignment statement. One target stores directly. Several targets
    /// that are all locals or upvalues store from registers, right to left.
    /// Several targets with a table or global among them use the
    /// pending-assignment instructions: every destination's table and key
    /// are recorded before any value is computed, and `AssignCommit` stores
    /// right to left, resumable between stores.
    fn assign(
        &mut self,
        depth: usize,
        targets: &[Target],
        values: &[Expr],
        span: Span,
    ) -> Result<(), CompileError> {
        if let [target] = targets {
            return self.assign_one(depth, target, values, span);
        }
        let mut places = Vec::with_capacity(targets.len());
        for target in targets {
            match target {
                Target::Name(name) => match self.assignable(depth, name)? {
                    Some(place) => places.push(place),
                    None => break,
                },
                Target::Index { .. } => break,
            }
        }
        if places.len() == targets.len() {
            let base = self.place_values(depth, values, targets.len(), span)?;
            for (index, place) in places.iter().enumerate().rev() {
                let src = slot(base, index, span)?;
                self.store_place(depth, *place, src, targets[index].span())?;
            }
            return Ok(());
        }
        self.assign_parallel(depth, targets, values, span)
    }

    /// Stores happen after the RHS is read; diagnostic spans still name the
    /// destination, while debug execution lines follow the completed expression.
    fn note_store_line(&mut self, depth: usize, first: usize, target: Span, statement: Span) {
        let offset = statement.end.saturating_sub(1);
        if self.lines.line(target.end.saturating_sub(1)) != self.lines.line(offset) {
            let func = &mut self.funcs[depth];
            for pc in first..func.ops.len() {
                func.line_at.push((pc, offset));
            }
        }
    }

    fn assign_one(
        &mut self,
        depth: usize,
        target: &Target,
        values: &[Expr],
        span: Span,
    ) -> Result<(), CompileError> {
        // A function statement is lowered to an assignment whose function
        // expression starts at the statement's keyword. PUC fixes its store
        // to that line, rather than the closing `end` of the function body.
        let store_span = if matches!(values, [Expr::Function { span: function, .. }] if function.start == span.start)
        {
            Span {
                start: span.start,
                end: span.start + 1,
            }
        } else {
            span
        };
        match target {
            Target::Name(name) => match self.assignable(depth, name)? {
                Some(place) => {
                    let src = self.place_values(depth, values, 1, span)?;
                    if values.len() == 1
                        && arithmetic_result(&values[0])
                        && let Place::Local(reg) = place
                        && let Some(
                            Op::Add { dst, .. } | Op::Arith { dst, .. } | Op::ArithK { dst, .. },
                        ) = self.funcs[depth].ops.last()
                        && *dst == src
                        && let Some(local) = self.funcs[depth].locals.iter().find(|l| l.reg == reg)
                    {
                        let candidate = (self.funcs[depth].ops.len() - 1, local.decl);
                        self.funcs[depth].destinations.push(candidate);
                    }
                    let first = self.funcs[depth].ops.len();
                    self.store_place(depth, place, src, name.span)?;
                    self.note_store_line(depth, first, name.span, store_span);
                }
                None => {
                    let src = self.place_values(depth, values, 1, span)?;
                    let first = self.funcs[depth].ops.len();
                    let obj = self.env_reg(depth, name.span)?;
                    let name_index = self.const_index(depth, &name.bytes, name.span)?;
                    self.emit(
                        depth,
                        Op::SetField {
                            obj,
                            name: name_index,
                            src,
                        },
                        name.span,
                    )?;
                    self.note_store_line(depth, first, name.span, store_span);
                }
            },
            Target::Index {
                base,
                key,
                span: target_span,
            } => {
                let obj = self.expr_r(depth, base)?;
                if let Expr::Str { bytes, .. } = key {
                    let name = self.const_index(depth, bytes, *target_span)?;
                    let src = self.place_values(depth, values, 1, span)?;
                    let first = self.funcs[depth].ops.len();
                    self.emit(depth, Op::SetField { obj, name, src }, *target_span)?;
                    self.note_store_line(depth, first, *target_span, store_span);
                } else {
                    let key = self.expr_r(depth, key)?;
                    let src = self.place_values(depth, values, 1, span)?;
                    let first = self.funcs[depth].ops.len();
                    self.emit(depth, Op::SetIndex { obj, key, src }, *target_span)?;
                    self.note_store_line(depth, first, *target_span, store_span);
                }
            }
        }
        Ok(())
    }

    fn assign_parallel(
        &mut self,
        depth: usize,
        targets: &[Target],
        values: &[Expr],
        span: Span,
    ) -> Result<(), CompileError> {
        let mut upvalue_temps = Vec::new();
        for target in targets {
            match target {
                Target::Name(name) => match self.assignable(depth, name)? {
                    Some(Place::Local(reg)) => {
                        self.emit(depth, Op::AssignLocal { reg }, name.span)?;
                    }
                    Some(Place::Upvalue(index)) => {
                        let temp = self.alloc(depth, name.span)?;
                        self.emit(depth, Op::AssignLocal { reg: temp }, name.span)?;
                        upvalue_temps.push((index, temp, name.span));
                    }
                    None => {
                        let mark = self.funcs[depth].free;
                        let table = self.env_reg(depth, name.span)?;
                        let key = self.string_reg(depth, &name.bytes, name.span)?;
                        self.emit(depth, Op::AssignField { table, key }, name.span)?;
                        self.set_free(depth, mark);
                    }
                },
                Target::Index {
                    base,
                    key,
                    span: target_span,
                } => {
                    let mark = self.funcs[depth].free;
                    let table = self.expr_r(depth, base)?;
                    let key = match key {
                        Expr::Str { bytes, .. } => self.string_reg(depth, bytes, *target_span)?,
                        other => self.expr_r(depth, other)?,
                    };
                    self.emit(depth, Op::AssignField { table, key }, *target_span)?;
                    self.set_free(depth, mark);
                }
            }
        }
        let src = self.place_values(depth, values, targets.len(), span)?;
        let n = u8::try_from(targets.len()).map_err(|_| limit(span, "too many targets"))?;
        self.emit(depth, Op::AssignCommit { src, n }, span)?;
        for (index, temp, name_span) in upvalue_temps {
            self.emit(depth, Op::SetUpvalue { index, src: temp }, name_span)?;
        }
        Ok(())
    }

    /// A new temporary holding the string constant `bytes`.
    fn string_reg(&mut self, depth: usize, bytes: &[u8], span: Span) -> Result<u8, CompileError> {
        let const_index = self.const_index(depth, bytes, span)?;
        let dst = self.alloc(depth, span)?;
        self.emit(depth, Op::LoadBytes { dst, const_index }, span)?;
        Ok(dst)
    }

    fn store_place(
        &mut self,
        depth: usize,
        place: Place,
        src: u8,
        span: Span,
    ) -> Result<(), CompileError> {
        match place {
            Place::Local(reg) => {
                if reg != src {
                    self.emit(depth, Op::Move { dst: reg, src }, span)?;
                }
            }
            Place::Upvalue(index) => self.emit(depth, Op::SetUpvalue { index, src }, span)?,
        }
        Ok(())
    }

    fn define(&mut self, depth: usize, name: &Name) -> Result<(), CompileError> {
        if self.funcs[depth].locals.len() >= MAX_LOCALS || self.funcs[depth].nact >= MAX_REGISTERS {
            let function = if depth == 0 {
                "main function".to_owned()
            } else {
                format!(
                    "function at line {}",
                    self.lines.line(self.funcs[depth].span.start)
                )
            };
            return Err(limit(
                name.span,
                &format!("too many local variables (limit is {MAX_LOCALS}) in {function}"),
            )
            .with_optional_near(self.near_at(name.span)));
        }
        let func = &mut self.funcs[depth];
        let reg = func.nact;
        func.decls.push(Decl {
            reg,
            captured: false,
            close: false,
        });
        func.debug_locals.push(crate::debuginfo::LocalInfo {
            name: name.bytes.clone(),
            reg,
            start: func.ops.len() as u32,
            end: func.ops.len() as u32,
        });
        func.locals.push(LocalRec {
            name: name.bytes.clone(),
            reg,
            debug: func.debug_locals.len() - 1,
            decl: func.decls.len() - 1,
            captured: false,
            close: false,
            readonly: false,
        });
        func.nact += 1;
        if func.free < func.nact {
            func.free = func.nact;
        }
        func.high = func.high.max(func.free);
        Ok(())
    }

    fn const_index(&mut self, depth: usize, bytes: &[u8], span: Span) -> Result<u32, CompileError> {
        let func = &mut self.funcs[depth];
        if let Some(&index) = func.const_lookup.get(bytes) {
            return Ok(index);
        }
        if func.consts.len() >= self.limits.max_constants {
            return Err(limit(span, "too many constants"));
        }
        let index = u32::try_from(func.consts.len()).map_err(|_| limit(span, "constant index"))?;
        let bytes = bytes.to_vec();
        func.const_lookup.insert(bytes.clone(), index);
        func.consts.push(bytes);
        Ok(index)
    }

    fn alloc(&mut self, depth: usize, span: Span) -> Result<u8, CompileError> {
        let func = &mut self.funcs[depth];
        if func.free >= MAX_REGISTERS {
            return Err(limit(span, "register limit"));
        }
        let reg = func.free;
        func.free += 1;
        func.high = func.high.max(func.free);
        Ok(reg)
    }

    fn set_free(&mut self, depth: usize, free: u8) {
        let func = &mut self.funcs[depth];
        func.free = free;
        func.high = func.high.max(free);
    }

    fn emit(&mut self, depth: usize, op: Op, span: Span) -> Result<(), CompileError> {
        if self.funcs[depth].ops.len() >= self.limits.max_instructions {
            return Err(limit(span, "too many instructions"));
        }
        self.funcs[depth].ops.push(op);
        self.funcs[depth].spans.push(span);
        Ok(())
    }
}

/// A call or `...`: an expression whose values a list can take all of.
/// What leaves locals from register `from` on: `CloseScope` when one of
/// them is `<close>`, which closes captured locals too; `CloseUpvalues`
/// when one is captured; nothing otherwise. Block exits and gotos share it.
fn exit_op(from: u8, close: bool, captured: bool) -> Option<Op> {
    if close {
        Some(Op::CloseScope { from })
    } else if captured {
        Some(Op::CloseUpvalues { from })
    } else {
        None
    }
}

fn is_multret(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { .. } | Expr::Vararg { .. })
        || matches!(expr, Expr::Chain { rest, .. } if matches!(rest.last(), Some(ChainStep::Call { .. })))
}

/// Register `base + index`, refused past the register limit before any
/// instruction names it.
fn slot(base: u8, index: usize, span: Span) -> Result<u8, CompileError> {
    let index = u8::try_from(index).map_err(|_| limit(span, "register limit"))?;
    base.checked_add(index)
        .filter(|reg| *reg < MAX_REGISTERS)
        .ok_or_else(|| limit(span, "register limit"))
}

fn limit(span: Span, message: &str) -> CompileError {
    CompileError::new(CompileErrorKind::Limit, span, message)
}

fn invalid(span: Span, message: &str) -> CompileError {
    CompileError::new(CompileErrorKind::InvalidProgram, span, message)
}

fn proto_count(spec: &ProtoSpec) -> usize {
    1 + spec.children.iter().map(proto_count).sum::<usize>()
}

fn op_count(spec: &ProtoSpec) -> usize {
    spec.ops.len() + spec.children.iter().map(op_count).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{HostRegistry, Journal};
    use crate::id::StepOutcome;
    use crate::opcode::Capture;
    use crate::runtime::{Config, Runtime};
    use crate::value::Value;

    const CLOSURE_PAIR: &[u8] = include_bytes!("../fixtures/lua/closure_pair.lua");
    const BRANCH_CLOSE: &[u8] = include_bytes!("../fixtures/lua/branch_close.lua");

    fn run(source: &str) -> Vec<Value> {
        run_bytes(source.as_bytes())
    }

    fn run_bytes(source: &[u8]) -> Vec<Value> {
        let chunk = compile(source).unwrap_or_else(|error| panic!("{error:?}"));
        let mut runtime = Runtime::boot(
            Config::default(),
            HostRegistry::proof(),
            &chunk.proto,
            false,
        )
        .unwrap();
        let mut journal = Journal::new();
        let outcome = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap();
        assert!(matches!(outcome, StepOutcome::Completed), "{outcome:?}");
        runtime.entry_results().unwrap()
    }

    fn ints(values: &[Value]) -> Vec<Option<i64>> {
        values
            .iter()
            .map(|value| match value {
                Value::Integer(integer) => Some(*integer),
                Value::Nil => None,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn flat_chains_run_and_drop_on_two_megabyte_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let marker = 0u8;
                let top = (&marker as *const u8) as usize;
                crate::limits::STACK_FLOOR.with(|floor| floor.set(top));
                for op in ["+", "-", "*", "/", "//", "%", "&", "|", "~", "<<", ">>"] {
                    let source = format!("return 1{}\n", format!("{op}1").repeat(100_000));
                    run_bytes(source.as_bytes());
                }
                for op in ["==", "~=", "<", "<=", ">", ">="] {
                    let tail = format!("{op}true").repeat(100_000);
                    let source = format!("return true{tail}\n");
                    let chunk = compile(source.as_bytes()).unwrap();
                    let mut runtime = Runtime::boot(
                        Config::default(),
                        HostRegistry::proof(),
                        &chunk.proto,
                        false,
                    )
                    .unwrap();
                    let outcome = runtime
                        .run_until_terminal(u64::MAX, &mut Journal::new())
                        .unwrap();
                    assert!(matches!(
                        outcome,
                        StepOutcome::Completed | StepOutcome::LuaError(_)
                    ));
                }
                for op in [" and ", " or "] {
                    let source =
                        format!("return true{}true\n", format!("{op}true").repeat(100_000));
                    run_bytes(source.as_bytes());
                }
                for source in [
                    format!("local t={{}}; t.x=t; return t{}\n", ".x".repeat(100_000)),
                    format!("local t={{}}; t[1]=t; return t{}\n", "[1]".repeat(100_000)),
                    format!(
                        "local function f() return f end; return f{}\n",
                        "()".repeat(100_000)
                    ),
                    format!(
                        "local o={{}}; function o:m() return self end; return o{}\n",
                        ":m()".repeat(100_000)
                    ),
                    format!(
                        "local t={{}}; local function f() return t end; t.x=f; return t{}\n",
                        ".x()".repeat(50_000)
                    ),
                ] {
                    run_bytes(source.as_bytes());
                }
                for source in [
                    format!("return 1{}+", "+1".repeat(100_000)),
                    format!("return t{}.", ".x".repeat(100_000)),
                ] {
                    assert!(compile(source.as_bytes()).is_err());
                }
                let distinct = format!(
                    "local t={{}}; return t{}",
                    (0..100_000).map(|i| format!(".f{i}")).collect::<String>()
                );
                let distinct = compile(distinct.as_bytes()).unwrap();
                assert_eq!(distinct.proto.byte_consts.len(), 100_000);
                drop(distinct);
                let nested_functions = format!(
                    "return {}1{}",
                    "function() return ".repeat(98),
                    " end".repeat(98),
                );
                run_bytes(nested_functions.as_bytes());
                crate::limits::STACK_FLOOR.with(|floor| {
                    eprintln!(
                        "flat-chain compiler stack probe: {} bytes",
                        top.abs_diff(floor.get())
                    );
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn arithmetic_destinations_preserve_aliases_and_parallel_stores() {
        for (source, expected) in [
            ("local a, b = 3, 7 a = b + a return a", vec![Some(10)]),
            ("local a = 3 a = (a + 2) * (a + 4) return a", vec![Some(35)]),
            (
                "local a, b = 3, 7 a, b = b + 1, a + 1 return a, b",
                vec![Some(8), Some(4)],
            ),
            (
                "local a, b = 3, 7 a, b = b, a return a, b",
                vec![Some(7), Some(3)],
            ),
            (
                "local a = 3 a = false and (a + 1) return a or 42",
                vec![Some(42)],
            ),
            (
                "local a = 3 a = true or (a + 1) return a and 42",
                vec![Some(42)],
            ),
        ] {
            assert_eq!(ints(&run(source)), expected, "{source}");
        }
        let chunk = compile(b"local s = 0 for i = 1, 10 do s = s + (i & 3) end return s").unwrap();
        assert!(
            chunk
                .proto
                .ops
                .iter()
                .any(|op| matches!(op, Op::Add { dst: 0, a: 0, .. }))
        );
        // There is no result-copy chain in the loop body.
        let prep = chunk
            .proto
            .ops
            .iter()
            .position(|op| matches!(op, Op::ForPrep { .. }))
            .unwrap();
        let end = chunk
            .proto
            .ops
            .iter()
            .position(|op| matches!(op, Op::ForLoop { .. }))
            .unwrap();
        assert!(
            !chunk.proto.ops[prep + 1..end]
                .iter()
                .any(|op| matches!(op, Op::Move { .. }))
        );
        assert_eq!(chunk.instruction_count(), chunk.mapped_instructions());
    }

    #[test]
    fn arithmetic_destination_waits_for_later_captures() {
        let source = b"local x = 0 local f ::again:: x = x + 1 if not f then f = function() return x end goto again end return f()";
        let chunk = compile(source).unwrap();
        assert!(
            !chunk
                .proto
                .ops
                .iter()
                .any(|op| matches!(op, Op::ArithK { dst: 0, .. }))
        );
        assert_eq!(ints(&run_bytes(source)), vec![Some(2)]);
    }

    #[test]
    fn arithmetic_destination_keeps_operator_line_and_maps_debug_pcs() {
        let chunk = compile(
            b"local s = 0\ns =\n s +\n 1\nlocal function f() return 1 end\nreturn s, f()\n",
        )
        .unwrap();
        let debug = chunk.proto.debug.as_ref().unwrap();
        let add = chunk
            .proto
            .ops
            .iter()
            .position(|op| matches!(op, Op::ArithK { dst: 0, .. }))
            .unwrap();
        assert_eq!(debug.lines[add], 3);
        // The deleted assignment copy was the sole instruction on line 2.
        assert!(!debug.lines.contains(&2));
        assert_eq!(debug.lines.len(), chunk.proto.ops.len());
        let call = debug.calls.iter().find(|call| call.name == b"f").unwrap();
        assert!(matches!(chunk.proto.ops[call.pc as usize], Op::Call { .. }));
        for local in &debug.locals {
            assert!(local.start <= local.end && local.end as usize <= chunk.proto.ops.len());
        }
        assert_eq!(chunk.instruction_count(), chunk.mapped_instructions());
    }

    #[test]
    fn cmpbr_selection_keeps_values_and_maps_debug_pcs() {
        let chunk = compile(b"local a, b = 1, 2\nlocal value = a < b\nif not (a < b) then\nreturn value\nend\nwhile a < b do a = a + 1 end\nrepeat a = a - 1 until a == 0\nreturn value\n").unwrap();
        let debug = chunk.proto.debug.as_ref().unwrap();
        let compares = chunk
            .proto
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Compare { .. }))
            .count();
        assert_eq!(compares, 1, "the local's value must be materialized");
        let branches: Vec<_> = chunk
            .proto
            .ops
            .iter()
            .enumerate()
            .filter_map(|(pc, op)| {
                if let Op::CompareBranch { sense, offset, .. } = op {
                    Some((pc, *sense, *offset))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(branches.len(), 3);
        assert!(branches[0].1);
        assert_eq!(debug.lines[branches[0].0], 3);
        assert!(branches[2].2 < 0, "repeat has a backward conditional edge");
        assert_eq!(debug.lines.len(), chunk.proto.ops.len());
        for local in &debug.locals {
            assert!(local.start <= local.end && local.end as usize <= chunk.proto.ops.len());
        }
        assert_eq!(chunk.instruction_count(), chunk.mapped_instructions());
    }

    // Run the original and a string.dump/load copy, checkpointing both the
    // installed source and the loaded function before either finishes.
    // Collect the temporary dump before holding both copies in a snapshot.
    fn wide_round_trip(body: &str, expected: i64) -> CompiledChunk {
        let source = format!(
            "local f = function() {body} end local a = f() \
             local g = assert(load(string.dump(f))) collectgarbage('collect') ready = 1 return a, g()"
        );
        let chunk = compile(source.as_bytes()).unwrap();
        let registry = HostRegistry::proof();
        let mut runtime =
            Runtime::boot(Config::default(), registry.clone(), &chunk.proto, false).unwrap();
        runtime.install_base().unwrap();
        runtime.install_string().unwrap();
        let bytes = runtime.snapshot().unwrap();
        runtime = Runtime::from_snapshot(&bytes, &registry, runtime.effect_domain()).unwrap();
        let mut journal = Journal::new();
        // The first invocation can be large; inspect only the short wrapper
        // one instruction at a time after it returns.
        runtime
            .run(chunk.proto.children[0].ops.len() as u64, &mut journal)
            .unwrap();
        while !matches!(runtime.global_integer("ready"), Ok(1)) {
            assert!(matches!(
                runtime.run(1, &mut journal).unwrap(),
                StepOutcome::Paused(_)
            ));
        }
        let bytes = runtime.snapshot().unwrap();
        let mut restored =
            Runtime::from_snapshot(&bytes, &registry, runtime.effect_domain()).unwrap();
        for run in [&mut runtime, &mut restored] {
            assert!(matches!(
                run.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                StepOutcome::Completed
            ));
            assert_eq!(
                ints(&run.entry_results().unwrap()),
                vec![Some(expected), Some(expected)]
            );
        }
        chunk
    }

    #[test]
    fn large_function_runs_dumps_and_checkpoints() {
        let body = format!("local x = 0 {} return 42", "x = nil;".repeat(50_100));
        let chunk = wide_round_trip(&body, 42);
        assert!(chunk.proto.children[0].ops.len() > 100_000);
        assert_eq!(chunk.instruction_count(), chunk.mapped_instructions());
    }

    #[test]
    fn long_forward_and_backward_jumps_round_trip() {
        let body = format!(
            "local x = 0 if false then {} end \
             while x < 1 do {} x = 1 end return x",
            "x = nil;".repeat(16_500),
            "x = nil;".repeat(16_500)
        );
        let chunk = wide_round_trip(&body, 1);
        let ops = &chunk.proto.children[0].ops;
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::JumpIfFalse { offset, .. } if *offset > 32_767))
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::Jump { offset } if *offset < -32_767))
        );
    }

    #[test]
    fn many_string_constants_round_trip() {
        let mut body = String::from("local x ");
        for index in 0..4_100 {
            body.push_str(&format!("x = 'constant{index}';"));
        }
        body.push_str("return #x");
        let chunk = wide_round_trip(&body, 12);
        assert!(chunk.proto.children[0].byte_consts.len() > 4_096);
    }

    #[test]
    fn many_field_names_round_trip() {
        let mut body = String::from("local t = {} ");
        for index in 0..4_100 {
            body.push_str(&format!("t.field{index} = {index};"));
        }
        body.push_str("return t.field4099");
        let chunk = wide_round_trip(&body, 4_099);
        assert!(chunk.proto.children[0].byte_consts.len() > 4_096);
    }

    #[test]
    fn many_child_functions_round_trip() {
        let mut body = String::from("local f ");
        for index in 0..300 {
            body.push_str(&format!("f = function() return {index} end;"));
        }
        body.push_str("return f()");
        let chunk = wide_round_trip(&body, 299);
        assert!(chunk.proto.children[0].children.len() > 256);
    }

    #[test]
    fn configurable_limits_refuse_oversized_code_and_clamp() {
        let source = b"local f = function() return 'a', 'b' end return f()";
        let small = [
            CompileLimits {
                max_instructions: 1,
                ..CompileLimits::default()
            },
            CompileLimits {
                max_constants: 1,
                ..CompileLimits::default()
            },
            CompileLimits {
                max_functions: 1,
                ..CompileLimits::default()
            },
            CompileLimits {
                max_source_bytes: source.len() - 1,
                ..CompileLimits::default()
            },
        ];
        for limits in small {
            assert_eq!(
                compile_with_limits(source, &limits).unwrap_err().kind,
                CompileErrorKind::Limit
            );
        }
        let large = CompileLimits {
            max_instructions: usize::MAX,
            max_constants: usize::MAX,
            max_functions: usize::MAX,
            max_source_bytes: usize::MAX,
        };
        assert_eq!(
            large.clamped(),
            CompileLimits {
                max_instructions: MAX_INSTRUCTIONS,
                max_constants: MAX_CONSTS,
                max_functions: MAX_PROTOS,
                max_source_bytes: MAX_SOURCE_BYTES
            }
        );
        assert_eq!(
            compile_with_limits(source, &large).unwrap().proto,
            compile(source).unwrap().proto
        );
    }

    #[test]
    fn source_and_literals_can_exceed_the_old_limits() {
        let source = format!(
            "{}return '{}'",
            " ".repeat((1 << 20) + 1),
            "a".repeat((1 << 16) + 1)
        );
        assert!(compile(source.as_bytes()).is_ok());
        let limits = CompileLimits {
            max_source_bytes: crate::limits::DEFAULT_SOURCE_BYTES + 1,
            ..CompileLimits::default()
        };
        let mut source = vec![b' '; limits.max_source_bytes];
        source[..8].copy_from_slice(b"return 1");
        assert!(compile_with_limits(&source, &limits).is_ok());
        assert_eq!(compile(&source).unwrap_err().kind, CompileErrorKind::Limit);
    }

    #[test]
    fn deep_source_reports_a_limit_without_overflowing_the_stack() {
        for source in [
            format!("{}return 1{}", "do ".repeat(1_000), " end".repeat(1_000)),
            format!("return {}1{}", "(".repeat(1_000), ")".repeat(1_000)),
            format!(
                "{}return 1{}",
                "local f = function() ".repeat(100),
                " end".repeat(100)
            ),
        ] {
            assert_eq!(
                compile(source.as_bytes()).unwrap_err().kind,
                CompileErrorKind::Limit
            );
        }
        // PUC loops over these forms; they are flat despite long AST spines.
        for source in [
            format!("return {}1", "1+".repeat(1_000)),
            format!("return x{}", ".x".repeat(1_000)),
            format!("return (x{}){}", ".x".repeat(150), ".x".repeat(150)),
            format!(
                "return (function() return x{} end){}",
                ".x".repeat(150),
                ".x".repeat(150)
            ),
        ] {
            assert!(compile(source.as_bytes()).is_ok());
        }
        // The deepest parenthesis chain PUC accepts through direct `load`
        // must fit the default 2 MiB Rust test thread stack with headroom.
        let marker = 0u8;
        let top = (&marker as *const u8) as usize;
        crate::limits::STACK_FLOOR.with(|floor| floor.set(usize::MAX));
        let accepted = format!("return {}1{}", "(".repeat(195), ")".repeat(195));
        assert!(compile(accepted.as_bytes()).is_ok());
        let stack_bytes = crate::limits::STACK_FLOOR.with(|floor| top.saturating_sub(floor.get()));
        eprintln!("195 parentheses: parser/compiler stack span {stack_bytes} bytes");
        assert!(stack_bytes < 1_800_000);
    }

    #[test]
    fn smoke_addition_and_the_closure_fixture() {
        assert_eq!(
            ints(&run("local x = 40; local y = 2; return x + y")),
            vec![Some(42)]
        );
        assert_eq!(
            ints(&run(
                "local add = function(x, y) return x + y end return add(40, 2)"
            )),
            vec![Some(42)]
        );
        assert_eq!(
            ints(&run_bytes(CLOSURE_PAIR)),
            vec![Some(1), Some(1), Some(2), Some(2)]
        );
    }

    #[test]
    fn scope_rules_and_shared_captures() {
        assert_eq!(
            ints(&run("local x = 1 local x = x + 1 return x")),
            vec![Some(2)]
        );
        assert_eq!(
            ints(&run(
                "local x = 1 local f = function() return x end local x = 2 return f(), x"
            )),
            vec![Some(1), Some(2)]
        );
        assert_eq!(
            ints(&run(
                "local f = function() return 10, 20 end local a, b = f() local c, d = (f()) return a, b, c, d"
            )),
            vec![Some(10), Some(20), Some(10), None]
        );
        assert_eq!(
            ints(&run(
                "local outer = function() local n = 1 local mid = function() local inner = function() return n end return inner() end return mid() end return outer()"
            )),
            vec![Some(1)]
        );
        let chunk = compile(
            b"local outer = function() local n = 1 local mid = function() local inner = function() return n end return inner() end return mid() end return outer()",
        )
        .unwrap();
        let outer = &chunk.proto.children[0];
        let mid = &outer.children[0];
        let inner = &mid.children[0];
        assert_eq!(mid.captures, vec![Capture::Local(0)]);
        assert_eq!(inner.captures, vec![Capture::Upvalue(0)]);
        let pair = compile(CLOSURE_PAIR).unwrap();
        let make_pair = &pair.proto.children[0];
        assert_eq!(make_pair.children[0].captures, vec![Capture::Local(0)]);
        assert_eq!(make_pair.children[1].captures, vec![Capture::Local(0)]);
        assert_eq!(compile(CLOSURE_PAIR).unwrap().proto, pair.proto);
    }

    #[test]
    fn literals_spans_limits_and_rejection() {
        match &run("return true")[..] {
            [Value::Bool(true)] => {}
            other => panic!("{other:?}"),
        }
        match &run("return 'ab\\0c'")[..] {
            [Value::String(_)] => {}
            other => panic!("{other:?}"),
        }
        let source = "local x = 40\nlocal y = 2\nreturn x + y";
        let plus = source.find('+').unwrap() as u32;
        let chunk = compile(source.as_bytes()).unwrap();
        assert!(chunk.root_span_covers(plus));
        assert!(chunk.instruction_count() < 30);
        assert!(run("return z") == vec![Value::Nil]);
        assert_eq!(
            compile(b"goto done").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
        assert!(compile(b"local x <const> = 1").is_ok());
        let mut locals = String::new();
        for _ in 0..MAX_LOCALS + 1 {
            locals.push_str("local a = 1 ");
        }
        locals.push_str("return a");
        assert_eq!(
            compile(locals.as_bytes()).unwrap_err().kind,
            CompileErrorKind::Limit
        );
        let mut broken = compile(b"return 1").unwrap();
        broken.proto.ops[0] = Op::LoadInt { dst: 40, value: 1 };
        broken.proto.max_reg = 1;
        assert_eq!(
            check::validate(&broken.proto).unwrap_err().kind,
            CompileErrorKind::InvalidProgram
        );
    }

    #[test]
    fn if_uses_lua_truthiness_and_one_condition_value() {
        let pick = |cond: &str| ints(&run(&format!("if {cond} then return 1 else return 2 end")));
        assert_eq!(pick("false"), vec![Some(2)]);
        assert_eq!(pick("nil"), vec![Some(2)]);
        assert_eq!(pick("true"), vec![Some(1)]);
        assert_eq!(pick("0"), vec![Some(1)]);
        assert_eq!(pick("7"), vec![Some(1)]);
        assert_eq!(pick("''"), vec![Some(1)]);
        assert_eq!(
            ints(&run(
                "local f = function() return false, true end if f() then return 1 else return 2 end"
            )),
            vec![Some(2)]
        );
        assert_eq!(
            ints(&run(
                "local a = 1 if true then if false then a = 2 else a = 3 end end return a"
            )),
            vec![Some(3)]
        );
        assert_eq!(
            ints(&run("if nil then return 1 end return 2")),
            vec![Some(2)]
        );
        assert!(run("local a = 1 if a then a = 2 end").is_empty());
        assert!(run("if true then local y = 1 end return y") == vec![Value::Nil]);
        assert!(run("if false then local y = 1 else return y end") == vec![Value::Nil]);
        assert_eq!(
            compile(b"goto done").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
    }

    #[test]
    fn leaving_a_branch_closes_only_captured_locals_and_reuses_their_register() {
        let chunk = compile(BRANCH_CLOSE).unwrap();
        let ops = &chunk.proto.ops;
        let close = ops
            .iter()
            .position(|op| *op == Op::CloseUpvalues { from: 2 })
            .expect("then-branch close");
        assert!(matches!(ops[close + 1], Op::Jump { .. }));
        assert!(ops.contains(&Op::LoadInt { dst: 2, value: 99 }));
        assert_eq!(
            ints(&run_bytes(BRANCH_CLOSE)),
            vec![Some(10), Some(11), Some(11), Some(99)]
        );

        let quiet = compile(b"local a if true then local b = 1 a = b end return a").unwrap();
        assert!(
            !quiet
                .proto
                .ops
                .iter()
                .any(|op| matches!(op, Op::CloseUpvalues { .. }))
        );

        let else_source = "local g if false then g = function() return 0 end else local y = 5 g = function() return y end end local z = 6 return g()";
        let chunk = compile(else_source.as_bytes()).unwrap();
        assert!(chunk.proto.ops.contains(&Op::CloseUpvalues { from: 1 }));
        assert_eq!(ints(&run(else_source)), vec![Some(5)]);

        let returned = "local g = function() if true then local x = 7 return function() return x end end end local h = g() local a = 1 local b = 2 local c = 3 return h()";
        let chunk = compile(returned.as_bytes()).unwrap();
        assert!(
            !chunk.proto.children[0]
                .ops
                .iter()
                .any(|op| matches!(op, Op::CloseUpvalues { .. }))
        );
        assert_eq!(ints(&run(returned)), vec![Some(7)]);
    }
}
