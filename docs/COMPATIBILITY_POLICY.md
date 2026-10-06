# Compatibility policy

Moonseed uses Cargo SemVer. During 0.x, a minor release may change the supported
Rust API; patch releases preserve it. Bug and security fixes can change erroneous
behavior. Breaking changes, MSRV increases and format revisions are recorded in
the changelog. Pin an exact release when reproducible execution is required.

## Separate contracts

| Surface | 0.1 contract |
|---|---|
| Rust API | The documented rooted embedding API is supported. Match `non_exhaustive` errors and outcomes with a fallback. Deprecated integer-only APIs and hidden proof/measurement helpers are outside this surface; unstable counters have no compatibility promise. |
| Lua semantics | Lua 5.4 source and Lua-visible semantics are the target, subject to the [documented differences](LUA_COMPATIBILITY.md). A fix may change incorrect results or diagnostics in a patch release. Implementation-defined order, identity text and exact compiler-generated hook positions are not PUC compatibility promises. `_VERSION` remains `Lua 5.4`. |
| Snapshot schema | Schema **25**. Restore requires matching semantic revisions, valid state and the host prerequisites below. No automatic conversion from another schema. |
| Binary chunks | Format **2**, containing bytecode revision **14**. Moonseed-specific code containers, portable across supported Moonseed native/Wasm targets; not PUC chunks and not runtime checkpoints. Incompatible revisions are rejected. |
| Fuel | Revision **7** defines accounting. A compiler change may reduce instructions, source-program fuel totals and pause locations without changing this revision. Pin compiler/build identity for exact budgets. |
| Bytecode | Revision **14** identifies the instruction encoding and semantics used in chunks and snapshots. A compatible Rust API does not imply compatible bytecode. |
| Tables and collection | Table semantics revision **4** and GC policy revision **12** are also checked by snapshots. Collection timing and table behavior are part of deterministic execution. |

Format or semantic revision changes require their own recorded compatibility
notice; the crate version alone does not describe a saved image. Revisions are
necessary checks, not a promise that every older binary can read every future
extension carrying the same schema number. In particular, older schema-25
readers reject extended buffered-file payloads written by the 0.1 runtime;
the current reader accepts the original schema-25 file payloads. Keep the writer
version/build with checkpoints and use a reader documented to support that writer.

## Snapshots

Snapshots are portable across supported native and Wasm targets for the same
schema and semantic revisions, subject to host registrations, resource policy
and target-independent codecs. They serialize logical object identities and VM
state, not Rust layouts or process addresses. They are execution checkpoints,
**not an archival format**. There is no long-term migration guarantee.

Restore constructs a fresh runtime only after validation. It requires the same
effect domain, each retained native callback and host-hook symbol, and the
registered userdata types with their matching codec or rebind policy. Even a
masked-off retained host hook needs its symbol. Moonseed supplies its own
library symbols; `Host::libraries` can deny them without installing new globals
or granting external authority. An explicit registration retains precedence.

The host retains the journal, callback state, backend contents, resource keys
and pending-operation dispatch state separately. Rebind resolves a key to an
already-owned resource; it must not repeat an external acquisition or effect.
A host codec must validate untrusted bytes and have the same meaning on both
targets. Output and other optional capabilities may be omitted at decode time
but needed for subsequent execution.

Common refusal cases:

| Condition | Result |
|---|---|
| Live native file, pipe, or pending acquisition using a refusing backend | `SnapshotError::NonPortableResource` |
| Host userdata registered without a portable codec or rebind policy | Snapshot refuses with `NonPortableUserdata` |
| Missing native or host-hook symbol, or denied library symbol | Restore refuses with `UnknownHostSymbol` |
| Missing userdata registration, incompatible policy, invalid codec bytes or unavailable rebind key/backend | Restore refuses with the corresponding userdata, codec or `Rebind` error |
| Host limits below what the retained objects, strings, stack, heap or image need | Restore refuses with `LimitExceeded` |
| Incompatible revisions, malformed references or continuation state | Restore refuses during decode/validation |

A host's resource limits are not raised by a snapshot. Restore applies the
smaller runtime resource limits and the host's snapshot-size bound; a retained
library buffer can fail at its next growth under a lower string limit. Lower
limits can change later allocation failures and collection schedules, so they
are outside an identical-replay claim. See [the resource model](adr/0052-scalability-envelope.md).

Owned Rust roots belong to one runtime. Reacquire them after restore through
globals or `ObjectId`; the old roots give `ApiError::WrongRuntime`.

## Determinism boundary

For the same build, program, host registrations, effect domain, limits, entropy
and host replies, changing fuel slicing or checkpoint schedules preserves the
semantic result. Observations include values, object aliasing, journal outcomes,
fuel and the documented hook/collection state. Wall time, number of host polls,
Rust allocation addresses and execution caches are outside that contract.

Native/Wasm tests cover the integer kernel and additional math, string,
coroutine, userdata, collection, hook and capability fixtures. This is bounded
cross-target evidence, not certification of arbitrary floating-point programs
or host implementations. Math uses portable `libm`; NaN sign/payload are excluded.
No cross-version determinism is promised. See [DETERMINISM.md](DETERMINISM.md)
for the tested observations and schedules.

External callbacks and pure resolvers must return deterministic answers, or
persist and replay their observations. A checkpoint preserves VM history, not
the future external world. Committed reads replay recorded bytes; new reads
observe the backend. Source reads in separate ranges need host-pinned versions
for an atomic view. Journal deduplication is not an atomic transaction between
an external side effect and durable recording.

Restoring also rewinds VM fuel and sequence counters. Keep non-rollback quotas
outside the VM when a guest must not reset its budget. A snapshot checksum detects
corruption, not authenticity; validate provenance before trusting a checkpoint.

## MSRV

The minimum supported Rust version is **1.88**. A minor 0.x release may raise
it, with the new minimum recorded in the changelog. Patch releases keep the
minor series' MSRV. The repository's development toolchain pin can be newer;
it does not change the library's declared minimum.
