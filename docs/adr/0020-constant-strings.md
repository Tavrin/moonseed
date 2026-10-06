# ADR 0020 — Prototype-owned constant strings

## Context

A string constant in source (`"foo"`, or the key of `t.foo`) was stored as bytes in its prototype. Every execution that needed it as a value made a new string object:
- `LoadBytes`;
- a `GetField` miss handed to `__index`;
- every `SetField` that reached the slow path (insert, delete, `__newindex`).

Collection was not automatic, so a loop of these reached the object limit: 1,000 iterations of `s = s + t.foo` over an `__index` function allocated 1,005 objects instead of 15.

## Decision

When a prototype is installed, each of its constants becomes a string object, and the prototype holds it (`Proto::const_strings`, parallel to `byte_consts`).
- `LoadBytes` stores that object.
- A slow-path `GetField` or `SetField` passes it as the key.
- Running code never allocates a constant.
- The hot tier still looks keys up by the borrowed bytes, so nothing changed there.

The collector traces a prototype's constant strings. A prototype keeps its constants alive, and they die with it. Today a prototype lives as long as its chunk: the entry thread's closure roots the main prototype, and a prototype roots its children. Loaded code does not become unreachable yet, so no program keeps strings from code it can no longer run. The unit test `a_prototype_owns_its_constant_strings` checks that an unrooted prototype and its strings are collected together.

The compiler already deduplicates constants within one function. Two functions using `"foo"` have two objects with the same bytes. Strings stay value-semantic: equality, table keys, and dead traversal anchors compare bytes. A string made at run time compares and indexes the same as a constant, and a test checks this with a host-made string.

## Snapshots

A prototype now writes the ids of its constant strings instead of their bytes, and decode fills the bytes from the string section. An id that names nothing, or names anything but a string, is refused as `DanglingReference`. So is a string named by two constants, as `InvalidStructure`. Installation never shares one, and refusing it keeps a small snapshot from decoding into many copies of one large string. The wire layout changed, so this is part of schema 6.

The mapping is ordinary graph state, not a cache. It was not left to be rebuilt after restore, because a lazily rebuilt cache would allocate at different points in a restored run than in an uninterrupted one. Object ids and object counts would then drift, and so would the automatic collection schedule of ADR 0021.

## Alternatives

A runtime-wide interner for short strings, as PUC Lua has. It would need weak entries or an immortal table, and it couples every string allocation to the collector. Nothing here needs it yet.

A lazily filled constant cache, rebuilt after restore. It is cheaper for constants that never run, but it breaks the exact allocation history above.

## Consequences

Loading a chunk allocates one object per distinct constant per function, and these count toward the object limit. `LoadBytes` is now a register write. Dynamic string construction (concatenation, the string library) will allocate normally when it exists; this ADR covers only constants.
