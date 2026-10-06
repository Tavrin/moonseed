# ADR 0010 — Traversal anchors and the smallest border

## Context

`next` has to keep working after the current key is deleted, including across a checkpoint. The anchor that makes that possible must not keep a collectable key alive. Length has to be deterministic. Lua defines a border and, for a table that is not a sequence, allows any border. A PUC result for a holed table is not a portable expected number.

Schema 2 stored only live entries and dropped a key on deletion. That cannot restore `next(t, deleted_key)`.

## Decision

Enumeration order is Moonseed insertion order. That is stronger than Lua, which leaves order unspecified. `next(t, nil)` is the first live slot. Later calls follow that vector and skip dead slots. An unknown key, including NaN, is `LuaFault::NextKey`. A non-table is `LuaFault::Type`.

A deleted live slot becomes a dead anchor in place. The anchor stores the key identity and is not a GC edge. Object keys store an `ObjectId`. String keys store bytes, so a new string with the same content still finds the slot after the original string object is collected. Integer, boolean, and canonical float keys are copied values. The anchor is not a live entry: `get` returns nil, and raw length ignores it.

Updating a live value does not move the slot. Deleting a different existing key turns that slot into an anchor and does not move the others. Inserting a key that is not currently live, including reinserting a deleted key, drops every dead anchor, keeps the remaining live order, and appends the new key. That is the reclamation rule. (Since Phase 3.30 the insert drops the anchors only once they outnumber half the live entries, and otherwise appends past them; a key may then have anchors before its live slot. See ADR 0052.) Pure deletion does not compact. A cycle of insert-then-delete therefore keeps at most one anchor. A table that only shrinks keeps one anchor per deleted key until the next absent-key insert. Those anchors are what `next` needs, so they are not dropped early.

`next` takes the table and the previous key. There is no iterator object. The opcode writes the successor key and its value, or nil and nil at the end. The stdlib shape where a finished `next` returns one nil is not wrapped yet. `pairs` is not implemented.

Links from a slot to the next live slot are rebuilt from the vector on restore. They are not snapshot fields. After later deletions a link may point at a slot that has since died, and `next` follows until it reaches a live slot. One `Next` can therefore walk a dead run. At 4095 dead anchors that walk was about 6.5 µs on the measurement machine, about 1.6 ns per skipped slot. Fuel is still one. The walk is not chunked. It is not constant work.

Raw length is the smallest border: the length of the contiguous positive-integer prefix starting at 1, or 0 when key 1 is absent. That is always a Lua border, and it is the only border of a sequence. Empty is 0. Zero, negatives, non-integral floats, strings, and object keys do not extend it. Integral `1.0` is the integer key 1, as before. The search probes at most `B + 1` keys, where `B` is that prefix and `B` cannot exceed the number of live positive integer keys. It does not allocate a slot per missing integer and it does not compute `maxinteger + 1`. It is not Lua's O(log n) array binary search. A 1024-key sequence took about 11 µs.

Dead anchors and live entries are schema 3. Each slot carries its ordinal, which must be `0..count-1`. A live nil value, a non-canonical float, a duplicate key, a dead object id of 0 or past `next_object_id`, and a dead object id of a string, proto, or upvalue are rejected. A dead object id with no object is allowed: the object may already have been collected. Schema 1 and schema 2 are not restored. `next_live` indexes, hash buckets, and any future field cache stay out of the snapshot.

## Alternatives

Keep PUC's "any border" by copying its array/hash split. That would make the result depend on representation and still not be a single portable number.

Drop the key on delete and reject `next` from it. That fails the deletion-during-traversal requirement and the checkpoint test.

Treat the dead anchor as a weak handle to the key object. A different string object with the same bytes would miss, and a collected object would leave a dangling handle.

Chunk the dead-run walk now. The measured walk is linear and visible, and a resumable scanner would add checkpoint state for a cost that is still small next to the rest of the interpreter. It stays a recorded limit.

## Consequences

A future field cache must miss when a live value is replaced, when the slot becomes dead, and when an absent-key insert compacts and moves live slots. A cached length must miss when a positive integer key appears or disappears. Restore rebuilds slots, so a cache of slot indexes is dropped with the runtime. Metamethods are still absent; `__index`, `__newindex`, and `__len` are further invalidations and are not implemented.

Insertion during traversal is not a language guarantee. The reclamation rule defines it for determinism: anchors are cleared and the new key is last.
