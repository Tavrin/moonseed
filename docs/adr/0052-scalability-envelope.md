# ADR 0052 — The scalability envelope: one resource model, and every state it allows is a snapshot

## Context

Moonseed's limits came from the proof kernel: 10,000 live objects per arena, 1 MiB strings, 1 MiB snapshots holding at most 1 MiB of strings in all, 10,000 entries per table in a snapshot, and 10,000 instructions, 4,096 constants and 256 functions in compiled code. The 64 MiB logical-heap quota (ADR 0025) was never the binding limit: a program building 100,000 small strings failed on the object limit at under 2 MiB of heap, and a table of 12,000 entries ran but could not be checkpointed.

A runtime whose main feature is portable checkpoints has to keep one invariant, which ADR 0028 already kept for the stack:

> Every runtime state the configured limits allow can be snapshotted, and restored by a runtime with the same or larger limits.

Raising each constant is not enough. A larger limit makes hostile input more dangerous wherever the decoder trusted a constant to bound an allocation.

## Decision

### Three kinds of limit

| Kind | Bounds | Where |
|---|---|---|
| Runtime resource limits | what Lua execution may create | `Config`, `Limits`, `Runtime::limits` |
| Compiler limits | what source or a binary chunk may compile to | `CompileLimits`, `compile_with_limits` |
| Decoder and container limits | what untrusted bytes may make before validation | `Limits::max_snapshot_bytes`, the decode budget, structural ceilings |

They are compatible but not identical: a container ceiling bounds what any configuration could hold, while the runtime limit is what this one does.

### Runtime resource limits

| Limit | Default | Range | Notes |
|---|---|---|---|
| `max_logical_heap` | 64 MiB | 1 ..= 2^31 | the quota (ADR 0025); the ceiling keeps a full heap's snapshot under the snapshot ceiling and a table's slots under their `u32` index |
| `max_objects` | 1,048,576 | 1 ..= 2^28 | all kinds together |
| `max_stack_slots` | 50,000 | 1,024 ..= 100,000 | per thread (ADR 0028), unchanged |
| `max_string_bytes` | 2^30 | 1,024 ..= 2^30 | one string; by default only the quota bounds a string |
| `max_snapshot_bytes` | 128 MiB | 4,096 ..= 2^31 | one snapshot, written or read; host policy |

`Config::limits` gives the clamped set and `Runtime::limits` the set a runtime runs under. Every limit stays enforced: a sandbox lowers them, and none is removed.

- **Objects.** The smallest object is 32 logical bytes, so the default quota admits about two million objects. A slot costs 32 to 200 bytes of host memory besides what the object owns (string 40, upvalue 32, closure 48, native closure 72, userdata 88, table 120, prototype 160, thread 200), so the default is half of that, which keeps the slots of a full heap near 200 MB. A heap of closures reaches the default object limit at 54.5 MiB. Arenas grow on demand from empty and check only the ceiling; slot indexes stay `u32`, with room for as many dead objects again, so `Handle`, `ObjectId` and `Value` (16 bytes) are unchanged.
- **Strings.** A string is checked against the string limit and the quota before its bytes are allocated. `..` reports a result that does not fit under the quota now without copying anything, and the runtime collects and tries once more. `string.rep` and table-concatenation preflight their total, while `string.format`, `string.pack`, `gsub` and tracebacks charge their buffers as they grow, against `Runtime::string_room`: the string limit and what the quota leaves. `string.dump` uses the same room. The longest transient copy is an argument string that already exists. `Heap::alloc_string` checks the string limit too.
- **Tables.** A table grows by doubling (slots and index), within the quota and the object limit. An insert used to drop every dead anchor whenever one existed, rebuilding the whole table. Deleting a key and inserting a new one, over and over on a table of 100,000 entries, cost quadratic time: 20,000 such pairs did not finish in 120 seconds (PUC Lua: 0.09 s). An insert now drops the anchors only once they outnumber half the live entries, so each rebuild is paid for by as many deletes. The same pairs now take 0.10 s. A key may have dead anchors before its live slot, which is always its last; `next` from a dead key is unchanged (ADR 0010). Table semantics revision 4.

### Compiler limits

`CompileLimits` bounds compilation, and `compile_with_limits(source, &limits)` applies it. `compile` and Lua's `load` use the defaults.

| Limit | Default | Ceiling (validators, binary chunks, snapshots) |
|---|---|---|
| Instructions per function | 2^20 | 2^24 |
| Constants per function | 2^20 | 2^24 |
| Functions per chunk, its own included | 65,536 | 2^20 |
| Source bytes, a literal included | 16 MiB | 2^30 |

- Jump offsets are now `i32`, and constant, field-name and child indexes `u32`. `Op` is still 16 bytes, and a compile-time assertion keeps it there. Bytecode revision 12.
- A function's children are bounded only by the chunk's function limit, not 200. Parser nesting (200 levels), function nesting (64), locals (200), upvalues (200) and registers (250) are unchanged: they bound recursion or match Lua's own, and no plausible source reached them.
- The compiler reserves nothing in proportion to a limit, only to its input. Deep nesting is still a `Limit` error, never a stack overflow.
- The binary chunk format stays at revision 2: its counts were already `u32`, and the widened instruction operands come with the bytecode revision, which a chunk records and `load` checks. The reader checks each count against the input left and the ceilings before it allocates.
- A `load` reader holds at most `load`'s 16 MiB of source, and so does a snapshot of one.

### Snapshots

- **Size.** Measured snapshots take 0.33 to 0.99 bytes per logical byte (threads 0.33–0.46, closures 0.66–0.85, mixed 0.68–0.87, tables 0.73–0.80, strings 0.79–0.87, userdata 0.84–0.99) at 1 to 60 MiB of heap (PERFORMANCE.md, Phase 3.30). A full default heap makes a snapshot of at most about 64 MiB, so the default bound is 128 MiB. The encoder checks the bound after every object, so it stops within one object of it. It still builds the image and then one output vector; streaming is later work.
- **Decoding untrusted bytes.** A snapshot longer than the bound is refused unread. Every count is checked against what is left of the input (each item takes at least a byte), and no list reserves more than 1,024 items before they decode. The decoder keeps a budget: four times the logical heap it may restore into, plus the snapshot's length, charged 16 bytes per counted item (debug information included) and a byte per byte of strings and payloads. A valid snapshot stays inside it: the charge per item is below each item's logical cost. A malformed one, with every field legal on its own, is refused when the budget runs out instead of after it decoded everything. The charge is not the host memory: a decoded item is up to about three times its charge (a table slot is 40 bytes), so a malformed snapshot can make up to about twelve times the quota before it is refused, and a valid restore of a full default heap allocated 128 to 469 MiB beyond the snapshot, the most for a heap of a million closures (measured, PERFORMANCE.md). Objects are counted against the object limit section by section as they decode. Every remaining structural ceiling is what any configuration could hold: 2^28 objects, 2^30-byte strings, 2^28 entries in a table.
- **Lookups.** Restore validation used to look a string key up by scanning every string, coroutine chains by scanning every thread, and native symbols by scanning the symbols read so far, which is quadratic in a large heap: a malformed snapshot of a million symbols would have taken some twenty minutes to refuse. The image now indexes its strings and threads by id once, and symbols go through a hash set.
- **Binary chunks.** `load` gives the chunk reader a budget of twice the heap's room plus the chunk's length, charged like a snapshot's, and caps its reservations: a 24 MiB chunk could otherwise decode to about 200 MiB before validation.
- **Schema 20.** The string limit is written after the stack bound. There is no bound on all strings' bytes together.

### Restore into other limits

`Runtime::from_snapshot_with_limits(bytes, registry, domain, limits)` restores under the host's `limits`. `Runtime::from_snapshot` is the same with `Limits::default()`.

- The restored runtime runs under, for each limit, the smaller of the snapshot's and the host's. A host with equal or larger limits gets the runtime exactly as it was, and no host limit is ever raised.
- A host limit smaller than the state needs refuses the snapshot with `LimitExceeded` before any runtime exists. That applies to:
  - more live objects;
  - a longer stack;
  - a longer string;
  - a larger logical heap;
  - a longer snapshot.
- The logical heap may already be past the snapshot's own quota when a host grew its userdata (ADR 0042). (Corrected in Phase 3.31: restore has refused userdata payloads past the quota since ADR 0045, so `snapshot()` now refuses such a heap with `LimitExceeded`; see ADR 0053.)
- The snapshot bound is not snapshot state. A restored runtime writes snapshots within the host's bound.
- A library buffer in progress (a `table.concat`, `gsub` or traceback text) is not checked against a smaller restored string limit: it fails at its next growth with a Lua memory error, after the runtime exists. Only string objects are checked before.

### Measurements at scale

The collectors show no superlinear work from 100,000 to 500,000 live objects: a major cycle's work per object falls (incremental 6.2 to 4.3 units, generational 3.7 to 3.7). The longest young slice was 3.0 ms and the longest major slice 9.4 ms, with no quantum. A young collection traverses a whole old table that was written to, as Lua's does: its work grows with that table, not with the objects remembered. Snapshot encoding of a heap near the default quota takes 0.24 to 0.52 s and restore 0.29 to 1.34 s (release build, loaded machine). A 50,294-object, 13 MiB state, stopped in generational work, crosses between native and wasm32 with byte-identical snapshots both ways. Tables are in PERFORMANCE.md.

### Review

One review round. Its findings, and what was done:

- Dead anchors kept uncharged copies of string keys, and amortized compaction let them accumulate past the quota (found by the first, interrupted pass). Fixed: an insert also drops them once their bytes pass the live keys' by 4 KiB.
- Debug information decoded outside the budget, with full reservations. Fixed.
- The binary-chunk reader decoded 8 to 12 times its input without a budget. Fixed (above).
- The native-symbol section was quadratic. Fixed (above).
- The decode budget understated host memory. The text above now gives the measured figures.
- A quota past 2 GiB could not be snapshotted, and a table past 2^28 slots neither. Fixed: the quota is clamped to 2^31.
- Host lookups by object id (`with_userdata_*`, `call_closure`, `resume_thread`, wait completion) scan the heap, so a host touching many objects in a large heap pays for each. Not changed: it belongs to the embedding API (Phase 3.31).
- A smaller restored string limit is not checked against library buffers in progress. Recorded above.

Separately, widening jump offsets to `i32` had put the offset into the value every hot instruction returns, doubling it: numeric loops went from 30 to 49 ns, and calls slowed by 20 to 40%. The jump now carries no offset; the dispatcher reads it from the instruction. Loops and calls are back at parity. Rows through cold handlers (metamethods, globals) still differ from the parent commit by 0 to 17% depending on layout (PERFORMANCE.md).

## Alternatives

- **Raise every constant to a large number.** This was rejected because each constant was also the decoder's only defence: a 10,000-entry bound made `Vec::with_capacity(count)` safe, but a 2^28 bound does not.
- **Restore under the snapshot's own limits, as before.** This was rejected because a host could not lower a limit for an untrusted checkpoint, and a forged header could raise the decoder's own bounds.
- **Refuse a snapshot whose recorded limits exceed the host's.** This was rejected because a state well inside a smaller host limit is still valid for that host. The rule checks what the state uses.
- **A separate default string limit, such as 64 MiB.** This was rejected because a host that raised the quota would still have strings cut short. The quota bounds strings by default, and a sandbox can lower the string limit separately.

## Consequences

- Snapshot schema 20, bytecode revision 12, table semantics revision 4; binary chunk format 2 and GC policy 12 unchanged. Snapshots of earlier schemas are not restored.
- The object limit is part of the collector's schedule (ADR 0051): with a million objects of headroom, the major-collection threshold follows bytes, as in Lua. A test that relied on 10,000 objects of headroom to make majors bad now sets that limit itself.
- `Config` has two new fields. `Limits`, `Config::limits`, `Runtime::limits`, `Runtime::from_snapshot_with_limits`, `CompileLimits` and `compile_with_limits` are new public API.
