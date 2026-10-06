# ADR 0041 — The coroutine library over the existing thread machinery

## Context

Moonseed has had coroutines inside the VM since Phase 1: threads with frame stacks of their own, the hand-bytecode `NewThread`, `Resume`, `Yield`, and `CloseThread` instructions, a resumer link (`resumed_by`), failed coroutines that keep their stacks (ADR 0026), and snapshots of all of it. Lua programs could not reach any of it: there was no `coroutine` library. Four files of the official suite stopped on it (`calls`, `coroutine`, `nextvar`, `strings`), and many real programs use it.

## Decision

### Thread states

The library adds no state to a thread. Lua's four statuses are read from what there is:

| Moonseed | `coroutine.status` |
|---|---|
| the active thread | `running` |
| `LuaSuspended`, no frames (never resumed) | `suspended` |
| `LuaSuspended`, with frames (yielded) | `suspended` |
| `Ready` or `Waiting`, not active (it resumed the running one) | `normal` |
| `Completed` (returned, or closed) | `dead` |
| `Failed` (error nothing caught; its stack stays) | `dead` |

A coroutine waiting on the host is still running in Lua's terms; the host sees the whole run waiting. `coroutine.running` returns the active thread, and `true` for the entry thread, which is `registry[1]`. `coroutine.isyieldable` is false for the entry thread, for a thread being closed, and for a thread with a frame no yield may cross (an `xpcall` message handler, or a builtin without a continuation); true otherwise, dead coroutines included, as in Lua.

### Creating and starting

`coroutine.create(f)` takes a function value, not a callable table, as Lua does. The new thread has no frames and holds `f` in stack slot 0. The first resume puts the arguments after it and enters `f` as the thread's first frame when it is a Lua function. Any other function (`print`, `error`, `pcall`, `coroutine.yield` itself) is called from a trampoline: a three-instruction vararg function, `Vararg`, `Call`, `Return`, installed per coroutine. It stands for the C entry Lua's `lua_resume` has, and debug levels skip it, recognising it by its exact shape.

### Resuming

`coroutine.resume(co, ...)` and a `coroutine.wrap` function:
1. refuse a running or normal coroutine ("cannot resume non-suspended coroutine") and a dead one ("cannot resume dead coroutine");
2. refuse a 197th coroutine in one chain of resumes ("C stack overflow"). Lua 5.4.9 counts resumes and C calls together against 200 and stops a chain of plain resumes there; Moonseed counts the coroutines only, so chains through C calls go a little deeper than in Lua;
3. check that the coroutine's stack has room for every argument, however many the receiving call keeps, as Lua's `lua_checkstack` does; past the bound, "too many arguments to resume", and nothing has moved;
4. link the coroutine to the running thread and make it the active thread. The resumer's frame stays on its call; nothing is pushed for it and nothing nests on the Rust stack.

A coroutine suspended in `coroutine.yield` is on that call; the resume's arguments become its results, delivered as any builtin's results are. One suspended after the hand-bytecode `Yield` instruction takes no values.

### Answering the resumer

A yield, a return from the coroutine's last frame, an error nothing in it catches, and the end of a close each give control back to the resumer. How the resumer receives the outcome is read from the call it is making, not stored: `coroutine.resume` gets `true` and the values, or `false` and the error; a `wrap` function gets the values; `coroutine.close` gets `true`, or `false` and the error; the `Resume` and `CloseThread` instructions keep their pending state from before. The resumer's stack is checked for every value first; past the bound, `coroutine.resume` gets `false` and "too many results to resume", a `wrap` function raises it, and the coroutine stays as it was. Values and nil holes are carried exactly, counted, never inferred from the last non-nil value.

An error ends the coroutine without unwinding it, as since Phase 3.14: `resume` returns it, and the stack stays for `debug` until the coroutine is closed. A `wrap` function closes the coroutine first (its pending `<close>` values run with the error, and cannot yield), then raises the error left, with the position of the Lua function that called it before a string error that is not a memory error (`luaL_where(1)`).

### Closing

`coroutine.close` accepts a dead or suspended coroutine: a never-started one is dead at once; one with frames closes through `CloseThread`'s unwind, as since Phase 3.14. It returns `true`, or `false` and the error, and the coroutine is dead and empty; closing it again returns `true`. A running coroutine raises "cannot close a running coroutine", a normal one "cannot close a normal coroutine".

### Debug

A coroutine's levels are its own frames (ADR 0040): a suspended one shows `coroutine.yield` at level 0, a failed one the builtin that raised, a normal one `coroutine.resume`. While an error's unwind runs the `__close` values of the frames it leaves, those frames are no levels: Lua pops them before it closes anything, so the `__close` function has no name and the `pcall` that caught the error is its caller. The same holds for a coroutine being closed.

### Fuel, collection, snapshots

Every instruction and builtin call costs its fuel on whatever thread runs it, so switching threads buys nothing.

Since source can now make threads, a thread's stack counts against the logical heap: a thread costs its fixed 256 logical bytes and 16 for each stack slot. Collections count every stack's length; each place that grows a stack outside a call's own registers charges the growth, and the check before a call, a result window, varargs, or a coroutine transfer grows a stack refuses growth past the quota ("not enough memory"; a resume reports it as Lua's `lua_checkstack` failure, "too many arguments to resume"). While a memory error's unwind runs `__close` values, the thread may still grow within its stack bound, as Lua's spare stack lets those closes run. This is GC policy 7.

A coroutine is an ordinary object: a `wrap` function keeps its thread through its one value, a resumer is reachable from the thread it resumed, and nothing else roots a suspended thread; collecting one does not run its `<close>` values, as in Lua. The snapshot wire format is unchanged (schema 14). Restore now checks the thread graph: the active thread's chain of resumers is acyclic and ends at the entry thread; each resumer waits in a call that resumes or closes the thread above it, and a closing thread only under `coroutine.close` or a `wrap` function; no thread outside the chain runs; a coroutine never resumed holds a function in slot 0 and nothing else.

## Alternatives

- **A scheduler record of who resumed whom and how.** The thread link and the resumer's call site already say it; a second copy would have to be kept in step and validated against the first.
- **A flag for "suspended in `coroutine.yield`".** Also derivable from the call site. The one ambiguity is hand-built bytecode whose `Yield` instruction is followed by a call of `coroutine.yield`, which a resume then treats as the yield call.
- **Running a builtin body without a frame.** Every builtin runs from a frame's call site; a trampoline frame keeps that true.
- **Resuming on the Rust stack, as `lua_resume` does.** It would give up pausing and checkpointing inside coroutines.

## Consequences

**Revisions.** GC policy 7 (thread stacks count). Snapshot schema 14, bytecode 11, tables 3, fuel 4, binary chunk format 2: unchanged; restore accepts no state it did not before except coroutines the library makes.

**From the milestone review** (one round, by a separate reviewer):
- Blocker, fixed: thread stacks were not charged to the heap quota. A thousand coroutines each holding 40,000 values reached 638 MB of process memory against the 64 MiB quota while `collectgarbage("count")` grew by 320 KB. Stacks now count, as above; the same program stops at the quota, and a test fills a 4 MiB quota with coroutine stacks and catches the memory error.
- Major, fixed: restore accepted a closing coroutine under a `wrap` function whose unwind carried no error; finishing the close then hit a host error. Restore now requires the error.
- Minor, fixed: restore accepted a coroutine that had finished but was still linked to its resumer, and a suspended coroutine whose pending call was not `coroutine.yield`. Both are refused now.
- Minor, fixed: another thread's level 0 was missing when its pending builtin was reached through `pcall`, `xpcall`, or a metamethod (`pcall(coroutine.yield)`, `__index = coroutine.yield`, a resume through `pcall`). It is read from the thread's call site now.
- Minor, recorded: a coroutine whose body is a builtin makes a trampoline prototype and closure of its own, so such coroutines reach the object limit three times sooner than ones with a Lua body.
- Held up: transfers at the stack bound in every shape, resume cycles, every status and yield boundary, `wrap` and close ordering, host waits in every construct, fuel, collection of cycles and trampolines, and the hand-bytecode paths.

**Evidence.** A 122-line corpus against Lua 5.4.9 (counts and nil holes, statuses from every vantage point, errors, builtin bodies, close, wrap and its prefixes, generic `for` over `wrap`, resume depth, yields across 11 boundaries, debug views, close levels) matches, and keeps its output and fuel under small quanta and with a collection, checkpoint, and restore at every step. Host waits inside resumes, wraps, nested resumes, and closes restore exactly once. Transfers at the stack bound fail as Lua's do, one value short of it succeed. 196 nested coroutines and a thousand resume-and-close rounds run on a 256 KiB Rust stack. Native and wasm32 agree.

Phase 3.32's transfer helper bulk-copies only ordinary Lua result windows that
already fit the receiving stack and its ordinary bound. It keeps every retained
slot, nil padding, scratch clearing, truncation, top and PC identical to native
return delivery. Metamethod/boundary continuations, short restored stacks,
growth and reserves retain native delivery; checks for all supplied arguments
and results and the publication order of status/resumer/active thread stay in
place. Ordinary suspended yield sites decode their prototype and instruction
once; boundary and metamethod sites still use the general call-site decoder.

**Suite.** No file stops on `coroutine`. With hooks stubbed and each assert reporting its line, `coroutine.lua` runs to its end with two failures, neither in the library: a hook trace, and a weak table the collector never clears.
