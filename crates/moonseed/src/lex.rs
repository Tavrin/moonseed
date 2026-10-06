//! Byte lexer for the Lua 5.4 lexical surface.
//!
//! Source is `&[u8]`. Identifiers use the Lua alphabet, not Unicode. A chunk
//! shebang is not a token: the `lua` program strips it, `load` does not.
//!
//! Numeral scanning follows Lua 5.4: dots and an exponent marker are consumed
//! liberally, then the text is accepted or rejected as one number. `3..4` is
//! therefore a malformed number. `3. .. 4` is a float, `..`, and an integer.

use crate::error::{CompileError, CompileErrorKind};
#[cfg(any(test, feature = "__measure"))]
use crate::limits::DEFAULT_SOURCE_BYTES;
use crate::limits::MAX_SOURCE_BYTES;
use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    And,
    Break,
    Do,
    Else,
    Elseif,
    End,
    False,
    For,
    Function,
    Goto,
    If,
    In,
    Local,
    Nil,
    Not,
    Or,
    Repeat,
    Return,
    Then,
    True,
    Until,
    While,
    Name,
    Integer(i64),
    Float(u64),
    Str(Vec<u8>),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Hash,
    Amp,
    Tilde,
    Pipe,
    Shl,
    Shr,
    Idiv,
    EqEq,
    TildeEq,
    LtEq,
    GtEq,
    Lt,
    Gt,
    Eq,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    ColonColon,
    Semi,
    Colon,
    Comma,
    Dot,
    Concat,
    Dots,
    Eof,
}

pub(crate) struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    max_source_bytes: usize,
    string_start: Option<usize>,
}

impl<'a> Lexer<'a> {
    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn new(source: &'a [u8]) -> Result<Self, CompileError> {
        Self::with_limit(source, DEFAULT_SOURCE_BYTES)
    }

    pub(crate) fn with_limit(
        source: &'a [u8],
        max_source_bytes: usize,
    ) -> Result<Self, CompileError> {
        let max_source_bytes = max_source_bytes.min(MAX_SOURCE_BYTES);
        if source.len() > max_source_bytes {
            return Err(CompileError::new(
                CompileErrorKind::Limit,
                Span::new(0, 0),
                format!("source exceeds {max_source_bytes} bytes"),
            ));
        }
        Ok(Self {
            source,
            pos: 0,
            max_source_bytes,
            string_start: None,
        })
    }

    pub(crate) fn slice(&self, span: Span) -> &'a [u8] {
        &self.source[span.start as usize..span.end as usize]
    }

    pub(crate) fn line(&self, offset: u32) -> u32 {
        crate::span::line_col(self.source, offset).0
    }

    /// Lua's near-token display: EOF is unquoted, all other tokens retain
    /// their source spelling, including quotes and malformed bytes.
    pub(crate) fn near(&self, token: &Token) -> Vec<u8> {
        if matches!(token.kind, TokenKind::Eof) {
            b"<eof>".to_vec()
        } else if let TokenKind::Str(bytes) = &token.kind {
            let source = self.slice(token.span);
            let delimiter = if source[0] == b'[' {
                2 + source[1..].iter().take_while(|byte| **byte == b'=').count()
            } else {
                1
            };
            let mut text = source[..delimiter].to_vec();
            text.extend_from_slice(bytes);
            text.extend_from_slice(&source[source.len() - delimiter..]);
            quote_near(&text)
        } else {
            quote_near(self.slice(token.span))
        }
    }

    pub(crate) fn next_token(&mut self) -> Result<Token, CompileError> {
        loop {
            let start = self.pos;
            let Some(byte) = self.peek() else {
                return Ok(self.token(TokenKind::Eof, start));
            };
            match byte {
                b' ' | b'\t' | 0x0b | 0x0c => {
                    self.bump();
                }
                b'\n' | b'\r' => self.consume_newline(),
                b'-' if self.peek_at(1) == Some(b'-') => {
                    self.bump();
                    self.bump();
                    self.skip_comment(start)?;
                }
                b'-' => return Ok(self.one(TokenKind::Minus, start)),
                b'[' => return self.bracket_or_string(start),
                b'\'' | b'"' => return self.short_string(byte, start),
                b'.' => return self.dot(start),
                b'0'..=b'9' => return self.read_numeral(start),
                b'=' => return Ok(self.either(start, b'=', TokenKind::EqEq, TokenKind::Eq)),
                b'<' => {
                    return Ok(self.either2(
                        start,
                        b'=',
                        TokenKind::LtEq,
                        b'<',
                        TokenKind::Shl,
                        TokenKind::Lt,
                    ));
                }
                b'>' => {
                    return Ok(self.either2(
                        start,
                        b'=',
                        TokenKind::GtEq,
                        b'>',
                        TokenKind::Shr,
                        TokenKind::Gt,
                    ));
                }
                b'/' => return Ok(self.either(start, b'/', TokenKind::Idiv, TokenKind::Slash)),
                b'~' => return Ok(self.either(start, b'=', TokenKind::TildeEq, TokenKind::Tilde)),
                b':' => {
                    return Ok(self.either(start, b':', TokenKind::ColonColon, TokenKind::Colon));
                }
                b'+' => return Ok(self.one(TokenKind::Plus, start)),
                b'*' => return Ok(self.one(TokenKind::Star, start)),
                b'%' => return Ok(self.one(TokenKind::Percent, start)),
                b'^' => return Ok(self.one(TokenKind::Caret, start)),
                b'#' => return Ok(self.one(TokenKind::Hash, start)),
                b'&' => return Ok(self.one(TokenKind::Amp, start)),
                b'|' => return Ok(self.one(TokenKind::Pipe, start)),
                b'(' => return Ok(self.one(TokenKind::LParen, start)),
                b')' => return Ok(self.one(TokenKind::RParen, start)),
                b'{' => return Ok(self.one(TokenKind::LBrace, start)),
                b'}' => return Ok(self.one(TokenKind::RBrace, start)),
                b']' => return Ok(self.one(TokenKind::RBracket, start)),
                b';' => return Ok(self.one(TokenKind::Semi, start)),
                b',' => return Ok(self.one(TokenKind::Comma, start)),
                c if is_lua_alpha(c) => return Ok(self.name(start)),
                _ => {
                    self.bump();
                    return Err(self.err(
                        CompileErrorKind::UnexpectedCharacter,
                        start,
                        self.pos,
                        "unexpected symbol",
                    ));
                }
            }
        }
    }

    fn bracket_or_string(&mut self, start: usize) -> Result<Token, CompileError> {
        match self.scan_bracket()? {
            Bracket::Long { level } => {
                let bytes = self.read_long(level, start, false)?;
                Ok(self.token(TokenKind::Str(bytes), start))
            }
            Bracket::Single => Ok(self.token(TokenKind::LBracket, start)),
            Bracket::Broken { end } => Err(self.err(
                CompileErrorKind::InvalidLongStringDelimiter,
                start,
                end,
                "invalid long string delimiter",
            )),
        }
    }

    fn dot(&mut self, start: usize) -> Result<Token, CompileError> {
        if self.peek_at(1) == Some(b'.') {
            self.bump();
            self.bump();
            if self.peek() == Some(b'.') {
                self.bump();
                return Ok(self.token(TokenKind::Dots, start));
            }
            return Ok(self.token(TokenKind::Concat, start));
        }
        if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            return self.read_numeral(start);
        }
        Ok(self.one(TokenKind::Dot, start))
    }

    fn skip_comment(&mut self, start: usize) -> Result<(), CompileError> {
        if self.peek() == Some(b'[') {
            match self.scan_bracket()? {
                Bracket::Long { level } => self.read_long(level, start, true).map(|_| ())?,
                Bracket::Single | Bracket::Broken { .. } => self.skip_line(),
            }
        } else {
            self.skip_line();
        }
        Ok(())
    }

    fn skip_line(&mut self) {
        while let Some(byte) = self.peek() {
            if byte == b'\n' || byte == b'\r' {
                break;
            }
            self.bump();
        }
    }

    fn scan_bracket(&mut self) -> Result<Bracket, CompileError> {
        self.bump();
        let mut level = 0u32;
        while self.peek() == Some(b'=') {
            self.bump();
            level = level.saturating_add(1);
        }
        if self.peek() == Some(b'[') {
            self.bump();
            return Ok(Bracket::Long { level });
        }
        if level == 0 {
            return Ok(Bracket::Single);
        }
        Ok(Bracket::Broken { end: self.pos })
    }

    fn read_long(
        &mut self,
        level: u32,
        start: usize,
        comment: bool,
    ) -> Result<Vec<u8>, CompileError> {
        let mut out = Vec::new();
        if self.at_newline() {
            self.consume_newline();
        }
        loop {
            let Some(byte) = self.peek() else {
                let kind = if comment {
                    CompileErrorKind::UnfinishedLongComment
                } else {
                    CompileErrorKind::UnfinishedLongString
                };
                let what = if comment { "comment" } else { "string" };
                return Err(self
                    .err(kind, start, self.pos, format!("unfinished long {what}"))
                    .with_opening_line(self.line(pos_u32(start))));
            };
            if byte == b']' {
                let mark = self.pos;
                self.bump();
                let mut eqs = 0u32;
                while self.peek() == Some(b'=') {
                    self.bump();
                    eqs = eqs.saturating_add(1);
                }
                if self.peek() == Some(b']') && eqs == level {
                    self.bump();
                    break;
                }
                if !comment {
                    let taken = self.source[mark..self.pos].to_vec();
                    self.push_bytes(&mut out, taken, start)?;
                }
                continue;
            }
            if byte == b'\n' || byte == b'\r' {
                self.consume_newline();
                if !comment {
                    self.push_bytes(&mut out, vec![b'\n'], start)?;
                }
                continue;
            }
            self.bump();
            if !comment {
                self.push_bytes(&mut out, vec![byte], start)?;
            }
        }
        Ok(out)
    }

    fn short_string(&mut self, quote: u8, start: usize) -> Result<Token, CompileError> {
        self.string_start = Some(start);
        self.bump();
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.err(
                    CompileErrorKind::UnfinishedString,
                    start,
                    self.pos,
                    "unfinished string",
                ));
            };
            if byte == quote {
                self.bump();
                break;
            }
            if byte == b'\n' || byte == b'\r' {
                let mut text = vec![quote];
                text.extend_from_slice(&out);
                return Err(self
                    .err(
                        CompileErrorKind::UnfinishedString,
                        start,
                        self.pos,
                        "unfinished string",
                    )
                    .with_near(quote_near(&text)));
            }
            if byte == b'\\' {
                let escape_start = self.pos;
                self.bump();
                if let Err(mut error) = self.read_escape(&mut out, start) {
                    if error.kind == CompileErrorKind::MalformedEscape {
                        let end = error
                            .near
                            .as_ref()
                            .map_or(self.pos, |near| start + near.len().saturating_sub(2));
                        let mut text = vec![quote];
                        text.extend_from_slice(&out);
                        if end >= escape_start {
                            text.extend_from_slice(&self.source[escape_start..end]);
                        }
                        error.near = Some(quote_near(&text));
                    }
                    return Err(error);
                }
                continue;
            }
            self.bump();
            self.push_bytes(&mut out, vec![byte], start)?;
        }
        self.string_start = None;
        Ok(self.token(TokenKind::Str(out), start))
    }

    fn read_escape(&mut self, out: &mut Vec<u8>, start: usize) -> Result<(), CompileError> {
        let esc = self.pos - 1;
        let Some(byte) = self.bump() else {
            return Err(self.err(
                CompileErrorKind::UnfinishedString,
                start,
                self.pos,
                "unfinished string",
            ));
        };
        let simple = match byte {
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'v' => Some(0x0b),
            b'\\' => Some(b'\\'),
            b'\'' => Some(b'\''),
            b'"' => Some(b'"'),
            _ => None,
        };
        if let Some(value) = simple {
            return self.push_bytes(out, vec![value], start);
        }
        match byte {
            b'\n' | b'\r' => {
                if let Some(second) = self.peek() {
                    let pair =
                        (byte == b'\n' && second == b'\r') || (byte == b'\r' && second == b'\n');
                    if pair {
                        self.bump();
                    }
                }
                self.push_bytes(out, vec![b'\n'], start)
            }
            b'z' => {
                while let Some(next) = self.peek() {
                    if next == b'\n' || next == b'\r' {
                        self.consume_newline();
                    } else if is_lua_space(next) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            b'x' => {
                let hi = self.escape_hex(esc)?;
                let lo = self.escape_hex(esc)?;
                self.push_bytes(out, vec![(hi << 4) | lo], start)
            }
            b'u' => {
                let encoded = self.utf8_escape(esc)?;
                self.push_bytes(out, encoded, start)
            }
            b'0'..=b'9' => {
                let mut value = u32::from(byte - b'0');
                let mut count = 1;
                while count < 3 {
                    let Some(digit) = self.peek() else { break };
                    if !digit.is_ascii_digit() {
                        break;
                    }
                    self.bump();
                    value = value * 10 + u32::from(digit - b'0');
                    count += 1;
                }
                if value > 255 {
                    return Err(self
                        .err(
                            CompileErrorKind::MalformedEscape,
                            esc,
                            self.pos,
                            "decimal escape too large",
                        )
                        .with_near(quote_near(
                            &self.source[start..(self.pos + 1).min(self.source.len())],
                        )));
                }
                self.push_bytes(out, vec![value as u8], start)
            }
            _ => Err(self.err(
                CompileErrorKind::MalformedEscape,
                esc,
                self.pos,
                "invalid escape sequence",
            )),
        }
    }

    fn escape_hex(&mut self, esc: usize) -> Result<u8, CompileError> {
        let Some(byte) = self.bump() else {
            return Err(self.err(
                CompileErrorKind::MalformedEscape,
                esc,
                self.pos,
                "hexadecimal digit expected",
            ));
        };
        hex_val(byte).ok_or_else(|| {
            self.err(
                CompileErrorKind::MalformedEscape,
                esc,
                self.pos,
                "hexadecimal digit expected",
            )
        })
    }

    fn utf8_escape(&mut self, esc: usize) -> Result<Vec<u8>, CompileError> {
        if self.bump() != Some(b'{') {
            return Err(self.err(
                CompileErrorKind::MalformedEscape,
                esc,
                self.pos,
                "missing '{'",
            ));
        }
        let mut value = 0u32;
        let mut any = false;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.err(
                    CompileErrorKind::MalformedEscape,
                    esc,
                    self.pos,
                    "missing '}'",
                ));
            };
            if byte == b'}' {
                if !any {
                    return Err(self.err(
                        CompileErrorKind::MalformedEscape,
                        esc,
                        self.pos,
                        "hexadecimal digit expected",
                    ));
                }
                self.bump();
                break;
            }
            let Some(digit) = hex_val(byte) else {
                return Err(self
                    .err(
                        CompileErrorKind::MalformedEscape,
                        esc,
                        self.pos,
                        "hexadecimal digit expected",
                    )
                    .with_near(quote_near(
                        &self.source[self.string_start.unwrap_or(esc)
                            ..(self.pos + 1).min(self.source.len())],
                    )));
            };
            self.bump();
            any = true;
            if value > (0x7fff_ffff >> 4) {
                return Err(self.err(
                    CompileErrorKind::MalformedEscape,
                    esc,
                    self.pos,
                    "UTF-8 value too large",
                ));
            }
            value = (value << 4) | u32::from(digit);
        }
        Ok(crate::utf8::encode(value))
    }

    fn read_numeral(&mut self, start: usize) -> Result<Token, CompileError> {
        let is_hex = self.source.get(start) == Some(&b'0')
            && matches!(self.source.get(start + 1), Some(b'x' | b'X'));
        let mut cursor = if is_hex { start + 2 } else { start };
        while cursor < self.source.len() {
            let byte = self.source[cursor];
            let exponent = if is_hex {
                matches!(byte, b'p' | b'P')
            } else {
                matches!(byte, b'e' | b'E')
            };
            if exponent {
                cursor += 1;
                if matches!(self.source.get(cursor), Some(b'+' | b'-')) {
                    cursor += 1;
                }
                continue;
            }
            let digit = if is_hex {
                hex_val(byte).is_some()
            } else {
                byte.is_ascii_digit()
            };
            if digit || byte == b'.' {
                cursor += 1;
                continue;
            }
            break;
        }
        if self.source.get(cursor).copied().is_some_and(is_lua_alpha) {
            cursor += 1;
        }
        self.pos = cursor;
        let text = &self.source[start..cursor];
        match classify_numeral(text) {
            Some(kind) => Ok(self.token(kind, start)),
            None => Err(self.err(
                CompileErrorKind::MalformedNumber,
                start,
                cursor,
                "malformed number",
            )),
        }
    }

    fn name(&mut self, start: usize) -> Token {
        self.bump();
        while self.peek().is_some_and(is_lua_alnum) {
            self.bump();
        }
        let kind = keyword(&self.source[start..self.pos]).unwrap_or(TokenKind::Name);
        self.token(kind, start)
    }

    fn push_bytes(
        &mut self,
        out: &mut Vec<u8>,
        bytes: Vec<u8>,
        start: usize,
    ) -> Result<(), CompileError> {
        if out.len().saturating_add(bytes.len()) > self.max_source_bytes {
            return Err(self.err(
                CompileErrorKind::Limit,
                start,
                self.pos,
                format!("literal exceeds {} bytes", self.max_source_bytes),
            ));
        }
        out.extend(bytes);
        Ok(())
    }

    fn one(&mut self, kind: TokenKind, start: usize) -> Token {
        self.bump();
        self.token(kind, start)
    }

    fn either(&mut self, start: usize, second: u8, yes: TokenKind, no: TokenKind) -> Token {
        self.bump();
        if self.peek() == Some(second) {
            self.bump();
            self.token(yes, start)
        } else {
            self.token(no, start)
        }
    }

    fn either2(
        &mut self,
        start: usize,
        a: u8,
        a_kind: TokenKind,
        b: u8,
        b_kind: TokenKind,
        no: TokenKind,
    ) -> Token {
        self.bump();
        if self.peek() == Some(a) {
            self.bump();
            self.token(a_kind, start)
        } else if self.peek() == Some(b) {
            self.bump();
            self.token(b_kind, start)
        } else {
            self.token(no, start)
        }
    }

    fn token(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            span: Span::new(pos_u32(start), pos_u32(self.pos)),
        }
    }

    fn err(
        &self,
        kind: CompileErrorKind,
        start: usize,
        end: usize,
        message: impl Into<String>,
    ) -> CompileError {
        let near = if kind == CompileErrorKind::UnexpectedCharacter {
            match self.source.get(start).copied() {
                Some(0) | None => None,
                Some(byte) if !(32..127).contains(&byte) => {
                    Some(format!("'<\\{byte}>'").into_bytes())
                }
                Some(_) => Some(quote_near(&self.source[start..end])),
            }
        } else if matches!(
            kind,
            CompileErrorKind::UnfinishedString
                | CompileErrorKind::UnfinishedLongString
                | CompileErrorKind::UnfinishedLongComment
        ) {
            Some(b"<eof>".to_vec())
        } else if matches!(
            kind,
            CompileErrorKind::MalformedNumber
                | CompileErrorKind::MalformedEscape
                | CompileErrorKind::InvalidLongStringDelimiter
                | CompileErrorKind::UnexpectedCharacter
        ) {
            let from = if kind == CompileErrorKind::MalformedEscape {
                self.string_start.unwrap_or(start)
            } else {
                start
            };
            Some(quote_near(&self.source[from..end.min(self.source.len())]))
        } else {
            None
        };
        let error = CompileError::new(kind, Span::new(pos_u32(start), pos_u32(end)), message);
        if let Some(near) = near {
            error.with_near(near)
        } else {
            error
        }
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.source.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek(), Some(b'\n' | b'\r'))
    }

    fn consume_newline(&mut self) {
        let Some(first) = self.bump() else {
            return;
        };
        if let Some(second) = self.peek() {
            let pair = (first == b'\n' && second == b'\r') || (first == b'\r' && second == b'\n');
            if pair {
                self.bump();
            }
        }
    }
}

enum Bracket {
    Long { level: u32 },
    Single,
    Broken { end: usize },
}

// Tests and the measure feature call this. The parser pulls tokens itself.
fn quote_near(source: &[u8]) -> Vec<u8> {
    let source = &source[..source
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(source.len())];
    let mut out = Vec::with_capacity(source.len().saturating_add(2));
    out.push(b'\'');
    out.extend_from_slice(source);
    out.push(b'\'');
    out
}

#[cfg(any(test, feature = "__measure"))]
pub(crate) fn tokenize(source: &[u8]) -> Result<Vec<Token>, CompileError> {
    let mut lexer = Lexer::new(source)?;
    let mut out = Vec::new();
    loop {
        let before = lexer.pos;
        let token = lexer.next_token()?;
        let done = matches!(token.kind, TokenKind::Eof);
        out.push(token);
        if done {
            break;
        }
        if lexer.pos <= before || out.len() > source.len().saturating_add(1) {
            return Err(CompileError::new(
                CompileErrorKind::Limit,
                Span::new(pos_u32(before), pos_u32(lexer.pos)),
                "lexer did not advance",
            ));
        }
    }
    Ok(out)
}

fn pos_u32(pos: usize) -> u32 {
    u32::try_from(pos).unwrap_or(u32::MAX)
}

fn is_lua_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_lua_alpha(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_lua_alnum(byte: u8) -> bool {
    is_lua_alpha(byte) || byte.is_ascii_digit()
}

fn keyword(text: &[u8]) -> Option<TokenKind> {
    Some(match text {
        b"and" => TokenKind::And,
        b"break" => TokenKind::Break,
        b"do" => TokenKind::Do,
        b"else" => TokenKind::Else,
        b"elseif" => TokenKind::Elseif,
        b"end" => TokenKind::End,
        b"false" => TokenKind::False,
        b"for" => TokenKind::For,
        b"function" => TokenKind::Function,
        b"goto" => TokenKind::Goto,
        b"if" => TokenKind::If,
        b"in" => TokenKind::In,
        b"local" => TokenKind::Local,
        b"nil" => TokenKind::Nil,
        b"not" => TokenKind::Not,
        b"or" => TokenKind::Or,
        b"repeat" => TokenKind::Repeat,
        b"return" => TokenKind::Return,
        b"then" => TokenKind::Then,
        b"true" => TokenKind::True,
        b"until" => TokenKind::Until,
        b"while" => TokenKind::While,
        _ => return None,
    })
}

/// A runtime string converted to a number, as Lua's `luaO_str2num` does
/// for `tonumber` and numeric `for`. PUC Lua's lexer converts numerals with
/// the same function, so the unsigned body goes through
/// [`classify_numeral`]. On top of that: surrounding Lua whitespace, one
/// optional sign, and no embedded bytes of any other kind. A negated decimal
/// `9223372036854775808` is the integer `math.mininteger`, not a float.
/// `inf` and `nan` are not numerals and are rejected.
pub(crate) fn string_to_number(bytes: &[u8]) -> Option<crate::value::Value> {
    use crate::value::Value;
    let start = bytes.iter().position(|byte| !is_lua_space(*byte))?;
    let end = bytes.iter().rposition(|byte| !is_lua_space(*byte))? + 1;
    let mut text = &bytes[start..end];
    let negative = match text.first() {
        Some(b'-') => {
            text = &text[1..];
            true
        }
        Some(b'+') => {
            text = &text[1..];
            false
        }
        _ => false,
    };
    Some(match classify_numeral(text)? {
        TokenKind::Integer(integer) => Value::Integer(if negative {
            integer.wrapping_neg()
        } else {
            integer
        }),
        TokenKind::Float(bits) => {
            let float = f64::from_bits(bits);
            let digits = text
                .iter()
                .position(|byte| *byte != b'0')
                .map(|at| &text[at..]);
            if negative && digits == Some(b"9223372036854775808".as_slice()) {
                return Some(Value::Integer(i64::MIN));
            }
            Value::Float(if negative { -float } else { float })
        }
        _ => return None,
    })
}

fn classify_numeral(text: &[u8]) -> Option<TokenKind> {
    if text.len() >= 2 && text[0] == b'0' && matches!(text[1], b'x' | b'X') {
        return classify_hex(&text[2..]);
    }
    classify_dec(text)
}

fn classify_dec(text: &[u8]) -> Option<TokenKind> {
    let mut saw_dot = false;
    let mut saw_exp = false;
    let mut exp_digits = false;
    let mut digits = 0usize;
    let mut index = 0;
    while index < text.len() {
        match text[index] {
            b'0'..=b'9' => {
                if saw_exp {
                    exp_digits = true;
                } else {
                    digits += 1;
                }
                index += 1;
            }
            b'.' => {
                if saw_dot || saw_exp {
                    return None;
                }
                saw_dot = true;
                index += 1;
            }
            b'e' | b'E' => {
                if saw_exp {
                    return None;
                }
                saw_exp = true;
                index += 1;
                if matches!(text.get(index), Some(b'+' | b'-')) {
                    index += 1;
                }
            }
            _ => return None,
        }
    }
    if digits == 0 || (saw_exp && !exp_digits) {
        return None;
    }
    if !saw_dot
        && !saw_exp
        && let Some(integer) = parse_i64_digits(text)
    {
        return Some(TokenKind::Integer(integer));
    }
    Some(TokenKind::Float(parse_f64_decimal(text)?))
}

fn classify_hex(body: &[u8]) -> Option<TokenKind> {
    let mut dotted = false;
    let mut digits = 0usize;
    let mut index = 0;
    while index < body.len() {
        let byte = body[index];
        if matches!(byte, b'p' | b'P') {
            break;
        }
        if byte == b'.' {
            if dotted {
                return None;
            }
            dotted = true;
            index += 1;
            continue;
        }
        hex_val(byte)?;
        digits += 1;
        index += 1;
    }
    if digits == 0 {
        return None;
    }
    if index < body.len() {
        let exp = parse_binary_exp(&body[index + 1..])?;
        return Some(TokenKind::Float(hex_float(&body[..index], exp)?));
    }
    if dotted {
        return Some(TokenKind::Float(hex_float(body, 0)?));
    }
    Some(TokenKind::Integer(hex_wrap(body)))
}

fn parse_i64_digits(text: &[u8]) -> Option<i64> {
    let mut acc = 0i64;
    if text.is_empty() {
        return None;
    }
    for &byte in text {
        let digit = i64::from(byte - b'0');
        acc = acc.checked_mul(10)?.checked_add(digit)?;
    }
    Some(acc)
}

fn parse_f64_decimal(text: &[u8]) -> Option<u64> {
    if text.len() > LONG_DECIMAL {
        return parse_f64_decimal(&shorten_decimal(text)?);
    }
    let text = std::str::from_utf8(text).ok()?;
    let normalized;
    let parsed = if text.as_bytes().contains(&b'.') {
        normalized = bare_dot_to_zero(text);
        normalized.parse::<f64>().ok()?
    } else {
        text.parse::<f64>().ok()?
    };
    Some(parsed.to_bits())
}

/// Decimal numerals longer than this are shortened before parsing: Rust's
/// parser gives infinity for some numerals of about a million digits that
/// C's `strtod`, which Lua uses, reads exactly.
const LONG_DECIMAL: usize = 1000;

/// A decimal numeral (digits, an optional `.`, an optional exponent) with
/// the same value to 800 significant digits: leading zeros dropped, a `1`
/// kept after the 800th digit when any later digit is not zero, so the
/// rounding is the same, and the rest folded into the exponent.
fn shorten_decimal(text: &[u8]) -> Option<Vec<u8>> {
    const KEEP: usize = 800;
    let (mantissa, exponent) = match text.iter().position(|b| matches!(b, b'e' | b'E')) {
        Some(at) => (&text[..at], &text[at + 1..]),
        None => (text, &b""[..]),
    };
    let mut exp: i64 = 0;
    if !exponent.is_empty() {
        let (negative, digits) = match exponent[0] {
            b'-' => (true, &exponent[1..]),
            b'+' => (false, &exponent[1..]),
            _ => (false, exponent),
        };
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        for &byte in digits {
            exp = exp
                .saturating_mul(10)
                .saturating_add(i64::from(byte - b'0'));
        }
        if negative {
            exp = -exp;
        }
    }
    let mut kept = Vec::with_capacity(KEEP + 1);
    let mut sticky = false;
    // The position of the decimal point among the significant digits.
    let mut point: i64 = 0;
    let mut dotted = false;
    for &byte in mantissa {
        if byte == b'.' {
            dotted = true;
            continue;
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        if kept.is_empty() && byte == b'0' {
            if dotted {
                point -= 1;
            }
            continue;
        }
        if !dotted {
            point += 1;
        }
        if kept.len() < KEEP {
            kept.push(byte);
        } else if byte != b'0' {
            sticky = true;
        }
    }
    if kept.is_empty() {
        return Some(b"0".to_vec());
    }
    if sticky {
        kept.push(b'1');
    }
    let exp = point
        .saturating_add(exp)
        .clamp(-1_000_000_000, 1_000_000_000);
    let mut out = b"0.".to_vec();
    out.extend_from_slice(&kept);
    out.extend_from_slice(format!("e{exp}").as_bytes());
    Some(out)
}

fn bare_dot_to_zero(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 1);
    for (index, &byte) in bytes.iter().enumerate() {
        out.push(byte as char);
        if byte == b'.' && !matches!(bytes.get(index + 1), Some(b'0'..=b'9')) {
            out.push('0');
        }
    }
    out
}

fn parse_binary_exp(tail: &[u8]) -> Option<i32> {
    if tail.is_empty() {
        return None;
    }
    let (sign, digits) = match tail[0] {
        b'+' => (1i32, &tail[1..]),
        b'-' => (-1, &tail[1..]),
        _ => (1, tail),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut acc = 0i32;
    for &byte in digits {
        match acc
            .checked_mul(10)
            .and_then(|value| value.checked_add(i32::from(byte - b'0')))
        {
            Some(value) => acc = value,
            None => return Some(sign.saturating_mul(20_000)),
        }
    }
    Some(sign.saturating_mul(acc))
}

fn hex_wrap(digits: &[u8]) -> i64 {
    let mut acc = 0u64;
    for &byte in digits {
        let Some(digit) = hex_val(byte) else {
            return 0;
        };
        acc = acc.wrapping_mul(16).wrapping_add(u64::from(digit));
    }
    acc as i64
}

fn hex_float(mantissa: &[u8], exp: i32) -> Option<u64> {
    // Lua 5.4's `lua_strx2number`: at most 30 significant digits go into
    // the value, each with its own rounding; leading zeros are skipped and
    // later digits only move the exponent, so a numeral of any length gives
    // Lua's value without overflowing on the way.
    const MAX_SIG_DIGITS: u32 = 30;
    let mut value = 0.0f64;
    let mut exponent = 0i64;
    let mut significant = 0u32;
    let mut zeros = 0u32;
    let mut dotted = false;
    for &byte in mantissa {
        if byte == b'.' {
            if dotted {
                return None;
            }
            dotted = true;
            continue;
        }
        let digit = hex_val(byte)?;
        if significant == 0 && digit == 0 {
            zeros = zeros.saturating_add(1);
        } else if significant < MAX_SIG_DIGITS {
            significant += 1;
            value = value * 16.0 + f64::from(digit);
        } else {
            exponent += 1;
        }
        if dotted {
            exponent -= 1;
        }
    }
    if significant == 0 && zeros == 0 {
        return None;
    }
    let exponent = (exponent * 4).saturating_add(i64::from(exp));
    let exponent = i32::try_from(exponent.clamp(-100_000, 100_000)).unwrap_or(0);
    Some(libm::scalbn(value, exponent).to_bits())
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::line_col;

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source.as_bytes())
            .unwrap_or_else(|error| panic!("{source:?} -> {error:?}"))
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    fn fail(source: &[u8]) -> CompileError {
        tokenize(source).expect_err("expected a lexical error")
    }

    fn string_of(source: &str) -> Vec<u8> {
        match &kinds(source)[..] {
            [TokenKind::Str(bytes), TokenKind::Eof] => bytes.clone(),
            other => panic!("{source:?} -> {other:?}"),
        }
    }

    #[test]
    fn keywords_are_not_names_and_case_matters() {
        assert_eq!(
            kinds("and break do else elseif end false for function goto if in"),
            vec![
                TokenKind::And,
                TokenKind::Break,
                TokenKind::Do,
                TokenKind::Else,
                TokenKind::Elseif,
                TokenKind::End,
                TokenKind::False,
                TokenKind::For,
                TokenKind::Function,
                TokenKind::Goto,
                TokenKind::If,
                TokenKind::In,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("local nil not or repeat return then true until while"),
            vec![
                TokenKind::Local,
                TokenKind::Nil,
                TokenKind::Not,
                TokenKind::Or,
                TokenKind::Repeat,
                TokenKind::Return,
                TokenKind::Then,
                TokenKind::True,
                TokenKind::Until,
                TokenKind::While,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("And AND _x"),
            vec![
                TokenKind::Name,
                TokenKind::Name,
                TokenKind::Name,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn punctuation_and_dot_boundaries() {
        assert_eq!(
            kinds("+ - * / % ^ # & ~ | << >> // == ~= <= >= < > = ( ) { } [ ] :: ; : , . .. ..."),
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::Caret,
                TokenKind::Hash,
                TokenKind::Amp,
                TokenKind::Tilde,
                TokenKind::Pipe,
                TokenKind::Shl,
                TokenKind::Shr,
                TokenKind::Idiv,
                TokenKind::EqEq,
                TokenKind::TildeEq,
                TokenKind::LtEq,
                TokenKind::GtEq,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::Eq,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::LBracket,
                TokenKind::RBracket,
                TokenKind::ColonColon,
                TokenKind::Semi,
                TokenKind::Colon,
                TokenKind::Comma,
                TokenKind::Dot,
                TokenKind::Concat,
                TokenKind::Dots,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("3. .. 4"),
            vec![
                TokenKind::Float(3.0f64.to_bits()),
                TokenKind::Concat,
                TokenKind::Integer(4),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("..5 ...5 .5"),
            vec![
                TokenKind::Concat,
                TokenKind::Integer(5),
                TokenKind::Dots,
                TokenKind::Integer(5),
                TokenKind::Float(0.5f64.to_bits()),
                TokenKind::Eof,
            ]
        );
        let glued = fail(b"3..4");
        assert_eq!(glued.kind, CompileErrorKind::MalformedNumber);
        assert_eq!(glued.span, Span::new(0, 4));
        assert_eq!(fail(b"3...4").kind, CompileErrorKind::MalformedNumber);
        assert_eq!(fail(b"1.2.3").kind, CompileErrorKind::MalformedNumber);
        assert_eq!(fail(b".5.1").kind, CompileErrorKind::MalformedNumber);
    }

    #[test]
    fn numerals_match_lua_5_4_values() {
        assert_eq!(
            kinds("0 00 3 345"),
            vec![
                TokenKind::Integer(0),
                TokenKind::Integer(0),
                TokenKind::Integer(3),
                TokenKind::Integer(345),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("0xff 0XFF 0xBEBADA 0x1e 0xe"),
            vec![
                TokenKind::Integer(255),
                TokenKind::Integer(255),
                TokenKind::Integer(0xBEBADA),
                TokenKind::Integer(30),
                TokenKind::Integer(14),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("9223372036854775807"),
            vec![TokenKind::Integer(i64::MAX), TokenKind::Eof]
        );
        assert_eq!(
            kinds("0x8000000000000000"),
            vec![TokenKind::Integer(i64::MIN), TokenKind::Eof]
        );
        assert_eq!(
            kinds("0xFFFFFFFFFFFFFFFF"),
            vec![TokenKind::Integer(-1), TokenKind::Eof]
        );
        assert_eq!(
            kinds("0x10000000000000001"),
            vec![TokenKind::Integer(1), TokenKind::Eof]
        );
        assert_eq!(
            kinds("9223372036854775808"),
            vec![TokenKind::Float(0x43e0_0000_0000_0000), TokenKind::Eof]
        );
        assert_eq!(
            kinds("9223372036854775809"),
            vec![TokenKind::Float(0x43e0_0000_0000_0000), TokenKind::Eof]
        );
        assert_eq!(
            kinds("1e+2 1E-1 12.e1 0e0"),
            vec![
                TokenKind::Float(100.0f64.to_bits()),
                TokenKind::Float(0.1f64.to_bits()),
                TokenKind::Float(120.0f64.to_bits()),
                TokenKind::Float(0.0f64.to_bits()),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("0x1.fp10 0x1.1 0x0.1E 0xep1 0x1.921FB54442D18P+1"),
            vec![
                TokenKind::Float(1984.0f64.to_bits()),
                TokenKind::Float(1.0625f64.to_bits()),
                TokenKind::Float(0x3fbe_0000_0000_0000),
                TokenKind::Float(28.0f64.to_bits()),
                TokenKind::Float(0x4009_21fb_5444_2d18),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("1e309"),
            vec![TokenKind::Float(f64::INFINITY.to_bits()), TokenKind::Eof]
        );
        assert_eq!(
            kinds("1e-400"),
            vec![TokenKind::Float(0.0f64.to_bits()), TokenKind::Eof]
        );
        for source in [
            "0x", "0x.", "0xp1", "1e+", "1e", "0x1p+", "1a", "1_", "0x1z", "0x1.fp",
        ] {
            assert_eq!(
                fail(source.as_bytes()).kind,
                CompileErrorKind::MalformedNumber,
                "{source}"
            );
        }
        // A numeral is bounded only by the source: a long one is read as
        // Lua reads it.
        let long = format!("return 0x{}a", "0".repeat(100_000));
        assert!(crate::compile(long.as_bytes()).is_ok());
    }

    #[test]
    fn short_and_long_strings() {
        assert_eq!(string_of(r#"'alo\n123"'"#), b"alo\n123\"");
        assert_eq!(string_of("'\\97lo\\10\\04923\"'"), b"alo\n123\"");
        assert_eq!(string_of("'\\0'"), vec![0]);
        assert_eq!(string_of("'\\255'"), vec![255]);
        assert_eq!(string_of("'\\x00'"), vec![0]);
        assert_eq!(string_of("'\\xFF'"), vec![255]);
        assert_eq!(string_of("'\\8'"), vec![8]);
        assert_eq!(string_of("'\\49'"), b"1");
        assert_eq!(string_of("'\\0490'"), b"10");
        assert_eq!(string_of("'\\u{41}'"), b"A");
        assert_eq!(string_of("'\\u{0}'"), vec![0]);
        assert_eq!(string_of("'\\u{D800}'"), vec![0xed, 0xa0, 0x80]);
        assert_eq!(string_of("'\\u{110000}'"), vec![0xf4, 0x90, 0x80, 0x80]);
        assert_eq!(
            string_of("'\\u{7FFFFFFF}'"),
            vec![0xfd, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf]
        );
        assert_eq!(fail(b"'\\256'").kind, CompileErrorKind::MalformedEscape);
        assert_eq!(fail(br"'\x0'").kind, CompileErrorKind::MalformedEscape);
        assert_eq!(fail(br"'\u{}'").kind, CompileErrorKind::MalformedEscape);
        assert_eq!(
            fail(br"'\u{80000000}'").kind,
            CompileErrorKind::MalformedEscape
        );
        assert_eq!(fail(br"'\q'").kind, CompileErrorKind::MalformedEscape);
        assert_eq!(fail(b"'\\256'").span, Span::new(1, 5));
        assert_eq!(string_of("[=[alo\n123\"]=]"), b"alo\n123\"");
        assert_eq!(string_of("[[\nhello]]"), b"hello");
        assert_eq!(string_of("[[\n\nhello]]"), b"\nhello");
        assert_eq!(string_of("[[a\r\nb]]"), b"a\nb");
        assert_eq!(string_of("[[a\n\rb]]"), b"a\nb");
        assert_eq!(string_of("[[a\rb]]"), b"a\nb");
        assert_eq!(string_of("[=[]]=]"), b"]");
        assert_eq!(string_of("[==[a]==]"), b"a");
        assert_eq!(
            tokenize(b"\"\xff\"").unwrap()[0].kind,
            TokenKind::Str(vec![0xff])
        );
        assert_eq!(
            tokenize(b"[[\xff]]").unwrap()[0].kind,
            TokenKind::Str(vec![0xff])
        );
        assert_eq!(
            fail(b"[[hello").kind,
            CompileErrorKind::UnfinishedLongString
        );
        assert_eq!(fail(b"'abc").kind, CompileErrorKind::UnfinishedString);
        assert_eq!(fail(b"'ab\n'").kind, CompileErrorKind::UnfinishedString);
        assert_eq!(
            fail(b"[==").kind,
            CompileErrorKind::InvalidLongStringDelimiter
        );
        assert_eq!(
            fail(b"[=a").kind,
            CompileErrorKind::InvalidLongStringDelimiter
        );
        assert_eq!(fail(b"[==").span, Span::new(0, 3));
    }

    #[test]
    fn escapes_that_cross_newlines_and_comments() {
        assert_eq!(
            tokenize(b"'a\\\nb'").unwrap()[0].kind,
            TokenKind::Str(b"a\nb".to_vec())
        );
        assert_eq!(
            tokenize(b"'a\\\r\nb'").unwrap()[0].kind,
            TokenKind::Str(b"a\nb".to_vec())
        );
        assert_eq!(
            tokenize(b"'a\\\rb'").unwrap()[0].kind,
            TokenKind::Str(b"a\nb".to_vec())
        );
        assert_eq!(
            tokenize(b"'a\\z  \n  b'").unwrap()[0].kind,
            TokenKind::Str(b"ab".to_vec())
        );
        assert_eq!(
            tokenize(b"'a\\z\x0b\x0c b'").unwrap()[0].kind,
            TokenKind::Str(b"ab".to_vec())
        );
        assert_eq!(
            kinds("-- comment\nreturn"),
            vec![TokenKind::Return, TokenKind::Eof]
        );
        assert_eq!(
            kinds("--[=[ still\n a comment ]=] return"),
            vec![TokenKind::Return, TokenKind::Eof]
        );
        assert_eq!(
            kinds("--[==\nreturn 1"),
            vec![TokenKind::Return, TokenKind::Integer(1), TokenKind::Eof]
        );
        assert_eq!(
            kinds("--[abc\n1"),
            vec![TokenKind::Integer(1), TokenKind::Eof]
        );
        assert_eq!(
            fail(b"--[=[ hello").kind,
            CompileErrorKind::UnfinishedLongComment
        );
        assert_eq!(
            kinds("--[==[a]==]2"),
            vec![TokenKind::Integer(2), TokenKind::Eof]
        );
        // A long comment does not close on a different level.
        assert_eq!(
            kinds("--[=[ ]==] ]=]1"),
            vec![TokenKind::Integer(1), TokenKind::Eof]
        );
    }

    #[test]
    fn line_breaks_and_hash_are_bytes() {
        let source = b"a\r\nb\n\rc\rd";
        assert_eq!(line_col(source, 0), (1, 1));
        assert_eq!(line_col(source, 1), (1, 2));
        assert_eq!(line_col(source, 3), (2, 1));
        assert_eq!(line_col(source, 6), (3, 1));
        assert!(line_col(source, 8).0 >= 4);
        // `#` is the length operator. The lua CLI's shebang strip is not lexical.
        assert_eq!(
            kinds("#comment"),
            vec![TokenKind::Hash, TokenKind::Name, TokenKind::Eof]
        );
        let tokens = tokenize(b"@").unwrap_err();
        assert_eq!(tokens.kind, CompileErrorKind::UnexpectedCharacter);
        assert_eq!(tokens.span, Span::new(0, 1));
    }

    #[test]
    fn runtime_strings_convert_like_lua() {
        use crate::value::Value::{Float as F, Integer as I};
        let cases: &[(&[u8], Option<crate::value::Value>)] = &[
            (b"3", Some(I(3))),
            (b" -0x10 ", Some(I(-16))),
            (b"+5", Some(I(5))),
            (b"- 5", None),
            (b"1e", None),
            (b"0x", None),
            (b"inf", None),
            (b"nan", None),
            (b"1 2", None),
            (b"1\0", None),
            (b"", None),
            (b"  ", None),
            (b".5", Some(F(0.5))),
            (b"5.", Some(F(5.0))),
            (b"0x1p4", Some(F(16.0))),
            (b"-0x8000000000000000", Some(I(i64::MIN))),
            (b"-9223372036854775808", Some(I(i64::MIN))),
            (b"-09223372036854775808", Some(I(i64::MIN))),
            (b"9223372036854775808", Some(F(9_223_372_036_854_775_808.0))),
            (b"99999999999999999999", Some(F(1e20))),
            (b"\t2.5\n", Some(F(2.5))),
        ];
        for (text, want) in cases {
            assert_eq!(
                string_to_number(text),
                *want,
                "{:?}",
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn malformed_bytes_do_not_panic() {
        let mut state = 0x1234_5678_u64;
        let mut step = |buf: &mut [u8]| {
            for byte in buf.iter_mut() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                *byte = (state >> 33) as u8;
            }
        };
        for size in [0usize, 1, 7, 64, 200] {
            let mut bytes = vec![0u8; size];
            step(&mut bytes);
            match tokenize(&bytes) {
                Ok(tokens) => {
                    assert!(matches!(tokens.last().unwrap().kind, TokenKind::Eof));
                    for token in tokens {
                        assert!(token.span.start <= token.span.end);
                        assert!(token.span.end as usize <= bytes.len());
                    }
                }
                Err(error) => {
                    assert!(error.span.start <= error.span.end);
                    assert!(error.span.end as usize <= bytes.len() || error.span.end == 0);
                }
            }
        }
        for source in [
            b"[[".as_slice(),
            b"[=[",
            b"--[=[",
            b"'\\",
            b"\\u{",
            b"0x",
            b"1e+",
            b"'",
            b"\"",
            b"[[\r\n",
            b"\xff\x00'\x00",
            &[0xff; 300][..],
        ] {
            let _ = tokenize(source);
        }
        let huge = vec![b' '; DEFAULT_SOURCE_BYTES + 1];
        assert_eq!(tokenize(&huge).unwrap_err().kind, CompileErrorKind::Limit);
    }
}
