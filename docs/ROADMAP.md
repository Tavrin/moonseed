# Roadmap

The 0.2 backlog is a set of candidates, not a release schedule. Changes need a
concrete use case, measured cost where relevant, and tests that retain the
[compatibility and determinism contracts](COMPATIBILITY_POLICY.md).

| Candidate | Work needed |
|---|---|
| Call/return hooks | Measure a path that keeps call/return-only hooks out of per-instruction cold execution; preserve event order, fuel, yields and checkpoint state. |
| Interpreter and startup costs | Re-profile calls, comparator callbacks, allocation churn, coroutines and library installation on representative applications. Keep checked handles and resource accounting. |
| Long-string `%p` | Consider canonical constant sharing without weakening heap accounting or snapshot alias validation. Equal bytes do not currently promise equal identity tokens. |
| Buffered native IO and modules | Tune line reads, flushes, cached `require` and source loading using matched inputs and controlled measurements. Preserve Pending and replay behavior. Logical no/full/line buffering already exists. |
| Async host ergonomics | Simplify typed completion, pending-operation persistence and shutdown through waits without duplicating host effects. |
| Streaming snapshots | Reduce peak encoding/decoding memory while retaining bounded validation and deterministic images. |
| Copy-on-write forks | Investigate shared checkpoint storage and isolation; define interaction with roots, host rebinds and external effect domains first. |
| Time-travel debugging | Build stepping and history inspection around checkpoints and durable journals. Keep non-rollback host budgets separate. |
| Locales | Evaluate explicit portable locale data. Do not use process-global locale changes. |
| Dynamic modules | Research an explicit module ABI and capability policy. Arbitrary Lua C modules remain unsupported. |
| JIT research | Assess whether a compiled tier can preserve fuel, safe points, debug observations and portable checkpoints. No implementation commitment. |

Performance comparisons must retain source and binary identities, matching
outputs and the measurement method. Instruction reductions do not establish
wall-clock improvements. Current results are in [PERFORMANCE.md](PERFORMANCE.md).
