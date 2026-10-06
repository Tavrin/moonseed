//! UTF-8 builtin identities and checkpointable bounded work.
use crate::host::{Builtin, HostRegistry};
use crate::id::SnapshotError;
use crate::opcode::{read_i64, read_u8, read_u32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Utf8Fn {
    Char,
    Len,
    Codepoint,
    Offset,
    Codes,
    StrictStep,
    LaxStep,
}
pub(crate) const FUNCTIONS: [(&str, &str, Utf8Fn); 5] = [
    ("char", "utf8.char", Utf8Fn::Char),
    ("len", "utf8.len", Utf8Fn::Len),
    ("codepoint", "utf8.codepoint", Utf8Fn::Codepoint),
    ("offset", "utf8.offset", Utf8Fn::Offset),
    ("codes", "utf8.codes", Utf8Fn::Codes),
];
pub(crate) const STRICT_STEP: &str = "utf8.strictstep";
pub(crate) const LAX_STEP: &str = "utf8.laxstep";
pub(crate) const CHARPATTERN: &[u8] = b"[\0-\x7f\xc2-\xfd][\x80-\xbf]*";
/// Register UTF-8 functions and the two stable iterator values.
pub fn register_utf8(registry: &mut HostRegistry) {
    for (_, symbol, function) in FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Utf8(function));
    }
    registry.register_builtin(STRICT_STEP, Builtin::Utf8(Utf8Fn::StrictStep));
    registry.register_builtin(LAX_STEP, Builtin::Utf8(Utf8Fn::LaxStep));
}
/// At most 256 sequences or arguments per step; navigation reads at most
/// 4096 bytes per step, as string-library bulk work does.
pub(crate) const SEQUENCES: usize = 256;
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Utf8Work {
    Char {
        next: u32,
        out: Vec<u8>,
    },
    Scan {
        pos: u32,
        end: u32,
        count: u32,
        strict: bool,
        points: bool,
    },
    /// `seeking` finishes the continuation run of a boundary move.
    Offset {
        pos: u32,
        remaining: i64,
        direction: i64,
        seeking: bool,
    },
    Iterate {
        pos: u32,
        strict: bool,
    },
}
impl Utf8Work {
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Scan {
                count,
                points: true,
                ..
            } => *count,
            _ => 0,
        }
    }
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Char { out, .. } => out.len(),
            _ => 0,
        }
    }
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Char { next, out: bytes } => {
                out.push(1);
                out.extend(next.to_le_bytes());
                out.extend((bytes.len() as u32).to_le_bytes());
                out.extend(bytes);
            }
            Self::Scan {
                pos,
                end,
                count,
                strict,
                points,
            } => {
                out.push(2);
                for n in [pos, end, count] {
                    out.extend(n.to_le_bytes());
                }
                out.extend([u8::from(*strict), u8::from(*points)]);
            }
            Self::Offset {
                pos,
                remaining,
                direction,
                seeking,
            } => {
                out.push(3);
                out.extend(pos.to_le_bytes());
                out.extend(remaining.to_le_bytes());
                out.extend(direction.to_le_bytes());
                out.push(u8::from(*seeking));
            }
            Self::Iterate { pos, strict } => {
                out.push(4);
                out.extend(pos.to_le_bytes());
                out.push(u8::from(*strict));
            }
        }
    }
    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let flag = |input: &mut &[u8]| match read_u8(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::InvalidTag),
        };
        Ok(match read_u8(input)? {
            1 => {
                let next = read_u32(input)?;
                let len = read_u32(input)? as usize;
                if len > crate::heap::STRING_CEILING || len > input.len() {
                    return Err(SnapshotError::Truncated);
                }
                let (bytes, rest) = input.split_at(len);
                *input = rest;
                Self::Char {
                    next,
                    out: bytes.to_vec(),
                }
            }
            2 => Self::Scan {
                pos: read_u32(input)?,
                end: read_u32(input)?,
                count: read_u32(input)?,
                strict: flag(input)?,
                points: flag(input)?,
            },
            3 => Self::Offset {
                pos: read_u32(input)?,
                remaining: read_i64(input)?,
                direction: read_i64(input)?,
                seeking: flag(input)?,
            },
            4 => Self::Iterate {
                pos: read_u32(input)?,
                strict: flag(input)?,
            },
            _ => return Err(SnapshotError::InvalidTag),
        })
    }
    pub(crate) fn fits(&self, passed: u32, len: Option<usize>) -> bool {
        match self {
            Self::Char { next, out } => {
                *next <= passed
                    // A failed final allocation can leave the completed
                    // work with its buffer taken, while the error unwinds.
                    && (*next == passed || out.len() >= *next as usize)
                    && out.len() <= (*next as usize).saturating_mul(6)
                    && out.len() <= crate::heap::STRING_CEILING
            }
            Self::Scan {
                pos,
                end,
                count,
                points,
                ..
            } => len.is_some_and(|len| {
                passed >= 1
                    && *pos as usize <= len
                    && *end as usize <= len
                    && *pos <= end.saturating_add(5)
                    && count <= pos
                    && count <= end
                    && (!points || *count <= *crate::runtime::STACK_SLOTS_RANGE.end())
            }),
            Self::Offset {
                pos,
                remaining,
                direction,
                seeking,
            } => {
                passed >= 2
                    && len.is_some_and(|len| *pos as usize <= len)
                    && [-1, 0, 1].contains(direction)
                    && match direction {
                        -1 => *remaining <= 0,
                        0 => *remaining == 0,
                        _ => *remaining >= 0,
                    }
                    && (!seeking || (*direction != 0 && *remaining != 0))
            }
            Self::Iterate { pos, .. } => passed >= 1 && len.is_some_and(|len| *pos as usize <= len),
        }
    }
}
