# ADR 0054 — Constant-operand arithmetic

## Decision

Compile a binary arithmetic or bitwise operator with an integer literal
operand to `ArithK { op, dst, reg, constant, reverse }` (tag 52). The
64-bit immediate holds the exact integer. `reverse` puts the constant first.
Parentheses around literals do not prevent selection; float, unary and computed
expressions keep their ordinary code. For two integer literals, keep the left
load and embed the right operand. No arithmetic is folded at compile time.
`Op` remains 16 bytes on native and wasm.

The integer-first form avoids a prototype constant pool, installation/GC
accounting changes, or a new snapshot continuation. A measured typed-immediate
candidate also fit in 16 bytes, but routing integer-register/float-immediate
operations through the requested cold path increased numeric_loops Ir by
12.34%; native_calls rose 3.43%. Reject it under the 1% regression threshold.
The ten million `0.5` loads in numeric_loops remain. A larger typed pool would
not fix the mixed-type dispatch cost, so it is deferred as well.

The hot arm uses scalar integer operands and the shared `int_op`, then
`hot_numbers` for operations such as remainder, division and power. Mixed types,
numeric strings and metamethods decline to the existing arithmetic semantic helper. A decline
writes neither the destination nor PC. Both forms share `op_arith` and the
VM-owned `MetaEvent::Store` continuation, passing the original operands in
their original order. Existing destination/capture safety rules apply.

## Compatibility, fuel and debug

Bytecode revision is 13, checked exactly by snapshot and binary-chunk headers.
Old and future revisions are rejected before decoding. Snapshot schema stays
22 and binary chunk format stays 2: neither the prototype structure nor pending
state layout changes. GC policy stays 12; its accounting rules are unchanged.

Fuel revision stays 6: each executed instruction costs one unit. Each removed
load removes a suspension/checkpoint boundary and lowers the Lua-instruction
component of fuel. Compilation/register sizes can also change GC work. A
cross-build source budget or suspension location is not promised; pin the
compiler when those matter. Tiny quanta and checkpoint schedules must agree
for the newly compiled code, including yielding/waiting metamethods.

The arithmetic instruction retains the operator's line and metamethod name.
Removing a literal load can remove its line from `activelines`; local PC ranges
and call sites are emitted against the actual new code. Constants never become
fake register operands. The baseline has no arithmetic operand-name error
attribution: `fault` uses reserved generic messages. Those messages are
preserved in both orders; adding Lua's `(local 'x')` text is separate work.

## Evidence

See `results/arithk/RESULTS.md` for fresh before/after full corpus counters,
whole-program callgrind Ir with divisor 100, binary/source identity, exact
assertion changes, gates and residual acceptance limits. Machine wall/cycle
times on 2026-10-02 are diagnostic only.
