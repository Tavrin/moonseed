//! `string.pack`, `string.packsize` and `string.unpack` as resumable engines.
//!
//! A port of the PACK/UNPACK section of Lua 5.4.9's `lstrlib.c` over a fixed
//! ABI instead of the host's C types: `b` 1 byte, `h` 2, `i` 4, `l` `j` `T`
//! 8, `f` IEEE binary32, `d` `n` binary64, native endianness little, and the
//! maximum alignment of `!` 8. That is what PUC Lua uses on x86-64 Linux.
//! Bytes are assembled explicitly in either order and floats are converted
//! by their bits, so nothing depends on the target.
//!
//! The VM owns Lua values: the engines ask for arguments one at a time and
//! report errors as [`PackError`], whose [`PackError::message`] is PUC's
//! text and whose [`PackError::arg`] is the argument an argument error names.
//! As in C, the format ends at its first zero byte.
//!
//! Every engine takes a budget: 1 per option, plus `len / 64` for string
//! bytes copied or scanned. An engine stops with "pending" only between
//! options, so its state can be encoded there and resumed later.

/// Largest integral size, C's `MAXINTSIZE`.
const MAXINTSIZE: i64 = 16;
/// Size of a Lua integer, C's `SZINT`.
const SZINT: u32 = 8;
/// C's `MAXSIZE` on a 64-bit host: `INT_MAX`.
const MAXSIZE: i64 = i32::MAX as i64;
/// `offsetof(struct cD, u)`: the default of `!`.
const MAXALIGN: u32 = 8;
/// Largest size a `c` option can carry: `getnum` stops once past
/// `(MAXSIZE - 9) / 10`, so it reads at most `(MAXSIZE - 9) / 10 * 10 + 9`.
const MAX_NUM: u32 = ((MAXSIZE - 9) / 10 * 10 + 9) as u32;
/// Longest format or data string the engines look at; positions are `u32`.
const MAX_INPUT: usize = (u32::MAX - 32) as usize;

/// A `string.pack` family error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackError {
    /// `invalid format option '%c'` (a plain error).
    InvalidOption(u8),
    /// `integral size (%d) out of limits [1,16]` (a plain error).
    SizeOutOfLimits(i32),
    /// `missing size for format option 'c'` (a plain error).
    MissingCSize,
    /// `invalid next option for option 'X'` (argument 1).
    InvalidNextX,
    /// `format asks for alignment not power of 2` (argument 1).
    AlignNotPowerOf2,
    /// `integer overflow` (the argument being packed).
    IntegerOverflow { arg: u32 },
    /// `unsigned overflow` (the argument being packed).
    UnsignedOverflow { arg: u32 },
    /// `string longer than given size` (the argument being packed).
    StringLonger { arg: u32 },
    /// `string length does not fit in given size` (the argument being packed).
    LengthDoesNotFit { arg: u32 },
    /// `string contains zeros` (the argument being packed).
    ContainsZeros { arg: u32 },
    /// `variable-length format` (`string.packsize`, argument 1).
    VariableLength,
    /// `format result too large` (`string.packsize`, argument 1).
    ResultTooLarge,
    /// `data string too short` (`string.unpack`, argument 2).
    DataTooShort,
    /// `unfinished string for format 'z'` (`string.unpack`, argument 2).
    UnfinishedZ,
    /// `%d-byte integer does not fit into Lua Integer` (a plain error).
    DoesNotFit(u8),
    /// The packed string would pass the caller's limit. Moonseed's own
    /// error; PUC Lua has no such limit.
    TooLarge,
    /// A `give_*` call that does not answer the pending [`Need`]: a caller
    /// bug, not a Lua error.
    Protocol,
}

impl PackError {
    /// The argument an argument error names (`bad argument #n`), or `None`
    /// for a plain error.
    pub(crate) fn arg(&self) -> Option<u32> {
        match *self {
            PackError::InvalidNextX
            | PackError::AlignNotPowerOf2
            | PackError::VariableLength
            | PackError::ResultTooLarge => Some(1),
            PackError::DataTooShort | PackError::UnfinishedZ => Some(2),
            PackError::IntegerOverflow { arg }
            | PackError::UnsignedOverflow { arg }
            | PackError::StringLonger { arg }
            | PackError::LengthDoesNotFit { arg }
            | PackError::ContainsZeros { arg } => Some(arg),
            _ => None,
        }
    }

    /// PUC's message: the whole message of a plain error, the parenthesised
    /// reason of an argument error.
    pub(crate) fn message(&self) -> Vec<u8> {
        let text = match *self {
            PackError::InvalidOption(byte) => {
                let mut text = b"invalid format option '".to_vec();
                text.push(byte);
                text.push(b'\'');
                return text;
            }
            PackError::SizeOutOfLimits(size) => {
                format!("integral size ({size}) out of limits [1,{MAXINTSIZE}]")
            }
            PackError::MissingCSize => "missing size for format option 'c'".into(),
            PackError::InvalidNextX => "invalid next option for option 'X'".into(),
            PackError::AlignNotPowerOf2 => "format asks for alignment not power of 2".into(),
            PackError::IntegerOverflow { .. } => "integer overflow".into(),
            PackError::UnsignedOverflow { .. } => "unsigned overflow".into(),
            PackError::StringLonger { .. } => "string longer than given size".into(),
            PackError::LengthDoesNotFit { .. } => "string length does not fit in given size".into(),
            PackError::ContainsZeros { .. } => "string contains zeros".into(),
            PackError::VariableLength => "variable-length format".into(),
            PackError::ResultTooLarge => "format result too large".into(),
            PackError::DataTooShort => "data string too short".into(),
            PackError::UnfinishedZ => "unfinished string for format 'z'".into(),
            PackError::DoesNotFit(size) => {
                format!("{size}-byte integer does not fit into Lua Integer")
            }
            PackError::TooLarge => "resulting string too large".into(),
            PackError::Protocol => "pack argument of the wrong kind".into(),
        };
        text.into_bytes()
    }
}

/// The kind of argument an option packs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// `luaL_checkinteger`.
    Integer,
    /// `luaL_checknumber`.
    Number,
    /// `luaL_checklstring`.
    String,
}

/// What [`Packer::next`] stopped at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackStep {
    /// Argument `arg` (1-based, the format is argument 1) is needed.
    Need {
        arg: u32,
        need: Need,
    },
    /// The format is finished; `out` holds the result.
    Done,
    /// The budget ran out; call `next` again.
    Pending,
    Error(PackError),
}

/// C's `KOption`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Int,
    Uint,
    Float,
    Number,
    Double,
    Char,
    Str,
    Zstr,
    Padding,
    PaddAlign,
    Nop,
}

impl Kind {
    const ALL: [Kind; 11] = [
        Kind::Int,
        Kind::Uint,
        Kind::Float,
        Kind::Number,
        Kind::Double,
        Kind::Char,
        Kind::Str,
        Kind::Zstr,
        Kind::Padding,
        Kind::PaddAlign,
        Kind::Nop,
    ];

    fn code(self) -> u64 {
        Kind::ALL.iter().position(|&k| k == self).unwrap_or(0) as u64
    }

    fn from_code(code: u64) -> Option<Kind> {
        Kind::ALL.get(usize::try_from(code).ok()?).copied()
    }

    /// Whether `size` is one `getoption` can give this kind.
    fn size_ok(self, size: u32) -> bool {
        match self {
            Kind::Int | Kind::Uint | Kind::Str => (1..=16).contains(&size),
            Kind::Float => size == 4,
            Kind::Number | Kind::Double => size == 8,
            Kind::Char => size <= MAX_NUM,
            Kind::Padding => size == 1,
            Kind::Zstr | Kind::PaddAlign | Kind::Nop => size == 0,
        }
    }

    fn need(self) -> Option<Need> {
        match self {
            Kind::Int | Kind::Uint => Some(Need::Integer),
            Kind::Float | Kind::Number | Kind::Double => Some(Need::Number),
            Kind::Char | Kind::Str | Kind::Zstr => Some(Need::String),
            Kind::Padding | Kind::PaddAlign | Kind::Nop => None,
        }
    }
}

/// C's `Header` without the Lua state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    little: bool,
    maxalign: u32,
}

impl Header {
    /// C's `initheader`.
    fn new() -> Self {
        Header {
            little: true,
            maxalign: 1,
        }
    }

    fn encode(&self, out: &mut Vec<u64>) {
        out.push(u64::from(self.little));
        out.push(u64::from(self.maxalign));
    }

    fn decode(words: &mut &[u64]) -> Option<Self> {
        let little = take_bool(words)?;
        let maxalign = take(words)?;
        if !(1..=MAXINTSIZE as u64).contains(&maxalign) {
            return None;
        }
        Some(Header {
            little,
            maxalign: maxalign as u32,
        })
    }
}

fn take(words: &mut &[u64]) -> Option<u64> {
    let (&first, rest) = words.split_first()?;
    *words = rest;
    Some(first)
}

fn take_bool(words: &mut &[u64]) -> Option<bool> {
    match take(words)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn take_u32(words: &mut &[u64], max: u64) -> Option<u32> {
    let word = take(words)?;
    if word > max {
        return None;
    }
    u32::try_from(word).ok()
}

fn clip(bytes: &[u8]) -> &[u8] {
    bytes.get(..MAX_INPUT).unwrap_or(bytes)
}

/// The format byte at `p`, or 0 past the end (C's terminator).
fn at(fmt: &[u8], p: u32) -> u8 {
    fmt.get(p as usize).copied().unwrap_or(0)
}

fn charge(budget: &mut i64, cost: u64) {
    *budget = budget.saturating_sub(i64::try_from(cost).unwrap_or(i64::MAX));
}

/// Mirrors C's `getnum`.
fn getnum(fmt: &[u8], p: &mut u32, df: i64) -> i64 {
    if !at(fmt, *p).is_ascii_digit() {
        return df;
    }
    let mut a: i64 = 0;
    loop {
        a = a * 10 + i64::from(at(fmt, *p) - b'0');
        *p += 1;
        if !(at(fmt, *p).is_ascii_digit() && a <= (MAXSIZE - 9) / 10) {
            return a;
        }
    }
}

/// Mirrors C's `getnumlimit`.
fn getnumlimit(fmt: &[u8], p: &mut u32, df: i64) -> Result<u32, PackError> {
    let size = getnum(fmt, p, df);
    if size > MAXINTSIZE || size <= 0 {
        return Err(PackError::SizeOutOfLimits(size as i32));
    }
    Ok(size as u32)
}

/// Mirrors C's `getoption`. The caller has checked that `p` is not at the
/// end of the format.
fn getoption(h: &mut Header, fmt: &[u8], p: &mut u32) -> Result<(Kind, u32), PackError> {
    let opt = at(fmt, *p);
    if (*p as usize) < fmt.len() {
        *p += 1;
    }
    Ok(match opt {
        b'b' => (Kind::Int, 1),
        b'B' => (Kind::Uint, 1),
        b'h' => (Kind::Int, 2),
        b'H' => (Kind::Uint, 2),
        b'l' | b'j' => (Kind::Int, 8),
        b'L' | b'J' | b'T' => (Kind::Uint, 8),
        b'f' => (Kind::Float, 4),
        b'n' => (Kind::Number, 8),
        b'd' => (Kind::Double, 8),
        b'i' => (Kind::Int, getnumlimit(fmt, p, 4)?),
        b'I' => (Kind::Uint, getnumlimit(fmt, p, 4)?),
        b's' => (Kind::Str, getnumlimit(fmt, p, 8)?),
        b'c' => {
            let size = getnum(fmt, p, -1);
            if size == -1 {
                return Err(PackError::MissingCSize);
            }
            (Kind::Char, size as u32)
        }
        b'z' => (Kind::Zstr, 0),
        b'x' => (Kind::Padding, 1),
        b'X' => (Kind::PaddAlign, 0),
        b' ' => (Kind::Nop, 0),
        b'<' => {
            h.little = true;
            (Kind::Nop, 0)
        }
        b'>' => {
            h.little = false;
            (Kind::Nop, 0)
        }
        b'=' => {
            h.little = true;
            (Kind::Nop, 0)
        }
        b'!' => {
            h.maxalign = getnumlimit(fmt, p, i64::from(MAXALIGN))?;
            (Kind::Nop, 0)
        }
        other => return Err(PackError::InvalidOption(other)),
    })
}

/// Mirrors C's `getdetails`: the option, its size, and the padding that
/// aligns it at `total`.
fn getdetails(
    h: &mut Header,
    total: u64,
    fmt: &[u8],
    p: &mut u32,
) -> Result<(Kind, u32, u32), PackError> {
    let (opt, size) = getoption(h, fmt, p)?;
    let mut align = size;
    if opt == Kind::PaddAlign {
        if at(fmt, *p) == 0 {
            return Err(PackError::InvalidNextX);
        }
        let (next, next_align) = getoption(h, fmt, p)?;
        if next == Kind::Char || next_align == 0 {
            return Err(PackError::InvalidNextX);
        }
        align = next_align;
    }
    let ntoalign = if align <= 1 || opt == Kind::Char {
        0
    } else {
        let align = align.min(h.maxalign);
        if align & (align - 1) != 0 {
            return Err(PackError::AlignNotPowerOf2);
        }
        let mask = u64::from(align - 1);
        ((u64::from(align) - (total & mask)) & mask) as u32
    };
    Ok((opt, size, ntoalign))
}

/// Mirrors C's `packint`: `size` bytes of `n`, sign-extended past 8 bytes
/// when `neg`.
fn packint(out: &mut Vec<u8>, n: u64, little: bool, size: u32, neg: bool) {
    let size = size.min(MAXINTSIZE as u32) as usize;
    let mut buff = [0u8; MAXINTSIZE as usize];
    for (i, byte) in buff.iter_mut().enumerate().take(size) {
        *byte = if i < SZINT as usize {
            (n >> (8 * i)) as u8
        } else if neg {
            0xff
        } else {
            0
        };
    }
    let buff = &mut buff[..size];
    if !little {
        buff.reverse();
    }
    out.extend_from_slice(buff);
}

/// Mirrors C's `unpackint` over exactly `size` bytes.
fn unpackint(bytes: &[u8], little: bool, signed: bool) -> Result<i64, PackError> {
    let size = bytes.len();
    let byte = |i: usize| -> u8 {
        let index = if little { i } else { size - 1 - i };
        bytes.get(index).copied().unwrap_or(0)
    };
    let limit = size.min(SZINT as usize);
    let mut res: u64 = 0;
    for i in (0..limit).rev() {
        res = (res << 8) | u64::from(byte(i));
    }
    if size < SZINT as usize {
        if signed && size > 0 {
            let mask = 1u64 << (size * 8 - 1);
            res = (res ^ mask).wrapping_sub(mask);
        }
    } else if size > SZINT as usize {
        let mask = if !signed || (res as i64) >= 0 {
            0
        } else {
            0xff
        };
        for i in limit..size {
            if byte(i) != mask {
                return Err(PackError::DoesNotFit(size as u8));
            }
        }
    }
    Ok(res as i64)
}

/// `(float)x` on the bits of `x`: round to nearest even, and a NaN keeps
/// its sign and top payload bits and becomes quiet, as x86-64 does.
fn f64_to_f32_bits(x: u64) -> u32 {
    let sign = ((x >> 63) as u32) << 31;
    let exp = ((x >> 52) & 0x7ff) as i32;
    let frac = x & ((1u64 << 52) - 1);
    if exp == 0x7ff {
        return if frac == 0 {
            sign | 0x7f80_0000
        } else {
            sign | 0x7fc0_0000 | (frac >> 29) as u32
        };
    }
    if exp == 0 {
        // A binary64 subnormal is far below half the smallest binary32 one.
        return sign;
    }
    let m = (1u64 << 52) | frac;
    let e32 = exp - 1023 + 127;
    let shift = (if e32 >= 1 { 29 } else { 29 + (1 - e32) }) as u32;
    if shift >= 54 {
        return sign;
    }
    let half = 1u64 << (shift - 1);
    let rem = m & ((1u64 << shift) - 1);
    let mut q = m >> shift;
    if rem > half || (rem == half && q & 1 == 1) {
        q += 1;
    }
    if e32 >= 1 {
        let mut e32 = e32 as u32;
        if q == 1 << 24 {
            q = 1 << 23;
            e32 += 1;
        }
        if e32 >= 0xff {
            return sign | 0x7f80_0000;
        }
        sign | (e32 << 23) | (q as u32 & 0x7f_ffff)
    } else {
        sign | q as u32
    }
}

/// `(double)f` on the bits of `f`: exact, and a NaN becomes quiet, as
/// x86-64 does.
fn f32_to_f64_bits(f: u32) -> u64 {
    let sign = u64::from(f >> 31) << 63;
    let exp = (f >> 23) & 0xff;
    let frac = u64::from(f & 0x7f_ffff);
    if exp == 0xff {
        return if frac == 0 {
            sign | (0x7ffu64 << 52)
        } else {
            sign | (0x7ffu64 << 52) | (1u64 << 51) | (frac << 29)
        };
    }
    if exp == 0 {
        if frac == 0 {
            return sign;
        }
        // frac * 2^-149 with its top bit at `top`.
        let top = 63 - frac.leading_zeros() as i64;
        let exp64 = (top - 149 + 1023) as u64;
        let mant = (frac << (52 - top)) & ((1u64 << 52) - 1);
        return sign | (exp64 << 52) | mant;
    }
    sign | ((u64::from(exp) + 1023 - 127) << 52) | (frac << 29)
}

/// The bytes of `bits`, `size` of them, in the given order.
fn put_bits(out: &mut Vec<u8>, bits: u64, size: u32, little: bool) {
    packint(out, bits, little, size, false);
}

fn get_bits(bytes: &[u8], little: bool) -> u64 {
    let size = bytes.len();
    let mut res = 0u64;
    for i in (0..size.min(8)).rev() {
        let index = if little { i } else { size - 1 - i };
        res = (res << 8) | u64::from(bytes.get(index).copied().unwrap_or(0));
    }
    res
}

/// The option [`Packer`] waits on an argument for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Awaiting {
    kind: Kind,
    size: u32,
}

/// `string.pack` between options (C's `str_pack` loop).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Packer {
    h: Header,
    /// Position in the format.
    fpos: u32,
    /// C's `arg`: the last argument taken.
    arg: u32,
    /// C's `totalsize`: bytes this packer has produced.
    total: u64,
    /// Budget owed for string bytes copied by the last `give_string`.
    owed: u64,
    awaiting: Option<Awaiting>,
}

impl Default for Packer {
    fn default() -> Self {
        Self::new()
    }
}

impl Packer {
    pub(crate) fn new() -> Self {
        Packer {
            h: Header::new(),
            fpos: 0,
            arg: 1,
            total: 0,
            owed: 0,
            awaiting: None,
        }
    }

    /// Appends padding and literal bytes to `out`, advancing through `fmt`
    /// until an option needs an argument, the format ends, the budget runs
    /// out, or an error. `limit` is the largest `out` may grow to; passing
    /// it is [`PackError::TooLarge`], found before `out` grows. While an
    /// argument is pending, `next` asks for it again without charge.
    pub(crate) fn next(
        &mut self,
        fmt: &[u8],
        out: &mut Vec<u8>,
        limit: usize,
        budget: &mut i64,
    ) -> PackStep {
        let fmt = clip(fmt);
        if let Some(awaiting) = self.awaiting {
            return match awaiting.kind.need() {
                Some(need) => PackStep::Need {
                    arg: self.arg,
                    need,
                },
                None => PackStep::Error(PackError::Protocol),
            };
        }
        charge(budget, std::mem::take(&mut self.owed));
        loop {
            if at(fmt, self.fpos) == 0 {
                return PackStep::Done;
            }
            if *budget <= 0 {
                return PackStep::Pending;
            }
            charge(budget, 1);
            let mut h = self.h;
            let mut fpos = self.fpos;
            let (kind, size, ntoalign) = match getdetails(&mut h, self.total, fmt, &mut fpos) {
                Ok(details) => details,
                Err(error) => return PackStep::Error(error),
            };
            let pad = ntoalign as usize + usize::from(kind == Kind::Padding);
            if out.len().saturating_add(pad) > limit {
                return PackStep::Error(PackError::TooLarge);
            }
            out.resize(out.len() + pad, 0);
            self.h = h;
            self.fpos = fpos;
            self.total = self
                .total
                .saturating_add(u64::from(ntoalign))
                .saturating_add(u64::from(size));
            if let Some(need) = kind.need() {
                self.arg = self.arg.saturating_add(1);
                self.awaiting = Some(Awaiting { kind, size });
                return PackStep::Need {
                    arg: self.arg,
                    need,
                };
            }
        }
    }

    fn awaiting(&self, need: Need) -> Result<Awaiting, PackError> {
        match self.awaiting {
            Some(awaiting) if awaiting.kind.need() == Some(need) => Ok(awaiting),
            _ => Err(PackError::Protocol),
        }
    }

    fn room(out: &[u8], add: u64, limit: usize) -> Result<(), PackError> {
        let add = usize::try_from(add).map_err(|_| PackError::TooLarge)?;
        if out.len().saturating_add(add) > limit {
            return Err(PackError::TooLarge);
        }
        Ok(())
    }

    /// Answers a [`Need::Integer`] (C's `Kint` and `Kuint` cases).
    pub(crate) fn give_integer(
        &mut self,
        _fmt: &[u8],
        n: i64,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), PackError> {
        let Awaiting { kind, size } = self.awaiting(Need::Integer)?;
        let arg = self.arg;
        if size < SZINT {
            let bits = size * 8;
            if kind == Kind::Int {
                let lim = 1i64 << (bits - 1);
                if !(-lim <= n && n < lim) {
                    return Err(PackError::IntegerOverflow { arg });
                }
            } else if (n as u64) >= (1u64 << bits) {
                return Err(PackError::UnsignedOverflow { arg });
            }
        }
        Self::room(out, u64::from(size), limit)?;
        packint(
            out,
            n as u64,
            self.h.little,
            size,
            kind == Kind::Int && n < 0,
        );
        self.awaiting = None;
        Ok(())
    }

    /// Answers a [`Need::Number`] (C's `Kfloat`, `Knumber` and `Kdouble`).
    pub(crate) fn give_number(
        &mut self,
        _fmt: &[u8],
        x: f64,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), PackError> {
        let Awaiting { kind, size } = self.awaiting(Need::Number)?;
        Self::room(out, u64::from(size), limit)?;
        let bits = if kind == Kind::Float {
            u64::from(f64_to_f32_bits(x.to_bits()))
        } else {
            x.to_bits()
        };
        put_bits(out, bits, size, self.h.little);
        self.awaiting = None;
        Ok(())
    }

    /// Answers a [`Need::String`] (C's `Kchar`, `Kstring` and `Kzstr`).
    pub(crate) fn give_string(
        &mut self,
        _fmt: &[u8],
        s: &[u8],
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), PackError> {
        let Awaiting { kind, size } = self.awaiting(Need::String)?;
        let arg = self.arg;
        let len = s.len() as u64;
        match kind {
            Kind::Char => {
                if len > u64::from(size) {
                    return Err(PackError::StringLonger { arg });
                }
                Self::room(out, u64::from(size), limit)?;
                out.extend_from_slice(s);
                out.resize(out.len() + (size as usize - s.len()), 0);
                self.owed = u64::from(size) / 64;
            }
            Kind::Str => {
                if !(size >= 8 || len < (1u64 << (size * 8))) {
                    return Err(PackError::LengthDoesNotFit { arg });
                }
                Self::room(out, u64::from(size).saturating_add(len), limit)?;
                packint(out, len, self.h.little, size, false);
                out.extend_from_slice(s);
                self.total = self.total.saturating_add(len);
                self.owed = len / 64;
            }
            _ => {
                if s.contains(&0) {
                    return Err(PackError::ContainsZeros { arg });
                }
                Self::room(out, len.saturating_add(1), limit)?;
                out.extend_from_slice(s);
                out.push(0);
                self.total = self.total.saturating_add(len + 1);
                self.owed = len / 64;
            }
        }
        self.awaiting = None;
        Ok(())
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        self.h.encode(out);
        out.push(u64::from(self.fpos));
        out.push(u64::from(self.arg));
        out.push(self.total);
        out.push(self.owed);
        match self.awaiting {
            None => out.extend([0, 0, 0]),
            Some(Awaiting { kind, size }) => out.extend([1, kind.code(), u64::from(size)]),
        }
    }

    /// Decodes a state for a format of `fmt_len` bytes.
    pub(crate) fn decode(words: &mut &[u64], fmt_len: usize) -> Option<Self> {
        let fmt_len = fmt_len.min(MAX_INPUT) as u64;
        let h = Header::decode(words)?;
        let fpos = take_u32(words, fmt_len)?;
        // One argument per format byte at most.
        let arg = take_u32(words, fmt_len + 1)?;
        let total = take(words)?;
        let owed = take(words)?;
        if owed > u64::from(u32::MAX) {
            return None;
        }
        let tag = take_bool(words)?;
        let kind = take(words)?;
        let size = take(words)?;
        let awaiting = if tag {
            let kind = Kind::from_code(kind)?;
            let size = u32::try_from(size).ok()?;
            if kind.need().is_none() || !kind.size_ok(size) || arg < 2 {
                return None;
            }
            Some(Awaiting { kind, size })
        } else {
            if kind != 0 || size != 0 {
                return None;
            }
            None
        };
        Some(Packer {
            h,
            fpos,
            arg,
            total,
            owed,
            awaiting,
        })
    }
}

/// `string.packsize` between options (C's `str_packsize`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SizeCounter {
    h: Header,
    fpos: u32,
    total: u64,
}

impl Default for SizeCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl SizeCounter {
    pub(crate) fn new() -> Self {
        SizeCounter {
            h: Header::new(),
            fpos: 0,
            total: 0,
        }
    }

    /// The size of the format's result, `None` while pending.
    pub(crate) fn run(&mut self, fmt: &[u8], budget: &mut i64) -> Result<Option<u64>, PackError> {
        let fmt = clip(fmt);
        loop {
            if at(fmt, self.fpos) == 0 {
                return Ok(Some(self.total));
            }
            if *budget <= 0 {
                return Ok(None);
            }
            charge(budget, 1);
            let (kind, size, ntoalign) = getdetails(&mut self.h, self.total, fmt, &mut self.fpos)?;
            if kind == Kind::Str || kind == Kind::Zstr {
                return Err(PackError::VariableLength);
            }
            let size = i64::from(size) + i64::from(ntoalign);
            if self.total as i64 > MAXSIZE - size {
                return Err(PackError::ResultTooLarge);
            }
            self.total += size as u64;
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        self.h.encode(out);
        out.push(u64::from(self.fpos));
        out.push(self.total);
    }

    pub(crate) fn decode(words: &mut &[u64], fmt_len: usize) -> Option<Self> {
        let h = Header::decode(words)?;
        let fpos = take_u32(words, fmt_len.min(MAX_INPUT) as u64)?;
        let total = take(words)?;
        if total > MAXSIZE as u64 {
            return None;
        }
        Some(SizeCounter { h, fpos, total })
    }
}

/// A value `string.unpack` produces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Unpacked {
    Integer(i64),
    Number(f64),
    /// The bytes `data[start..end]`.
    Bytes {
        start: u32,
        end: u32,
    },
}

impl Unpacked {
    fn encode(&self, out: &mut Vec<u64>) {
        match *self {
            Unpacked::Integer(n) => out.extend([0, n as u64, 0]),
            Unpacked::Number(x) => out.extend([1, x.to_bits(), 0]),
            Unpacked::Bytes { start, end } => out.extend([2, u64::from(start), u64::from(end)]),
        }
    }

    fn decode(words: &mut &[u64], data_len: u64) -> Option<Self> {
        let tag = take(words)?;
        let a = take(words)?;
        let b = take(words)?;
        match tag {
            0 if b == 0 => Some(Unpacked::Integer(a as i64)),
            1 if b == 0 => Some(Unpacked::Number(f64::from_bits(a))),
            2 if a <= b && b <= data_len => Some(Unpacked::Bytes {
                start: a as u32,
                end: b as u32,
            }),
            _ => None,
        }
    }
}

/// What [`Unpacker::next`] stopped at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UnpackStep {
    /// The next result. It stays pending, and `next` gives it again,
    /// until [`Unpacker::accept`].
    Value(Unpacked),
    /// The format is finished; `next` is the 1-based position after the
    /// last byte read (C's final result).
    Done {
        next: i64,
    },
    Pending,
    Error(PackError),
}

/// A produced value the caller has not yet accepted, and where the
/// unpacker goes once it has.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Staged {
    value: Unpacked,
    fpos: u32,
    pos: u32,
}

/// `string.unpack` between options (C's `str_unpack` loop).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Unpacker {
    h: Header,
    fpos: u32,
    /// Position in the data, 0-based; never past its end.
    pos: u32,
    staged: Option<Staged>,
}

impl Unpacker {
    /// Starts at the 0-based `pos` the caller checked against the data
    /// (C's `posrelatI` and "initial position out of string").
    pub(crate) fn new(pos: u32) -> Self {
        Unpacker {
            h: Header::new(),
            fpos: 0,
            pos,
            staged: None,
        }
    }

    /// Produces the next value, or the final position, or pending, or an
    /// error.
    pub(crate) fn next(&mut self, fmt: &[u8], data: &[u8], budget: &mut i64) -> UnpackStep {
        let (fmt, data) = (clip(fmt), clip(data));
        if let Some(staged) = self.staged {
            return UnpackStep::Value(staged.value);
        }
        let ld = data.len() as u64;
        loop {
            if at(fmt, self.fpos) == 0 {
                return UnpackStep::Done {
                    next: i64::from(self.pos) + 1,
                };
            }
            if *budget <= 0 {
                return UnpackStep::Pending;
            }
            charge(budget, 1);
            let pos = u64::from(self.pos).min(ld);
            let mut fpos = self.fpos;
            let (kind, size, ntoalign) = match getdetails(&mut self.h, pos, fmt, &mut fpos) {
                Ok(details) => details,
                Err(error) => return UnpackStep::Error(error),
            };
            let size64 = u64::from(size);
            if u64::from(ntoalign) + size64 > ld - pos {
                return UnpackStep::Error(PackError::DataTooShort);
            }
            let pos = pos + u64::from(ntoalign);
            let item = data
                .get(pos as usize..(pos + size64) as usize)
                .unwrap_or(&[]);
            let little = self.h.little;
            let (value, end) = match kind {
                Kind::Int | Kind::Uint => match unpackint(item, little, kind == Kind::Int) {
                    Ok(n) => (Unpacked::Integer(n), pos + size64),
                    Err(error) => return UnpackStep::Error(error),
                },
                Kind::Float => {
                    let bits = f32_to_f64_bits(get_bits(item, little) as u32);
                    (Unpacked::Number(f64::from_bits(bits)), pos + size64)
                }
                Kind::Number | Kind::Double => (
                    Unpacked::Number(f64::from_bits(get_bits(item, little))),
                    pos + size64,
                ),
                Kind::Char => {
                    charge(budget, size64 / 64);
                    let end = pos + size64;
                    (bytes(pos, end), end)
                }
                Kind::Str => {
                    let len = match unpackint(item, little, false) {
                        Ok(len) => len as u64,
                        Err(error) => return UnpackStep::Error(error),
                    };
                    if len > ld - pos - size64 {
                        return UnpackStep::Error(PackError::DataTooShort);
                    }
                    charge(budget, len / 64);
                    let start = pos + size64;
                    (bytes(start, start + len), start + len)
                }
                Kind::Zstr => {
                    let rest = data.get(pos as usize..).unwrap_or(&[]);
                    let len = rest.iter().position(|&b| b == 0).unwrap_or(rest.len()) as u64;
                    charge(budget, len / 64);
                    if pos + len >= ld {
                        return UnpackStep::Error(PackError::UnfinishedZ);
                    }
                    (bytes(pos, pos + len), pos + len + 1)
                }
                Kind::Padding | Kind::PaddAlign | Kind::Nop => {
                    self.fpos = fpos;
                    self.pos = (pos + size64) as u32;
                    continue;
                }
            };
            self.staged = Some(Staged {
                value,
                fpos,
                pos: end as u32,
            });
            return UnpackStep::Value(value);
        }
    }

    /// Takes the value [`Unpacker::next`] produced; the next call moves on.
    pub(crate) fn accept(&mut self) {
        if let Some(staged) = self.staged.take() {
            self.fpos = staged.fpos;
            self.pos = staged.pos;
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        self.h.encode(out);
        out.push(u64::from(self.fpos));
        out.push(u64::from(self.pos));
        match &self.staged {
            None => out.extend([0, 0, 0, 0, 0, 0]),
            Some(staged) => {
                out.push(1);
                staged.value.encode(out);
                out.push(u64::from(staged.fpos));
                out.push(u64::from(staged.pos));
            }
        }
    }

    /// Decodes a state for a format of `fmt_len` and data of `data_len`
    /// bytes.
    pub(crate) fn decode(words: &mut &[u64], fmt_len: usize, data_len: usize) -> Option<Self> {
        let fmt_len = fmt_len.min(MAX_INPUT) as u64;
        let data_len = data_len.min(MAX_INPUT) as u64;
        let h = Header::decode(words)?;
        let fpos = take_u32(words, fmt_len)?;
        let pos = take_u32(words, data_len)?;
        let staged = if take_bool(words)? {
            let value = Unpacked::decode(words, data_len)?;
            let next_fpos = take_u32(words, fmt_len)?;
            let next_pos = take_u32(words, data_len)?;
            if next_fpos <= fpos || next_pos < pos {
                return None;
            }
            Some(Staged {
                value,
                fpos: next_fpos,
                pos: next_pos,
            })
        } else {
            if take(words)? != 0 || words.len() < 4 {
                return None;
            }
            for _ in 0..4 {
                if take(words)? != 0 {
                    return None;
                }
            }
            None
        };
        Some(Unpacker {
            h,
            fpos,
            pos,
            staged,
        })
    }
}

fn bytes(start: u64, end: u64) -> Unpacked {
    Unpacked::Bytes {
        start: start as u32,
        end: end as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum A<'a> {
        I(i64),
        N(f64),
        S(&'a [u8]),
    }

    /// Runs a pack with `step` budget per refill, encoding and decoding the
    /// state at every stop. Returns the result and the budget used.
    fn pack_with(fmt: &[u8], args: &[A], step: i64) -> (Result<Vec<u8>, PackError>, i64) {
        let mut p = Packer::new();
        let mut out = Vec::new();
        let (mut budget, mut used) = (step, 0);
        let mut next_arg = args.iter();
        loop {
            let before = budget;
            let r = p.next(fmt, &mut out, 1 << 20, &mut budget);
            used += before - budget;
            let mut words = Vec::new();
            p.encode(&mut words);
            let mut slice = &words[..];
            p = Packer::decode(&mut slice, fmt.len()).unwrap();
            assert!(slice.is_empty());
            match r {
                PackStep::Done => return (Ok(out), used),
                PackStep::Error(e) => return (Err(e), used),
                PackStep::Pending => budget += step,
                PackStep::Need { .. } => {
                    let given = match *next_arg.next().unwrap() {
                        A::I(n) => p.give_integer(fmt, n, &mut out, 1 << 20),
                        A::N(x) => p.give_number(fmt, x, &mut out, 1 << 20),
                        A::S(s) => p.give_string(fmt, s, &mut out, 1 << 20),
                    };
                    if let Err(e) = given {
                        return (Err(e), used);
                    }
                }
            }
        }
    }

    fn pack(fmt: &[u8], args: &[A]) -> Result<Vec<u8>, PackError> {
        pack_with(fmt, args, i64::MAX / 2).0
    }

    fn packsize(fmt: &[u8]) -> Result<u64, PackError> {
        let mut budget = i64::MAX / 2;
        SizeCounter::new().run(fmt, &mut budget).map(|n| n.unwrap())
    }

    /// Unpacked values as (kind, bits, bits), the final position as kind 3.
    type Values = Vec<(u8, u64, u64)>;

    fn unpack_with(
        fmt: &[u8],
        data: &[u8],
        pos: u32,
        step: i64,
    ) -> (Result<Values, PackError>, i64) {
        let mut u = Unpacker::new(pos);
        let (mut budget, mut used, mut values) = (step, 0, Vec::new());
        loop {
            let before = budget;
            let r = u.next(fmt, data, &mut budget);
            used += before - budget;
            let mut words = Vec::new();
            u.encode(&mut words);
            let mut slice = &words[..];
            u = Unpacker::decode(&mut slice, fmt.len(), data.len()).unwrap();
            assert!(slice.is_empty());
            match r {
                UnpackStep::Value(v) => {
                    values.push(match v {
                        Unpacked::Integer(n) => (0, n as u64, 0),
                        Unpacked::Number(x) => (1, x.to_bits(), 0),
                        Unpacked::Bytes { start, end } => (2, start.into(), end.into()),
                    });
                    u.accept();
                }
                UnpackStep::Done { next } => {
                    values.push((3, next as u64, 0));
                    return (Ok(values), used);
                }
                UnpackStep::Pending => budget += step,
                UnpackStep::Error(e) => return (Err(e), used),
            }
        }
    }

    fn unpack(fmt: &[u8], data: &[u8]) -> Result<Values, PackError> {
        unpack_with(fmt, data, 0, i64::MAX / 2).0
    }

    #[test]
    fn canonical_abi_sizes() {
        for (fmt, size) in [
            ("b", 1),
            ("B", 1),
            ("h", 2),
            ("H", 2),
            ("i", 4),
            ("I", 4),
            ("l", 8),
            ("L", 8),
            ("j", 8),
            ("J", 8),
            ("T", 8),
            ("f", 4),
            ("d", 8),
            ("n", 8),
            ("x", 1),
            ("!bd", 16),
            ("!bXd", 8),
            ("i16", 16),
        ] {
            assert_eq!(packsize(fmt.as_bytes()), Ok(size), "{fmt}");
        }
    }

    #[test]
    fn integers_pack_in_either_order_and_extend_past_eight_bytes() {
        assert_eq!(
            pack(b"<i3>i3", &[A::I(0x010203), A::I(-2)]).unwrap(),
            [3, 2, 1, 0xff, 0xff, 0xfe]
        );
        let mut wide = vec![0xfe];
        wide.extend([0xff; 15]);
        assert_eq!(pack(b"<i16", &[A::I(-2)]).unwrap(), wide);
        let mut unsigned = vec![0xfe];
        unsigned.extend([0xff; 7]);
        unsigned.extend([0; 2]);
        assert_eq!(pack(b"<I10", &[A::I(-2)]).unwrap(), unsigned);
    }

    #[test]
    fn narrow_integers_check_their_range() {
        assert_eq!(
            pack(b"bb", &[A::I(1), A::I(128)]),
            Err(PackError::IntegerOverflow { arg: 3 })
        );
        assert!(pack(b"b", &[A::I(-128)]).is_ok());
        assert_eq!(
            pack(b"H", &[A::I(65536)]),
            Err(PackError::UnsignedOverflow { arg: 2 })
        );
        assert_eq!(
            pack(b"I7", &[A::I(-1)]),
            Err(PackError::UnsignedOverflow { arg: 2 })
        );
        assert!(pack(b"I8", &[A::I(-1)]).is_ok());
    }

    #[test]
    fn integers_unpack_with_sign_extension_and_fit_checks() {
        assert_eq!(
            unpack(b"<i3", &[0xfe, 0xff, 0xff]),
            Ok(vec![(0, (-2i64) as u64, 0), (3, 4, 0)])
        );
        assert_eq!(unpack(b">I2", &[1, 2]), Ok(vec![(0, 0x102, 0), (3, 3, 0)]));
        let mut data = [0xffu8; 12];
        assert_eq!(
            unpack(b"<i12", &data),
            Ok(vec![(0, u64::MAX, 0), (3, 13, 0)])
        );
        assert_eq!(unpack(b"<I12", &data), Err(PackError::DoesNotFit(12)));
        data[11] = 0;
        assert_eq!(unpack(b"<i12", &data), Err(PackError::DoesNotFit(12)));
    }

    #[test]
    fn floats_narrow_as_a_c_cast_and_widen_exactly() {
        let f = |x: u64| f64_to_f32_bits(x);
        assert_eq!(f((1.0f64 + 2f64.powi(-24)).to_bits()), 1.0f32.to_bits());
        assert_eq!(f((1.0f64 + 3.0 * 2f64.powi(-24)).to_bits()), 0x3f80_0002);
        assert_eq!(f(2f64.powi(-150).to_bits()), 0);
        assert_eq!(f((2f64.powi(-150) * 1.5).to_bits()), 1);
        assert_eq!(f((f32::MAX as f64 + 2f64.powi(103)).to_bits()), 0x7f80_0000);
        assert_eq!(f(0xfff0_0020_0000_0000), 0xffc0_0100);
        assert_eq!(f32_to_f64_bits(0x7f80_0001), 0x7ff8_0000_2000_0000);
        assert_eq!(f32_to_f64_bits(1), 2f64.powi(-149).to_bits());
        for bits in [
            0u32,
            0x8000_0000,
            0x3f80_0000,
            0x007f_ffff,
            0x0080_0000,
            0x7f7f_ffff,
            0xff80_0000,
        ] {
            assert_eq!(
                f32_to_f64_bits(bits),
                (f32::from_bits(bits) as f64).to_bits()
            );
            assert_eq!(f(f32_to_f64_bits(bits)), bits);
        }
        assert_eq!(
            pack(b">f<d", &[A::N(1.0), A::I(0).into_num()]).unwrap(),
            [0x3f, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    impl A<'_> {
        fn into_num(self) -> Self {
            match self {
                A::I(n) => A::N(n as f64),
                other => other,
            }
        }
    }

    #[test]
    fn strings_pack_as_c_does() {
        assert_eq!(pack(b"c4", &[A::S(b"ab")]).unwrap(), b"ab\0\0");
        assert_eq!(
            pack(b"c1", &[A::S(b"ab")]),
            Err(PackError::StringLonger { arg: 2 })
        );
        assert_eq!(pack(b">s2", &[A::S(b"ab")]).unwrap(), b"\0\x02ab");
        assert_eq!(
            pack(b"s1", &[A::S(&[b'a'; 256])]),
            Err(PackError::LengthDoesNotFit { arg: 2 })
        );
        assert_eq!(pack(b"zz", &[A::S(b"a"), A::S(b"")]).unwrap(), b"a\0\0");
        assert_eq!(
            pack(b"z", &[A::S(b"a\0")]),
            Err(PackError::ContainsZeros { arg: 2 })
        );
    }

    #[test]
    fn strings_unpack_as_ranges_of_the_data() {
        assert_eq!(unpack(b"c2", b"abc"), Ok(vec![(2, 0, 2), (3, 3, 0)]));
        assert_eq!(unpack(b"s1", b"\x02abc"), Ok(vec![(2, 1, 3), (3, 4, 0)]));
        assert_eq!(unpack(b"s1", b"\x04abc"), Err(PackError::DataTooShort));
        assert_eq!(
            unpack(b"zb", b"ab\0c"),
            Ok(vec![(2, 0, 2), (0, 99, 0), (3, 5, 0)])
        );
        assert_eq!(unpack(b"z", b"ab"), Err(PackError::UnfinishedZ));
        assert_eq!(unpack(b"i4", b"abc"), Err(PackError::DataTooShort));
    }

    #[test]
    fn alignment_follows_maxalign_and_x() {
        assert_eq!(
            pack(b"!4bi4", &[A::I(1), A::I(2)]).unwrap(),
            [1, 0, 0, 0, 2, 0, 0, 0]
        );
        assert_eq!(pack(b"!bXi2b", &[A::I(1), A::I(2)]).unwrap(), [1, 0, 2]);
        assert_eq!(packsize(b"!2bXi8"), Ok(2));
        assert_eq!(packsize(b"bXc1"), Err(PackError::InvalidNextX));
        assert_eq!(packsize(b"bX"), Err(PackError::InvalidNextX));
        assert_eq!(packsize(b"bX "), Err(PackError::InvalidNextX));
        assert_eq!(packsize(b"!i3"), Err(PackError::AlignNotPowerOf2));
        assert_eq!(packsize(b"i3"), Ok(3));
        // Unpack aligns on the absolute data position.
        assert_eq!(
            unpack_with(b"!2h", b"\0\0\x01\x02", 1, i64::MAX / 2).0,
            Ok(vec![(0, 0x201, 0), (3, 5, 0)])
        );
    }

    #[test]
    fn format_errors_match_c() {
        assert_eq!(packsize(b"y"), Err(PackError::InvalidOption(b'y')));
        assert_eq!(packsize(b"i0"), Err(PackError::SizeOutOfLimits(0)));
        assert_eq!(packsize(b"i17"), Err(PackError::SizeOutOfLimits(17)));
        assert_eq!(
            packsize(b"!99999999999"),
            Err(PackError::SizeOutOfLimits(999_999_999))
        );
        assert_eq!(packsize(b"i00004"), Ok(4));
        assert_eq!(packsize(b"c"), Err(PackError::MissingCSize));
        assert_eq!(packsize(b"i\0garbage"), Ok(4));
        assert_eq!(
            PackError::SizeOutOfLimits(17).message(),
            b"integral size (17) out of limits [1,16]"
        );
        assert_eq!(PackError::InvalidNextX.arg(), Some(1));
    }

    #[test]
    fn packsize_rejects_variable_and_huge_formats() {
        assert_eq!(packsize(b"bs"), Err(PackError::VariableLength));
        assert_eq!(packsize(b"z"), Err(PackError::VariableLength));
        assert_eq!(packsize(b"c2147483639c8"), Ok(2147483647));
        assert_eq!(packsize(b"c2147483639c9"), Err(PackError::ResultTooLarge));
    }

    #[test]
    fn output_limit_is_checked_before_growing() {
        let mut p = Packer::new();
        let mut out = vec![7; 3];
        let mut budget = 100;
        assert_eq!(
            p.next(b"c4", &mut out, 6, &mut budget),
            PackStep::Need {
                arg: 2,
                need: Need::String
            }
        );
        assert_eq!(
            p.give_string(b"c4", b"a", &mut out, 6),
            Err(PackError::TooLarge)
        );
        assert_eq!(out, [7; 3]);
        let mut p = Packer::new();
        assert_eq!(
            p.next(b"xxxx", &mut out, 6, &mut budget),
            PackStep::Error(PackError::TooLarge)
        );
        assert_eq!(out, [7, 7, 7, 0, 0, 0]);
    }

    #[test]
    fn a_give_of_the_wrong_kind_is_refused() {
        let mut p = Packer::new();
        let mut out = Vec::new();
        let mut budget = 100;
        assert_eq!(
            p.give_integer(b"i", 1, &mut out, 99),
            Err(PackError::Protocol)
        );
        p.next(b"i", &mut out, 99, &mut budget);
        assert_eq!(
            p.give_string(b"i", b"", &mut out, 99),
            Err(PackError::Protocol)
        );
        assert_eq!(p.give_integer(b"i", 1, &mut out, 99), Ok(()));
    }

    #[test]
    fn resumption_with_budget_one_matches_the_straight_run() {
        let big = vec![b'a'; 300];
        let packs: [(&[u8], Vec<A>); 4] = [
            (b" <!4 b Xi4 i3 >h", vec![A::I(1), A::I(-5), A::I(300)]),
            (
                b"c200 s2 z f d",
                vec![A::S(b"ab"), A::S(&big), A::S(b"xyz"), A::N(0.1), A::N(-2.5)],
            ),
            (b"x x i16 j", vec![A::I(-1), A::I(i64::MIN)]),
            (
                b"bbbbb",
                vec![A::I(1), A::I(2), A::I(3), A::I(999), A::I(5)],
            ),
        ];
        for (fmt, args) in &packs {
            let straight = pack_with(fmt, args, i64::MAX / 2);
            for step in [1, 2, 3, 7] {
                assert_eq!(pack_with(fmt, args, step), straight, "{step}");
            }
            let data = pack(fmt, args).unwrap_or_default();
            let straight = unpack_with(fmt, &data, 0, i64::MAX / 2);
            for step in [1, 2, 5] {
                assert_eq!(unpack_with(fmt, &data, 0, step), straight);
            }
            let straight_size = {
                let mut b = i64::MAX / 2;
                (SizeCounter::new().run(fmt, &mut b), i64::MAX / 2 - b)
            };
            let mut c = SizeCounter::new();
            let (mut budget, mut used) = (1, 0);
            let result = loop {
                let before = budget;
                let r = c.run(fmt, &mut budget);
                used += before - budget;
                let mut words = Vec::new();
                c.encode(&mut words);
                c = SizeCounter::decode(&mut &words[..], fmt.len()).unwrap();
                match r {
                    Ok(None) => budget += 1,
                    other => break other,
                }
            };
            assert_eq!((result, used), straight_size);
        }
        // A staged value survives a snapshot and is given again.
        let mut u = Unpacker::new(0);
        let mut budget = 10;
        assert_eq!(
            u.next(b"bb", b"\x05\x06", &mut budget),
            UnpackStep::Value(Unpacked::Integer(5))
        );
        let mut words = Vec::new();
        u.encode(&mut words);
        let mut u = Unpacker::decode(&mut &words[..], 2, 2).unwrap();
        assert_eq!(
            u.next(b"bb", b"\x05\x06", &mut budget),
            UnpackStep::Value(Unpacked::Integer(5))
        );
        u.accept();
        assert_eq!(
            u.next(b"bb", b"\x05\x06", &mut budget),
            UnpackStep::Value(Unpacked::Integer(6))
        );
    }

    #[test]
    fn decode_fuzz_never_panics() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let fmt = b"!4bXi4 c3 s1 z i16 >d f";
        let data = b"\x01\0\0\0\x02\0\0\0abc\x01xz\0";
        for _ in 0..200_000 {
            let n = (rand() % 16) as usize;
            let words: Vec<u64> = (0..n)
                .map(|_| match rand() % 4 {
                    0 => rand(),
                    1 => rand() % 3,
                    _ => rand() % 40,
                })
                .collect();
            let mut out = Vec::new();
            let mut budget = 50;
            if let Some(mut p) = Packer::decode(&mut &words[..], fmt.len()) {
                let _ = p.next(fmt, &mut out, 1 << 12, &mut budget);
                let _ = p.give_string(fmt, b"ab", &mut out, 1 << 12);
                let _ = p.give_integer(fmt, -1, &mut out, 1 << 12);
                let _ = p.give_number(fmt, 1.5, &mut out, 1 << 12);
                let _ = p.next(fmt, &mut out, 1 << 12, &mut budget);
            }
            if let Some(mut c) = SizeCounter::decode(&mut &words[..], fmt.len()) {
                let _ = c.run(fmt, &mut budget);
            }
            if let Some(mut u) = Unpacker::decode(&mut &words[..], fmt.len(), data.len()) {
                for _ in 0..4 {
                    let _ = u.next(fmt, data, &mut budget);
                    u.accept();
                }
            }
        }
    }
}
