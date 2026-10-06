//! A prototype's debug information (ADR 0040): what `debug.getinfo`,
//! `debug.getlocal`, `debug.getupvalue`, and tracebacks read. It never
//! changes what a program computes, and costs no fuel.
//!
//! The compiler makes it; snapshots and unstripped binary chunks keep it.
//! Line numbers are written as signed deltas in a variable-length integer
//! code, so a line costs about a byte per instruction.

use crate::id::SnapshotError;
use crate::opcode::{read_u8, read_u32};

/// Longest name restore and binary `load` accept. Names are kept whole,
/// as the source wrote them, so a checkpoint never changes one; they are
/// bounded by the source and literal limits, and charged to the heap.
pub(crate) const MAX_NAME_BYTES: usize = crate::heap::STRING_CEILING;

/// A local variable's name, register, and the instructions it is active
/// over: from `start` up to, not including, `end` (Lua's `LocVar`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalInfo {
    pub(crate) name: Vec<u8>,
    pub(crate) reg: u8,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

/// How a call names the function it calls (Lua's `getobjname`): what
/// `debug.getinfo` reports as `namewhat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameKind {
    Global = 1,
    Local = 2,
    Method = 3,
    Field = 4,
    Upvalue = 5,
    Constant = 6,
    ForIterator = 7,
}

impl NameKind {
    pub(crate) fn from_u8(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::Global,
            2 => Self::Local,
            3 => Self::Method,
            4 => Self::Field,
            5 => Self::Upvalue,
            6 => Self::Constant,
            7 => Self::ForIterator,
            _ => return None,
        })
    }

    /// Lua's `namewhat`.
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Local => "local",
            Self::Method => "method",
            Self::Field => "field",
            Self::Upvalue => "upvalue",
            Self::Constant => "constant",
            Self::ForIterator => "for iterator",
        }
    }
}

/// The name of the function the call at `pc` calls, as its source wrote
/// it: Moonseed records at compile time what Lua finds by symbolic
/// execution (ADR 0040).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallName {
    pub(crate) pc: u32,
    pub(crate) kind: NameKind,
    pub(crate) name: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct DebugInfo {
    /// The chunk's name, on a chunk's root prototype only: its children
    /// have their parent's. `None` on a root is Lua's `=?`.
    pub(crate) source: Option<Vec<u8>>,
    /// The lines of the function's `function` and `end`; 0 for a chunk.
    pub(crate) line_defined: u32,
    pub(crate) last_line_defined: u32,
    /// The source line of each instruction.
    pub(crate) lines: Vec<u32>,
    /// Locals, parameters first, in the order they are declared.
    pub(crate) locals: Vec<LocalInfo>,
    /// Each upvalue's name, by capture.
    pub(crate) upvalues: Vec<Vec<u8>>,
    /// The names of called functions, by call instruction, in code order.
    pub(crate) calls: Vec<CallName>,
}

impl DebugInfo {
    /// Entries the logical heap charges a reference each for (ADR 0040):
    /// lines, locals, upvalue names, and the name bytes over a word each.
    pub(crate) fn logical_size(&self) -> u64 {
        let refs = self.lines.len() + self.locals.len() + self.upvalues.len() + self.calls.len();
        let names: usize = self
            .locals
            .iter()
            .map(|local| local.name.len())
            .chain(self.upvalues.iter().map(Vec::len))
            .chain(self.calls.iter().map(|call| call.name.len()))
            .sum();
        crate::heap::cost::REF * refs as u64 + names as u64
    }

    /// The information as bytes, without the source, which the caller
    /// writes its own way.
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, u64::from(self.line_defined));
        write_varint(out, u64::from(self.last_line_defined));
        write_varint(out, self.lines.len() as u64);
        let mut previous = i64::from(self.line_defined);
        for line in &self.lines {
            write_signed(out, i64::from(*line) - previous);
            previous = i64::from(*line);
        }
        write_varint(out, self.locals.len() as u64);
        for local in &self.locals {
            write_name(out, &local.name);
            out.push(local.reg);
            write_varint(out, u64::from(local.start));
            write_varint(out, u64::from(local.end));
        }
        write_varint(out, self.upvalues.len() as u64);
        for name in &self.upvalues {
            write_name(out, name);
        }
        write_varint(out, self.calls.len() as u64);
        let mut previous = 0u32;
        for call in &self.calls {
            write_varint(out, u64::from(call.pc - previous));
            previous = call.pc;
            out.push(call.kind as u8);
            write_name(out, &call.name);
        }
    }

    /// What a stripped chunk keeps (ADR 0040): the defining lines, and the
    /// call names Lua still finds without local and upvalue names. A
    /// global becomes a field, since `_ENV` has no name left; an upvalue
    /// is `?`; a local has no name.
    pub(crate) fn stripped(&self) -> Self {
        Self {
            source: None,
            line_defined: self.line_defined,
            last_line_defined: self.last_line_defined,
            lines: Vec::new(),
            locals: Vec::new(),
            upvalues: Vec::new(),
            calls: self
                .calls
                .iter()
                .filter_map(|call| {
                    let (kind, name) = match call.kind {
                        NameKind::Local => return None,
                        NameKind::Global => (NameKind::Field, call.name.clone()),
                        NameKind::Upvalue => (NameKind::Upvalue, b"?".to_vec()),
                        kind => (kind, call.name.clone()),
                    };
                    Some(CallName {
                        pc: call.pc,
                        kind,
                        name,
                    })
                })
                .collect(),
        }
    }

    /// The name of the function the call at `pc` calls, if recorded.
    pub(crate) fn call_name(&self, pc: u32) -> Option<&CallName> {
        self.calls
            .binary_search_by_key(&pc, |call| call.pc)
            .ok()
            .map(|index| &self.calls[index])
    }

    /// Read what [`Self::encode`] wrote, for a prototype of `ops`
    /// instructions, `captures` upvalues, and `max_reg` registers: every
    /// count, range, and register is checked against them before anything
    /// is allocated.
    pub(crate) fn decode(
        input: &mut &[u8],
        ops: usize,
        captures: usize,
        max_reg: u8,
    ) -> Result<Self, SnapshotError> {
        let line_defined = read_line(input)?;
        let last_line_defined = read_line(input)?;
        let count = read_count(input, ops)?;
        if count != 0 && count != ops {
            return Err(SnapshotError::InvalidStructure);
        }
        let mut lines = Vec::with_capacity(count.min(1024));
        let mut previous = i64::from(line_defined);
        for _ in 0..count {
            previous = previous
                .checked_add(read_signed(input)?)
                .filter(|line| (0..=i64::from(MAX_LINE)).contains(line))
                .ok_or(SnapshotError::InvalidStructure)?;
            lines.push(previous as u32);
        }
        // A local takes at least four bytes.
        let count = read_count(input, input.len() / 4)?;
        let mut locals = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let name = read_name(input)?;
            let reg = read_u8(input)?;
            let start = read_line(input)?;
            let end = read_line(input)?;
            if start > end || end as usize > ops || reg >= max_reg {
                return Err(SnapshotError::InvalidStructure);
            }
            locals.push(LocalInfo {
                name,
                reg,
                start,
                end,
            });
        }
        let count = read_count(input, captures)?;
        if count != 0 && count != captures {
            return Err(SnapshotError::InvalidStructure);
        }
        let mut upvalues = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            upvalues.push(read_name(input)?);
        }
        let count = read_count(input, ops)?;
        let mut calls: Vec<CallName> = Vec::with_capacity(count.min(1024));
        let mut pc = 0u32;
        for index in 0..count {
            let delta = read_line(input)?;
            // Strictly increasing, and inside the code.
            if index > 0 && delta == 0 {
                return Err(SnapshotError::InvalidStructure);
            }
            pc = pc
                .checked_add(delta)
                .filter(|pc| (*pc as usize) < ops)
                .ok_or(SnapshotError::InvalidStructure)?;
            let kind = NameKind::from_u8(read_u8(input)?).ok_or(SnapshotError::InvalidTag)?;
            calls.push(CallName {
                pc,
                kind,
                name: read_name(input)?,
            });
        }
        Ok(Self {
            source: None,
            line_defined,
            last_line_defined,
            lines,
            locals,
            upvalues,
            calls,
        })
    }
}

/// Lua's largest line: the lexer counts lines in an `int`.
pub(crate) const MAX_LINE: u32 = i32::MAX as u32;

pub(crate) fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_signed(out: &mut Vec<u8>, value: i64) {
    // Zigzag: small magnitudes of either sign take one byte.
    write_varint(out, ((value << 1) ^ (value >> 63)) as u64);
}

fn write_name(out: &mut Vec<u8>, name: &[u8]) {
    write_varint(out, name.len() as u64);
    out.extend_from_slice(name);
}

pub(crate) fn read_varint(input: &mut &[u8]) -> Result<u64, SnapshotError> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = read_u8(input)?;
        if shift == 63 && byte > 1 {
            return Err(SnapshotError::InvalidStructure);
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(SnapshotError::InvalidStructure)
}

fn read_signed(input: &mut &[u8]) -> Result<i64, SnapshotError> {
    let raw = read_varint(input)?;
    Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
}

fn read_line(input: &mut &[u8]) -> Result<u32, SnapshotError> {
    u32::try_from(read_varint(input)?)
        .ok()
        .filter(|line| *line <= MAX_LINE)
        .ok_or(SnapshotError::InvalidStructure)
}

/// A count, charged to a snapshot's decode budget like the snapshot's own
/// counts (ADR 0052).
fn read_count(input: &mut &[u8], max: usize) -> Result<usize, SnapshotError> {
    let count = usize::try_from(read_varint(input)?)
        .ok()
        .filter(|count| *count <= max && *count <= input.len())
        .ok_or(SnapshotError::LimitExceeded)?;
    crate::snapshot::spend_items(count as u64)?;
    Ok(count)
}

fn read_name(input: &mut &[u8]) -> Result<Vec<u8>, SnapshotError> {
    let len = read_count(input, MAX_NAME_BYTES)?;
    if len > input.len() {
        return Err(SnapshotError::Truncated);
    }
    let (name, rest) = input.split_at(len);
    *input = rest;
    Ok(name.to_vec())
}

/// The source of a chunk as bytes, written as a length and the bytes.
pub(crate) fn write_source(out: &mut Vec<u8>, source: &[u8]) {
    out.extend((source.len() as u32).to_le_bytes());
    out.extend_from_slice(source);
}

pub(crate) fn read_source(input: &mut &[u8], max: usize) -> Result<Vec<u8>, SnapshotError> {
    let len = read_u32(input)? as usize;
    if len > max || len > input.len() {
        return Err(SnapshotError::LimitExceeded);
    }
    let (source, rest) = input.split_at(len);
    *input = rest;
    Ok(source.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forged_debug_counts_and_overflowing_varints_are_refused() {
        let mut bytes = vec![0, 0]; // Defining lines.
        write_varint(&mut bytes, crate::limits::MAX_INSTRUCTIONS as u64);
        bytes.push(0);
        assert!(
            DebugInfo::decode(&mut bytes.as_slice(), crate::limits::MAX_INSTRUCTIONS, 0, 1)
                .is_err()
        );
        let mut bytes = vec![0x80; 9];
        bytes.push(2);
        assert_eq!(
            read_varint(&mut bytes.as_slice()),
            Err(SnapshotError::InvalidStructure)
        );
    }

    #[test]
    fn debug_information_reads_back_as_written() {
        let info = DebugInfo {
            source: None,
            line_defined: 3,
            last_line_defined: 90,
            lines: vec![3, 4, 4, 900, 2, 2, 90],
            locals: vec![LocalInfo {
                name: b"x".to_vec(),
                reg: 2,
                start: 1,
                end: 7,
            }],
            upvalues: vec![b"_ENV".to_vec(), b"up".to_vec()],
            calls: vec![
                CallName {
                    pc: 2,
                    kind: NameKind::Global,
                    name: b"print".to_vec(),
                },
                CallName {
                    pc: 5,
                    kind: NameKind::Method,
                    name: b"m".to_vec(),
                },
            ],
        };
        let mut out = Vec::new();
        info.encode(&mut out);
        assert_eq!(DebugInfo::decode(&mut &out[..], 7, 2, 3).unwrap(), info);
        // Six of the seven lines take a byte.
        assert!(out.len() < 60, "{}", out.len());
        // Counts that disagree with the code are refused.
        assert!(DebugInfo::decode(&mut &out[..], 6, 2, 3).is_err());
        assert!(DebugInfo::decode(&mut &out[..], 7, 1, 3).is_err());
        // A local in a register the function does not have.
        assert!(DebugInfo::decode(&mut &out[..], 7, 2, 2).is_err());
        for len in 0..out.len() {
            assert!(DebugInfo::decode(&mut &out[..len], 7, 2, 3).is_err());
        }
    }
}
