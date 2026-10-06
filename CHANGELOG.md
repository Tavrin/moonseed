# Changelog

## 0.1.0 (2026-10-06)

First release. Minimum supported Rust version: 1.88.

### Capabilities

- Lua 5.4 source language and Lua-visible runtime semantics, with base,
  coroutine, math, table, string, UTF-8, package, IO and OS libraries.
  Debug introspection and Lua/host hooks are opt-in.
- Rooted values, checked conversions, typed Rust callbacks, host userdata,
  Rust-to-Lua calls and resumable native-to-Lua continuations.
- Fuel pauses, host waits and Lua yields; object, logical-heap, stack and
  compiler limits; incremental and generational collection.
- Portable execution checkpoints, including coroutines, hooks, pending waits,
  RNG and collector state. Userdata can use a portable codec, rebind by key,
  or refuse snapshots. Restore validates host registrations and limits.
- Explicit host capabilities for filesystem, streams, clocks, civil time,
  environment and processes. No external authority is granted by default.
  File writes support no/full/line buffering with checkpointed pending bytes.
- Journaled host observations and effects with exact request matching on replay.
  `os.exit` returns a terminal host outcome without exiting the embedding process.

### Compatibility and limits

The unmodified Lua 5.4.9 suite compiles 33/33 files: 14 PASS and 19 FAIL,
with zero UNKNOWN classifications. See the [classified ledger](docs/LUA_54_SUITE.md).
There is no Lua C API, arbitrary C-module loading, PUC binary-chunk support,
non-C locale, or PUC-compatible standalone command line.

Snapshots use schema 25, bytecode 14, tables 4, fuel 7 and GC 12.
Moonseed binary chunks use format 2. These are separate version boundaries;
checkpoints are not an archival format. Live native files and pipes refuse
snapshots. Native path checks have a filesystem race window and do not provide
strong confinement. See [release notes](docs/RELEASE_NOTES_0.1.0.md) and the
[compatibility policy](docs/COMPATIBILITY_POLICY.md).

The prior development log is preserved verbatim in [DEVELOPMENT_HISTORY.md](docs/DEVELOPMENT_HISTORY.md).
Its relative links retain their original repository-root context.
