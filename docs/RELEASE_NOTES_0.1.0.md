# Moonseed 0.1.0

Released 2026-10-06.

Moonseed implements the Lua 5.4 source language and its Lua-visible runtime
semantics, with most of the standard library. It does not implement the Lua C
API, PUC Lua binary chunks or C modules. The `io` and `os` libraries only reach
the outside world through capabilities the host provides.

The first release provides a rooted Rust API, typed callbacks and userdata,
resumable execution, resource limits, portable checkpoints and journaled host
effects. Rust 1.88 is the minimum. The default `native-host` feature compiles
adapters without granting authority; `counters` is optional, unstable diagnostics.
See the [embedding guide](EMBEDDING.md) for usage.

The unmodified Lua 5.4.9 suite compiles 33/33 files: **14 PASS, 19 FAIL,
zero UNKNOWN**. Failed files remain failures even when their first blocker is
an accepted boundary. The [suite ledger](LUA_54_SUITE.md) records each disposition;
the [compatibility guide](LUA_COMPATIBILITY.md) records finer differences.

## Known limitations

- No Lua C API, dynamic C modules, `package.loadlib`, PUC chunks, or
  PUC-compatible standalone CLI. Rust callbacks and Lua/host module loaders
  are supported.
- C locale only. Local civil time falls back to UTC unless the host supplies
  timezone conversion. Native CPU time is Linux-only; native temporary-file
  allocation is Unix-only. Unsupported adapters report errors.
- Table order, collection timing, object identity text and long-string `%p`
  sharing differ from PUC. Portable math can differ from its host libm by an
  ulp; NaN sign and payload are outside the determinism contract.
- Hook counts follow Moonseed instructions, not PUC instructions. Some line
  traces differ. Enabled hooks use the cold executor and can be expensive.
  The private `_HOOKKEY` registry convention is not implemented.
- Resource and compiler bounds differ from PUC. Ordinary calls allow 1,000
  frames plus an 80-frame error/close reserve; stacks default to 50,000 slots,
  with a 100,000-slot ceiling. Plain resume chains stop at 196 coroutines.
- Runtime state is single-threaded, neither `Send` nor `Sync`. Callbacks must
  bound their own work. Fuel does not bound host CPU time; logical heap bytes
  do not bound process memory.
- Live native files and pipes refuse checkpoints. Rebindable files and host
  userdata need retained resources on restore. A snapshot does not contain the
  external journal, host callbacks, backend files or pending-operation ledger.
- Snapshots and binary chunks require compatible revisions. There is no
  archival migration promise or cross-version determinism guarantee.
- Native filesystem checks have a TOCTOU gap. Shell execution, if enabled,
  has host-process privileges and is not confined by the filesystem root.
- Replay needs the original journal lineage. Restoring does not undo external
  work; atomic durability between an effect and its journal record belongs to
  the host. New reads can see changed backend contents.
- No interactive `debug.debug` shell or `debug.setcstacklimit`. Installing
  debug grants access to otherwise hidden Lua locals, upvalues and metatables.

The [compatibility policy](COMPATIBILITY_POLICY.md) defines version boundaries.
See [security](../SECURITY.md) for host responsibilities and
[performance](PERFORMANCE.md#release-instruction-counts) for measured costs.
