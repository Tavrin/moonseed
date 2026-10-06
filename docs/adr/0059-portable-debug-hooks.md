# ADR 0059: portable Lua and host debug hooks

Status: Accepted

Date: 2026-10-05

Keep hook state in a lazy `Heap::hooks` side table with weak generation-checked
thread owners, outside ThreadObj and hot instruction dispatch. Live threads
root their Lua hook and delivery values; entries never root their owner.
`hook_trap` is derived at cold changes and checked once per `run_hot` epoch.
Hooked threads, including c/r-only hooks, use the existing cold executor.
Call/return events observe semantic activations, not implementation helpers.

Registry symbols identify synchronous host callbacks; rooted thread values
enforce runtime ownership. New threads follow observed PUC Lua 5.4.9
inheritance, superseding the early non-inheritance requirement: mask/base
count are copied with a fresh countdown. Lua targets become inherited debug
wrappers without a child function; host targets inherit their stable symbol.

One delivery path charges one fuel unit before invocation. Pending events
survive quantum exhaustion. Lua delivery uses a hidden non-yieldable
`Boundary::Hook`; `HookNative` preserves semantic native activations across
steps. Suppression covers the callback, descendants, waits and error handlers,
and recovers on return/unwind. Finalizers suppress delivery; ordinary closes
remain observable. Transfer windows live only in the owning HookState during
delivery. Count advances once per begun Moonseed instruction, including
suppressed Lua bodies, independently of fuel and source lines.

A host line/count callback may yield zero values in a yieldable coroutine,
with no Rust continuation or host wait in the callback. Preserve the instruction
cursor and skip redelivery/recount on resume; resume arguments do not become
instruction results, and a count yield skips a simultaneous line event.
A suspended marker survives replacement/removal and is cancelled on close.
Illegal call/return/tail yields return catchable errors instead of the pinned
C oracle's crash. Host panics propagate and leave execution/snapshots refused;
no arbitrary-panic recovery is promised.

Schema 23 stores targets, masks, base/remaining counts, suppression, pending
events, historical/current cursors, instruction stages, yield markers,
transfer windows, return continuations and Hook/HookNative boundaries.
Restore resolves host symbols before runtime construction or userdata rebind
and validates references, charges, canonical names, counters, activation/event
phases, transfer windows and suppression/yield combinations. Schema 22 is not
upgraded. Fuel revision 7 includes the delivery unit; bytecode 14, tables 4,
GC policy 12 and binary chunk format 2 are unchanged by hooks.

The review fixes argument-window quota checks before writes, main reload hook
charging and cursor reset, callback reentry through custom conversions,
pending-event validation, native transfers and compiler source positions.
The private PUC `_HOOKKEY` is not mirrored: a decorative table would give
incorrect mutable callback authority, while a faithful second authority would
conflict with canonical thread ownership. Absolute count positions and extra
PUC operand-copy line events (notably multiline concatenation of locals) are
not emulated by adding instructions or changing fuel. Stripped execution is
checked separately from MSC versus PUC dumped bytes. The compatibility harness
implements only hook-related T commands through the public API, not a C API.

Final Z evidence has 151/151 hook acceptance cases and +0.116434% hooks-off
Ir geomean versus `eee7dff`; alloc_churn +1.222328% remains attributed rather
than removed from the metric. Enabled hooks have substantial cold-executor
cost, and warmed nonallocating callbacks have zero VM allocations per event.
See [compatibility](../LUA_COMPATIBILITY.md#debug-hooks-phase-335),
[performance](../PERFORMANCE.md#phase-335-debug-hooks) and
[embedding](../EMBEDDING.md#host-debug-hooks).
