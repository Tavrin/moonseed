# ADR 0006 — Object-id snapshots

## Context

The checkpoint has to move between native and wasm32. Arena layout and pointer width differ, and they will change.

## Decision

Hand-written little-endian records, schema 1, magic `MNSD`. References are `ObjectId`. Generations and handles are not written. Counts are checked before allocation. CRC-32 detects corruption and is not an authentication. `from_snapshot` is a constructor: failure does not yield a runtime.

Safe points are instruction boundaries plus `Prepared` and `Waiting`.

## Alternatives

`serde` of Rust structs, or dumping process memory. Both leak layout. BLAKE3 can replace CRC-32 later if we need a stronger corruption check; it still would not authenticate the sender.

## Revisit

Schema 3 is current (ADR 0010). Schemas 1 and 2 are not restored. `LoadBool` is opcode tag 31 inside that schema (ADR 0011); no existing field moved, and a snapshot without the tag still restores. A later schema should keep failing closed rather than being guessed.
