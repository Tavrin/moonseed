# ADR 0004 — Stop-the-world reference collector

## Context

The kernel has to prove cycles, roots, and upvalues before it is worth building an incremental collector.

## Decision

Iterative mark and sweep from explicit roots. No weak tables, no finalizers, no incremental phases. Objects are traced by slot index from values stored in live objects. Collection is a caller-invoked safe point.

## Alternatives

Start with a generational or incremental collector. Rejected until the reference collector has a failing test an incremental one must also pass.

## Revisit

When a real script’s pause time is measured and is too long. Keep this collector runnable as an oracle.
