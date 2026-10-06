# ADR 0007 — Tables, strings, and the hasher

## Context

Lua’s observable table order must not be hash-bucket order. String equality is by bytes. A predictable hasher is a future HashDoS question, not something a fixed seed solves by itself.

## Decision

`Vec` of entries in insertion order, plus a `HashMap` using an in-tree stable hasher for lookup only. Update-in-place does not move an entry. Deletion compacts and rebuilds the index. String keys are byte vectors. Object keys are `ObjectId`s. Float normalization matches the integer/NaN/`-0.0` rules we intend to keep.

Strings are not interned.

`next`, raw length, and deletion during traversal are specified in ADR 0010. This record's "delete compacts immediately" rule is superseded there: a delete leaves a dead anchor until an absent-key insert.

## Alternatives

`indexmap` as the table. Not needed to test the semantics. Interning every string. Deferred; it is an implementation strategy, not a language rule.

## Revisit

When adversarial key sets show up in a benchmark or a fuzzer. The hasher can be replaced without changing enumeration order. That replacement is the HashDoS decision, and it is still open.
