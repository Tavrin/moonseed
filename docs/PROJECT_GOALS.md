# Project goals

## Mission

Moonseed is a Lua runtime written in Rust. It aims to be among the best Lua runtimes available to Rust programs, and it is designed so that a running computation can be treated as deterministic, resumable, portable state.

Most of that is not built yet. `STATUS.md` says what works today, `LUA_COMPATIBILITY.md` says which parts of Lua, and `ROADMAP.md` gives the order of work. This file is the intent those documents are measured against.

## Long-term bars

### Lua fidelity

Lua 5.4 is the compatibility target. Once enough of the language exists, the official Lua test suite becomes a release gate. Phase 3.20 pinned the Lua 5.4.9 suite by hash and recorded a baseline (`LUA_LANGUAGE_AUDIT.md`); the libraries it needs now order the roadmap, and each run of it is compared with the last. What one consumer happens to use does not define "complete". Every deliberate difference from Lua 5.4 is written down in `LUA_COMPATIBILITY.md`.

### Performance

The portable interpreter should be competitive with the strongest Rust Lua interpreters on a representative corpus fixed in advance. The internal engineering target is roughly within 10% of the fastest qualifying Rust interpreter, by geometric mean, on that corpus. That is a target, not a current claim, and no comparison has been run yet.

Startup, throughput, latency, allocation, memory, and host crossings are measured separately. Interpreters are compared with interpreters, and compiled tiers with compiled tiers. Per-workload regressions stay visible; an average does not hide them.

### Embedding

A Rust application should be able to do all of the following without `unsafe` and without knowing how the collector works:

- create a runtime and choose which libraries and capabilities it gets;
- load source;
- read and write globals and tables;
- register Rust functions and define userdata;
- call Lua functions and receive typed values;
- handle errors;
- set resource limits;
- drive suspended execution;
- checkpoint and restore where supported.

The bar for ergonomics is the mature embedding libraries.

### Runtime control

Fuel, memory limits, and capability restrictions are product features. Running out of fuel normally pauses a script so it can be continued, rather than killing it.

### State control

Exact portable checkpoints, deterministic replay, and explicit continuations are core capabilities of the runtime. They are not extensions for one consumer.

### Portability

Native targets and `wasm32-unknown-unknown` are both first-class. Any behavior advertised as deterministic across targets is tested on those targets, not assumed.

### Production quality

Before a 1.0, Moonseed should have:

- fuzzing and hostile-input tests;
- security documentation;
- a compatibility matrix;
- a published benchmark method;
- a semver and release policy;
- API documentation with examples;
- production users other than its first one.

## What should set it apart

The aim is one runtime with all of these at once:

- Lua compatibility;
- fast execution;
- safe Rust embedding;
- stackless resumability;
- deterministic execution;
- portable exact checkpoints;
- Wasm;
- resource governance;
- later, cheap forks of a running state;
- later, a compiled tier that can always return to the portable canonical state.

The later items are research directions. Any of them may change or be dropped when the evidence says so.

## Reference points

These are tracked for comparison, not ranked:

- PUC Lua 5.4 for semantics and as the conventional interpreter baseline;
- Piccolo for stackless, sandboxable pure-Rust design;
- omniLua for compatibility, Wasm, and pure-Rust embedding;
- Luna for interpreter, JIT, and AOT performance;
- mlua for Rust embedding ergonomics;
- LuaJIT and Luau for performance on subsets they share with Lua 5.4.

Competitors change. Versions are pinned when a comparison is run. No document claims "faster than", "more compatible than", or "best" without a result that can be reproduced.

## First consumer

Moss, a game engine, is the first demanding production user. It supplies real workloads: game scripting, calls across an ECS boundary, deterministic replay, browser execution, agent play harnesses, checkpointing, and frame-time budgets. Moonseed contains no engine types and no Moss-specific behavior. "Moss does not need it" is not a reason for a permanent gap.

## Maturity direction

These are directions, not dates or promises:

| Version | Meaning |
|---|---|
| 0.1 | A useful real Lua runtime |
| 0.2 | A credible embedding alternative |
| 0.3 | Compatibility and performance hardened |
| 0.4 | Checkpoint and determinism tooling as the differentiator |
| 0.5+ | Branching and compiled-execution research |
