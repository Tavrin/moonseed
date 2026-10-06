//! Hand-built prototypes for the proof kernel. Not a parser.

use crate::opcode::{COUNT_OPEN, Capture, Op};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProtoSpec {
    pub(crate) ops: Vec<Op>,
    pub(crate) byte_consts: Vec<Vec<u8>>,
    pub(crate) captures: Vec<Capture>,
    pub(crate) children: Vec<ProtoSpec>,
    pub(crate) max_reg: u8,
    pub(crate) params: u8,
    pub(crate) vararg: bool,
    /// Debug information (ADR 0040). Hand-built programs have none.
    pub(crate) debug: Option<Box<crate::debuginfo::DebugInfo>>,
}

struct Asm {
    ops: Vec<Op>,
    byte_consts: Vec<Vec<u8>>,
    captures: Vec<Capture>,
    children: Vec<ProtoSpec>,
    max_reg: u8,
    params: u8,
    vararg: bool,
}

impl Asm {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            byte_consts: Vec::new(),
            captures: Vec::new(),
            children: Vec::new(),
            max_reg: 0,
            params: 0,
            vararg: false,
        }
    }

    fn touch(&mut self, reg: u8) {
        self.max_reg = self.max_reg.max(reg.saturating_add(1));
    }

    fn emit(&mut self, op: Op) {
        match op {
            Op::LoadNil { dst }
            | Op::LoadInt { dst, .. }
            | Op::LoadFloat { dst, .. }
            | Op::LoadBool { dst, .. }
            | Op::LoadBytes { dst, .. }
            | Op::NewTable { dst }
            | Op::MakeClosure { dst, .. }
            | Op::GetUpvalue { dst, .. }
            | Op::NewThread { dst, .. }
            | Op::GetGlobal { dst } => self.touch(dst),
            Op::Move { dst, src } => {
                self.touch(dst);
                self.touch(src);
            }
            Op::Add { dst, a, b } => {
                self.touch(dst);
                self.touch(a);
                self.touch(b);
            }
            Op::GetTable { dst, table, key } => {
                self.touch(dst);
                self.touch(table);
                self.touch(key);
            }
            Op::SetTable { table, key, src } => {
                self.touch(table);
                self.touch(key);
                self.touch(src);
            }
            Op::SetUpvalue { src, .. } => self.touch(src),
            Op::Call { func, nargs, .. } | Op::TailCall { func, nargs } => {
                if nargs == COUNT_OPEN {
                    self.touch(func);
                } else {
                    self.touch(func.saturating_add(nargs));
                }
            }
            Op::Return { base, count } | Op::Yield { base, count } => {
                if count != COUNT_OPEN && count > 0 {
                    self.touch(base.saturating_add(count - 1));
                } else {
                    self.touch(base);
                }
            }
            Op::CallHost { dst, arg, .. } => {
                self.touch(dst);
                self.touch(arg);
            }
            Op::Resume { dest, thread, .. } => {
                self.touch(dest);
                self.touch(thread);
            }
            Op::VarargLen { dst } | Op::AssignLocal { reg: dst } => self.touch(dst),
            Op::Vararg { dst, count } => {
                self.touch(dst);
                if count != COUNT_OPEN && count > 0 {
                    self.touch(dst.saturating_add(count - 1));
                }
            }
            Op::OpenLen { dst, from } => {
                self.touch(dst);
                self.touch(from);
            }
            Op::AssignField { table, key } => {
                self.touch(table);
                self.touch(key);
            }
            Op::AssignCommit { src, n } => {
                self.touch(src);
                if n != COUNT_OPEN && n > 0 {
                    self.touch(src.saturating_add(n - 1));
                }
            }
            Op::Jump { .. } => {}
            Op::JumpIfFalse { src, .. } => self.touch(src),
            Op::CloseUpvalues { from } | Op::CloseScope { from } | Op::MarkClose { reg: from } => {
                self.touch(from)
            }
            Op::CloseThread { dst, thread } => {
                self.touch(dst.saturating_add(1));
                self.touch(thread);
            }
            Op::Len { dst, src } => {
                self.touch(dst);
                self.touch(src);
            }
            Op::Index { dst, obj, key } => {
                self.touch(dst);
                self.touch(obj);
                self.touch(key);
            }
            Op::SetIndex { obj, key, src } => {
                self.touch(obj);
                self.touch(key);
                self.touch(src);
            }
            Op::GetField { dst, obj, .. } => {
                self.touch(dst);
                self.touch(obj);
            }
            Op::SetField { obj, src, .. } => {
                self.touch(obj);
                self.touch(src);
            }
            Op::SetList { table, src, .. } => {
                self.touch(table);
                self.touch(src);
            }
            Op::Neg { dst, src } | Op::BNot { dst, src } => {
                self.touch(dst);
                self.touch(src);
            }
            Op::Arith { dst, a, b, .. } | Op::Concat { dst, a, b } => {
                self.touch(dst);
                self.touch(a);
                self.touch(b);
            }
            Op::ArithK { dst, reg, .. } => {
                self.touch(dst);
                self.touch(reg);
            }
            Op::ForPrep { base, .. } | Op::ForLoop { base, .. } => {
                self.touch(base.saturating_add(3));
            }
            Op::GenericForLoop { base, .. } => self.touch(base.saturating_add(4)),
            Op::Compare { dst, a, b, .. } => {
                self.touch(dst);
                self.touch(a);
                self.touch(b);
            }
            Op::JumpIfLt { a, b, .. } | Op::CompareBranch { a, b, .. } => {
                self.touch(a);
                self.touch(b);
            }
            Op::Next { dst, table, key } => {
                self.touch(dst.saturating_add(1));
                self.touch(table);
                self.touch(key);
            }
            Op::RawLen { dst, table } => {
                self.touch(dst);
                self.touch(table);
            }
            Op::Halt => {}
        }
        self.ops.push(op);
    }

    fn bytes(&mut self, text: &str) -> u32 {
        let index = self.byte_consts.len() as u32;
        self.byte_consts.push(text.as_bytes().to_vec());
        index
    }

    fn child(&mut self, spec: ProtoSpec) -> u32 {
        let index = self.children.len() as u32;
        self.children.push(spec);
        index
    }

    fn finish(self) -> ProtoSpec {
        let max_reg = self.max_reg.max(self.params).max(1);
        ProtoSpec {
            ops: self.ops,
            byte_consts: self.byte_consts,
            captures: self.captures,
            children: self.children,
            max_reg,
            params: self.params,
            vararg: self.vararg,
            debug: None,
        }
    }
}

fn proto_inc() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.captures = vec![Capture::Local(0)];
    asm.emit(Op::GetUpvalue { dst: 0, index: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    asm.emit(Op::SetUpvalue { index: 0, src: 0 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

fn proto_get() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.captures = vec![Capture::Local(0)];
    asm.emit(Op::GetUpvalue { dst: 0, index: 0 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

fn proto_make_pair() -> ProtoSpec {
    let mut asm = Asm::new();
    let inc = asm.child(proto_inc());
    let get = asm.child(proto_get());
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::MakeClosure { dst: 1, child: inc });
    asm.emit(Op::MakeClosure { dst: 2, child: get });
    asm.emit(Op::Return { base: 1, count: 2 });
    asm.finish()
}

fn proto_inner() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.params = 1;
    asm.emit(Op::Call {
        func: 0,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

fn proto_outer() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.params = 2;
    let inner = asm.child(proto_inner());
    asm.emit(Op::MakeClosure {
        dst: 2,
        child: inner,
    });
    asm.emit(Op::Move { dst: 3, src: 0 });
    asm.emit(Op::Call {
        func: 2,
        nargs: 1,
        nresults: 1,
    });
    // Save the result before the next call reuses this register window.
    asm.emit(Op::Move { dst: 0, src: 2 });
    asm.emit(Op::Call {
        func: 1,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Return { base: 0, count: 2 });
    asm.finish()
}

fn proto_yielder() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 42 });
    asm.emit(Op::Yield { base: 0, count: 1 });
    asm.emit(Op::LoadInt { dst: 0, value: 43 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

pub(crate) fn canonical() -> ProtoSpec {
    let mut asm = Asm::new();
    let make_pair = asm.child(proto_make_pair());
    let outer = asm.child(proto_outer());
    let yielder = asm.child(proto_yielder());
    let key_b = asm.bytes("b");
    let key_a = asm.bytes("a");
    let key_tag = asm.bytes("tag");
    let key_inc = asm.bytes("inc");
    let key_get = asm.bytes("get");
    let key_mark = asm.bytes("mark");
    let key_inc_result = asm.bytes("inc_result");
    let key_get_result = asm.bytes("get_result");
    let key_yielder = asm.bytes("yielder");
    let key_yielded = asm.bytes("yielded");
    let key_global_a = asm.bytes("A");
    let sym_mark = asm.bytes("mark");

    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::NewTable { dst: 1 });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_b,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 1,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_a,
    });
    asm.emit(Op::SetTable {
        table: 1,
        key: 2,
        src: 0,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_tag,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 7 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 3,
    });

    asm.emit(Op::MakeClosure {
        dst: 2,
        child: make_pair,
    });
    asm.emit(Op::Call {
        func: 2,
        nargs: 0,
        nresults: 2,
    });
    asm.emit(Op::Move { dst: 4, src: 2 });
    asm.emit(Op::Move { dst: 5, src: 3 });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_inc,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 4,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_get,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 5,
    });

    asm.emit(Op::LoadInt { dst: 3, value: 1 });
    asm.emit(Op::CallHost {
        dst: 6,
        symbol: u16::try_from(sym_mark).unwrap(),
        arg: 3,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_mark,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 6,
    });

    asm.emit(Op::MakeClosure {
        dst: 7,
        child: outer,
    });
    asm.emit(Op::Move { dst: 8, src: 4 });
    asm.emit(Op::Move { dst: 9, src: 5 });
    asm.emit(Op::Call {
        func: 7,
        nargs: 2,
        nresults: 2,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_inc_result,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 7,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_get_result,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 8,
    });

    asm.emit(Op::NewThread {
        dst: 10,
        child: yielder,
    });
    asm.emit(Op::Resume {
        dest: 11,
        thread: 10,
        nresults: 1,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_yielder,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 10,
    });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_yielded,
    });
    asm.emit(Op::SetTable {
        table: 0,
        key: 2,
        src: 11,
    });

    asm.emit(Op::GetGlobal { dst: 12 });
    asm.emit(Op::LoadBytes {
        dst: 2,
        const_index: key_global_a,
    });
    asm.emit(Op::SetTable {
        table: 12,
        key: 2,
        src: 0,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

pub(crate) fn park_program() -> ProtoSpec {
    let mut asm = Asm::new();
    let key_result = asm.bytes("result");
    let sym_park = asm.bytes("park");
    asm.emit(Op::LoadInt { dst: 0, value: 1 });
    asm.emit(Op::CallHost {
        dst: 1,
        symbol: u16::try_from(sym_park).unwrap(),
        arg: 0,
    });
    asm.emit(Op::GetGlobal { dst: 2 });
    asm.emit(Op::LoadBytes {
        dst: 3,
        const_index: key_result,
    });
    asm.emit(Op::SetTable {
        table: 2,
        key: 3,
        src: 1,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

pub(crate) fn yield_program() -> ProtoSpec {
    proto_yielder()
}

#[cfg(test)]
fn proto_many() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 10 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 30 });
    asm.emit(Op::Return { base: 0, count: 3 });
    asm.finish()
}

#[cfg(test)]
fn proto_none() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::Return { base: 0, count: 0 });
    asm.finish()
}

#[cfg(test)]
fn proto_one() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 7 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

/// `function count(...) return select('#', ...), ... end`
#[cfg(test)]
fn proto_count() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.vararg = true;
    asm.params = 0;
    asm.emit(Op::VarargLen { dst: 0 });
    asm.emit(Op::Vararg {
        dst: 1,
        count: COUNT_OPEN,
    });
    asm.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });
    asm.finish()
}

#[cfg(test)]
fn proto_box() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

/// Hand-lowering of the multi-result fixture in `crates/moonseed/fixtures/lua/multi_result.lua`.
///
/// Final entry registers:
/// 0 open count, 1 paren count,
/// 2..5 `count(many())` values `3, 10, nil, 30`,
/// 6..7 `count((many()))` values `1, 10`,
/// 8..11 `many()` adjusted to 4 results,
/// 12..13 `many()` adjusted to 2 results (`10, nil`, not `10, 30`),
/// 14 single result 7,
/// 15..16 zero-result call padded to two nils,
/// 20 the closure after a zero-result call, 21 the cleared argument slot.
#[cfg(test)]
pub(crate) fn results_program() -> ProtoSpec {
    let mut asm = Asm::new();
    let many = asm.child(proto_many());
    let count = asm.child(proto_count());
    let none = asm.child(proto_none());
    let one = asm.child(proto_one());
    asm.emit(Op::MakeClosure {
        dst: 30,
        child: many,
    });
    asm.emit(Op::MakeClosure {
        dst: 31,
        child: count,
    });
    asm.emit(Op::MakeClosure {
        dst: 32,
        child: none,
    });
    asm.emit(Op::MakeClosure {
        dst: 33,
        child: one,
    });

    asm.emit(Op::Move { dst: 20, src: 31 });
    asm.emit(Op::Move { dst: 21, src: 30 });
    asm.emit(Op::Call {
        func: 21,
        nargs: 0,
        nresults: COUNT_OPEN,
    });
    asm.emit(Op::Call {
        func: 20,
        nargs: COUNT_OPEN,
        nresults: COUNT_OPEN,
    });
    asm.emit(Op::OpenLen { dst: 0, from: 20 });
    asm.emit(Op::Move { dst: 2, src: 20 });
    asm.emit(Op::Move { dst: 3, src: 21 });
    asm.emit(Op::Move { dst: 4, src: 22 });
    asm.emit(Op::Move { dst: 5, src: 23 });

    asm.emit(Op::Move { dst: 20, src: 31 });
    asm.emit(Op::Move { dst: 21, src: 30 });
    asm.emit(Op::Call {
        func: 21,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Call {
        func: 20,
        nargs: 1,
        nresults: COUNT_OPEN,
    });
    asm.emit(Op::OpenLen { dst: 1, from: 20 });
    asm.emit(Op::Move { dst: 6, src: 20 });
    asm.emit(Op::Move { dst: 7, src: 21 });

    asm.emit(Op::Move { dst: 20, src: 30 });
    asm.emit(Op::Call {
        func: 20,
        nargs: 0,
        nresults: 4,
    });
    asm.emit(Op::Move { dst: 8, src: 20 });
    asm.emit(Op::Move { dst: 9, src: 21 });
    asm.emit(Op::Move { dst: 10, src: 22 });
    asm.emit(Op::Move { dst: 11, src: 23 });

    asm.emit(Op::Move { dst: 20, src: 30 });
    asm.emit(Op::Call {
        func: 20,
        nargs: 0,
        nresults: 2,
    });
    asm.emit(Op::Move { dst: 12, src: 20 });
    asm.emit(Op::Move { dst: 13, src: 21 });

    asm.emit(Op::Move { dst: 20, src: 33 });
    asm.emit(Op::Call {
        func: 20,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 14, src: 20 });

    asm.emit(Op::Move { dst: 20, src: 32 });
    asm.emit(Op::Call {
        func: 20,
        nargs: 0,
        nresults: 2,
    });
    asm.emit(Op::Move { dst: 15, src: 20 });
    asm.emit(Op::Move { dst: 16, src: 21 });

    asm.emit(Op::Move { dst: 20, src: 30 });
    asm.emit(Op::LoadInt { dst: 21, value: 99 });
    asm.emit(Op::Call {
        func: 20,
        nargs: 0,
        nresults: 0,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// `i, t[i] = 2, 99` with `i` starting at 1.
///
/// Registers at `Halt`: 0 is `i`, 1 is `t`, 2 is `t[1]`, 3 is `t[2]`.
#[cfg(test)]
pub(crate) fn assign_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 1 });
    asm.emit(Op::NewTable { dst: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 2 });
    asm.emit(Op::LoadInt { dst: 3, value: 99 });
    asm.emit(Op::Move { dst: 4, src: 0 });
    asm.emit(Op::AssignLocal { reg: 0 });
    asm.emit(Op::AssignField { table: 1, key: 4 });
    asm.emit(Op::AssignCommit { src: 2, n: 2 });
    asm.emit(Op::LoadInt { dst: 5, value: 1 });
    asm.emit(Op::GetTable {
        dst: 2,
        table: 1,
        key: 5,
    });
    asm.emit(Op::LoadInt { dst: 5, value: 2 });
    asm.emit(Op::GetTable {
        dst: 3,
        table: 1,
        key: 5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// Both destinations address `t[1]`. Right-to-left stores leave `t[1] == 2`.
#[cfg(test)]
pub(crate) fn assign_order_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 2 });
    asm.emit(Op::LoadInt { dst: 4, value: 3 });
    asm.emit(Op::LoadInt { dst: 5, value: 1 });
    asm.emit(Op::GetTable {
        dst: 6,
        table: 0,
        key: 5,
    });
    asm.emit(Op::AssignField { table: 0, key: 5 });
    asm.emit(Op::AssignField { table: 0, key: 6 });
    asm.emit(Op::AssignCommit { src: 3, n: 2 });
    asm.emit(Op::GetTable {
        dst: 7,
        table: 0,
        key: 5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// The returned table is live only in the result register after the call.
#[cfg(test)]
pub(crate) fn box_program() -> ProtoSpec {
    let mut asm = Asm::new();
    let box_proto = asm.child(proto_box());
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: box_proto,
    });
    asm.emit(Op::Call {
        func: 0,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// The field key is a heap object held by the pending assignment, not by a register.
#[cfg(test)]
pub(crate) fn assign_key_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::NewTable { dst: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 99 });
    asm.emit(Op::AssignField { table: 0, key: 1 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::AssignCommit { src: 2, n: 1 });
    asm.emit(Op::Halt);
    asm.finish()
}

/// `r0` counts from 0 to `limit` by one. Three instructions per iteration.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn counted_adds(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 0 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    asm.emit(Op::Add { dst: 3, a: 3, b: 1 });
    asm.emit(Op::JumpIfLt {
        a: 3,
        b: 2,
        offset: -3,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// Each iteration takes one conditional, skips another, and falls through a third.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn branchy(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 0 });
    asm.emit(Op::LoadInt { dst: 4, value: 0 });
    asm.emit(Op::LoadInt { dst: 5, value: 1 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    asm.emit(Op::Add { dst: 3, a: 3, b: 1 });
    asm.emit(Op::JumpIfLt {
        a: 4,
        b: 5,
        offset: 1,
    });
    asm.emit(Op::Jump { offset: 1 });
    asm.emit(Op::JumpIfLt {
        a: 5,
        b: 4,
        offset: 1,
    });
    asm.emit(Op::JumpIfLt {
        a: 3,
        b: 2,
        offset: -6,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn proto_add_one() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.params = 1;
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

/// `r1` becomes `limit` after that many scalar calls.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn scalar_calls(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let leaf = asm.child(proto_add_one());
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: leaf,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::Move { dst: 3, src: 0 });
    asm.emit(Op::Move { dst: 4, src: 1 });
    asm.emit(Op::Call {
        func: 3,
        nargs: 1,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 1, src: 3 });
    asm.emit(Op::JumpIfLt {
        a: 1,
        b: 2,
        offset: -5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn proto_call_leaf() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.params = 2;
    asm.emit(Op::Move { dst: 2, src: 1 });
    asm.emit(Op::Move { dst: 3, src: 0 });
    asm.emit(Op::Call {
        func: 2,
        nargs: 1,
        nresults: 1,
    });
    asm.emit(Op::Return { base: 2, count: 1 });
    asm.finish()
}

/// `r2` becomes `limit`. Each iteration is a Lua call that itself calls.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn nested_calls(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let leaf = asm.child(proto_add_one());
    let mid = asm.child(proto_call_leaf());
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: leaf,
    });
    asm.emit(Op::MakeClosure { dst: 1, child: mid });
    asm.emit(Op::LoadInt { dst: 2, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 3,
        value: limit,
    });
    asm.emit(Op::Move { dst: 4, src: 1 });
    asm.emit(Op::Move { dst: 5, src: 2 });
    asm.emit(Op::Move { dst: 6, src: 0 });
    asm.emit(Op::Call {
        func: 4,
        nargs: 2,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 2, src: 4 });
    asm.emit(Op::JumpIfLt {
        a: 2,
        b: 3,
        offset: -6,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn proto_triple() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 10 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 30 });
    asm.emit(Op::Return { base: 0, count: 3 });
    asm.finish()
}

/// `r1` sums the non-nil results. `limit` iterations of `10, nil, 30` yield `40 * limit`.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn multi_calls(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let many = asm.child(proto_triple());
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: many,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 0 });
    asm.emit(Op::LoadInt { dst: 8, value: 1 });
    asm.emit(Op::Move { dst: 10, src: 0 });
    asm.emit(Op::Call {
        func: 10,
        nargs: 0,
        nresults: 3,
    });
    asm.emit(Op::Add {
        dst: 1,
        a: 1,
        b: 10,
    });
    asm.emit(Op::Add {
        dst: 1,
        a: 1,
        b: 12,
    });
    asm.emit(Op::Add { dst: 3, a: 3, b: 8 });
    asm.emit(Op::JumpIfLt {
        a: 3,
        b: 2,
        offset: -6,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn proto_bump() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.captures = vec![Capture::Local(0)];
    asm.emit(Op::GetUpvalue { dst: 0, index: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    asm.emit(Op::SetUpvalue { index: 0, src: 0 });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

/// `r0` is the shared upvalue and ends at `limit`.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn upvalue_calls(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let bump = asm.child(proto_bump());
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::MakeClosure {
        dst: 1,
        child: bump,
    });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::Move { dst: 3, src: 1 });
    asm.emit(Op::Call {
        func: 3,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::JumpIfLt {
        a: 0,
        b: 2,
        offset: -3,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn field_loop(string_key: bool, limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let key = if string_key {
        let index = asm.bytes("k");
        Op::LoadBytes {
            dst: 1,
            const_index: index,
        }
    } else {
        Op::LoadInt { dst: 1, value: 1 }
    };
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(key);
    asm.emit(Op::LoadInt { dst: 2, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 3,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 4, value: 0 });
    asm.emit(Op::LoadInt { dst: 5, value: 1 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::GetTable {
        dst: 2,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Add { dst: 2, a: 2, b: 5 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::Add { dst: 4, a: 4, b: 5 });
    asm.emit(Op::JumpIfLt {
        a: 4,
        b: 3,
        offset: -5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// `r2` is the field value and ends at `limit`. The string key is loaded once.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn string_fields(limit: i64) -> ProtoSpec {
    field_loop(true, limit)
}

/// `r2` is the field value and ends at `limit`. The key is the integer 1.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn int_fields(limit: i64) -> ProtoSpec {
    field_loop(false, limit)
}

/// `r1` counts allocations. `r0` holds the latest table.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn alloc_churn(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 1, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 2,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 1 });
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::Add { dst: 1, a: 1, b: 3 });
    asm.emit(Op::JumpIfLt {
        a: 1,
        b: 2,
        offset: -3,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// `r0` becomes `limit`. The host symbol is `tick` and must return `arg + 1`.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn host_ticks(limit: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    let symbol = asm.bytes("tick");
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 1,
        value: limit,
    });
    asm.emit(Op::CallHost {
        dst: 2,
        symbol: u16::try_from(symbol).unwrap(),
        arg: 0,
    });
    asm.emit(Op::Move { dst: 0, src: 2 });
    asm.emit(Op::JumpIfLt {
        a: 0,
        b: 1,
        offset: -3,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

pub(crate) struct TableProof {
    pub(crate) spec: ProtoSpec,
    /// Instructions to run so the next one is `Next` after the current key was deleted.
    pub(crate) resume_at: u32,
}

/// Insertion-order traversal, deletion of the current key, and raw borders.
///
/// After `resume_at` instructions, register 10 holds key `1`, that key is a
/// dead anchor, and the following `Next` must yield `2`.
pub(crate) fn table_semantics_program() -> TableProof {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    for (key, value) in [(1, 10), (2, 20), (3, 30)] {
        asm.emit(Op::LoadInt { dst: 1, value: key });
        asm.emit(Op::LoadInt { dst: 2, value });
        asm.emit(Op::SetTable {
            table: 0,
            key: 1,
            src: 2,
        });
    }
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Next {
        dst: 10,
        table: 0,
        key: 1,
    });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 10,
        src: 2,
    });
    let resume_at = asm.ops.len() as u32;
    asm.emit(Op::Next {
        dst: 12,
        table: 0,
        key: 10,
    });
    asm.emit(Op::Next {
        dst: 14,
        table: 0,
        key: 12,
    });
    asm.emit(Op::Next {
        dst: 16,
        table: 0,
        key: 14,
    });

    asm.emit(Op::NewTable { dst: 3 });
    for (key, value) in [(1, 10), (2, 20), (3, 30)] {
        asm.emit(Op::LoadInt { dst: 1, value: key });
        asm.emit(Op::LoadInt { dst: 2, value });
        asm.emit(Op::SetTable {
            table: 3,
            key: 1,
            src: 2,
        });
    }
    asm.emit(Op::RawLen { dst: 18, table: 3 });

    asm.emit(Op::NewTable { dst: 4 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 10 });
    asm.emit(Op::SetTable {
        table: 4,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 3 });
    asm.emit(Op::LoadInt { dst: 2, value: 30 });
    asm.emit(Op::SetTable {
        table: 4,
        key: 1,
        src: 2,
    });
    asm.emit(Op::RawLen { dst: 19, table: 4 });

    asm.emit(Op::NewTable { dst: 5 });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 20 });
    asm.emit(Op::SetTable {
        table: 5,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 3 });
    asm.emit(Op::LoadInt { dst: 2, value: 30 });
    asm.emit(Op::SetTable {
        table: 5,
        key: 1,
        src: 2,
    });
    asm.emit(Op::RawLen { dst: 20, table: 5 });

    asm.emit(Op::NewTable { dst: 6 });
    asm.emit(Op::RawLen { dst: 21, table: 6 });

    asm.emit(Op::NewTable { dst: 7 });
    asm.emit(Op::LoadInt { dst: 1, value: 0 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: -1 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 1,
        src: 2,
    });
    let label = asm.bytes("x");
    asm.emit(Op::LoadBytes {
        dst: 1,
        const_index: label,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 1,
        src: 2,
    });
    asm.emit(Op::NewTable { dst: 8 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 8,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 7 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 8 });
    asm.emit(Op::SetTable {
        table: 7,
        key: 1,
        src: 2,
    });
    asm.emit(Op::RawLen { dst: 22, table: 7 });

    asm.emit(Op::NewTable { dst: 9 });
    asm.emit(Op::LoadInt {
        dst: 1,
        value: 1_000_000_000_000,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 9,
        key: 1,
        src: 2,
    });
    asm.emit(Op::RawLen { dst: 23, table: 9 });

    asm.emit(Op::NewTable { dst: 24 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 24,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 24,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt {
        dst: 1,
        value: i64::MAX,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 24,
        key: 1,
        src: 2,
    });
    asm.emit(Op::RawLen { dst: 26, table: 24 });
    asm.emit(Op::Halt);
    TableProof {
        spec: asm.finish(),
        resume_at,
    }
}

/// Tiny program used to trip the hard fuel limit quickly.
pub(crate) fn spin_adds() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    for _ in 0..32 {
        asm.emit(Op::Add { dst: 0, a: 0, b: 1 });
    }
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) struct DropPoint {
    pub(crate) spec: ProtoSpec,
    pub(crate) before_drop: u32,
}

#[cfg(test)]
pub(crate) fn invalid_next_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 10 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 99 });
    asm.emit(Op::Next {
        dst: 4,
        table: 0,
        key: 3,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn next_type_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 1 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Next {
        dst: 2,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn next_nan_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadFloat {
        dst: 1,
        bits: f64::NAN.to_bits(),
    });
    asm.emit(Op::Next {
        dst: 2,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn update_during_next_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 10 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 20 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Next {
        dst: 3,
        table: 0,
        key: 1,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 99 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 3,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 21 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::Next {
        dst: 5,
        table: 0,
        key: 3,
    });
    asm.emit(Op::GetTable {
        dst: 7,
        table: 0,
        key: 3,
    });
    asm.emit(Op::GetTable {
        dst: 8,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn delete_other_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    for (key, value) in [(1, 10), (2, 20), (3, 30)] {
        asm.emit(Op::LoadInt { dst: 1, value: key });
        asm.emit(Op::LoadInt { dst: 2, value });
        asm.emit(Op::SetTable {
            table: 0,
            key: 1,
            src: 2,
        });
    }
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Next {
        dst: 3,
        table: 0,
        key: 1,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::Next {
        dst: 5,
        table: 0,
        key: 3,
    });
    asm.emit(Op::Next {
        dst: 7,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Next {
        dst: 9,
        table: 0,
        key: 5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn reinsert_program() -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 10 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 20 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 1, value: 1 });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 11 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Next {
        dst: 3,
        table: 0,
        key: 1,
    });
    asm.emit(Op::Next {
        dst: 5,
        table: 0,
        key: 3,
    });
    asm.emit(Op::Next {
        dst: 7,
        table: 0,
        key: 5,
    });
    asm.emit(Op::GetTable {
        dst: 8,
        table: 0,
        key: 5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

#[cfg(test)]
pub(crate) fn object_key_program() -> DropPoint {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::NewTable { dst: 1 });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 20 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 3,
        src: 2,
    });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::Next {
        dst: 5,
        table: 0,
        key: 1,
    });
    let before_drop = asm.ops.len() as u32;
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::Halt);
    DropPoint {
        spec: asm.finish(),
        before_drop,
    }
}

#[cfg(test)]
pub(crate) fn string_key_program() -> DropPoint {
    let mut asm = Asm::new();
    let label = asm.bytes("k");
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadBytes {
        dst: 1,
        const_index: label,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    asm.emit(Op::LoadInt { dst: 3, value: 2 });
    asm.emit(Op::LoadInt { dst: 2, value: 20 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 3,
        src: 2,
    });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 2,
    });
    let before_drop = asm.ops.len() as u32;
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::LoadBytes {
        dst: 4,
        const_index: label,
    });
    asm.emit(Op::Next {
        dst: 5,
        table: 0,
        key: 4,
    });
    asm.emit(Op::Halt);
    DropPoint {
        spec: asm.finish(),
        before_drop,
    }
}

/// Keys `1..=n` stored as the integer `1`. Optionally deleted again, leaving
/// `n` dead anchors. Register 1 finishes equal to `n`.
#[cfg(feature = "__measure")]
pub(crate) fn filled_table(n: i64, delete_all: bool) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::NewTable { dst: 0 });
    asm.emit(Op::LoadInt { dst: 1, value: 0 });
    asm.emit(Op::LoadInt { dst: 2, value: n });
    asm.emit(Op::LoadInt { dst: 3, value: 1 });
    asm.emit(Op::Add { dst: 1, a: 1, b: 3 });
    asm.emit(Op::SetTable {
        table: 0,
        key: 1,
        src: 3,
    });
    asm.emit(Op::JumpIfLt {
        a: 1,
        b: 2,
        offset: -3,
    });
    if delete_all {
        asm.emit(Op::LoadInt { dst: 1, value: 0 });
        asm.emit(Op::LoadNil { dst: 4 });
        asm.emit(Op::Add { dst: 1, a: 1, b: 3 });
        asm.emit(Op::SetTable {
            table: 0,
            key: 1,
            src: 4,
        });
        asm.emit(Op::JumpIfLt {
            a: 1,
            b: 2,
            offset: -3,
        });
    }
    asm.emit(Op::Halt);
    asm.finish()
}

/// Hand lowering of `crates/moonseed/fixtures/lua/closure_pair.lua`.
///
/// The source compiler does not have to emit this exact instruction stream.
#[cfg(any(test, feature = "__measure"))]
pub(crate) fn closure_pair_hand() -> ProtoSpec {
    let mut asm = Asm::new();
    let make_pair = asm.child(proto_make_pair());
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: make_pair,
    });
    asm.emit(Op::Call {
        func: 0,
        nargs: 0,
        nresults: 2,
    });
    asm.emit(Op::Move { dst: 2, src: 0 });
    asm.emit(Op::Call {
        func: 2,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 3, src: 1 });
    asm.emit(Op::Call {
        func: 3,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 4, src: 0 });
    asm.emit(Op::Call {
        func: 4,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Move { dst: 5, src: 1 });
    asm.emit(Op::Call {
        func: 5,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::Return { base: 2, count: 4 });
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
fn capturing(mut spec: ProtoSpec, reg: u8) -> ProtoSpec {
    spec.captures = vec![Capture::Local(reg)];
    spec
}

#[cfg(any(test, feature = "__measure"))]
fn proto_const(value: i64) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value });
    asm.emit(Op::Return { base: 0, count: 1 });
    asm.finish()
}

#[cfg(any(test, feature = "__measure"))]
/// Hand form of `crates/moonseed/fixtures/lua/branch_close.lua`. With `close` false the
/// `CloseUpvalues` becomes a self-move, so the captured `x` stays open on a
/// register that `local x = 99` then reuses.
pub(crate) fn branch_close_hand(close: bool) -> ProtoSpec {
    let mut asm = Asm::new();
    let inc = asm.child(capturing(proto_inc(), 2));
    let get = asm.child(capturing(proto_get(), 2));
    let zero = asm.child(proto_const(0));
    asm.emit(Op::LoadNil { dst: 0 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::LoadBool {
        dst: 3,
        value: true,
    });
    asm.emit(Op::JumpIfFalse { src: 3, offset: 5 });
    asm.emit(Op::LoadInt { dst: 2, value: 10 });
    asm.emit(Op::MakeClosure { dst: 0, child: inc });
    asm.emit(Op::MakeClosure { dst: 1, child: get });
    asm.emit(if close {
        Op::CloseUpvalues { from: 2 }
    } else {
        Op::Move { dst: 2, src: 2 }
    });
    asm.emit(Op::Jump { offset: 2 });
    asm.emit(Op::MakeClosure {
        dst: 0,
        child: zero,
    });
    asm.emit(Op::MakeClosure {
        dst: 1,
        child: zero,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 99 });
    for (dst, src) in [(3, 1), (4, 0), (5, 1)] {
        asm.emit(Op::Move { dst, src });
        asm.emit(Op::Call {
            func: dst,
            nargs: 0,
            nresults: 1,
        });
    }
    asm.emit(Op::Move { dst: 6, src: 2 });
    asm.emit(Op::Return { base: 3, count: 4 });
    asm.finish()
}

#[cfg(test)]
/// Program counter of the `CloseUpvalues` in [`branch_close_hand`].
pub(crate) const BRANCH_CLOSE_PC: u32 = 7;

#[cfg(test)]
/// A table captured by two closures, closed, then left reachable only
/// through the closed cell. `drop` frees everything but the call result.
pub(crate) fn closed_heap_program() -> ProtoSpec {
    let mut asm = Asm::new();
    let get = asm.child(capturing(proto_get(), 1));
    asm.emit(Op::NewTable { dst: 1 });
    asm.emit(Op::MakeClosure { dst: 0, child: get });
    asm.emit(Op::MakeClosure { dst: 2, child: get });
    asm.emit(Op::CloseUpvalues { from: 1 });
    asm.emit(Op::LoadNil { dst: 1 });
    asm.emit(Op::LoadNil { dst: 2 });
    asm.emit(Op::Move { dst: 3, src: 0 });
    asm.emit(Op::Call {
        func: 3,
        nargs: 0,
        nresults: 1,
    });
    asm.emit(Op::LoadNil { dst: 0 });
    asm.emit(Op::Return { base: 3, count: 1 });
    asm.finish()
}

/// `r0` counts to `limit`. Each iteration runs one `JumpIfFalse` on a
/// constant condition: taken (skipping a filler) when `cond` is false, not
/// taken when true.
#[cfg(feature = "__measure")]
pub(crate) fn truth_branches(limit: i64, cond: bool) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.emit(Op::LoadInt { dst: 0, value: 0 });
    asm.emit(Op::LoadInt {
        dst: 1,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: 2, value: 1 });
    asm.emit(Op::LoadBool {
        dst: 3,
        value: cond,
    });
    asm.emit(Op::JumpIfFalse { src: 3, offset: 1 });
    asm.emit(Op::LoadNil { dst: 4 });
    asm.emit(Op::Add { dst: 0, a: 0, b: 2 });
    asm.emit(Op::JumpIfLt {
        a: 0,
        b: 1,
        offset: -4,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// `r[n]` counts to `limit`. Each iteration makes one closure capturing
/// registers `0..n`. With `close`, a `CloseUpvalues { from: 0 }` follows, so
/// every iteration allocates and closes `n` fresh cells; without it the
/// closure reuses the `n` cells that are still open.
#[cfg(feature = "__measure")]
pub(crate) fn close_loop(n: u8, limit: i64, close: bool) -> ProtoSpec {
    let mut child = Asm::new();
    child.captures = (0..n).map(Capture::Local).collect();
    child.emit(Op::Return { base: 0, count: 0 });
    let mut asm = Asm::new();
    let proto = asm.child(child.finish());
    let (count, stop, one, func) = (n, n + 1, n + 2, n + 3);
    asm.emit(Op::LoadInt {
        dst: count,
        value: 0,
    });
    asm.emit(Op::LoadInt {
        dst: stop,
        value: limit,
    });
    asm.emit(Op::LoadInt { dst: one, value: 1 });
    asm.emit(Op::MakeClosure {
        dst: func,
        child: proto,
    });
    asm.emit(if close {
        Op::CloseUpvalues { from: 0 }
    } else {
        Op::Move {
            dst: func,
            src: func,
        }
    });
    asm.emit(Op::Add {
        dst: count,
        a: count,
        b: one,
    });
    asm.emit(Op::JumpIfLt {
        a: count,
        b: stop,
        offset: -5,
    });
    asm.emit(Op::Halt);
    asm.finish()
}

/// A coroutine body that runs `pcall(f)` where `f` yields 41, then returns
/// 2. Main resumes it twice and returns `41, true, 2`: the protected call
/// stays in progress across the yield.
#[cfg(test)]
pub(crate) fn pcall_yield_program() -> ProtoSpec {
    let mut yielder = Asm::new();
    yielder.emit(Op::LoadInt { dst: 0, value: 41 });
    yielder.emit(Op::Yield { base: 0, count: 1 });
    yielder.emit(Op::LoadInt { dst: 0, value: 2 });
    yielder.emit(Op::Return { base: 0, count: 1 });

    let mut body = Asm::new();
    let f = body.child(yielder.finish());
    let pcall = body.bytes("pcall");
    body.emit(Op::GetGlobal { dst: 0 });
    body.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: pcall,
    });
    body.emit(Op::MakeClosure { dst: 2, child: f });
    body.emit(Op::Call {
        func: 1,
        nargs: 1,
        nresults: COUNT_OPEN,
    });
    body.emit(Op::Return {
        base: 1,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    main.emit(Op::NewThread { dst: 0, child });
    main.emit(Op::Resume {
        dest: 1,
        thread: 0,
        nresults: 1,
    });
    main.emit(Op::Resume {
        dest: 2,
        thread: 0,
        nresults: 2,
    });
    main.emit(Op::Return { base: 1, count: 3 });
    main.finish()
}

/// A coroutine body that runs `xpcall(f, h)`: `f` raises `"e"`, and the
/// message handler `h` tries to yield. A handler cannot yield, so each try
/// is another error in the handler, until "error in error handling". Main
/// resumes the body once and returns what `xpcall` returned.
#[cfg(test)]
pub(crate) fn handler_yield_program() -> ProtoSpec {
    let mut raiser = Asm::new();
    let error = raiser.bytes("error");
    let message = raiser.bytes("e");
    raiser.emit(Op::GetGlobal { dst: 0 });
    raiser.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: error,
    });
    raiser.emit(Op::LoadBytes {
        dst: 2,
        const_index: message,
    });
    raiser.emit(Op::LoadInt { dst: 3, value: 0 });
    raiser.emit(Op::Call {
        func: 1,
        nargs: 2,
        nresults: 0,
    });
    raiser.emit(Op::Return { base: 0, count: 0 });

    let mut handler = Asm::new();
    handler.params = 1;
    handler.emit(Op::Yield { base: 0, count: 1 });
    handler.emit(Op::Return { base: 0, count: 1 });

    let mut body = Asm::new();
    let f = body.child(raiser.finish());
    let h = body.child(handler.finish());
    let xpcall = body.bytes("xpcall");
    body.emit(Op::GetGlobal { dst: 0 });
    body.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: xpcall,
    });
    body.emit(Op::MakeClosure { dst: 2, child: f });
    body.emit(Op::MakeClosure { dst: 3, child: h });
    body.emit(Op::Call {
        func: 1,
        nargs: 2,
        nresults: COUNT_OPEN,
    });
    body.emit(Op::Return {
        base: 1,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    main.emit(Op::NewThread { dst: 0, child });
    main.emit(Op::Resume {
        dest: 1,
        thread: 0,
        nresults: 2,
    });
    main.emit(Op::Return { base: 1, count: 2 });
    main.finish()
}

/// `v(1, 2, 3)` where the vararg `v` calls a function and then returns
/// `...`. The call's return must not trim `v`'s varargs, which sit above
/// its registers.
#[cfg(test)]
pub(crate) fn vararg_after_call_program() -> ProtoSpec {
    let mut callee = Asm::new();
    callee.emit(Op::Return { base: 0, count: 0 });

    let mut v = Asm::new();
    v.vararg = true;
    let g = v.child(callee.finish());
    v.emit(Op::MakeClosure { dst: 0, child: g });
    v.emit(Op::Call {
        func: 0,
        nargs: 0,
        nresults: 0,
    });
    v.emit(Op::Vararg {
        dst: 0,
        count: COUNT_OPEN,
    });
    v.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let f = main.child(v.finish());
    main.emit(Op::MakeClosure { dst: 0, child: f });
    main.emit(Op::LoadInt { dst: 1, value: 1 });
    main.emit(Op::LoadInt { dst: 2, value: 2 });
    main.emit(Op::LoadInt { dst: 3, value: 3 });
    main.emit(Op::Call {
        func: 0,
        nargs: 3,
        nresults: COUNT_OPEN,
    });
    main.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });
    main.finish()
}

/// `v(1, 2, 3)`, where vararg `v` calls `pcall(g)` with an empty Lua `g`,
/// or `pcall(add, 5, 6)` when `native`, keeping two results or none, then
/// returns `...`: `1 2 3`. The protected call's return must not trim or
/// clear `v`'s varargs. `v` holds eight registers so that `g`'s frame stays
/// below its varargs: a callee frame that reaches past its caller's
/// registers overlaps them, a layout gap of its own (`LUA_COMPATIBILITY.md`).
#[cfg(test)]
pub(crate) fn vararg_pcall_program(native: bool, keep: bool) -> ProtoSpec {
    let mut callee = Asm::new();
    callee.emit(Op::Return { base: 0, count: 0 });

    let mut v = Asm::new();
    v.vararg = true;
    let g = v.child(callee.finish());
    let pcall = v.bytes("pcall");
    let add = v.bytes("add");
    let nresults = if keep { 2 } else { 0 };
    v.emit(Op::LoadInt { dst: 7, value: 0 });
    v.emit(Op::GetGlobal { dst: 0 });
    v.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: pcall,
    });
    if native {
        v.emit(Op::GetField {
            dst: 2,
            obj: 0,
            name: add,
        });
        v.emit(Op::LoadInt { dst: 3, value: 5 });
        v.emit(Op::LoadInt { dst: 4, value: 6 });
        v.emit(Op::Call {
            func: 1,
            nargs: 3,
            nresults,
        });
    } else {
        v.emit(Op::MakeClosure { dst: 2, child: g });
        v.emit(Op::Call {
            func: 1,
            nargs: 1,
            nresults,
        });
    }
    v.emit(Op::Vararg {
        dst: 0,
        count: COUNT_OPEN,
    });
    v.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let f = main.child(v.finish());
    main.emit(Op::MakeClosure { dst: 0, child: f });
    main.emit(Op::LoadInt { dst: 1, value: 1 });
    main.emit(Op::LoadInt { dst: 2, value: 2 });
    main.emit(Op::LoadInt { dst: 3, value: 3 });
    main.emit(Op::Call {
        func: 0,
        nargs: 3,
        nresults: COUNT_OPEN,
    });
    main.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });
    main.finish()
}

/// A `__close` handler: appends `name`, then its error argument when that
/// is not nil, to the global table `log`; then yields `name` when `yields`,
/// and raises `fail` (level 0) when given.
fn closer_proto(name: &str, yields: bool, fail: Option<&str>) -> ProtoSpec {
    let mut asm = Asm::new();
    asm.params = 2;
    let log = asm.bytes("log");
    let label = asm.bytes(name);
    asm.emit(Op::GetGlobal { dst: 2 });
    asm.emit(Op::GetField {
        dst: 3,
        obj: 2,
        name: log,
    });
    for error_arg in [false, true] {
        asm.emit(Op::Len { dst: 4, src: 3 });
        asm.emit(Op::LoadInt { dst: 5, value: 1 });
        asm.emit(Op::Add { dst: 4, a: 4, b: 5 });
        if error_arg {
            asm.emit(Op::Move { dst: 6, src: 1 });
        } else {
            asm.emit(Op::LoadBytes {
                dst: 6,
                const_index: label,
            });
        }
        asm.emit(Op::SetIndex {
            obj: 3,
            key: 4,
            src: 6,
        });
    }
    if yields {
        asm.emit(Op::LoadBytes {
            dst: 6,
            const_index: label,
        });
        asm.emit(Op::Yield { base: 6, count: 1 });
    }
    if let Some(fail) = fail {
        emit_error(&mut asm, 7, fail);
    }
    asm.emit(Op::Return { base: 0, count: 0 });
    asm.finish()
}

/// `error(message, 0)`, using registers `r..r + 3`.
fn emit_error(asm: &mut Asm, r: u8, message: &str) {
    let error = asm.bytes("error");
    let text = asm.bytes(message);
    asm.emit(Op::GetGlobal { dst: r });
    asm.emit(Op::GetField {
        dst: r,
        obj: r,
        name: error,
    });
    asm.emit(Op::LoadBytes {
        dst: r + 1,
        const_index: text,
    });
    asm.emit(Op::LoadInt {
        dst: r + 2,
        value: 0,
    });
    asm.emit(Op::Call {
        func: r,
        nargs: 2,
        nresults: 0,
    });
}

/// `r = setmetatable({}, { __close = closer })`, a value `MarkClose` takes,
/// using registers `r..r + 4`.
fn emit_closable(asm: &mut Asm, r: u8, closer: ProtoSpec) {
    let closer = asm.child(closer);
    let setmetatable = asm.bytes("setmetatable");
    let close = asm.bytes("__close");
    asm.emit(Op::GetGlobal { dst: r + 1 });
    asm.emit(Op::GetField {
        dst: r,
        obj: r + 1,
        name: setmetatable,
    });
    asm.emit(Op::NewTable { dst: r + 1 });
    asm.emit(Op::NewTable { dst: r + 2 });
    asm.emit(Op::MakeClosure {
        dst: r + 3,
        child: closer,
    });
    asm.emit(Op::SetField {
        obj: r + 2,
        name: close,
        src: r + 3,
    });
    asm.emit(Op::Call {
        func: r,
        nargs: 2,
        nresults: 1,
    });
    asm.emit(Op::MarkClose { reg: r });
}

/// `pcall(f)` from register `r`, where `f` is the child `body`; two results
/// at `r`, `r + 1`.
fn emit_pcall(asm: &mut Asm, r: u8, body: ProtoSpec) {
    let body = asm.child(body);
    let pcall = asm.bytes("pcall");
    asm.emit(Op::GetGlobal { dst: r });
    asm.emit(Op::GetField {
        dst: r,
        obj: r,
        name: pcall,
    });
    asm.emit(Op::MakeClosure {
        dst: r + 1,
        child: body,
    });
    asm.emit(Op::Call {
        func: r,
        nargs: 1,
        nresults: 2,
    });
}

/// Coroutine and `<close>` behaviours the source compiler cannot express
/// yet, each checked against Lua 5.4.9 written with the coroutine library.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CloseCase {
    /// A scope's `__close` yields; the coroutine then returns "done".
    ScopeYield,
    /// `return 10, 20, 30` whose `__close` yields; the results survive.
    ReturnYield,
    /// Inside `pcall`, `error("E", 0)` whose `__close` yields.
    UnwindYield,
    /// The coroutine raises "boom" holding a `<close>` value: the value is
    /// not closed until `CloseThread`, which answers `false, "boom"`; a
    /// second `CloseThread` answers `true`.
    ErrorThenClose,
    /// As `ErrorThenClose`, but "boom" comes from `g`, which `f` tail-called
    /// (ADR 0029): the failed coroutine keeps its body's frame and `g`'s,
    /// not `f`'s. Lua 5.4.9 gives `ErrorThenClose`'s line.
    TailError,
    /// A coroutine suspended holding two values: `CloseThread` closes them,
    /// newest first, with nil.
    SuspendedClose,
    /// As `SuspendedClose`, and the newer `__close` raises "eb".
    CloseError,
    /// A `__close` that yields, run by `CloseThread`.
    CloseYield,
    /// The coroutine is suspended inside its own `pcall`; the `__close` that
    /// `CloseThread` runs raises "ea", which that `pcall` does not catch.
    InnerPcall,
}

impl CloseCase {
    pub(crate) const ALL: [CloseCase; 9] = [
        Self::ScopeYield,
        Self::ReturnYield,
        Self::UnwindYield,
        Self::ErrorThenClose,
        Self::TailError,
        Self::SuspendedClose,
        Self::CloseError,
        Self::CloseYield,
        Self::InnerPcall,
    ];
}

/// The main program for `case`. It sets `log = {}`, makes the coroutine,
/// drives it, and returns what `CloseCase` describes followed by
/// `log[1..=4]`.
pub(crate) fn close_case_program(case: CloseCase) -> ProtoSpec {
    let plain = || closer_proto("a", false, None);
    let mut body = Asm::new();
    match case {
        CloseCase::ScopeYield => {
            emit_closable(&mut body, 0, closer_proto("a", true, None));
            body.emit(Op::CloseScope { from: 0 });
            let done = body.bytes("done");
            body.emit(Op::LoadBytes {
                dst: 0,
                const_index: done,
            });
            body.emit(Op::Return { base: 0, count: 1 });
        }
        CloseCase::ReturnYield => {
            emit_closable(&mut body, 0, closer_proto("a", true, None));
            for (offset, value) in [10, 20, 30].into_iter().enumerate() {
                body.emit(Op::LoadInt {
                    dst: 1 + offset as u8,
                    value,
                });
            }
            body.emit(Op::Return { base: 1, count: 3 });
        }
        CloseCase::UnwindYield => {
            let mut inner = Asm::new();
            emit_closable(&mut inner, 0, closer_proto("a", true, None));
            emit_error(&mut inner, 1, "E");
            inner.emit(Op::Return { base: 0, count: 0 });
            emit_pcall(&mut body, 0, inner.finish());
            body.emit(Op::Return { base: 0, count: 2 });
        }
        CloseCase::ErrorThenClose => {
            emit_closable(&mut body, 0, plain());
            emit_error(&mut body, 1, "boom");
            body.emit(Op::Return { base: 0, count: 0 });
        }
        CloseCase::TailError => {
            let mut g = Asm::new();
            emit_error(&mut g, 0, "boom");
            g.emit(Op::Return { base: 0, count: 0 });
            let mut f = Asm::new();
            let g = f.child(g.finish());
            f.emit(Op::MakeClosure { dst: 0, child: g });
            f.emit(Op::TailCall { func: 0, nargs: 0 });
            f.emit(Op::Return {
                base: 0,
                count: COUNT_OPEN,
            });
            emit_closable(&mut body, 0, plain());
            let f = body.child(f.finish());
            body.emit(Op::MakeClosure { dst: 1, child: f });
            body.emit(Op::Call {
                func: 1,
                nargs: 0,
                nresults: 0,
            });
            body.emit(Op::Return { base: 0, count: 0 });
        }
        CloseCase::SuspendedClose | CloseCase::CloseError | CloseCase::CloseYield => {
            let (first, second) = match case {
                CloseCase::CloseError => (plain(), closer_proto("b", false, Some("eb"))),
                CloseCase::CloseYield => (
                    closer_proto("a", true, None),
                    closer_proto("b", false, None),
                ),
                _ => (plain(), closer_proto("b", false, None)),
            };
            emit_closable(&mut body, 0, first);
            emit_closable(&mut body, 1, second);
            body.emit(Op::Yield { base: 0, count: 0 });
            body.emit(Op::Return { base: 0, count: 0 });
        }
        CloseCase::InnerPcall => {
            let mut inner = Asm::new();
            emit_closable(&mut inner, 0, closer_proto("a", false, Some("ea")));
            inner.emit(Op::Yield { base: 0, count: 0 });
            inner.emit(Op::Return { base: 0, count: 0 });
            emit_pcall(&mut body, 0, inner.finish());
            // Never reached: the pcall does not catch the close's error.
            let after = body.bytes("after");
            body.emit(Op::LoadBytes {
                dst: 0,
                const_index: after,
            });
            body.emit(Op::Return { base: 0, count: 1 });
        }
    }

    let mut main = Asm::new();
    let child = main.child(body.finish());
    let log = main.bytes("log");
    // r0 = globals, r1 = log, r2 = the coroutine, results from r3.
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::NewTable { dst: 1 });
    main.emit(Op::SetField {
        obj: 0,
        name: log,
        src: 1,
    });
    main.emit(Op::NewThread { dst: 2, child });
    let resume = |main: &mut Asm, dest: u8, nresults: u8| {
        main.emit(Op::Resume {
            dest,
            thread: 2,
            nresults,
        });
    };
    let results: u8 = match case {
        CloseCase::ScopeYield => {
            resume(&mut main, 3, 1);
            resume(&mut main, 4, 1);
            2
        }
        CloseCase::ReturnYield => {
            resume(&mut main, 3, 1);
            resume(&mut main, 4, 3);
            4
        }
        CloseCase::UnwindYield => {
            resume(&mut main, 3, 1);
            resume(&mut main, 4, 2);
            3
        }
        CloseCase::ErrorThenClose | CloseCase::TailError => {
            // pcall(function(co) return (resume co) end, co), then #log,
            // then two closes.
            let mut resumer = Asm::new();
            resumer.params = 1;
            resumer.emit(Op::Resume {
                dest: 1,
                thread: 0,
                nresults: 1,
            });
            resumer.emit(Op::Return { base: 1, count: 1 });
            let resumer = main.child(resumer.finish());
            let pcall = main.bytes("pcall");
            main.emit(Op::GetField {
                dst: 3,
                obj: 0,
                name: pcall,
            });
            main.emit(Op::MakeClosure {
                dst: 4,
                child: resumer,
            });
            main.emit(Op::Move { dst: 5, src: 2 });
            main.emit(Op::Call {
                func: 3,
                nargs: 2,
                nresults: 2,
            });
            main.emit(Op::Len { dst: 5, src: 1 });
            main.emit(Op::CloseThread { dst: 6, thread: 2 });
            main.emit(Op::CloseThread { dst: 8, thread: 2 });
            7
        }
        CloseCase::SuspendedClose
        | CloseCase::CloseError
        | CloseCase::CloseYield
        | CloseCase::InnerPcall => {
            resume(&mut main, 3, 0);
            main.emit(Op::CloseThread { dst: 3, thread: 2 });
            2
        }
    };
    let out = 3 + results;
    for index in 0..4u8 {
        main.emit(Op::LoadInt {
            dst: out + index,
            value: i64::from(index) + 1,
        });
        main.emit(Op::Index {
            dst: out + index,
            obj: 1,
            key: out + index,
        });
    }
    main.emit(Op::Return {
        base: 3,
        count: results + 4,
    });
    main.finish()
}

/// A vararg function `f(...)` with three registers calls `g`, a function
/// with `callee_regs` registers that writes its last one, then returns its
/// first two extras. `main` returns `f(41, 42)`. A layout that keeps the
/// extras above `f`'s registers lets `g`'s frame cover them.
pub(crate) fn vararg_overlap_program(callee_regs: u8) -> ProtoSpec {
    let mut g = Asm::new();
    g.emit(Op::LoadInt {
        dst: callee_regs - 1,
        value: 99,
    });
    g.emit(Op::Return { base: 0, count: 0 });

    let mut f = Asm::new();
    f.vararg = true;
    let g = f.child(g.finish());
    f.emit(Op::MakeClosure { dst: 0, child: g });
    f.emit(Op::Call {
        func: 0,
        nargs: 0,
        nresults: 1,
    });
    f.emit(Op::Vararg { dst: 0, count: 2 });
    f.emit(Op::LoadNil { dst: 2 });
    f.emit(Op::Return { base: 0, count: 2 });

    let mut main = Asm::new();
    let f = main.child(f.finish());
    main.emit(Op::MakeClosure { dst: 0, child: f });
    main.emit(Op::LoadInt { dst: 1, value: 41 });
    main.emit(Op::LoadInt { dst: 2, value: 42 });
    main.emit(Op::Call {
        func: 0,
        nargs: 2,
        nresults: 2,
    });
    main.emit(Op::Return { base: 0, count: 2 });
    main.finish()
}

/// A coroutine calls a vararg `f(1, nil, 3)`. `f` holds a `<close>` value
/// whose `__close` is `closer_proto("a", true, None)`, yields with its
/// extras live, then returns `...`, so the close yields again with the
/// results waiting. Main resumes three times and returns the close's
/// yield, `f`'s three results, and `log[1..=2]`. Lua 5.4.9, with the
/// coroutine library, gives a 1 nil 3 a nil.
pub(crate) fn vararg_coroutine_program() -> ProtoSpec {
    let mut f = Asm::new();
    f.vararg = true;
    emit_closable(&mut f, 0, closer_proto("a", true, None));
    f.emit(Op::Yield { base: 1, count: 0 });
    f.emit(Op::Vararg {
        dst: 1,
        count: COUNT_OPEN,
    });
    f.emit(Op::Return {
        base: 1,
        count: COUNT_OPEN,
    });

    let mut body = Asm::new();
    let f = body.child(f.finish());
    body.emit(Op::MakeClosure { dst: 0, child: f });
    body.emit(Op::LoadInt { dst: 1, value: 1 });
    body.emit(Op::LoadNil { dst: 2 });
    body.emit(Op::LoadInt { dst: 3, value: 3 });
    body.emit(Op::Call {
        func: 0,
        nargs: 3,
        nresults: COUNT_OPEN,
    });
    body.emit(Op::Return {
        base: 0,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    let log = main.bytes("log");
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::NewTable { dst: 1 });
    main.emit(Op::SetField {
        obj: 0,
        name: log,
        src: 1,
    });
    main.emit(Op::NewThread { dst: 2, child });
    main.emit(Op::Resume {
        dest: 3,
        thread: 2,
        nresults: 0,
    });
    main.emit(Op::Resume {
        dest: 3,
        thread: 2,
        nresults: 1,
    });
    main.emit(Op::Resume {
        dest: 4,
        thread: 2,
        nresults: 3,
    });
    for index in 0..2u8 {
        main.emit(Op::LoadInt {
            dst: 7 + index,
            value: i64::from(index) + 1,
        });
        main.emit(Op::Index {
            dst: 7 + index,
            obj: 1,
            key: 7 + index,
        });
    }
    main.emit(Op::Return { base: 3, count: 6 });
    main.finish()
}

/// A coroutine runs `for x in iter, 3, nil, closer do sum = sum + x end`,
/// compiled as the compiler does, and returns `sum`. The iterator yields
/// its control, then answers `upto(s, c)`; the closer is
/// `closer_proto("h", true, None)`. Main resumes the coroutine six times
/// and returns the six values it got, then `log[1..=4]`. Lua 5.4.9, with
/// the coroutine library, gives nil 1 2 3 h 6 h nil nil nil.
pub(crate) fn generic_for_yield_program() -> ProtoSpec {
    let mut iter = Asm::new();
    iter.params = 2;
    let upto = iter.bytes("upto");
    iter.emit(Op::Move { dst: 2, src: 1 });
    iter.emit(Op::Yield { base: 2, count: 1 });
    iter.emit(Op::GetGlobal { dst: 2 });
    iter.emit(Op::GetField {
        dst: 2,
        obj: 2,
        name: upto,
    });
    iter.emit(Op::Move { dst: 3, src: 0 });
    iter.emit(Op::Move { dst: 4, src: 1 });
    iter.emit(Op::Call {
        func: 2,
        nargs: 2,
        nresults: COUNT_OPEN,
    });
    iter.emit(Op::Return {
        base: 2,
        count: COUNT_OPEN,
    });

    // r0 = sum; the hidden values at r1..=r4; x at r5.
    let mut body = Asm::new();
    let iter = body.child(iter.finish());
    body.emit(Op::LoadInt { dst: 0, value: 0 });
    body.emit(Op::MakeClosure {
        dst: 1,
        child: iter,
    });
    body.emit(Op::LoadInt { dst: 2, value: 3 });
    body.emit(Op::LoadNil { dst: 3 });
    emit_closable(&mut body, 4, closer_proto("h", true, None));
    body.emit(Op::Jump { offset: 1 });
    body.emit(Op::Add { dst: 0, a: 0, b: 5 });
    for offset in 0..3 {
        body.emit(Op::Move {
            dst: 5 + offset,
            src: 1 + offset,
        });
    }
    body.emit(Op::Call {
        func: 5,
        nargs: 2,
        nresults: 1,
    });
    body.emit(Op::GenericForLoop {
        base: 1,
        offset: -6,
    });
    body.emit(Op::CloseScope { from: 1 });
    body.emit(Op::Return { base: 0, count: 1 });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    let log = main.bytes("log");
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::NewTable { dst: 1 });
    main.emit(Op::SetField {
        obj: 0,
        name: log,
        src: 1,
    });
    main.emit(Op::NewThread { dst: 2, child });
    for dest in 3..9 {
        main.emit(Op::Resume {
            dest,
            thread: 2,
            nresults: 1,
        });
    }
    for index in 0..4u8 {
        main.emit(Op::LoadInt {
            dst: 9 + index,
            value: i64::from(index) + 1,
        });
        main.emit(Op::Index {
            dst: 9 + index,
            obj: 1,
            key: 9 + index,
        });
    }
    main.emit(Op::Return { base: 3, count: 10 });
    main.finish()
}

/// `r = setmetatable({}, { __close = <global name> })`, then `MarkClose`,
/// using registers `r..r + 4`.
#[cfg(test)]
fn emit_closable_global(asm: &mut Asm, r: u8, name: &str) {
    let setmetatable = asm.bytes("setmetatable");
    let close = asm.bytes("__close");
    let global = asm.bytes(name);
    asm.emit(Op::GetGlobal { dst: r + 1 });
    asm.emit(Op::GetField {
        dst: r,
        obj: r + 1,
        name: setmetatable,
    });
    asm.emit(Op::GetField {
        dst: r + 3,
        obj: r + 1,
        name: global,
    });
    asm.emit(Op::NewTable { dst: r + 1 });
    asm.emit(Op::NewTable { dst: r + 2 });
    asm.emit(Op::SetField {
        obj: r + 2,
        name: close,
        src: r + 3,
    });
    asm.emit(Op::Call {
        func: r,
        nargs: 2,
        nresults: 1,
    });
    asm.emit(Op::MarkClose { reg: r });
}

/// A coroutine suspended inside its own `pcall` holding a value whose
/// `__close` is the native `park`; main closes it with `CloseThread` and
/// returns the two results. The close waits on the host there.
#[cfg(test)]
pub(crate) fn thread_close_wait_program() -> ProtoSpec {
    let mut inner = Asm::new();
    emit_closable_global(&mut inner, 0, "park");
    inner.emit(Op::Yield { base: 0, count: 0 });
    inner.emit(Op::Return { base: 0, count: 0 });
    let mut body = Asm::new();
    emit_pcall(&mut body, 0, inner.finish());
    let caught = body.bytes("caught");
    body.emit(Op::LoadBytes {
        dst: 2,
        const_index: caught,
    });
    body.emit(Op::Return { base: 2, count: 1 });
    let mut main = Asm::new();
    let child = main.child(body.finish());
    main.emit(Op::NewThread { dst: 0, child });
    main.emit(Op::Resume {
        dest: 1,
        thread: 0,
        nresults: 0,
    });
    main.emit(Op::CloseThread { dst: 1, thread: 0 });
    main.emit(Op::Return { base: 1, count: 2 });
    main.finish()
}

/// Two coroutines. With `normal`, A resumes B and B tries to close A, which
/// is normal, not suspended. Otherwise A resumes B, B raises "boom" holding
/// a `<close>` value, and A fails with it; main catches A's failure with
/// `pcall`, then closes B. Main returns `pcall`'s two results, then, when
/// not `normal`, `CloseThread`'s two and `log[1..=2]`.
#[cfg(test)]
pub(crate) fn nested_coroutine_program(normal: bool) -> ProtoSpec {
    let name_a = "A";
    let name_b = "B";
    let mut b = Asm::new();
    if normal {
        let a = b.bytes(name_a);
        b.emit(Op::GetGlobal { dst: 0 });
        b.emit(Op::GetField {
            dst: 0,
            obj: 0,
            name: a,
        });
        b.emit(Op::CloseThread { dst: 1, thread: 0 });
    } else {
        emit_closable(&mut b, 0, closer_proto("b", false, None));
        emit_error(&mut b, 1, "boom");
    }
    b.emit(Op::Return { base: 0, count: 0 });
    let mut a = Asm::new();
    let global_b = a.bytes(name_b);
    a.emit(Op::GetGlobal { dst: 0 });
    a.emit(Op::GetField {
        dst: 0,
        obj: 0,
        name: global_b,
    });
    a.emit(Op::Resume {
        dest: 1,
        thread: 0,
        nresults: 0,
    });
    a.emit(Op::Return { base: 0, count: 0 });
    let mut resumer = Asm::new();
    resumer.params = 1;
    resumer.emit(Op::Resume {
        dest: 1,
        thread: 0,
        nresults: 0,
    });
    resumer.emit(Op::Return { base: 0, count: 0 });

    let mut main = Asm::new();
    let child_b = main.child(b.finish());
    let child_a = main.child(a.finish());
    let resumer = main.child(resumer.finish());
    let log = main.bytes("log");
    let global_a = main.bytes(name_a);
    let global_b = main.bytes(name_b);
    let pcall = main.bytes("pcall");
    // r0 globals, r1 log, r2 B, r3 A, results from r4.
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::NewTable { dst: 1 });
    main.emit(Op::SetField {
        obj: 0,
        name: log,
        src: 1,
    });
    main.emit(Op::NewThread {
        dst: 2,
        child: child_b,
    });
    main.emit(Op::SetField {
        obj: 0,
        name: global_b,
        src: 2,
    });
    main.emit(Op::NewThread {
        dst: 3,
        child: child_a,
    });
    main.emit(Op::SetField {
        obj: 0,
        name: global_a,
        src: 3,
    });
    main.emit(Op::GetField {
        dst: 4,
        obj: 0,
        name: pcall,
    });
    main.emit(Op::MakeClosure {
        dst: 5,
        child: resumer,
    });
    main.emit(Op::Move { dst: 6, src: 3 });
    main.emit(Op::Call {
        func: 4,
        nargs: 2,
        nresults: 2,
    });
    if normal {
        main.emit(Op::Return { base: 4, count: 2 });
        return main.finish();
    }
    main.emit(Op::CloseThread { dst: 6, thread: 2 });
    for index in 0..2u8 {
        main.emit(Op::LoadInt {
            dst: 8 + index,
            value: i64::from(index) + 1,
        });
        main.emit(Op::Index {
            dst: 8 + index,
            obj: 1,
            key: 8 + index,
        });
    }
    main.emit(Op::Return { base: 4, count: 6 });
    main.finish()
}

/// [`source_coroutine_program`]'s coroutine, running `co_source`, kept in
/// the global `co` and resumed `resumes` times, each inside `pcall`; then
/// main runs `load(main_source)()` and returns its results (ADR 0040).
#[cfg(test)]
pub(crate) fn coroutine_then_program(co_source: &str, resumes: u8, main_source: &str) -> ProtoSpec {
    let mut yielder = Asm::new();
    yielder.params = 1;
    yielder.emit(Op::Yield { base: 0, count: 1 });
    yielder.emit(Op::Return { base: 0, count: 1 });

    let mut body = Asm::new();
    let load = body.bytes("load");
    let text = body.bytes(co_source);
    body.emit(Op::GetGlobal { dst: 0 });
    body.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: load,
    });
    body.emit(Op::LoadBytes {
        dst: 2,
        const_index: text,
    });
    body.emit(Op::Call {
        func: 1,
        nargs: 1,
        nresults: 1,
    });
    body.emit(Op::Call {
        func: 1,
        nargs: 0,
        nresults: COUNT_OPEN,
    });
    body.emit(Op::Return {
        base: 1,
        count: COUNT_OPEN,
    });

    // `pcall`s this, so a resume that fails does not end main.
    let mut resumer = Asm::new();
    let co_name = resumer.bytes("co");
    resumer.emit(Op::GetGlobal { dst: 0 });
    resumer.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: co_name,
    });
    resumer.emit(Op::Resume {
        dest: 2,
        thread: 1,
        nresults: 2,
    });
    resumer.emit(Op::Return { base: 2, count: 2 });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    let y = main.child(yielder.finish());
    let r = main.child(resumer.finish());
    let yield_name = main.bytes("yield");
    let co_name = main.bytes("co");
    let pcall = main.bytes("pcall");
    let load = main.bytes("load");
    let text = main.bytes(main_source);
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::MakeClosure { dst: 1, child: y });
    main.emit(Op::SetField {
        obj: 0,
        name: yield_name,
        src: 1,
    });
    main.emit(Op::NewThread { dst: 1, child });
    main.emit(Op::SetField {
        obj: 0,
        name: co_name,
        src: 1,
    });
    main.emit(Op::MakeClosure { dst: 2, child: r });
    for _ in 0..resumes {
        main.emit(Op::GetField {
            dst: 3,
            obj: 0,
            name: pcall,
        });
        main.emit(Op::Move { dst: 4, src: 2 });
        main.emit(Op::Call {
            func: 3,
            nargs: 1,
            nresults: 0,
        });
    }
    let func = 3;
    main.emit(Op::GetField {
        dst: func,
        obj: 0,
        name: load,
    });
    main.emit(Op::LoadBytes {
        dst: func + 1,
        const_index: text,
    });
    main.emit(Op::Call {
        func,
        nargs: 1,
        nresults: 1,
    });
    main.emit(Op::Call {
        func,
        nargs: 0,
        nresults: COUNT_OPEN,
    });
    main.emit(Op::Return {
        base: func,
        count: COUNT_OPEN,
    });
    main.finish()
}

/// A coroutine whose body runs `load(source)()`, with a global `yield`
/// that yields its one argument, so source code can yield (ADR 0031).
/// Main resumes it `resumes` times, two results each, and returns them all.
#[cfg(test)]
pub(crate) fn source_coroutine_program(source: &str, resumes: u8) -> ProtoSpec {
    let mut yielder = Asm::new();
    yielder.params = 1;
    yielder.emit(Op::Yield { base: 0, count: 1 });
    yielder.emit(Op::Return { base: 0, count: 1 });

    let mut body = Asm::new();
    let load = body.bytes("load");
    let text = body.bytes(source);
    body.emit(Op::GetGlobal { dst: 0 });
    body.emit(Op::GetField {
        dst: 1,
        obj: 0,
        name: load,
    });
    body.emit(Op::LoadBytes {
        dst: 2,
        const_index: text,
    });
    body.emit(Op::Call {
        func: 1,
        nargs: 1,
        nresults: 1,
    });
    body.emit(Op::Call {
        func: 1,
        nargs: 0,
        nresults: COUNT_OPEN,
    });
    body.emit(Op::Return {
        base: 1,
        count: COUNT_OPEN,
    });

    let mut main = Asm::new();
    let child = main.child(body.finish());
    let y = main.child(yielder.finish());
    let name = main.bytes("yield");
    main.emit(Op::GetGlobal { dst: 0 });
    main.emit(Op::MakeClosure { dst: 1, child: y });
    main.emit(Op::SetField {
        obj: 0,
        name,
        src: 1,
    });
    main.emit(Op::NewThread { dst: 1, child });
    for resume in 0..resumes {
        main.emit(Op::Resume {
            dest: 2 + 2 * resume,
            thread: 1,
            nresults: 2,
        });
    }
    main.emit(Op::Return {
        base: 2,
        count: 2 * resumes,
    });
    main.finish()
}
