# ADR 0056 — Interpreter core and execution caches

Status: **Accepted** (Phase 3.32, 2026-10-04).

## Context

At `48f94bd`, the revision-2 corpus costs 5.300x PUC Lua 5.4.9 by geometric
mean whole-process instructions. Repeated register checks, cold re-entry,
frame/result setup, key hashing and library dispatch account for much of the
gap. Warmed Lua calls already allocate nothing; avoiding malloc alone cannot
remove their frame cost. The campaign profiled these paths before changing
them. Its design source is the phase's hot-core RFC (kept with the phase's working
notes, outside the repository); the implementation differs where
restored stacks and measured code shape required it.

## Decision

Keep the safe Rust interpreter and canonical heap frames. Build the hot core
over temporary borrows of canonical state, with disposable lookup caches and
bounded reusable storage. Accept **Outcome B**: substantial instruction savings
with identified remaining hotspots. At `d9154eb`, the corpus geomean is 2.701x
PUC, a reported 1.962x improvement; final2 changes the PUC control binary.
Same-source Moonseed-only absolute Ir improves 1.960x. The minimum 2x gate is missed by about
2%. This is completion of the measured campaign, not a passed minimum gate.
See [PERFORMANCE.md](../PERFORMANCE.md#phase-332-interpreter-performance-campaign)
for all workloads, machine conditions, rejected designs and source revisions.

### Epoch and frame window

`Runtime::run_hot` has two borrow levels. An epoch performs the schedule,
trap, collector and pending-finalizer checks and resolves the active ready
thread. Its single instruction allowance is
`min(quantum, fuel_limit.saturating_sub(fuel_consumed))`; each fetched instruction
decrements it once before execution. Every exit publishes exact used fuel,
remaining quantum and the current PC before cold execution, faults, callbacks
or a supported pause. The other existing charges for GC and builtin work remain.

A frame window resolves the closure and immutable prototype, then splits the
stack into the registers and the slots below the frame base. Validated
bytecode bounds every operand by `max_reg`; the hot handlers index that register
slice directly. The running closure's constants, cached hashes, hints and
upvalues are borrowed with it. **A short restored stack declines to `poll`**,
which retains nil-on-miss reads and lazy write growth. Unlike the initial RFC
proposal, entering the core does not resize such a stack and change its next
checkpoint image.

Handlers update the local PC and return the one-byte `Step` (`Continue`, `Cold`,
`Frame`, or `Fault`). The old `Option<Flow>` and second jump-offset dispatch are
removed. Same-type integer/float arithmetic and comparisons, numeric negation
and integer `ForLoop` use shared inline primitives. Compact mixed-number and
float-loop helpers remain out of line. Coercions, unusual values, faults and
metamethod fallback retain the shared semantic handlers.

Closed upvalues and open upvalues on the active thread below the register
window run inside the core. Writes retain the collector barrier. Another
thread's open upvalue, an unavailable slot or invalid handle declines. Arena
generations remain checked at access boundaries; resolving the frame once
does not turn a stale table, string or upvalue handle into a valid one.

### Calls, results and continuations

Eligible Lua calls push an ordinary `Frame` and returns pop it within the
epoch; each transition re-derives the frame window. Fixed and open argument
and result counts use the same stack-window semantics: interior nils, padding,
discarded results, scratch clearing, truncation and `top` retain their meanings.
Eligible tail calls replace the frame in constant stack space. `Frame::pending`
is boxed (native Frame 120 → 72 bytes), an in-memory layout change only.

Fast setup preflights closure kind, non-vararg prototype, depth, stack reserve,
quota and source/destination windows before mutation. Natives decline early;
vararg callees, closes, open upvalues requiring closure, special boundaries
and reserve/quota/fault cases use the existing path. A new stack high-water
charge or due collector work returns `FrameStep::Resync`, publishing the epoch
and returning to `poll` so the existing scheduling boundary is preserved.
Ordinary returns retain the slow path's exact stack contents and length.

Direct Lua metamethod frames retain the canonical `MetaCall` on the caller.
Return helpers complete plain continuations only when the quantum allows it;
an exhausted quantum leaves that continuation available for checkpointing.
The direct `__call` path re-reads the current metatable handler on every call;
callable chains, native/vararg handlers and failed preconditions decline before
mutation. Library-to-Lua callbacks, including sort comparators and gsub
functions, use fast frame setup and ordinary boundary result windows. A builtin
continuation still charges one fuel unit; message-handler completion remains
uncharged. Errors, yield restrictions, debug call names and pending machines
remain canonical. Ordinary coroutine resume/yield delivery reuses transfer
buffers and copies validated result windows; unusual boundaries and growth
use the general path.

### Execution caches and storage rules

These structures are derived from canonical state or hold reusable empty
storage. They are not snapshot authority and may be dropped or rebuilt.

| Structure | Validation, invalidation and lifetime |
|---|---|
| Cached string hashes | Immutable string objects lazily hash through `OnceCell`; prototype byte constants derive hashes on install/restore. Equality still compares bytes, so hash equality grants no identity. Hashes are not serialized or logically charged; debug checks recompute them. Strings remain non-interned. |
| Stored string-key owners | A shared allocation holds visible bytes followed by the cached hash. TableKey stays 24 bytes; keys up to 31 bytes need one allocation, longer keys use a temporary buffer. Live/dead anchors own bytes independently of the Lua string object. Snapshots write visible bytes only and restore derives the hash; logical key charges exclude the suffix and capacity. |
| Per-instruction field slot hints | A prototype with fields has two `Cell<u32>` candidates/op, for the name and raw `__index` field. Every hit checks bounds, live string-key slot, hash, length and all bytes, then reads the current value. Failed hints use normal lookup and refresh. Deletion, compaction, GC, restore, table switches and metatable changes need no eager invalidation because every use validates. Install/restore starts empty. No cached table identities, values or negative results; 8 physical bytes/op, no snapshot/logical charge. |
| Dense integer accelerator | `Vec<u32>` maps positive key minus one to slot plus one (zero absent). Ordered slots hold values and traversal anchors; the hash index is authoritative when indexed. Sparse keys use it directly. Growth admits keys within twice the retained slot count and rounds geometrically, bounded by four u32 words/slot; deletes retain anchor mapping and compaction/restore rebuild. Refill probes newly covered keys or scans slots according to their relative cost, bounding total growth work linearly. Derived capacity is not serialized or logically charged. |
| Five-slot small tables | At most five retained slots, including anchors, use a bounded reverse scan and allocate neither lookup index. The sixth append builds both; restore/compaction rebuild and release them on shrink. Last-slot lookup, insertion/traversal order, dead anchors, raw border and logical charges are unchanged. |
| Immediate scalar builtins and iterators | Checked per call, without persistent value caches. Eligible abs/floor/ceil/min/max, type and one-byte string.byte finish directly with the original fuel charge. ipairs and next/pairs validate state/key/result windows; ipairs declines when `__index` is needed and pairs retains `__pairs` resolution. Coercion, errors, larger windows and suspension-capable cases use ordinary machines. |
| Rooted userdata location hints | Owner/root checks precede index/generation/ObjectId/liveness validation. A mismatch falls back to the randomized by-id map; an old slot or matching generation alone is insufficient. Restore requires reacquiring roots. Mutable guards retain a validated location only while their exclusive runtime borrow prevents collection, reuse or restore. Hints are not serialized. |
| Embedding storage | Four owned-value Vec buffers are cleared, dropping roots before pooling; argument, result, stack and frame capacity is reused. One spare native-boundary box is reused. Live continuations retain values in traced, serialized stack slots. Pools start empty on construction/restore and are bounded in count, not an allocation-free promise for arbitrary nesting or capacity growth. |
| Weak main-call trampoline | Its handles are validated, never rooted by the cache; collection can make the next call cold. Restore derives an existing live trampoline from the heap or a later call creates it. Cache fields are not serialized. |
| Spare MetaCall box | One empty runtime-owned box, with every field overwritten before use. Live continuation metadata remains on frames; the spare retains no Lua roots or close state. Construction/restore starts empty; completion recycles eligible plain calls. No snapshot field or logical charge changes. |

Table mutation still uses the existing reference-aware collector barriers and
quota checks. Fast slot writes update only a validated live value; nil deletion,
insertion, dynamic misses and `__newindex` retain their handlers. Sort validates
each operation against the current table/metatable rather than trusting a
persistent plain-table flag. Its quicksort and bounded batch transitions stay
unchanged. String-library step buffers and formatter output storage are reused
without changing matcher transitions, fuel or live machine state.

### Compiler and compatibility

Destination-aware arithmetic emission removes temporary result copies and
adjacent scalar stores only after whole-function capture/close analysis.
Captured or to-be-closed locals, multiple assignments and unsafe alternate
entries retain their stores. Operand order and suspended metamethod commit
remain intact; branches, local ranges, lines and call PCs follow emitted code.

[ADR 0054](0054-constant-operand-arithmetic.md) adds exact integer-immediate
`ArithK` (bytecode 12 → 13), preserving operand order and cold arithmetic
continuations. [ADR 0055](0055-compare-and-branch.md) adds `CompareBranch`
(13 → 14) for comparisons used only as branches; value-producing comparisons
retain their boolean result. `Op` remains 16 bytes.

Code constants at `d9154eb` are bytecode **14**, snapshot schema **22**, tables
**4**, fuel **6**, GC policy **12**, binary chunk **2**. Snapshot and chunk
loaders refuse other bytecode revisions. One fuel unit per emitted instruction
is unchanged, but removing instructions removes pause boundaries and changes
per-source budgets and possibly GC work. Compiler identity must be pinned
when a host requires stable source-level fuel budgets; a fused operation does
not reproduce a checkpoint between its former component instructions.

### Gate Q and reviews

Debug/test-only `HotCoreMode::{Full, NoFastCalls, Off}` selects the optimized
core, disables fast frame paths, or uses cold dispatch. It is absent from
ordinary release builds. Gate Q compares outcomes, snapshot bytes, fuel,
observations, output, journal and GC state/logs at quanta 1 and 7, with existing
single-step restore, depth/quota, close, coroutine, numeric and debug fixtures.
Caches also have uncached/differential checks. These establish the exercised
boundaries, not universal program equivalence.

The campaign records two review rounds at `08d4448`. Their findings and
dispositions are:

- **Restored-top panic, fixed `4c675c3`:** restore accepts `top > stack.len()`;
  the slow path reads absent return slots as nil. Fast open returns checked
  destinations but not all source slots and could panic. Both `fast_return`
  and `fast_builtin_return` now decline on a source range outside the stack.
  A restored-top regression exercises the difference.
- **Off-mode proof gap, fixed `4c675c3`:** immediate scalar builtins previously
  ran in Off too. They are now disabled there so Gate Q compares their original
  path; the review's 23 edge-case programs were already equivalent.
- **Scalar-store barrier asymmetry, recorded:** the hot path can skip a
  generational barrier for a non-reference value where slow string-key stores
  take it. Both are collector-safe; this existed before the campaign. After
  collection, their bookkeeping snapshots can differ, limiting byte-for-byte
  Gate Q coverage. This is not claimed fixed.
- **Accelerator refill complexity, fixed `128d529`:** repeatedly scanning a
  string-heavy table on geometric integer growth caused O(n log n) work.
  262,144 string slots plus 19 integer inserts examined 4,980,907 slots.
  Hybrid range probes/slot scans bound total growth work linearly; the
  adversarial extra Ir falls 85.3M → 70.6M, with both branches checked against
  the index, including sparse keys and dead anchors.

R2's 96,000 randomized mutations and 334 checkpoint comparisons found no
other representation defect in those probes. R1 exercised 89 programs and
adversarial paths but did not run wasm itself. Lane native/wasm fingerprints
and Moss replay gates supply their own evidence; the final oracle is 13/13
at `d9154eb`. The official suite remains at four passes with unchanged
blockers, not full Lua compatibility. The last string-key/sort and setup
changes followed the reviews; their lane witnesses passed, but the review
rounds were not rerun against the final revision.

## Consequences

Fuel, canonical frames, portable snapshots, exact logical charges and host
effect replay remain requirements. Caches reduce repeated work without making
their contents necessary to resume. Their physical memory is additional to
logical quota accounting, bounded by code/slot counts or pool counts; storage
capacity can still grow with valid workloads. Dropping pooled roots before
reuse and validating every predicted location are implementation obligations.

The RFC's original stack-frame guards were waived after whole-corpus evidence;
the final CAL measurement reserves 520 bytes versus the stage-4 320-byte goal.
Call+Return0 remains 654 Ir against the 400-Ir aspiration and PUC about 133–155.
Rejected split epochs, caller-window caches, float immediates and upvalue-field
ops show that smaller frames or fewer bytecodes alone do not prove a win.
The proposed continuation must re-profile residual frame/dispatch, comparator,
metamethod, iterator, allocation and coroutine costs and repeat the semantic
and whole-corpus gates. Loaded paired wall measurements are diagnostic and
cannot turn the missed instruction gate into a pass.
