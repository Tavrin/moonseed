# ADR 0055 — Compare-and-branch

## Decision

Emit `CompareBranch { kind, a, b, sense, offset }` (tag 53) when an `if`,
`elseif`, `while` or `repeat` condition contains a comparison, including
short-circuit `and` / `or`, parentheses and `not`. Recursive branch emission
preserves short-circuit evaluation. The comparison's boolean selects its branch directly;
value-producing comparisons, including operands of value-producing `and`/`or`,
keep `Compare`. Conditions without a branch-only comparison retain existing
value emission. Select from the expression tree, not an unchecked peephole.
Operands are evaluated left to right before swapping them for `>` / `>=`.
Close operations and lexical scopes remain on their original edges.

`Op` stays 16 bytes. Defer an integer-immediate form: an exact i64 immediate,
i32 offset, kind, register, order and sense need additional packing to keep
that size. This first form removes the compare/branch pair without a new pool.

The single hot arm uses shared `compare::int_op` / `float_op` for int/int and
float/float. Mixed numbers, strings, other primitive values and metamethods
use the same cold `ops::compare` as `Compare`, including exact mixed ordering,
NaN and `~=` negation. A decline changes neither registers nor PC.

The existing VM-owned `MetaEvent::Truth` continuation holds the handler and
its result. Its reserved destination `COUNT_OPEN` (255, never a register)
means finish the `CompareBranch` at the saved PC. Restore checks this marker,
the comparison's negation and the callable argument window against that op.
Kind, sense and offset live in the immutable instruction, not new pending
fields. Immediate and resumed results share `finish_truth`; neither path
re-evaluates operands or calls the handler again. A native wait, callable
handler, Lua yield or checkpoint carries the same continuation.

## Compatibility, fuel and debug

Bytecode revision **13 -> 14**, checked exactly by snapshot and chunk headers.
Snapshot schema **22**, chunk format **2**, GC policy **12**, and fuel revision
**6** remain. No encoded prototype or continuation layout changes. Old and
future bytecode revisions are refused before decoding prototypes.

One executed fused op costs one fuel unit and has one instruction pause
boundary. A pause can no longer occur between compare and branch. Suspended
handlers still expose their own safe points, and their result commit completes
the already charged instruction without an extra branch charge. Per-source
totals and hard-fuel outcomes can change; pin the compiler for cross-build
budgets. Smaller register/code footprints can also change GC work. Newly
compiled code must agree across all quanta and checkpoints.

The op keeps the condition's source line and comparison metamethod name.
Debug locals and call PCs are emitted/remapped against actual code. Removing
the boolean temporary and a branch can remove temporary observations or a
source line from `activelines`; named locals and operand lifetimes remain
valid. Fault messages remain the existing comparison helper's messages.

## Evidence

`results/cmpbr/RESULTS.md` records full corpus VM counts, divisor-100
whole-program callgrind Ir, source/binary identity, exact fuel assertion
changes, Gate Q, quantum-1 restores, oracle, native/wasm and Moss gates.
Wall and cycle measurements on 2026-10-02 are diagnostic only.
