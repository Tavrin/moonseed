# ADR 0031 — The base library: VM builtins, resumable base-function frames, and `print` as an external effect

## Context

Phase 3.20 ran the official Lua 5.4.9 suite and found every file stopping in its first lines on a missing base global: `print` in 24 files, then `require`, `io`, `collectgarbage`, `math`, `string`. Moonseed had ten base functions: `setmetatable`, `getmetatable`, `rawget`, `rawset`, `rawlen`, `rawequal`, `select` as ordinary natives, and `error`, `pcall`, `xpcall` implemented by the VM (ADR 0024).

Several of the missing functions call Lua:
- `tostring` and `print` call `__tostring`;
- `pairs` calls `__pairs`;
- `ipairs`'s iterator indexes, which can call `__index`;
- `load` calls its reader.

A native function (`NativeFn`) cannot call back into Lua, and nothing may wait on the Rust stack. `print` is also the first base function that acts outside the VM: its output must reach the host once, even when a run is restored from a checkpoint.

Lua 5.4.9 lets a coroutine yield across some of these calls and not others. Checked with the reference interpreter:
- `__pairs` may yield: `luaB_pairs` calls it with a continuation.
- `__tostring` under `tostring` or `print` may not. The error is "attempt to yield across a C-call boundary".
- `__index` under `ipairs`'s iterator may not.
- A `load` reader may not, and `load` returns that error as `nil, message`.

## Decision

### Which functions the VM implements

These base functions are VM builtins (`host::Builtin`), implemented in `runtime/builtins.rs`:
- `assert`, `type`, `tostring`, `tonumber`;
- `print`;
- `next`, `pairs`, `ipairs`, and `ipairs`'s iterator (registry symbol `base.ipairs_next`, not a global);
- `collectgarbage`;
- `load`.

A builtin can raise an error of any class, make strings and native values, and push frames.

The seven older natives stay natives. `error`, `pcall`, and `xpcall` stay where ADR 0024 put them.

`register_base` registers all of them. `Runtime::install_base` binds every name in `BASE_FUNCTIONS`, `_G` (the globals table itself), and `_VERSION` (`"Lua 5.4"`). `Runtime::install_base_only(names)` binds a chosen subset, for a sandbox that leaves out `print` or `load`.

### Reserved strings

The reserved strings used to be one error object per error class, made at boot so raising never allocates. They now also hold:
- the eight type names;
- `true` and `false`.

So `type`, and `tostring` of nil or a boolean, never allocate.

Five error classes are new:

| Class | Raised by |
|---|---|
| `Assert` | `assert(false)`, whose error is "assertion failed!" |
| `Argument` | a base function's bad argument |
| `ToString` | `__tostring` returned neither a string nor a number |
| `Reader` | a `load` reader returned a value of the wrong type |
| `Unsupported` | a collector mode or tuning option Moonseed lacks |

### A base function that calls Lua pushes a frame

It pushes a `Boundary::Builtin { func, passed, advance_caller, task }` frame, like `pcall`'s `Protect` frame:
- Its arguments stay where they were passed, at `func + 1 ..`.
- The call it makes goes at `func + 1 + passed`, just above them, and its results land there.
- The call is ordinary. A Lua callee pushes a frame; a native runs, waits, or is external; a table with `__call` resolves.
- When the call returns, the frame is on top again. `finish_builtin` then goes on from the result, as its own step. That step costs one unit of fuel and quantum, as an instruction does, because it may call again: a native returns at once, so a native `load` reader would otherwise run without end inside one quantum. `finish_protect` stays uncharged; it never calls.
- At the end, the frame pops and its results go to `func` through the caller's result window, as a native's do.

A base function that needs no Lua call pushes no frame and returns like a native. Only `tostring(v)` with `__tostring`, `pairs(t)` with `__pairs`, an `ipairs` step that reaches a function `__index`, and `load` with a reader pay for a frame.

The frame's `task` holds no Lua values:

| Task | Meaning |
|---|---|
| `ToString` | the result must be a string or a number |
| `Print { next }` | the argument being converted; those before it are written |
| `Pairs` | the first three results are `pairs`'s |
| `Ipairs { index }` | the index being read |
| `Load { source }` | the bytes the reader has given |

Each task also answers:
- **`wants`**: how many results its call wants: 3 for `Pairs`, else 1.
- **`yieldable`**: true only for `Pairs`. `do_yield` refuses a yield when any frame of the thread is a non-yieldable builtin, as it already did for a message handler.
- **`catches`**: true only for `Load`. An error unwinding from the reader stops at the `load` frame, and `load` returns `nil` and the error object. This covers the reader's own errors, a yield across it, a memory error, and a reader result of the wrong type. Every other builtin frame lets errors through to the nearest `pcall`.

`load` keeps the message handler of the protected call it runs in, as Lua's protected parser does. When the nearest catching frame below the `load` frame is an `xpcall`, its handler runs first. The `Handler` boundary records a `target`: the frame the unwind stops at once the handler returns. That is the `xpcall` frame, or the `load` frame. Under a `pcall`, or no protected call, no handler runs.

The frames are snapshot state (schema 11). Restore checks a builtin frame as it checks a `Protect` frame, and more:
- it sits on the call it stands for, owns no registers, and has its arguments on the stack;
- its task's cursor is within its arguments;
- a `load` source is at most the source limit;
- the frame above answers its call (`validate_called`);
- an unwind targets only a frame that catches.

### `tostring` and `print`

Both convert with one function, `plain_text`, which follows Lua's `luaL_tolstring`:
- A string is itself.
- A number is written as `..` writes it (`concat::number_text`). Lua 5.4.9 uses the same `%.14g` for both, so there is no second number format.
- nil and the booleans are their names.
- Any other value is its kind and an identity. The kind is the metatable's `__name` if that is a string, else the type name. The identity is `0x` and the hex `ObjectId` for tables, Lua functions, and threads; a native function is `function: builtin: <symbol>`.

Lua prints a memory address there. An `ObjectId` is the same on every target and across a checkpoint. The exact text is not a promise.

`__tostring` is looked up raw and called with ordinary call semantics; the first result is used. A string or number is accepted: Lua's `lua_tolstring` converts a number. Anything else raises `ToString`. The global `tostring` is never consulted: `print` does not call it.

### `print` writes as external effects

`Runtime::set_output` gives the output: a host closure that receives bytes. It is host state, not snapshot state, and a restored runtime needs it set again. Without one, `print` writes nothing.

`print` writes arguments with no `__tostring` together, with a tab before each argument but the first and a newline after the last. Before calling an argument's `__tostring`, it writes the arguments before that one. So:
- `print(1, bad)` writes `1` before `bad`'s error, with no newline, as Lua does.
- A `__tostring` that itself prints writes after the arguments before it and before its own argument's tab, as Lua does.
- `print(a, b, c)` with plain values is one write.

A write reaches the output in pieces: small ones gathered up to 64 KiB, a longer string on its own. So the host memory a `print` takes is bounded by its longest string, not by all its arguments together.

Each write is one external effect:
- Its id is the next effect sequence number, which is snapshot state.
- It is committed through the embedder's `Journal`, so a restored run that replays an id already in the journal writes nothing.
- A restore from a checkpoint, given the journal as it was then, writes exactly what the uninterrupted run wrote after that point. Given the finished run's journal, it writes nothing again.

There is no prepared stop before a `print` write, unlike an `External` native: the write happens in the step that decides it. The id is deterministic, so a checkpoint on either side of the step replays correctly.

### `load`

`load(chunk [, chunkname [, mode [, env]]])` for text chunks.

**The chunk.** A string or a number is compiled at once. A function (a Lua closure or a native, not a callable table, as in Lua) is a reader:
- It is called from a `load` frame until it returns nil or an empty string.
- Each string or number it returns is appended to `Task::Load { source }`.
- The source is charged to the logical heap while it grows, and every collection counts it (GC policy revision 3).
- Past the source limit (1 MiB), `load` returns `nil, "source exceeds 1048576 bytes"`. Past the heap quota, it returns `nil, "not enough memory"`.

**Reading.** The reader is called until the end of the chunk, and the chunk is compiled then; Lua parses as it reads, and stops calling the reader at the first syntax error. The mode is checked on the first piece, as Lua checks it on the first byte, so a refused mode reads one piece.

**Mode.**
- A chunk starting with `\27` is a binary chunk. Lua's mode check comes first: "attempt to load a binary chunk (mode is 't')".
- A binary chunk that the mode allows is then refused: "attempt to load a binary chunk: not supported by Moonseed". There is no bytecode loader.
- A text chunk under mode `b` is "attempt to load a text chunk (mode is 'b')".
- The mode ends at a zero byte, as a C string does.

**Compile errors.** `load` returns `nil` and `chunkname:line: message`. The chunk name is shown as Lua's `luaO_chunkid` shows it; the message is Moonseed's own. The chunk name is used only there: prototypes carry no source name.

**The environment.** The function is an ordinary vararg Lua closure. Its one upvalue, `_ENV`, is a closed cell holding:
- `env` when a fourth argument is given, even nil;
- otherwise the globals table, as Lua's `load` uses the registry's globals.

`Runtime::instantiate` makes it, and `boot` shares the code. `Runtime::load_function` makes one for the host.

### The other functions

**`assert`**
- Returns every argument when the first is true.
- Otherwise raises the second argument as it is, even nil, or the reserved "assertion failed!" when there is none.

**`type`**
- Returns a reserved name. No metamethod applies.

**`tonumber`**
- Without a base, a number is returned as it is. A string goes through `lex::string_to_number`, the conversion arithmetic and `for` already use. Anything else gives nil.
- With a base: an integer 2 to 36, then a string, read as Lua's `l_str2int` does, wrapping on overflow.

**`next`**
- The table traversal the `Next` instruction uses.
- At the end it returns one nil.

**`pairs`**
- Calls `__pairs` with the value, if its metatable has one, and returns three results.
- Otherwise returns `next`, the value, and nil. A non-table is accepted, as in Lua.

**`ipairs`**
- Returns its iterator, the value, and 0.
- Each step takes `i + 1` with integer wrap, then reads `t[i + 1]` with ordinary indexing.
- A nil result ends the loop, as one nil.

**`collectgarbage`**

| Option | Result |
|---|---|
| `collect` (the default) | a full collection; returns 0 |
| `step` | a full collection, which finishes a cycle; returns true |
| `count` | the logical heap in KiB, not allocator memory |
| `stop`, `restart` | set automatic collection off or on, which is snapshot state |
| `isrunning` | whether automatic collection is on |
| `incremental`, `generational`, `setpause`, `setstepmul` | an `Unsupported` error, not accepted and ignored |

### Revisions

| Revision | Value | Why |
|---|---|---|
| Snapshot schema | 11 | the reserved strings and builtin frames |
| Bytecode | 11 | unchanged: no instruction was added |
| Tables | 3 | unchanged |
| Fuel | 2 | one unit per step of a base-function frame that goes on from a call's result |
| GC policy | 3 | the `load` source charge |

## Alternatives

- **Natives that call back into Lua on the Rust stack.** This breaks the stackless model (ADR 0019, ADR 0024). A wait, a pause, or a checkpoint inside `__tostring` would be impossible.
- **One frame kind per function.** Five small state machines would each need a snapshot format and validation. One frame kind with a task shares the call, return, unwind, and restore code with `pcall`.
- **Buffering all of `print` and writing at the end.** This changes Lua's order: output before an error in a later argument would be lost, and a `__tostring` that prints would write first.
- **Every `print` argument as its own effect.** It would give the same order, with more journal entries and no gain.
- **A `load` that accepts only strings.** A reader would then need a later redesign. The frame costs little once `print` needs one.
- **Accepting and ignoring unsupported collector options.** A test that tunes the collector would pass while testing nothing.

## Consequences

**Language.** The base library is Lua 5.4's, except:
- `dofile`, `loadfile`, and `warn` are missing. `require` belongs to the package library.
- Error messages are Moonseed's.
- The default `tostring` of an object shows an `ObjectId`.
- `collectgarbage` has no incremental or generational mode.
- Binary chunks are refused.

**Evidence.**
- Six base fixtures give Lua 5.4.9's output, under the quantum, checkpoint, and collection schedules and a checkpoint at every step for the output.
- A `tonumber` corpus of 32,000 conversions matches Lua 5.4.9.
- A yield across each base function behaves as it does in Lua 5.4.9.

**Embedding.**
- `set_output`, `install_base_only`, and `load_function` are new, unstable API.
- The journal grows by one entry per `print` write, as it does per external native call.

**Cost.** `PERFORMANCE.md`, Phase 3.21.

**From the milestone review:**
- `print` first built one buffer of all its text; 16,000 one-mebibyte arguments aborted the host. It now writes in pieces.
- A native `load` reader ran about a million calls inside one quantum and one unit of fuel. The frame's steps now cost fuel (fuel revision 2).
- A reader was read to its end before the mode was checked. The mode is now checked on its first piece.
- An enclosing `xpcall`'s handler did not run for errors `load` returns; it now does, through the handler's `target`.
- A mode with a zero byte was read whole.
- `print`'s task held a second cursor that restore accepted out of step with the first, which duplicated output. The task now has one cursor.
- Two differences are documented rather than changed. A reader is read to the end before compiling, where Lua stops at the first syntax error. And `next(t, 1.0)` treats `1.0` as the key `1`.
