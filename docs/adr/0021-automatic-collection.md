# ADR 0021 — Deterministic automatic collection

## Context

The collector (ADR 0004) was correct but ran only when the host called `Runtime::collect`. A loop that made garbage reached the object limit (`MemoryLimit`) even when almost nothing was live. Lua manages memory automatically; a general-purpose runtime has to as well. Moonseed also promises that a run, a run sliced into quanta, and a run restored from a checkpoint behave the same on every certified target. So when collections happen has to be decided by the program's history, not by the host allocator, the clock, or the target.

## Decision

The collector is unchanged: stop-the-world mark and sweep, still the correctness reference, still available as an explicit full collection. This ADR decides when it runs.

### Logical cost

Every allocation adds a fixed logical cost to a debt counter. The unit is the logical byte (`heap::cost`); it is a number, not a measurement of host memory.

| Allocation | Cost |
|---|---|
| Any object | 32 |
| A string | 32 + its length |
| A table slot that grows the table (live entry or dead anchor) | 32 |
| A closure | 32 + 8 per upvalue |
| A prototype | 32 + 8 per instruction, constant, capture, and child |
| A thread | 256, stack included |

Table growth is charged in `Heap::table_insert`, the one place a table gains a slot. Stack growth, `Vec` capacity, and hash-index capacity are not charged: they depend on the host allocator, not on semantics.

### Trigger and threshold

When the debt reaches the threshold, the next safe point runs a full collection. After any full collection (automatic, under pressure, or requested) the debt is reset. The survivors' logical size `live` is summed with the same costs, and the next threshold is:

```text
threshold = max(OBJECT, min(max(live, min_debt), OBJECT * (max_objects - live_objects) / 2))
```

- **`live`:** collect again once as much has been allocated as survived. The heap may double between collections, and a mostly-live heap is not collected over and over.
- **`min_debt`:** `Config::gc_min_debt`, 64 KiB by default. It stops small heaps from being collected after every few allocations.
- **Headroom term:** collect before half the remaining room under the object limit is used, since every object costs at least `OBJECT`. The automatic collection therefore runs well before the hard limit.
- **Lower bound `OBJECT`:** guarantees progress.

### Safe points

An allocation may raise the debt in the middle of an instruction. The collection itself starts only at the top of `run_hot`'s slice loop: after a step has finished, before the next one starts. That is where an explicit collection is legal: frames and registers are written back and every pending operation is in the heap. It is the same protocol as before, reached one more way. There is no second root protocol.

`run` also collects before it returns, if the last step made a collection due. Otherwise a step that ends a run (a wait, a fault, a yield) would leave the collection to the next `run`. The host's calls in between, such as `complete_wait` clearing a native's arguments, would then change what survives. The schedule is the same whatever the quantum and whatever the host does between runs.

### The hard limit

The automatic threshold and the hard object limit (`Config::max_objects`, `MemoryLimit`) are separate. The threshold normally collects long before the limit. Instructions that allocate objects (`NewTable`, closure creation, `NewThread`) check first. If their objects would not fit, they collect before allocating anything, at a point where every reference is still in a root.

A program that keeps everything it allocates still reaches `MemoryLimit`. The limit is VM state that a checkpoint restores, as before. It is not a host-wide quota: restoring an old snapshot hands back its room.

Remaining gap: an allocation made by host API calls outside a step (binding a native, for example) does not collect under pressure.

### Snapshot state

Schema 6 writes the collection state:
- the automatic flag;
- the debt and the threshold;
- `min_debt`;
- the live size at the last collection;
- the collection count.

It also writes a GC policy revision in the header, `GC_REVISION = 1`. A decoder accepts only its own revision, so a snapshot never continues under different costs or a different threshold rule.

Mark bits and work lists are not state. A checkpoint cannot happen during a collection.

Host roots are not snapshot state (ADR 0003): a restored runtime starts with none. An object kept alive only by a host root survives collections in the original run. After a restore it survives only if the host roots it again before the next collection, and the schedules match only if it does.

The snapshot already contained every live object, garbage included. So debt, threshold, and object counts are canonical, and a restored run collects at the same points as the original. Tests check this with a collection log, under quanta 1, 2, 3, and 7 and restores every few slices. The wasm probe's `gc_schedule_fingerprint` checks it between native and wasm32.

### API

- `Config::auto_gc`: on by default.
- `Config::gc_min_debt`.
- `Runtime::set_auto_gc`: its setting is snapshot state.
- `Runtime::collect`: a full collection now.
- `Runtime::memory`, which returns `MemoryUsage`: objects, the limit, logical bytes, debt, threshold, collections, and the flag.

There is no `collectgarbage` yet.

## Alternatives

- **Trigger on host allocator bytes, or on `size_of`.** Not deterministic across targets or allocators.
- **Trigger on wall time.** Not deterministic.
- **Collect only when an allocation fails.** The failing allocation is in the middle of an instruction, where Rust locals may hold the only handle to a new object. Collecting there needs pinning at every allocation site.
- **Incremental or generational collection now.** It needs write barriers and a measured baseline. This ADR gives the baseline, and the stop-the-world collector stays the reference to compare against.

## Consequences

Programs that make garbage run without host intervention. Weak tables and finalizers will make collection timing observable to Lua. Because timing is already a function of the program's history, those features can be deterministic too. Their semantics must be defined against collection points, not wall time.

Full-collection pauses grow with the live heap, about 9 ns per object on the development machine (`PERFORMANCE.md`). An incremental collector is the answer to long pauses, and will be measured against these numbers.
