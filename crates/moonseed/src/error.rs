//! Structured frontend errors and their single Lua diagnostic renderer.

use std::fmt;

use crate::span::Span;

/// The category of a source compilation failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompileErrorKind {
    /// A numeric literal could not be parsed.
    MalformedNumber,
    /// A quoted string was not terminated.
    UnfinishedString,
    /// A string escape is invalid.
    MalformedEscape,
    /// A long string was not terminated.
    UnfinishedLongString,
    /// A long comment was not terminated.
    UnfinishedLongComment,
    /// A long string delimiter is invalid.
    InvalidLongStringDelimiter,
    /// The lexer encountered an invalid character.
    UnexpectedCharacter,
    /// The source violates Lua syntax.
    Syntax,
    /// The source requests an unsupported construct.
    Unsupported,
    /// Compilation exceeded a compiler limit.
    Limit,
    /// The emitted bytecode failed validation.
    InvalidProgram,
}

/// A compilation failure with its source range and diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompileError {
    /// The failure category.
    pub kind: CompileErrorKind,
    /// The source bytes containing the failure.
    pub span: Span,
    /// The diagnostic text.
    pub message: String,
    /// The 1-based source line of the diagnostic.
    pub line: u32,
    /// The token following `near`, already displayed using Lua's token rules.
    pub near: Option<Vec<u8>>,
    short_src: Vec<u8>,
    opening_line: Option<u32>,
    diagnostic_line: Option<u32>,
}

impl CompileError {
    pub(crate) fn new(kind: CompileErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            span,
            message: message.into(),
            line: 1,
            near: None,
            short_src: Vec::new(),
            opening_line: None,
            diagnostic_line: None,
        }
    }

    pub(crate) fn with_near(mut self, near: Vec<u8>) -> Self {
        self.near = Some(near);
        self
    }

    pub(crate) fn with_optional_near(mut self, near: Option<Vec<u8>>) -> Self {
        self.near = near;
        self
    }

    pub(crate) fn with_opening_line(mut self, line: u32) -> Self {
        self.opening_line = Some(line);
        self
    }

    pub(crate) fn with_diagnostic_line(mut self, line: u32) -> Self {
        self.diagnostic_line = Some(line);
        self
    }

    pub(crate) fn with_source(mut self, source: &[u8]) -> Self {
        let offset = if matches!(
            self.kind,
            CompileErrorKind::UnfinishedString
                | CompileErrorKind::UnfinishedLongString
                | CompileErrorKind::UnfinishedLongComment
        ) {
            self.span.end
        } else {
            self.span.start
        };
        self.line = self
            .diagnostic_line
            .unwrap_or_else(|| crate::span::line_col(source, offset).0);
        self.short_src = crate::chunkname::chunk_id(source);
        self
    }

    /// Render the complete Lua diagnostic for `chunk_name` (for example,
    /// `@diag.lua`, `=chunk`, or a literal source name).
    pub fn render(&self, chunk_name: &[u8]) -> Vec<u8> {
        self.render_short(&crate::chunkname::chunk_id(chunk_name), usize::MAX)
            .expect("source diagnostics fit in usize")
    }

    /// Render only when the complete string fits the runtime's string limit.
    /// The runtime still charges the resulting string to its heap quota.
    pub(crate) fn render_bounded(&self, chunk_name: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
        self.render_short(&crate::chunkname::chunk_id(chunk_name), max_bytes)
    }

    fn render_short(&self, short_src: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
        let prefix = format!(":{}: ", self.line);
        let opening = self
            .opening_line
            .map(|line| format!(" (starting at line {line})"));
        let near_size = self.near.as_ref().map_or(0, |near| 6 + near.len());
        let size = short_src
            .len()
            .checked_add(prefix.len())?
            .checked_add(self.message.len())?
            .checked_add(opening.as_ref().map_or(0, String::len))?
            .checked_add(near_size)?;
        if size > max_bytes {
            return None;
        }
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(short_src);
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(self.message.as_bytes());
        if let Some(opening) = opening {
            out.extend_from_slice(opening.as_bytes());
        }
        if let Some(near) = &self.near {
            out.extend_from_slice(b" near ");
            out.extend_from_slice(near);
        }
        Some(out)
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self
            .render_short(&self.short_src, usize::MAX)
            .ok_or(fmt::Error)?;
        f.write_str(&String::from_utf8_lossy(&rendered))
    }
}

impl std::error::Error for CompileError {}

#[cfg(test)]
mod tests {
    use super::{CompileErrorKind, Span};

    #[test]
    fn parser_error_keeps_kind_span_and_near_token() {
        let source = b"local x = *";
        let error = crate::compile(source).unwrap_err();
        assert_eq!(error.kind, CompileErrorKind::Syntax);
        assert_eq!(error.span, Span::new(10, 11));
        assert_eq!(error.near.as_deref(), Some(b"'*'".as_slice()));
        assert_eq!(
            error.render(b"@diag.lua"),
            b"diag.lua:1: unexpected symbol near '*'".to_vec()
        );
        assert_eq!(
            error.to_string(),
            "[string \"local x = *\"]:1: unexpected symbol near '*'"
        );
    }
}
