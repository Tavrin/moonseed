# ADR 0049 — Warnings

## Context

Lua 5.4 has a warning channel: `warn(msg1, ...)` and the warnings the runtime emits, such as a finalizer's error. The C host installs a warning function, which receives each piece with a "to be continued" flag and decides what control messages (`@on`, `@off`) mean. Moonseed's VM never writes to stderr, and every output must replay exactly once across checkpoints.

## Decision

- **Sink.** `Runtime::set_warnings(Box<dyn FnMut(&[u8], bool)>)`: host state like the `print` output, never in a snapshot. Each piece arrives with Lua's `tocont` flag, true for all but the last, and ends at its first zero byte, as Lua's C-string pieces do. Without a sink warnings go nowhere.
- **Effect.** A whole warning is one external effect committed through the journal under the next effect id, as each `print` write is (ADR 0031): a replay of a committed effect sends nothing again.
- **Control messages** (`warn("@on")`) are passed to the sink like any warning; what they mean is the sink's choice, as it is the warning function's in Lua. The VM has no on/off state.
- **`warn`** follows `luaB_warn`: at least one argument, every argument a string or a number (converted as `tostring` does for numbers, no `__tostring`), sent as one warning, no results; otherwise Lua's argument error.
- **Finalizer errors** go out as `error in __gc (msg)` (ADR 0048).

## Consequences

The base library binds `warn`. Tests and the reference harness print each warning as `[warn] text`, and the GC corpus compares them with Lua 5.4.9's.
