# ADR 0051 — Generational collection

## Context

Phase 3.28's incremental collector (ADR 0050) bounds the work of each step, but every cycle traces the whole live heap, and on write-heavy programs its atomic phase can take as long as a stop-the-world collection. Lua 5.4 runs in generational mode by default: frequent young collections that trace what was made since the last one, and a major collection only once memory has grown enough. Moonseed must also keep collection deterministic, charged in fuel, and checkpointable at any unit.

## Decision

**Ages.** Each arena keeps one age byte per slot beside the mark byte: `NEW`, `SURVIVAL`, `OLD1`, `OLD`, `TOUCHED1`, `TOUCHED2`, Lua's ages. Lua's `OLD0` exists only for its forward barrier, which Moonseed does not have, so restore refuses it. Ages mean something only in generational form, and are all `NEW` otherwise.

**Generational form** (`Collector::generational`). Old objects are black, or gray while touched; young objects are white and on their arena's young list, in the order made. Writes are barriers between collections.

**One barrier.** `Arena::get_mut` stays the only mutable access. On a black object it grays it, pushes it on the arena's `again` list, and makes an old age `TOUCHED1`. In generational form `again` is the remembered set, kept free of duplicates by the mark. `Arena::get_mut_storing(handle, references)` is the same barrier for a write that says whether it gives the object a reference. A write that gives none (a number stored, an entry removed) cannot make an old object refer to a young one, so it is no barrier between collections, as Lua's barrier checks that the value is collectable. While a cycle marks, every write is a barrier, because a write that moves entries could move them behind a scan. Table stores use it, so filling an old table with numbers does not make the next young collection trace it.

**Ages advance** as in Lua's `sweepgen`:
- A `NEW` object surviving a young collection becomes `SURVIVAL`, white again.
- A `SURVIVAL` object surviving one becomes `OLD1`, black, on the collector's revisit list, because what it refers to may still be young. A string, which refers to nothing, becomes `OLD` at once.
- An `OLD1` object is traced by the next young collection and becomes `OLD`.
- An old object written to becomes `TOUCHED1` (gray, in `again`). The young collection that traces it makes it `TOUCHED2`, on the revisit list, so the one after traces it again. It becomes `OLD` unless written to since.

Between collections no black `OLD` object refers to a white one, weak references aside (`gc::check_gen_invariant`, checked at restore and in tests).

**A young collection** runs whole before Lua runs again, like an atomic phase (`Collector::holds`), in bounded units charged to fuel. The executor may pause, and the runtime be checkpointed, between units. Its steps, each a snapshot phase:
1. The revisit list: `OLD1` and `TOUCHED2` objects become `OLD` and gray (Lua's `markold`).
2. Phase 3.28's atomic steps: the roots, `again`, propagation, weak modes, weak values, separation of the dead registered objects, resurrection, and weak keys. Marking only grays white objects, so old objects are traced only when listed.
3. The young sweep: a unit per young-list entry.
4. The touched correction.

Weak tables, ephemerons, and finalizers follow Phase 3.27's rules for the objects the collection traces. An old weak table can hold a young object only if written to, and then it is traced. A young finalizable object found dead is queued at once, resurrected, and freed by a later young collection with no major. An old one waits for a major, as in Lua. A resurrected object keeps aging normally. Roots are not aged: a young collection grays them like any cycle.

**Major collections reuse the incremental cycle.** A major collection, Lua's `fullgen` or a bad collection's `stepgenfull`:
1. Leaves generational form: a sweep that frees nothing makes every marked object white and every age `NEW` (Lua's `enterinc`).
2. Runs a whole incremental cycle in bounded steps; Lua runs between them, as in Phase 3.28.
3. At the end of the atomic phase decides (`Decide`) whether the sweep makes the survivors old (Lua's `sweep2old`). Survivors become black and `OLD`, or `TOUCHED1` if written to since; objects made during the sweep are young.

Lua's major collections stop the world; Moonseed's are as bounded as its incremental cycles. Collector behavior is not portable in Lua (the manual says so), and the visible semantics are the same.

**Scheduling** (`GcState::minor_done`, `major_growth`, `major_due`):
- A young collection is due once the minor multiplier's percent of what the last one kept is allocated (Lua's `setminordebt`). It comes no sooner than `Config::gc_min_debt`, the documented least allocation between automatic collections.
- A major collection is due once memory (what the last collection kept, and what was made since) grows past the major multiplier's percent of what the last major kept (Lua's `genstep`). That growth is at least twice `gc_min_debt`, so a young collection can always come first. It is capped at three quarters of the room left under the quota and under the object limit, the latter counted at the average size of the objects the last major kept. Young collections are capped at half, so near the limits they go on until old objects fill half the room.
- A step only notes that a generational step is due. The work loop decides between young and major when it runs, so whether the host polls a waiting run cannot move a collection.

**Falling back** (Lua's bad collections):
- A major whose survivors do not leave at least half its growth free does not make them old. Its sweep is incremental, and `GcState::bad` records what it kept.
- From then on cycles are incremental and scheduled by the pause.
- One that keeps less than an eighth more than the bad one returns to generational form; otherwise it records its own result.
- The declared mode stays generational; this is snapshot state, not a third mode. Lua compares atomic work, Moonseed what was kept, in logical bytes.

**Full and emergency collections** in generational form leave it, run a whole cycle, and enter it again, as Lua's `fullgen` does. While falling back they stay incremental, as Lua's `fullinc`. An emergency collection is always full, so old garbage filling the quota is reclaimed.

**`collectgarbage`:**
- `generational [minor [major]]` stores the minor multiplier as a byte and the major one divided by four, 0 leaving either alone, as `lua_gc` does. It returns the previous mode. Entering generational form is a full collection that makes survivors old, done before it returns. In generational mode, not falling back, it enters nothing, even while a major collection runs, as in Lua.
- `incremental` returns the previous mode and leaves generational form in the steps that follow; a sweep making survivors old finishes first.
- `step` follows Lua's `genstep` by state:
  - **Generational:** one whole young collection, or a whole major one when one is due and the step leaves debt to pay. `step(0)` clears the debt, so it is young. Either way it returns false, as Lua's leaves its collector between young collections, even after a bad major.
  - **Falling back after a bad major** (Lua's `stepgenfull`): a whole cycle that decides whether to return to young collections; a cycle already running finishes first. It returns true if it stays incremental, and false once it returns.
  - **A major collection running:** all of it, returning false.
  - **Incremental mode:** an incremental step, true when its cycle ended.
- The mode Lua chose (`GcState::generational`) and the collector's machinery (`Collector::generational`, false while a major runs on the incremental cycle) are apart: mode calls test the first. `generational` while generational and not falling back only stores the parameters, even during a major. While falling back, Lua's collector is incremental, so the call enters generational form with a full collection.
- `collect` is a full collection.

**Default mode.** `Config::gc_mode` defaults to `GcMode::Generational`, Lua 5.4's default. Booting runs a full collection that makes what exists old, as `lua.c` does after opening the libraries. Embeddings choose `GcMode::Incremental` at boot, and Lua may change mode at any time.

**Accounting** (revised by the stabilization):
- **The exact logical heap.** `GcState::used` is the sum of every object's logical size, kept exactly at every allocation, growth, compaction, shrink and free. The hard quota is on it, and so are `collectgarbage("count")` and `Runtime::memory`. `live`, `debt`, thresholds and the major base are scheduling estimates only; none authorizes an allocation.
- **What a sweep frees** stays in `used` until the sweep ends (`Collector::unreleased`), then leaves it at once. A restored sweep, which never had the dead objects, gives back the same amount at the same point, so a restored run sees the same `count`.
- **Shrinks.** A table that compacts or a userdata whose charge shrinks leaves the logical heap at once (`Heap::give_back`). No allocation is undone, so the schedule is left alone, and no collector work owed is forgiven. That is why a compaction cannot stall a sweep.
- **Threads.** A thread is charged for what it holds beyond its object: stack slots up to the registers its frames can write, and the bytes its builtins hold while they work (`ThreadObj::charged_slots`, `charged_held`). Those shrink without anything being freed, so a collection that begins to trace a thread charges it again for what it holds then. A young collection traces only the threads that ran since, as running a thread is a write to it. No collection walks every thread to measure them. While library work runs out of its frame (`Heap::working`), its bytes are in no frame, so a collection then keeps the thread's charge.
- **Scheduling.** A young collection takes `used` as what it kept. A major collection's base is `used` less what was made while its sweep ran.

**Finalizable objects by age.** `Finalizers::registered` stays in registration order. Two positions mark where it is known old: `old_until` (entries before it are old objects) and `new_from` (entries from it on were registered since the last young collection), Lua's `finobjold1` and `finobjsur`. A young collection's `separate` looks only from `old_until` on: old objects are never white during one. After each young collection the positions move up one generation. A sweep making survivors old sets both to the list's end, and leaving generational form sets both to 0.

**`again` membership.** One bit per slot records that the slot is on its arena's `again` list, whoever holds the slot. An entry left by an object since made white or freed is never doubled: when the slot's object is gray again, the entry already there serves. A gray-stack entry no longer gray is skipped.

**Snapshots** (schema 19):
- The mode, multipliers, major base, falling back, every object's age, the young lists, the revisit and touched lists, and a young collection's progress.
- Each thread's charges, what the running sweep frees, and the finalizer positions.
- `used` itself is not written. Restore counts it from the decoded objects, plus the image's `unreleased`. A forged `unreleased` can only make the heap count larger than it is, never let an allocation pass the quota.
- Marks and ages are written only where they differ from the default: black and `OLD` off the young lists in generational form, white and `NEW` otherwise. A generational snapshot is no larger than an incremental one.
- A young sweep restored starts its lists over, skipping what it passed.

Restore refuses:
- an unknown age, or `OLD0`;
- an object on a young list that is not young, or listed twice;
- a white object on no young list;
- a touched object not in `again`;
- an `OLD1` or `TOUCHED2` object not remembered, or remembered twice;
- a thread charged less than it holds;
- finalizer positions out of order, or naming a young object old;
- `unreleased` outside a sweep;
- a major base past the quota;
- falling back in generational form;
- a young collection's phase outside one;
- a decision in generational form;
- generational form in incremental mode between collections;
- a state breaking the generational invariant.

**Host writes** to a userdata's payload (`Runtime::with_userdata_mut`, `with_userdata_bytes_mut`) first finish a young collection or an atomic phase running, as any host call by id does, and give the object no reference.

**Restore** checks one generational invariant, `gc::check_gen_invariant`, in every phase of generational form. No young object can be freed while an old object refers to it, weakly or strongly, unless a young collection traces the old one first:
- **Between young collections, and as one begins:** an old object refers to no white one unless remembered. Gray objects wait in `again` or on the gray stack. `TOUCHED1` objects are gray, and `OLD1` and `TOUCHED2` objects are remembered.
- **In a young collection's atomic phase:** the tri-color invariant, with weak tables not reached this cycle counted strongly. No object is `OLD1` or `TOUCHED2` any more.
- **In its sweep and correction:** nothing is gray. `TOUCHED1` objects are on `touched`, and `OLD1` and `TOUCHED2` objects are remembered. A black object neither listed nor a young survivor the sweep has still to pass refers to no white object and no new one, which the sweep would make white.
- **In a sweep making survivors old:** survivors are new and marked until reached, then old and black or touched and gray; objects made since are new, white and listed. No black object refers to a white one.
- **Finalizable objects:** the registered objects before `old_until` are old.

Tests check it, the tri-color invariant, the exact logical heap, and that no handle is stale, at every step of torture programs. Bounded random tampering with ages, marks, lists, positions and charges never restores into a state that breaks them.

## Alternatives

- **A forward barrier (Lua's `OLD0`):** the barrier would need the value written at every write site. The parent-side barrier covers every kind of object from one accessor; `get_mut_storing` gives the one fact a site knows cheaply, whether it stores a reference.
- **Young collections interleaved with Lua:** objects made during a young collection would need ages of their own, and the young list would change under the sweep. They are short (their work grows with young and touched objects), and a host's quantum still pauses them.
- **Stop-the-world majors, as Lua's:** would undo Phase 3.28's bounded pauses.
- **Lua's major trigger counting only what young collections keep:** missed the garbage a finalizer makes while collection waits, which Lua's trigger counts.
- **Writing every old object's mark:** about 10 bytes per object more in every generational snapshot.

## Consequences

Snapshot schema 18 and GC policy 11 (19 and 12 after the stabilization). Bytecode 11, tables 3, fuel 6, and binary chunk format 2 are unchanged: a unit of collector work means what it did. Allocation-heavy programs with a tiny live heap run 5–8% slower by default (a young-list entry per allocation, young collections in one slice); programs with a live heap collect up to 13 times less (PERFORMANCE.md).

**From the milestone review** (one round, by a separate reviewer; no blockers):
- Major, fixed: a table compacted while a sweep made survivors old, before the sweep reached it, was given back from the debt and then counted at its smaller size, so the estimate fell below the heap (64,192 bytes, 10%) and a fill passed the quota by 64,034 bytes. A userdata's shrink did the same. Nothing is given back during that sweep now; every give-back goes through `Heap::give_back`.
- Major, fixed: restore accepted an image tampered mid young collection (an old table pointed at a young one nothing else held), and mid sweep to old (a touched object in no list), which later left a handle to a freed object. Both are refused now.
- Minor, fixed: giving bytes back lowered the step schedule, so a compaction during a major's sweep stalled it until the object limit forced an emergency collection. The schedule is left alone.
- Minor, fixed: `step(0)` did a major collection when one was due; Lua's clears the debt, so it does a young one (corpus section M, checked against Lua 5.4.9).
- Minor, fixed: `collectgarbage("generational")` during a major collection asked for a full collection; it enters nothing now, as in Lua.
- Minor, fixed: a host write to a userdata between a young collection's units could leave a stale `again` entry, tracing the object twice; host payload writes now finish the young collection first.
- Minor, accepted: every young collection walks every registered finalizable object (`separate`, a unit each) and every thread's frames (to measure what threads hold, uncharged). Both are bounded by the object limit; with 9,000 finalizable objects, young collections did 9.7 million units over a run where incremental cycles did 44.7 million.
- Held up: barrier coverage (no mutable access outside the collector skips `get_mut`), two further torture programs matching Lua 5.4.9 in both modes with the invariant checked at every quantum-1 point and restores every 3 to 11 steps, determinism at quanta 1, 2, 5, and 97, every legitimate checkpoint restored, `lua_gc`'s encodings and previous modes, mode switches, and the object limit.

**Stabilization** (schema 19, GC policy 12). The first review's findings were reopened as acceptance issues and fixed by design, not by patching the reproducers:
- #1 and #3: the exact logical heap above replaced the estimate as the quota's authority. The give-back that stalled a sweep is gone with it.
- #2: one validator for every phase. Seeded random tampering found five more gaps before it passed, all closed:
  - an old object pointed at a marked new object mid atomic phase;
  - an old object from the revisit list, already gray, pointed at a new one;
  - a white new object listed after the atomic phase;
  - a survivor listed young in a sweep making survivors old;
  - finalizer positions naming an object younger than its place.
- #4: `step` follows `genstep` and `stepgenfull` by state. It also returned true after a bad major, where Lua returns false.
- #5: mode calls test the declared mode.
- #6: no young collection looks at old finalizable objects or walks every thread.
- #7: one bit per slot keeps `again` duplicate-free.
- Found on the way:
  - restore refused a legitimate checkpoint taken while an empty ephemeron table was being scanned (Phase 3.28);
  - inlining the barrier's new slow path slowed calls 10–12%, fixed by moving it out of line.

**From the stabilization review** (one round, by a separate reviewer):
- **Blocker, fixed:** library work runs out of its frame (`run_lib`, `run_aux`). An emergency collection inside it, here `gsub` allocating a capture near the quota, measured the running thread without that work's bytes. The logical heap fell about 1.2 KB below the heap at some quotas, and the checkpoint after was refused. While library work is out of its frame (`Heap::working`), a collection now keeps the thread's charge. The review's reproducer is a permanent test, and it fails without the fix.
- **Held up:**
  - the exact count through finalizer-, coroutine- and held-bytes-heavy runs with restores, which give the same `used`;
  - no stall;
  - the tamper fuzz at six more seeds;
  - `step` against Lua 5.4.9;
  - mode calls mid-major, mid sweep to old, and mid fallback;
  - finalizer positions through resurrection, re-registration, mode switches, and bad majors;
  - the `again` bit.
