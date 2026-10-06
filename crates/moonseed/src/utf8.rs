//! Lua extended UTF-8: byte encoding and strict/lax decoding.

/// Lua's `\u{...}` encoding. Code points reach `2^31 - 1`, including
/// surrogates and values above Unicode's maximum, so this is not `char`.
pub(crate) fn encode(mut code: u32) -> Vec<u8> {
    debug_assert!(code <= 0x7fff_ffff);
    if code < 0x80 {
        return vec![code as u8];
    }
    let mut reversed = Vec::new();
    let mut mfb = 0x3fu32;
    loop {
        reversed.push((0x80 | (code & 0x3f)) as u8);
        code >>= 6;
        mfb >>= 1;
        if code <= mfb {
            break;
        }
    }
    let first = (((!mfb).wrapping_shl(1) & 0xff) as u8) | (code as u8);
    let mut out = Vec::with_capacity(reversed.len() + 1);
    out.push(first);
    out.extend(reversed.into_iter().rev());
    out
}

/// Decode one sequence without reading outside the byte string.
pub(crate) fn decode(bytes: &[u8], pos: usize, strict: bool) -> Option<(u32, usize)> {
    let first = *bytes.get(pos)?;
    let width = match first {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        0xf8..=0xfb => 5,
        0xfc..=0xfd => 6,
        _ => return None,
    };
    let mut code = u32::from(first & (0xff >> width));
    if width == 1 {
        code = u32::from(first);
    }
    for offset in 1..width {
        let byte = *bytes.get(pos.checked_add(offset)?)?;
        if !continuation(byte) {
            return None;
        }
        code = (code << 6) | u32::from(byte & 0x3f);
    }
    let minimum = [0, 0x80, 0x800, 0x10000, 0x200000, 0x4000000][width - 1];
    if code < minimum
        || code > 0x7fffffff
        || (strict && (code > 0x10ffff || (0xd800..=0xdfff).contains(&code)))
    {
        return None;
    }
    Some((code, pos + width))
}

pub(crate) fn continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// Lua's u_posrelat, preserving zero and huge positive positions.
pub(crate) fn relative(pos: i64, len: usize) -> i64 {
    if pos >= 0 {
        pos
    } else if pos.unsigned_abs() > len as u64 {
        0
    } else {
        len as i64 + pos + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_and_malformed_sequences() {
        for code in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xd800, 0xdfff, 0xe000, 0xffff, 0x10000, 0x10ffff,
            0x110000, 0x1fffff, 0x200000, 0x3ffffff, 0x4000000, 0x7fffffff,
        ] {
            let bytes = encode(code);
            assert_eq!(decode(&bytes, 0, false), Some((code, bytes.len())));
            let scalar = code <= 0x10ffff && !(0xd800..=0xdfff).contains(&code);
            assert_eq!(decode(&bytes, 0, true).is_some(), scalar);
            for end in 0..bytes.len() {
                assert_eq!(decode(&bytes[..end], 0, false), None);
            }
            for i in 1..bytes.len() {
                let mut bad = bytes.clone();
                bad[i] = 0x7f;
                assert_eq!(decode(&bad, 0, false), None);
            }
        }
        for bytes in [
            &[0xc0, 0x80][..],
            &[0xc1, 0xbf],
            &[0xe0, 0x9f, 0xbf],
            &[0xf0, 0x8f, 0xbf, 0xbf],
            &[0xf8, 0x87, 0xbf, 0xbf, 0xbf],
            &[0xfc, 0x83, 0xbf, 0xbf, 0xbf, 0xbf],
            &[0xfe],
            &[0xff],
            &[0x80],
        ] {
            assert_eq!(decode(bytes, 0, false), None);
            assert_eq!(decode(bytes, usize::MAX, true), None);
        }
        assert_eq!(relative(i64::MIN, 5), 0);
        assert_eq!(relative(i64::MAX, 5), i64::MAX);
    }
}
