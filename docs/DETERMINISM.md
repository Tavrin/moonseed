# Determinism

## Levels

| Level | Meaning | Evidence |
|---|---|---|
| D0 | Same build, same program, same host registry, same effect domain, same limits: same semantic result | Tested |
| D1 | That contract across native and `wasm32-unknown-unknown` | Integer kernel and the additional fixtures described below; not arbitrary host programs |
| D2 | Across Moonseed versions | Not claimed |

See [the compatibility policy](COMPATIBILITY_POLICY.md) for release and format
boundaries. Historical revision numbers below describe individual changes; the
current snapshot requires schema 25, bytecode 14, tables 4, fuel 7 and GC 12.

## What the semantic observation includes

Final integers, `ObjectId` aliasing (the cycle, the shared upvalue), yielder status, journal outcomes, fuel consumed, hook traces and countdown state. Fuel slicing and checkpoint schedules are required to match this.

## Compiler output and source budgets

Destination-aware arithmetic emission removes result-copy instructions from new
compilations. That change kept fuel revision **6**: each executed VM instruction still
costs one unit. Per-source fuel totals and suspension locations can change;
embedders requiring identical source budgets across builds must pin the compiler
identity. Existing snapshots and binary chunks retain their stored instructions.
Destination-only changes left bytecode revision 12, snapshot schema 22, and GC
policy revision 12 unchanged. Constant-operand arithmetic subsequently raises
bytecode revision to 13 (ADR 0054). Compare-and-branch raises it to 14 (ADR 0055).

The new bytecode must give identical results and fuel under whole-run, small
quantum, and checkpoint-every-step schedules. Removing copies also changes
prototype/register logical sizes and can change GC work and finalizer timing;
VM instruction savings alone are not an exact prediction of total fuel savings.
The compiler preserves close operations and never redirects arithmetic into a
captured local. Arithmetic callbacks and faults see the uncommitted destination.

`ArithK` removes a literal-load instruction and its safe point. Fuel revision 6
still charges one unit per executed instruction; source totals, active lines,
and source-level suspension positions can change. Snapshot schema 22 and binary
chunk format 2 retain their layouts, but both reject bytecode revision mismatches.
Integer immediates retain all 64 bits and operand order. Float literals retain
their ordinary loads. The shared
arithmetic continuation keeps yielding/waiting metamethods checkpointable.

`CompareBranch` combines a branch-only comparison and its branch into one
fuel unit and one instruction pause boundary (ADR 0055). Yielding/waiting
handlers retain VM-owned Truth state until their returned truthiness, with
`~=` negation, selects the saved branch. No checkpoint exists between the
comparison result and that branch. Newly compiled code agrees across schedules;
cross-build source budgets and safe-point positions are not promised.

## What it does not include

Pause count, wall time, arena indexes, generations, the owner token, hash-bucket order.

## Policies that are part of the contract

- Table enumeration follows live insertion order. Lookup hashing must not leak into that order. The stable hasher exists so lookup itself does not follow `std`’s per-process or per-target randomization. It is not a HashDoS defense.
- Integer arithmetic wraps. The certified program uses integers only.
- Float keys, when used, map `-0.0` to `0`, reject NaN, and map exact integral floats to integers.
- Floats (ADR 0032). Every transcendental function (`math.sin` … `math.log`, `atan`, `exp`), `sqrt`, `floor`, `ceil`, `fmod`, and `^` use the portable `libm` crate on every target, never the host's C library. Their results have the same bits native and on wasm32, checked over 2,000 inputs per function (`math_bits_fingerprint`) with no decimal formatting in between. IEEE `+ - * /` are exact everywhere. The sign and payload of a NaN are not part of the contract: they may differ between targets. Results may differ from a host C library's by 1 ulp; that is a difference from PUC Lua on that host, not across Moonseed targets.
- `math.random` is snapshot state and draws from Lua 5.4's xoshiro256**. Seeds come from `math.randomseed(x, y)`, from `Config::entropy` through a deterministic stream, or from host entropy recorded as journaled effects; never from the clock (ADR 0032).
- `table.sort` picks its pivots deterministically: the same list and order function give the same comparisons on every run and target (ADR 0033).
- Strings (ADR 0034–0038). Nothing in the string library reads the locale, the clock, or an address:
  - Character classes and case follow the C locale for every byte.
  - `string.format` and `tostring` produce their digits in pure Rust, the same bytes on every target; every NaN prints as `nan`.
  - `%p` prints object ids, builtin symbols, and, for strings of at most 40 bytes, a token made from their bytes.
  - `string.pack` uses one ABI on every target, with explicit byte order, and floats go through their bits.
  - `string.dump` writes the same bytes for the same function on every target, and nothing that depends on the run before it.
  - The string fingerprint (`source_string_fingerprint`) compares fixtures, dumped chunks, packed floats, and formatted numbers native against wasm32.
- Debug introspection (ADR 0040). Everything `debug` reports comes from the program: debug information made by the compiler, stack levels, and object ids in `tostring`. The traceback's search of `package.loaded` follows insertion order, never a hash layout. `debug.setlocal` can make a numeric `for`'s state a non-number, where Lua's behaviour is undefined; Moonseed raises a Lua error. The debug fingerprint (`source_debug_fingerprint`) compares the debug and package corpora, dumped debug information, and waits inside `require` native against wasm32.
- Coroutines (ADR 0041). A switch between threads is a step of the same run, charged like any call; nothing about it lives on the Rust stack or depends on the host's scheduling. The coroutine fingerprint (`source_coroutine_fingerprint`) compares the coroutine corpus and host waits inside resumes, wraps, nested resumes, and closes native against wasm32.
- Userdata (ADR 0042–0045). A full userdata's identity is its `ObjectId`, a light userdata's its domain and 64 bits: equality, table keys, `tostring`, and `%p` never see an address, an arena index, or a `TypeId`. A host key is a number the host picks; a VM token (`debug.upvalueid`) is the id of the cell it names, and ids are never reused. A snapshot writes byte payloads exactly and portable host values through their type's codec, which must give the same bytes on every target, and refuses host values without one. When a host value's Rust `Drop` runs is not part of the contract and Lua cannot observe it. The userdata fingerprint (`source_userdata_fingerprint`) compares the userdata corpus, host methods, and waits holding userdata native against wasm32, and a waiting runtime holding every kind of userdata gives the same snapshot bytes on both and finishes the same from either.
- Collection is observable (ADR 0046 to ADR 0048): weak tables lose entries and finalizers run. Under the deterministic profile the automatic schedule is part of the contract: which safe point schedules a step, how many units of work each step does and where each phase of a cycle begins and ends, which objects a cycle finds dead, the finalizer queue's order, the fuel the work costs, and the order of warnings and effects are the same under every quantum and checkpoint schedule and on every target, given the same host replies and entropy (ADR 0050). The quantum decides only where the executor pauses inside the collector's work; a step does exactly the units it owes, finishing any piece it began, and an atomic phase begun runs to its end before Lua does. Snapshots taken in the middle of a cycle hold the collector's state as it is; a snapshot never finishes or resets a cycle. The collector's events fold into a hash in its state (`GcState::trace`) that tests compare across schedules. Every finalizer a collection queues runs before the interrupted code takes another step; that timing is Moonseed's, not PUC Lua's, which promises none. A snapshot includes what weak tables hold, so a checkpoint never changes what the next collection decides. In generational mode (ADR 0051) the choice between a young and a major collection, every age, the remembered set, and falling back are snapshot state, decided by the work loop when it runs, never by when a step was scheduled or whether the host polled a waiting run. The quota and `collectgarbage("count")` read the exact logical heap, which a restore counts again from the objects: what a running sweep has freed leaves it only when the sweep ends, so a restored sweep, which never had the dead objects, gives the same count at every step. The GC fingerprint (`source_gc_semantics_fingerprint`) compares the GC corpus, the `collectgarbage` corpus (tiny steps, checkpointed mid-cycle), the generational corpus, the `step` corpus, their warnings, their snapshot bytes, and a finalizer waiting on the host, native against wasm32.
- A pattern search, `gsub`, `format`, or `pack` does a bounded amount of work per step and carries any debt from a bulk operation in its snapshot state, so its fuel does not depend on where steps or checkpoints fall.
- A fuel pause does not change results. A hard fuel limit or a different object cap is a different run, not a slicing of the same one.
- Restoring a snapshot continues the stored fuel and sequence counters. That is replay. It is not a security budget. See `SECURITY.md`.
- Host results are part of the contract only when the journal is restored with the VM. Moonseed does not invent a missing external effect.

Changing the effect domain, the bytecode, or the snapshot schema is outside D0.

### Hook checkpoints (Phase 3.35)

Count, fuel and line events measure different things. Count advances once per
begun Moonseed instruction, including suppressed Lua hook-body instructions;
continuation polls, native work, GC and delivery bookkeeping do not advance it.
Lines are compiler source positions, with an event at entry, a new line or a
backward jump. Fuel charges semantic work: revision **7** adds exactly one unit
per delivered hook, and Lua hook bodies use ordinary fuel. An inherited Lua
wrapper without a callback incurs no delivery charge. Count precedes line at
the same instruction boundary. Absolute count positions need not match PUC's
bytecode, and neither count nor line events use the quantum as a clock.

If the quantum ends before delivery, the event remains pending and is delivered
before the interrupted operation continues; it is not recounted. Snapshot
schema **23** encodes Lua targets, inherited wrappers and registered host
symbols, masks, base/remaining counts, suppression, line/restore cursors,
instruction stages, pending events, transfer ranges and Hook/HookNative
boundaries. Mid-Lua-hook execution, host waits inside that hook, errors and legal
host-hook yields are checkpointable. A legal host line/count yield transfers
zero values; resume ignores its arguments and executes once without redelivery.
Replacing or clearing the suspended hook preserves that continuation; closing
cancels its yield marker.

Restore requires registered host symbols before construction or userdata rebind.
Callbacks and their Rust state are not serialized. A deterministic host hook
must return the same action and edits for the same observation. Keep callbacks
pure, or record and replay their external effects and callback state in a host
journal coordinated with the VM checkpoint; hook registration does not journal
effects automatically. Wall clocks and unrecorded profiler buffers are outside
the replay contract.

The 30-program matrix checks traces, count state and fuel under quanta 1/2/3/7,
both collectors, hot/cold modes and checkpoint-every-step schedules (720 runs).
Fuel comparisons use each collector's own reference because their collection
work differs. Host preemption has a separate 64-run matrix, and native/Wasm
hook snapshot exchange is tested. These establish the tested schedules, not D2.

### UTF-8 checkpoints (Phase 3.36)

`utf8` uses the existing builtin fuel model: the first work step runs under
the call's charge, and each continuation costs one unit. `char` handles at
most **256 arguments** per step; `len` and `codepoint` decode at most **256
sequences**. Offset navigation and manual iterator continuation-byte skips
run at most **4,096 navigation steps**. These bounds do not depend on the
host clock, target or quantum. Fuel revision **7** is unchanged.

Snapshot schema **24** stores `Work::Utf8` cursors, counts, strict/lax mode,
navigation state and char's charged partial buffer; codepoint results stay in
accounted scratch slots until the call completes. Operations are checkpointable
mid-construction, mid-scan, before invalid input and in either navigation
direction. Internal work steps emit no hooks; semantic calls and iterator calls
retain their ordinary call/return events and transfer metadata.

The UTF-8 matrix checks results, errors, fuel and exact heap charges with
quanta **1/2/3/7**, both collectors and restore at every scheduled boundary.
Unhooked intermediate snapshots also match byte-for-byte. Hooked runs compare
observable traces and charges because restored debug-info tables can have a
different slot layout. The existing string fingerprint includes UTF-8 and
matches native/Wasm results.

### Host capability replay (Phase 3.37)

Every operation has one of three classes:

| Class | Operations |
|---|---|
| Pure / deterministic | UTC civil conversion and formatting, `os.difftime`, C-locale queries, mode/path processing, VM cursor changes and buffered-byte consumption |
| Journaled read | Readability probes, positional/source/stdin/pipe reads, file size, wall/CPU clock, local civil conversion and zone names, environment lookup, shell availability |
| Exactly-once mutation | Open, write/append, flush, close, remove/rename, temporary file/name reservation, stdout/stderr writes, execute/popen and pipe writes/close |

Even deterministic VFS/mock capability calls use the journal boundary. Records
retain `EffectId { domain, sequence }`, class, exact request bytes and typed
success/failure. Replay checks operation, resource/path, offset/length and write
bytes as applicable; a mismatch is a VM error. Persist full records separately
and reconstruct the journal with `seed_record`. Exactly-once is deduplication
with those records; atomic crash durability between external mutation and
journal persistence remains the host's responsibility.

The replay rule is: **snapshots preserve VM history; they do not freeze the future external world**. Committed reads replay old bytes and committed writes
are not repeated, even after backend mutation. New operations see current
backend state. Source loading journals each range separately; a host needing
one atomic version across ranges must pin immutable/versioned source itself.

Filesystem IO is positional. Logical cursors, numeral lookahead, read-ahead
bytes, consumed positions and pending output live in file userdata and schema 25.
Buffered writes have no external effect until a semantic flush. Waiting and
completed-before-consumption flushes retain the exact buffer and request;
the preserved journal prevents a repeated write. Restoring
mid-buffer preserves already observed bytes; later refills are new external
reads. Backend files/open-key tables are not in the snapshot. VFS files require
their preserved Rebind backend; live native files and pipes refuse snapshots.
Standard streams rebind by kind and need matching host stream state.

A Pending operation preserves request, semantic sequence, work phase and host
token. Waiting costs no fuel. Complete with `complete_capability`, then resume
with the same journal; the helper commits that result without calling the
backend again. Both waiting and completed-before-consumption states are
checkpointable subject to resource policy. Host dispatch and pending-token
state must survive restore; a VM wait alone does not deduplicate host dispatch.

The integrated matrix uses quanta 1/2/3/7, both collectors and Ready/Pending
hosts, checking each against its matching uninterrupted baseline. Replay
adversaries mutate the VFS and check old results, no repeated mutations and
new reads. The portable host fingerprint matches native/Wasm after mid-operation
restores and shutdown. These witnesses cover the supplied fixture, not arbitrary
host implementations. Ready/Pending allocation histories may change GC work;
their fuel is compared to the corresponding host baseline. Fuel revision 7
and the other semantic revisions remain unchanged by Phase 3.37.
