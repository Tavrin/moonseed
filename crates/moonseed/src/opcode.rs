//! Private bytecode. Not a public compatibility format.
//!
//! Tags below are the snapshot schema's explicit integers, not Rust's
//! in-memory enum discriminant.

use crate::id::SnapshotError;

/// Exact result or argument count. `u8::MAX` is the open / all-values mode.
/// Zero is a real count: discard every result, or pass no arguments.
pub(crate) const COUNT_OPEN: u8 = u8::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Capture {
    /// Register in the enclosing frame, captured as an open upvalue.
    Local(u8),
    /// Upvalue already held by the enclosing closure.
    Upvalue(u8),
}

/// Which primitive comparison `Compare` / `CompareBranch` performs. `>` and `>=` compile
/// to `Lt` and `Le` with the operands swapped, as in Lua.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CmpKind {
    Eq,
    Ne,
    Lt,
    Le,
}

impl CmpKind {
    fn tag(self) -> u8 {
        match self {
            Self::Eq => 0,
            Self::Ne => 1,
            Self::Lt => 2,
            Self::Le => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, SnapshotError> {
        Ok(match tag {
            0 => Self::Eq,
            1 => Self::Ne,
            2 => Self::Lt,
            3 => Self::Le,
            _ => return Err(SnapshotError::InvalidTag),
        })
    }
}

/// The binary operator of `Op::Arith`. `+` has its own opcode, `Add`,
/// whose integer case runs in the hot tier. See `arith`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Idiv,
    Mod,
    Pow,
    Band,
    Bor,
    Bxor,
    Shl,
    Shr,
}

impl ArithOp {
    pub(crate) fn is_bitwise(self) -> bool {
        matches!(
            self,
            Self::Band | Self::Bor | Self::Bxor | Self::Shl | Self::Shr
        )
    }

    /// The metamethod consulted when the operands are not numbers.
    pub(crate) fn event(self) -> &'static [u8] {
        match self {
            Self::Add => b"__add",
            Self::Sub => b"__sub",
            Self::Mul => b"__mul",
            Self::Div => b"__div",
            Self::Idiv => b"__idiv",
            Self::Mod => b"__mod",
            Self::Pow => b"__pow",
            Self::Band => b"__band",
            Self::Bor => b"__bor",
            Self::Bxor => b"__bxor",
            Self::Shl => b"__shl",
            Self::Shr => b"__shr",
        }
    }

    fn tag(self) -> u8 {
        self as u8
    }

    fn from_tag(tag: u8) -> Result<Self, SnapshotError> {
        Ok(match tag {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Idiv,
            5 => Self::Mod,
            6 => Self::Pow,
            7 => Self::Band,
            8 => Self::Bor,
            9 => Self::Bxor,
            10 => Self::Shl,
            11 => Self::Shr,
            _ => return Err(SnapshotError::InvalidTag),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Op {
    LoadNil {
        dst: u8,
    },
    LoadInt {
        dst: u8,
        value: i64,
    },
    LoadFloat {
        dst: u8,
        bits: u64,
    },
    LoadBool {
        dst: u8,
        value: bool,
    },
    LoadBytes {
        dst: u8,
        const_index: u32,
    },
    Move {
        dst: u8,
        src: u8,
    },
    Add {
        dst: u8,
        a: u8,
        b: u8,
    },
    NewTable {
        dst: u8,
    },
    GetTable {
        dst: u8,
        table: u8,
        key: u8,
    },
    SetTable {
        table: u8,
        key: u8,
        src: u8,
    },
    MakeClosure {
        dst: u8,
        child: u32,
    },
    GetUpvalue {
        dst: u8,
        index: u8,
    },
    SetUpvalue {
        index: u8,
        src: u8,
    },
    Call {
        func: u8,
        nargs: u8,
        nresults: u8,
    },
    Return {
        base: u8,
        count: u8,
    },
    Yield {
        base: u8,
        count: u8,
    },
    /// Resumable. Entering it charges once and stops in `Prepared` before the
    /// host runs. The program counter stays on this instruction until the
    /// host result is written.
    CallHost {
        dst: u8,
        symbol: u16,
        arg: u8,
    },
    NewThread {
        dst: u8,
        child: u32,
    },
    /// `nresults == COUNT_OPEN` keeps every yielded or returned value.
    /// The child runs on its own thread; this frame records `Pending::Resuming`
    /// and does not advance until the child yields or returns.
    Resume {
        dest: u8,
        thread: u8,
        nresults: u8,
    },
    GetGlobal {
        dst: u8,
    },
    /// Number of varargs available to this frame. Lowering of `select('#', ...)`.
    VarargLen {
        dst: u8,
    },
    /// Copy varargs to `dst`. `COUNT_OPEN` copies every vararg and extends `top`.
    Vararg {
        dst: u8,
        count: u8,
    },
    /// `top - from`, the width of an open result region. Lowering of
    /// `select('#', <open call>)`.
    OpenLen {
        dst: u8,
        from: u8,
    },
    /// Record a local destination. Does not store.
    AssignLocal {
        reg: u8,
    },
    /// Record a table destination using the table and key values as they are now.
    AssignField {
        table: u8,
        key: u8,
    },
    /// Resumable. Charges once, then stores right to left from the recorded
    /// destinations. The pending cursor is a safe point before any store and
    /// between stores.
    AssignCommit {
        src: u8,
        n: u8,
    },
    /// Relative to the next instruction. A jump never closes upvalues; an
    /// edge that leaves a scope with captured locals is compiled as
    /// `CloseUpvalues` followed by `Jump`.
    Jump {
        offset: i32,
    },
    /// Jump when `src` is `nil` or `false`. Every other value, including `0`
    /// and the empty string, falls through.
    JumpIfFalse {
        src: u8,
        offset: i32,
    },
    /// Close this frame's open upvalues whose register is `from` or above.
    /// The cell keeps its identity and takes the register's current value.
    /// Ordinary captured locals only; this is not Lua's `<close>`.
    CloseUpvalues {
        from: u8,
    },
    /// `dst = -src`: numbers and numeric strings (see `arith`), else
    /// `__unm`.
    Neg {
        dst: u8,
        src: u8,
    },
    /// Prepare a numeric `for` over `base..=base + 3` (see `fornum`).
    /// When the loop runs zero times, jump by `offset`, past its `ForLoop`.
    ForPrep {
        base: u8,
        offset: i32,
    },
    /// Advance a numeric `for`; while it continues, jump by `offset` back to
    /// the first body instruction.
    ForLoop {
        base: u8,
        offset: i32,
    },
    /// `dst = obj[key]`, language-level indexing (see `index`), not raw.
    Index {
        dst: u8,
        obj: u8,
        key: u8,
    },
    /// `obj[key] = src`, language-level assignment.
    SetIndex {
        obj: u8,
        key: u8,
        src: u8,
    },
    /// `dst = obj.name` with `name` the string constant `name`.
    GetField {
        dst: u8,
        obj: u8,
        name: u32,
    },
    /// `obj.name = src` with `name` a string constant.
    SetField {
        obj: u8,
        name: u32,
        src: u8,
    },
    /// Raw-store the open result region starting at `src` into
    /// `table[start]`, `table[start + 1]`, and so on. A constructor's last
    /// list field when it is a multi-result call. One fuel.
    SetList {
        table: u8,
        src: u8,
        start: u32,
    },
    /// `dst = #src`, language-level length: a string's byte length, a
    /// table's `__len`, else its raw border. See `index::len`.
    Len {
        dst: u8,
        src: u8,
    },
    /// `dst = a <kind> b` as a boolean: primitive comparison (see
    /// `compare`), else `__eq`, `__lt`, or `__le` (see `ops`).
    Compare {
        kind: CmpKind,
        dst: u8,
        a: u8,
        b: u8,
    },
    /// Compare without materializing a value; jump if its boolean equals
    /// `sense`. A yielding handler finishes through the Truth continuation.
    CompareBranch {
        kind: CmpKind,
        a: u8,
        b: u8,
        sense: bool,
        offset: i32,
    },
    /// Jump when both registers are integers and `a < b`.
    JumpIfLt {
        a: u8,
        b: u8,
        offset: i32,
    },
    /// Raw `next`. Nil `key` starts at the first live entry. Writes the
    /// successor key at `dst` and its value at `dst + 1`, or nil and nil at
    /// the end. One fuel. Not the stdlib result-count wrapper.
    Next {
        dst: u8,
        table: u8,
        key: u8,
    },
    /// Raw table border. Not the length metamethod.
    RawLen {
        dst: u8,
        table: u8,
    },
    /// `dst = a <op> b` for every binary arithmetic and bitwise operator
    /// but `+`: primitive (see `arith`), else the operator's metamethod.
    Arith {
        op: ArithOp,
        dst: u8,
        a: u8,
        b: u8,
    },
    /// Arithmetic with one exact integer immediate. `reverse` puts it
    /// before `reg`; mixed numbers and non-numbers use the shared cold path.
    ArithK {
        op: ArithOp,
        dst: u8,
        reg: u8,
        constant: i64,
        reverse: bool,
    },
    /// `dst = ~src`: an integral number, else `__bnot`.
    BNot {
        dst: u8,
        src: u8,
    },
    /// `dst = a .. b`: strings and numbers, else `__concat`.
    Concat {
        dst: u8,
        a: u8,
        b: u8,
    },
    /// Register `reg` holds a new `<close>` local: nil and false are
    /// ignored; any other value must have a `__close` metamethod, and joins
    /// the thread's to-be-closed list (ADR 0026).
    MarkClose {
        reg: u8,
    },
    /// Leave every scope down to register `from`: close its open upvalues,
    /// then call `__close` on each of its to-be-closed values, newest first,
    /// then continue after the instruction. Where a scope holds only
    /// captured locals, `CloseUpvalues` is enough.
    CloseScope {
        from: u8,
    },
    /// Close the suspended or failed coroutine in `thread`, as
    /// `coroutine.close` would: run its pending `__close` calls without
    /// yielding, then `dst, dst + 1 = true, nil` or `false, error`.
    CloseThread {
        dst: u8,
        thread: u8,
    },
    /// Decide a generic `for` after its iterator call, which left the loop
    /// variables at `base + 4`. When the first is not nil, copy it to the
    /// hidden control at `base + 2` and jump by `offset` back to the body;
    /// otherwise fall through to the loop's exit. Only nil ends the loop.
    GenericForLoop {
        base: u8,
        offset: i32,
    },
    /// `return f(args)` outside every `<close>` scope (ADR 0029): call the
    /// value at `func` with `nargs` arguments, `COUNT_OPEN` for those up to
    /// `top`, in place of the running frame, whose caller gets every result
    /// the callee returns. A Lua callee's frame replaces this one. A
    /// `Return` of the open window at `func` always follows, for a native
    /// callee a thread's first frame calls.
    TailCall {
        func: u8,
        nargs: u8,
    },
    Halt,
}

// Wide jump offsets and constant, name, and child indexes must not make
// every instruction bigger: `LoadInt`'s `i64` already sets the size.
const _: () = assert!(std::mem::size_of::<Op>() == 16);

impl Op {
    /// Registers this instruction can replace. Used only by cold diagnostic
    /// symbolic execution; keep this match exhaustive as the opcode grows.
    pub(crate) fn writes(self, reg: u8) -> bool {
        match self {
            Self::LoadNil { dst }
            | Self::LoadInt { dst, .. }
            | Self::LoadFloat { dst, .. }
            | Self::LoadBool { dst, .. }
            | Self::LoadBytes { dst, .. }
            | Self::Move { dst, .. }
            | Self::Add { dst, .. }
            | Self::NewTable { dst }
            | Self::GetTable { dst, .. }
            | Self::MakeClosure { dst, .. }
            | Self::GetUpvalue { dst, .. }
            | Self::CallHost { dst, .. }
            | Self::NewThread { dst, .. }
            | Self::GetGlobal { dst }
            | Self::VarargLen { dst }
            | Self::OpenLen { dst, .. }
            | Self::Neg { dst, .. }
            | Self::Index { dst, .. }
            | Self::GetField { dst, .. }
            | Self::Len { dst, .. }
            | Self::Compare { dst, .. }
            | Self::RawLen { dst, .. }
            | Self::Arith { dst, .. }
            | Self::ArithK { dst, .. }
            | Self::BNot { dst, .. }
            | Self::Concat { dst, .. } => reg == dst,
            Self::Call { func, .. } | Self::TailCall { func, .. } => reg >= func,
            // A resume can return an open result window, or an error pair.
            Self::Resume { dest, .. } => reg >= dest,
            Self::Vararg { dst, count } => {
                reg >= dst && (count == COUNT_OPEN || reg < dst.saturating_add(count))
            }
            Self::Next { dst, .. } | Self::CloseThread { dst, .. } => {
                reg == dst || u16::from(reg) == u16::from(dst) + 1
            }
            Self::ForPrep { base, .. } => reg >= base && u16::from(reg) <= u16::from(base) + 3,
            Self::ForLoop { base, .. } => {
                reg == base
                    || u16::from(reg) == u16::from(base) + 1
                    || u16::from(reg) == u16::from(base) + 3
            }
            Self::GenericForLoop { base, .. } => u16::from(reg) == u16::from(base) + 2,
            // Destinations are recorded by preceding AssignLocal/AssignField
            // instructions; without tracing that pending list, any register
            // may have been assigned here.
            Self::AssignCommit { .. } => true,
            Self::SetTable { .. }
            | Self::SetUpvalue { .. }
            | Self::Return { .. }
            | Self::Yield { .. }
            | Self::AssignLocal { .. }
            | Self::AssignField { .. }
            | Self::Jump { .. }
            | Self::JumpIfFalse { .. }
            | Self::CloseUpvalues { .. }
            | Self::SetIndex { .. }
            | Self::SetField { .. }
            | Self::SetList { .. }
            | Self::CompareBranch { .. }
            | Self::JumpIfLt { .. }
            | Self::MarkClose { .. }
            | Self::CloseScope { .. }
            | Self::Halt => false,
        }
    }

    /// Explicit control-flow edge for diagnostic writer validation. Kept
    /// exhaustive, like [`Op::writes`], so a new branch cannot be missed.
    pub(crate) fn jump_offset(self) -> Option<i32> {
        match self {
            Self::Jump { offset }
            | Self::JumpIfFalse { offset, .. }
            | Self::ForPrep { offset, .. }
            | Self::ForLoop { offset, .. }
            | Self::CompareBranch { offset, .. }
            | Self::JumpIfLt { offset, .. }
            | Self::GenericForLoop { offset, .. } => Some(offset),
            Self::Add { .. }
            | Self::Arith { .. }
            | Self::ArithK { .. }
            | Self::AssignCommit { .. }
            | Self::AssignField { .. }
            | Self::AssignLocal { .. }
            | Self::BNot { .. }
            | Self::Call { .. }
            | Self::CallHost { .. }
            | Self::CloseScope { .. }
            | Self::CloseThread { .. }
            | Self::CloseUpvalues { .. }
            | Self::Compare { .. }
            | Self::Concat { .. }
            | Self::GetField { .. }
            | Self::GetGlobal { .. }
            | Self::GetTable { .. }
            | Self::GetUpvalue { .. }
            | Self::Halt
            | Self::Index { .. }
            | Self::Len { .. }
            | Self::LoadBool { .. }
            | Self::LoadBytes { .. }
            | Self::LoadFloat { .. }
            | Self::LoadInt { .. }
            | Self::LoadNil { .. }
            | Self::MakeClosure { .. }
            | Self::MarkClose { .. }
            | Self::Move { .. }
            | Self::Neg { .. }
            | Self::NewTable { .. }
            | Self::NewThread { .. }
            | Self::Next { .. }
            | Self::OpenLen { .. }
            | Self::RawLen { .. }
            | Self::Resume { .. }
            | Self::Return { .. }
            | Self::SetField { .. }
            | Self::SetIndex { .. }
            | Self::SetList { .. }
            | Self::SetTable { .. }
            | Self::SetUpvalue { .. }
            | Self::TailCall { .. }
            | Self::Vararg { .. }
            | Self::VarargLen { .. }
            | Self::Yield { .. } => None,
        }
    }

    pub(crate) fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::LoadNil { dst } => {
                out.push(1);
                out.push(dst);
            }
            Self::LoadInt { dst, value } => {
                out.push(2);
                out.push(dst);
                out.extend(value.to_le_bytes());
            }
            Self::LoadFloat { dst, bits } => {
                out.push(3);
                out.push(dst);
                out.extend(bits.to_le_bytes());
            }
            Self::LoadBytes { dst, const_index } => {
                out.push(4);
                out.push(dst);
                out.extend(const_index.to_le_bytes());
            }
            Self::Move { dst, src } => {
                out.push(5);
                out.push(dst);
                out.push(src);
            }
            Self::Add { dst, a, b } => {
                out.push(6);
                out.push(dst);
                out.push(a);
                out.push(b);
            }
            Self::NewTable { dst } => {
                out.push(7);
                out.push(dst);
            }
            Self::GetTable { dst, table, key } => {
                out.push(8);
                out.push(dst);
                out.push(table);
                out.push(key);
            }
            Self::SetTable { table, key, src } => {
                out.push(9);
                out.push(table);
                out.push(key);
                out.push(src);
            }
            Self::MakeClosure { dst, child } => {
                out.push(10);
                out.push(dst);
                out.extend(child.to_le_bytes());
            }
            Self::GetUpvalue { dst, index } => {
                out.push(11);
                out.push(dst);
                out.push(index);
            }
            Self::SetUpvalue { index, src } => {
                out.push(12);
                out.push(index);
                out.push(src);
            }
            Self::Call {
                func,
                nargs,
                nresults,
            } => {
                out.push(13);
                out.push(func);
                out.push(nargs);
                out.push(nresults);
            }
            Self::Return { base, count } => {
                out.push(14);
                out.push(base);
                out.push(count);
            }
            Self::Yield { base, count } => {
                out.push(15);
                out.push(base);
                out.push(count);
            }
            Self::CallHost { dst, symbol, arg } => {
                out.push(16);
                out.push(dst);
                out.extend(symbol.to_le_bytes());
                out.push(arg);
            }
            Self::NewThread { dst, child } => {
                out.push(17);
                out.push(dst);
                out.extend(child.to_le_bytes());
            }
            Self::Resume {
                dest,
                thread,
                nresults,
            } => {
                out.push(18);
                out.push(dest);
                out.push(thread);
                out.push(nresults);
            }
            Self::GetGlobal { dst } => {
                out.push(19);
                out.push(dst);
            }
            Self::VarargLen { dst } => {
                out.push(21);
                out.push(dst);
            }
            Self::Vararg { dst, count } => {
                out.push(22);
                out.push(dst);
                out.push(count);
            }
            Self::OpenLen { dst, from } => {
                out.push(23);
                out.push(dst);
                out.push(from);
            }
            Self::AssignLocal { reg } => {
                out.push(24);
                out.push(reg);
            }
            Self::AssignField { table, key } => {
                out.push(25);
                out.push(table);
                out.push(key);
            }
            Self::AssignCommit { src, n } => {
                out.push(26);
                out.push(src);
                out.push(n);
            }
            Self::Jump { offset } => {
                out.push(27);
                out.extend(offset.to_le_bytes());
            }
            Self::JumpIfLt { a, b, offset } => {
                out.push(28);
                out.push(a);
                out.push(b);
                out.extend(offset.to_le_bytes());
            }
            Self::Next { dst, table, key } => {
                out.push(29);
                out.push(dst);
                out.push(table);
                out.push(key);
            }
            Self::RawLen { dst, table } => {
                out.push(30);
                out.push(dst);
                out.push(table);
            }
            Self::LoadBool { dst, value } => {
                out.push(31);
                out.push(dst);
                out.push(u8::from(value));
            }
            Self::CloseUpvalues { from } => {
                out.push(32);
                out.push(from);
            }
            Self::JumpIfFalse { src, offset } => {
                out.push(33);
                out.push(src);
                out.extend(offset.to_le_bytes());
            }
            Self::Compare { kind, dst, a, b } => {
                out.push(34);
                out.push(kind.tag());
                out.push(dst);
                out.push(a);
                out.push(b);
            }
            Self::CompareBranch {
                kind,
                a,
                b,
                sense,
                offset,
            } => {
                out.push(53);
                out.push(kind.tag());
                out.push(a);
                out.push(b);
                out.push(u8::from(sense));
                out.extend(offset.to_le_bytes());
            }
            Self::Neg { dst, src } => {
                out.push(35);
                out.push(dst);
                out.push(src);
            }
            Self::ForPrep { base, offset } => {
                out.push(36);
                out.push(base);
                out.extend(offset.to_le_bytes());
            }
            Self::ForLoop { base, offset } => {
                out.push(37);
                out.push(base);
                out.extend(offset.to_le_bytes());
            }
            Self::Index { dst, obj, key } => {
                out.push(38);
                out.push(dst);
                out.push(obj);
                out.push(key);
            }
            Self::SetIndex { obj, key, src } => {
                out.push(39);
                out.push(obj);
                out.push(key);
                out.push(src);
            }
            Self::GetField { dst, obj, name } => {
                out.push(40);
                out.push(dst);
                out.push(obj);
                out.extend(name.to_le_bytes());
            }
            Self::SetField { obj, name, src } => {
                out.push(41);
                out.push(obj);
                out.extend(name.to_le_bytes());
                out.push(src);
            }
            Self::SetList { table, src, start } => {
                out.push(42);
                out.push(table);
                out.push(src);
                out.extend(start.to_le_bytes());
            }
            Self::Len { dst, src } => {
                out.push(43);
                out.push(dst);
                out.push(src);
            }
            Self::Arith { op, dst, a, b } => {
                out.push(44);
                out.push(op.tag());
                out.push(dst);
                out.push(a);
                out.push(b);
            }
            Self::ArithK {
                op,
                dst,
                reg,
                constant,
                reverse,
            } => {
                out.push(52);
                out.push(op.tag());
                out.push(dst);
                out.push(reg);
                out.extend(constant.to_le_bytes());
                out.push(u8::from(reverse));
            }
            Self::BNot { dst, src } => {
                out.push(45);
                out.push(dst);
                out.push(src);
            }
            Self::Concat { dst, a, b } => {
                out.push(46);
                out.push(dst);
                out.push(a);
                out.push(b);
            }
            Self::MarkClose { reg } => {
                out.push(47);
                out.push(reg);
            }
            Self::CloseScope { from } => {
                out.push(48);
                out.push(from);
            }
            Self::CloseThread { dst, thread } => {
                out.push(49);
                out.push(dst);
                out.push(thread);
            }
            Self::GenericForLoop { base, offset } => {
                out.push(50);
                out.push(base);
                out.extend(offset.to_le_bytes());
            }
            Self::TailCall { func, nargs } => {
                out.push(51);
                out.push(func);
                out.push(nargs);
            }
            Self::Halt => out.push(20),
        }
    }

    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let tag = read_u8(input)?;
        Ok(match tag {
            1 => Self::LoadNil {
                dst: read_u8(input)?,
            },
            2 => Self::LoadInt {
                dst: read_u8(input)?,
                value: read_i64(input)?,
            },
            3 => Self::LoadFloat {
                dst: read_u8(input)?,
                bits: read_u64(input)?,
            },
            4 => Self::LoadBytes {
                dst: read_u8(input)?,
                const_index: read_u32(input)?,
            },
            5 => Self::Move {
                dst: read_u8(input)?,
                src: read_u8(input)?,
            },
            6 => Self::Add {
                dst: read_u8(input)?,
                a: read_u8(input)?,
                b: read_u8(input)?,
            },
            7 => Self::NewTable {
                dst: read_u8(input)?,
            },
            8 => Self::GetTable {
                dst: read_u8(input)?,
                table: read_u8(input)?,
                key: read_u8(input)?,
            },
            9 => Self::SetTable {
                table: read_u8(input)?,
                key: read_u8(input)?,
                src: read_u8(input)?,
            },
            10 => Self::MakeClosure {
                dst: read_u8(input)?,
                child: read_u32(input)?,
            },
            11 => Self::GetUpvalue {
                dst: read_u8(input)?,
                index: read_u8(input)?,
            },
            12 => Self::SetUpvalue {
                index: read_u8(input)?,
                src: read_u8(input)?,
            },
            13 => Self::Call {
                func: read_u8(input)?,
                nargs: read_u8(input)?,
                nresults: read_u8(input)?,
            },
            14 => Self::Return {
                base: read_u8(input)?,
                count: read_u8(input)?,
            },
            15 => Self::Yield {
                base: read_u8(input)?,
                count: read_u8(input)?,
            },
            16 => Self::CallHost {
                dst: read_u8(input)?,
                symbol: read_u16(input)?,
                arg: read_u8(input)?,
            },
            17 => Self::NewThread {
                dst: read_u8(input)?,
                child: read_u32(input)?,
            },
            18 => Self::Resume {
                dest: read_u8(input)?,
                thread: read_u8(input)?,
                nresults: read_u8(input)?,
            },
            19 => Self::GetGlobal {
                dst: read_u8(input)?,
            },
            20 => Self::Halt,
            43 => Self::Len {
                dst: read_u8(input)?,
                src: read_u8(input)?,
            },
            44 => Self::Arith {
                op: ArithOp::from_tag(read_u8(input)?)?,
                dst: read_u8(input)?,
                a: read_u8(input)?,
                b: read_u8(input)?,
            },
            52 => Self::ArithK {
                op: ArithOp::from_tag(read_u8(input)?)?,
                dst: read_u8(input)?,
                reg: read_u8(input)?,
                constant: read_i64(input)?,
                reverse: read_bool(input)?,
            },
            53 => Self::CompareBranch {
                kind: CmpKind::from_tag(read_u8(input)?)?,
                a: read_u8(input)?,
                b: read_u8(input)?,
                sense: read_bool(input)?,
                offset: read_i32(input)?,
            },
            45 => Self::BNot {
                dst: read_u8(input)?,
                src: read_u8(input)?,
            },
            46 => Self::Concat {
                dst: read_u8(input)?,
                a: read_u8(input)?,
                b: read_u8(input)?,
            },
            47 => Self::MarkClose {
                reg: read_u8(input)?,
            },
            48 => Self::CloseScope {
                from: read_u8(input)?,
            },
            49 => Self::CloseThread {
                dst: read_u8(input)?,
                thread: read_u8(input)?,
            },
            50 => Self::GenericForLoop {
                base: read_u8(input)?,
                offset: read_i32(input)?,
            },
            51 => Self::TailCall {
                func: read_u8(input)?,
                nargs: read_u8(input)?,
            },
            38 => Self::Index {
                dst: read_u8(input)?,
                obj: read_u8(input)?,
                key: read_u8(input)?,
            },
            39 => Self::SetIndex {
                obj: read_u8(input)?,
                key: read_u8(input)?,
                src: read_u8(input)?,
            },
            40 => Self::GetField {
                dst: read_u8(input)?,
                obj: read_u8(input)?,
                name: read_u32(input)?,
            },
            41 => Self::SetField {
                obj: read_u8(input)?,
                name: read_u32(input)?,
                src: read_u8(input)?,
            },
            42 => Self::SetList {
                table: read_u8(input)?,
                src: read_u8(input)?,
                start: read_u32(input)?,
            },
            35 => Self::Neg {
                dst: read_u8(input)?,
                src: read_u8(input)?,
            },
            36 => Self::ForPrep {
                base: read_u8(input)?,
                offset: read_i32(input)?,
            },
            37 => Self::ForLoop {
                base: read_u8(input)?,
                offset: read_i32(input)?,
            },
            21 => Self::VarargLen {
                dst: read_u8(input)?,
            },
            22 => Self::Vararg {
                dst: read_u8(input)?,
                count: read_u8(input)?,
            },
            23 => Self::OpenLen {
                dst: read_u8(input)?,
                from: read_u8(input)?,
            },
            24 => Self::AssignLocal {
                reg: read_u8(input)?,
            },
            25 => Self::AssignField {
                table: read_u8(input)?,
                key: read_u8(input)?,
            },
            26 => Self::AssignCommit {
                src: read_u8(input)?,
                n: read_u8(input)?,
            },
            27 => Self::Jump {
                offset: read_i32(input)?,
            },
            28 => Self::JumpIfLt {
                a: read_u8(input)?,
                b: read_u8(input)?,
                offset: read_i32(input)?,
            },
            29 => Self::Next {
                dst: read_u8(input)?,
                table: read_u8(input)?,
                key: read_u8(input)?,
            },
            30 => Self::RawLen {
                dst: read_u8(input)?,
                table: read_u8(input)?,
            },
            31 => Self::LoadBool {
                dst: read_u8(input)?,
                value: read_u8(input)? != 0,
            },
            32 => Self::CloseUpvalues {
                from: read_u8(input)?,
            },
            34 => Self::Compare {
                kind: CmpKind::from_tag(read_u8(input)?)?,
                dst: read_u8(input)?,
                a: read_u8(input)?,
                b: read_u8(input)?,
            },
            33 => Self::JumpIfFalse {
                src: read_u8(input)?,
                offset: read_i32(input)?,
            },
            _ => return Err(SnapshotError::InvalidTag),
        })
    }
}

fn read_bool(input: &mut &[u8]) -> Result<bool, SnapshotError> {
    match read_u8(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SnapshotError::InvalidTag),
    }
}

pub(crate) fn read_u8(input: &mut &[u8]) -> Result<u8, SnapshotError> {
    let (first, rest) = input.split_first().ok_or(SnapshotError::Truncated)?;
    *input = rest;
    Ok(*first)
}

pub(crate) fn read_u16(input: &mut &[u8]) -> Result<u16, SnapshotError> {
    read_array(input).map(u16::from_le_bytes)
}

pub(crate) fn read_u32(input: &mut &[u8]) -> Result<u32, SnapshotError> {
    read_array(input).map(u32::from_le_bytes)
}

pub(crate) fn read_u64(input: &mut &[u8]) -> Result<u64, SnapshotError> {
    read_array(input).map(u64::from_le_bytes)
}

pub(crate) fn read_i64(input: &mut &[u8]) -> Result<i64, SnapshotError> {
    read_array(input).map(i64::from_le_bytes)
}

pub(crate) fn read_i32(input: &mut &[u8]) -> Result<i32, SnapshotError> {
    read_array(input).map(i32::from_le_bytes)
}

fn read_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], SnapshotError> {
    if input.len() < N {
        return Err(SnapshotError::Truncated);
    }
    let mut buf = [0; N];
    buf.copy_from_slice(&input[..N]);
    *input = &input[N..];
    Ok(buf)
}

pub(crate) fn encode_capture(capture: Capture, out: &mut Vec<u8>) {
    match capture {
        Capture::Local(slot) => {
            out.push(1);
            out.push(slot);
        }
        Capture::Upvalue(index) => {
            out.push(2);
            out.push(index);
        }
    }
}

pub(crate) fn decode_capture(input: &mut &[u8]) -> Result<Capture, SnapshotError> {
    match read_u8(input)? {
        1 => Ok(Capture::Local(read_u8(input)?)),
        2 => Ok(Capture::Upvalue(read_u8(input)?)),
        _ => Err(SnapshotError::InvalidTag),
    }
}

#[cfg(test)]
mod wide_tests {
    use super::*;

    #[test]
    fn wide_operands_keep_their_full_width_in_the_codec() {
        assert_eq!(std::mem::size_of::<Op>(), 16);
        for op in [
            Op::LoadBytes {
                dst: 1,
                const_index: u32::MAX,
            },
            Op::GetField {
                dst: 1,
                obj: 2,
                name: u32::MAX,
            },
            Op::SetField {
                obj: 1,
                name: u32::MAX,
                src: 2,
            },
            Op::MakeClosure {
                dst: 1,
                child: u32::MAX,
            },
            Op::NewThread {
                dst: 1,
                child: u32::MAX,
            },
            Op::Jump { offset: i32::MIN },
            Op::JumpIfFalse {
                src: 1,
                offset: i32::MAX,
            },
            Op::JumpIfLt {
                a: 1,
                b: 2,
                offset: i32::MIN,
            },
            Op::ForPrep {
                base: 1,
                offset: i32::MAX,
            },
            Op::ForLoop {
                base: 1,
                offset: i32::MIN,
            },
            Op::GenericForLoop {
                base: 1,
                offset: i32::MIN,
            },
        ] {
            let mut bytes = Vec::new();
            op.encode(&mut bytes);
            let mut input = bytes.as_slice();
            assert_eq!(Op::decode(&mut input).unwrap(), op);
            assert!(input.is_empty());
            assert!(Op::decode(&mut &bytes[..bytes.len() - 1]).is_err());
        }
    }
}
