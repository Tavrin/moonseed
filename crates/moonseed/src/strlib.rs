//! The `string` library's functions and their resumable state (ADR 0034).
//!
//! The VM side is `runtime/string.rs`. Lua strings are bytes: nothing here
//! reads UTF-8, and case and character classes follow the C locale.

use crate::heap::STRING_CEILING;
use crate::host::{Builtin, HostRegistry};
use crate::id::SnapshotError;
use crate::opcode::{read_i64, read_u8, read_u32, read_u64};

/// A `string` function, or one of the string metatable's arithmetic
/// metamethods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrFn {
    Byte,
    Char,
    Dump,
    Find,
    Format,
    Gmatch,
    /// The iterator `gmatch` returns: a native closure (ADR 0035).
    GmatchStep,
    Gsub,
    Len,
    Lower,
    Match,
    Pack,
    PackSize,
    Rep,
    Reverse,
    Sub,
    Unpack,
    Upper,
    /// `__add` and the others in the string metatable: string operands
    /// convert to numbers, as `lstrlib.c`'s `arith` does.
    Arith(StrArith),
}

/// The arithmetic events the string metatable answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrArith {
    Add,
    Sub,
    Mul,
    Mod,
    Pow,
    Div,
    Idiv,
    Unm,
}

/// The `string` functions: field name, registry symbol, function.
pub(crate) const STRING_FUNCTIONS: [(&str, &str, StrFn); 17] = [
    ("byte", "string.byte", StrFn::Byte),
    ("char", "string.char", StrFn::Char),
    ("dump", "string.dump", StrFn::Dump),
    ("find", "string.find", StrFn::Find),
    ("format", "string.format", StrFn::Format),
    ("gmatch", "string.gmatch", StrFn::Gmatch),
    ("gsub", "string.gsub", StrFn::Gsub),
    ("len", "string.len", StrFn::Len),
    ("lower", "string.lower", StrFn::Lower),
    ("match", "string.match", StrFn::Match),
    ("pack", "string.pack", StrFn::Pack),
    ("packsize", "string.packsize", StrFn::PackSize),
    ("rep", "string.rep", StrFn::Rep),
    ("reverse", "string.reverse", StrFn::Reverse),
    ("sub", "string.sub", StrFn::Sub),
    ("unpack", "string.unpack", StrFn::Unpack),
    ("upper", "string.upper", StrFn::Upper),
];

/// The string metatable's arithmetic metamethods: event, registry symbol,
/// operation (`stringmetamethods` in `lstrlib.c`).
pub(crate) const STRING_ARITH: [(&str, &str, StrArith); 8] = [
    ("__add", "string.__add", StrArith::Add),
    ("__sub", "string.__sub", StrArith::Sub),
    ("__mul", "string.__mul", StrArith::Mul),
    ("__mod", "string.__mod", StrArith::Mod),
    ("__pow", "string.__pow", StrArith::Pow),
    ("__div", "string.__div", StrArith::Div),
    ("__idiv", "string.__idiv", StrArith::Idiv),
    ("__unm", "string.__unm", StrArith::Unm),
];

/// The symbol of `gmatch`'s iterator.
pub(crate) const GMATCH_STEP: &str = "string.gmatch.step";

/// Register the `string` functions, the string metatable's metamethods,
/// and `gmatch`'s iterator.
pub fn register_string(registry: &mut HostRegistry) {
    for (_, symbol, function) in STRING_FUNCTIONS {
        registry.register_builtin(symbol, Builtin::String(function));
    }
    for (_, symbol, op) in STRING_ARITH {
        registry.register_builtin(symbol, Builtin::String(StrFn::Arith(op)));
    }
    registry.register_builtin(GMATCH_STEP, Builtin::String(StrFn::GmatchStep));
}

/// Output bytes a stepped string function makes per step: the unit of its
/// fuel. A performance choice, not semantics.
pub(crate) const BYTE_BATCH: usize = 4096;

/// Lua's `posrelatI`: a start position, 1-based, from a relative one.
/// Never below 1; may pass the end.
pub(crate) fn start_position(pos: i64, len: usize) -> u64 {
    if pos > 0 {
        pos as u64
    } else if pos == 0 || pos.unsigned_abs() > len as u64 {
        1
    } else {
        (len as i64 + pos + 1) as u64
    }
}

/// Lua's `getendpos`: an end position, 1-based and inclusive, clipped to
/// `0..=len`.
pub(crate) fn end_position(pos: i64, len: usize) -> u64 {
    if pos > len as i64 {
        len as u64
    } else if pos >= 0 {
        pos as u64
    } else if pos.unsigned_abs() > len as u64 {
        0
    } else {
        (len as i64 + pos + 1) as u64
    }
}

/// What a string function built byte by byte copies from its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Build {
    /// `sub`: the subject from byte `start`, 0-based.
    Sub {
        start: u32,
    },
    Reverse,
    Lower,
    Upper,
    /// `rep`: the subject, `len` bytes, repeated, with the separator,
    /// `sep` bytes, between copies.
    Rep {
        len: u32,
        sep: u32,
    },
}

/// A string function's state between steps (ADR 0034). Values stay on
/// the stack: in the arguments, converted to strings in place, and in
/// scratch slots.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StrWork {
    /// A result of `total` bytes, built [`BYTE_BATCH`] bytes a step; `out`
    /// is charged to the logical heap as it grows.
    Build {
        build: Build,
        total: u32,
        out: Vec<u8>,
    },
    /// A string metamethod calling the second operand's metamethod
    /// (`trymt`); its result goes to scratch 0.
    Arith { op: StrArith, called: bool },
    /// `string.format`: the text so far, charged to the logical heap.
    /// `waiting`: a `__tostring` call for `%s` is out, its result to go
    /// to scratch 0.
    Format {
        formatter: Box<crate::strformat::Formatter>,
        out: Vec<u8>,
        waiting: bool,
        debt: i64,
    },
    /// `string.pack`: the bytes so far, charged to the logical heap.
    Pack {
        packer: Box<crate::strpack::Packer>,
        out: Vec<u8>,
        debt: i64,
    },
    PackSize {
        counter: Box<crate::strpack::SizeCounter>,
        debt: i64,
    },
    /// `string.unpack`: `count` results so far, in scratch slots.
    Unpack {
        unpacker: Box<crate::strpack::Unpacker>,
        count: u32,
        debt: i64,
    },
    /// `string.find` (`find`) or `string.match`.
    Find { find: bool, seek: Seek, debt: i64 },
    /// A call of `gmatch`'s iterator, the native closure being called,
    /// which holds the subject and the pattern (ADR 0035).
    GmatchStep {
        gmatch: Box<crate::strpat::Gmatch>,
        debt: i64,
    },
    /// `string.gsub`: the result so far, charged to the logical heap.
    /// `site`: the match whose replacement a Lua call or index is
    /// producing, into scratch 0.
    Gsub {
        engine: Box<crate::strpat::Gsub>,
        out: Vec<u8>,
        changed: bool,
        site: Option<(u32, u32)>,
        debt: i64,
    },
}

/// How `find` searches: for the bytes of the pattern, or for the pattern.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Seek {
    Plain(Box<crate::strpat::PlainSearch>),
    Pattern(Box<crate::strpat::Search>),
}

/// A string a string function reads: its argument, or a value of the
/// native closure being called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Arg(u32),
    Closure(u32),
}

/// Work units a string engine (patterns, `format`, `pack`) does per step,
/// on top of the debt a bulk operation left (ADR 0034). A performance
/// choice, not semantics.
pub(crate) const ENGINE_BUDGET: i64 = 256;

impl StrWork {
    /// Scratch slots the work keeps above the arguments.
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Build { .. }
            | Self::Pack { .. }
            | Self::PackSize { .. }
            | Self::Find { .. }
            | Self::GmatchStep { .. } => 0,
            Self::Arith { .. } | Self::Format { .. } | Self::Gsub { .. } => 1,
            Self::Unpack { count, .. } => *count,
        }
    }

    /// Bytes the work holds outside the heap, charged to it.
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Build { out, .. }
            | Self::Format { out, .. }
            | Self::Pack { out, .. }
            | Self::Gsub { out, .. } => out.len(),
            Self::Arith { .. }
            | Self::PackSize { .. }
            | Self::Unpack { .. }
            | Self::Find { .. }
            | Self::GmatchStep { .. } => 0,
        }
    }
}

impl StrWork {
    /// The work as snapshot bytes (ADR 0034): a tag, then its fields.
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        let bytes = |out: &mut Vec<u8>, bytes: &[u8]| {
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        let words = |out: &mut Vec<u8>, encode: &dyn Fn(&mut Vec<u64>)| {
            let mut words = Vec::new();
            encode(&mut words);
            out.extend((words.len() as u32).to_le_bytes());
            for word in words {
                out.extend(word.to_le_bytes());
            }
        };
        match self {
            Self::Build {
                build,
                total,
                out: text,
            } => {
                out.push(1);
                match build {
                    Build::Sub { start } => {
                        out.push(1);
                        out.extend(start.to_le_bytes());
                    }
                    Build::Reverse => out.push(2),
                    Build::Lower => out.push(3),
                    Build::Upper => out.push(4),
                    Build::Rep { len, sep } => {
                        out.push(5);
                        out.extend(len.to_le_bytes());
                        out.extend(sep.to_le_bytes());
                    }
                }
                out.extend(total.to_le_bytes());
                bytes(out, text);
            }
            Self::Arith { op, called } => {
                out.push(2);
                out.push(*op as u8);
                out.push(u8::from(*called));
            }
            Self::Format {
                formatter,
                out: text,
                waiting,
                debt,
            } => {
                out.push(3);
                words(out, &|words| formatter.encode(words));
                bytes(out, text);
                out.push(u8::from(*waiting));
                out.extend(debt.to_le_bytes());
            }
            Self::Pack {
                packer,
                out: text,
                debt,
            } => {
                out.push(4);
                words(out, &|words| packer.encode(words));
                bytes(out, text);
                out.extend(debt.to_le_bytes());
            }
            Self::PackSize { counter, debt } => {
                out.push(5);
                words(out, &|words| counter.encode(words));
                out.extend(debt.to_le_bytes());
            }
            Self::Unpack {
                unpacker,
                count,
                debt,
            } => {
                out.push(6);
                words(out, &|words| unpacker.encode(words));
                out.extend(count.to_le_bytes());
                out.extend(debt.to_le_bytes());
            }
            Self::Find { find, seek, debt } => {
                out.push(7);
                out.push(u8::from(*find));
                // 1 for a plain search, as `decode` reads it.
                match seek {
                    Seek::Plain(search) => {
                        out.push(1);
                        words(out, &|words| search.encode(words));
                    }
                    Seek::Pattern(search) => {
                        out.push(0);
                        words(out, &|words| search.encode(words));
                    }
                }
                out.extend(debt.to_le_bytes());
            }
            Self::GmatchStep { gmatch, debt } => {
                out.push(8);
                words(out, &|words| gmatch.encode(words));
                out.extend(debt.to_le_bytes());
            }
            Self::Gsub {
                engine,
                out: text,
                changed,
                site,
                debt,
            } => {
                out.push(9);
                words(out, &|words| engine.encode(words));
                bytes(out, text);
                out.push(u8::from(*changed));
                match site {
                    None => out.push(0),
                    Some((start, end)) => {
                        out.push(1);
                        out.extend(start.to_le_bytes());
                        out.extend(end.to_le_bytes());
                    }
                }
                out.extend(debt.to_le_bytes());
            }
        }
    }

    /// Read what [`Self::encode`] wrote. Bounds wait for [`Self::fits`].
    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let flag = |input: &mut &[u8]| match read_u8(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::InvalidTag),
        };
        let bytes = |input: &mut &[u8]| -> Result<Vec<u8>, SnapshotError> {
            let len = read_u32(input)? as usize;
            if len > STRING_CEILING || len > input.len() {
                return Err(SnapshotError::Truncated);
            }
            let (head, rest) = input.split_at(len);
            *input = rest;
            Ok(head.to_vec())
        };
        // An engine's words, decoded against the largest lengths: `fits`
        // checks them again against the real ones.
        let engine = |input: &mut &[u8]| -> Result<Vec<u64>, SnapshotError> {
            let count = read_u32(input)? as usize;
            if count > MAX_ENGINE_WORDS {
                return Err(SnapshotError::LimitExceeded);
            }
            (0..count).map(|_| read_u64(input)).collect()
        };
        const ANY: u32 = u32::MAX;
        let bad = || SnapshotError::InvalidStructure;
        Ok(match read_u8(input)? {
            1 => {
                let build = match read_u8(input)? {
                    1 => Build::Sub {
                        start: read_u32(input)?,
                    },
                    2 => Build::Reverse,
                    3 => Build::Lower,
                    4 => Build::Upper,
                    5 => Build::Rep {
                        len: read_u32(input)?,
                        sep: read_u32(input)?,
                    },
                    _ => return Err(SnapshotError::InvalidTag),
                };
                let total = read_u32(input)?;
                Self::Build {
                    build,
                    total,
                    out: bytes(input)?,
                }
            }
            2 => Self::Arith {
                op: *STRING_ARITH
                    .get(usize::from(read_u8(input)?))
                    .map(|(_, _, op)| op)
                    .ok_or(SnapshotError::InvalidTag)?,
                called: flag(input)?,
            },
            3 => {
                let words = engine(input)?;
                // The formatter keeps the argument count it is decoded
                // with: its own, which `fits` checks against the call's.
                let nargs = words
                    .get(1)
                    .and_then(|nargs| u32::try_from(*nargs).ok())
                    .ok_or_else(bad)?;
                let formatter = crate::strformat::Formatter::decode(&mut &words[..], ANY, nargs)
                    .ok_or_else(bad)?;
                Self::Format {
                    formatter: Box::new(formatter),
                    out: bytes(input)?,
                    waiting: flag(input)?,
                    debt: read_i64(input)?,
                }
            }
            4 => {
                let words = engine(input)?;
                let packer = crate::strpack::Packer::decode(&mut &words[..], ANY as usize)
                    .ok_or_else(bad)?;
                Self::Pack {
                    packer: Box::new(packer),
                    out: bytes(input)?,
                    debt: read_i64(input)?,
                }
            }
            5 => {
                let words = engine(input)?;
                let counter = crate::strpack::SizeCounter::decode(&mut &words[..], ANY as usize)
                    .ok_or_else(bad)?;
                Self::PackSize {
                    counter: Box::new(counter),
                    debt: read_i64(input)?,
                }
            }
            6 => {
                let words = engine(input)?;
                let unpacker =
                    crate::strpack::Unpacker::decode(&mut &words[..], ANY as usize, ANY as usize)
                        .ok_or_else(bad)?;
                Self::Unpack {
                    unpacker: Box::new(unpacker),
                    count: read_u32(input)?,
                    debt: read_i64(input)?,
                }
            }
            7 => {
                let find = flag(input)?;
                let plain = flag(input)?;
                let words = engine(input)?;
                let words = &mut &words[..];
                let seek = if plain {
                    Seek::Plain(Box::new(
                        crate::strpat::PlainSearch::decode(words, ANY, ANY).ok_or_else(bad)?,
                    ))
                } else {
                    Seek::Pattern(Box::new(
                        crate::strpat::Search::decode(words, ANY, ANY).ok_or_else(bad)?,
                    ))
                };
                Self::Find {
                    find,
                    seek,
                    debt: read_i64(input)?,
                }
            }
            8 => {
                let words = engine(input)?;
                Self::GmatchStep {
                    gmatch: Box::new(
                        crate::strpat::Gmatch::decode(&mut &words[..], ANY - 1, ANY)
                            .ok_or_else(bad)?,
                    ),
                    debt: read_i64(input)?,
                }
            }
            9 => {
                let words = engine(input)?;
                let engine =
                    crate::strpat::Gsub::decode(&mut &words[..], ANY - 1, ANY).ok_or_else(bad)?;
                let out = bytes(input)?;
                let changed = flag(input)?;
                let site = if flag(input)? {
                    Some((read_u32(input)?, read_u32(input)?))
                } else {
                    None
                };
                Self::Gsub {
                    engine: Box::new(engine),
                    out,
                    changed,
                    site,
                    debt: read_i64(input)?,
                }
            }
            _ => return Err(SnapshotError::InvalidTag),
        })
    }

    /// Whether restore may continue this work (ADR 0034): the arguments it
    /// reads are strings of the lengths its counters assume, and the
    /// counters stay within them. `string(i)` is the length of argument
    /// `i` when it is a string.
    pub(crate) fn fits(&self, passed: u32, source: &dyn Fn(Source) -> Option<usize>) -> bool {
        let string = |index| source(Source::Arg(index));
        match self {
            Self::Build { build, total, out } => {
                let total = *total as usize;
                let Some(len) = string(0) else {
                    return false;
                };
                let shape = match *build {
                    Build::Sub { start } => (start as usize).saturating_add(total) <= len,
                    Build::Reverse | Build::Lower | Build::Upper => total == len,
                    Build::Rep { len: copy, sep } => {
                        let sep_fits = match string(2) {
                            Some(given) => given == sep as usize,
                            None => sep == 0,
                        };
                        let period = copy as usize + sep as usize;
                        copy as usize == len
                            && sep_fits
                            && total > 0
                            && period > 0
                            && (total + sep as usize).is_multiple_of(period)
                    }
                };
                shape && out.len() < total && total <= STRING_CEILING
            }
            // A metamethod's call is made in the step that starts it.
            Self::Arith { called, .. } => *called && passed >= 1,
            Self::Format {
                formatter,
                out,
                debt,
                ..
            } => {
                let Some(fmt) = string(0) else {
                    return false;
                };
                let mut words = Vec::new();
                formatter.encode(&mut words);
                crate::strformat::Formatter::decode(
                    &mut &words[..],
                    fmt as u32,
                    passed.saturating_sub(1),
                )
                .is_some()
                    && out.len() <= STRING_CEILING
                    && debt_fits(*debt)
            }
            Self::Pack { packer, out, debt } => {
                let Some(fmt) = string(0) else {
                    return false;
                };
                let mut words = Vec::new();
                packer.encode(&mut words);
                crate::strpack::Packer::decode(&mut &words[..], fmt).is_some()
                    && out.len() <= STRING_CEILING
                    && debt_fits(*debt)
            }
            Self::PackSize { counter, debt } => {
                let Some(fmt) = string(0) else {
                    return false;
                };
                let mut words = Vec::new();
                counter.encode(&mut words);
                crate::strpack::SizeCounter::decode(&mut &words[..], fmt).is_some()
                    && debt_fits(*debt)
            }
            Self::Unpack {
                unpacker,
                count,
                debt,
            } => {
                let (Some(fmt), Some(data)) = (string(0), string(1)) else {
                    return false;
                };
                let mut words = Vec::new();
                unpacker.encode(&mut words);
                crate::strpack::Unpacker::decode(&mut &words[..], fmt, data).is_some()
                    && *count <= MAX_STACK_RESULTS
                    && debt_fits(*debt)
            }
            Self::Find { seek, debt, .. } => {
                let (Some(subject), Some(pattern)) = (string(0), string(1)) else {
                    return false;
                };
                let (subject, pattern) = (subject as u32, pattern as u32);
                let mut words = Vec::new();
                let decoded = match seek {
                    Seek::Plain(search) => {
                        search.encode(&mut words);
                        crate::strpat::PlainSearch::decode(&mut &words[..], subject, pattern)
                            .is_some()
                    }
                    Seek::Pattern(search) => {
                        search.encode(&mut words);
                        crate::strpat::Search::decode(&mut &words[..], subject, pattern).is_some()
                    }
                };
                decoded && debt_fits(*debt)
            }
            Self::GmatchStep { gmatch, debt } => {
                let (Some(subject), Some(pattern)) =
                    (source(Source::Closure(0)), source(Source::Closure(1)))
                else {
                    return false;
                };
                let mut words = Vec::new();
                gmatch.encode(&mut words);
                crate::strpat::Gmatch::decode(&mut &words[..], subject as u32, pattern as u32)
                    .is_some()
                    && debt_fits(*debt)
            }
            Self::Gsub {
                engine,
                out,
                site,
                debt,
                ..
            } => {
                let (Some(subject), Some(pattern)) = (string(0), string(1)) else {
                    return false;
                };
                let mut words = Vec::new();
                engine.encode(&mut words);
                crate::strpat::Gsub::decode(&mut &words[..], subject as u32, pattern as u32)
                    .is_some()
                    && site.is_none_or(|(start, end)| start <= end && end as usize <= subject)
                    && out.len() <= STRING_CEILING
                    && debt_fits(*debt)
            }
        }
    }
}

/// Engine state words a snapshot may hold for one string function.
const MAX_ENGINE_WORDS: usize = 4096;

/// The most results a string function keeps in scratch slots: the
/// largest stack bound (`runtime::STACK_SLOTS_RANGE`).
const MAX_STACK_RESULTS: u32 = *crate::runtime::STACK_SLOTS_RANGE.end();

/// A debt a bulk operation can leave: at most one string's worth.
fn debt_fits(debt: i64) -> bool {
    (-(STRING_CEILING as i64)..=0).contains(&debt)
}

/// Append the next part of a built result: at most [`BYTE_BATCH`]
/// bytes, from `subject` (argument 1) and `sep` (`rep`'s separator).
/// Returns whether the result is complete. The caller checked `total`
/// against the string limit and the quota before the first part.
pub(crate) fn build_part(build: Build, total: u32, subject: &[u8], sep: &[u8], out: &mut Vec<u8>) {
    let done = out.len();
    let end = (done + BYTE_BATCH).min(total as usize);
    match build {
        Build::Sub { start } => {
            let from = start as usize + done;
            let to = start as usize + end;
            out.extend_from_slice(subject.get(from..to).unwrap_or_default());
        }
        Build::Reverse => {
            // Output bytes `done..end` are subject bytes `len - end..len - done`,
            // backward.
            let len = subject.len();
            let part = subject
                .get(len.saturating_sub(end)..len.saturating_sub(done))
                .unwrap_or_default();
            out.extend(part.iter().rev());
        }
        Build::Lower => {
            out.extend(
                subject
                    .get(done..end)
                    .unwrap_or_default()
                    .iter()
                    .map(|b| crate::strpat::to_lower(*b)),
            );
        }
        Build::Upper => {
            out.extend(
                subject
                    .get(done..end)
                    .unwrap_or_default()
                    .iter()
                    .map(|b| crate::strpat::to_upper(*b)),
            );
        }
        Build::Rep { len, sep: sep_len } => {
            let period = len as usize + sep_len as usize;
            let mut at = done;
            while at < end {
                let offset = at % period;
                let (source, from) = if offset < len as usize {
                    (subject, offset)
                } else {
                    (sep, offset - len as usize)
                };
                let piece_end = if offset < len as usize {
                    len as usize
                } else {
                    sep_len as usize
                };
                let take = (piece_end - from).min(end - at);
                out.extend_from_slice(source.get(from..from + take).unwrap_or_default());
                at += take;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_follow_posrelat_and_getendpos() {
        // posrelatI
        assert_eq!(start_position(3, 5), 3);
        assert_eq!(start_position(0, 5), 1);
        assert_eq!(start_position(-1, 5), 5);
        assert_eq!(start_position(-5, 5), 1);
        assert_eq!(start_position(-6, 5), 1);
        assert_eq!(start_position(i64::MIN, 5), 1);
        assert_eq!(start_position(i64::MAX, 5), i64::MAX as u64);
        assert_eq!(start_position(-1, 0), 1);
        // getendpos
        assert_eq!(end_position(9, 5), 5);
        assert_eq!(end_position(0, 5), 0);
        assert_eq!(end_position(-1, 5), 5);
        assert_eq!(end_position(-5, 5), 1);
        assert_eq!(end_position(-6, 5), 0);
        assert_eq!(end_position(i64::MIN, 5), 0);
        assert_eq!(end_position(i64::MAX, 5), 5);
    }

    #[test]
    fn builds_come_out_whole_across_parts() {
        let subject: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let run = |build: Build, total: u32, sep: &[u8]| {
            let mut out = Vec::new();
            while out.len() < total as usize {
                build_part(build, total, &subject, sep, &mut out);
            }
            out
        };
        let reversed: Vec<u8> = subject.iter().rev().copied().collect();
        assert_eq!(run(Build::Reverse, 10_000, b""), reversed);
        assert_eq!(
            run(Build::Sub { start: 7 }, 9_000, b""),
            subject[7..9_007].to_vec()
        );
        let upper: Vec<u8> = subject.iter().map(u8::to_ascii_uppercase).collect();
        assert_eq!(run(Build::Upper, 10_000, b""), upper);
        let short = &subject[..3];
        let mut expected = Vec::new();
        for copy in 0..2000 {
            if copy > 0 {
                expected.extend_from_slice(b", ");
            }
            expected.extend_from_slice(short);
        }
        let mut out = Vec::new();
        while out.len() < expected.len() {
            build_part(
                Build::Rep { len: 3, sep: 2 },
                expected.len() as u32,
                short,
                b", ",
                &mut out,
            );
        }
        assert_eq!(out, expected);
    }
}
