# ADR 0017 — Native functions as Lua values

## Context

Host functions were registry symbols reached only through `CallHost`: one integer in, one integer out, and every call got an effect id and went through the journal. A script could not store one in a variable, pass it, or put it in `_ENV`. `setmetatable`, the standard library, and any engine API need native functions that behave like any other Lua function. They also need several arguments and results, calls that can wait, and no journal for operations whose only effects are inside the VM.

## Decision

### Identity

A native function is `Value::Native(i)`. `i` indexes `Heap::natives`, the runtime's list of interned registry symbols. A symbol is interned the first time a value for it is made, so one symbol always gives the same value. Equality is index equality: the same symbol is equal, different symbols are unequal, and ordering faults. A native value can be a table key. It is not a collectable object and holds no Rust pointer.

The symbol is the portable identity. Each runtime also keeps `native_slots`, the registry slot each interned symbol resolves to. That is a cache: it is not in a snapshot, restore rebuilds it, and a call does not look up a string.

A future native closure with captured host state would be a distinct object with its own identity. That is a separate value form, and this one stays as it is.

### Registration and binding

`HostRegistry::register_native(symbol, policy, function)` registers a `NativeFn`, `fn(&mut NativeCall) -> NativeOutcome`. `Runtime::set_global_native(name, symbol)` binds a global to it. From there Lua passes it around like any value. All of this is unstable API.

### Calls

Source `f(...)` is the ordinary `Call` whatever `f` holds. `Call` on a closure takes the existing Lua path, unchanged. On a native it goes to an out-of-line `call_native`. On anything else it is now the Lua fault `BadCall`; before, it was reported as VM corruption. `Call` runs in `exec`, so `run_hot` is unaffected.

The native reads its arguments from their registers and pushes its results. The results are written at the callee slot and adjusted to the call's wanted count. The same `finish_result_window` a Lua return uses then closes the window, so zero, one, many, nil holes, fixed counts, and open results all follow the existing rules. The argument and result buffers are reused across calls, so a call does not allocate.

### Policies and outcomes

Each registration declares a `NativePolicy`:

- `VmLocal`: the function reads and returns values, and anything it changes is VM state that a checkpoint restores. It runs when `Call` executes. It gets no effect id and does not see the journal.
- `External`: the function acts outside the VM. `Call` charges one fuel unit, allocates an effect id, and stops in `NativePrepared`, a safe point before the function runs, as `CallHost` does. `poll` then runs it without charging again, with the id and the journal, so a replayed id returns the committed outcome.

The policy is declared, not inferred from the symbol.

A native returns `Ready` (its pushed values are the results), `Pending(key)` (the frame records `NativeWaiting` and the run returns `Waiting`), or `Fault` (`LuaFault::Native`). `complete_wait` delivers one integer result through the same result window. Starting a new wait clears the record of the last completed key, so a function may reuse a key across successive waits; completing the same wait twice is still `AlreadyCompleted`.

While a native call is prepared or waiting, the frame's `pc` stays on the `Call` and the arguments stay in their registers. The pending state records only the effect sequence and the wait key. A checkpoint needs nothing else, and the collector already traces the arguments.

A native cannot call Lua. A function that needs to, such as a sort comparator, `pcall`, or a metamethod-aware library function, will need an explicit continuation. That is later work, and recursion on the Rust stack is ruled out.

### Snapshots

Schema 4 adds three things: a section listing `Heap::natives` in order, value tag 8 and dead-key tag 6 for a native by index, and the pending states `NativePrepared { sequence }` and `NativeWaiting { sequence?, wait_key }`. The schema changes because this is a new wire-level value form, not only new code.

Restore rejects:

- a duplicate symbol (`InvalidStructure`);
- a symbol the supplied registry lacks (`UnknownHostSymbol`);
- a native index past the section (`DanglingReference`);
- a native pending state whose instruction is not a `Call` on a native value, or whose policy does not match (`InvalidStructure`). `NativePrepared`, or a sequence on `NativeWaiting`, requires `External`.

All of this is checked before a runtime exists.

## Alternatives

A separate `CallNative` opcode chosen by the compiler. A variable's contents are not known at compile time, and `__call` will need the same dispatch.

Serialize a registry index. That ties a snapshot to one registration order.

Journal every native call. Library functions like `setmetatable` change only VM state, which a checkpoint already covers. A journal entry each time would be overhead with no effect to deduplicate.

## Consequences

`CallHost` and the legacy `HostFn` remain for the hand-built proof programs. Library functions, metatables, and engine bindings will be `NativeFn`s. The callback context is narrow: it reads numbers and booleans, pushes numbers, booleans, and nil, and passes any value through unchanged. Strings, tables, userdata, and typed conversions are embedding work still to come.
