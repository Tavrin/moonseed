# ADR 0036 — Moonseed binary chunks: `string.dump` and binary `load`

## Context

Lua's string library is not complete without `string.dump`, and `load` accepts what it writes. Lua promises only that a dumped function loads back into the same implementation. PUC Lua's chunks hold its own bytecode, in the host's sizes and byte order.

A function's upvalue values are not dumped. A function loaded from a binary chunk has fresh upvalues: the first is set to `load`'s `env`, or the globals, and the rest are nil.

## Decision

`string.dump` writes a Moonseed binary chunk (`chunk.rs`):

- the signature `"\x1bMSC"`, the chunk format revision (2), the bytecode revision it holds (12), and a flags byte (bit 0: stripped), little-endian;
- each prototype, before its children: `max_reg`, `params`, `vararg`; its instructions, in the snapshot's instruction encoding; its constants, each a length and bytes; its captures; its debug information (ADR 0040), with the source name on the root; its child count;
- a CRC-32 of everything before.

Counts and byte lengths are little-endian `u32`s. Debug PCs and local ranges use variable-length unsigned integers. These fields already represent the structural ceilings: 1 << 24 instructions and constants per function, and 1 << 20 functions per chunk. Bytecode revision 12 widens jumps to `i32` and constant, field-name, and child indexes to `u32`; the chunk layout remains revision 2. Other revisions are refused before decoding.

The chunk holds code only: no object ids, no upvalue values, no runtime state. The same function dumps to the same bytes on every target, whatever the program did before.

`load` reads a chunk whose first byte is Lua's escape byte as binary, when its mode allows `b` (the default `bt` does). Reading:

- checks the signature, both revisions, the flags, and the checksum;
- checks every count against the structural ceilings and against what is left of the input before it allocates anything for it;
- builds the prototype tree with an explicit stack, at most 64 deep, at most 1 << 20 prototypes;
- passes the result through the same validator as compiled code (`check::validate_binary`), which allows the root any number of upvalues;
- only then installs it.

A PUC Lua chunk, a damaged chunk, or a chunk from another revision is refused with a message, as `lundump.c` words it: `binary string: bad binary format (not a Moonseed chunk)`. A chunk read from a reader function is checked the same way once the reader has given it all.

The loaded function's first upvalue is `env` or the globals; the others start nil.

**`strip`.** As ADR 0040 specifies, stripping keeps defining lines and recoverable call names, and drops instruction lines, local and upvalue names, and the source name.

**Dumpability.** A Lua function is dumped whole, with its nested functions. A native, a builtin, or a native closure is not: "unable to dump given function".

## Alternatives

- **PUC Lua's format.** Its bytecode is not Moonseed's, and its sizes and byte order are the host's.
- **The snapshot format.** A snapshot is a running state, with object ids and upvalue values; a dumped function is code.
- **No checksum.** The validator already rejects impossible code; the checksum makes accidental damage a clear refusal rather than a function built from different, valid code.

## Consequences

- The chunk format has its own revision, separate from the snapshot schema and the bytecode revision. A change to the instruction set changes the bytecode revision, and chunks from before are refused.
- The instruction encoding is shared with snapshots, so one codec is maintained.
- Evidence: unit tests flip every bit of a chunk (fixing the checksum so damage reaches the validator) and truncate it at every length; a proof loads 3,000 damaged chunks; the dump bytes are part of the native↔wasm32 fingerprint.
