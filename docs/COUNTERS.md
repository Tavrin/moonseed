# Execution counters

The optional `counters` feature measures executed work without changing the
ordinary build. It is independent of the timing-oriented `measure` feature.
`moonseed-bench` and the separate, private `integration/moss` workspace pass it through;
all three leave it disabled by default. No counter state enters snapshots.

Build `moonseed-run` with `--features counters`, then set
`MOONSEED_COUNTERS=/path/workload.json` when running a corpus file. The dump
includes execution and exit finalization, including a partial run that reports a
Lua error; compilation and library installation are excluded. A process killed
by a resource limit cannot write its exit dump. The environment variable is
ignored when the feature is disabled.

For Moss, build its executable with `--features counters` and set
`MOONSEED_COUNTERS_DIR=/path/results`. This selects a counter collection run:
100 and 1,000 interactions per frame, ten warmup frames, then fifty separately
measured frames for each size. Files are named `moss-100-00.json`, etc. This mode
returns after collection and does not run the timing benchmark. Run
`python3 tools/counters_report.py /path/results` to summarize corpus and Moss
JSON together as `COUNTERS.md`.

Embedders can use the feature-gated methods:

```rust,ignore
runtime.reset_counters();
let scope = runtime.counter_scope();
// Host-side API setup and execution of this runtime.
let json = runtime.counters().to_json();
drop(scope);
```

`run`, `start_call`, and legacy `call_closure` install a scope automatically.
Use an explicit scope to also include host-side table operations and other API
work between execution calls. Scopes nest and restore their previous sink on
unwind. A scope owns a reference to its runtime's sink, without borrowing the
runtime. Drop it before measuring host operations on another runtime. A reset
starts a new interval and breaks the opcode-pair chain. Returned counters are
independent copies; querying them does not reset the runtime.

The sink is an `Rc<RefCell<_>>` owned by the runtime. A thread-local pointer
routes leaf measurements, including key hashing, into the active sink. Counts
are plain integers with no atomic operations. Every increment and scope is
conditionally compiled; the disabled macro does not evaluate its arguments.
The optional JSON dependency is absent from the default dependency graph.

JSON contains opcode counts, nonzero opcode pairs, named events and allocations
by object type. Absent event/object kinds mean zero. Each allocation is `[count, initial_logical_bytes]`, not host
allocator traffic or subsequent container growth. Opcode pairs follow actual
execution across calls/coroutines, not static bytecode adjacency. Common tier
entries include rare fallthrough, so exclusive common instructions are
`dispatch_common_entries - dispatch_rare`. Hot attempts include declines;
`dispatch_hot` counts completed hot handling. The report documents the other
counter denominators and overlap, including repeated table probes on fallback.
Use frequencies with instruction/miss profiles before making performance
claims; these counts are not a wall-clock baseline.
