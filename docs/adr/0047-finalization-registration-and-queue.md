# ADR 0047 — Finalization: registration and the queue

## Context

Lua 5.4 finalizes tables and full userdata whose metatable has `__gc`, but only objects marked for finalization: an object is marked when its metatable is set and that metatable already has a `__gc` field. Adding `__gc` to the metatable later marks nothing. Finalizers run in reverse order of marking; an object being finalized is resurrected for its finalizer, with everything it reaches; it may be marked again from inside its finalizer. Weak references interact: an object being finalized is already gone from weak values when its finalizer runs, but still a weak key, so the finalizer can find what a weak-key table holds for it.

## Decision

**Registration** happens in one place, `Heap::set_metatable`, which every way of setting an object's metatable goes through: base `setmetatable`, `debug.setmetatable` on a table or full userdata, and a host's `NativeCall::set_metatable`. The object is registered when the new metatable holds a non-nil `__gc` (any value: it need not be callable), the object is not registered or waiting already, and the runtime is not closing (Lua's `luaC_checkfinalizer`). Setting a metatable on a registered object keeps its place. `debug.setmetatable` on a number or a function sets a type metatable and registers nothing.

**State** (`Heap::finalizers`, snapshot state):
- `registered`: objects in registration order; not roots.
- `pending`: objects found dead, in the order their finalizers run; roots, so an object stays (resurrected) until its finalizer has run.
- a mark on each object, set while it is registered or pending and cleared just before its finalizer is called, so the finalizer may register it again; rebuilt on restore from the two lists.

**A collection** (`gc::collect`), in Lua's `atomic` order:
1. mark from the roots, pending objects included, settling ephemerons;
2. clear weak values;
3. move registered objects left unmarked to `pending`, newest registration first (each collection's batch after any left from an earlier one), and mark them and what they reach;
4. clear weak keys, and the weak values of weak tables first reached in step 3;
5. sweep.

An object is therefore reclaimed at the earliest by the collection after the one that found it dead, if its finalizer did not resurrect or register it again; a host userdata's Rust value is dropped only then, never before its `__gc`.

**Lookup at call time.** The queue holds objects, not functions: `__gc` is read from the object's current metatable when its turn comes (ADR 0048). A `__gc` removed by an earlier finalizer is not called; one replaced is the new one.

## Alternatives

- **Registering on any metatable that later gets `__gc`.** Not Lua: the registration-time rule is observable, and Lua's own test suite checks it.
- **Ordering by object id or arena index.** Registration order is what Lua defines; ids would reorder objects whose metatable was set late.

## Consequences

Snapshot schema 16 (the two lists, the flags, finalizer frames); GC policy 9. Restore refuses a list naming a missing object or one that is not a table or full userdata, an object in either list twice, a running finalizer without its frame, registered objects while closing.
