# Architecture

Moonseed is a stackless Lua 5.4 runtime in safe Rust. Its byte-source frontend
compiles the language to private bytecode; execution can pause, wait, checkpoint
and restore without keeping a continuation on the Rust stack.

The public embedding API is described in [EMBEDDING.md](EMBEDDING.md) and [ADR 0053](adr/0053-embedding-api.md). Runtime internals stay private; native-to-Lua calls use snapshot-backed continuation frames.

An active Lua call has one 40-byte native `Frame` with inline closure, PC,
window, result and vararg metadata. Pending calls, metamethods, assignments,
host waits and builtin/protected boundaries use a `FrameCold` box only while
present. The value `Stack` exposes its logical slice while retaining physical
high-water storage; `Frames` indexes live slots by depth and overwrites the
next slot on push. Popped frames release cold state, and retained inactive
slots are neither GC roots nor snapshot content. Eligible fixed Lua calls,
returns and frame switches run inside the hot instruction loop through a
shared frame builder; unusual windows and continuations take the checked
general path. Snapshots encode semantic active frames rather than native
layout. See [ADR 0057](adr/0057-compact-activation-records.md).

## Source frontend

```text
source bytes
    → lexer
    → AST
    → compiler
    → detached prototype
    → validation
    → Runtime::boot
```

The lexer reads bytes. Spans are byte ranges. Compiling the same bytes twice produces the same prototype: locals, upvalues, constants, children, and registers follow lexical order. A compile error does not allocate a runtime.

Guest source compilation (`load`, file loading and resolver modules) has an
AST allocation budget of four times the current logical-heap headroom. The
parser charges vector capacity growth using the stored type's `size_of`, boxed
expression nodes, and retained string/name/diagnostic bytes. Each charge has a
2× safety margin for lowering, tree traversal and allocation copies, plus a
16-byte allowance per box or vector growth. Charges are not refunded when a
node is flattened or discarded. Numeric constructor fields share a vector;
separators cost no retained storage. Exhaustion returns `not enough memory`.
This bounds compilation proportionally to the heap quota; it is not an exact
process RSS limit. The host `compile` and `compile_with_limits` APIs retain
their existing compiler limits without this guest allocation budget.

Instruction spans sit on the compiled chunk. They are not copied into the prototype the VM runs, and they are not written into a snapshot. What the VM keeps instead is each prototype's debug information (ADR 0040, `debuginfo.rs`): a line per instruction, locals with the instructions they are live over, upvalue names, and the name each call's function has at the call site, which the compiler records where Lua would find it by symbolic execution. The chunk's name is one string object, shared by its prototypes. Debug information is snapshot state and binary chunk content, and counts against the heap.

ADR 0011 has the subset, the limits, and the `3..4` numeral rule.

Each block is a compiler scope: the local count and register frontier on entry. A local becomes captured when a nested function resolves it as `Capture::Local`. Every exit from a block, whether fallthrough or jump, goes through one helper: if a local in the block was captured, emit `CloseUpvalues { from: frontier on entry }`, then drop the block's locals and reset the frontier so later locals reuse the registers. `Jump` never closes. ADR 0012.

Some syntax is lowering only, onto instructions that already exist. `and` and `or` put the left operand in a new register and jump over the right operand when the left decides; `not` is a jump and two `LoadBool`s. Truth has one definition, `Value::truthy`, which the conditional jump, comparison metamethod results, and `<close>` share. `v:name(args)` evaluates `v` into a register, copies it to the first argument's, and looks `name` up with `GetField`, the ordinary indexing that follows `__index`. A `function` statement is parsed as the assignment it stands for. `local function` defines its local before compiling the body, then stores the closure in the local's register. None of these adds runtime state, so pauses, waits, and checkpoints inside them are those of the instructions they compile to.

Labels and `goto` exist only in the compiler (ADR 0030). Each function keeps its visible labels, its forward gotos waiting for a label, and every local it has declared with its final captured and `<close>` flags. A label resolves the waiting gotos of its block, and refuses one that would enter a local's scope; a trailing label counts its block's locals out. Each goto is two instruction slots, filled when the function ends: a `Jump`, or the scope-exit close its left locals need and a `Jump`. Bytecode has no labels, and a goto adds no runtime state.

The compiler refuses any register at or past the 250-register limit before it emits an instruction naming it, so every overflow is a `Limit` error.

`<const>` and `<close>` locals are read-only through one test, `readonly`, which every assignment to a name goes through; a capture inherits it, so a nested function at any depth cannot assign one either. The rule is the compiler's alone. Bytecode and snapshots do not carry it, and the validator checks only structural safety, so hand-built bytecode may write such a register.

## Comparisons

`compare.rs` is the only definition of raw equality and of order on two numbers or two strings. `Op::Compare` carries `Eq`, `Ne`, `Lt`, or `Le`; `>` and `>=` are `Lt` and `Le` with swapped operands. Mixed integer/float comparisons bound the float by its floor or ceiling instead of rounding the integer. Strings order by bytes, with no locale. `ops::compare` adds `__eq`, `__lt`, and `__le`. Two-number comparisons run in the hot tier through the same functions. ADR 0014, ADR 0023.

## Operators

`arith.rs` defines every arithmetic and bitwise operator on numbers (a string takes part through the string metatable's metamethods, ADR 0034), `concat.rs` defines `..` and the text of a number, and `ops.rs` picks a metamethod when the primitive rule does not apply: the first operand's, else the second's. `+` is `Add`; the other binary operators are `Arith` with an operator; unary `~` is `BNot`; `..` is `Concat`. Two-number `Add` and `Arith` run in the hot tier through `hot_arith`, which calls the same `arith::numbers` as the common handler `op_arith`. ADR 0023.

## Loops

Each compiled function keeps a stack of loop contexts: the scope outside the body, and the pending `break` jumps. A `while` body is a scope per iteration, left before the backedge. A `repeat` body's scope stays open through `until` and is left on both edges after the test. `break` leaves every scope back to the loop's outer scope with `emit_scope_exit`, then jumps to the exit. The VM sees only jumps and closes.

## Metatables and metamethod calls

A table and a full userdata hold an optional metatable handle, traced by the collector and written to snapshots by `ObjectId`. Every other basic type, light userdata included, has at most one metatable, in `Heap::type_metatables`; the string library sets the string one, `debug.setmetatable` any (ADR 0034, ADR 0042). `Heap::metatable_of` finds a value's metatable (a table's in line, anything else out of line), and `index::metamethod` reads an event raw from it; every metamethod lookup goes through that pair. `index.rs` resolves `Index` / `SetIndex` / `GetField` / `SetField`, an assignment store, and `Len` to either a value or a function to call. Operators resolve in `ops.rs` the same way. A call is started by `call_meta`: the handler and arguments go to scratch slots above the frame, and the frame records a `MetaCall`: how the instruction finishes (store the first result, store its truth, or drop it), the slot, the argument count, and the phase. The handler runs through the ordinary call path, including `__call` resolution. Its first result is committed by an uncharged step that finishes the instruction. `MetaCall` is snapshot state.

A `Call` on a value that is not a function resolves `__call` inside the instruction: the arguments move up one slot, the value becomes the first argument, and `__call`'s value takes its place, up to 200 times. The call's result window is untouched. A native reads its argument count from `top`, which `call` sets. The hot tier still handles only table hits; everything else goes to `exec`, where the metamethod paths live. ADR 0018, ADR 0019.

## Errors and protected calls

A Lua error is a class (`LuaFault`) and an error object, which is any Lua value. `runtime/diag.rs` is the cold authority for runtime wording, source positions, argument errors and error-level prefixes. It uses `crate::chunkname::chunk_id`, shared with compile diagnostics and debug introspection. Runtime faults reconstruct operand names from the failing instruction, active locals, upvalues, constants and earlier register writers. `Op::writes` and `Op::jump_offset` exhaustively describe writes and explicit branches; the findsetreg-style scan rejects a writer made ambiguous by a forward join or backward loop edge. New opcodes must declare both properties. No diagnostic sidecar or new snapshot/chunk field is needed; stripped chunks degrade according to their remaining debug information and constants. See [ADR 0058](adr/0058-diagnostic-authority-and-static-provenance.md).

The lexer and parser produce structured `CompileError` kinds, source spans and near tokens. One bounded renderer supplies the host display and `load` result. Library and host-adapter argument failures use the cold argument-error path, which resolves a native's name from its Lua call site, adjusts method `self`, and adds the caller position. A native tail call retains the Lua frame until its compiled `Return`, so that position and the caller's debug level remain available ([ADR 0029](adr/0029-proper-tail-calls.md)). A `VmError` is corruption or host misuse and is never caught. An allocation that fails a memory limit becomes a Lua memory error at the step boundary; memory diagnostics use a reserved string without allocation, and other formatting falls back to reserved class text on a quota failure.

Raising sets the thread's `unwind` to `Raised` and touches no frame. `poll` then advances it one uncharged step at a time:
1. It finds the nearest boundary frame, calling an `xpcall` message handler on top of the failing frames if there is one.
2. It pops frames one per step, closing each frame's open upvalues and dropping its pending call, metamethod call, and assignment. A frame with `<close>` values first closes them, below.
3. It finishes the protected call with `false, error`, or, with no protected call, fails the thread.

`pcall` and `xpcall` are base natives that push a boundary frame (`Protect`) and call the function above it through the ordinary call path. The function's return to that frame is completed by a `poll` step, which writes `true` and the results where `pcall`'s results go. A message handler gets its own boundary frame (`Handler`) above the failing frames.

Boundary frames, the unwind, and a failed thread's error are snapshot state, so a checkpoint can be taken inside a protected call, inside a message handler, or between two pops of an unwind. ADR 0024. The logical-heap quota is ADR 0025.

A base function that calls Lua pushes a third kind of boundary frame, `Builtin`, and calls above it the same way. These are `tostring` and `print` calling `__tostring`, `pairs` calling `__pairs`, `ipairs`'s iterator reaching `__index`, and `load` calling its reader.
- The frame keeps the base function's arguments where they were passed, and a small task with no Lua values: which argument `print` is converting, the index `ipairs` is reading, the source `load` has read.
- When the call returns, a `poll` step goes on from its result. The frame then pops, and its results go to the call site as a native's do.
- The task decides whether a yield may cross the frame (only `pairs`), and whether an error stops there (only `load`, which returns it).
- A base function that needs no Lua call pushes nothing.

The `table` functions and `math.min` / `math.max` use the same frame with a library task (ADR 0033). Each is a state machine over semantic operations: read, write, length, compare, call the order function. The operations go through the instructions' own code, so they follow every metamethod. Values the machine keeps sit in scratch slots above its arguments. A step runs at most 32 operations, and each further step costs a unit of fuel, so a long loop pauses, collects, and checkpoints between steps. A frame is pushed only for a Lua call or a second step.

`math` uses the portable `libm` crate for every transcendental function and for `^`, so floats have the same bits on every target. Library state lives in `Heap::library`: `math.random`'s generator and its deterministic entropy stream, snapshot state (ADR 0032).

The `string` functions use the same frames and operations with a string task (`strlib::StrWork`), in a loop of their own (`run_str`), so the table functions' loop stays small (ADR 0034):
- A result of known length is built 4,096 bytes a step. The pattern (`strpat.rs`, ADR 0037), format (`strformat.rs`), and pack (`strpack.rs`, ADR 0038) engines are pure state machines over byte offsets that do 256 units of work a step; a bulk compare or copy is charged in one go and leaves a debt for the next steps.
- The bytes built so far are held in the task and charged to the logical heap as they grow.
- An engine's state encodes to integers, and restore decodes it against the actual strings, so a restored search goes on where it stopped.
- A call of Lua (a `gsub` replacement, `%s`'s `__tostring`, the string metatable's fallback to the other operand's metamethod) is an operation like a table function's `__index` call; no yield crosses it.
- Errors carry Lua's wording, raised with `Runtime::fault_text`.

`string.gmatch` returns a native closure (`Value::NativeClosure`, ADR 0035): a heap object naming a builtin, with the values and integers it keeps. A call of one runs its builtin, which finds the closure in the call slot. `string.dump` writes a Moonseed binary chunk (`chunk.rs`, ADR 0036), and `load` reads one back through the same validator as compiled code.

`print` writes through the host's output (`Runtime::set_output`). Each write is an external effect committed through the journal, so a restored run does not write twice. ADR 0031.

A coroutine that an error escapes is marked failed but keeps its stack, as in Lua 5.4; the entry thread and host calls unwind instead.

## To-be-closed variables

A thread lists the stack slots of its active `<close>` values, in declaration order (`MarkClose` adds one). Leaving a scope with `CloseScope`, returning, unwinding through a frame, and closing a coroutine with `CloseThread` all end the same way: the frame's metamethod call becomes a `Close` event saying which values close and what follows (continue, return these results, resume this unwind). Each value's `__close` then runs as an ordinary call above the frame, looked up when it runs, one per step, newest first. Between calls the frame is `Idle`; a call can pause, wait, yield, raise, or be checkpointed, and the frame's state says where to go on. A close that raises starts a new unwind, which reaches the same frame and closes the rest with the new error. `CloseThread` runs a coroutine's closes as an unwind with no target that its own boundaries do not catch, and that cannot yield. ADR 0026.

## Functions

There are three layers. Lua closures come first. Native functions are ordinary values (`Value::Native`) backed by registry symbols. Host and engine capabilities are exposed through native functions and userdata (ADR 0044). A native's identity is its symbol, interned per runtime. The registry slot is a cache that a snapshot does not contain. `Call` dispatches on the callee: a closure takes the Lua path, a native an out-of-line native path, and anything else faults. A native's results go through the same result-window code as a Lua return. The base functions that raise, call Lua, make strings, or act on the collector or the output are registered like natives but handled by the VM (`call_builtin`, `runtime/builtins.rs`). `Runtime::install_base` binds the whole base library; `install_base_only` binds part of it, for a sandbox. `VmLocal` natives run immediately. `External` natives stop in a prepared safe point with an effect id first. Either can wait. The standard library will use the same value and call model rather than compiler special cases. ADR 0017.

## Indexing and `_ENV`

`GetTable` / `SetTable` are raw. Source uses `Index`, `GetField`, `SetIndex`, and `SetField`, defined in `index.rs`, and parallel assignment stores through the same set. They resolve only tables now. `__index` and `__newindex` will attach where they find no live entry or no table. That will be a resumable slow path in `exec`, since a metamethod can call Lua. The hot tier handles only a live-key read and a live-key non-nil update, and declines everything else, so it never needs to know about metamethods. String keys are probed with a borrowed `KeyView`, without allocating.

A compiled chunk has one capture, `_ENV`, bound at load to the runtime's globals table. That table is a GC root and a snapshot field, so the environment's identity survives restore as an `ObjectId`. A free name compiles to `GetField` / `SetField` on whichever `_ENV` is lexically visible, local or upvalue. A constructor is `NewTable` and one store per field in source order; a trailing call field is `SetList`. ADR 0016.

## Numeric `for`

A loop owns four registers: the hidden index, the hidden iterations-left count (integer loop) or limit (float loop), the hidden step, and the visible control variable. The step's subtype is the mode. `ForPrep` fills them from the three evaluated expressions or skips the loop; `ForLoop` advances them and jumps back. Both are in `fornum.rs`. The control variable is rewritten from the index each iteration, so the body cannot steer the loop. It is the body scope's first local, and the scope exit before `ForLoop` closes it when captured. All four are ordinary values in the snapshot. ADR 0015.

## Generic `for`

A generic `for` keeps four hidden locals at `base..base + 3`: the iterator, the state, the control value, and the closing value, which `MarkClose` registers once as a `<close>` value. The loop variables are the body scope's first locals, from `base + 4`. Each iteration copies the iterator, state, and control to `base + 4` with three `Move`s and calls the copy with an ordinary `Call` wanting one result per variable; the results land on the loop variables. `GenericForLoop` then ends the loop on a nil first result, or saves it as the control and jumps back to the body. The exit and `break` are one `CloseScope` from `base`. The iterator call is ordinary call state, so the loop adds no continuation and no snapshot field. ADR 0027.

## Varargs and the stack bound

A call to a vararg function leaves its extra arguments where the caller passed them, just above the function's slot, and starts the frame's registers above them: the argument window is rotated so the fixed parameters come first in the registers. The frame records only how many extras it has; they are `base - vararg_len .. base`, and results return to the slot below them. Every call the frame makes is at or above its base, so none can reach them. `Vararg` copies them with the result-window rules a call uses. A thread's stack is bounded by `Config::max_stack_slots`; frames use seven-eighths and error handling the rest, like the frame reserve. The bound is snapshot state and restore enforces it, so the runtime and the snapshot accept the same stacks. ADR 0028.

## Tail calls

`return f(args)`, alone and outside every `<close>` scope, compiles to `TailCall` followed by the `Return` of its open window. For a Lua callee, the running frame closes its open upvalues, moves the callee and arguments down to its own call slot (overlapping moves are safe), and is replaced by the callee's frame, built by the same code as an ordinary call's, at the same depth and with the same wanted result count. For a native callee, the tail-calling Lua frame stays live while the native runs or waits. Its open upvalues close and dead slots clear first; the compiled `Return` then delivers the native's open results. This retains Lua's caller position, argument name and debug level. Restore also accepts older snapshots in which that frame was erased. ADR 0029's amendment supersedes its original native-erasure decision.

## Upvalues

A captured local is a cell with an `ObjectId`. While its scope is live the cell is `Open { thread, slot }`: reads and writes go to that stack slot, and the thread keeps an open list of `(slot, cell)`. Two closures of one local find the same cell through that list. `CloseUpvalues { from }` and `Return` turn every open cell at or above the threshold into `Closed(value)`. Identity does not change, so both closures still share it. After that the slot can be reused or cleared, and the closed value is traced through the cell, not the stack. Open and closed state are both snapshot state; restore does not close anything.

## State that must survive a pause

Anything that must survive an executor pause or a checkpoint is stored in the heap: values, frames, program counters, upvalues, pending host calls, the open-result `top`, recorded assignment destinations, protected-call and message-handler boundaries, hook configuration and delivery continuations, and an error being unwound. The interpreter pushes a `Frame` for a Lua call. It does not recurse per call.

## Dispatch

Three tiers, one meaning per opcode. `run_hot` borrows an epoch over the ready active thread and a frame window over its validated register slice, prototype and upvalues. One fuel allowance counts down; `hot_op` writes the local PC and returns a one-byte `Step`. Numeric operations, branches, eligible upvalues and table hits stay in the core. Eligible Lua call/return/tail transitions rebuild only the frame window; open results, Lua metamethods and library callbacks retain ordinary canonical frames and continuations. Due collector work or new stack high-water charges resynchronize through `poll`. A short restored stack uses the slow path's nil reads and lazy growth. See [ADR 0056](adr/0056-interpreter-core-and-execution-caches.md).

Cold declines publish PC, fuel and quantum before `exec` dispatches the already charged instruction. Common cold handlers and `exec_rare` retain coercions, misses, special frames and faults (ADR 0022). `poll` handles pending host calls and stores, unwind, boundaries, pauses and non-ready threads. Every supported safe point has complete canonical heap state; quantum 1 uses the same semantics. Checked immediate builtins and iterators avoid their library machine when eligible; other cases retain it. Debug/test `HotCoreMode` compares optimized and cold paths and is absent from ordinary release builds. A new opcode enters a faster tier only when measured.

`run_hot` checks the cached `hook_trap` beside the ordinary trap once per epoch.
It is true when the active thread has a nonzero hook mask, pending event,
hook-yield marker, or mandatory after-return continuation. Hook changes,
thread switches, continuation changes and restore recompute it at cold points. Hooked threads, including c/r-only hooks,
execute through `poll` and the existing cold executor. Hooks off add no
per-instruction work; there is no second interpreter.

A call writes its results into the caller's register window. `u8::MAX` means every value below `top`, including interior nils. Any smaller count, including zero, is exact: pad with nil or drop the excess. Assignment destinations are captured before the stores. The stores run right to left, one fuel charge for the commit, and the cursor between them is a safe point.

## Canonical state and execution caches

Execution caches are not part of the continuation. Cached string hashes, validated per-instruction field slots, dense integer indexes and userdata location hints reduce repeated lookup; restore derives them or starts them empty. Reusable embedding buffers, weak validated call trampolines and one spare native/MetaCall box retain storage without retaining live continuation authority. At every supported pause or checkpoint, canonical heap state is enough to continue. Dropping a cache must not change results, identity, charged fuel or host effects. [ADR 0056](adr/0056-interpreter-core-and-execution-caches.md) records validation, rebuild and lifetime rules.

## Identity

| Thing | Survives a snapshot? |
|---|---|
| `ObjectId` | Yes |
| `Handle { index, generation }` | No |
| Arena generation | No |
| Runtime owner token | No |

A `Root` is a strong GC root bound to one runtime. Restore mints a new owner token and fresh slots, then remaps `ObjectId`s. Old roots fail. A handle is not a root; freeing a slot bumps its generation, and a generation that would wrap retires the slot.

## Suspension

`StepOutcome` keeps these apart:

- `Paused(FuelExhausted)`, including a quantum of zero
- `LuaYielded`
- `Waiting(WaitKey)`
- `LuaError`: an error that no protected call caught
- `Terminated`, including `FuelLimitExceeded`
- `ExitRequested { status, close }`: uncatchable exit, returned after requested closing

`CallHost` charges one fuel unit, stores `Pending::Prepared`, and leaves the program counter on that instruction. `Prepared` and `Waiting` are executor safe points even though the instruction has not finished. Invoking the host, or completing the wait, is the rest of the same instruction and is not charged again.

## Coroutines

A coroutine is a thread object with its own stack and frames. The library (ADR 0041) adds no thread state: `coroutine.create` makes a thread with no frames and its body in slot 0; `resume` links the child to the running thread (`resumed_by`) and makes it the active thread, leaving the resumer's frame on its call; a yield, a return from the child's last frame, an uncaught error, or the end of a close hands control back. What the resumer receives is decided by the call it is making (`coroutine.resume`, a `wrap` function, `coroutine.close`, or the hand-bytecode `Resume` and `CloseThread`), read from its call site. A coroutine suspended in `coroutine.yield` stays on that call, and the next resume's arguments are that call's results. Nothing about a switch lives on the Rust stack, so every state in a chain of resumes is a checkpoint like any other.

## Host effects

Host functions are names in a registry. The snapshot stores the name. Warnings go to a host sink, each one an effect committed through the journal like a `print` write (ADR 0049).

### Host capabilities (Phase 3.37)

`hostcaps` defines six independent optional traits: `Filesystem`, `Stdio`,
`Clock`, `CivilTime`, `Environment` and `Process`. `HostCapabilities`, the
builder and restore host carry shared objects implementing them. Public traits
use byte paths, resource IDs, structured `HostIoError`s and typed
`Completion::Ready`/`Pending` results; they expose no heap, frame or journal
internals. `Libraries::IO` and `OS` are `u16` selection flags in `STANDARD`;
installing functions does not install authority. `hostcaps/native.rs`, behind
`native-host`, is the sole ambient OS boundary. The VFS and testing mocks work
without it. See [ADR 0060](adr/0060-host-capabilities.md).

`runtime/capability.rs` is the single boundary for builtin external operations.
It allocates an `EffectId` from domain/sequence and journals the operation class,
exact encoded request and success or failure. Requests include resource/path,
offset, length and write bytes as applicable. Replay verifies request and class
before returning the stored outcome; mismatch is a VM error and never invokes
the backend. A Pending request roots its bytes and host token on a waiting
frame. `complete_capability` validates and saves a typed result; the builtin
resumes the same phase, commits it through the helper and advances once.
Waiting consumes no fuel. Journal records and backend objects remain host-owned.

`iolib.rs` defines one internal full-userdata file type, with a shared `FILE*`
metatable and native lines closures. State records backend/kind, resource key,
mode, closed flag, logical cursor, EOF, numeral lookahead and buffering hint.
`runtime/io.rs` uses positional backend IO: the VM owns the cursor. The file
also owns a canonical read-ahead buffer and consumed-prefix position; line and
numeral reads refill at 16 KiB, all/count reads at most 64 KiB. Retained buffer
bytes are charged to the userdata in both collector modes. Successful
seek/close and actual writes discard read-ahead; filesystem flush discards it
and returns numeral lookahead to the logical position. Sequential stdio flush
retains already consumed backend bytes. Buffer bytes, position and exact charge
are checkpoint state, not a derived cache.

Open allocates a closed userdata before acquiring a resource, then marks it
open. Every explicit close, `__close`, `__gc`, iterator exhaustion and shutdown
uses one journaled close authority. Registry roots retain default input/output.
`io.lines(filename, ...)` returns iterator, nil, nil and the closing file;
generic-for break and exhaustion cannot double close. Iterator read errors raise.
`setvbuf` retains validated hints; writes pass through immediately.

File policy is per value, independent of host-registered userdata policy:

| Backend / state | Snapshot policy |
|---|---|
| Live VFS file | `HandlePolicy::Rebind`; restore verifies the preserved open key |
| Standard stream | Rebind by kind through the supplied `Stdio` |
| Live native file or pipe | `Refuse`; `SnapshotError::NonPortableResource` |
| Closed file | Encodes closed state without requiring a backend |

Pending acquisitions and completed acquisition keys not yet consumed by Lua
obey the same policy. Restore rejects duplicate live filesystem keys, invalid
buffer positions/charges and inconsistent closed/work states before publishing
a runtime. No descriptor is serialized and no handle is closed to make a
snapshot succeed. Preserve the VFS resource table separately; a VM snapshot
does not capture its external contents.

`runtime/hostload.rs` acquires source via journaled probes and handle-free
`read_file_range` calls of at most 64 KiB, with charged source accumulation and
bounded path substitution. `loadfile` shares `load`'s compiler, binary decoder,
mode and environment authority. `Task::DoFile` calls the resulting chunk with
a yieldable all-results continuation. Filesystem loading retains no live native
file between calls. The package searcher order is preload, optional filesystem
Lua, then optional host resolver. Paths configure search, not authority.

`civil.rs` implements pure Gregorian UTC conversion, normalization and C-locale
formatting. `runtime/os.rs` separates those computations from journaled wall/CPU
clock, local civil, environment and process operations. Local conversion falls
back to UTC without `CivilTime`; offset, inverse conversion and zone-name
observations use the same helper when supplied. Locale never changes globally.

`os.exit` uses `StepOutcome::ExitRequested { status, close }` and the matching
`CallOutcome`, not process termination. The request bypasses protected calls.
With closing enabled, the main thread's pending close variables run before
registered finalizers; suspended coroutine locals remain pending. The exit
status and Scopes/Finalizers/Terminal phases are canonical snapshot state.
Closing may pause or wait; only afterward does the host receive the terminal
outcome. Later run/load/call operations are refused.

## Userdata and host objects

A full userdata is an object in its own arena (`Heap::userdata`): an `ObjectId`, an optional metatable, a fixed slice of user values, a payload, and the logical bytes the payload counts (ADR 0042). A light userdata is a value, a domain and 64 bits, never an object (ADR 0043). `Value` stays 16 bytes. The payload is bytes Lua cannot read, or a Rust value of a host type registered in `HostRegistry` by symbol and `TypeId` (ADR 0044). The Rust value is owned by its object; every access goes through `userdata.rs`, a `TypeId`-checked downcast, and lends it for as long as a `NativeCall` or a `Runtime::with_userdata*` closure is borrowed. A native function cannot call Lua, so no borrow spans Lua code, a collection, or a restore. The collector traces a userdata's metatable and user values and nothing inside a host value; freeing it drops the value (Rust `Drop`, not `__gc`). A snapshot writes a byte payload, or a portable type's symbol and codec bytes, and refuses a host value whose type has no codec (ADR 0045). `EffectId` is `(domain, sequence)`. The VM keeps `next_sequence` and at most one pending call per frame. It does not keep a list of past effects. The embedder’s journal maps an id to the outcome that was committed. A repeat returns that outcome.

`from_snapshot` takes the domain the host expects. A mismatch is `EffectDomainMismatch` and builds no runtime.

## Tables and strings

Entries live in a `Vec` in insertion order. Tables with at most five retained slots (dead anchors included) use a bounded scan; larger tables build a stable-hash lookup index and a bounded dense positive-integer slot accelerator. Values and traversal links remain in slots. Restore and compaction rebuild the indexes; geometric growth uses range probes or slot scans to keep total refill work linear. String keys compare by bytes, with cached hashes as a rejection filter. Field hints validate a live slot and the complete key on every access. Table, closure, thread, and full userdata keys compare by `ObjectId`, light userdata keys by domain and bits. Integer `1` and float `1.0` are one key. `-0.0` is integer `0`. NaN and nil are not keys. See [ADR 0056](adr/0056-interpreter-core-and-execution-caches.md).

`next` walks that vector. A deleted key stays in place as a dead anchor so the walk can continue, including after restore. The anchor stores an `ObjectId` or string bytes and is not a GC root. Insertion compacts anchors once they outnumber half the live entries or their key bytes exceed live key bytes plus 4 KiB; updates retain their key object. Raw length is the smallest Lua border, not the `#` metamethod. ADR 0010 and ADR 0052 record the order, reclamation and complexity.

Strings are immutable bytes and are not interned. String objects lazily cache their hashes; prototype constants derive hashes on install/restore. Loading a constant or probing with it does not allocate (ADR 0020). Storing a new string key creates an independent shared owner holding visible bytes and the hash suffix; short keys need one allocation, and TableKey stays 24 bytes. Snapshots write only visible bytes and rebuild derived hashes/indexes/hints; logical charges and traversal order do not depend on their physical storage (ADR 0056).

### UTF-8 (Phase 3.36)

`utf8.rs` holds the shared extended encoder used by the lexer's `\u{...}` and
`utf8.char`, the bounds-checked strict/lax byte decoder, and relative-position
handling. `utf8lib.rs` defines builtin identities and `Utf8Work`; `runtime/utf8.rs`
installs the six-field module and runs it through the existing library engine.
`Work::Utf8` holds Char, Scan, Offset or Iterate state, retaining cursor/count
fields and char's charged output buffer. Arguments and partial codepoint
results use the engine's rooted, accounted stack slots. Stable strict/lax
iterator values take their state and control as arguments. Completion and hooks
use the existing native return path; internal steps add no hook events.

## Registry and modules

The Lua registry is an ordinary table on the heap (`Heap::registry`), a root, with the main thread, the globals, `_LOADED`, and `_PRELOAD` (ADR 0039). It is what Lua code sees; the VM's own state is never a registry field. Every library installer registers its table in `_LOADED` through one function, so install order does not matter. `require` and the preload searcher are machines on the library engine (ADR 0033), whose reads, writes, and calls are steps.

## Debug introspection

`runtime/debug.rs` maps Lua's stack levels onto frames: Lua frames are Lua functions, `pcall` and builtin frames are C functions, `xpcall` handler frames are skipped, and level 0 is the running builtin, or on another thread the builtin its top frame is calling. A frame's current instruction is its `pc`, or `pc - 1` under a Lua frame its own `Call` pushed. Names come from the caller's recorded call names or its pending metamethod. A frame made by a tail call is marked (`Frame::tail`). `debug.traceback` is a machine whose search through `package.loaded` is stepped and checkpointed. Hook inspection uses the same semantic activations and exposes transfer windows during delivery.

### Debug hooks (Phase 3.35)

`Heap::hooks` is a lazy side table of boxed `HookState`s keyed by thread
`ObjectId`, with weak generation-checked owners. Never-installed heaps allocate
no hook storage; ThreadObj, Frame and FrameCold retain their sizes. GC traces a
hook's Lua function, event names and native activation values only through its
live thread, then reaps dead owners. No global map roots threads.

State includes the target (None, Lua function, inherited Lua wrapper, or host
registry slot), mask, base/remaining count, suppression, line and restore
cursors, instruction stage, pending event, after-return continuation, transfer
owner/window and hook-yield marker. New threads copy mask/base count with fresh
transient state; inherited Lua wrappers have no function, while host targets
inherit their symbol. `HostRegistry::register_hook` maps stable symbols to
synchronous callbacks. Registry slots are caches; snapshots store symbols.

`runtime/hooks.rs` has one cold delivery path. Count precedes line before an
instruction begins; call/tail events follow activation entry/replacement and
return events precede removal. `HookNative` boundaries retain one semantic
native activation across its implementation steps. Transfer information lives
in HookState, only for the observed activation during delivery, not in every
FrameCold. Pending events preserve their frame, transfer and continuation when
the quantum ends before the one-unit delivery charge.

Lua delivery pushes a hidden, non-yieldable `Boundary::Hook` and invokes the
callback with event/line arguments. Suppression covers descendants, waits and
error handlers, and is restored on return or recovery. Finalizers suppress
delivery; ordinary `__close` calls follow the existing call/return machinery.
Host callbacks use `HookContext` without internal handles or Lua reentry.
A legal line/count Yield records a hook-yield marker and instruction stage;
resume consumes these before ordinary mask/count checks and executes once.
Removal/replacement retains this marker until resume; close cancels it. See
[ADR 0059](adr/0059-portable-debug-hooks.md) and
[embedding hooks](EMBEDDING.md#host-debug-hooks).

## Collection

Incremental mark and sweep (ADR 0050), with Lua's semantics for weak tables, ephemerons, and finalizers (ADR 0046, ADR 0047). `gc::Collector` is snapshot state: the phase (pause, begin, propagate, atomic, sweep), the gray stack, the object being traced and its position, the weak and ephemeron lists, the values waiting on their keys, and the sweep's count. `gc::work` does a budget of units from it at the runtime's safe points: an object begun, a reference traced, a weak entry cleared, an object swept. Each arena keeps one mark byte per slot in a dense array (two whites that swap after the atomic phase, gray, black, free) and an `again` list. `Arena::get_mut`, the only mutable access to a heap object outside the collector, is the write barrier: while a cycle marks, a black object about to change goes gray onto `again`, to be traced in the atomic phase. The heap's roots are not barriered; the atomic phase grays them all again. New objects are live for the cycle they are made in.

The atomic phase follows Lua's `atomic` order, each step resumable: gray the roots again; trace (the `again` lists, the gray stack, ephemeron tables last, a value waiting on its key released when the key is marked, so chains settle in linear time); read each weak table's `__mode` again and trace again any whose mode changed; clear weak values; move the registered objects found dead to the finalizer queue, newest registration first, and trace them; clear weak keys and the weak values of tables first reached that way; swap the whites. Lua does not run during it, though the executor may pause and checkpoint between its units. The sweep then frees objects left with the old white, a bounded number at a time. Registration happens in `Heap::set_metatable`, the one place an object's metatable is set. Finalizers do not run inside the collector: the queue is canonical state, and `runtime/finalize.rs` starts one per step as a `Boundary::Finalizer` frame at a clean instruction boundary, before the interrupted code goes on (ADR 0048).

Allocations add a fixed logical cost to a debt (`GcState`, snapshot state). A cycle begins when the debt reaches a threshold computed from what the last cycle kept, `pause`, and the room under the object limit and the logical-heap quota (ADR 0021); during a cycle a step every 2^`stepsize` bytes owes `stepmul` units per KiB, and more near the limits. The runtime checks at every safe point whether a step is due, and does collector work in `poll` before anything Lua does next, paid in fuel at `gc::WORK_PER_FUEL` units per unit of fuel. A step does exactly the units it owes, an atomic phase begun runs to its end, so the quantum decides only where the executor pauses. An allocation that would pass the quota or the object limit runs a full collection at once and then raises a memory error (ADR 0025). While a finalizer runs, or a batch of them is queued, no step is scheduled; an allocation that needs room still collects, as Lua's emergency collection does.

A thread counts a fixed cost and 16 logical bytes per stack slot (ADR 0041). A full userdata counts 32 bytes, 16 per user value, and its payload's charge: its byte length, or what its host type declares (ADR 0042, ADR 0044). The roots: globals, the registry, the type metatables, the entry and active threads, host roots, temporary pins, the finalizer queue. Everything a thread holds is traced: stack slots, frames, assignment destinations, results. A dead traversal anchor is not a root. Cycles die when nothing roots them. An open upvalue keeps the thread it points at. Slots above a call's result window that belonged to the callee are cleared so a dead temporary is not a root. The Phase 3.27 stop-the-world collector is kept as `gc_reference.rs`, an oracle for tests and measurements.

Generational mode (ADR 0051, the default) is a form of the same heap. Each arena keeps an age byte per slot beside the mark and a young list. Old objects are black, young ones white. `Arena::get_mut` (and `get_mut_storing`, for a write that says whether it stores a reference) grays an old object about to change, puts it on `again`, and makes it touched. A young collection is a cycle that runs whole before Lua goes on:
- it grays the `OLD1` and `TOUCHED2` objects on the collector's revisit list;
- it runs the atomic steps, so marking stops at old objects;
- it sweeps the young lists, aging survivors as Lua's `sweepgen` does;
- it makes the touched objects it traced `TOUCHED2`.

A major collection leaves generational form with a sweep that whitens without freeing, runs an incremental cycle, and decides at its atomic phase whether its sweep makes the survivors old or falls back on incremental cycles (`Decide`, `GcState::bad`). Young collections come every `minormul` percent of what the last one kept, at least `gc_min_debt`; majors once memory grows `majormul` percent past the last major's. The estimate after a young collection measures again what threads hold beyond their objects, so it does not drift.

## Snapshots

Little-endian, magic `MNSD`, schema version 25, then four revisions: bytecode 14, tables 4, fuel 7, GC policy 12. Schema 4 added the native-function section and native values. Schema 5 added table metatables and a frame's metamethod call. Schema 6 writes a prototype's constants as references to its string objects, and the automatic-collection state. Schema 7 names a metamethod call by how it finishes. Schema 8 adds boundary frames, a thread's unwind and error, the reserved error strings, and the heap quota. Schema 9 adds a thread's to-be-closed list and its coroutine and closing flags, and a frame's `Close` state. Schema 10 keeps a frame's extra-argument count without a position, and adds the stack bound. Schema 11 adds base-function frames and their tasks. Schema 12 adds the standard libraries' state and library tasks. Schema 13 adds each basic type's metatable, native closures, string tasks, and new error classes. Schema 14 adds the registry, package and traceback tasks, each prototype's debug information and chunk name, and a frame's tail-call mark. Schema 15 adds full userdata (a section after the native closures), light userdata values and keys, and refuses a full-userdata type metatable. Schema 16 adds the finalization section (registered and pending objects by id, the running and closing flags), finalizer frames, and `collectgarbage`'s frame waiting for them. Schema 17 adds the collector section: its parameters and schedule, its phase, every mark but the current white, its lists by object id, and the sweep's count; dead objects waiting for the sweep are left out, and a snapshot never finishes a cycle. The schema changes when the wire layout changes. The bytecode revision changes when an opcode is added or its meaning changes. A decoder accepts only its own values, so old or newer code is refused at the header. Every integer width is explicit. The CRC is a corruption check, not an authentication. Decode rejects absurd counts before allocating them, and restore bounds every thread's stack and slot index by the snapshot's stack bound. `Runtime::snapshot` applies the same bounds, so a state restore would refuse for size fails when the checkpoint is taken. Restore fills a staging graph and only then returns a `Runtime`. Schemas 1 and 2 are not restored. Dead traversal anchors are in the snapshot. Slot-to-slot `next` links are rebuilt from that order and are not written. Source spans are not written; debug information is. Tags 31–51 are `LoadBool`, `CloseUpvalues`, `JumpIfFalse`, `Compare`, `Neg`, `ForPrep`, `ForLoop`, `Index`, `SetIndex`, `GetField`, `SetField`, `SetList`, `Len`, `Arith`, `BNot`, `Concat`, `MarkClose`, `CloseScope`, `CloseThread`, `GenericForLoop`, and `TailCall`.

Schema 23 adds per-thread hook images and Hook/HookNative boundaries, including
pending delivery, transfer ownership, return continuations, suppression and
host-yield instruction markers. Restore resolves every host-hook symbol before
runtime construction or userdata rebind. It validates masks/counts, canonical
event names, charges, cursor/frame bounds, pending activation and event phase,
transfer windows, and suppression/yield combinations. Schema 22 is refused;
bytecode 14, tables 4, GC 12 and binary chunk format 2 are unchanged in 3.35.

Schema 24 adds `Work::Utf8` (builtin-work tag 12), including partial char
bytes, scan cursors/counts and navigation state. Restore checks argument
counts, initialized scratch slots, cursor/buffer relationships and charges,
and rejects callback waits on UTF-8 work. Schema 23 is refused; bytecode 14,
tables 4, fuel 7, GC 12 and binary chunk format 2 remain unchanged in 3.36.

Schema 25 adds capability pending-work tag 8, file userdata payload tag 4,
HostLoad/DoFile/IO task tags 8/9/10, OS work tag 13 and terminal exit state.
Rooted wait payloads retain exact request bytes, opaque host token and typed
completion bytes. File images retain cursors, lookahead, read-ahead bytes and
consumed position. Restore validates operations, bounds, result types,
completed-key history, resource policy, buffer charges and exit phases.
Schema 24 is refused; bytecode 14, tables 4, fuel 7, GC 12 and binary chunk 2
are unchanged. Capabilities and journal entries remain outside snapshots.

Restore decodes each host value with its type's codec into the heap it is building, so a refusal drops everything decoded so far; it refuses unknown or snapshot-refusing host types, values that declare more than the image recorded, byte payloads whose charge is not their length, payloads that together pass the quota, and light tokens naming ids the image never reached (ADR 0045).

Restore also checks the thread graph (ADR 0041): one acyclic chain of resumers from the active thread to the entry thread, each waiting in a call that resumes or closes the thread above it, and no other thread running.

Restore runs every prototype through the compiler's bytecode check (`check_code`) and checks each child's captures against every parent that lists it. A closure must hold exactly as many upvalues as its prototype captures. A failure is `InvalidBytecode`, and no runtime is built. The order is: bounded decode, staging graph, graph and reference checks, bytecode checks, then the runtime.

This is an exact checkpoint for this bytecode. It is not a durable save, and it is not a promise across schema versions.

## Official test suite

`crates/moonseed-compat` runs each file of the pinned official Lua 5.4.9 suite
through the public builder with `Libraries::ALL`, explicit native capabilities,
`install_arg` and `./?.lua`. A composed filesystem exposes immutable suite files
before writable scratch files, consistently for open/probe/source reads. Nested
loading uses the real library paths. `tools/lua_suite.sh` runs fresh processes
under bwrap; shell confinement comes from that outer sandbox. There is no
unconfined fallback. Results are in `tests/compat/lua54-current.json` and
`lua54-phase337.json`: 33/33 compile, 14/33 PASS. The harness retains chunk names,
missing-global observations and output, and supplies only hook-related
`T.sethook`/`T.resume`, not the full C-test library. Numbered ledgers preserve
earlier runs; `tools/lua_suite_diff.py` compares them.

## Workspace

One runtime crate. `moonseed-wasm-probe` is a scalar export of the same kernel so the wasm32 build can exchange snapshot bytes without an unsafe pointer ABI. `moonseed-compat` is the official-suite harness, a binary over the public API. More crates wait until a boundary has survived a refactor.
