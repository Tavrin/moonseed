//! File identity and checkpointed IO work. No ambient host access.
use crate::hostcaps::{HandlePolicy, OpenMode, ResourceId};
use crate::opcode::{read_i64, read_u8, read_u32, read_u64};
use crate::{HostRegistry, SnapshotError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IoFn {
    Open,
    Tmpfile,
    Popen,
    Input,
    Output,
    Type,
    Close,
    Flush,
    Read,
    Write,
    Lines,
    FileClose,
    FileFlush,
    FileRead,
    FileWrite,
    FileLines,
    Seek,
    Setvbuf,
    Gc,
    ToString,
    LinesStep,
}
pub(crate) const FUNCTIONS: [(&str, &str, IoFn); 11] = [
    ("open", "io.open", IoFn::Open),
    ("tmpfile", "io.tmpfile", IoFn::Tmpfile),
    ("popen", "io.popen", IoFn::Popen),
    ("input", "io.input", IoFn::Input),
    ("output", "io.output", IoFn::Output),
    ("type", "io.type", IoFn::Type),
    ("close", "io.close", IoFn::Close),
    ("flush", "io.flush", IoFn::Flush),
    ("read", "io.read", IoFn::Read),
    ("write", "io.write", IoFn::Write),
    ("lines", "io.lines", IoFn::Lines),
];
pub(crate) const METHODS: [(&str, &str, IoFn); 11] = [
    ("close", "file.close", IoFn::FileClose),
    ("flush", "file.flush", IoFn::FileFlush),
    ("read", "file.read", IoFn::FileRead),
    ("write", "file.write", IoFn::FileWrite),
    ("lines", "file.lines", IoFn::FileLines),
    ("seek", "file.seek", IoFn::Seek),
    ("setvbuf", "file.setvbuf", IoFn::Setvbuf),
    ("__gc", "file.__gc", IoFn::Gc),
    ("__close", "file.__close", IoFn::Gc),
    ("__tostring", "file.__tostring", IoFn::ToString),
    ("__name", "", IoFn::Type),
];
/// Register the IO functions and internal file methods; grants no authority.
pub fn register_io(registry: &mut HostRegistry) {
    for (_, symbol, f) in FUNCTIONS.into_iter().chain(METHODS) {
        if !symbol.is_empty() {
            registry.register_builtin(symbol, crate::host::Builtin::Io(f));
        }
    }
    registry.register_builtin("io.linesstep", crate::host::Builtin::Io(IoFn::LinesStep));
}
/// File, anonymous temp file, stdin, stdout, stderr, pipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileState {
    pub(crate) kind: u8,
    pub(crate) id: ResourceId,
    pub(crate) mode: OpenMode,
    pub(crate) policy: HandlePolicy,
    pub(crate) cursor: u64,
    pub(crate) closed: bool,
    pub(crate) buffering: u8,
    pub(crate) write_capacity: u32,
    pub(crate) write_buffer: Vec<u8>,
    pub(crate) write_flush: u32,
    pub(crate) write_next: bool,
    pub(crate) lookahead: Option<u8>,
    pub(crate) eof: bool,
    // Cursor counts logical bytes, not read-ahead. read_pos is the consumed
    // prefix; even that prefix stays canonical until the bounded refill.
    pub(crate) read_buffer: Vec<u8>,
    pub(crate) read_pos: u32,
}
pub(crate) const FILE_CHARGE: u64 = 96;
pub(crate) const READ_CHUNK: usize = 16384;
// PUC 5.4.9 on the frozen Linux oracle calls setvbuf with NULL. libc
// allocates the filesystem block size, ignoring the requested size.
pub(crate) const WRITE_CAPACITY: u32 = 4096;
impl FileState {
    /// Only the scalar operation parameters; never copy read-ahead per line.
    pub(crate) fn parameters(&self) -> Self {
        Self {
            kind: self.kind,
            id: self.id,
            mode: self.mode,
            policy: self.policy,
            cursor: self.cursor,
            closed: self.closed,
            buffering: self.buffering,
            write_capacity: self.write_capacity,
            write_buffer: Vec::new(),
            write_flush: self.write_flush,
            write_next: self.write_next,
            lookahead: self.lookahead,
            eof: self.eof,
            read_buffer: Vec::new(),
            read_pos: 0,
        }
    }
    pub(crate) fn charge(&self) -> u64 {
        FILE_CHARGE + self.read_buffer.len() as u64 + self.write_buffer.len() as u64
    }
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.extend(self.id.0.to_le_bytes());
        out.extend([
            self.mode.read as u8,
            self.mode.write as u8,
            self.mode.append as u8,
            self.mode.create as u8,
            self.mode.truncate as u8,
            self.mode.binary as u8,
        ]);
        out.push(if self.policy == HandlePolicy::Rebind {
            0
        } else {
            1
        });
        out.extend(self.cursor.to_le_bytes());
        out.extend([self.closed as u8, self.buffering | 128]);
        out.push(self.lookahead.is_some() as u8);
        out.push(self.lookahead.unwrap_or(0));
        out.push(self.eof as u8);
        out.extend(self.read_pos.to_le_bytes());
        out.extend((self.read_buffer.len() as u32).to_le_bytes());
        out.extend(&self.read_buffer);
        out.extend(self.write_capacity.to_le_bytes());
        out.extend(self.write_flush.to_le_bytes());
        out.push(self.write_next as u8);
        out.extend((self.write_buffer.len() as u32).to_le_bytes());
        out.extend(&self.write_buffer);
    }
    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let kind = read_u8(input)?;
        let id = ResourceId(read_u64(input)?);
        let mode = OpenMode {
            read: flag(input)?,
            write: flag(input)?,
            append: flag(input)?,
            create: flag(input)?,
            truncate: flag(input)?,
            binary: flag(input)?,
        };
        let policy = if flag(input)? {
            HandlePolicy::Refuse
        } else {
            HandlePolicy::Rebind
        };
        let cursor = read_u64(input)?;
        let closed = flag(input)?;
        let tag = read_u8(input)?;
        let buffering = tag & 127;
        let has = flag(input)?;
        let byte = read_u8(input)?;
        let eof = flag(input)?;
        let read_pos = read_u32(input)?;
        let n = read_u32(input)? as usize;
        if n > 65536 || n > input.len() {
            return Err(SnapshotError::InvalidStructure);
        }
        let (bytes, rest) = input.split_at(n);
        let read_buffer = bytes.to_vec();
        *input = rest;
        // Legacy schema-25 files have no pending output. The reserved high
        // bit adds logical buffering without rejecting those older images.
        let (write_capacity, write_flush, write_next, write_buffer) = if tag & 128 != 0 {
            let capacity = read_u32(input)?;
            let flush = read_u32(input)?;
            let next = flag(input)?;
            let n = read_u32(input)? as usize;
            if n > 65536 || n > input.len() {
                return Err(SnapshotError::InvalidStructure);
            }
            let (bytes, rest) = input.split_at(n);
            *input = rest;
            (capacity, flush, next, bytes.to_vec())
        } else {
            (WRITE_CAPACITY, 0, false, Vec::new())
        };
        if !matches!(write_capacity, 1 | WRITE_CAPACITY)
            || (write_next && (buffering != 1 || write_buffer.is_empty() || write_flush != 0))
            || write_flush as usize > write_buffer.len()
            || (write_flush == 0 && write_buffer.len() > write_capacity as usize)
            || (buffering == 0 && write_flush as usize != write_buffer.len())
            || (!write_buffer.is_empty() && (closed || !mode.write || has || n != 0 || eof))
            || write_buffer.len() as u64 > cursor
            || read_pos as usize > n
            || read_pos as u64 > cursor
            || (eof && n != 0)
            || (closed && (n != 0 || read_pos != 0))
            || (!mode.read && n != 0)
            || cursor
                .checked_add(n.saturating_sub(read_pos as usize) as u64)
                .is_none_or(|end| end > i64::MAX as u64)
            || kind > 5
            || buffering > 2
            || !mode.valid()
            || (!has && byte != 0)
            || cursor > i64::MAX as u64
            || (has && (closed || cursor == 0))
            || (!closed && kind < 2 && (id.0 == 0 || policy != HandlePolicy::Rebind))
            || (kind == 5 && !closed)
        {
            return Err(SnapshotError::InvalidStructure);
        }
        Ok(Self {
            kind,
            id,
            mode,
            policy,
            cursor,
            closed,
            buffering,
            write_capacity,
            write_buffer,
            write_flush,
            write_next,
            lookahead: has.then_some(byte),
            eof,
            read_buffer,
            read_pos,
        })
    }
}
fn flag(input: &mut &[u8]) -> Result<bool, SnapshotError> {
    match read_u8(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SnapshotError::InvalidTag),
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IoWork {
    Open {
        path: Vec<u8>,
        action: u8,
    },
    Read {
        start: u32,
        count: u32,
        got: u32,
        format: u8,
        remaining: u64,
        buffer: Vec<u8>,
        numeral: u8,
        digits: u32,
        hex: bool,
        iterator: bool,
        toclose: bool,
    },
    Write {
        start: u32,
        next: u32,
        offset: u64,
        failed: bool,
    },
    Seek {
        whence: u8,
        offset: i64,
    },
    Flush,
    Setvbuf {
        buffering: u8,
    },
    Close {
        quiet: bool,
        exhausted: bool,
        failed: bool,
    },
}
impl IoWork {
    #[cold]
    #[inline(never)]
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Read { got, .. } => 1 + *got,
            Self::Write { failed: true, .. } | Self::Close { failed: true, .. } => 3,
            _ => 1,
        }
    }
    #[cold]
    #[inline(never)]
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Open { path, .. } => path.len(),
            Self::Read { buffer, .. } => buffer.len(),
            _ => 0,
        }
    }
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Open { path, action } => {
                out.extend([1, *action]);
                out.extend((path.len() as u32).to_le_bytes());
                out.extend(path);
            }
            Self::Read {
                start,
                count,
                got,
                format,
                remaining,
                buffer,
                numeral,
                digits,
                hex,
                iterator,
                toclose,
            } => {
                out.push(2);
                for n in [start, count, got] {
                    out.extend(n.to_le_bytes());
                }
                out.push(*format);
                out.extend(remaining.to_le_bytes());
                out.extend((buffer.len() as u32).to_le_bytes());
                out.extend(buffer);
                out.push(*numeral);
                out.extend(digits.to_le_bytes());
                out.extend([*hex as u8, *iterator as u8, *toclose as u8]);
            }
            Self::Write {
                start,
                next,
                offset,
                failed,
            } => {
                out.push(3);
                out.extend(start.to_le_bytes());
                out.extend(next.to_le_bytes());
                out.extend(offset.to_le_bytes());
                out.push(*failed as u8);
            }
            Self::Seek { whence, offset } => {
                out.extend([4, *whence]);
                out.extend(offset.to_le_bytes());
            }
            Self::Flush => out.push(5),
            Self::Setvbuf { buffering } => out.extend([8, *buffering]),
            Self::Close {
                quiet,
                exhausted,
                failed,
            } => out.extend([if *failed { 7 } else { 6 }, *quiet as u8, *exhausted as u8]),
        }
    }
    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let bytes = |input: &mut &[u8]| -> Result<Vec<u8>, SnapshotError> {
            let n = read_u32(input)? as usize;
            if n > crate::heap::STRING_CEILING || n > input.len() {
                return Err(SnapshotError::Truncated);
            }
            let (a, b) = input.split_at(n);
            *input = b;
            Ok(a.to_vec())
        };
        Ok(match read_u8(input)? {
            1 => {
                let action = read_u8(input)?;
                if action > 4 {
                    return Err(SnapshotError::InvalidStructure);
                }
                Self::Open {
                    action,
                    path: bytes(input)?,
                }
            }
            2 => {
                let start = read_u32(input)?;
                let count = read_u32(input)?;
                let got = read_u32(input)?;
                let format = read_u8(input)?;
                let remaining = read_u64(input)?;
                let buffer = bytes(input)?;
                let numeral = read_u8(input)?;
                let digits = read_u32(input)?;
                let hex = flag(input)?;
                let iterator = flag(input)?;
                let toclose = flag(input)?;
                if format > 6
                    || numeral > 10
                    || got > count.max(1)
                    || digits > 200
                    || (format == 5 && buffer.len() > 200)
                    || (toclose && !iterator)
                    || start > 1
                {
                    return Err(SnapshotError::InvalidStructure);
                }
                Self::Read {
                    start,
                    count,
                    got,
                    format,
                    remaining,
                    buffer,
                    numeral,
                    digits,
                    hex,
                    iterator,
                    toclose,
                }
            }
            3 => Self::Write {
                start: read_u32(input)?,
                next: read_u32(input)?,
                offset: read_u64(input)?,
                failed: flag(input)?,
            },
            4 => {
                let whence = read_u8(input)?;
                if whence > 2 {
                    return Err(SnapshotError::InvalidStructure);
                }
                Self::Seek {
                    whence,
                    offset: read_i64(input)?,
                }
            }
            5 => Self::Flush,
            8 => {
                let buffering = read_u8(input)?;
                if buffering > 2 {
                    return Err(SnapshotError::InvalidStructure);
                }
                Self::Setvbuf { buffering }
            }
            tag @ (6 | 7) => Self::Close {
                quiet: flag(input)?,
                exhausted: flag(input)?,
                failed: tag == 7,
            },
            _ => return Err(SnapshotError::InvalidTag),
        })
    }
}
