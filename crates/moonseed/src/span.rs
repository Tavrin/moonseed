//! Half-open byte spans. Line and column are derived and are not identity.

/// Byte range `[start, end)` into the source that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The first byte offset, inclusive.
    pub start: u32,
    /// The last byte offset, exclusive.
    pub end: u32,
}

impl Span {
    /// Construct a half-open source byte range.
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Return the smallest range covering both input ranges.
    pub fn cover(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Whether offset lies inside this half-open range.
    pub fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }
}

/// 1-based line and 1-based byte column. `\n`, `\r`, `\n\r`, and `\r\n` are
/// each one line break. The column counts bytes, not Unicode scalar values.
pub fn line_col(source: &[u8], offset: u32) -> (u32, u32) {
    let offset = (offset as usize).min(source.len());
    let mut line = 1u32;
    let mut col = 1u32;
    let mut index = 0;
    while index < offset {
        let byte = source[index];
        if byte == b'\n' || byte == b'\r' {
            let pair = index + 1 < source.len()
                && ((byte == b'\n' && source[index + 1] == b'\r')
                    || (byte == b'\r' && source[index + 1] == b'\n'));
            let width = if pair { 2 } else { 1 };
            if index + width > offset {
                break;
            }
            index += width;
            line = line.saturating_add(1);
            col = 1;
        } else {
            index += 1;
            col = col.saturating_add(1);
        }
    }
    (line, col)
}
