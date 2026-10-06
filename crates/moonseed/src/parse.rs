//! Recursive-descent parser for the supported statement subset.
//!
//! Expressions use precedence climbing so the table can grow to the rest of
//! Lua without a new parser. Operators other than `+` parse here and fail
//! in the compiler.

use crate::ast::{BinOp, Block, ChainStep, Chunk, Expr, Name, Stmt, TableField, Target, UnOp};
use crate::error::{CompileError, CompileErrorKind};
use crate::lex::{Lexer, Token, TokenKind};
use crate::limits::{MAX_EXPR_TREE_DEPTH, MAX_PARSE_DEPTH};
use crate::span::Span;

#[cfg(test)]
pub(crate) fn parse(source: &[u8]) -> Result<Chunk, CompileError> {
    parse_with_limit(source, crate::limits::DEFAULT_SOURCE_BYTES)
}

#[cfg(test)]
pub(crate) fn parse_with_limit(
    source: &[u8],
    max_source_bytes: usize,
) -> Result<Chunk, CompileError> {
    parse_with_diagnostics(source, max_source_bytes).map(|(chunk, _)| chunk)
}

/// A token still current immediately after parsing the source token at the
/// given offset. Compiler errors use these parser observations on cold paths.
pub(crate) type DiagnosticNear = Vec<(u32, Vec<u8>)>;

#[cfg(test)]
pub(crate) fn parse_with_diagnostics(
    source: &[u8],
    max_source_bytes: usize,
) -> Result<(Chunk, DiagnosticNear), CompileError> {
    parse_with_diagnostics_at_depth(source, max_source_bytes, 0, u64::MAX)
}

/// `max_ast_bytes` bounds retained AST allocations with a safety margin for
/// transient compiler memory. Host compilation passes `u64::MAX`.
pub(crate) fn parse_with_diagnostics_at_depth(
    source: &[u8],
    max_source_bytes: usize,
    c_frames: u32,
    max_ast_bytes: u64,
) -> Result<(Chunk, DiagnosticNear), CompileError> {
    let mut parser = Parser::new(source, max_source_bytes, c_frames, max_ast_bytes)?;
    let chunk = parser.parse_chunk()?;
    if !matches!(parser.look.kind, TokenKind::Eof) {
        return Err(parser.syntax("<eof> expected"));
    }
    Ok((chunk, parser.diagnostic_near))
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    look: Token,
    depth: u32,
    tree_depth: u32,
    diagnostic_near: DiagnosticNear,
    ast_bytes_left: u64,
}

impl<'a> Parser<'a> {
    fn new(
        source: &'a [u8],
        max_source_bytes: usize,
        c_frames: u32,
        max_ast_bytes: u64,
    ) -> Result<Self, CompileError> {
        let mut lexer = Lexer::with_limit(source, max_source_bytes)?;
        let look = lexer.next_token()?;
        Ok(Self {
            lexer,
            look,
            depth: c_frames.min(MAX_PARSE_DEPTH),
            tree_depth: 0,
            diagnostic_near: Vec::new(),
            ast_bytes_left: max_ast_bytes,
        })
    }

    fn parse_chunk(&mut self) -> Result<Chunk, CompileError> {
        let start = self.look.span.start;
        let body = self.parse_block(false)?;
        let end = self.look.span.start.max(start);
        Ok(Chunk {
            span: Span::new(start, end),
            body,
        })
    }

    fn parse_block(&mut self, stop_at_end: bool) -> Result<Block, CompileError> {
        let start = self.look.span.start;
        let mut stmts = Vec::new();
        let mut end = start;
        loop {
            if matches!(self.look.kind, TokenKind::Eof) {
                break;
            }
            if stop_at_end
                && matches!(
                    self.look.kind,
                    TokenKind::End | TokenKind::Else | TokenKind::Elseif | TokenKind::Until
                )
            {
                break;
            }
            if matches!(self.look.kind, TokenKind::Return) {
                let stmt = self.parse_stmt()?;
                end = stmt_span(&stmt).end;
                self.reserve(&mut stmts)?;
                stmts.push(stmt);
                break;
            }
            let stmt = self.parse_stmt()?;
            end = stmt_span(&stmt).end;
            self.reserve(&mut stmts)?;
            stmts.push(stmt);
        }
        // PUC delays creating a label until it has parsed the following
        // run of labels and semicolons. The innermost label is created first,
        // and every duplicate in that run is diagnosed at its lookahead.
        let mut index = 0;
        while index < stmts.len() {
            if !matches!(stmts[index], Stmt::Label { .. } | Stmt::Empty { .. }) {
                index += 1;
                continue;
            }
            let first = index;
            while index < stmts.len()
                && matches!(stmts[index], Stmt::Label { .. } | Stmt::Empty { .. })
            {
                index += 1;
            }
            let next = stmts.get(index).map(stmt_span).unwrap_or(self.look.span);
            for stmt in &mut stmts[first..index] {
                if let Stmt::Label {
                    diagnostic_span, ..
                } = stmt
                {
                    *diagnostic_span = next;
                }
            }
            stmts[first..index].reverse();
        }
        // Labels followed only by labels and `;` end the block, unless
        // `until` does: its condition still sees the block's locals.
        if !matches!(self.look.kind, TokenKind::Until) {
            for stmt in stmts.iter_mut().rev() {
                match stmt {
                    Stmt::Label { last, .. } => *last = true,
                    Stmt::Empty { .. } => {}
                    _ => break,
                }
            }
        }
        Ok(Block {
            span: Span::new(start, end),
            stmts,
        })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, CompileError> {
        self.enter()?;
        let result = self.parse_stmt_inner();
        self.depth -= 1;
        result
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt, CompileError> {
        match &self.look.kind {
            TokenKind::Semi => {
                let token = self.bump()?;
                Ok(Stmt::Empty { span: token.span })
            }
            TokenKind::Local => self.parse_local(),
            TokenKind::Function => self.parse_function_stmt(),
            TokenKind::Name | TokenKind::LParen => {
                let expr = self.parse_expr()?;
                self.prefix_stmt(expr)
            }
            TokenKind::If => self.parse_if(),
            TokenKind::Do => {
                let keyword = self.bump()?.span;
                let body = self.parse_block(true)?;
                let end =
                    self.expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")?;
                Ok(Stmt::Do {
                    span: keyword.cover(end.span),
                    body,
                })
            }
            TokenKind::While => {
                let keyword = self.bump()?.span;
                let cond = self.parse_expr()?;
                self.expect_kind(|kind| matches!(kind, TokenKind::Do), "'do' expected")?;
                let body = self.parse_block(true)?;
                let end =
                    self.expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")?;
                Ok(Stmt::While {
                    span: keyword.cover(end.span),
                    cond,
                    body,
                })
            }
            TokenKind::Repeat => {
                let keyword = self.bump()?.span;
                let body = self.parse_block(true)?;
                self.expect_kind(|kind| matches!(kind, TokenKind::Until), "'until' expected")?;
                let cond = self.parse_expr()?;
                Ok(Stmt::Repeat {
                    span: keyword.cover(cond.span()),
                    body,
                    cond,
                })
            }
            TokenKind::Break => {
                let span = self.bump()?.span;
                Ok(Stmt::Break { span })
            }
            TokenKind::Return => self.parse_return(),
            TokenKind::Until => Err(self.syntax("unexpected symbol")),
            TokenKind::For => self.parse_for(),
            TokenKind::ColonColon => {
                let open = self.bump()?.span;
                let name = self.expect_name()?;
                let close = self.expect_kind(
                    |kind| matches!(kind, TokenKind::ColonColon),
                    "'::' expected",
                )?;
                Ok(Stmt::Label {
                    span: open.cover(close.span),
                    diagnostic_span: self.look.span,
                    name,
                    last: false,
                })
            }
            TokenKind::Goto => {
                let keyword = self.bump()?.span;
                let name = self.expect_name()?;
                Ok(Stmt::Goto {
                    span: keyword.cover(name.span),
                    name,
                })
            }
            TokenKind::In | TokenKind::Then => Err(self.syntax("unexpected symbol")),
            TokenKind::End => Err(self.syntax("unexpected symbol")),
            TokenKind::Else | TokenKind::Elseif => Err(self.syntax("unexpected symbol")),
            _ => Err(self.syntax("unexpected symbol")),
        }
    }

    fn parse_for(&mut self) -> Result<Stmt, CompileError> {
        let keyword = self.bump()?.span;
        let name = self.expect_name()?;
        if matches!(self.look.kind, TokenKind::Comma | TokenKind::In) {
            return self.parse_generic_for(keyword, name);
        }
        self.expect_kind(|kind| matches!(kind, TokenKind::Eq), "'=' or 'in' expected")?;
        let init = self.parse_expr()?;
        self.expect_kind(|kind| matches!(kind, TokenKind::Comma), "',' expected")?;
        let limit = self.parse_expr()?;
        let step = if matches!(self.look.kind, TokenKind::Comma) {
            self.bump()?;
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_kind(|kind| matches!(kind, TokenKind::Do), "'do' expected")?;
        let body = self.parse_block(true)?;
        let end = self.expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")?;
        Ok(Stmt::NumericFor {
            span: keyword.cover(end.span),
            name,
            init,
            limit,
            step,
            body,
        })
    }

    /// `for first, ... in explist do body end`, after `first`.
    fn parse_generic_for(&mut self, keyword: Span, first: Name) -> Result<Stmt, CompileError> {
        self.charge(std::mem::size_of::<Name>())?;
        let mut names = vec![first];
        while matches!(self.look.kind, TokenKind::Comma) {
            self.bump()?;
            self.reserve(&mut names)?;
            names.push(self.expect_name()?);
        }
        self.expect_kind(|kind| matches!(kind, TokenKind::In), "'in' expected")?;
        let values = self.expr_list()?;
        self.expect_kind(|kind| matches!(kind, TokenKind::Do), "'do' expected")?;
        let body = self.parse_block(true)?;
        let end = self.expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")?;
        Ok(Stmt::GenericFor {
            span: keyword.cover(end.span),
            names,
            values,
            body,
        })
    }

    fn parse_if(&mut self) -> Result<Stmt, CompileError> {
        let keyword = self.bump()?.span;
        let mut arms = Vec::new();
        loop {
            let cond = self.parse_expr()?;
            let then =
                self.expect_kind(|kind| matches!(kind, TokenKind::Then), "'then' expected")?;
            let mut block = self.parse_block(true)?;
            // Retain the branch token without enlarging the private AST.
            block.span.start = then.span.start;
            self.reserve(&mut arms)?;
            arms.push((cond, block));
            if !matches!(self.look.kind, TokenKind::Elseif) {
                break;
            }
            self.bump()?;
        }
        let else_block = if matches!(self.look.kind, TokenKind::Else) {
            self.bump()?;
            Some(self.parse_block(true)?)
        } else {
            None
        };
        let end = self.expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")?;
        Ok(Stmt::If {
            span: keyword.cover(end.span),
            arms,
            else_block,
        })
    }

    fn parse_local(&mut self) -> Result<Stmt, CompileError> {
        let start = self.bump()?.span;
        if matches!(self.look.kind, TokenKind::Function) {
            let keyword = self.bump()?.span;
            let name = self.expect_name()?;
            let func = self.parse_function_body(keyword, false)?;
            return Ok(Stmt::LocalFunction {
                span: start.cover(func.span()),
                name,
                func,
            });
        }
        let (names, close, consts) = self.local_names()?;
        let mut values = Vec::new();
        if matches!(self.look.kind, TokenKind::Eq) {
            self.bump()?;
            values = self.expr_list()?;
        }
        let end = values
            .last()
            .map(Expr::span)
            .or_else(|| names.last().map(|name| name.span))
            .unwrap_or(start);
        Ok(Stmt::Local {
            span: start.cover(end),
            names,
            values,
            close,
            consts,
        })
    }

    /// The names of a `local` list, the index of its `<close>` one, and the
    /// indexes of its `<const>` ones. A name takes one attribute; a list
    /// holds at most one `<close>`.
    #[allow(clippy::type_complexity)]
    fn local_names(&mut self) -> Result<(Vec<Name>, Option<usize>, Vec<usize>), CompileError> {
        let mut names = Vec::new();
        let mut close = None;
        let mut consts = Vec::new();
        loop {
            self.reserve(&mut names)?;
            names.push(self.expect_name()?);
            if matches!(self.look.kind, TokenKind::Lt) {
                let open = self.bump()?;
                let attrib = self.expect_name()?;
                self.expect_kind(|kind| matches!(kind, TokenKind::Gt), "'>' expected")?;
                match attrib.bytes.as_slice() {
                    b"close" if close.is_some() => {
                        return Err(CompileError::new(
                            CompileErrorKind::Syntax,
                            open.span.cover(attrib.span),
                            "multiple to-be-closed variables in local list",
                        ));
                    }
                    b"close" => close = Some(names.len() - 1),
                    b"const" => {
                        self.reserve(&mut consts)?;
                        consts.push(names.len() - 1);
                    }
                    _ => {
                        return Err(CompileError::new(
                            CompileErrorKind::Syntax,
                            attrib.span,
                            format!(
                                "unknown attribute '{}'",
                                String::from_utf8_lossy(&attrib.bytes)
                            ),
                        ));
                    }
                }
            }
            if !matches!(self.look.kind, TokenKind::Comma) {
                return Ok((names, close, consts));
            }
            self.bump()?;
        }
    }

    /// `function a.b.c:m body`, the assignment `a.b.c.m = function body`,
    /// with `self` first among the parameters for `:m`. The target is an
    /// ordinary assignment target: a local or global name, or a field
    /// stored through `__newindex`, with the prefix indexed as usual.
    fn parse_function_stmt(&mut self) -> Result<Stmt, CompileError> {
        let keyword = self.bump()?.span;
        let first = self.expect_name()?;
        let mut target = Target::Name(first);
        let mut method = false;
        let mut components = 0u32;
        while matches!(self.look.kind, TokenKind::Dot | TokenKind::Colon) {
            method = matches!(self.look.kind, TokenKind::Colon);
            self.bump()?;
            let name = self.expect_name()?;
            let base = match target {
                Target::Name(name) => Expr::Name {
                    span: name.span,
                    bytes: name.bytes,
                },
                Target::Index { span, base, key } => {
                    self.charge_boxes(2)?;
                    Expr::Index {
                        span,
                        base: Box::new(base),
                        key: Box::new(key),
                    }
                }
            };
            components += 1;
            let base = if components > MAX_EXPR_TREE_DEPTH {
                match base {
                    Expr::Index { span, base, key } => {
                        self.append_chain(*base, ChainStep::Index { span, key: *key })?
                    }
                    other => other,
                }
            } else {
                base
            };
            target = Target::Index {
                span: base.span().cover(name.span),
                base,
                key: Expr::Str {
                    span: name.span,
                    bytes: name.bytes,
                },
            };
            if method {
                break;
            }
        }
        let func = self.parse_function_body(keyword, method)?;
        self.charge(std::mem::size_of::<Target>() + std::mem::size_of::<Expr>())?;
        Ok(Stmt::Assign {
            span: keyword.cover(func.span()),
            targets: vec![target],
            values: vec![func],
        })
    }

    fn prefix_stmt(&mut self, expr: Expr) -> Result<Stmt, CompileError> {
        if matches!(self.look.kind, TokenKind::Eq | TokenKind::Comma) {
            self.charge(std::mem::size_of::<Target>())?;
            let mut targets = vec![target(expr)?];
            while matches!(self.look.kind, TokenKind::Comma) {
                self.bump()?;
                let next = self.parse_primary()?;
                self.reserve(&mut targets)?;
                targets.push(target(next)?);
            }
            let eq = self.bump()?;
            if !matches!(eq.kind, TokenKind::Eq) {
                return Err(CompileError::new(
                    CompileErrorKind::Syntax,
                    eq.span,
                    "'=' expected",
                ));
            }
            let values = self.expr_list()?;
            let span = targets[0].span().cover(
                values
                    .last()
                    .map(Expr::span)
                    .unwrap_or(targets.last().unwrap().span()),
            );
            return Ok(Stmt::Assign {
                span,
                targets,
                values,
            });
        }
        if matches!(expr, Expr::Call { .. })
            || matches!(&expr, Expr::Chain { rest, .. } if matches!(rest.last(), Some(ChainStep::Call { .. })))
        {
            return Ok(Stmt::Call {
                span: expr.span(),
                call: expr,
            });
        }
        // A prefix that is neither an assignment nor a call fails at the
        // current lookahead, as PUC's expression statement check does.
        Err(self.syntax("syntax error"))
    }

    fn parse_return(&mut self) -> Result<Stmt, CompileError> {
        let start = self.bump()?.span;
        let values = if self.starts_expr() {
            self.expr_list()?
        } else {
            Vec::new()
        };
        let mut end = values.last().map(Expr::span).unwrap_or(start);
        if matches!(self.look.kind, TokenKind::Semi) {
            end = self.bump()?.span;
        }
        Ok(Stmt::Return {
            span: start.cover(end),
            values,
        })
    }

    fn expr_list(&mut self) -> Result<Vec<Expr>, CompileError> {
        self.charge(std::mem::size_of::<Expr>())?;
        let mut values = vec![self.parse_expr()?];
        while matches!(self.look.kind, TokenKind::Comma) {
            self.bump()?;
            self.reserve(&mut values)?;
            values.push(self.parse_expr()?);
        }
        Ok(values)
    }

    fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        let saved = self.tree_depth;
        let result = self.parse_binop(0);
        self.tree_depth = saved;
        result
    }

    fn parse_binop(&mut self, min_prec: u8) -> Result<Expr, CompileError> {
        self.enter()?;
        let result = self.parse_binop_inner(min_prec);
        self.depth -= 1;
        if let Ok(expr) = &result {
            self.check_tree(expr)?;
        }
        result
    }

    fn parse_binop_inner(&mut self, min_prec: u8) -> Result<Expr, CompileError> {
        let first = self.parse_unary()?;
        self.binop_tail(min_prec, first)
    }

    /// Continue a binary expression whose first operand is already parsed.
    ///
    /// PUC's `subexpr` loops over left-associative operators. Only parsing
    /// the right operand enters another `subexpr` level.
    fn binop_tail(&mut self, min_prec: u8, mut left: Expr) -> Result<Expr, CompileError> {
        while let Some((op, prec, right_assoc)) = binop(&self.look.kind) {
            if prec < min_prec {
                break;
            }
            if !right_assoc {
                self.enter_tree();
            }
            // A right-associative operator's right side takes the rest of
            // the chain by recursing, which takes the depth bound already.
            self.bump()?;
            let next_min = if right_assoc { prec } else { prec + 1 };
            let right = self.parse_binop(next_min)?;
            let span = left.span().cover(right.span());
            left = if !right_assoc && self.tree_depth > MAX_EXPR_TREE_DEPTH {
                self.append_chain(left, ChainStep::Binary { span, op, right })?
            } else {
                self.charge_boxes(2)?;
                Expr::Binary {
                    span,
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        let op = match &self.look.kind {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Not => Some(UnOp::Not),
            TokenKind::Hash => Some(UnOp::Len),
            TokenKind::Tilde => Some(UnOp::Bnot),
            _ => None,
        };
        let Some(op) = op else {
            return self.parse_primary();
        };
        let token = self.bump()?;
        // Unary precedence is 11, below `^`, so `-a^b` is `-(a^b)`.
        let expr = self.parse_binop(11)?;
        self.charge_boxes(1)?;
        Ok(Expr::Unary {
            span: token.span.cover(expr.span()),
            op,
            expr: Box::new(expr),
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, CompileError> {
        let expr = match &self.look.kind {
            TokenKind::Nil => {
                let span = self.bump()?.span;
                Expr::Nil { span }
            }
            TokenKind::True => {
                let span = self.bump()?.span;
                Expr::Bool { span, value: true }
            }
            TokenKind::False => {
                let span = self.bump()?.span;
                Expr::Bool { span, value: false }
            }
            TokenKind::Integer(value) => {
                let value = *value;
                let span = self.bump()?.span;
                Expr::Integer { span, value }
            }
            TokenKind::Float(bits) => {
                let bits = *bits;
                let span = self.bump()?.span;
                Expr::Float { span, bits }
            }
            TokenKind::Str(_) => {
                let token = self.bump()?;
                let TokenKind::Str(bytes) = token.kind else {
                    unreachable!("token was a string");
                };
                Expr::Str {
                    span: token.span,
                    bytes,
                }
            }
            TokenKind::Name => {
                let name = self.expect_name()?;
                return self.with_suffix(Expr::Name {
                    span: name.span,
                    bytes: name.bytes,
                });
            }
            TokenKind::LParen => {
                let open = self.bump()?.span;
                let inner = self.parse_expr()?;
                let close = self
                    .expect_kind(|kind| matches!(kind, TokenKind::RParen), "')' expected")
                    .map_err(|mut error| {
                        if matches!(self.look.kind, TokenKind::Eof) {
                            error.message = format!(
                                "')' expected (to close '(' at line {})",
                                self.lexer.line(open.start)
                            );
                        }
                        error
                    })?;
                self.charge_boxes(1)?;
                return self.with_suffix(Expr::Paren {
                    span: open.cover(close.span),
                    inner: Box::new(inner),
                });
            }
            TokenKind::Function => {
                let keyword = self.bump()?.span;
                self.parse_function_body(keyword, false)?
            }
            // Whether the enclosing function is vararg is the compiler's
            // check, as a name's scope is.
            TokenKind::Dots => {
                self.record_near(self.look.span.start)?;
                let span = self.bump()?.span;
                Expr::Vararg { span }
            }
            TokenKind::LBrace => self.parse_table()?,
            _ => return Err(self.syntax("unexpected symbol")),
        };
        // Literals, constructors, and function bodies take no suffix: Lua's
        // grammar allows `[`, `.`, and calls only after a name or `( exp )`.
        Ok(expr)
    }

    /// `{ field [sep field]* [sep] }`, `sep` being `,` or `;`.
    fn parse_table(&mut self) -> Result<Expr, CompileError> {
        let open = self.bump()?.span;
        let mut fields = Vec::new();
        while !matches!(self.look.kind, TokenKind::RBrace) {
            self.reserve(&mut fields)?;
            fields.push(self.parse_field()?);
            if matches!(self.look.kind, TokenKind::Comma | TokenKind::Semi) {
                self.bump()?;
            } else {
                break;
            }
        }
        let close = self.expect_kind(|kind| matches!(kind, TokenKind::RBrace), "'}' expected")?;
        Ok(Expr::Table {
            span: open.cover(close.span),
            fields,
        })
    }

    fn parse_field(&mut self) -> Result<TableField, CompileError> {
        match self.look.kind {
            TokenKind::LBracket => {
                self.bump()?;
                let key = self.parse_expr()?;
                self.expect_kind(|kind| matches!(kind, TokenKind::RBracket), "']' expected")?;
                self.expect_kind(|kind| matches!(kind, TokenKind::Eq), "'=' expected")?;
                let value = self.parse_expr()?;
                Ok(TableField::Keyed { key, value })
            }
            TokenKind::Name => {
                let name = self.expect_name()?;
                if matches!(self.look.kind, TokenKind::Eq) {
                    self.bump()?;
                    let value = self.parse_expr()?;
                    return Ok(TableField::Keyed {
                        key: Expr::Str {
                            span: name.span,
                            bytes: name.bytes,
                        },
                        value,
                    });
                }
                // A list field that starts with a name: finish it as an
                // expression from that name.
                let saved = self.tree_depth;
                let first = self.with_suffix(Expr::Name {
                    span: name.span,
                    bytes: name.bytes,
                })?;
                let expr = self.binop_tail(0, first);
                self.tree_depth = saved;
                let expr = expr?;
                self.check_tree(&expr)?;
                Ok(TableField::List(expr))
            }
            _ => Ok(TableField::List(self.parse_expr()?)),
        }
    }

    /// `(params) block end`. A method, `function t:m(...)`, has `self`
    /// before its declared parameters.
    fn parse_function_body(&mut self, keyword: Span, method: bool) -> Result<Expr, CompileError> {
        let open = self.expect_kind(|kind| matches!(kind, TokenKind::LParen), "'(' expected")?;
        let mut params = Vec::new();
        if method {
            self.reserve(&mut params)?;
            self.charge(4)?;
            params.push(Name {
                span: open.span,
                bytes: b"self".to_vec(),
            });
        }
        let mut vararg = false;
        // `()`, `(...)`, `(a, b)`, or `(a, b, ...)`: `...` only last.
        if !matches!(self.look.kind, TokenKind::RParen) {
            loop {
                if matches!(self.look.kind, TokenKind::Dots) {
                    self.bump()?;
                    vararg = true;
                    break;
                }
                self.reserve(&mut params)?;
                params.push(self.expect_name()?);
                if !matches!(self.look.kind, TokenKind::Comma) {
                    break;
                }
                self.bump()?;
            }
        }
        self.expect_kind(|kind| matches!(kind, TokenKind::RParen), "')' expected")?;
        let body = self.parse_block(true)?;
        let end = self
            .expect_kind(|kind| matches!(kind, TokenKind::End), "'end' expected")
            .map_err(|mut error| {
                if matches!(self.look.kind, TokenKind::Eof) {
                    error.message = format!(
                        "'end' expected (to close 'function' at line {})",
                        self.lexer.line(keyword.start)
                    );
                }
                error
            })?;
        let expr = Expr::Function {
            span: keyword.cover(end.span),
            params,
            vararg,
            body,
        };
        self.check_tree(&expr)?;
        Ok(expr)
    }

    /// Suffixes extend the left spine and become flat steps past the
    /// recursive lowering threshold.
    fn with_suffix(&mut self, mut expr: Expr) -> Result<Expr, CompileError> {
        loop {
            if matches!(
                self.look.kind,
                TokenKind::LParen
                    | TokenKind::LBracket
                    | TokenKind::Dot
                    | TokenKind::LBrace
                    | TokenKind::Str(_)
                    | TokenKind::Colon
            ) {
                self.enter_tree();
            }
            match &self.look.kind {
                TokenKind::LParen => {
                    let call = self.parse_call(expr, None)?;
                    expr = self.flatten_suffix(call)?;
                }
                TokenKind::LBracket => {
                    self.bump()?;
                    let key = self.parse_expr()?;
                    let close = self
                        .expect_kind(|kind| matches!(kind, TokenKind::RBracket), "']' expected")?;
                    self.charge_boxes(2)?;
                    expr = self.flatten_suffix(Expr::Index {
                        span: expr.span().cover(close.span),
                        base: Box::new(expr),
                        key: Box::new(key),
                    })?;
                }
                TokenKind::Dot => {
                    self.bump()?;
                    let name = self.expect_name()?;
                    self.charge_boxes(2)?;
                    expr = self.flatten_suffix(Expr::Index {
                        span: expr.span().cover(name.span),
                        base: Box::new(expr),
                        key: Box::new(Expr::Str {
                            span: name.span,
                            bytes: name.bytes,
                        }),
                    })?;
                }
                TokenKind::LBrace | TokenKind::Str(_) => {
                    let call = self.parse_call(expr, None)?;
                    expr = self.flatten_suffix(call)?;
                }
                TokenKind::Colon => {
                    self.bump()?;
                    let name = self.expect_name()?;
                    if !matches!(
                        self.look.kind,
                        TokenKind::LParen | TokenKind::LBrace | TokenKind::Str(_)
                    ) {
                        return Err(self.syntax("function arguments expected"));
                    }
                    let call = self.parse_call(expr, Some(name))?;
                    expr = self.flatten_suffix(call)?;
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn append_chain(&mut self, left: Expr, step: ChainStep) -> Result<Expr, CompileError> {
        let span = step.span();
        match left {
            Expr::Chain {
                first, mut rest, ..
            } => {
                self.reserve(&mut rest)?;
                rest.push(step);
                Ok(Expr::Chain { span, first, rest })
            }
            mut first => {
                // Convert the existing prefix as well. Otherwise the first
                // flat step would still own a 300-deep recursive destructor.
                self.charge(std::mem::size_of::<ChainStep>())?;
                let mut reversed = vec![step];
                loop {
                    first = match first {
                        Expr::Binary {
                            span,
                            op,
                            left,
                            right,
                        } => {
                            self.reserve(&mut reversed)?;
                            reversed.push(ChainStep::Binary {
                                span,
                                op,
                                right: *right,
                            });
                            *left
                        }
                        Expr::Index { span, base, key } => {
                            self.reserve(&mut reversed)?;
                            reversed.push(ChainStep::Index { span, key: *key });
                            *base
                        }
                        Expr::Call {
                            span,
                            func,
                            method,
                            args,
                        } => {
                            self.reserve(&mut reversed)?;
                            reversed.push(ChainStep::Call { span, method, args });
                            *func
                        }
                        other => {
                            reversed.reverse();
                            self.charge_boxes(1)?;
                            return Ok(Expr::Chain {
                                span,
                                first: Box::new(other),
                                rest: reversed,
                            });
                        }
                    };
                }
            }
        }
    }

    fn flatten_suffix(&mut self, expr: Expr) -> Result<Expr, CompileError> {
        if self.tree_depth <= MAX_EXPR_TREE_DEPTH {
            return Ok(expr);
        }
        match expr {
            Expr::Index { span, base, key } => {
                self.append_chain(*base, ChainStep::Index { span, key: *key })
            }
            Expr::Call {
                span,
                func,
                method,
                args,
            } => self.append_chain(*func, ChainStep::Call { span, method, args }),
            other => Ok(other),
        }
    }

    /// The arguments of a call: `(explist)`, one table constructor, or one
    /// string literal.
    fn parse_call(&mut self, func: Expr, method: Option<Name>) -> Result<Expr, CompileError> {
        let (args, end) = match self.look.kind {
            TokenKind::LBrace => {
                let table = self.parse_table()?;
                let end = table.span();
                self.charge(std::mem::size_of::<Expr>())?;
                (vec![table], end)
            }
            TokenKind::Str(_) => {
                let token = self.bump()?;
                let TokenKind::Str(bytes) = token.kind else {
                    unreachable!("token was a string");
                };
                self.charge(std::mem::size_of::<Expr>())?;
                (
                    vec![Expr::Str {
                        span: token.span,
                        bytes,
                    }],
                    token.span,
                )
            }
            _ => {
                self.bump()?;
                let mut args = Vec::new();
                if !matches!(self.look.kind, TokenKind::RParen) {
                    args = self.expr_list()?;
                }
                let close =
                    self.expect_kind(|kind| matches!(kind, TokenKind::RParen), "')' expected")?;
                (args, close.span)
            }
        };
        self.charge_boxes(1)?;
        Ok(Expr::Call {
            span: func.span().cover(end),
            func: Box::new(func),
            method,
            args,
        })
    }

    fn starts_expr(&self) -> bool {
        matches!(
            self.look.kind,
            TokenKind::Nil
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Integer(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::Name
                | TokenKind::LParen
                | TokenKind::Function
                | TokenKind::Minus
                | TokenKind::Not
                | TokenKind::Hash
                | TokenKind::Tilde
                | TokenKind::Dots
                | TokenKind::LBrace
        )
    }

    fn expect_name(&mut self) -> Result<Name, CompileError> {
        if !matches!(self.look.kind, TokenKind::Name) {
            return Err(self.syntax("<name> expected"));
        }
        let token = self.bump()?;
        self.record_near(token.span.start)?;
        self.charge((token.span.end - token.span.start) as usize)?;
        Ok(Name {
            bytes: self.lexer.slice(token.span).to_vec(),
            span: token.span,
        })
    }

    fn expect_kind(
        &mut self,
        pred: impl Fn(&TokenKind) -> bool,
        message: &str,
    ) -> Result<Token, CompileError> {
        if !pred(&self.look.kind) {
            return Err(self.syntax(message));
        }
        self.bump()
    }

    fn bump(&mut self) -> Result<Token, CompileError> {
        if let TokenKind::Str(bytes) = &self.look.kind {
            self.charge(bytes.capacity())?;
        }
        let next = self.lexer.next_token()?;
        Ok(std::mem::replace(&mut self.look, next))
    }

    /// Charge actual retained bytes with a 2x safety margin for lowering,
    /// tree traversal, allocator overhead and transient reallocation copies.
    /// Charges are cumulative: flattening/dropping nodes does not refund them.
    fn charge(&mut self, bytes: usize) -> Result<(), CompileError> {
        if self.ast_bytes_left == u64::MAX {
            return Ok(());
        }
        self.ast_bytes_left = self
            .ast_bytes_left
            .checked_sub((bytes as u64).saturating_mul(2))
            .ok_or_else(|| self.memory_error())?;
        Ok(())
    }

    fn memory_error(&self) -> CompileError {
        CompileError::new(
            CompileErrorKind::Limit,
            self.look.span,
            crate::limits::COMPILE_MEMORY,
        )
    }

    fn charge_boxes(&mut self, count: usize) -> Result<(), CompileError> {
        // Include a conservative per-allocation header/alignment allowance.
        self.charge(count * (std::mem::size_of::<Expr>() + 16))
    }

    fn reserve<T>(&mut self, values: &mut Vec<T>) -> Result<(), CompileError> {
        if self.ast_bytes_left == u64::MAX || values.len() < values.capacity() {
            return Ok(());
        }
        let old = values.capacity();
        let new = old.saturating_mul(2).max(4);
        self.charge((new - old).saturating_mul(std::mem::size_of::<T>()) + 16)?;
        values
            .try_reserve_exact(new - values.len())
            .map_err(|_| self.memory_error())?;
        Ok(())
    }

    fn record_near(&mut self, offset: u32) -> Result<(), CompileError> {
        // `near` owns quoted source bytes; string quoting also uses a temporary
        // vector. The decoded spelling is no longer than the source spelling.
        let bytes = (self.look.span.end - self.look.span.start) as usize + 2;
        self.charge(bytes.saturating_mul(2) + 16)?;
        // Borrow the two parser fields separately to grow this retained vector.
        let mut near = std::mem::take(&mut self.diagnostic_near);
        self.reserve(&mut near)?;
        near.push((offset, self.lexer.near(&self.look)));
        self.diagnostic_near = near;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), CompileError> {
        #[cfg(test)]
        crate::limits::note_stack();
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(CompileError::new(
                CompileErrorKind::Limit,
                self.look.span,
                "C stack overflow",
            ));
        }
        self.depth += 1;
        Ok(())
    }

    // Check actual tree height as well as the parser's loop counter. A
    // parenthesized chain can otherwise reset that counter and grow another
    // full chain at each level, overflowing lowering or recursive AST drop.
    fn check_tree(&self, expr: &Expr) -> Result<(), CompileError> {
        let mut pending = vec![(expr, 0u32)];
        let mut blocks = Vec::new();
        loop {
            if let Some((block, depth)) = blocks.pop() {
                let block: &Block = block;
                if depth > MAX_EXPR_TREE_DEPTH {
                    return Err(CompileError::new(
                        CompileErrorKind::Limit,
                        block.span,
                        format!("expression tree nesting exceeds {MAX_EXPR_TREE_DEPTH}"),
                    ));
                }
                for stmt in &block.stmts {
                    match stmt {
                        Stmt::Local { values, .. } | Stmt::Return { values, .. } => {
                            pending.extend(values.iter().map(|expr| (expr, depth)))
                        }
                        Stmt::Assign {
                            targets, values, ..
                        } => {
                            pending.extend(values.iter().map(|expr| (expr, depth)));
                            for target in targets {
                                if let Target::Index { base, key, .. } = target {
                                    pending.push((base, depth));
                                    pending.push((key, depth));
                                }
                            }
                        }
                        Stmt::Call { call, .. } => pending.push((call, depth)),
                        Stmt::LocalFunction { func, .. } => pending.push((func, depth)),
                        Stmt::Do { body, .. } => blocks.push((body, depth + 1)),
                        Stmt::While { cond, body, .. } | Stmt::Repeat { cond, body, .. } => {
                            pending.push((cond, depth));
                            blocks.push((body, depth + 1));
                        }
                        Stmt::If {
                            arms, else_block, ..
                        } => {
                            for (cond, body) in arms {
                                pending.push((cond, depth));
                                blocks.push((body, depth + 1));
                            }
                            if let Some(body) = else_block {
                                blocks.push((body, depth + 1));
                            }
                        }
                        Stmt::NumericFor {
                            init,
                            limit,
                            step,
                            body,
                            ..
                        } => {
                            pending.push((init, depth));
                            pending.push((limit, depth));
                            pending.extend(step.iter().map(|expr| (expr, depth)));
                            blocks.push((body, depth + 1));
                        }
                        Stmt::GenericFor { values, body, .. } => {
                            pending.extend(values.iter().map(|expr| (expr, depth)));
                            blocks.push((body, depth + 1));
                        }
                        _ => {}
                    }
                }
                continue;
            }
            let Some((expr, depth)) = pending.pop() else {
                break;
            };
            if depth > MAX_EXPR_TREE_DEPTH {
                return Err(CompileError::new(
                    CompileErrorKind::Limit,
                    expr.span(),
                    format!("expression tree nesting exceeds {MAX_EXPR_TREE_DEPTH}"),
                ));
            }
            let next = depth + 1;
            match expr {
                Expr::Paren { inner, .. } | Expr::Unary { expr: inner, .. } => {
                    pending.push((inner, next));
                }
                Expr::Binary { left, right, .. } => {
                    pending.push((left, depth));
                    pending.push((right, next));
                }
                Expr::Index { base, key, .. } => {
                    pending.push((base, depth));
                    pending.push((key, next));
                }
                Expr::Call { func, args, .. } => {
                    pending.push((func, depth));
                    pending.extend(args.iter().map(|arg| (arg, next)));
                }
                Expr::Chain { first, rest, .. } => {
                    pending.push((first, depth));
                    for step in rest {
                        match step {
                            ChainStep::Binary { right, .. } => pending.push((right, next)),
                            ChainStep::Index { key, .. } => pending.push((key, next)),
                            ChainStep::Call { args, .. } => {
                                pending.extend(args.iter().map(|arg| (arg, next)))
                            }
                        }
                    }
                }
                Expr::Function { body, .. } => blocks.push((body, next)),
                Expr::Table { fields, .. } => {
                    for field in fields {
                        match field {
                            TableField::List(value) => pending.push((value, next)),
                            TableField::Keyed { key, value } => {
                                pending.push((key, next));
                                pending.push((value, next));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn enter_tree(&mut self) {
        self.tree_depth = self.tree_depth.saturating_add(1);
    }

    fn syntax(&self, message: &str) -> CompileError {
        CompileError::new(CompileErrorKind::Syntax, self.look.span, message)
            .with_near(self.lexer.near(&self.look))
            .with_diagnostic_line(self.lexer.line(self.look.span.end))
    }
}

fn target(expr: Expr) -> Result<Target, CompileError> {
    match expr {
        Expr::Name { span, bytes } => Ok(Target::Name(Name { span, bytes })),
        Expr::Index { span, base, key } => Ok(Target::Index {
            span,
            base: *base,
            key: *key,
        }),
        Expr::Chain {
            first, mut rest, ..
        } if matches!(rest.last(), Some(ChainStep::Index { .. })) => {
            let Some(ChainStep::Index { span, key }) = rest.pop() else {
                unreachable!()
            };
            let base = if let Some(last) = rest.last() {
                Expr::Chain {
                    span: last.span(),
                    first,
                    rest,
                }
            } else {
                *first
            };
            Ok(Target::Index { span, base, key })
        }
        other => Err(CompileError::new(
            CompileErrorKind::Syntax,
            other.span(),
            "cannot assign to this expression",
        )),
    }
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::Empty { span }
        | Stmt::Local { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Call { span, .. }
        | Stmt::If { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Repeat { span, .. }
        | Stmt::Break { span }
        | Stmt::NumericFor { span, .. }
        | Stmt::LocalFunction { span, .. }
        | Stmt::Label { span, .. }
        | Stmt::Goto { span, .. }
        | Stmt::GenericFor { span, .. } => *span,
    }
}

fn binop(kind: &TokenKind) -> Option<(BinOp, u8, bool)> {
    Some(match kind {
        TokenKind::Or => (BinOp::Or, 1, false),
        TokenKind::And => (BinOp::And, 2, false),
        TokenKind::Lt => (BinOp::Lt, 3, false),
        TokenKind::Gt => (BinOp::Gt, 3, false),
        TokenKind::LtEq => (BinOp::Le, 3, false),
        TokenKind::GtEq => (BinOp::Ge, 3, false),
        TokenKind::TildeEq => (BinOp::Ne, 3, false),
        TokenKind::EqEq => (BinOp::Eq, 3, false),
        TokenKind::Pipe => (BinOp::Bor, 4, false),
        TokenKind::Tilde => (BinOp::Bxor, 5, false),
        TokenKind::Amp => (BinOp::Band, 6, false),
        TokenKind::Shl => (BinOp::Shl, 7, false),
        TokenKind::Shr => (BinOp::Shr, 7, false),
        TokenKind::Concat => (BinOp::Concat, 8, true),
        TokenKind::Plus => (BinOp::Add, 9, false),
        TokenKind::Minus => (BinOp::Sub, 9, false),
        TokenKind::Star => (BinOp::Mul, 10, false),
        TokenKind::Slash => (BinOp::Div, 10, false),
        TokenKind::Idiv => (BinOp::Idiv, 10, false),
        TokenKind::Percent => (BinOp::Mod, 10, false),
        TokenKind::Caret => (BinOp::Pow, 12, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn returned(source: &str) -> String {
        let chunk = parse(source.as_bytes()).unwrap_or_else(|error| panic!("{error:?}"));
        let Stmt::Return { values, .. } = &chunk.body.stmts[0] else {
            panic!("not a return: {source}");
        };
        assert_eq!(values.len(), 1);
        shape(&values[0])
    }

    fn shape(expr: &Expr) -> String {
        match expr {
            Expr::Name { bytes, .. } => String::from_utf8_lossy(bytes).into_owned(),
            Expr::Integer { value, .. } => value.to_string(),
            Expr::Binary {
                op, left, right, ..
            } => {
                format!("({}{}{})", shape(left), op_char(*op), shape(right))
            }
            Expr::Unary { op, expr, .. } => format!("({}{})", un_char(*op), shape(expr)),
            other => panic!("unshaped {other:?}"),
        }
    }

    fn op_char(op: BinOp) -> &'static str {
        match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Pow => "^",
            BinOp::Concat => "..",
            BinOp::And => " and ",
            BinOp::Or => " or ",
            other => panic!("{other:?}"),
        }
    }

    fn un_char(op: UnOp) -> &'static str {
        match op {
            UnOp::Neg => "-",
            UnOp::Not => "not ",
            UnOp::Len => "#",
            UnOp::Bnot => "~",
        }
    }

    #[test]
    fn precedence_matches_lua() {
        assert_eq!(returned("return a+b+c"), "((a+b)+c)");
        assert_eq!(returned("return a^b^c"), "(a^(b^c))");
        assert_eq!(returned("return -a^b"), "(-(a^b))");
        assert_eq!(returned("return a..b..c"), "(a..(b..c))");
        assert_eq!(returned("return a+b*c"), "(a+(b*c))");
        assert_eq!(returned("return not a and b"), "((not a) and b)");
        assert_eq!(returned("return a^-b^c"), "(a^(-(b^c)))");
    }

    #[test]
    fn return_must_end_the_block_and_unsupported_is_not_parsed_as_success() {
        assert!(parse(b"return 1;").is_ok());
        assert_eq!(
            parse(b"return 1;;").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
        assert_eq!(parse(b";;; return 1").unwrap().body.stmts.len(), 4);
        let Stmt::GenericFor {
            names,
            values,
            span,
            ..
        } = &parse(b"for k, v in x, y, z do end").unwrap().body.stmts[0]
        else {
            panic!("not a generic for");
        };
        assert_eq!((names.len(), values.len()), (2, 3));
        assert_eq!(*span, Span::new(0, 26));
        for source in [
            &b"for k, v = 1, 2 do end"[..],
            b"for k in do end",
            b"for in x do end",
            b"for k, in x do end",
            b"for k <close> in x do end",
            b"for k in x end",
        ] {
            assert_eq!(
                parse(source).unwrap_err().kind,
                CompileErrorKind::Syntax,
                "{}",
                String::from_utf8_lossy(source)
            );
        }
        assert!(parse(b"for i = 1, 2 do end").is_ok());
        assert!(parse(b"for i = 3, 1, -1 do break end").is_ok());
        assert_eq!(
            parse(b"for i = 1 do end").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
        let Stmt::If { arms, .. } = &parse(b"if a then elseif b then elseif c then else end")
            .unwrap()
            .body
            .stmts[0]
        else {
            panic!("not an if");
        };
        assert_eq!(arms.len(), 3);
        assert_eq!(
            parse(b"until x").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
        assert!(parse(b"repeat local x = 1 until x").is_ok());
        assert!(parse(b"while a do break local y = 1 end").is_ok());
        let source = b"if a then local x = 1 else return x end";
        let Stmt::If { span, .. } = &parse(source).unwrap().body.stmts[0] else {
            panic!("not an if");
        };
        assert_eq!(*span, Span::new(0, source.len() as u32));
        assert_eq!(parse(b"else").unwrap_err().kind, CompileErrorKind::Syntax);
        assert_eq!(
            parse(b"local f = function() else end").unwrap_err().kind,
            CompileErrorKind::Syntax
        );
        let deep = format!("{}1{}", "(".repeat(300), ")".repeat(300));
        assert_eq!(
            parse(deep.as_bytes()).unwrap_err().kind,
            CompileErrorKind::Limit
        );
    }
}
