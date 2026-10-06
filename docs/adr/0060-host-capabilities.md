# ADR 0060: explicit host capabilities

Status: Accepted

Date: 2026-10-05

Expose Lua 5.4.9 IO/OS, loadfile/dofile, package.searchpath and filesystem Lua
modules through six independent optional capabilities: `Filesystem`, `Stdio`,
`Clock`, `CivilTime`, `Environment` and `Process`. The builder and restore host
carry shared objects; public traits expose bytes, resource IDs, typed completion
and structured errors, never VM heap/frame/journal internals. `Libraries::IO`
and `OS` join `STANDARD` through a `u16` selection mask. Installing libraries
grants no authority. Native adapters are feature-gated in the sole ambient OS
module; the VFS and deterministic mocks are portable.

Classify operations before invoking them:

| Class | Examples |
|---|---|
| Pure / deterministic | UTC civil conversion/formatting, difftime, C-locale query, mode/path processing and logical cursor changes |
| Journaled read | Probes, positional/source/stream reads, size, clocks, supplied civil conversion/zone names, environment, shell availability |
| Exactly-once mutation | Open, write/append, flush, close, remove/rename, temp reservations, stream writes, execute/popen and pipe writes/close |

All external builtin operations use one capability helper. Journal identity is
`EffectId { domain, sequence }`, class and exact encoded request, including
resource/path, offsets, lengths and write bytes. Retain typed results and errors.
Replay verifies request and class, raising a VM error on mismatch before backend
invocation. Persist the journal separately from snapshots; exactly-once is
deduplication within that preserved lineage, not atomic crash durability of an
external mutation and journal persistence.

Snapshots preserve VM history; they do not freeze the future external world.
Committed reads return old observations and mutations are not repeated; new
operations see current host state. Loadfile/searchers read journaled ranges
without retaining native handles. Atomic source versions across ranges require
immutable/versioned host storage. This rule also applies to prefetched file bytes.

Use positional filesystem IO. Lua file userdata owns the logical cursor,
numeral lookahead, EOF and canonical read-ahead bytes/consumed position. Buffer
bytes are charged and serialized. Line/numeral refills use 16 KiB; other
read/write requests are at most 64 KiB. Bounded work uses existing fuel and
continuation machinery. `setvbuf` selects logical no/full/line buffering. Pending output is charged
in the file userdata and journaled only when stdio semantics flush it: explicit
flush/close/seek/read, overflowing a filled buffer, whole blocks in a bulk
write, or a newline in line mode. Default-output switching retains the old
handle's pending bytes. The frozen Linux PUC oracle uses a 4096-byte file
buffer with NULL `setvbuf` storage, ignoring requested sizes; Moonseed preserves
that effective capacity, including libc's one-byte buffer after unbuffered mode.

One internal file type supports a per-value live-handle policy: VFS Rebind
verifies a preserved open key; standard streams rebind by kind; native files and
pipes Refuse with `NonPortableResource`. Closed files encode without a backend.
Pending acquisitions, including completed keys not yet consumed by Lua, obey
the same policy. Never serialize descriptors or close/drop handles to permit
a checkpoint. Restore checks aliases, work state, buffers and charges before
publishing a runtime. Explicit close, iterator exhaustion, `__close`, `__gc`
and shutdown share one journaled close authority. Dropping a runtime runs no Lua
cleanup and must not invalidate keys held for restore in a shared backend.

Capability `Completion::Pending` retains request, host token, sequence and work
phase in an ordinary VM wait. `complete_capability` validates a typed result;
the resumed helper commits it once without re-invoking the operation. Waiting
costs no fuel. Hosts retain pending-token/dispatch state separately and drive
closing through waits. Ordinary native waits keep their existing Completion API.

`os.exit` returns terminal `ExitRequested { status, close }`, never a process
exit and never catchable by protected calls. With closing enabled, close pending
main-thread variables then registered finalizers; suspended coroutine locals
remain pending. Exit status and closing phase survive fuel pauses, waits and
snapshots. The host owns status mapping and job/process policy.

Use the C locale only, without process-global locale changes. Pure Rust provides
UTC conversion, normalization and formatting. Without civil authority local
time falls back to UTC; supplied local conversion/zone names are journaled.
Clock capability separates wall time from CPU seconds. No C API, loadlib or
dynamic C modules are introduced.

Profiles are assembled rather than inferred: sandbox (none), read-only VFS,
deterministic VFS/mocks, filesystem with process disabled, rooted native host,
or explicit full native host. `package_paths` configures search without granting
authority; `install_arg` is explicit. Native component/symlink validation has
a TOCTOU gap and needs a trusted stable namespace; strong confinement uses VFS
or an outer OS sandbox. Shell commands are not confined by a filesystem root.

Schema 24 → 25 stores capability waits, file/iterator/loading/OS work, buffered
bytes and exit state. Fuel 7, bytecode 14, tables 4, GC 12 and binary chunk 2
remain unchanged. Phase 3.37 passes the frozen host corpus (1,869/1,869),
checkpoint/replay and portable native/Wasm witnesses; the official suite rises
8 → 14 PASS of 33. Long-string `%p`, c/r hook execution,
startup/IO wall costs and async ergonomics remain Phase 3.38 closure work.

See [embedding](../EMBEDDING.md#host-capabilities),
[compatibility](../LUA_COMPATIBILITY.md#io-and-os-phase-337),
[determinism](../DETERMINISM.md#host-capability-replay-phase-337), and
[security](../../SECURITY.md#host-capability-authority).

Phase 3.38 B1 extends file payloads inside schema 25 with a reserved high-bit
marker on the buffering tag, followed by capacity, scheduled-drain state and
pending output bytes. New readers also accept the original schema-25 file
payload (empty pending output); older readers reject the extended tag. Close
work uses tag 7 only when carrying a deferred write error; setvbuf work uses
tag 8. No top-level revision changes. A completed write stays in the journal
while its unchanged pending bytes are consumed once by the resumed builtin.
