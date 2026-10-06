//! Private frontend tree. Not a stable syntax API.

use crate::span::Span;

#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    pub(crate) span: Span,
    pub(crate) body: Block,
}

#[derive(Clone, Debug)]
pub(crate) struct Block {
    pub(crate) span: Span,
    pub(crate) stmts: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub(crate) struct Name {
    pub(crate) span: Span,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    Empty {
        span: Span,
    },
    /// `local a <const>, b <close>, c = ...`. `close` is the index of the
    /// one name with the `<close>` attribute, if any; `consts` those with
    /// `<const>`, any number of them.
    Local {
        span: Span,
        names: Vec<Name>,
        values: Vec<Expr>,
        close: Option<usize>,
        consts: Vec<usize>,
    },
    Assign {
        span: Span,
        targets: Vec<Target>,
        values: Vec<Expr>,
    },
    Return {
        span: Span,
        values: Vec<Expr>,
    },
    Call {
        span: Span,
        call: Expr,
    },
    /// `if c1 then ... elseif c2 then ... [else ...] end`. `arms` holds each
    /// condition with its block, in source order. Each block is its own scope.
    If {
        span: Span,
        arms: Vec<(Expr, Block)>,
        else_block: Option<Block>,
    },
    Do {
        span: Span,
        body: Block,
    },
    While {
        span: Span,
        cond: Expr,
        body: Block,
    },
    /// `repeat body until cond`. `cond` is inside the body's scope.
    Repeat {
        span: Span,
        body: Block,
        cond: Expr,
    },
    Break {
        span: Span,
    },
    /// `::name::`. `last` when only labels and `;` follow it in its block
    /// and the block does not end in `until`: Lua treats the block's
    /// locals as out of scope there, so a `goto` may jump to it past them.
    Label {
        span: Span,
        /// Lookahead after the closing `::`, where PUC checks duplicates.
        diagnostic_span: Span,
        name: Name,
        last: bool,
    },
    Goto {
        span: Span,
        name: Name,
    },
    /// `for name = init, limit [, step] do body end`.
    NumericFor {
        span: Span,
        name: Name,
        init: Expr,
        limit: Expr,
        step: Option<Expr>,
        body: Block,
    },
    /// `local function name body`: `name` is in scope in its own body,
    /// unlike `local name = function body`. `func` is an `Expr::Function`.
    LocalFunction {
        span: Span,
        name: Name,
        func: Expr,
    },
    /// `for n1, n2, ... in explist do body end`. `names` holds at least one.
    GenericFor {
        span: Span,
        names: Vec<Name>,
        values: Vec<Expr>,
        body: Block,
    },
}

/// An assignment destination. `a.name` is an `Index` with a string key.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Name(Name),
    Index { span: Span, base: Expr, key: Expr },
}

impl Target {
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Name(name) => name.span,
            Self::Index { span, .. } => *span,
        }
    }
}

/// One constructor field. `name = v` is `Keyed` with a string key.
#[derive(Clone, Debug)]
pub(crate) enum TableField {
    List(Expr),
    Keyed { key: Expr, value: Expr },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Idiv,
    Mod,
    Pow,
    Concat,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Band,
    Bor,
    Bxor,
    Shl,
    Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg,
    Not,
    Len,
    Bnot,
}

#[derive(Clone, Debug)]
pub(crate) enum ChainStep {
    Binary {
        span: Span,
        op: BinOp,
        right: Expr,
    },
    Index {
        span: Span,
        key: Expr,
    },
    Call {
        span: Span,
        method: Option<Name>,
        args: Vec<Expr>,
    },
}

impl ChainStep {
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Binary { span, .. } | Self::Index { span, .. } | Self::Call { span, .. } => *span,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    /// A long left spine. Each step consumes the preceding value in order.
    Chain {
        span: Span,
        first: Box<Expr>,
        rest: Vec<ChainStep>,
    },
    /// Compiler-only value for lowering a chain step without revisiting its prefix.
    Resolved {
        span: Span,
        reg: u8,
    },
    Nil {
        span: Span,
    },
    Bool {
        span: Span,
        value: bool,
    },
    Integer {
        span: Span,
        value: i64,
    },
    Float {
        span: Span,
        bits: u64,
    },
    Str {
        span: Span,
        bytes: Vec<u8>,
    },
    Name {
        span: Span,
        bytes: Vec<u8>,
    },
    Paren {
        span: Span,
        inner: Box<Expr>,
    },
    /// `function (params [, ...]) body end`. `vararg` is the trailing
    /// `...`.
    Function {
        span: Span,
        params: Vec<Name>,
        vararg: bool,
        body: Block,
    },
    /// `...`, the extra arguments of the enclosing vararg function.
    Vararg {
        span: Span,
    },
    /// `func(args)`, or `func:method(args)` when `method` is set: `func`
    /// is then the receiver, evaluated once, and passed as the first
    /// argument to its field `method`.
    Call {
        span: Span,
        func: Box<Expr>,
        method: Option<Name>,
        args: Vec<Expr>,
    },
    Binary {
        span: Span,
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        span: Span,
        op: UnOp,
        expr: Box<Expr>,
    },
    /// `base[key]`; `base.name` is parsed to this with a string key.
    Index {
        span: Span,
        base: Box<Expr>,
        key: Box<Expr>,
    },
    Table {
        span: Span,
        fields: Vec<TableField>,
    },
}

impl Expr {
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Chain { span, .. }
            | Self::Resolved { span, .. }
            | Self::Nil { span }
            | Self::Bool { span, .. }
            | Self::Integer { span, .. }
            | Self::Float { span, .. }
            | Self::Str { span, .. }
            | Self::Name { span, .. }
            | Self::Paren { span, .. }
            | Self::Function { span, .. }
            | Self::Vararg { span }
            | Self::Call { span, .. }
            | Self::Binary { span, .. }
            | Self::Unary { span, .. }
            | Self::Index { span, .. }
            | Self::Table { span, .. } => *span,
        }
    }
}
