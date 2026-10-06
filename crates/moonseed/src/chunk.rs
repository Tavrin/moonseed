//! Moonseed binary chunks: what `string.dump` writes and `load` reads
//! (ADR 0036).
//!
//! A binary chunk is a function's code, not a running state: its
//! prototype tree, with no upvalue values and no object ids. The format
//! is Moonseed's own, the same bytes on every target:
//!
//! - the signature `"\x1bMSC"`, then the chunk format revision and the
//!   bytecode revision, as little-endian `u16`s, and a flags byte (bit 0:
//!   stripped);
//! - the root prototype, each prototype followed by its children:
//!   `max_reg`, `params`, `vararg` as bytes; the instruction count and the
//!   instructions, in the snapshot's instruction encoding; the constant
//!   count and each constant's length and bytes; the capture count and the
//!   captures; a debug flag byte and, when it is 1, the debug information
//!   (ADR 0040) and a source flag byte, 1 followed by the chunk's name on
//!   the root only; the child count;
//! - a CRC-32 of everything before it.
//!
//! Reading checks every count against what is left of the input and the
//! structural ceilings before it allocates, and the result goes through
//! the same prototype validator as compiled code before anything is
//! installed. A stripped chunk keeps each function's `linedefined` and
//! `lastlinedefined` and drops its lines, local and upvalue names, and the
//! chunk's name, as Lua's `strip` does.

use crate::limits::{MAX_CONSTS, MAX_FUNC_NEST, MAX_INSTRUCTIONS, MAX_PROTOS, MAX_UPVALUES};
use crate::opcode::{Op, decode_capture, encode_capture};
use crate::program::ProtoSpec;

/// The first bytes of every Moonseed binary chunk. The escape byte is
/// Lua's mark of a binary chunk; PUC Lua's own chunks go on with `Lua`.
pub(crate) const SIGNATURE: &[u8; 4] = b"\x1bMSC";

/// The binary chunk format's revision (ADR 0036). Separate from the
/// bytecode revision, which the chunk records too.
pub(crate) const CHUNK_REVISION: u16 = 2;

const HEADER: usize = 4 + 2 + 2 + 1;

/// Why a chunk was refused: the text `load` puts after the chunk's name.
pub(crate) type Refusal = &'static str;

/// The chunk of `spec`, or `None` when it would pass `limit` bytes.
pub(crate) fn dump(spec: &ProtoSpec, strip: bool, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(SIGNATURE);
    out.extend(CHUNK_REVISION.to_le_bytes());
    out.extend(crate::snapshot::BYTECODE_REVISION.to_le_bytes());
    out.push(u8::from(strip));
    // Prototypes in order, each before its children, without recursion.
    let mut pending = vec![spec];
    while let Some(proto) = pending.pop() {
        out.push(proto.max_reg);
        out.push(proto.params);
        out.push(u8::from(proto.vararg));
        out.extend((proto.ops.len() as u32).to_le_bytes());
        for op in &proto.ops {
            op.encode(&mut out);
        }
        out.extend((proto.byte_consts.len() as u32).to_le_bytes());
        for bytes in &proto.byte_consts {
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        out.extend((proto.captures.len() as u32).to_le_bytes());
        for capture in &proto.captures {
            encode_capture(*capture, &mut out);
        }
        match &proto.debug {
            None => out.push(0),
            Some(debug) if strip => {
                out.push(1);
                debug.stripped().encode(&mut out);
                out.push(0);
            }
            Some(debug) => {
                out.push(1);
                debug.encode(&mut out);
                match &debug.source {
                    Some(source) if std::ptr::eq(proto, spec) => {
                        out.push(1);
                        crate::debuginfo::write_source(&mut out, source);
                    }
                    _ => out.push(0),
                }
            }
        }
        out.extend((proto.children.len() as u32).to_le_bytes());
        pending.extend(proto.children.iter().rev());
        if out.len() > limit {
            return None;
        }
    }
    out.extend(crate::snapshot::crc32(&out).to_le_bytes());
    (out.len() <= limit).then_some(out)
}

/// A prototype being read: its fields, and how many children it still
/// waits for.
struct Partial {
    spec: ProtoSpec,
    children: u32,
}

/// Read a chunk. The result is validated: [`crate::check::validate_binary`]
/// accepted it. Decoding makes at most about `budget` bytes of structure
/// before it is refused (ADR 0052): the caller passes what the heap has
/// room for.
pub(crate) fn undump(bytes: &[u8], budget: u64) -> Result<ProtoSpec, Refusal> {
    if bytes.len() < HEADER + 4 || !bytes.starts_with(SIGNATURE) {
        return Err(
            if bytes.starts_with(SIGNATURE) || bytes.len() < SIGNATURE.len() {
                "bad binary format (truncated chunk)"
            } else {
                "bad binary format (not a Moonseed chunk)"
            },
        );
    }
    let (body, sum) = bytes.split_at(bytes.len() - 4);
    let chunk = u16::from_le_bytes([body[4], body[5]]);
    let bytecode = u16::from_le_bytes([body[6], body[7]]);
    if chunk != CHUNK_REVISION || bytecode != crate::snapshot::BYTECODE_REVISION {
        return Err("bad binary format (version mismatch)");
    }
    if body[8] > 1 {
        return Err("bad binary format (flags)");
    }
    if u32::from_le_bytes([sum[0], sum[1], sum[2], sum[3]]) != crate::snapshot::crc32(body) {
        return Err("bad binary format (checksum)");
    }
    let mut input = &body[HEADER..];
    let _budget = crate::snapshot::BudgetScope::set(budget);
    let spec = read_tree(&mut input)?;
    if !input.is_empty() {
        return Err("bad binary format (trailing bytes)");
    }
    crate::check::validate_binary(&spec).map_err(|_| "bad binary format (invalid code)")?;
    Ok(spec)
}

/// The prototype tree, depth first, with an explicit stack bounded by
/// [`MAX_FUNC_NEST`].
fn read_tree(input: &mut &[u8]) -> Result<ProtoSpec, Refusal> {
    let mut count = 0usize;
    let mut stack: Vec<Partial> = Vec::new();
    loop {
        count += 1;
        if count > MAX_PROTOS {
            return Err("bad binary format (too many functions)");
        }
        let mut partial = read_proto(input, count == 1)?;
        // Attach finished prototypes to their parents.
        loop {
            if partial.children > 0 {
                if stack.len() + 1 >= MAX_FUNC_NEST as usize {
                    return Err("bad binary format (functions nested too deeply)");
                }
                stack.push(partial);
                break;
            }
            match stack.last_mut() {
                None => return Ok(partial.spec),
                Some(parent) => {
                    parent.spec.children.push(partial.spec);
                    parent.children -= 1;
                    if parent.children > 0 {
                        break;
                    }
                    partial = stack.pop().ok_or("bad binary format")?;
                }
            }
        }
    }
}

fn read_proto(input: &mut &[u8], root: bool) -> Result<Partial, Refusal> {
    const TRUNCATED: Refusal = "bad binary format (truncated chunk)";
    let byte = |input: &mut &[u8]| crate::opcode::read_u8(input).map_err(|_| TRUNCATED);
    let count = |input: &mut &[u8], max: usize, least: usize| -> Result<usize, Refusal> {
        let n = crate::opcode::read_u32(input).map_err(|_| TRUNCATED)? as usize;
        if n > max {
            return Err("bad binary format (count out of range)");
        }
        // Each item takes at least `least` bytes: a count the input cannot
        // hold is refused before anything is allocated for it.
        if n.checked_mul(least)
            .ok_or("bad binary format (count out of range)")?
            > input.len()
        {
            return Err(TRUNCATED);
        }
        crate::snapshot::spend_items(n as u64).map_err(|_| "not enough memory")?;
        Ok(n)
    };
    let max_reg = byte(input)?;
    let params = byte(input)?;
    let vararg = match byte(input)? {
        0 => false,
        1 => true,
        _ => return Err("bad binary format (vararg flag)"),
    };
    let n = count(input, MAX_INSTRUCTIONS, 1)?;
    let mut ops = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        ops.push(Op::decode(input).map_err(|_| "bad binary format (instruction)")?);
    }
    let n = count(input, MAX_CONSTS, 4)?;
    let mut byte_consts = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let len = count(input, crate::heap::STRING_CEILING, 1)?;
        let (bytes, rest) = input.split_at(len);
        byte_consts.push(bytes.to_vec());
        *input = rest;
    }
    let n = count(input, MAX_UPVALUES, 2)?;
    let mut captures = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        captures.push(decode_capture(input).map_err(|_| "bad binary format (capture)")?);
    }
    const DEBUG: Refusal = "bad binary format (debug information)";
    let debug = match byte(input)? {
        0 => None,
        1 => {
            let mut debug =
                crate::debuginfo::DebugInfo::decode(input, ops.len(), captures.len(), max_reg)
                    .map_err(|_| DEBUG)?;
            match byte(input)? {
                0 => {}
                1 if root => {
                    debug.source = Some(
                        crate::debuginfo::read_source(input, crate::heap::STRING_CEILING)
                            .map_err(|_| DEBUG)?,
                    );
                }
                _ => return Err(DEBUG),
            }
            Some(Box::new(debug))
        }
        _ => return Err(DEBUG),
    };
    let children = count(input, MAX_PROTOS, HEADER)? as u32;
    Ok(Partial {
        spec: ProtoSpec {
            ops,
            byte_consts,
            captures,
            children: Vec::new(),
            max_reg,
            params,
            vararg,
            debug,
        },
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(source: &str) -> ProtoSpec {
        crate::compile::compile(source.as_bytes()).unwrap().proto
    }

    /// A chunk decodes only within the budget its reader is given: what the
    /// heap has room for, not some multiple of the chunk's length.
    #[test]
    fn a_chunk_decodes_within_its_budget() {
        let source = "local x = 0 ".to_string() + &"x = x + 1 ".repeat(20_000);
        let bytes = dump(&chunk(&source), false, usize::MAX).unwrap();
        assert_eq!(undump(&bytes, 64 << 10).unwrap_err(), "not enough memory");
        assert!(undump(&bytes, 64 << 20).is_ok());
    }

    #[test]
    fn chunks_read_back_as_written() {
        let spec = chunk(
            "local a, b = 1, 'x' local function f(...) return a, b, ... end \
             return function() return f(2) end",
        );
        let bytes = dump(&spec, false, 1 << 20).unwrap();
        assert!(bytes.starts_with(SIGNATURE));
        assert_eq!(undump(&bytes, u64::MAX).unwrap(), spec);
        // Stripping keeps the defining lines and drops the rest.
        let stripped = dump(&spec, true, 1 << 20).unwrap();
        assert!(stripped.len() < bytes.len());
        let back = undump(&stripped, u64::MAX).unwrap();
        let debug = back.children[0].debug.as_ref().unwrap();
        assert_eq!(debug.line_defined, 1);
        assert!(debug.lines.is_empty() && debug.locals.is_empty() && debug.upvalues.is_empty());
        assert_eq!(back.ops, spec.ops);
        assert!(dump(&spec, false, 16).is_none());
    }

    #[test]
    fn huge_counts_are_refused_before_reserving_memory() {
        let spec = chunk("return 1");
        let bytes = dump(&spec, true, 1 << 20).unwrap();
        let mut instructions = Vec::new();
        for op in &spec.ops {
            op.encode(&mut instructions);
        }
        let constants_at = HEADER + 3 + 4 + instructions.len();
        let children_at = bytes.len() - 8;
        for (at, ceiling) in [
            (HEADER + 3, MAX_INSTRUCTIONS),
            (constants_at, MAX_CONSTS),
            (children_at, MAX_PROTOS),
        ] {
            for count in [ceiling as u32, u32::MAX] {
                let mut forged = bytes[..bytes.len() - 4].to_vec();
                forged[at..at + 4].copy_from_slice(&count.to_le_bytes());
                let sum = crate::snapshot::crc32(&forged);
                forged.extend(sum.to_le_bytes());
                assert!(undump(&forged, u64::MAX).is_err());
            }
        }
    }

    #[test]
    fn different_revisions_are_refused_at_the_header() {
        let bytes = dump(
            &chunk("local x = 7 if x < 8 then return 20 - x end return x"),
            false,
            1 << 20,
        )
        .unwrap();
        for (at, revision) in [
            (4, CHUNK_REVISION - 1),
            (6, crate::snapshot::BYTECODE_REVISION - 1),
            (6, crate::snapshot::BYTECODE_REVISION + 1),
        ] {
            let mut forged = bytes.clone();
            forged[at..at + 2].copy_from_slice(&revision.to_le_bytes());
            assert_eq!(
                undump(&forged, u64::MAX).unwrap_err(),
                "bad binary format (version mismatch)"
            );
        }
    }

    #[test]
    fn damaged_chunks_are_refused_without_panic() {
        let spec =
            chunk("local t = {} for i = 1, 3 do t[i] = function() return i end end return t");
        let bytes = dump(&spec, false, 1 << 20).unwrap();
        assert!(undump(b"\x1bLua\x54\x00", u64::MAX).is_err());
        assert!(undump(b"\x1b", u64::MAX).is_err());
        for len in 0..bytes.len() {
            assert!(undump(&bytes[..len], u64::MAX).is_err(), "prefix {len}");
        }
        // Every single-bit flip, with the checksum fixed up so the damage
        // reaches the decoder and the validator.
        let body = bytes.len() - 4;
        for index in 0..body {
            for bit in 0..8 {
                let mut damaged = bytes[..body].to_vec();
                damaged[index] ^= 1 << bit;
                let sum = crate::snapshot::crc32(&damaged);
                damaged.extend(sum.to_le_bytes());
                let _ = undump(&damaged, u64::MAX);
            }
        }
    }
}
