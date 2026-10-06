//! Lua 5.4.9's `string.format` (`str_format` in `lstrlib.c`) as a pure,
//! resumable formatter.
//!
//! The output is the bytes PUC Lua produces on glibc x86-64 in the C locale,
//! computed without the C library: integers by hand, decimal float digits
//! from Rust's correctly rounded `core::fmt` (round half to even on the exact
//! binary value, as glibc does), `%a` by hand with glibc's half-even rule on
//! hex digits. One deliberate difference: every NaN prints as `nan` (`NAN`),
//! whatever its sign bit, because the sign of a NaN differs across targets.
//!
//! The VM drives a [`Formatter`]: [`Formatter::next`] copies literal text and
//! stops at each item, saying which argument it needs and in what form; the
//! VM converts the argument (possibly calling `__tostring`) and hands it back
//! through one of the `give_*` methods. What the caller must apply first:
//!
//! - [`Need::Integer`] (`%c %d %i %u %o %x %X`): `luaL_checkinteger`.
//! - [`Need::Number`] (`%a %A %e %E %f %g %G`): `luaL_checknumber`.
//! - [`Need::String`] (`%s`): `luaL_tolstring` (may call `__tostring`/`__name`).
//! - [`Need::Pointer`] (`%p`): `lua_topointer`; Moonseed passes a token
//!   text instead of an address, `None` for values without one.
//! - [`Need::Literal`] (`%q`): no conversion; the raw value as a [`Literal`].
//!
//! The caller raises argument-type errors itself. For `%c` and `%a`/`%A`, C
//! validates the item before converting the argument, so `next` has already
//! done it; for the other conversions the `give_*` method validates after.
//!
//! Argument numbers (`arg` fields) are C's: the format string is argument 1,
//! the first value argument 2.

use std::fmt::Write as _;

/// `MAX_FORMAT`: an item whose flags, digits and conversion span 22 bytes or
/// more is "invalid format (too long)".
const MAX_FORMAT: usize = 32;
/// `L_FMTFLAGSF`: flags of `a A e E f g G`.
const FLAGS_F: &[u8] = b"-+#0 ";
/// `L_FMTFLAGSX`: flags of `o x X`.
const FLAGS_X: &[u8] = b"-#0";
/// `L_FMTFLAGSI`: flags of `d i`.
const FLAGS_I: &[u8] = b"-+0 ";
/// `L_FMTFLAGSU`: flags of `u`.
const FLAGS_U: &[u8] = b"-0";
/// `L_FMTFLAGSC`: flags of `c p s`.
const FLAGS_C: &[u8] = b"-";
/// Largest run of literal text copied in one budgeted step.
const CHUNK: usize = 4096;
/// Words written by [`Formatter::encode`].
const STATE_WORDS: usize = 6;

/// Why a format failed. [`Formatter::message`] gives PUC's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormatError {
    /// `luaL_argerror(arg, "no value")`: more items than arguments.
    NoValue { arg: u32 },
    /// `"invalid format (too long)"`.
    InvalidFormat,
    /// `"invalid conversion '<form>' to 'format'"`: an unknown conversion
    /// (`%n`, `%F`, `%l`...), or a `%` at the end of the format.
    InvalidConversion,
    /// `"invalid conversion specification: '<form>'"`: a flag the
    /// conversion does not accept, a width or precision over two digits, a
    /// precision where none is allowed.
    InvalidSpecification,
    /// `"specifier '%q' cannot have modifiers"`.
    QModifiers,
    /// `luaL_argerror(arg, "value has no literal form")`.
    NoLiteral { arg: u32 },
    /// `luaL_argerror(arg, "string contains zeros")`.
    StringContainsZeros { arg: u32 },
    /// Not a Lua error: the formatter was called out of turn (a `give_*`
    /// that does not match the pending item, a call after an error, or a
    /// format string that does not match a decoded state).
    Misuse,
}

/// The form in which the current item wants its argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    Integer,
    Number,
    String,
    Pointer,
    Literal,
}

/// What [`Formatter::next`] reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The item needs argument `arg`; answer with the matching `give_*`.
    Need {
        arg: u32,
        need: Need,
    },
    /// The budget ran out; call `next` again.
    Pending,
    /// The whole format has been written.
    Done,
    Error(FormatError),
}

/// A `%q` argument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Literal<'a> {
    String(&'a [u8]),
    Integer(i64),
    Float(f64),
    Nil,
    Bool(bool),
    /// Any other type: "value has no literal form".
    Other,
}

/// One parsed conversion specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    pub(crate) minus: bool,
    pub(crate) plus: bool,
    pub(crate) space: bool,
    pub(crate) hash: bool,
    pub(crate) zero: bool,
    pub(crate) width: u8,
    pub(crate) precision: Option<u8>,
    pub(crate) conversion: u8,
}

impl Spec {
    /// A specification with no flags, width or precision.
    pub(crate) const fn new(conversion: u8) -> Self {
        Spec {
            minus: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: 0,
            precision: None,
            conversion,
        }
    }

    /// Parses the text after `%` of an item that passed `checkformat`.
    fn parse(item: &[u8]) -> Self {
        let mut spec = Spec::new(item.last().copied().unwrap_or(b's'));
        let mut i = 0;
        while let Some(&byte) = item.get(i) {
            match byte {
                b'-' => spec.minus = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.hash = true,
                b'0' => spec.zero = true,
                _ => break,
            }
            i += 1;
        }
        let (width, next) = two_digits(item, i);
        spec.width = width;
        i = next;
        if item.get(i) == Some(&b'.') {
            let (precision, _) = two_digits(item, i + 1);
            spec.precision = Some(precision);
        }
        spec
    }
}

/// The value of at most two decimal digits at `i`, and the index after them.
fn two_digits(item: &[u8], mut i: usize) -> (u8, usize) {
    let mut value = 0u8;
    for _ in 0..2 {
        match item.get(i) {
            Some(&digit) if digit.is_ascii_digit() => {
                value = value * 10 + (digit - b'0');
                i += 1;
            }
            _ => break,
        }
    }
    (value, i)
}

/// `checkformat`: `item` is the text after `%`, ending with the conversion.
fn check_format(item: &[u8], flags: &[u8], precision: bool) -> bool {
    let mut i = 0;
    while item.get(i).is_some_and(|byte| flags.contains(byte)) {
        i += 1;
    }
    if item.get(i) != Some(&b'0') {
        i = two_digits(item, i).1;
        if precision && item.get(i) == Some(&b'.') {
            i = two_digits(item, i + 1).1;
        }
    }
    item.get(i).is_some_and(|byte| byte.is_ascii_alphabetic())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Copying literal text.
    Text,
    /// Waiting for the argument of the item at `item_start`.
    Item,
    Done,
    Failed,
}

/// The progress of one `string.format` call. Plain data; see
/// [`Formatter::encode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Formatter {
    nargs: u32,
    /// Next byte of the format to read.
    pos: u32,
    /// C's `arg`: the last argument used (1 = the format string).
    arg: u32,
    phase: Phase,
    /// The last item: its text after `%` starts here...
    item_start: u32,
    /// ...and spans this many bytes, conversion included.
    item_len: u32,
}

impl Formatter {
    /// `nargs`: arguments after the format string (C's `top - 1`).
    pub(crate) fn new(nargs: u32) -> Self {
        Formatter {
            nargs,
            pos: 0,
            arg: 1,
            phase: Phase::Text,
            item_start: 0,
            item_len: 0,
        }
    }

    /// Copies literal text and `%%` into `out`, then parses the next item
    /// and returns what it needs, or `Done`, `Error`, or `Pending` when the
    /// budget ran out. A literal run costs `1 + len / 64` (runs are cut at
    /// every `%` and every 4096 bytes), `%%` and an item cost 1. Called
    /// again while an item waits, it returns the same `Need`.
    pub(crate) fn next(&mut self, fmt: &[u8], out: &mut Vec<u8>, budget: &mut i64) -> Step {
        match self.phase {
            Phase::Text => {}
            Phase::Item => {
                return match self
                    .item(fmt)
                    .and_then(|item| need_of(item.last().copied()))
                {
                    Some(need) => Step::Need {
                        arg: self.arg,
                        need,
                    },
                    None => self.fail(FormatError::Misuse),
                };
            }
            Phase::Done => return Step::Done,
            Phase::Failed => return Step::Error(FormatError::Misuse),
        }
        loop {
            let pos = self.pos as usize;
            let Some(&byte) = fmt.get(pos) else {
                self.phase = Phase::Done;
                return Step::Done;
            };
            if *budget <= 0 {
                return Step::Pending;
            }
            if byte != b'%' {
                let limit = fmt.len().min(pos.saturating_add(CHUNK));
                let run = fmt.get(pos..limit).unwrap_or_default();
                let len = run.iter().position(|&b| b == b'%').unwrap_or(run.len());
                out.extend_from_slice(run.get(..len).unwrap_or_default());
                *budget -= 1 + (len / 64) as i64;
                self.pos = to_u32(pos + len);
                continue;
            }
            *budget -= 1;
            let start = pos + 1;
            // C reads the terminating zero after a final `%`.
            if fmt.get(start) == Some(&b'%') {
                out.push(b'%');
                self.pos = to_u32(start + 1);
                continue;
            }
            return self.begin_item(fmt, start);
        }
    }

    /// The `no value` check, `getformat`, and the checks C runs before the
    /// argument is converted.
    fn begin_item(&mut self, fmt: &[u8], start: usize) -> Step {
        self.arg = self.arg.saturating_add(1);
        if u64::from(self.arg) > u64::from(self.nargs) + 1 {
            return self.fail(FormatError::NoValue { arg: self.arg });
        }
        // getformat: flags, digits and '.', then one more byte (the
        // conversion, or C's terminating zero at the end).
        let span = fmt
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| b"-+#0 123456789.".contains(b))
            .count();
        let len = span + 1;
        if len >= MAX_FORMAT - 10 {
            return self.fail(FormatError::InvalidFormat);
        }
        self.item_start = to_u32(start);
        self.item_len = to_u32(len);
        self.pos = to_u32((start + len).min(fmt.len()));
        let item = fmt.get(start..start + len).unwrap_or_default();
        let conversion = fmt.get(start + span).copied().unwrap_or(0);
        let ok = match conversion {
            b'c' => check_format(item, FLAGS_C, false),
            b'a' | b'A' => check_format(item, FLAGS_F, true),
            b'q' if len != 1 => return self.fail(FormatError::QModifiers),
            _ => true,
        };
        if !ok {
            return self.fail(FormatError::InvalidSpecification);
        }
        match need_of(Some(conversion)) {
            Some(need) => {
                self.phase = Phase::Item;
                Step::Need {
                    arg: self.arg,
                    need,
                }
            }
            None => self.fail(FormatError::InvalidConversion),
        }
    }

    fn fail(&mut self, error: FormatError) -> Step {
        self.phase = Phase::Failed;
        Step::Error(error)
    }

    /// The pending item's text after `%`.
    fn item<'f>(&self, fmt: &'f [u8]) -> Option<&'f [u8]> {
        let start = self.item_start as usize;
        fmt.get(start..start.checked_add(self.item_len as usize)?)
    }

    /// The pending item and its conversion, if the phase and `fmt` allow
    /// `conversions`.
    fn pending<'f>(
        &self,
        fmt: &'f [u8],
        conversions: &[u8],
    ) -> Result<(&'f [u8], u8), FormatError> {
        if self.phase != Phase::Item {
            return Err(FormatError::Misuse);
        }
        let item = self.item(fmt).ok_or(FormatError::Misuse)?;
        match item.last() {
            Some(&conversion) if conversions.contains(&conversion) => Ok((item, conversion)),
            _ => Err(FormatError::Misuse),
        }
    }

    fn finish(&mut self, result: Result<(), FormatError>) -> Result<(), FormatError> {
        self.phase = if result.is_ok() {
            Phase::Text
        } else {
            Phase::Failed
        };
        result
    }

    /// The argument of `%c %d %i %u %o %x %X`, after `luaL_checkinteger`.
    pub(crate) fn give_integer(
        &mut self,
        fmt: &[u8],
        n: i64,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let result = self
            .pending(fmt, b"cdiuoxX")
            .and_then(|(item, conversion)| {
                let (flags, precision) = match conversion {
                    b'c' => (FLAGS_C, false),
                    b'd' | b'i' => (FLAGS_I, true),
                    b'u' => (FLAGS_U, true),
                    _ => (FLAGS_X, true),
                };
                if !check_format(item, flags, precision) {
                    return Err(FormatError::InvalidSpecification);
                }
                format_integer(&Spec::parse(item), n, out);
                Ok(())
            });
        self.finish(result)
    }

    /// The argument of `%a %A %e %E %f %g %G`, after `luaL_checknumber`.
    pub(crate) fn give_number(
        &mut self,
        fmt: &[u8],
        x: f64,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let result = self.pending(fmt, b"aAeEfgG").and_then(|(item, _)| {
            if !check_format(item, FLAGS_F, true) {
                return Err(FormatError::InvalidSpecification);
            }
            format_float(&Spec::parse(item), x, out);
            Ok(())
        });
        self.finish(result)
    }

    /// The argument of `%s`, after `luaL_tolstring`.
    pub(crate) fn give_string(
        &mut self,
        fmt: &[u8],
        s: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let arg = self.arg;
        let result = self.pending(fmt, b"s").and_then(|(item, _)| {
            if item.len() == 1 {
                out.extend_from_slice(s);
                return Ok(());
            }
            if s.contains(&0) {
                return Err(FormatError::StringContainsZeros { arg });
            }
            if !check_format(item, FLAGS_C, true) {
                return Err(FormatError::InvalidSpecification);
            }
            if !item.contains(&b'.') && s.len() >= 100 {
                out.extend_from_slice(s);
            } else {
                format_text(&Spec::parse(item), s, out);
            }
            Ok(())
        });
        self.finish(result)
    }

    /// The argument of `%p`: the token Moonseed prints for the value, or
    /// `None` where `lua_topointer` gives `NULL` (printed `(null)`).
    pub(crate) fn give_pointer(
        &mut self,
        fmt: &[u8],
        token: Option<&[u8]>,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let result = self.pending(fmt, b"p").and_then(|(item, _)| {
            if !check_format(item, FLAGS_C, false) {
                return Err(FormatError::InvalidSpecification);
            }
            format_text(&Spec::parse(item), token.unwrap_or(b"(null)"), out);
            Ok(())
        });
        self.finish(result)
    }

    /// The argument of `%q` (`addliteral`).
    pub(crate) fn give_literal(
        &mut self,
        fmt: &[u8],
        value: Literal<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let arg = self.arg;
        let result = self.pending(fmt, b"q").and_then(|(item, _)| {
            if item.len() != 1 {
                return Err(FormatError::QModifiers);
            }
            match value {
                Literal::String(s) => add_quoted(s, out),
                Literal::Integer(i64::MIN) => out.extend_from_slice(b"0x8000000000000000"),
                Literal::Integer(n) => format_integer(&Spec::new(b'd'), n, out),
                Literal::Float(x) => quote_float(x, out),
                Literal::Nil => out.extend_from_slice(b"nil"),
                Literal::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
                Literal::Other => return Err(FormatError::NoLiteral { arg }),
            }
            Ok(())
        });
        self.finish(result)
    }

    /// C's `form` for the last item: `%`, then the item's text up to its
    /// first zero byte (C prints it as a C string).
    pub(crate) fn form(&self, fmt: &[u8]) -> Vec<u8> {
        let start = (self.item_start as usize).min(fmt.len());
        let end = start.saturating_add(self.item_len as usize).min(fmt.len());
        let text = fmt.get(start..end).unwrap_or_default();
        let text = text.split(|&b| b == 0).next().unwrap_or_default();
        let mut form = vec![b'%'];
        form.extend_from_slice(text);
        form
    }

    /// PUC's message for `error` raised by this formatter on `fmt`. For the
    /// argument errors it is `luaL_argerror`'s extra message ("no value"...),
    /// which the caller wraps as `bad argument #<arg> to 'format' (...)`.
    pub(crate) fn message(&self, fmt: &[u8], error: FormatError) -> Vec<u8> {
        let quoted = |before: &[u8], after: &[u8]| {
            let mut text = before.to_vec();
            text.extend_from_slice(&self.form(fmt));
            text.extend_from_slice(after);
            text
        };
        match error {
            FormatError::NoValue { .. } => b"no value".to_vec(),
            FormatError::InvalidFormat => b"invalid format (too long)".to_vec(),
            FormatError::InvalidConversion => quoted(b"invalid conversion '", b"' to 'format'"),
            FormatError::InvalidSpecification => {
                quoted(b"invalid conversion specification: '", b"'")
            }
            FormatError::QModifiers => b"specifier '%q' cannot have modifiers".to_vec(),
            FormatError::NoLiteral { .. } => b"value has no literal form".to_vec(),
            FormatError::StringContainsZeros { .. } => b"string contains zeros".to_vec(),
            FormatError::Misuse => b"string.format called out of turn".to_vec(),
        }
    }

    /// Appends the state as 6 words.
    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        let phase = match self.phase {
            Phase::Text => 0,
            Phase::Item => 1,
            Phase::Done => 2,
            Phase::Failed => 3,
        };
        out.extend_from_slice(&[
            phase,
            u64::from(self.nargs),
            u64::from(self.pos),
            u64::from(self.arg),
            u64::from(self.item_start),
            u64::from(self.item_len),
        ]);
    }

    /// Reads a state written by [`Formatter::encode`] for a format of
    /// `fmt_len` bytes and `nargs` arguments; `None` for words no run
    /// could have produced.
    pub(crate) fn decode(words: &mut &[u64], fmt_len: u32, nargs: u32) -> Option<Self> {
        let (head, rest) = words.split_first_chunk::<STATE_WORDS>()?;
        let [phase, stored_nargs, pos, arg, item_start, item_len] = head.map(u32::try_from);
        let (pos, arg, item_start, item_len) =
            (pos.ok()?, arg.ok()?, item_start.ok()?, item_len.ok()?);
        let phase = match phase.ok()? {
            0 => Phase::Text,
            1 => Phase::Item,
            2 => Phase::Done,
            3 => Phase::Failed,
            _ => return None,
        };
        let item_end = u64::from(item_start) + u64::from(item_len);
        let valid = stored_nargs.ok()? == nargs
            && pos <= fmt_len
            && arg >= 1
            // "no value" leaves `arg` one past the last argument.
            && u64::from(arg) <= u64::from(nargs) + 1 + u64::from(phase == Phase::Failed)
            && item_start <= fmt_len
            && (item_len as usize) < MAX_FORMAT - 10
            && match phase {
                Phase::Text => true,
                Phase::Item => {
                    item_start >= 1 && item_len >= 1 && arg >= 2 && item_end == u64::from(pos)
                }
                Phase::Done => pos == fmt_len,
                Phase::Failed => true,
            };
        if !valid {
            return None;
        }
        *words = rest;
        Some(Formatter {
            nargs,
            pos,
            arg,
            phase,
            item_start,
            item_len,
        })
    }
}

fn need_of(conversion: Option<u8>) -> Option<Need> {
    Some(match conversion? {
        b'c' | b'd' | b'i' | b'u' | b'o' | b'x' | b'X' => Need::Integer,
        b'a' | b'A' | b'e' | b'E' | b'f' | b'g' | b'G' => Need::Number,
        b's' => Need::String,
        b'p' => Need::Pointer,
        b'q' => Need::Literal,
        _ => return None,
    })
}

/// Formats never exceed 1 MiB positions, so this never saturates in use.
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Writes `prefix` and `body` padded to the width: spaces on the left,
/// spaces on the right with `-`, or zeros between them with `0` when
/// `zero_pad` allows it (glibc's `pad` placement).
fn pad(spec: &Spec, prefix: &[u8], body: &[u8], zero_pad: bool, out: &mut Vec<u8>) {
    let fill = usize::from(spec.width).saturating_sub(prefix.len() + body.len());
    if spec.minus {
        out.extend_from_slice(prefix);
        out.extend_from_slice(body);
        out.resize(out.len() + fill, b' ');
    } else if zero_pad && spec.zero {
        out.extend_from_slice(prefix);
        out.resize(out.len() + fill, b'0');
        out.extend_from_slice(body);
    } else {
        out.resize(out.len() + fill, b' ');
        out.extend_from_slice(prefix);
        out.extend_from_slice(body);
    }
}

/// `%s` with `-`, width and precision (the precision cuts the text).
fn format_text(spec: &Spec, s: &[u8], out: &mut Vec<u8>) {
    let len = spec
        .precision
        .map_or(s.len(), |p| s.len().min(usize::from(p)));
    pad(spec, b"", s.get(..len).unwrap_or(s), false, out);
}

/// glibc's `%c %d %i %u %o %x %X` for `n` (C's `%lld` family; `%c` takes
/// the low byte of the `(int)` cast). Other conversions print as `%d`.
pub(crate) fn format_integer(spec: &Spec, n: i64, out: &mut Vec<u8>) {
    if spec.conversion == b'c' {
        pad(spec, b"", &[n as u8], false, out);
        return;
    }
    let (negative, magnitude, base, digits): (bool, u64, u64, &[u8; 16]) = match spec.conversion {
        b'u' => (false, n as u64, 10, b"0123456789abcdef"),
        b'o' => (false, n as u64, 8, b"0123456789abcdef"),
        b'x' => (false, n as u64, 16, b"0123456789abcdef"),
        b'X' => (false, n as u64, 16, b"0123456789ABCDEF"),
        _ => (n < 0, n.unsigned_abs(), 10, b"0123456789abcdef"),
    };
    // The digits and precision zeros fit in a fixed buffer: precision is
    // a u8, and even the longest u64 octal representation is only 22 bytes.
    // Write backwards so no allocation or reversal is needed per item.
    let mut body = [b'0'; u8::MAX as usize + 1];
    let mut start = body.len();
    if magnitude != 0 || spec.precision != Some(0) {
        let mut rest = magnitude;
        loop {
            start -= 1;
            body[start] = digits[(rest % base) as usize];
            rest /= base;
            if rest == 0 {
                break;
            }
        }
    }
    let precision = usize::from(spec.precision.unwrap_or(0));
    start = start.min(body.len() - precision);
    if spec.conversion == b'o' && spec.hash && body.get(start) != Some(&b'0') {
        start -= 1;
    }
    let prefix: &[u8] = match spec.conversion {
        b'x' if spec.hash && magnitude != 0 => b"0x",
        b'X' if spec.hash && magnitude != 0 => b"0X",
        b'u' | b'o' | b'x' | b'X' => b"",
        _ if negative => b"-",
        _ if spec.plus => b"+",
        _ if spec.space => b" ",
        _ => b"",
    };
    pad(spec, prefix, &body[start..], spec.precision.is_none(), out);
}

/// glibc's `%a %A %e %E %f %F %g %G` for `x` (C's `%.14g` is
/// `Spec { precision: Some(14), ..Spec::new(b'g') }`). NaN is always
/// positive. Other conversions print as `%g`.
pub(crate) fn format_float(spec: &Spec, x: f64, out: &mut Vec<u8>) {
    let upper = spec.conversion.is_ascii_uppercase();
    let sign: &[u8] = if x.is_sign_negative() && !x.is_nan() {
        b"-"
    } else if spec.plus {
        b"+"
    } else if spec.space {
        b" "
    } else {
        b""
    };
    if !x.is_finite() {
        let body: &[u8] = match (x.is_nan(), upper) {
            (true, false) => b"nan",
            (true, true) => b"NAN",
            (false, false) => b"inf",
            (false, true) => b"INF",
        };
        pad(spec, sign, body, false, out);
        return;
    }
    let x = x.abs();
    let mut prefix = sign.to_vec();
    let mut body = String::new();
    match spec.conversion.to_ascii_lowercase() {
        b'a' => {
            prefix.extend_from_slice(if upper { b"0X" } else { b"0x" });
            hex_body(spec, x, &mut body);
        }
        b'e' => {
            let precision = usize::from(spec.precision.unwrap_or(6));
            exponential(x, precision, spec.hash, upper, &mut body);
        }
        b'f' => {
            let precision = usize::from(spec.precision.unwrap_or(6));
            let _ = write!(body, "{x:.precision$}");
            if spec.hash && precision == 0 {
                body.push('.');
            }
        }
        _ => general(spec, x, upper, &mut body),
    }
    pad(spec, &prefix, body.as_bytes(), true, out);
}

/// `%e` of a non-negative finite `x`: `d.ddde+XX`.
fn exponential(x: f64, precision: usize, hash: bool, upper: bool, body: &mut String) {
    let text = format!("{x:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    body.push_str(mantissa);
    if hash && precision == 0 {
        body.push('.');
    }
    push_exponent(body, if upper { 'E' } else { 'e' }, exponent);
}

fn push_exponent(body: &mut String, letter: char, exponent: i32) {
    let sign = if exponent < 0 { '-' } else { '+' };
    let _ = write!(body, "{letter}{sign}{:02}", exponent.unsigned_abs());
}

/// `%g` of a non-negative finite `x`: `%e` or `%f` by the exponent after
/// rounding to P significant digits, trailing zeros dropped unless `#`.
fn general(spec: &Spec, x: f64, upper: bool, body: &mut String) {
    let significant = match spec.precision {
        None => 6,
        Some(0) => 1,
        Some(p) => usize::from(p),
    };
    let decimals = significant - 1;
    let text = format!("{x:.decimals$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let fixed = (-4..significant as i32).contains(&exponent);
    let mut digits = if fixed {
        let decimals = (significant as i32 - 1 - exponent) as usize;
        format!("{x:.decimals$}")
    } else {
        mantissa.to_string()
    };
    if spec.hash {
        if !digits.contains('.') {
            digits.push('.');
        }
    } else if digits.contains('.') {
        digits.truncate(digits.trim_end_matches('0').trim_end_matches('.').len());
    }
    body.push_str(&digits);
    if !fixed {
        push_exponent(body, if upper { 'E' } else { 'e' }, exponent);
    }
}

/// glibc's `%a` digits after `0x` for a non-negative finite `x`
/// (`__printf_fphex`): leading `1` (`0` for subnormals and zero, exponent
/// -1022 or 0), trailing zeros dropped without a precision, otherwise
/// rounded half to even on the hex digits; a carry may make the leading
/// digit `2`.
fn hex_body(spec: &Spec, x: f64, body: &mut String) {
    const FRACTION_BITS: u32 = 52;
    let bits = x.to_bits();
    let biased = (bits >> FRACTION_BITS) as i32;
    let mut fraction = bits & ((1 << FRACTION_BITS) - 1);
    let (mut leading, exponent) = match (biased, fraction) {
        (0, 0) => (0u64, 0),
        (0, _) => (0, -1022),
        _ => (1, biased - 1023),
    };
    let mut count = match spec.precision {
        None => 13 - (fraction.trailing_zeros().min(52) / 4) as usize,
        Some(p) => usize::from(p),
    };
    if count < 13 {
        let shift = 4 * (13 - count as u32);
        let kept = fraction >> shift;
        let rest = fraction & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        let odd = if count == 0 { leading & 1 } else { kept & 1 } == 1;
        let mut kept = kept;
        if rest > half || (rest == half && odd) {
            kept += 1;
            if kept >> (4 * count as u32) != 0 {
                kept = 0;
                leading += 1;
            }
        }
        fraction = kept << shift;
    }
    let hex: &[u8; 16] = if spec.conversion == b'A' {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    body.push(char::from(hex[leading as usize & 15]));
    if count > 0 || spec.hash {
        body.push('.');
    }
    for i in 0..13usize.min(count) {
        let nibble = (fraction >> (48 - 4 * i)) & 15;
        body.push(char::from(hex[nibble as usize]));
    }
    while count > 13 {
        body.push('0');
        count -= 1;
    }
    let letter = if spec.conversion == b'A' { 'P' } else { 'p' };
    let sign = if exponent < 0 { '-' } else { '+' };
    let _ = write!(body, "{letter}{sign}{}", exponent.unsigned_abs());
}

/// `addquoted`.
fn add_quoted(s: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for (i, &byte) in s.iter().enumerate() {
        if byte == b'"' || byte == b'\\' || byte == b'\n' {
            out.push(b'\\');
            out.push(byte);
        } else if byte < 0x20 || byte == 0x7f {
            // C reads the terminating zero after the last byte.
            let next_is_digit = s.get(i + 1).is_some_and(u8::is_ascii_digit);
            let text = if next_is_digit {
                format!("\\{byte:03}")
            } else {
                format!("\\{byte}")
            };
            out.extend_from_slice(text.as_bytes());
        } else {
            out.push(byte);
        }
    }
    out.push(b'"');
}

/// `quotefloat`: `1e9999`, `-1e9999`, `(0/0)`, otherwise `%a`.
fn quote_float(x: f64, out: &mut Vec<u8>) {
    if x == f64::INFINITY {
        out.extend_from_slice(b"1e9999");
    } else if x == f64::NEG_INFINITY {
        out.extend_from_slice(b"-1e9999");
    } else if x.is_nan() {
        out.extend_from_slice(b"(0/0)");
    } else {
        format_float(&Spec::new(b'a'), x, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Arg<'a> {
        Int(i64),
        Num(f64),
        Str(&'a [u8]),
        Ptr(Option<&'a [u8]>),
        Lit(Literal<'a>),
    }

    fn give(
        f: &mut Formatter,
        fmt: &[u8],
        arg: Arg<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        match arg {
            Arg::Int(n) => f.give_integer(fmt, n, out),
            Arg::Num(x) => f.give_number(fmt, x, out),
            Arg::Str(s) => f.give_string(fmt, s, out),
            Arg::Ptr(p) => f.give_pointer(fmt, p, out),
            Arg::Lit(v) => f.give_literal(fmt, v, out),
        }
    }

    /// Runs `fmt` with `budget` per call to `next`, encoding and decoding
    /// the state at every stop when `resume` is set. Returns the result and
    /// the total budget consumed.
    fn drive(
        fmt: &[u8],
        args: &[Arg<'_>],
        budget: i64,
        resume: bool,
    ) -> (Result<Vec<u8>, FormatError>, i64) {
        let nargs = args.len() as u32;
        let mut f = Formatter::new(nargs);
        let mut out = Vec::new();
        let mut used = 0;
        let mut debt = 0;
        loop {
            let mut left = budget + debt;
            let before = left;
            let step = f.next(fmt, &mut out, &mut left);
            used += before - left;
            debt = left.min(0);
            if resume {
                let mut words = Vec::new();
                f.encode(&mut words);
                let mut slice = &words[..];
                f = Formatter::decode(&mut slice, fmt.len() as u32, nargs).unwrap();
                assert!(slice.is_empty());
            }
            match step {
                Step::Pending => {}
                Step::Done => return (Ok(out), used),
                Step::Error(e) => return (Err(e), used),
                Step::Need { arg, .. } => {
                    if let Err(e) = give(&mut f, fmt, args[arg as usize - 2], &mut out) {
                        return (Err(e), used);
                    }
                }
            }
        }
    }

    fn run(fmt: &[u8], args: &[Arg<'_>]) -> Result<Vec<u8>, FormatError> {
        drive(fmt, args, i64::MAX / 2, false).0
    }

    fn text(fmt: &str, arg: Arg<'_>) -> String {
        String::from_utf8(run(fmt.as_bytes(), &[arg]).unwrap()).unwrap()
    }

    #[test]
    fn literal_text_and_percent_are_copied() {
        assert_eq!(run(b"a%%b\0c", &[]).unwrap(), b"a%b\0c");
        assert_eq!(run(b"", &[]).unwrap(), b"");
        assert_eq!(run(b"x=%d;", &[Arg::Int(5)]).unwrap(), b"x=5;");
    }

    #[test]
    fn integers_follow_glibc() {
        let cases = [
            ("%5.3d", 7, "  007"),
            ("%-+5d", 3, "+3   "),
            ("% 05d", 3, " 0003"),
            ("%+05.3d", 4, " +004"),
            ("%.0d", 0, ""),
            ("%#.0o", 0, "0"),
            ("%#.0x", 0, ""),
            ("%#x", 255, "0xff"),
            ("%#08.3X", 255, "   0X0FF"),
            ("%u", -1, "18446744073709551615"),
            ("%#o", 8, "010"),
            ("%i", i64::MIN, "-9223372036854775808"),
            ("%5c", 321, "    A"),
        ];
        for (fmt, n, expected) in cases {
            assert_eq!(text(fmt, Arg::Int(n)), expected, "{fmt}");
        }
        assert_eq!(run(b"%-3c|", &[Arg::Int(256)]).unwrap(), b"\0  |");
    }

    #[test]
    fn decimal_floats_follow_glibc() {
        let cases = [
            ("%10.3f", 1.5, "     1.500"),
            ("%-+10.3e", 2.0, "+2.000e+00"),
            ("%#g", 1.0, "1.00000"),
            ("%.0f", 0.5, "0"),
            ("%.0f", 1.5, "2"),
            ("%.0f", 2.5, "2"),
            ("%g", 1e-5, "1e-05"),
            ("%#.0e", 1.0, "1.e+00"),
            ("%G", 1e300, "1E+300"),
            ("%.3g", -0.0, "-0"),
            ("%010f", f64::INFINITY, "       inf"),
            ("%-6E|", f64::NEG_INFINITY, "-INF  |"),
            ("%+f", f64::NAN, "+nan"),
            ("%f", -f64::NAN, "nan"),
            ("%08.2f", -1.005, "-0001.00"),
        ];
        for (fmt, x, expected) in cases {
            assert_eq!(text(fmt, Arg::Num(x)), expected, "{fmt}");
        }
    }

    #[test]
    fn hex_floats_follow_glibc() {
        let cases = [
            ("%.0a", 1.5, "0x2p+0"),
            ("%.1a", f64::from_bits(0x3ff1_8000_0000_0000), "0x1.2p+0"),
            ("%.1a", f64::from_bits(0x3ff2_8000_0000_0000), "0x1.2p+0"),
            ("%.1a", f64::from_bits(0x3fff_8000_0000_0000), "0x2.0p+0"),
            ("%.0a", f64::from_bits(0x0008_0000_0000_0000), "0x0p-1022"),
            ("%a", f64::from_bits(1), "0x0.0000000000001p-1022"),
            ("%a", 0.0, "0x0p+0"),
            ("%020a", 1.5, "0x0000000000001.8p+0"),
            ("%#.0a", 1.0, "0x1.p+0"),
            ("%A", -0.0, "-0X0P+0"),
            ("%.3a", 1.0, "0x1.000p+0"),
        ];
        for (fmt, x, expected) in cases {
            assert_eq!(text(fmt, Arg::Num(x)), expected, "{fmt}");
        }
    }

    #[test]
    fn strings_follow_lua_rules() {
        assert_eq!(run(b"%s", &[Arg::Str(b"a\0b")]).unwrap(), b"a\0b");
        assert_eq!(
            run(b"%5s", &[Arg::Str(b"a\0b")]),
            Err(FormatError::StringContainsZeros { arg: 2 })
        );
        assert_eq!(text("%-5.1s|", Arg::Str(b"abc")), "a    |");
        assert_eq!(text("%.s|", Arg::Str(b"abc")), "|");
        let long = [b'x'; 150];
        assert_eq!(run(b"%5s", &[Arg::Str(&long)]).unwrap(), long);
        assert_eq!(text("%.3s", Arg::Str(&long)), "xxx");
    }

    #[test]
    fn pointers_print_their_token() {
        assert_eq!(text("%-10p|", Arg::Ptr(None)), "(null)    |");
        assert_eq!(text("%6p", Arg::Ptr(Some(b"0x2a"))), "  0x2a");
        assert_eq!(
            run(b"%.3p", &[Arg::Ptr(None)]),
            Err(FormatError::InvalidSpecification)
        );
    }

    #[test]
    fn literals_quote_like_addliteral() {
        let q = |v| run(b"%q", &[Arg::Lit(v)]);
        assert_eq!(
            q(Literal::String(b"\x001\n\r\"\\\x7f9\x80")).unwrap(),
            b"\"\\0001\\\n\\13\\\"\\\\\\1279\x80\""
        );
        assert_eq!(
            q(Literal::Integer(i64::MIN)).unwrap(),
            b"0x8000000000000000"
        );
        assert_eq!(q(Literal::Integer(-7)).unwrap(), b"-7");
        assert_eq!(q(Literal::Float(1.0)).unwrap(), b"0x1p+0");
        assert_eq!(q(Literal::Float(0.1)).unwrap(), b"0x1.999999999999ap-4");
        assert_eq!(q(Literal::Float(f64::NEG_INFINITY)).unwrap(), b"-1e9999");
        assert_eq!(q(Literal::Float(-f64::NAN)).unwrap(), b"(0/0)");
        assert_eq!(q(Literal::Nil).unwrap(), b"nil");
        assert_eq!(q(Literal::Bool(false)).unwrap(), b"false");
        assert_eq!(q(Literal::Other), Err(FormatError::NoLiteral { arg: 2 }));
        assert_eq!(
            run(b"%10q", &[Arg::Lit(Literal::Nil)]),
            Err(FormatError::QModifiers)
        );
    }

    #[test]
    fn errors_come_in_c_order_with_puc_messages() {
        let message = |fmt: &[u8], args: &[Arg<'_>]| {
            let mut f = Formatter::new(args.len() as u32);
            let mut out = Vec::new();
            let mut budget = 1000;
            loop {
                match f.next(fmt, &mut out, &mut budget) {
                    Step::Need { arg, .. } => {
                        if let Err(e) = give(&mut f, fmt, args[arg as usize - 2], &mut out) {
                            return (e, f.message(fmt, e));
                        }
                    }
                    Step::Error(e) => return (e, f.message(fmt, e)),
                    other => panic!("{other:?}"),
                }
            }
        };
        let check = |fmt: &[u8], args: &[Arg<'_>], error, text: &str| {
            assert_eq!(message(fmt, args), (error, text.as_bytes().to_vec()));
        };
        check(b"%#c", &[], FormatError::NoValue { arg: 2 }, "no value");
        check(
            b"%d %d",
            &[Arg::Int(1)],
            FormatError::NoValue { arg: 3 },
            "no value",
        );
        check(
            b"%-----------------------d",
            &[Arg::Int(1)],
            FormatError::InvalidFormat,
            "invalid format (too long)",
        );
        check(
            b"%5\0d",
            &[Arg::Int(1)],
            FormatError::InvalidConversion,
            "invalid conversion '%5' to 'format'",
        );
        check(
            b"ab%",
            &[Arg::Int(1)],
            FormatError::InvalidConversion,
            "invalid conversion '%' to 'format'",
        );
        check(
            b"%F",
            &[Arg::Num(1.0)],
            FormatError::InvalidConversion,
            "invalid conversion '%F' to 'format'",
        );
        check(
            b"%100d",
            &[Arg::Int(1)],
            FormatError::InvalidSpecification,
            "invalid conversion specification: '%100d'",
        );
        // `%c` is checked before its argument is asked for, `%d` after.
        let mut f = Formatter::new(1);
        assert_eq!(
            f.next(b"%#c", &mut Vec::new(), &mut 10),
            Step::Error(FormatError::InvalidSpecification)
        );
        let mut f = Formatter::new(1);
        let need = f.next(b"%#d", &mut Vec::new(), &mut 10);
        assert_eq!(
            need,
            Step::Need {
                arg: 2,
                need: Need::Integer
            }
        );
        assert_eq!(
            f.give_integer(b"%#d", 1, &mut Vec::new()),
            Err(FormatError::InvalidSpecification)
        );
        assert_eq!(
            f.next(b"%#d", &mut Vec::new(), &mut 10),
            Step::Error(FormatError::Misuse)
        );
    }

    #[test]
    fn general_14_matches_moonseed_tostring() {
        // Moonseed's `concat.rs` cases: `%.14g`, then `.0` on integral text.
        let cases = [
            (1.5, "1.5"),
            (3.0, "3.0"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (0.1, "0.1"),
            (2f64.powi(53), "9.007199254741e+15"),
            (1e100, "1e+100"),
            (1e15, "1e+15"),
            (1e14, "1e+14"),
            (123_456_789_012_345.0, "1.2345678901234e+14"),
            (12_345_678_901_234.0, "12345678901234.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (2f64.sqrt(), "1.4142135623731"),
            (-1.25e-300, "-1.25e-300"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (-f64::NAN, "nan"),
            (1.0 / 3.0, "0.33333333333333"),
            (99_999_999_999_999.5, "1e+14"),
        ];
        let spec = Spec {
            precision: Some(14),
            ..Spec::new(b'g')
        };
        for (x, expected) in cases {
            let mut out = Vec::new();
            format_float(&spec, x, &mut out);
            if out.iter().all(|&b| b == b'-' || b.is_ascii_digit()) {
                out.extend_from_slice(b".0");
            }
            assert_eq!(out, expected.as_bytes(), "{x:e}");
        }
    }

    #[test]
    fn any_budget_schedule_gives_the_same_bytes_and_fuel() {
        let long = vec![b'y'; 9000];
        let mut fmt = b"head %5.2f and %% %s | ".to_vec();
        fmt.extend_from_slice(&long);
        fmt.extend_from_slice(b"%q%-4d%");
        let args = [
            Arg::Num(12.345),
            Arg::Str(b"str"),
            Arg::Lit(Literal::String(b"q\n")),
            Arg::Int(-2),
            Arg::Int(0),
        ];
        let reference = drive(&fmt, &args, i64::MAX / 2, false);
        assert_eq!(reference.0, Err(FormatError::InvalidConversion));
        fmt.pop();
        let reference = drive(&fmt, &args, i64::MAX / 2, false);
        assert!(reference.0.is_ok());
        for budget in [1, 2, 3, 7, 100] {
            for resume in [false, true] {
                assert_eq!(
                    drive(&fmt, &args, budget, resume),
                    reference,
                    "{budget} {resume}"
                );
            }
        }
    }

    #[test]
    fn decode_rejects_bad_words_without_panicking() {
        let fmt = b"ab %5d %s %q %% %";
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut accepted = 0;
        for round in 0..200_000 {
            let words: Vec<u64> = (0..STATE_WORDS)
                .map(|_| {
                    let w = next();
                    if round % 2 == 0 { w % 24 } else { w }
                })
                .collect();
            let mut slice = &words[..];
            let Some(mut f) = Formatter::decode(&mut slice, fmt.len() as u32, 2) else {
                assert_eq!(slice.len(), STATE_WORDS);
                continue;
            };
            accepted += 1;
            let mut out = Vec::new();
            for _ in 0..8 {
                let mut budget = 100;
                match f.next(fmt, &mut out, &mut budget) {
                    Step::Need { .. } => {
                        let _ = f.give_integer(fmt, 1, &mut out);
                        let _ = f.give_string(fmt, b"s", &mut out);
                        let _ = f.give_literal(fmt, Literal::Nil, &mut out);
                    }
                    Step::Pending => {}
                    Step::Done | Step::Error(_) => break,
                }
            }
            assert!(out.len() < 200);
            let _ = f.message(fmt, FormatError::InvalidConversion);
        }
        assert!(accepted > 0);
        assert!(Formatter::decode(&mut &[0u64; 5][..], 3, 0).is_none());
    }
}
