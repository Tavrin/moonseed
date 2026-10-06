//! Structural ceilings and default compiler bounds.
//!
//! These stop hostile inputs before a Rust allocation failure or a stack
//! overflow. They are not Moonseed's sandbox profile.

pub(crate) const DEFAULT_SOURCE_BYTES: usize = 16 << 20;
pub(crate) const MAX_SOURCE_BYTES: usize = 1 << 30;
/// Lua's memory error message, used when that budget is spent.
pub(crate) const COMPILE_MEMORY: &str = "not enough memory";
// The direct compiler admits one more C level than protected `load`, matching
// PUC's `luac` versus `load` entry points.
pub(crate) const MAX_PARSE_DEPTH: u32 = 198;
// Genuinely nested expression shapes still use recursive lowering. Long
// left-associative and suffix spines use the flat AST/lowering path instead.
pub(crate) const MAX_EXPR_TREE_DEPTH: u32 = 300;
// Leave function nesting to the parser's PUC C-level bound (97 bodies through
// protected load); the validator and binary chunk reader share this ceiling.
pub(crate) const MAX_FUNC_NEST: u32 = 99;
pub(crate) const MAX_LOCALS: usize = 200;
pub(crate) const MAX_UPVALUES: usize = 255;
/// Exclusive register count. The highest index is one below this.
pub(crate) const MAX_REGISTERS: u8 = 250;
pub(crate) const MAX_PROTOS: usize = 1 << 20;
pub(crate) const MAX_CONSTS: usize = 1 << 24;
pub(crate) const MAX_INSTRUCTIONS: usize = 1 << 24;

#[cfg(test)]
thread_local! {
    pub(crate) static STACK_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

#[cfg(test)]
pub(crate) fn note_stack() {
    let marker = 0u8;
    let address = (&marker as *const u8) as usize;
    STACK_FLOOR.with(|floor| floor.set(floor.get().min(address)));
}
