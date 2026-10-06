# PUC vs Moonseed benchmark corpus

This is corpus revision 2 for the Phase 3.32 performance campaign. The 15
revision-1 Lua files remain byte-identical; seven isolated workloads are added.
The frozen profile, instruction and perf collectors, embedding/Moss runner,
and environment policy are documented in [the methodology](../tools/BENCHMARKING.md). The scripts, iteration counts, inputs, and runner are preregistered
in this repository. Run the same files for each revision. Do not resize a
workload, change its algorithm, or remove a failing workload to improve a ratio.
The 2026-10-02 wall/cycle results are diagnostic only: the machine is not in
performance mode. Callgrind instruction counts drive decisions.

## Workloads

Each script is self-contained and prints one deterministic checksum line.
The nonempty workloads are sized for roughly 0.2–1 s on PUC Lua 5.4.9. Wall
time on a loaded machine varies. Integer checksums stay within signed 64-bit
range; the float checksum uses an explicit format. Inputs do not use clocks,
files, external state, or either runtime's random-number generator.

| Script | Work measured |
|---|---|
| `empty.lua` | Startup, library installation, one checksum print, and shutdown. |
| `fib.lua` | Recursive Lua calls: `fib(34)`. |
| `table_fields.lua` | Field and array reads/writes, branches, and 600,000 update rounds. |
| `alloc_churn.lua` | 3,000,000 small-table allocations, automatic GC, and a survivor ring. |
| `strings.lua` | 360,000 formatted strings, concatenation, `table.concat`, and `gmatch` captures. |
| `sort.lua` | Four pairs of 100,000-number sorts, default ascending order and a descending Lua comparator; complete order validation and weighted checksums. |
| `numeric_loops.lua` | 60,000,000 integer iterations, 10,000,000 float iterations, and nested stepped loops. |
| `generic_for.lua` | 40,000 traversals of 128-element tables with `pairs` and `ipairs`; commutative sums avoid depending on `pairs` order. |
| `method_calls.lua` | 4,000,000 `obj:move()` calls through a shared prototype. |
| `closures.lua` | 10,000,000 calls to 32 closures with mutable shared upvalues. |
| `coroutines.lua` | 3,000,000 resume/yield exchanges with integer messages. |
| `metamethods.lua` | 4,000,000 rounds invoking Lua `__index`, `__add`, and `__call` handlers. |
| `patterns.lua` | 200,000 rounds of `find`, `match`, balanced matching, and `gsub` with captures. |
| `native_calls.lua` | 2,000,000 rounds of standard library natives: `math.abs/min/max/floor`, `string.byte`, and `type`. |
| `application.lua` | 256 entities over 12,000 frames: tables, methods, captured state, formatted event strings, and final concatenation. |

Revision 2 adds `branches`, `array_access`, `globals`, `tail_recursion`,
`field_writes`, `string_concat`, and `string_format`. These calibrated at
163–379 ms on PUC on 2026-10-02 (diagnostic timing). The manifest in
`tools/bench_manifest.py` supplies category tags and explicit profiling-only
scaling rules. Generated scaled sources are stored with results, never here.

The native workload uses library natives only. The existing userdata harness
requires custom native registration in both hosts; this corpus runs stock PUC
Lua and the standard Moonseed runner. It does not measure userdata allocation
or host-specific userdata operations.

## Run

Run from the repository root. The stable profile freezes one codegen unit,
fat LTO, portable CPU code generation, and line-table symbols:

```sh
export CARGO_TARGET_DIR=target
python3 tools/bench_build.py --output RESULTS/build.json
W=1 R=9 CORE=4 python3 tools/bench_corpus.py \
  --json RESULTS/corpus.json \
  > RESULTS/corpus.md
```

Defaults are `W=1` warm-up run per engine, `R=9` measured repetitions, and
`CORE=4`. Use `R=3` for a smoke run. `W=0` is accepted for diagnostics; use warm
runs for comparisons. `PUC`, `MOONSEED_RUN`, and `LUAC` override binary paths.
The default PUC is
`vendor/lua-5.4.9/src/lua`;
the default Moonseed runner is `$CARGO_TARGET_DIR/bench-stable/moonseed-run` at the
target directory shown above. Set `MOONSEED_RUN` if building elsewhere.
The harness checks that PUC reports Lua 5.4.9. It uses a sibling `luac` when
available; without one, PUC compile time is omitted. Use the `luac` from the
same PUC build when overriding it.

The standalone runner accepts one file, `--timings`, and `--compile-only`:

```sh
( ulimit -v 2000000; timeout 300 taskset -c 4 "$CARGO_TARGET_DIR/bench-stable/moonseed-run" --timings bench/corpus/empty.lua )
( ulimit -v 2000000; timeout 300 taskset -c 4 "$CARGO_TARGET_DIR/bench-stable/moonseed-run" --compile-only bench/corpus/application.lua )
```

It installs standard libraries and debug, runs with unlimited fuel and the
VM's default memory/GC configuration, forwards `print` to stdout, and closes
the runtime to run finalizers. An uncaught error writes `ERROR: <msg>` to
stderr and exits nonzero. It does not lift VM resource limits. The corpus
uses only base, string, table, math, and coroutine; Moonseed has no io/os
libraries. PUC's standard startup includes libraries absent from Moonseed;
the empty script reports this startup difference.

## Measurement rules and columns

Every child process runs under `ulimit -v 2000000` and `timeout 300`, with a
SIGKILL fallback after two seconds. Both runtimes and `luac -p` are pinned to
the same requested core with `taskset -c`. The harness refuses an unavailable
core. Locale is `C`, timezone is `UTC`, and PUC uses `-E` to ignore Lua
environment initialization. A repetition starts a fresh process; warm runs
warm machine/file caches, rather than retaining a VM between repetitions.

For each workload, all W warm-up pairs precede the R measured pairs. PUC and
Moonseed alternate within those pairs, and successive workloads reverse which
runtime starts. The separate `luac -p` probe follows each pair. No workload
processes run concurrently within the harness. Other users' processes can
still compete with the pinned core or share its physical CPU and memory.

The first pair establishes a byte-for-byte reference checksum. Every later
sample must match it. With `W=0`, the first measured pair supplies the reference. A failed run leaves the check
unavailable; different successful outputs produce a checksum mismatch.
Neither receives a ratio. An engine stops after its first error, mismatch,
malformed timing line, or timeout. The harness continues the other engines
and workloads, writes the full reports, and exits 1 if any engine failed or
any equality check was unavailable. Failed or incomplete samples never form
a reported median. Keep workloads that fail now due to the 10,000-live-object
limit and rerun them unchanged after that limit is lifted. Memory errors
include object counts to distinguish that limit from other resource failures.

All table times are milliseconds:

- **PUC/Moonseed median and p95**: total elapsed process wall time measured
  with Python's monotonic clock, including the cap/pinning wrappers, loading,
  source reads, compilation, boot, execution, output, and shutdown. p95 uses
  the nearest rank, `ceil(0.95 * R)`; at R=3 or R=9 it is the maximum sample.
- **Ratio**: Moonseed median wall time divided by PUC median wall time, only
  when both have all R successful samples and the checksum check is equal.
- **MS compile/boot/run**: independent medians and p95 values of the nanosecond intervals
  from `--timings`. Compile covers compilation and chunk naming, excluding
  source reading; boot covers registry, runtime, library, and output setup;
  run covers execution, finalizers, and flushing stdout. These are not a
  decomposition of the median wall sample and need not sum to that median.
- **PUC compile and compile p95**: separate `luac -p FILE` process wall time.
  This includes `luac` startup and source reading; it is not parse-only time
  and is not directly comparable to Moonseed's internal compile interval.
- **Check / status**: output equality, or an explicit failure and diagnostic.

Startup is reported separately using `empty.lua`, including its checksum
print. Compile and startup times are not subtracted from workload wall times.
JSON preserves all warm-ups, measured samples, diagnostics, checksum bytes,
source hashes, runner hash, executable hashes, paths, and settings. Changed
inputs during a run invalidate it; do not edit or rebuild while measuring.

Wall acceptance requires a quiet machine in performance mode. The JSON marks
`not_acceptance_evidence` when any measured one-minute load exceeds the
configured threshold (default 1.0), or diagnostic mode is enabled (default). State the load with every
result. The report header records CPU model, the selected core's governor
when readable, and `uptime` including load averages before and after the run.
Use the same hardware, core, governor, PUC build, release build configuration,
W, R, and resource caps for comparisons. Do not use shared-machine smoke
numbers as the final baseline. No final baseline numbers are recorded here.
