# ADR 0005 — Suspension kinds and effect ids

## Context

A fuel pause must not look like `coroutine.yield`. A checkpoint around a host call must not run the effect twice or pretend it can undo one that already happened.

## Decision

Separate `Paused`, `LuaYielded`, `Waiting`, `LuaError`, and `Terminated`. `CallHost` stops in `Prepared` before the host runs. `Waiting` stays until `complete_wait(key, result)`. Fuel is charged once on entry.

`EffectId` is `(domain, sequence)`. The VM stores `next_sequence` and the pending call, not a history. The journal stores the outcome. Restore requires the expected domain.

## Alternatives

One `Yield` reason for every stop. Rejected. A VM-side log of every committed effect. Rejected because it grows without bound and still does not own the external journal.

## Revisit

When a host operation returns a non-integer outcome (an entity id, a string). The journal’s outcome type has to grow with that, still keyed by `EffectId`.
