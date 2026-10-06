# ADR 0003 — Generational handles and ObjectId

## Context

Snapshots and collection need an identity that is not an address. Arena slots are reused.

## Decision

`ObjectId` is monotonic, never reused, and is the only id in a snapshot. `Handle` is `{index, generation}` and is not serialized. Restore allocates fresh slots. The owner token is a process-local atomic counter, not OS entropy, and is not serialized. A `Root` is a strong root tied to that token.

## Evidence

On the bench profile, a checked lookup cost about 1.5× a raw slot read (about 1 ns versus about 0.7 ns) on an AMD Ryzen 9 7945HX, rustc 1.98.1. That is not an order of magnitude and is not why the canonical program sits near 95 ns/instruction. Keep handles.

## Alternatives

`gc-arena` branded pointers. Not used. The cost above does not justify switching during Phase 1. NaN-boxing was not tried.

## Revisit

If a later profile shows handle checks as a material fraction of dispatch on a realistic script, measure a denser value representation without putting addresses into snapshots.
