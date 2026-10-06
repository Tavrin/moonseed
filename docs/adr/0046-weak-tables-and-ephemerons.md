# ADR 0046 — Weak tables and ephemerons

## Context

Lua makes a table weak through its metatable's `__mode`: `k` for weak keys, `v` for weak values, both for both. A weak-key table with strong values is an ephemeron table: a value is reachable through it only when its key is reachable without it. Until now Moonseed ignored `__mode`, so weak tables held everything forever (`closure.lua` waited for one to clear until the fuel ran out). The collector is a full stop-the-world mark and sweep, and stays one: this phase gives it Lua's semantics, and a later collector (Phase 3.28) must keep them.

## Decision

**A weak table is an ordinary table.** No value kind, no separate table type. The collector reads the mode from the metatable each time it traverses the table, as Lua does: `__mode` must be a short string (at most 40 bytes, Lua's short-string bound), and `k` or `v` count when they come before any zero byte. A change of `__mode`, or of the metatable, takes effect at the next collection; nothing caches it.

**What can be cleared.** Tables, Lua closures, native closures (Lua's C closures), threads, and full userdata. Strings are values to weak tables, as in Lua, and are never cleared for being weak; nor are numbers, booleans, light userdata, or builtins (Lua's light C functions). `weak_object` in `gc.rs` is the one place that decides.

**Marking.**
- Weak values: keys are traced, values that can be cleared are not.
- Weak keys and values: neither side is traced (strings are marked, and kept).
- Ephemerons (weak keys only): a table is deferred until the rest of the reachable graph is marked; then each entry whose key is marked has its value traced, and each entry whose key is not yet marked parks its value in a map keyed by the key object. Marking an object releases the values parked under it. Chains settle in any order, across any number of tables, in time linear in the entries: there are no repeated passes and no pass limit. A cycle through values back to their keys keeps nothing.

**Clearing** uses the table's own delete, so a removed entry leaves a dead anchor and `next` goes on past it after a collection (ADR 0010); the anchor keeps nothing alive. Weak values are cleared before finalizers are looked for, weak keys after (ADR 0047).

**Explicit collections and dead slots.** PUC Lua scans a running thread's stack only up to the top of the current call, so a temporary left above it by finished code keeps nothing. Moonseed scans whole stacks; `collectgarbage("collect")` and `("step")` therefore clear the active thread's slots above the call's arguments first, which nothing live occupies at a call. Without that, a key left in a dead register by a finished `for` kept an ephemeron alive where Lua frees it.

**Snapshots** write every object in the heap, weakly held ones included, so a weak table holds after restore exactly what it held before; inclusion in a snapshot is not reachability, and the next collection decides as it would have without the checkpoint.

## Alternatives

- **Iterating ephemeron tables to a fixed point**, as PUC does. Correct, but quadratic on chains built against traversal order; a waiting map is linear.
- **Caching the mode per table.** Stale when a shared metatable changes; reading it costs one raw lookup per table that has a metatable.

## Consequences

GC policy 9 (with ADR 0047). Measured: a full collection over 4,000 live entries costs about 8 ns per entry for a strong table, 13 with weak values, 14 for an ephemeron table, 15 per link for a 4,500-link chain built backwards; clearing 9,000 dead weak values about 52 ns each (PERFORMANCE.md).
