# ADR 0040 — Debug information, and the `debug` library without hooks

Compare-and-branch (ADR 0055) retains the condition's line and comparison
metamethod name through suspension. Locals/call PCs use actual emitted code;
the removed boolean temporary is no longer observable between compare and
branch. Named operand locals remain available while a handler yields.

## Context

Phase 3.32 constant-operand arithmetic (ADR 0054) keeps the operator line and
metamethod name on `ArithK`. A removed literal load can remove its source line
from active lines; ranges and call PCs use the emitted code. Immediates are not
registers for debug-local lookup. Arithmetic fault text remains the baseline's
reserved generic message; operand-name attribution is not implemented.

Thirteen files of the official suite start with `require "debug"`. Tracebacks, `debug.getinfo`, and `debug.getlocal` need what Moonseed's prototypes did not keep: a chunk name, the line of each instruction, local and upvalue names, and how each function was called. Lua's hooks (`debug.sethook`) need more: a check in the dispatch loop on every instruction. This phase adds the information and the introspection, and leaves hooks for a milestone of their own.

## Decision

### Debug information

Each prototype carries a `DebugInfo` (`debuginfo.rs`), made by the compiler:
- `linedefined` and `lastlinedefined`: the lines of the function's `function` and `end`; 0 and 0 for a chunk, whose `what` is then `main`.
- A line for every instruction.
- The locals, parameters first, in declaration order, each with its name, its register, and the instructions it is live over (`start` up to `end`), as Lua's `LocVar`. Internal locals have Lua's names: three `(for state)` before a numeric `for`'s variable, four before a generic `for`'s. `<const>` locals are ordinary registers in Moonseed, so they are visible to `debug.getlocal`; Lua folds constant ones away.
- Each upvalue's name.
- The name of the function each call calls, recorded when the call is compiled. Lua finds these at run time by symbolic execution of its bytecode (`getobjname`); Moonseed's instruction set differs, and the compiler knows the answer. The rules are Lua's: a local, an upvalue, or a global by the called name's scope; `obj:m()` is a method; `t.k()` is a field `k`, `t[1]()` the field `integer index` (Lua's `GETI`, integer keys 0 to 255), `t[x]()` the field `?`; a field of `_ENV` is a global; parentheses change nothing; a string constant is a constant; a generic `for`'s call is `for iterator`.

The chunk's name is a string object on every prototype of the chunk (`Proto::source`), shared, not copied:
- `load(s)` names the chunk with the string `s` itself, or with its `chunkname`;
- `load(reader)` names it `=(load)`;
- a binary chunk carries its own name;
- a host names a compiled chunk with `CompiledChunk::set_chunk_name`, otherwise it is `=?`.

**Lines.** An instruction's line is where the construct that made it ends, as Lua's is the last token read when it was emitted. As in Lua: a call is on the line where its arguments open (`(`, a string, or `{`), so each call of a multi-line method chain has its own line; an arithmetic, bitwise, or concatenation operator's instruction is on the operator's line, a unary one's on its operator's; a table constructor is on its `{` line; a numeric or generic `for`'s loop instructions are on the `for` line; a test is on its condition's line; the jump past an `else` is on the arm's last line. Every function now ends in a `Return` on its `end` line, even after a `return`, as Lua's does: `activelines` shows it. Lines break at `\n`, `\r`, `\r\n`, and `\n\r`, each one break, as in Lua's lexer. Differences remain where Lua's code generator emits code Moonseed's does not, or later: Lua emits no test for `while true` and `until true`, folds constant arithmetic (`x + 1 * 2`) and `<const>` locals, loads a constant operand of `..` only when it reaches the operator, and emits a field read only when its value is used, so `activelines` and a few `currentline`s differ there.

**Cost and checks.** Debug information is charged to the logical heap: a reference for every line, local, upvalue name, and call name, plus the names' bytes (GC policy 6). It is in snapshots (schema 14), with line numbers as signed deltas in a variable-length code, about a byte an instruction. Restore, binary `load`, and the validator check that there is a line for every instruction or none, a name for every upvalue or none, locals within the code and within the function's registers, call names at increasing instructions within the code, and names within the string limit. Names are kept whole, so a checkpoint or a dump never changes one.

**Binary chunks.** Format revision 2 adds a prototype's debug information, and the chunk's name on the root. `string.dump(f, true)` keeps what a stripped Lua chunk keeps: `linedefined`, `lastlinedefined`, and the call names Lua can still find without local and upvalue names (a global becomes a field, since `_ENV` has no name left, and an upvalue's name is `?`). A stripped function's source is `=?`, its lines -1, its upvalues `(no name)`.

### Stack levels

A frame records whether a tail call made it (`Frame::tail`, schema 14): `istailcall`, and the traceback's `(...tail calls...)`.

A level is Lua's. Level 0 is the running function: on the running thread, the debug function itself. Each level below is the frame under the one above:
- a Lua frame is a Lua function;
- a `pcall` frame, and a builtin's frame (ADR 0031), are C functions: `what` is `C`, `source` `=[C]`;
- an `xpcall` handler's frame marks where the handler was called, and is no level.

On another thread, level 0 is the builtin its top Lua frame is calling, when there is one: `coroutine.yield`'s place, a `resume`, or the builtin that raised the error a failed coroutine died of, as Lua shows `[C]: in function 'error'`.

A frame's current instruction is its `pc`, except under a Lua frame its own `Call` pushed, whose `pc` has moved past the call. A metamethod call, a boundary, and a builtin leave the `pc` on the instruction.

A function's name at a level comes from the call site below it: the recorded call name, or `metamethod` and the event (`index`, `add`, `close`, …) when the caller is in a metamethod call, as Lua's `funcnamefromcode`. A level reached by a tail call has no name. A level called from a C function has none.

Two differences from PUC Lua follow from Moonseed's structure. A builtin called by a tail call runs in its caller's place (ADR 0029), so the Lua function that made the tail call is not a level; Lua keeps it under the C function. And the host has no C function under the main chunk: Lua's standalone interpreter shows `[C]: in ?` there, and counts it in a long traceback's skip.

### The `debug` library

Installed by `install_debug` (with `register_debug`), and by no standard installer. `getregistry`, `getmetatable`, `setmetatable`, `getinfo`, `getlocal`, `setlocal`, `getupvalue`, `setupvalue`, `upvaluejoin`, and `traceback`, each checking its arguments in `ldblib.c`'s order with Lua's messages:
- `getmetatable` ignores `__metatable`. `setmetatable` sets a table's own metatable, or the metatable every value of a basic type shares (ADR 0034): nil, booleans, numbers, strings, functions, and threads.
- `getinfo` takes a level or a function, and the options `S l n r t u f L`; `>` or any other letter is "invalid option". `ftransfer` and `ntransfer` are 0, as outside a hook.
- `getlocal` and `setlocal` find locals live at the level's instruction, `(temporary)` for any other slot of the frame below the call it is making, and `(vararg)` for negative indices, a vararg frame's extra arguments. Given a function, `getlocal` names its parameters. A C level has no locals in Moonseed; Lua names its stack slots `(C temporary)`.
- `getupvalue` gives a Lua function's upvalues by name, and a builtin's values with the empty name, as Lua's C closures. `setupvalue` sets a Lua function's; a builtin's values are its own state, so it refuses them (no results), where Lua would set them.
- `upvaluejoin` makes the first function's upvalue the very cell of the second's, open or closed.
- `traceback` follows `luaL_traceback`: the message (a number converted, anything else not a string returned as it is), `stack traceback:`, a line a level from the given level (1 on the running thread, 0 on another), the first 10 and last 11 levels past 22 with Lua's "(skipping N levels)" count, and each function named first by `pushglobalfuncname`: a search of `package.loaded` and the tables in it for the function, raw, string keys only, `_G.` dropped.

**Stepping.** Everything but `traceback` does bounded work in one step: a level walk (at most 1,080 frames), or at most one function's lines for `activelines`. `traceback` is a machine (`DebugWork`): its search looks at 256 table entries a step, each step after the first costs a unit of fuel, and its text so far is part of its state, charged to the logical heap, at most the 1 MiB string limit ("not enough memory" past it). A checkpoint inside it restores the search where it was.

**Absent, not stubbed.** `sethook` and `gethook` (a hook needs a check in the dispatch loop; a milestone of its own), `debug.debug` (it reads the console), `getuservalue` and `setuservalue` (Moonseed has no userdata), `upvalueid` (it returns a light userdata identity), and `setcstacklimit` (deprecated in 5.4). A call to one is a call of nil.

**Safety.** The library reads and writes every local, upvalue, and metatable, and the registry: it breaks every boundary a sandbox draws, so hosts opt in. The VM stays memory-safe and deterministic under it. Everything it can write is a Lua value that the VM checks where it uses it; the one exception was a numeric `for`'s state, which `setlocal` can make a non-number: the loop now raises "'for' loop state is not a number" there, a Lua error. Lua's behaviour in that case is undefined.

## Alternatives

- **Symbolic execution over Moonseed's bytecode, as Lua's `getobjname`.** More code, on an instruction set whose registers the compiler already knows.
- **Stubs for hooks and `upvalueid`.** A program would run on with wrong answers; the milestone forbids placeholders.
- **A C level under the main chunk.** It would describe a host that does not exist.
- **Keeping the calling frame for tail-called builtins.** ADR 0029 chose to hand them down; changing that is its own decision.

## Consequences

**Revisions.** Snapshot schema 14 (debug information and chunk names in prototypes, the tail-call mark, traceback tasks). GC policy 6 (debug information and a traceback's text count against the heap). Binary chunk format 2. Bytecode 11, tables 3, and fuel 4 are unchanged.

**Cost.** A 1,510-instruction chunk carries 15,641 logical bytes of debug information, about 10 bytes an instruction, and its snapshot grows from 11,130 to 15,545 bytes. `string.dump` of a 300-statement function takes 50 µs instead of 31 µs and loading it 57 µs instead of 36 µs; compiling a source 4% longer. No row of the ordinary benchmarks moved beyond noise: nothing was added to the dispatch loop.

**From the milestone review** (one round, by a separate reviewer):
- Local and upvalue names were cut to 256 bytes when written: a checkpoint, a dump, or a collection after restore changed what `getlocal` and `getupvalue` returned, and the heap's count. Names are now kept whole; a proof checks a 300-byte name through a checkpoint at every step and a dump.
- Calls in multi-line method chains, and operators split over lines, were on other lines than Lua's. Fixed as described under **Lines**, and the corpus covers them.
- A traceback's text was bounded by the string limit but not checked against the heap quota. It now is, and is charged as it grows.
- `load`'s room check did not count a new chunk-name string. It now does.
- Recorded, not fixed: the `activelines` differences above; `debug.getinfo(f, 'L')` builds one line table per call and every debug call walks the frames, work that fuel charges as one call (bounded by 10,000 instructions and 1,080 frames); a `require` name with a zero byte is not cut at it as Lua's C strings are; a stripped function's call through a local has no name, where Lua finds the local's source; an `xpcall` handler calling `debug.traceback(m, 2)` loses the failing function, from the tail-call and C-level differences together.
- Held up: no host error or panic under a `setlocal` fuzz of every register kind (loop states, to-be-closed slots, call windows, frames inside metamethods and closes), type metatables on every basic type, upvalue joins on running frames, and crafted snapshots.

**Evidence.** A corpus of 180 lines (`corpus_debug.lua`: `getinfo` on every kind of function and option, chunk names, call names of every kind including 17 metamethods, locals, varargs, temporaries, `for` states, `setlocal`, upvalues and joins, type metatables, the registry, tracebacks with tail calls and module names, stripped and unstripped dumps, the main chunk, lines of multi-line calls and operators) gives Lua 5.4.9's output. Suspended and failed coroutines are introspected (hand bytecode: the coroutine library does not exist yet). A traceback restores at every step of its search with the same text and fuel. Native and wasm32 agree on both corpora and on dumped debug information.

**Suite.** None of the 13 files that `require "debug"` passes. Each now stops later, on: the coroutine library (`calls`, `coroutine`), hooks (`db`), `upvalueid` (`goto`), compile-error wording (`constructs`, `errors`, `literals`), `io` or `os` (`events`, `files`), `collectgarbage` modes (`gc`, `gengc`), the heap quota (`big`), and a module that is a suite file (`locals`).

## Phase 3.32 arithmetic result destinations

Arithmetic emission accepts a free result slot without changing operand
evaluation. Single-local assignments record candidates and wait until all local
captures are known, including captures after a backward goto. Only a terminal
`Add`/`Arith` and its immediately following scalar store can be folded. Captured
or to-be-closed declarations and alternate branch entries veto the fold. No
parallel assignment, call result, short-circuit result, concatenation, or close
operation is folded into a local. Arithmetic commits only on success, after a
resumable metamethod returns; it never exposes an intermediate RHS value in the
local. The normal temporary result-slot path also benefits call arguments and
return windows without overwriting any active local.

The final compaction maps every relative branch and both endpoints of local PC
ranges, as well as call-site PCs. The arithmetic operation retains its original
source span and operator line; removed copies lose their lines, intentionally
allowing `activelines` to lose a line represented only by such a copy. Debug
observers, yielding metamethods, error handlers, and quantum-1 restores exercise
this contract. The representation and all format revisions are unchanged.
