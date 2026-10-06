# Security

Moonseed is an experiment. It is not a sandbox you should point at untrusted programs yet.

## Host capability authority

Moonseed grants no filesystem/process/environment authority by default.
Standard streams, clocks and local civil conversion are also absent by default.
Installing `Libraries::STANDARD` installs functions separately from attaching
capabilities. Hosts choose these independent grants and their backend limits:

| Capability | Authority | Checkpoint boundary |
|---|---|---|
| Filesystem | `io.open/read/write/seek/flush/close`, temp files/names, remove/rename; loadfile/dofile, searchpath and module reads use the same backend. Read-only policy denies mutations | Reads journaled; acquisitions/mutations exactly-once. VFS live keys rebind; native live files refuse; closed files need no backend |
| Stdio | stdin and stdout/stderr IO and flush; stdin loading | Journaled reads/writes; streams rebind by kind; host stream state is external |
| Clock | Wall time for `os.time()`/implicit date and CPU time for `os.clock()` | Journaled observations; no clock means unavailable, not a wall-time substitute |
| CivilTime | Local offset, inverse conversion and zone abbreviation | Supplied observations journaled; explicit UTC and absent-capability UTC fallback are pure |
| Environment | `os.getenv` reads supplied values | Journaled observations; absent capability returns nil |
| Process | `os.execute` and `io.popen`, pipe reads/writes/flush/close | Availability/reads journaled; execution and mutations exactly-once; live pipes refuse checkpoints |
| Output / warnings | `print` and `warn` byte sinks, independent of Stdio | Journaled writes; sinks are reattached on restore |
| Entropy | Seeds for unseeded `math.randomseed` | Journaled external seeds; deterministic configured stream when absent |
| ModuleResolver | Host-supplied source, binary or registered native module loaders | External resolution records bytes; Pure policy requires immutable deterministic replies |
| HostEnv / registrations | Resources, native callbacks, userdata rebinds and host hooks chosen by the host | Not serialized; callback authority and effect policy are host responsibilities |

The native filesystem rejects parent/absolute paths and checks components for
symlinks. Per-component validation and subsequent OS operations have a TOCTOU
gap: concurrent replacement can escape the root, even with symlinks disabled.
It requires a trusted, stable namespace: hard links and mount points already
inside the root are followed, and opening a FIFO or device there can block
the host thread. Allowing symlinks can expose targets outside the root. It is not a
strong sandbox boundary. Use the VFS or an outer OS sandbox for confinement.
The standalone VFS fixture importer also requires a trusted, stable source tree;
it rejects observed symlinks and special files before seeding the VFS. Process
commands passed to `os.execute` or `io.popen`, when explicitly enabled, run a
shell with the host process's privileges. Guest command strings can execute
arbitrary commands; a filesystem root does not confine them. The official-suite harness uses bwrap for that
outer boundary.

All IO observations and effects retain exact journal requests. The host must
persist that journal for replay; deduplication is not atomic crash durability
between a mutation and journal persistence. Pending file acquisitions obey the
same live-resource policy, including completed acquisition keys not yet consumed
by Lua. Drive `begin_close` through waits to run Lua resource cleanup; dropping a
runtime does not close keys held by a shared backend needed for restoration.

Native temporary creation uses exclusive reservation, OS entropy and Unix mode
0600; named reservations persist until removed. Host resource limits (open-file
count, allowed paths and bytes per operation) are separate from Lua heap/fuel
limits. Builtin read/write requests are at most 64 KiB; a synchronous capability
must bound its own CPU work. Unwinding panics from the filesystem, stdio,
clock, civil-time, environment and process capabilities become structured
errors. Panics from natives, host hooks, output/warning sinks, entropy sources
and module resolvers propagate and leave the runtime unusable: later runs and
snapshots are refused. Aborting panics remain host/target behavior. C-only locale support never
changes the process locale. No dynamic C modules are loaded.

`os.exit` never kills the process. It reports `ExitRequested` to the embedder,
without needing process authority and without being caught by `pcall`.
Closing can pause or wait before that terminal outcome; the host decides how
to finish the script or job.

## What the kernel does bound

- Instruction fuel. A hard limit ends the run as `Terminated(FuelLimitExceeded)`. `pcall` cannot catch it, and no script path clears it. Stepping again stays terminated.
- Object count, on boot and on allocation.
- The logical heap (`Config::max_logical_heap`, default 64 MiB, ADR 0025). Every allocation is checked before it happens. Past the quota, Lua gets a catchable "not enough memory" error; the error object is reserved at boot, so raising it allocates nothing.
- One string made by `..`: bounded by `Config::max_string_bytes` and the logical-heap quota, checked before copying. The string limit defaults to 1 GiB; the default heap quota is smaller.
- Call depth: 1,000 Lua and protected calls, plus room for `xpcall` message handlers and for the `__close` calls of a stack overflow's unwind, 1,080 frames in all. Past it, Lua gets a catchable "stack overflow". Message handlers that keep failing stop after 20 levels. A `__close` that recurses or raises is bounded by the same limits; it cannot grow the Rust stack, since closes run as ordinary calls (ADR 0026). A tail call does not count against the depth (ADR 0029): tail recursion runs in constant stack space and is bounded only by fuel, as a loop is.
- Snapshot size, object counts, string bytes, frames, and registers. Oversized declarations return `SnapshotError::LimitExceeded` before the corresponding allocation.
- Each thread's stack: `Config::max_stack_slots`, 50,000 value slots by default, at most 100,000 (ADR 0028). Calls, extra arguments, open results, and a host completion's values are all checked before the stack grows; past the bound, Lua gets a catchable "stack overflow". A thread's stack is therefore at most 1.6 MB of values.
- Every restored thread's stack, and every stack slot index it names, lies within its snapshot's stack bound: `top`, frame bases and limits, extra arguments, metamethod-call slots, assignment sources and register targets, and open upvalues. A snapshot that claims a larger index, or a bound outside the configurable range, is refused; before Phase 3.13 such an index could make the host abort on a 32 GB allocation. Restore also checks that each frame's registers match its prototype and start above its caller's, and that each Lua frame answers the call its caller is making (ADR 0029).
- `load` (ADR 0031): a reader's source is at most the default 16 MiB source limit, checked before each piece is appended, and it is charged to the logical heap as it grows, so nested readers cannot hold more than the quota. Past either, `load` returns a failure. A mode that refuses a chunk's kind is applied on its first byte: a string is not compiled, and a reader is not called again. Each call of a reader is a step that costs fuel, so a native reader cannot run without end inside one quantum.
- `print` hands the host's output its text in pieces of at most 64 KiB, or one string, however many arguments it gets.
- The table library (ADR 0033) runs at most 32 reads, writes, lengths, or comparisons per unit of fuel, so no table function, however long its list, runs unbounded work in one step. `table.sort` keeps every index within the list's length and its range stack within 40 entries, whatever its order function does. `table.unpack` checks its count against the stack bound before reading, and `table.concat` its text against the configured string limit and the heap quota as it grows.
- A function the VM implements, called by another (`__newindex = table.insert`, `__tostring = tostring`, `pcall(pcall, …)`), is called in its own step (ADR 0033). A chain of them cannot deepen the Rust stack, and each level costs a unit of fuel. Before Phase 3.22, such a chain nested on the Rust stack and could abort a host thread with a small stack, or trap on wasm32.
- The string library (ADR 0034–0038) does a bounded amount of work per unit of fuel: 4,096 bytes of a built result, or 256 units of pattern, format, or pack work, a bulk compare or copy charged by its length. A result is checked against the configured string limit and the heap quota before it is made (`rep`, `sub`, `reverse`, `upper`, `lower`) or as it grows (`gsub`, `format`, `pack`), and the bytes built so far are charged to the logical heap. `string.byte` and `string.unpack` check the stack for their results before making them.
- Patterns (ADR 0037) run on an explicit stack of at most 200 frames, Lua's own bound, never the Rust stack; a pattern that backtracks without end runs until fuel stops it.
- Binary chunks (ADR 0036) are read as untrusted input: every count is checked against the compiler's bounds and against the bytes left before anything is allocated for it, nesting is at most 64 deep, and the result passes the same validator as compiled code before anything is installed. A damaged or foreign chunk is a `load` failure. Restore checks a string function's state against the strings it reads, and a native closure's values and numbers against what its builtin makes.
- `require` (ADRs 0039, 0060) bounds and charges collected searcher messages. Modules come from preload, an explicitly supplied filesystem or host resolver. `package.path` cannot grant authority beyond its backend. Loader source ranges and failed readability probes retain exact journal identity; no native handle survives a range call.
- Debug hooks (ADR 0059): each delivery costs one fuel unit before the callback, with ordinary fuel for Lua hook bodies. A quantum ending before delivery leaves a pending event; no callback runs for free. Hook argument-window stack and logical-heap quota checks happen before any argument writes. Hook storage is charged to its owning thread, and loading a new main chunk retains that charge while resetting activation state.
- Hook restore checks weak owners and references, mask/count combinations, canonical event names, charges, cursor bounds, pending-event activation and instruction/native phase, transfer windows, return continuations, suppression and yield markers. Every retained host-hook symbol must be registered before runtime construction or userdata rebind, even if masked off; missing symbols fail transactionally. Callback code and host state are not serialized.
- Debug information (ADR 0040) in a snapshot or a binary chunk is checked before use: a line for every instruction or none, locals within the code and the function's registers, call names within the code, names of at most 256 bytes.
- `debug.traceback` looks at 256 table entries per unit of fuel while it searches `package.loaded` for names, and its text is charged to the heap and bounded by the configured string limit. The other debug functions do bounded work: a walk of at most 1,080 frames, or one function's lines.
- Coroutines (ADR 0041): a switch is a builtin call charged like any other, so a loop of resumes runs out of fuel like any loop. A chain of resumes stops at 196 coroutines ("C stack overflow"), and none of it nests on the Rust stack. Values moved between threads are checked against the receiving stack's bound, for all of them, before any is moved. Restore checks that the running threads form one acyclic chain of resumers.
- Userdata (ADR 0042–0045): a byte payload is bounded by the configured string limit and heap quota and a userdata has at most 65,534 user values; a userdata is an object under the object limit, and its bytes, user values, and what a host type declares are checked against the quota before anything is made, and a host value's growth is refused past it (`set_userdata_charge`). A host value is reached only through its registered type, by a `TypeId`-checked downcast, for no longer than a native call or a host closure borrows the runtime; a native returns a continuation before Lua executes, so no Rust borrow spans Lua code, and a conflicting borrow does not compile. Lua source cannot read or write a payload. A snapshot refuses a host value without a portable codec or rebind policy rather than dropping it, and one that declares more than it was charged; restore treats codec bytes as untrusted, bounded by the configured limits, refuses a heap past its object limit, and drops everything it decoded when it refuses. A light userdata token the VM made cannot be handed back in by the host, so no runtime receives another's.
- Weak tables and finalizers (ADR 0046 to ADR 0048): ephemeron chains settle in time linear in their entries, with no repeated passes; finalizers run as ordinary calls, one per step, each start costing a unit of fuel and its code ordinary fuel; no scheduled collection starts inside one or between the finalizers of a batch, and after each the next scheduled collection waits for the interrupted code's own allocation, so finalizers that register themselves again cannot take every step; an allocation the quota refuses collects once and fails, never looping on finalizable garbage; an error in a finalizer is a warning, never an escape from `pcall`; closing may pass the object limit by the two objects of its frame closure, never fail for room.
- `math.random` never reads the clock or an address. Its seeds come from the runtime's configuration or from a host source recorded in the journal (ADR 0032).
- `Runtime::snapshot` refuses, with `LimitExceeded`, any state restore would refuse for size, so a checkpoint that cannot be restored fails when it is taken.
- Malformed snapshots fail before a runtime is published. The caller’s existing runtime is not mutated.

## Errors are not a way out

`pcall` and `xpcall` catch Lua errors: `error`, faults, memory errors, stack overflow. They do not catch `VmError`, which means corruption or host misuse, fuel termination, or an exit request. A test corrupts a frame under `pcall` and checks that the host gets `Err(Corrupt)`.

## Known gaps

- A thread's stack counts against the logical heap, 16 logical bytes a slot, since source can make threads (ADR 0041). The quota is checked where calls, results, varargs, and coroutine transfers grow a stack; a register write inside a call's own frame does not grow it. While a memory error's unwind runs `__close` values, a thread may still grow its stack within its stack bound, 800 KB of values by default, as Lua's spare stack lets those closes run.
- Collection is charged to fuel: one unit per four units of collector work (ADR 0050), emergency collections included; the host's `Runtime::collect` is the host's own work. Each step is bounded, a large table or stack traced a piece at a time. Lua does not run during an atomic phase, which re-traces every table written to while the cycle marked (a 300,000-entry table being filled made an atomic phase of 0.9 ms with no quantum); the quantum still splits it between units. A few pieces run whole once begun, each bounded by the object limit: moving the dead registered objects to the finalizer queue, the roots, the values waiting on one ephemeron key, a thread's open upvalues. The quantum stops a slice between units, not inside such a piece, so a slice may pass its quantum by one piece; the fuel limit likewise. Objects whose finalizers register them again are finalized at every cycle, paid in fuel. In generational mode (the default, ADR 0051) a young collection runs whole before Lua goes on, as an atomic phase does, its work growing with the young objects and the old objects written to since the last one; a write that stores only numbers does not make an old object traced again. Old garbage waits for a major collection, but an allocation that would pass the quota or the object limit still runs a full collection first. Near the quota or the object limit, steps grow so the cycle ends before the limit; a heap kept near a limit is collected often. An embedder that needs a CPU bound must also keep a wall-clock limit outside the VM.
- Synchronous host hooks must bound their own work; delivery fuel does not meter Rust callback instructions. A Rust panic propagates and leaves the runtime in callback state, so later execution and snapshots are refused. Discard it after catching a panic.
- A host root asked for during an atomic phase finishes that phase first, charged to the run's fuel. Host values are dropped when the sweep frees their userdata, a bounded number per step.
- An `__index` or `__newindex` chain can traverse up to 2,000 links inside one charged operation. String hashes are cached, but key comparisons and chain traversal still cost work. Fuel is not a wall-clock bound.
- `debug.getinfo(f, 'L')` builds a table of a function's lines, and every debug call walks the thread's frames; fuel charges each as one call. The work is bounded by the function size and 1,080 frames; the default compiler limit is 2^20 instructions per function.
- Compiling is not charged to fuel. Its transient memory is bounded instead: source from `load`, `loadfile`, `dofile` and resolver modules is parsed within a budget of four times the logical-heap headroom, counted at 512 bytes a token, and past it `load` returns "not enough memory". Before Phase 3.38 a dense 15 MB chunk under the default quota built a syntax tree of over 4 GB. The host's own `compile` has no such budget. `load` of a 1 MiB source costs one `Call` of fuel; a loop of such loads is bounded by fuel per call, not per byte compiled. `tostring` and `print` of long strings are likewise one call each, and so are `string.dump` and loading a binary chunk, whose size is bounded by the string limit.
- The journal is the embedder's, and grows by one entry per `print` write and per external native call. A script that prints in a loop grows it until the fuel limit. An embedder that runs long scripts must prune committed effects it no longer needs for replay.
- Result copying and the argument shift of `__call` are not charged per value. Both are bounded, by the register window and by the 200-step `__call` limit.
- A host value's logical size is what its type declares. Moonseed cannot see a Rust value's own allocations: a host that lets untrusted Lua grow one must report the growth with `NativeCall::set_userdata_charge`, or Lua can grow host memory past the quota. A host value must not hold Moonseed values; the collector cannot see them (ADR 0044).
- Rust `Drop` of a host value runs when the collector frees its userdata, after any `__gc` has run. It is not Lua's `__gc`: a host that closes files or sockets in `Drop` does so at a point Lua cannot see or order, and a snapshot taken before it keeps no record of it. Lua-visible cleanup belongs in `__gc`, and at shutdown in `Runtime::begin_close`.
- Logical bytes are not process memory. A table filled with integers uses about five times its logical size on x86_64, so a table at the 64 MiB default can reach about 330 MB.
- Snapshot size is bounded by `Config::max_snapshot_bytes` (128 MiB by default). A host can set a smaller bound that refuses an otherwise runnable state. Encoding and decoding allocate temporary storage beyond the logical heap; see ADR 0052 for the resource model.
- A failed coroutine keeps its stack, as Lua 5.4 requires, until it is closed or collected. A script that makes and fails coroutines holds that memory; the object limit and the thread's frame bound still apply.
- Restore applies the smaller of the saved and host resource limits, with the host snapshot-size bound, and refuses state that cannot fit. The saved fuel limit and consumed counter remain execution state. Authenticate checkpoints and keep any non-rollback execution budget outside the VM.

## Running adversarial programs

Reviews, fuzzers, and any run of generated or hostile input go through `tools/capped.sh`, which caps address space, CPU time, and wall-clock time for the command and everything it starts. The optional Lua 5.4 oracle test runs PUC Lua the same way, under 1 GB and 20 s of CPU. An uncapped review run of a runaway program once exhausted this machine's memory.

## What a snapshot does not protect

The fuel counter and `next_sequence` are part of the checkpoint because replay needs them. Restoring an older snapshot rewinds them. That is not an unbypassable quota. An embedder that must stop a guest from resetting its budget has to keep a separate, non-rollback counter outside the VM. Logical-heap accounting is snapshot state too; restore clamps resource limits to the host policy. Host IO quotas and budgets that must survive rollback belong outside the checkpoint. See [the compatibility policy](docs/COMPATIBILITY_POLICY.md#snapshots).

A checksum detects accidental corruption. It does not authenticate a sender.

## Host effects

`mark` records an outcome in the embedder’s journal under `EffectId { domain, sequence }`. A repeat of the same id returns the stored outcome and does not append again. Moonseed does not own the journal. If the VM is restored from after the call and the journal is not, the effect is missing and the VM will not run it again. If the journal is restored and the VM is still `Prepared`, the host must return the original outcome, not a new one.

Ordinary `from_snapshot` rejects a different effect domain. Continuing in another lineage is not a silent restore.

A sandbox chooses its libraries and capabilities: `Runtime::install_base_only`
can omit `print` or `load`, and `print` writes nothing without an output sink.
`loadfile`, `dofile`, IO and module search have no ambient filesystem access.
Persist exact capability request records with the VM checkpoint: snapshots
preserve VM history; they do not freeze the future external world. Committed
reads replay old results; new operations observe the current backend. Source
loading across ranges needs host-pinned versions for an atomic view.

## The debug library is not for sandboxes

`debug` (ADR 0040) reads and writes every local, upvalue, and metatable of every function and thread, joins upvalues, changes the metatable of every string, number, or function at once, and reaches the registry. A script with it can reach any value any other code holds. It is installed only when the host calls `install_debug`; `install_standard` leaves it out. Under it the VM stays memory-safe and deterministic: what it writes are Lua values, checked where the VM uses them, and a numeric `for` whose state it broke raises a Lua error.

Host debug hook callbacks are trusted host code, like registered natives.
Inspection views borrow only the callback; retained values require owned roots.
Raw operations do not invoke Lua or wait, and callback reentry, including
replacing the main chunk through a custom conversion, is refused. Host yields
are zero-value line/count yields in yieldable coroutines; other yields are
catchable Lua errors. Lua hooks remain non-yieldable. A sandbox should omit
`debug` unless this inspection and mutation authority is intended.

## Handles

A `Root` is a strong root for the runtime that issued it. It does not become a root of a restored runtime. A handle is not a root; after collection it fails the generation check. Slots whose generation would wrap are retired.

## Reporting

There is no security contact yet. Do not open a public issue for a vulnerability you believe is exploitable; this tree is not a released sandbox.
