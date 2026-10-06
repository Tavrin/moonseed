# ADR 0048 — Running finalizers, their timing, and closing the runtime

## Context

A finalizer is Lua code: it may call natives, wait on the host, run out of fuel, be checkpointed, and fail. It cannot run inside the collector, which is synchronous Rust, and must not nest on the Rust stack. Lua runs finalizers in `GCTM`, on the thread that triggered the collection, without yields, with the collector stopped, and turns their errors into warnings. Lua does not promise when a finalizer runs; Moonseed must decide deterministically.

## Decision

**Timing (Moonseed's rule, not a Lua guarantee).** Every finalizer a collection queues runs, in queue order, before the interrupted code takes another step. No scheduled collection starts while they run or between them, as Lua runs a batch with nothing collected in between; and when one ends, the next scheduled collection waits until the interrupted code has allocated the smallest debt of its own, so a finalizer that makes garbage and registers its object again cannot take every step. Concretely, a finalizer starts at the next step at which the active thread is at a clean point: a Lua frame between instructions, with no metamethod call, pending call, assignment, or unwind in progress; or `collectgarbage`'s frame waiting for the finalizers; or, while closing, no frame at all. A collection run by the host between steps (`Runtime::collect`) queues finalizers that run at the next step of `run`. `collectgarbage("collect")` and `("step")` return once the finalizers queued when their collection ended have run (`Task::Collect` counts them); finalizers that later collections queue run after the call returns.

**The call.** Each finalizer is a `Boundary::Finalizer` frame pushed on the active thread, above everything the frame below holds (its registers and anything up to `top`), with `__gc` and the object above it; `__gc` is looked up from the object's current metatable, and without one nothing is called. Starting it costs one fuel unit, as a call does (fuel revision 5); what it runs costs fuel as usual. Its results are dropped, its slots go and `top` is put back when it ends, and the frame below is untouched. A finalizer runs on the coroutine that was running, as in Lua: `coroutine.running` and `isyieldable` (false) see that.

**Inside a finalizer:**
- no yield crosses the frame ("attempt to yield across a C-call boundary", caught as an error);
- no scheduled or requested collection starts, and `collectgarbage` returns nil for every option (Lua's `lua_gc` while stopped); an allocation that needs room still collects, as Lua's emergency collection does, so a finalizer may make more garbage than the limits hold at once;
- a host wait is allowed: the run waits, can be checkpointed, and resumes the finalizer exactly once;
- an error stops at the frame, with no `xpcall` handler below consulted, and becomes the warning `error in __gc (msg)`, `msg` being a string error's text or `error object is not a string` (Lua's `luaE_warnerror`); the next finalizer then runs.

**Memory pressure.** An allocation the quota refuses collects once and is tried again; the collection queues dead finalizable objects but cannot run their finalizers mid-instruction, so they are reclaimed after their finalizers have run, at a later collection. There is no second emergency collection and no loop: what remains past the quota is a memory error. Objects that register themselves again each time are finalized at each collection, as in Lua; near the quota that is a collection, and every such finalizer, per allocation, all of it paid in fuel.

**Closing** (`Runtime::begin_close`, unstable API). Once a run has ended (completed or failed, on the entry thread), `begin_close` puts every registered object on the queue after those already there, newest registration first, and stops all registration (Lua's `GCSTPCLS`). It makes, after a collection, the one closure the finalizer frames on the frameless entry thread name; that closure may pass the object limit and the quota by its own size, so closing never fails for room. A failed run keeps its error while it closes. `run` then runs them on the entry thread, errors as warnings, and returns the run's own end again; the runtime may be checkpointed meanwhile. Dropping the runtime afterwards frees everything, and only then do host values' Rust `Drop`s run. Lua's shutdown finalizers need this call: dropping a runtime runs no Lua.

## Alternatives

- **A dedicated finalizer thread.** `coroutine.running` inside a finalizer would differ from Lua's.
- **Running finalizers inside the collection.** Rust recursion, no checkpoints, no waits.
- **Lua's lazier timing** (a few finalizers per collection step). Moonseed's collector has no steps yet; running all of them at once is deterministic, and Lua permits it.

## Consequences

**From the milestone review** (one round, by a separate reviewer):
- Blocker, fixed: automatic collections ran between finalizers and queued again objects their finalizers had registered again, so `collectgarbage()` waited on them for ever, and the program could take a step per finalizer; Lua returns after one. Now no scheduled collection runs during a batch, `collectgarbage` waits for its own batch only, and each finalizer's garbage leaves the interrupted code room before the next collection.
- Blocker, fixed: no collection at all ran inside a finalizer, so one making more than the object limit's worth of short-lived objects failed for memory, where Lua's emergency collections let it finish. Allocations now collect inside finalizers.
- Blocker, fixed: closing allocated a closure per finalizer, and near the object limit failed before running any; it now makes one, past the limits if need be.
- Blockers, fixed: three snapshot states the runtime makes were refused or accepted wrongly: `collectgarbage` with more than two arguments waiting for a finalizer; a failed run closing; a finalizer frame at a thread's bottom outside closing, which restored and then hit a host error.
- Major, fixed: the fast interpreter tier was off whenever a finalizer was queued, including while one ran; a finalizer now runs in it like any code.
- Minor, fixed: a warning piece with a zero byte now ends there, as Lua's C-string warnings do; `gmatch`'s state userdata makes room before it is made.
- Held up: weak modes and what they clear, ephemeron chains and cycles, resurrection timing, the registration rule, errors, coroutines and yields in finalizers, every `collectgarbage` call site, and `warn`'s checks; the review's programs matched Lua 5.4.9 apart from known message wording, and restored at every step with the same output and fuel.

Fuel revision 5, snapshot schema 16. Debug levels skip the finalizer frame: level 2 inside `__gc` is whatever was running. Hooks (Phase 3.30) will need to know a finalizer is running; the frame says so.
