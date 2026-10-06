# ADR 0038 — One pack ABI for every target

## Context

`string.pack`, `string.packsize`, and `string.unpack` describe binary data with C's types: native `short`, `int`, `long`, `size_t`, `float`, `double`, native byte order, and native alignment. On PUC Lua those are the host's, so the same format gives different bytes on different machines. Moonseed runs the same program natively and on wasm32 and must give the same bytes.

## Decision

Moonseed uses one ABI on every target, the one PUC Lua uses on x86-64 Linux:

| Option | Size |
|---|---|
| `b`, `B`, `c` | 1 |
| `h`, `H` | 2 |
| `i`, `I` without a size | 4 |
| `l`, `L`, `j`, `J`, `T`, and `s` without a size | 8 |
| `f` | 4, IEEE binary32 |
| `d`, `n` | 8, IEEE binary64 |
| native byte order (`=`, and the default) | little-endian |
| largest alignment (`!` without a size) | 8 |

Nothing reads the Rust target's pointer width or byte order: every integer is assembled byte by byte, and floats go through their bits. `<` and `>` still choose the byte order. Sizes `i1`..`i16`, `I1`..`I16`, `s1`..`s16`, and `!1`..`!16` follow Lua, including the check that bytes past 8 are pure sign or zero extension.

`strpack.rs` is a port of `str_pack`, `str_packsize`, and `str_unpack` as resumable machines: the VM converts each argument itself and hands it over, and takes each result before the next is made, checking the stack first. Every error is Lua's, at the same moment. A result past the 1 MiB string limit is the memory error.

## Alternatives

- **The target's ABI.** Native and wasm32 would pack differently.
- **A 32-bit ABI.** It would match no common PUC build, so the oracle could not check it.

## Consequences

- On x86-64 Linux, Moonseed and PUC Lua pack the same bytes; about 253,000 generated cases matched the oracle, and the ABI table above was checked against it.
- On a PUC Lua build with other native sizes, formats that use native sizes differ from it; formats with explicit sizes and byte order do not.
