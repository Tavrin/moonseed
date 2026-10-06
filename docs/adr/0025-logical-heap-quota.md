# ADR 0025 — Aggregate logical heap quota

## Context

Before Phase 3.13, memory was bounded by two things:
- the object count (`Config::max_objects`, at most 10,000);
- the 1 MiB bound on one string made by `..`.

Neither bounds the total:
- 10,000 strings of just under 1 MiB each is about 10 GB.
- A single table has no entry limit.

A script could still exhaust the host's memory. Lua also has to be able to catch the resulting error (ADR 0024), so the limit must stop the allocation before it happens, not kill the process after.

## Decision

**The quota.** `Config::max_logical_heap` is a hard quota on the logical heap. The default is 64 MiB; Phase 3.13 set 256 MiB, and Phase 3.14 lowered it after the RSS measurement below. The default is a provisional setting for embedding untrusted scripts, not a Lua compatibility promise: trusted applications raise it, and named sandboxed and trusted profiles may replace the single default once the accounting and real workloads justify them. It uses ADR 0021's logical-byte costs:
- 32 per object;
- 32 per table slot, live or a dead anchor;
- a string's length;
- 8 per reference a closure or prototype holds;
- 256 per thread.

The costs are the same on every target, so the quota is reached at the same instruction on native and wasm32.

**What counts.** `live + debt` is what survived the last collection plus everything allocated since. It is an upper bound on the live heap. (Superseded by ADR 0051's stabilization: the quota is on `GcState::used`, the exact sum of every object's logical size, and `live` and `debt` only schedule collections.)

**The check.** Before an allocation of `n` logical bytes, `live + debt + n <= quota` must hold. Every allocation checks:
- strings and tables in the heap;
- closures, upvalues, threads, and prototypes in `ensure_room`;
- a table insert that adds a slot. An insert over a live key, or of nil, never fails.

**Collect first, then fail.**
- An instruction that allocates first calls `make_room(objects, bytes)`. If the allocation would not fit under either limit, `make_room` runs a full collection at the start of the instruction, before anything is held outside the roots.
- If it still does not fit, the allocation returns `MemoryLimit`, and the step boundary raises `LuaFault::Memory` (ADR 0024).
- A table store the quota refuses collects once and tries again. The refused store changed nothing, and its table, key, and value are in registers, constants, or the frame's recorded targets, so collecting there is as safe as at the instruction's start. This covers field and indexed stores, assignment stores, `SetList`, and `SetTable`.
- A native's `raw_set` does not collect. Values a native holds while it runs are not roots.
- A protected call that caught a memory error collects before it returns. The frames that held the memory are gone by then, so the code after `pcall`, natives included, finds the room they freed.
- With automatic collection off, nothing is collected and the error comes at once. The host chose to collect by hand.
- These rules change where collections run, so the GC policy revision is 2.

**The schedule near the quota.** The automatic threshold is also capped at half the room left under the quota. Collections come closer together as the heap approaches the quota, so an allocation rarely finds uncollected garbage in its way.

**The error needs no memory.** Its object, "not enough memory", is a string reserved at boot (ADR 0024). A message handler is not called for it. After `pcall` returns `false`, the frames that held the memory are gone, and the next collection frees it.

**Natives.** A native's `raw_set` that would pass the quota fails with `RawSetError::Memory`. If the native then returns `Fault`, the error is `Memory`, not `Native`.

**Separate limits.** The object limit stays a separate limit with its own check. The 1 MiB string bound stays too. `..` now checks it before copying, so a failing concatenation of two large strings costs about 400 ns instead of 20 µs.

**Snapshot.** The quota is in the snapshot's GC section (schema 8), and a restored runtime keeps it. Restore refuses a quota of 0. As with fuel, restoring an older snapshot restores its accounting. See SECURITY.md.

## What the quota does not cover

- **Thread stacks.** They are not charged by size. A thread's stack is bounded by its frame limit instead: 1,080 frames of at most 256 registers, about 4.4 MB. Source code cannot create threads yet; hand bytecode can. The coroutine library must charge stack growth before source can make threads in bulk.
- **Host-side buffers.** Native argument vectors, host results, and the journal are not charged.
- **Real memory.** Logical bytes are not RSS. On x86_64:
  - A table filled with integers to 256 MiB, the first default, reached 1.3 GB RSS, about 5× logical.
  - A table of 64 KiB strings reached 265 MB, about 1×.
  - Hosts that need a tighter bound should set the quota for their worst case.

## Alternatives

- **Count real allocations through a global allocator hook.** The count would differ between targets, allocators, and builds, and the error would move with them. It would also measure the host's allocations as well as the script's.
- **Check only at collection.** A single instruction could still allocate far past the limit before a collection sees it.
- **Per-object caps only.** They already exist, and they do not bound a sum.

## Consequences

- **Deterministic:** a script that hits the quota fails at the same instruction everywhere, and `pcall` catches it.
- **Cost:** the check is one comparison per allocation. A table insert looks the key up a second time only when the new slot would not fit.
- **Known cost problem, not fixed here:** filling one table to a 256 MiB quota takes 15 s and 1,695 collections. ADR 0021 caps the collection threshold by the object headroom (half the free object slots × 32 bytes, about 160 KB with the default limits). Table slots add debt without using object slots, so a growing table collects every few thousand inserts, and each collection marks the whole table. That is quadratic in the table's size. The fix is to trigger on objects made since the last collection, separately from debt. That is a GC policy revision and belongs to the GC track.
