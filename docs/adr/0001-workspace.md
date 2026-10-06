# ADR 0001 — One runtime crate

## Context

A large crate graph makes the early refactors expensive. The runtime, collector, and snapshot format are still one design.

## Decision

`crates/moonseed` holds the kernel. `crates/moonseed-wasm-probe` only exports it. No parser, GC, or snapshot crate until a boundary has been stable through a change.

## Alternatives

Eleven crates from the early sketch. Rejected for Phase 1.

## Revisit

When the compiler or the snapshot codec can change without rewriting the VM in the same patch, and the public API is ready to depend on that split.
