# ADR 0057 — Compact activation records and cold continuation state

Status: **Accepted** (Phase 3.33, 2026-10-04).

## Context

The Phase 3.32 frame was 72 bytes on native x86-64. Five optional pointers took
40 bytes, yet an ordinary fixed Lua call initialized all five to empty. The
warmed fixed 0/0 Call/Return probe cost 687 host instructions per call, including
one supporting `Move` (654 without it), against PUC Lua 5.4.9's 155 (133 without
its `Move`). The fixed 1/1 probe cost 708 against PUC's 187. Both probes exclude
startup through matched slopes. The audit also found repeated checked
closure/prototype lookups, frame-window reconstruction and stack regrowth.

## Decision

Keep one canonical `Frame`, 40 bytes on native x86-64. Its closure handle, PC,
base, limit, vararg length, result count and tail flag are inline. `FrameCold`
holds pending calls, host waits, assignment targets, metamethod state and
protected/builtin boundaries. A frame has a cold box exactly while at least one
such field is present. Empty boxes are cleared and recycled; the ordinary fixed
call allocates none. Library boundary and task boxes are also reset and recycled
after completion. A warmed fixed call and the measured callback machinery make
zero host allocation requests per call.

`Stack` keeps a logical `len` separate from its high-water `values.len()`.
Readers, the collector and snapshot encoder see only `values[..len]`; growing
the logical extent initializes newly visible slots to nil. `Frames` likewise
keeps `depth` separate from retained slots. An ordinary push overwrites the next
slot in place, then advances depth; pop lowers depth and releases cold state.
Slots beyond depth hold no cold payload and cannot retain GC roots. Physical
capacity stays warm. The fast fixed-frame builder is shared by ordinary calls,
eligible metamethod calls and library-to-Lua callbacks. Function and arguments
already in the call window become the callee's registers; missing parameters
are nil-filled and excess arguments cleared. Varargs, open windows, `__call`
insertion, closes and exceptional boundaries keep checked general paths.

The hot interpreter switches frames inside its instruction loop. A frame
switch still re-derives validated closure, prototype, stack and upvalue views;
the retained physical storage does not turn handles into unchecked pointers.
The thread's open-upvalue summary records whether anything is open above a
frame base; returns can skip a list walk when none is. Opening, closing and
restore maintain or rebuild that summary.

## Snapshots and validation

Native layout is not the portable image. The encoder reads active frames and
the logical stack through accessors, emitting the same semantic frame fields:
closure object ID, PC, base/limit, result contract, vararg count, tail state,
pending call, wait, targets, metamethod and boundary. Restore builds cold
storage only for nonempty exceptional state and starts with physical stack
length equal to the image's logical length and frame storage equal to depth.
The validator checks active frame extent, stack bound and charge, result and
caller-PC correspondence, vararg layout, tail flags and continuation states;
short restored stacks remain legal where the general path grows them lazily.
Inactive retained slots are neither encoded nor traced. A 3,587-image witness
was byte-identical to the phase baseline. Snapshot schema 22, bytecode 14,
tables 4, fuel 6, GC policy 12 and binary chunk format 2 stay unchanged.

## Two design passes and measured result

The first pass compacted the frame and simplified helpers but reached only
575 instructions for fixed 0/0, a 16.3% saving. It missed its 490-instruction
Gate N. `Vec::push` stayed out of line (22 instructions), 0/0 regrew a logical
stack slot on every call (52 in `grow_window`), and window re-derivation stayed
in `run_hot`. Metamethod probes also rose about 3% because prechecks were
duplicated. The second pass retained stack/frame storage and switched frames
inside the loop. It reached 445 instructions for fixed 0/0 (35.2% below 687)
and 486 for fixed 1/1 (31.3% below 708); metamethod probes recovered. The
second Gate N target of 400 was missed. Its stage-8 attribution places about
74 instructions in the three-op loop, 62 in two switches, 56 in checked callee
and caller resolution, and 37 in frame-arm dispatch/helper calls, with the
remaining builder, return and result-window work documented in
[PERFORMANCE.md](../PERFORMANCE.md#phase-333-activation-record-and-call-abi).

## Consequences

The 21-workload instruction-ratio geomean moves from 2.703x to 2.575x PUC
at the final revision. This meets the minimum useful outcome at its call-cost
floor, but misses the 2.2x corpus and 250-instruction call targets. The owner’s
rule stops further call optimization above 400 until a narrow continuation is
chosen. The logical stack adds checks on some slow paths, and the retained
physical storage can use more memory than the current logical extent; its
visibility, charging and GC roots remain governed by the logical view. The
phase review found no confirmed correctness finding. Raw source/binary
profiles, stage decisions and snapshot witnesses are kept with the phase notes
outside the repository; the measured values used for this decision are above
and in PERFORMANCE.md.
