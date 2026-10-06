//! The shared, bounded Lua source name used in diagnostics and debug data.

/// Lua's `luaO_chunkid`, with its 60-byte output buffer.
pub(crate) fn chunk_id(name: &[u8]) -> Vec<u8> {
    let name = &name[..name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len())];
    const ID_SIZE: usize = 60;
    match name.first() {
        Some(b'=') => {
            let rest = &name[1..];
            rest[..rest.len().min(ID_SIZE - 1)].to_vec()
        }
        Some(b'@') => {
            let rest = &name[1..];
            if name.len() <= ID_SIZE {
                rest.to_vec()
            } else {
                let keep = ID_SIZE - 4;
                let mut out = b"...".to_vec();
                out.extend_from_slice(&rest[rest.len() - keep..]);
                out
            }
        }
        _ => {
            const PRE: &[u8] = b"[string \"";
            const POS: &[u8] = b"\"]";
            let room = ID_SIZE - (PRE.len() + 3 + POS.len()) - 1;
            let newline = name.iter().position(|byte| *byte == b'\n');
            let mut out = PRE.to_vec();
            if name.len() < room && newline.is_none() {
                out.extend_from_slice(name);
            } else {
                let len = newline.unwrap_or(name.len()).min(room);
                out.extend_from_slice(&name[..len]);
                out.extend_from_slice(b"...");
            }
            out.extend_from_slice(POS);
            out
        }
    }
}
